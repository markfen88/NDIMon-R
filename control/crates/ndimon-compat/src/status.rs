//! Historical status projection and encoder compatibility stubs (Status.js).
use axum::{
    Router,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};

pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    let mut router = Router::new();
    for prefix in ["/v1/NDIEncoder", "/NDIEncoder"] {
        for name in ["encodesetup", "encodeTransport"] {
            router = router.route(
                &format!("{prefix}/{name}"),
                get(unsupported).post(unsupported),
            );
        }
    }
    router
}
async fn unsupported() -> Response {
    response(404, &json!({"status":"Command not supported"}))
}
fn truthy(value: &Value) -> bool {
    !value.is_null() && value != false && value != "" && value.as_f64() != Some(0.0)
}
fn fallback(object: &Value, key: &str, default: Value) -> Value {
    object
        .get(key)
        .filter(|v| truthy(v))
        .cloned()
        .unwrap_or(default)
}

/// Port of Status.js::buildStatus; the inputs are observations, never writers.
#[allow(clippy::too_many_arguments)]
pub fn project(
    ipc: &Value,
    sources: &Value,
    decoder: &Value,
    finder: &Value,
    device: &Value,
    system: Value,
    plugins: Vec<Value>,
    plugins_state_error: Value,
) -> Value {
    let mut stream = json!({});
    for (key, source, default) in [
        ("connected", "connected", json!(false)),
        ("source_name", "source_name", json!("")),
        ("platform", "platform", json!("Unknown")),
        ("total_frames", "total_frames", json!(0)),
        ("dropped_frames", "dropped_frames", json!(0)),
        ("display_width", "width", json!(0)),
        ("display_height", "height", json!(0)),
        ("display_refresh", "refresh", json!(0)),
        ("display_refresh_hz", "refresh_hz", json!(0)),
        ("input_width", "input_width", json!(0)),
        ("input_height", "input_height", json!(0)),
        ("codec", "codec", json!("none")),
        ("fps", "fps", json!(0)),
        ("connector", "connector", json!("")),
        ("output_alias", "output_alias", json!("")),
        ("recv_name", "recv_name", json!("")),
        ("device_name", "device_name", json!("")),
    ] {
        stream[key] = fallback(ipc, source, default);
    }
    stream["audio_enabled"] = json!(ipc["audio_enabled"] == true);
    let outputs: Vec<Value> = ipc["outputs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| {
            let mut output = json!({});
            for key in [
                "connector",
                "output_alias",
                "recv_name",
                "device_name",
                "source_name",
            ] {
                output[key] = fallback(o, key, json!(""));
            }
            for key in [
                "width",
                "height",
                "refresh",
                "refresh_hz",
                "input_width",
                "input_height",
                "fps",
                "decode_fps",
            ] {
                output[key] = fallback(o, key, json!(0));
            }
            for key in ["total_frames", "dropped_frames", "rotation"] {
                output[key] = o
                    .get(key)
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or(json!(0));
            }
            for key in ["connected", "hw_decode", "decode_saturated", "drm_ready"] {
                output[key] = fallback(o, key, json!(false));
            }
            for key in [
                "compressed_frame_observed",
                "audio_enabled",
                "audio_available",
            ] {
                output[key] = json!(o[key] == true);
            }
            for (key, default) in [
                ("codec", "none"),
                ("decode_backend", "none"),
                ("stream_type", "unknown"),
                ("scale_mode", "letterbox"),
            ] {
                output[key] = fallback(o, key, json!(default));
            }
            output["ch_num"] = fallback(o, "ch_num", json!(1));
            // JS tests undefined, not truthiness: explicit zero and null survive.
            output["output_index"] = o.get("output_index").cloned().unwrap_or_else(|| {
                o["ch_num"]
                    .as_i64()
                    .map(|n| json!(n - 1))
                    .unwrap_or(Value::Null)
            });
            output
        })
        .collect();
    let source_list: Vec<_> = sources["list"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.keys())
        .filter(|name| name.as_str() != "None")
        .cloned()
        .collect();
    json!({"stream":stream,"system":system,"sources":source_list,
        "decoder":{"audio":fallback(decoder,"NDIAudio",json!("NDIAudioEn")),
            "screensaver":fallback(decoder,"ScreenSaverMode",json!("BlackSS")),
            "tally":fallback(decoder,"TallyMode",json!("TallyOff")),
            "color_space":fallback(decoder,"ColorSpace",json!("YUV")),
            "decode_mode":fallback(device,"decode_mode",json!("auto"))},
        "discovery":{"enabled":truthy(&finder["NDIDisServIP"]),"server_ip":fallback(finder,"NDIDisServIP",json!(""))},
        "outputs":outputs,"plugins":plugins,"plugins_state_error":plugins_state_error,
        "audio":fallback(ipc,"audio",Value::Null)})
}

