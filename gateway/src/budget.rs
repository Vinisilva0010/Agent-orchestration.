//! Spending limits per task, per agent per month and per workspace per month.
//!
//! Before a call, the gateway reserves the highest possible cost of that call in
//! every limit. A call that does not fit is rejected before any money is spent.
//! After the call, the actual cost is charged and the rest of the reservation is
//! released. A reservation dropped without being settled (for example when the
//! provider fails) is released automatically. Because reservations count as
//! spending while they are open, parallel calls cannot exceed a limit together.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use serde::Serialize;
use thiserror::Error;
use time::OffsetDateTime;

use crate::{pricing::MICROS_PER_USD, tokens::TaskGrant};

/// Highest budget a single task can receive: 1000 USD.
pub const MAX_TASK_BUDGET_MICROS: u64 = 1_000 * MICROS_PER_USD;

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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Account {
    pub limit_micros: Option<u64>,
    pub spent_micros: u64,
    pub reserved_micros: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub period: String,
}

impl Account {
    /// Starts a new monthly period. Open reservations are kept.
    fn roll(&mut self, period: &str) {
        if self.period != period {
            self.period = period.to_string();
            self.spent_micros = 0;
        }
    }

    fn fits(&self, amount: u64) -> bool {
        match self.limit_micros {
            None => true,
            Some(limit) => {
                self.spent_micros
                    .saturating_add(self.reserved_micros)
                    .saturating_add(amount)
                    <= limit
            }
        }
    }

    fn remaining(&self) -> Option<u64> {
        self.limit_micros.map(|limit| {
            limit.saturating_sub(self.spent_micros.saturating_add(self.reserved_micros))
        })
    }

    fn reserve(&mut self, amount: u64) {
        self.reserved_micros = self.reserved_micros.saturating_add(amount);
    }

    fn finish(&mut self, reserved: u64, spent: u64) {
        self.reserved_micros = self.reserved_micros.saturating_sub(reserved);
        self.spent_micros = self.spent_micros.saturating_add(spent);
    }
}

#[derive(Debug, Default)]
struct State {
    tasks: HashMap<String, Account>,
    agents: HashMap<String, Account>,
    workspace: Account,
}

#[derive(Debug)]
pub struct Ledger {
    state: Mutex<State>,
}

impl Ledger {
    pub fn new(workspace_monthly_limit_micros: u64) -> Self {
        Self {
            state: Mutex::new(State {
                workspace: Account {
                    limit_micros: Some(workspace_monthly_limit_micros),
                    ..Account::default()
                },
                ..State::default()
            }),
        }
    }

    /// Locks for a decision. Fails closed if the ledger is in a bad state.
    fn lock(&self) -> Result<MutexGuard<'_, State>, BudgetError> {
        self.state.lock().map_err(|_| BudgetError::Unavailable)
    }

    /// Locks for recording spending, which must never be lost.
    fn lock_for_recording(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set_task_budget(&self, task_id: &str, limit_micros: u64) -> Result<(), BudgetError> {
        validate_task_budget(limit_micros)?;
        self.lock()?
            .tasks
            .entry(task_id.to_string())
            .or_default()
            .limit_micros = Some(limit_micros);
        Ok(())
    }

    pub fn set_agent_monthly_budget(
        &self,
        agent_id: &str,
        limit_micros: u64,
    ) -> Result<(), BudgetError> {
        if limit_micros == 0 {
            return Err(BudgetError::InvalidBudget);
        }
        self.lock()?
            .agents
            .entry(agent_id.to_string())
            .or_default()
            .limit_micros = Some(limit_micros);
        Ok(())
    }

    /// Reserves `amount_micros` in the task, agent and workspace limits.
    pub fn reserve(
        self: &Arc<Self>,
        grant: &TaskGrant,
        amount_micros: u64,
    ) -> Result<Reservation, BudgetError> {
        self.reserve_in(grant, amount_micros, &current_period())
    }

    fn reserve_in(
        self: &Arc<Self>,
        grant: &TaskGrant,
        amount: u64,
        period: &str,
    ) -> Result<Reservation, BudgetError> {
        let mut guard = self.lock()?;
        let state = &mut *guard;

        let task = state
            .tasks
            .get(&grant.task_id)
            .filter(|account| account.limit_micros.is_some())
            .ok_or(BudgetError::NoTaskBudget)?;
        if !task.fits(amount) {
            return Err(BudgetError::Exceeded("task"));
        }

        let agent = state.agents.entry(grant.agent_id.clone()).or_default();
        agent.roll(period);
        if !agent.fits(amount) {
            return Err(BudgetError::Exceeded("agent"));
        }

        state.workspace.roll(period);
        if !state.workspace.fits(amount) {
            return Err(BudgetError::Exceeded("workspace"));
        }

        agent.reserve(amount);
        state.workspace.reserve(amount);
        if let Some(task) = state.tasks.get_mut(&grant.task_id) {
            task.reserve(amount);
        }

        Ok(Reservation {
            ledger: Arc::clone(self),
            task_id: grant.task_id.clone(),
            agent_id: grant.agent_id.clone(),
            amount,
            finished: false,
        })
    }

    fn finish(&self, reservation: &Reservation, spent: u64) -> Option<u64> {
        let mut guard = self.lock_for_recording();
        let state = &mut *guard;

        state
            .agents
            .entry(reservation.agent_id.clone())
            .or_default()
            .finish(reservation.amount, spent);
        state.workspace.finish(reservation.amount, spent);

        let task = state.tasks.get_mut(&reservation.task_id)?;
        task.finish(reservation.amount, spent);
        task.remaining()
    }

    /// Workspace spending for the current month.
    pub fn workspace_usage(&self) -> Result<Account, BudgetError> {
        let mut guard = self.lock()?;
        guard.workspace.roll(&current_period());
        Ok(guard.workspace.clone())
    }
}

