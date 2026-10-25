//! OpenAI provider (Responses API).
//!
//! - The API key is read from the vault on every call and sent only in the
//!   `Authorization` header, marked sensitive.
//! - `store: false`, so OpenAI keeps no copy of requests or responses.
//!   Reasoning is therefore returned encrypted (`reasoning.encrypted_content`)
//!   as a neutral `reasoning` block and must be sent back with its turn.
//! - Only reasoning blocks carrying an OpenAI item id (`rs_...`) are replayed;
//!   reasoning from other providers is skipped.
//! - Prompt caching is automatic. Usage reports cached and written tokens,
//!   billed at their own rates. Reasoning tokens count toward
//!   `max_output_tokens` and are billed as output.
//!
//! Not yet verified against a live call (no paid key available): that
//! `input_tokens` includes cached and written tokens, that GPT-6 models accept
//! effort `low`, and the exact shape of GPT-6 reasoning items.

use std::{sync::Arc, time::Duration};

use reqwest::{
    Client, StatusCode,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue},
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

const API_BASE: &str = "https://api.openai.com";
/// The Responses API rejects `max_output_tokens` below this value.
const MIN_OUTPUT_TOKENS: u32 = 16;
/// Prompts above this size are priced at 2x input and 1.5x output. Published
/// for GPT-5.5 and applied to GPT-6 until OpenAI states otherwise, so spending
/// is never under-counted.
const LONG_PROMPT_TOKENS: u64 = 272_000;
const REASONING_ID_PREFIX: &str = "rs_";

/// Cache writes use the 30-minute rate (1.25x input), the default lifetime.
const fn openai_price(input: u64, output: u64, cache_read: u64) -> Price {
    Price {
        input,
        output,
        cache_read,
        cache_write: input + input / 4,
    }
}

const fn long_prompt(price: Price) -> Price {
    Price {
        input: price.input * 2,
        output: price.output + price.output / 2,
        cache_read: price.cache_read * 2,
        cache_write: price.cache_write * 2,
    }
}

/// Concrete model behind a tier. Prices are micro-dollars per million tokens,
/// from https://developers.openai.com/api/docs/pricing (checked 2026-10-25).
#[derive(Debug)]
struct ModelSpec {
    id: &'static str,
    /// `reasoning.effort` sent with every request.
    effort: &'static str,
    price: Price,
}

impl ModelSpec {
    fn price_for_prompt(&self, prompt_tokens: u64) -> Price {
        if prompt_tokens > LONG_PROMPT_TOKENS {
            long_prompt(self.price)
        } else {
            self.price
        }
    }
}

static LUNA: ModelSpec = ModelSpec {
    id: "gpt-6-luna",
    effort: "low",
    price: openai_price(100_000, 500_000, 10_000),
};

static SOL: ModelSpec = ModelSpec {
    id: "gpt-6.1-sol",
    effort: "low",
    price: openai_price(2_000_000, 10_000_000, 100_000),
};

static ASTRA: ModelSpec = ModelSpec {
    id: "gpt-6-astra",
    effort: "medium",
    price: openai_price(10_000_000, 50_000_000, 1_000_000),
};

static CATALOG: [&ModelSpec; 3] = [&LUNA, &SOL, &ASTRA];

fn model_for(tier: ModelTier) -> &'static ModelSpec {
    match tier {
        ModelTier::Small => &LUNA,
        ModelTier::Standard => &SOL,
        ModelTier::Large => &ASTRA,
    }
}

fn find_model(id: &str) -> Option<&'static ModelSpec> {
    CATALOG.iter().copied().find(|model| model.id == id)
}

pub struct OpenAiProvider {
    vault: Arc<Vault>,
    client: Client,
    base_url: String,
    retry_base: Duration,
}

impl OpenAiProvider {
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
            .load(CredentialProvider::Openai)
            .await
            .map_err(|_| ProviderError::Unavailable("credential store unavailable".to_string()))?
            .ok_or(ProviderError::NotConfigured)?;
        let mut auth = HeaderValue::from_str(&format!("Bearer {}", key.expose_secret()))
            .map_err(|_| ProviderError::CredentialRejected)?;
        auth.set_sensitive(true);

