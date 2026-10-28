//! The admin secret is required, long enough, and different from the issuer
//! secret.

use base64::{Engine, engine::general_purpose::STANDARD};
use gateway::config::{Config, ConfigError, RawConfig};

fn raw(admin_secret: Option<String>) -> RawConfig {
    RawConfig {
        bind: None,
        issuer_secret: Some("x".repeat(32)),
        admin_secret,
        monthly_budget_usd: Some("50".to_string()),
        database_url: Some("postgres://u:p@127.0.0.1:5432/db".to_string()),
        master_key: Some(STANDARD.encode([9u8; 32])),
        provider: None,
        gemini_model_small: None,
        gemini_model_standard: None,
        gemini_model_large: None,
    }
}

#[test]
fn the_admin_secret_is_required_long_and_different_from_the_issuer_secret() {
    assert!(matches!(
        Config::from_raw(raw(None)),
        Err(ConfigError::MissingAdminSecret)
    ));
    assert!(matches!(
        Config::from_raw(raw(Some("y".repeat(31)))),
        Err(ConfigError::ShortAdminSecret)
    ));
    assert!(matches!(
        Config::from_raw(raw(Some("x".repeat(32)))),
        Err(ConfigError::SameSecrets)
    ));
    assert!(Config::from_raw(raw(Some("y".repeat(32)))).is_ok());
}
