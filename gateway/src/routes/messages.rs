use axum::{Extension, Json, extract::State};

use crate::{
    AppState,
    provider::{CompletionRequest, CompletionResponse},
    routes::error::ApiError,
    tokens::TaskGrant,
};

/// Sends a provider-neutral completion request to the configured provider on
/// behalf of the task identified by the caller's token.
pub async fn create_message(
    State(state): State<AppState>,
    Extension(grant): Extension<TaskGrant>,
    Json(request): Json<CompletionRequest>,
) -> Result<Json<CompletionResponse>, ApiError> {
    let response = state.provider.complete(&request).await?;
    tracing::info!(
        task_id = %grant.task_id,
        agent_id = %grant.agent_id,
        provider = %response.provider,
        model = %response.model,
        input_tokens = response.usage.input_tokens,
        output_tokens = response.usage.output_tokens,
        cache_read_tokens = response.usage.cache_read_tokens,
        cache_write_tokens = response.usage.cache_write_tokens,
        "completion"
    );
    Ok(Json(response))
}
