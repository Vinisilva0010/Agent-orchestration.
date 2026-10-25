//! Google Gemini provider (Gemini Developer API, `generateContent`).
//!
//! - The API key is read from the vault on every call, so a key replaced by an
//!   admin takes effect immediately. It is sent only in the `x-goog-api-key`
//!   header, marked sensitive, and never in the URL or the body.
//! - Gemini 3 and later always think. Thinking tokens are billed as output and
//!   count against `maxOutputTokens`, so the reservation made before the call
//!   already covers them. Each model uses the lowest thinking level it accepts.
//! - Function calls carry a thought signature that must be sent back in the
//!   next request. It travels in the neutral `signature` field.
//! - Provider error bodies are never passed through raw: only the message is
//!   kept, truncated, without control characters and with the key redacted.

use std::{collections::HashMap, fmt::Write as _, sync::Arc, time::Duration};

use reqwest::{
    Client, StatusCode,
    header::{CONTENT_TYPE, HeaderValue},
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::http::{RETRY_BASE, build_client, clean, read_limited, with_retry};
use super::{
    CompletionRequest, CompletionResponse, ContentBlock, ModelTier, Provider, ProviderError,
    ProviderFuture, Role, StopReason, Usage,
};
use crate::{
    pricing::{Price, PriceChange, price_on, today_yyyymmdd},
    vault::{CredentialProvider, Vault},
};

const API_BASE: &str = "https://generativelanguage.googleapis.com";
const API_KEY_HEADER: &str = "x-goog-api-key";

/// Implicit caching has no write surcharge, so cache writes cost the input rate.
const fn gemini_price(input: u64, output: u64, cache_read: u64) -> Price {
    Price {
        input,
        output,
        cache_read,
        cache_write: input,
    }
}

/// Concrete model behind a tier. Prices are micro-dollars per million tokens,
/// from https://ai.google.dev/gemini-api/docs/pricing (checked 2026-10-25),
/// including the changes Google has already announced.
#[derive(Debug)]
struct ModelSpec {
    id: &'static str,
    thinking_level: &'static str,
    prices: &'static [PriceChange],
}

static SMALL_MODEL: ModelSpec = ModelSpec {
    id: "gemini-3.1-flash-lite",
    thinking_level: "MINIMAL",
    prices: &[PriceChange {
        from_yyyymmdd: 0,
        price: gemini_price(250_000, 1_500_000, 25_000),
    }],
};

static STANDARD_MODEL: ModelSpec = ModelSpec {
    id: "gemini-3.8-flash",
    thinking_level: "LOW",
    prices: &[
        PriceChange {
            from_yyyymmdd: 0,
            price: gemini_price(750_000, 3_750_000, 75_000),
        },
        PriceChange {
            from_yyyymmdd: 20270101,
            price: gemini_price(1_500_000, 7_500_000, 150_000),
        },
    ],
};

static LARGE_MODEL: ModelSpec = ModelSpec {
    id: "gemini-4-argon",
    thinking_level: "LOW",
    prices: &[
        PriceChange {
            from_yyyymmdd: 0,
            price: gemini_price(2_000_000, 10_000_000, 100_000),
        },
        PriceChange {
            from_yyyymmdd: 20261201,
            price: gemini_price(4_000_000, 20_000_000, 200_000),
        },
    ],
};

static FLASH_LITE_3_5: ModelSpec = ModelSpec {
    id: "gemini-3.5-flash-lite",
    thinking_level: "MINIMAL",
    prices: &[PriceChange {
        from_yyyymmdd: 0,
        price: gemini_price(300_000, 2_500_000, 30_000),
    }],
};

static FLASH_3_6: ModelSpec = ModelSpec {
    id: "gemini-3.6-flash",
    thinking_level: "LOW",
    prices: &[
        PriceChange {
            from_yyyymmdd: 0,
            price: gemini_price(750_000, 3_750_000, 75_000),
        },
        PriceChange {
            from_yyyymmdd: 20270101,
            price: gemini_price(1_500_000, 7_500_000, 150_000),
        },
    ],
};

/// Every model the gateway can price. Only these can be configured.
static CATALOG: [&ModelSpec; 5] = [
    &SMALL_MODEL,
    &FLASH_LITE_3_5,
    &FLASH_3_6,
    &STANDARD_MODEL,
    &LARGE_MODEL,
];

fn find_model(id: &str) -> Option<&'static ModelSpec> {
    CATALOG.iter().copied().find(|model| model.id == id)
}

