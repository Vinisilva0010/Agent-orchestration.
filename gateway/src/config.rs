//! Gateway configuration loaded from environment variables.

use std::net::SocketAddr;

use secrecy::SecretString;
use thiserror::Error;

use crate::pricing::parse_usd_to_micros;

/// Default bind address. Loopback only: the gateway must not be reachable from
/// the network unless the deployment explicitly configures it.
pub const DEFAULT_BIND: &str = "127.0.0.1:8081";

/// Minimum length of the secret shared between the main server and the gateway.
pub const MIN_ISSUER_SECRET_LEN: usize = 32;

#[derive(Debug)]
pub struct Config {
    pub bind: SocketAddr,
    /// Secret that authorizes issuing task tokens. Held only by the Rust
    /// services, never by the agents process.
    pub issuer_secret: SecretString,
    /// Spending limit for the whole workspace per calendar month (UTC).
    pub monthly_budget_micros: u64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("GATEWAY_BIND is not a valid socket address: {0}")]
    InvalidBind(String),
    #[error("GATEWAY_ISSUER_SECRET is not set")]
    MissingIssuerSecret,
    #[error("GATEWAY_ISSUER_SECRET must have at least 32 characters")]
    IssuerSecretTooShort,
    #[error("GATEWAY_MONTHLY_BUDGET_USD is not set")]
    MissingMonthlyBudget,
    #[error("GATEWAY_MONTHLY_BUDGET_USD must be a positive dollar amount such as 50 or 12.5")]
    InvalidMonthlyBudget,
}

impl Config {
    /// Reads `GATEWAY_BIND`, `GATEWAY_ISSUER_SECRET` and
    /// `GATEWAY_MONTHLY_BUDGET_USD` from the environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind = std::env::var("GATEWAY_BIND").ok();
        let secret = std::env::var("GATEWAY_ISSUER_SECRET").ok();
        let budget = std::env::var("GATEWAY_MONTHLY_BUDGET_USD").ok();
        Self::from_values(bind.as_deref(), secret, budget.as_deref())
    }

    /// Builds the configuration from explicit values. Kept separate from
    /// [`Config::from_env`] so it can be tested without modifying the process
    /// environment.
    pub fn from_values(
        bind: Option<&str>,
        issuer_secret: Option<String>,
        monthly_budget_usd: Option<&str>,
    ) -> Result<Self, ConfigError> {
        let raw = bind.unwrap_or(DEFAULT_BIND);
        let bind = raw
            .parse()
            .map_err(|_| ConfigError::InvalidBind(raw.to_string()))?;

        let secret = issuer_secret.ok_or(ConfigError::MissingIssuerSecret)?;
        if secret.chars().count() < MIN_ISSUER_SECRET_LEN {
            return Err(ConfigError::IssuerSecretTooShort);
        }

        let monthly_budget_micros =
            parse_usd_to_micros(monthly_budget_usd.ok_or(ConfigError::MissingMonthlyBudget)?)
                .filter(|micros| *micros > 0)
                .ok_or(ConfigError::InvalidMonthlyBudget)?;

        Ok(Self {
            bind,
            issuer_secret: SecretString::from(secret),
            monthly_budget_micros,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUDGET: Option<&str> = Some("50");

    fn valid_secret() -> Option<String> {
        Some("x".repeat(MIN_ISSUER_SECRET_LEN))
    }

    #[test]
    fn defaults_to_loopback() {
        let config = Config::from_values(None, valid_secret(), BUDGET).expect("valid config");
        assert!(config.bind.ip().is_loopback());
        assert_eq!(config.bind.port(), 8081);
        assert_eq!(config.monthly_budget_micros, 50_000_000);
    }

    #[test]
    fn accepts_custom_address() {
        let config = Config::from_values(Some("127.0.0.1:9000"), valid_secret(), BUDGET)
            .expect("valid config");
        assert_eq!(config.bind.port(), 9000);
    }

    #[test]
    fn rejects_invalid_address() {
        let error =
            Config::from_values(Some("not-an-address"), valid_secret(), BUDGET).unwrap_err();
        assert_eq!(
            error,
            ConfigError::InvalidBind("not-an-address".to_string())
        );
    }

    #[test]
    fn requires_issuer_secret() {
        let error = Config::from_values(None, None, BUDGET).unwrap_err();
        assert_eq!(error, ConfigError::MissingIssuerSecret);
    }

    #[test]
    fn rejects_short_issuer_secret() {
        let short = Some("x".repeat(MIN_ISSUER_SECRET_LEN - 1));
        let error = Config::from_values(None, short, BUDGET).unwrap_err();
        assert_eq!(error, ConfigError::IssuerSecretTooShort);
    }

    #[test]
    fn requires_monthly_budget() {
        let error = Config::from_values(None, valid_secret(), None).unwrap_err();
        assert_eq!(error, ConfigError::MissingMonthlyBudget);
    }

    #[test]
    fn rejects_invalid_monthly_budget() {
        for bad in ["0", "-5", "abc", "1.1234567"] {
            let error = Config::from_values(None, valid_secret(), Some(bad)).unwrap_err();
            assert_eq!(error, ConfigError::InvalidMonthlyBudget, "{bad}");
        }
    }

    #[test]
    fn debug_output_does_not_reveal_secret() {
        let secret = "s".repeat(MIN_ISSUER_SECRET_LEN);
        let config = Config::from_values(None, Some(secret.clone()), BUDGET).unwrap();
        assert!(!format!("{config:?}").contains(&secret));
    }
}
