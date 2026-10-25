#![allow(dead_code)]

use std::sync::Arc;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    response::Response,
};
use gateway::{AppState, app, provider::mock::MockProvider, tokens::TokenStore};
use secrecy::SecretString;
use serde_json::{Value, json};
use tower::ServiceExt;

pub const ISSUER_SECRET: &str = "test-issuer-secret-0123456789abcdef";

pub fn test_app() -> Router {
    app(AppState::new(
        Arc::new(MockProvider::new()),
        Arc::new(TokenStore::new()),
        &SecretString::from(ISSUER_SECRET.to_string()),
    ))
}

pub async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> Response {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let body = match body {
        Some(value) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    app.clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap()
}

pub async fn json_body(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

pub async fn issue_token(app: &Router, task_id: &str) -> String {
    let response = send(
        app,
        "POST",
        "/internal/tasks/tokens",
        Some(ISSUER_SECRET),
        Some(json!({ "task_id": task_id, "agent_id": "agent-1" })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    json_body(response).await["token"]
        .as_str()
        .unwrap()
        .to_string()
}
