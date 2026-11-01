//! Starting tasks. The server checks that the person may use the agent and
//! records a request; the engine worker claims it, creates the run and runs
//! the agent. The server never creates or drives runs itself.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::post,
};
use serde::{Deserialize, Serialize};

use crate::{
    agents::{MAX_TASK_BUDGET_MICROS, valid_agent_id},
    http::{ApiError, AppState, CurrentUser, set_actor},
};

/// Longest task text, in characters.
pub(crate) const MAX_TASK_CHARS: usize = 8000;
/// Requests one person may have waiting for the engine at once.
pub(crate) const MAX_PENDING_PER_PERSON: i64 = 5;
/// Namespace of the advisory lock that serializes one person's requests.
const PENDING_LOCK: i32 = 7_291_003;

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/agents/{agent_id}/tasks", post(start_task))
}

/// What a person may do with an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// The agent does not exist, or the person may not see it.
    Hidden,
    /// Sees the agent and its runs, acts on nothing.
    View,
    Use,
    Manage,
}

pub(crate) struct AgentAccess {
    pub(crate) access: Access,
    pub(crate) archived: bool,
    pub(crate) task_budget_micros: i64,
}

/// The same rule as the agent list: administrators see every agent; others
/// see agents they hold a permission on, while those are not archived.
/// Viewers never act.
pub(crate) async fn agent_access(
    state: &AppState,
    user: &CurrentUser,
    agent_id: &str,
) -> Result<AgentAccess, ApiError> {
    let hidden = AgentAccess {
        access: Access::Hidden,
        archived: false,
        task_budget_micros: 0,
    };
    if !valid_agent_id(agent_id) {
        return Ok(hidden);
    }
    let row: Option<(bool, i64, Option<String>)> = sqlx::query_as(
        "SELECT a.archived_at IS NOT NULL, a.task_budget_micros, p.level \
         FROM agents a \
         LEFT JOIN agent_permissions p ON p.agent_id = a.id AND p.user_id = $2 \
         WHERE a.id = $1",
    )
    .bind(agent_id)
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await?;
    let Some((archived, task_budget_micros, level)) = row else {
        return Ok(hidden);
    };
    let access = match (user.role, level.as_deref()) {
        ("admin", _) => Access::Manage,
        (_, None) => Access::Hidden,
        _ if archived => Access::Hidden,
        ("viewer", Some(_)) => Access::View,
        (_, Some("manage")) => Access::Manage,
        (_, Some("use")) => Access::Use,
        (_, Some(_)) => Access::Hidden,
    };
    Ok(AgentAccess {
        access,
        archived,
        task_budget_micros,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewTask {
    task: String,
    /// Defaults to the agent's task budget. Only administrators may go above it.
    budget_micros: Option<i64>,
}

#[derive(Serialize)]
struct Requested {
    id: i64,
}

fn clean_task(task: &str) -> Result<String, ApiError> {
    let task = task.trim();
    if task.is_empty() || task.chars().count() > MAX_TASK_CHARS || task.contains('\0') {
        return Err(ApiError::invalid("the task is 1 to 8000 characters"));
    }
    Ok(task.to_string())
}

async fn start_task(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(agent_id): Path<String>,
    Json(request): Json<NewTask>,
) -> Result<(StatusCode, Json<Requested>), ApiError> {
    let agent = agent_access(&state, &user, &agent_id).await?;
    match agent.access {
        Access::Hidden => return Err(ApiError::not_found()),
        Access::View => return Err(ApiError::not_allowed()),
        Access::Use | Access::Manage => {}
    }
    if agent.archived {
        return Err(ApiError::conflict(
            "agent_archived",
            "the agent is archived and takes no new tasks",
        ));
    }
    let task = clean_task(&request.task)?;
    let budget = request.budget_micros.unwrap_or(agent.task_budget_micros);
    if !(1..=MAX_TASK_BUDGET_MICROS).contains(&budget) {
        return Err(ApiError::invalid(
            "the task budget is more than 0 and at most US$ 100",
        ));
    }
    if budget > agent.task_budget_micros && user.role != "admin" {
        return Err(ApiError::forbidden());
    }

    let mut tx = state.pool.begin().await?;
    set_actor(&mut tx, &format!("user:{}", user.id)).await?;
    // Two requests from the same person at once cannot both pass the limit.
    sqlx::query("SELECT pg_advisory_xact_lock($1, ($2 % 2147483647)::int)")
        .bind(PENDING_LOCK)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM task_starts WHERE requested_by = $1 AND processed_at IS NULL",
    )
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;
    if pending >= MAX_PENDING_PER_PERSON {
        return Err(ApiError::too_many_pending());
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO task_starts (agent_id, task, budget_micros, requested_by) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(&agent_id)
    .bind(task)
    .bind(budget)
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(Requested { id })))
}

impl ApiError {
    /// Signed in and allowed to see, but not to act.
    pub(crate) fn not_allowed() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "forbidden",
            message: "you may not do this",
        }
    }

    fn too_many_pending() -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "too_many_pending_tasks",
            message: "wait until your earlier tasks start",
        }
    }
}
