//! Legacy schema-1 section conversion shared by router and typed undo.
use crate::assets::{AssetStore, StoredAsset};
use crate::persist::StateFile;
use base64::Engine;
use ndimon_types::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
fn invalid(reason: impl Into<String>) -> ApiError {
    ApiError::invalid(vec![FieldError::new("backup", reason)])
}
pub fn native(domain: Domain) -> bool {
    ndimon_render::ownership(domain) == ndimon_render::Ownership::Native
}
pub fn section_domain(key: &str) -> Option<Domain> {
    match key {
        "presets" => Some(Domain::Presets),
        "audio" => Some(Domain::Audio),
        "outputs" => Some(Domain::Outputs),
        "discovery" | "transport" => Some(Domain::Ndi),
        "device" | "identity" => Some(Domain::Device),
        "display" => Some(Domain::Display),
        _ => None,
    }
}
pub fn canonical(value: &Value) -> Result<String, String> {
    Ok(match value {
        Value::Object(m) => {
            let mut keys: Vec<_> = m.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            let mut entries = Vec::new();
            for k in keys {
                entries.push(format!(
                    "{}:{}",
                    serde_json::to_string(k).map_err(|e| e.to_string())?,
                    canonical(&m[k])?
                ));
            }
            format!("{{{}}}", entries.join(","))
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter()
                .map(canonical)
                .collect::<Result<Vec<_>, _>>()?
                .join(",")
        ),
        Value::Number(n) => ryu_js::Buffer::new()
            .format_finite(n.as_f64().ok_or("invalid number")?)
            .to_string(),
        _ => String::from_utf8(ndimon_render::node_json(value.clone())?)
            .map_err(|e| e.to_string())?,
    })
}
pub fn checksum(sections: &Value) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(canonical(sections)?.as_bytes())
    ))
}
fn merge(files: &mut ndimon_render::LegacyFiles, path: &str, patch: &Value) -> Result<(), String> {
    let mut value = files
        .get(path)
        .map(|s| serde_json::from_str::<Value>(s))
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or(json!({}));
    if !value.is_object() {
        value = json!({});
    }
    for (k, v) in patch.as_object().ok_or("section must be object")? {
        value[k] = v.clone();
    }
    files.insert(path.into(), value.to_string());
    Ok(())
}
pub fn candidate(
    state: &StateFile,
    sections: &Value,
    selected: &[String],
    assets: &AssetStore,
    pins: &mut Vec<StoredAsset>,
) -> Result<BackupDocument, ApiError> {
    let result = (|| -> Result<BackupDocument, String> {
        let mut next = state.document.clone();
        let mut files = state.legacy_files.clone();
        // Seed complete current projections before applying partial legacy patches.
        for spec in ndimon_render::PATHS {
            if native(spec.domain) && spec.path.ends_with(".json") {
                if let Ok(bytes) = ndimon_render::render_path(
                    spec,
                    &state.document,
                    state.domain_revision(spec.domain),
                    &files,
                ) {
                    files.insert(
                        spec.path.into(),
                        String::from_utf8(bytes).map_err(|e| e.to_string())?,
                    );
                }
            }
        }
        files.insert(
            "/etc/ndimon-device-settings.json".into(),
            String::from_utf8(ndimon_render::device::settings(&state.document, &files)?)
                .map_err(|e| e.to_string())?,
        );
        let want = |key: &str| selected.iter().any(|k| k == key) && sections.get(key).is_some();
        if want("audio") {
            next.audio =
                crate::audio::overlay(&state.document.audio, &sections["audio"]["settings"])
                    .map_err(|e| e.to_string())?;
        }
        if want("device") {
            if let Some(engine) = sections["device"].get("audio_engine") {
                next.audio.engine =
                    serde_json::from_value(engine.clone()).map_err(|e| e.to_string())?;
            }
            merge(
                &mut files,
                "/etc/ndimon-device-settings.json",
                &sections["device"],
            )?;
        }
        if want("presets") {
            files.insert(
                "/etc/ndimon-presets.json".into(),
                sections["presets"].to_string(),
            );
            ndimon_render::presets::import_json(&mut next, &files)?;
        }
        if want("outputs") {
            for (ch, v) in sections["outputs"]
                .as_object()
                .ok_or("outputs must be object")?
            {
                merge(&mut files, &format!("/etc/ndimon-dec{ch}-settings.json"), v)?;
            }
            ndimon_render::outputs::import_json(&mut next, &files)?;
            if !want("audio") {
                for (ch, item) in sections["outputs"]
                    .as_object()
                    .ok_or("outputs must be object")?
                {
                    if let Some(enabled) = item["NDIAudio"].as_str() {
                        let id: u8 = ch.parse().map_err(|_| "invalid output")?;
                        next.audio.outputs.entry(id).or_default().enabled = enabled == "NDIAudioEn";
                    }
                }
            }
        }
        if want("discovery") {
            let d = &sections["discovery"];
            merge(
                &mut files,
                "/etc/ndimon-find-settings.json",
                &json!({"NDIDisServIP":d["NDIDisServIP"].as_str().unwrap_or("")}),
            )?;
            if let Some(g) = d.get("ndi_groups") {
                merge(&mut files, "/etc/ndi-group.json", &json!({"ndi_groups":g}))?;
            }
            if let Some(v) = d.get("off_subnet_ips") {
                files.insert("/etc/ndi-config.json".into(), v.to_string());
            }
        }
        if want("transport") {
            merge(
                &mut files,
                "/etc/ndimon-rx-settings.json",
                &json!({"Rxpm":sections["transport"]["Rxpm"],"Rxpm_user_set":true}),
            )?;
        }
        if want("transport") || want("discovery") || want("outputs") {
            ndimon_render::ndi::import_json(&mut next, &files)?;
        }
        if want("identity") {
            let d = &sections["identity"];
            merge(
                &mut files,
                "/etc/ndimon-device-settings.json",
                &json!({"ndi_recv_name":d["ndi_recv_name"].as_str().unwrap_or("").trim()}),
            )?;
            let raw = d["ndi_recv_name"].as_str().unwrap_or("");
            if !raw.trim().is_empty() {
                files.insert("/etc/hostname".into(), alias_hostname(raw));
            }
        }
        if want("device") || want("identity") {
            ndimon_render::device::import_json(&mut next, &files)?;
        }
        if want("display") {
            let d = &sections["display"];
            if let Some(s) = d.get("splash") {
                let mut s = s.clone();
                s.as_object_mut()
                    .ok_or("splash must be object")?
                    .remove("logo_path");
                merge(&mut files, "/etc/ndimon-splash-settings.json", &s)?;
            }
            if let Some(s) = d.get("osd") {
                merge(&mut files, "/etc/ndimon-osd-settings.json", s)?;
            }
            let previous_logo = next.display.splash.logo.clone();
            ndimon_render::display::import_json(&mut next, &files)?;
            next.display.splash.logo = previous_logo;
            if native(Domain::Display) {
                let logo = if d["splash"]["logo_path"] == "" {
                    Some("")
                } else {
                    d["logo_base64"].as_str()
                };
                if let Some(logo) = logo {
                    if logo.is_empty() {
                        next.display.splash.logo = None;
                    } else {
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(logo)
                            .map_err(|_| "logo is not base64")?;
                        let stored = assets.put(&bytes)?;
                        next.display.splash.logo = Some(stored.asset.clone());
                        pins.push(stored);
                    }
                }
            }
        }
        if want("plugins") && native(Domain::Tessera) {
            if let Some(section) = sections["plugins"].get("tessera") {
                let data = &section["data"];
                let tessera = next.tessera.as_mut().ok_or("Tessera is not configured")?;
                let mut flows = data["flows"]
                    .as_array()
                    .ok_or("flows must be an array")?
                    .clone();
                for flow in &mut flows {
                    if flow.get("any_source").is_none() {
                        flow["any_source"] = json!(false);
                    }
                }
                tessera.flows = serde_json::from_value(json!(flows)).map_err(|e| e.to_string())?;
                if let Some(audio) = data.get("audio") {
                    let mut settings =
                        ndimon_render::audio::project(&next.audio, state.audio_revision());
                    for key in ["sources", "sinks"] {
                        if let Some(nodes) = audio[key].as_object() {
                            for (id, node) in nodes {
                                settings[key][id] = node.clone();
                            }
                        }
                    }
                    next.audio =
                        crate::audio::overlay(&next.audio, &settings).map_err(|e| e.to_string())?;
                }
            }
        }
        // Followed domains are deliberately retained; only the adapter may write them.
        for r in crate::domains::REGISTRY {
            if r.ownership() != ndimon_render::Ownership::Native {
                (r.copy)(&mut next, &state.document);
            }
        }
        ndimon_types::validate(&next).map_err(|e| {
            e.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        })?;
        let mut backup = BackupDocument::new(next);
        if let Some(asset) = &backup.document.display.splash.logo {
            backup.assets.push(asset.clone());
        }
        backup.legacy_sections = selected
            .iter()
            .filter(|k| section_domain(k).is_none_or(|d| !native(d)))
            .filter(|k| {
                k.as_str() != "plugins"
                    || !native(Domain::Tessera)
                    || sections["plugins"]
                        .as_object()
                        .is_none_or(|p| p.keys().any(|n| n != "tessera"))
            })
            .cloned()
            .collect();
        let mut fingerprint = json!({});
        for key in &backup.legacy_sections {
            if key != "auth" {
                fingerprint[key] = sections[key].clone();
            }
        }
        backup.legacy_fingerprint = checksum(&fingerprint)?;
        Ok(backup)
    })();
    result.map_err(invalid)
}