/// Model used for each tier, chosen from [`CATALOG`].
#[derive(Debug, Clone, Copy)]
pub struct TierModels {
    small: &'static ModelSpec,
    standard: &'static ModelSpec,
    large: &'static ModelSpec,
}

impl Default for TierModels {
    fn default() -> Self {
        Self {
            small: &SMALL_MODEL,
            standard: &STANDARD_MODEL,
            large: &LARGE_MODEL,
        }
    }
}

impl TierModels {
    /// Overrides the default model of each tier. `None` or an empty value keeps
    /// the default. Fails with the first id not in the catalog.
    pub fn from_ids(
        small: Option<&str>,
        standard: Option<&str>,
        large: Option<&str>,
    ) -> Result<Self, String> {
        let defaults = Self::default();
        let pick = |id: Option<&str>, default: &'static ModelSpec| match id.map(str::trim) {
            None | Some("") => Ok(default),
            Some(id) => find_model(id).ok_or_else(|| id.to_string()),
        };
        Ok(Self {
            small: pick(small, defaults.small)?,
            standard: pick(standard, defaults.standard)?,
            large: pick(large, defaults.large)?,
        })
    }

    /// Model ids for small, standard and large, in that order.
    pub fn ids(&self) -> [&'static str; 3] {
        [self.small.id, self.standard.id, self.large.id]
    }

    fn for_tier(&self, tier: ModelTier) -> &'static ModelSpec {
        match tier {
            ModelTier::Small => self.small,
            ModelTier::Standard => self.standard,
            ModelTier::Large => self.large,
        }
    }
}

pub struct GeminiProvider {
    vault: Arc<Vault>,
    client: Client,
    base_url: String,
    retry_base: Duration,
    models: TierModels,
}

impl GeminiProvider {
    pub fn new(vault: Arc<Vault>, models: TierModels) -> Result<Self, ProviderError> {
        let mut provider = Self::build(vault, API_BASE.to_string(), true)?;
        provider.models = models;
        Ok(provider)
    }

    fn build(vault: Arc<Vault>, base_url: String, https_only: bool) -> Result<Self, ProviderError> {
        let client = build_client(https_only)?;
        Ok(Self {
            vault,
            client,
            base_url,
            retry_base: RETRY_BASE,
            models: TierModels::default(),
        })
    }

    async fn call_once(
        &self,
        request: &CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        request.validate()?;
        let model = self.models.for_tier(request.tier);
        let body = serde_json::to_vec(&build_body(request, model)?).map_err(|_| {
            ProviderError::InvalidRequest("request could not be encoded".to_string())
        })?;

        let key = self
            .vault
            .load(CredentialProvider::Gemini)
            .await
            .map_err(|_| ProviderError::Unavailable("credential store unavailable".to_string()))?
            .ok_or(ProviderError::NotConfigured)?;
        let mut key_header = HeaderValue::from_str(key.expose_secret())
            .map_err(|_| ProviderError::CredentialRejected)?;
        key_header.set_sensitive(true);

        let url = format!(
            "{}/v1beta/models/{}:generateContent",
            self.base_url, model.id
        );
        let response = self
            .client
            .post(url)
            .header(API_KEY_HEADER, key_header)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|error| {
                tracing::warn!(
                    timeout = error.is_timeout(),
                    connect = error.is_connect(),
                    "gemini request failed"
                );
                if error.is_connect() {
                    ProviderError::TemporarilyUnavailable(
                        "could not connect to the provider".to_string(),
                    )
                } else {
                    ProviderError::Unavailable("could not reach the provider".to_string())
                }
            })?;

