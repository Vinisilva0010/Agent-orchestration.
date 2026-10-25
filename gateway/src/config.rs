//! Gateway configuration loaded from environment variables.

use std::net::SocketAddr;

use secrecy::SecretString;
use thiserror::Error;

use crate::{
    pricing::parse_usd_to_micros, provider::gemini::TierModels, vault::validate_master_key,
};

/// Default bind address. Loopback only: the gateway must not be reachable from
/// the network unless the deployment explicitly configures it.
pub const DEFAULT_BIND: &str = "127.0.0.1:8081";

/// Minimum length of the secret shared between the main server and the gateway.
pub const MIN_ISSUER_SECRET_LEN: usize = 32;

/// Model provider used for every call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// Deterministic local provider. No network, no key.
    Mock,
    Gemini,
    Anthropic,
}

#[derive(Debug)]
pub struct Config {
    pub bind: SocketAddr,
    /// Secret that authorizes issuing task tokens. Held only by the Rust
    /// services, never by the agents process.
    pub issuer_secret: SecretString,
    /// Spending limit for the whole workspace per calendar month (UTC).
    pub monthly_budget_micros: u64,
    /// PostgreSQL connection URL. Contains the database password.
    pub database_url: SecretString,
    /// Base64 of the 32-byte key that encrypts provider API keys. Never stored
    /// in the database.
    pub master_key: SecretString,
    pub provider: ProviderKind,
    /// Gemini model used for each tier.
    pub gemini_models: TierModels,
}

/// Raw values as read from the environment.
#[derive(Debug, Default)]
pub struct RawConfig {
    pub bind: Option<String>,
    pub issuer_secret: Option<String>,
    pub monthly_budget_usd: Option<String>,
    pub database_url: Option<String>,
    pub master_key: Option<String>,
    pub provider: Option<String>,
    pub gemini_model_small: Option<String>,
    pub gemini_model_standard: Option<String>,
    pub gemini_model_large: Option<String>,
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
    #[error("DATABASE_URL is not set")]
    MissingDatabaseUrl,
    #[error("DATABASE_URL must start with postgres:// or postgresql://")]
    InvalidDatabaseUrl,
    #[error("GATEWAY_MASTER_KEY is not set")]
    MissingMasterKey,
    #[error("GATEWAY_MASTER_KEY must be 32 bytes encoded in base64")]
    InvalidMasterKey,
    #[error("GATEWAY_PROVIDER must be mock, gemini or anthropic, got: {0}")]
    InvalidProvider(String),
    #[error("unknown Gemini model (not in the price catalog): {0}")]
    UnknownGeminiModel(String),
}

impl Config {
    /// Reads `GATEWAY_BIND`, `GATEWAY_ISSUER_SECRET`, `GATEWAY_MONTHLY_BUDGET_USD`,
    /// `DATABASE_URL`, `GATEWAY_MASTER_KEY` and `GATEWAY_PROVIDER` from the
    /// environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_raw(RawConfig {
            bind: std::env::var("GATEWAY_BIND").ok(),
            issuer_secret: std::env::var("GATEWAY_ISSUER_SECRET").ok(),
            monthly_budget_usd: std::env::var("GATEWAY_MONTHLY_BUDGET_USD").ok(),
            database_url: std::env::var("DATABASE_URL").ok(),
            master_key: std::env::var("GATEWAY_MASTER_KEY").ok(),
            provider: std::env::var("GATEWAY_PROVIDER").ok(),
            gemini_model_small: std::env::var("GATEWAY_GEMINI_MODEL_SMALL").ok(),
            gemini_model_standard: std::env::var("GATEWAY_GEMINI_MODEL_STANDARD").ok(),
            gemini_model_large: std::env::var("GATEWAY_GEMINI_MODEL_LARGE").ok(),
        })
    }

    /// Validates raw values. Kept separate from [`Config::from_env`] so it can be
    /// tested without modifying the process environment.
    pub fn from_raw(raw: RawConfig) -> Result<Self, ConfigError> {
        let bind_text = raw.bind.as_deref().unwrap_or(DEFAULT_BIND);
        let bind = bind_text
            .parse()
            .map_err(|_| ConfigError::InvalidBind(bind_text.to_string()))?;

        let secret = raw.issuer_secret.ok_or(ConfigError::MissingIssuerSecret)?;
        if secret.chars().count() < MIN_ISSUER_SECRET_LEN {
            return Err(ConfigError::IssuerSecretTooShort);
        }

        let monthly_budget_micros = parse_usd_to_micros(
            raw.monthly_budget_usd
                .as_deref()
                .ok_or(ConfigError::MissingMonthlyBudget)?,
        )
        .filter(|micros| *micros > 0)
        .ok_or(ConfigError::InvalidMonthlyBudget)?;

        let database_url = raw.database_url.ok_or(ConfigError::MissingDatabaseUrl)?;
        let is_postgres =
            database_url.starts_with("postgres://") || database_url.starts_with("postgresql://");
        if !is_postgres {
            return Err(ConfigError::InvalidDatabaseUrl);
        }

        let master_key = raw.master_key.ok_or(ConfigError::MissingMasterKey)?;
        validate_master_key(&master_key).map_err(|_| ConfigError::InvalidMasterKey)?;

        let provider = match raw.provider.as_deref().map(str::trim) {
            None | Some("mock") => ProviderKind::Mock,
            Some("gemini") => ProviderKind::Gemini,
            Some("anthropic") => ProviderKind::Anthropic,
            Some(other) => return Err(ConfigError::InvalidProvider(other.to_string())),
        };

        let gemini_models = TierModels::from_ids(
            raw.gemini_model_small.as_deref(),
            raw.gemini_model_standard.as_deref(),
            raw.gemini_model_large.as_deref(),
        )
        .map_err(ConfigError::UnknownGeminiModel)?;

        Ok(Self {
            bind,
            issuer_secret: SecretString::from(secret),
            monthly_budget_micros,
            database_url: SecretString::from(database_url),
            master_key: SecretString::from(master_key),
            provider,
            gemini_models,
        })
    }
}

