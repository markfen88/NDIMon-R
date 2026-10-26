//! Native NDI adapters preserve the legacy JSON and text envelopes.
use crate::{
    common::*,
    outputs::{error, invalid},
};
use axum::{
    Extension, Router,
    body::Bytes,
    extract::{FromRef, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::get,
};
use ndimon_state::{StateHandle, persist::StateFile};
use ndimon_types::*;
use serde_json::{Value, json};

pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    StateHandle: FromRef<S>,
{
    let finder = Router::new()
        .route("/List", get(sources))
        .route("/NdiOffSnSrc", get(extra).post(set_extra))
        .route("/NdiGrpName", get(groups).post(set_groups))
        .route("/NDIDisServer", get(discovery).post(set_discovery))
        .route("/refresh", get(use_post).post(refresh))
        .route("/reset", get(use_post).post(reset));
    let transport = Router::new().route("/decodeTransport", get(receive).post(set_receive));
    Router::new()
        .nest("/v1/NDIFinder", finder.clone())
        .nest("/NDIFinder", finder)
        .nest("/v1/NDIDecode", transport.clone())
        .nest("/NDIDecode", transport)
}
fn text(value: impl Into<String>) -> Response {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        value.into(),
    )
        .into_response()
}
fn projection(snapshot: &StateFile, id: &str) -> Result<Value, ApiError> {
    let spec = ndimon_render::PATHS
        .iter()
        .find(|p| p.path_id == id)
        .ok_or_else(|| invalid("NDI path missing".into()))?;
    let bytes = ndimon_render::ndi::render_path(
        spec,
        &snapshot.document,
        snapshot.revision,
        &snapshot.legacy_files,
    )
    .map_err(invalid)?;
    serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))
}
async fn get_projection(h: &StateHandle, id: &str) -> Response {
    match h.snapshot().await.and_then(|s| projection(&s, id)) {
        Ok(v) => success(v),
        Err(e) => error(e),
    }
}
async fn receive(State(h): State<StateHandle>) -> Response {
    get_projection(&h, "receive").await
}
async fn discovery(State(h): State<StateHandle>) -> Response {
    get_projection(&h, "find").await
}
async fn groups(State(h): State<StateHandle>) -> Response {
    match h.snapshot().await.and_then(|s| projection(&s, "groups")) {
        Ok(v) => success(json!({"ndi_groups":v["ndi_groups"]})),
        Err(e) => error(e),
    }
}
async fn extra(State(h): State<StateHandle>) -> Response {
    match h.snapshot().await.and_then(|s| projection(&s, "extra-ips")) {
        Ok(v) => text(v.to_string()),
        Err(e) => error(e),
    }
}
async fn sources(State(h): State<StateHandle>) -> Response {
    match h
        .query(Query::Catalog {
            name: Catalog::NdiSources,
            output: None,
        })
        .await
    {
        Ok(v) => success(v),
        Err(e) => error(e),
    }
}
async fn use_post() -> Response {
    response(405, json!({"ok":false,"error":"use POST"}))
}
async fn commit(
    h: &StateHandle,
    snapshot: &StateFile,
    ndi: Ndi,
    confirmation: &Value,
    origin: Origin,
) -> Result<(), ApiError> {
    submit_domain(
        h,
        Command::SetNdi(ndi),
        Some(snapshot.domain_revision(Domain::Ndi)),
        origin,
        confirmation,
        Domain::Ndi,
        invalid,
    )
    .await?;
    Ok(())
}
async fn set_receive(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    let value = match body(value) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let snapshot = match h.snapshot().await {
        Ok(s) => s,
        Err(e) => return error(e),
    };
    let mode = match value["Rxpm"].as_str() {
        Some("TCP") => Some(Transport::Tcp),
        Some("M-TCP") => Some(Transport::MultiTcp),
        Some("UDP") => Some(Transport::Udp),
        Some("Multicast") => Some(Transport::Multicast),
        Some("RUDP") => Some(Transport::Rudp),
        _ => None,
    };
    if let Some(mode) = mode {
        let mut ndi = snapshot.document.ndi.clone();
        ndi.receive = mode;
        if let Err(e) = commit(&h, &snapshot, ndi, &value, ui(session)).await {
            return error(e);
        }
    }
    get_projection(&h, "receive").await
}
async fn set_discovery(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    let value = match body(value) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let snapshot = match h.snapshot().await {
        Ok(s) => s,
        Err(e) => return error(e),
    };
    let mut ndi = snapshot.document.ndi.clone();
    if let Some(ip) = value.get("NDIDisServIP").filter(|v| !v.is_null()) {
        let ip = ip
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| ip.to_string());
        ndi.discovery_servers = split(&ip);
    }
    match commit(&h, &snapshot, ndi, &value, ui(session)).await {
        Ok(()) => get_projection(&h, "find").await,
        Err(e) => error(e),
    }
}
fn split(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}
fn raw(headers: &HeaderMap, bytes: &Bytes) -> Result<Value, ApiError> {
    if headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.starts_with("application/json"))
    {
        serde_json::from_slice(bytes).map_err(|e| invalid(e.to_string()))
    } else {
        String::from_utf8(bytes.to_vec())
            .map(Value::String)
            .map_err(|e| invalid(e.to_string()))
    }
}
async fn set_groups(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    let value = match raw(&headers, &bytes) {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    let groups = if let Some(s) = value.as_str() {
        s.replace(['\'', '"'], "").trim().to_string()
    } else if let Some(v) = value.get("ndi_groups").filter(|v| !v.is_null()) {
        v.as_str()
            .map(str::to_string)
            .unwrap_or_else(|| v.to_string())
            .trim()
            .to_string()
    } else {
        "public".into()
    };
    let snapshot = match h.snapshot().await {
        Ok(s) => s,
        Err(e) => return error(e),
    };
    let mut ndi = snapshot.document.ndi.clone();
    ndi.groups = groups
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .collect();
    if ndi.groups.is_empty() {
        ndi.groups.push("public".into());
    }
    match commit(&h, &snapshot, ndi, &value, ui(session)).await {
        Ok(()) => self::groups(State(h)).await,
        Err(e) => error(e),
    }
}
async fn set_extra(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    let value = match raw(&headers, &bytes) {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    let Some(raw) = value.as_str() else {
        return text("Bad request - Invalid IP");
    };
    let ips = raw.replace(['\'', '"', ' '], "");
    // Match the original octet parser, including decimal leading zeros, then
    // store canonical addresses accepted by the typed follower contract.
    let mut addresses = Vec::new();
    for ip in ips.split(',').filter(|s| !s.is_empty()) {
        let parts: Option<Vec<u8>> = ip
            .split('.')
            .map(|s| {
                if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                    None
                } else {
                    s.parse().ok()
                }
            })
            .collect();
        match parts {
            Some(p) if p.len() == 4 => {
                addresses.push(p.iter().map(u8::to_string).collect::<Vec<_>>().join("."))
            }
            _ => return text("Bad request - Invalid IP"),
        }
    }
    let snapshot = match h.snapshot().await {
        Ok(s) => s,
        Err(e) => return error(e),
    };
    let mut ndi = snapshot.document.ndi.clone();
    ndi.off_subnet_sources = addresses;
    match commit(&h, &snapshot, ndi, &value, ui(session)).await {
        Ok(()) => text("success"),
        Err(e) => error(e),
    }
}
async fn operation(
    h: StateHandle,
    session: Option<Extension<UiSession>>,
    bytes: Bytes,
    command: Command,
) -> Response {
    let value = if bytes.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => v,
            Err(e) => return error(invalid(e.to_string())),
        }
    };
    let snapshot = match h.snapshot().await {
        Ok(s) => s,
        Err(e) => return error(e),
    };
    match submit_domain(
        &h,
        command,
        Some(snapshot.domain_revision(Domain::Ndi)),
        ui(session),
        &value,
        Domain::Ndi,
        invalid,
    )
    .await
    {
        Ok(_) => text("success"),
        Err(e) => error(e),
    }
}
async fn refresh(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    bytes: Bytes,
) -> Response {
    operation(h, session, bytes, Command::RefreshNdi).await
}
async fn reset(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    bytes: Bytes,
) -> Response {
    operation(h, session, bytes, Command::ResetNdiDiscovery).await
}

#[cfg(test)]
mod tests;
