//! The server's HTTP API: sign in, sign out and the signed-in person.
//!
//! Sessions are random 32-byte tokens in a `__Host-` cookie (Secure, HttpOnly,
//! SameSite=Strict); only their SHA-256 is stored. A session ends after 1 hour
//! without use, 12 hours after sign-in, when it is revoked, when the account
//! is disabled, or when the password changes.
//!
//! Requests that change state must come from the configured origin (Origin
//! header, or Sec-Fetch-Site: same-origin), which blocks cross-site request
//! forgery even where a browser ignores SameSite.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{ConnectInfo, FromRequestParts, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header, request::Parts},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};

use crate::accounts::{self, Role};

pub const SESSION_COOKIE: &str = "__Host-ao_session";
const CLEARED_COOKIE: &str =
    "__Host-ao_session=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0";
/// Twelve hours, the absolute session lifetime (also in the SQL below).
const SESSION_MAX_AGE_SECONDS: u32 = 12 * 60 * 60;
/// Sign-in attempts allowed from one address per window, across all accounts.
const SIGN_IN_ATTEMPTS_PER_WINDOW: u32 = 20;
const SIGN_IN_WINDOW: Duration = Duration::from_secs(5 * 60);

#[derive(Debug)]
pub struct ApiError {
    pub(crate) status: StatusCode,
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
}

impl ApiError {
    pub(crate) fn invalid_credentials() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "invalid_credentials",
            message: "invalid email or password",
        }
    }

    pub(crate) fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: "sign in first",
        }
    }

    pub(crate) fn forbidden_origin() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "forbidden_origin",
            message: "the request did not come from the workspace",
        }
    }

    pub(crate) fn too_many_attempts() -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "too_many_attempts",
            message: "too many sign-in attempts; try again later",
        }
    }

    pub(crate) fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal",
            message: "internal error",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": { "code": self.code, "message": self.message } });
        (self.status, Json(body)).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!(error = %error, "database error");
        Self::internal()
    }
}

/// Counts sign-in attempts per client address in fixed windows.
#[derive(Default)]
pub struct SignInLimiter {
    attempts: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

impl SignInLimiter {
    /// Records an attempt and says whether it may go ahead. Fails closed.
    fn allow(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let Ok(mut attempts) = self.attempts.lock() else {
            return false;
        };
        if attempts.len() > 10_000 {
            attempts.retain(|_, (_, start)| now.duration_since(*start) < SIGN_IN_WINDOW);
        }
        let entry = attempts.entry(ip).or_insert((0, now));
        if now.duration_since(entry.1) >= SIGN_IN_WINDOW {
            *entry = (0, now);
        }
        entry.0 = entry.0.saturating_add(1);
        entry.0 <= SIGN_IN_ATTEMPTS_PER_WINDOW
    }
}

#[derive(Clone)]
pub struct AppState {
    pub(crate) pool: PgPool,
    public_origin: Arc<str>,
    limiter: Arc<SignInLimiter>,
    /// Checked when the account does not exist, so every failure costs the
    /// same time.
    dummy_hash: Arc<str>,
}

impl AppState {
    pub fn new(pool: PgPool, public_origin: &str) -> Result<Self, String> {
        let public_origin = validate_origin(public_origin)?;
        let mut random = [0u8; 24];
        getrandom::fill(&mut random).map_err(|_| "the system random generator is unavailable")?;
        let dummy_hash =
            accounts::hash_password(&SecretString::from(URL_SAFE_NO_PAD.encode(random)))
                .map_err(|error| error.to_string())?;
        Ok(Self {
            pool,
            public_origin: public_origin.into(),
            limiter: Arc::default(),
            dummy_hash: dummy_hash.into(),
        })
    }
}

/// `scheme://host[:port]`, exactly as browsers send it in the Origin header.
pub fn validate_origin(origin: &str) -> Result<String, String> {
    let rest = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .ok_or("SERVER_PUBLIC_ORIGIN must start with https:// or http://")?;
    if rest.is_empty() || rest.contains(['/', '?', '#', '@', ' ']) || origin.len() > 200 {
        return Err("SERVER_PUBLIC_ORIGIN must be scheme://host[:port] with no path".to_string());
    }
    Ok(origin.to_string())
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/session", post(sign_in).delete(sign_out))
        .route("/api/me", get(me))
        .merge(crate::admin::routes())
        .merge(crate::runs::routes())
        .layer(middleware::from_fn_with_state(state.clone(), check_origin))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

fn same_origin(headers: &HeaderMap, origin: &str) -> bool {
    match headers.get(header::ORIGIN) {
        Some(value) => value.as_bytes() == origin.as_bytes(),
        None => headers
            .get("sec-fetch-site")
            .is_some_and(|value| value.as_bytes() == b"same-origin"),
    }
}

async fn check_origin(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let safe = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    if safe || same_origin(request.headers(), &state.public_origin) {
        next.run(request).await
    } else {
        ApiError::forbidden_origin().into_response()
    }
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

pub(crate) fn new_token() -> Result<String, ApiError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| ApiError::internal())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub(crate) fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// The session token from the Cookie header, if it has the right shape.
fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| pair.trim().strip_prefix("__Host-ao_session="))
        .filter(|token| {
            token.len() == 43
                && token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .map(str::to_string)
}

pub(crate) fn session_cookie(token: &str) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={token}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={SESSION_MAX_AGE_SECONDS}"
    ))
    .map_err(|_| ApiError::internal())
}

