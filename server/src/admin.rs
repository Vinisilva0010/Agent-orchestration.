//! Administration through the HTTP API: accounts, roles and per-agent
//! permissions (administrators only), and changing one's own password.
//!
//! The last active administrator can never be demoted or disabled, so the
//! workspace cannot lock itself out. Disabling an account ends its sessions
//! at once. Every change is audited with the acting person as the actor.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, patch, put},
};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use crate::{
    accounts::{self, AccountError, Role},
    http::{
        ApiError, AppState, CurrentUser, new_token, record_failure, session_cookie, set_actor,
        token_hash,
    },
};

const ROLE_RULE: &str = "role must be admin, member or viewer";
const LEVEL_RULE: &str = "level must be use or manage";
const AGENT_RULE: &str = "agent ids are 1 to 128 letters, digits, '-' or '_'";

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/users", get(list_users).post(create_user))
        .route("/api/users/{id}", patch(update_user))
        .route(
            "/api/users/{id}/agents/{agent_id}",
            put(grant_agent).delete(revoke_agent),
        )
        .route("/api/me/password", put(change_password))
}

impl ApiError {
    pub(crate) fn forbidden() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "forbidden",
            message: "administrators only",
        }
    }

    pub(crate) fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: "not found",
        }
    }

    pub(crate) fn invalid(message: &'static str) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_request",
            message,
        }
    }

    pub(crate) fn conflict(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code,
            message,
        }
    }
}

impl From<AccountError> for ApiError {
    fn from(error: AccountError) -> Self {
        match &error {
            AccountError::PasswordLength => Self::invalid("passwords must be 15 to 128 characters"),
            AccountError::InvalidEmail => Self::invalid("the email address is not valid"),
            AccountError::InvalidName => {
                Self::invalid("the display name must be 1 to 100 characters")
            }
            AccountError::EmailTaken => {
                Self::conflict("email_taken", "an account with this email already exists")
            }
            other => {
                tracing::error!(error = %other, "account error");
                Self::internal()
            }
        }
    }
}

pub(crate) fn require_admin(user: &CurrentUser) -> Result<(), ApiError> {
    if user.role == "admin" {
        Ok(())
    } else {
        Err(ApiError::forbidden())
    }
}

pub(crate) fn actor(user: &CurrentUser) -> String {
    format!("user:{}", user.id)
}

pub(crate) fn valid_agent_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[derive(Serialize)]
struct UserSummary {
    id: i64,
    email: String,
    display_name: String,
    role: String,
    disabled: bool,
}

async fn list_users(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Vec<UserSummary>>, ApiError> {
    require_admin(&user)?;
    let rows: Vec<(i64, String, String, String, bool)> = sqlx::query_as(
        "SELECT id, email, display_name, role, disabled_at IS NOT NULL FROM users ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, email, display_name, role, disabled)| UserSummary {
                id,
                email,
                display_name,
                role,
                disabled,
            })
            .collect(),
    ))
}

/// Deliberately not Debug, so the password can never be printed.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewUser {
    email: String,
    display_name: String,
    role: String,
    password: String,
}

#[derive(Serialize)]
struct Created {
    id: i64,
}

