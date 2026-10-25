//! Provider-neutral request and response types and the provider interface.
//!
//! The agents process always speaks this format. The gateway translates it to
//! each provider, so agents never depend on a specific vendor API.

pub mod anthropic;
pub mod gemini;
mod http;
pub mod mock;
pub mod openai;

use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::pricing::Price;

/// Upper bound for output tokens accepted from callers.
pub const MAX_OUTPUT_TOKENS_LIMIT: u32 = 32_768;

/// Model size requested by the caller. The gateway maps it to a concrete model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Small,
    Standard,
    Large,
}

impl ModelTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Standard => "standard",
            Self::Large => "large",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

/// One piece of a message.
///
/// `signature` is an opaque value some providers attach to their output (for
/// example Gemini thought signatures). Callers must send it back unchanged
/// with the same block in the next request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    /// Model reasoning returned by some providers (Anthropic thinking blocks).
    /// Must be sent back unchanged, in order, with the turn it came from.
    /// `redacted_data` holds encrypted reasoning the provider did not show.
    Reasoning {
        #[serde(default)]
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redacted_data: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

/// A block of system instructions. `cache: true` marks it as a stable prefix
/// that may be served from the provider's prompt cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemBlock {
    pub text: String,
    #[serde(default)]
    pub cache: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionRequest {
    pub tier: ModelTier,
    #[serde(default)]
    pub system: Vec<SystemBlock>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    pub max_output_tokens: u32,
}

impl CompletionRequest {
    /// Rejects requests that no provider should receive.
    pub fn validate(&self) -> Result<(), ProviderError> {
        if self.messages.is_empty() {
            return Err(ProviderError::InvalidRequest(
                "messages must not be empty".to_string(),
            ));
        }
        if self.messages.first().map(|m| m.role) != Some(Role::User) {
            return Err(ProviderError::InvalidRequest(
                "the first message must come from the user".to_string(),
            ));
        }
        if self.max_output_tokens == 0 || self.max_output_tokens > MAX_OUTPUT_TOKENS_LIMIT {
            return Err(ProviderError::InvalidRequest(format!(
                "max_output_tokens must be between 1 and {MAX_OUTPUT_TOKENS_LIMIT}"
            )));
        }
        Ok(())
    }
}

/// Token usage as billed by the provider, split by cache behaviour.
/// `output_tokens` includes reasoning ("thinking") tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    /// The provider stopped for safety, recitation or a similar content rule.
    Refused,
    /// The provider stopped for a reason the gateway does not map.
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub provider: String,
    pub model: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub usage: Usage,
}

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
