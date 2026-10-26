//! Restore coordinator shared by the legacy router and typed UndoRestore.
use super::{
    legacy::{candidate, checksum, native},
    storage,
};
use crate::{StateHandle, persist::StateFile};
use base64::Engine;
use ndimon_types::*;
use serde_json::{Value, json};
fn invalid(reason: impl Into<String>) -> ApiError {
    ApiError::invalid(vec![FieldError::new("backup", reason)])
}
#[derive(Debug)]
pub struct RestoreResult {
    pub legacy: Value,
    pub reply: CommandReply,
}
const OUTPUT_KEYS: &[&str] = &[
    "SourceName",
    "SourceIP",
    "output_alias",
    "ScaleMode",
    "rotation",
    "videooutput",
    "NDIAudio",
    "TallyMode",
    "ScreenSaverMode",
    "ColorSpace",
];
const DEVICE_KEYS: &[&str] = &[
    "web_allowed_hosts",
    "watchdog_mode",
    "decode_mode",
    "audio_engine",
    "ntp_server",
    "reboot_schedule_enabled",
    "reboot_schedule_time",
    "reboot_schedule_days",
];
/// The exported output must pass the frozen schema-1 check that `inspect` runs.
/// The legacy file may spell the default scale mode `""` (the 1.3.0 beta wrote it
/// for a second output, HW-1 H2); the runtime reads that as `letterbox`, so the
/// document says so. The schema itself stays as 1.3.0 froze it.
fn exportable(mut output: Value) -> Value {
    if output["ScaleMode"] == "" {
        output["ScaleMode"] = json!("letterbox");
    }
    output
}
fn pick(v: &Value, keys: &[&str]) -> Value {
    let mut out = json!({});
    for key in keys {
        if let Some(v) = v.get(*key) {
            out[*key] = v.clone();
        }
    }
    out
}
fn rendered(state: &StateFile, path: &str) -> Result<Value, String> {
    let spec = ndimon_render::PATHS
        .iter()
        .find(|p| p.path_id == path)
        .ok_or("unknown backup artifact")?;
    serde_json::from_slice(&ndimon_render::render_path(
        spec,
        &state.document,
        state.domain_revision(spec.domain),
        &state.legacy_files,
    )?)
    .map_err(|e| e.to_string())
}
pub mod network;

