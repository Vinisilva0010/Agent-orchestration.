//! Runs waiting for a person, and people's requests about them.
//!
//! The server never drives a run or calls the gateway: it checks who may do
//! what and records a request that the engine worker carries out. The
//! database allows one waiting request per run.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::http::{ApiError, AppState, CurrentUser, set_actor};

const MAX_APPROVALS: usize = 100;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/runs", get(list_waiting))
        .route("/api/runs/{id}/decision", post(decide))
        .route("/api/runs/{id}/budget", post(raise_budget))
        .route("/api/runs/{id}/cancel", post(cancel))
}

/// Administrators act on every agent. Others need a permission on the agent:
/// 'use' to decide, 'manage' to cancel. Viewers never act.
async fn may_act(
    state: &AppState,
    user: &CurrentUser,
    agent_id: &str,
    manage: bool,
) -> Result<bool, ApiError> {
    match user.role {
        "admin" => return Ok(true),
        "viewer" => return Ok(false),
        _ => {}
    }
    let level: Option<String> = sqlx::query_scalar(
        "SELECT level FROM agent_permissions WHERE user_id = $1 AND agent_id = $2",
    )
    .bind(user.id)
    .bind(agent_id)
    .fetch_optional(&state.pool)
    .await?;
    Ok(match level.as_deref() {
        Some("manage") => true,
        Some("use") => !manage,
        _ => false,
    })
}

struct Run {
    agent_id: String,
    status: String,
    budget_micros: i64,
    pending_ids: Vec<String>,
}

async fn load_run(state: &AppState, run_id: &str) -> Result<Run, ApiError> {
    let row: Option<(String, String, i64, String)> = sqlx::query_as(
        "SELECT agent_id, status, budget_micros, coalesce(state->'pending', '[]'::jsonb)::text \
         FROM engine_runs WHERE id = $1",
    )
    .bind(run_id)
    .fetch_optional(&state.pool)
    .await?;
    let (agent_id, status, budget_micros, pending) = row.ok_or_else(ApiError::not_found)?;
    let pending: Value = serde_json::from_str(&pending).unwrap_or(Value::Null);
    let pending_ids = pending
        .as_array()
        .map(|calls| {
            calls
                .iter()
                .filter_map(|call| call.get("id")?.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(Run {
        agent_id,
        status,
        budget_micros,
        pending_ids,
    })
}

async fn record_request(
    state: &AppState,
    user: &CurrentUser,
    run_id: &str,
    kind: &'static str,
    payload: Value,
) -> Result<StatusCode, ApiError> {
    let mut tx = state.pool.begin().await?;
    set_actor(&mut tx, &format!("user:{}", user.id)).await?;
    let result = sqlx::query(
        "INSERT INTO run_requests (run_id, kind, payload, requested_by) VALUES ($1, $2, $3::jsonb, $4)",
    )
    .bind(run_id)
    .bind(kind)
    .bind(payload.to_string())
    .bind(user.id)
    .execute(&mut *tx)
    .await;
    match result {
        Ok(_) => {}
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
            return Err(ApiError::conflict(
                "request_pending",
                "a request for this run is already waiting for the engine",
            ));
        }
        Err(error) => return Err(error.into()),
    }
    tx.commit().await?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Serialize)]
struct WaitingRun {
    id: String,
    agent_id: String,
    status: String,
    budget_micros: i64,
    cost_micros: Option<i64>,
    pending: Value,
}

async fn list_waiting(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Vec<WaitingRun>>, ApiError> {
    let rows: Vec<(String, String, String, i64, Option<i64>, String)> = sqlx::query_as(
        "SELECT r.id, r.agent_id, r.status, r.budget_micros, (r.state->>'cost_micros')::bigint, \
                coalesce(r.state->'pending', '[]'::jsonb)::text \
         FROM engine_runs r \
         WHERE r.status IN ('awaiting_approval', 'awaiting_budget') \
           AND ($1 OR EXISTS (SELECT 1 FROM agent_permissions p \
                              WHERE p.user_id = $2 AND p.agent_id = r.agent_id)) \
         ORDER BY r.updated_at",
    )
    .bind(user.role == "admin")
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(
                |(id, agent_id, status, budget_micros, cost_micros, pending)| WaitingRun {
                    id,
                    agent_id,
                    status,
                    budget_micros,
                    cost_micros,
                    pending: serde_json::from_str(&pending).unwrap_or(Value::Null),
                },
            )
            .collect(),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRequest {
    /// Ids of the waiting calls to run. Every other waiting call is rejected.
    approve: Vec<String>,
}

async fn decide(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(run_id): Path<String>,
    Json(request): Json<DecisionRequest>,
) -> Result<StatusCode, ApiError> {
    let run = load_run(&state, &run_id).await?;
    if !may_act(&state, &user, &run.agent_id, false).await? {
        return Err(ApiError::forbidden());
    }
    if run.status != "awaiting_approval" {
        return Err(ApiError::conflict(
            "not_waiting",
            "the run is not waiting for approval",
        ));
    }
    if request.approve.len() > MAX_APPROVALS
        || request
            .approve
            .iter()
            .any(|id| !run.pending_ids.contains(id))
    {
        return Err(ApiError::invalid(
            "approve only actions the run is waiting on",
        ));
    }
    record_request(
        &state,
        &user,
        &run_id,
        "decision",
        json!({ "approve": request.approve }),
    )
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetRaise {
    budget_micros: u64,
}

/// Money: administrators only.
async fn raise_budget(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(run_id): Path<String>,
    Json(raise): Json<BudgetRaise>,
) -> Result<StatusCode, ApiError> {
    if user.role != "admin" {
        return Err(ApiError::forbidden());
    }
    let run = load_run(&state, &run_id).await?;
    if run.status != "awaiting_budget" {
        return Err(ApiError::conflict(
            "not_waiting",
            "the run is not waiting for budget",
        ));
    }
    let new_budget = i64::try_from(raise.budget_micros)
        .map_err(|_| ApiError::invalid("the budget is too large"))?;
    if new_budget <= run.budget_micros {
        return Err(ApiError::invalid(
            "the new budget must be higher than the current one",
        ));
    }
    record_request(
        &state,
        &user,
        &run_id,
        "budget",
        json!({ "budget_micros": raise.budget_micros }),
    )
    .await
}

async fn cancel(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(run_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let run = load_run(&state, &run_id).await?;
    if !may_act(&state, &user, &run.agent_id, true).await? {
        return Err(ApiError::forbidden());
    }
    if !matches!(run.status.as_str(), "awaiting_approval" | "awaiting_budget") {
        return Err(ApiError::conflict(
            "not_waiting",
            "only waiting runs can be cancelled",
        ));
    }
    record_request(&state, &user, &run_id, "cancel", json!({})).await
}
