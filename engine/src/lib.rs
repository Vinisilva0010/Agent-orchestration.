//! Agent engine.
//!
//! Runs an agent loop that reaches models only through the gateway, pauses
//! before irreversible tool calls until a person decides, and stops at step or
//! budget limits. Run state is saved in PostgreSQL after every step, and
//! irreversible tools run at most once. It never holds provider keys or the
//! vault master key.

pub mod agent;
pub mod gateway;
pub mod runner;
pub mod secrets_guard;
pub mod store;
pub mod tools;
