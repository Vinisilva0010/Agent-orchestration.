#![allow(dead_code)]

use std::sync::Arc;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    response::Response,
};
use gateway::{AppState, app, budget::Ledger, provider::mock::MockProvider, tokens::TokenStore};
use secrecy::SecretString;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

pub const ISSUER_SECRET: &str = "test-issuer-secret-0123456789abcdef";
/// Workspace limit used in tests: 100 USD.
pub const WORKSPACE_BUDGET_MICROS: u64 = 100_000_000;
/// Task budget used by `issue_token`: 1 USD.
pub const TASK_BUDGET_MICROS: u64 = 1_000_000;

/// Builds the gateway on a test database created by `sqlx::test`.
pub fn test_app(pool: PgPool) -> Router {
    app(AppState::new(
        Arc::new(MockProvider::new()),
        Arc::new(TokenStore::new(pool)),
        Arc::new(Ledger::new(WORKSPACE_BUDGET_MICROS)),
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
    issue_token_with(app, task_id, "agent-1", TASK_BUDGET_MICROS).await
}

pub async fn issue_token_with(
    app: &Router,
    task_id: &str,
    agent_id: &str,
    budget_micros: u64,
) -> String {
    let response = send(
        app,
        "POST",
        "/internal/tasks/tokens",
        Some(ISSUER_SECRET),
        Some(json!({ "task_id": task_id, "agent_id": agent_id, "budget_micros": budget_micros })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    json_body(response).await["token"]
        .as_str()
        .unwrap()
        .to_string()
}
