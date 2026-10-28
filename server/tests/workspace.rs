//! Keys, limits and usage through a fake gateway: administrators only, the
//! admin secret on every call, and keys never coming back.

use std::net::SocketAddr;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::connect_info::MockConnectInfo,
    http::{Method, Request, StatusCode, header},
};
use secrecy::SecretString;
use serde_json::{Value, json};
use server::{
    accounts::{self, Role},
    gateway_admin::GatewayAdmin,
    http::{AppState, router},
};
use sqlx::PgPool;
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header as header_is, method, path},
};

const ORIGIN: &str = "http://localhost:3000";
const PASSPHRASE: &str = "correct horse battery staple";
const ADMIN_SECRET: &str = "server-test-admin-secret-0123456789";
const KEY: &str = "sk-ant-test-key-0123456789abcdef";

fn with_mock_peer(app: Router) -> Router {
    app.layer(MockConnectInfo(SocketAddr::from(([203, 0, 113, 7], 4000))))
}

async fn setup(pool: &PgPool, gateway: &MockServer) -> (Router, i64) {
    server::migrate_all(pool).await.unwrap();
    let secret = SecretString::from(PASSPHRASE.to_string());
    let admin_id = accounts::bootstrap_admin(pool, "admin@example.com", "Admin", &secret)
        .await
        .unwrap();
    accounts::create_user(
        pool,
        "test",
        "alice@example.com",
        "Alice",
        Role::Member,
        &secret,
    )
    .await
    .unwrap();
    let client =
        GatewayAdmin::new(&gateway.uri(), SecretString::from(ADMIN_SECRET.to_string())).unwrap();
    let state = AppState::new(pool.clone(), ORIGIN)
        .unwrap()
        .with_gateway(client);
    (with_mock_peer(router(state)), admin_id)
}

async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, String) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::ORIGIN, ORIGIN);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let request = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response.headers().get(header::SET_COOKIE).map(|value| {
        value
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    });
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    (status, cookie.unwrap_or(text))
}

async fn sign_in(app: &Router, email: &str) -> String {
    let (status, cookie) = call(
        app,
        Method::POST,
        "/api/session",
        None,
        Some(json!({ "email": email, "password": PASSPHRASE })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    cookie
}

#[sqlx::test(migrations = false)]
async fn only_administrators_reach_the_gateway(pool: PgPool) {
    let gateway = MockServer::start().await;
    let (app, _) = setup(&pool, &gateway).await;
    let alice = sign_in(&app, "alice@example.com").await;
    let attempts = [
        (Method::GET, "/api/credentials", None),
        (
            Method::PUT,
            "/api/credentials/anthropic",
            Some(json!({ "api_key": KEY })),
        ),
        (Method::DELETE, "/api/credentials/anthropic", None),
        (
            Method::PUT,
            "/api/agents/support/budget",
            Some(json!({ "monthly_budget_micros": 1 })),
        ),
        (Method::GET, "/api/usage", None),
    ];
    for (method, uri, body) in attempts {
        assert_eq!(
            call(&app, method, uri, Some(&alice), body).await.0,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
    }
    assert!(
        gateway.received_requests().await.unwrap().is_empty(),
        "a member reached the gateway"
    );
}

#[sqlx::test(migrations = false)]
async fn keys_go_to_the_gateway_with_the_admin_secret_and_never_come_back(pool: PgPool) {
    let gateway = MockServer::start().await;
    let (app, admin_id) = setup(&pool, &gateway).await;
    Mock::given(method("PUT"))
        .and(path("/internal/credentials/anthropic"))
        .and(header_is(
            "authorization",
            format!("Bearer {ADMIN_SECRET}").as_str(),
        ))
        .and(body_json(json!({ "api_key": KEY })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&gateway)
        .await;
    Mock::given(method("PUT"))
        .and(path("/internal/credentials/openai"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({ "error": { "code": "bad_key", "message": format!("bad key {KEY}") } }),
        ))
        .mount(&gateway)
        .await;
    let admin = sign_in(&app, "admin@example.com").await;

    let (status, body) = call(
        &app,
        Method::PUT,
        "/api/credentials/anthropic",
        Some(&admin),
        Some(json!({ "api_key": KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!body.contains(KEY));

    let (status, body) = call(
        &app,
        Method::PUT,
        "/api/credentials/openai",
        Some(&admin),
        Some(json!({ "api_key": KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        !body.contains(KEY),
        "a gateway error leaked the key: {body}"
    );

    let (status, _) = call(
        &app,
        Method::PUT,
        "/api/credentials/mistral",
        Some(&admin),
        Some(json!({ "api_key": KEY })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unknown providers never reach the gateway"
    );

    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT action, actor, details::text FROM audit_events WHERE action LIKE 'admin.%' ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    for (action, actor, details) in &rows {
        assert_eq!(action, "admin.credential_store");
        assert_eq!(actor, &format!("user:{admin_id}"));
        assert!(!details.contains(KEY), "key in the audit log: {details}");
    }
    assert!(rows[0].2.contains("\"ok\"") && rows[1].2.contains("\"failed\""));
}

#[sqlx::test(migrations = false)]
async fn limits_and_usage_are_forwarded_and_need_a_configured_gateway(pool: PgPool) {
    let gateway = MockServer::start().await;
    let (app, _) = setup(&pool, &gateway).await;
    Mock::given(method("PUT"))
        .and(path("/internal/agents/support/budget"))
        .and(header_is(
            "authorization",
            format!("Bearer {ADMIN_SECRET}").as_str(),
        ))
        .and(body_json(json!({ "monthly_budget_micros": 5_000_000 })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({ "agent_id": "support", "monthly_budget_micros": 5_000_000 }),
            ),
        )
        .expect(1)
        .mount(&gateway)
        .await;
    Mock::given(method("GET"))
        .and(path("/internal/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "limit_micros": 100, "spent_micros": 5, "reserved_micros": 0, "period": "2026-10" })))
        .mount(&gateway)
        .await;
    let admin = sign_in(&app, "admin@example.com").await;

    let budget = json!({ "monthly_budget_micros": 5_000_000 });
    assert_eq!(
        call(
            &app,
            Method::PUT,
            "/api/agents/support/budget",
            Some(&admin),
            Some(budget)
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let (status, body) = call(&app, Method::GET, "/api/usage", Some(&admin), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("2026-10"), "{body}");

    let unconfigured = with_mock_peer(router(AppState::new(pool.clone(), ORIGIN).unwrap()));
    assert_eq!(
        call(&unconfigured, Method::GET, "/api/usage", Some(&admin), None)
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}
