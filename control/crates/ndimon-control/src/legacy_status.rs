//! Node-compatible monitoring routes. All device observations use native IPC.
use crate::{Shared, sse::HubMessage};
use axum::{
    Extension, Router,
    body::Body,
    extract::{Path, State},
    http::header,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::stream;
use ndimon_compat::{
    UiSession,
    status::{encode, project, response},
};
use ndimon_state::ipc::{Decoder, UnixDecoder};
use ndimon_types::{
    Command, CommandRequest, Domain, Event, Origin, Query, Service, ServiceAction, StatusSource,
};
use serde_json::{Value, json};
use std::{path::Path as FsPath, sync::Mutex, time::Duration};
use tokio::sync::broadcast::error::RecvError;

#[derive(Debug, Default)]
pub struct Runtime {
    cpu: Mutex<Option<(u64, u64)>>,
    clients: Mutex<std::collections::VecDeque<tokio::sync::watch::Sender<bool>>>,
    snapshot: tokio::sync::Mutex<Option<(tokio::time::Instant, Value)>>,
}
pub fn router() -> Router<Shared> {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/watchdog-stats", get(watchdog))
        .route("/api/status", get(status))
        .route("/api/services", get(services))
        .route("/api/services/{name}/restart", post(restart))
        .route("/api/events", get(events))
}
fn decoder(state: &Shared) -> UnixDecoder {
    UnixDecoder {
        socket: state.config.decoder_socket.clone(),
        ..UnixDecoder::default()
    }
}
async fn health(State(state): State<Shared>) -> Response {
    match decoder(&state).request(json!({"action":"health"})).await {
        Ok(value) => response(200, &value),
        Err(_) => response(503, &json!({"alive":false,"error":"IPC timeout"})),
    }
}
async fn read_json(root: &FsPath, path: &str) -> Value {
    tokio::fs::read(root.join(path.trim_start_matches('/')))
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| json!({}))
}
async fn watchdog(State(state): State<Shared>) -> Response {
    let bytes = tokio::fs::read(
        state
            .config
            .legacy_root
            .join("tmp/ndimon-watchdog-stats.json"),
    )
    .await;
    let value = bytes.ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).unwrap_or_else(||
        json!({"watchdog_mode":"unknown","recv_stalls":0,"video_stalls":0,"decoder_stalls":0,"display_freezes":0,"reconnects":0,"service_restarts":0}));
    response(200, &value)
}
async fn status(State(state): State<Shared>) -> Response {
    response(200, &build_status(&state).await)
}

async fn build_status(state: &Shared) -> Value {
    // One system sample per broadcast period even with several browser tabs.
    let mut cached = state.legacy_status.snapshot.lock().await;
    if let Some((at, value)) = &*cached {
        if at.elapsed() < Duration::from_millis(750) {
            return value.clone();
        }
    }
    let ipc = decoder(state)
        .request(json!({"action":"status"}))
        .await
        .unwrap_or_else(|error| json!({"ok":false,"error":error}));
    let root = &state.config.legacy_root;
    let sources_path = if tokio::fs::try_exists(root.join("run/ndimon/ndimon-sources.json"))
        .await
        .unwrap_or(false)
    {
        "/run/ndimon/ndimon-sources.json"
    } else {
        "/var/lib/ndimon/ndimon-sources.json"
    };
    let (sources, dec, finder, device) = tokio::join!(
        read_json(root, sources_path),
        read_json(root, "/etc/ndimon-dec1-settings.json"),
        read_json(root, "/etc/ndimon-find-settings.json"),
        read_json(root, "/etc/ndimon-device-settings.json")
    );
    let metrics_state = state.clone();
    let system = tokio::task::spawn_blocking(move || metrics(&metrics_state))
        .await
        .unwrap_or(Value::Null);
    let (plugins, error) = plugin_summary(state).await;
    let mut value = project(
        &ipc, &sources, &dec, &finder, &device, system, plugins, error,
    );
    // Source menu order is the finder's insertion order, not a sorted map.
    if let Ok(bytes) = tokio::fs::read(root.join(sources_path.trim_start_matches('/'))).await {
        if let Ok(sources) = serde_json::from_slice::<Sources>(&bytes) {
            value["sources"] = json!(sources.list.0);
        }
    }
    *cached = Some((tokio::time::Instant::now(), value.clone()));
    value
}

