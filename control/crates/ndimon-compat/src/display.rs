//! Legacy Splash routes and their coercions over the native display commands.
use crate::common::*;
use axum::{
    Extension, Router,
    extract::{DefaultBodyLimit, FromRef, State},
    response::Response,
    routing::{get, post},
};
use ndimon_state::{
    StateHandle,
    domains::display::assets::{MAX_LOGO_BYTES, StoredAsset},
};
use ndimon_types::*;
use serde_json::{Value, json};

type PublicBody = Result<axum::body::Bytes, axum::extract::rejection::BytesRejection>;

fn invalid(reason: String) -> ApiError {
    ApiError::invalid(vec![FieldError::new("display", reason)])
}
fn error(error: ApiError) -> Response {
    if let Some(response) = revision_conflict(
        &error,
        json!(["Display settings changed. Reload the latest settings before saving."]),
    ) {
        return response;
    }
    let status = match &error {
        ApiError::ApplyFailed { reason, .. }
            if ["decoder_offline", "timeout"].contains(&reason.as_str()) =>
        {
            503
        }
        _ => error.http_status(),
    };
    response(status, json!({"ok":false,"error":error.to_string()}))
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}
fn string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Object(_) => "[object Object]".into(),
        Value::Array(a) => a
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    string(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        _ => String::from_utf8(ndimon_render::node_json(value.clone()).unwrap_or_default())
            .unwrap_or_default(),
    }
}
fn percent(value: &Value) -> f64 {
    let text = string(value);
    let text = text.trim_start();
    let bytes = text.as_bytes();
    let mut end = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    if text.get(end..).is_some_and(|s| s.starts_with("Infinity")) {
        return if bytes.first() == Some(&b'-') {
            0.0
        } else {
            100.0
        };
    }
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    if bytes.get(end) == Some(&b'.') {
        end += 1;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
    }
    let mantissa = end;
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        end += 1;
        if matches!(bytes.get(end), Some(b'+' | b'-')) {
            end += 1;
        }
        let digits = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == digits {
            end = mantissa;
        }
    }
    text[..end].parse::<f64>().unwrap_or(0.0).clamp(0.0, 100.0)
}
fn truncate(value: &Value) -> String {
    String::from_utf16_lossy(&string(value).encode_utf16().take(128).collect::<Vec<_>>())
}
fn update_splash(display: &mut Display, body: &Value) -> Result<(), ApiError> {
    let mut values = serde_json::to_value(&display.splash).map_err(|e| invalid(e.to_string()))?;
    for key in [
        "bg_idle",
        "bg_live",
        "accent_idle",
        "accent_live",
        "text_idle",
        "text_live",
    ] {
        if let Some(value) = body.get(key) {
            values[key] = json!(string(value));
        }
    }
    for key in ["logo_x_pct", "logo_y_pct", "logo_w_pct", "text_height_pct"] {
        if let Some(value) = body.get(key) {
            values[key] = json!(percent(value));
        }
    }
    for key in [
        "show_signal_text",
        "show_device_name",
        "show_device_url",
        "show_sources_available",
    ] {
        if let Some(value) = body.get(key) {
            values[key] = json!(truthy(value));
        }
    }
    if matches!(body["backdrop"].as_str(), Some("gradient" | "flat")) {
        values["backdrop"] = body["backdrop"].clone();
    }
    if body
        .get("logo_path")
        .is_some_and(|v| !truthy(v) || string(v).is_empty())
    {
        values["logo"] = Value::Null;
    }
    // Nonempty paths cannot create a reference to absent or unverified bytes.
    // The UI round-trips the current managed pointer; uploads use SetLogo.
    display.splash = serde_json::from_value(values).map_err(|e| invalid(e.to_string()))?;
    Ok(())
}
fn update_osd(display: &mut Display, body: &Value) {
    if let Some(v) = body.get("enabled") {
        display.osd.enabled = truthy(v);
    }
    if let Some(v) = body.get("text") {
        display.osd.text = truncate(v);
    }
}
fn decode(data: &Value) -> Result<Vec<u8>, String> {
    if let Some(s) = data.as_str() {
        let mut bytes = Vec::with_capacity((s.len() * 3 / 4).min(MAX_LOGO_BYTES + 1));
        let mut bits = 0u32;
        let mut count = 0u32;
        for b in s.bytes() {
            let digit = match b {
                b'A'..=b'Z' => b - b'A',
                b'a'..=b'z' => b - b'a' + 26,
                b'0'..=b'9' => b - b'0' + 52,
                b'+' | b'-' => 62,
                b'/' | b'_' => 63,
                b'=' => break,
                _ => continue,
            };
            bits = (bits << 6) | u32::from(digit);
            count += 6;
            if count >= 8 {
                count -= 8;
                bytes.push((bits >> count) as u8);
                if bytes.len() > MAX_LOGO_BYTES {
                    return Err("logo is larger than 3 MB".into());
                }
                bits &= (1 << count) - 1;
            }
        }
        Ok(bytes)
    } else if let Some(array) = data.as_array() {
        if array.len() > MAX_LOGO_BYTES {
            return Err("logo is larger than 3 MB".into());
        }
        Ok(array
            .iter()
            .map(|n| n.as_i64().unwrap_or(0) as u8)
            .collect())
    } else {
        Err("invalid logo data".into())
    }
}
async fn store(h: &StateHandle, data: &Value) -> Result<StoredAsset, ApiError> {
    let bytes = decode(data).map_err(invalid)?;
    ndimon_state::assets::media_type(&bytes).map_err(invalid)?;
    let assets = h.assets.clone();
    tokio_store(assets, bytes).await
}
async fn tokio_store(
    assets: ndimon_state::domains::display::assets::AssetStore,
    bytes: Vec<u8>,
) -> Result<StoredAsset, ApiError> {
    // Asset publication fsyncs files and the directory away from HTTP workers.
    tokio::task::spawn_blocking(move || assets.put(&bytes))
        .await
        .map_err(|e| ApiError::Internal {
            message: e.to_string(),
        })?
        .map_err(|message| ApiError::Internal { message })
}
async fn read_splash(State(h): State<StateHandle>) -> Response {
    match h.snapshot().await {
        Ok(s) => success(ndimon_render::display::splash(&s.document.display.splash)),
        Err(e) => error(e),
    }
}
async fn read_osd(State(h): State<StateHandle>) -> Response {
    match h.snapshot().await {
        Ok(s) => success(ndimon_render::display::osd(
            &s.document.display.osd,
            &s.legacy_files,
        )),
        Err(e) => error(e),
    }
}
async fn mutate(
    h: &StateHandle,
    operation: &str,
    value: &Value,
    origin: Origin,
) -> Result<Value, ApiError> {
    let asset = if operation == "uploadLogo" {
        Some(store(h, &value["data"]).await?)
    } else {
        None
    };
    for _ in 0..8 {
        let snapshot = h.snapshot().await?;
        let mut display = snapshot.document.display.clone();
        let command = match operation {
            "config" => {
                update_splash(&mut display, value)?;
                Command::SetDisplay(display.clone())
            }
            "osd" => {
                update_osd(&mut display, value);
                Command::SetDisplay(display.clone())
            }
            "uploadLogo" => Command::SetLogo {
                asset: asset.as_ref().map(|a| a.asset.clone()),
            },
            "preview" => Command::PreviewSplash {
                source_available: truthy(&value["source_available"]),
            },
            _ => return Err(invalid("unknown display operation".into())),
        };
        let result = submit_domain(
            h,
            command,
            Some(snapshot.domain_revision(Domain::Display)),
            origin.clone(),
            value,
            Domain::Display,
            invalid,
        )
        .await;
        match result {
            Err(ApiError::RevisionConflict { .. }) => continue,
            Err(e) => return Err(e),
            Ok(_) => {
                return Ok(match operation {
                    "config" => {
                        json!({"ok":true,"config":ndimon_render::display::splash(&display.splash)})
                    }
                    "osd" => {
                        json!({"ok":true,"config":ndimon_render::display::osd(&display.osd,&snapshot.legacy_files)})
                    }
                    "uploadLogo" => {
                        let asset = asset.as_ref().expect("upload asset");
                        let size = h.assets.read(&asset.asset).map_err(invalid)?.len();
                        json!({"ok":true,"path":ndimon_render::display::logo_path(Some(&asset.asset)),"size":size})
                    }
                    _ => json!({"ok":true}),
                });
            }
        }
    }
    Err(ApiError::RevisionConflict {
        current_revision: h.snapshot().await?.domain_revision(Domain::Display),
    })
}
async fn public(
    h: StateHandle,
    operation: &str,
    session: Option<Extension<UiSession>>,
    value: PublicBody,
) -> Response {
    let bytes = match value {
        Ok(bytes) => bytes,
        Err(error) => {
            return response(
                error.status().as_u16(),
                json!({"ok":false,"errors":[error.body_text()]}),
            );
        }
    };
    // body-parser accepts an empty request as {}, and rejects scalar JSON.
    let value = if bytes.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) if value.is_object() || value.is_array() => value,
            Ok(_) => {
                return response(
                    400,
                    json!({"ok":false,"errors":["expected a JSON object or array"]}),
                );
            }
            Err(error) => return response(400, json!({"ok":false,"errors":[error.to_string()]})),
        }
    };
    if operation == "uploadLogo" && !truthy(&value["data"]) {
        return response(400, json!({"ok":false,"error":"no data"}));
    }
    match mutate(&h, operation, &value, ui(session)).await {
        Ok(v) => success(v),
        Err(ApiError::Invalid { errors, .. }) if operation == "uploadLogo" => response(
            400,
            json!({"ok":false,"error":errors.first().map(|e| e.reason.as_str()).unwrap_or("invalid logo")}),
        ),
        Err(e) => error(e),
    }
}
macro_rules! handler {
    ($name:ident, $operation:literal) => {
        async fn $name(
            State(h): State<StateHandle>,
            session: Option<Extension<UiSession>>,
            value: PublicBody,
        ) -> Response {
            public(h, $operation, session, value).await
        }
    };
}
handler!(config, "config");
handler!(osd, "osd");
handler!(upload, "uploadLogo");
handler!(preview, "preview");
pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    StateHandle: FromRef<S>,
{
    Router::new()
        .route("/v1/Splash/config", get(read_splash).post(config))
        .route("/v1/Splash/osd", get(read_osd).post(osd))
        .route("/v1/Splash/uploadLogo", post(upload))
        .route("/v1/Splash/preview", post(preview))
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
}
#[cfg(test)]
mod tests;
