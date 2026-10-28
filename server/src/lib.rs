//! Database administration: migrations and service roles.
//!
//! Migrations and role passwords are applied with the database owner's
//! connection. The services connect with their own roles (`ao_gateway`,
//! `ao_engine`), which can only use their own tables.

pub mod accounts;
mod admin;
pub mod gateway_admin;
pub mod http;
mod runs;
mod workspace;

use secrecy::{ExposeSecret, SecretString};
use sqlx::{PgPool, migrate::MigrateError};

pub const PASSWORD_RULE: &str =
    "role passwords must be 32 to 128 characters of letters, digits, '-' or '_'";

/// Applies the gateway, engine and server migrations, in that order: the
/// server's audit triggers attach to the other services' tables. They share
/// one migration table, so each migrator ignores the others' applied versions.
pub async fn migrate_all(pool: &PgPool) -> Result<(), MigrateError> {
    let mut gateway = sqlx::migrate!("../gateway/migrations");
    gateway.set_ignore_missing(true);
    gateway.run(pool).await?;
    let mut engine = sqlx::migrate!("../engine/migrations");
    engine.set_ignore_missing(true);
    engine.run(pool).await?;
    let mut server = sqlx::migrate!("./migrations");
    server.set_ignore_missing(true);
    server.run(pool).await
}

pub fn valid_password(password: &str) -> bool {
    (32..=128).contains(&password.len())
        && password
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Lets a service role log in with the given password. The password travels
/// as a bound parameter, never inside the SQL text.
pub async fn set_role_password(
    pool: &PgPool,
    role: &str,
    password: &SecretString,
) -> Result<(), String> {
    let statement = match role {
        "ao_gateway" => {
            "DO $$ BEGIN EXECUTE format('ALTER ROLE ao_gateway LOGIN PASSWORD %L', current_setting('ao.role_password')); END $$"
        }
        "ao_engine" => {
            "DO $$ BEGIN EXECUTE format('ALTER ROLE ao_engine LOGIN PASSWORD %L', current_setting('ao.role_password')); END $$"
        }
        "ao_server" => {
            "DO $$ BEGIN EXECUTE format('ALTER ROLE ao_server LOGIN PASSWORD %L', current_setting('ao.role_password')); END $$"
        }
        _ => return Err(format!("unknown role: {role}")),
    };
    if !valid_password(password.expose_secret()) {
        return Err(PASSWORD_RULE.to_string());
    }
    let failed = |error: sqlx::Error| format!("could not set the password of {role}: {error}");
    let mut tx = pool.begin().await.map_err(failed)?;
    sqlx::query("SELECT set_config('ao.role_password', $1, true)")
        .bind(password.expose_secret())
        .execute(&mut *tx)
        .await
        .map_err(failed)?;
    sqlx::raw_sql(statement)
        .execute(&mut *tx)
        .await
        .map_err(failed)?;
    tx.commit().await.map_err(failed)
}
