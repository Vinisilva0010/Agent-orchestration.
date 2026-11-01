//! The agents people work with: a name and description for each agent id,
//! its model tier and the default budget of one task. Administrators create,
//! change and archive agents. Everyone else sees only the agents they have a
//! permission for, and never archived ones; an agent they may not see answers
//! "not found", so its existence is not revealed.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use serde::{Deserialize, Serialize};

use crate::{
    admin::{actor, require_admin},
    http::{ApiError, AppState, CurrentUser, set_actor},
};

/// US$ 100: the most one task may be given, in millionths of a dollar.
pub(crate) const MAX_TASK_BUDGET_MICROS: i64 = 100_000_000;

const ID_RULE: &str = "agent ids are 1 to 128 letters, digits, '-' or '_'";
const NAME_RULE: &str = "the name is 1 to 100 characters";
const DESCRIPTION_RULE: &str = "the description is at most 500 characters";
const TIER_RULE: &str = "tier must be small, standard or large";
const BUDGET_RULE: &str = "the task budget is more than 0 and at most US$ 100";

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/agents", get(list_agents).post(create_agent))
        .route("/api/agents/{agent_id}", get(get_agent).patch(update_agent))
}

pub(crate) fn valid_agent_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn clean_name(name: &str) -> Result<String, ApiError> {
    let name = name.trim();
    if (1..=100).contains(&name.chars().count()) {
        Ok(name.to_string())
    } else {
        Err(ApiError::invalid(NAME_RULE))
    }
}

fn clean_description(description: &str) -> Result<String, ApiError> {
    let description = description.trim();
    if description.chars().count() <= 500 {
        Ok(description.to_string())
    } else {
        Err(ApiError::invalid(DESCRIPTION_RULE))
    }
}

fn check_tier(tier: &str) -> Result<(), ApiError> {
    if matches!(tier, "small" | "standard" | "large") {
        Ok(())
    } else {
        Err(ApiError::invalid(TIER_RULE))
    }
}

fn check_budget(micros: i64) -> Result<(), ApiError> {
    if (1..=MAX_TASK_BUDGET_MICROS).contains(&micros) {
        Ok(())
    } else {
        Err(ApiError::invalid(BUDGET_RULE))
    }
}

#[derive(Serialize)]
struct AgentSummary {
    id: String,
    display_name: String,
    description: String,
    tier: String,
    task_budget_micros: i64,
    archived: bool,
    /// Tasks waiting for a person: an approval or more budget.
    waiting: i64,
    running: i64,
    /// This month (UTC, as the gateway counts it).
    month_spent_micros: i64,
    month_limit_micros: Option<i64>,
    can_use: bool,
    can_manage: bool,
}

type SummaryRow = (
    String,
    String,
    String,
    String,
    i64,
    bool,
    i64,
    i64,
    i64,
    Option<i64>,
    Option<String>,
);

const SUMMARY_QUERY: &str = "\
    SELECT a.id, a.display_name, a.description, a.tier, a.task_budget_micros, \
           a.archived_at IS NOT NULL, \
           (SELECT count(*) FROM engine_runs r WHERE r.agent_id = a.id \
               AND r.status IN ('awaiting_approval', 'awaiting_budget')), \
           (SELECT count(*) FROM engine_runs r WHERE r.agent_id = a.id AND r.status = 'running'), \
           coalesce((SELECT m.spent_micros FROM monthly_spend m \
               WHERE m.scope = 'agent' AND m.scope_id = a.id \
                 AND m.period = to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM')), 0), \
           (SELECT b.monthly_limit_micros FROM agent_budgets b WHERE b.agent_id = a.id), \
           p.level \
    FROM agents a \
    LEFT JOIN agent_permissions p ON p.agent_id = a.id AND p.user_id = $2 \
    WHERE ($1 OR (p.user_id IS NOT NULL AND a.archived_at IS NULL)) \
      AND ($3::text IS NULL OR a.id = $3) \
    ORDER BY lower(a.display_name), a.id";

