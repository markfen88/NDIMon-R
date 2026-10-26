use crate::{
    domains::DomainModule,
    persist::{StateFile, err},
};
use ndimon_render::{LegacyFiles, PATHS};
use ndimon_types::*;
use serde_json::{Value, json};
use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::Path};

pub const EXTRA_INPUTS: &[&str] = &[
    "/etc/hostname",
    "/var/lib/tessera/state.toml",
    "/etc/tessera/config.d/90-probe-flows.toml",
    "/etc/alsa/conf.d/60-ndimon-tessera.conf",
    "/etc/alsa/conf.d/61-ndimon-tessera-ranges.conf",
];
pub fn read_files(root: &Path, keep_originals: bool) -> Result<LegacyFiles, String> {
    read_files_with_fallback(root, keep_originals, None)
}
/// P3's closed protocol has no privileged read operation. Until integration
/// supplies a privileged observation channel, keep inaccessible followed inputs
/// at their root-imported values while still following accessible legacy files.
pub fn read_files_with_fallback(
    root: &Path,
    keep_originals: bool,
    previous: Option<&LegacyFiles>,
) -> Result<LegacyFiles, String> {
    let mut files = LegacyFiles::new();
    for name in PATHS
        .iter()
        .map(|p| p.path)
        .chain(EXTRA_INPUTS.iter().copied())
    {
        if files.contains_key(name) {
            continue;
        }
        let path = root.join(name.trim_start_matches('/'));
        let bytes = match fs::read(&path) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && previous.is_some() => {
                if let Some(raw) = previous.and_then(|p| p.get(name)) {
                    files.insert(name.into(), raw.clone());
                }
                continue;
            }
            Err(e) => return Err(format!("{name}: {e}")),
        };
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(format!("{name}: input exceeds 4 MiB"));
        }
        if keep_originals {
            let backup = path.with_file_name(format!(
                "{}.pre-control",
                path.file_name().ok_or("input basename")?.to_string_lossy()
            ));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o640)
                .open(&backup)
            {
                Ok(mut f) => {
                    f.write_all(&bytes)
                        .and_then(|()| f.sync_all())
                        .map_err(err)?;
                    fs::File::open(path.parent().ok_or("input parent")?)
                        .and_then(|f| f.sync_all())
                        .map_err(err)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(format!("preserve {name}: {e}")),
            }
        }
        files.insert(name.into(), String::from_utf8(bytes).map_err(err)?);
    }
    Ok(files)
}
pub fn import(files: LegacyFiles) -> Result<StateFile, String> {
    let mut document = DesiredState::default();
    for renderer in ndimon_render::REGISTRY {
        let registration = crate::domains::get(renderer.domain).map_err(err)?;
        registration.module.import(&mut document, &files)?;
    }
    // Do not adopt unrelated legacy revision counters as the document CAS.
    document.revision = 0;
    let state = StateFile::new(document, files);
    state.check()?;
    Ok(state)
}
/// A domain the first import left at its defaults, and why. The legacy file it
/// came from is kept as `<file>.pre-control` next to the original.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotImported {
    pub domain: &'static str,
    pub reason: String,
}
impl std::fmt::Display for NotImported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.domain, self.reason)
    }
}
/// The import of an upgrade. Content the importer refuses costs its domain, not
/// the device: the stack is stopped for the migration, and a refused domain
/// that failed the whole import left the appliance dark (HW-1 H1, a Tessera
/// 0.1.0 config without a device name). A domain whose import fails, or whose
/// imported values do not validate, is left at its defaults and reported; the
/// others are imported as in `import`.
pub fn import_tolerant(files: LegacyFiles) -> Result<(StateFile, Vec<NotImported>), String> {
    let mut document = DesiredState::default();
    let mut skipped = Vec::new();
    for (name, import) in IMPORTERS {
        // Import into a copy: a refused domain may have written part of itself.
        let mut candidate = document.clone();
        match import(&mut candidate, &files) {
            Ok(()) => document = candidate,
            Err(reason) => skipped.push(NotImported {
                domain: name,
                reason,
            }),
        }
    }
    document.revision = 0;
    loop {
        let errors = match ndimon_types::validate(&document) {
            Ok(()) => break,
            Err(errors) => errors,
        };
        // The first path segment names the domain the value belongs to.
        let mut reset = false;
        for error in &errors {
            let top = error.field.split(['.', '[']).next().unwrap_or_default();
            let Some((name, _)) = IMPORTERS.iter().find(|(name, _)| *name == top) else {
                continue;
            };
            if skipped.iter().any(|s| s.domain == *name) {
                continue;
            }
            let mut value = serde_json::to_value(&document).map_err(err)?;
            let defaults = serde_json::to_value(DesiredState::default()).map_err(err)?;
            value[*name] = defaults[*name].clone();
            document = serde_json::from_value(value).map_err(err)?;
            skipped.push(NotImported {
                domain: name,
                reason: format!("{error}; its values were not imported"),
            });
            reset = true;
        }
        if !reset {
            // Not a value of one domain (or a default that does not validate).
            break;
        }
    }
    let state = StateFile::new(document, files);
    state.check()?;
    Ok((state, skipped))
}

