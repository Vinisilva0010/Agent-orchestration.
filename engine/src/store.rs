//! PostgreSQL storage for runs and for the at-most-once execution log.
//!
//! Every save checks the version that was read; a stale save is rejected, so
//! two processes cannot both advance the same run.

use std::time::Duration;

use protocol::ModelTier;
use secrecy::{ExposeSecret, SecretString};
use sqlx::{PgPool, postgres::PgPoolOptions};
use thiserror::Error;

use crate::agent::{RunState, ToolCall, ToolOutcome, UNKNOWN_OUTCOME};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("run not found")]
    NotFound,
    #[error("the run was changed by another process; reload it")]
    Conflict,
    #[error("invalid run data: {0}")]
    Invalid(&'static str),
    #[error("database unavailable")]
    Database,
}

fn db_error(error: sqlx::Error) -> StoreError {
    tracing::error!(error = %error, "engine store query failed");
    StoreError::Database
}

/// Opens a connection pool. Migrations are not applied here: they run with
/// the database owner's connection through `server migrate`.
pub async fn connect(database_url: &SecretString) -> Result<PgPool, String> {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url.expose_secret())
        .await
        .map_err(|_| "could not connect to the database".to_string())?;
    Ok(pool)
}

/// Applies the engine migrations. The gateway's migrations live in the same
/// database and are ignored here, as the gateway ignores these.
pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.set_ignore_missing(true);
    migrator.run(pool).await
}

fn tier_from(value: &str) -> Result<ModelTier, StoreError> {
    match value {
        "small" => Ok(ModelTier::Small),
        "standard" => Ok(ModelTier::Standard),
        "large" => Ok(ModelTier::Large),
        _ => Err(StoreError::Invalid("tier")),
    }
}

fn new_run_id() -> Result<String, StoreError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| StoreError::Invalid("random generator"))?;
    Ok(format!(
        "run_{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredRun {
    pub id: String,
    pub agent_id: String,
    pub tier: ModelTier,
    pub budget_micros: u64,
    pub version: i64,
    pub state: RunState,
}

pub struct RunStore {
    pool: PgPool,
}

impl RunStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        agent_id: &str,
        tier: ModelTier,
        budget_micros: u64,
        state: RunState,
    ) -> Result<StoredRun, StoreError> {
        let id = new_run_id()?;
        let budget = i64::try_from(budget_micros).map_err(|_| StoreError::Invalid("budget"))?;
        let json = serde_json::to_string(&state).map_err(|_| StoreError::Invalid("state"))?;
        sqlx::query(
            "INSERT INTO engine_runs (id, agent_id, tier, budget_micros, status, state) \
             VALUES ($1, $2, $3, $4, $5, $6::jsonb)",
        )
        .bind(&id)
        .bind(agent_id)
        .bind(tier.as_str())
        .bind(budget)
        .bind(state.status.as_str())
        .bind(json)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(StoredRun {
            id,
            agent_id: agent_id.to_string(),
            tier,
            budget_micros,
            version: 1,
            state,
        })
    }

    pub async fn load(&self, id: &str) -> Result<StoredRun, StoreError> {
        let row: Option<(String, String, i64, i64, String)> = sqlx::query_as(
            "SELECT agent_id, tier, budget_micros, version, state::text \
             FROM engine_runs WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?;
        let (agent_id, tier, budget, version, state) = row.ok_or(StoreError::NotFound)?;
        Ok(StoredRun {
            id: id.to_string(),
            agent_id,
            tier: tier_from(&tier)?,
            budget_micros: u64::try_from(budget).map_err(|_| StoreError::Invalid("budget"))?,
            version,
            state: serde_json::from_str(&state).map_err(|_| StoreError::Invalid("state"))?,
        })
    }

    /// Saves a new state if the run still has the version that was read.
    pub async fn save(&self, run: StoredRun, state: RunState) -> Result<StoredRun, StoreError> {
        let json = serde_json::to_string(&state).map_err(|_| StoreError::Invalid("state"))?;
        let version: Option<i64> = sqlx::query_scalar(
            "UPDATE engine_runs SET state = $1::jsonb, status = $2, version = version + 1, \
             updated_at = now() WHERE id = $3 AND version = $4 RETURNING version",
        )
        .bind(json)
        .bind(state.status.as_str())
        .bind(&run.id)
        .bind(run.version)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?;
        let version = version.ok_or(StoreError::Conflict)?;
        Ok(StoredRun {
            version,
            state,
            ..run
        })
    }

    /// Records that an irreversible call is starting. Returns the recorded
    /// outcome if the call was started before: the finished result, or an
    /// unknown outcome when an earlier attempt never finished.
    pub async fn begin_execution(
        &self,
        run_id: &str,
        call: &ToolCall,
    ) -> Result<Option<ToolOutcome>, StoreError> {
        let inserted = sqlx::query(
            "INSERT INTO engine_tool_executions (run_id, call_id, tool, state) \
             VALUES ($1, $2, $3, 'started') ON CONFLICT (run_id, call_id) DO NOTHING",
        )
        .bind(run_id)
        .bind(&call.id)
        .bind(&call.name)
        .execute(&self.pool)
        .await
        .map_err(db_error)?
        .rows_affected();
        if inserted == 1 {
            return Ok(None);
        }
        let (state, result, is_error): (String, Option<String>, Option<bool>) = sqlx::query_as(
            "SELECT state, result, is_error FROM engine_tool_executions \
             WHERE run_id = $1 AND call_id = $2",
        )
        .bind(run_id)
        .bind(&call.id)
        .fetch_one(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(Some(match (state.as_str(), result, is_error) {
            ("finished", Some(content), Some(is_error)) => ToolOutcome { content, is_error },
            _ => ToolOutcome {
                content: UNKNOWN_OUTCOME.to_string(),
                is_error: true,
            },
        }))
    }

    pub async fn finish_execution(
        &self,
        run_id: &str,
        call_id: &str,
        outcome: &ToolOutcome,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE engine_tool_executions SET state = 'finished', result = $3, is_error = $4, \
             finished_at = now() WHERE run_id = $1 AND call_id = $2 AND state = 'started'",
        )
        .bind(run_id)
        .bind(call_id)
        .bind(&outcome.content)
        .bind(outcome.is_error)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(())
    }
}
