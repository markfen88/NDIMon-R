//! Legacy network URLs and their native, recoverable transaction adapter.
mod difference;
use crate::{common::*, network_limit::Limiter};
use axum::{
    Extension, Router,
    body::to_bytes,
    extract::{ConnectInfo, FromRef, Path, Request, State},
    response::Response,
    routing::{get, post, put},
};
use ndimon_state::{StateHandle, Submission, domains::network::impact};
use ndimon_types::*;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug)]
struct Receipt {
    profiles: NetworkProfiles,
    revision: u64,
    token: ImpactToken,
    expires: Instant,
}
#[derive(Debug, Default)]
struct Previews(Mutex<BTreeMap<String, Receipt>>);
impl Previews {
    fn save(&self, key: String, profiles: NetworkProfiles, revision: u64, token: ImpactToken) {
        let mut receipts = self.0.lock().unwrap_or_else(|e| e.into_inner());
        receipts.retain(|_, r| r.expires > Instant::now());
        if receipts.len() >= 1024 {
            if let Some(key) = receipts.keys().next().cloned() {
                receipts.remove(&key);
            }
        }
        receipts.insert(
            key,
            Receipt {
                profiles,
                revision,
                token,
                expires: Instant::now() + Duration::from_secs(300),
            },
        );
    }
    fn take(&self, key: &str, profiles: &NetworkProfiles, revision: u64) -> Option<ImpactToken> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key)
            .filter(|r| {
                r.expires > Instant::now() && r.profiles == *profiles && r.revision == revision
            })
            .map(|r| r.token)
    }
}
pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    StateHandle: FromRef<S>,
{
    Router::new()
        .nest(
            "/v1/Network",
            Router::new()
                .route("/handoff/{operation}", post(handoff))
                .route("/profiles", get(profiles))
                .route("/profiles/preview", post(preview))
                .route("/profiles/apply", post(apply_profiles))
                .route("/profiles/rollback", post(rollback))
                .route("/recovery/accept", post(accept_recovery))
                .route("/status", get(status))
                .route("/check", post(check))
                .route("/apply", post(apply_base))
                .route("/rollback", post(rollback))
                .route("/vlans", get(vlans))
                .route("/vlans/{id}", put(disabled_vlan).delete(disabled_remove)),
        )
        .route("/api/net-pending", post(token_operation))
        .route("/api/net-confirm", post(token_operation))
        .route("/api/net-revert", post(token_operation))
        .layer(Extension(Arc::new(Previews::default())))
        .layer(Extension(Arc::new(Limiter::default())))
}
fn invalid(field: &str, reason: impl Into<String>) -> ApiError {
    ApiError::invalid(vec![FieldError::new(field, reason)])
}
fn legacy_error(error: ApiError) -> Response {
    if let ApiError::ConfirmationRequired { impact } = error {
        return response(
            409,
            json!({"ok":false,"error":"confirmation_required","impact":impact,"message":"Confirm the displayed network impact before applying."}),
        );
    }
    response(
        error.http_status(),
        json!({"ok":false,"error":error.to_string()}),
    )
}
fn key(session: &Option<Extension<UiSession>>) -> String {
    session
        .as_ref()
        .map_or_else(|| "legacy-http".into(), |s| s.0.0.clone())
}
fn profile_doc(profiles: &NetworkProfiles, revision: u64) -> Value {
    json!({"version":1,"revision":revision,"interface":profiles.interface,"backend":profiles.backend,"networks":profiles.networks,"management":profiles.roles.management,"ndi":profiles.roles.ndi,"tessera":profiles.roles.audio})
}
fn pending(value: &Value, token: Option<&str>) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    let mut result = json!({"phase":value["phase"],"address":value["management_address"].as_str().unwrap_or(""),"profiles":true,"remaining_seconds":value["remaining_seconds"],"confirm_seconds":60,"confirm_policy":value["confirm_policy"],
        "deadline":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()+value["remaining_seconds"].as_u64().unwrap_or(0)});
    // A recovery that failed stays on the page until it finishes or the
    // operator accepts the network as it is (NI-17).
    if value["recovery_pending"] == true {
        result["recovery_pending"] = json!(true);
        if let Ok(error) = serde_json::from_value::<ApiError>(value["recovery_error"].clone()) {
            result["recovery_error"] = json!(error.to_string());
        }
    }
    if let Some(token) = token {
        result["token"] = json!(token);
    }
    result
}
pub fn project_status(value: &Value, revision: u64) -> Result<Value, ApiError> {
    let profiles: NetworkProfiles = serde_json::from_value(value["profiles"].clone())
        .map_err(crate::network::internal_error_value)?;
    let rows = profiles
        .networks
        .iter()
        .map(|row| {
            let observed = value["networks"]
                .as_array()
                .and_then(|ns| ns.iter().find(|n| n["id"] == row.id));
            let address = observed
                .and_then(|n| n["addresses"].as_array())
                .and_then(|a| a.first());
            let mut result = json!(row);
            result["interface"] = json!(ndimon_netengine::validation::ifname(&profiles, row));
            result["live_address"] = json!(address.and_then(|a| a[0].as_str()).unwrap_or(""));
            result["live_prefix"] = json!(address.and_then(|a| a[1].as_u64()).unwrap_or(0));
            result["ready"] = json!(address.is_some());
            result["used_by"] = json!(
                [
                    ("management", &profiles.roles.management),
                    ("ndi", &profiles.roles.ndi),
                    ("tessera", &profiles.roles.audio)
                ]
                .iter()
                .filter(|(_, id)| **id == row.id)
                .map(|(role, _)| *role)
                .collect::<Vec<_>>()
            );
            result
        })
        .collect::<Vec<_>>();
    let mut result = json!({"ok":true,"settings":profile_doc(&profiles,revision),"networks":rows,"pending":pending(&value["pending"],None),"confirm_policy":profiles.confirm_policy,"externally_managed":value["externally_managed"]==true,"quiesced":value["quiesced"]==true,"stale_done":value["stale_done"].as_array().cloned().unwrap_or_default()});
    // Adoption refused (NE-3): nothing is editable, but the live addressing and
    // the reason stay readable. The keys are absent on an adopted device.
    if value["refusal"].is_object() {
        result["refusal"] = value["refusal"].clone();
        result["observed"] = json!(
            value["networks"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|n| {
                    let address = n["addresses"].as_array().and_then(|a| a.first());
                    json!({
                        "interface": n["interface"],
                        "live_address": address.and_then(|a| a[0].as_str()).unwrap_or(""),
                        "live_prefix": address.and_then(|a| a[1].as_u64()).unwrap_or(0),
                    })
                })
                .collect::<Vec<_>>()
        );
    }
    Ok(result)
}
fn internal_error_value(e: impl ToString) -> ApiError {
    ApiError::Internal {
        message: e.to_string(),
    }
}
async fn read_profiles(h: &StateHandle) -> Result<Value, ApiError> {
    let value = h.query(Query::NetworkProfiles).await?;
    let revision = value["domain_revision"].as_u64().unwrap_or(0);
    project_status(&value, revision)
}
async fn profiles(State(h): State<StateHandle>) -> Response {
    match read_profiles(&h).await {
        Ok(v) => success(v),
        Err(e) => legacy_error(e),
    }
}
async fn parse_profiles(
    h: &StateHandle,
    value: &Value,
) -> Result<(u64, NetworkProfiles), ApiError> {
    let bytes = serde_json::to_vec(value).map_err(internal_error_value)?;
    let (revision, mut profiles) = ndimon_netengine::validation::import_legacy(&bytes)?;
    profiles.confirm_policy = h.snapshot().await?.document.network.confirm_policy;
    Ok((revision, profiles))
}
async fn preview_impl(
    h: &StateHandle,
    cache: &Previews,
    session: &str,
    value: &Value,
) -> Result<Value, ApiError> {
    let (revision, profiles) = parse_profiles(h, value).await?;
    let state = h.snapshot().await?;
    if revision != state.domain_revision(Domain::Network) {
        return Err(ApiError::RevisionConflict {
            current_revision: state.domain_revision(Domain::Network),
        });
    }
    let plan = h
        .query(Query::PreviewNetwork {
            profiles: profiles.clone(),
        })
        .await?;
    let command = Command::StageNetwork {
        profiles: profiles.clone(),
    };
    let sealed = impact(&plan)
        .sealed(
            &command,
            &[format!(
                "network_revision_context:{}",
                state.domain_revision(Domain::Network)
            )],
        )
        .map_err(internal_error_value)?;
    cache.save(
        session.into(),
        profiles.clone(),
        revision,
        sealed.token.clone(),
    );
    Ok(
        json!({"ok":true,"settings":profile_doc(&profiles,revision),"unchanged":plan["unchanged"],"affected":plan["affected_roles"].as_array().map(|roles|roles.iter().map(|r|if r=="audio"{json!("tessera")}else{r.clone()}).collect::<Vec<_>>()).unwrap_or_default(),"removed":plan["removed_networks"],"requires_confirmation":true,"confirm_seconds":60,"confirm_policy":profiles.confirm_policy,"message":ndimon_netengine::validation::confirm_message(profiles.confirm_policy),"impact":sealed}),
    )
}
async fn preview(
    State(h): State<StateHandle>,
    Extension(cache): Extension<Arc<Previews>>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    let value = match body(value) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match preview_impl(&h, &cache, &key(&session), &value).await {
        Ok(value) => success(value),
        Err(error) => legacy_error(error),
    }
}
fn stage_reply(submission: Submission) -> Value {
    if submission.apply["unchanged"] == true {
        return json!({"ok":true,"unchanged":true});
    }
    let mut value = pending(
        &submission.apply["pending"],
        submission.apply["token"].as_str(),
    );
    value["ok"] = json!(true);
    value
}
async fn stage(
    h: &StateHandle,
    cache: &Previews,
    identity: &str,
    value: &Value,
    origin: Origin,
    session: Option<NetworkSessionPath>,
) -> Result<Value, ApiError> {
    let (revision, profiles) = parse_profiles(h, value).await?;
    let mut request = CommandRequest::new(Command::StageNetwork {
        profiles: profiles.clone(),
    });
    request.expect_revision = Some(revision);
    request.expect_impact = cache.take(identity, &profiles, revision);
    request.network_session = session;
    h.submit_domain(request, origin, Domain::Network)
        .await
        .map(stage_reply)
}
async fn apply_profiles(
    State(h): State<StateHandle>,
    Extension(cache): Extension<Arc<Previews>>,
    session: Option<Extension<UiSession>>,
    path: Option<Extension<NetworkSessionPath>>,
    value: Body,
) -> Response {
    let value = match body(value) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match stage(
        &h,
        &cache,
        &key(&session),
        &value,
        ui(session),
        path.map(|s| s.0),
    )
    .await
    {
        Ok(value) => success(value),
        Err(error) => legacy_error(error),
    }
}
async fn rollback(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    recovery(Command::RollbackNetwork, h, session, value).await
}
/// The way out of a recovery that cannot finish (NI-17): the same confirmed
/// request as a rollback, naming what the device found before it gives up.
async fn accept_recovery(
    State(h): State<StateHandle>,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    recovery(Command::AcceptNetworkRecovery, h, session, value).await
}
async fn recovery(
    command: Command,
    h: StateHandle,
    session: Option<Extension<UiSession>>,
    value: Body,
) -> Response {
    let value = match body(value) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let mut request = CommandRequest::new(command);
    request.confirm = value["confirm"] == true;
    if let Some(token) = value["expect_impact"].as_str() {
        request.expect_impact = Some(ImpactToken::new(token));
    }
    match h.submit_domain(request, ui(session), Domain::Network).await {
        Ok(submission) => success(recovery_reply(&submission.apply)),
        Err(error) => legacy_error(error),
    }
}
/// Plain ok, unless accepting the current network could not put a file back:
/// that file still holds what the failed change wrote, and the operator hears
/// of it (NI-19 W5).
fn recovery_reply(apply: &Value) -> Value {
    let unrestored = apply["unrestored"].as_array().cloned().unwrap_or_default();
    if unrestored.is_empty() {
        return json!({"ok":true});
    }
    let warnings: Vec<String> = unrestored
        .iter()
        .map(|file| {
            format!(
                "{} could not be restored and still holds what the failed change wrote: {}",
                file["path"].as_str().unwrap_or("a file"),
                file["reason"].as_str().unwrap_or("unknown reason")
            )
        })
        .collect();
    json!({"ok":true,"warnings":warnings,"unrestored":unrestored})
}
async fn status_value(h: &StateHandle) -> Result<Value, ApiError> {
    let native = h.query(Query::NetworkStatus).await?;
    let revision = native["domain_revision"].as_u64().unwrap_or(0);
    let profiles = project_status(&native, revision)?;
    let mut value = json!({"ok":true,"applicable":native["writable"],"reason":if native["writable"]==true{""}else{"backend_not_certified"},"backend":profiles["settings"]["backend"],"interface":profiles["settings"]["interface"],"profiles":profiles["settings"],"profile_pending":profiles["pending"],"vlan_capable":native["writable"],"vlan_reason":"","vlan_error":""});
    if let Some(row) = profiles["networks"].as_array().and_then(|rows| {
        rows.iter()
            .find(|n| n["id"] == profiles["settings"]["management"])
    }) {
        for name in [
            "mode",
            "address",
            "prefix",
            "gateway",
            "dns",
            "live_address",
            "live_prefix",
            "interface",
        ] {
            value[name] = row[name].clone();
        }
        // NE-2.12a: only present when the management row has search domains.
        if row["dns_search"].is_array() {
            value["dns_search"] = row["dns_search"].clone();
        }
    }
    if profiles["refusal"].is_object() {
        value["reason"] = profiles["refusal"]["reason"].clone();
        value["refusal"] = profiles["refusal"].clone();
        if let Some(live) = profiles["observed"].get(0) {
            for name in ["interface", "live_address", "live_prefix"] {
                value[name] = live[name].clone();
            }
        }
    }
    value["vlans"]=json!(profiles["settings"]["networks"].as_array().into_iter().flatten().filter(|n|!n["vlan_id"].is_null()).map(|n|json!({"id":n["vlan_id"],"mode":n["mode"],"address":n["address"],"prefix":n["prefix"],"gateway":n["gateway"],"allow_gateway":n["id"]==profiles["settings"]["management"],"managed":true})).collect::<Vec<_>>());
    Ok(value)
}
async fn status(State(h): State<StateHandle>) -> Response {
    match status_value(&h).await {
        Ok(v) => success(v),
        Err(e) => legacy_error(e),
    }
}
async fn vlans(State(h): State<StateHandle>) -> Response {
    match status_value(&h).await {
        Ok(v) => success(
            json!({"ok":true,"vlans":v["vlans"],"vlan_capable":v["vlan_capable"],"vlan_reason":v["vlan_reason"],"vlan_error":v["vlan_error"]}),
        ),
        Err(e) => legacy_error(e),
    }
}
async fn check(State(h): State<StateHandle>, value: Body) -> Response {
    let value = match body(value) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let address = value["address"].as_str().unwrap_or("").trim();
    if address.parse::<std::net::Ipv4Addr>().is_err() {
        return response(
            400,
            json!({"ok":false,"error":"address is not a valid IPv4 address"}),
        );
    }
    match h
        .query(Query::CheckNetworkAddress {
            address: address.into(),
        })
        .await
    {
        Ok(v) => success(v),
        Err(e) => legacy_error(e),
    }
}
async fn disabled_vlan(Path(_id): Path<String>) -> Response {
    response(
        409,
        json!({"ok":false,"error":"Edit the network profiles and use Apply network settings; immediate VLAN changes are disabled."}),
    )
}
async fn disabled_remove(Path(_id): Path<String>) -> Response {
    response(
        409,
        json!({"ok":false,"error":"Reassign any services, then remove the network in Network Settings and apply the changes."}),
    )
}
async fn apply_base(
    State(h): State<StateHandle>,
    Extension(cache): Extension<Arc<Previews>>,
    session: Option<Extension<UiSession>>,
    path: Option<Extension<NetworkSessionPath>>,
    value: Body,
) -> Response {
    let value = match body(value) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let result = async {
        let clean = super::network_validation::normalize(&value)?;
        let current = status_value(&h).await?;
        if current["applicable"] == false {
            let reason = current["reason"]
                .as_str()
                .unwrap_or("not supported on this system");
            // A refusal names the key and file (NE-2.12a), as adoption does.
            return Err(ApiError::Unsupported {
                what: match current["refusal"]["detail"].as_str() {
                    Some(detail) if !detail.is_empty() => format!("{reason}: {detail}"),
                    _ => reason.into(),
                },
            });
        }
        let duplicate = if clean["mode"] == "static" && clean["address"] != current["live_address"]
        {
            h.query(Query::CheckNetworkAddress {
                address: clean["address"].as_str().unwrap_or("").into(),
            })
            .await
            .ok()
            .is_some_and(|v| v["duplicate"] == true)
        } else {
            false
        };
        let mut profiles = current["profiles"].clone();
        let management = profiles["management"].clone();
        let row = profiles["networks"]
            .as_array_mut()
            .and_then(|rows| rows.iter_mut().find(|n| n["id"] == management))
            .ok_or_else(|| invalid("network", "missing management"))?;
        for (k, v) in clean
            .as_object()
            .ok_or_else(|| invalid("settings", "settings must be an object"))?
        {
            row[k] = v.clone();
        }
        if value["dry_run"] == true {
            preview_impl(&h, &cache, &key(&session), &profiles).await?;
            let mut result =
                difference::describe(&current, &clean, path.as_ref().map(|s| s.0.peer));
            result["ok"] = json!(true);
            result["dry_run"] = json!(true);
            result["duplicate"] = json!(duplicate);
            return Ok(result);
        }
        let mut result = stage(
            &h,
            &cache,
            &key(&session),
            &profiles,
            ui(session),
            path.map(|s| s.0),
        )
        .await?;
        result["duplicate"] = json!(duplicate);
        Ok(result)
    }
    .await;
    match result {
        Ok(v) => success(v),
        Err(e) => legacy_error(e),
    }
}
async fn token_operation(
    State(h): State<StateHandle>,
    Extension(limiter): Extension<Arc<Limiter>>,
    request: Request,
) -> Response {
    let path = request.uri().path().to_owned();
    let session = request.extensions().get::<NetworkSessionPath>().copied();
    let peer = session.map(|s| s.peer).or_else(|| {
        request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|s| s.0.ip())
    });
    let Some(peer) = peer else {
        return internal_error(ApiError::PermissionDenied);
    };
    // A partial unauthenticated body must never hold the shared Keep gate.
    let bytes = match tokio::time::timeout(
        Duration::from_secs(10),
        to_bytes(request.into_body(), 65536),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        _ => return internal_error(invalid("token", "invalid_token")),
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return internal_error(invalid("token", "invalid_token")),
    };
    let _gate = match tokio::time::timeout(Duration::from_secs(2), limiter.gate.lock()).await {
        Ok(gate) => gate,
        Err(_) => return internal_error(ApiError::RateLimited),
    };
    if limiter.limited(peer, Instant::now()) {
        return internal_error(ApiError::RateLimited);
    }
    let result = async {
        let token = value["token"].as_str().unwrap_or("");
        if !ndimon_netengine::store::valid_id(token) {
            return Err(invalid("token", "invalid_token"));
        }
        if path == "/api/net-pending" {
            let status = h
                .query(Query::NetworkPending {
                    token: token.into(),
                })
                .await?;
            return Ok(json!({"ok":true,"pending":pending(&status,Some(token))}));
        }
        let command = if path == "/api/net-confirm" {
            Command::ConfirmNetwork {
                token: token.into(),
            }
        } else {
            Command::RevertNetwork {
                token: token.into(),
            }
        };
        let mut command = CommandRequest::new(command);
        command.network_session = session;
        let applied = h
            .submit_domain(command, Origin::Migration, Domain::Network)
            .await?;
        Ok(json!({"ok":true,"revision":applied.reply.revision}))
    }
    .await;
    match result {
        Ok(value) => success(value),
        Err(error) => {
            if error.counts_network_token_failure() {
                limiter.record(peer, Instant::now());
            }
            internal_error(error)
        }
    }
}