#[cfg(test)]
mod tests {
    use base64::{Engine, engine::general_purpose::STANDARD};

    use super::*;

    const DB_URL: &str = "postgres://user:db-password-123@127.0.0.1:5433/db";

    fn master_key() -> String {
        STANDARD.encode([9u8; 32])
    }

    fn valid() -> RawConfig {
        RawConfig {
            bind: None,
            issuer_secret: Some("x".repeat(MIN_ISSUER_SECRET_LEN)),
            monthly_budget_usd: Some("50".to_string()),
            database_url: Some(DB_URL.to_string()),
            master_key: Some(master_key()),
            provider: None,
            gemini_model_small: None,
            gemini_model_standard: None,
            gemini_model_large: None,
        }
    }

    #[test]
    fn accepts_valid_configuration_with_loopback_default() {
        let config = Config::from_raw(valid()).expect("valid config");
        assert!(config.bind.ip().is_loopback());
        assert_eq!(config.bind.port(), 8081);
        assert_eq!(config.monthly_budget_micros, 50_000_000);
        assert_eq!(config.provider, ProviderKind::Mock);
    }

    #[test]
    fn accepts_custom_address() {
        let config = Config::from_raw(RawConfig {
            bind: Some("127.0.0.1:9000".to_string()),
            ..valid()
        })
        .expect("valid config");
        assert_eq!(config.bind.port(), 9000);
    }

    #[test]
    fn rejects_invalid_address() {
        let error = Config::from_raw(RawConfig {
            bind: Some("not-an-address".to_string()),
            ..valid()
        })
        .unwrap_err();
        assert_eq!(
            error,
            ConfigError::InvalidBind("not-an-address".to_string())
        );
    }

    #[test]
    fn requires_issuer_secret_of_minimum_length() {
        let missing = Config::from_raw(RawConfig {
            issuer_secret: None,
            ..valid()
        });
        assert_eq!(missing.unwrap_err(), ConfigError::MissingIssuerSecret);

        let short = Config::from_raw(RawConfig {
            issuer_secret: Some("x".repeat(MIN_ISSUER_SECRET_LEN - 1)),
            ..valid()
        });
        assert_eq!(short.unwrap_err(), ConfigError::IssuerSecretTooShort);
    }

    #[test]
    fn requires_valid_monthly_budget() {
        let missing = Config::from_raw(RawConfig {
            monthly_budget_usd: None,
            ..valid()
        });
        assert_eq!(missing.unwrap_err(), ConfigError::MissingMonthlyBudget);

        for bad in ["0", "-5", "abc", "1.1234567"] {
            let error = Config::from_raw(RawConfig {
                monthly_budget_usd: Some(bad.to_string()),
                ..valid()
            })
            .unwrap_err();
            assert_eq!(error, ConfigError::InvalidMonthlyBudget, "{bad}");
        }
    }

    #[test]
    fn requires_postgres_database_url() {
        let missing = Config::from_raw(RawConfig {
            database_url: None,
            ..valid()
        });
        assert_eq!(missing.unwrap_err(), ConfigError::MissingDatabaseUrl);

        let wrong = Config::from_raw(RawConfig {
            database_url: Some("mysql://user:pass@host/db".to_string()),
            ..valid()
        });
        assert_eq!(wrong.unwrap_err(), ConfigError::InvalidDatabaseUrl);
    }

    #[test]
    fn requires_32_byte_master_key() {
        let missing = Config::from_raw(RawConfig {
            master_key: None,
            ..valid()
        });
        assert_eq!(missing.unwrap_err(), ConfigError::MissingMasterKey);

        for bad in [STANDARD.encode([9u8; 16]), "not base64 !!".to_string()] {
            let error = Config::from_raw(RawConfig {
                master_key: Some(bad),
                ..valid()
            })
            .unwrap_err();
            assert_eq!(error, ConfigError::InvalidMasterKey);
        }
    }

    #[test]
    fn parses_provider_choice() {
        let gemini = Config::from_raw(RawConfig {
            provider: Some("gemini".to_string()),
            ..valid()
        })
        .unwrap();
        assert_eq!(gemini.provider, ProviderKind::Gemini);

        let anthropic = Config::from_raw(RawConfig {
            provider: Some("anthropic".to_string()),
            ..valid()
        })
        .unwrap();
        assert_eq!(anthropic.provider, ProviderKind::Anthropic);

        let unknown = Config::from_raw(RawConfig {
            provider: Some("openai".to_string()),
            ..valid()
        })
        .unwrap_err();
        assert_eq!(unknown, ConfigError::InvalidProvider("openai".to_string()));
    }

    #[test]
    fn rejects_gemini_models_outside_the_catalog() {
        let error = Config::from_raw(RawConfig {
            gemini_model_standard: Some("gemini-9-unknown".to_string()),
            ..valid()
        })
        .unwrap_err();
        assert_eq!(
            error,
            ConfigError::UnknownGeminiModel("gemini-9-unknown".to_string())
        );
    }

    #[test]
    fn debug_output_does_not_reveal_secrets() {
        let config = Config::from_raw(valid()).unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains(&"x".repeat(MIN_ISSUER_SECRET_LEN)));
        assert!(!debug.contains("db-password-123"));
        assert!(!debug.contains(&master_key()));
    }
}
