use std::time::Duration;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};

use crate::{AppState, budget::validate_task_budget, routes::error::ApiError, tokens::DEFAULT_TTL};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueRequest {
    task_id: String,
    agent_id: String,
    /// Required. A task without a budget cannot call any model.
    budget_micros: u64,
    #[serde(default)]
    ttl_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct IssueResponse {
    token: String,
    expires_at: u64,
}

/// Issues a task token. Only callers holding the issuer secret reach this.
pub async fn issue(
    State(state): State<AppState>,
    Json(request): Json<IssueRequest>,
) -> Result<(StatusCode, Json<IssueResponse>), ApiError> {
    validate_task_budget(request.budget_micros)?;
    let ttl = request.ttl_seconds.map_or(DEFAULT_TTL, Duration::from_secs);
    let issued = state
        .tokens
        .issue(&request.task_id, &request.agent_id, ttl)
        .await?;
    state
        .ledger
        .set_task_budget(&request.task_id, request.budget_micros)?;
    tracing::info!(
        task_id = %request.task_id,
        agent_id = %request.agent_id,
        ttl_seconds = ttl.as_secs(),
        budget_micros = request.budget_micros,
        "task token issued"
    );
    Ok((
        StatusCode::CREATED,
        Json(IssueResponse {
            token: issued.token,
            expires_at: issued.expires_at_unix,
        }),
    ))
}

#[derive(Debug, Serialize)]
pub struct RevokeResponse {
    revoked: u64,
}

/// Revokes every token of a task.
pub async fn revoke(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<RevokeResponse>, ApiError> {
    let revoked = state.tokens.revoke_task(&task_id).await?;
    tracing::info!(task_id = %task_id, revoked, "task tokens revoked");
    Ok(Json(RevokeResponse { revoked }))
}
