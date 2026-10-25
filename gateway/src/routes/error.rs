use axum::{
    Json,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::{budget::BudgetError, provider::ProviderError, tokens::TokenError, vault::VaultError};

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

    pub fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: "not found".to_string(),
        }
    }

    fn new(status: StatusCode, code: &'static str, message: String) -> Self {
        Self {
            status,
            code,
            message,
        }
    }

    fn bad_request(message: String) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }

    fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "internal error".to_string(),
        )
    }
}

impl From<ProviderError> for ApiError {
    fn from(error: ProviderError) -> Self {
        let message = error.to_string();
        match error {
            ProviderError::InvalidRequest(_) => Self::bad_request(message),
            ProviderError::Unavailable(_) => {
                Self::new(StatusCode::BAD_GATEWAY, "provider_unavailable", message)
            }
            ProviderError::NotConfigured => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "provider_not_configured",
                message,
            ),
            ProviderError::CredentialRejected => {
                Self::new(StatusCode::BAD_GATEWAY, "provider_auth_failed", message)
            }
            ProviderError::RateLimited => Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                "provider_rate_limited",
                message,
            ),
        }
    }
}

impl From<TokenError> for ApiError {
    fn from(error: TokenError) -> Self {
        match error {
            TokenError::InvalidId(_) | TokenError::InvalidTtl => {
                Self::bad_request(error.to_string())
            }
            TokenError::Unauthorized => Self::unauthorized(),
            TokenError::RandomUnavailable | TokenError::StoreUnavailable => Self::internal(),
        }
    }
}

impl From<BudgetError> for ApiError {
    fn from(error: BudgetError) -> Self {
        match error {
            BudgetError::NoTaskBudget | BudgetError::Exceeded(_) => Self::new(
                StatusCode::PAYMENT_REQUIRED,
                "budget_exceeded",
                error.to_string(),
            ),
            BudgetError::InvalidBudget => Self::bad_request(error.to_string()),
            BudgetError::Unavailable => Self::internal(),
        }
    }
}

impl From<VaultError> for ApiError {
    fn from(error: VaultError) -> Self {
        match error {
            VaultError::InvalidApiKey | VaultError::UnknownProvider => {
                Self::bad_request(error.to_string())
            }
            VaultError::InvalidMasterKey
            | VaultError::Decryption
            | VaultError::Encryption
            | VaultError::RandomUnavailable
            | VaultError::StoreUnavailable => Self::internal(),
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
