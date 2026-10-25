//! Provider gateway: the only component allowed to hold provider API keys.
//!
//! Every model call from the agents process goes through this service with a
//! short-lived, task-scoped token. Keys never leave this process.

pub mod config;
mod routes;

use axum::{Router, extract::DefaultBodyLimit, routing::get};
use tower_http::trace::TraceLayer;

/// Maximum accepted request body. Model requests with long context can be large,
/// but anything above this is rejected before it reaches a handler.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Builds the gateway HTTP application.
///
/// The trace layer logs method and URI only. Headers are never logged because
/// they carry access tokens.
pub fn app() -> Router {
    Router::new()
        .route("/health", get(routes::health::health))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
}
