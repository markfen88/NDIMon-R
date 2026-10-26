//! Plugin management, process-plugin commands and static UI are native.
use crate::common::*;
use axum::{
    Extension, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, FromRef, Path, Query as HttpQuery, State},
    http::{HeaderMap, StatusCode, header},
    response::Response,
    routing::{get, post},
};
use ndimon_state::{StateHandle, domains::plugins::invalid};
use ndimon_types::*;
use serde_json::{Value, json};
use tokio::sync::Mutex;

static MUTATION: Mutex<()> = Mutex::const_new(());
fn routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    StateHandle: FromRef<S>,
{
    Router::new()
        .route("/", get(list))
        .route("/feed", get(feed))
        .route("/feed/refresh", post(refresh))
        .route("/state/reset", post(reset))
        .route("/install", post(install))
        .route("/upload", post(upload).layer(DefaultBodyLimit::disable()))
        .route("/jobs/{job}/cancel", post(cancel))
        .route("/{name}/commands/{command}", post(process_command))
        .route("/{name}/enable", post(enable))
        .route("/{name}/disable", post(disable))
        .route("/{name}/update", post(update))
        .route("/{name}/uninstall", post(uninstall))
}
pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    StateHandle: FromRef<S>,
{
    Router::new()
        .nest("/v1/plugins", routes())
        .route("/plugins/{name}/{*path}", get(asset))
}
fn error(e: ApiError) -> Response {
    let status = e.http_status();
    match e {
        ApiError::ConfirmationRequired { impact } => response(
            status,
            json!({"ok":false,"error":"confirmation_required","impact":impact}),
        ),
        ApiError::RevisionConflict { current_revision } => response(
            status,
            json!({"ok":false,"error":"revision_conflict","revision":current_revision}),
        ),
        ApiError::Invalid { errors, .. } => response(
            status,
            json!({"ok":false,"error":errors.first().map(|e|e.reason.as_str()).unwrap_or("invalid plugin request")}),
        ),
        ApiError::StillApplying => response(409, json!({"ok":false,"error":"busy"})),
        other => {
            let message = match &other {
                ApiError::Internal { message } => message.clone(),
                ApiError::ApplyFailed { reason, .. } => reason.clone(),
                _ => other.to_string(),
            };
            if message.contains("host_too_old") {
                return response(409, json!({"ok":false,"error":"host_too_old"}));
            }
            if message.contains("plugins_state_corrupt") {
                return response(409, json!({"ok":false,"error":"plugins_state_corrupt"}));
            }
            if message == "busy" {
                return response(409, json!({"ok":false,"error":"busy"}));
            }
            let status = if message.contains("plugins_state_corrupt")
                || message.contains("busy")
                || message.contains("host_too_old")
            {
                409
            } else {
                status
            };
            response(status, json!({"ok":false,"error":message}))
        }
    }
}
async fn query(h: StateHandle, q: Query) -> Response {
    match h.query(q).await {
        Ok(v) => success(v),
        Err(e) => error(e),
    }
}
async fn list(State(h): State<StateHandle>) -> Response {
    query(h, Query::Plugins).await
}
async fn feed(State(h): State<StateHandle>) -> Response {
    query(h, Query::PluginFeed).await
}
fn parse(bytes: Bytes) -> Result<Value, Box<Response>> {
    if bytes.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| Box::new(response(400, json!({"ok":false,"error":"invalid JSON"}))))
}
async fn submit(h: &StateHandle, command: Command, value: &Value, origin: Origin) -> Response {
    let snapshot = match h.snapshot().await {
        Ok(s) => s,
        Err(e) => return error(e),
    };
    let revision = value["revision"]
        .as_u64()
        .unwrap_or(snapshot.domain_revision(Domain::Plugins));
    let plugin = match &command {
        Command::InstallPlugin { name, .. }
        | Command::UpdatePlugin { name, .. }
        | Command::SetPluginEnabled { name, .. }
        | Command::RemovePlugin { name, .. } => Some(name.clone()),
        _ => None,
    };
    let submitted = submit_domain(
        h,
        command,
        Some(revision),
        origin,
        value,
        Domain::Plugins,
        invalid,
    )
    .await;
    let result = match submitted {
        Ok(reply) => {
            if let Some(job) = reply.apply["job"].as_str() {
                ndimon_state::domains::plugins::wait_result(h, job).await
            } else {
                Ok(reply.apply)
            }
        }
        Err(error) => Err(error),
    };
    match result {
        Ok(reply) => success(reply),
        Err(e) => {
            if let (Some(name), ApiError::ApplyFailed { step, reason, .. }) = (plugin, &e) {
                if step == "apply"
                    && !["busy", "plugins_state_corrupt", "host_too_old"]
                        .iter()
                        .any(|marker| reason.contains(marker))
                {
                    let entry = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        h.query(Query::PluginStatus { name }),
                    )
                    .await
                    .ok()
                    .and_then(Result::ok);
                    // A refused install leaves nothing on the device: say so, not "broken".
                    let state = match &entry {
                        Some(Value::Null) => "absent",
                        Some(entry) => entry["state"].as_str().unwrap_or("broken"),
                        None => "broken",
                    };
                    // A refusal that left nothing behind is about the request (the
                    // archive, the host), not a fault of the device: 400 (NI-17).
                    let status = if state == "absent" {
                        400
                    } else {
                        e.http_status()
                    };
                    return lifecycle_error(status, state, reason);
                }
            }
            error(e)
        }
    }
}
fn lifecycle_error(status: u16, state: &str, reason: &str) -> Response {
    response(
        status,
        json!({"ok":false,"state":state,"error":reason,"log":reason}),
    )
}
async fn simple(
    h: StateHandle,
    session: Option<Extension<UiSession>>,
    bytes: Bytes,
    command: Command,
) -> Response {
    let _guard = if matches!(command, Command::CancelPluginJob { .. }) {
        None
    } else {
        match MUTATION.try_lock() {
            Ok(guard) => Some(guard),
            Err(_) => return response(409, json!({"ok":false,"error":"busy"})),
        }
    };
    let value = match parse(bytes) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    submit(&h, command, &value, ui(session)).await
}
async fn reset(
    State(h): State<StateHandle>,
    s: Option<Extension<UiSession>>,
    b: Bytes,
) -> Response {
    simple(h, s, b, Command::ResetPluginState).await
}
async fn refresh(State(h): State<StateHandle>) -> Response {
    query(h, Query::RefreshPluginFeed).await
}
async fn cancel(
    State(h): State<StateHandle>,
    Path(job): Path<String>,
    s: Option<Extension<UiSession>>,
    b: Bytes,
) -> Response {
    simple(h, s, b, Command::CancelPluginJob { job }).await
}
async fn install(
    State(h): State<StateHandle>,
    s: Option<Extension<UiSession>>,
    b: Bytes,
) -> Response {
    let value = match parse(b) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let name = value["name"].as_str().unwrap_or("").to_string();
    mutation(h, s, value, name, "install".into()).await
}
/// A lifecycle verb of `/v1/plugins/{name}/…` for the plugin `name`.
pub(crate) async fn lifecycle(
    h: StateHandle,
    session: Option<Extension<UiSession>>,
    bytes: Bytes,
    name: String,
    action: &'static str,
) -> Response {
    let value = match parse(bytes) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    mutation(h, session, value, name, action.into()).await
}
macro_rules! lifecycle_route {
    ($handler:ident, $action:literal) => {
        async fn $handler(
            State(h): State<StateHandle>,
            Path(name): Path<String>,
            session: Option<Extension<UiSession>>,
            bytes: Bytes,
        ) -> Response {
            lifecycle(h, session, bytes, name, $action).await
        }
    };
}
lifecycle_route!(enable, "enable");
lifecycle_route!(disable, "disable");
lifecycle_route!(update, "update");
lifecycle_route!(uninstall, "uninstall");
async fn mutation(
    h: StateHandle,
    session: Option<Extension<UiSession>>,
    value: Value,
    name: String,
    action: String,
) -> Response {
    let Ok(_guard) = MUTATION.try_lock() else {
        return response(409, json!({"ok":false,"error":"busy"}));
    };
    match h.snapshot().await {
        Ok(state) if state.plugin_job.is_some() => {
            return response(409, json!({"ok":false,"error":"busy"}));
        }
        Ok(_) => (),
        Err(e) => return error(e),
    }
    if !crate::validation::identifier(&name) || name.len() < 2 {
        return response(400, json!({"ok":false,"error":"invalid plugin name"}));
    }
    if !["install", "update", "enable", "disable", "uninstall"].contains(&action.as_str()) {
        return response(404, json!({"ok":false,"error":"not found"}));
    }
    let command = if ["install", "update"].contains(&action.as_str()) {
        if value.get("path").is_some() {
            return response(
                400,
                json!({"ok":false,"error":"caller paths are not accepted"}),
            );
        }
        let source = if action == "update" {
            "feed"
        } else {
            value["source"].as_str().unwrap_or("")
        };
        if !["feed", "upload"].contains(&source) {
            return response(400, json!({"ok":false,"error":"invalid source"}));
        }
        let mut prepared = match h
            .query(Query::PreparePluginInstall {
                name: name.clone(),
                source: source.into(),
                version: value["version"].as_str().map(str::to_string),
            })
            .await
        {
            Ok(v) => v,
            Err(e) => return error(e),
        };
        if prepared.get("error").is_some() {
            // A refusal that left nothing behind, said as an install says it.
            if prepared["refused"] == true {
                let reason = prepared["error"].as_str().unwrap_or("refused");
                return lifecycle_error(400, "absent", reason);
            }
            return response(
                if action == "update" && prepared["error"] == "plugin not in feed" {
                    409
                } else {
                    prepared["status"].as_u64().unwrap_or(400) as u16
                },
                json!({"ok":false,"error":prepared["error"]}),
            );
        }
        let snapshot = match h.snapshot().await {
            Ok(s) => s,
            Err(e) => return error(e),
        };
        let version = prepared["version"].as_str().unwrap_or("").to_string();
        // Debian comparison is shared with the transitional feed host.
        if let Some(installed) = snapshot.document.plugins.get(&name) {
            let cmp =
                ndimon_state::domains::plugins::compare_versions(&version, &installed.version);
            if cmp.is_lt() && value["force"] != true {
                return response(409, json!({"ok":false,"error":"downgrade requires force"}));
            }
            if action == "update" && !cmp.is_gt() {
                return response(409, json!({"ok":false,"error":"no newer version"}));
            }
        } else if action == "update" {
            return response(409, json!({"ok":false,"error":"no newer version"}));
        }
        let enable = value["enable"] == true
            || (action == "update"
                && snapshot
                    .document
                    .plugins
                    .get(&name)
                    .is_some_and(|p| p.enabled));
        let archive = prepared["archive"]
            .take()
            .as_str()
            .unwrap_or("")
            .to_string();
        if action == "update" {
            Command::UpdatePlugin {
                name,
                archive,
                version,
                enable,
                force: value["force"] == true,
            }
        } else {
            Command::InstallPlugin {
                name,
                archive,
                version,
                enable,
                force: value["force"] == true,
            }
        }
    } else {
        let entry = match h.query(Query::PluginStatus { name: name.clone() }).await {
            Ok(v) => v,
            Err(e) => return error(e),
        };
        if entry.is_null() {
            return response(404, json!({"ok":false,"error":"plugin not installed"}));
        }
        if action == "uninstall" {
            Command::RemovePlugin {
                name,
                purge: value["purge"] == true,
                force: value["force"] == true,
            }
        } else {
            let state = entry["state"].as_str().unwrap_or("");
            // A manifest-v1 plugin cannot run. It may be disabled, which clears
            // the error state, but enabling it would only run its root hooks
            // and then report the migration error.
            let legacy = entry["migration_warning"].is_string();
            if ["broken", "error"].contains(&state)
                && !(legacy && state == "error" && action == "disable")
            {
                return lifecycle_error(409, state, entry["error"].as_str().unwrap_or(state));
            }
            if legacy && action == "enable" {
                return response(409, json!({"ok":false,"error":entry["migration_warning"]}));
            }
            Command::SetPluginEnabled {
                name,
                enabled: action == "enable",
                version: entry["version"].as_str().map(str::to_string),
            }
        }
    };
    drop(_guard);
    submit(&h, command, &value, ui(session)).await
}
#[cfg(test)]
mod tests;

