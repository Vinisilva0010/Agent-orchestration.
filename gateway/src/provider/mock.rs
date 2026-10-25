//! Deterministic provider for tests and local development.
//!
//! It never touches the network and never needs a key.
//!
//! - If the last user message contains `[mock:tool:NAME]` and a tool named NAME
//!   was offered, it answers with a call to that tool. Otherwise it answers text.
//! - Cache behaviour mirrors prompt caching: the first request with a given set
//!   of cacheable system blocks is billed as a cache write, later ones as reads.

use std::{
    collections::HashSet,
    hash::{DefaultHasher, Hash, Hasher},
    sync::Mutex,
};

use serde_json::json;

use super::{
    CompletionRequest, CompletionResponse, ContentBlock, Message, ModelTier, Provider,
    ProviderError, ProviderFuture, Role, StopReason, ToolSpec, Usage,
};
use crate::pricing::Price;

const TOOL_MARKER_PREFIX: &str = "[mock:tool:";
const TEXT_REPLY: &str = "mock response";

#[derive(Debug, Default)]
pub struct MockProvider {
    cached_prefixes: Mutex<HashSet<u64>>,
}

impl MockProvider {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Rough token estimate: one token per four characters, rounded up.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

fn block_tokens(block: &ContentBlock) -> u64 {
    match block {
        ContentBlock::Text { text } => estimate_tokens(text),
        ContentBlock::ToolUse { name, input, .. } => {
            estimate_tokens(name) + estimate_tokens(&input.to_string())
        }
        ContentBlock::ToolResult { content, .. } => estimate_tokens(content),
    }
}

fn message_tokens(messages: &[Message]) -> u64 {
    messages
        .iter()
        .flat_map(|m| &m.content)
        .map(block_tokens)
        .sum()
}

fn tool_tokens(tools: &[ToolSpec]) -> u64 {
    tools
        .iter()
        .map(|t| {
            estimate_tokens(&t.name)
                + estimate_tokens(&t.description)
                + estimate_tokens(&t.input_schema.to_string())
        })
        .sum()
}

fn requested_tool(request: &CompletionRequest) -> Option<&ToolSpec> {
    let last_user = request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)?;
    last_user.content.iter().find_map(|block| match block {
        ContentBlock::Text { text } => {
            let start = text.find(TOOL_MARKER_PREFIX)? + TOOL_MARKER_PREFIX.len();
            let end = text[start..].find(']')? + start;
            let name = &text[start..end];
            request.tools.iter().find(|t| t.name == name)
        }
        _ => None,
    })
}

