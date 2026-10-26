//! Audio compatibility routes and legacy error/operation mappings.
use crate::{common::*, validation};
use axum::{
    Extension, Router,
    extract::{FromRef, Path, State},
    response::Response,
    routing::{get, patch},
};
use ndimon_state::{StateHandle, Submission, audio, catalog};
use ndimon_types::*;
use serde_json::{Value, json};

pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    StateHandle: FromRef<S>,
{
    Router::new()
        .nest("/v1/Audio", audio_router())
        .route(
            "/v1/DeviceSettings/audioEngine",
            get(device_engine).post(device_set_engine),
        )
        .route(
            "/DeviceSettings/audioEngine",
            get(device_engine).post(device_set_engine),
        )
}
pub fn audio_router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    StateHandle: FromRef<S>,
{
    Router::new()
        .route("/endpoints", get(endpoints))
        .route("/outputs/{ch}", get(output).put(put_output))
        .route("/engine", get(engine).post(set_engine).put(set_engine))
        .route("/settings", get(settings).put(put_settings))
        .route("/sources/{id}", patch(patch_source))
        .route("/sinks/{id}", patch(patch_sink))
        .route("/status", get(status))
}
fn code(error: &ApiError) -> &'static str {
    match error {
        ApiError::RevisionConflict { .. } => "revision_conflict",
        ApiError::ConfirmationRequired { .. } => "confirmation_required",
        ApiError::Invalid { .. } => "invalid",
        ApiError::ApplyFailed { .. } => "apply_failed",
        ApiError::Locked { field, .. } if field == "legacy_edit_unsupported" => {
            "legacy_edit_unsupported"
        }
        ApiError::Locked { field, .. } if field == "matrix_confirmation_required" => {
            "matrix_confirmation_required"
        }
        _ => "audio_error",
    }
}
fn legacy_error(error: ApiError) -> Response {
    if let Some(reply) = revision_conflict(
        &error,
        json!(["Audio settings changed. Reload the latest settings before saving."]),
    ) {
        return reply;
    }
    let mut value = json!({"ok":false,"error":code(&error),"errors":[error.to_string()],"status":error.http_status()});
    if let ApiError::Invalid { errors, .. } = &error {
        value =
            json!({"ok":false,"errors":errors.iter().map(ToString::to_string).collect::<Vec<_>>()});
    }
    if let ApiError::ConfirmationRequired { impact } = &error {
        value["impact"] = json!(impact);
    }
    let mut status = error.http_status();
    if let ApiError::ApplyFailed { reason, step, .. } = &error {
        let native = serde_json::from_str::<Value>(reason).ok();
        if let Some(fields) = native.as_ref().and_then(Value::as_object) {
            for (key, v) in fields {
                value[key] = v.clone();
            }
        }
        status = if native
            .as_ref()
            .is_some_and(|v| v["error"] == "decoder_offline" || v["error"] == "timeout")
            || ["decoder_offline", "timeout"].contains(&reason.as_str())
        {
            503
        } else if native
            .as_ref()
            .is_some_and(|v| v["restart_required"] == true)
        {
            409
        } else if step == "preflight" || native.is_some() {
            400
        } else {
            500
        };
        value["status"] = json!(status);
        value["ok"] = json!(false);
    }
    response(status, value)
}
fn invalid(reason: impl Into<String>) -> ApiError {
    ApiError::invalid(vec![FieldError::new("audio", reason)])
}
fn number(value: &str) -> Result<u8, ApiError> {
    let ch = value
        .parse::<u8>()
        .ok()
        .filter(|ch| (1..=8).contains(ch))
        .ok_or_else(|| invalid("output must be1..8"))?;
    Ok(ch)
}
async fn settings(State(h): State<StateHandle>) -> Response {
    match h.query(Query::AudioSettings).await {
        Ok(v) => success(v),
        Err(e) => legacy_error(e),
    }
}
async fn engine(State(h): State<StateHandle>) -> Response {
    match h.snapshot().await {
        Ok(s) => success(json!({"audio_engine":s.document.audio.engine,"experimental":true})),
        Err(e) => legacy_error(e),
    }
}
async fn device_engine(State(h): State<StateHandle>) -> Response {
    match h.snapshot().await {
        Ok(s) => success(json!({"audio_engine":s.document.audio.engine})),
        Err(e) => legacy_error(e),
    }
}
async fn status(State(h): State<StateHandle>) -> Response {
    match h.query(Query::AudioStatus).await {
        Ok(v) => success(v),
        Err(e) => response(503, json!({"ok":false,"error":e.to_string()})),
    }
}
async fn output(State(h): State<StateHandle>, Path(ch): Path<String>) -> Response {
    match number(&ch) {
        Ok(output) => match h.query(Query::OutputAudio { output }).await {
            Ok(v) => success(v),
            Err(e) => legacy_error(e),
        },
        Err(e) => legacy_error(e),
    }
}
async fn gathered(h: &StateHandle) -> Result<Value, ApiError> {
    let data = h
        .query(Query::Catalog {
            name: Catalog::AudioEndpoints,
            output: None,
        })
        .await?;
    let audio: Audio =
        serde_json::from_value(data["audio"].clone()).map_err(|e| invalid(e.to_string()))?;
    Ok(catalog::build(
        &data["outputs"],
        data.get("tessera_status").filter(|v| !v.is_null()),
        data["tessera_enabled"] == true,
        &audio,
    ))
}
async fn endpoints(State(h): State<StateHandle>) -> Response {
    match gathered(&h).await {
        Ok(c) => match h.snapshot().await {
            Ok(s) => success(catalog::public(&c, s.audio_revision())),
            Err(e) => legacy_error(e),
        },
        Err(e) => legacy_error(e),
    }
}
fn applied(result: Submission) -> Value {
    json!({"ok":true,"revision":result.audio_revision,"apply":result.apply})
}
async fn save(
    h: &StateHandle,
    command: Command,
    revision: Option<u64>,
    origin: Origin,
    confirmation: &Value,
) -> Result<Value, ApiError> {
    Ok(applied(
        submit_domain(
            h,
            command,
            revision,
            origin,
            confirmation,
            Domain::Audio,
            invalid,
        )
        .await?,
    ))
}

