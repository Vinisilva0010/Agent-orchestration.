//! Provider interface. The neutral request and response types live in the
//! `protocol` crate, shared with the agent engine, and are re-exported here.

pub mod anthropic;
pub mod gemini;
mod http;
pub mod mock;
pub mod openai;

use std::{future::Future, pin::Pin};

pub use protocol::{
    CompletionRequest, CompletionResponse, ContentBlock, MAX_OUTPUT_TOKENS_LIMIT, Message,
    ModelTier, Role, StopReason, SystemBlock, ToolSpec, Usage, ValidationError,
};
use thiserror::Error;

use crate::pricing::Price;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProviderError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    /// Transient failure (overload, 5xx, connection refused). Safe to retry later.
    #[error("provider temporarily unavailable: {0}")]
    TemporarilyUnavailable(String),
    #[error("no API key is configured for this provider")]
    NotConfigured,
    #[error("the provider rejected the stored API key")]
    CredentialRejected,
    #[error("the provider rate limit was reached; try again later")]
    RateLimited,
}

impl From<ValidationError> for ProviderError {
    fn from(error: ValidationError) -> Self {
        Self::InvalidRequest(error.0)
    }
}

pub type ProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CompletionResponse, ProviderError>> + Send + 'a>>;

/// A model provider. Implementations hold their own credentials and never
/// expose them through requests, responses or errors.
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    /// Price used to reserve budget for a call whose input cannot exceed
    /// `max_input_tokens`. Fails when no price is known, so a call is never
    /// made without a cost bound.
    fn price(&self, tier: ModelTier, max_input_tokens: u64) -> Result<Price, ProviderError>;
    /// Actual cost of a completed call. Never above the reservation made with
    /// `reserved`. Defaults to pricing the usage at the reservation price.
    fn cost_micros(&self, completion: &CompletionResponse, reserved: &Price) -> u64 {
        reserved.cost_micros(&completion.usage)
    }
    fn complete<'a>(&'a self, request: &'a CompletionRequest) -> ProviderFuture<'a>;
}
