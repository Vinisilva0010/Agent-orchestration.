mod common;

use axum::http::StatusCode;
use sqlx::PgPool;

#[sqlx::test(migrations = "./migrations")]
async fn health_returns_ok_without_authentication(pool: PgPool) {
    let app = common::test_app(pool);
    let response = common::send(&app, "GET", "/health", None, None).await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = common::json_body(response).await;
    assert_eq!(json["status"], "ok");
    assert_eq!(json["service"], "gateway");
}

#[sqlx::test(migrations = "./migrations")]
async fn unknown_route_returns_not_found(pool: PgPool) {
    let app = common::test_app(pool);
    let response = common::send(&app, "GET", "/does-not-exist", None, None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
