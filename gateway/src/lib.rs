//! Provider gateway: the only component allowed to hold provider API keys.
//!
//! Every model call from the agents process goes through this service with a
//! short-lived, task-scoped token. Keys never leave this process.

mod auth;
pub mod budget;
pub mod config;
pub mod db;
pub mod pricing;
pub mod provider;
mod routes;
pub mod tokens;

use std::sync::Arc;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post, put},
};
use budget::Ledger;
use provider::Provider;
use secrecy::{ExposeSecret, SecretString};
use tokens::TokenStore;
use tower_http::trace::TraceLayer;

/// Maximum accepted request body. Model requests with long context can be large,
/// but anything above this is rejected before it reaches a handler.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Shared state for request handlers.
#[derive(Clone)]
pub struct AppState {
    pub provider: Arc<dyn Provider>,
    pub tokens: Arc<TokenStore>,
    pub ledger: Arc<Ledger>,
    /// Hash of the issuer secret. The plain secret is not kept after startup.
    issuer_secret_hash: [u8; 32],
}

impl AppState {
    pub fn new(
        provider: Arc<dyn Provider>,
        tokens: Arc<TokenStore>,
        ledger: Arc<Ledger>,
        issuer_secret: &SecretString,
    ) -> Self {
        Self {
            provider,
            tokens,
            ledger,
            issuer_secret_hash: tokens::sha256(issuer_secret.expose_secret().as_bytes()),
        }
    }
}

/// Builds the gateway HTTP application.
///
/// The trace layer logs method and URI only. Headers are never logged because
/// they carry access tokens and the issuer secret.
pub fn app(state: AppState) -> Router {
    let task_routes = Router::new()
        .route("/v1/messages", post(routes::messages::create_message))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_task_token,
        ));

    let issuer_routes = Router::new()
        .route("/internal/tasks/tokens", post(routes::tokens::issue))
        .route(
            "/internal/tasks/{task_id}/revoke",
            post(routes::tokens::revoke),
        )
        .route(
            "/internal/agents/{agent_id}/budget",
            put(routes::budget::set_agent_budget),
        )
        .route("/internal/usage", get(routes::budget::workspace_usage))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_issuer,
        ));

    Router::new()
        .route("/health", get(routes::health::health))
        .merge(task_routes)
        .merge(issuer_routes)
        .with_state(state)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
}