async fn upload(
    State(handle): State<StateHandle>,
    HttpQuery(query): HttpQuery<std::collections::BTreeMap<String, String>>,
    headers: HeaderMap,
    mut body: Body,
) -> Response {
    use http_body_util::BodyExt;
    use tokio::io::AsyncWriteExt;
    let name = query.get("name").map(String::as_str).unwrap_or("");
    if !crate::validation::identifier(name) || name.len() < 2 {
        return response(400, json!({"ok":false,"error":"invalid plugin name"}));
    }
    let media = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !["application/octet-stream", "application/gzip"].contains(&media.as_str()) {
        return response(400, json!({"ok":false,"error":"raw archive required"}));
    }
    let limit = ndimon_state::domains::plugins::host::MAX_ARCHIVE;
    if headers
        .get(header::CONTENT_LENGTH)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .is_some_and(|n| n > limit)
    {
        return response(413, json!({"ok":false,"error":"archive exceeds 200mb"}));
    }
    let directory = match handle.plugin_upload_directory().await {
        Ok(p) => p,
        Err(e) => return error(e),
    };
    let save = async {
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|e| e.to_string())?;
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|e| e.to_string())?;
        let temp = tempfile::NamedTempFile::new_in(&directory).map_err(|e| e.to_string())?;
        let file = temp.reopen().map_err(|e| e.to_string())?;
        let mut file = tokio::fs::File::from_std(file);
        let mut size = 0u64;
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|e| e.to_string())?;
            if let Ok(bytes) = frame.into_data() {
                size = size.saturating_add(bytes.len() as u64);
                if size > limit {
                    return Err("archive exceeds 200mb".into());
                }
                file.write_all(&bytes).await.map_err(|e| e.to_string())?;
            }
        }
        file.sync_all().await.map_err(|e| e.to_string())?;
        temp.persist(directory.join(format!("{name}.tar.gz")))
            .map_err(|e| e.to_string())?;
        Ok::<_, String>(())
    };
    match tokio::time::timeout(std::time::Duration::from_secs(300), save).await {
        Ok(Ok(())) => success(json!({"ok":true,"name":name})),
        Ok(Err(e)) => response(
            if e == "archive exceeds 200mb" {
                413
            } else {
                400
            },
            json!({"ok":false,"error":e}),
        ),
        Err(_) => response(408, json!({"ok":false,"error":"upload timeout"})),
    }
}
async fn asset(
    State(handle): State<StateHandle>,
    Path((name, path)): Path<(String, String)>,
) -> Response {
    use base64::Engine;
    let value = match handle.plugin_asset(name, path).await {
        Ok(v) => v,
        Err(_) => return response(404, json!({"ok":false,"error":"not found"})),
    };
    let bytes = match base64::engine::general_purpose::STANDARD
        .decode(value["data"].as_str().unwrap_or(""))
    {
        Ok(b) => b,
        Err(_) => return response(500, json!({"ok":false,"error":"invalid UI asset"})),
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            value["mime"].as_str().unwrap_or("application/octet-stream"),
        )
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        .header("x-frame-options", "SAMEORIGIN")
        .body(Body::from(bytes))
        .unwrap()
}

async fn process_command(
    State(handle): State<StateHandle>,
    Path((name, command)): Path<(String, String)>,
    session: Option<Extension<UiSession>>,
    bytes: Bytes,
) -> Response {
    let value = match parse(bytes) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    match handle
        .plugin_command(
            name,
            command,
            value.get("input").cloned().unwrap_or_else(|| json!({})),
            value["expect_revision"].as_u64(),
            ui(session),
        )
        .await
    {
        Ok((submission, output)) => {
            success(json!({"ok":true,"revision":submission.reply.revision,"output":output}))
        }
        Err(e) => error(e),
    }
}
