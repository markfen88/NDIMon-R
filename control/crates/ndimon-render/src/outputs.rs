//! Legacy outputs import and projection; ownership remains table driven.
use crate::legacy::{mode, resolution};
use crate::{LegacyFiles, PathSpec};
use ndimon_types::*;
use serde_json::json;

pub fn import_json(state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String> {
    let read = |name: &str| crate::legacy::read(files, name);
    let d = read("ndimon-device-settings.json")?;
    for id in 1..=8 {
        let name = format!("ndimon-dec{id}-settings.json");
        if !files.contains_key(&format!("/etc/{name}")) {
            continue;
        }
        let v = read(&name)?;
        if v.get("ChNum")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .is_some_and(|n| n != u64::from(id))
        {
            return Err(format!("{name}: conflicting ChNum"));
        }
        let mut out = Output::default();
        out.alias = v["output_alias"].as_str().unwrap_or("").trim().into();
        let source = v["SourceName"].as_str().unwrap_or("");
        if !source.is_empty() && source != "None" {
            out.source = Some(NdiSourceRef::new(
                source,
                v["SourceIP"].as_str().unwrap_or(""),
            ));
        }
        out.source_selection = v["SourceSelection"].as_str().unwrap_or("NDI").into();
        out.video.resolution = resolution(v["videooutput"].as_str().unwrap_or("auto"))?;
        if let Some(v) = v.get("ScaleMode").filter(|v| *v != "") {
            out.video.scale_mode = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
        }
        if let Some(v) = v.get("rotation") {
            out.video.rotation = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
        }
        out.screensaver = match v["ScreenSaverMode"].as_str().unwrap_or("BlackSS") {
            "SplashSS" => ScreenSaver::Splash,
            "CaptureSS" => ScreenSaver::Capture,
            _ => ScreenSaver::Black,
        };
        state.outputs.insert(id, out);
    }
    if !files.contains_key("/etc/ndimon-dec1-settings.json") {
        state
            .outputs
            .get_mut(&1)
            .expect("default primary")
            .video
            .resolution = resolution(d["videooutput"].as_str().unwrap_or("auto"))?;
    }
    Ok(())
}

pub fn render_path(
    spec: &PathSpec,
    state: &DesiredState,
    _revision: u64,
    files: &LegacyFiles,
) -> Result<Vec<u8>, String> {
    let mut values = match spec.path_id {
        id if id.starts_with("dec") => {
            let id: u8 = id[3..].parse().map_err(|_| "invalid decoder path")?;
            let out = state.outputs.get(&id).ok_or("output missing")?;
            json!({"ChNum":id,"SourceName":out.source.as_ref().map(|s|s.name.as_str()).unwrap_or("None"),"SourceIP":out.source.as_ref().map(|s|s.endpoint.as_str()).unwrap_or(""),"SourceSelection":out.source_selection,"videooutput":mode(&out.video.resolution),"ScaleMode":out.video.scale_mode,"rotation":out.video.rotation,"output_alias":out.alias,"ScreenSaverMode":match out.screensaver {ScreenSaver::Splash=>"SplashSS",ScreenSaver::Capture=>"CaptureSS",_=>"BlackSS"},"NDIAudio":if state.audio.outputs.get(&id).is_some_and(|o|o.enabled){"NDIAudioEn"}else{"NDIAudioDis"},"TallyMode":match state.ndi.tally{TallyMode::On=>"TallyOn",TallyMode::Video=>"VideoMode",_=>"TallyOff"},"ColorSpace":if state.ndi.color_space==ColorSpace::Rgb{"RGB"}else{"YUV"}})
        }
        _ => return Err(format!("unknown outputs artifact: {}", spec.path_id)),
    };
    // Keep omitted defaults and legacy empty/None and auto/hdmi spellings.
    if let Some(raw) = files.get(spec.path) {
        let old: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        if old["ChNum"].as_str().and_then(|s| s.parse::<u64>().ok()) == values["ChNum"].as_u64() {
            values["ChNum"] = old["ChNum"].clone();
        }
        let defaults = json!({"ChNum":values["ChNum"],"SourceName":"None","SourceIP":"","SourceSelection":"NDI","videooutput":"auto","ScaleMode":"letterbox","rotation":0,"output_alias":"","ScreenSaverMode":"BlackSS","NDIAudio":if spec.path_id=="dec1"{"NDIAudioEn"}else{"NDIAudioDis"},"TallyMode":"TallyOff","ColorSpace":"YUV"});
        for (key, value) in defaults.as_object().expect("defaults") {
            if old.get(key).is_none() && values[key] == *value {
                values.as_object_mut().expect("projection").remove(key);
            }
        }
        if values["SourceName"] == "None" && old["SourceName"] != "None" {
            values["SourceName"] = json!("");
        }
        if values["videooutput"] == "auto"
            && matches!(old["videooutput"].as_str(), Some("" | "hdmi"))
        {
            values["videooutput"] = old["videooutput"].clone();
        }
        if values["ScaleMode"] == "letterbox" && old["ScaleMode"] == "" {
            values["ScaleMode"] = json!("");
        }
    }
    crate::legacy::merge_json(spec, values, files)
}

pub fn include(spec: &PathSpec, state: &DesiredState) -> bool {
    spec.path_id
        .strip_prefix("dec")
        .and_then(|s| s.parse::<u8>().ok())
        .is_some_and(|id| state.outputs.contains_key(&id))
}
pub fn drift_bytes(
    artifact: &crate::Artifact,
    _state: &DesiredState,
    files: &LegacyFiles,
) -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
    let project = |bytes: &[u8]| -> Result<Vec<u8>, String> {
        let mut value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        value
            .as_object_mut()
            .ok_or("expected output settings object")?
            .retain(|key, _| {
                [
                    "ChNum",
                    "SourceName",
                    "SourceIP",
                    "SourceSelection",
                    "videooutput",
                    "ScaleMode",
                    "rotation",
                    "output_alias",
                    "ScreenSaverMode",
                    "NDIAudio",
                    "TallyMode",
                    "ColorSpace",
                ]
                .contains(&key.as_str())
            });
        crate::node_json(value)
    };
    Ok((
        project(&artifact.bytes)?,
        files
            .get(artifact.spec().path)
            .map(|raw| project(raw.as_bytes()))
            .transpose()?,
    ))
}
