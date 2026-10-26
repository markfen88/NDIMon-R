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
    let values = dns(value);
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
        return json!({"present":true,"applicable":true,"needs_prompt":false,"warnings":[],"message":""});
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
        let dns = dns(backup);
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
        if let (Some(IpAddr::V4(peer)), Ok(address)) = (peer, address.parse::<Ipv4Addr>()) {
            if (1..=32).contains(&prefix)
                && u32::from(peer) >> (32 - prefix) != u32::from(address) >> (32 - prefix)
            {
                warnings.push(format!(
                    "Your computer ({peer}) may not be able to reach the device at {address}."
                ));
            }
        }
    }
    json!({"present":true,"applicable":true,"needs_prompt":true,"current_mode":if cur_mode.is_empty(){"unknown"}else{cur_mode},"backup_mode":mode,"message":message,"warnings":warnings})
}
