//! Sandbox commands:
//!
//!   sandbox prepare
//!     Pulls the pinned image (runs never pull).
//!
//!   sandbox run TASK_ID
//!     Runs Python code read from standard input and prints the result as JSON.
//!
//!   sandbox serve
//!     Serves POST /v1/run on loopback for the engine. Reads SANDBOX_SECRET
//!     (32+ characters) and SANDBOX_BIND (default 127.0.0.1:8095), and refuses
//!     to start with gateway secrets, provider keys or database URLs.
//!
//! All commands read SANDBOX_ROOT (absolute folder for task files) and
//! SANDBOX_RUNTIME (runc or runsc; default: Docker's default runtime).

use std::{io::Read, net::SocketAddr, process::ExitCode};

use sandbox::{
    Limits, Sandbox,
    service::{ServiceState, router},
};
use secrecy::SecretString;
use serde_json::json;

const USAGE: &str =
    "usage:\n  sandbox prepare\n  sandbox run TASK_ID   (code on standard input)\n  sandbox serve";

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [command] if command == "prepare" => prepare().await,
        [command] if command == "serve" => serve().await,
        [command, task_id] if command == "run" => run(task_id).await,
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(64)
        }
    }
}

fn open_sandbox() -> Result<Sandbox, String> {
    let root = std::env::var("SANDBOX_ROOT").map_err(|_| "SANDBOX_ROOT is not set")?;
    let runtime = std::env::var("SANDBOX_RUNTIME").ok();
    Sandbox::new(root, runtime.as_deref()).map_err(|error| error.to_string())
}

async fn prepare() -> ExitCode {
    match sandbox::ensure_image().await {
        Ok(()) => {
            println!("sandbox image ready");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(3)
        }
    }
}

async fn run(task_id: &str) -> ExitCode {
    let sandbox = match open_sandbox() {
        Ok(sandbox) => sandbox,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(4);
        }
    };
    let mut code = String::new();
    if std::io::stdin().read_to_string(&mut code).is_err() {
        eprintln!("could not read the code from standard input");
        return ExitCode::from(4);
    }
    match sandbox.run_python(task_id, &code, &Limits::default()).await {
        Ok(output) => {
            println!(
                "{}",
                json!({
                    "exit_code": output.exit_code, "timed_out": output.timed_out,
                    "truncated": output.truncated, "stdout": output.stdout, "stderr": output.stderr,
                    "files": output.files,
                })
            );
            if output.exit_code == Some(0) {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(3)
            }
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(3)
        }
    }
}

/// The sandbox holds Docker, which is root-equivalent on the host, so it must
/// hold nothing else.
fn unsafe_environment() -> Vec<String> {
    let mut problems: Vec<String> = std::env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .filter(|name| {
            let upper = name.to_ascii_uppercase();
            upper.starts_with("GATEWAY_")
                || upper.contains("API_KEY")
                || upper.contains("PRIVATE_KEY")
                || upper.contains("DATABASE_URL")
        })
        .map(|name| format!("{name}: the sandbox must hold only its own secret"))
        .collect();
    problems.sort();
    problems
}

async fn serve() -> ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let problems = unsafe_environment();
    if !problems.is_empty() {
        eprintln!("unsafe environment: {}", problems.join("; "));
        return ExitCode::from(4);
    }
    let fail = |message: String| {
        eprintln!("{message}");
        ExitCode::from(4)
    };
    let sandbox = match open_sandbox() {
        Ok(sandbox) => sandbox,
        Err(error) => return fail(error),
    };
    let Ok(secret) = std::env::var("SANDBOX_SECRET") else {
        return fail("SANDBOX_SECRET is not set".to_string());
    };
    let state = match ServiceState::new(sandbox, &SecretString::from(secret)) {
        Ok(state) => state,
        Err(error) => return fail(error),
    };
    let bind = std::env::var("SANDBOX_BIND").unwrap_or_else(|_| "127.0.0.1:8095".to_string());
    let Ok(address) = bind.parse::<SocketAddr>() else {
        return fail(format!("SANDBOX_BIND is not an address: {bind}"));
    };
    if !address.ip().is_loopback() {
        return fail("SANDBOX_BIND must be a loopback address".to_string());
    }
    if let Err(error) = sandbox::ensure_image().await {
        eprintln!("{error}");
        return ExitCode::from(3);
    }
    let listener = match tokio::net::TcpListener::bind(address).await {
        Ok(listener) => listener,
        Err(error) => return fail(format!("could not listen on {address}: {error}")),
    };
    tracing::info!(bind = %address, "sandbox listening");
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    match axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sandbox failed: {error}");
            ExitCode::from(3)
        }
    }
}
