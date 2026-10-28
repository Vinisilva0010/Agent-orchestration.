//! Provider keys, agent limits and usage, through the gateway's admin routes.
//! Administrators only. Keys pass through to the gateway and are never stored,
//! logged or returned; who asked is recorded in admin_actions and audited.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, put},
};
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    admin::{actor, require_admin, valid_agent_id},
    gateway_admin::{GatewayAdmin, GatewayAdminError},
    http::{ApiError, AppState, CurrentUser, set_actor},
};

const PROVIDERS: [&str; 3] = ["anthropic", "openai", "gemini"];

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/credentials", get(list_credentials))
        .route(
            "/api/credentials/{provider}",
            put(store_credential).delete(delete_credential),
        )
        .route("/api/agents/{agent_id}/budget", put(set_agent_budget))
        .route("/api/usage", get(usage))
}

impl AppState {
    /// Enables the key, limit and usage routes.
    pub fn with_gateway(mut self, gateway: GatewayAdmin) -> Self {
        self.gateway = Some(Arc::new(gateway));
        self
    }
}

impl ApiError {
    fn gateway_unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "gateway_unavailable",
            message: "the gateway is not configured or not reachable",
        }
    }
}

impl From<GatewayAdminError> for ApiError {
    fn from(error: GatewayAdminError) -> Self {
        match error {
            GatewayAdminError::NotFound => Self::not_found(),
            GatewayAdminError::Rejected => Self::invalid("the gateway rejected the request"),
            GatewayAdminError::Unavailable => Self::gateway_unavailable(),
        }
    }
}

fn gateway(state: &AppState) -> Result<&GatewayAdmin, ApiError> {
    state
        .gateway
        .as_deref()
        .ok_or_else(ApiError::gateway_unavailable)
}

fn check_provider(provider: &str) -> Result<(), ApiError> {
    if PROVIDERS.contains(&provider) {
        Ok(())
    } else {
        Err(ApiError::invalid(
            "provider must be anthropic, openai or gemini",
        ))
    }
}

/// Records who asked for a gateway change. A failure here is logged, not
/// returned: the gateway has already made the change and audited it.
async fn record_action(
    state: &AppState,
    user: &CurrentUser,
    kind: &str,
    target: &str,
    details: Value,
    ok: bool,
) {
    let result = async {
        let mut tx = state.pool.begin().await?;
        set_actor(&mut tx, &actor(user)).await?;
        sqlx::query(
            "INSERT INTO admin_actions (kind, target, details, outcome, requested_by) VALUES ($1, $2, $3::jsonb, $4, $5)",
        )
        .bind(kind)
        .bind(target)
        .bind(details.to_string())
        .bind(if ok { "ok" } else { "failed" })
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }
    .await;
    if let Err(error) = result {
        tracing::error!(error = %error, kind, target, user_id = user.id, "could not record an admin action");
    }
}

async fn list_credentials(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Value>, ApiError> {
    require_admin(&user)?;
    Ok(Json(gateway(&state)?.credentials().await?))
}

/// Deliberately not Debug, so the key can never be printed.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreKey {
    api_key: String,
}

async fn store_credential(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(provider): Path<String>,
    Json(request): Json<StoreKey>,
) -> Result<StatusCode, ApiError> {
    require_admin(&user)?;
    check_provider(&provider)?;
    let api_key = SecretString::from(request.api_key);
    let length = secrecy::ExposeSecret::expose_secret(&api_key)
        .chars()
        .count();
    let shape_ok = (8..=512).contains(&length)
        && !secrecy::ExposeSecret::expose_secret(&api_key)
            .chars()
            .any(|c| c.is_whitespace() || c.is_control());
    if !shape_ok {
        return Err(ApiError::invalid("the key is not valid"));
    }
    let result = gateway(&state)?.store_credential(&provider, &api_key).await;
    record_action(
        &state,
        &user,
        "credential_store",
        &provider,
        json!({}),
        result.is_ok(),
    )
    .await;
    result?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_credential(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(provider): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&user)?;
    check_provider(&provider)?;
    let result = gateway(&state)?.delete_credential(&provider).await;
    record_action(
        &state,
        &user,
        "credential_delete",
        &provider,
        json!({}),
        result.is_ok(),
    )
    .await;
    result?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBudget {
    monthly_budget_micros: u64,
}

async fn set_agent_budget(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(agent_id): Path<String>,
    Json(request): Json<AgentBudget>,
) -> Result<StatusCode, ApiError> {
    require_admin(&user)?;
    if !valid_agent_id(&agent_id) {
        return Err(ApiError::invalid(
            "agent ids are 1 to 128 letters, digits, '-' or '_'",
        ));
    }
    let result = gateway(&state)?
        .set_agent_budget(&agent_id, request.monthly_budget_micros)
        .await;
    let details = json!({ "monthly_budget_micros": request.monthly_budget_micros });
    record_action(
        &state,
        &user,
        "agent_budget_set",
        &agent_id,
        details,
        result.is_ok(),
    )
    .await;
    result?;
    Ok(StatusCode::NO_CONTENT)
}

async fn usage(State(state): State<AppState>, user: CurrentUser) -> Result<Json<Value>, ApiError> {
    require_admin(&user)?;
    Ok(Json(gateway(&state)?.usage().await?))
}
