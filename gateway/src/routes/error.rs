use axum::{
    Json,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::{provider::ProviderError, tokens::TokenError};

/// Error returned to callers. Messages never include credentials, tokens or raw
/// provider output.
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    /// Same response for every authentication failure, so callers cannot tell
    /// a missing token from an expired or revoked one.
    pub fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: "unauthorized".to_string(),
        }
    }

    fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message: "internal error".to_string(),
        }
    }
}

impl From<ProviderError> for ApiError {
    fn from(error: ProviderError) -> Self {
        let (status, code) = match &error {
            ProviderError::InvalidRequest(_) => (StatusCode::BAD_REQUEST, "invalid_request"),
            ProviderError::Unavailable(_) => (StatusCode::BAD_GATEWAY, "provider_unavailable"),
        };
        Self {
            status,
            code,
            message: error.to_string(),
        }
    }
}

impl From<TokenError> for ApiError {
    fn from(error: TokenError) -> Self {
        match error {
            TokenError::InvalidId(_) | TokenError::InvalidTtl => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_request",
                message: error.to_string(),
            },
            TokenError::Unauthorized => Self::unauthorized(),
            TokenError::RandomUnavailable | TokenError::StoreUnavailable => Self::internal(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(json!({ "error": { "code": self.code, "message": self.message } }));
        if self.status == StatusCode::UNAUTHORIZED {
            (self.status, [(header::WWW_AUTHENTICATE, "Bearer")], body).into_response()
        } else {
            (self.status, body).into_response()
        }
    }
}
