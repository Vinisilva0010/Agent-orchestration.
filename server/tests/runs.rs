//! Requests about waiting runs: who may make them, one at a time, and each
//! claimed by exactly one worker.

use std::net::SocketAddr;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::connect_info::MockConnectInfo,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use engine::store::RunStore;
use secrecy::SecretString;
use serde_json::{Value, json};
use server::{
    accounts::{self, Role},
    http::{AppState, router},
};
use sqlx::PgPool;
use tower::ServiceExt;

const ORIGIN: &str = "http://localhost:3000";
const PASSPHRASE: &str = "correct horse battery staple";
const RUN: &str = "run_0123456789abcdef0123456789abcdef";
const BUDGET_RUN: &str = "run_fedcba9876543210fedcba9876543210";

struct Setup {
    app: Router,
    alice_id: i64,
}

async fn setup(pool: &PgPool) -> Setup {
    server::migrate_all(pool).await.unwrap();
    let secret = SecretString::from(PASSPHRASE.to_string());
    accounts::bootstrap_admin(pool, "admin@example.com", "Admin", &secret)
        .await
        .unwrap();
    let alice_id = accounts::create_user(
        pool,
        "test",
        "alice@example.com",
        "Alice",
        Role::Member,
        &secret,
    )
    .await
    .unwrap();
    let vera_id = accounts::create_user(
        pool,
        "test",
        "vera@example.com",
        "Vera",
        Role::Viewer,
        &secret,
    )
    .await
    .unwrap();
    accounts::create_user(
        pool,
        "test",
        "bob@example.com",
        "Bob",
        Role::Member,
        &secret,
    )
    .await
    .unwrap();
    for user_id in [alice_id, vera_id] {
        sqlx::query("INSERT INTO agent_permissions (user_id, agent_id, level) VALUES ($1, 'support', 'use')")
            .bind(user_id)
            .execute(pool)
            .await
            .unwrap();
    }
    for (id, status) in [(RUN, "awaiting_approval"), (BUDGET_RUN, "awaiting_budget")] {
        sqlx::query(
            "INSERT INTO engine_runs (id, agent_id, tier, budget_micros, status, state) \
             VALUES ($1, 'support', 'small', 100, $2, $3::jsonb)",
        )
        .bind(id)
        .bind(status)
        .bind(r#"{"pending": [{"id": "call_1", "name": "archive_report", "input": {}}], "approved": null}"#)
        .execute(pool)
        .await
        .unwrap();
    }
    let app = router(AppState::new(pool.clone(), ORIGIN).unwrap())
        .layer(MockConnectInfo(SocketAddr::from(([203, 0, 113, 7], 4000))));
    Setup { app, alice_id }
}

async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, String) {
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
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn sign_in(app: &Router, email: &str) -> String {
    let body = json!({ "email": email, "password": PASSPHRASE });
    let (status, headers, _) = call(app, Method::POST, "/api/session", None, Some(body)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{email} could not sign in");
    let set = headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
    set.split(';').next().unwrap().to_string()
}

#[sqlx::test(migrations = false)]
async fn only_people_allowed_on_the_agent_decide_once_and_each_request_is_claimed_once(
    pool: PgPool,
) {
    let t = setup(&pool).await;
    let alice = sign_in(&t.app, "alice@example.com").await;
    let vera = sign_in(&t.app, "vera@example.com").await;
    let bob = sign_in(&t.app, "bob@example.com").await;
    let uri = format!("/api/runs/{RUN}/decision");
    let approve = json!({ "approve": ["call_1"] });

    assert_eq!(
        call(
            &t.app,
            Method::POST,
            &uri,
            Some(&vera),
            Some(approve.clone())
        )
        .await
        .0,
        StatusCode::FORBIDDEN,
        "viewers never decide"
    );
    assert_eq!(
        call(
            &t.app,
            Method::POST,
            &uri,
            Some(&bob),
            Some(approve.clone())
        )
        .await
        .0,
        StatusCode::FORBIDDEN,
        "no permission on the agent"
    );
    assert_eq!(
        call(
            &t.app,
            Method::POST,
            &uri,
            Some(&alice),
            Some(json!({ "approve": ["call_9"] }))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY,
        "only waiting actions can be approved"
    );
    assert_eq!(
        call(
            &t.app,
            Method::POST,
            &uri,
            Some(&alice),
            Some(approve.clone())
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        call(&t.app, Method::POST, &uri, Some(&alice), Some(approve))
            .await
            .0,
        StatusCode::CONFLICT,
        "one waiting request per run"
    );

    let (_, _, listed_for_bob) = call(&t.app, Method::GET, "/api/runs", Some(&bob), None).await;
    assert!(!listed_for_bob.contains(RUN), "{listed_for_bob}");
    let (_, _, listed_for_alice) = call(&t.app, Method::GET, "/api/runs", Some(&alice), None).await;
    assert!(
        listed_for_alice.contains(RUN) && listed_for_alice.contains("archive_report"),
        "{listed_for_alice}"
    );

    let store = RunStore::new(pool.clone());
    let claimed = store.claim_request().await.unwrap().unwrap();
    assert_eq!(
        (claimed.run_id.as_str(), claimed.kind.as_str()),
        (RUN, "decision")
    );
    assert_eq!(claimed.payload["approve"][0], "call_1");
    assert!(
        store.claim_request().await.unwrap().is_none(),
        "a request is claimed once"
    );
    store.finish_request(claimed.id, "completed").await.unwrap();

    let actor: String =
        sqlx::query_scalar("SELECT actor FROM audit_events WHERE action = 'run_request.created'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(actor, format!("user:{}", t.alice_id));
    let outcome: String = sqlx::query_scalar("SELECT outcome FROM run_requests")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(outcome, "completed");
}

#[sqlx::test(migrations = false)]
async fn budget_is_for_administrators_and_cancelling_needs_manage(pool: PgPool) {
    let t = setup(&pool).await;
    let admin = sign_in(&t.app, "admin@example.com").await;
    let alice = sign_in(&t.app, "alice@example.com").await;
    let budget = format!("/api/runs/{BUDGET_RUN}/budget");

    assert_eq!(
        call(
            &t.app,
            Method::POST,
            &budget,
            Some(&alice),
            Some(json!({ "budget_micros": 1000 }))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &t.app,
            Method::POST,
            &budget,
            Some(&admin),
            Some(json!({ "budget_micros": 50 }))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a raise must be higher than the current budget"
    );
    assert_eq!(
        call(
            &t.app,
            Method::POST,
            &budget,
            Some(&admin),
            Some(json!({ "budget_micros": 1000 }))
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        call(
            &t.app,
            Method::POST,
            &format!("/api/runs/{BUDGET_RUN}/decision"),
            Some(&admin),
            Some(json!({ "approve": [] }))
        )
        .await
        .0,
        StatusCode::CONFLICT,
        "a run waiting for budget is not waiting for approval"
    );

    let cancel = format!("/api/runs/{RUN}/cancel");
    assert_eq!(
        call(&t.app, Method::POST, &cancel, Some(&alice), None)
            .await
            .0,
        StatusCode::FORBIDDEN,
        "use is not enough to cancel"
    );
    sqlx::query("UPDATE agent_permissions SET level = 'manage' WHERE user_id = $1")
        .bind(t.alice_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&t.app, Method::POST, &cancel, Some(&alice), None)
            .await
            .0,
        StatusCode::ACCEPTED
    );
}
