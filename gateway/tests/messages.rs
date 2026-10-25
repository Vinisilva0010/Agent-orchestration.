mod common;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn post(body: Value) -> axum::response::Response {
    common::test_app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

fn valid_request() -> Value {
    json!({
        "tier": "small",
        "system": [{ "text": "You are a helpful agent.", "cache": true }],
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hello" }] }],
        "max_output_tokens": 256
    })
}

#[tokio::test]
async fn returns_completion_with_usage() {
    let response = post(valid_request()).await;
    assert_eq!(response.status(), StatusCode::OK);

    let json = common::json_body(response).await;
    assert_eq!(json["provider"], "mock");
    assert_eq!(json["stop_reason"], "end_turn");
    assert!(json["usage"]["input_tokens"].as_u64().unwrap() > 0);
    assert!(json["usage"]["cache_write_tokens"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn returns_tool_call_when_requested() {
    let mut body = valid_request();
    body["messages"][0]["content"][0]["text"] = json!("[mock:tool:search]");
    body["tools"] = json!([{
        "name": "search",
        "description": "Search documents",
        "input_schema": { "type": "object" }
    }]);

    let json = common::json_body(post(body).await).await;
    assert_eq!(json["stop_reason"], "tool_use");
    assert_eq!(json["content"][0]["name"], "search");
}

#[tokio::test]
async fn rejects_empty_messages_with_clear_error() {
    let mut body = valid_request();
    body["messages"] = json!([]);

    let response = post(body).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let json = common::json_body(response).await;
    assert_eq!(json["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn rejects_unknown_fields() {
    let mut body = valid_request();
    body["api_key"] = json!("should-never-be-accepted");

    let response = post(body).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
