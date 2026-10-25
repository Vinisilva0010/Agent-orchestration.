use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};

use crate::{AppState, budget::Account, routes::error::ApiError, tokens::validate_id};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBudgetRequest {
    monthly_budget_micros: u64,
}

#[derive(Debug, Serialize)]
pub struct AgentBudgetResponse {
    agent_id: String,
    monthly_budget_micros: u64,
}

/// Sets the monthly spending limit of one agent.
pub async fn set_agent_budget(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Json(request): Json<AgentBudgetRequest>,
) -> Result<Json<AgentBudgetResponse>, ApiError> {
    validate_id(&agent_id, "agent_id")?;
    state
        .ledger
        .set_agent_monthly_budget(&agent_id, request.monthly_budget_micros)
        .await?;
    tracing::info!(
        agent_id = %agent_id,
        monthly_budget_micros = request.monthly_budget_micros,
        "agent budget set"
    );
    Ok(Json(AgentBudgetResponse {
        agent_id,
        monthly_budget_micros: request.monthly_budget_micros,
    }))
}

/// Workspace spending for the current month.
pub async fn workspace_usage(State(state): State<AppState>) -> Result<Json<Account>, ApiError> {
    Ok(Json(state.ledger.workspace_usage().await?))
}
