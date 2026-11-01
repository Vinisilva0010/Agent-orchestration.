//! Agent registry rules through the real router.

mod common;

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
use sqlx::PgPool;
use tower::ServiceExt;

const ORIGIN: &str = "http://localhost:3000";
const ADMIN_PASSWORD: &str = "admin passphrase for tests";
const ALICE_PASSWORD: &str = "correct horse battery staple";

struct Setup {
    app: Router,
    admin: String,
    alice: String,
    admin_id: i64,
    alice_id: i64,
}

fn secret(value: &str) -> SecretString {
    SecretString::from(value.to_string())
}

async fn setup(pool: &PgPool) -> Setup {
    server::migrate_all(pool).await.unwrap();
    let admin_id =
        accounts::bootstrap_admin(pool, "admin@example.com", "Admin", &secret(ADMIN_PASSWORD))
            .await
            .unwrap();
    let alice_id = accounts::create_user(
        pool,
        "test",
        "alice@example.com",
        "Alice",
        Role::Member,
        &secret(ALICE_PASSWORD),
    )
    .await
    .unwrap();
    let app = router(AppState::new(common::server_pool(pool).await, ORIGIN).unwrap())
        .layer(MockConnectInfo(SocketAddr::from(([203, 0, 113, 7], 4000))));
    let admin = sign_in(&app, "admin@example.com", ADMIN_PASSWORD).await;
    let alice = sign_in(&app, "alice@example.com", ALICE_PASSWORD).await;
    Setup {
        app,
        admin,
        alice,
        admin_id,
        alice_id,
    }
}

async fn sign_in(app: &Router, email: &str, password: &str) -> String {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/session")
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "email": email, "password": password }).to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert!(
        response.status().is_success(),
        "sign-in failed: {}",
        response.status()
    );
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    cookie.split(';').next().unwrap().to_string()
}

