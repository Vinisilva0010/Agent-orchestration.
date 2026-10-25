use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::{
    AppState,
    provider::{CompletionRequest, CompletionResponse, ProviderError},
};

/// Sends a provider-neutral completion request to the configured provider.
pub async fn create_message(
    State(state): State<AppState>,
    Json(request): Json<CompletionRequest>,
) -> Result<Json<CompletionResponse>, ApiError> {
    let response = state.provider.complete(&request).await?;
    Ok(Json(response))
}

/// Error returned to callers. Messages never include credentials or raw
/// provider responses.
pub struct ApiError(ProviderError);

impl From<ProviderError> for ApiError {
    fn from(error: ProviderError) -> Self {
        Self(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = match &self.0 {
            ProviderError::InvalidRequest(_) => (StatusCode::BAD_REQUEST, "invalid_request"),
            ProviderError::Unavailable(_) => (StatusCode::BAD_GATEWAY, "provider_unavailable"),
        };
        let body = json!({ "error": { "code": code, "message": self.0.to_string() } });
        (status, Json(body)).into_response()
    }
}
