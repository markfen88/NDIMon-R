//! Schema-1 validation and the legacy restore review projection.
use super::{
    coordinator, difference,
    legacy::{canonical, checksum},
    semantic,
};
use crate::StateHandle;
use ndimon_types::Query;
use serde_json::{Value, json};

fn text<'a>(v: &'a Value, key: &str, default: &'a str) -> &'a str {
    v[key].as_str().filter(|s| !s.is_empty()).unwrap_or(default)
}
fn changed(a: &Value, b: &Value) -> bool {
    canonical(a).ok() != canonical(b).ok()
}
fn pick(v: &Value, keys: &[&str]) -> Value {
    let mut out = json!({});
    for k in keys {
        if let Some(v) = v.get(*k) {
            out[*k] = v.clone();
        }
    }
    out
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64() != Some(0.0),
        _ => true,
    }
}
pub fn validate(doc: Value) -> Result<Value, String> {
    if !doc.is_object() && !doc.is_array() {
        return Err("not a valid backup file".into());
    }
    // JSON.parse represents every number as a JS Number, including 1.0.
    let mut doc: Value = serde_json::from_str(&super::legacy::stringify(&doc, false)?)
        .map_err(|_| "not a valid backup file")?;
    if truthy(&doc["schema_version"]) && doc["schema_version"] != 1 {
        return Err("this backup is from a newer version; update this device first".into());
    }
    super::schema::check(&doc)?;
    if doc["checksum"] != checksum(&doc["sections"])? {
        return Err("this backup is damaged or was edited".into());
    }
    semantic::check(&mut doc["sections"])?;
    Ok(doc)
}
pub async fn review(h: &StateHandle, doc: Value, peer: &str) -> Result<Value, String> {
    let doc = validate(doc)?;
    let state = h.snapshot().await.map_err(|e| e.to_string())?;
    let mut current =
        coordinator::native_sections(h, &state, true, true).map_err(|e| e.to_string())?;
    if let Some(plugins) = doc["sections"]["plugins"].as_object() {
        // native_sections has Tessera's; the others come through their export
        // hooks, as an export of this device would have them (just the ones the
        // backup names; nothing is imported). The Plugins row compares these.
        let names = plugins.keys().cloned().collect::<Vec<_>>();
        let captured =
            super::plugins::capture_named(h.backup_plugins.as_ref(), &state, &names).await;
        for (name, section) in captured.as_object().into_iter().flatten() {
            if !current["plugins"].is_object() {
                current["plugins"] = json!({});
            }
            current["plugins"][name] = section.clone();
        }
    }
    let mut network = json!({"present":false,"needs_prompt":false,"warnings":[]});
    if let Some(backup) = doc["sections"].get("network") {
        let status = network_status(h, &current).await;
        network = difference::describe(&status, backup, peer.parse().ok());
        if network["needs_prompt"] == true
            && backup["mode"] == "static"
            && h.query(Query::CheckNetworkAddress {
                address: text(backup, "address", "").into(),
            })
            .await
            .ok()
            .is_some_and(|v| v["duplicate"] == true)
        {
            network["duplicate"] = json!(true);
            network["warnings"].as_array_mut().expect("network warnings").push(json!(format!("Another device on this network is already using {}. Applying this will cause an address conflict.",text(backup,"address",""))));
        }
    }
    let sections = rows(h, &current, &doc["sections"]).await;
    let extra = doc["sections"]["outputs"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(ch, _)| current["outputs"].get(*ch).is_none())
        .map(|(ch, _)| ch)
        .collect::<Vec<_>>();
    Ok(json!({"document":doc,"sections":sections,"network":network,"extra_outputs":extra}))
}
async fn network_status(h: &StateHandle, current: &Value) -> Value {
    let Ok(status) = h.query(Query::NetworkStatus).await else {
        return json!({"applicable":false,"mode":"unknown","reason":"status unavailable"});
    };
    // A device that has committed no network has no network section to export.
    // The backup is then compared with the profile the Network page proposes
    // (what the engine would adopt), which is what the page can apply.
    let section = if current["network"].is_object() {
        Some(current["network"].clone())
    } else {
        coordinator::network::proposal(&status).and_then(|proposed| {
            coordinator::network::section(
                &proposed,
                status["domain_revision"].as_u64().unwrap_or(0),
            )
            .ok()
        })
    };
    let Some(mut result) = section else {
        // Adoption refused (or no usable status): say why, not "status unavailable".
        let reason = status["refusal"]["reason"]
            .as_str()
            .unwrap_or("backend_not_certified");
        return json!({"applicable":false,"mode":"unknown","reason":reason});
    };
    result["applicable"] = json!(status["writable"] == true);
    result["reason"] = json!(if status["writable"] == true {
        ""
    } else {
        "backend_not_certified"
    });
    let management = result["profiles"]["management"].clone();
    if let Some(row) = status["networks"]
        .as_array()
        .and_then(|rows| rows.iter().find(|n| n["id"] == management))
    {
        if let Some(address) = row["addresses"].as_array().and_then(|a| a.first()) {
            result["live_address"] = address[0].clone();
            result["live_prefix"] = address[1].clone();
        }
    }
    result
}
async fn rows(h: &StateHandle, current: &Value, incoming: &Value) -> Vec<Value> {
    let mut rows = Vec::new();
    let mut add = |key: &str, label: &str, changed: bool, summary: String| {
        rows.push(
            json!({"key":key,"label":label,"changed":changed,"summary":summary,"selected":changed}),
        );
    };
    if let Some(outputs) = incoming.get("outputs") {
        let mut keys = current["outputs"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, _)| k.clone())
            .chain(
                outputs
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(k, _)| k.clone()),
            )
            .collect::<Vec<_>>();
        keys.sort();
        keys.dedup();
        let mut lines = Vec::new();
        for ch in keys {
            let a = &current["outputs"][&ch];
            let b = &outputs[&ch];
            let mut bits = Vec::new();
            for (key, label, default) in [
                ("SourceName", "source", ""),
                ("ScaleMode", "scale", "letterbox"),
            ] {
                if text(a, key, default) != text(b, key, default) {
                    bits.push(format!(
                        "{label} {} → {}",
                        text(
                            a,
                            key,
                            if default.is_empty() {
                                "(none)"
                            } else {
                                default
                            }
                        ),
                        text(
                            b,
                            key,
                            if default.is_empty() {
                                "(none)"
                            } else {
                                default
                            }
                        )
                    ));
                }
            }
            let a = a["rotation"].as_u64().unwrap_or(0);
            let b = b["rotation"].as_u64().unwrap_or(0);
            if a != b {
                bits.push(format!("rotation {a} → {b}"));
            }
            if !bits.is_empty() {
                lines.push(format!("Output {ch}: {}", bits.join(", ")));
            }
        }
        add(
            "outputs",
            "Outputs and sources",
            !lines.is_empty(),
            if lines.is_empty() {
                "No changes".into()
            } else {
                lines.join("\n")
            },
        );
    }
    if let Some(v) = incoming.get("presets") {
        add(
            "presets",
            "Source presets",
            super::legacy::stringify(&current["presets"]["presets"], false).ok()
                != super::legacy::stringify(&v["presets"], false).ok(),
            format!(
                "{} preset(s) in the backup",
                v["presets"].as_array().map_or(0, Vec::len)
            ),
        );
    }
    if let Some(v) = incoming.get("discovery") {
        let a = &current["discovery"];
        let change = [
            ("NDIDisServIP", ""),
            ("ndi_groups", "public"),
            ("off_subnet_ips", ""),
        ]
        .iter()
        .any(|(k, d)| text(a, k, d) != text(v, k, d));
        add(
            "discovery",
            "Discovery, groups, extra IPs",
            change,
            if change {
                format!(
                    "Groups “{}” → “{}”; Discovery Server “{}” → “{}”",
                    text(a, "ndi_groups", "public"),
                    text(v, "ndi_groups", "public"),
                    text(a, "NDIDisServIP", ""),
                    text(v, "NDIDisServIP", "")
                )
            } else {
                "No changes".into()
            },
        );
    }
    if let Some(v) = incoming.get("transport") {
        let a = text(&current["transport"], "Rxpm", "RUDP");
        let b = text(v, "Rxpm", "undefined");
        add("transport", "Transport", a != b, format!("{a} → {b}"));
    }
    if let Some(v) = incoming.get("display") {
        let a = &current["display"];
        let mut lines = Vec::new();
        let keys = semantic::SPLASH_KEYS
            .iter()
            .copied()
            .filter(|k| *k != "logo_path")
            .collect::<Vec<_>>();
        if changed(&pick(&a["splash"], &keys), &pick(&v["splash"], &keys)) {
            lines.push("Splash colours or overlay text differ".into());
        }
        let enabled = a["osd"]["enabled"] == true;
        let next = v["osd"]["enabled"] == true;
        let before = text(&a["osd"], "text", "");
        let after = text(&v["osd"], "text", "");
        if enabled != next || before != after {
            lines.push(format!(
                "OSD {} → {}{}",
                if enabled { "on" } else { "off" },
                if next { "on" } else { "off" },
                if before != after {
                    format!(
                        "; text “{}” → “{}”",
                        if before.is_empty() { "(none)" } else { before },
                        if after.is_empty() { "(none)" } else { after }
                    )
                } else {
                    String::new()
                }
            ));
        }
        let old = text(a, "logo_base64", "");
        let logo = if v["splash"]["logo_path"] == "" {
            Some("")
        } else {
            v["logo_base64"].as_str()
        };
        match logo {
            Some("") if !old.is_empty() => lines.push("Logo will be removed".into()),
            Some(logo) if !logo.is_empty() && old.is_empty() => {
                lines.push("Logo will be added".into())
            }
            Some(logo) if !logo.is_empty() && logo != old => {
                lines.push("Logo image differs".into())
            }
            _ => {}
        }
        add(
            "display",
            "Splash and OSD",
            !lines.is_empty(),
            if lines.is_empty() {
                "No changes".into()
            } else {
                lines.join("\n")
            },
        );
    }
    if let Some(v) = incoming.get("audio") {
        add(
            "audio",
            "Audio",
            changed(&current["audio"]["settings"], &v["settings"]),
            "Audio routing, gains and delays".into(),
        );
    }
    if let Some(v) = incoming.get("device") {
        let change = changed(&current["device"], v);
        add(
            "device",
            "Decoder, watchdog, NTP, reboot schedule",
            change,
            if change {
                "Settings differ".into()
            } else {
                "No changes".into()
            },
        );
    }
    if let Some(v) = incoming.get("identity") {
        let host = h.backup_io.hostname();
        let same = text(v, "hostname", "") == host;
        let change = text(&current["identity"], "ndi_recv_name", "")
            != text(v, "ndi_recv_name", "")
            || !same;
        let summary = format!(
            "Backup “{}”, this device “{}”.{}",
            text(v, "ndi_recv_name", text(v, "hostname", "")),
            text(&current["identity"], "ndi_recv_name", &host),
            if same {
                ""
            } else {
                " Leave this off when setting up a second device from this backup."
            }
        );
        rows.push(json!({"key":"identity","label":"Device name","changed":change,"selected":same && change,"summary":summary}));
    }
    if let Some(v) = incoming.get("plugins") {
        let names = v
            .as_object()
            .expect("schema plugins")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut unavailable = false;
        for name in &names {
            if name != "tessera" && !h.backup_plugins.available(name).await {
                unavailable = true;
                break;
            }
        }
        // Unchanged when every plugin section equals what this device exports; a
        // plugin whose hooks are unavailable cannot be compared and counts as changed.
        let change = unavailable
            || names
                .iter()
                .any(|name| changed(&current["plugins"][name], &v[name]));
        rows.push(json!({"key":"plugins","label":"Plugins","changed":change,"summary":if unavailable {format!("{}; restorable after plugin support is available",names.join(", "))}else if names.is_empty(){"No plugin settings".into()}else if change {names.join(", ")}else{format!("{}; no changes",names.join(", "))},"selected":change}));
    }
    if incoming.get("auth").is_some() {
        rows.push(json!({"key":"auth","label":"Admin password","changed":true,"selected":false,"summary":"Imports the password hash. You will be asked to log in again."}));
    }
    rows
}