#[derive(serde::Deserialize)]
struct Sources {
    #[serde(default)]
    list: SourceNames,
}
#[derive(Default)]
struct SourceNames(Vec<String>);
impl<'de> serde::Deserialize<'de> for SourceNames {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = SourceNames;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("source map")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut names = Vec::new();
                while let Some(name) = map.next_key::<String>()? {
                    map.next_value::<serde::de::IgnoredAny>()?;
                    if name != "None" {
                        names.push(name);
                    }
                }
                Ok(SourceNames(names))
            }
        }
        d.deserialize_map(Visitor)
    }
}

async fn plugin_summary(state: &Shared) -> (Vec<Value>, Value) {
    let Some(handle) = &state.document else {
        return (vec![], Value::Null);
    };
    // The plugin host owns its own transport.
    let list = match tokio::time::timeout(Duration::from_millis(500), handle.query(Query::Plugins))
        .await
    {
        Ok(Ok(value)) => value,
        _ => return local_plugin_summary(state).await,
    };
    let mut plugins = Vec::new();
    for entry in list["plugins"].as_array().into_iter().flatten() {
        let mut summary = json!({"status":entry.get("status").cloned().unwrap_or(Value::Null)});
        for key in ["name", "state", "version", "title", "icon"] {
            if let Some(value) = entry.get(key) {
                summary[key] = value.clone();
            }
        }
        plugins.push(summary);
    }
    (
        plugins,
        list.get("plugins_state_error")
            .cloned()
            .unwrap_or(Value::Null),
    )
}

