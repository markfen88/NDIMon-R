//! NE-2: privileged validation; the document is always ndimon-types' model.
use crate::{MAX_MESSAGE, Result, invalid};
use ndimon_types::{
    AddressMode, ApiError, ConfirmPolicy, FieldError, NetworkProfile, NetworkProfiles,
};
use serde_json::Value;
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};

/// Operator wording for the confirmation window. Under `token_only` the change can be
/// kept from any address that holds the token, so only `require_management_address`
/// tells the operator to use the new management connection.
pub fn confirm_message(policy: ConfirmPolicy) -> &'static str {
    match policy {
        ConfirmPolicy::RequireManagementAddress => {
            "Keep the changes through the new management connection within 60 seconds after they are applied, or all network changes revert."
        }
        _ => {
            "Keep the changes within 60 seconds after they are applied, or all network changes revert."
        }
    }
}
pub fn ifname(p: &NetworkProfiles, n: &NetworkProfile) -> String {
    n.vlan_id.map_or_else(
        || p.interface.clone(),
        |tag| format!("{}.{tag}", p.interface),
    )
}
pub fn assigned<'a>(p: &'a NetworkProfiles, id: &str) -> Result<&'a NetworkProfile> {
    p.networks
        .iter()
        .find(|n| n.id == id)
        .ok_or_else(|| ApiError::NotFound {
            what: format!("network:{id}"),
        })
}
pub fn physical(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 15
        && s != "lo"
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
pub fn unicast(a: Ipv4Addr) -> bool {
    let b = a.octets();
    b[0] != 0 && b[0] != 127 && b[0] < 224 && !(b[0] == 169 && b[1] == 254)
}
pub fn subnet(a: Ipv4Addr, prefix: u8) -> (u32, u32) {
    let mask = u32::MAX << (32 - prefix);
    let low = u32::from(a) & mask;
    (low, low | !mask)
}
pub fn mapped(a: IpAddr) -> Option<Ipv4Addr> {
    match a {
        IpAddr::V4(a) => Some(a),
        IpAddr::V6(a) => a.to_ipv4_mapped(),
    }
}

/// Accumulate path-qualified errors rather than aborting at the first row.
pub fn validate(
    candidate: &NetworkProfiles,
    previous: &NetworkProfiles,
) -> Result<NetworkProfiles> {
    let mut p = candidate.clone();
    let mut errors = Vec::new();
    let mut add = |field: String, reason: &str| errors.push(FieldError::new(field, reason));
    if p.interface != previous.interface || p.backend != previous.backend {
        add("network.interface".into(), "fixed_port_backend");
    }
    if !physical(&p.interface) {
        add("network.interface".into(), "invalid_physical_interface");
    }
    if p.backend.is_none() {
        add("network.backend".into(), "invalid_document");
    }
    if !(1..=9).contains(&p.networks.len()) {
        add("network.networks".into(), "network_count");
    }
    if p.networks.iter().filter(|n| n.vlan_id.is_some()).count() > 8 {
        add("network.networks".into(), "tagged_network_count");
    }
    if !matches!(
        p.confirm_policy,
        ConfirmPolicy::TokenOnly | ConfirmPolicy::RequireManagementAddress
    ) {
        add("network.confirm_policy".into(), "invalid_policy");
    }
    let mut ids = BTreeSet::new();
    let mut tags = BTreeSet::new();
    let mut subnets: Vec<(u32, u32)> = Vec::new();
    for (i, n) in p.networks.iter_mut().enumerate() {
        let path = format!("network.networks[{i}]");
        if n.id.is_empty()
            || n.id.len() > 32
            || !n.id.as_bytes()[0].is_ascii_lowercase()
            || !n
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            add(format!("{path}.id"), "invalid_network_id");
        }
        if !ids.insert(n.id.clone()) {
            add(format!("{path}.id"), "duplicate_network_id");
        }
        if n.name.trim().is_empty()
            || n.name.chars().count() > 64
            || n.name.chars().any(char::is_control)
        {
            add(format!("{path}.name"), "invalid_network_name");
        }
        n.name = n.name.trim().into();
        // NE-2.12a: search domains apply to DHCP and static management alike.
        if n.id != p.roles.management && !n.dns_search.is_empty() {
            add(format!("{path}.dns_search"), "gateway_dns_management_only");
        }
        if n.dns_search.len() > ndimon_types::MAX_DNS_SEARCH {
            add(format!("{path}.dns_search"), "invalid_dns_search_list");
        }
        for (j, d) in n.dns_search.iter().enumerate() {
            if !ndimon_types::valid_dns_search_domain(d) {
                add(
                    format!("{path}.dns_search[{j}]"),
                    "invalid_dns_search_domain",
                );
            }
        }
        if n.vlan_id.is_some_and(|t| !(1..=4094).contains(&t)) {
            add(format!("{path}.vlan_id"), "invalid_vlan_id");
        }
        if !tags.insert(n.vlan_id) {
            add(format!("{path}.vlan_id"), "duplicate_vlan");
        }
        let name = n
            .vlan_id
            .map_or_else(|| p.interface.clone(), |t| format!("{}.{t}", p.interface));
        if name.len() > 15 {
            add(format!("{path}.vlan_id"), "interface_name_too_long");
        }
        match n.mode {
            AddressMode::Dhcp => {
                n.address.clear();
                n.prefix = 0;
                n.gateway.clear();
                n.dns.clear();
                continue;
            }
            AddressMode::Static => (),
            _ => {
                add(format!("{path}.mode"), "invalid_mode");
                continue;
            }
        }
        if !(8..=30).contains(&n.prefix) {
            add(format!("{path}.prefix"), "prefix_out_of_range");
        }
        let address = n.address.parse::<Ipv4Addr>();
        match address {
            Err(_) => add(format!("{path}.address"), "invalid_ipv4_address"),
            Ok(a) => {
                if !unicast(a) {
                    add(format!("{path}.address"), "unusable_unicast_address");
                }
                if (8..=30).contains(&n.prefix) {
                    let (low, high) = subnet(a, n.prefix);
                    if u32::from(a) == low || u32::from(a) == high {
                        add(format!("{path}.address"), "network_or_broadcast_address");
                    }
                    if subnets.iter().any(|(l, h)| low <= *h && *l <= high) {
                        add(format!("{path}.address"), "subnets_overlap");
                    }
                    subnets.push((low, high));
                    if !n.gateway.is_empty() {
                        match n.gateway.parse::<Ipv4Addr>() {
                            Err(_) => add(format!("{path}.gateway"), "invalid_gateway"),
                            Ok(g) => {
                                if u32::from(g) < low || u32::from(g) > high {
                                    add(format!("{path}.gateway"), "gateway_outside_subnet");
                                }
                                if !unicast(g)
                                    || g == a
                                    || u32::from(g) == low
                                    || u32::from(g) == high
                                {
                                    add(format!("{path}.gateway"), "unusable_gateway");
                                }
                            }
                        }
                    }
                }
            }
        }
        if n.id != p.roles.management && (!n.gateway.is_empty() || !n.dns.is_empty()) {
            add(format!("{path}.gateway"), "gateway_dns_management_only");
        }
        if n.dns.len() > 3 {
            add(format!("{path}.dns"), "invalid_dns_list");
        }
        for (j, d) in n.dns.iter().enumerate() {
            match d.parse::<Ipv4Addr>() {
                Err(_) => add(format!("{path}.dns[{j}]"), "invalid_dns_address"),
                Ok(a) if !unicast(a) => add(format!("{path}.dns[{j}]"), "unusable_dns_address"),
                _ => (),
            }
        }
    }
    for (role, id) in [
        ("management", &p.roles.management),
        ("ndi", &p.roles.ndi),
        ("audio", &p.roles.audio),
    ] {
        if !ids.contains(id) {
            add(
                format!("network.roles.{role}"),
                "assignment_requires_replacement",
            );
        }
    }
    if errors.is_empty() {
        Ok(p)
    } else {
        Err(ApiError::invalid(errors))
    }
}

