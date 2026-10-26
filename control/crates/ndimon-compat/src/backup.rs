//! Legacy schema-1 backup transport over one native cross-domain transaction.
use crate::{common::*, validation};
use axum::{
    Extension, Router,
    extract::{DefaultBodyLimit, FromRef, Query, State},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use ndimon_state::backup::coordinator::capture;
#[cfg(test)]
use ndimon_state::backup::legacy::checksum;
use ndimon_state::backup::{review, storage};
use ndimon_state::{StateHandle, persist::StateFile};
use ndimon_types::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
const SECTIONS: &[&str] = &[
    "plugins",
    "outputs",
    "presets",
    "discovery",
    "transport",
    "display",
    "device",
    "identity",
    "auth",
    "audio",
];
pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    StateHandle: FromRef<S>,
{
    Router::new()
        .route("/v1/Backup/export", get(export))
        .route(
            "/v1/Backup/inspect",
            post(inspect).layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route("/v1/Backup/apply", post(apply))
        .route("/v1/Backup/undo", post(undo))
        .route("/v1/Backup/snapshots", get(snapshots))
}
fn legacy_error(problem: ApiError) -> Response {
    match problem {
        ApiError::Invalid { field, reason, .. } if field == "backup" => error(400, reason),
        ApiError::ConfirmationRequired { impact } => response(
            409,
            json!({"ok":false,"error":"confirmation_required","impact":impact}),
        ),
        ApiError::RevisionConflict { current_revision } => response(
            409,
            json!({"ok":false,"error":"revision_conflict","revision":current_revision}),
        ),
        error => response(
            error.http_status(),
            json!({"ok":false,"error":error.to_string()}),
        ),
    }
}
fn error(status: u16, message: impl Into<String>) -> Response {
    response(status, json!({"ok":false,"error":message.into()}))
}
fn peer(path: &Option<Extension<NetworkSessionPath>>) -> String {
    path.as_ref()
        .map_or_else(String::new, |p| p.0.peer.to_string())
}
#[cfg(test)]
fn check(doc: &Value) -> Result<(), String> {
    ndimon_state::backup::review::validate(doc.clone()).map(|_| ())
}
async fn export(
    State(h): State<StateHandle>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Response {
    let state = match h.snapshot().await {
        Ok(s) => s,
        Err(e) => return legacy_error(e),
    };
    let network = query.get("network").is_none_or(|v| v != "0");
    match capture(
        &h,
        &state,
        network,
        query.get("password").is_some_and(|v| v == "1"),
        false,
    )
    .await
    {
        Ok(mut doc) => {
            if let Some(warning) =
                ndimon_state::backup::coordinator::network_export_warning(&state, network)
            {
                // The checksum covers the sections only.
                let mut warnings = vec![json!(warning)];
                warnings.extend(doc["warnings"].as_array().cloned().unwrap_or_default());
                doc["warnings"] = json!(warnings);
            }
            let alias = doc["source_device"]["ndi_alias"]
                .as_str()
                .unwrap_or("ndimon");
            let name = safe_alias(alias);
            match ndimon_state::backup::legacy::stringify(&doc, true) {
                Ok(bytes) => (
                    [
                        ("content-type", "application/json".to_string()),
                        (
                            "content-disposition",
                            format!(
                                "attachment; filename=\"ndimon-{name}-{}.json\"",
                                storage::stamp()
                            ),
                        ),
                    ],
                    bytes,
                )
                    .into_response(),
                Err(e) => error(500, e.to_string()),
            }
        }
        Err(e) => legacy_error(e),
    }
}
fn candidate(
    state: &StateFile,
    sections: &Value,
    selected: &[String],
    h: &StateHandle,
) -> Result<BackupDocument, ApiError> {
    if selected.iter().any(|k| k == "audio") && sections.get("audio").is_some() {
        validation::legacy(&sections["audio"]["settings"]).map_err(ApiError::invalid)?;
    }
    ndimon_state::backup::legacy::candidate(state, sections, selected, &h.assets, &mut Vec::new())
}
async fn inspect(
    State(h): State<StateHandle>,
    path: Option<Extension<NetworkSessionPath>>,
    value: Body,
) -> Response {
    let doc = match body(value) {
        Ok(v) => v,
        Err(e) => return *e,
    };
    let reviewed = match review::review(&h, doc.clone(), &peer(&path)).await {
        Ok(v) => v,
        Err(e) => return error(400, e),
    };
    let doc = &reviewed["document"];
    let state = match h.snapshot().await {
        Ok(v) => v,
        Err(e) => return legacy_error(e),
    };
    let selected = doc["sections"]
        .as_object()
        .map(|o| {
            o.keys()
                .filter(|k| k.as_str() != "network")
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Err(e) = candidate(&state, &doc["sections"], &selected, &h) {
        return legacy_error(e);
    }
    let dir = h.backup_root().join("import");
    if let Err(e) = storage::directory(&dir) {
        return error(500, e);
    }
    storage::sweep(&dir);
    let id = match storage::random(16) {
        Ok(v) => v,
        Err(e) => return error(500, e),
    };
    if let Err(e) = storage::write(&dir.join(format!("{id}.json")), doc) {
        return error(500, e);
    }
    success(
        json!({"ok":true,"import_id":id,"created_at":doc["created_at"],"source_device":doc.get("source_device").cloned().unwrap_or(json!({})),"app":doc.get("app").cloned().unwrap_or(json!({})),"sections":reviewed["sections"],"network":reviewed["network"],"extra_outputs":reviewed["extra_outputs"]}),
    )
}
async fn apply(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    path: Option<Extension<NetworkSessionPath>>,
    value: Body,
) -> Response {
    let v = match body(value) {
        Ok(v) => v,
        Err(e) => return *e,
    };
    let id = v["import_id"].as_str().unwrap_or("");
    if id.len() != 32
        || !id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return error(400, "unknown import");
    }
    let _lock = h.lock_backup().await;
    let file = h.backup_root().join("import").join(format!("{id}.json"));
    if storage::expired(&file) {
        return error(400, "that restore expired; choose the file again");
    }
    let doc = match storage::read(&file) {
        Ok(v) => v,
        Err(e) => return error(400, e),
    };
    let selected = v["sections"]
        .as_array()
        .map(|v| {
            v.iter()
                .map(|v| v.as_str().unwrap_or("").to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if selected.iter().any(|k| !SECTIONS.contains(&k.as_str())) {
        return error(400, "unknown section");
    }
    if selected.is_empty() && v["apply_network"] != true {
        return error(400, "choose at least one section");
    }
    match restore(
        &h,
        &doc,
        &selected,
        v["apply_network"] == true,
        &v,
        ui(session),
        path.map(|p| p.0),
    )
    .await
    {
        Ok(out) => {
            let _ = std::fs::remove_file(file);
            success(out)
        }
        Err(e) => legacy_error(e),
    }
}
async fn restore(
    h: &StateHandle,
    doc: &Value,
    selected: &[String],
    network: bool,
    confirmation: &Value,
    origin: Origin,
    connection: Option<NetworkSessionPath>,
) -> Result<Value, ApiError> {
    ndimon_state::backup::coordinator::restore(
        h,
        doc,
        selected,
        network,
        confirmation,
        origin,
        connection,
    )
    .await
    .map(|v| v.legacy)
}
async fn snapshots(State(h): State<StateHandle>) -> Response {
    success(json!({"snapshots":storage::snapshots(&h.backup_root().join("backups"))}))
}
async fn undo(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    path: Option<Extension<NetworkSessionPath>>,
    value: Body,
) -> Response {
    let v = match body(value) {
        Ok(v) => v,
        Err(e) => return *e,
    };
    let _lock = h.lock_backup().await;
    let dir = h.backup_root().join("backups");
    let mut names = storage::snapshots(&dir)
        .into_iter()
        .filter_map(|v| v["name"].as_str().map(str::to_string))
        .collect::<Vec<_>>();
    names.sort();
    names.reverse();
    let name = v["name"]
        .as_str()
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| names.first().cloned());
    let Some(name) = name.filter(|n| names.contains(n) && !n.contains("..") && !n.contains('/'))
    else {
        return error(404, "no snapshot");
    };
    let doc = match storage::read(&dir.join(&name)) {
        Ok(v) => v,
        Err(_) => return error(500, "snapshot is unreadable"),
    };
    let reviewed = match review::review(&h, doc.clone(), &peer(&path)).await {
        Ok(v) => v,
        Err(e) => return error(400, e),
    };
    let selected = doc["sections"]
        .as_object()
        .map(|o| {
            o.keys()
                .filter(|k| k.as_str() != "network")
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    match restore(
        &h,
        &reviewed["document"],
        &selected,
        reviewed["network"]["needs_prompt"] == true,
        &v,
        ui(session),
        path.map(|p| p.0),
    )
    .await
    {
        Ok(out) => {
            let mut reply = json!({"ok":true,"restored":name});
            for (key, value) in out.as_object().expect("restore reply") {
                if key != "ok" {
                    reply[key] = value.clone();
                }
            }
            success(reply)
        }
        Err(e) => legacy_error(e),
    }
}
#[cfg(test)]
mod tests;

fn safe_alias(alias: &str) -> String {
    let mut value = String::new();
    let mut replacing = false;
    for c in alias.chars() {
        if c.is_ascii_alphanumeric() || c == '-' {
            value.push(c);
            replacing = false;
        } else if !replacing {
            value.push('-');
            replacing = true;
        }
    }
    let value = value.trim_matches('-').chars().take(32).collect::<String>();
    if value.is_empty() {
        "ndimon".into()
    } else {
        value
    }
}