        let response = self
            .client
            .post(format!("{}/v1/responses", self.base_url))
            .header(AUTHORIZATION, auth)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|error| transport_error("openai", &error))?;

        let status = response.status();
        let bytes = read_limited(response).await?;
        if !status.is_success() {
            return Err(map_error(status, &bytes, &key));
        }
        parse_response(&bytes, model)
    }
}

impl Provider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn price(&self, tier: ModelTier, max_input_tokens: u64) -> Result<Price, ProviderError> {
        Ok(model_for(tier).price_for_prompt(max_input_tokens))
    }

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
        Box::pin(with_retry(self.retry_base, "openai", move || {
            self.call_once(request)
        }))
    }
}

/// Appends pending text parts as one message item, keeping block order.
fn flush(items: &mut Vec<Value>, role: &str, parts: &mut Vec<Value>) {
    if !parts.is_empty() {
        items.push(json!({
            "type": "message",
            "role": role,
            "content": std::mem::take(parts),
        }));
    }
}

/// Translates a neutral request into a Responses API body.
fn build_body(request: &CompletionRequest, model: &ModelSpec) -> Result<Value, ProviderError> {
    if request.max_output_tokens < MIN_OUTPUT_TOKENS {
        return Err(ProviderError::InvalidRequest(format!(
            "max_output_tokens must be at least {MIN_OUTPUT_TOKENS} for this provider"
        )));
    }

    let mut items = Vec::new();
    for message in &request.messages {
        let (role, text_type) = match message.role {
            Role::User => ("user", "input_text"),
            Role::Assistant => ("assistant", "output_text"),
        };
        let mut parts = Vec::new();
        for block in &message.content {
            match block {
                ContentBlock::Text { text, .. } => {
                    if !text.is_empty() {
                        parts.push(json!({ "type": text_type, "text": text }));
                    }
                }
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => {
                    flush(&mut items, role, &mut parts);
                    items.push(json!({
                        "type": "function_call",
                        "call_id": id,
                        "name": name,
                        "arguments": input.to_string(),
                    }));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    flush(&mut items, role, &mut parts);
                    let output = if *is_error {
                        format!("[tool error] {content}")
                    } else {
                        content.clone()
                    };
                    items.push(json!({
                        "type": "function_call_output",
                        "call_id": tool_use_id,
                        "output": output,
                    }));
                }
                ContentBlock::Reasoning {
                    text,
                    signature,
                    redacted_data,
                } => {
                    let (Some(id), Some(data)) = (signature, redacted_data) else {
                        continue;
                    };
                    if !id.starts_with(REASONING_ID_PREFIX) {
                        continue;
                    }
                    flush(&mut items, role, &mut parts);
                    let summary = if text.is_empty() {
                        json!([])
                    } else {
                        json!([{ "type": "summary_text", "text": text }])
                    };
                    items.push(json!({
                        "type": "reasoning",
                        "id": id,
                        "encrypted_content": data,
                        "summary": summary,
                    }));
                }
            }
        }
        flush(&mut items, role, &mut parts);
    }

    let mut body = Map::new();
    body.insert("model".to_string(), json!(model.id));
    body.insert("input".to_string(), Value::Array(items));
    body.insert(
        "max_output_tokens".to_string(),
        json!(request.max_output_tokens),
    );
    body.insert("store".to_string(), json!(false));
    body.insert(
        "include".to_string(),
        json!(["reasoning.encrypted_content"]),
    );
    body.insert("reasoning".to_string(), json!({ "effort": model.effort }));

    let instructions: Vec<&str> = request
        .system
        .iter()
        .map(|block| block.text.as_str())
        .filter(|text| !text.is_empty())
        .collect();
    if !instructions.is_empty() {
        body.insert("instructions".to_string(), json!(instructions.join("\n\n")));
    }

    if !request.tools.is_empty() {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                    "strict": false,
                })
            })
            .collect();
        body.insert("tools".to_string(), Value::Array(tools));
    }
    Ok(Value::Object(body))
}