/// Bound bytes before parsing. The shared model rejects unknown keys and types.
pub fn parse(bytes: &[u8]) -> Result<NetworkProfiles> {
    if bytes.len() > MAX_MESSAGE {
        return Err(invalid("network", "message_too_large"));
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| invalid("network", "invalid_document"))?;
    shape(&value)?;
    serde_json::from_value(value).map_err(|_| invalid("network", "invalid_type"))
}

fn shape(v: &Value) -> Result<()> {
    let mut errors = Vec::new();
    // `optional` keys may be absent; any other key outside `names` is refused.
    fn keys(
        v: &Value,
        path: &str,
        names: &[&str],
        optional: &[&str],
        reason: &str,
        errors: &mut Vec<FieldError>,
    ) {
        let Some(m) = v.as_object() else {
            errors.push(FieldError::new(path, "invalid_type"));
            return;
        };
        for key in m
            .keys()
            .filter(|k| !names.contains(&k.as_str()) && !optional.contains(&k.as_str()))
        {
            errors.push(FieldError::new(format!("{path}.{key}"), reason));
        }
        for key in names.iter().filter(|k| !m.contains_key(**k)) {
            errors.push(FieldError::new(format!("{path}.{key}"), reason));
        }
    }
    keys(
        v,
        "network",
        &[
            "interface",
            "backend",
            "networks",
            "roles",
            "confirm_policy",
        ],
        &[],
        "invalid_document",
        &mut errors,
    );
    keys(
        &v["roles"],
        "network.roles",
        &["management", "ndi", "audio"],
        &[],
        "invalid_document",
        &mut errors,
    );
    if let Some(rows) = v["networks"].as_array() {
        for (i, row) in rows.iter().enumerate() {
            let p = format!("network.networks[{i}]");
            keys(
                row,
                &p,
                &[
                    "id", "name", "vlan_id", "mode", "address", "prefix", "gateway", "dns",
                ],
                &["dns_search"],
                "invalid_profile",
                &mut errors,
            );
            // NE-2.12a: a missing key means empty; a present one is a bounded list.
            if let Some(search) = row.get("dns_search") {
                match search.as_array() {
                    Some(a) if a.len() <= ndimon_types::MAX_DNS_SEARCH => {
                        for (j, d) in a.iter().enumerate() {
                            if !d.is_string() {
                                errors.push(FieldError::new(
                                    format!("{p}.dns_search[{j}]"),
                                    "invalid_dns_search_domain",
                                ));
                            }
                        }
                    }
                    _ => errors.push(FieldError::new(
                        format!("{p}.dns_search"),
                        "invalid_dns_search_list",
                    )),
                }
            }
            if !row["vlan_id"].is_null() && row["vlan_id"].as_u64().is_none() {
                errors.push(FieldError::new(format!("{p}.vlan_id"), "invalid_vlan_id"));
            }
            if row["prefix"].as_u64().is_none() {
                errors.push(FieldError::new(format!("{p}.prefix"), "prefix_not_integer"));
            }
            for field in ["id", "name", "address", "gateway"] {
                if !row[field].is_string() {
                    errors.push(FieldError::new(format!("{p}.{field}"), "invalid_type"));
                }
            }
            if ![Some("dhcp"), Some("static")].contains(&row["mode"].as_str()) {
                errors.push(FieldError::new(format!("{p}.mode"), "invalid_mode"));
            }
            if row["dns"].as_array().is_none_or(|a| a.len() > 3) {
                errors.push(FieldError::new(format!("{p}.dns"), "invalid_dns_list"));
            }
            if let Some(dns) = row["dns"].as_array() {
                for (j, d) in dns.iter().enumerate() {
                    if !d.is_string() {
                        errors.push(FieldError::new(
                            format!("{p}.dns[{j}]"),
                            "invalid_dns_address",
                        ));
                    }
                }
            }
        }
    } else {
        errors.push(FieldError::new("network.networks", "invalid_type"));
    }
    if ![Some("token_only"), Some("require_management_address")]
        .contains(&v["confirm_policy"].as_str())
    {
        errors.push(FieldError::new("network.confirm_policy", "invalid_policy"));
    }
    for role in ["management", "ndi", "audio"] {
        if !v["roles"][role].is_string() {
            errors.push(FieldError::new(
                format!("network.roles.{role}"),
                "invalid_type",
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::invalid(errors))
    }
}

/// U1 semantic wire adapter. Revision/version are the protocol envelope; every
/// row is immediately converted into the sole shared NetworkProfile type.
pub fn import_semantic(bytes: &[u8]) -> Result<(u64, NetworkProfiles)> {
    if bytes.len() > MAX_MESSAGE {
        return Err(invalid("network", "message_too_large"));
    }
    let v: Value =
        serde_json::from_slice(bytes).map_err(|_| invalid("network", "invalid_document"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| invalid("network", "invalid_document"))?;
    let allowed = [
        "version",
        "revision",
        "interface",
        "backend",
        "networks",
        "management",
        "ndi",
        "audio",
        "policies",
    ];
    if obj.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(invalid("network", "invalid_document"));
    }
    if v["version"].as_u64() != Some(1) {
        return Err(invalid("network.version", "invalid_version_revision"));
    }
    let revision = v["revision"]
        .as_u64()
        .ok_or_else(|| invalid("network.revision", "invalid_version_revision"))?;
    let policy = match v.get("policies") {
        None => "token_only",
        Some(Value::Object(p)) if p.len() == 1 => p
            .get("confirm")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("network.policies.confirm", "invalid_policy"))?,
        _ => return Err(invalid("network.policies", "invalid_policy")),
    };
    let rows = v["networks"]
        .as_array()
        .ok_or_else(|| invalid("network.networks", "invalid_type"))?;
    let mut converted = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let path = format!("network.networks[{i}]");
        // NE-2.12a: `dns_search` sits beside `ipv4` and may be omitted.
        if row.as_object().is_none_or(|m| {
            ["id", "name", "vlan_id", "ipv4"]
                .iter()
                .any(|k| !m.contains_key(*k))
                || m.keys()
                    .any(|k| !["id", "name", "vlan_id", "ipv4", "dns_search"].contains(&k.as_str()))
        }) {
            return Err(invalid(&path, "invalid_profile"));
        }
        let ipv4 = row["ipv4"]
            .as_object()
            .ok_or_else(|| invalid(&format!("{path}.ipv4"), "invalid_ipv4"))?;
        let dhcp = ipv4.get("mode").and_then(Value::as_str) == Some("dhcp");
        let allowed = if dhcp {
            &["mode"][..]
        } else {
            &["mode", "address", "prefix", "gateway", "dns"][..]
        };
        if ipv4.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(invalid(&format!("{path}.ipv4"), "invalid_ipv4"));
        }
        let mut flat = serde_json::json!({"id":row["id"],"name":row["name"],"vlan_id":row["vlan_id"],"mode":row["ipv4"]["mode"],
            "address":if dhcp { Value::String(String::new()) } else { row["ipv4"]["address"].clone() },
            "prefix":if dhcp { Value::from(0) } else { row["ipv4"]["prefix"].clone() },
            "gateway":if dhcp || row["ipv4"]["gateway"].is_null() { Value::String(String::new()) } else { row["ipv4"]["gateway"].clone() },
            "dns":if dhcp { serde_json::json!([]) } else { row["ipv4"].get("dns").cloned().unwrap_or_else(|| serde_json::json!([])) }});
        if let Some(search) = row.get("dns_search") {
            flat["dns_search"] = search.clone();
        }
        converted.push(flat);
    }
    let flat = serde_json::json!({"interface":v["interface"],"backend":v["backend"],"networks":converted,
        "roles":{"management":v["management"],"ndi":v["ndi"],"audio":v["audio"]},"confirm_policy":policy});
    let p = parse(&serde_json::to_vec(&flat).map_err(|_| invalid("network", "invalid_document"))?)?;
    Ok((revision, validate(&p, &p)?))
}

