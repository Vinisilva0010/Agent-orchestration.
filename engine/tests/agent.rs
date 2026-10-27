use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use engine::{
    agent::{Agent, AgentConfig, Decision, EngineError, REJECTED_RESULT, RunStatus},
    gateway::GatewayClient,
    tools::{Tool, ToolRegistry, TypedTool},
};
use protocol::{CompletionRequest, ContentBlock, ModelTier};
use schemars::JsonSchema;
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::{Value, json};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ArchiveArgs {
    report_id: String,
}

#[derive(Default)]
struct Counts {
    get_time: Arc<AtomicU32>,
    archive: Arc<AtomicU32>,
}

fn registry(counts: &Counts) -> ToolRegistry {
    let time_calls = Arc::clone(&counts.get_time);
    let archive_calls = Arc::clone(&counts.archive);
    let get_time = TypedTool::new("get_time", "Current time.", move |_: NoArgs| {
        time_calls.fetch_add(1, Ordering::SeqCst);
        async { Ok("2026-10-26T12:00:00Z".to_string()) }
    });
    let archive = TypedTool::new(
        "archive_report",
        "Archives a report.",
        move |args: ArchiveArgs| {
            archive_calls.fetch_add(1, Ordering::SeqCst);
            async move { Ok(format!("archived {}", args.report_id)) }
        },
    )
    .mark_irreversible();
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(get_time), Arc::new(archive)];
    ToolRegistry::new(tools).unwrap()
}

fn reply(content: Value, stop_reason: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "provider": "mock", "model": "mock-standard", "content": content, "stop_reason": stop_reason,
        "usage": { "input_tokens": 1, "output_tokens": 1, "cache_read_tokens": 0, "cache_write_tokens": 0 },
        "cost_micros": 5, "task_budget_remaining_micros": 100
    }))
}

/// Mounts replies that are served once each, in order.
async fn gateway(replies: Vec<ResponseTemplate>) -> MockServer {
    let server = MockServer::start().await;
    for (index, reply) in replies.into_iter().enumerate() {
        Mock::given(method("POST"))
            .respond_with(reply)
            .up_to_n_times(1)
            .with_priority(u8::try_from(index + 1).unwrap())
            .mount(&server)
            .await;
    }
    server
}

fn agent(server: &MockServer, counts: &Counts, config: AgentConfig) -> Agent {
    let client = GatewayClient::new(
        &server.uri(),
        SecretString::from("tsk_test-token".to_string()),
    )
    .unwrap();
    Agent::new(client, registry(counts), config)
}

async fn sent(server: &MockServer, index: usize) -> CompletionRequest {
    serde_json::from_slice(&server.received_requests().await.unwrap()[index].body).unwrap()
}

fn tool_use(name: &str, input: Value) -> Value {
    json!({ "type": "tool_use", "id": "call_1", "name": name, "input": input })
}

fn tool_result(request: &CompletionRequest) -> (String, bool) {
    match &request.messages[2].content[0] {
        ContentBlock::ToolResult {
            content, is_error, ..
        } => (content.clone(), *is_error),
        other => panic!("expected a tool result, got {other:?}"),
    }
}

#[tokio::test]
async fn answers_text_and_sends_cached_system_and_tools() {
    let server = gateway(vec![reply(
        json!([{ "type": "text", "text": "done" }]),
        "end_turn",
    )])
    .await;
    let counts = Counts::default();
    let config = AgentConfig {
        tier: ModelTier::Small,
        ..AgentConfig::default()
    };
    let agent = agent(&server, &counts, config);
    let state = agent.drive(agent.start("hello")).await;
    assert_eq!(state.status, RunStatus::Completed);
    assert_eq!(state.final_text, "done");
    assert_eq!(state.cost_micros, 5);

    let request = sent(&server, 0).await;
    assert_eq!(request.tier, ModelTier::Small);
    assert!(request.system[0].cache);
    let names: Vec<&str> = request
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    assert_eq!(names, ["get_time", "archive_report"]);
}

#[tokio::test]
async fn reversible_tool_runs_and_model_output_is_sent_back_unchanged() {
    let first = json!([
        { "type": "reasoning", "signature": "sig-1" },
        { "type": "tool_use", "id": "call_1", "name": "get_time", "input": {}, "signature": "gemini-sig" }
    ]);
    let server = gateway(vec![
        reply(first.clone(), "tool_use"),
        reply(
            json!([{ "type": "text", "text": "it is noon" }]),
            "end_turn",
        ),
    ])
    .await;
    let counts = Counts::default();
    let agent = agent(&server, &counts, AgentConfig::default());
    let state = agent.drive(agent.start("what time is it?")).await;
    assert_eq!(state.status, RunStatus::Completed);
    assert_eq!(counts.get_time.load(Ordering::SeqCst), 1);

    let second = sent(&server, 1).await;
    let expected: Vec<ContentBlock> = serde_json::from_value(first).unwrap();
    assert_eq!(second.messages[1].content, expected);
    assert_eq!(
        tool_result(&second),
        ("2026-10-26T12:00:00Z".to_string(), false)
    );
}