/// JSON.stringify field order and number spelling, optionally indented by two.
/// Integer-index object keys precede other keys in JS enumeration order.
pub fn stringify(value: &Value, pretty: bool) -> Result<String, String> {
    fn encode(value: &Value, pretty: bool, depth: usize) -> Result<String, String> {
        let indent = |n| {
            if pretty {
                "  ".repeat(n)
            } else {
                String::new()
            }
        };
        let (open, close, entries) = match value {
            Value::Object(object) => {
                let mut keys = object.keys().collect::<Vec<_>>();
                keys.sort_by_key(|key| {
                    let index = key
                        .parse::<u32>()
                        .ok()
                        .filter(|n| *n < u32::MAX && n.to_string() == key.as_str());
                    (index.is_none(), index.unwrap_or(0))
                });
                let entries = keys
                    .into_iter()
                    .map(|key| {
                        Ok(format!(
                            "{}:{}{}",
                            serde_json::to_string(key).map_err(|e| e.to_string())?,
                            if pretty { " " } else { "" },
                            encode(&object[key], pretty, depth + 1)?
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                ('{', '}', entries)
            }
            Value::Array(array) => (
                '[',
                ']',
                array
                    .iter()
                    .map(|v| encode(v, pretty, depth + 1))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            _ => return canonical(value),
        };
        if entries.is_empty() {
            return Ok(format!("{open}{close}"));
        }
        if pretty {
            Ok(format!(
                "{open}\n{}{}\n{}{close}",
                indent(depth + 1),
                entries.join(&format!(",\n{}", indent(depth + 1))),
                indent(depth)
            ))
        } else {
            Ok(format!("{open}{}{close}", entries.join(",")))
        }
    }
    encode(value, pretty, 0)
}