/// Versioned old-helper adapter, with no second document model.
pub fn import_legacy(bytes: &[u8]) -> Result<(u64, NetworkProfiles)> {
    if bytes.len() > MAX_MESSAGE {
        return Err(invalid("network", "message_too_large"));
    }
    let mut v: Value =
        serde_json::from_slice(bytes).map_err(|_| invalid("network", "invalid_document"))?;
    let m = v
        .as_object_mut()
        .ok_or_else(|| invalid("network", "invalid_document"))?;
    let allowed = [
        "version",
        "revision",
        "interface",
        "backend",
        "networks",
        "management",
        "ndi",
        "tessera",
    ];
    if m.len() != allowed.len() || m.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(invalid("network", "invalid_document"));
    }
    if m.remove("version").and_then(|v| v.as_u64()) != Some(1) {
        return Err(invalid("network.version", "invalid_version_revision"));
    }
    let revision = m
        .remove("revision")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| invalid("network.revision", "invalid_version_revision"))?;
    if revision > 9_007_199_254_740_991 {
        return Err(invalid("network.revision", "invalid_version_revision"));
    }
    let roles = serde_json::json!({"management":m.remove("management"),"ndi":m.remove("ndi"),"audio":m.remove("tessera")});
    m.insert("roles".into(), roles);
    m.insert("confirm_policy".into(), Value::String("token_only".into()));
    if let Some(rows) = m.get("networks").and_then(Value::as_array) {
        for (i, r) in rows.iter().enumerate() {
            if r["address"].as_str().is_none_or(|s| s.len() > 15)
                || r["gateway"].as_str().is_none_or(|s| s.len() > 15)
                || r["prefix"].as_u64().is_none_or(|p| p > 30)
                || r["dns"].as_array().is_none_or(|ds| {
                    ds.len() > 3 || ds.iter().any(|d| d.as_str().is_none_or(|s| s.len() > 15))
                })
                // NE-2.12a: optional; the list shape is judged by `parse`.
                || r["dns_search"]
                    .as_array()
                    .is_some_and(|ds| ds.iter().any(|d| d.as_str().is_some_and(|s| s.len() > 253)))
            {
                return Err(invalid(
                    &format!("network.networks[{i}]"),
                    "invalid_profile",
                ));
            }
        }
    }
    let p = parse(&serde_json::to_vec(&v).map_err(|_| invalid("network", "invalid_document"))?)?;
    Ok((revision, validate(&p, &p)?))
}
