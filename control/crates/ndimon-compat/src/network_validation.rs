//! Preserve the legacy validators used by base settings and backup VLAN input.
use ndimon_types::{ApiError, FieldError};
use serde_json::{Value, json};
use std::net::Ipv4Addr;
fn invalid(reason: &str) -> ApiError {
    ApiError::invalid(vec![FieldError::new("network", reason)])
}
fn text(value: &Value) -> String {
    value.as_str().unwrap_or("").trim().into()
}
fn number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}
fn address(value: &str) -> Option<Ipv4Addr> {
    value.parse().ok()
}
fn subnet(address: Ipv4Addr, prefix: u8) -> (u32, u32) {
    let mask = u32::MAX << (32 - prefix);
    let low = u32::from(address) & mask;
    (low, low | !mask)
}
pub fn normalize(value: &Value) -> Result<Value, ApiError> {
    if value["mode"] == "dhcp" {
        return Ok(json!({"mode":"dhcp","address":"","prefix":0,"gateway":"","dns":[]}));
    }
    if value["mode"] != "static" {
        return Err(invalid("mode must be dhcp or static"));
    }
    let raw = text(&value["address"]);
    let Some(ip) = address(&raw) else {
        return Err(invalid("address is not a valid IPv4 address"));
    };
    let prefix = number(&value["prefix"])
        .filter(|p| (8..=30).contains(p))
        .ok_or_else(|| invalid("prefix must be between 8 and 30"))? as u8;
    if !ndimon_netengine::validation::unicast(ip) {
        return Err(invalid("address is not a usable unicast address"));
    }
    let (low, high) = subnet(ip, prefix);
    if [low, high].contains(&u32::from(ip)) {
        return Err(invalid("address is the network or broadcast address"));
    }
    let gateway = text(&value["gateway"]);
    if !gateway.is_empty() {
        let ip = address(&gateway).ok_or_else(|| invalid("gateway is not a valid IPv4 address"))?;
        if !(low..=high).contains(&u32::from(ip)) {
            return Err(invalid("gateway is not inside the address subnet"));
        }
    }
    let dns: Vec<_> = value["dns"]
        .as_array()
        .into_iter()
        .flatten()
        .map(text)
        .filter(|s| !s.is_empty())
        .collect();
    if dns.len() > 3 || dns.iter().any(|s| address(s).is_none()) {
        return Err(invalid("dns must be at most 3 IPv4 addresses"));
    }
    Ok(json!({"mode":"static","address":raw,"prefix":prefix,"gateway":gateway,"dns":dns}))
}
pub fn vlan(id: &str, value: &Value, current: &Value) -> Result<Value, ApiError> {
    let id = id
        .parse::<u16>()
        .ok()
        .filter(|n| (1..=4094).contains(n))
        .ok_or_else(|| invalid("VLAN id must be between 1 and 4094"))?;
    if !value.is_object() {
        return Err(invalid("settings must be an object"));
    }
    if current["interface"]
        .as_str()
        .is_some_and(|name| name.len() + 1 + id.to_string().len() > 15)
    {
        return Err(invalid("VLAN interface name must fit within 15 characters"));
    }
    let rows = current["vlans"].as_array();
    if rows.is_some_and(|rows| rows.len() >= 8 && !rows.iter().any(|row| row["id"] == id)) {
        return Err(invalid("at most 8 VLANs are allowed"));
    }
    if rows.is_some_and(|rows| {
        rows.iter()
            .any(|row| row["id"] == id && row["managed"] == false)
    }) {
        return Err(invalid("VLAN is not managed by ndimon"));
    }
    if value.get("allow_gateway").is_some_and(|v| !v.is_boolean()) {
        return Err(invalid("allow_gateway must be a boolean"));
    }
    if !text(&value["gateway"]).is_empty() && value["allow_gateway"] != true {
        return Err(invalid("gateway requires allow_gateway: true"));
    }
    let mut clean = normalize(value)?;
    if clean["mode"] == "static" {
        let ip = address(clean["address"].as_str().unwrap_or(""))
            .ok_or_else(|| invalid("invalid address"))?;
        let prefix = clean["prefix"].as_u64().unwrap_or(24) as u8;
        for (addr, bits) in [("address", "prefix"), ("live_address", "live_prefix")] {
            if let (Some(base), Some(base_prefix)) = (
                address(&text(&current[addr])),
                number(&current[bits]).filter(|p| (1..=32).contains(p)),
            ) {
                if subnet(ip, prefix.min(base_prefix as u8)).0
                    == subnet(base, prefix.min(base_prefix as u8)).0
                {
                    return Err(invalid("overlaps base subnet"));
                }
            }
        }
    }
    clean
        .as_object_mut()
        .ok_or_else(|| invalid("settings must be an object"))?
        .remove("dns");
    clean["id"] = json!(id);
    clean["allow_gateway"] = json!(value["allow_gateway"] == true);
    Ok(clean)
}