type Importer = fn(&mut DesiredState, &LegacyFiles) -> Result<(), String>;
/// The domains in the renderer registry's order, each imported through its concrete module type.
/// The importer binary stays small only while nothing in it reaches the registry's
/// trait objects: a call through them keeps the whole vtable of every domain
/// (the apply, replay and rollback code with it, 8 MB), which the strict `import`
/// avoids because the compiler resolves its loop at compile time.
const IMPORTERS: &[(&str, Importer)] = &[
    ("device", |d, f| {
        <crate::domains::device::Module as DomainModule>::import(
            &crate::domains::device::Module,
            d,
            f,
        )
    }),
    ("outputs", |d, f| {
        <crate::domains::outputs::Module as DomainModule>::import(
            &crate::domains::outputs::Module,
            d,
            f,
        )
    }),
    ("ndi", |d, f| {
        <crate::domains::ndi::Module as DomainModule>::import(&crate::domains::ndi::Module, d, f)
    }),
    ("audio", |d, f| {
        <crate::domains::audio::Module as DomainModule>::import(
            &crate::domains::audio::Module,
            d,
            f,
        )
    }),
    ("display", |d, f| {
        <crate::domains::display::Module as DomainModule>::import(
            &crate::domains::display::Module,
            d,
            f,
        )
    }),
    ("presets", |d, f| {
        <crate::domains::presets::Module as DomainModule>::import(
            &crate::domains::presets::Module,
            d,
            f,
        )
    }),
    ("plugins", |d, f| {
        <crate::domains::plugins::Module as DomainModule>::import(
            &crate::domains::plugins::Module,
            d,
            f,
        )
    }),
    ("network", |d, f| {
        <crate::domains::network::Module as DomainModule>::import(
            &crate::domains::network::Module,
            d,
            f,
        )
    }),
    ("tessera", |d, f| {
        <crate::domains::tessera::Module as DomainModule>::import(
            &crate::domains::tessera::Module,
            d,
            f,
        )
    }),
];

