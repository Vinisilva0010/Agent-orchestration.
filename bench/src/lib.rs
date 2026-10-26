//! Cost benchmark for the agent engine.
//!
//! Runs a fixed set of tasks against a real gateway several times and reports
//! the billed cost per successfully completed task, including cache use. A task
//! counts as solved only when the final state of the whole desk matches the
//! expected state, so collateral changes count as failures.

pub mod desk;
pub mod harness;
pub mod metrics;
pub mod tasks;
