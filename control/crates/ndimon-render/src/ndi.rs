//! Legacy ndi import and projection; ownership remains table driven.
use crate::legacy::split;
use crate::{LegacyFiles, PathSpec};
use ndimon_types::*;
use serde_json::json;

pub fn import_json(state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String> {
    let read = |name: &str| crate::legacy::read(files, name);
    if files.contains_key("/etc/ndimon-dec1-settings.json") {
        let v = read("ndimon-dec1-settings.json")?;
        state.ndi.tally = match v["TallyMode"].as_str().unwrap_or("TallyOff") {
            "TallyOn" => TallyMode::On,
            "VideoMode" => TallyMode::Video,
            _ => TallyMode::Off,
        };
        state.ndi.color_space = if v["ColorSpace"] == "RGB" {
            ColorSpace::Rgb
        } else {
            ColorSpace::Yuv
        };
    }
    let rx = read("ndimon-rx-settings.json")?;
    state.ndi.receive = match rx["Rxpm"].as_str().unwrap_or("RUDP") {
        "TCP" if rx["Rxpm_user_set"] == true => Transport::Tcp,
        "M-TCP" => Transport::MultiTcp,
        "UDP" => Transport::Udp,
        "Multicast" => Transport::Multicast,
        _ => Transport::Rudp,
    };
    state.ndi.discovery_servers = split(
        read("ndimon-find-settings.json")?["NDIDisServIP"]
            .as_str()
            .unwrap_or(""),
    );
    let groups = split(
        read("ndi-group.json")?["ndi_groups"]
            .as_str()
            .unwrap_or("public"),
    );
    state.ndi.groups = if groups.is_empty() {
        vec!["public".into()]
    } else {
        groups
    };
    state.ndi.off_subnet_sources = split(read("ndi-config.json")?.as_str().unwrap_or(""));
    // 1.3.x always let a Discovery Server route the outputs; an upgrade keeps
    // that, a new install (no legacy NDI files) starts with it off.
    state.ndi.remote_control = [
        "/etc/ndimon-dec1-settings.json",
        "/etc/ndimon-rx-settings.json",
        "/etc/ndimon-find-settings.json",
        "/etc/ndi-group.json",
        "/etc/ndi-config.json",
    ]
    .iter()
    .any(|path| files.contains_key(*path));
    Ok(())
}

pub fn render_path(
    spec: &PathSpec,
    state: &DesiredState,
    _revision: u64,
    files: &LegacyFiles,
) -> Result<Vec<u8>, String> {
    let mut values = match spec.path_id {
        "receive" => {
            json!({"Rxpm":match state.ndi.receive { Transport::Tcp=>"TCP",Transport::Udp=>"UDP",Transport::Multicast=>"Multicast",Transport::MultiTcp=>"M-TCP",_=>"RUDP" },"Rxpm_user_set":true})
        }
        "find" => {
            json!({"NDIDisServIP":state.ndi.discovery_servers.join(","),"NDIDisServ":if state.ndi.discovery_servers.is_empty(){"NDIDisServDis"}else{"NDIDisServEn"}})
        }
        "groups" => json!({"ndi_groups":state.ndi.groups.join(",")}),
        "extra-ips" => {
            if let Some(raw) = files.get(spec.path) {
                if let Ok(serde_json::Value::String(saved)) = serde_json::from_str(raw) {
                    if split(&saved) == state.ndi.off_subnet_sources {
                        return Ok(raw.as_bytes().to_vec());
                    }
                }
            }
            return crate::node_json(json!(state.ndi.off_subnet_sources.join(",")));
        }
        _ => return Err(format!("unknown ndi artifact: {}", spec.path_id)),
    };
    if spec.path_id == "groups" {
        if let Some(raw) = files.get(spec.path) {
            let old: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
            if let Some(groups) = old["ndi_groups"].as_str() {
                if split(groups) == state.ndi.groups {
                    values["ndi_groups"] = json!(groups);
                }
            }
        }
    }
    crate::legacy::merge_json(spec, values, files)
}
