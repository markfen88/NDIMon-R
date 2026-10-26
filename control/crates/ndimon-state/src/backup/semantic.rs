//! Pure schema-1 semantic checks retained from Backup.js and Network.js.
use serde_json::{Value, json};
use std::collections::BTreeSet;
pub const SPLASH_KEYS: &[&str] = &[
    "bg_idle",
    "bg_live",
    "accent_idle",
    "accent_live",
    "logo_path",
    "logo_x_pct",
    "logo_y_pct",
    "logo_w_pct",
    "text_idle",
    "text_live",
    "text_height_pct",
    "backdrop",
    "show_signal_text",
    "show_device_name",
    "show_device_url",
    "show_sources_available",
];
fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
fn ipv4(s: &str) -> Option<u32> {
    let parts = s.split('.').collect::<Vec<_>>();
    if parts.len() != 4 {
        return None;
    }
    let mut value = 0;
    for p in parts {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n = p.parse::<u32>().ok()?;
        if n > 255 {
            return None;
        }
        value = value << 8 | n;
    }
    Some(value)
}
fn endpoint(s: &str) -> bool {
    let mut parts = s.split(':');
    parts
        .next()
        .unwrap_or("")
        .parse::<std::net::Ipv4Addr>()
        .is_ok()
        && parts.next().is_none_or(|p| {
            p.len() <= 5 && !p.starts_with('0') && p.parse::<u16>().is_ok_and(|n| n > 0)
        })
        && parts.next().is_none()
}
fn same_subnet(a: &str, b: &str, p: u64) -> bool {
    match (ipv4(a), ipv4(b)) {
        (Some(a), Some(b)) if p <= 32 => p == 0 || a >> (32 - p) == b >> (32 - p),
        _ => false,
    }
}
/// NE-2.12a: DHCP and static management rows may both carry search domains.
fn search_domains(v: &Value) -> Result<(), String> {
    let Some(list) = v.get("dns_search") else {
        return Ok(());
    };
    if list.as_array().is_none_or(|a| {
        a.len() > ndimon_types::MAX_DNS_SEARCH
            || a.iter().any(|d| {
                !d.as_str()
                    .is_some_and(ndimon_types::valid_dns_search_domain)
            })
    }) {
        return Err("dns_search must be at most 6 host names".into());
    }
    Ok(())
}
fn normalize(v: &Value) -> Result<(), String> {
    if v["mode"] == "dhcp" {
        return Ok(());
    }
    if v["mode"] != "static" {
        return Err("mode must be dhcp or static".into());
    }
    let address = text(v, "address").trim();
    let gateway = text(v, "gateway").trim();
    let a = ipv4(address).ok_or("address is not a valid IPv4 address")?;
    let prefix = v["prefix"]
        .as_u64()
        .ok_or("prefix must be between 8 and 30")?;
    if !(8..=30).contains(&prefix) {
        return Err("prefix must be between 8 and 30".into());
    }
    if a == 0 || a >> 24 == 127 || a >> 16 == 0xa9fe || a >> 28 == 0xe {
        return Err("address is not a usable unicast address".into());
    }
    let mask = u32::MAX << (32 - prefix);
    let host = a & !mask;
    if host == 0 || host == !mask {
        return Err("address is the network or broadcast address".into());
    }
    if !gateway.is_empty() && ipv4(gateway).is_none() {
        return Err("gateway is not a valid IPv4 address".into());
    }
    if !gateway.is_empty() && !same_subnet(address, gateway, prefix) {
        return Err("gateway is not inside the address subnet".into());
    }
    if v["dns"].as_array().is_some_and(|a| {
        a.len() > 3
            || a.iter().any(|d| {
                !d.as_str().unwrap_or("").trim().is_empty()
                    && ipv4(d.as_str().unwrap_or("").trim()).is_none()
            })
    }) {
        return Err("dns must be at most 3 IPv4 addresses".into());
    }
    Ok(())
}
pub fn check(s: &mut Value) -> Result<(), String> {
    if let Some(hosts) = s
        .pointer_mut("/device/web_allowed_hosts")
        .and_then(Value::as_array_mut)
    {
        for host in hosts {
            let value = host.as_str().unwrap_or("");
            if !ndimon_types::valid_web_hostname(value) {
                return Err("Additional host names must be DNS names without wildcards, ports or IP literals".into());
            }
            *host = json!(value.to_ascii_lowercase());
        }
    }
    if let Some(audio) = s.get("audio") {
        audio_check(&audio["settings"])?;
    }
    if let Some(d) = s.get_mut("discovery") {
        if let Some(v) = d.get_mut("NDIDisServIP") {
            let parts = v
                .as_str()
                .unwrap_or("")
                .trim()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>();
            if !parts.iter().all(|p| endpoint(p)) {
                return Err(
                    "Discovery Server must be IPv4 with an optional port from 1 to 65535".into(),
                );
            }
            *v = json!(parts.join(","));
        }
        if let Some(v) = d.get_mut("ndi_groups") {
            let raw = v.as_str().unwrap_or("").trim();
            if raw.chars().any(|c| c <= '\u{1f}') {
                return Err("NDI groups contain control characters".into());
            }
            *v = json!(if raw.is_empty() { "public" } else { raw });
        }
        if let Some(v) = d.get_mut("off_subnet_ips") {
            let parts = v
                .as_str()
                .unwrap_or("")
                .trim()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>();
            if !parts.iter().all(|p| ipv4(p).is_some()) {
                return Err("Extra IPs must be comma-separated IPv4 addresses".into());
            }
            *v = json!(parts.join(","));
        }
    }
    for p in s["presets"]["presets"].as_array().into_iter().flatten() {
        let ip = text(p, "ip");
        if !ip.is_empty() && !endpoint(ip) {
            return Err("a preset IP is not a valid IPv4 endpoint".into());
        }
    }
    for p in s["outputs"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, v)| v)
    {
        let ip = text(p, "SourceIP");
        if !ip.is_empty() && !endpoint(ip) {
            return Err("an output source IP is not a valid IPv4 endpoint".into());
        }
        if text(p, "SourceName").chars().any(|c| c <= '\u{1f}') {
            return Err("a source name contains control characters".into());
        }
    }
    if let Some(splash) = s.pointer_mut("/display/splash") {
        let mut clean = json!({});
        for key in SPLASH_KEYS {
            if let Some(v) = splash.get(*key) {
                clean[*key] = v.clone();
            }
        }
        for key in ["bg_idle", "bg_live", "accent_idle", "accent_live"] {
            if let Some(v) = clean.get(key) {
                let value = v.as_str().unwrap_or("");
                if !value.is_empty()
                    && !(value.len() == 7
                        && value.starts_with('#')
                        && value[1..].bytes().all(|b| b.is_ascii_hexdigit()))
                {
                    return Err(format!("splash colour {key} must be #RRGGBB"));
                }
            }
        }
        if clean
            .get("backdrop")
            .is_some_and(|v| v != "gradient" && v != "flat")
        {
            return Err("splash backdrop must be one of gradient, flat".into());
        }
        *splash = clean;
    }
    let n = &s["network"];
    if n["mode"] == "static" {
        normalize(n)?;
    }
    if let Some(p) = n.get("profiles") {
        let mut ids = BTreeSet::new();
        let mut tags = BTreeSet::new();
        for row in p["networks"].as_array().into_iter().flatten() {
            if !ids.insert(text(row, "id")) || !tags.insert(row["vlan_id"].to_string()) {
                return Err("Duplicate network identifier or VLAN".into());
            }
        }
        for role in ["management", "ndi", "tessera"] {
            if !ids.contains(text(p, role)) {
                return Err(format!("{role} requires a replacement network"));
            }
        }
        for row in p["networks"].as_array().into_iter().flatten() {
            search_domains(row)?;
            normalize(row)?;
        }
    } else if let Some(vlans) = n["vlans"].as_array() {
        let mut ids = BTreeSet::new();
        for v in vlans {
            let id = v["id"].as_u64().unwrap_or(0);
            if !ids.insert(id) {
                return Err("duplicate VLAN id".into());
            }
            let interface = text(n, "interface");
            if !interface.is_empty() && format!("{interface}.{id}").len() > 15 {
                return Err("VLAN interface name must fit within 15 characters".into());
            }
            if !text(v, "gateway").is_empty() && v["allow_gateway"] != true {
                return Err("gateway requires allow_gateway: true".into());
            }
            normalize(v)?;
            if v["mode"] == "static" {
                let p = v["prefix"]
                    .as_u64()
                    .unwrap_or(0)
                    .min(n["prefix"].as_u64().unwrap_or(0));
                if p != 0 && same_subnet(text(v, "address"), text(n, "address"), p) {
                    return Err("overlaps base subnet".into());
                }
            }
        }
    }
    Ok(())
}
fn audio_check(v: &Value) -> Result<(), String> {
    let mut errors = Vec::new();
    let sources = &v["sources"];
    let sinks = &v["sinks"];
    let master = text(&v["engine"], "master");
    if v["version"] == 1 {
        if sources["ndi"]["type"] != "ndi" {
            errors.push("sources.ndi.type must be ndi".into());
        }
        for (id, s) in sources.as_object().into_iter().flatten() {
            if id != "ndi" && s["type"] != "alsa" {
                errors.push(format!("sources.{id}.type must be alsa"));
            }
        }
        if sinks
            .get(master)
            .is_none_or(|s| s["enabled"] != true || s["type"] != "alsa")
        {
            errors.push("engine.master must name an enabled ALSA sink".into());
        }
    } else {
        let populated = [sources, sinks, &v["routes"]]
            .iter()
            .any(|a| a.as_object().is_some_and(|o| !o.is_empty()));
        if populated && sinks.get(master).is_none_or(|s| s["enabled"] != true)
            || !populated && !master.is_empty()
        {
            errors.push("engine.master must name an enabled ALSA sink".into());
        }
        for (kind, nodes) in [("source", sources), ("sink", sinks)] {
            for (name, n) in nodes.as_object().into_iter().flatten() {
                if n.get("channel_count").is_some_and(|c| c != &n["channels"]) {
                    errors.push(format!("{kind} {name}: channel_count must equal channels"));
                }
                if n["channel_start"].as_u64().unwrap_or(1) + n["channels"].as_u64().unwrap_or(0)
                    > 65536
                {
                    errors.push(format!("{kind} {name}: channel range overflows"));
                }
            }
        }
        let sinks = sinks
            .as_object()
            .expect("schema sinks")
            .iter()
            .collect::<Vec<_>>();
        for i in 0..sinks.len() {
            for j in i + 1..sinks.len() {
                let (a, x) = sinks[i];
                let (b, y) = sinks[j];
                if x["alsa_pcm"] == y["alsa_pcm"]
                    && (x["alsa_pcm"] != "auto" || x["resource"] == y["resource"])
                    || overlap(x, y)
                {
                    errors.push(format!(
                        "sinks {a}/{b} would create overlapping physical writers"
                    ));
                }
            }
        }
        let mut contributions = std::collections::BTreeMap::new();
        let mut inputs: std::collections::BTreeMap<u64, Vec<(&str, &Value)>> =
            std::collections::BTreeMap::new();
        for (name, r) in v["routes"].as_object().into_iter().flatten() {
            let ch = r["output"].as_u64().unwrap_or(0);
            let source = text(r, "source");
            let sink = text(r, "sink");
            let s = &sources[source];
            let t = &v["sinks"][sink];
            if v["outputs"].get(ch.to_string()).is_none() || s.is_null() || t.is_null() {
                errors.push(format!(
                    "route {name} references missing output/source/sink"
                ));
                continue;
            }
            if r["enabled"] != true
                || v["outputs"][ch.to_string()]["enabled"] != true
                || s["enabled"] != true
                || t["enabled"] != true
            {
                continue;
            }
            let selected = inputs.entry(ch).or_default();
            for (id, other) in selected.iter() {
                if *id != source && (identity(other) == identity(s) || overlap(other, s)) {
                    errors.push(format!(
                        "output {ch} has overlapping source selections {id}/{source}"
                    ));
                }
            }
            if !selected.iter().any(|(id, _)| *id == source) {
                selected.push((source, s));
            }
            let key = format!("{}->{}", identity(s), identity(t));
            if let Some(prior) = contributions.get(&key) {
                errors.push(format!(
                    "route {name} duplicates source/sink contribution from output {prior}"
                ));
            } else {
                contributions.insert(key, ch);
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors.join("; "));
    }
    if v["version"] == 2 {
        for output in v["outputs"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(_, v)| v)
        {
            if let Some(settings) = output.pointer("/legacy/settings") {
                let truthy = match settings {
                    Value::Null => false,
                    Value::Bool(v) => *v,
                    Value::Number(v) => v.as_f64() != Some(0.0),
                    Value::String(v) => !v.is_empty(),
                    _ => true,
                };
                if !truthy {
                    continue;
                }
                let schema = &super::schema::schema()["properties"]["sections"]["properties"]["audio"]
                    ["properties"]["settings"]["anyOf"][0];
                let errors = super::schema::errors(schema, settings)
                    .into_iter()
                    .map(|e| {
                        e.strip_prefix("/ ")
                            .map(|tail| format!(" {tail}"))
                            .unwrap_or(e)
                    })
                    .collect::<Vec<_>>();
                let result = if errors.is_empty() {
                    audio_check(settings)
                } else {
                    Err(errors.join("; "))
                };
                if let Err(error) = result {
                    return Err(format!("invalid legacy projection metadata; {error}"));
                }
            }
        }
    }
    Ok(())
}

fn overlap(a: &Value, b: &Value) -> bool {
    let a0 = a["channel_start"].as_u64().unwrap_or(1);
    let b0 = b["channel_start"].as_u64().unwrap_or(1);
    !text(a, "resource").is_empty()
        && a["resource"] == b["resource"]
        && a0
            < b0 + b["channel_count"]
                .as_u64()
                .unwrap_or_else(|| b["channels"].as_u64().unwrap_or(0))
        && b0
            < a0 + a["channel_count"]
                .as_u64()
                .unwrap_or_else(|| a["channels"].as_u64().unwrap_or(0))
}
fn identity(v: &Value) -> String {
    if v["type"] == "ndi" {
        format!("ndi:{}", v["output"])
    } else if !text(v, "resource").is_empty() {
        format!(
            "{}:{}:{}",
            text(v, "resource"),
            v["channel_start"].as_u64().unwrap_or(1),
            v["channel_count"]
                .as_u64()
                .unwrap_or_else(|| v["channels"].as_u64().unwrap_or(0))
        )
    } else {
        format!("alsa:{}", text(v, "alsa_pcm"))
    }
}
