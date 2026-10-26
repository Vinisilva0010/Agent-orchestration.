//! Client for the Rust gateway.
//!
//! It holds only a short-lived task token, never provider keys. The token is
//! sent only to the configured gateway: HTTPS, or plain HTTP on loopback.
//! Redirects are not followed and proxy settings are ignored, so the token
//! cannot be forwarded to another host. Debug output never shows the token.

use std::{fmt, time::Duration};

use protocol::{CompletionRequest, CompletionResponse};
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue},
    redirect::Policy,
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use thiserror::Error;

pub const TASK_TOKEN_PREFIX: &str = "tsk_";
/// The gateway allows up to 600 s per provider call.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(620);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_ERROR_CHARS: usize = 300;

/// Gateway answer: the neutral completion plus what it cost.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GatewayResponse {
    #[serde(flatten)]
    pub completion: CompletionResponse,
    pub cost_micros: u64,
    pub task_budget_remaining_micros: Option<u64>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum GatewayError {
    #[error("invalid gateway configuration: {0}")]
    Config(String),
    #[error("budget exceeded: {0}")]
    BudgetExceeded(String),
    #[error("provider unavailable ({status} {code}): {message}")]
    ProviderUnavailable {
        status: u16,
        code: String,
        message: String,
    },
    #[error("gateway rejected the request ({status} {code}): {message}")]
    Rejected {
        status: u16,
        code: String,
        message: String,
    },
    #[error("gateway unreachable")]
    Unreachable,
    #[error("unexpected gateway response")]
    InvalidResponse,
}

fn config(message: &str) -> GatewayError {
    GatewayError::Config(message.to_string())
}

/// Accepts `https://host`, or `http://` only on 127.0.0.1, localhost or ::1.
pub fn validate_base_url(url: &str) -> Result<Url, GatewayError> {
    let parsed = Url::parse(url).map_err(|_| config("gateway URL is not valid"))?;
    let loopback = matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    match parsed.scheme() {
        "https" if parsed.host_str().is_some() => Ok(parsed),
        "http" if loopback => Ok(parsed),
        _ => Err(config(
            "gateway URL must use https, or http on loopback only",
        )),
    }
}

pub struct GatewayClient {
    http: Client,
    messages_url: Url,
    token: SecretString,
}

impl fmt::Debug for GatewayClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GatewayClient")
            .field("messages_url", &self.messages_url.as_str())
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

impl GatewayClient {
    pub fn new(base_url: &str, task_token: SecretString) -> Result<Self, GatewayError> {
        if !task_token.expose_secret().starts_with(TASK_TOKEN_PREFIX) {
            return Err(config("task token must start with tsk_"));
        }
        let base = validate_base_url(base_url)?;
        let messages_url = Url::parse(&format!(
            "{}/v1/messages",
            base.as_str().trim_end_matches('/')
        ))
        .map_err(|_| config("gateway URL is not valid"))?;

        // reqwest is built without a bundled crypto backend; use ring, as the
        // gateway does. Ignored if one is already installed.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(Policy::none())
            .no_proxy()
            .https_only(base.scheme() == "https")
            .build()
            .map_err(|_| config("could not build the HTTP client"))?;

        Ok(Self {
            http,
            messages_url,
            token: task_token,
        })
    }

    /// Sends one neutral completion request on behalf of the task.
    pub async fn complete(
        &self,
        request: &CompletionRequest,
    ) -> Result<GatewayResponse, GatewayError> {
        let mut auth = HeaderValue::from_str(&format!("Bearer {}", self.token.expose_secret()))
            .map_err(|_| config("task token contains invalid characters"))?;
        auth.set_sensitive(true);
        let body =
            serde_json::to_vec(request).map_err(|_| config("request could not be encoded"))?;

        let response = self
            .http
            .post(self.messages_url.clone())
            .header(AUTHORIZATION, auth)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| GatewayError::Unreachable)?;

        let status = response.status();
        let bytes = read_limited(response).await?;
        if status == StatusCode::OK {
            return serde_json::from_slice(&bytes).map_err(|_| GatewayError::InvalidResponse);
        }

        let (code, message) = serde_json::from_slice::<ErrorEnvelope>(&bytes)
            .map(|envelope| (envelope.error.code, envelope.error.message))
            .unwrap_or_else(|_| ("unknown".to_string(), String::new()));
        let message: String = message.chars().take(MAX_ERROR_CHARS).collect();
        let status = status.as_u16();
        Err(match status {
            402 => GatewayError::BudgetExceeded(message),
            429 | 502 | 503 => GatewayError::ProviderUnavailable {
                status,
                code,
                message,
            },
            _ => GatewayError::Rejected {
                status,
                code,
                message,
            },
        })
    }
}

async fn read_limited(mut response: Response) -> Result<Vec<u8>, GatewayError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(GatewayError::InvalidResponse);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| GatewayError::Unreachable)?
    {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(GatewayError::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
