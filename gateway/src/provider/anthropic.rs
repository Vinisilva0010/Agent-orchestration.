//! Anthropic provider (Claude Messages API).
//!
//! - The API key is read from the vault on every call and sent only in the
//!   `x-api-key` header, marked sensitive. Never in the URL or the body.
//! - Thinking blocks, including redacted ones, come back to callers as neutral
//!   `reasoning` blocks. They must be sent back unchanged with the tool calls
//!   they accompanied: Anthropic rejects modified or missing blocks.
//! - Cacheable system blocks get one cache breakpoint, on the last of them, so
//!   tools and the whole stable system prefix are read from cache later.
//! - Thinking tokens count toward `max_tokens` and are billed as output, so the
//!   reservation made before the call already covers them.

use std::{sync::Arc, time::Duration};

use reqwest::{
    Client, StatusCode,
    header::{CONTENT_TYPE, HeaderValue},
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{
    CompletionRequest, CompletionResponse, ContentBlock, ModelTier, Provider, ProviderError,
    ProviderFuture, Role, StopReason, Usage,
    http::{RETRY_BASE, build_client, clean, read_limited, transport_error, with_retry},
};
use crate::{
    pricing::Price,
    vault::{CredentialProvider, Vault},
};

const API_BASE: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Cache writes use the 5-minute rate (1.25x input), the default TTL.
const fn anthropic_price(input: u64, output: u64, cache_read: u64) -> Price {
    Price {
        input,
        output,
        cache_read,
        cache_write: input + input / 4,
    }
}

/// Concrete model behind a tier. Prices are micro-dollars per million tokens,
/// from https://platform.claude.com/docs/about-claude/pricing (checked
/// 2026-10-25).
#[derive(Debug)]
struct ModelSpec {
    id: &'static str,
    /// `thinking.type` sent with every request.
    thinking: &'static str,
    /// `output_config.effort` sent with every request.
    effort: &'static str,
    price: Price,
    /// Prompts above this many tokens (input plus cache reads and writes) are
    /// billed at the second price.
    long_prompt: Option<(u64, Price)>,
}

impl ModelSpec {
    fn price_for_prompt(&self, prompt_tokens: u64) -> Price {
        match self.long_prompt {
            Some((threshold, price)) if prompt_tokens > threshold => price,
            _ => self.price,
        }
    }
}

/// Small tier: thinking off and low effort, the cheapest setting the model has.
static HAIKU: ModelSpec = ModelSpec {
    id: "claude-haiku-5-5",
    thinking: "disabled",
    effort: "low",
    price: anthropic_price(100_000, 500_000, 10_000),
    long_prompt: Some((100_000, anthropic_price(500_000, 2_500_000, 50_000))),
};

static SONNET: ModelSpec = ModelSpec {
    id: "claude-sonnet-5-5",
    thinking: "adaptive",
    effort: "medium",
    price: anthropic_price(2_000_000, 10_000_000, 100_000),
    long_prompt: None,
};

/// Thinking cannot be disabled on Opus 5.5.
static OPUS: ModelSpec = ModelSpec {
    id: "claude-opus-5-5",
    thinking: "adaptive",
    effort: "medium",
    price: anthropic_price(4_000_000, 20_000_000, 200_000),
    long_prompt: None,
};

static CATALOG: [&ModelSpec; 3] = [&HAIKU, &SONNET, &OPUS];

fn model_for(tier: ModelTier) -> &'static ModelSpec {
    match tier {
        ModelTier::Small => &HAIKU,
        ModelTier::Standard => &SONNET,
        ModelTier::Large => &OPUS,
    }
}

fn find_model(id: &str) -> Option<&'static ModelSpec> {
    CATALOG.iter().copied().find(|model| model.id == id)
}

pub struct AnthropicProvider {
    vault: Arc<Vault>,
    client: Client,
    base_url: String,
    retry_base: Duration,
}

impl AnthropicProvider {
    pub fn new(vault: Arc<Vault>) -> Result<Self, ProviderError> {
        Self::build(vault, API_BASE.to_string(), true)
    }

    fn build(vault: Arc<Vault>, base_url: String, https_only: bool) -> Result<Self, ProviderError> {
        Ok(Self {
            vault,
            client: build_client(https_only)?,
            base_url,
            retry_base: RETRY_BASE,
        })
    }

