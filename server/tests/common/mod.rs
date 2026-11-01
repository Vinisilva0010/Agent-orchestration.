//! Shared setup for the task and run tests: a workspace with an
//! administrator, two members and a viewer, signed in through the real router.

#![allow(dead_code)]

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
    http::{AppState, router},
};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;

pub const ORIGIN: &str = "http://localhost:3000";
const PASSWORD: &str = "a long passphrase for tests";

pub struct Person {
    pub id: i64,
    pub cookie: String,
}

pub struct Setup {
    pub app: Router,
    pub admin: Person,
    /// Member who gets permissions in the tests.
    pub alice: Person,
    /// Member with no permission at all.
    pub bob: Person,
    pub viewer: Person,
}

fn secret(value: &str) -> SecretString {
    SecretString::from(value.to_string())
}

pub async fn setup(pool: &PgPool) -> Setup {
    server::migrate_all(pool).await.unwrap();
    let admin_id = accounts::bootstrap_admin(pool, "admin@example.com", "Admin", &secret(PASSWORD))
        .await
        .unwrap();
    let mut people = Vec::new();
    for (email, name, role) in [
        ("alice@example.com", "Alice", Role::Member),
        ("bob@example.com", "Bob", Role::Member),
        ("viewer@example.com", "Vera", Role::Viewer),
    ] {
        let id = accounts::create_user(pool, "test", email, name, role, &secret(PASSWORD))
            .await
            .unwrap();
        people.push((id, email));
    }
    let server_pool = server_pool(pool).await;
    let app = router(AppState::new(server_pool, ORIGIN).unwrap())
        .layer(MockConnectInfo(SocketAddr::from(([203, 0, 113, 7], 4000))));
    let admin = Person {
        id: admin_id,
        cookie: sign_in(&app, "admin@example.com").await,
    };
    let mut signed = Vec::new();
    for (id, email) in people {
        signed.push(Person {
            id,
            cookie: sign_in(&app, email).await,
        });
    }
    let viewer = signed.pop().unwrap();
    let bob = signed.pop().unwrap();
    let alice = signed.pop().unwrap();
    Setup {
        app,
        admin,
        alice,
        bob,
        viewer,
    }
}

/// A pool whose connections act as the server's own database role, as in
/// production, so tests also prove its privileges are enough. Migrations and
/// test fixtures keep using the owner's pool.
pub async fn server_pool(pool: &PgPool) -> PgPool {
    PgPoolOptions::new()
        .max_connections(5)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET ROLE ao_server").execute(conn).await?;
                Ok(())
            })
        })
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap()
}

async fn sign_in(app: &Router, email: &str) -> String {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/session")
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "email": email, "password": PASSWORD }).to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert!(response.status().is_success(), "sign-in failed");
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    cookie.split(';').next().unwrap().to_string()
}

pub async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    cookie: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::ORIGIN, ORIGIN)
        .header(header::COOKIE, cookie);
    let request = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 22).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Creates an agent through the API as the administrator.
pub async fn create_agent(setup: &Setup, id: &str, budget_micros: i64) {
    let (status, _) = call(
        &setup.app,
        Method::POST,
        "/api/agents",
        &setup.admin.cookie,
        Some(json!({
            "id": id,
            "display_name": format!("Agent {id}"),
            "tier": "small",
            "task_budget_micros": budget_micros,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

/// Grants a permission through the API as the administrator.
pub async fn grant(setup: &Setup, user_id: i64, agent_id: &str, level: &str) {
    let (status, _) = call(
        &setup.app,
        Method::PUT,
        &format!("/api/users/{user_id}/agents/{agent_id}"),
        &setup.admin.cookie,
        Some(json!({ "level": level })),
    )
    .await;
    assert!(status.is_success(), "grant failed: {status}");
}

/// Runs SQL as the tests' owner connection with an audit actor.
pub async fn as_test(pool: &PgPool, sql: &'static str) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('ao.actor', 'test', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::raw_sql(sql).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}
