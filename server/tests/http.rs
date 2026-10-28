//! Sign-in, sessions and origin checks through the real router.

use std::net::SocketAddr;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::connect_info::MockConnectInfo,
    http::{HeaderMap, Request, StatusCode, header},
};
use secrecy::SecretString;
use server::{
    accounts::{self, Role},
    http::{AppState, router},
};
use sqlx::PgPool;
use tower::ServiceExt;

const ORIGIN: &str = "http://localhost:3000";
const PASSPHRASE: &str = "correct horse battery staple";
const WRONG: &str = "a wrong passphrase here";

async fn app(pool: &PgPool) -> Router {
    server::migrate_all(pool).await.unwrap();
    accounts::create_user(
        pool,
        "test",
        "alice@example.com",
        "Alice",
        Role::Member,
        &SecretString::from(PASSPHRASE.to_string()),
    )
    .await
    .unwrap();
    router(AppState::new(pool.clone(), ORIGIN).unwrap())
        .layer(MockConnectInfo(SocketAddr::from(([203, 0, 113, 7], 4000))))
}

fn sign_in(email: &str, password: &str, origin: Option<&str>) -> Request<Body> {
    let mut builder =
        Request::post("/api/session").header(header::CONTENT_TYPE, "application/json");
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    builder
        .body(Body::from(
            serde_json::json!({ "email": email, "password": password }).to_string(),
        ))
        .unwrap()
}

fn me(cookie: &str) -> Request<Body> {
    Request::get("/api/me")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, String) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, headers, String::from_utf8(body.to_vec()).unwrap())
}

/// The `name=value` part of the Set-Cookie header.
fn cookie(headers: &HeaderMap) -> String {
    let set = headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
    set.split(';').next().unwrap().to_string()
}

#[sqlx::test(migrations = false)]
async fn sign_in_sets_a_strict_host_cookie_and_sign_out_ends_the_session(pool: PgPool) {
    let app = app(&pool).await;
    let (status, headers, _) =
        send(&app, sign_in("Alice@Example.com", PASSPHRASE, Some(ORIGIN))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let set_cookie = headers
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    for part in [
        "__Host-ao_session=",
        "Path=/",
        "Secure",
        "HttpOnly",
        "SameSite=Strict",
    ] {
        assert!(set_cookie.contains(part), "missing {part}: {set_cookie}");
    }
    assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");

    let cookie = cookie(&headers);
    let token = cookie.strip_prefix("__Host-ao_session=").unwrap();
    let stored: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sessions WHERE token_hash = sha256(convert_to($1, 'UTF8'))",
    )
    .bind(token)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored, 1, "only the token's SHA-256 is stored");

    let (status, _, body) = send(&app, me(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("alice@example.com") && !body.contains("argon2"),
        "{body}"
    );

    let sign_out = Request::delete("/api/session")
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, ORIGIN)
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = send(&app, sign_out).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        headers
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(send(&app, me(&cookie)).await.0, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = false)]
async fn a_wrong_password_and_an_unknown_account_get_the_same_answer(pool: PgPool) {
    let app = app(&pool).await;
    let wrong = send(&app, sign_in("alice@example.com", WRONG, Some(ORIGIN))).await;
    let unknown = send(
        &app,
        sign_in("nobody@example.com", PASSPHRASE, Some(ORIGIN)),
    )
    .await;
    assert_eq!(wrong.0, StatusCode::UNAUTHORIZED);
    assert_eq!((wrong.0, &wrong.2), (unknown.0, &unknown.2));
    assert!(wrong.1.get(header::SET_COOKIE).is_none());
}

#[sqlx::test(migrations = false)]
async fn ten_wrong_passwords_lock_the_account(pool: PgPool) {
    let app = app(&pool).await;
    for _ in 0..10 {
        assert_eq!(
            send(&app, sign_in("alice@example.com", WRONG, Some(ORIGIN)))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        send(&app, sign_in("alice@example.com", PASSPHRASE, Some(ORIGIN)))
            .await
            .0,
        StatusCode::UNAUTHORIZED,
        "a locked account must not sign in, even with the right password"
    );
    let locked: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = 'user.locked'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(locked, 1);
}

#[sqlx::test(migrations = false)]
async fn state_changes_from_other_origins_are_refused(pool: PgPool) {
    let app = app(&pool).await;
    assert_eq!(
        send(
            &app,
            sign_in(
                "alice@example.com",
                PASSPHRASE,
                Some("https://evil.example")
            )
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, sign_in("alice@example.com", PASSPHRASE, None))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let anonymous = Request::get("/api/me").body(Body::empty()).unwrap();
    assert_eq!(send(&app, anonymous).await.0, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = false)]
async fn one_address_gets_twenty_sign_in_attempts_per_window(pool: PgPool) {
    let app = app(&pool).await;
    for _ in 0..20 {
        assert_eq!(
            send(
                &app,
                sign_in("nobody@example.com", PASSPHRASE, Some(ORIGIN))
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        send(&app, sign_in("alice@example.com", PASSPHRASE, Some(ORIGIN)))
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[sqlx::test(migrations = false)]
async fn sessions_end_when_idle_or_when_the_account_changes(pool: PgPool) {
    let app = app(&pool).await;

    let idle = cookie(
        &send(&app, sign_in("alice@example.com", PASSPHRASE, Some(ORIGIN)))
            .await
            .1,
    );
    sqlx::query("UPDATE sessions SET last_seen_at = now() - interval '61 minutes'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        send(&app, me(&idle)).await.0,
        StatusCode::UNAUTHORIZED,
        "idle session"
    );

    let before_change = cookie(
        &send(&app, sign_in("alice@example.com", PASSPHRASE, Some(ORIGIN)))
            .await
            .1,
    );
    sqlx::query("UPDATE users SET password_changed_at = now()")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        send(&app, me(&before_change)).await.0,
        StatusCode::UNAUTHORIZED,
        "password changed"
    );

    let before_disable = cookie(
        &send(&app, sign_in("alice@example.com", PASSPHRASE, Some(ORIGIN)))
            .await
            .1,
    );
    sqlx::query("UPDATE users SET disabled_at = now()")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        send(&app, me(&before_disable)).await.0,
        StatusCode::UNAUTHORIZED,
        "disabled account"
    );
    assert_eq!(
        send(&app, sign_in("alice@example.com", PASSPHRASE, Some(ORIGIN)))
            .await
            .0,
        StatusCode::UNAUTHORIZED,
        "a disabled account must not sign in"
    );
}
