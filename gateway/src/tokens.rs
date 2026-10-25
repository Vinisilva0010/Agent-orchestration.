//! Task-scoped access tokens, stored in PostgreSQL.
//!
//! A token is 32 random bytes from the operating system, encoded as text with a
//! `tsk_` prefix. Only its SHA-256 hash is stored, so a copy of the database
//! cannot be used to call the gateway. Each token is bound to one task, expires
//! quickly and can be revoked at any time. Revoked rows are kept for auditing.
//! Expiry is checked against the database clock.

use std::time::Duration;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use thiserror::Error;
use time::OffsetDateTime;

pub const DEFAULT_TTL: Duration = Duration::from_secs(15 * 60);
pub const MAX_TTL: Duration = Duration::from_secs(60 * 60);
pub const TOKEN_PREFIX: &str = "tsk_";
const TOKEN_BYTES: usize = 32;
const MAX_ID_LEN: usize = 128;

pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(data));
    out
}

/// What a valid token allows: calls on behalf of one task and one agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskGrant {
    pub task_id: String,
    pub agent_id: String,
}

pub struct IssuedToken {
    pub token: String,
    pub expires_at_unix: u64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TokenError {
    #[error("{0} must be 1 to 128 characters of letters, digits, '-' or '_'")]
    InvalidId(&'static str),
    #[error("ttl_seconds must be between 1 and 3600")]
    InvalidTtl,
    #[error("random number generator unavailable")]
    RandomUnavailable,
    #[error("token store unavailable")]
    StoreUnavailable,
    #[error("unauthorized")]
    Unauthorized,
}

pub(crate) fn validate_id(value: &str, field: &'static str) -> Result<(), TokenError> {
    let valid = !value.is_empty()
        && value.len() <= MAX_ID_LEN
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(TokenError::InvalidId(field))
    }
}

/// Logs the database error and returns a generic one. Query parameters are
/// never logged.
fn store_error(error: sqlx::Error) -> TokenError {
    tracing::error!(error = %error, "token store query failed");
    TokenError::StoreUnavailable
}

#[derive(Debug, Clone)]
pub struct TokenStore {
    pool: PgPool,
}

impl TokenStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn issue(
        &self,
        task_id: &str,
        agent_id: &str,
        ttl: Duration,
    ) -> Result<IssuedToken, TokenError> {
        validate_id(task_id, "task_id")?;
        validate_id(agent_id, "agent_id")?;
        if ttl.is_zero() || ttl > MAX_TTL {
            return Err(TokenError::InvalidTtl);
        }
        let ttl_seconds = i64::try_from(ttl.as_secs()).map_err(|_| TokenError::InvalidTtl)?;

        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes).map_err(|_| TokenError::RandomUnavailable)?;
        let token = format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes));
        let hash = sha256(token.as_bytes());

        let expires_at: OffsetDateTime = sqlx::query_scalar(
            "INSERT INTO task_tokens (token_hash, task_id, agent_id, expires_at) \
             VALUES ($1, $2, $3, now() + $4 * interval '1 second') \
             RETURNING expires_at",
        )
        .bind(&hash[..])
        .bind(task_id)
        .bind(agent_id)
        .bind(ttl_seconds)
        .fetch_one(&self.pool)
        .await
        .map_err(store_error)?;

        Ok(IssuedToken {
            token,
            expires_at_unix: u64::try_from(expires_at.unix_timestamp()).unwrap_or(0),
        })
    }

    /// Returns the grant for a valid token. Missing, malformed, expired and
    /// revoked tokens all produce the same error.
    pub async fn verify(&self, token: &str) -> Result<TaskGrant, TokenError> {
        if !token.starts_with(TOKEN_PREFIX) {
            return Err(TokenError::Unauthorized);
        }
        let hash = sha256(token.as_bytes());
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT task_id, agent_id FROM task_tokens \
             WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now()",
        )
        .bind(&hash[..])
        .fetch_optional(&self.pool)
        .await
        .map_err(store_error)?;

        row.map(|(task_id, agent_id)| TaskGrant { task_id, agent_id })
            .ok_or(TokenError::Unauthorized)
    }

    /// Revokes every active token of a task. Returns how many were revoked.
    pub async fn revoke_task(&self, task_id: &str) -> Result<u64, TokenError> {
        let result = sqlx::query(
            "UPDATE task_tokens SET revoked_at = now() \
             WHERE task_id = $1 AND revoked_at IS NULL",
        )
        .bind(task_id)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_identifiers() {
        assert!(validate_id("task-1_A", "task_id").is_ok());
        let long = "a".repeat(129);
        for bad in ["", "has space", "semi;colon", "quote'", long.as_str()] {
            assert_eq!(
                validate_id(bad, "task_id"),
                Err(TokenError::InvalidId("task_id"))
            );
        }
    }

    #[test]
    fn hashes_are_deterministic_and_fixed_length() {
        assert_eq!(sha256(b"abc"), sha256(b"abc"));
        assert_ne!(sha256(b"abc"), sha256(b"abd"));
        assert_eq!(sha256(b"").len(), 32);
    }
}
