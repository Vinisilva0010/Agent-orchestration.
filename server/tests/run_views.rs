//! Following tasks: who sees an agent's runs, and what a run shows.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use common::{Setup, as_test, call, create_agent, grant, setup};
use engine::{
    agent::{Agent, AgentConfig, RunState, RunStatus},
    gateway::GatewayClient,
    runner::Runner,
    store::RunStore,
    tools::{Tool, ToolRegistry, TypedTool},
};
use protocol::ModelTier;
use schemars::JsonSchema;
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

const RUN: &str = "run_0123456789abcdef0123456789abcdef";

async fn insert_run(pool: &PgPool, id: &str, agent: &str, status: &str, state: &Value) {
    sqlx::query(
        "INSERT INTO engine_runs (id, agent_id, tier, budget_micros, status, state) \
         VALUES ($1, $2, 'small', 500000, $3, $4::jsonb)",
    )
    .bind(id)
    .bind(agent)
    .bind(status)
    .bind(state.to_string())
    .execute(pool)
    .await
    .unwrap();
}

/// A run waiting on an irreversible action, after a rejected one, a finished
/// irreversible one and a plain tool call.
fn waiting_state() -> Value {
    json!({
        "messages": [
            { "role": "user", "content": [{ "type": "text", "text": "Archive q3 and tell me the time" }] },
            { "role": "assistant", "content": [
                { "type": "reasoning", "text": "PRIVATE CHAIN OF THOUGHT" },
                { "type": "text", "text": "Checking the time first." },
                { "type": "tool_use", "id": "t1", "name": "get_time", "input": {} },
                { "type": "tool_use", "id": "r0", "name": "archive_report", "input": { "report_id": "q2" } },
                { "type": "tool_use", "id": "e0", "name": "archive_report", "input": { "report_id": "q1" } }
            ]},
            { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "t1", "content": "12:00", "is_error": false },
                { "type": "tool_result", "tool_use_id": "r0", "content": "Rejected by a human reviewer.", "is_error": true },
                { "type": "tool_result", "tool_use_id": "e0", "content": "archived q1", "is_error": false }
            ]},
            { "role": "assistant", "content": [
                { "type": "text", "text": "I will archive report q3." },
                { "type": "tool_use", "id": "a1", "name": "archive_report", "input": { "report_id": "q3" } },
                { "type": "tool_use", "id": "t2", "name": "get_time", "input": {} }
            ]}
        ],
        "steps": 2,
        "pending": [
            { "id": "a1", "name": "archive_report", "input": { "report_id": "q3" } },
            { "id": "t2", "name": "get_time", "input": {} }
        ],
        "status": "awaiting_approval",
        "final_text": "",
        "error": null,
        "cost_micros": 1234,
        "approvals": [
            { "call_id": "r0", "tool": "archive_report", "description": "Archives a report.", "irreversible": true, "approved": false },
            { "call_id": "e0", "tool": "archive_report", "description": "Archives a report.", "irreversible": true, "approved": true },
            { "call_id": "a1", "tool": "archive_report", "description": "Archives a report. This cannot be undone.", "irreversible": true, "approved": null }
        ]
    })
}

async fn waiting_run(setup: &Setup, pool: &PgPool) {
    create_agent(setup, "support", 500_000).await;
    insert_run(pool, RUN, "support", "awaiting_approval", &waiting_state()).await;
    as_test(
        pool,
        "INSERT INTO engine_tool_executions (run_id, call_id, tool, state, result, is_error, finished_at) \
         VALUES ('run_0123456789abcdef0123456789abcdef', 'e0', 'archive_report', 'finished', 'archived q1', false, now())",
    )
    .await;
}

