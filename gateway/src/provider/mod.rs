//! Provider-neutral request and response types and the provider interface.
//!
//! The agents process always speaks this format. The gateway translates it to
//! each provider, so agents never depend on a specific vendor API.

pub mod mock;

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
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
}

pub type ProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CompletionResponse, ProviderError>> + Send + 'a>>;

/// A model provider. Implementations hold their own credentials and never
/// expose them through requests, responses or errors.
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    /// Price of the model this provider uses for a tier.
    fn price(&self, tier: ModelTier) -> Price;
    fn complete<'a>(&'a self, request: &'a CompletionRequest) -> ProviderFuture<'a>;
}
