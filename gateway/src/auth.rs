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
    let grant = bearer(request.headers()).and_then(|token| state.tokens.verify(token).ok());
    match grant {
        Some(grant) => {
            request.extensions_mut().insert(grant);
            next.run(request).await
        }
        None => ApiError::unauthorized().into_response(),
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
