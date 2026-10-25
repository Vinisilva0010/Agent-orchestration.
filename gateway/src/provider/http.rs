//! HTTP plumbing shared by the real providers: client settings, bounded
//! response reading, error-message cleaning and retries with backoff.

use std::{future::Future, time::Duration};

use reqwest::{Client, Response, redirect::Policy};
use secrecy::{ExposeSecret, SecretString};

use super::ProviderError;

/// Largest provider response the gateway reads.
pub(crate) const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_ERROR_DETAIL_CHARS: usize = 300;
/// Long outputs can take minutes without streaming; providers recommend a
/// 10-minute limit for non-streaming requests.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Total attempts for a transient failure, including the first one.
pub(crate) const MAX_ATTEMPTS: u32 = 3;
pub(crate) const RETRY_BASE: Duration = Duration::from_secs(1);

/// Client without redirects, so a key is never sent to another host.
/// `https_only` is false only in tests that use a local fake server.
pub(crate) fn build_client(https_only: bool) -> Result<Client, ProviderError> {
    // reqwest is built without a bundled crypto backend. Use ring, the same
    // backend used for database TLS. Ignored if one is already installed.
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(Policy::none())
        .https_only(https_only)
        .user_agent(concat!(
            "agent-orchestration-gateway/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .map_err(|_| ProviderError::Unavailable("could not build the HTTP client".to_string()))
}

/// Reads a response body, refusing anything above [`MAX_RESPONSE_BYTES`].
pub(crate) async fn read_limited(mut response: Response) -> Result<Vec<u8>, ProviderError> {
    let too_large = || ProviderError::Unavailable("the provider response is too large".to_string());
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        ProviderError::Unavailable("the provider response was interrupted".to_string())
    })? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Removes control characters, truncates, and redacts the key if present.
pub(crate) fn clean(text: &str, key: Option<&SecretString>) -> String {
    let redacted = match key {
        Some(key) => text.replace(key.expose_secret(), "[REDACTED]"),
        None => text.to_string(),
    };
    let cleaned: String = redacted
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_ERROR_DETAIL_CHARS)
        .collect();
    if cleaned.is_empty() {
        "no details".to_string()
    } else {
        cleaned
    }
}

/// Connection failures are transient. Timeouts are not retried, so a stuck
/// call cannot hold a task for several timeouts in a row.
pub(crate) fn transport_error(provider: &'static str, error: &reqwest::Error) -> ProviderError {
    tracing::warn!(
        provider,
        timeout = error.is_timeout(),
        connect = error.is_connect(),
        "provider request failed"
    );
    if error.is_connect() {
        ProviderError::TemporarilyUnavailable("could not connect to the provider".to_string())
    } else {
        ProviderError::Unavailable("could not reach the provider".to_string())
    }
}

/// `base * 2^(attempt - 1)` plus up to 50% random jitter.
pub(crate) fn backoff(base: Duration, attempt: u32) -> Duration {
    let exponential = base.saturating_mul(1 << (attempt - 1).min(16));
    let mut byte = [0u8; 1];
    let _ = getrandom::fill(&mut byte);
    exponential + exponential.mul_f64(f64::from(byte[0]) / 510.0)
}

/// Runs `attempt` until it succeeds, fails permanently, or [`MAX_ATTEMPTS`]
/// transient failures happen. Providers do not bill failed calls, and the
/// budget reservation stays held while retrying.
pub(crate) async fn with_retry<T, F, Fut>(
    base: Duration,
    provider: &'static str,
    mut attempt: F,
) -> Result<T, ProviderError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ProviderError>>,
{
    let mut number = 1;
    loop {
        match attempt().await {
            Err(ProviderError::TemporarilyUnavailable(_)) if number < MAX_ATTEMPTS => {
                let delay = backoff(base, number);
                tracing::warn!(
                    provider,
                    attempt = number,
                    delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                    "provider temporarily unavailable, retrying"
                );
                tokio::time::sleep(delay).await;
                number += 1;
            }
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    #[test]
    fn backoff_doubles_with_bounded_jitter() {
        let base = Duration::from_secs(1);
        for attempt in 1..=3 {
            let delay = backoff(base, attempt);
            let floor = base * (1 << (attempt - 1));
            assert!(
                delay >= floor && delay <= floor + floor / 2,
                "{attempt}: {delay:?}"
            );
        }
    }

    #[test]
    fn clean_redacts_strips_and_truncates() {
        let key = SecretString::from("secret-key-123".to_string());
        let out = clean(
            &format!("bad secret-key-123\n{}", "x".repeat(500)),
            Some(&key),
        );
        assert!(!out.contains("secret-key-123"));
        assert!(out.contains("[REDACTED]"));
        assert!(!out.contains('\n'));
        assert_eq!(out.chars().count(), MAX_ERROR_DETAIL_CHARS);
        assert_eq!(clean("", None), "no details");
    }

    #[tokio::test]
    async fn retries_only_transient_failures() {
        let calls = AtomicU32::new(0);
        let result = with_retry(Duration::ZERO, "test", || {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                if n < 2 {
                    Err(ProviderError::TemporarilyUnavailable("busy".to_string()))
                } else {
                    Ok(n)
                }
            }
        })
        .await;
        assert_eq!(result, Ok(2));

        let calls = AtomicU32::new(0);
        let result: Result<u32, ProviderError> = with_retry(Duration::ZERO, "test", || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(ProviderError::RateLimited) }
        })
        .await;
        assert_eq!(result, Err(ProviderError::RateLimited));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
