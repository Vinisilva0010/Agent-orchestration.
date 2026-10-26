//! Drives runs with durable state: the run is saved after every step, and
//! irreversible tools go through the store's at-most-once execution log.

use thiserror::Error;

use crate::{
    agent::{Agent, Decision, EngineError, ExecutionGuard, RunStatus, ToolCall, ToolOutcome},
    store::{RunStore, StoreError, StoredRun},
};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RunnerError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Engine(#[from] EngineError),
}

struct StoreGuard<'a> {
    store: &'a RunStore,
    run_id: String,
}

impl ExecutionGuard for StoreGuard<'_> {
    async fn begin(&self, call: &ToolCall) -> Result<Option<ToolOutcome>, String> {
        self.store
            .begin_execution(&self.run_id, call)
            .await
            .map_err(|error| error.to_string())
    }

    async fn finish(&self, call: &ToolCall, outcome: &ToolOutcome) -> Result<(), String> {
        self.store
            .finish_execution(&self.run_id, &call.id, outcome)
            .await
            .map_err(|error| error.to_string())
    }
}

pub struct Runner<'a> {
    agent: &'a Agent,
    store: &'a RunStore,
}

impl<'a> Runner<'a> {
    pub fn new(agent: &'a Agent, store: &'a RunStore) -> Self {
        Self { agent, store }
    }

    /// Steps the run until it finishes or waits, saving after every step.
    pub async fn drive(&self, mut run: StoredRun) -> Result<StoredRun, RunnerError> {
        let guard = StoreGuard {
            store: self.store,
            run_id: run.id.clone(),
        };
        while run.state.status == RunStatus::Running {
            let next = self.agent.step_with(run.state.clone(), &guard).await;
            run = self.store.save(run, next).await?;
        }
        Ok(run)
    }

    /// Applies a decision to a waiting run. The decision is saved before any
    /// tool runs, so a concurrent decision on the same run fails with a
    /// conflict instead of running anything.
    pub async fn decide(
        &self,
        run_id: &str,
        decision: &Decision,
    ) -> Result<StoredRun, RunnerError> {
        let run = self.store.load(run_id).await?;
        let next = self.agent.record_decision(run.state.clone(), decision)?;
        let run = self.store.save(run, next).await?;
        self.drive(run).await
    }
}
