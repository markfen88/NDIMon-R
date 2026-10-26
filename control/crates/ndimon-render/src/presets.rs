//! Legacy preset list projection, including its insertion order.
use crate::{LegacyFiles, PathSpec};
use ndimon_types::*;
use serde_json::{Value, json};

pub fn list(state: &DesiredState) -> Value {
    let mut entries: Vec<_> = state.presets.iter().collect();
    entries.sort_by_key(|(_, p)| p.position.unwrap_or(u32::MAX));
    json!({"presets":entries.into_iter().map(|(name,p)|json!({"name":name,"source":p.source.name,"ip":p.source.endpoint})).collect::<Vec<_>>()})
}
pub fn import_json(state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String> {
    state.presets.clear();
    if let Some(presets) = crate::legacy::read(files, "ndimon-presets.json")?["presets"].as_array()
    {
        for (position, p) in presets.iter().enumerate() {
            let mut preset = Preset::new(NdiSourceRef::new(
                p["source"].as_str().unwrap_or(""),
                p["ip"].as_str().unwrap_or(""),
            ));
            preset.position = Some(u32::try_from(position).map_err(|e| e.to_string())?);
            state.presets.insert(
                p["name"].as_str().ok_or("preset name missing")?.into(),
                preset,
            );
        }
    }
    Ok(())
}
pub fn render_path(
    spec: &PathSpec,
    state: &DesiredState,
    _revision: u64,
    _files: &LegacyFiles,
) -> Result<Vec<u8>, String> {
    if spec.path_id != "presets" {
        return Err(format!("unknown presets artifact: {}", spec.path_id));
    }
    // The legacy writeJson format is compact JSON replacing the complete file.
    // Entries need name/source/ip order, independently of serde's map order.
    let values = list(state);
    let entries = values["presets"].as_array().expect("preset list");
    let mut text = String::from("{\"presets\":[");
    for (i, p) in entries.iter().enumerate() {
        if i != 0 {
            text.push(',');
        }
        text.push_str(&format!(
            "{{\"name\":{},\"source\":{},\"ip\":{}}}",
            p["name"], p["source"], p["ip"]
        ));
    }
    text.push_str("]}");
    Ok(text.into_bytes())
}