async fn create_user(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(request): Json<NewUser>,
) -> Result<(StatusCode, Json<Created>), ApiError> {
    require_admin(&user)?;
    let role = Role::parse(&request.role).ok_or_else(|| ApiError::invalid(ROLE_RULE))?;
    let password = SecretString::from(request.password);
    let id = accounts::create_user(
        &state.pool,
        &actor(&user),
        &request.email,
        &request.display_name,
        role,
        &password,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(Created { id })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserChange {
    role: Option<String>,
    disabled: Option<bool>,
}

async fn update_user(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(change): Json<UserChange>,
) -> Result<StatusCode, ApiError> {
    require_admin(&user)?;
    let role = match change.role.as_deref() {
        Some(value) => Some(Role::parse(value).ok_or_else(|| ApiError::invalid(ROLE_RULE))?),
        None => None,
    };
    let mut tx = state.pool.begin().await?;
    set_actor(&mut tx, &actor(&user)).await?;
    // The same lock as the first-administrator bootstrap: two changes at the
    // same time cannot both remove "the other" administrator.
    sqlx::query("SELECT pg_advisory_xact_lock(7291002)")
        .execute(&mut *tx)
        .await?;
    let current: Option<(String, bool)> =
        sqlx::query_as("SELECT role, disabled_at IS NOT NULL FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let (current_role, currently_disabled) = current.ok_or_else(ApiError::not_found)?;
    let new_role: &str = match role {
        Some(role) => role.as_str(),
        None => &current_role,
    };
    let new_disabled = change.disabled.unwrap_or(currently_disabled);

    let was_active_admin = current_role == "admin" && !currently_disabled;
    let stays_active_admin = new_role == "admin" && !new_disabled;
    if was_active_admin && !stays_active_admin {
        let others: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM users WHERE role = 'admin' AND disabled_at IS NULL AND id <> $1",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if others == 0 {
            return Err(ApiError::conflict(
                "last_admin",
                "the workspace needs at least one active administrator",
            ));
        }
    }

    sqlx::query(
        "UPDATE users SET role = $2, \
             disabled_at = CASE WHEN $3 THEN coalesce(disabled_at, now()) ELSE NULL END \
         WHERE id = $1",
    )
    .bind(id)
    .bind(new_role)
    .bind(new_disabled)
    .execute(&mut *tx)
    .await?;
    if new_disabled {
        sqlx::query(
            "UPDATE sessions SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentGrant {
    level: String,
}

async fn grant_agent(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, agent_id)): Path<(i64, String)>,
    Json(grant): Json<AgentGrant>,
) -> Result<StatusCode, ApiError> {
    require_admin(&user)?;
    if !valid_agent_id(&agent_id) {
        return Err(ApiError::invalid(AGENT_RULE));
    }
    if !matches!(grant.level.as_str(), "use" | "manage") {
        return Err(ApiError::invalid(LEVEL_RULE));
    }
    let mut tx = state.pool.begin().await?;
    set_actor(&mut tx, &actor(&user)).await?;
    let result = sqlx::query(
        "INSERT INTO agent_permissions (user_id, agent_id, level) VALUES ($1, $2, $3) \
         ON CONFLICT (user_id, agent_id) DO UPDATE SET level = EXCLUDED.level, granted_at = now()",
    )
    .bind(id)
    .bind(&agent_id)
    .bind(&grant.level)
    .execute(&mut *tx)
    .await;
    match result {
        Ok(_) => {}
        Err(sqlx::Error::Database(error)) if error.is_foreign_key_violation() => {
            return Err(ApiError::not_found());
        }
        Err(error) => return Err(error.into()),
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_agent(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, agent_id)): Path<(i64, String)>,
) -> Result<StatusCode, ApiError> {
    require_admin(&user)?;
    let mut tx = state.pool.begin().await?;
    set_actor(&mut tx, &actor(&user)).await?;
    let removed = sqlx::query("DELETE FROM agent_permissions WHERE user_id = $1 AND agent_id = $2")
        .bind(id)
        .bind(&agent_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if removed == 0 {
        return Err(ApiError::not_found());
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Deliberately not Debug, so the passwords can never be printed.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasswordChange {
    current_password: String,
    new_password: String,
}

/// Needs the current password. Ends every session of the account and opens a
/// new one for this browser, so a stolen session or password stops working.
async fn change_password(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(change): Json<PasswordChange>,
) -> Result<Response, ApiError> {
    let current = SecretString::from(change.current_password);
    let new = SecretString::from(change.new_password);
    accounts::check_password_rules(&new)?;
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
        .bind(user.id)
        .fetch_one(&state.pool)
        .await?;
    let matches = tokio::task::spawn_blocking(move || accounts::verify_password(&current, &hash))
        .await
        .map_err(|_| ApiError::internal())?;
    if !matches {
        record_failure(&state.pool, user.id).await?;
        return Err(ApiError::invalid_credentials());
    }
    let new_hash = tokio::task::spawn_blocking(move || accounts::hash_password(&new))
        .await
        .map_err(|_| ApiError::internal())??;
    let token = new_token()?;

    let mut tx = state.pool.begin().await?;
    set_actor(&mut tx, &actor(&user)).await?;
    sqlx::query("UPDATE users SET password_hash = $2, password_changed_at = now() WHERE id = $1")
        .bind(user.id)
        .bind(&new_hash)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE sessions SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    // Created in the same transaction, so it is not older than the change.
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1, $2, now() + interval '12 hours')",
    )
    .bind(token_hash(&token))
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, session_cookie(&token)?);
    Ok(response)
}
