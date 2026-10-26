//! Splash/OSD compatibility projections and portable legacy import.
use crate::{LegacyFiles, PathSpec};
use ndimon_types::*;
use serde_json::{Value, json};

pub const SPLASH_PATH: &str = "/etc/ndimon-splash-settings.json";
pub const OSD_PATH: &str = "/etc/ndimon-osd-settings.json";

pub fn logo_path(asset: Option<&AssetRef>) -> &'static str {
    match asset.map(|a| a.media_type.as_str()) {
        Some("image/png") => "/etc/ndi-splash-logo.png",
        Some("image/jpeg") => "/etc/ndi-splash-logo.jpg",
        _ => "",
    }
}

/// Key order is api/splash-config.js::DEFAULTS (JSON.stringify, no newline).
pub fn splash(s: &Splash) -> Value {
    json!({
        "bg_idle":s.bg_idle,"bg_live":s.bg_live,
        "accent_idle":s.accent_idle,"accent_live":s.accent_live,
        "logo_path":logo_path(s.logo.as_ref()),
        "logo_x_pct":s.logo_x_pct,"logo_y_pct":s.logo_y_pct,"logo_w_pct":s.logo_w_pct,
        "text_idle":s.text_idle,"text_live":s.text_live,"text_height_pct":s.text_height_pct,
        "backdrop":s.backdrop,"show_signal_text":s.show_signal_text,
        "show_device_name":s.show_device_name,"show_device_url":s.show_device_url,
        "show_sources_available":s.show_sources_available
    })
}

pub fn osd(osd: &Osd, files: &LegacyFiles) -> Value {
    let mut v = json!({"enabled":false,"text":""});
    if let Some(old) = files
        .get(OSD_PATH)
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
    {
        if let Some(object) = old.as_object() {
            for (key, value) in object {
                v[key] = value.clone();
            }
        }
    }
    v["enabled"] = json!(osd.enabled);
    v["text"] = json!(osd.text);
    v
}

pub fn import_json(state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String> {
    let read = |path: &str| {
        files
            .get(path)
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .unwrap_or(json!({}))
    };
    let stored = read(SPLASH_PATH);
    let mut values = serde_json::to_value(Splash::default()).map_err(|e| e.to_string())?;
    if let Some(object) = stored.as_object() {
        for (key, value) in object {
            if key != "logo" && values.get(key).is_some() {
                values[key] = value.clone();
            }
        }
    }
    if !matches!(values["backdrop"].as_str(), Some("gradient" | "flat")) {
        values["backdrop"] = json!("gradient");
    }
    state.display.splash = serde_json::from_value(values).map_err(|e| e.to_string())?;
    let stored = read(OSD_PATH);
    let mut values = json!(Osd::default());
    for key in ["enabled", "text"] {
        if let Some(value) = stored.get(key) {
            values[key] = value.clone();
        }
    }
    state.display.osd = serde_json::from_value(values).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn render_path(
    spec: &PathSpec,
    state: &DesiredState,
    _revision: u64,
    files: &LegacyFiles,
) -> Result<Vec<u8>, String> {
    crate::node_json(match spec.path_id {
        "splash" => splash(&state.display.splash),
        "osd" => osd(&state.display.osd, files),
        _ => return Err(format!("unknown display artifact: {}", spec.path_id)),
    })
}