#[tokio::test]
async fn irreversible_tool_waits_for_approval() {
    let server = gateway(vec![
        reply(
            json!([tool_use("archive_report", json!({ "report_id": "q3" }))]),
            "tool_use",
        ),
        reply(json!([{ "type": "text", "text": "archived" }]), "end_turn"),
    ])
    .await;
    let counts = Counts::default();
    let agent = agent(&server, &counts, AgentConfig::default());

    let paused = agent.drive(agent.start("archive q3")).await;
    assert_eq!(paused.status, RunStatus::AwaitingApproval);
    let actions = agent.approval_actions(&paused);
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].name, "archive_report");
    assert_eq!(actions[0].input, json!({ "report_id": "q3" }));
    assert_eq!(counts.archive.load(Ordering::SeqCst), 0);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    let done = agent.decide(paused, &Decision::ApproveAll).await.unwrap();
    assert_eq!(done.status, RunStatus::Completed);
    assert_eq!(counts.archive.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn anything_but_explicit_approval_rejects() {
    let decisions = [
        Decision::RejectAll,
        Decision::PerCall(HashMap::new()),
        Decision::PerCall(HashMap::from([("other_call".to_string(), true)])),
        Decision::PerCall(HashMap::from([("call_1".to_string(), false)])),
    ];
    for decision in decisions {
        let server = gateway(vec![
            reply(
                json!([tool_use("archive_report", json!({ "report_id": "q3" }))]),
                "tool_use",
            ),
            reply(
                json!([{ "type": "text", "text": "not archived" }]),
                "end_turn",
            ),
        ])
        .await;
        let counts = Counts::default();
        let agent = agent(&server, &counts, AgentConfig::default());
        let paused = agent.drive(agent.start("archive q3")).await;
        let done = agent.decide(paused, &decision).await.unwrap();
        assert_eq!(done.status, RunStatus::Completed, "{decision:?}");
        assert_eq!(counts.archive.load(Ordering::SeqCst), 0, "{decision:?}");
        assert_eq!(
            tool_result(&sent(&server, 1).await),
            (REJECTED_RESULT.to_string(), true)
        );
    }
}

#[tokio::test]
async fn invalid_arguments_skip_approval_and_go_back_to_the_model() {
    let server = gateway(vec![
        reply(
            json!([tool_use(
                "archive_report",
                json!({ "_unparsed_arguments": "{" })
            )]),
            "tool_use",
        ),
        reply(json!([{ "type": "text", "text": "sorry" }]), "end_turn"),
    ])
    .await;
    let counts = Counts::default();
    let agent = agent(&server, &counts, AgentConfig::default());
    let state = agent.drive(agent.start("archive")).await;
    assert_eq!(state.status, RunStatus::Completed);
    assert_eq!(counts.archive.load(Ordering::SeqCst), 0);
    let (content, is_error) = tool_result(&sent(&server, 1).await);
    assert!(is_error);
    assert!(content.contains("invalid arguments"), "{content}");
}

#[tokio::test]
async fn stops_when_the_budget_is_spent() {
    let server = gateway(vec![ResponseTemplate::new(402).set_body_json(
        json!({ "error": { "code": "budget_exceeded", "message": "task budget exceeded" } }),
    )])
    .await;
    let counts = Counts::default();
    let agent = agent(&server, &counts, AgentConfig::default());
    let state = agent.drive(agent.start("hello")).await;
    assert_eq!(state.status, RunStatus::BudgetExceeded);
    assert_eq!(state.error.as_deref(), Some("task budget exceeded"));
}

#[tokio::test]
async fn gateway_failures_end_the_run_with_the_reason() {
    let server = gateway(vec![ResponseTemplate::new(503).set_body_json(
        json!({ "error": { "code": "provider_temporarily_unavailable", "message": "busy" } }),
    )])
    .await;
    let counts = Counts::default();
    let agent = agent(&server, &counts, AgentConfig::default());
    let state = agent.drive(agent.start("hello")).await;
    assert_eq!(state.status, RunStatus::Failed);
    assert!(state.error.unwrap().contains("busy"));
}

#[tokio::test]
async fn stops_at_the_step_limit() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(reply(json!([tool_use("get_time", json!({}))]), "tool_use"))
        .mount(&server)
        .await;
    let counts = Counts::default();
    let config = AgentConfig {
        max_steps: 2,
        ..AgentConfig::default()
    };
    let agent = agent(&server, &counts, config);
    let state = agent.drive(agent.start("loop")).await;
    assert_eq!(state.status, RunStatus::MaxSteps);
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    assert_eq!(counts.get_time.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn decide_requires_a_waiting_run() {
    let server = gateway(vec![]).await;
    let counts = Counts::default();
    let agent = agent(&server, &counts, AgentConfig::default());
    let error = agent
        .decide(agent.start("hello"), &Decision::ApproveAll)
        .await
        .unwrap_err();
    assert_eq!(error, EngineError::NotAwaitingApproval);
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// Prompt caches only pay off if the start of every call is byte-identical:
/// instructions, tool definitions and the first message must not change
/// between the steps of one run.
#[tokio::test]
async fn the_cacheable_prefix_is_identical_across_steps() {
    let server = gateway(vec![
        reply(json!([tool_use("get_time", json!({}))]), "tool_use"),
        reply(json!([{ "type": "text", "text": "noon" }]), "end_turn"),
    ])
    .await;
    let counts = Counts::default();
    let agent = agent(&server, &counts, AgentConfig::default());
    agent.drive(agent.start("what time is it?")).await;

    let first = sent(&server, 0).await;
    let second = sent(&server, 1).await;
    let bytes = |value: &dyn erased::Json| value.bytes();
    assert_eq!(bytes(&first.system), bytes(&second.system));
    assert_eq!(bytes(&first.tools), bytes(&second.tools));
    assert_eq!(bytes(&first.messages[0]), bytes(&second.messages[0]));
}

mod erased {
    pub trait Json {
        fn bytes(&self) -> Vec<u8>;
    }
    impl<T: serde::Serialize> Json for T {
        fn bytes(&self) -> Vec<u8> {
            serde_json::to_vec(self).unwrap()
        }
    }
}

fn gateway_error(status: u16, code: &str) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .set_body_json(json!({ "error": { "code": code, "message": "try later" } }))
}

async fn calls(server: &MockServer) -> usize {
    server.received_requests().await.unwrap().len()
}

fn escalating(from: ModelTier, to: ModelTier) -> AgentConfig {
    AgentConfig {
        tier: from,
        escalate_to: Some(to),
        ..AgentConfig::default()
    }
}

#[tokio::test]
async fn moves_up_a_tier_when_overloaded_and_drops_older_reasoning() {
    let server = gateway(vec![
        reply(
            json!([
                { "type": "reasoning", "text": "", "signature": "sig-small" },
                tool_use("get_time", json!({}))
            ]),
            "tool_use",
        ),
        gateway_error(503, "provider_temporarily_unavailable"),
        reply(json!([{ "type": "text", "text": "noon" }]), "end_turn"),
    ])
    .await;
    let counts = Counts::default();
    let agent = agent(
        &server,
        &counts,
        escalating(ModelTier::Small, ModelTier::Standard),
    );
    let state = agent.drive(agent.start("what time is it?")).await;

    assert_eq!(state.status, RunStatus::Completed);
    assert_eq!(state.tier, Some(ModelTier::Standard));
    let has_reasoning = |request: &CompletionRequest| {
        request.messages[1]
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Reasoning { .. }))
    };
    let failed = sent(&server, 1).await;
    let retried = sent(&server, 2).await;
    assert_eq!(failed.tier, ModelTier::Small);
    assert!(has_reasoning(&failed));
    assert_eq!(retried.tier, ModelTier::Standard);
    assert!(
        !has_reasoning(&retried),
        "reasoning from the smaller model was sent"
    );
    assert!(
        state.messages[1]
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Reasoning { .. })),
        "the saved history must keep the reasoning"
    );
}