async fn call(
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
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn agent(id: &str, name: &str) -> Value {
    json!({
        "id": id,
        "display_name": name,
        "description": "Answers customer tickets.",
        "task_budget_micros": 500_000,
    })
}

async fn create(setup: &Setup, id: &str, name: &str) {
    let (status, _) = call(
        &setup.app,
        Method::POST,
        "/api/agents",
        &setup.admin,
        Some(agent(id, name)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

/// Writes as the tests' administrator connection, with an audit actor.
async fn as_test(pool: &PgPool, sql: &'static str, user_id: Option<i64>) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('ao.actor', 'test', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let query = sqlx::query(sql);
    let query = match user_id {
        Some(id) => query.bind(id),
        None => query,
    };
    query.execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = false)]
async fn only_admins_create_agents(pool: PgPool) {
    let setup = setup(&pool).await;
    let (status, _) = call(
        &setup.app,
        Method::POST,
        "/api/agents",
        &setup.alice,
        Some(agent("support", "Suporte")),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, body) = call(
        &setup.app,
        Method::POST,
        "/api/agents",
        &setup.admin,
        Some(agent("support", "Suporte")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["id"], "support");

    let (status, body) = call(
        &setup.app,
        Method::POST,
        "/api/agents",
        &setup.admin,
        Some(agent("support", "Outro")),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "agent_exists");

    let (status, _) = call(
        &setup.app,
        Method::PATCH,
        "/api/agents/support",
        &setup.alice,
        Some(json!({ "archived": true })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = false)]
async fn agent_fields_are_validated_and_the_id_never_changes(pool: PgPool) {
    let setup = setup(&pool).await;
    let bad = [
        json!({ "id": "bad id", "display_name": "X", "task_budget_micros": 1 }),
        json!({ "id": "ok", "display_name": "   ", "task_budget_micros": 1 }),
        json!({ "id": "ok", "display_name": "X", "task_budget_micros": 0 }),
        json!({ "id": "ok", "display_name": "X", "task_budget_micros": 100_000_001 }),
        json!({ "id": "ok", "display_name": "X", "tier": "huge", "task_budget_micros": 1 }),
        json!({ "id": "ok", "display_name": "X", "description": "d".repeat(501), "task_budget_micros": 1 }),
    ];
    for body in bad {
        let (status, _) = call(
            &setup.app,
            Method::POST,
            "/api/agents",
            &setup.admin,
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }

    create(&setup, "support", "Suporte").await;
    for body in [
        json!({ "id": "other" }),
        json!({}),
        json!({ "task_budget_micros": -5 }),
    ] {
        let (status, _) = call(
            &setup.app,
            Method::PATCH,
            "/api/agents/support",
            &setup.admin,
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    let (status, _) = call(
        &setup.app,
        Method::PATCH,
        "/api/agents/missing",
        &setup.admin,
        Some(json!({ "archived": true })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = false)]
async fn people_see_only_the_agents_they_may_use(pool: PgPool) {
    let setup = setup(&pool).await;
    create(&setup, "support", "Suporte").await;
    create(&setup, "finance", "Financeiro").await;

    let (_, list) = call(&setup.app, Method::GET, "/api/agents", &setup.alice, None).await;
    assert_eq!(list, json!([]));

    as_test(
        &pool,
        "INSERT INTO agent_permissions (user_id, agent_id, level) VALUES ($1, 'support', 'use')",
        Some(setup.alice_id),
    )
    .await;
    let (_, list) = call(&setup.app, Method::GET, "/api/agents", &setup.alice, None).await;
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], "support");
    assert_eq!(list[0]["can_use"], true);
    assert_eq!(list[0]["can_manage"], false);

    let (status, _) = call(
        &setup.app,
        Method::GET,
        "/api/agents/finance",
        &setup.alice,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &setup.app,
        Method::GET,
        "/api/agents/support",
        &setup.alice,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = call(
        &setup.app,
        Method::PATCH,
        "/api/agents/support",
        &setup.admin,
        Some(json!({ "archived": true })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, list) = call(&setup.app, Method::GET, "/api/agents", &setup.alice, None).await;
    assert_eq!(list, json!([]));

    let (_, list) = call(&setup.app, Method::GET, "/api/agents", &setup.admin, None).await;
    let support = list
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "support")
        .unwrap()
        .clone();
    assert_eq!(support["archived"], true);
    assert_eq!(support["can_use"], false);
}

#[sqlx::test(migrations = false)]
async fn agents_show_this_months_spend(pool: PgPool) {
    let setup = setup(&pool).await;
    create(&setup, "support", "Suporte").await;
    as_test(
        &pool,
        "INSERT INTO monthly_spend (scope, scope_id, period, spent_micros) VALUES \
         ('agent', 'support', to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM'), 3120000), \
         ('agent', 'support', '2000-01', 999)",
        None,
    )
    .await;
    as_test(
        &pool,
        "INSERT INTO agent_budgets (agent_id, monthly_limit_micros) VALUES ('support', 20000000)",
        None,
    )
    .await;
    let (_, body) = call(
        &setup.app,
        Method::GET,
        "/api/agents/support",
        &setup.admin,
        None,
    )
    .await;
    assert_eq!(body["month_spent_micros"], 3_120_000);
    assert_eq!(body["month_limit_micros"], 20_000_000);
    assert_eq!(body["waiting"], 0);
}

#[sqlx::test(migrations = false)]
async fn agent_changes_are_audited_with_the_person(pool: PgPool) {
    let setup = setup(&pool).await;
    create(&setup, "support", "Suporte").await;
    let (status, _) = call(
        &setup.app,
        Method::PATCH,
        "/api/agents/support",
        &setup.admin,
        Some(json!({ "display_name": "Atendimento", "task_budget_micros": 1_000_000 })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let events: Vec<String> = sqlx::query_scalar("SELECT row_to_json(e)::text FROM audit_events e")
        .fetch_all(&pool)
        .await
        .unwrap();
    let actor = format!("user:{}", setup.admin_id);
    let created = events
        .iter()
        .find(|e| e.contains("agent.created"))
        .expect("agent.created");
    assert!(created.contains(&actor), "{created}");
    let changed = events
        .iter()
        .find(|e| e.contains("agent.changed"))
        .expect("agent.changed");
    assert!(
        changed.contains(&actor) && changed.contains("Atendimento"),
        "{changed}"
    );
}
