//! Administration rules through the real router.

use std::net::SocketAddr;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::connect_info::MockConnectInfo,
    http::{HeaderMap, Method, Request, StatusCode, header},
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

fn secret(value: &str) -> SecretString {
    SecretString::from(value.to_string())
}

struct Setup {
    app: Router,
    admin_id: i64,
    alice_id: i64,
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
    let app = router(AppState::new(pool.clone(), ORIGIN).unwrap())
        .layer(MockConnectInfo(SocketAddr::from(([203, 0, 113, 7], 4000))));
    Setup {
        app,
        admin_id,
        alice_id,
    }
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

fn cookie(headers: &HeaderMap) -> String {
    let set = headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
    set.split(';').next().unwrap().to_string()
}

async fn sign_in(app: &Router, email: &str, password: &str) -> Option<String> {
    let body = json!({ "email": email, "password": password });
    let (status, headers, _) = call(app, Method::POST, "/api/session", None, Some(body)).await;
    (status == StatusCode::NO_CONTENT).then(|| cookie(&headers))
}

#[sqlx::test(migrations = false)]
async fn only_administrators_manage_accounts(pool: PgPool) {
    let t = setup(&pool).await;
    let admin = sign_in(&t.app, "admin@example.com", ADMIN_PASSWORD)
        .await
        .unwrap();
    let alice = sign_in(&t.app, "alice@example.com", ALICE_PASSWORD)
        .await
        .unwrap();
    let bob = json!({ "email": "bob@example.com", "display_name": "Bob", "role": "viewer", "password": "another long passphrase" });

    assert_eq!(
        call(
            &t.app,
            Method::POST,
            "/api/users",
            Some(&alice),
            Some(bob.clone())
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&t.app, Method::GET, "/api/users", Some(&alice), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &t.app,
            Method::POST,
            "/api/users",
            Some(&admin),
            Some(bob.clone())
        )
        .await
        .0,
        StatusCode::CREATED
    );
    assert_eq!(
        call(&t.app, Method::POST, "/api/users", Some(&admin), Some(bob))
            .await
            .0,
        StatusCode::CONFLICT
    );

    let short = json!({ "email": "carol@example.com", "display_name": "Carol", "role": "member", "password": "too short" });
    assert_eq!(
        call(
            &t.app,
            Method::POST,
            "/api/users",
            Some(&admin),
            Some(short)
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let bad_role = json!({ "email": "dave@example.com", "display_name": "Dave", "role": "owner", "password": "another long passphrase" });
    assert_eq!(
        call(
            &t.app,
            Method::POST,
            "/api/users",
            Some(&admin),
            Some(bad_role)
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let (status, _, body) = call(&t.app, Method::GET, "/api/users", Some(&admin), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("bob@example.com") && !body.contains("argon2"),
        "{body}"
    );
    assert!(
        sign_in(&t.app, "bob@example.com", "another long passphrase")
            .await
            .is_some()
    );
}

#[sqlx::test(migrations = false)]
async fn the_last_active_administrator_cannot_be_demoted_or_disabled(pool: PgPool) {
    let t = setup(&pool).await;
    let admin = sign_in(&t.app, "admin@example.com", ADMIN_PASSWORD)
        .await
        .unwrap();
    let me = format!("/api/users/{}", t.admin_id);

    let (status, _, body) = call(
        &t.app,
        Method::PATCH,
        &me,
        Some(&admin),
        Some(json!({ "role": "member" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains("last_admin"), "{body}");
    assert_eq!(
        call(
            &t.app,
            Method::PATCH,
            &me,
            Some(&admin),
            Some(json!({ "disabled": true }))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );

    let alice = format!("/api/users/{}", t.alice_id);
    assert_eq!(
        call(
            &t.app,
            Method::PATCH,
            &alice,
            Some(&admin),
            Some(json!({ "role": "admin" }))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(
            &t.app,
            Method::PATCH,
            &me,
            Some(&admin),
            Some(json!({ "role": "member" }))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let roles: Vec<String> = sqlx::query_scalar("SELECT role FROM users ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(roles, ["member", "admin"]);
}

#[sqlx::test(migrations = false)]
async fn disabling_an_account_ends_its_sessions_at_once(pool: PgPool) {
    let t = setup(&pool).await;
    let admin = sign_in(&t.app, "admin@example.com", ADMIN_PASSWORD)
        .await
        .unwrap();
    let alice = sign_in(&t.app, "alice@example.com", ALICE_PASSWORD)
        .await
        .unwrap();
    let uri = format!("/api/users/{}", t.alice_id);

    assert_eq!(
        call(
            &t.app,
            Method::PATCH,
            &uri,
            Some(&admin),
            Some(json!({ "disabled": true }))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&t.app, Method::GET, "/api/me", Some(&alice), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sessions WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(t.alice_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(open, 0, "every session of a disabled account is revoked");
    assert!(
        sign_in(&t.app, "alice@example.com", ALICE_PASSWORD)
            .await
            .is_none()
    );

    assert_eq!(
        call(
            &t.app,
            Method::PATCH,
            &uri,
            Some(&admin),
            Some(json!({ "disabled": false }))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(
        sign_in(&t.app, "alice@example.com", ALICE_PASSWORD)
            .await
            .is_some()
    );
}

#[sqlx::test(migrations = false)]
async fn agent_permissions_are_recorded_with_the_acting_administrator(pool: PgPool) {
    let t = setup(&pool).await;
    let admin = sign_in(&t.app, "admin@example.com", ADMIN_PASSWORD)
        .await
        .unwrap();
    let uri = format!("/api/users/{}/agents/support", t.alice_id);

    assert_eq!(
        call(
            &t.app,
            Method::PUT,
            &uri,
            Some(&admin),
            Some(json!({ "level": "use" }))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let actor: String = sqlx::query_scalar(
        "SELECT actor FROM audit_events WHERE action = 'agent_permission.granted'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(actor, format!("user:{}", t.admin_id));

    assert_eq!(
        call(
            &t.app,
            Method::PUT,
            &uri,
            Some(&admin),
            Some(json!({ "level": "owner" }))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let bad_agent = format!("/api/users/{}/agents/bad%20id", t.alice_id);
    assert_eq!(
        call(
            &t.app,
            Method::PUT,
            &bad_agent,
            Some(&admin),
            Some(json!({ "level": "use" }))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        call(
            &t.app,
            Method::PUT,
            "/api/users/999999/agents/support",
            Some(&admin),
            Some(json!({ "level": "use" }))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&t.app, Method::DELETE, &uri, Some(&admin), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&t.app, Method::DELETE, &uri, Some(&admin), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrations = false)]
async fn changing_the_password_needs_the_current_one_and_ends_other_sessions(pool: PgPool) {
    let t = setup(&pool).await;
    let first = sign_in(&t.app, "alice@example.com", ALICE_PASSWORD)
        .await
        .unwrap();
    let second = sign_in(&t.app, "alice@example.com", ALICE_PASSWORD)
        .await
        .unwrap();
    let new = "a brand new passphrase here";

    let wrong = json!({ "current_password": "not the current passphrase", "new_password": new });
    assert_eq!(
        call(
            &t.app,
            Method::PUT,
            "/api/me/password",
            Some(&first),
            Some(wrong)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );

    let right = json!({ "current_password": ALICE_PASSWORD, "new_password": new });
    let (status, headers, _) = call(
        &t.app,
        Method::PUT,
        "/api/me/password",
        Some(&first),
        Some(right),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let third = cookie(&headers);

    assert_eq!(
        call(&t.app, Method::GET, "/api/me", Some(&first), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&t.app, Method::GET, "/api/me", Some(&second), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&t.app, Method::GET, "/api/me", Some(&third), None)
            .await
            .0,
        StatusCode::OK
    );
    assert!(
        sign_in(&t.app, "alice@example.com", ALICE_PASSWORD)
            .await
            .is_none()
    );
    assert!(sign_in(&t.app, "alice@example.com", new).await.is_some());
}
