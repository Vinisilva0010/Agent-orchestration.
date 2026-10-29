//! The service runs code only for callers holding the sandbox secret.

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use sandbox::{
    Sandbox,
    service::{ServiceState, router},
};
use secrecy::SecretString;
use serde_json::json;
use tower::ServiceExt;

const SECRET: &str = "sandbox-test-secret-0123456789abcdef";

fn root() -> std::path::PathBuf {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).unwrap();
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    std::env::temp_dir().join(format!("ao-sandbox-service-{suffix}"))
}

fn app() -> Router {
    let sandbox = Sandbox::new(root(), None).unwrap();
    router(ServiceState::new(sandbox, &SecretString::from(SECRET.to_string())).unwrap())
}

async fn post(app: &Router, secret: Option<&str>, body: serde_json::Value) -> (StatusCode, String) {
    let mut builder = Request::post("/v1/run").header(header::CONTENT_TYPE, "application/json");
    if let Some(secret) = secret {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {secret}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn the_service_runs_code_only_with_the_secret() {
    sandbox::ensure_image().await.unwrap();
    let app = app();
    let body = json!({ "task_id": "service", "code": "print('hello from the sandbox')" });

    assert_eq!(
        post(&app, None, body.clone()).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(
            &app,
            Some("not-the-secret-not-the-secret-0123"),
            body.clone()
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(
            &app,
            Some(SECRET),
            json!({ "task_id": "../escape", "code": "print(1)" })
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (status, response) = post(&app, Some(SECRET), body).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert!(
        response.contains("hello from the sandbox") && response.contains("\"exit_code\":0"),
        "{response}"
    );
}

#[test]
fn the_service_needs_a_long_secret() {
    let sandbox = Sandbox::new(root(), None).unwrap();
    assert!(ServiceState::new(sandbox, &SecretString::from("short".to_string())).is_err());
}