impl Provider for MockProvider {
    fn name(&self) -> &'static str {
        "mock"
    }

    /// Prices close to common market rates, so tests exercise realistic numbers.
    /// Cache reads cost 10% of input and cache writes 125% of input.
    fn price(&self, tier: ModelTier) -> Price {
        match tier {
            ModelTier::Small => Price {
                input: 1_000_000,
                output: 5_000_000,
                cache_read: 100_000,
                cache_write: 1_250_000,
            },
            ModelTier::Standard => Price {
                input: 3_000_000,
                output: 15_000_000,
                cache_read: 300_000,
                cache_write: 3_750_000,
            },
            ModelTier::Large => Price {
                input: 5_000_000,
                output: 25_000_000,
                cache_read: 500_000,
                cache_write: 6_250_000,
            },
        }
    }

    fn complete<'a>(&'a self, request: &'a CompletionRequest) -> ProviderFuture<'a> {
        Box::pin(async move {
            request.validate()?;

            let (cacheable, uncached): (Vec<_>, Vec<_>) =
                request.system.iter().partition(|b| b.cache);
            let cacheable_tokens: u64 = cacheable.iter().map(|b| estimate_tokens(&b.text)).sum();
            let uncached_tokens: u64 = uncached.iter().map(|b| estimate_tokens(&b.text)).sum();

            let mut usage = Usage {
                input_tokens: uncached_tokens
                    + tool_tokens(&request.tools)
                    + message_tokens(&request.messages),
                ..Usage::default()
            };

            if cacheable_tokens > 0 {
                let mut hasher = DefaultHasher::new();
                for block in &cacheable {
                    block.text.hash(&mut hasher);
                }
                let first_time = self
                    .cached_prefixes
                    .lock()
                    .map_err(|_| ProviderError::Unavailable("mock cache unavailable".to_string()))?
                    .insert(hasher.finish());
                if first_time {
                    usage.cache_write_tokens = cacheable_tokens;
                } else {
                    usage.cache_read_tokens = cacheable_tokens;
                }
            }

            let (content, stop_reason) = match requested_tool(request) {
                Some(tool) => (
                    vec![ContentBlock::ToolUse {
                        id: format!("mock_call_{}", tool.name),
                        name: tool.name.clone(),
                        input: json!({}),
                    }],
                    StopReason::ToolUse,
                ),
                None => (
                    vec![ContentBlock::Text {
                        text: TEXT_REPLY.to_string(),
                    }],
                    StopReason::EndTurn,
                ),
            };
            usage.output_tokens = content.iter().map(block_tokens).sum();

            Ok(CompletionResponse {
                provider: self.name().to_string(),
                model: format!("mock-{}", request.tier.as_str()),
                content,
                stop_reason,
                usage,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::provider::{MAX_OUTPUT_TOKENS_LIMIT, ModelTier, SystemBlock};

    fn user_text(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    fn request(text: &str) -> CompletionRequest {
        CompletionRequest {
            tier: ModelTier::Small,
            system: vec![],
            messages: vec![user_text(text)],
            tools: vec![],
            max_output_tokens: 256,
        }
    }

    fn search_tool() -> ToolSpec {
        ToolSpec {
            name: "search".to_string(),
            description: "Search documents".to_string(),
            input_schema: json!({"type": "object"}),
        }
    }

    #[test]
    fn estimates_one_token_per_four_chars_rounded_up() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[tokio::test]
    async fn answers_text_by_default() {
        let response = MockProvider::new()
            .complete(&request("hello"))
            .await
            .unwrap();
        assert_eq!(response.stop_reason, StopReason::EndTurn);
        assert_eq!(response.model, "mock-small");
        assert!(response.usage.input_tokens > 0);
        assert!(response.usage.output_tokens > 0);
    }

    #[tokio::test]
    async fn calls_offered_tool_when_marker_present() {
        let mut req = request("please [mock:tool:search] now");
        req.tools.push(search_tool());
        let response = MockProvider::new().complete(&req).await.unwrap();
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        assert!(
            matches!(&response.content[0], ContentBlock::ToolUse { name, .. } if name == "search")
        );
    }

    #[tokio::test]
    async fn ignores_marker_for_tool_not_offered() {
        let response = MockProvider::new()
            .complete(&request("[mock:tool:delete_everything]"))
            .await
            .unwrap();
        assert_eq!(response.stop_reason, StopReason::EndTurn);
    }

    #[tokio::test]
    async fn bills_cache_write_first_then_cache_read() {
        let provider = MockProvider::new();
        let mut req = request("hello");
        req.system.push(SystemBlock {
            text: "Company rules that never change.".to_string(),
            cache: true,
        });

        let first = provider.complete(&req).await.unwrap();
        assert!(first.usage.cache_write_tokens > 0);
        assert_eq!(first.usage.cache_read_tokens, 0);

        let second = provider.complete(&req).await.unwrap();
        assert_eq!(second.usage.cache_write_tokens, 0);
        assert_eq!(
            second.usage.cache_read_tokens,
            first.usage.cache_write_tokens
        );
    }

    #[tokio::test]
    async fn rejects_invalid_requests() {
        let provider = MockProvider::new();

        let mut empty = request("x");
        empty.messages.clear();
        assert!(matches!(
            provider.complete(&empty).await,
            Err(ProviderError::InvalidRequest(_))
        ));

        let mut too_long = request("x");
        too_long.max_output_tokens = MAX_OUTPUT_TOKENS_LIMIT + 1;
        assert!(matches!(
            provider.complete(&too_long).await,
            Err(ProviderError::InvalidRequest(_))
        ));

        let mut starts_with_assistant = request("x");
        starts_with_assistant.messages[0].role = Role::Assistant;
        assert!(matches!(
            provider.complete(&starts_with_assistant).await,
            Err(ProviderError::InvalidRequest(_))
        ));
    }
}
