//! Agent engine.
//!
//! Runs an agent loop that reaches models only through the gateway, pauses
//! before irreversible tool calls until a person decides, and stops at step or
//! budget limits. It never holds provider keys or the vault master key.

pub mod agent;
pub mod gateway;
pub mod secrets_guard;
pub mod tools;
