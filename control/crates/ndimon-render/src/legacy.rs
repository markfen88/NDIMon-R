//! Shared legacy JSON helpers and compatibility entry points.
use crate::{LegacyFiles, PathSpec, replace_key};
use ndimon_types::*;
use serde_json::{Value, json};

pub(crate) fn read(files: &LegacyFiles, name: &str) -> Result<Value, String> {
    files
        .get(&format!("/etc/{name}"))
        .map(|raw| serde_json::from_str(raw).map_err(|e| format!("{name}: {e}")))
        .unwrap_or_else(|| Ok(json!({})))
}
pub fn import_json(files: &LegacyFiles) -> Result<DesiredState, String> {
    let mut state = DesiredState::default();
    for renderer in crate::REGISTRY {
        if let Some(import) = renderer.import {
            import(&mut state, files)?;
        }
    }
    Ok(state)
}
pub fn render(
    spec: &PathSpec,
    state: &DesiredState,
    files: &LegacyFiles,
) -> Result<Vec<u8>, String> {
    crate::render_path(spec, state, state.revision, files)
}
pub fn device(state: &DesiredState, files: &LegacyFiles) -> Result<Vec<u8>, String> {
    crate::device::settings(state, files)
}
pub(crate) fn merge_json(
    spec: &PathSpec,
    values: Value,
    files: &LegacyFiles,
) -> Result<Vec<u8>, String> {
    let mut raw = files
        .get(spec.path)
        .cloned()
        .unwrap_or_else(|| "{}".into())
        .into_bytes();
    for (key, value) in values.as_object().ok_or("expected object")? {
        raw = replace_key(
            std::str::from_utf8(&raw).map_err(|e| e.to_string())?,
            key,
            value,
        )?;
    }
    Ok(raw)
}
pub(crate) fn split(v: &str) -> Vec<String> {
    v.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}
pub(crate) fn resolution(v: &str) -> Result<Resolution, String> {
    if ["", "auto", "hdmi"].contains(&v) {
        return Ok(Resolution::Auto);
    }
    let (size, hz) = v.split_once('@').unwrap_or((v, "0"));
    let (w, h) = size.split_once('x').ok_or("unsupported video mode")?;
    Ok(Resolution::Mode {
        width: w.parse().map_err(|_| "invalid width")?,
        height: h.parse().map_err(|_| "invalid height")?,
        refresh_hz: hz.parse().map_err(|_| "invalid refresh")?,
    })
}
pub(crate) fn mode(value: &Resolution) -> String {
    match value {
        Resolution::Auto => "auto".into(),
        Resolution::Mode {
            width,
            height,
            refresh_hz,
        } => {
            if *refresh_hz == 0.0 {
                format!("{width}x{height}")
            } else {
                format!("{width}x{height}@{refresh_hz}")
            }
        }
        _ => "auto".into(),
    }
}