#[sqlx::test(migrations = false)]
async fn runs_are_visible_only_to_people_who_see_the_agent(pool: PgPool) {
    let setup = setup(&pool).await;
    waiting_run(&setup, &pool).await;
    let detail = format!("/api/runs/{RUN}");
    let list = "/api/agents/support/runs";

    for uri in [detail.as_str(), list] {
        let (status, _) = call(&setup.app, Method::GET, uri, &setup.bob.cookie, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
    let (_, waiting) = call(
        &setup.app,
        Method::GET,
        "/api/runs",
        &setup.bob.cookie,
        None,
    )
    .await;
    assert_eq!(waiting, json!([]));

    grant(&setup, setup.viewer.id, "support", "use").await;
    let (status, body) = call(&setup.app, Method::GET, &detail, &setup.viewer.cookie, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["can_decide"], false, "viewers see, never act");
    assert_eq!(body["can_cancel"], false);

    grant(&setup, setup.alice.id, "support", "use").await;
    let (_, body) = call(&setup.app, Method::GET, &detail, &setup.alice.cookie, None).await;
    assert_eq!(body["can_decide"], true);
    assert_eq!(body["can_cancel"], false, "cancelling needs manage");
    assert_eq!(
        body["can_raise_budget"], false,
        "money is for administrators"
    );
    grant(&setup, setup.alice.id, "support", "manage").await;
    let (_, body) = call(&setup.app, Method::GET, &detail, &setup.alice.cookie, None).await;
    assert_eq!(body["can_cancel"], true);
    let (_, waiting) = call(
        &setup.app,
        Method::GET,
        "/api/runs",
        &setup.alice.cookie,
        None,
    )
    .await;
    assert_eq!(waiting[0]["id"], RUN);
    assert_eq!(waiting[0]["agent_name"], "Agent support");
    assert_eq!(waiting[0]["can_decide"], true);
    assert_eq!(waiting[0]["approvals"][0]["call_id"], "a1");

    // Archiving hides the agent's runs from everyone but administrators.
    let (status, _) = call(
        &setup.app,
        Method::PATCH,
        "/api/agents/support",
        &setup.admin.cookie,
        Some(json!({ "archived": true })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    for uri in [detail.as_str(), list] {
        let (status, _) = call(&setup.app, Method::GET, uri, &setup.alice.cookie, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        let (status, _) = call(&setup.app, Method::GET, uri, &setup.admin.cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
    }
    let (_, waiting) = call(
        &setup.app,
        Method::GET,
        "/api/runs",
        &setup.alice.cookie,
        None,
    )
    .await;
    assert_eq!(waiting, json!([]));

    for uri in [
        "/api/runs/run_ffffffffffffffffffffffffffffffff",
        "/api/runs/not-a-run",
        "/api/agents/missing/runs",
    ] {
        let (status, _) = call(&setup.app, Method::GET, uri, &setup.admin.cookie, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
}

#[sqlx::test(migrations = false)]
async fn a_run_shows_its_conversation_activity_and_waiting_actions(pool: PgPool) {
    let setup = setup(&pool).await;
    waiting_run(&setup, &pool).await;
    let (status, body) = call(
        &setup.app,
        Method::GET,
        &format!("/api/runs/{RUN}"),
        &setup.admin.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.to_string().contains("PRIVATE CHAIN OF THOUGHT"),
        "reasoning is never shown"
    );
    assert_eq!(body["status"], "awaiting_approval");
    assert_eq!(body["task"], "Archive q3 and tell me the time");
    assert_eq!(body["cost_micros"], 1234);
    assert_eq!(body["budget_micros"], 500_000);
    assert_eq!(body["agent_name"], "Agent support");
    assert_eq!(body["can_decide"], true);
    assert_eq!(body["can_raise_budget"], false);

    assert_eq!(
        body["conversation"],
        json!([
            { "role": "person", "text": "Archive q3 and tell me the time", "tools": [] },
            { "role": "agent", "text": "Checking the time first.", "tools": ["get_time", "archive_report", "archive_report"] },
            { "role": "agent", "text": "I will archive report q3.", "tools": ["archive_report", "get_time"] }
        ])
    );

    // Only the irreversible call waits for a person; the plain call runs on
    // its own after the decision.
    assert_eq!(
        body["approvals"],
        json!([{
            "call_id": "a1",
            "tool": "archive_report",
            "description": "Archives a report. This cannot be undone.",
            "input": { "report_id": "q3" },
            "irreversible": true,
            "reason": "I will archive report q3."
        }])
    );

    let activity = body["activity"].as_array().unwrap();
    let outcome = |id: &str| {
        activity
            .iter()
            .find(|item| item["call_id"] == id)
            .unwrap_or_else(|| panic!("{id} missing"))
            .clone()
    };
    assert_eq!(outcome("t1")["outcome"], "done");
    assert_eq!(outcome("t1")["result"], "12:00");
    assert_eq!(outcome("t1")["irreversible"], false);
    assert_eq!(outcome("r0")["outcome"], "rejected");
    assert_eq!(outcome("r0")["result"], Value::Null);
    assert_eq!(outcome("e0")["outcome"], "done");
    assert_eq!(outcome("e0")["irreversible"], true);
    assert!(
        outcome("e0")["finished_at"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    assert_eq!(outcome("a1")["outcome"], "waiting");
    assert_eq!(outcome("a1")["irreversible"], true);
    assert_eq!(outcome("t2")["outcome"], "waiting");
    assert_eq!(activity.len(), 5);
}

#[sqlx::test(migrations = false)]
async fn the_agent_list_shows_the_queue_and_the_latest_fifty_runs(pool: PgPool) {
    let setup = setup(&pool).await;
    create_agent(&setup, "support", 500_000).await;
    grant(&setup, setup.alice.id, "support", "use").await;
    for index in 0..55 {
        let id = format!("run_{index:032x}");
        let state = json!({
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": format!("cli task {index}") }] }],
            "status": "completed", "cost_micros": index
        });
        insert_run(&pool, &id, "support", "completed", &state).await;
    }
    as_test(
        &pool,
        "UPDATE engine_runs SET created_at = now() - (('x' || substr(id, 30))::bit(28)::int || ' minutes')::interval",
    )
    .await;
    let (status, _) = call(
        &setup.app,
        Method::POST,
        "/api/agents/support/tasks",
        &setup.alice.cookie,
        Some(json!({ "task": "x".repeat(1000) })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (status, list) = call(
        &setup.app,
        Method::GET,
        "/api/agents/support/runs",
        &setup.alice.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 51, "the queued request plus 50 runs");
    assert_eq!(list[0]["status"], "queued");
    assert_eq!(list[0]["run_id"], Value::Null);
    assert_eq!(list[0]["requested_by"], "Alice");
    assert_eq!(list[0]["budget_micros"], 500_000);
    assert_eq!(
        list[0]["task"].as_str().unwrap().chars().count(),
        300,
        "lists carry a short task"
    );
    // Newest run first; runs started outside the workspace show their
    // first message as the task and no requester.
    assert_eq!(list[1]["run_id"], format!("run_{:032x}", 0));
    assert_eq!(list[1]["task"], "cli task 0");
    assert_eq!(list[1]["requested_by"], Value::Null);
    assert_eq!(list[50]["run_id"], format!("run_{:032x}", 49));
    assert_eq!(list[50]["cost_micros"], 49);
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ArchiveArgs {
    report_id: String,
}

/// The engine's saved state and the server's view agree: a call the engine
/// holds for approval is shown as irreversible, with the tool's description.
#[sqlx::test(migrations = false)]
async fn actions_held_by_the_engine_are_shown_as_irreversible(pool: PgPool) {
    let setup = setup(&pool).await;
    create_agent(&setup, "support", 500_000).await;
    let gateway = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "provider": "mock", "model": "mock-small",
            "content": [
                { "type": "text", "text": "Archiving as asked." },
                { "type": "tool_use", "id": "call_1", "name": "archive_report", "input": { "report_id": "q3" } }
            ],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 1, "output_tokens": 1, "cache_read_tokens": 0, "cache_write_tokens": 0 },
            "cost_micros": 9, "task_budget_remaining_micros": 100
        })))
        .mount(&gateway)
        .await;
    let archive = TypedTool::new(
        "archive_report",
        "Archives a report. This cannot be undone.",
        |args: ArchiveArgs| async move { Ok(format!("archived {}", args.report_id)) },
    )
    .mark_irreversible();
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(archive)];
    let agent = Agent::new(
        GatewayClient::new(&gateway.uri(), SecretString::from("tsk_test".to_string())).unwrap(),
        ToolRegistry::new(tools).unwrap(),
        AgentConfig::default(),
    );
    let store = RunStore::new(pool.clone());
    let run = store
        .create(
            "support",
            ModelTier::Small,
            500_000,
            RunState::new("archive q3"),
        )
        .await
        .unwrap();
    let run = Runner::new(&agent, &store).drive(run).await.unwrap();
    assert_eq!(run.state.status, RunStatus::AwaitingApproval);

    let (status, body) = call(
        &setup.app,
        Method::GET,
        &format!("/api/runs/{}", run.id),
        &setup.admin.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["approvals"],
        json!([{
            "call_id": "call_1",
            "tool": "archive_report",
            "description": "Archives a report. This cannot be undone.",
            "input": { "report_id": "q3" },
            "irreversible": true,
            "reason": "Archiving as asked."
        }])
    );
    assert_eq!(body["cost_micros"], 9);
}