// During the transition a stopped plugin host must not hide installed intent.
// Runtime status supplied by PX-1's list projection is retained above; this
// fallback reads only the native registry/manifest projections, never executes
// third-party JavaScript.
async fn local_plugin_summary(state: &Shared) -> (Vec<Value>, Value) {
    let root = &state.config.legacy_root;
    let saved = read_json(root, "/etc/ndimon-plugins.json").await;
    let runtime = read_json(root, "/var/lib/ndimon/plugin-runtime.json").await;
    let Ok(mut dirs) = tokio::fs::read_dir(root.join("opt/ndimon-r/plugins")).await else {
        return (vec![], Value::Null);
    };
    let mut names = Vec::new();
    while let Ok(Some(dir)) = dirs.next_entry().await {
        let name = dir.file_name().to_string_lossy().into_owned();
        if dir.file_type().await.is_ok_and(|t| t.is_dir()) && valid_plugin_name(&name) {
            names.push(name);
        }
    }
    names.sort();
    let mut result = Vec::new();
    for name in names {
        let manifest = read_json(root, &format!("/opt/ndimon-r/plugins/{name}/plugin.json")).await;
        let mut entry = json!({"name":name,"state":"error","status":null});
        if manifest["name"] == name && manifest["version"].is_string() {
            let observation = &runtime["plugins"][&name];
            let enabled = saved[&name]["enabled"] == true
                && saved[&name]["pending_enable"] != true
                && (observation.is_null()
                    || (observation["enabled"] == true && observation["pending_enable"] != true));
            entry["state"] = json!(if enabled { "enabled" } else { "installed" });
            if tokio::fs::try_exists(root.join(format!("opt/ndimon-r/plugins/{name}/.broken")))
                .await
                .unwrap_or(false)
            {
                entry["state"] = json!("broken");
            }
            for key in ["version", "title", "icon"] {
                if let Some(value) = manifest.get(key) {
                    entry[key] = value.clone();
                }
            }
        }
        result.push(entry);
    }
    (result, Value::Null)
}
fn valid_plugin_name(name: &str) -> bool {
    (2..=32).contains(&name.len())
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn metrics(state: &Shared) -> Value {
    let root = &state.config.legacy_root;
    let read = |path: &str| {
        std::fs::read_to_string(root.join(path.trim_start_matches('/'))).unwrap_or_default()
    };
    // libuv os.cpus includes user/nice/system/idle/irq. Linux folds iowait
    // into idle and softirq into irq; guest time is already included in user.
    let stat = read("/proc/stat");
    let times: Vec<u64> = stat
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(|v| v.parse().unwrap_or(0))
        .collect();
    let time = |i| times.get(i).copied().unwrap_or(0);
    let idle = time(3) + time(4);
    let total = time(0) + time(1) + time(2) + idle + time(5) + time(6);
    let mut previous = state.legacy_status.cpu.lock().expect("CPU sampler");
    let cpu = previous
        .map(|(old_total, old_idle)| {
            let delta = total.saturating_sub(old_total);
            if delta == 0 {
                0
            } else {
                ((delta.saturating_sub(idle.saturating_sub(old_idle))) as f64 * 100.0
                    / delta as f64)
                    .round() as u64
            }
        })
        .unwrap_or(0);
    *previous = Some((total, idle));
    let mem = read("/proc/meminfo");
    let kb = |key: &str| {
        mem.lines()
            .find_map(|line| {
                line.strip_prefix(key)
                    .and_then(|v| v.split_whitespace().next())
                    .and_then(|v| v.parse::<u64>().ok())
            })
            .unwrap_or(0)
    };
    let total = kb("MemTotal:");
    // MemAvailable includes the page cache the kernel gives back on demand;
    // MemFree does not, so a board that had just built or copied a lot showed
    // half its memory used (HW-1 H8: 1888 MB shown, 467 MB really in use). A
    // kernel without MemAvailable keeps the old figure.
    let free = if mem.lines().any(|line| line.starts_with("MemAvailable:")) {
        kb("MemAvailable:")
    } else {
        kb("MemFree:")
    };
    let used = total.saturating_sub(free);
    let mut temps = json!({});
    if let Ok(zones) = std::fs::read_dir(root.join("sys/class/thermal")) {
        for zone in zones
            .flatten()
            .filter(|z| z.file_name().to_string_lossy().starts_with("thermal_zone"))
        {
            let Ok(raw) = std::fs::read_to_string(zone.path().join("temp")) else {
                continue;
            };
            let Ok(raw) = raw.trim().parse::<f64>() else {
                continue;
            };
            if !raw.is_finite() {
                continue;
            }
            let name = std::fs::read_to_string(zone.path().join("type"))
                .unwrap_or_else(|_| zone.file_name().to_string_lossy().into_owned());
            let name = name.trim();
            let temp = (raw / 100.0 + 0.5).floor() / 10.0;
            if temps
                .get(name)
                .and_then(Value::as_f64)
                .is_none_or(|old| temp > old)
            {
                temps[name] = json!(temp);
            }
        }
    }
    let seconds = read("/proc/uptime")
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;
    let (d, h, m) = (seconds / 86400, seconds % 86400 / 3600, seconds % 3600 / 60);
    let uptime = if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    };
    json!({"cpu_percent":cpu,"memory":{"used_mb":(used as f64 / 1024.0).round() as u64,
        "total_mb":(total as f64 / 1024.0).round() as u64,"percent":if total == 0 {0} else {(used as f64 * 100.0 / total as f64).round() as u64}},"temps":temps,"uptime":uptime})
}

pub const MANAGED_SERVICES: &[(&str, Service)] = &[
    ("ndimon-r", Service::Decoder),
    ("ndimon-finder", Service::Finder),
    ("ndimon-control", Service::Control),
];
async fn services() -> Response {
    let mut services = Vec::new();
    for (name, _) in MANAGED_SERVICES {
        let output = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::process::Command::new("systemctl")
                .args([
                    "show",
                    &format!("{name}.service"),
                    "--no-pager",
                    "--property=ActiveState,SubState,MainPID,ExecMainStartTimestamp",
                ])
                .kill_on_drop(true)
                .output(),
        )
        .await;
        let raw = match output {
            Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            _ => String::new(),
        };
        services.push(service_status(name, &raw));
    }
    response(200, &json!({"services":services}))
}
pub fn service_status(name: &str, raw: &str) -> Value {
    let prop = |key: &str, default: &str| {
        raw.lines()
            .find_map(|line| {
                line.split_once('=')
                    .filter(|(k, _)| *k == key)
                    .map(|(_, v)| v)
            })
            .filter(|v| !v.is_empty())
            .unwrap_or(default)
            .to_owned()
    };
    json!({"name":name,"active":prop("ActiveState","unknown"),"sub":prop("SubState","unknown"),
        "pid":prop("MainPID","0").parse::<u64>().unwrap_or(0),"started":prop("ExecMainStartTimestamp","")})
}
async fn restart(
    State(state): State<Shared>,
    Path(name): Path<String>,
    session: Option<Extension<UiSession>>,
) -> Response {
    let Some((_, service)) = MANAGED_SERVICES.iter().find(|(n, _)| *n == name) else {
        return response(
            400,
            &json!({"ok":false,"error":format!("Unknown service: {name}")}),
        );
    };
    let Some(handle) = &state.document else {
        return response(
            500,
            &json!({"ok":false,"error":format!("Failed to restart {name}: state unavailable")}),
        );
    };
    let mut request = CommandRequest::new(Command::ServiceControl {
        service: *service,
        action: ServiceAction::Restart,
    });
    // The legacy UI already confirms service restarts, like /System actions.
    request.confirm = true;
    let origin = Origin::Ui {
        session: session
            .map(|Extension(s)| s.0)
            .unwrap_or_else(|| "legacy".into()),
    };
    match handle.submit_domain(request, origin, Domain::Device).await {
        Ok(_) => response(
            200,
            &json!({"ok":true,"message":format!("{name} restarted")}),
        ),
        Err(error) => response(
            500,
            &json!({"ok":false,"error":format!("Failed to restart {name}: {error}")}),
        ),
    }
}

