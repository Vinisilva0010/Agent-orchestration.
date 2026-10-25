mod common;

use axum::http::StatusCode;

#[tokio::test]
async fn health_returns_ok_without_authentication() {
    let app = common::test_app();
    let response = common::send(&app, "GET", "/health", None, None).await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = common::json_body(response).await;
    assert_eq!(json["status"], "ok");
    assert_eq!(json["service"], "gateway");
}

#[tokio::test]
async fn unknown_route_returns_not_found() {
    let app = common::test_app();
    let response = common::send(&app, "GET", "/does-not-exist", None, None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
