//! Schema-1 network conversion; staging remains the last restore operation.
use crate::{persist::StateFile, plan::internal};
use ndimon_types::*;
use serde_json::{Value, json};

pub fn capture(state: &StateFile) -> Result<Value, ApiError> {
    section(
        &state.document.network,
        state.domain_revision(Domain::Network),
    )
}

/// The schema-1 view of one set of profiles: the committed ones, or (review and
/// restore on a device that has committed none) the ones the Network page proposes.
pub fn section(profiles: &NetworkProfiles, revision: u64) -> Result<Value, ApiError> {
    let management = profiles
        .networks
        .iter()
        .find(|n| n.id == profiles.roles.management)
        .ok_or_else(|| internal("missing management network"))?;
    Ok(json!({
        "profiles":{"version":1,"revision":revision,"interface":profiles.interface,"backend":profiles.backend,"networks":profiles.networks,"management":profiles.roles.management,"ndi":profiles.roles.ndi,"tessera":profiles.roles.audio},
        "mode":management.mode,"address":management.address,"prefix":management.prefix,"gateway":management.gateway,"dns":management.dns,
        "manager":profiles.backend,"interface":profiles.interface,"applicable":true,
        "vlans":profiles.networks.iter().filter_map(|n| n.vlan_id.map(|id| json!({"id":id,"mode":n.mode,"address":n.address,"prefix":n.prefix,"gateway":n.gateway,"allow_gateway":n.id==profiles.roles.management}))).collect::<Vec<_>>()
    }))
}

/// What the Network page proposes while nothing is committed (the profile the
/// engine would adopt, from the status query); `None` when adoption is refused
/// or the status carries no usable profile.
pub fn proposal(status: &Value) -> Option<NetworkProfiles> {
    serde_json::from_value::<NetworkProfiles>(status["profiles"].clone())
        .ok()
        .filter(|p| p.backend.is_some() && p.networks.iter().any(|n| n.id == p.roles.management))
}

/// `proposed`: the profile to start from on a device that has committed none.
pub fn candidate(
    state: &StateFile,
    section: &Value,
    selected: bool,
    proposed: Option<&NetworkProfiles>,
) -> Result<Option<NetworkProfiles>, ApiError> {
    if !selected || section.is_null() || section["mode"] == "unknown" {
        return Ok(None);
    }
    let current = match proposed {
        Some(proposed) if state.document.network.backend.is_none() => proposed,
        _ if state.document.network.backend.is_none() => {
            return Err(ApiError::Unsupported {
                what: "backend_not_certified".into(),
            });
        }
        _ => &state.document.network,
    };
    let mut profiles = if let Some(value) = section.get("profiles") {
        let mut value = value.clone();
        value["revision"] = json!(state.domain_revision(Domain::Network));
        value["interface"] = json!(current.interface);
        value["backend"] = json!(current.backend);
        ndimon_netengine::validation::import_legacy(&serde_json::to_vec(&value).map_err(internal)?)?
            .1
    } else {
        let mut profiles = current.clone();
        let row = profiles
            .networks
            .iter_mut()
            .find(|n| n.id == profiles.roles.management)
            .ok_or_else(|| internal("missing management network"))?;
        *row = patch(row.clone(), section)?;
        if let Some(vlans) = section["vlans"].as_array() {
            profiles.networks.retain(|n| n.vlan_id.is_none());
            for vlan in vlans {
                let id = vlan["id"]
                    .as_u64()
                    .and_then(|n| u16::try_from(n).ok())
                    .ok_or_else(|| {
                        ApiError::invalid(vec![FieldError::new(
                            "network.vlans.id",
                            "invalid VLAN id",
                        )])
                    })?;
                let mut row = current
                    .networks
                    .iter()
                    .find(|n| n.vlan_id == Some(id))
                    .cloned()
                    .unwrap_or_else(|| {
                        NetworkProfile::dhcp(format!("vlan-{id}"), format!("VLAN {id}"))
                    });
                row.vlan_id = Some(id);
                profiles.networks.push(patch(row, vlan)?);
            }
        }
        profiles
    };
    profiles.confirm_policy = current.confirm_policy;
    let profiles = ndimon_netengine::validation::validate(&profiles, current)?;
    Ok((profiles != *current).then_some(profiles))
}

fn patch(row: NetworkProfile, value: &Value) -> Result<NetworkProfile, ApiError> {
    let mut row = serde_json::to_value(row).map_err(internal)?;
    for key in ["mode", "address", "prefix", "gateway", "dns"] {
        if let Some(value) = value.get(key) {
            row[key] = value.clone();
        }
    }
    if row["mode"] == "dhcp" {
        row["address"] = json!("");
        row["prefix"] = json!(0);
        row["gateway"] = json!("");
        row["dns"] = json!([]);
    }
    serde_json::from_value(row)
        .map_err(|e| ApiError::invalid(vec![FieldError::new("network", e.to_string())]))
}

pub fn pending(reply: &Value) -> Value {
    let pending = &reply["pending"];
    json!({"ok":true,"profiles":true,"token":reply["token"],"phase":pending["phase"],
        "address":pending["management_address"].as_str().unwrap_or(""),"remaining_seconds":pending["remaining_seconds"],"confirm_seconds":60,
        "deadline":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs().saturating_add(pending["remaining_seconds"].as_u64().unwrap_or(0))})
}
