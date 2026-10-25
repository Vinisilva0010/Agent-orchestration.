mod common;

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;

fn valid_request() -> Value {
    json!({
        "tier": "small",
        "system": [{ "text": "You are a helpful agent.", "cache": true }],
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hello" }] }],
        "max_output_tokens": 256
    })
}

#[sqlx::test(migrations = "./migrations")]
async fn rejects_request_without_token(pool: PgPool) {
    let app = common::test_app(pool);
    let response = common::send(&app, "POST", "/v1/messages", None, Some(valid_request())).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        common::json_body(response).await["error"]["code"],
        "unauthorized"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn rejects_unknown_token(pool: PgPool) {
    let app = common::test_app(pool);
    let response = common::send(
        &app,
        "POST",
        "/v1/messages",
        Some("tsk_not_a_real_token"),
        Some(valid_request()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "./migrations")]
async fn rejects_issuer_secret_used_as_task_token(pool: PgPool) {
    let app = common::test_app(pool);
    let response = common::send(
        &app,
        "POST",
        "/v1/messages",
        Some(common::ISSUER_SECRET),
        Some(valid_request()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "./migrations")]
async fn returns_completion_with_valid_token(pool: PgPool) {
    let app = common::test_app(pool);
    let token = common::issue_token(&app, "task-1").await;

    let response = common::send(
        &app,
        "POST",
        "/v1/messages",
        Some(&token),
        Some(valid_request()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let json = common::json_body(response).await;
    assert_eq!(json["provider"], "mock");
    assert_eq!(json["stop_reason"], "end_turn");
    assert!(json["usage"]["cache_write_tokens"].as_u64().unwrap() > 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn returns_tool_call_when_requested(pool: PgPool) {
    let app = common::test_app(pool);
    let token = common::issue_token(&app, "task-1").await;

    let mut body = valid_request();
    body["messages"][0]["content"][0]["text"] = json!("[mock:tool:search]");
    body["tools"] = json!([{
        "name": "search",
        "description": "Search documents",
        "input_schema": { "type": "object" }
    }]);

    let response = common::send(&app, "POST", "/v1/messages", Some(&token), Some(body)).await;
    let json = common::json_body(response).await;
    assert_eq!(json["stop_reason"], "tool_use");
    assert_eq!(json["content"][0]["name"], "search");
}

#[sqlx::test(migrations = "./migrations")]
async fn rejects_empty_messages_with_clear_error(pool: PgPool) {
    let app = common::test_app(pool);
    let token = common::issue_token(&app, "task-1").await;

    let mut body = valid_request();
    body["messages"] = json!([]);

    let response = common::send(&app, "POST", "/v1/messages", Some(&token), Some(body)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        common::json_body(response).await["error"]["code"],
        "invalid_request"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn rejects_unknown_fields(pool: PgPool) {
    let app = common::test_app(pool);
    let token = common::issue_token(&app, "task-1").await;

    let mut body = valid_request();
    body["api_key"] = json!("should-never-be-accepted");

    let response = common::send(&app, "POST", "/v1/messages", Some(&token), Some(body)).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