/// Budget held for one call until it is settled or dropped.
pub struct Reservation {
    ledger: Arc<Ledger>,
    task_id: String,
    agent_id: String,
    amount: u64,
    finished: bool,
}

impl Reservation {
    /// Charges the actual cost and releases the rest of the reservation.
    /// Returns the remaining task budget.
    pub fn settle(mut self, actual_micros: u64) -> Option<u64> {
        self.finished = true;
        self.ledger.finish(&self, actual_micros)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.finished {
            self.ledger.finish(self, 0);
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

    fn ledger(workspace_limit: u64) -> Arc<Ledger> {
        Arc::new(Ledger::new(workspace_limit))
    }

    fn workspace(ledger: &Ledger) -> Account {
        ledger.state.lock().unwrap().workspace.clone()
    }

    #[test]
    fn rejects_task_without_budget() {
        let ledger = ledger(1_000);
        assert_eq!(
            ledger.reserve_in(&grant("t1", "a1"), 1, PERIOD).err(),
            Some(BudgetError::NoTaskBudget)
        );
    }

    #[test]
    fn charges_actual_cost_and_releases_the_rest() {
        let ledger = ledger(1_000);
        ledger.set_task_budget("t1", 500).unwrap();
        let reservation = ledger.reserve_in(&grant("t1", "a1"), 200, PERIOD).unwrap();
        assert_eq!(workspace(&ledger).reserved_micros, 200);

        assert_eq!(reservation.settle(50), Some(450));
        let ws = workspace(&ledger);
        assert_eq!(ws.spent_micros, 50);
        assert_eq!(ws.reserved_micros, 0);
    }

    #[test]
    fn dropped_reservation_is_released_without_charge() {
        let ledger = ledger(1_000);
        ledger.set_task_budget("t1", 500).unwrap();
        drop(ledger.reserve_in(&grant("t1", "a1"), 200, PERIOD).unwrap());
        let ws = workspace(&ledger);
        assert_eq!(ws.spent_micros, 0);
        assert_eq!(ws.reserved_micros, 0);
    }

    #[test]
    fn open_reservations_block_parallel_overspending() {
        let ledger = ledger(1_000);
        ledger.set_task_budget("t1", 100).unwrap();
        let first = ledger.reserve_in(&grant("t1", "a1"), 60, PERIOD).unwrap();
        assert_eq!(
            ledger.reserve_in(&grant("t1", "a1"), 60, PERIOD).err(),
            Some(BudgetError::Exceeded("task"))
        );
        drop(first);
        assert!(ledger.reserve_in(&grant("t1", "a1"), 60, PERIOD).is_ok());
    }

    #[test]
    fn workspace_limit_applies_across_tasks() {
        let ledger = ledger(100);
        ledger.set_task_budget("t1", 1_000).unwrap();
        ledger.set_task_budget("t2", 1_000).unwrap();
        ledger
            .reserve_in(&grant("t1", "a1"), 80, PERIOD)
            .unwrap()
            .settle(80);
        assert_eq!(
            ledger.reserve_in(&grant("t2", "a2"), 30, PERIOD).err(),
            Some(BudgetError::Exceeded("workspace"))
        );
    }

    #[test]
    fn agent_monthly_limit_applies_across_tasks() {
        let ledger = ledger(10_000);
        ledger.set_agent_monthly_budget("a1", 100).unwrap();
        ledger.set_task_budget("t1", 1_000).unwrap();
        ledger.set_task_budget("t2", 1_000).unwrap();
        ledger
            .reserve_in(&grant("t1", "a1"), 90, PERIOD)
            .unwrap()
            .settle(90);
        assert_eq!(
            ledger.reserve_in(&grant("t2", "a1"), 20, PERIOD).err(),
            Some(BudgetError::Exceeded("agent"))
        );
        assert!(ledger.reserve_in(&grant("t2", "a2"), 20, PERIOD).is_ok());
    }

    #[test]
    fn monthly_limits_reset_in_a_new_month() {
        let ledger = ledger(100);
        ledger.set_task_budget("t1", 1_000).unwrap();
        ledger.set_task_budget("t2", 1_000).unwrap();
        ledger
            .reserve_in(&grant("t1", "a1"), 80, "2026-10")
            .unwrap()
            .settle(80);
        assert!(
            ledger
                .reserve_in(&grant("t2", "a1"), 30, "2026-10")
                .is_err()
        );
        assert!(ledger.reserve_in(&grant("t2", "a1"), 30, "2026-11").is_ok());
    }

    #[test]
    fn task_budget_does_not_reset_monthly() {
        let ledger = ledger(10_000);
        ledger.set_task_budget("t1", 100).unwrap();
        ledger
            .reserve_in(&grant("t1", "a1"), 90, "2026-10")
            .unwrap()
            .settle(90);
        assert_eq!(
            ledger.reserve_in(&grant("t1", "a1"), 20, "2026-11").err(),
            Some(BudgetError::Exceeded("task"))
        );
    }

    #[test]
    fn rejects_invalid_budgets() {
        let ledger = ledger(1_000);
        assert_eq!(
            ledger.set_task_budget("t1", 0),
            Err(BudgetError::InvalidBudget)
        );
        assert_eq!(
            ledger.set_task_budget("t1", MAX_TASK_BUDGET_MICROS + 1),
            Err(BudgetError::InvalidBudget)
        );
        assert_eq!(
            ledger.set_agent_monthly_budget("a1", 0),
            Err(BudgetError::InvalidBudget)
        );
    }
}
