//! Provider-neutral request and response types.
//!
//! The gateway and the agent engine both depend on this crate, so a change to
//! the wire format breaks the build of both instead of failing at runtime.
//! The tests at the bottom pin the exact JSON shape of every type.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

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
    /// Model reasoning returned by some providers (Anthropic thinking blocks,
    /// OpenAI encrypted reasoning). Must be sent back unchanged, in order,
    /// with the turn it came from. `redacted_data` holds encrypted reasoning.
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

/// A request that no provider should receive. The message is safe to show.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct ValidationError(pub String);

impl CompletionRequest {
    /// Rejects requests that no provider should receive.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.messages.is_empty() {
            return Err(ValidationError("messages must not be empty".to_string()));
        }
        if self.messages.first().map(|m| m.role) != Some(Role::User) {
            return Err(ValidationError(
                "the first message must come from the user".to_string(),
            ));
        }
        if self.max_output_tokens == 0 || self.max_output_tokens > MAX_OUTPUT_TOKENS_LIMIT {
            return Err(ValidationError(format!(
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn to_json(block: &ContentBlock) -> Value {
        serde_json::to_value(block).unwrap()
    }

    #[test]
    fn content_blocks_have_a_fixed_wire_format() {
        let text = ContentBlock::Text {
            text: "hi".to_string(),
            signature: None,
        };
        assert_eq!(to_json(&text), json!({ "type": "text", "text": "hi" }));

        let tool_use = ContentBlock::ToolUse {
            id: "call_1".to_string(),
            name: "search".to_string(),
            input: json!({ "q": "x" }),
            signature: Some("sig".to_string()),
        };
        assert_eq!(
            to_json(&tool_use),
            json!({ "type": "tool_use", "id": "call_1", "name": "search", "input": { "q": "x" }, "signature": "sig" })
        );

        let result: ContentBlock = serde_json::from_value(
            json!({ "type": "tool_result", "tool_use_id": "call_1", "content": "ok" }),
        )
        .unwrap();
        assert!(matches!(
            result,
            ContentBlock::ToolResult {
                is_error: false,
                ..
            }
        ));

        let reasoning = ContentBlock::Reasoning {
            text: String::new(),
            signature: None,
            redacted_data: Some("enc".to_string()),
        };
        assert_eq!(
            to_json(&reasoning),
            json!({ "type": "reasoning", "text": "", "redacted_data": "enc" })
        );
    }

    #[test]
    fn requests_reject_unknown_fields() {
        let body = json!({
            "tier": "small",
            "messages": [{ "role": "user", "content": [] }],
            "max_output_tokens": 10,
            "api_key": "should never be accepted"
        });
        assert!(serde_json::from_value::<CompletionRequest>(body).is_err());

        let message = json!({ "role": "user", "content": [], "extra": 1 });
        assert!(serde_json::from_value::<Message>(message).is_err());
    }

    #[test]
    fn tiers_and_stop_reasons_use_snake_case() {
        assert_eq!(
            serde_json::to_value(ModelTier::Standard).unwrap(),
            json!("standard")
        );
        assert_eq!(
            serde_json::to_value(StopReason::ToolUse).unwrap(),
            json!("tool_use")
        );
        assert_eq!(
            serde_json::to_value(StopReason::MaxTokens).unwrap(),
            json!("max_tokens")
        );
    }

    fn request() -> CompletionRequest {
        CompletionRequest {
            tier: ModelTier::Small,
            system: vec![],
            messages: vec![Message {
                role: Role::User,
                content: vec![],
            }],
            tools: vec![],
            max_output_tokens: 256,
        }
    }

    #[test]
    fn validation_rules() {
        assert!(request().validate().is_ok());

        let mut empty = request();
        empty.messages.clear();
        assert_eq!(
            empty.validate().unwrap_err().to_string(),
            "messages must not be empty"
        );

        let mut assistant_first = request();
        assistant_first.messages[0].role = Role::Assistant;
        assert!(assistant_first.validate().is_err());

        for bad in [0, MAX_OUTPUT_TOKENS_LIMIT + 1] {
            let mut req = request();
            req.max_output_tokens = bad;
            assert!(req.validate().is_err(), "{bad}");
        }
    }
}
