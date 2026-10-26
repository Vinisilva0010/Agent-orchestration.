//! Database connection and schema migrations.

use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use sqlx::{PgPool, migrate::MigrateError, postgres::PgPoolOptions};

/// Opens a connection pool. The URL contains the database password and is
/// only exposed at the moment the connection is made.
pub async fn connect(database_url: &SecretString) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url.expose_secret())
        .await
}

/// Applies the migrations embedded at compile time from `gateway/migrations`.
pub async fn migrate(pool: &PgPool) -> Result<(), MigrateError> {
    // The engine's migrations live in the same database; ignore them here.
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.set_ignore_missing(true);
    migrator.run(pool).await
}
