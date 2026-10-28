//! The server's client for the gateway's admin routes. It holds only the
//! admin secret, which changes keys and limits and reads usage but cannot
//! issue task tokens, so the server cannot spend or read any key.

use std::time::Duration;

use reqwest::{Client, Method, StatusCode, Url, header, redirect::Policy};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};

#[derive(Debug, PartialEq, Eq)]
pub enum GatewayAdminError {
    NotFound,
    /// The gateway refused the request. Its message is never passed on: it
    /// could repeat what was sent, such as a key.
    Rejected,
    Unavailable,
}

pub struct GatewayAdmin {
    base: Url,
    secret: SecretString,
    http: Client,
}

/// Variables the server must never hold: it only reaches the gateway's
/// admin routes.
pub fn unsafe_server_env(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut problems: Vec<String> = names
        .into_iter()
        .filter(|name| {
            let upper = name.to_ascii_uppercase();
            matches!(
                upper.as_str(),
                "GATEWAY_ISSUER_SECRET" | "GATEWAY_MASTER_KEY"
            ) || upper.contains("API_KEY")
                || upper.contains("PRIVATE_KEY")
        })
        .map(|name| {
            format!("{name}: the server must not hold this; it uses only the gateway admin secret")
        })
        .collect();
    problems.sort();
    problems
}

impl GatewayAdmin {
    pub fn new(base_url: &str, secret: SecretString) -> Result<Self, String> {
        let base = Url::parse(base_url).map_err(|_| "SERVER_GATEWAY_URL is not a valid URL")?;
        let loopback = matches!(base.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        let allowed = (base.scheme() == "https" && base.host_str().is_some())
            || (base.scheme() == "http" && loopback);
        if !allowed {
            return Err("SERVER_GATEWAY_URL must use https, or http on loopback only".to_string());
        }
        if secret.expose_secret().chars().count() < 32 {
            return Err("GATEWAY_ADMIN_SECRET must have at least 32 characters".to_string());
        }
        // reqwest is built without a bundled crypto backend; use ring, as the
        // gateway and the engine do. Ignored if one is already installed.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .redirect(Policy::none())
            .no_proxy()
            .https_only(base.scheme() == "https")
            .build()
            .map_err(|_| "could not build the HTTP client")?;
        Ok(Self { base, secret, http })
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<Option<Value>, GatewayAdminError> {
        let url = self
            .base
            .join(path)
            .map_err(|_| GatewayAdminError::Unavailable)?;
        let mut request = self
            .http
            .request(method, url)
            .bearer_auth(self.secret.expose_secret());
        if let Some(body) = body {
            request = request
                .header(header::CONTENT_TYPE, "application/json")
                .body(body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| GatewayAdminError::Unavailable)?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            tracing::error!("the gateway refused the admin secret");
            return Err(GatewayAdminError::Unavailable);
        }
        if status == StatusCode::NOT_FOUND {
            return Err(GatewayAdminError::NotFound);
        }
        if status.is_client_error() {
            return Err(GatewayAdminError::Rejected);
        }
        if !status.is_success() {
            return Err(GatewayAdminError::Unavailable);
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| GatewayAdminError::Unavailable)?;
        if bytes.is_empty() {
            return Ok(None);
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| GatewayAdminError::Unavailable)
    }

    /// Configured providers with key version and update time. Never keys.
    pub async fn credentials(&self) -> Result<Value, GatewayAdminError> {
        Ok(self
            .send(Method::GET, "/internal/credentials", None)
            .await?
            .unwrap_or(Value::Null))
    }

    /// `provider` must already be validated: it becomes part of the path.
    pub async fn store_credential(
        &self,
        provider: &str,
        api_key: &SecretString,
    ) -> Result<(), GatewayAdminError> {
        let body = serde_json::to_vec(&json!({ "api_key": api_key.expose_secret() }))
            .map_err(|_| GatewayAdminError::Unavailable)?;
        self.send(
            Method::PUT,
            &format!("/internal/credentials/{provider}"),
            Some(body),
        )
        .await
        .map(|_| ())
    }

    pub async fn delete_credential(&self, provider: &str) -> Result<(), GatewayAdminError> {
        self.send(
            Method::DELETE,
            &format!("/internal/credentials/{provider}"),
            None,
        )
        .await
        .map(|_| ())
    }

    /// `agent_id` must already be validated: it becomes part of the path.
    pub async fn set_agent_budget(
        &self,
        agent_id: &str,
        monthly_budget_micros: u64,
    ) -> Result<(), GatewayAdminError> {
        let body = serde_json::to_vec(&json!({ "monthly_budget_micros": monthly_budget_micros }))
            .map_err(|_| GatewayAdminError::Unavailable)?;
        self.send(
            Method::PUT,
            &format!("/internal/agents/{agent_id}/budget"),
            Some(body),
        )
        .await
        .map(|_| ())
    }

    pub async fn usage(&self) -> Result<Value, GatewayAdminError> {
        Ok(self
            .send(Method::GET, "/internal/usage", None)
            .await?
            .unwrap_or(Value::Null))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_refuses_the_issuer_secret_the_master_key_and_provider_keys() {
        let names = [
            "GATEWAY_ISSUER_SECRET",
            "GATEWAY_MASTER_KEY",
            "ANTHROPIC_API_KEY",
            "GATEWAY_ADMIN_SECRET",
            "SERVER_DATABASE_URL",
        ];
        let problems = unsafe_server_env(names.map(str::to_string));
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(
            problems
                .iter()
                .all(|problem| !problem.contains("ADMIN_SECRET"))
        );
    }
}