/// Compatibility entry point for callers that retain only the native audio domain.
pub fn followed(files: LegacyFiles, audio: Audio) -> Result<StateFile, String> {
    let mut previous = DesiredState::default();
    previous.audio = audio;
    followed_document(files, &previous)
}
pub fn followed_document(files: LegacyFiles, previous: &DesiredState) -> Result<StateFile, String> {
    let mut document = previous.clone();
    for renderer in ndimon_render::REGISTRY {
        let registration = crate::domains::get(renderer.domain).map_err(err)?;
        if registration.ownership() == ndimon_render::Ownership::Followed {
            // Start each imported domain at its defaults; deleted legacy keys
            // must not retain a previous non-default value.
            let mut imported = DesiredState::default();
            imported.plugins = document.plugins.clone();
            registration.module.import(&mut imported, &files)?;
            (registration.copy)(&mut document, &imported);
        }
    }
    Ok(StateFile::new(document, files))
}
fn toml_json(raw: &str) -> Result<Value, String> {
    let value: toml::Value = toml::from_str(raw).map_err(err)?;
    serde_json::to_value(value).map_err(err)
}
/// Tessera's default device name is the host name (`config.toml`: "Default: the
/// host name"). The model holds 1..=31 bytes without controls: cut at a
/// character boundary, never fail on the length of a host name.
fn default_device_name(files: &LegacyFiles) -> String {
    let host = files
        .get("/etc/hostname")
        .map(|raw| raw.lines().next().unwrap_or_default().trim())
        .unwrap_or_default();
    let mut name = String::new();
    for c in host.chars().filter(|c| !c.is_control()) {
        if name.len() + c.len_utf8() > 31 {
            break;
        }
        name.push(c);
    }
    if name.is_empty() {
        "NDIMON".into()
    } else {
        name
    }
}
pub(crate) fn import_tessera(
    document: &mut DesiredState,
    files: &LegacyFiles,
) -> Result<(), String> {
    let Some(raw) = files.get("/etc/tessera/config.d/60-ndimon.toml") else {
        return Ok(());
    };
    let config = toml_json(raw)?;
    let device = config
        .get("device")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().find(|v| v["key"] == "main" || v["id"] == "main"))
        .or_else(|| config.pointer("/devices/main"))
        .ok_or("Tessera main device missing; cannot guess runtime name or latency")?;
    let runtime = files
        .get("/var/lib/tessera/state.toml")
        .map(|v| toml_json(v))
        .transpose()?
        .unwrap_or(json!({}));
    let effective = runtime
        .pointer("/device/main")
        .or_else(|| runtime.pointer("/devices/main"))
        .unwrap_or(&Value::Null)
        .clone();
    let locked = device.get("locked").and_then(Value::as_array);
    let get = |key: &str| {
        let lock_key = match key {
            "latency_us" => "latency",
            "rx_labels" => "rx_channel_labels",
            "tx_labels" => "tx_channel_labels",
            other => other,
        };
        if locked.is_some_and(|keys| keys.iter().any(|v| v == lock_key)) {
            device.get(key)
        } else {
            effective.get(key).or_else(|| device.get(key))
        }
    };
    // Tessera 0.1.0 never persisted a device name when it was not changed; the
    // daemon's own default is the host name (HW-1 H1). Say what was done.
    let name = match get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
    {
        Some(name) => name.to_owned(),
        None => {
            let name = default_device_name(files);
            eprintln!(
                "state migration: the Tessera device has no name in its configuration; using the host name {name:?}, which is Tessera's own default"
            );
            name
        }
    };
    let latency = get("latency_us")
        .or_else(|| get("latency"))
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or("Tessera effective latency must be imported")?;
    let mut tessera = Tessera::new(name, latency);
    tessera.protocol_flow_lock = config
        .pointer("/control/locked")
        .and_then(Value::as_array)
        .is_some_and(|keys| keys.iter().any(|v| v == "flows"));
    tessera.enabled = document.plugins.get("tessera").is_some_and(|p| p.enabled);
    let mut d = serde_json::to_value(&tessera.device).map_err(err)?;
    for key in [
        "sample_rate",
        "encoding",
        "rx_channels",
        "tx_channels",
        "rx_labels",
        "tx_labels",
    ] {
        if let Some(v) = get(key) {
            d[key] = v.clone();
        }
    }
    for (old, new) in [
        ("rx_channel_labels", "rx_labels"),
        ("tx_channel_labels", "tx_labels"),
    ] {
        if let Some(v) = get(old) {
            d[new] = v.clone();
        }
    }
    if let Some(v) = config.pointer("/discovery/advertise_devices") {
        d["advertise_devices"] = v.clone();
    }
    tessera.device = serde_json::from_value(d).map_err(err)?;
    let mut clock = serde_json::to_value(&tessera.clock).map_err(err)?;
    for key in ["role", "priority"] {
        let clock_locked = config
            .pointer("/clock/locked")
            .and_then(Value::as_array)
            .is_some_and(|keys| keys.iter().any(|v| v == key));
        if let Some(v) = (!clock_locked)
            .then(|| runtime.get("clock").and_then(|c| c.get(key)))
            .flatten()
            .or_else(|| config.get("clock").and_then(|c| c.get(key)))
        {
            clock[key] = v.clone();
        }
    }
    tessera.clock = serde_json::from_value(clock).map_err(err)?;
    for path in [
        "/etc/tessera/config.d/61-ndimon-flows.toml",
        "/etc/tessera/config.d/90-probe-flows.toml",
    ] {
        let Some(raw) = files.get(path) else {
            continue;
        };
        let v = toml_json(raw)?;
        if let Some(flows) = v["flow"].as_array() {
            for flow in flows {
                if flow.get("device").is_some_and(|v| v != "main")
                    || flow
                        .get("channels")
                        .and_then(Value::as_array)
                        .is_some_and(|v| !v.is_empty())
                {
                    return Err("Tessera flow cannot be represented as main/all channels".into());
                }
                let mut value = json!({"id":flow["id"],"direction":flow["direction"],"peer":null,"port":flow["port"],"any_source":false,"frames_per_packet":null});
                for key in ["peer", "any_source", "frames_per_packet"] {
                    if let Some(v) = flow.get(key) {
                        value[key] = v.clone();
                    }
                }
                let f: TesseraFlow = serde_json::from_value(value).map_err(err)?;
                if let Some(existing) = tessera.flows.iter().find(|x| {
                    x.id == f.id
                        || (x.direction == f.direction && x.peer == f.peer && x.port == f.port)
                }) {
                    if existing != &f {
                        return Err("conflicting controller/plugin flow during import".into());
                    }
                } else {
                    tessera.flows.push(f);
                }
            }
        }
    }
    document.tessera = Some(tessera);
    Ok(())
}

