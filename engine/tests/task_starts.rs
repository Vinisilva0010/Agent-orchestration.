//! Task requests recorded by the server become runs exactly once.

use std::sync::Arc;

use engine::{
    agent::{Agent, AgentConfig, RunStatus},
    gateway::GatewayClient,
    runner::Runner,
    store::{AGENT_ARCHIVED, ClaimedTask, RunStore},
    tools::ToolRegistry,
};
use protocol::{ContentBlock, ModelTier};
use secrecy::SecretString;
use serde_json::json;
use sqlx::PgPool;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

/// The server's tables (users, agents, task_starts) come from its migrations,
/// applied after the gateway's and the engine's as `server migrate` does.
async fn migrate_all(pool: &PgPool) {
    let mut gateway = sqlx::migrate!("../gateway/migrations");
    gateway.set_ignore_missing(true);
    gateway.run(pool).await.unwrap();
    let mut engine = sqlx::migrate!("./migrations");
    engine.set_ignore_missing(true);
    engine.run(pool).await.unwrap();
    let mut server = sqlx::migrate!("../server/migrations");
    server.set_ignore_missing(true);
    server.run(pool).await.unwrap();
}

/// A person and two agents ('support' on the large tier, 'old' archived).
async fn workspace(pool: &PgPool) -> i64 {
    migrate_all(pool).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('ao.actor', 'test', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let user: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, display_name, role, password_hash) \
         VALUES ('alice@example.com', 'Alice', 'member', '$argon2id$test') RETURNING id",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, display_name, tier, task_budget_micros, created_by) \
         VALUES ('support', 'Suporte', 'large', 500000, $1), \
                ('old', 'Antigo', 'small', 500000, $1)",
    )
    .bind(user)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("UPDATE agents SET archived_at = now() WHERE id = 'old'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    user
}

async fn request(pool: &PgPool, user: i64, agent: &str, task: &str, budget: i64) -> i64 {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('ao.actor', 'test', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let id = sqlx::query_scalar(
        "INSERT INTO task_starts (agent_id, task, budget_micros, requested_by) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(agent)
    .bind(task)
    .bind(budget)
    .bind(user)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

fn first_text(run_state: &engine::agent::RunState) -> String {
    match &run_state.messages[0].content[0] {
        ContentBlock::Text { text, .. } => text.clone(),
        other => panic!("expected the task text, got {other:?}"),
    }
}

#[sqlx::test(migrations = false)]
async fn each_task_request_starts_exactly_one_run(pool: PgPool) {
    let user = workspace(&pool).await;
    let mut requested = Vec::new();
    for index in 0..6 {
        requested.push(
            request(
                &pool,
                user,
                "support",
                &format!("task {index}"),
                1000 + index,
            )
            .await,
        );
    }

    // Twelve workers race for six requests.
    let store = Arc::new(RunStore::new(pool.clone()));
    let mut workers = Vec::new();
    for _ in 0..12 {
        let store = Arc::clone(&store);
        workers.push(tokio::spawn(async move {
            let mut claimed = Vec::new();
            while let Some(task) = store.claim_task_start().await.unwrap() {
                claimed.push(task);
            }
            claimed
        }));
    }
    let mut started = Vec::new();
    for worker in workers {
        for task in worker.await.unwrap() {
            match task {
                ClaimedTask::Started { task_start_id, run } => started.push((task_start_id, run)),
                ClaimedTask::Refused { reason, .. } => panic!("refused: {reason}"),
            }
        }
    }
    started.sort_by_key(|(id, _)| *id);
    let ids: Vec<i64> = started.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, requested, "every request started once");

    for (index, (task_start_id, run)) in started.iter().enumerate() {
        assert_eq!(run.agent_id, "support");
        assert_eq!(run.tier, ModelTier::Large, "the agent's tier");
        assert_eq!(
            run.budget_micros,
            1000 + index as u64,
            "the request's budget"
        );
        assert_eq!(run.state.status, RunStatus::Running);
        assert_eq!(first_text(&run.state), format!("task {index}"));
        let (run_id, outcome): (Option<String>, Option<String>) =
            sqlx::query_as("SELECT run_id, outcome FROM task_starts WHERE id = $1")
                .bind(task_start_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(run_id.as_deref(), Some(run.id.as_str()));
        assert_eq!(outcome.as_deref(), Some("started"));
    }
    let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM engine_runs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 6);
    assert_eq!(store.claim_task_start().await.unwrap(), None);

    let started_events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = 'task.started'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(started_events, 6);
}

#[sqlx::test(migrations = false)]
async fn a_request_for_an_archived_agent_creates_no_run(pool: PgPool) {
    let user = workspace(&pool).await;
    let id = request(&pool, user, "old", "do something", 1000).await;
    let store = RunStore::new(pool.clone());
    assert_eq!(
        store.claim_task_start().await.unwrap(),
        Some(ClaimedTask::Refused {
            task_start_id: id,
            reason: AGENT_ARCHIVED.to_string()
        })
    );
    assert_eq!(store.claim_task_start().await.unwrap(), None);
    let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM engine_runs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 0);
    let failed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = 'task.failed'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(failed, 1);
}

#[sqlx::test(migrations = false)]
async fn a_started_task_runs_after_the_claim_is_committed(pool: PgPool) {
    let user = workspace(&pool).await;
    request(&pool, user, "support", "say hello", 50_000).await;
    let store = RunStore::new(pool.clone());
    let Some(ClaimedTask::Started { run, .. }) = store.claim_task_start().await.unwrap() else {
        panic!("expected a started task");
    };

    let gateway = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "provider": "mock", "model": "mock-large",
            "content": [{ "type": "text", "text": "hello" }], "stop_reason": "end_turn",
            "usage": { "input_tokens": 1, "output_tokens": 1, "cache_read_tokens": 0, "cache_write_tokens": 0 },
            "cost_micros": 7, "task_budget_remaining_micros": 49_993
        })))
        .mount(&gateway)
        .await;
    let client = GatewayClient::new(
        &gateway.uri(),
        SecretString::from("tsk_test-token".to_string()),
    )
    .unwrap();
    let agent = Agent::new(
        client,
        ToolRegistry::default(),
        AgentConfig {
            tier: run.tier,
            ..AgentConfig::default()
        },
    );
    let finished = Runner::new(&agent, &store).drive(*run).await.unwrap();
    assert_eq!(finished.state.status, RunStatus::Completed);
    assert_eq!(finished.state.final_text, "hello");
    let sent: protocol::CompletionRequest =
        serde_json::from_slice(&gateway.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(sent.tier, ModelTier::Large);
    let (status, cost): (String, i64) = sqlx::query_as(
        "SELECT status, (state->>'cost_micros')::bigint FROM engine_runs WHERE id = $1",
    )
    .bind(&finished.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((status.as_str(), cost), ("completed", 7));
}
