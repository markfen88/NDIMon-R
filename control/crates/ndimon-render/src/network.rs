//! Pure network projections. Privd engine journals and publishes these runtime
//! artifacts; generic desired-state rendering must never overwrite a candidate.
use crate::{LegacyFiles, PathSpec};
use ndimon_types::*;
use serde_json::{Value, json};

pub fn import_json(state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String> {
    if let Some(raw) = files.get("/var/lib/ndimon-network/committed.json") {
        let v: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        if v["version"] != 1 {
            return Err("unsupported committed network version".into());
        }
        let mut network = serde_json::to_value(&state.network).map_err(|e| e.to_string())?;
        for key in ["interface", "backend", "networks", "confirm_policy"] {
            if let Some(v) = v.get(key) {
                network[key] = v.clone();
            }
        }
        for (old, new) in [
            ("management", "management"),
            ("ndi", "ndi"),
            ("tessera", "audio"),
        ] {
            if let Some(v) = v.get(old) {
                network["roles"][new] = v.clone();
            }
        }
        state.network = serde_json::from_value(network).map_err(|e| e.to_string())?;
        for row in &mut state.network.networks {
            if row.mode == AddressMode::Dhcp {
                row.address.clear();
                row.prefix = 0;
                row.gateway.clear();
                row.dns.clear();
            }
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
    let values = match spec.path_id {
        "network" => {
            json!({"version":1,"revision":state.revision,"interface":state.network.interface,"backend":state.network.backend,"networks":state.network.networks,"management":state.network.roles.management,"ndi":state.network.roles.ndi,"tessera":state.network.roles.audio})
        }
        "network-bindings" => return bindings(&state.network).map(String::into_bytes),
        "network-banner" => {
            return Ok(
                "Pending network change — the new address will appear above\n"
                    .as_bytes()
                    .to_vec(),
            );
        }
        "network-ndi-slice" | "network-audio-slice" => {
            return Ok(
                "[Unit]\nDescription=NDIMon network traffic boundary\n[Slice]\n"
                    .as_bytes()
                    .to_vec(),
            );
        }
        "network-decoder-slice" | "network-finder-slice" => {
            return Ok(dropin("ndimonndi.slice").into_bytes());
        }
        "network-tessera-slice" => return Ok(dropin("ndimontessera.slice").into_bytes()),
        "tessera-network" => return tessera_binding(&state.network).map(String::into_bytes),
        _ => return Err(format!("unknown network artifact: {}", spec.path_id)),
    };
    crate::legacy::merge_json(spec, values, files)
}

/// Engine publication includes snapshots, absence and pending bindings. Leaving
/// these out of generic rendering is essential during unrelated actor writes.
pub fn include_artifact(_spec: &PathSpec, _state: &DesiredState) -> bool {
    false
}
fn interface(profiles: &NetworkProfiles, id: &str) -> Result<String, String> {
    let row = profiles
        .networks
        .iter()
        .find(|n| n.id == id)
        .ok_or_else(|| format!("missing network role: {id}"))?;
    Ok(row.vlan_id.map_or_else(
        || profiles.interface.clone(),
        |tag| format!("{}.{tag}", profiles.interface),
    ))
}
pub fn bindings(profiles: &NetworkProfiles) -> Result<String, String> {
    serde_json::to_string_pretty(&json!({"management":interface(profiles,&profiles.roles.management)?,"ndi":interface(profiles,&profiles.roles.ndi)?,"tessera":interface(profiles,&profiles.roles.audio)?})).map(|s| s+"\n").map_err(|e| e.to_string())
}
pub fn tessera_binding(profiles: &NetworkProfiles) -> Result<String, String> {
    let name = interface(profiles, &profiles.roles.audio)?;
    Ok(format!(
        "[network]\ninterfaces = [\"{name}\"]\nlocked = [\"primary\"]\n"
    ))
}
pub fn dropin(slice: &str) -> String {
    format!(
        "[Unit]\nRequires=ndimon-net-boot.service\nAfter=ndimon-net-boot.service\n[Service]\nSlice={slice}\n"
    )
}
