mod common;

use axum::http::StatusCode;
use serde_json::json;
use sqlx::PgPool;

const ISSUE: &str = "/internal/tasks/tokens";

#[sqlx::test(migrations = "./migrations")]
async fn issuing_requires_issuer_secret(pool: PgPool) {
    let app = common::test_app(pool);
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

#[sqlx::test(migrations = "./migrations")]
async fn task_token_cannot_issue_more_tokens(pool: PgPool) {
    let app = common::test_app(pool);
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

#[sqlx::test(migrations = "./migrations")]
async fn issues_token_with_expiry(pool: PgPool) {
    let app = common::test_app(pool);
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

#[sqlx::test(migrations = "./migrations")]
async fn rejects_invalid_issue_requests(pool: PgPool) {
    let app = common::test_app(pool);
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

#[sqlx::test(migrations = "./migrations")]
async fn revoked_token_stops_working(pool: PgPool) {
    let app = common::test_app(pool);
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

#[sqlx::test(migrations = "./migrations")]
async fn revoking_requires_issuer_secret(pool: PgPool) {
    let app = common::test_app(pool);
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

fn small_request() -> serde_json::Value {
    json!({
        "tier": "small",
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hi" }] }],
        "max_output_tokens": 16
    })
}

#[sqlx::test(migrations = "./migrations")]
async fn expired_token_is_rejected(pool: PgPool) {
    let app = common::test_app(pool.clone());
    let token = common::issue_token(&app, "task-exp").await;

    sqlx::query(
        "UPDATE task_tokens SET expires_at = now() - interval '1 second' WHERE task_id = 'task-exp'",
    )
    .execute(&pool)
    .await
    .unwrap();

    let response = common::send(
        &app,
        "POST",
        "/v1/messages",
        Some(&token),
        Some(small_request()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "./migrations")]
async fn revoked_tokens_stay_recorded(pool: PgPool) {
    let app = common::test_app(pool.clone());
    common::issue_token(&app, "task-rev").await;
    common::send(
        &app,
        "POST",
        "/internal/tasks/task-rev/revoke",
        Some(common::ISSUER_SECRET),
        None,
    )
    .await;

    let (total, revoked): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(revoked_at) FROM task_tokens WHERE task_id = 'task-rev'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((total, revoked), (1, 1));
}

#[sqlx::test(migrations = "./migrations")]
async fn token_survives_gateway_restart(pool: PgPool) {
    let first = common::test_app(pool.clone());
    let token = common::issue_token(&first, "task-restart").await;
    drop(first);

    let restarted = common::test_app(pool);
    let response = common::send(
        &restarted,
        "POST",
        "/v1/messages",
        Some(&token),
        Some(small_request()),
    )
    .await;
    // Both the token and the task budget survive the restart.
    assert_eq!(response.status(), StatusCode::OK);
}
