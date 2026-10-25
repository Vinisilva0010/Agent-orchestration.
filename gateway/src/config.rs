//! Gateway configuration loaded from environment variables.

use std::net::SocketAddr;

use thiserror::Error;

/// Default bind address. Loopback only: the gateway must not be reachable from
/// the network unless the deployment explicitly configures it.
pub const DEFAULT_BIND: &str = "127.0.0.1:8081";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub bind: SocketAddr,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("GATEWAY_BIND is not a valid socket address: {0}")]
    InvalidBind(String),
}

impl Config {
    /// Reads `GATEWAY_BIND` from the environment, falling back to [`DEFAULT_BIND`].
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_bind_value(std::env::var("GATEWAY_BIND").ok().as_deref())
    }

    /// Builds the configuration from an optional bind value.
    /// Kept separate from [`Config::from_env`] so it can be tested without
    /// modifying the process environment.
    pub fn from_bind_value(value: Option<&str>) -> Result<Self, ConfigError> {
        let raw = value.unwrap_or(DEFAULT_BIND);
        let bind = raw
            .parse()
            .map_err(|_| ConfigError::InvalidBind(raw.to_string()))?;
        Ok(Self { bind })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_loopback() {
        let config = Config::from_bind_value(None).expect("default must be valid");
        assert!(config.bind.ip().is_loopback());
        assert_eq!(config.bind.port(), 8081);
    }

    #[test]
    fn accepts_custom_address() {
        let config = Config::from_bind_value(Some("127.0.0.1:9000")).expect("valid address");
        assert_eq!(config.bind.port(), 9000);
    }

    #[test]
    fn rejects_invalid_address() {
        let error = Config::from_bind_value(Some("not-an-address")).unwrap_err();
        assert_eq!(
            error,
            ConfigError::InvalidBind("not-an-address".to_string())
        );
    }
}
