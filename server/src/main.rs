//! Administration commands, run with the database owner's connection
//! (ADMIN_DATABASE_URL):
//!
//!   server migrate
//!     Applies every migration and sets the service role passwords from
//!     GATEWAY_DB_PASSWORD, ENGINE_DB_PASSWORD and SERVER_DB_PASSWORD.
//!
//!   server create-admin EMAIL NAME
//!     Creates the first administrator, reading the password from standard
//!     input. Refused once an administrator exists.
//!
//!   server serve
//!     Runs the HTTP API with the server's own role. Reads SERVER_DATABASE_URL,
//!     SERVER_PUBLIC_ORIGIN (where the workspace page is served, for example
//!     https://agents.example.com) and SERVER_BIND (default 127.0.0.1:8090).

use std::{io::BufRead, net::SocketAddr, process::ExitCode, time::Duration};

use secrecy::{ExposeSecret, SecretString};
use sqlx::{PgPool, postgres::PgPoolOptions};
use zeroize::Zeroize;

const USAGE: &str = "usage:\n  server migrate\n  server create-admin EMAIL NAME   (password on standard input)\n  server serve";

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [command] if command == "migrate" => migrate().await,
        [command] if command == "serve" => serve().await,
        [command, email, name] if command == "create-admin" => create_admin(email, name).await,
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(64);
        }
    };
    match result {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn connect() -> Result<PgPool, String> {
    let url = SecretString::from(
        std::env::var("ADMIN_DATABASE_URL").map_err(|_| "ADMIN_DATABASE_URL is not set")?,
    );
    PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(url.expose_secret())
        .await
        .map_err(|error| format!("could not connect: {error}"))
}

async fn migrate() -> Result<String, String> {
    let mut passwords = Vec::new();
    for (role, variable) in [
        ("ao_gateway", "GATEWAY_DB_PASSWORD"),
        ("ao_engine", "ENGINE_DB_PASSWORD"),
        ("ao_server", "SERVER_DB_PASSWORD"),
    ] {
        let value = std::env::var(variable).map_err(|_| format!("{variable} is not set"))?;
        if !server::valid_password(&value) {
            return Err(format!("{variable}: {}", server::PASSWORD_RULE));
        }
        passwords.push((role, SecretString::from(value)));
    }
    let pool = connect().await?;
    server::migrate_all(&pool)
        .await
        .map_err(|error| format!("migrate failed: {error}"))?;
    for (role, password) in &passwords {
        server::set_role_password(&pool, role, password).await?;
    }
    Ok("migrations applied; service role passwords set".to_string())
}

async fn create_admin(email: &str, name: &str) -> Result<String, String> {
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|_| "could not read the password from standard input")?;
    let password = SecretString::from(line.trim_end_matches(['\r', '\n']).to_string());
    line.zeroize();
    let pool = connect().await?;
    let id = server::accounts::bootstrap_admin(&pool, email, name, &password)
        .await
        .map_err(|error| format!("create-admin failed: {error}"))?;
    Ok(format!("administrator created (user id {id})"))
}

async fn serve() -> Result<String, String> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let url = SecretString::from(
        std::env::var("SERVER_DATABASE_URL").map_err(|_| "SERVER_DATABASE_URL is not set")?,
    );
    let origin =
        std::env::var("SERVER_PUBLIC_ORIGIN").map_err(|_| "SERVER_PUBLIC_ORIGIN is not set")?;
    let bind = std::env::var("SERVER_BIND").unwrap_or_else(|_| "127.0.0.1:8090".to_string());
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(url.expose_secret())
        .await
        .map_err(|error| format!("could not connect: {error}"))?;
    let state = server::http::AppState::new(pool, &origin)?;
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|error| format!("could not listen on {bind}: {error}"))?;
    tracing::info!(bind = %bind, origin = %origin, "server listening");
    axum::serve(
        listener,
        server::http::router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await
    .map_err(|error| format!("server failed: {error}"))?;
    Ok("server stopped".to_string())
}

async fn shutdown() {
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = terminate => {}
    }
}
