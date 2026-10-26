//! Session API and the legacy login/password JSON adapters.

use crate::{Shared, errors, peer, sessions};
use axum::Json;
use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::IpAddr;
use std::time::Instant;

#[derive(Deserialize)]
struct Login {
    #[serde(default)]
    password: String,
}
#[derive(Deserialize)]
struct Password {
    #[serde(default)]
    current: String,
    #[serde(default)]
    password: String,
}
#[derive(Deserialize)]
struct PasswordChange {
    #[serde(default)]
    current_password: String,
    #[serde(default)]
    new_password: String,
}

async fn body<T: serde::de::DeserializeOwned>(request: Request) -> Result<T, &'static str> {
    let bytes = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        to_bytes(request.into_body(), 16 * 1024),
    )
    .await
    .map_err(|_| "request body timed out")?
    .map_err(|_| "body exceeds 16 KiB")?;
    serde_json::from_slice(&bytes).map_err(|_| "expected a JSON object")
}

#[derive(Serialize)]
struct LegacyLogin {
    ok: bool,
    token: String,
    default_password: bool,
    csrf: String,
}

pub async fn login(State(state): State<Shared>, request: Request) -> Response {
    log_in(state, request, true).await
}
pub async fn login_v1(State(state): State<Shared>, request: Request) -> Response {
    log_in(state, request, false).await
}

async fn log_in(state: Shared, request: Request, legacy: bool) -> Response {
    let address = peer(request.extensions()).unwrap_or(IpAddr::from([0, 0, 0, 0]));
    let login: Login = match body(request).await {
        Ok(body) => body,
        Err(reason) => return errors::invalid("body", reason),
    };
    if let Some(response) = source_limited(&state, address) {
        return response;
    }
    tokio::time::sleep(state.limiter.soft_delay(Instant::now())).await;
    // Bound concurrent memory-hard verification and serialize local writes.
    let _gate = state.auth_gate.lock().await;
    if let Some(response) = source_limited(&state, address) {
        return response;
    }
    let stamp = match state.password.verify_and_upgrade(login.password).await {
        Ok(Some(stamp)) => stamp,
        Ok(None) => {
            state.limiter.failed(address, Instant::now());
            return errors::unauthorized();
        }
        Err(_) => return errors::internal(),
    };
    state.limiter.succeeded(address);
    let (token, session) = state.sessions.create(stamp, Instant::now());
    let cookie = [(
        header::SET_COOKIE,
        sessions::cookie(&token, state.config.cookie_secure),
    )];
    if legacy {
        (
            cookie,
            Json(LegacyLogin {
                ok: true,
                token,
                default_password: state.password.is_default(),
                csrf: session.csrf,
            }),
        )
            .into_response()
    } else {
        (cookie, Json(json!({"csrf": session.csrf}))).into_response()
    }
}

fn source_limited(state: &Shared, address: IpAddr) -> Option<Response> {
    let retry = state.limiter.retry_after(address, Instant::now())?;
    let mut response = errors::error(
        StatusCode::TOO_MANY_REQUESTS,
        json!("rate_limited"),
        "too many attempts, retry shortly",
    );
    response.headers_mut().insert(
        header::RETRY_AFTER,
        retry
            .as_secs()
            .saturating_add(1)
            .to_string()
            .parse()
            .expect("numeric retry delay"),
    );
    Some(response)
}

pub async fn logout(State(state): State<Shared>, headers: HeaderMap) -> Response {
    log_out(state, headers, true)
}
pub async fn logout_v1(State(state): State<Shared>, headers: HeaderMap) -> Response {
    log_out(state, headers, false)
}

fn log_out(state: Shared, headers: HeaderMap, legacy: bool) -> Response {
    if let Some(auth) = state.authenticate(&headers) {
        state.sessions.remove(&auth.token);
    }
    let cookie = [(
        header::SET_COOKIE,
        sessions::clear_cookie(state.config.cookie_secure),
    )];
    if legacy {
        (cookie, Json(json!({"ok": true}))).into_response()
    } else {
        (cookie, StatusCode::NO_CONTENT).into_response()
    }
}

#[derive(Serialize)]
struct AuthStatus {
    auth_required: bool,
    logged_in: bool,
    default_password: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    csrf: Option<String>,
}

pub async fn status(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let auth = state.authenticate(&headers);
    Json(AuthStatus {
        auth_required: true,
        logged_in: auth.is_some(),
        default_password: state.password.is_default(),
        csrf: auth.map(|auth| auth.session.csrf),
    })
    .into_response()
}

pub async fn password(State(state): State<Shared>, request: Request) -> Response {
    change_password(state, request, true).await
}
pub async fn password_v1(State(state): State<Shared>, request: Request) -> Response {
    change_password(state, request, false).await
}

/// `GET /api/v1/session`: the native `/api/auth-status`. Anyone may ask; a signed-in
/// session also learns its CSRF token and whether the factory password is still
/// set (an anonymous visitor is told neither).
pub async fn session_v1(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let Some(auth) = state.authenticate(&headers) else {
        return Json(json!({"logged_in": false})).into_response();
    };
    Json(json!({
        "logged_in": true,
        "csrf": auth.session.csrf,
        "default_password": state.password.is_default(),
    }))
    .into_response()
}

async fn change_password(state: Shared, request: Request, legacy: bool) -> Response {
    let address = peer(request.extensions()).unwrap_or(IpAddr::from([0, 0, 0, 0]));
    // The session API names the fields after `SessionCommand::ChangePassword`.
    let (current, new, field) = if legacy {
        match body::<Password>(request).await {
            Ok(change) => (change.current, change.password, "password"),
            Err(reason) => return errors::invalid("body", reason),
        }
    } else {
        match body::<PasswordChange>(request).await {
            Ok(change) => (change.current_password, change.new_password, "new_password"),
            Err(reason) => return errors::invalid("body", reason),
        }
    };
    if let Some(response) = source_limited(&state, address) {
        return response;
    }
    tokio::time::sleep(state.limiter.soft_delay(Instant::now())).await;
    let _gate = state.auth_gate.lock().await;
    if let Some(response) = source_limited(&state, address) {
        return response;
    }
    match state.password.verify_and_upgrade(current).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            state.limiter.failed(address, Instant::now());
            return errors::denied("current password is wrong");
        }
        Err(_) => return errors::internal(),
    }
    if new.encode_utf16().count() < 6 {
        return errors::invalid(field, "new password must be at least 6 characters");
    }
    let stamp = match state.password.set(new).await {
        Ok(stamp) => stamp,
        Err(_) => return errors::internal(),
    };
    state.limiter.succeeded(address);
    state.sessions.clear();
    let (token, session) = state.sessions.create(stamp, Instant::now());
    let reply = if legacy {
        json!({"ok": true, "csrf": session.csrf})
    } else {
        json!({"csrf": session.csrf})
    };
    (
        [(
            header::SET_COOKIE,
            sessions::cookie(&token, state.config.cookie_secure),
        )],
        Json(reply),
    )
        .into_response()
}