#[cfg(test)]
mod tests;

async fn handoff(
    State(h): State<StateHandle>,
    Path(operation): Path<String>,
    session: Option<Extension<UiSession>>,
    path: Option<Extension<NetworkSessionPath>>,
    value: Body,
) -> Response {
    let result = async {
        let value = value.map_err(|_| invalid("handoff", "invalid_document"))?.0;
        let fields = value.as_object().ok_or_else(||invalid("handoff","invalid_document"))?;
        let argument = match operation.as_str() {"prepare"=>"", "cancel"=>"id", "import"|"commit"=>"source", _=>""};
        if fields.keys().any(|k| k != argument && !["expect_revision","expect_impact","confirm"].contains(&k.as_str()))
            || value.get("confirm").is_some_and(|v|!v.is_boolean()) {
            return Err(invalid("handoff","invalid_document"));
        }
        let command = match operation.as_str() {
            "prepare" => Command::PrepareNetworkHandoff,
            "cancel" => Command::CancelNetworkHandoff { id: value["id"].as_str().ok_or_else(|| invalid("id", "invalid_handoff_id"))?.into() },
            "import" | "commit" => {
                let source: NetworkHandoff = serde_json::from_value(value["source"].clone()).map_err(|_| invalid("source", "invalid_document"))?;
                if operation == "import" { Command::ImportNetworkHandoff { source } } else { Command::CommitNetworkHandoff { source } }
            }
            _ => return Err(invalid("operation", "unsupported_operation")),
        };
        let mut request = CommandRequest::new(command);
        request.expect_revision = value.get("expect_revision").map(|v| v.as_u64().ok_or_else(|| invalid("expect_revision", "invalid_type"))).transpose()?;
        request.expect_impact = value.get("expect_impact").map(|v| serde_json::from_value(v.clone()).map_err(|_| invalid("expect_impact", "invalid_type"))).transpose()?;
        request.confirm = value["confirm"] == true;
        request.network_session = path.map(|p| p.0);
        let submission = h.submit_domain(request, ui(session), Domain::Network).await?;
        Ok(json!({"ok":true,"revision":submission.reply.revision,"result":submission.apply,"report":submission.reply.report}))
    }.await;
    match result {
        Ok(value) => success(value),
        Err(error) => internal_error(error),
    }
}