pub(crate) async fn set_actor(conn: &mut PgConnection, actor: &str) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT set_config('ao.actor', $1, true)")
        .bind(actor)
        .execute(conn)
        .await
        .map(|_| ())
}

/// The signed-in person. Extracting it checks the session.
#[derive(Debug, Clone, Serialize)]
pub struct CurrentUser {
    pub id: i64,
    pub email: String,
    pub display_name: String,
    pub role: &'static str,
    #[serde(skip)]
    token_hash: Vec<u8>,
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = session_token(&parts.headers).ok_or_else(ApiError::unauthorized)?;
        let hash = token_hash(&token);
        let row: Option<(i64, String, String, String, bool)> = sqlx::query_as(
            "SELECT u.id, u.email, u.display_name, u.role, s.last_seen_at < now() - interval '1 minute' \
             FROM sessions s JOIN users u ON u.id = s.user_id \
             WHERE s.token_hash = $1 AND s.revoked_at IS NULL AND s.expires_at > now() \
               AND s.last_seen_at > now() - interval '1 hour' \
               AND s.created_at >= u.password_changed_at AND u.disabled_at IS NULL",
        )
        .bind(&hash)
        .fetch_optional(&state.pool)
        .await?;
        let (id, email, display_name, role, stale) = row.ok_or_else(ApiError::unauthorized)?;
        let role = Role::parse(&role).ok_or_else(ApiError::internal)?.as_str();
        if stale {
            sqlx::query("UPDATE sessions SET last_seen_at = now() WHERE token_hash = $1")
                .bind(&hash)
                .execute(&state.pool)
                .await?;
        }
        Ok(Self {
            id,
            email,
            display_name,
            role,
            token_hash: hash,
        })
    }
}

/// Deliberately not Debug, so the password can never be printed.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignInRequest {
    email: String,
    password: String,
}

async fn sign_in(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(request): Json<SignInRequest>,
) -> Result<Response, ApiError> {
    if !state.limiter.allow(peer.ip()) {
        return Err(ApiError::too_many_attempts());
    }
    let password = SecretString::from(request.password);
    let account: Option<(i64, String, bool)> = match accounts::normalize_email(&request.email) {
        Ok(email) => {
            sqlx::query_as(
                "SELECT id, password_hash, \
                        disabled_at IS NOT NULL OR coalesce(locked_until > now(), false) \
                 FROM users WHERE email = $1",
            )
            .bind(email)
            .fetch_optional(&state.pool)
            .await?
        }
        Err(_) => None,
    };
    // The password is checked even when the account is missing, locked or
    // disabled, so every failure takes the same time.
    let hash = account
        .as_ref()
        .map_or_else(|| state.dummy_hash.to_string(), |(_, hash, _)| hash.clone());
    let matches = tokio::task::spawn_blocking(move || accounts::verify_password(&password, &hash))
        .await
        .map_err(|_| ApiError::internal())?;
    let Some((user_id, _, blocked)) = account else {
        return Err(ApiError::invalid_credentials());
    };
    if blocked {
        return Err(ApiError::invalid_credentials());
    }
    if !matches {
        record_failure(&state.pool, user_id).await?;
        return Err(ApiError::invalid_credentials());
    }

    let token = new_token()?;
    let mut tx = state.pool.begin().await?;
    set_actor(&mut tx, &format!("user:{user_id}")).await?;
    sqlx::query("UPDATE users SET failed_logins = 0 WHERE id = $1 AND failed_logins <> 0")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1, $2, now() + interval '12 hours')",
    )
    .bind(token_hash(&token))
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, session_cookie(&token)?);
    Ok(response)
}

/// Ten wrong passwords in a row lock the account for 15 minutes.
pub(crate) async fn record_failure(pool: &PgPool, user_id: i64) -> Result<(), ApiError> {
    let mut tx = pool.begin().await?;
    set_actor(&mut tx, "anonymous").await?;
    sqlx::query(
        "UPDATE users SET \
             failed_logins = CASE WHEN failed_logins + 1 >= 10 THEN 0 ELSE failed_logins + 1 END, \
             locked_until = CASE WHEN failed_logins + 1 >= 10 THEN now() + interval '15 minutes' \
                                 ELSE locked_until END \
         WHERE id = $1",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn sign_out(State(state): State<AppState>, user: CurrentUser) -> Result<Response, ApiError> {
    let mut tx = state.pool.begin().await?;
    set_actor(&mut tx, &format!("user:{}", user.id)).await?;
    sqlx::query(
        "UPDATE sessions SET revoked_at = now() WHERE token_hash = $1 AND revoked_at IS NULL",
    )
    .bind(&user.token_hash)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_static(CLEARED_COOKIE));
    Ok(response)
}

async fn me(user: CurrentUser) -> Json<CurrentUser> {
    Json(user)
}
