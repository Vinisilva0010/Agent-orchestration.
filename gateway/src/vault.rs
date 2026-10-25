//! Encrypted storage for provider API keys.
//!
//! Keys are encrypted with AES-256-GCM under a master key that is never stored
//! in the database. Every write uses a fresh random 96-bit nonce. The provider
//! name and key version are bound as associated data, so a ciphertext copied
//! from one provider's row to another fails to decrypt. No HTTP route ever
//! returns a key; only provider implementations read them, inside the gateway.

use std::fmt;

use aes_gcm::{
    Aes256Gcm, KeyInit,
    aead::{Aead, Nonce, Payload},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use sqlx::PgPool;
use thiserror::Error;
use time::OffsetDateTime;
use zeroize::Zeroize;

pub const MASTER_KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;
const KEY_VERSION: i32 = 1;
const MAX_API_KEY_LEN: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialProvider {
    Anthropic,
    Openai,
    Gemini,
}

impl CredentialProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::Gemini => "gemini",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::Openai),
            "gemini" => Some(Self::Gemini),
            _ => None,
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VaultError {
    #[error("GATEWAY_MASTER_KEY must be 32 bytes encoded in base64")]
    InvalidMasterKey,
    #[error("api_key must be 1 to 512 visible characters without spaces")]
    InvalidApiKey,
    #[error("unknown provider; expected anthropic, openai or gemini")]
    UnknownProvider,
    #[error("credential could not be decrypted")]
    Decryption,
    #[error("credential could not be encrypted")]
    Encryption,
    #[error("random number generator unavailable")]
    RandomUnavailable,
    #[error("credential store unavailable")]
    StoreUnavailable,
}

/// Public information about a stored credential. Never contains the key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CredentialInfo {
    pub provider: String,
    pub key_version: i32,
    pub updated_at_unix: i64,
}

/// Checks that a master key decodes to exactly 32 bytes.
pub fn validate_master_key(encoded: &str) -> Result<(), VaultError> {
    let mut bytes = STANDARD
        .decode(encoded.trim())
        .map_err(|_| VaultError::InvalidMasterKey)?;
    let valid = bytes.len() == MASTER_KEY_BYTES;
    bytes.zeroize();
    if valid {
        Ok(())
    } else {
        Err(VaultError::InvalidMasterKey)
    }
}

pub fn validate_api_key(key: &str) -> Result<(), VaultError> {
    let valid = !key.is_empty()
        && key.len() <= MAX_API_KEY_LEN
        && key.bytes().all(|b| b.is_ascii_graphic());
    if valid {
        Ok(())
    } else {
        Err(VaultError::InvalidApiKey)
    }
}

fn associated_data(provider: CredentialProvider, key_version: i32) -> Vec<u8> {
    format!(
        "agent-orchestration:provider-credential:{}:v{key_version}",
        provider.as_str()
    )
    .into_bytes()
}

fn store_error(error: sqlx::Error) -> VaultError {
    tracing::error!(error = %error, "credential store query failed");
    VaultError::StoreUnavailable
}

pub struct Vault {
    pool: PgPool,
    cipher: Aes256Gcm,
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Vault { .. }")
    }
}

impl Vault {
    pub fn new(pool: PgPool, master_key: &SecretString) -> Result<Self, VaultError> {
        let mut bytes = STANDARD
            .decode(master_key.expose_secret().trim())
            .map_err(|_| VaultError::InvalidMasterKey)?;
        let cipher = if bytes.len() == MASTER_KEY_BYTES {
            Aes256Gcm::new_from_slice(&bytes).map_err(|_| VaultError::InvalidMasterKey)
        } else {
            Err(VaultError::InvalidMasterKey)
        };
        bytes.zeroize();
        Ok(Self {
            pool,
            cipher: cipher?,
        })
    }

