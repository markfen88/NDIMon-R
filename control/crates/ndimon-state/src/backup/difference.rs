//! Text and shape retained for the base-settings backup preview.
use serde_json::{Value, json};
use std::net::{IpAddr, Ipv4Addr};
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn dns(value: &Value) -> Vec<String> {
    let mut dns: Vec<_> = value["dns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    dns.sort();
    dns
}
fn dns_label(value: &Value) -> String {
    let values = value["dns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    if values.is_empty() {
        "(none)".into()
    } else {
        values.join(", ")
    }
}
pub fn describe(current: &Value, backup: &Value, peer: Option<IpAddr>) -> Value {
    let cur_mode = text(current, "mode");
    let mode = text(backup, "mode");
    if backup.is_null() {
        return json!({"present":false,"needs_prompt":false,"warnings":[]});
    }
    if backup.get("profiles").is_some() {
        let strip = |v: &Value| {
            let mut v = v.clone();
            if let Some(o) = v.as_object_mut() {
                o.remove("revision");
            }
            super::legacy::stringify(&v, false).expect("profile JSON")
        };
        let changed = current.get("profiles").is_none()
            || strip(&current["profiles"]) != strip(&backup["profiles"]);
        return json!({"present":true,"needs_prompt":changed,"message":if changed {"Network profiles or service assignments differ. Applying them requires confirmation through the management network."} else {""},"warnings":if changed {vec!["All network profiles are restored in one protected transaction."]}else{vec![]}});
    }
    if mode == "unknown" || current["applicable"] == false {
        let reason = text(current, "reason");
        let reason = if reason.is_empty() {
            String::new()
        } else {
            format!(" ({reason})")
        };
        return json!({"present":true,"applicable":false,"needs_prompt":false,"warnings":[],"message":format!("Network settings in this backup cannot be applied on this system{reason}. They will be skipped.")});
    }
    let equal = cur_mode == mode
        && (mode == "dhcp"
            || (text(current, "address") == text(backup, "address")
                && current["prefix"] == backup["prefix"]
                && text(current, "gateway") == text(backup, "gateway")
                && dns(current) == dns(backup)));
    if equal {
        let changed = !vlans_equal(&current["vlans"], &backup["vlans"]);
        return json!({"present":true,"applicable":true,"warnings":[],"message":if changed {"VLAN settings in the backup differ. Apply them?"}else{""},"needs_prompt":changed});
    }
    let address = text(backup, "address");
    let prefix = backup["prefix"].as_u64().unwrap_or(0);
    let gateway = text(backup, "gateway");
    let message = if mode == "dhcp" {
        let current_address = text(current, "address");
        let suffix = if current_address.is_empty() {
            String::new()
        } else {
            format!(" ({current_address})")
        };
        format!(
            "The backup uses DHCP. This device is currently set to a static address{suffix}. The new address will come from the DHCP server and is not known in advance. It will be shown on the connected display."
        )
    } else if ["dhcp", "unknown"].contains(&cur_mode) {
        let gateway = if gateway.is_empty() {
            String::new()
        } else {
            format!(", gateway {gateway}")
        };
        let dns = backup["dns"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        let dns = if dns.is_empty() {
            String::new()
        } else {
            format!(", DNS {}", dns.join(", "))
        };
        let live = text(current, "live_address");
        let live = if live.is_empty() {
            String::new()
        } else {
            format!(" (current address {live})")
        };
        format!(
            "The network configuration in this backup is different. The backup uses a static address: {address}/{prefix}{gateway}{dns}. This device currently uses DHCP{live}."
        )
    } else {
        let mut bits = Vec::new();
        if text(current, "address") != address || current["prefix"] != backup["prefix"] {
            bits.push(format!(
                "address {}/{} → {address}/{prefix}",
                text(current, "address"),
                current["prefix"]
            ));
        }
        let old = text(current, "gateway");
        if old != gateway {
            bits.push(format!(
                "gateway {} → {}",
                if old.is_empty() { "(none)" } else { old },
                if gateway.is_empty() {
                    "(none)"
                } else {
                    gateway
                }
            ));
        }
        if dns(current) != dns(backup) {
            bits.push(format!(
                "DNS {} → {}",
                dns_label(current),
                dns_label(backup)
            ));
        }
        format!(
            "The network configuration in this backup is different. The backup uses a static address. {}. Overwrite the current network settings?",
            if bits.is_empty() {
                format!("{address}/{prefix}")
            } else {
                bits.join("; ")
            }
        )
    };
    let mut warnings = Vec::new();
    if mode == "static" {
        if let Some(peer) = peer {
            let peer = match peer {
                IpAddr::V6(v) => v.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(peer),
                _ => peer,
            };
            let same = match (peer, address.parse::<Ipv4Addr>()) {
                (IpAddr::V4(peer), Ok(address)) if (1..=32).contains(&prefix) => {
                    u32::from(peer) >> (32 - prefix) == u32::from(address) >> (32 - prefix)
                }
                _ => false,
            };
            if !same {
                warnings.push(format!(
                    "Your computer ({peer}) may not be able to reach the device at {address}."
                ));
            }
        }
    }
    json!({"present":true,"applicable":true,"needs_prompt":true,"current_mode":if cur_mode.is_empty(){"unknown"}else{cur_mode},"backup_mode":mode,"message":message,"warnings":warnings})
}

fn vlan_settings(v: &Value) -> Value {
    let static_mode = v["mode"] == "static";
    json!({"id":v["id"],"mode":v["mode"],"address":if static_mode {text(v,"address")}else{""},"prefix":if static_mode {v["prefix"].as_u64().unwrap_or(0)}else{0},"gateway":text(v,"gateway"),"allow_gateway":v["allow_gateway"]==true})
}
fn vlans_equal(current: &Value, incoming: &Value) -> bool {
    if !incoming.is_array() {
        return true;
    }
    let key = |v: &Value| {
        let mut rows = v
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v["managed"] != false)
            .map(vlan_settings)
            .collect::<Vec<_>>();
        rows.sort_by_key(|v| v["id"].as_u64());
        rows
    };
    key(current) == key(incoming)
}