/// Report names, never values. Unknown fields remain in the exact source text,
/// including fields deliberately outside P2's native audio ownership.
pub fn warnings(files: &LegacyFiles) -> Vec<String> {
    let mut warnings = Vec::new();
    for (path, raw) in files {
        let keys: &[&str] = match path.as_str() {
            "/etc/ndimon-device-settings.json" => &[
                "mode",
                "videooutput",
                "ndi_recv_name",
                "audio_engine",
                "decode_mode",
                "watchdog_mode",
                "ntp_server",
                "reboot_schedule_enabled",
                "reboot_schedule_time",
                "reboot_schedule_days",
                "AnalogAudioInGain",
                "AnalogAudioOutGain",
                "plugin_feeds",
                "allow_insecure_feeds",
                "web_allowed_hosts",
            ],
            "/etc/ndimon-about-settings.json" => {
                &["GroupName", "MACAddress", "device_name", "host_name"]
            }
            "/etc/ndimon-audio-settings.json" => &[
                "version",
                "revision",
                "engine",
                "sources",
                "sinks",
                "outputs",
                "routes",
                "video_delay_ms",
            ],
            "/etc/ndimon-rx-settings.json" => &["Rxpm", "Rxpm_user_set"],
            "/etc/ndimon-find-settings.json" => &["NDIDisServ", "NDIDisServIP"],
            "/etc/ndi-group.json" => &["ndi_groups"],
            "/etc/ndimon-presets.json" => &["presets"],
            "/etc/ndimon-osd-settings.json" => &["enabled", "text"],
            "/etc/ndimon-splash-settings.json" => &[
                "bg_idle",
                "bg_live",
                "accent_idle",
                "accent_live",
                "logo_path",
                "logo_x_pct",
                "logo_y_pct",
                "logo_w_pct",
                "text_idle",
                "text_live",
                "text_height_pct",
                "backdrop",
                "show_signal_text",
                "show_device_name",
                "show_device_url",
                "show_sources_available",
                "show_box",
            ],
            "/var/lib/ndimon-network/committed.json" => &[
                "version",
                "revision",
                "interface",
                "backend",
                "networks",
                "management",
                "ndi",
                "tessera",
                "confirm_policy",
            ],
            path if path.starts_with("/etc/ndimon-dec") && path.ends_with("-settings.json") => &[
                "ChNum",
                "SourceName",
                "SourceIP",
                "SourceSelection",
                "videooutput",
                "ScaleMode",
                "rotation",
                "output_alias",
                "NDIAudio",
                "ScreenSaverMode",
                "TallyMode",
                "ColorSpace",
            ],
            _ => continue,
        };
        if let Ok(Value::Object(values)) = serde_json::from_str::<Value>(raw) {
            for key in values.keys() {
                if !keys.contains(&key.as_str()) {
                    warnings.push(format!("{path}: unknown key {key}"));
                }
            }
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_tolerant_importer_covers_the_registry_in_order() {
        let registry: Vec<_> = ndimon_render::REGISTRY
            .iter()
            .map(|renderer| crate::domains::get(renderer.domain).unwrap().name)
            .collect();
        let importers: Vec<_> = IMPORTERS.iter().map(|(name, _)| *name).collect();
        assert_eq!(importers, registry);
    }
    #[test]
    fn the_default_device_name_is_cut_on_a_character_boundary() {
        let name = |host: &str| {
            default_device_name(&LegacyFiles::from([("/etc/hostname".into(), host.into())]))
        };
        assert_eq!(name("studio-1\n"), "studio-1");
        // 30 bytes and a two-byte character: the 31-byte limit falls inside it.
        assert_eq!(name(&format!("{}é\n", "a".repeat(30))), "a".repeat(30));
        assert_eq!(
            name(&format!("{}é\n", "a".repeat(29))),
            format!("{}é", "a".repeat(29))
        );
        assert_eq!(name("\n"), "NDIMON");
        assert_eq!(name("a\u{7}b\n"), "ab");
        assert_eq!(default_device_name(&LegacyFiles::new()), "NDIMON");
    }
}
