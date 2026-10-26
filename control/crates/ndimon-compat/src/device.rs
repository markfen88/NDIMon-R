//! Legacy device projections and the private restore handover.
use crate::common::*;
pub(crate) mod legacy;
use axum::{
    Extension, Router,
    extract::{FromRef, OriginalUri, State},
    response::{IntoResponse, Response},
    routing::get,
};
use legacy::{response, success};
use ndimon_state::StateHandle;
use ndimon_types::*;
use serde_json::{Value, json};

pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    StateHandle: FromRef<S>,
{
    let mut router = Router::new();
    for prefix in ["/v1/DeviceSettings", "/DeviceSettings"] {
        for setting in [
            "operationmode",
            "hostname",
            "ndi-alias",
            "decode-mode",
            "watchdog-mode",
            "ntp",
            "reboot-schedule",
            "web-allowed-hosts",
        ] {
            let path = format!("{prefix}/{setting}");
            router = if setting == "hostname" {
                router.route(&path, get(read))
            } else {
                router.route(&path, get(read).post(write))
            };
        }
    }
    for path in ["/v1/ApiSettings", "/v1/ApiSettings/"] {
        router = router.route(path, get(about).post(put_about));
    }
    for path in ["/v1/AboutMe", "/v1/AboutMe/", "/AboutMe", "/AboutMe/"] {
        router = router.route(path, get(about_me));
    }
    router
}
fn invalid(reason: String) -> ApiError {
    ApiError::invalid(vec![FieldError::new("device", reason)])
}
pub(crate) fn error(error: ApiError) -> Response {
    if let Some(r) = revision_conflict(
        &error,
        json!(["Device settings changed. Reload the latest settings before saving."]),
    ) {
        return r;
    }
    let message = match &error {
        ApiError::Invalid { reason, .. } => reason.clone(),
        ApiError::Unsupported { what } => what.clone(),
        ApiError::ApplyFailed { reason, .. } => reason.clone(),
        _ => error.to_string(),
    };
    let status = if message == "update already running" {
        409
    } else {
        error.http_status()
    };
    response(status, json!({"ok":false,"error":message}))
}
async fn query(h: &StateHandle, q: Query) -> Response {
    match h.query(q).await {
        Ok(v) => success(v),
        Err(e) => error(e),
    }
}
async fn read(State(h): State<StateHandle>, OriginalUri(uri): OriginalUri) -> Response {
    let state = match h.snapshot().await {
        Ok(s) => s,
        Err(e) => return error(e),
    };
    let d = state.document.device;
    match uri.path().rsplit('/').next().unwrap_or("") {
        "hostname" => query(&h, Query::Hostname).await,
        "ntp" => query(&h, Query::Ntp).await,
        "reboot-schedule" => query(&h, Query::RebootSchedule).await,
        "operationmode" => (
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            match d.mode {
                OperationMode::Encode => "encode",
                _ => "decode",
            },
        )
            .into_response(),
        "ndi-alias" => success(json!({"ndi_recv_name":d.ndi_alias})),
        "decode-mode" => success(json!({"decode_mode":d.decode_mode})),
        "watchdog-mode" => success(json!({"watchdog_mode":d.watchdog_mode})),
        "web-allowed-hosts" => success(json!({"web_allowed_hosts":d.web_allowed_hosts})),
        _ => unreachable!("registered device setting"),
    }
}
async fn submit(
    h: &StateHandle,
    patch: DevicePatch,
    revision: Option<u64>,
    origin: Origin,
    confirmation: &Value,
) -> Result<(), ApiError> {
    submit_domain(
        h,
        Command::SetDevice(patch),
        revision,
        origin,
        confirmation,
        Domain::Device,
        invalid,
    )
    .await
    .map(|_| ())
}
fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or("").trim().into()
}
async fn write(
    State(h): State<StateHandle>,
    OriginalUri(uri): OriginalUri,
    session: Option<Extension<UiSession>>,
    bytes: axum::body::Bytes,
) -> Response {
    let setting = uri.path().rsplit('/').next().unwrap_or("");
    let body = if setting == "operationmode" {
        json!(String::from_utf8_lossy(&bytes).to_string())
    } else {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => v,
            Err(_) => return response(400, json!({"ok":false,"error":"invalid JSON"})),
        }
    };
    for (route, key) in [("ndi-alias", "ndi_recv_name"), ("ntp", "ntp_server")] {
        if setting == route
            && body
                .get(key)
                .is_some_and(|v| !v.is_null() && !v.is_string())
        {
            return response(
                400,
                json!({"ok":false,"error":format!("{key} must be a string")}),
            );
        }
    }
    let mut p = DevicePatch::default();
    let result = match setting {
        "operationmode" => {
            let mode = body.as_str().unwrap_or("");
            p.mode = match mode {
                "decode" => Some(OperationMode::Decode),
                "encode" => Some(OperationMode::Encode),
                _ => None,
            };
            if p.mode.is_some() {
                if let Err(e) = submit(&h, p, None, ui(session), &Value::Null).await {
                    return error(e);
                }
            }
            return read(State(h), OriginalUri(uri)).await;
        }
        "ndi-alias" => {
            let v = text(&body, "ndi_recv_name");
            p.ndi_alias = Some(v.clone());
            json!({"ndi_recv_name":v})
        }
        "decode-mode" => {
            let v = text(&body, "decode_mode");
            p.decode_mode = match v.as_str() {
                "auto" => Some(DecodeMode::Auto),
                "hardware" => Some(DecodeMode::Hardware),
                "software" => Some(DecodeMode::Software),
                _ => {
                    return response(
                        400,
                        json!({"ok":false,"error":"must be auto, hardware, or software"}),
                    );
                }
            };
            json!({"ok":true,"decode_mode":v})
        }
        "watchdog-mode" => {
            let v = text(&body, "watchdog_mode");
            p.watchdog_mode = match v.as_str() {
                "disabled" => Some(WatchdogMode::Disabled),
                "passive" => Some(WatchdogMode::Passive),
                "active" => Some(WatchdogMode::Active),
                _ => {
                    return response(
                        400,
                        json!({"ok":false,"error":"must be disabled, passive, or active"}),
                    );
                }
            };
            json!({"ok":true,"watchdog_mode":v})
        }
        "ntp" => {
            let v = text(&body, "ntp_server");
            p.ntp_server = Some(v.clone());
            json!({"ok":true,"ntp_server":v})
        }
        "web-allowed-hosts" => {
            let hosts = match serde_json::from_value::<Vec<String>>(
                body["web_allowed_hosts"].clone(),
            ) {
                Ok(hosts) => hosts,
                Err(_) => {
                    return response(
                        400,
                        json!({"ok":false,"error":"web_allowed_hosts must be a list of DNS names"}),
                    );
                }
            };
            let hosts: Vec<_> = hosts
                .into_iter()
                .map(|host| host.to_ascii_lowercase())
                .collect();
            p.web_allowed_hosts = Some(hosts.clone());
            json!({"ok":true,"web_allowed_hosts":hosts})
        }
        "reboot-schedule" => {
            let time = text(&body, "time");
            if !ndimon_render::device::valid_time(&time) {
                return response(400, json!({"ok":false,"error":"time must be HH:MM (24h)"}));
            }
            let Some(days) = body["days"]
                .as_array()
                .and_then(|v| ndimon_render::device::normalize_days(v))
            else {
                return response(
                    400,
                    json!({"ok":false,"error":"days must be weekday names"}),
                );
            };
            let enabled = truthy(&body["enabled"]);
            if enabled && days.is_empty() {
                return response(400, json!({"ok":false,"error":"select at least one day"}));
            }
            let mut s = RebootSchedule::default();
            s.enabled = enabled;
            s.time = time;
            s.days = days;
            p.reboot_schedule = Some(s);
            Value::Null
        }
        _ => unreachable!("registered device setting"),
    };
    let revision = body["revision"].as_u64();
    if let Err(e) = submit(&h, p, revision, ui(session), &body).await {
        return error(e);
    }
    if setting == "reboot-schedule" {
        return match h.query(Query::RebootSchedule).await {
            Ok(mut v) => {
                v["ok"] = json!(true);
                success(v)
            }
            Err(e) => error(e),
        };
    }
    success(result)
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
        _ => true,
    }
}
fn about_bytes(state: &ndimon_state::persist::StateFile) -> Result<Vec<u8>, ApiError> {
    let spec = ndimon_render::PATHS
        .iter()
        .find(|s| s.path_id == "about")
        .expect("about artifact");
    let bytes = ndimon_render::device::render_path(
        spec,
        &state.document,
        state.domain_revision(Domain::Device),
        &state.legacy_files,
    )
    .map_err(invalid)?;
    legacy::compact(&bytes).map_err(invalid)
}
async fn about(State(h): State<StateHandle>) -> Response {
    match h.snapshot().await.and_then(|s| about_bytes(&s)) {
        Ok(bytes) => (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            bytes,
        )
            .into_response(),
        Err(e) => error(e),
    }
}
async fn put_about(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    let value = match body(value) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let state = match h.snapshot().await {
        Ok(s) => s,
        Err(e) => return error(e),
    };
    let mut a = state.document.device.about.clone();
    for (key, field) in [
        ("GroupName", &mut a.group_name),
        ("MACAddress", &mut a.mac_address),
        ("device_name", &mut a.device_name),
        ("host_name", &mut a.host_name),
    ] {
        if let Some(v) = value.get(key) {
            let Some(v) = v.as_str() else {
                return response(
                    400,
                    json!({"ok":false,"error":format!("{key} must be a string")}),
                );
            };
            *field = v.into();
        }
    }
    let mut p = DevicePatch::default();
    p.about = Some(a);
    if let Err(e) = submit(
        &h,
        p,
        Some(state.domain_revision(Domain::Device)),
        ui(session),
        &value,
    )
    .await
    {
        return error(e);
    }
    about(State(h)).await
}
async fn about_me(State(h): State<StateHandle>) -> Response {
    query(&h, Query::AboutMe).await
}
#[cfg(test)]
mod tests;
