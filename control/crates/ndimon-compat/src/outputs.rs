//! Legacy output/decode URLs compile to serialized native commands.
use crate::common::*;
use axum::{
    Extension, Router,
    extract::{FromRef, Query as Params, State},
    response::Response,
    routing::{get, post},
};
use ndimon_state::{StateHandle, Submission, persist::StateFile};
use ndimon_types::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    StateHandle: FromRef<S>,
{
    let decode = Router::new()
        .route("/decodestatus", get(status))
        .route("/decodesetup", get(setup).post(set_setup))
        .route("/connectTo", get(use_post).post(connect))
        .route("/capture", get(capture));
    let video = Router::new()
        .route("/modes", get(modes))
        .route("/resolution", get(resolution).post(set_resolution))
        .route("/rotation", post(set_rotation))
        .route("/scaleMode", post(set_scale))
        .route(
            "/setresolution",
            get(|| async { response(405, json!({"ok":false,"error":"use POST /resolution"})) }),
        );
    Router::new()
        .nest("/v1/NDIDecode", decode.clone())
        .nest("/NDIDecode", decode)
        .nest("/v1/VideoOutput", video.clone())
        .nest("/VideoOutput", video)
        .route(
            "/v1/DeviceSettings/output-alias",
            get(alias).post(set_alias),
        )
        .route("/DeviceSettings/output-alias", get(alias).post(set_alias))
}
pub(crate) fn invalid(reason: String) -> ApiError {
    ApiError::invalid(vec![FieldError::new("outputs", reason)])
}
pub(crate) fn error(error: ApiError) -> Response {
    if let Some(reply) =
        revision_conflict(&error, json!(["Settings changed. Reload before saving."]))
    {
        return reply;
    }
    if let ApiError::ConfirmationRequired { impact } = &error {
        return response(
            error.http_status(),
            json!({"ok":false,"error":"confirmation_required","impact":impact,"status":error.http_status()}),
        );
    }
    if let ApiError::ApplyFailed { reason, .. } = &error {
        let native = serde_json::Deserializer::from_str(reason)
            .into_iter::<Value>()
            .next()
            .and_then(Result::ok);
        let value = native
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({"ok":false,"error":reason}));
        return ipc_response(value);
    }
    let message = match &error {
        ApiError::Invalid { errors, .. } if errors.len() == 1 && errors[0].field == "outputs" => {
            errors[0].reason.clone()
        }
        ApiError::Invalid { errors, .. } => errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "),
        _ => error.to_string(),
    };
    response(error.http_status(), json!({"ok":false,"error":message}))
}
pub(crate) fn ipc_response(value: Value) -> Response {
    let status = match value["error"].as_str() {
        Some("timeout" | "expired") => 504,
        Some("decoder_offline") => 503,
        Some("rotation_busy" | "resolution_busy") => 409,
        _ if value["ok"] == false => 400,
        _ => 200,
    };
    response(status, value)
}
fn int(value: &Value) -> Option<i64> {
    if let Some(n) = value.as_i64() {
        return Some(n);
    }
    if let Some(n) = value.as_f64().filter(|n| n.is_finite()) {
        return Some(n as i64);
    }
    let text = value.as_str()?.trim_start();
    let count = text
        .chars()
        .enumerate()
        .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && (*c == '-' || *c == '+')))
        .count();
    text.get(..count)?.parse().ok()
}
pub(crate) fn channel(value: &Value) -> Result<u8, ApiError> {
    int(value)
        .and_then(|n| u8::try_from(n).ok())
        .filter(|n| (1..=8).contains(n))
        .ok_or_else(|| invalid("output must be 1–8".into()))
}
fn index(value: &Value) -> u8 {
    int(value).filter(|n| (0..8).contains(n)).unwrap_or(0) as u8
}
async fn submit(
    h: &StateHandle,
    snapshot: &StateFile,
    command: Command,
    confirmation: &Value,
    origin: Origin,
) -> Result<Submission, ApiError> {
    submit_domain(
        h,
        command,
        Some(snapshot.domain_revision(Domain::Outputs)),
        origin,
        confirmation,
        Domain::Outputs,
        invalid,
    )
    .await
}
async fn submit_atomic(
    h: &StateHandle,
    snapshot: &StateFile,
    command: Command,
    confirmation: &Value,
    origin: Origin,
) -> Result<(), ApiError> {
    // Shared decode fields span three domains: CAS the complete snapshot so a
    // concurrent NDI/audio change cannot be overwritten by a stale adapter.
    h.submit(
        request(command, Some(snapshot.revision), confirmation, invalid)?,
        origin,
    )
    .await?;
    Ok(())
}
pub(crate) fn projection(snapshot: &StateFile, id: u8) -> Result<Value, ApiError> {
    let spec = ndimon_render::PATHS
        .iter()
        .find(|p| p.path_id == format!("dec{id}"))
        .ok_or_else(|| invalid("output path missing".into()))?;
    let bytes = ndimon_render::outputs::render_path(
        spec,
        &snapshot.document,
        snapshot.revision,
        &snapshot.legacy_files,
    )
    .map_err(invalid)?;
    serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))
}
async fn use_post() -> Response {
    response(405, json!({"ok":false,"error":"use POST"}))
}
async fn status(State(h): State<StateHandle>) -> Response {
    match h.query(Query::Status).await {
        Ok(v) => success(v),
        Err(e) => error(e),
    }
}
async fn setup(State(h): State<StateHandle>) -> Response {
    match h.snapshot().await.and_then(|s| projection(&s, 1)) {
        Ok(v) => success(v),
        Err(e) => error(e),
    }
}
async fn set_setup(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    let value = match body(value) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    match compile_setup(&h, &value, ui(session)).await {
        Ok(v) => success(v),
        Err(e) => error(e),
    }
}
async fn compile_setup(h: &StateHandle, body: &Value, origin: Origin) -> Result<Value, ApiError> {
    let snapshot = h.snapshot().await?;
    if body.get("ChNum").is_some_and(|v| int(v) != Some(1)) {
        return Err(invalid("ChNum must match output 1".into()));
    }
    let mut out = snapshot
        .document
        .outputs
        .get(&1)
        .cloned()
        .unwrap_or_default();
    match body["ScreenSaverMode"].as_str() {
        Some("SplashSS") => out.screensaver = ScreenSaver::Splash,
        Some("BlackSS") => out.screensaver = ScreenSaver::Black,
        Some("CaptureSS") => out.screensaver = ScreenSaver::Capture,
        _ => (),
    }
    let mut ndi = snapshot.document.ndi.clone();
    match body["TallyMode"].as_str() {
        Some("TallyOn") => ndi.tally = TallyMode::On,
        Some("TallyOff") => ndi.tally = TallyMode::Off,
        Some("VideoMode") => ndi.tally = TallyMode::Video,
        _ => (),
    }
    match body["ColorSpace"].as_str() {
        Some("RGB") => ndi.color_space = ColorSpace::Rgb,
        Some("YUV") => ndi.color_space = ColorSpace::Yuv,
        _ => (),
    }
    let mut enabled = BTreeMap::new();
    match body["NDIAudio"].as_str() {
        Some("NDIAudioEn") => {
            enabled.insert(1, true);
        }
        Some("NDIAudioDis") => {
            enabled.insert(1, false);
        }
        _ => (),
    }
    submit_atomic(
        h,
        &snapshot,
        Command::PutOutputSettings {
            outputs: BTreeMap::from([(1, out)]),
            ndi: Some(ndi),
            audio_enabled: enabled,
        },
        body,
        origin,
    )
    .await?;
    projection(&h.snapshot().await?, 1)
}
async fn connect(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    let value = match body(value) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    match compile_connect(&h, &value, ui(session)).await {
        Ok(v) => success(v),
        Err(e) => error(e),
    }
}
async fn compile_connect(h: &StateHandle, body: &Value, origin: Origin) -> Result<Value, ApiError> {
    let id = channel(body.get("Output").unwrap_or(&json!(1)))
        .map_err(|_| invalid("output must be 1–8".into()))?;
    let endpoint = body["SourceIP"].as_str().unwrap_or("");
    if body
        .get("SourceIP")
        .is_some_and(|v| !v.is_null() && v != "" && !v.is_string())
        || (!endpoint.is_empty() && !ndimon_state::domains::outputs::valid_endpoint(endpoint))
    {
        return Err(invalid("invalid source IPv4 endpoint".into()));
    }
    let snapshot = h.snapshot().await?;
    let name = body["SourceName"].as_str().unwrap_or("");
    let command = if name.is_empty() || name == "None" {
        Command::ForgetSource { output: id }
    } else {
        let mut endpoint = endpoint.to_string();
        if endpoint.is_empty() {
            let sources = h
                .query(Query::Catalog {
                    name: Catalog::NdiSources,
                    output: None,
                })
                .await
                .unwrap_or(Value::Null);
            if let Some(found) = sources["list"][name]
                .as_str()
                .filter(|s| ndimon_state::domains::outputs::valid_endpoint(s))
            {
                endpoint = found.into();
            }
        }
        Command::SelectSource {
            output: id,
            source: NdiSourceRef::new(name, endpoint),
        }
    };
    submit(h, &snapshot, command, body, origin).await?;
    let mut reply = json!({"ok":true});
    if let Some(name) = body.get("SourceName") {
        reply["SourceName"] = name.clone();
    }
    Ok(reply)
}
async fn capture() -> Response {
    error(ApiError::Unsupported {
        what: "frame capture has no decoder consumer".into(),
    })
}
async fn alias(
    State(h): State<StateHandle>,
    Params(p): Params<BTreeMap<String, String>>,
) -> Response {
    let id = match channel(&json!(p.get("ch").map(String::as_str).unwrap_or("1"))) {
        Ok(id) => id,
        Err(_) => return response(400, json!({"ok":false,"error":"ch must be 1–8"})),
    };
    match h.snapshot().await {
        Ok(s) => success(
            json!({"output_alias":s.document.outputs.get(&id).map(|o| o.alias.as_str()).unwrap_or(""),"ch":id}),
        ),
        Err(e) => error(e),
    }
}
async fn set_alias(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    let value = match body(value) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let id = match channel(value.get("ch").unwrap_or(&json!(1))) {
        Ok(id) => id,
        Err(_) => return response(400, json!({"ok":false,"error":"ch must be 1–8"})),
    };
    let snapshot = match h.snapshot().await {
        Ok(s) => s,
        Err(e) => return error(e),
    };
    let alias = value["output_alias"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    match submit(
        &h,
        &snapshot,
        Command::SetOutputAlias {
            output: id,
            alias: alias.clone(),
        },
        &value,
        ui(session),
    )
    .await
    {
        Ok(_) => success(json!({"output_alias":alias,"ch":id})),
        Err(e) => error(e),
    }
}
async fn modes_value(h: &StateHandle, p: &BTreeMap<String, String>) -> Result<Value, ApiError> {
    h.query(Query::Catalog {
        name: Catalog::DisplayModes,
        output: Some(index(&json!(p.get("output"))) + 1),
    })
    .await
}
async fn modes(
    State(h): State<StateHandle>,
    Params(p): Params<BTreeMap<String, String>>,
) -> Response {
    match modes_value(&h, &p).await {
        Ok(v) => success(v),
        Err(e) => error(e),
    }
}
async fn resolution(
    State(h): State<StateHandle>,
    Params(p): Params<BTreeMap<String, String>>,
) -> Response {
    match modes_value(&h, &p).await {
        Ok(v) => success(v.get("current").cloned().unwrap_or(json!({}))),
        Err(e) => error(e),
    }
}
async fn video(
    h: StateHandle,
    value: Body,
    session: Option<Extension<UiSession>>,
    kind: &str,
) -> Response {
    let value = match body(value) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    match compile_video(&h, &value, ui(session), kind).await {
        Ok(v) => ipc_response(v),
        Err(e) => error(e),
    }
}
async fn compile_video(
    h: &StateHandle,
    value: &Value,
    origin: Origin,
    kind: &str,
) -> Result<Value, ApiError> {
    let id = index(&value["output"]) + 1;
    let snapshot = h.snapshot().await?;
    let mut video = snapshot
        .document
        .outputs
        .get(&id)
        .cloned()
        .unwrap_or_default()
        .video;
    match kind {
        "resolution" if value["auto"] == true => video.resolution = Resolution::Auto,
        "resolution" => {
            let width = int(&value["width"])
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n > 0)
                .ok_or_else(|| invalid("width and height required".into()))?;
            let height = int(&value["height"])
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n > 0)
                .ok_or_else(|| invalid("width and height required".into()))?;
            let hz = value["refresh_hz"]
                .as_f64()
                .or_else(|| value["refresh_hz"].as_str().and_then(|s| s.parse().ok()))
                .filter(|n| *n != 0.0)
                .unwrap_or_else(|| int(&value["refresh"]).unwrap_or(0) as f64);
            video.resolution = Resolution::Mode {
                width,
                height,
                refresh_hz: hz,
            };
        }
        "rotation" => {
            let angle = int(value.get("degrees").unwrap_or(&json!(0))).unwrap_or(-1);
            if ![0, 90, 180, 270].contains(&angle) {
                return Err(invalid("degrees must be 0, 90, 180, or 270".into()));
            }
            if [90, 270].contains(&angle) {
                return Err(invalid("rotation_unsupported".into()));
            }
            video.rotation = angle as u16;
        }
        _ => {
            video.scale_mode = serde_json::from_value(
                value
                    .get("scale_mode")
                    .cloned()
                    .unwrap_or(json!("letterbox")),
            )
            .map_err(|_| invalid("invalid scale_mode".into()))?
        }
    }
    let result = submit(
        h,
        &snapshot,
        Command::SetOutputVideo { output: id, video },
        value,
        origin,
    )
    .await?;
    if kind == "scale" {
        return Ok(
            json!({"ok":true,"scale_mode":value.get("scale_mode").cloned().unwrap_or(json!("letterbox"))}),
        );
    }
    if result.apply.as_object().is_some_and(|o| o.len() > 1) {
        return Ok(result.apply);
    }
    // Minimal setter acknowledgements still need the live modes response.
    let mut modes = h
        .query(Query::Catalog {
            name: Catalog::DisplayModes,
            output: Some(id),
        })
        .await?;
    modes["ok"] = json!(true);
    Ok(modes)
}
async fn set_resolution(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    video(h, value, session, "resolution").await
}
async fn set_rotation(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    video(h, value, session, "rotation").await
}
async fn set_scale(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    video(h, value, session, "scale").await
}

#[cfg(test)]
pub(crate) mod tests;