    /// Encrypts and stores the key of a provider, replacing any previous one.
    pub async fn store(
        &self,
        provider: CredentialProvider,
        api_key: &SecretString,
    ) -> Result<(), VaultError> {
        validate_api_key(api_key.expose_secret())?;

        let mut nonce = [0u8; NONCE_BYTES];
        getrandom::fill(&mut nonce).map_err(|_| VaultError::RandomUnavailable)?;
        let aad = associated_data(provider, KEY_VERSION);
        let ciphertext = self
            .cipher
            .encrypt(
                &Nonce::<Aes256Gcm>::from(nonce),
                Payload {
                    msg: api_key.expose_secret().as_bytes(),
                    aad: &aad,
                },
            )
            .map_err(|_| VaultError::Encryption)?;

        sqlx::query(
            "INSERT INTO provider_credentials (provider, ciphertext, nonce, key_version) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (provider) DO UPDATE SET ciphertext = EXCLUDED.ciphertext, \
             nonce = EXCLUDED.nonce, key_version = EXCLUDED.key_version, updated_at = now()",
        )
        .bind(provider.as_str())
        .bind(&ciphertext)
        .bind(&nonce[..])
        .bind(KEY_VERSION)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    /// Decrypts the key of a provider. For use inside the gateway only.
    pub async fn load(
        &self,
        provider: CredentialProvider,
    ) -> Result<Option<SecretString>, VaultError> {
        let row: Option<(Vec<u8>, Vec<u8>, i32)> = sqlx::query_as(
            "SELECT ciphertext, nonce, key_version FROM provider_credentials WHERE provider = $1",
        )
        .bind(provider.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(store_error)?;
        let Some((ciphertext, nonce, key_version)) = row else {
            return Ok(None);
        };

        let nonce: [u8; NONCE_BYTES] = nonce
            .as_slice()
            .try_into()
            .map_err(|_| VaultError::Decryption)?;
        let aad = associated_data(provider, key_version);
        let plaintext = self
            .cipher
            .decrypt(
                &Nonce::<Aes256Gcm>::from(nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| VaultError::Decryption)?;

        match String::from_utf8(plaintext) {
            Ok(key) => Ok(Some(SecretString::from(key))),
            Err(error) => {
                error.into_bytes().zeroize();
                Err(VaultError::Decryption)
            }
        }
    }

    /// Deletes the key of a provider. Returns whether one existed.
    pub async fn delete(&self, provider: CredentialProvider) -> Result<bool, VaultError> {
        let result = sqlx::query("DELETE FROM provider_credentials WHERE provider = $1")
            .bind(provider.as_str())
            .execute(&self.pool)
            .await
            .map_err(store_error)?;
        Ok(result.rows_affected() > 0)
    }

    /// Lists configured providers without their keys.
    pub async fn list(&self) -> Result<Vec<CredentialInfo>, VaultError> {
        let rows: Vec<(String, i32, OffsetDateTime)> = sqlx::query_as(
            "SELECT provider, key_version, updated_at FROM provider_credentials ORDER BY provider",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(rows
            .into_iter()
            .map(|(provider, key_version, updated_at)| CredentialInfo {
                provider,
                key_version,
                updated_at_unix: updated_at.unix_timestamp(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master(byte: u8) -> SecretString {
        SecretString::from(STANDARD.encode([byte; MASTER_KEY_BYTES]))
    }

    fn api_key(text: &str) -> SecretString {
        SecretString::from(text.to_string())
    }

    #[test]
    fn validates_master_key_length() {
        assert!(validate_master_key(&STANDARD.encode([1u8; 32])).is_ok());
        assert_eq!(
            validate_master_key(&STANDARD.encode([1u8; 16])),
            Err(VaultError::InvalidMasterKey)
        );
        assert_eq!(
            validate_master_key("not base64 !!"),
            Err(VaultError::InvalidMasterKey)
        );
    }

    #[test]
    fn validates_api_keys() {
        assert!(validate_api_key("sk-test_ABC.123").is_ok());
        let long = "a".repeat(MAX_API_KEY_LEN + 1);
        for bad in ["", "has space", "line\nbreak", long.as_str()] {
            assert_eq!(validate_api_key(bad), Err(VaultError::InvalidApiKey));
        }
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn stores_and_loads_a_key(pool: PgPool) {
        let vault = Vault::new(pool, &master(7)).unwrap();
        vault
            .store(CredentialProvider::Anthropic, &api_key("sk-test-123"))
            .await
            .unwrap();
        let loaded = vault
            .load(CredentialProvider::Anthropic)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.expose_secret(), "sk-test-123");
        assert!(
            vault
                .load(CredentialProvider::Openai)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn database_never_holds_the_plaintext(pool: PgPool) {
        let vault = Vault::new(pool.clone(), &master(7)).unwrap();
        let key = "sk-plaintext-must-not-appear";
        vault
            .store(CredentialProvider::Gemini, &api_key(key))
            .await
            .unwrap();
        let (ciphertext, first_nonce): (Vec<u8>, Vec<u8>) =
            sqlx::query_as("SELECT ciphertext, nonce FROM provider_credentials")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(!ciphertext.windows(key.len()).any(|w| w == key.as_bytes()));

        vault
            .store(CredentialProvider::Gemini, &api_key(key))
            .await
            .unwrap();
        let second_nonce: Vec<u8> = sqlx::query_scalar("SELECT nonce FROM provider_credentials")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_ne!(
            first_nonce, second_nonce,
            "every write must use a new nonce"
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn wrong_master_key_cannot_decrypt(pool: PgPool) {
        Vault::new(pool.clone(), &master(7))
            .unwrap()
            .store(CredentialProvider::Openai, &api_key("sk-test-123"))
            .await
            .unwrap();
        let other = Vault::new(pool, &master(8)).unwrap();
        assert_eq!(
            other.load(CredentialProvider::Openai).await.err(),
            Some(VaultError::Decryption)
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn ciphertext_moved_to_another_provider_fails(pool: PgPool) {
        let vault = Vault::new(pool.clone(), &master(7)).unwrap();
        vault
            .store(CredentialProvider::Anthropic, &api_key("sk-anthropic"))
            .await
            .unwrap();
        vault
            .store(CredentialProvider::Openai, &api_key("sk-openai"))
            .await
            .unwrap();
        sqlx::query(
            "UPDATE provider_credentials SET ciphertext = a.ciphertext, nonce = a.nonce \
             FROM provider_credentials a \
             WHERE a.provider = 'anthropic' AND provider_credentials.provider = 'openai'",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            vault.load(CredentialProvider::Openai).await.err(),
            Some(VaultError::Decryption)
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn lists_without_keys_and_deletes(pool: PgPool) {
        let vault = Vault::new(pool, &master(7)).unwrap();
        vault
            .store(CredentialProvider::Gemini, &api_key("sk-secret-value"))
            .await
            .unwrap();
        let listed = vault.list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].provider, "gemini");
        assert!(!format!("{listed:?}").contains("sk-secret-value"));

        assert!(vault.delete(CredentialProvider::Gemini).await.unwrap());
        assert!(!vault.delete(CredentialProvider::Gemini).await.unwrap());
        assert!(vault.list().await.unwrap().is_empty());
    }
}