pub fn native_sections(
    h: &StateHandle,
    state: &StateFile,
    network: bool,
    disabled: bool,
) -> Result<Value, ApiError> {
    let mut sections = json!({});
    if network && native(Domain::Network) && state.document.network.backend.is_some() {
        sections["network"] = self::network::capture(state)?;
    }
    if native(Domain::Presets) {
        sections["presets"] = ndimon_render::presets::list(&state.document);
    }
    if native(Domain::Audio) {
        sections["audio"] = json!({"settings":ndimon_render::audio::project(&state.document.audio,state.audio_revision())});
    }
    if native(Domain::Outputs) {
        let mut outputs = json!({});
        for ch in state.document.outputs.keys() {
            outputs[ch.to_string()] = exportable(pick(
                &rendered(state, &format!("dec{ch}")).map_err(invalid)?,
                OUTPUT_KEYS,
            ));
        }
        sections["outputs"] = outputs;
    }
    if native(Domain::Ndi) {
        sections["discovery"] = json!({"NDIDisServIP":state.document.ndi.discovery_servers.join(","),"ndi_groups":state.document.ndi.groups.join(","),"off_subnet_ips":state.document.ndi.off_subnet_sources.join(",")});
        sections["transport"] = pick(&rendered(state, "receive").map_err(invalid)?, &["Rxpm"]);
    }
    if native(Domain::Device) {
        let settings: Value = serde_json::from_slice(
            &ndimon_render::device::settings(&state.document, &state.legacy_files)
                .map_err(invalid)?,
        )
        .map_err(|e| invalid(e.to_string()))?;
        sections["device"] = pick(&settings, DEVICE_KEYS);
        sections["identity"] = json!({"ndi_recv_name":state.document.device.ndi_alias,"hostname":state.document.device.hostname});
    }
    if native(Domain::Display) {
        sections["display"]["splash"] = rendered(state, "splash").map_err(invalid)?;
        sections["display"]["osd"] = json!({"enabled":state.document.display.osd.enabled,"text":state.document.display.osd.text});
        if let Some(asset) = &state.document.display.splash.logo {
            let bytes = h
                .assets
                .read(asset)
                .map_err(|_| invalid("referenced logo asset unavailable"))?;
            sections["display"]["logo_base64"] =
                json!(base64::engine::general_purpose::STANDARD.encode(bytes));
            sections["display"]["logo_ext"] = json!(if asset.media_type == "image/png" {
                "png"
            } else {
                "jpg"
            });
        } else {
            sections["display"]["logo_base64"] = json!("");
            sections["display"]["logo_ext"] = json!("");
        }
    }
    if native(Domain::Tessera) {
        if let Some(t) = &state.document.tessera {
            if disabled
                || state
                    .document
                    .plugins
                    .get("tessera")
                    .is_some_and(|p| p.enabled)
            {
                let flows = t
                    .flows
                    .iter()
                    .map(|f| {
                        let mut v = json!({"id":f.id,"direction":f.direction,"port":f.port});
                        if f.any_source {
                            v["any_source"] = json!(true);
                        } else {
                            v["peer"] = json!(f.peer);
                        }
                        if let Some(n) = f.frames_per_packet {
                            v["frames_per_packet"] = json!(n);
                        }
                        v
                    })
                    .collect::<Vec<_>>();
                let mut data = json!({"flows":flows});
                let audio =
                    ndimon_render::audio::project(&state.document.audio, state.audio_revision());
                if audio["sources"].get("net-in").is_some()
                    && audio["sinks"].get("net-out").is_some()
                {
                    data["audio"] = json!({"sources":{"net-in":audio["sources"]["net-in"]},"sinks":{"net-out":audio["sinks"]["net-out"]}});
                }
                if !sections["plugins"].is_object() {
                    sections["plugins"] = json!({});
                }
                sections["plugins"]["tessera"] = json!({"version":state.document.plugins.get("tessera").map(|p|p.version.as_str()).unwrap_or(""),"data":data});
            }
        }
    }
    Ok(sections)
}
/// What an export that asked for the network says when it has none to give.
/// Nothing is committed to export, whatever the reason (a first adoption still
/// to be done, or one the backends refused): the backup must not silently lack
/// what was asked for.
pub fn network_export_warning(state: &StateFile, network: bool) -> Option<String> {
    (network && native(Domain::Network) && state.document.network.backend.is_none()).then(|| {
        "network: not exported, this device has no committed network configuration".to_string()
    })
}
pub async fn capture(
    h: &StateHandle,
    state: &StateFile,
    network: bool,
    password: bool,
    disabled: bool,
) -> Result<Value, ApiError> {
    let mut sections = native_sections(h, state, network, disabled)?;
    let mut warnings = Vec::new();
    let plugin_sections =
        super::plugins::capture(h.backup_plugins.as_ref(), state, disabled, &mut warnings).await;
    for (name, section) in plugin_sections.as_object().expect("plugin sections") {
        if !sections["plugins"].is_object() {
            sections["plugins"] = json!({});
        }
        sections["plugins"][name] = section.clone();
    }
    if password {
        if let Some(auth) =
            super::environment::export_auth(h.backup_io.as_ref()).map_err(crate::plan::internal)?
        {
            sections["auth"] = auth;
        }
    }
    if native(Domain::Audio) {
        sections["device"]["audio_engine"] = json!(state.document.audio.engine);
    }
    let mut ordered = json!({});
    for key in [
        "outputs",
        "presets",
        "discovery",
        "transport",
        "display",
        "audio",
        "device",
        "identity",
        "plugins",
        "network",
        "auth",
    ] {
        if let Some(v) = sections.get(key) {
            ordered[key] = v.clone();
        }
    }
    let io = h.backup_io.as_ref();
    let read = |path: &str, fallback: &str| -> Result<String, ApiError> {
        Ok(io
            .read(path)
            .ok()
            .flatten()
            .and_then(|v| String::from_utf8(v).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| fallback.into()))
    };
    let alias = ordered["identity"]["ndi_recv_name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("ndimon");
    let mut doc = json!({});
    if !warnings.is_empty() {
        doc["warnings"] = json!(warnings);
    }
    doc["format"] = json!("ndimon-backup");
    doc["schema_version"] = json!(1);
    doc["created_at"] = json!(storage::iso(std::time::SystemTime::now()));
    let commit = crate::domains::device::system::parse_build_info(&read(
        crate::domains::device::system::BUILD_INFO,
        "",
    )?)
    .0;
    let commit = if commit.is_empty() {
        read("/etc/ndimon-build-commit", "")?
    } else {
        commit
    };
    doc["app"] = json!({"firmware":read("/etc/ndimon-firmware-version","1.4.0")?,"commit":commit,"ndi_version":read("/etc/ndimon-ndi-version","6.x")?});
    doc["source_device"] = json!({"hostname":io.hostname(),"ndi_alias":alias,"platform":io.platform(),"mac_suffix":io.mac_suffix()});
    doc["sections"] = ordered;
    doc["checksum"] = json!(checksum(&doc["sections"]).map_err(invalid)?);
    Ok(doc)
}

pub async fn restore(
    h: &StateHandle,
    doc: &Value,
    selected: &[String],
    network: bool,
    confirmation: &Value,
    origin: Origin,
    connection: Option<NetworkSessionPath>,
) -> Result<RestoreResult, ApiError> {
    let state = h.snapshot().await?;
    if network && !native(Domain::Network) && doc["sections"].get("network").is_some() {
        return Err(ApiError::Unsupported {
            what: "native network backup restore requires the native network adapter".into(),
        });
    }
    let network_profiles = if native(Domain::Network) {
        let mut proposed = None;
        if state.document.network.backend.is_some() {
            let status = h.query(Query::NetworkStatus).await?;
            if !status["pending"].is_null() {
                return Err(ApiError::Locked {
                    field: "network".into(),
                    by: Some("pending_revision".into()),
                });
            }
        } else if network && !doc["sections"]["network"].is_null() {
            // Nothing committed yet: a change starts from the profile the
            // Network page proposes, as the review compared against it.
            proposed = self::network::proposal(&h.query(Query::NetworkStatus).await?);
        }
        self::network::candidate(
            &state,
            &doc["sections"]["network"],
            network,
            proposed.as_ref(),
        )?
    } else {
        None
    };
    if let Some(profiles) = &network_profiles {
        // Refuse invalid/unavailable backends before any restore write.
        let preview = h
            .query(Query::PreviewNetwork {
                profiles: profiles.clone(),
            })
            .await?;
        if preview["writable"] != true && preview["unchanged"] != true {
            return Err(ApiError::Unsupported {
                what: "backend_not_certified".into(),
            });
        }
    }
    let mut pins = Vec::new();
    let mut document = candidate(&state, &doc["sections"], selected, &h.assets, &mut pins)?;
    if let Some(profiles) = &network_profiles {
        document.document.network = profiles.clone();
        document.legacy_sections.push("network".into());
    }
    let preview = h
        .query(ndimon_types::Query::InspectBackup {
            document: Box::new(document.clone()),
        })
        .await?;
    let impact: Impact =
        serde_json::from_value(preview["impact"].clone()).map_err(|e| invalid(e.to_string()))?;
    let command = Command::ImportBackup {
        document: Box::new(document.clone()),
        scope: RestoreScope::All,
    };
    let mut request = CommandRequest::new(command);
    let expected = confirmation["expect_revision"]
        .as_u64()
        .unwrap_or(state.revision);
    if expected != state.revision {
        return Err(ApiError::RevisionConflict {
            current_revision: state.revision,
        });
    }
    request.expect_revision = Some(expected);
    request.confirm = confirmation["confirm"] == true;
    if let Some(token) = confirmation.get("expect_impact").filter(|v| !v.is_null()) {
        request.expect_impact =
            Some(serde_json::from_value(token.clone()).map_err(|e| invalid(e.to_string()))?);
    }
    if (request
        .expect_impact
        .as_ref()
        .is_some_and(|t| t != &impact.token))
        || (impact.needs_confirmation()
            && origin.requires_confirmation()
            && !request.confirm
            && request.expect_impact.is_none())
    {
        return Err(ApiError::ConfirmationRequired { impact });
    }
    let writing = selected.iter().any(|k| doc["sections"].get(k).is_some())
        || network
            && doc["sections"]["network"]["mode"]
                .as_str()
                .is_some_and(|v| v != "unknown");
    if !writing {
        return Ok(RestoreResult {
            legacy: json!({"ok":true,"snapshot":null,"results":{},"network":null,"relogin":false,"noop":true}),
            reply: CommandReply::new(state.revision, ApplyReport::default()),
        });
    }
    let previous = capture(h, &state, true, true, true).await?;
    let name = format!(
        "pre-restore-{}-{}.json",
        storage::stamp(),
        storage::random(3).map_err(invalid)?
    );
    let backups = h.backup_root().join("backups");
    storage::write(&backups.join(&name), &previous).map_err(invalid)?;
    storage::prune(&backups);
    request.expect_impact = Some(impact.token);
    let mut reply = h.send(request, origin.clone(), None).await?.reply;
    let followed: Vec<_> = selected
        .iter()
        .filter(|k| document.legacy_sections.contains(k))
        .cloned()
        .collect();
    let mut results = json!({});
    for key in selected {
        if !followed.contains(key) && doc["sections"].get(key).is_some() {
            results[key] = if key == "audio" {
                json!("restored")
            } else {
                json!({"ok":true})
            };
        }
    }
    let mut out = json!({"results":{},"network":null,"relogin":false});
    for key in ["auth", "plugins"] {
        if !followed.iter().any(|k| k == key) || doc["sections"].get(key).is_none() {
            continue;
        }
        match key {
            "auth" if doc["sections"].get("auth").is_some() => {
                let record = super::environment::import_auth(&doc["sections"]["auth"])
                    .map_err(crate::plan::internal)?;
                h.backup_io
                    .import_auth(record)
                    .await
                    .map_err(crate::plan::internal)?;
                out["results"]["auth"] = json!({"ok":true,"relogin":true});
                out["relogin"] = json!(true);
            }
            "plugins" => {
                out["results"]["plugins"] = super::plugins::apply(
                    h.backup_plugins.as_ref(),
                    &state,
                    &doc["sections"]["plugins"],
                )
                .await
            }
            _ => return Err(invalid("backup section has no native restore adapter")),
        }
    }
    if let Some(items) = out["results"].as_object() {
        for (k, v) in items {
            results[k] = v.clone();
        }
    }
    if native(Domain::Tessera)
        && selected.iter().any(|k| k == "plugins")
        && doc["sections"]["plugins"].get("tessera").is_some()
    {
        if !results["plugins"].is_object() {
            results["plugins"] = json!({"ok":true,"plugins":{}});
        }
        results["plugins"]["plugins"]["tessera"] = json!({"ok":true});
    }
    if let Some(profiles) = network_profiles {
        let current = h.snapshot().await?;
        let mut stage = CommandRequest::new(Command::StageNetwork { profiles });
        stage.expect_revision = Some(current.revision);
        stage.network_session = connection;
        stage.confirm = true; // The aggregate restore impact was accepted above.
        match h.send(stage, origin, None).await {
            Ok(submission) if submission.apply["unchanged"] == true => {
                results["network"] = json!({"ok":true,"skipped":"already matches"});
            }
            Ok(submission) => {
                out["network"] = self::network::pending(&submission.apply);
                results["network"] = json!({"ok":true,"pending":true});
            }
            Err(error) => {
                results["network"] = json!({"ok":false,"error":error.to_string()});
            }
        }
    }
    let mut ordered = json!({});
    for key in [
        "audio",
        "outputs",
        "discovery",
        "transport",
        "display",
        "device",
        "identity",
        "presets",
        "auth",
        "services",
        "plugins",
        "network",
    ] {
        if let Some(value) = results.get(key) {
            ordered[key] = value.clone();
        }
    }
    out = json!({"ok":true,"snapshot":name,"results":ordered,"network":out["network"],"relogin":out["relogin"]});
    // V2 plugin hooks commit through the same actor after the settings import.
    reply.revision = h.snapshot().await?.revision;
    Ok(RestoreResult { legacy: out, reply })
}

pub async fn undo(
    h: &StateHandle,
    request: CommandRequest,
    origin: Origin,
) -> Result<RestoreResult, ApiError> {
    let _lock = h.lock_backup().await;
    let Command::UndoRestore { snapshot } = request.command else {
        return Err(invalid("not an undo command"));
    };
    let current = h.snapshot().await?;
    if request
        .expect_revision
        .is_some_and(|r| r != current.revision)
    {
        return Err(ApiError::RevisionConflict {
            current_revision: current.revision,
        });
    }
    let dir = h.backup_root().join("backups");
    if !snapshot.starts_with("pre-restore-")
        || !snapshot.ends_with(".json")
        || snapshot.contains('/')
        || snapshot.contains("..")
    {
        return Err(invalid("no snapshot"));
    }
    let doc = storage::read(&dir.join(&snapshot)).map_err(invalid)?;
    let requester = request
        .network_session
        .map_or_else(String::new, |p| p.peer.to_string());
    let reviewed = super::review::review(h, doc, &requester)
        .await
        .map_err(invalid)?;
    let selected = reviewed["document"]["sections"]
        .as_object()
        .ok_or_else(|| invalid("snapshot has no sections"))?
        .keys()
        .filter(|k| k.as_str() != "network")
        .cloned()
        .collect::<Vec<_>>();
    let confirmation = json!({"confirm":request.confirm,"expect_impact":request.expect_impact,"expect_revision":request.expect_revision});
    let mut out = restore(
        h,
        &reviewed["document"],
        &selected,
        reviewed["network"]["needs_prompt"] == true,
        &confirmation,
        origin,
        request.network_session,
    )
    .await?;
    out.legacy["restored"] = json!(snapshot);
    Ok(out)
}
