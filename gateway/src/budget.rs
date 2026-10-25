//! Spending limits per task, per agent per month and per workspace per month,
//! stored in PostgreSQL.
//!
//! Before a call, the gateway reserves the highest possible cost of that call in
//! every limit, inside one transaction that locks the task, agent and workspace
//! rows in that fixed order. A call that does not fit is rejected before any
//! money is spent. Open reservations count as spending, so parallel calls (even
//! from several gateway instances) cannot exceed a limit together.
//!
//! Each open reservation is a row in `budget_reservations`. Finishing it deletes
//! the row, charges the actual cost, releases the rest and records the call in
//! `usage_events`, all in one transaction. A reservation dropped without being
//! settled (for example when the client disconnects) is released in the
//! background, and reservations left behind by a crash are released by a
//! periodic sweeper.

use std::{sync::Arc, time::Duration};

use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};
use thiserror::Error;
use time::OffsetDateTime;

use crate::{pricing::MICROS_PER_USD, provider::Usage, tokens::TaskGrant};

/// Highest budget a single task can receive: 1000 USD.
pub const MAX_TASK_BUDGET_MICROS: u64 = 1_000 * MICROS_PER_USD;
/// Reservations older than this are considered abandoned and released.
pub const STALE_RESERVATION_AFTER: Duration = Duration::from_secs(15 * 60);
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BudgetError {
    #[error("the task has no budget")]
    NoTaskBudget,
    #[error("the {0} budget would be exceeded")]
    Exceeded(&'static str),
    #[error("budget_micros must be between 1 and 1000000000")]
    InvalidBudget,
    #[error("budget ledger unavailable")]
    Unavailable,
}

/// Validates a task budget before anything is issued.
pub fn validate_task_budget(limit_micros: u64) -> Result<(), BudgetError> {
    if limit_micros == 0 || limit_micros > MAX_TASK_BUDGET_MICROS {
        Err(BudgetError::InvalidBudget)
    } else {
        Ok(())
    }
}

fn current_period() -> String {
    let now = OffsetDateTime::now_utc();
    format!("{:04}-{:02}", now.year(), u8::from(now.month()))
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn fits(limit: i64, spent: i64, reserved: i64, amount: i64) -> bool {
    spent.saturating_add(reserved).saturating_add(amount) <= limit
}

/// Logs the database error and returns a generic one.
fn db_error(error: sqlx::Error) -> BudgetError {
    tracing::error!(error = %error, "budget ledger query failed");
    BudgetError::Unavailable
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Account {
    pub limit_micros: Option<u64>,
    pub spent_micros: u64,
    pub reserved_micros: u64,
    pub period: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOutcome {
    Completed,
    Failed,
}

impl CallOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// What gets written to the append-only usage log when a call finishes.
#[derive(Debug, Clone)]
pub struct UsageRecord {
    pub provider: String,
    pub model: String,
    pub outcome: CallOutcome,
    pub usage: Usage,
    pub cost_micros: u64,
}

struct Finished {
    released: bool,
    task_remaining_micros: Option<u64>,
}

#[derive(Debug)]
pub struct Ledger {
    pool: PgPool,
    workspace_limit_micros: u64,
}

impl Ledger {
    pub fn new(pool: PgPool, workspace_monthly_limit_micros: u64) -> Self {
        Self {
            pool,
            workspace_limit_micros: workspace_monthly_limit_micros,
        }
    }

    pub async fn set_task_budget(
        &self,
        task_id: &str,
        limit_micros: u64,
    ) -> Result<(), BudgetError> {
        validate_task_budget(limit_micros)?;
        sqlx::query(
            "INSERT INTO task_budgets (task_id, limit_micros) VALUES ($1, $2) \
             ON CONFLICT (task_id) DO UPDATE \
             SET limit_micros = EXCLUDED.limit_micros, updated_at = now()",
        )
        .bind(task_id)
        .bind(to_i64(limit_micros))
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    pub async fn set_agent_monthly_budget(
        &self,
        agent_id: &str,
        limit_micros: u64,
    ) -> Result<(), BudgetError> {
        if limit_micros == 0 {
            return Err(BudgetError::InvalidBudget);
        }
        sqlx::query(
            "INSERT INTO agent_budgets (agent_id, monthly_limit_micros) VALUES ($1, $2) \
             ON CONFLICT (agent_id) DO UPDATE \
             SET monthly_limit_micros = EXCLUDED.monthly_limit_micros, updated_at = now()",
        )
        .bind(agent_id)
        .bind(to_i64(limit_micros))
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    /// Reserves `amount_micros` in the task, agent and workspace limits.
    pub async fn reserve(
        self: &Arc<Self>,
        grant: &TaskGrant,
        amount_micros: u64,
    ) -> Result<Reservation, BudgetError> {
        self.reserve_in(grant, amount_micros, &current_period())
            .await
    }

    async fn reserve_in(
        self: &Arc<Self>,
        grant: &TaskGrant,
        amount_micros: u64,
        period: &str,
    ) -> Result<Reservation, BudgetError> {
        let amount = to_i64(amount_micros);
        let mut tx = self.pool.begin().await.map_err(db_error)?;

        let task: Option<(i64, i64, i64)> = sqlx::query_as(
            "SELECT limit_micros, spent_micros, reserved_micros FROM task_budgets \
             WHERE task_id = $1 FOR UPDATE",
        )
        .bind(&grant.task_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
        let (limit, spent, reserved) = task.ok_or(BudgetError::NoTaskBudget)?;
        if !fits(limit, spent, reserved, amount) {
            return Err(BudgetError::Exceeded("task"));
        }

        let agent_limit: Option<i64> = sqlx::query_scalar(
            "SELECT monthly_limit_micros FROM agent_budgets WHERE agent_id = $1",
        )
        .bind(&grant.agent_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
        let (agent_spent, agent_reserved) =
            lock_monthly(&mut tx, "agent", &grant.agent_id, period).await?;
        if let Some(limit) = agent_limit
            && !fits(limit, agent_spent, agent_reserved, amount)
        {
            return Err(BudgetError::Exceeded("agent"));
        }

        let (ws_spent, ws_reserved) = lock_monthly(&mut tx, "workspace", "", period).await?;
        if !fits(
            to_i64(self.workspace_limit_micros),
            ws_spent,
            ws_reserved,
            amount,
        ) {
            return Err(BudgetError::Exceeded("workspace"));
        }

        sqlx::query(
            "UPDATE task_budgets SET reserved_micros = reserved_micros + $2, updated_at = now() \
             WHERE task_id = $1",
        )
        .bind(&grant.task_id)
        .bind(amount)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        for (scope, scope_id) in [("agent", grant.agent_id.as_str()), ("workspace", "")] {
            sqlx::query(
                "UPDATE monthly_spend SET reserved_micros = reserved_micros + $4 \
                 WHERE scope = $1 AND scope_id = $2 AND period = $3",
            )
            .bind(scope)
            .bind(scope_id)
            .bind(period)
            .bind(amount)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        }

        let id: i64 = sqlx::query_scalar(
            "INSERT INTO budget_reservations (task_id, agent_id, period, amount_micros) \
             VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(&grant.task_id)
        .bind(&grant.agent_id)
        .bind(period)
        .bind(amount)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;

        tx.commit().await.map_err(db_error)?;
        Ok(Reservation {
            ledger: Arc::clone(self),
            id,
            finished: false,
        })
    }

    /// Finishes a reservation: charges `record.cost_micros` (zero when there is
    /// no record), releases the rest and logs the call. Finishing an already
    /// finished reservation does nothing.
    async fn finish(&self, id: i64, record: Option<UsageRecord>) -> Result<Finished, BudgetError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;

        let row: Option<(String, String, String, i64)> = sqlx::query_as(
            "DELETE FROM budget_reservations WHERE id = $1 \
             RETURNING task_id, agent_id, period, amount_micros",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
        let Some((task_id, agent_id, period, amount)) = row else {
            return Ok(Finished {
                released: false,
                task_remaining_micros: None,
            });
        };
        let cost = record.as_ref().map_or(0, |r| to_i64(r.cost_micros));

        let task: Option<(i64, i64, i64)> = sqlx::query_as(
            "UPDATE task_budgets \
             SET reserved_micros = GREATEST(reserved_micros - $2, 0), \
                 spent_micros = spent_micros + $3, updated_at = now() \
             WHERE task_id = $1 \
             RETURNING limit_micros, spent_micros, reserved_micros",
        )
        .bind(&task_id)
        .bind(amount)
        .bind(cost)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;

        for (scope, scope_id) in [("agent", agent_id.as_str()), ("workspace", "")] {
            sqlx::query(
                "UPDATE monthly_spend \
                 SET reserved_micros = GREATEST(reserved_micros - $4, 0), \
                     spent_micros = spent_micros + $5 \
                 WHERE scope = $1 AND scope_id = $2 AND period = $3",
            )
            .bind(scope)
            .bind(scope_id)
            .bind(&period)
            .bind(amount)
            .bind(cost)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        }

        if let Some(record) = &record {
            sqlx::query(
                "INSERT INTO usage_events \
                 (task_id, agent_id, provider, model, outcome, input_tokens, output_tokens, \
                  cache_read_tokens, cache_write_tokens, reserved_micros, cost_micros) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            )
            .bind(&task_id)
            .bind(&agent_id)
            .bind(&record.provider)
            .bind(&record.model)
            .bind(record.outcome.as_str())
            .bind(to_i64(record.usage.input_tokens))
            .bind(to_i64(record.usage.output_tokens))
            .bind(to_i64(record.usage.cache_read_tokens))
            .bind(to_i64(record.usage.cache_write_tokens))
            .bind(amount)
            .bind(cost)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        }

        tx.commit().await.map_err(db_error)?;
        Ok(Finished {
            released: true,
            task_remaining_micros: task.map(|(limit, spent, reserved)| {
                to_u64(limit.saturating_sub(spent).saturating_sub(reserved))
            }),
        })
    }

    /// Releases reservations older than `older_than` without charging them.
    pub async fn release_stale_reservations(
        &self,
        older_than: Duration,
    ) -> Result<u64, BudgetError> {
        let seconds = to_i64(older_than.as_secs());
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM budget_reservations WHERE created_at < now() - $1 * interval '1 second'",
        )
        .bind(seconds)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;

        let mut released = 0;
        for id in ids {
            if self.finish(id, None).await?.released {
                released += 1;
            }
        }
        Ok(released)
    }

    /// Runs [`Ledger::release_stale_reservations`] every minute in the background.
    pub fn spawn_stale_reservation_sweeper(self: &Arc<Self>) {
        let ledger = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(SWEEP_INTERVAL);
            loop {
                interval.tick().await;
                match ledger
                    .release_stale_reservations(STALE_RESERVATION_AFTER)
                    .await
                {
                    Ok(0) | Err(_) => {}
                    Ok(released) => {
                        tracing::warn!(released, "released stale budget reservations");
                    }
                }
            }
        });
    }

    /// Workspace spending for the current month.
    pub async fn workspace_usage(&self) -> Result<Account, BudgetError> {
        let period = current_period();
        let row: Option<(i64, i64)> = sqlx::query_as(
            "SELECT spent_micros, reserved_micros FROM monthly_spend \
             WHERE scope = 'workspace' AND scope_id = '' AND period = $1",
        )
        .bind(&period)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?;
        let (spent, reserved) = row.unwrap_or((0, 0));
        Ok(Account {
            limit_micros: Some(self.workspace_limit_micros),
            spent_micros: to_u64(spent),
            reserved_micros: to_u64(reserved),
            period,
        })
    }
}

/// Creates the monthly row if needed and locks it for this transaction.
async fn lock_monthly(
    tx: &mut Transaction<'_, Postgres>,
    scope: &str,
    scope_id: &str,
    period: &str,
) -> Result<(i64, i64), BudgetError> {
    sqlx::query(
        "INSERT INTO monthly_spend (scope, scope_id, period) VALUES ($1, $2, $3) \
         ON CONFLICT DO NOTHING",
    )
    .bind(scope)
    .bind(scope_id)
    .bind(period)
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;

    sqlx::query_as(
        "SELECT spent_micros, reserved_micros FROM monthly_spend \
         WHERE scope = $1 AND scope_id = $2 AND period = $3 FOR UPDATE",
    )
    .bind(scope)
    .bind(scope_id)
    .bind(period)
    .fetch_one(&mut **tx)
    .await
    .map_err(db_error)
}

/// Budget held for one call until it is settled or dropped.
pub struct Reservation {
    ledger: Arc<Ledger>,
    id: i64,
    finished: bool,
}

impl Reservation {
    /// Charges the actual cost, releases the rest and records the call.
    /// Returns the remaining task budget.
    pub async fn settle(mut self, record: UsageRecord) -> Result<Option<u64>, BudgetError> {
        self.finished = true;
        Ok(self
            .ledger
            .finish(self.id, Some(record))
            .await?
            .task_remaining_micros)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let ledger = Arc::clone(&self.ledger);
        let id = self.id;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if ledger.finish(id, None).await.is_err() {
                    tracing::warn!(
                        reservation_id = id,
                        "could not release reservation; the sweeper will"
                    );
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD: &str = "2026-10";

    fn grant(task: &str, agent: &str) -> TaskGrant {
        TaskGrant {
            task_id: task.to_string(),
            agent_id: agent.to_string(),
        }
    }

    fn record(cost: u64) -> UsageRecord {
        UsageRecord {
            provider: "mock".to_string(),
            model: "mock-small".to_string(),
            outcome: CallOutcome::Completed,
            usage: Usage::default(),
            cost_micros: cost,
        }
    }

    async fn workspace(pool: &PgPool, period: &str) -> (i64, i64) {
        sqlx::query_as(
            "SELECT spent_micros, reserved_micros FROM monthly_spend \
             WHERE scope = 'workspace' AND scope_id = '' AND period = $1",
        )
        .bind(period)
        .fetch_optional(pool)
        .await
        .unwrap()
        .unwrap_or((0, 0))
    }

    async fn event_count(pool: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM usage_events")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn rejects_task_without_budget(pool: PgPool) {
        let ledger = Arc::new(Ledger::new(pool, 1_000));
        assert_eq!(
            ledger.reserve_in(&grant("t1", "a1"), 1, PERIOD).await.err(),
            Some(BudgetError::NoTaskBudget)
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn charges_actual_cost_releases_rest_and_records_call(pool: PgPool) {
        let ledger = Arc::new(Ledger::new(pool.clone(), 1_000));
        ledger.set_task_budget("t1", 500).await.unwrap();
        let reservation = ledger
            .reserve_in(&grant("t1", "a1"), 200, PERIOD)
            .await
            .unwrap();
        assert_eq!(workspace(&pool, PERIOD).await, (0, 200));

        assert_eq!(reservation.settle(record(50)).await.unwrap(), Some(450));
        assert_eq!(workspace(&pool, PERIOD).await, (50, 0));
        assert_eq!(event_count(&pool).await, 1);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn dropped_reservation_is_released_without_charge(pool: PgPool) {
        let ledger = Arc::new(Ledger::new(pool.clone(), 1_000));
        ledger.set_task_budget("t1", 500).await.unwrap();
        drop(
            ledger
                .reserve_in(&grant("t1", "a1"), 200, PERIOD)
                .await
                .unwrap(),
        );

        for _ in 0..100 {
            if workspace(&pool, PERIOD).await.1 == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(workspace(&pool, PERIOD).await, (0, 0));
        assert_eq!(event_count(&pool).await, 0);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn parallel_reservations_never_exceed_the_task_budget(pool: PgPool) {
        let ledger = Arc::new(Ledger::new(pool, 1_000_000));
        ledger.set_task_budget("t1", 100).await.unwrap();

        let handles: Vec<_> = (0..10)
            .map(|_| {
                let ledger = Arc::clone(&ledger);
                tokio::spawn(async move { ledger.reserve_in(&grant("t1", "a1"), 20, PERIOD).await })
            })
            .collect();
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.unwrap());
        }

        let accepted = results.iter().filter(|r| r.is_ok()).count();
        let rejected = results
            .iter()
            .filter(|r| matches!(r, Err(BudgetError::Exceeded("task"))))
            .count();
        assert_eq!((accepted, rejected), (5, 5));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn workspace_limit_applies_across_tasks(pool: PgPool) {
        let ledger = Arc::new(Ledger::new(pool, 100));
        ledger.set_task_budget("t1", 1_000).await.unwrap();
        ledger.set_task_budget("t2", 1_000).await.unwrap();
        ledger
            .reserve_in(&grant("t1", "a1"), 80, PERIOD)
            .await
            .unwrap()
            .settle(record(80))
            .await
            .unwrap();
        assert_eq!(
            ledger
                .reserve_in(&grant("t2", "a2"), 30, PERIOD)
                .await
                .err(),
            Some(BudgetError::Exceeded("workspace"))
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn agent_monthly_limit_applies_across_tasks(pool: PgPool) {
        let ledger = Arc::new(Ledger::new(pool, 10_000));
        ledger.set_agent_monthly_budget("a1", 100).await.unwrap();
        ledger.set_task_budget("t1", 1_000).await.unwrap();
        ledger.set_task_budget("t2", 1_000).await.unwrap();
        ledger
            .reserve_in(&grant("t1", "a1"), 90, PERIOD)
            .await
            .unwrap()
            .settle(record(90))
            .await
            .unwrap();
        assert_eq!(
            ledger
                .reserve_in(&grant("t2", "a1"), 20, PERIOD)
                .await
                .err(),
            Some(BudgetError::Exceeded("agent"))
        );
        assert!(
            ledger
                .reserve_in(&grant("t2", "a2"), 20, PERIOD)
                .await
                .is_ok()
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn monthly_limits_reset_in_a_new_month(pool: PgPool) {
        let ledger = Arc::new(Ledger::new(pool, 100));
        ledger.set_task_budget("t1", 1_000).await.unwrap();
        ledger.set_task_budget("t2", 1_000).await.unwrap();
        ledger
            .reserve_in(&grant("t1", "a1"), 80, "2026-10")
            .await
            .unwrap()
            .settle(record(80))
            .await
            .unwrap();
        assert!(
            ledger
                .reserve_in(&grant("t2", "a1"), 30, "2026-10")
                .await
                .is_err()
        );
        assert!(
            ledger
                .reserve_in(&grant("t2", "a1"), 30, "2026-11")
                .await
                .is_ok()
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn task_budget_does_not_reset_monthly(pool: PgPool) {
        let ledger = Arc::new(Ledger::new(pool, 10_000));
        ledger.set_task_budget("t1", 100).await.unwrap();
        ledger
            .reserve_in(&grant("t1", "a1"), 90, "2026-10")
            .await
            .unwrap()
            .settle(record(90))
            .await
            .unwrap();
        assert_eq!(
            ledger
                .reserve_in(&grant("t1", "a1"), 20, "2026-11")
                .await
                .err(),
            Some(BudgetError::Exceeded("task"))
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn sweeper_releases_abandoned_reservations(pool: PgPool) {
        let ledger = Arc::new(Ledger::new(pool.clone(), 1_000));
        ledger.set_task_budget("t1", 500).await.unwrap();
        let reservation = ledger
            .reserve_in(&grant("t1", "a1"), 200, PERIOD)
            .await
            .unwrap();
        // Simulate a crash: the reservation is never settled nor dropped.
        std::mem::forget(reservation);
        sqlx::query("UPDATE budget_reservations SET created_at = now() - interval '1 hour'")
            .execute(&pool)
            .await
            .unwrap();

        let released = ledger
            .release_stale_reservations(STALE_RESERVATION_AFTER)
            .await
            .unwrap();
        assert_eq!(released, 1);
        assert_eq!(workspace(&pool, PERIOD).await, (0, 0));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn rejects_invalid_budgets(pool: PgPool) {
        let ledger = Ledger::new(pool, 1_000);
        assert_eq!(
            ledger.set_task_budget("t1", 0).await,
            Err(BudgetError::InvalidBudget)
        );
        assert_eq!(
            ledger
                .set_task_budget("t1", MAX_TASK_BUDGET_MICROS + 1)
                .await,
            Err(BudgetError::InvalidBudget)
        );
        assert_eq!(
            ledger.set_agent_monthly_budget("a1", 0).await,
            Err(BudgetError::InvalidBudget)
        );
    }
}
