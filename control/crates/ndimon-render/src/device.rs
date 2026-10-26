//! Legacy device import and projection; ownership remains table driven.
use crate::{Artifact, LegacyFiles, PathSpec};
use ndimon_types::*;
use serde_json::{Value, json};

pub fn import_json(state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String> {
    let read = |name: &str| crate::legacy::read(files, name);
    let d = read("ndimon-device-settings.json")?;
    let mut device = serde_json::to_value(&state.device).map_err(|e| e.to_string())?;
    for (legacy, key) in [
        ("mode", "mode"),
        ("ndi_recv_name", "ndi_alias"),
        ("decode_mode", "decode_mode"),
        ("watchdog_mode", "watchdog_mode"),
        ("ntp_server", "ntp_server"),
        ("plugin_feeds", "plugin_feeds"),
        ("allow_insecure_feeds", "allow_insecure_feeds"),
        ("web_allowed_hosts", "web_allowed_hosts"),
    ] {
        if let Some(value) = d.get(legacy) {
            device[key] = value.clone();
        }
    }
    if let Some(alias) = device["ndi_alias"].as_str() {
        device["ndi_alias"] = json!(alias.trim());
    }
    if let Some(hosts) = device["web_allowed_hosts"].as_array_mut() {
        for host in hosts {
            if let Some(name) = host.as_str() {
                *host = json!(name.to_ascii_lowercase());
            }
        }
    }
    for (old, new) in [
        ("AnalogAudioInGain", "analog_audio_in_gain"),
        ("AnalogAudioOutGain", "analog_audio_out_gain"),
    ] {
        if let Some(v) = d.get(old) {
            device[new] = json!(
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                    .ok_or("invalid analog gain")?
            );
        }
    }
    if let Some(v) = d.get("reboot_schedule_enabled") {
        device["reboot_schedule"]["enabled"] = json!(match v {
            Value::Bool(v) => *v,
            Value::Null => false,
            Value::String(s) => !s.is_empty(),
            Value::Number(n) => n.as_f64() != Some(0.0),
            _ => true,
        });
    }
    if let Some(time) = d
        .get("reboot_schedule_time")
        .and_then(Value::as_str)
        .filter(|t| valid_time(t))
    {
        device["reboot_schedule"]["time"] = json!(time);
    }
    if let Some(days) = d.get("reboot_schedule_days").and_then(Value::as_array) {
        let normalized = normalize_days(days);
        if let Some(days) = normalized.filter(|d| !d.is_empty()) {
            device["reboot_schedule"]["days"] = json!(days);
        }
    }
    let about = read("ndimon-about-settings.json")?;
    for (old, new) in [
        ("GroupName", "group_name"),
        ("MACAddress", "mac_address"),
        ("device_name", "device_name"),
        ("host_name", "host_name"),
    ] {
        if let Some(v) = about.get(old) {
            device["about"][new] = v.clone();
        }
    }
    if let Some(hostname) = files.get("/etc/hostname") {
        device["hostname"] = json!(hostname.trim());
    }
    state.device = serde_json::from_value(device).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn render_path(
    spec: &PathSpec,
    state: &DesiredState,
    _revision: u64,
    files: &LegacyFiles,
) -> Result<Vec<u8>, String> {
    let values = match spec.path_id {
        "device" => return settings(state, files),
        "about" => {
            json!({"GroupName":state.device.about.group_name,"MACAddress":state.device.about.mac_address,"device_name":state.device.about.device_name,"host_name":state.device.about.host_name})
        }
        _ => return Err(format!("unknown device artifact: {}", spec.path_id)),
    };
    crate::legacy::merge_json(spec, values, files)
}

pub fn settings(state: &DesiredState, files: &LegacyFiles) -> Result<Vec<u8>, String> {
    let d = &state.device;
    let values = json!({"mode":d.mode,"ndi_recv_name":d.ndi_alias,"decode_mode":d.decode_mode,"watchdog_mode":d.watchdog_mode,"ntp_server":d.ntp_server,"reboot_schedule_enabled":d.reboot_schedule.enabled,"reboot_schedule_time":d.reboot_schedule.time,"reboot_schedule_days":d.reboot_schedule.days,"AnalogAudioInGain":d.analog_audio_in_gain.to_string(),"AnalogAudioOutGain":d.analog_audio_out_gain.to_string(),"plugin_feeds":d.plugin_feeds,"allow_insecure_feeds":d.allow_insecure_feeds,"web_allowed_hosts":d.web_allowed_hosts});
    let mut raw = files
        .get("/etc/ndimon-device-settings.json")
        .cloned()
        .unwrap_or_else(|| "{}".into())
        .into_bytes();
    for (key, value) in values.as_object().expect("device object") {
        raw = crate::replace_key(
            std::str::from_utf8(&raw).map_err(|e| e.to_string())?,
            key,
            value,
        )?;
    }
    Ok(raw)
}

pub fn include(spec: &PathSpec, state: &DesiredState) -> bool {
    let _ = (spec, state);
    true
}

/// The legacy adapter accepts long weekday names and emits Sun-first order.
pub fn normalize_days(days: &[Value]) -> Option<Vec<Weekday>> {
    let mut result = Vec::new();
    for day in days {
        let day = day.as_str()?.to_ascii_lowercase();
        let day = match day.get(..3)? {
            "sun" => Weekday::Sun,
            "mon" => Weekday::Mon,
            "tue" => Weekday::Tue,
            "wed" => Weekday::Wed,
            "thu" => Weekday::Thu,
            "fri" => Weekday::Fri,
            "sat" => Weekday::Sat,
            _ => return None,
        };
        if !result.contains(&day) {
            result.push(day);
        }
    }
    result.sort();
    Some(result)
}
pub fn valid_time(time: &str) -> bool {
    let b = time.as_bytes();
    b.len() == 5
        && b[2] == b':'
        && b.iter()
            .enumerate()
            .all(|(i, b)| i == 2 || b.is_ascii_digit())
        && time[..2].parse::<u8>().is_ok_and(|v| v < 24)
        && time[3..].parse::<u8>().is_ok_and(|v| v < 60)
}
pub fn drift_bytes(
    artifact: &Artifact,
    state: &DesiredState,
    files: &LegacyFiles,
) -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
    if artifact.path_id != "device" {
        return Ok((
            artifact.bytes.clone(),
            files
                .get(artifact.spec().path)
                .map(|s| s.as_bytes().to_vec()),
        ));
    }
    let expected: Value = serde_json::from_slice(&settings(state, &LegacyFiles::new())?)
        .map_err(|e| e.to_string())?;
    let actual = files
        .get(artifact.spec().path)
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .map(|raw| {
            let mut projection = serde_json::Map::new();
            for key in expected.as_object().expect("device settings object").keys() {
                projection.insert(key.clone(), raw.get(key).cloned().unwrap_or(Value::Null));
            }
            crate::node_json(Value::Object(projection))
        })
        .transpose()?;
    Ok((crate::node_json(expected)?, actual))
}
