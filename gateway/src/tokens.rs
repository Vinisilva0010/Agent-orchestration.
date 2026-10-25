//! Task-scoped access tokens.
//!
//! A token is 32 random bytes from the operating system, encoded as text with a
//! `tsk_` prefix. Only its SHA-256 hash is stored, so a copy of the store cannot
//! be used to call the gateway. Each token is bound to one task, expires quickly
//! and can be revoked at any time.

use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const DEFAULT_TTL: Duration = Duration::from_secs(15 * 60);
pub const MAX_TTL: Duration = Duration::from_secs(60 * 60);
pub const TOKEN_PREFIX: &str = "tsk_";
const TOKEN_BYTES: usize = 32;
const MAX_ID_LEN: usize = 128;

type TokenHash = [u8; 32];

pub(crate) fn sha256(data: &[u8]) -> TokenHash {
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

#[derive(Debug)]
struct Entry {
    grant: TaskGrant,
    expires_at: Instant,
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

fn validate_id(value: &str, field: &'static str) -> Result<(), TokenError> {
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

#[derive(Debug, Default)]
pub struct TokenStore {
    entries: Mutex<HashMap<TokenHash, Entry>>,
}

impl TokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<MutexGuard<'_, HashMap<TokenHash, Entry>>, TokenError> {
        self.entries
            .lock()
            .map_err(|_| TokenError::StoreUnavailable)
    }

    pub fn issue(
        &self,
        task_id: &str,
        agent_id: &str,
        ttl: Duration,
    ) -> Result<IssuedToken, TokenError> {
        self.issue_at(task_id, agent_id, ttl, Instant::now())
    }

    fn issue_at(
        &self,
        task_id: &str,
        agent_id: &str,
        ttl: Duration,
        now: Instant,
    ) -> Result<IssuedToken, TokenError> {
        validate_id(task_id, "task_id")?;
        validate_id(agent_id, "agent_id")?;
        if ttl.is_zero() || ttl > MAX_TTL {
            return Err(TokenError::InvalidTtl);
        }

        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes).map_err(|_| TokenError::RandomUnavailable)?;
        let token = format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes));

        let mut entries = self.lock()?;
        entries.retain(|_, entry| entry.expires_at > now);
        entries.insert(
            sha256(token.as_bytes()),
            Entry {
                grant: TaskGrant {
                    task_id: task_id.to_string(),
                    agent_id: agent_id.to_string(),
                },
                expires_at: now + ttl,
            },
        );

        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Ok(IssuedToken {
            token,
            expires_at_unix: now_unix + ttl.as_secs(),
        })
    }

    /// Returns the grant for a valid token. Missing, malformed, expired and
    /// revoked tokens all produce the same error.
    pub fn verify(&self, token: &str) -> Result<TaskGrant, TokenError> {
        self.verify_at(token, Instant::now())
    }

    fn verify_at(&self, token: &str, now: Instant) -> Result<TaskGrant, TokenError> {
        if !token.starts_with(TOKEN_PREFIX) {
            return Err(TokenError::Unauthorized);
        }
        let entries = self.lock()?;
        match entries.get(&sha256(token.as_bytes())) {
            Some(entry) if entry.expires_at > now => Ok(entry.grant.clone()),
            _ => Err(TokenError::Unauthorized),
        }
    }

    /// Revokes every token of a task. Returns how many were revoked.
    pub fn revoke_task(&self, task_id: &str) -> Result<usize, TokenError> {
        let mut entries = self.lock()?;
        let before = entries.len();
        entries.retain(|_, entry| entry.grant.task_id != task_id);
        Ok(before - entries.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);

    #[test]
    fn issues_prefixed_unique_tokens() {
        let store = TokenStore::new();
        let a = store.issue("task-1", "agent-1", MINUTE).unwrap();
        let b = store.issue("task-1", "agent-1", MINUTE).unwrap();
        assert!(a.token.starts_with(TOKEN_PREFIX));
        assert_ne!(a.token, b.token);
    }

    #[test]
    fn verifies_valid_token_and_returns_grant() {
        let store = TokenStore::new();
        let issued = store.issue("task-1", "agent-1", MINUTE).unwrap();
        let grant = store.verify(&issued.token).unwrap();
        assert_eq!(grant.task_id, "task-1");
        assert_eq!(grant.agent_id, "agent-1");
    }

    #[test]
    fn stores_only_the_hash() {
        let store = TokenStore::new();
        let issued = store.issue("task-1", "agent-1", MINUTE).unwrap();
        let entries = store.entries.lock().unwrap();
        assert!(entries.contains_key(&sha256(issued.token.as_bytes())));
        assert!(
            entries
                .keys()
                .all(|k| k.as_slice() != issued.token.as_bytes())
        );
    }

    #[test]
    fn rejects_unknown_and_malformed_tokens() {
        let store = TokenStore::new();
        store.issue("task-1", "agent-1", MINUTE).unwrap();
        assert_eq!(store.verify("tsk_unknown"), Err(TokenError::Unauthorized));
        assert_eq!(store.verify("not-a-token"), Err(TokenError::Unauthorized));
        assert_eq!(store.verify(""), Err(TokenError::Unauthorized));
    }

    #[test]
    fn rejects_expired_token() {
        let store = TokenStore::new();
        let now = Instant::now();
        let issued = store.issue_at("task-1", "agent-1", MINUTE, now).unwrap();
        assert!(
            store
                .verify_at(&issued.token, now + Duration::from_secs(59))
                .is_ok()
        );
        assert_eq!(
            store.verify_at(&issued.token, now + MINUTE),
            Err(TokenError::Unauthorized)
        );
    }

    #[test]
    fn revoke_removes_only_tokens_of_that_task() {
        let store = TokenStore::new();
        let a = store.issue("task-1", "agent-1", MINUTE).unwrap();
        let b = store.issue("task-1", "agent-2", MINUTE).unwrap();
        let other = store.issue("task-2", "agent-1", MINUTE).unwrap();

        assert_eq!(store.revoke_task("task-1").unwrap(), 2);
        assert_eq!(store.verify(&a.token), Err(TokenError::Unauthorized));
        assert_eq!(store.verify(&b.token), Err(TokenError::Unauthorized));
        assert!(store.verify(&other.token).is_ok());
    }

    #[test]
    fn rejects_invalid_ids_and_ttl() {
        let store = TokenStore::new();
        let long = "a".repeat(129);
        for bad in ["", "has space", "semi;colon", long.as_str()] {
            assert_eq!(
                store.issue(bad, "agent-1", MINUTE).err(),
                Some(TokenError::InvalidId("task_id"))
            );
        }
        assert_eq!(
            store.issue("task-1", "agent 1", MINUTE).err(),
            Some(TokenError::InvalidId("agent_id"))
        );
        assert_eq!(
            store.issue("task-1", "agent-1", Duration::ZERO).err(),
            Some(TokenError::InvalidTtl)
        );
        assert_eq!(
            store
                .issue("task-1", "agent-1", MAX_TTL + Duration::from_secs(1))
                .err(),
            Some(TokenError::InvalidTtl)
        );
    }
}