async fn put_settings(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    let mut value = match body(value) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    // Confirmation belongs to the request, not the persisted legacy document.
    let mut confirmation = json!({});
    if let Some(object) = value.as_object_mut() {
        for key in ["confirm", "expect_impact"] {
            if let Some(field) = object.remove(key) {
                confirmation[key] = field;
            }
        }
    }
    match save_settings(&h, value, ui(session), &confirmation).await {
        Ok(v) => success(v),
        Err(e) => legacy_error(e),
    }
}
async fn save_settings(
    h: &StateHandle,
    mut value: Value,
    origin: Origin,
    confirmation: &Value,
) -> Result<Value, ApiError> {
    validation::legacy(&value).map_err(ApiError::invalid)?;
    let snapshot = h.snapshot().await?;
    let revision = value.get("revision").and_then(Value::as_u64);
    if revision != Some(snapshot.audio_revision()) {
        return Err(ApiError::RevisionConflict {
            current_revision: snapshot.audio_revision(),
        });
    }
    if value["version"] == 1 && snapshot.document.audio.engine == AudioEngine::V2 {
        audio::ensure_legacy_editable(&snapshot.document.audio, &gathered(h).await?)?;
    }
    let audio = if value["version"] == 1 {
        audio::overlay(&snapshot.document.audio, &value)?
    } else {
        ndimon_render::audio::import(
            &value,
            snapshot.document.audio.engine,
            ndimon_render::audio::primary(&snapshot.document.audio),
        )
        .map_err(invalid)?
    };
    // Graph snapshots cannot carry an independently selected, stale engine.
    value
        .as_object_mut()
        .expect("validated object")
        .remove("revision");
    save(h, Command::PutAudio(audio), revision, origin, confirmation).await
}
async fn patch_source(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    Path(id): Path<String>,
    value: Body,
) -> Response {
    patch_node(h, id, value, "sources", ui(session)).await
}
async fn patch_sink(
    session: Option<Extension<UiSession>>,
    State(h): State<StateHandle>,
    Path(id): Path<String>,
    value: Body,
) -> Response {
    patch_node(h, id, value, "sinks", ui(session)).await
}
async fn patch_node(
    h: StateHandle,
    id: String,
    value: Body,
    kind: &str,
    origin: Origin,
) -> Response {
    let value = match body(value) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    match patch_value(&h, &id, kind, value, origin).await {
        Ok(v) => success(v),
        Err(e) => legacy_error(e),
    }
}
async fn patch_value(
    h: &StateHandle,
    id: &str,
    kind: &str,
    changes: Value,
    origin: Origin,
) -> Result<Value, ApiError> {
    if !validation::identifier(id) || !changes.is_object() {
        return Err(invalid("invalid endpoint patch"));
    }
    let mut settings = h.query(Query::AudioSettings).await?;
    if settings[kind].get(id).is_none() {
        settings[kind][id] = json!({});
    }
    for (key, v) in changes
        .as_object()
        .ok_or_else(|| invalid("invalid endpoint patch"))?
    {
        settings[kind][id][key] = v.clone();
    }
    save_settings(h, settings, origin, &json!({})).await
}
async fn put_output(
    State(h): State<StateHandle>,
    Path(ch): Path<String>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    let value = match body(value) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let output = match number(&ch) {
        Ok(v) => v,
        Err(e) => return legacy_error(e),
    };
    match save_output(&h, output, value, ui(session)).await {
        Ok(v) => success(v),
        Err(e) => legacy_error(e),
    }
}
async fn save_output(
    h: &StateHandle,
    output: u8,
    value: Value,
    origin: Origin,
) -> Result<Value, ApiError> {
    validation::choices(&value).map_err(ApiError::invalid)?;
    let snapshot = h.snapshot().await?;
    let revision = value["revision"].as_u64();
    if revision != Some(snapshot.audio_revision()) {
        return Err(ApiError::RevisionConflict {
            current_revision: snapshot.audio_revision(),
        });
    }
    let mut choice = OutputAudio::default();
    choice.enabled = value["enabled"].as_bool().unwrap_or(false);
    choice.inputs =
        serde_json::from_value(value["inputs"].clone()).map_err(|e| invalid(e.to_string()))?;
    choice.destinations = serde_json::from_value(value["destinations"].clone())
        .map_err(|e| invalid(e.to_string()))?;
    let c = gathered(h).await?;
    let mut audio = match audio::compile_output(
        &snapshot.document.audio,
        output,
        choice.clone(),
        value["normalize_matrix"] == true,
        &c,
    ) {
        Ok(audio) => audio,
        Err(ApiError::Locked { field, .. }) if field == "matrix_confirmation_required" => {
            let before = audio::canonicalize(&snapshot.document.audio);
            let after = audio::compile_output(&snapshot.document.audio, output, choice, true, &c)?;
            let matrix = |audio: &Audio| {
                audio
                    .routes
                    .values()
                    .filter(|r| r.output == output)
                    .map(|r| json!({"source":r.source,"sink":r.sink,"gain_db":r.gain_db}))
                    .collect::<Vec<_>>()
            };
            return Ok(
                json!({"ok":false,"error":"matrix_confirmation_required","status":409,"errors":["Legacy per-destination gains/mutes would change; explicit normalization required"],"impact":{"warning":"Muted destinations may become audible","before":matrix(&before),"after":matrix(&after)}}),
            );
        }
        Err(e) => return Err(e),
    };
    // Incident #5: selecting a graph also selects its engine in this command.
    // The actor asks for confirmation when this changes the running engine.
    audio.engine = AudioEngine::V2;
    let mut candidate = snapshot.document.clone();
    candidate.audio = audio.clone();
    if let Err(errors) = ndimon_types::validate(&candidate) {
        return Err(ApiError::Locked {
            field: errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
            by: None,
        });
    }
    save(h, Command::PutAudio(audio), revision, origin, &value).await
}
async fn set_engine(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    let value = match body(value) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    if !value.is_object()
        || value.as_object().is_some_and(|o| {
            o.keys()
                .any(|k| !["audio_engine", "confirm"].contains(&k.as_str()))
        })
        || ![json!("v1"), json!("v2")].contains(&value["audio_engine"])
        || value["confirm"] != true
    {
        return response(
            400,
            json!({"ok":false,"status":400,"error":"explicit_confirmation_required","errors":["Explicit engine confirmation is required"]}),
        );
    }
    match save_engine(&h, &value, ui(session), false).await {
        Ok(v) => success(v),
        Err(e) => legacy_error(e),
    }
}
async fn device_set_engine(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    let value = match body(value) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    if ![json!("v1"), json!("v2")].contains(&value["audio_engine"]) {
        return response(
            400,
            json!({"ok":false,"error":"audio_engine must be v1 or v2"}),
        );
    }
    match save_engine(&h, &value, ui(session), true).await {
        Ok(v) => success(v),
        Err(e) => legacy_error(e),
    }
}
async fn save_engine(
    h: &StateHandle,
    value: &Value,
    origin: Origin,
    device: bool,
) -> Result<Value, ApiError> {
    let engine: AudioEngine = serde_json::from_value(value["audio_engine"].clone())
        .map_err(|e| invalid(e.to_string()))?;
    let snapshot = h.snapshot().await?;
    let changed = snapshot.document.audio.engine != engine;
    h.submit_audio(
        request(
            Command::SetAudioEngine { engine },
            Some(snapshot.audio_revision()),
            value,
            invalid,
        )?,
        origin,
    )
    .await?;
    Ok(if device {
        json!({"ok":true,"audio_engine":engine,"restart_required":true})
    } else if changed {
        json!({"ok":true,"audio_engine":engine,"restarted":true})
    } else {
        json!({"ok":true,"audio_engine":engine})
    })
}