    async fn call_once(
        &self,
        request: &CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        request.validate()?;
        let model = model_for(request.tier);
        let body = serde_json::to_vec(&build_body(request, model)?).map_err(|_| {
            ProviderError::InvalidRequest("request could not be encoded".to_string())
        })?;

        let key = self
            .vault
            .load(CredentialProvider::Anthropic)
            .await
            .map_err(|_| ProviderError::Unavailable("credential store unavailable".to_string()))?
            .ok_or(ProviderError::NotConfigured)?;
        let mut key_header = HeaderValue::from_str(key.expose_secret())
            .map_err(|_| ProviderError::CredentialRejected)?;
        key_header.set_sensitive(true);

        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", key_header)
            .header("anthropic-version", API_VERSION)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|error| transport_error("anthropic", &error))?;

        let status = response.status();
        let bytes = read_limited(response).await?;
        if !status.is_success() {
            return Err(map_error(status, &bytes, &key));
        }
        parse_response(&bytes, model)
    }
}

impl Provider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn price(&self, tier: ModelTier, max_input_tokens: u64) -> Result<Price, ProviderError> {
        Ok(model_for(tier).price_for_prompt(max_input_tokens))
    }

    /// Prices the call at the rate for the prompt length it actually had.
    fn cost_micros(&self, completion: &CompletionResponse, reserved: &Price) -> u64 {
        let usage = &completion.usage;
        match find_model(&completion.model) {
            Some(model) => {
                let prompt = usage
                    .input_tokens
                    .saturating_add(usage.cache_read_tokens)
                    .saturating_add(usage.cache_write_tokens);
                model.price_for_prompt(prompt).cost_micros(usage)
            }
            None => reserved.cost_micros(usage),
        }
    }

    fn complete<'a>(&'a self, request: &'a CompletionRequest) -> ProviderFuture<'a> {
        Box::pin(with_retry(self.retry_base, "anthropic", move || {
            self.call_once(request)
        }))
    }
}

/// Translates a neutral request into a Messages API body.
fn build_body(request: &CompletionRequest, model: &ModelSpec) -> Result<Value, ProviderError> {
    let mut messages = Vec::with_capacity(request.messages.len());
    for message in &request.messages {
        let mut content = Vec::with_capacity(message.content.len());
        for block in &message.content {
            match block {
                // Empty text blocks are rejected by the API. They can appear
                // when a conversation started with another provider.
                ContentBlock::Text { text, .. } => {
                    if !text.is_empty() {
                        content.push(json!({ "type": "text", "text": text }));
                    }
                }
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => content.push(json!({
                    "type": "tool_use", "id": id, "name": name, "input": input,
                })),
                ContentBlock::ToolResult {
                    tool_use_id,
                    content: output,
                    is_error,
                } => content.push(json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": output,
                    "is_error": is_error,
                })),
                ContentBlock::Reasoning {
                    text,
                    signature,
                    redacted_data,
                } => {
                    let block = match (redacted_data, signature) {
                        (Some(data), _) => json!({ "type": "redacted_thinking", "data": data }),
                        (None, Some(signature)) => json!({
                            "type": "thinking", "thinking": text, "signature": signature,
                        }),
                        (None, None) => {
                            return Err(ProviderError::InvalidRequest(
                                "reasoning blocks must keep the signature or redacted data they came with"
                                    .to_string(),
                            ));
                        }
                    };
                    content.push(block);
                }
            }
        }
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        messages.push(json!({ "role": role, "content": content }));
    }

    let mut body = Map::new();
    body.insert("model".to_string(), json!(model.id));
    body.insert("max_tokens".to_string(), json!(request.max_output_tokens));
    body.insert("messages".to_string(), Value::Array(messages));

    let last_cached = request.system.iter().rposition(|block| block.cache);
    let system: Vec<Value> = request
        .system
        .iter()
        .enumerate()
        .filter(|(_, block)| !block.text.is_empty())
        .map(|(index, block)| {
            let mut value = json!({ "type": "text", "text": block.text });
            if Some(index) == last_cached {
                value["cache_control"] = json!({ "type": "ephemeral" });
            }
            value
        })
        .collect();
    if !system.is_empty() {
        body.insert("system".to_string(), Value::Array(system));
    }

    if !request.tools.is_empty() {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.input_schema,
                })
            })
            .collect();
        body.insert("tools".to_string(), Value::Array(tools));
    }

    body.insert("thinking".to_string(), json!({ "type": model.thinking }));
    body.insert(
        "output_config".to_string(),
        json!({ "effort": model.effort }),
    );
    Ok(Value::Object(body))
}

#[derive(Deserialize)]
struct MessageResponse {
    #[serde(default)]
    content: Vec<ResponseBlock>,
    stop_reason: Option<String>,
    #[serde(default)]
    usage: AnthropicUsage,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
    #[serde(other)]
    Unknown,
}