#[tokio::test]
async fn never_moves_up_without_permission() {
    let server = gateway(vec![gateway_error(503, "provider_temporarily_unavailable")]).await;
    let counts = Counts::default();
    let agent = agent(&server, &counts, AgentConfig::default());
    let state = agent.drive(agent.start("hello")).await;
    assert_eq!(state.status, RunStatus::Failed);
    assert_eq!(state.tier, None);
    assert_eq!(calls(&server).await, 1);
}

#[tokio::test]
async fn auth_failures_do_not_move_up() {
    let server = gateway(vec![gateway_error(502, "provider_auth_failed")]).await;
    let counts = Counts::default();
    let agent = agent(
        &server,
        &counts,
        escalating(ModelTier::Small, ModelTier::Large),
    );
    let state = agent.drive(agent.start("hello")).await;
    assert_eq!(state.status, RunStatus::Failed);
    assert_eq!(calls(&server).await, 1);
}

#[tokio::test]
async fn stops_moving_up_at_the_configured_limit() {
    let server = gateway(vec![
        gateway_error(429, "provider_rate_limited"),
        gateway_error(503, "provider_temporarily_unavailable"),
    ])
    .await;
    let counts = Counts::default();
    let agent = agent(
        &server,
        &counts,
        escalating(ModelTier::Small, ModelTier::Standard),
    );
    let state = agent.drive(agent.start("hello")).await;
    assert_eq!(state.status, RunStatus::Failed);
    assert_eq!(calls(&server).await, 2);
    assert_eq!(sent(&server, 1).await.tier, ModelTier::Standard);
}
