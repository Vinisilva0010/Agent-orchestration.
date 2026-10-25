use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use secrecy::SecretString;
use serde::Deserialize;

use crate::{
    AppState,
    routes::error::ApiError,
    vault::{CredentialInfo, CredentialProvider, VaultError},
};

/// Request body for storing a key. Deliberately does not derive Debug, so the
/// key can never be printed by accident.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreCredentialRequest {
    api_key: String,
}

fn parse_provider(value: &str) -> Result<CredentialProvider, ApiError> {
    CredentialProvider::parse(value).ok_or_else(|| VaultError::UnknownProvider.into())
}

/// Encrypts and stores a provider key. The key is never returned.
pub async fn store(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(request): Json<StoreCredentialRequest>,
) -> Result<StatusCode, ApiError> {
    let provider = parse_provider(&provider)?;
    let api_key = SecretString::from(request.api_key);
    state.vault.store(provider, &api_key).await?;
    tracing::info!(provider = provider.as_str(), "provider credential stored");
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Result<StatusCode, ApiError> {
    let provider = parse_provider(&provider)?;
    if state.vault.delete(provider).await? {
        tracing::info!(provider = provider.as_str(), "provider credential deleted");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found())
    }
}

/// Lists configured providers. Never includes keys.
pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<CredentialInfo>>, ApiError> {
    Ok(Json(state.vault.list().await?))
}
