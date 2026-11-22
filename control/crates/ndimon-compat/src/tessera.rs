//! Native implementations of the unchanged Tessera plugin URLs. Every mutation
//! is one actor command; this router never writes files or talks to tesserad.
use crate::common::*;
use axum::{
    Extension, Router,
    body::Bytes,
    extract::{FromRef, Path, Query as HttpQuery, State},
    response::Response,
    routing::{get, post},
};
use ndimon_state::{StateHandle, Submission, audio, domains::tessera::flow_values};
use ndimon_types::*;
use serde_json::{Value, json};
type ReadCache = std::sync::Arc<tokio::sync::Mutex<Option<(std::time::Instant, u64, Value)>>>;
const PREFIX: &str = "/v1/plugins/tessera";
pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    StateHandle: FromRef<S>,
{
    Router::new()
        .nest(
            PREFIX,
            Router::new()
                // The plugin lifecycle verbs of every plugin. This prefix is
                // more specific than `/v1/plugins/{name}/…`, so without these
                // routes `{operation}` would answer them (and 404 "not enabled"
                // for the one verb that can remove a broken Tessera).
                .route("/enable", post(enable))
                .route("/disable", post(disable))
                .route("/update", post(update))
                .route("/uninstall", post(uninstall))
                .route(
                    "/routing/{operation}",
                    get(routing_read).post(routing_write),
                )
                .route("/{operation}", get(read).post(write).put(write))
                .route("/audio/{operation}", post(write_audio)),
        )
        .layer(Extension(ReadCache::default()))
}
macro_rules! lifecycle_route {
    ($handler:ident, $action:literal) => {
        async fn $handler(
            State(h): State<StateHandle>,
            session: Option<Extension<UiSession>>,
            bytes: Bytes,
        ) -> Response {
            crate::plugins::lifecycle(h, session, bytes, "tessera".into(), $action).await
        }
    };
}
lifecycle_route!(enable, "enable");
lifecycle_route!(disable, "disable");
lifecycle_route!(update, "update");
lifecycle_route!(uninstall, "uninstall");
fn invalid(reason: impl Into<String>) -> ApiError {
    ApiError::invalid(vec![FieldError::new("tessera", reason)])
}
fn object(value: &Value, keys: &[&str]) -> Result<(), ApiError> {
    if !value.is_object()
        || value
            .as_object()
            .is_some_and(|m| m.keys().any(|k| !keys.contains(&k.as_str())))
    {
        return Err(invalid("unknown or invalid fields"));
    }
    Ok(())
}
fn integer(value: &Value, min: u64, max: u64, name: &str) -> Result<u64, ApiError> {
    value
        .as_u64()
        .filter(|n| (*n >= min) && (*n <= max))
        .ok_or_else(|| invalid(format!("{name} must be {min}..{max}")))
}
fn legacy_error(e: ApiError) -> Response {
    let encoded = json!(e);
    let code = if let Some(code) = encoded.as_str() {
        code.to_string()
    } else {
        encoded
            .as_object()
            .and_then(|m| m.keys().next())
            .cloned()
            .unwrap_or("internal".into())
    };
    let mut value = json!({"ok":false,"error":code,"detail":e.to_string()});
    let mut status = e.http_status();
    if let ApiError::ConfirmationRequired { impact } = &e {
        value["impact"] = impact
            .counterpart
            .as_deref()
            .cloned()
            .unwrap_or_else(|| json!(impact));
    }
    if let ApiError::ApplyFailed { reason, step, .. } = &e {
        if let Ok(detail) = serde_json::from_str::<Value>(reason) {
            if let Some(code) = detail["error"].as_str() {
                value["error"] = json!(code);
                value["detail"] = detail["detail"].clone();
                status = match code {
                    "offline" => 503,
                    "timeout" => 504,
                    "protocol" | "response_too_large" => 502,
                    "refused" => 409,
                    _ => 500,
                };
            }
        } else if step == "preflight" {
            status = 400;
        }
    }
    response(status, value)
}
async fn enabled(h: &StateHandle) -> Result<(), ApiError> {
    if h.snapshot()
        .await?
        .document
        .plugins
        .get("tessera")
        .is_some_and(|p| p.installed && p.enabled)
    {
        Ok(())
    } else {
        Err(ApiError::NotFound {
            what: "Tessera plugin is not enabled".into(),
        })
    }
}
async fn cached_status(h: &StateHandle, cache: &ReadCache) -> Result<Value, ApiError> {
    let revision = h.snapshot().await?.revision;
    let mut saved = cache.lock().await;
    if let Some((at, rev, value)) = &*saved {
        if *rev == revision && at.elapsed() < std::time::Duration::from_secs(2) {
            return Ok(value.clone());
        }
    }
    let value = h.query(Query::TesseraStatus).await?;
    *saved = Some((std::time::Instant::now(), revision, value.clone()));
    Ok(value)
}
async fn read(
    State(h): State<StateHandle>,
    Extension(cache): Extension<ReadCache>,
    Path(op): Path<String>,
) -> Response {
    if let Err(e) = enabled(&h).await {
        return legacy_error(e);
    }
    let result = match op.as_str() {
        "health" => cached_status(&h, &cache)
            .await
            .map(|s| json!({"installed":true,"daemon":s["daemon"]})),
        "status" => cached_status(&h, &cache).await,
        "metrics" => h.query(Query::TesseraMetrics).await,
        "diagnostics" => h.query(Query::TesseraDiagnostics).await,
        "flows" => h.query(Query::TesseraFlows).await,
        "subscriptions" => Err(ApiError::Unsupported {
            what: "Vendor subscription control is unavailable; configure static flows.".into(),
        }),
        _ => Err(ApiError::NotFound { what: op }),
    };
    match result {
        Ok(v) => success(v),
        Err(e) => legacy_error(e),
    }
}
fn short(value: Option<&String>, name: &str, max: usize) -> Result<Option<String>, ApiError> {
    match value {
        Some(v) if v.is_empty() || v.len() > max || v.chars().any(char::is_control) => {
            Err(invalid(format!("invalid {name}")))
        }
        v => Ok(v.cloned()),
    }
}
fn endpoint_query(
    q: &std::collections::BTreeMap<String, String>,
) -> Result<TesseraEndpointRef, ApiError> {
    Ok(TesseraEndpointRef::new(
        short(q.get("id"), "endpoint", 256)?.ok_or_else(|| invalid("endpoint required"))?,
        short(q.get("generation"), "generation", 256)?
            .ok_or_else(|| invalid("generation required"))?,
    ))
}
/// Tessera 0.5 routing, read through the daemon: `devices?cursor=`,
/// `channels?id=&generation=&direction=rx|tx&cursor=`, `receiver?id=&generation=&first=`.
async fn routing_read(
    State(h): State<StateHandle>,
    Path(op): Path<String>,
    HttpQuery(q): HttpQuery<std::collections::BTreeMap<String, String>>,
) -> Response {
    if let Err(e) = enabled(&h).await {
        return legacy_error(e);
    }
    let read = (|| {
        Ok::<_, ApiError>(match op.as_str() {
            "devices" => TesseraRoutingRead::Devices {
                cursor: short(q.get("cursor"), "cursor", 512)?,
            },
            "channels" => TesseraRoutingRead::Channels {
                endpoint: endpoint_query(&q)?,
                direction: match q.get("direction").map(String::as_str) {
                    Some("rx") => Direction::Rx,
                    Some("tx") => Direction::Tx,
                    _ => return Err(invalid("direction must be rx or tx")),
                },
                cursor: short(q.get("cursor"), "cursor", 512)?,
            },
            "receiver" => TesseraRoutingRead::Receiver {
                endpoint: endpoint_query(&q)?,
                first: q
                    .get("first")
                    .map(|v| v.parse::<u16>().ok().filter(|n| *n >= 1))
                    .unwrap_or(Some(1))
                    .ok_or_else(|| invalid("first must be 1..65535"))?,
            },
            "inspect" => TesseraRoutingRead::Inspect,
            _ => return Err(ApiError::NotFound { what: op.clone() }),
        })
    })();
    let result = match read {
        Ok(read) => h.query(Query::TesseraRouting { read }).await,
        Err(e) => Err(e),
    };
    match result {
        Ok(v) => success(v),
        Err(e) => legacy_error(e),
    }
}
/// `route` `{expectation, change}` executes one reviewed receiver change;
/// `sources` `{sources: [...]}` saves the trusted source hosts.
async fn routing_write(
    State(h): State<StateHandle>,
    Path(op): Path<String>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    if let Err(e) = enabled(&h).await {
        return legacy_error(e);
    }
    let value = match body(value) {
        Ok(v) => v,
        Err(e) => return *e,
    };
    let origin = ui(session);
    let result = async {
        match op.as_str() {
            "route" => {
                object(&value, &["expectation", "change"])?;
                let expectation = value["expectation"]
                    .as_str()
                    .filter(|e| !e.is_empty() && e.len() <= 256)
                    .ok_or_else(|| invalid("expectation required"))?
                    .to_string();
                let change: TesseraRouteChange = serde_json::from_value(value["change"].clone())
                    .map_err(|e| invalid(format!("invalid routing change: {e}")))?;
                let submission = submit_domain(
                    &h,
                    Command::TesseraRoute {
                        expectation,
                        change,
                    },
                    None,
                    origin,
                    &json!({}),
                    Domain::Tessera,
                    invalid,
                )
                .await?;
                Ok(json!({"ok":true,"result":submission.apply.get("tessera_result").cloned().unwrap_or(json!("done"))}))
            }
            "reconcile" => {
                object(&value, &["review"])?;
                let review = value["review"]
                    .as_str()
                    .filter(|e| !e.is_empty() && e.len() <= 256)
                    .ok_or_else(|| invalid("review required"))?
                    .to_string();
                let submission = submit_domain(
                    &h,
                    Command::TesseraReconcileRouting { review },
                    None,
                    origin,
                    &json!({}),
                    Domain::Tessera,
                    invalid,
                )
                .await?;
                Ok(json!({"ok":true,"result":submission.apply.get("tessera_result").cloned().unwrap_or(json!("done"))}))
            }
            "sources" => {
                object(&value, &["sources"])?;
                let sources: Vec<String> = serde_json::from_value(value["sources"].clone())
                    .map_err(|_| invalid("sources must be an array of device IDs"))?;
                let snapshot = h.snapshot().await?;
                let rev = snapshot.domain_revision(Domain::Tessera);
                let mut tessera = configured(&snapshot.document)?;
                tessera.routing_sources = sources;
                save(&h, Command::SetTessera(tessera), rev, &json!({}), origin)
                    .await
                    .map(|_| json!({"ok":true}))
            }
            _ => Err(ApiError::NotFound { what: op.clone() }),
        }
    }
    .await;
    match result {
        Ok(v) => success(v),
        Err(e) => legacy_error(e),
    }
}
async fn write(
    State(h): State<StateHandle>,
    Path(op): Path<String>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    if let Err(e) = enabled(&h).await {
        return legacy_error(e);
    }
    let value = match body(value) {
        Ok(v) => v,
        Err(e) => return *e,
    };
    match mutate(&h, &op, &value, ui(session)).await {
        Ok(v) => success(v),
        Err(e) => legacy_error(e),
    }
}
async fn write_audio(
    State(h): State<StateHandle>,
    Path(op): Path<String>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    if let Err(e) = enabled(&h).await {
        return legacy_error(e);
    }
    let value = match body(value) {
        Ok(v) => v,
        Err(e) => return *e,
    };
    match audio_mutate(&h, &op, &value, ui(session)).await {
        Ok(v) => success(v),
        Err(e) => legacy_error(e),
    }
}
async fn save(
    h: &StateHandle,
    command: Command,
    revision: u64,
    value: &Value,
    origin: Origin,
) -> Result<Submission, ApiError> {
    let mut confirmation = value.clone();
    if let Some(expect) = value.get("expect") {
        if !expect.is_string() || expect.as_str() == Some("") {
            return Err(invalid("confirmation token required"));
        }
        confirmation["expect_impact"] = expect.clone();
    }
    submit_domain(
        h,
        command,
        Some(revision),
        origin,
        &confirmation,
        Domain::Tessera,
        invalid,
    )
    .await
}
fn configured(state: &DesiredState) -> Result<Tessera, ApiError> {
    state
        .tessera
        .clone()
        .ok_or_else(|| invalid("Tessera is not configured"))
}
pub fn parse_flows(value: &Value) -> Result<Vec<TesseraFlow>, ApiError> {
    let values = value
        .as_array()
        .ok_or_else(|| invalid("flows must be an array fitting the 60KB plugin write limit"))?;
    values
        .iter()
        .map(|v| {
            object(
                v,
                &[
                    "id",
                    "direction",
                    "peer",
                    "port",
                    "frames_per_packet",
                    "any_source",
                ],
            )?;
            let mut v = v.clone();
            let m = v.as_object_mut().expect("object checked");
            m.entry("peer").or_insert(Value::Null);
            m.entry("frames_per_packet").or_insert(Value::Null);
            m.entry("any_source").or_insert(json!(false));
            serde_json::from_value(v).map_err(|e| invalid(e.to_string()))
        })
        .collect()
}
async fn mutate(h: &StateHandle, op: &str, v: &Value, origin: Origin) -> Result<Value, ApiError> {
    if op == "subscriptions" {
        return Err(ApiError::Unsupported {
            what: "Vendor subscription control is unavailable; configure static flows.".into(),
        });
    }
    let snapshot = h.snapshot().await?;
    let rev = snapshot.domain_revision(Domain::Tessera);
    if op == "service" {
        object(v, &["action", "confirm", "expect"])?;
        let action = serde_json::from_value::<TesseraServiceAction>(v["action"].clone())
            .map_err(|_| invalid("invalid service action"))?;
        let result = save(h, Command::TesseraService { action }, rev, v, origin).await?;
        return Ok(if action == TesseraServiceAction::Reload {
            json!({"ok":true,"result":result.apply.get("tessera_result").cloned().unwrap_or(json!("done"))})
        } else {
            json!({"ok":true})
        });
    }
    let mut tessera = configured(&snapshot.document)?;
    let mut previous_channels = None;
    let command = match op {
        "flows" => {
            object(v, &["flows", "expect"])?;
            Command::PutTesseraFlows {
                flows: parse_flows(&v["flows"])?,
            }
        }
        "clock" => {
            object(v, &["role", "priority", "expect"])?;
            if v.as_object()
                .expect("checked")
                .keys()
                .filter(|k| k.as_str() != "expect")
                .count()
                != 1
            {
                return Err(invalid("change one clock field at a time"));
            }
            if let Some(role) = v.get("role") {
                tessera.clock.role = serde_json::from_value(role.clone())
                    .map_err(|_| invalid("invalid clock role"))?;
            } else {
                tessera.clock.priority = integer(&v["priority"], 0, 255, "priority")? as u8;
            }
            Command::SetTessera(tessera)
        }
        "device" => {
            object(
                v,
                &[
                    "name",
                    "sample_rate",
                    "encoding",
                    "latency_us",
                    "label",
                    "expect",
                ],
            )?;
            let keys = v
                .as_object()
                .expect("checked")
                .keys()
                .filter(|k| k.as_str() != "expect")
                .collect::<Vec<_>>();
            if keys.len() != 1 {
                return Err(invalid("change one device field at a time"));
            }
            let key = keys[0].as_str();
            if key == "label" {
                let label = &v["label"];
                object(label, &["direction", "channel", "label"])?;
                return save(h,Command::SetTesseraLabel {direction:serde_json::from_value(label["direction"].clone()).map_err(|_|invalid("invalid channel label"))?,channel:integer(&label["channel"],1,65535,"channel")? as u16,label:serde_json::from_value(label["label"].clone()).map_err(|_|invalid("invalid channel label"))?},rev,v,origin).await.map(|s|json!({"ok":true,"result":s.apply.get("tessera_result").cloned().unwrap_or(json!("done"))}));
            }
            let mut device = json!(tessera.device);
            device[key] = v[key].clone();
            tessera.device = serde_json::from_value(device).map_err(|e| invalid(e.to_string()))?;
            Command::SetTessera(tessera)
        }
        "channels" => {
            object(v, &["rx", "tx", "expect"])?;
            previous_channels =
                Some(json!({"rx":tessera.device.rx_channels,"tx":tessera.device.tx_channels}));
            tessera.device.rx_channels = integer(&v["rx"], 1, 16, "rx")? as u16;
            tessera.device.tx_channels = integer(&v["tx"], 1, 16, "tx")? as u16;
            Command::SetTessera(tessera)
        }
        _ => return Err(ApiError::NotFound { what: op.into() }),
    };
    let saved = save(h, command, rev, v, origin).await?;
    let mut result = json!({"ok":true,"result":saved.apply.get("tessera_result").cloned().unwrap_or(json!("done"))});
    if op == "flows" {
        result["flows"] = flow_values(
            &h.snapshot()
                .await?
                .document
                .tessera
                .ok_or_else(|| invalid("Tessera missing after apply"))?
                .flows,
        );
    }
    if let Some(counts) = previous_channels {
        result["previous_channels"] = counts;
    }
    Ok(result)
}
async fn audio_mutate(
    h: &StateHandle,
    op: &str,
    v: &Value,
    origin: Origin,
) -> Result<Value, ApiError> {
    let s = h.snapshot().await?;
    let mut settings = ndimon_render::audio::project(&s.document.audio, s.audio_revision());
    let mut engine = s.document.audio.engine;
    match op {
        "enable" => {
            object(v, &["confirm", "expect"])?;
            if v["confirm"] != true && v.get("expect").is_none() {
                let mut impact = Impact::new(ImpactScope::Service);
                impact.restarts.push(Service::Decoder);
                impact.lines.push(
                    "Enable experimental audio engine v2; decoder video/audio restart briefly"
                        .into(),
                );
                impact = impact
                    .sealed(&Command::PutAudio(s.document.audio.clone()), &[])
                    .map_err(|e| invalid(e.to_string()))?;
                return Err(ApiError::ConfirmationRequired { impact });
            }
            let status = h.query(Query::TesseraStatus).await?;
            let device = status["status"]["devices"]
                .as_array()
                .and_then(|a| a.iter().find(|d| d["key"] == "main"))
                .ok_or_else(|| invalid("main Tessera device missing"))?;
            if device["sample_rate"] != 48000 {
                return Err(invalid(
                    "NDIMon audio engine operates at 48000Hz; set Tessera to48000Hz before enabling",
                ));
            }
            let rx = device["rx_channels"]
                .as_array()
                .map(Vec::len)
                .filter(|n| (1..=16).contains(n))
                .ok_or_else(|| invalid("invalid RX PCM channels"))?;
            let tx = device["tx_channels"]
                .as_array()
                .map(Vec::len)
                .filter(|n| (1..=16).contains(n))
                .ok_or_else(|| invalid("invalid TX PCM channels"))?;
            let input = settings["sources"]["net-in"].clone();
            let output = settings["sinks"]["net-out"].clone();
            settings["sources"]["net-in"] = json!({"type":"alsa","enabled":true,"channels":rx,"delay_ms":input["delay_ms"].as_u64().unwrap_or(0),"gain_db":input["gain_db"].as_f64().unwrap_or(0.0),"alsa_pcm":"ndimon_net_in"});
            settings["sinks"]["net-out"] = json!({"type":"alsa","enabled":true,"channels":tx,"delay_ms":0,"alsa_pcm":"ndimon_net_out","mix":output.get("mix").cloned().unwrap_or(json!({"ndi":0,"net-in":-60}))});
            if settings["sinks"]["hdmi"]["mix"].get("net-in").is_none() {
                settings["sinks"]["hdmi"]["mix"]["net-in"] = json!(0);
            }
            engine = AudioEngine::V2;
        }
        "disable" => {
            object(v, &["expect"])?;
            for (kind, id) in [("sources", "net-in"), ("sinks", "net-out")] {
                if let Some(node) = settings[kind].get_mut(id) {
                    node["enabled"] = json!(false);
                }
            }
            engine = AudioEngine::V1;
        }
        "delay" => {
            object(v, &["ms"])?;
            integer(&v["ms"], 0, 2000, "ms")?;
            if settings["sources"].get("net-in").is_none() {
                return Err(invalid("network input is not registered"));
            }
            settings["sources"]["net-in"]["delay_ms"] = v["ms"].clone();
        }
        "routing" => {
            object(v, &["net_out", "hdmi"])?;
            for (key, id) in [("net_out", "net-out"), ("hdmi", "hdmi")] {
                if let Some(mix) = v.get(key) {
                    object(mix, &["ndi", "net-in"])?;
                    if settings["sinks"].get(id).is_none() {
                        return Err(invalid("network audio is not registered"));
                    }
                    for (source, gain) in mix.as_object().expect("checked") {
                        if !gain
                            .as_f64()
                            .is_some_and(|n| n.is_finite() && (-60.0..=12.0).contains(&n))
                        {
                            return Err(invalid("gain must be -60..12dB"));
                        }
                        settings["sinks"][id]["mix"][source] = gain.clone();
                    }
                }
            }
        }
        _ => return Err(ApiError::NotFound { what: op.into() }),
    }
    let catalog = h
        .query(Query::Catalog {
            name: Catalog::AudioEndpoints,
            output: None,
        })
        .await?;
    let catalog = ndimon_state::catalog::build(
        &catalog["outputs"],
        catalog.get("tessera_status").filter(|s| !s.is_null()),
        catalog["tessera_enabled"] == true,
        &s.document.audio,
    );
    audio::ensure_legacy_editable(&s.document.audio, &catalog)?;
    let mut audio = audio::overlay(&s.document.audio, &settings)?;
    audio.engine = engine;
    let mut confirmation = v.clone();
    // The legacy disable button already explicitly selects v1. As before, it
    // is a blind-confirmed operation; the enable button requires its own click.
    if op == "disable" {
        confirmation["confirm"] = json!(true);
    }
    if let Some(token) = confirmation.get("expect").cloned() {
        confirmation["expect_impact"] = token;
    }
    let request = request(
        Command::PutAudio(audio),
        Some(s.audio_revision()),
        &confirmation,
        invalid,
    )?;
    h.submit_audio(request, origin).await?;
    Ok(match op {
        "enable" => json!({"ok":true,"audio_engine":"v2","restart_required":false}),
        "disable" => json!({"ok":true,"audio_engine":"v1"}),
        _ => json!({"ok":true}),
    })
}
#[cfg(test)]
mod tests;
