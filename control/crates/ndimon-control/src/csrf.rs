//! Cookie CSRF and the legacy Origin/Host check; bearer requests need no CSRF.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::sessions::Authenticated;
use crate::{Shared, errors, peer};

pub fn mutates(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

pub fn origin_allowed(method: &Method, headers: &HeaderMap) -> bool {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return true;
    }
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    // Normalize both authorities, including default ports and IPv6 spelling;
    // reject opaque/null origins and malformed authority.
    let Some(origin) = origin.to_str().ok().and_then(|v| url::Url::parse(v).ok()) else {
        return false;
    };
    if !matches!(origin.scheme(), "http" | "https") || origin.host_str().is_none() {
        return false;
    }
    let Ok(host) = url::Url::parse(&format!("{}://{host}", origin.scheme())) else {
        return false;
    };
    host.username().is_empty()
        && host.password().is_none()
        && host.path() == "/"
        && host.query().is_none()
        && host.fragment().is_none()
        && origin.host() == host.host()
        && origin.port() == host.port()
}

pub fn token_allowed(method: &Method, headers: &HeaderMap, auth: &Authenticated) -> bool {
    !mutates(method)
        || auth.bearer
        || headers
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| crate::auth::same(v, &auth.session.csrf))
}

pub fn protected(path: &str) -> bool {
    // Former Express routing was case-insensitive. No spelling of a protected
    // path may bypass authentication: /API, /V1, an encoded (`/%61pi`) or a
    // doubled (`//api`) form. Check the raw, percent-decoded and
    // slash-collapsed forms.
    let decoded = percent_encoding::percent_decode_str(path).decode_utf8_lossy();
    [path, decoded.as_ref()]
        .iter()
        .any(|form| protected_form(&collapse_slashes(form)))
}

fn collapse_slashes(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if !(c == '/' && out.ends_with('/')) {
            out.push(c);
        }
    }
    out
}

fn protected_form(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    [
        "/v1",
        "/api",
        "/ndidecode",
        "/ndiencoder",
        "/ndifinder",
        "/videooutput",
        "/devicesettings",
        "/system",
        "/aboutme",
        "/plugins",
    ]
    .iter()
    .any(|prefix| {
        path == *prefix
            || path
                .strip_prefix(*prefix)
                .is_some_and(|v| v.starts_with('/'))
    })
}

pub async fn guard(State(state): State<Shared>, mut request: Request, next: Next) -> Response {
    if protected(request.uri().path()) && !crate::hosts::allowed(request.headers()) {
        let configured = if let Some(document) = &state.document {
            document.snapshot().await.ok().is_some_and(|state| {
                crate::hosts::configured_allowed(
                    request.headers(),
                    &state.document.device.web_allowed_hosts,
                )
            })
        } else {
            false
        };
        if !configured {
            let host = crate::hosts::display_host(request.headers());
            return (
                axum::http::StatusCode::MISDIRECTED_REQUEST,
                axum::Json(serde_json::json!({
                    "ok": false,
                    "error": "misdirected_host",
                    "message": format!("This device is not reachable under {host}. Open it by its IP address and add the name under Settings → Security → Additional host names.")
                })),
            ).into_response();
        }
    }
    if !origin_allowed(request.method(), request.headers()) {
        return errors::denied("Origin must match Host");
    }
    let path = request.uri().path();
    let exempt = (request.method() == Method::POST
        && matches!(
            path,
            "/api/login"
                | "/api/v1/session"
                | "/api/net-confirm"
                | "/api/net-pending"
                | "/api/net-revert"
        ))
        || (request.method() == Method::GET
            && matches!(path, "/api/auth-status" | "/api/v1/session"))
        || (request.method() == Method::GET
            && path == "/api/health"
            && peer(request.extensions()).is_some_and(|ip| ip.is_loopback()));
    let auth = state.authenticate(request.headers());
    if !exempt && protected(path) {
        let Some(auth) = auth.as_ref() else {
            return errors::unauthorized();
        };
        if !token_allowed(request.method(), request.headers(), auth) {
            return errors::denied("missing or wrong X-CSRF-Token");
        }
    }
    if auth.is_some()
        || (request.method() == Method::POST
            && matches!(
                path,
                "/api/net-confirm" | "/api/net-pending" | "/api/net-revert"
            ))
    {
        if let Some(info) = request
            .extensions()
            .get::<axum::extract::ConnectInfo<crate::connections::SocketInfo>>()
            .map(|info| info.0)
        {
            // Peer identity is still available when querying the socket's
            // destination failed. Token-only Keep permits that missing local.
            request
                .extensions_mut()
                .insert(axum::extract::ConnectInfo(info.peer));
            if let Some(path) = info.network_session() {
                request.extensions_mut().insert(path);
            }
        }
    }
    if let Some(auth) = auth {
        request
            .extensions_mut()
            .insert(ndimon_compat::UiSession(auth.session.id.clone()));
        request.extensions_mut().insert(auth);
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_paths_cannot_be_spelled_around() {
        for path in [
            "/api/health",
            "/API/Health",
            "/v1/System/reboot",
            "/%61pi/events",
            "/%2Fapi/x",
            "//api/x",
            "/v1//System",
            "/%76%31/Backup/export",
            "/plugins/hello/ui.html",
            "/NDIFinder",
        ] {
            assert!(protected(path), "{path}");
        }
        for path in [
            "/",
            "/index.html",
            "/vendor/bootstrap/js/x.js",
            "/fonts/Inter.ttf",
            "/apiary",
            "/v10",
        ] {
            assert!(!protected(path), "{path}");
        }
    }

    #[test]
    fn origins_follow_node_url_host_rules() {
        for (host, origin, expected) in [
            ("device.test", "http://device.test", true),
            ("device.test", "http://device.test:80", true),
            ("device.test:80", "http://device.test", true),
            ("DEVICE.test:443", "https://device.test", true),
            ("192.0.2.1:80", "http://192.0.2.1:80", true),
            ("[2001:db8:0::1]:80", "http://[2001:db8::1]", true),
            ("device.test:8080", "http://device.test:8080", true),
            ("device.test", "http://device.test:8080", false),
            ("[::1]:8080", "http://[::1]:8080", true),
            ("device.test", "null", false),
            ("device.test", "https://elsewhere", false),
            ("device.test", "http://device.test.attacker", false),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("host", host.parse().unwrap());
            headers.insert("origin", origin.parse().unwrap());
            assert_eq!(
                origin_allowed(&Method::POST, &headers),
                expected,
                "{host} {origin}"
            );
            assert!(origin_allowed(&Method::GET, &headers));
        }
    }
}