/// JSON.stringify-compatible numbers and the field insertion order in Status.js.
pub fn encode(value: &Value) -> Vec<u8> {
    let Value::Object(map) = value else {
        if let Value::Array(values) = value {
            let mut bytes = vec![b'['];
            for (i, value) in values.iter().enumerate() {
                if i != 0 {
                    bytes.push(b',');
                }
                bytes.extend(encode(value));
            }
            bytes.push(b']');
            return bytes;
        }
        return ndimon_render::node_json(value.clone()).expect("JSON observation");
    };
    let order: &[&str] = if map.contains_key("stream") {
        &[
            "type",
            "stream",
            "system",
            "sources",
            "decoder",
            "discovery",
            "outputs",
            "plugins",
            "plugins_state_error",
            "audio",
        ]
    } else if map.contains_key("display_width") {
        &[
            "connected",
            "source_name",
            "platform",
            "total_frames",
            "dropped_frames",
            "display_width",
            "display_height",
            "display_refresh",
            "display_refresh_hz",
            "input_width",
            "input_height",
            "codec",
            "fps",
            "audio_enabled",
            "connector",
            "output_alias",
            "recv_name",
            "device_name",
        ]
    } else if map.contains_key("ch_num") {
        &[
            "ch_num",
            "output_index",
            "connector",
            "output_alias",
            "recv_name",
            "device_name",
            "connected",
            "source_name",
            "width",
            "height",
            "refresh",
            "refresh_hz",
            "input_width",
            "input_height",
            "fps",
            "total_frames",
            "dropped_frames",
            "stream_type",
            "compressed_frame_observed",
            "rotation",
            "audio_enabled",
            "audio_available",
            "codec",
            "decode_backend",
            "hw_decode",
            "decode_fps",
            "decode_saturated",
            "scale_mode",
            "drm_ready",
        ]
    } else if map.contains_key("cpu_percent") {
        &["cpu_percent", "memory", "temps", "uptime"]
    } else if map.contains_key("used_mb") {
        &["used_mb", "total_mb", "percent"]
    } else if map.contains_key("screensaver") {
        &[
            "audio",
            "screensaver",
            "tally",
            "color_space",
            "decode_mode",
        ]
    } else if map.contains_key("server_ip") {
        &["enabled", "server_ip"]
    } else if map.contains_key("status") && map.contains_key("name") {
        &["name", "state", "version", "title", "icon", "status"]
    } else if map.contains_key("active") && map.contains_key("pid") {
        &["name", "active", "sub", "pid", "started"]
    } else if map.contains_key("watchdog_mode") {
        &[
            "watchdog_mode",
            "recv_stalls",
            "video_stalls",
            "decoder_stalls",
            "display_freezes",
            "reconnects",
            "service_restarts",
        ]
    } else if map.get("type").and_then(Value::as_str) == Some("plugin_progress") {
        &["name", "step", "error", "type"]
    } else if map.contains_key("ok") {
        &["ok", "error", "message"]
    } else {
        &[]
    };
    let keys = order
        .iter()
        .copied()
        .filter(|k| map.contains_key(*k))
        .chain(
            map.keys()
                .map(String::as_str)
                .filter(|k| !order.contains(k)),
        );
    let mut bytes = vec![b'{'];
    for (i, key) in keys.enumerate() {
        if i != 0 {
            bytes.push(b',');
        }
        bytes.extend(serde_json::to_vec(key).expect("JSON key"));
        bytes.push(b':');
        bytes.extend(encode(&map[key]));
    }
    bytes.push(b'}');
    bytes
}
pub fn response(status: u16, value: &Value) -> Response {
    (
        StatusCode::from_u16(status).expect("HTTP status"),
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        encode(value),
    )
        .into_response()
}
