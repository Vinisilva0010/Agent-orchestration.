//! The sandbox as a service on loopback: the engine sends code with the
//! sandbox secret and gets the result. Only this process uses Docker.

use std::sync::Arc;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;

use crate::{Limits, MAX_CODE_BYTES, Sandbox, SandboxError};

pub const MAX_CONCURRENT_RUNS: usize = 4;

#[derive(Clone)]
pub struct ServiceState {
    sandbox: Arc<Sandbox>,
    /// Hash of the sandbox secret. The plain secret is not kept.
    secret_hash: [u8; 32],
    permits: Arc<Semaphore>,
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(bytes);
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&digest);
    hash
}

impl ServiceState {
    pub fn new(sandbox: Sandbox, secret: &SecretString) -> Result<Self, String> {
        if secret.expose_secret().chars().count() < 32 {
            return Err("SANDBOX_SECRET must have at least 32 characters".to_string());
        }
        Ok(Self {
            sandbox: Arc::new(sandbox),
            secret_hash: sha256(secret.expose_secret().as_bytes()),
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_RUNS)),
        })
    }
}

pub fn router(state: ServiceState) -> Router {
    Router::new()
        .route("/v1/run", post(run))
        .layer(DefaultBodyLimit::max(MAX_CODE_BYTES + 4096))
        .with_state(state)
}

fn authorized(headers: &HeaderMap, secret_hash: &[u8; 32]) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|presented| bool::from(sha256(presented.as_bytes()).ct_eq(secret_hash)))
}

fn error(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    (
        status,
        Json(json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunRequest {
    task_id: String,
    code: String,
}

#[derive(Serialize)]
struct RunResponse {
    exit_code: Option<i32>,
    output: String,
    timed_out: bool,
    truncated: bool,
    files: Vec<String>,
}

/// The secret is checked before the body is read.
async fn run(State(state): State<ServiceState>, headers: HeaderMap, body: Bytes) -> Response {
    if !authorized(&headers, &state.secret_hash) {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "a valid sandbox secret is required",
        );
    }
    let Ok(request) = serde_json::from_slice::<RunRequest>(&body) else {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            "expected task_id and code",
        );
    };
    let Ok(_permit) = Arc::clone(&state.permits).try_acquire_owned() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "busy",
            "too many runs at once; try again shortly",
        );
    };
    match state
        .sandbox
        .run_python(&request.task_id, &request.code, &Limits::default())
        .await
    {
        Ok(output) => {
            if !output.stderr.trim().is_empty() {
                tracing::warn!(task_id = %request.task_id, messages = %output.stderr, "sandbox messages");
            }
            Json(RunResponse {
                exit_code: output.exit_code,
                output: output.stdout,
                timed_out: output.timed_out,
                truncated: output.truncated,
                files: output.files,
            })
            .into_response()
        }
        Err(SandboxError::InvalidTask) => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_task",
            "task ids are 1 to 128 letters, digits, '-' or '_'",
        ),
        Err(SandboxError::CodeTooLarge) => error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "code_too_large",
            "the code is larger than 256 KiB",
        ),
        Err(other) => {
            tracing::error!(error = %other, "sandbox run failed");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "the sandbox could not run the code",
            )
        }
    }
}