/// `input_tokens` excludes cache reads and writes, which are reported apart.
#[derive(Deserialize, Default)]
#[serde(default)]
struct AnthropicUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    message: String,
}

fn parse_response(body: &[u8], model: &ModelSpec) -> Result<CompletionResponse, ProviderError> {
    let parsed: MessageResponse = serde_json::from_slice(body).map_err(|_| {
        ProviderError::Unavailable("unexpected response from the provider".to_string())
    })?;

    let content = parsed
        .content
        .into_iter()
        .filter_map(|block| match block {
            ResponseBlock::Text { text } => Some(ContentBlock::Text {
                text,
                signature: None,
            }),
            ResponseBlock::ToolUse { id, name, input } => Some(ContentBlock::ToolUse {
                id,
                name,
                input: if input.is_null() { json!({}) } else { input },
                signature: None,
            }),
            ResponseBlock::Thinking {
                thinking,
                signature,
            } => Some(ContentBlock::Reasoning {
                text: thinking,
                signature,
                redacted_data: None,
            }),
            ResponseBlock::RedactedThinking { data } => Some(ContentBlock::Reasoning {
                text: String::new(),
                signature: None,
                redacted_data: Some(data),
            }),
            ResponseBlock::Unknown => None,
        })
        .collect();

    let stop_reason = match parsed.stop_reason.as_deref() {
        Some("end_turn" | "stop_sequence") | None => StopReason::EndTurn,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("refusal") => StopReason::Refused,
        Some(_) => StopReason::Other,
    };

    let usage = parsed.usage;
    Ok(CompletionResponse {
        provider: "anthropic".to_string(),
        model: model.id.to_string(),
        content,
        stop_reason,
        usage: Usage {
            input_tokens: usage.input_tokens.unwrap_or(0),
            output_tokens: usage.output_tokens.unwrap_or(0),
            cache_read_tokens: usage.cache_read_input_tokens.unwrap_or(0),
            cache_write_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
        },
    })
}

