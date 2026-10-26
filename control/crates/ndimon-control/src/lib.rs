//! HTTP front door: authentication, CSRF, host admission and the native routers.

pub mod auth;
pub mod config;
pub mod connections;
pub mod csrf;
pub mod errors;
pub mod headers;
mod hosts;
pub mod legacy_auth;
pub mod legacy_status;
pub mod native;
pub mod sessions;
pub mod sse;
pub mod static_files;
pub mod supervisor;

use auth::PasswordFile;
use axum::Router;
use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRef};
use axum::http::{Extensions, HeaderMap, StatusCode, header};
use axum::middleware;
use axum::routing::{get, post};
use config::Config;
use ndimon_state::StateHandle;
use serde_json::json;
use sessions::{Authenticated, LoginLimiter, Sessions};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct Shared(pub Arc<AppState>);
impl std::ops::Deref for Shared {
    type Target = AppState;
    fn deref(&self) -> &AppState {
        &self.0
    }
}
impl From<Arc<AppState>> for Shared {
    fn from(state: Arc<AppState>) -> Self {
        Self(state)
    }
}
impl FromRef<AppState> for StateHandle {
    fn from_ref(state: &AppState) -> Self {
        state
            .document
            .clone()
            .expect("native routes require the state actor")
    }
}
impl FromRef<Shared> for StateHandle {
    fn from_ref(state: &Shared) -> Self {
        Self::from_ref(&**state)
    }
}

#[derive(Debug)]
pub struct AppState {
    pub config: Config,
    pub password: PasswordFile,
    pub sessions: Sessions,
    pub limiter: LoginLimiter,
    pub hub: sse::EventHub,
    pub auth_gate: tokio::sync::Mutex<()>,
    pub document: Option<StateHandle>,
    pub legacy_status: legacy_status::Runtime,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            password: PasswordFile(config.auth_file.clone()),
            config,
            sessions: Sessions::default(),
            limiter: LoginLimiter::default(),
            hub: sse::EventHub::new(256),
            auth_gate: tokio::sync::Mutex::new(()),
            document: None,
            legacy_status: legacy_status::Runtime::default(),
        }
    }

    /// Attach the actor before constructing native routers. The front-door-only
    /// constructor remains useful for isolated authentication tests.
    pub fn with_document(mut self, document: StateHandle) -> Self {
        self.document = Some(document);
        self
    }

    pub fn authenticate(&self, headers: &HeaderMap) -> Option<Authenticated> {
        let credential = self.password.stamp().ok()?;
        // Valid explicit Bearer credentials take precedence over cookies.
        if let Some(token) = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
        {
            if let Some(session) = self.sessions.get(token, credential, Instant::now()) {
                return Some(Authenticated {
                    token: token.into(),
                    session,
                    bearer: true,
                });
            }
        }
        let token = sessions::cookie_token(headers)?;
        let session = self.sessions.get(token, credential, Instant::now())?;
        Some(Authenticated {
            token: token.into(),
            session,
            bearer: false,
        })
    }
}

pub fn router(state: impl Into<Shared>) -> Router {
    let state = state.into();
    let native = if state.document.is_some() {
        native::router()
            .merge(ndimon_compat::legacy_router::<Shared>())
            .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
    } else {
        Router::new()
    };
    Router::new()
        .merge(native)
        .merge(legacy_status::router())
        .route(
            "/api/v1/session",
            get(legacy_auth::session_v1)
                .post(legacy_auth::login_v1)
                .delete(legacy_auth::logout_v1),
        )
        .route("/api/v1/session/password", post(legacy_auth::password_v1))
        .route("/api/login", post(legacy_auth::login))
        .route("/api/logout", post(legacy_auth::logout))
        .route("/api/auth-status", get(legacy_auth::status))
        .route("/api/password", post(legacy_auth::password))
        .route("/api/v1/events", get(sse::events))
        .fallback(static_files::dispatch)
        .method_not_allowed_fallback(|| async {
            errors::error(
                StatusCode::METHOD_NOT_ALLOWED,
                json!({"unsupported": {"what": "method"}}),
                "method not allowed",
            )
        })
        .layer(middleware::from_fn_with_state(state.clone(), csrf::guard))
        .layer(middleware::from_fn(headers::security_headers))
        .with_state(state)
}

/// The accepted TCP peer; forwarding headers never confer loopback trust.
pub fn peer(extensions: &Extensions) -> Option<IpAddr> {
    let ip = extensions
        .get::<ConnectInfo<connections::SocketInfo>>()
        .map(|info| info.0.peer.ip())
        .or_else(|| {
            extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|info| info.0.ip())
        })?;
    Some(match ip {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or(IpAddr::V6(ip), IpAddr::V4),
        other => other,
    })
}
