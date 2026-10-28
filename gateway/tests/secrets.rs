//! The issuer secret and the admin secret each open only their own routes.

mod common;

use axum::http::StatusCode;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test(migrations = "./migrations")]
async fn each_secret_opens_only_its_own_routes(pool: PgPool) {
    let app = common::test_app(pool);
    let key = json!({ "api_key": "sk-ant-test-0123456789abcdef" });
    let token = json!({ "task_id": "task-1", "agent_id": "agent-1", "budget_micros": 1000 });

    // The issuer secret cannot change keys or limits, or read usage.
    let response = common::send(
        &app,
        "PUT",
        "/internal/credentials/anthropic",
        Some(common::ISSUER_SECRET),
        Some(key),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = common::send(
        &app,
        "PUT",
        "/internal/agents/agent-1/budget",
        Some(common::ISSUER_SECRET),
        Some(json!({ "monthly_limit_usd": "5" })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = common::send(
        &app,
        "GET",
        "/internal/usage",
        Some(common::ISSUER_SECRET),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // The admin secret cannot issue or revoke task tokens, so it cannot spend.
    let response = common::send(
        &app,
        "POST",
        "/internal/tasks/tokens",
        Some(common::ADMIN_SECRET),
        Some(token.clone()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = common::send(
        &app,
        "POST",
        "/internal/tasks/task-1/revoke",
        Some(common::ADMIN_SECRET),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // Each secret works on its own routes.
    let response = common::send(
        &app,
        "POST",
        "/internal/tasks/tokens",
        Some(common::ISSUER_SECRET),
        Some(token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let response = common::send(
        &app,
        "GET",
        "/internal/usage",
        Some(common::ADMIN_SECRET),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}
