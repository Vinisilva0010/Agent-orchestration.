mod common;

use axum::http::StatusCode;
use serde_json::json;
use sqlx::PgPool;

const KEY: &str = "sk-test-not-a-real-key-1234567890";

#[sqlx::test(migrations = "./migrations")]
async fn credential_routes_require_issuer_secret(pool: PgPool) {
    let app = common::test_app(pool);
    let token = common::issue_token(&app, "task-1").await;

    let as_task = common::send(
        &app,
        "PUT",
        "/internal/credentials/anthropic",
        Some(&token),
        Some(json!({ "api_key": KEY })),
    )
    .await;
    assert_eq!(as_task.status(), StatusCode::UNAUTHORIZED);

    let anonymous = common::send(&app, "GET", "/internal/credentials", None, None).await;
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "./migrations")]
async fn stored_key_is_never_returned(pool: PgPool) {
    let app = common::test_app(pool);
    let stored = common::send(
        &app,
        "PUT",
        "/internal/credentials/anthropic",
        Some(common::ISSUER_SECRET),
        Some(json!({ "api_key": KEY })),
    )
    .await;
    assert_eq!(stored.status(), StatusCode::NO_CONTENT);
    assert!(!common::body_text(stored).await.contains(KEY));

    let listed = common::send(
        &app,
        "GET",
        "/internal/credentials",
        Some(common::ISSUER_SECRET),
        None,
    )
    .await;
    assert_eq!(listed.status(), StatusCode::OK);
    let text = common::body_text(listed).await;
    assert!(text.contains("anthropic"));
    assert!(!text.contains(KEY));

    let single = common::send(
        &app,
        "GET",
        "/internal/credentials/anthropic",
        Some(common::ISSUER_SECRET),
        None,
    )
    .await;
    assert_eq!(single.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[sqlx::test(migrations = "./migrations")]
async fn rejects_unknown_provider_and_invalid_keys(pool: PgPool) {
    let app = common::test_app(pool);
    let unknown = common::send(
        &app,
        "PUT",
        "/internal/credentials/someprovider",
        Some(common::ISSUER_SECRET),
        Some(json!({ "api_key": KEY })),
    )
    .await;
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);

    let with_space = common::send(
        &app,
        "PUT",
        "/internal/credentials/openai",
        Some(common::ISSUER_SECRET),
        Some(json!({ "api_key": "sk has spaces" })),
    )
    .await;
    assert_eq!(with_space.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "./migrations")]
async fn deletes_credentials(pool: PgPool) {
    let app = common::test_app(pool);
    common::send(
        &app,
        "PUT",
        "/internal/credentials/gemini",
        Some(common::ISSUER_SECRET),
        Some(json!({ "api_key": KEY })),
    )
    .await;

    let first = common::send(
        &app,
        "DELETE",
        "/internal/credentials/gemini",
        Some(common::ISSUER_SECRET),
        None,
    )
    .await;
    assert_eq!(first.status(), StatusCode::NO_CONTENT);

    let second = common::send(
        &app,
        "DELETE",
        "/internal/credentials/gemini",
        Some(common::ISSUER_SECRET),
        None,
    )
    .await;
    assert_eq!(second.status(), StatusCode::NOT_FOUND);
}
