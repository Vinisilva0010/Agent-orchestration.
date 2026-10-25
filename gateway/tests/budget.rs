mod common;

use axum::http::StatusCode;
use serde_json::{Value, json};

fn small_request() -> Value {
    json!({
        "tier": "small",
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hello" }] }],
        "max_output_tokens": 64
    })
}

#[tokio::test]
async fn response_reports_cost_and_remaining_task_budget() {
    let app = common::test_app();
    let token = common::issue_token(&app, "task-1").await;

    let response = common::send(
        &app,
        "POST",
        "/v1/messages",
        Some(&token),
        Some(small_request()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let json = common::json_body(response).await;
    let cost = json["cost_micros"].as_u64().unwrap();
    let remaining = json["task_budget_remaining_micros"].as_u64().unwrap();
    assert!(cost > 0);
    assert_eq!(remaining, common::TASK_BUDGET_MICROS - cost);
}

#[tokio::test]
async fn call_that_cannot_fit_the_task_budget_is_rejected() {
    let app = common::test_app();
    let token = common::issue_token_with(&app, "task-tiny", "agent-1", 10).await;

    let response = common::send(
        &app,
        "POST",
        "/v1/messages",
        Some(&token),
        Some(small_request()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
    assert_eq!(
        common::json_body(response).await["error"]["code"],
        "budget_exceeded"
    );
}

#[tokio::test]
async fn task_stops_when_its_budget_runs_out() {
    let app = common::test_app();
    let token = common::issue_token_with(&app, "task-limited", "agent-1", 2_000).await;

    let mut statuses = Vec::new();
    for _ in 0..200 {
        let response = common::send(
            &app,
            "POST",
            "/v1/messages",
            Some(&token),
            Some(small_request()),
        )
        .await;
        statuses.push(response.status());
        if response.status() != StatusCode::OK {
            break;
        }
    }
    assert!(statuses.len() > 1, "at least one call should succeed");
    assert_eq!(statuses.last(), Some(&StatusCode::PAYMENT_REQUIRED));
}

#[tokio::test]
async fn issuing_requires_a_valid_budget() {
    let app = common::test_app();

    let missing = common::send(
        &app,
        "POST",
        "/internal/tasks/tokens",
        Some(common::ISSUER_SECRET),
        Some(json!({ "task_id": "task-1", "agent_id": "agent-1" })),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let zero = common::send(
        &app,
        "POST",
        "/internal/tasks/tokens",
        Some(common::ISSUER_SECRET),
        Some(json!({ "task_id": "task-1", "agent_id": "agent-1", "budget_micros": 0 })),
    )
    .await;
    assert_eq!(zero.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn agent_monthly_budget_is_enforced() {
    let app = common::test_app();
    let set = common::send(
        &app,
        "PUT",
        "/internal/agents/agent-cheap/budget",
        Some(common::ISSUER_SECRET),
        Some(json!({ "monthly_budget_micros": 10 })),
    )
    .await;
    assert_eq!(set.status(), StatusCode::OK);

    let token = common::issue_token_with(&app, "task-a", "agent-cheap", 1_000_000).await;
    let response = common::send(
        &app,
        "POST",
        "/v1/messages",
        Some(&token),
        Some(small_request()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
}

#[tokio::test]
async fn workspace_usage_requires_issuer_and_reports_spending() {
    let app = common::test_app();
    let token = common::issue_token(&app, "task-1").await;
    common::send(
        &app,
        "POST",
        "/v1/messages",
        Some(&token),
        Some(small_request()),
    )
    .await;

    let as_task = common::send(&app, "GET", "/internal/usage", Some(&token), None).await;
    assert_eq!(as_task.status(), StatusCode::UNAUTHORIZED);

    let response = common::send(
        &app,
        "GET",
        "/internal/usage",
        Some(common::ISSUER_SECRET),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = common::json_body(response).await;
    assert_eq!(json["limit_micros"], common::WORKSPACE_BUDGET_MICROS);
    assert!(json["spent_micros"].as_u64().unwrap() > 0);
    assert_eq!(json["reserved_micros"], 0);
}