struct StreamState {
    app: Shared,
    receiver: tokio::sync::broadcast::Receiver<HubMessage>,
    tick: tokio::time::Interval,
    keepalive: tokio::time::Interval,
    cancelled: tokio::sync::watch::Receiver<bool>,
    connected: bool,
    pending_status: bool,
}
fn frame(value: &Value) -> Vec<u8> {
    let mut bytes = b"data: ".to_vec();
    bytes.extend(encode(value));
    bytes.extend(b"\n\n");
    bytes
}
async fn events(State(app): State<Shared>) -> Response {
    let (cancel, cancelled) = tokio::sync::watch::channel(false);
    {
        let mut clients = app.legacy_status.clients.lock().expect("legacy clients");
        clients.retain(|client| !client.is_closed());
        clients.push_back(cancel);
        if clients.len() > 20 {
            if let Some(oldest) = clients.pop_front() {
                let _ = oldest.send(true);
            }
        }
    }
    let start = tokio::time::Instant::now();
    let state = StreamState {
        receiver: app.hub.subscribe(),
        app,
        cancelled,
        connected: false,
        pending_status: false,
        tick: tokio::time::interval_at(
            start + Duration::from_millis(1500),
            Duration::from_millis(1500),
        ),
        keepalive: tokio::time::interval_at(
            start + Duration::from_secs(15),
            Duration::from_secs(15),
        ),
    };
    let mut state = state;
    state
        .tick
        .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    state
        .keepalive
        .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let stream = stream::unfold(state, |mut state| async move {
        if *state.cancelled.borrow() {
            return None;
        }
        let bytes = if !state.connected {
            state.connected = true;
            frame(&json!({"type":"connected"}))
        } else if state.pending_status {
            state.pending_status = false;
            let mut value = build_status(&state.app).await;
            value["type"] = json!("status");
            frame(&value)
        } else {
            loop {
                tokio::select! {
                    biased;
                    _ = state.cancelled.changed() => return None,
                    _ = state.keepalive.tick() => break b":\n\n".to_vec(),
                    event = state.receiver.recv() => match event {
                        Ok(HubMessage::Event(event)) => {
                            if let Event::Status { source, data } = event.as_ref() {
                                state.pending_status = *source == StatusSource::Decoder && matches!(data["type"].as_str(), Some("connection" | "remote_source"));
                                if state.pending_status { *state.app.legacy_status.snapshot.lock().await = None; }
                                if (*source == StatusSource::Decoder || data["type"] == "plugin_progress") && data.get("type").is_some() {
                                    break frame(data);
                                }
                            }
                        },
                        Ok(HubMessage::Resync) | Err(RecvError::Lagged(_)) => {
                            let mut value = build_status(&state.app).await; value["type"] = json!("status"); break frame(&value);
                        },
                        Err(RecvError::Closed) => return None,
                    },
                    _ = state.tick.tick() => {
                        let mut value = build_status(&state.app).await; value["type"] = json!("status"); break frame(&value);
                    }
                }
            }
        };
        Some((Ok::<_, std::convert::Infallible>(bytes), state))
    });
    (
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::CONNECTION, "keep-alive"),
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}
