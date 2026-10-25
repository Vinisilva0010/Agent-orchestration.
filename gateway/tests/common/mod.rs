#![allow(dead_code)]

use std::sync::Arc;

use axum::{Router, body::to_bytes, response::Response};
use gateway::{AppState, app, provider::mock::MockProvider};

pub fn test_app() -> Router {
    app(AppState::new(Arc::new(MockProvider::new())))
}

pub async fn json_body(response: Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