        let status = response.status();
        let bytes = read_limited(response).await?;
        if !status.is_success() {
            return Err(map_error(status, &bytes, &key));
        }
        parse_response(&bytes, model)
    }
}

impl GeminiProvider {
    async fn call_with_retry(
        &self,
        request: &CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        with_retry(self.retry_base, "gemini", || self.call_once(request)).await
    }
}

impl Provider for GeminiProvider {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn price(&self, tier: ModelTier, _max_input_tokens: u64) -> Result<Price, ProviderError> {
        let model = self.models.for_tier(tier);
        price_on(model.prices, today_yyyymmdd()).ok_or_else(|| {
            ProviderError::Unavailable(format!("no price is known for {}", model.id))
        })
    }

    fn complete<'a>(&'a self, request: &'a CompletionRequest) -> ProviderFuture<'a> {
        Box::pin(self.call_with_retry(request))
    }
}

fn attach_signature(part: &mut Map<String, Value>, signature: Option<&String>) {
    if let Some(signature) = signature {
        part.insert(
            "thoughtSignature".to_string(),
            Value::String(signature.clone()),
        );
    }
}

/// Translates a neutral request into a `generateContent` body.
fn build_body(request: &CompletionRequest, model: &ModelSpec) -> Result<Value, ProviderError> {
    let mut tool_names: HashMap<&str, &str> = HashMap::new();
    let mut contents = Vec::with_capacity(request.messages.len());

    for message in &request.messages {
        let mut parts = Vec::with_capacity(message.content.len());
        for block in &message.content {
            let mut part = Map::new();
            match block {
                ContentBlock::Text { text, signature } => {
                    part.insert("text".to_string(), Value::String(text.clone()));
                    attach_signature(&mut part, signature.as_ref());
                }
                ContentBlock::ToolUse {
                    id,
                    name,
                    input,
                    signature,
                } => {
                    if !input.is_object() {
                        return Err(ProviderError::InvalidRequest(format!(
                            "tool_use {id}: input must be a JSON object"
                        )));
                    }
                    tool_names.insert(id.as_str(), name.as_str());
                    part.insert(
                        "functionCall".to_string(),
                        json!({ "name": name, "args": input }),
                    );
                    attach_signature(&mut part, signature.as_ref());
                }
                ContentBlock::Reasoning { .. } => continue,
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let name = tool_names.get(tool_use_id.as_str()).ok_or_else(|| {
                        ProviderError::InvalidRequest(format!(
                            "tool_result refers to an unknown tool_use_id: {tool_use_id}"
                        ))
                    })?;
                    let mut response = Map::new();
                    let field = if *is_error { "error" } else { "result" };
                    response.insert(field.to_string(), Value::String(content.clone()));
                    part.insert(
                        "functionResponse".to_string(),
                        json!({ "name": name, "response": response }),
                    );
                }
            }
            parts.push(Value::Object(part));
        }
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "model",
        };
        contents.push(json!({ "role": role, "parts": parts }));
    }

    let mut body = Map::new();
    body.insert("contents".to_string(), Value::Array(contents));
    if !request.system.is_empty() {
        let parts: Vec<Value> = request
            .system
            .iter()
            .map(|block| json!({ "text": block.text }))
            .collect();
        body.insert("systemInstruction".to_string(), json!({ "parts": parts }));
    }
    if !request.tools.is_empty() {
        let declarations: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parametersJsonSchema": tool.input_schema,
                })
            })
            .collect();
        body.insert(
            "tools".to_string(),
            json!([{ "functionDeclarations": declarations }]),
        );
    }
    body.insert(
        "generationConfig".to_string(),
        json!({
            "maxOutputTokens": request.max_output_tokens,
            "thinkingConfig": { "thinkingLevel": model.thinking_level },
        }),
    );
    Ok(Value::Object(body))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    usage_metadata: Option<UsageMetadata>,
    prompt_feedback: Option<PromptFeedback>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    content: Option<CandidateContent>,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct CandidateContent {
    #[serde(default)]
    parts: Vec<Part>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Part {
    text: Option<String>,
    function_call: Option<FunctionCall>,
    #[serde(default)]
    thought: bool,
    thought_signature: Option<String>,
}

#[derive(Deserialize)]
struct FunctionCall {
    id: Option<String>,
    name: String,
    #[serde(default)]
    args: Value,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct UsageMetadata {
    prompt_token_count: u64,
    cached_content_token_count: u64,
    candidates_token_count: u64,
    thoughts_token_count: u64,
    tool_use_prompt_token_count: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromptFeedback {
    block_reason: Option<String>,
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

/// `promptTokenCount` includes cached tokens, which are billed at the cache
/// rate. Output includes thinking tokens.
fn to_usage(metadata: &UsageMetadata) -> Usage {
    let prompt = metadata
        .prompt_token_count
        .saturating_add(metadata.tool_use_prompt_token_count);
    let cached = metadata.cached_content_token_count.min(prompt);
    Usage {
        input_tokens: prompt - cached,
        output_tokens: metadata
            .candidates_token_count
            .saturating_add(metadata.thoughts_token_count),
        cache_read_tokens: cached,
        cache_write_tokens: 0,
    }
}

fn random_call_id() -> Result<String, ProviderError> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|_| {
        ProviderError::Unavailable("random number generator unavailable".to_string())
    })?;
    let mut id = String::from("call_");
    for byte in bytes {
        let _ = write!(id, "{byte:02x}");
    }
    Ok(id)
}

fn parse_response(body: &[u8], model: &ModelSpec) -> Result<CompletionResponse, ProviderError> {
    let parsed: GenerateResponse = serde_json::from_slice(body).map_err(|_| {
        ProviderError::Unavailable("unexpected response from the provider".to_string())
    })?;
    let usage = parsed
        .usage_metadata
        .as_ref()
        .map(to_usage)
        .unwrap_or_default();

    let Some(candidate) = parsed.candidates.into_iter().next() else {
        let reason = parsed
            .prompt_feedback
            .and_then(|feedback| feedback.block_reason)
            .unwrap_or_else(|| "no candidates".to_string());
        return Err(ProviderError::InvalidRequest(format!(
            "the provider returned no answer: {}",
            clean(&reason, None)
        )));
    };

    let mut content = Vec::new();
    for part in candidate.content.map(|c| c.parts).unwrap_or_default() {
        if part.thought {
            continue;
        }
        if let Some(call) = part.function_call {
            let id = match call.id {
                Some(id) if !id.is_empty() => id,
                _ => random_call_id()?,
            };
            let input = if call.args.is_null() {
                json!({})
            } else {
                call.args
            };
            content.push(ContentBlock::ToolUse {
                id,
                name: call.name,
                input,
                signature: part.thought_signature,
            });
        } else if let Some(text) = part.text
            && (!text.is_empty() || part.thought_signature.is_some())
        {
            content.push(ContentBlock::Text {
                text,
                signature: part.thought_signature,
            });
        }
    }

    let has_tool_call = content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolUse { .. }));
    let stop_reason = if has_tool_call {
        StopReason::ToolUse
    } else {
        match candidate.finish_reason.as_deref() {
            Some("STOP") | None => StopReason::EndTurn,
            Some("MAX_TOKENS") => StopReason::MaxTokens,
            Some(
                "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII"
                | "IMAGE_SAFETY" | "LANGUAGE",
            ) => StopReason::Refused,
            Some(_) => StopReason::Other,
        }
    };

    Ok(CompletionResponse {
        provider: "gemini".to_string(),
        model: model.id.to_string(),
        content,
        stop_reason,
        usage,
    })
}

