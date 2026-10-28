//! `server migrate`: applies every migration with the database owner's
//! connection and sets the passwords of the service roles.
//!
//! Reads ADMIN_DATABASE_URL, GATEWAY_DB_PASSWORD and ENGINE_DB_PASSWORD.

use std::{process::ExitCode, time::Duration};

use secrecy::{ExposeSecret, SecretString};
use sqlx::postgres::PgPoolOptions;

const USAGE: &str = "usage: server migrate";

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args != ["migrate"] {
        eprintln!("{USAGE}");
        return ExitCode::from(64);
    }
    match migrate().await {
        Ok(()) => {
            println!("migrations applied; service role passwords set");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("migrate failed: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn migrate() -> Result<(), String> {
    let url = SecretString::from(
        std::env::var("ADMIN_DATABASE_URL").map_err(|_| "ADMIN_DATABASE_URL is not set")?,
    );
    let mut passwords = Vec::new();
    for (role, variable) in [
        ("ao_gateway", "GATEWAY_DB_PASSWORD"),
        ("ao_engine", "ENGINE_DB_PASSWORD"),
    ] {
        let value = std::env::var(variable).map_err(|_| format!("{variable} is not set"))?;
        if !server::valid_password(&value) {
            return Err(format!("{variable}: {}", server::PASSWORD_RULE));
        }
        passwords.push((role, SecretString::from(value)));
    }
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(url.expose_secret())
        .await
        .map_err(|error| format!("could not connect: {error}"))?;
    server::migrate_all(&pool)
        .await
        .map_err(|error| error.to_string())?;
    for (role, password) in &passwords {
        server::set_role_password(&pool, role, password).await?;
    }
    Ok(())
}
