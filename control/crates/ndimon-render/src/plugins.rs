//! Legacy plugins import and projection; ownership remains table driven.
use crate::{LegacyFiles, PathSpec};
use ndimon_types::*;
use serde_json::{Value, json};

pub fn import_json(state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String> {
    let read = |name: &str| crate::legacy::read(files, name);
    // Preserve corrupt source bytes for the administrator's explicit reset;
    // the legacy host supplies the observation and refuses lifecycle effects.
    if let Some(plugins) = read("ndimon-plugins.json").unwrap_or(json!({})).as_object() {
        for (name, v) in plugins {
            let mut p = PluginState::default();
            p.installed = v["installed"].as_bool().unwrap_or(false)
                || v["version"].as_str().is_some_and(|v| !v.is_empty());
            // A pending request is accepted intent; the pending phase itself is
            // kept by the legacy runtime as an observation, never desired data.
            p.enabled = v["enabled"] == true || v["pending_enable"] == true;
            p.version = v["version"].as_str().unwrap_or("").into();
            if v["state"].is_object() {
                p.state = serde_json::from_value(v["state"].clone()).map_err(|e| e.to_string())?;
            }
            state.plugins.insert(name.clone(), p);
        }
    }
    Ok(())
}

pub fn render_path(
    spec: &PathSpec,
    state: &DesiredState,
    _revision: u64,
    files: &LegacyFiles,
) -> Result<Vec<u8>, String> {
    if let Some(raw) = files.get(spec.path) {
        if serde_json::from_str::<Value>(raw).map_or(true, |v| !v.is_object()) {
            return Ok(raw.as_bytes().to_vec());
        }
    }
    let values = match spec.path_id {
        "plugins" => json!(state.plugins),
        _ => return Err(format!("unknown plugins artifact: {}", spec.path_id)),
    };
    // Whole-file ownership: removed plugins must disappear. Keep only unknown
    // fields of surviving entries. Imported pending/error observations remain
    // readable during handover; the runtime journal supersedes them.
    let old: Value = files
        .get(spec.path)
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or(json!({}));
    let mut result = serde_json::Map::new();
    for (name, value) in values.as_object().ok_or("expected plugins object")? {
        let mut entry = old
            .get(name)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (key, value) in value.as_object().ok_or("expected plugin entry")? {
            entry.insert(key.clone(), value.clone());
        }
        // A previous activation request cannot survive accepted disable intent.
        if value["enabled"] != true {
            entry.remove("pending_enable");
        }
        result.insert(name.clone(), Value::Object(entry));
    }
    crate::node_json(Value::Object(result))
}

/// Ignore unknown fields and runtime observations when checking native drift.
pub fn drift_bytes(
    artifact: &crate::Artifact,
    state: &DesiredState,
    files: &LegacyFiles,
) -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
    let expected = crate::node_json(json!(state.plugins))?;
    let observed = files
        .get(artifact.spec().path)
        .map(|raw| {
            let mut document = DesiredState::default();
            import_json(&mut document, files)?;
            let parsed: Value = match serde_json::from_str::<Value>(raw) {
                Ok(value) if value.is_object() => value,
                _ => return Ok(raw.as_bytes().to_vec()),
            };
            // Import translates an old pending request into desired intent once.
            // Drift compares only native fields, never that runtime observation.
            for (name, entry) in parsed.as_object().ok_or("expected plugins object")? {
                if let Some(plugin) = document.plugins.get_mut(name) {
                    plugin.enabled = entry["enabled"] == true;
                }
            }
            crate::node_json(json!(document.plugins))
        })
        .transpose()?;
    Ok((expected, observed))
}
