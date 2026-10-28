//! People who use the workspace: password rules, password hashing and
//! account creation.

use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{PasswordHasher, PasswordVerifier},
};
use secrecy::{ExposeSecret, SecretString};
use sqlx::{PgConnection, PgPool};

/// NIST SP 800-63B-4: at least 15 characters when the password is the only
/// factor, and no composition rules. Counted in characters, not bytes.
pub const MIN_PASSWORD_CHARS: usize = 15;
pub const MAX_PASSWORD_CHARS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Admin,
    Member,
    Viewer,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Member => "member",
            Self::Viewer => "viewer",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "admin" => Some(Self::Admin),
            "member" => Some(Self::Member),
            "viewer" => Some(Self::Viewer),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AccountError {
    #[error("passwords must be 15 to 128 characters")]
    PasswordLength,
    #[error("the email address is not valid")]
    InvalidEmail,
    #[error("the display name must be 1 to 100 characters")]
    InvalidName,
    #[error("an account with this email already exists")]
    EmailTaken,
    #[error("an administrator already exists")]
    AdminExists,
    #[error("password hashing failed")]
    Hashing,
    #[error("database error: {0}")]
    Database(String),
}

impl From<sqlx::Error> for AccountError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

/// Argon2id with the OWASP minimum: 19 MiB of memory, 2 iterations, 1 lane.
fn hasher() -> Result<Argon2<'static>, AccountError> {
    let params = Params::new(19 * 1024, 2, 1, None).map_err(|_| AccountError::Hashing)?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

pub fn check_password_rules(password: &SecretString) -> Result<(), AccountError> {
    let chars = password.expose_secret().chars().count();
    if (MIN_PASSWORD_CHARS..=MAX_PASSWORD_CHARS).contains(&chars) {
        Ok(())
    } else {
        Err(AccountError::PasswordLength)
    }
}

/// A PHC string with a fresh random salt.
pub fn hash_password(password: &SecretString) -> Result<String, AccountError> {
    check_password_rules(password)?;
    let hash = hasher()?
        .hash_password(password.expose_secret().as_bytes())
        .map_err(|_| AccountError::Hashing)?;
    Ok(hash.to_string())
}

/// False for a wrong password and for a hash that cannot be read.
pub fn verify_password(password: &SecretString, hash: &str) -> bool {
    hasher().is_ok_and(|argon2| {
        argon2
            .verify_password(password.expose_secret().as_bytes(), hash)
            .is_ok()
    })
}

/// Trimmed and lowercased. Only the shape is checked: a real address is
/// proven by whoever receives the invitation.
pub fn normalize_email(email: &str) -> Result<String, AccountError> {
    let email = email.trim().to_lowercase();
    let valid = email.len() <= 254
        && !email.chars().any(char::is_whitespace)
        && email.matches('@').count() == 1
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        });
    if valid {
        Ok(email)
    } else {
        Err(AccountError::InvalidEmail)
    }
}

fn check_name(display_name: &str) -> Result<&str, AccountError> {
    let name = display_name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        Err(AccountError::InvalidName)
    } else {
        Ok(name)
    }
}

async fn insert_user(
    conn: &mut PgConnection,
    actor: &str,
    email: &str,
    name: &str,
    role: Role,
    hash: &str,
) -> Result<i64, AccountError> {
    sqlx::query("SELECT set_config('ao.actor', $1, true)")
        .bind(actor)
        .execute(&mut *conn)
        .await?;
    let result = sqlx::query_scalar(
        "INSERT INTO users (email, display_name, role, password_hash) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(email)
    .bind(name)
    .bind(role.as_str())
    .bind(hash)
    .fetch_one(&mut *conn)
    .await;
    match result {
        Ok(id) => Ok(id),
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
            Err(AccountError::EmailTaken)
        }
        Err(error) => Err(error.into()),
    }
}

/// Creates an account. `actor` is recorded in the audit log as who did it.
pub async fn create_user(
    pool: &PgPool,
    actor: &str,
    email: &str,
    display_name: &str,
    role: Role,
    password: &SecretString,
) -> Result<i64, AccountError> {
    let email = normalize_email(email)?;
    let name = check_name(display_name)?;
    let hash = hash_password(password)?;
    let mut tx = pool.begin().await?;
    let id = insert_user(&mut tx, actor, &email, name, role, &hash).await?;
    tx.commit().await?;
    Ok(id)
}

/// Creates the first administrator. Refused once any administrator exists,
/// so it cannot be used to add accounts later.
pub async fn bootstrap_admin(
    pool: &PgPool,
    email: &str,
    display_name: &str,
    password: &SecretString,
) -> Result<i64, AccountError> {
    let email = normalize_email(email)?;
    let name = check_name(display_name)?;
    check_password_rules(password)?;
    let mut tx = pool.begin().await?;
    // Two bootstraps at the same time cannot both see "no administrator".
    sqlx::query("SELECT pg_advisory_xact_lock(7291002)")
        .execute(&mut *tx)
        .await?;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE role = 'admin')")
            .fetch_one(&mut *tx)
            .await?;
    if exists {
        return Err(AccountError::AdminExists);
    }
    let hash = hash_password(password)?;
    let id = insert_user(&mut tx, "installer", &email, name, Role::Admin, &hash).await?;
    tx.commit().await?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSPHRASE: &str = "correct horse battery staple";

    fn secret(value: &str) -> SecretString {
        SecretString::from(value.to_string())
    }

    #[test]
    fn password_length_follows_nist_and_counts_characters() {
        assert_eq!(
            check_password_rules(&secret(&"a".repeat(14))),
            Err(AccountError::PasswordLength)
        );
        assert!(check_password_rules(&secret(&"a".repeat(15))).is_ok());
        assert!(check_password_rules(&secret(&"é".repeat(15))).is_ok());
        assert!(check_password_rules(&secret(&"a".repeat(128))).is_ok());
        assert_eq!(
            check_password_rules(&secret(&"a".repeat(129))),
            Err(AccountError::PasswordLength)
        );
    }

    #[test]
    fn hashes_with_owasp_argon2id_parameters_and_verifies() {
        let hash = hash_password(&secret(PASSPHRASE)).unwrap();
        assert!(
            hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "{hash}"
        );
        assert!(verify_password(&secret(PASSPHRASE), &hash));
        assert!(!verify_password(
            &secret("correct horse battery stapler"),
            &hash
        ));
        assert!(!verify_password(&secret(PASSPHRASE), "not a hash"));
        assert_ne!(
            hash,
            hash_password(&secret(PASSPHRASE)).unwrap(),
            "each hash has its own salt"
        );
    }

    #[test]
    fn emails_are_normalized_and_checked() {
        assert_eq!(
            normalize_email(" Alice@Example.COM ").unwrap(),
            "alice@example.com"
        );
        for bad in ["alice", "a@b", "a@@b.c", "a b@c.d", "@b.c", "a@.b.c"] {
            assert_eq!(
                normalize_email(bad),
                Err(AccountError::InvalidEmail),
                "{bad}"
            );
        }
    }
}
