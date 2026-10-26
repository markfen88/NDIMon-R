//! Legacy preset storage and recall through the native outputs source command.
use crate::common::*;
use axum::{
    Extension, Router,
    extract::{FromRef, State},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use ndimon_state::StateHandle;
use ndimon_types::*;
use serde_json::{Value, json};

pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    StateHandle: FromRef<S>,
{
    Router::new()
        .route("/v1/Presets/list", get(list))
        .route("/v1/Presets/save", post(save))
        .route("/v1/Presets/delete", post(delete))
        .route("/v1/Presets/recall", post(recall))
}
pub(crate) fn endpoint(text: &str) -> bool {
    let (ip, port) = text
        .split_once(':')
        .map_or((text, None), |(a, b)| (a, Some(b)));
    if port.is_some_and(|s| {
        s.parse::<u16>()
            .ok()
            .is_none_or(|p| p == 0 || p.to_string() != s)
    }) {
        return false;
    }
    ip.parse::<std::net::Ipv4Addr>()
        .is_ok_and(|p| p.to_string() == ip)
}
fn string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(false) => String::new(),
        Value::Number(n) if n.as_i64() == Some(0) => String::new(),
        _ => v.to_string(),
    }
}
fn invalid(reason: String) -> ApiError {
    ApiError::invalid(vec![FieldError::new("presets", reason)])
}
fn failure(message: &str) -> Response {
    response(400, json!({"ok":false,"error":message}))
}
fn projection(state: &ndimon_state::persist::StateFile, ok: bool) -> Response {
    let spec = ndimon_render::PATHS
        .iter()
        .find(|p| p.path_id == "presets")
        .expect("registered presets path");
    match ndimon_render::presets::render_path(
        spec,
        &state.document,
        state.domain_revision(Domain::Presets),
        &state.legacy_files,
    ) {
        Ok(bytes) => {
            let bytes = if ok {
                [b"{\"ok\":true,".as_slice(), &bytes[1..]].concat()
            } else {
                bytes
            };
            (
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response()
        }
        Err(e) => internal_error(invalid(e)),
    }
}
async fn list(State(h): State<StateHandle>) -> Response {
    match h.snapshot().await {
        Ok(s) => projection(&s, false),
        Err(e) => internal_error(e),
    }
}
async fn save(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    mutate(h, session, value, true).await
}
async fn delete(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    mutate(h, session, value, false).await
}
async fn mutate(
    h: StateHandle,
    session: Option<Extension<UiSession>>,
    value: Body,
    save: bool,
) -> Response {
    let v = match body(value) {
        Ok(v) => v,
        Err(e) => return *e,
    };
    let name = string(&v["name"]).trim().to_string();
    let command = if save {
        let name = String::from_utf16_lossy(&name.encode_utf16().take(64).collect::<Vec<_>>());
        let source = string(&v["source"]).trim().to_string();
        let ip = v
            .get("ip")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(json!(""));
        if ip != "" && !ip.as_str().is_some_and(endpoint) {
            return failure("invalid source IPv4 endpoint");
        }
        if name.is_empty() {
            return failure("name required");
        }
        if source.is_empty() {
            return failure("source required");
        }
        let snapshot = match h.snapshot().await {
            Ok(v) => v,
            Err(e) => return internal_error(e),
        };
        if snapshot.document.presets.len() >= 32 && !snapshot.document.presets.contains_key(&name) {
            return failure("max 32 presets");
        }
        Command::SavePreset {
            name,
            preset: Preset::new(NdiSourceRef::new(source, ip.as_str().unwrap_or(""))),
        }
    } else {
        Command::DeletePreset { name }
    };
    let revision = v.get("revision").and_then(Value::as_u64);
    match submit_domain(
        &h,
        command,
        revision,
        ui(session),
        &v,
        Domain::Presets,
        invalid,
    )
    .await
    {
        Ok(_) => match h.snapshot().await {
            Ok(s) => projection(&s, true),
            Err(e) => internal_error(e),
        },
        Err(e) => internal_error(e),
    }
}
async fn recall(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    let v = match body(value) {
        Ok(v) => v,
        Err(e) => return *e,
    };
    let output = match crate::outputs::channel(&v["output"]) {
        Ok(id) => id,
        Err(_) => return failure("output must be 1–8"),
    };
    let name = string(&v["name"]).trim().to_string();
    let snapshot = match h.snapshot().await {
        Ok(s) => s,
        Err(e) => return internal_error(e),
    };
    let Some(preset) = snapshot.document.presets.get(&name) else {
        return response(404, json!({"ok":false,"error":"preset not found"}));
    };
    let mut source = preset.source.clone();
    if source.endpoint.is_empty() {
        let catalog = h
            .query(Query::Catalog {
                name: Catalog::NdiSources,
                output: None,
            })
            .await
            .unwrap_or(Value::Null);
        if let Some(endpoint) = catalog["list"][&source.name]
            .as_str()
            .filter(|s| ndimon_state::domains::outputs::valid_endpoint(s))
        {
            source.endpoint = endpoint.into();
        }
    }
    match submit_domain(
        &h,
        Command::SelectSource { output, source },
        Some(
            v["revision"]
                .as_u64()
                .unwrap_or(snapshot.domain_revision(Domain::Outputs)),
        ),
        ui(session),
        &v,
        Domain::Outputs,
        crate::outputs::invalid,
    )
    .await
    {
        Ok(_) => (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            format!(
                "{{\"ok\":true,\"recalled\":{},\"source\":{}}}",
                json!(name),
                json!(preset.source.name)
            ),
        )
            .into_response(),
        Err(e) => crate::outputs::error(e),
    }
}
#[cfg(test)]
mod tests;
