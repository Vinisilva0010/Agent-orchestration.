mod common;

use axum::http::StatusCode;
use serde_json::json;

const ISSUE: &str = "/internal/tasks/tokens";

#[tokio::test]
async fn issuing_requires_issuer_secret() {
    let app = common::test_app();
    let body = json!({ "task_id": "task-1", "agent_id": "agent-1" });

    let missing = common::send(&app, "POST", ISSUE, None, Some(body.clone())).await;
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

    let wrong = common::send(
        &app,
        "POST",
        ISSUE,
        Some("wrong-secret-0123456789abcdef0123456789"),
        Some(body),
    )
    .await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn task_token_cannot_issue_more_tokens() {
    let app = common::test_app();
    let token = common::issue_token(&app, "task-1").await;

    let response = common::send(
        &app,
        "POST",
        ISSUE,
        Some(&token),
        Some(json!({ "task_id": "task-2", "agent_id": "agent-1" })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn issues_token_with_expiry() {
    let app = common::test_app();
    let response = common::send(
        &app,
        "POST",
        ISSUE,
        Some(common::ISSUER_SECRET),
        Some(json!({ "task_id": "task-1", "agent_id": "agent-1", "budget_micros": 1000, "ttl_seconds": 60 })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let json = common::json_body(response).await;
    assert!(json["token"].as_str().unwrap().starts_with("tsk_"));
    assert!(json["expires_at"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn rejects_invalid_issue_requests() {
    let app = common::test_app();
    let cases = [
        json!({ "task_id": "has space", "agent_id": "agent-1", "budget_micros": 1000 }),
        json!({ "task_id": "task-1", "agent_id": "agent-1", "budget_micros": 1000, "ttl_seconds": 0 }),
        json!({ "task_id": "task-1", "agent_id": "agent-1", "budget_micros": 1000, "ttl_seconds": 3601 }),
    ];
    for body in cases {
        let response =
            common::send(&app, "POST", ISSUE, Some(common::ISSUER_SECRET), Some(body)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    let unknown_field = common::send(
        &app,
        "POST",
        ISSUE,
        Some(common::ISSUER_SECRET),
        Some(json!({ "task_id": "task-1", "agent_id": "agent-1", "budget_micros": 1000, "extra": 1 })),
    )
    .await;
    assert_eq!(unknown_field.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn revoked_token_stops_working() {
    let app = common::test_app();
    let token = common::issue_token(&app, "task-9").await;
    let request = json!({
        "tier": "small",
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hi" }] }],
        "max_output_tokens": 16
    });

    let before = common::send(
        &app,
        "POST",
        "/v1/messages",
        Some(&token),
        Some(request.clone()),
    )
    .await;
    assert_eq!(before.status(), StatusCode::OK);

    let revoke = common::send(
        &app,
        "POST",
        "/internal/tasks/task-9/revoke",
        Some(common::ISSUER_SECRET),
        None,
    )
    .await;
    assert_eq!(revoke.status(), StatusCode::OK);
    assert_eq!(common::json_body(revoke).await["revoked"], 1);

    let after = common::send(&app, "POST", "/v1/messages", Some(&token), Some(request)).await;
    assert_eq!(after.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoking_requires_issuer_secret() {
    let app = common::test_app();
    let token = common::issue_token(&app, "task-1").await;
    let response = common::send(
        &app,
        "POST",
        "/internal/tasks/task-1/revoke",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
