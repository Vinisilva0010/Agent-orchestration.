//! Request authentication for the two kinds of callers.
//!
//! - Task routes (`/v1/*`) require a valid task token. The grant is attached to
//!   the request so handlers know which task and agent are calling.
//! - Issuer routes (`/internal/*`) require the issuer secret, compared in
//!   constant time against its stored hash.
//!
//! Both checks run before the request body is read.

use axum::{
    extract::{Request, State},
    http::{HeaderMap, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use subtle::ConstantTimeEq;

use crate::{AppState, routes::error::ApiError, tokens::sha256};

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

pub async fn require_task_token(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(token) = bearer(request.headers()).map(str::to_owned) else {
        return ApiError::unauthorized().into_response();
    };
    match state.tokens.verify(&token).await {
        Ok(grant) => {
            request.extensions_mut().insert(grant);
            next.run(request).await
        }
        // Store failures are reported as internal errors and never let the
        // request through.
        Err(error) => ApiError::from(error).into_response(),
    }
}

pub async fn require_issuer(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let authorized = bearer(request.headers()).is_some_and(|presented| {
        bool::from(
            sha256(presented.as_bytes())
                .as_slice()
                .ct_eq(state.issuer_secret_hash.as_slice()),
        )
    });
    if authorized {
        next.run(request).await
    } else {
        ApiError::unauthorized().into_response()
    }
}

/// Key, limit and usage routes require the admin secret, compared in constant
/// time against its stored hash.
pub async fn require_admin(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let authorized = bearer(request.headers()).is_some_and(|presented| {
        bool::from(
            sha256(presented.as_bytes())
                .as_slice()
                .ct_eq(state.admin_secret_hash.as_slice()),
        )
    });
    if authorized {
        next.run(request).await
    } else {
        ApiError::unauthorized().into_response()
    }
}
