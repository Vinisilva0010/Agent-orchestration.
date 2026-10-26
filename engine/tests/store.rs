use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use engine::{
    agent::{Agent, AgentConfig, Decision, RunState, RunStatus, UNKNOWN_OUTCOME},
    gateway::GatewayClient,
    runner::{Runner, RunnerError},
    store::{RunStore, StoreError, migrate},
    tools::{Tool, ToolRegistry, TypedTool},
};
use protocol::{ContentBlock, ModelTier};
use schemars::JsonSchema;
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ArchiveArgs {
    report_id: String,
}

fn agent(server: &MockServer, archived: &Arc<AtomicU32>) -> Agent {
    let calls = Arc::clone(archived);
    let archive = TypedTool::new(
        "archive_report",
        "Archives a report.",
        move |args: ArchiveArgs| {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { Ok(format!("archived {}", args.report_id)) }
        },
    )
    .mark_irreversible();
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(archive)];
    let client = GatewayClient::new(
        &server.uri(),
        SecretString::from("tsk_test-token".to_string()),
    )
    .unwrap();
    Agent::new(
        client,
        ToolRegistry::new(tools).unwrap(),
        AgentConfig::default(),
    )
}

fn reply(content: Value, stop_reason: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "provider": "mock", "model": "mock-standard", "content": content, "stop_reason": stop_reason,
        "usage": { "input_tokens": 1, "output_tokens": 1, "cache_read_tokens": 0, "cache_write_tokens": 0 },
        "cost_micros": 5, "task_budget_remaining_micros": 100
    }))
}

/// A gateway that asks for archive_report, then answers text.
async fn gateway() -> MockServer {
    let server = MockServer::start().await;
    let calls = json!([{ "type": "tool_use", "id": "call_1", "name": "archive_report", "input": { "report_id": "q3" } }]);
    let replies = [
        reply(calls, "tool_use"),
        reply(json!([{ "type": "text", "text": "done" }]), "end_turn"),
    ];
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

/// Creates a run and drives it until it waits for approval.
async fn waiting_run(store: &RunStore, agent: &Agent) -> String {
    let run = store
        .create(
            "demo-agent",
            ModelTier::Standard,
            50_000,
            RunState::new("archive q3"),
        )
        .await
        .unwrap();
    let run = Runner::new(agent, store).drive(run).await.unwrap();
    assert_eq!(run.state.status, RunStatus::AwaitingApproval);
    run.id
}

fn tool_result(state: &RunState) -> (String, bool) {
    match &state.messages[2].content[0] {
        ContentBlock::ToolResult {
            content, is_error, ..
        } => (content.clone(), *is_error),
        other => panic!("expected a tool result, got {other:?}"),
    }
}

#[sqlx::test(migrations = false)]
async fn approval_survives_a_new_process(pool: PgPool) {
    migrate(&pool).await.unwrap();
    let server = gateway().await;
    let archived = Arc::new(AtomicU32::new(0));
    let run_id = {
        let store = RunStore::new(pool.clone());
        waiting_run(&store, &agent(&server, &archived)).await
    };

    // A new store and agent, as a different process would build them.
    let store = RunStore::new(pool);
    let agent = agent(&server, &archived);
    let run = Runner::new(&agent, &store)
        .decide(&run_id, &Decision::ApproveAll)
        .await
        .unwrap();
    assert_eq!(run.state.status, RunStatus::Completed);
    assert_eq!(archived.load(Ordering::SeqCst), 1);
    assert_eq!(store.load(&run_id).await.unwrap().state, run.state);
}

#[sqlx::test(migrations = false)]
async fn concurrent_decisions_run_the_tool_once(pool: PgPool) {
    migrate(&pool).await.unwrap();
    let server = gateway().await;
    let archived = Arc::new(AtomicU32::new(0));
    let store = RunStore::new(pool);
    let agent = agent(&server, &archived);
    let run_id = waiting_run(&store, &agent).await;

    let runner = Runner::new(&agent, &store);
    let (first, second) = tokio::join!(
        runner.decide(&run_id, &Decision::ApproveAll),
        runner.decide(&run_id, &Decision::ApproveAll)
    );
    let outcomes = [first, second];
    assert_eq!(outcomes.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(outcomes.iter().any(|r| matches!(
        r,
        Err(RunnerError::Store(StoreError::Conflict) | RunnerError::Engine(_))
    )));
    assert_eq!(archived.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrations = false)]
async fn an_action_started_before_a_crash_never_runs_again(pool: PgPool) {
    migrate(&pool).await.unwrap();
    let server = gateway().await;
    let archived = Arc::new(AtomicU32::new(0));
    let store = RunStore::new(pool.clone());
    let agent = agent(&server, &archived);
    let run_id = waiting_run(&store, &agent).await;
    sqlx::query(
        "INSERT INTO engine_tool_executions (run_id, call_id, tool, state) \
         VALUES ($1, 'call_1', 'archive_report', 'started')",
    )
    .bind(&run_id)
    .execute(&pool)
    .await
    .unwrap();

    let run = Runner::new(&agent, &store)
        .decide(&run_id, &Decision::ApproveAll)
        .await
        .unwrap();
    assert_eq!(archived.load(Ordering::SeqCst), 0);
    assert_eq!(tool_result(&run.state), (UNKNOWN_OUTCOME.to_string(), true));
}

#[sqlx::test(migrations = false)]
async fn a_finished_action_is_reused_not_repeated(pool: PgPool) {
    migrate(&pool).await.unwrap();
    let server = gateway().await;
    let archived = Arc::new(AtomicU32::new(0));
    let store = RunStore::new(pool.clone());
    let agent = agent(&server, &archived);
    let run_id = waiting_run(&store, &agent).await;
    sqlx::query(
        "INSERT INTO engine_tool_executions (run_id, call_id, tool, state, result, is_error, finished_at) \
         VALUES ($1, 'call_1', 'archive_report', 'finished', 'archived earlier', false, now())",
    )
    .bind(&run_id)
    .execute(&pool)
    .await
    .unwrap();

    let run = Runner::new(&agent, &store)
        .decide(&run_id, &Decision::ApproveAll)
        .await
        .unwrap();
    assert_eq!(archived.load(Ordering::SeqCst), 0);
    assert_eq!(
        tool_result(&run.state),
        ("archived earlier".to_string(), false)
    );
}

#[sqlx::test(migrations = false)]
async fn stale_saves_are_rejected(pool: PgPool) {
    migrate(&pool).await.unwrap();
    let store = RunStore::new(pool);
    let run = store
        .create("demo-agent", ModelTier::Small, 1_000, RunState::new("x"))
        .await
        .unwrap();
    let state = run.state.clone();
    let saved = store.save(run.clone(), state.clone()).await.unwrap();
    assert_eq!(saved.version, 2);
    assert_eq!(
        store.save(run, state).await.unwrap_err(),
        StoreError::Conflict
    );
}