fn map_error(status: StatusCode, body: &[u8], key: &SecretString) -> ProviderError {
    tracing::warn!(status = status.as_u16(), "anthropic returned an error");
    let message = serde_json::from_slice::<ErrorEnvelope>(body)
        .map(|envelope| envelope.error.message)
        .unwrap_or_default();
    let detail = clean(&message, Some(key));
    match status.as_u16() {
        401 | 403 => ProviderError::CredentialRejected,
        429 => ProviderError::RateLimited,
        402 => ProviderError::Unavailable(format!(
            "the provider account has a billing problem: {detail}"
        )),
        code @ 500..=599 => ProviderError::TemporarilyUnavailable(format!(
            "the provider returned status {code}: {detail}"
        )),
        400..=499 => {
            ProviderError::InvalidRequest(format!("the provider rejected the request: {detail}"))
        }
        code => {
            ProviderError::Unavailable(format!("the provider returned status {code}: {detail}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
    use sqlx::PgPool;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    use super::*;
    use crate::provider::{Message, SystemBlock, ToolSpec, http::MAX_ATTEMPTS};

    const KEY: &str = "sk-ant-test-not-a-real-key-0123456789";

    fn text(value: &str) -> ContentBlock {
        ContentBlock::Text {
            text: value.to_string(),
            signature: None,
        }
    }

    fn user(content: Vec<ContentBlock>) -> Message {
        Message {
            role: Role::User,
            content,
        }
    }

    fn assistant(content: Vec<ContentBlock>) -> Message {
        Message {
            role: Role::Assistant,
            content,
        }
    }

    fn request(tier: ModelTier, messages: Vec<Message>) -> CompletionRequest {
        CompletionRequest {
            tier,
            system: vec![],
            messages,
            tools: vec![],
            max_output_tokens: 256,
        }
    }

    fn hello() -> CompletionRequest {
        request(ModelTier::Standard, vec![user(vec![text("hi")])])
    }

    fn ok_body() -> Value {
        json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-sonnet-5-5",
            "content": [
                { "type": "thinking", "thinking": "", "signature": "sig-1" },
                { "type": "text", "text": "hello", "citations": null }
            ],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 50, "output_tokens": 20,
                "cache_creation_input_tokens": 100, "cache_read_input_tokens": 300
            }
        })
    }

    async fn vault(pool: PgPool, key: Option<&str>) -> Arc<Vault> {
        let master = SecretString::from(BASE64.encode([7u8; 32]));
        let vault = Arc::new(Vault::new(pool, &master).unwrap());
        if let Some(key) = key {
            vault
                .store(
                    CredentialProvider::Anthropic,
                    &SecretString::from(key.to_string()),
                )
                .await
                .unwrap();
        }
        vault
    }

    fn provider(vault: Arc<Vault>, server: &MockServer) -> AnthropicProvider {
        let mut provider = AnthropicProvider::build(vault, server.uri(), false).unwrap();
        provider.retry_base = Duration::ZERO;
        provider
    }

    #[test]
    fn translates_system_cache_tools_and_reasoning() {
        let mut req = request(
            ModelTier::Standard,
            vec![
                user(vec![text("find x")]),
                assistant(vec![
                    ContentBlock::Reasoning {
                        text: String::new(),
                        signature: Some("sig-1".to_string()),
                        redacted_data: None,
                    },
                    ContentBlock::Reasoning {
                        text: String::new(),
                        signature: None,
                        redacted_data: Some("opaque".to_string()),
                    },
                    ContentBlock::Text {
                        text: String::new(),
                        signature: Some("gemini-only".to_string()),
                    },
                    ContentBlock::ToolUse {
                        id: "toolu_1".to_string(),
                        name: "search".to_string(),
                        input: json!({ "q": "x" }),
                        signature: None,
                    },
                ]),
                user(vec![ContentBlock::ToolResult {
                    tool_use_id: "toolu_1".to_string(),
                    content: "boom".to_string(),
                    is_error: true,
                }]),
            ],
        );
        req.system = vec![
            SystemBlock {
                text: "company rules".to_string(),
                cache: true,
            },
            SystemBlock {
                text: "agent role".to_string(),
                cache: true,
            },
            SystemBlock {
                text: "today".to_string(),
                cache: false,
            },
        ];
        req.tools.push(ToolSpec {
            name: "search".to_string(),
            description: "Search documents".to_string(),
            input_schema: json!({ "type": "object" }),
        });

        let body = build_body(&req, &SONNET).unwrap();
        assert_eq!(body["model"], "claude-sonnet-5-5");
        assert_eq!(body["max_tokens"], 256);
        assert!(body["system"][0].get("cache_control").is_none());
        assert_eq!(body["system"][1]["cache_control"]["type"], "ephemeral");
        assert!(body["system"][2].get("cache_control").is_none());

        let turn = body["messages"][1]["content"].as_array().unwrap();
        assert_eq!(turn.len(), 3, "the empty text block is dropped");
        assert_eq!(turn[0]["type"], "thinking");
        assert_eq!(turn[0]["signature"], "sig-1");
        assert_eq!(turn[1]["type"], "redacted_thinking");
        assert_eq!(turn[1]["data"], "opaque");
        assert_eq!(turn[2]["type"], "tool_use");

        let result = &body["messages"][2]["content"][0];
        assert_eq!(result["type"], "tool_result");
        assert_eq!(result["is_error"], true);
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "medium");
    }

    #[test]
    fn small_tier_turns_thinking_off() {
        let body = build_body(
            &request(ModelTier::Small, vec![user(vec![text("hi")])]),
            &HAIKU,
        )
        .unwrap();
        assert_eq!(body["model"], "claude-haiku-5-5");
        assert_eq!(body["thinking"]["type"], "disabled");
        assert_eq!(body["output_config"]["effort"], "low");
    }

    #[test]
    fn rejects_reasoning_without_signature_or_data() {
        let req = request(
            ModelTier::Standard,
            vec![
                user(vec![text("x")]),
                assistant(vec![ContentBlock::Reasoning {
                    text: "edited".to_string(),
                    signature: None,
                    redacted_data: None,
                }]),
            ],
        );
        assert!(matches!(
            build_body(&req, &SONNET),
            Err(ProviderError::InvalidRequest(_))
        ));
    }

    #[test]
    fn parses_blocks_usage_and_stop_reasons() {
        let response = parse_response(&serde_json::to_vec(&ok_body()).unwrap(), &SONNET).unwrap();
        assert_eq!(response.provider, "anthropic");
        assert_eq!(response.stop_reason, StopReason::EndTurn);
        assert_eq!(
            response.content,
            vec![
                ContentBlock::Reasoning {
                    text: String::new(),
                    signature: Some("sig-1".to_string()),
                    redacted_data: None,
                },
                ContentBlock::Text {
                    text: "hello".to_string(),
                    signature: None,
                },
            ]
        );
        assert_eq!(
            response.usage,
            Usage {
                input_tokens: 50,
                output_tokens: 20,
                cache_read_tokens: 300,
                cache_write_tokens: 100,
            }
        );

        let tool_call = json!({
            "content": [
                { "type": "redacted_thinking", "data": "opaque" },
                { "type": "server_tool_use", "id": "srv_1", "name": "web_search", "input": {} },
                { "type": "tool_use", "id": "toolu_1", "name": "search", "input": { "q": "x" } }
            ],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        });
        let response = parse_response(&serde_json::to_vec(&tool_call).unwrap(), &SONNET).unwrap();
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        assert_eq!(response.content.len(), 2, "unknown block types are skipped");
        assert!(matches!(
            &response.content[0],
            ContentBlock::Reasoning { redacted_data: Some(d), .. } if d == "opaque"
        ));

        for (reason, expected) in [
            ("max_tokens", StopReason::MaxTokens),
            ("refusal", StopReason::Refused),
            ("pause_turn", StopReason::Other),
        ] {
            let body = json!({ "content": [], "stop_reason": reason, "usage": {} });
            let response = parse_response(&serde_json::to_vec(&body).unwrap(), &SONNET).unwrap();
            assert_eq!(response.stop_reason, expected, "{reason}");
        }
    }

    #[test]
    fn haiku_long_prompts_use_the_higher_price() {
        assert_eq!(HAIKU.price_for_prompt(100_000).input, 100_000);
        assert_eq!(HAIKU.price_for_prompt(100_001).input, 500_000);
        assert_eq!(HAIKU.price_for_prompt(1).cache_write, 125_000);
        assert_eq!(SONNET.price_for_prompt(900_000).input, 2_000_000);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn reservation_and_cost_follow_prompt_length(pool: PgPool) {
        let server = MockServer::start().await;
        let anthropic = provider(vault(pool, None).await, &server);
        assert_eq!(
            anthropic.price(ModelTier::Small, 50_000).unwrap().input,
            100_000
        );
        let reserved = anthropic.price(ModelTier::Small, 2_000_000).unwrap();
        assert_eq!(reserved.input, 500_000);

        let long = CompletionResponse {
            provider: "anthropic".to_string(),
            model: "claude-haiku-5-5".to_string(),
            content: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 1_000_000,
                ..Usage::default()
            },
        };
        assert_eq!(anthropic.cost_micros(&long, &reserved), 500_000);

        let short = CompletionResponse {
            usage: Usage {
                input_tokens: 10_000,
                ..Usage::default()
            },
            ..long.clone()
        };
        assert_eq!(anthropic.cost_micros(&short, &reserved), 1_000);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn sends_key_only_in_header(pool: PgPool) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", KEY))
            .and(header("anthropic-version", API_VERSION))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;

        let anthropic = provider(vault(pool, Some(KEY)).await, &server);
        let response = anthropic.complete(&hello()).await.unwrap();
        assert_eq!(response.model, "claude-sonnet-5-5");

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].url.query().is_none());
        assert!(requests[0].headers.get("authorization").is_none());
        assert!(!String::from_utf8_lossy(&requests[0].body).contains(KEY));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn missing_key_sends_nothing(pool: PgPool) {
        let server = MockServer::start().await;
        let anthropic = provider(vault(pool, None).await, &server);
        assert_eq!(
            anthropic.complete(&hello()).await.unwrap_err(),
            ProviderError::NotConfigured
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn maps_errors_retries_overload_and_redacts_the_key(pool: PgPool) {
        let vault = vault(pool, Some(KEY)).await;
        let empty = json!({});
        for (status, expected) in [
            (401, ProviderError::CredentialRejected),
            (403, ProviderError::CredentialRejected),
            (429, ProviderError::RateLimited),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status).set_body_json(&empty))
                .mount(&server)
                .await;
            let error = provider(Arc::clone(&vault), &server)
                .complete(&hello())
                .await
                .unwrap_err();
            assert_eq!(error, expected, "status {status}");
        }

        let overloaded = json!({ "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" } });
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(529).set_body_json(&overloaded))
            .mount(&server)
            .await;
        let error = provider(Arc::clone(&vault), &server)
            .complete(&hello())
            .await
            .unwrap_err();
        assert!(
            matches!(error, ProviderError::TemporarilyUnavailable(ref m) if m.contains("Overloaded"))
        );
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            MAX_ATTEMPTS as usize
        );

        let echoed = json!({ "type": "error", "error": {
            "type": "invalid_request_error", "message": format!("bad key {KEY}\nline")
        }});
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(&echoed))
            .mount(&server)
            .await;
        let error = provider(Arc::clone(&vault), &server)
            .complete(&hello())
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(matches!(error, ProviderError::InvalidRequest(_)));
        assert!(!message.contains(KEY));
        assert!(message.contains("[REDACTED]"));
    }
}