async fn visible(
    state: &AppState,
    user: &CurrentUser,
    only: Option<&str>,
) -> Result<Vec<AgentSummary>, ApiError> {
    let rows: Vec<SummaryRow> = sqlx::query_as(SUMMARY_QUERY)
        .bind(user.role == "admin")
        .bind(user.id)
        .bind(only)
        .fetch_all(&state.pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                display_name,
                description,
                tier,
                task_budget_micros,
                archived,
                waiting,
                running,
                month_spent_micros,
                month_limit_micros,
                level,
            )| {
                let (may_use, can_manage) = match (user.role, level.as_deref()) {
                    ("admin", _) => (true, true),
                    ("viewer", _) => (false, false),
                    (_, Some("manage")) => (true, true),
                    (_, Some("use")) => (true, false),
                    _ => (false, false),
                };
                AgentSummary {
                    id,
                    display_name,
                    description,
                    tier,
                    task_budget_micros,
                    archived,
                    waiting,
                    running,
                    month_spent_micros,
                    month_limit_micros,
                    can_use: may_use && !archived,
                    can_manage,
                }
            },
        )
        .collect())
}

async fn list_agents(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Vec<AgentSummary>>, ApiError> {
    Ok(Json(visible(&state, &user, None).await?))
}

async fn get_agent(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(agent_id): Path<String>,
) -> Result<Json<AgentSummary>, ApiError> {
    if !valid_agent_id(&agent_id) {
        return Err(ApiError::not_found());
    }
    visible(&state, &user, Some(&agent_id))
        .await?
        .pop()
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewAgent {
    id: String,
    display_name: String,
    #[serde(default)]
    description: String,
    tier: Option<String>,
    task_budget_micros: i64,
}

#[derive(Serialize)]
struct Created {
    id: String,
}

async fn create_agent(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(request): Json<NewAgent>,
) -> Result<(StatusCode, Json<Created>), ApiError> {
    require_admin(&user)?;
    if !valid_agent_id(&request.id) {
        return Err(ApiError::invalid(ID_RULE));
    }
    let display_name = clean_name(&request.display_name)?;
    let description = clean_description(&request.description)?;
    let tier = request.tier.unwrap_or_else(|| "standard".to_string());
    check_tier(&tier)?;
    check_budget(request.task_budget_micros)?;

    let mut tx = state.pool.begin().await?;
    set_actor(&mut tx, &actor(&user)).await?;
    let result = sqlx::query(
        "INSERT INTO agents (id, display_name, description, tier, task_budget_micros, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&request.id)
    .bind(display_name)
    .bind(description)
    .bind(tier)
    .bind(request.task_budget_micros)
    .bind(user.id)
    .execute(&mut *tx)
    .await;
    match result {
        Ok(_) => {}
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
            return Err(ApiError::conflict(
                "agent_exists",
                "an agent with this id already exists",
            ));
        }
        Err(error) => return Err(error.into()),
    }
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(Created { id: request.id })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentChange {
    display_name: Option<String>,
    description: Option<String>,
    tier: Option<String>,
    task_budget_micros: Option<i64>,
    archived: Option<bool>,
}

async fn update_agent(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(agent_id): Path<String>,
    Json(change): Json<AgentChange>,
) -> Result<StatusCode, ApiError> {
    require_admin(&user)?;
    let display_name = change.display_name.as_deref().map(clean_name).transpose()?;
    let description = change
        .description
        .as_deref()
        .map(clean_description)
        .transpose()?;
    if let Some(tier) = &change.tier {
        check_tier(tier)?;
    }
    if let Some(micros) = change.task_budget_micros {
        check_budget(micros)?;
    }
    if display_name.is_none()
        && description.is_none()
        && change.tier.is_none()
        && change.task_budget_micros.is_none()
        && change.archived.is_none()
    {
        return Err(ApiError::invalid("nothing to change"));
    }

    let mut tx = state.pool.begin().await?;
    set_actor(&mut tx, &actor(&user)).await?;
    let updated = sqlx::query(
        "UPDATE agents SET \
             display_name = coalesce($2, display_name), \
             description = coalesce($3, description), \
             tier = coalesce($4, tier), \
             task_budget_micros = coalesce($5, task_budget_micros), \
             archived_at = CASE WHEN $6::boolean IS NULL THEN archived_at \
                                WHEN $6::boolean THEN coalesce(archived_at, now()) \
                                ELSE NULL END, \
             updated_at = now() \
         WHERE id = $1",
    )
    .bind(&agent_id)
    .bind(display_name)
    .bind(description)
    .bind(change.tier)
    .bind(change.task_budget_micros)
    .bind(change.archived)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