#[derive(Deserialize)]
struct ResponsesBody {
    #[serde(default)]
    output: Vec<OutputItem>,
    status: Option<String>,
    incomplete_details: Option<IncompleteDetails>,
    #[serde(default)]
    usage: OpenAiUsage,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OutputItem {
    Message {
        #[serde(default)]
        content: Vec<MessagePart>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        #[serde(default)]
        arguments: String,
    },
    Reasoning {
        id: String,
        #[serde(default)]
        summary: Vec<SummaryPart>,
        encrypted_content: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MessagePart {
    OutputText {
        text: String,
    },
    Refusal {
        refusal: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct SummaryPart {
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct IncompleteDetails {
    reason: Option<String>,
}

/// `input_tokens` includes cached and newly cached tokens.
#[derive(Deserialize, Default)]
#[serde(default)]
struct OpenAiUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    input_tokens_details: Option<InputDetails>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct InputDetails {
    cached_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
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

/// Arguments arrive as a JSON string. Invalid JSON is passed on under a fixed
/// key so the caller's tool validation rejects it instead of guessing.
fn parse_arguments(arguments: &str) -> Value {
    if arguments.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(arguments).unwrap_or_else(|_| json!({ "_unparsed_arguments": arguments }))
}

fn parse_response(body: &[u8], model: &ModelSpec) -> Result<CompletionResponse, ProviderError> {
    let parsed: ResponsesBody = serde_json::from_slice(body).map_err(|_| {
        ProviderError::Unavailable("unexpected response from the provider".to_string())
    })?;

    let mut content = Vec::new();
    let mut refused = false;
    for item in parsed.output {
        match item {
            OutputItem::Message { content: parts } => {
                for part in parts {
                    match part {
                        MessagePart::OutputText { text } => content.push(ContentBlock::Text {
                            text,
                            signature: None,
                        }),
                        MessagePart::Refusal { refusal } => {
                            refused = true;
                            content.push(ContentBlock::Text {
                                text: refusal,
                                signature: None,
                            });
                        }
                        MessagePart::Unknown => {}
                    }
                }
            }
            OutputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => content.push(ContentBlock::ToolUse {
                id: call_id,
                name,
                input: parse_arguments(&arguments),
                signature: None,
            }),
            OutputItem::Reasoning {
                id,
                summary,
                encrypted_content,
            } => content.push(ContentBlock::Reasoning {
                text: summary
                    .into_iter()
                    .map(|part| part.text)
                    .collect::<Vec<_>>()
                    .join("\n"),
                signature: Some(id),
                redacted_data: encrypted_content,
            }),
            OutputItem::Unknown => {}
        }
    }

    let has_tool_call = content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolUse { .. }));
    let reason = parsed.incomplete_details.and_then(|details| details.reason);
    let stop_reason = if has_tool_call {
        StopReason::ToolUse
    } else if refused {
        StopReason::Refused
    } else {
        match (parsed.status.as_deref(), reason.as_deref()) {
            (Some("completed") | None, _) => StopReason::EndTurn,
            (Some("incomplete"), Some("max_output_tokens")) => StopReason::MaxTokens,
            (Some("incomplete"), Some("content_filter")) => StopReason::Refused,
            _ => StopReason::Other,
        }
    };

    let usage = parsed.usage;
    let details = usage.input_tokens_details.unwrap_or_default();
    let cached = details.cached_tokens.unwrap_or(0);
    let written = details.cache_write_tokens.unwrap_or(0);
    let total_input = usage.input_tokens.unwrap_or(0);
    Ok(CompletionResponse {
        provider: "openai".to_string(),
        model: model.id.to_string(),
        content,
        stop_reason,
        usage: Usage {
            input_tokens: total_input.saturating_sub(cached).saturating_sub(written),
            output_tokens: usage.output_tokens.unwrap_or(0),
            cache_read_tokens: cached,
            cache_write_tokens: written,
        },
    })
}

fn map_error(status: StatusCode, body: &[u8], key: &SecretString) -> ProviderError {
    tracing::warn!(status = status.as_u16(), "openai returned an error");
    let message = serde_json::from_slice::<ErrorEnvelope>(body)
        .map(|envelope| envelope.error.message)
        .unwrap_or_default();
    let detail = clean(&message, Some(key));
    match status.as_u16() {
        401 | 403 => ProviderError::CredentialRejected,
        429 => ProviderError::RateLimited,
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

    const KEY: &str = "sk-proj-test-not-a-real-key-0123456789";

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
            "id": "resp_1", "object": "response", "status": "completed", "model": "gpt-6.1-sol",
            "output": [
                { "type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "enc-1" },
                { "type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
                  "content": [{ "type": "output_text", "text": "hello", "annotations": [] }] }
            ],
            "usage": {
                "input_tokens": 1000,
                "input_tokens_details": { "cached_tokens": 600, "cache_write_tokens": 300 },
                "output_tokens": 40,
                "output_tokens_details": { "reasoning_tokens": 30 },
                "total_tokens": 1040
            }
        })
    }

    async fn vault(pool: PgPool, key: Option<&str>) -> Arc<Vault> {
        let master = SecretString::from(BASE64.encode([7u8; 32]));
        let vault = Arc::new(Vault::new(pool, &master).unwrap());
        if let Some(key) = key {
            vault
                .store(
                    CredentialProvider::Openai,
                    &SecretString::from(key.to_string()),
                )
                .await
                .unwrap();
        }
        vault
    }

    fn provider(vault: Arc<Vault>, server: &MockServer) -> OpenAiProvider {
        let mut provider = OpenAiProvider::build(vault, server.uri(), false).unwrap();
        provider.retry_base = Duration::ZERO;
        provider
    }

    #[test]
    fn translates_instructions_items_tools_and_reasoning() {
        let mut req = request(
            ModelTier::Standard,
            vec![
                user(vec![text("find x")]),
                assistant(vec![
                    ContentBlock::Reasoning {
                        text: String::new(),
                        signature: Some("rs_1".to_string()),
                        redacted_data: Some("enc-1".to_string()),
                    },
                    ContentBlock::Reasoning {
                        text: String::new(),
                        signature: Some("anthropic-signature".to_string()),
                        redacted_data: None,
                    },
                    text("let me search"),
                    ContentBlock::ToolUse {
                        id: "call_1".to_string(),
                        name: "search".to_string(),
                        input: json!({ "q": "x" }),
                        signature: None,
                    },
                ]),
                user(vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
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
                text: "today".to_string(),
                cache: false,
            },
        ];
        req.tools.push(ToolSpec {
            name: "search".to_string(),
            description: "Search documents".to_string(),
            input_schema: json!({ "type": "object" }),
        });

        let body = build_body(&req, &SOL).unwrap();
        assert_eq!(body["model"], "gpt-6.1-sol");
        assert_eq!(body["store"], false);
        assert_eq!(body["include"][0], "reasoning.encrypted_content");
        assert_eq!(body["reasoning"]["effort"], "low");
        assert_eq!(body["max_output_tokens"], 256);
        assert_eq!(body["instructions"], "company rules\n\ntoday");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["parameters"]["type"], "object");

        let items = body["input"].as_array().unwrap();
        assert_eq!(items.len(), 5, "the foreign reasoning block is skipped");
        assert_eq!(items[0]["content"][0]["type"], "input_text");
        assert_eq!(items[1]["type"], "reasoning");
        assert_eq!(items[1]["encrypted_content"], "enc-1");
        assert_eq!(items[2]["role"], "assistant");
        assert_eq!(items[2]["content"][0]["type"], "output_text");
        assert_eq!(items[3]["type"], "function_call");
        assert_eq!(items[3]["arguments"], "{\"q\":\"x\"}");
        assert_eq!(items[4]["type"], "function_call_output");
        assert_eq!(items[4]["output"], "[tool error] boom");
    }

    #[test]
    fn rejects_output_limit_below_the_api_minimum() {
        let mut req = hello();
        req.max_output_tokens = 15;
        assert!(matches!(
            build_body(&req, &SOL),
            Err(ProviderError::InvalidRequest(_))
        ));
    }

    #[test]
    fn parses_items_usage_and_stop_reasons() {
        let response = parse_response(&serde_json::to_vec(&ok_body()).unwrap(), &SOL).unwrap();
        assert_eq!(response.provider, "openai");
        assert_eq!(response.stop_reason, StopReason::EndTurn);
        assert_eq!(
            response.content,
            vec![
                ContentBlock::Reasoning {
                    text: String::new(),
                    signature: Some("rs_1".to_string()),
                    redacted_data: Some("enc-1".to_string()),
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
                input_tokens: 100,
                output_tokens: 40,
                cache_read_tokens: 600,
                cache_write_tokens: 300,
            }
        );

        let calls = json!({
            "status": "completed",
            "output": [
                { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "search", "arguments": "{\"q\":\"x\"}" },
                { "type": "function_call", "id": "fc_2", "call_id": "call_2", "name": "search", "arguments": "not json" },
                { "type": "web_search_call", "id": "ws_1" }
            ],
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        });
        let response = parse_response(&serde_json::to_vec(&calls).unwrap(), &SOL).unwrap();
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        assert_eq!(response.content.len(), 2, "unknown item types are skipped");
        assert!(matches!(
            &response.content[0],
            ContentBlock::ToolUse { id, input, .. } if id == "call_1" && input["q"] == "x"
        ));
        assert!(matches!(
            &response.content[1],
            ContentBlock::ToolUse { input, .. } if input["_unparsed_arguments"] == "not json"
        ));

        let refusal = json!({ "status": "completed", "output": [
            { "type": "message", "content": [{ "type": "refusal", "refusal": "I can't help with that." }] }
        ]});
        let response = parse_response(&serde_json::to_vec(&refusal).unwrap(), &SOL).unwrap();
        assert_eq!(response.stop_reason, StopReason::Refused);

        for (reason, expected) in [
            ("max_output_tokens", StopReason::MaxTokens),
            ("content_filter", StopReason::Refused),
        ] {
            let body = json!({ "status": "incomplete", "incomplete_details": { "reason": reason }, "output": [] });
            let response = parse_response(&serde_json::to_vec(&body).unwrap(), &SOL).unwrap();
            assert_eq!(response.stop_reason, expected, "{reason}");
        }
    }

    #[test]
    fn long_prompts_use_the_higher_price() {
        assert_eq!(SOL.price_for_prompt(272_000).input, 2_000_000);
        let long = SOL.price_for_prompt(272_001);
        assert_eq!(long.input, 4_000_000);
        assert_eq!(long.output, 15_000_000);
        assert_eq!(LUNA.price.cache_write, 125_000);
        assert_eq!(ASTRA.price.cache_write, 12_500_000);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn cost_follows_the_actual_prompt_length(pool: PgPool) {
        let server = MockServer::start().await;
        let openai = provider(vault(pool, None).await, &server);
        let reserved = openai.price(ModelTier::Small, 1_000_000).unwrap();
        assert_eq!(reserved.input, 200_000);
        let short = CompletionResponse {
            provider: "openai".to_string(),
            model: "gpt-6-luna".to_string(),
            content: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 10_000,
                ..Usage::default()
            },
        };
        assert_eq!(openai.cost_micros(&short, &reserved), 1_000);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn sends_key_only_in_authorization_header(pool: PgPool) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", format!("Bearer {KEY}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;

        let openai = provider(vault(pool, Some(KEY)).await, &server);
        let response = openai.complete(&hello()).await.unwrap();
        assert_eq!(response.model, "gpt-6.1-sol");

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].url.query().is_none());
        assert!(!String::from_utf8_lossy(&requests[0].body).contains(KEY));
        let sent: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(sent["store"], false);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn missing_key_sends_nothing(pool: PgPool) {
        let server = MockServer::start().await;
        let openai = provider(vault(pool, None).await, &server);
        assert_eq!(
            openai.complete(&hello()).await.unwrap_err(),
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

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503).set_body_json(&empty))
            .mount(&server)
            .await;
        let error = provider(Arc::clone(&vault), &server)
            .complete(&hello())
            .await
            .unwrap_err();
        assert!(matches!(error, ProviderError::TemporarilyUnavailable(_)));
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            MAX_ATTEMPTS as usize
        );

        let echoed = json!({ "error": { "message": format!("Incorrect API key provided: {KEY}"), "type": "invalid_request_error" } });
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