/// Google answers 400 with reason `API_KEY_INVALID` for a bad key, so that case
/// is detected from the body as well as from 401 and 403.
fn map_error(status: StatusCode, body: &[u8], key: &SecretString) -> ProviderError {
    tracing::warn!(status = status.as_u16(), "gemini returned an error");
    let key_invalid = String::from_utf8_lossy(body).contains("API_KEY_INVALID");
    if key_invalid || status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return ProviderError::CredentialRejected;
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return ProviderError::RateLimited;
    }
    if status.is_client_error() {
        let message = serde_json::from_slice::<ErrorEnvelope>(body)
            .map(|envelope| envelope.error.message)
            .unwrap_or_default();
        return ProviderError::InvalidRequest(format!(
            "the provider rejected the request: {}",
            clean(&message, Some(key))
        ));
    }
    let message = serde_json::from_slice::<ErrorEnvelope>(body)
        .map(|envelope| envelope.error.message)
        .unwrap_or_default();
    ProviderError::TemporarilyUnavailable(format!(
        "the provider returned status {}: {}",
        status.as_u16(),
        clean(&message, Some(key))
    ))
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
    use crate::provider::{
        Message, SystemBlock, ToolSpec,
        http::{MAX_ATTEMPTS, MAX_RESPONSE_BYTES},
    };

    const KEY: &str = "AIza-test-not-a-real-key-0123456789";
    const STANDARD_PATH: &str = "/v1beta/models/gemini-3.8-flash:generateContent";

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

    fn request(messages: Vec<Message>) -> CompletionRequest {
        CompletionRequest {
            tier: ModelTier::Standard,
            system: vec![],
            messages,
            tools: vec![],
            max_output_tokens: 256,
        }
    }

    fn hello() -> CompletionRequest {
        request(vec![user(vec![text("hi")])])
    }

    fn ok_body() -> Value {
        json!({
            "candidates": [{
                "content": { "role": "model", "parts": [{ "text": "hello", "thoughtSignature": "sig-text" }] },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 100,
                "cachedContentTokenCount": 40,
                "candidatesTokenCount": 10,
                "thoughtsTokenCount": 30,
                "totalTokenCount": 140
            },
            "modelVersion": "gemini-3.8-flash"
        })
    }

    async fn vault(pool: PgPool, key: Option<&str>) -> Arc<Vault> {
        let master = SecretString::from(BASE64.encode([7u8; 32]));
        let vault = Arc::new(Vault::new(pool, &master).unwrap());
        if let Some(key) = key {
            vault
                .store(
                    CredentialProvider::Gemini,
                    &SecretString::from(key.to_string()),
                )
                .await
                .unwrap();
        }
        vault
    }

    fn provider(vault: Arc<Vault>, server: &MockServer) -> GeminiProvider {
        let mut provider = GeminiProvider::build(vault, server.uri(), false).unwrap();
        provider.retry_base = Duration::ZERO;
        provider
    }

    #[test]
    fn translates_roles_system_tools_and_config() {
        let mut req = request(vec![
            user(vec![text("find x")]),
            assistant(vec![ContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "search".to_string(),
                input: json!({ "q": "x" }),
                signature: Some("sig-a".to_string()),
            }]),
            user(vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: "found".to_string(),
                is_error: false,
            }]),
        ]);
        req.system.push(SystemBlock {
            text: "rules".to_string(),
            cache: true,
        });
        req.tools.push(ToolSpec {
            name: "search".to_string(),
            description: "Search documents".to_string(),
            input_schema: json!({ "type": "object" }),
        });

        let body = build_body(&req, &STANDARD_MODEL).unwrap();
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "rules");
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][1]["role"], "model");
        let call = &body["contents"][1]["parts"][0];
        assert_eq!(call["functionCall"]["name"], "search");
        assert_eq!(call["functionCall"]["args"]["q"], "x");
        assert_eq!(call["thoughtSignature"], "sig-a");
        let result = &body["contents"][2]["parts"][0]["functionResponse"];
        assert_eq!(result["name"], "search");
        assert_eq!(result["response"]["result"], "found");
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"]["type"],
            "object"
        );
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 256);
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "LOW"
        );
    }

    #[test]
    fn rejects_tool_result_without_matching_call() {
        let req = request(vec![user(vec![ContentBlock::ToolResult {
            tool_use_id: "missing".to_string(),
            content: "x".to_string(),
            is_error: false,
        }])]);
        assert!(matches!(
            build_body(&req, &STANDARD_MODEL),
            Err(ProviderError::InvalidRequest(_))
        ));
    }

    #[test]
    fn rejects_tool_input_that_is_not_an_object() {
        let req = request(vec![
            user(vec![text("x")]),
            assistant(vec![ContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "search".to_string(),
                input: json!("not an object"),
                signature: None,
            }]),
        ]);
        assert!(matches!(
            build_body(&req, &STANDARD_MODEL),
            Err(ProviderError::InvalidRequest(_))
        ));
    }

    #[test]
    fn parses_text_signature_and_usage() {
        let bytes = serde_json::to_vec(&ok_body()).unwrap();
        let response = parse_response(&bytes, &STANDARD_MODEL).unwrap();
        assert_eq!(response.provider, "gemini");
        assert_eq!(response.model, "gemini-3.8-flash");
        assert_eq!(response.stop_reason, StopReason::EndTurn);
        assert_eq!(
            response.content,
            vec![ContentBlock::Text {
                text: "hello".to_string(),
                signature: Some("sig-text".to_string()),
            }]
        );
        assert_eq!(
            response.usage,
            Usage {
                input_tokens: 60,
                output_tokens: 40,
                cache_read_tokens: 40,
                cache_write_tokens: 0,
            }
        );
    }

    #[test]
    fn parses_function_calls_and_skips_thoughts() {
        let body = json!({
            "candidates": [{
                "content": { "parts": [
                    { "text": "internal reasoning", "thought": true },
                    { "functionCall": { "name": "search", "args": { "q": "x" } }, "thoughtSignature": "sig-a" },
                    { "functionCall": { "name": "search", "args": { "q": "y" } } }
                ] },
                "finishReason": "STOP"
            }]
        });
        let response =
            parse_response(&serde_json::to_vec(&body).unwrap(), &STANDARD_MODEL).unwrap();
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        assert_eq!(response.content.len(), 2);
        let ids: Vec<&str> = response
            .content
            .iter()
            .map(|block| match block {
                ContentBlock::ToolUse { id, .. } => id.as_str(),
                other => panic!("unexpected block {other:?}"),
            })
            .collect();
        assert!(ids.iter().all(|id| id.starts_with("call_")));
        assert_ne!(ids[0], ids[1]);
        assert!(matches!(
            &response.content[0],
            ContentBlock::ToolUse { signature: Some(s), .. } if s == "sig-a"
        ));
        assert!(matches!(
            &response.content[1],
            ContentBlock::ToolUse {
                signature: None,
                ..
            }
        ));
    }

    #[test]
    fn maps_finish_reasons() {
        for (reason, expected) in [
            ("MAX_TOKENS", StopReason::MaxTokens),
            ("SAFETY", StopReason::Refused),
            ("MALFORMED_FUNCTION_CALL", StopReason::Other),
        ] {
            let body =
                json!({ "candidates": [{ "content": { "parts": [] }, "finishReason": reason }] });
            let response =
                parse_response(&serde_json::to_vec(&body).unwrap(), &STANDARD_MODEL).unwrap();
            assert_eq!(response.stop_reason, expected, "{reason}");
        }
    }

    #[test]
    fn reports_blocked_prompts() {
        let body = json!({ "promptFeedback": { "blockReason": "SAFETY" } });
        let error =
            parse_response(&serde_json::to_vec(&body).unwrap(), &STANDARD_MODEL).unwrap_err();
        assert!(matches!(&error, ProviderError::InvalidRequest(m) if m.contains("SAFETY")));
    }

    #[test]
    fn prices_follow_announced_changes() {
        let on = |model: &ModelSpec, day| price_on(model.prices, day).unwrap().input;
        assert_eq!(on(&STANDARD_MODEL, 20261231), 750_000);
        assert_eq!(on(&STANDARD_MODEL, 20270101), 1_500_000);
        assert_eq!(on(&LARGE_MODEL, 20261130), 2_000_000);
        assert_eq!(on(&LARGE_MODEL, 20261201), 4_000_000);
        assert_eq!(on(&SMALL_MODEL, 20300101), 250_000);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn sends_key_only_in_header(pool: PgPool) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(STANDARD_PATH))
            .and(header(API_KEY_HEADER, KEY))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;

        let gemini = provider(vault(pool, Some(KEY)).await, &server);
        let response = gemini.complete(&hello()).await.unwrap();
        assert_eq!(response.stop_reason, StopReason::EndTurn);

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].url.query().is_none());
        assert!(!requests[0].url.as_str().contains(KEY));
        assert!(!String::from_utf8_lossy(&requests[0].body).contains(KEY));
        let sent: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(sent["contents"][0]["parts"][0]["text"], "hi");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn missing_key_sends_nothing(pool: PgPool) {
        let server = MockServer::start().await;
        let gemini = provider(vault(pool, None).await, &server);
        assert_eq!(
            gemini.complete(&hello()).await.unwrap_err(),
            ProviderError::NotConfigured
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn maps_provider_errors_without_leaking_the_key(pool: PgPool) {
        let vault = vault(pool, Some(KEY)).await;
        let invalid_key = json!({ "error": {
            "code": 400, "message": "API key not valid.", "status": "INVALID_ARGUMENT",
            "details": [{ "reason": "API_KEY_INVALID" }]
        }});
        let echoed =
            json!({ "error": { "code": 400, "message": format!("bad request for {KEY}\nline") } });
        let empty = json!({});

        let cases = [
            (400, &invalid_key, ProviderError::CredentialRejected),
            (403, &empty, ProviderError::CredentialRejected),
            (429, &empty, ProviderError::RateLimited),
            (
                503,
                &empty,
                ProviderError::TemporarilyUnavailable(
                    "the provider returned status 503: no details".to_string(),
                ),
            ),
        ];
        for (status, body, expected) in cases {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status).set_body_json(body))
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
        assert!(!message.contains('\n'));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn rejects_oversized_responses(pool: PgPool) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("x".repeat(MAX_RESPONSE_BYTES + 1)),
            )
            .mount(&server)
            .await;
        let error = provider(vault(pool, Some(KEY)).await, &server)
            .complete(&hello())
            .await
            .unwrap_err();
        assert!(matches!(error, ProviderError::Unavailable(_)));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn retries_temporary_failures(pool: PgPool) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .with_priority(2)
            .mount(&server)
            .await;
        let response = provider(vault(pool, Some(KEY)).await, &server)
            .complete(&hello())
            .await
            .unwrap();
        assert_eq!(response.stop_reason, StopReason::EndTurn);
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn gives_up_after_max_attempts(pool: PgPool) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let error = provider(vault(pool, Some(KEY)).await, &server)
            .complete(&hello())
            .await
            .unwrap_err();
        assert!(matches!(error, ProviderError::TemporarilyUnavailable(_)));
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            MAX_ATTEMPTS as usize
        );
    }

    #[test]
    fn tier_models_accept_only_catalog_ids() {
        assert_eq!(
            TierModels::default().ids(),
            [
                "gemini-3.1-flash-lite",
                "gemini-3.8-flash",
                "gemini-4-argon"
            ]
        );
        let custom = TierModels::from_ids(Some("gemini-3.5-flash-lite"), Some(" "), None).unwrap();
        assert_eq!(
            custom.ids(),
            [
                "gemini-3.5-flash-lite",
                "gemini-3.8-flash",
                "gemini-4-argon"
            ]
        );
        assert_eq!(
            TierModels::from_ids(None, Some("gemini-9-unknown"), None).unwrap_err(),
            "gemini-9-unknown"
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn uses_the_configured_model_for_a_tier(pool: PgPool) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-3.5-flash-lite:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;
        let mut gemini = provider(vault(pool, Some(KEY)).await, &server);
        gemini.models = TierModels::from_ids(Some("gemini-3.5-flash-lite"), None, None).unwrap();
        let mut req = hello();
        req.tier = ModelTier::Small;
        let response = gemini.complete(&req).await.unwrap();
        assert_eq!(response.model, "gemini-3.5-flash-lite");
        assert_eq!(gemini.price(ModelTier::Small, 0).unwrap().input, 300_000);
    }
}
