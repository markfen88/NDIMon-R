//! Legacy shape checks precede conversion; portable cross-field validation is
//! still performed by ndimon-types inside the actor and reports all errors.
use ndimon_types::FieldError;
use serde_json::Value;
pub fn identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
fn add(out: &mut Vec<FieldError>, field: &str, reason: &str) {
    out.push(FieldError::new(field, reason));
}
fn shape(v: &Value, path: &str, allowed: &[&str], required: &[&str], out: &mut Vec<FieldError>) {
    let Some(obj) = v.as_object() else {
        add(out, path, "must be object");
        return;
    };
    for key in obj.keys() {
        if !allowed.contains(&key.as_str()) {
            add(
                out,
                &format!("{path}/{key}"),
                "must NOT have additional properties",
            );
        }
    }
    for key in required {
        if !obj.contains_key(*key) {
            add(out, path, &format!("must have required property '{key}'"));
        }
    }
}
fn integer(v: &Value, path: &str, min: u64, max: u64, out: &mut Vec<FieldError>) {
    if !v.as_u64().is_some_and(|n| (min..=max).contains(&n)) {
        add(out, path, &format!("must be integer {min}..{max}"));
    }
}
fn boolean(v: &Value, path: &str, out: &mut Vec<FieldError>) {
    if !v.is_boolean() {
        add(out, path, "must be boolean");
    }
}
fn gain(v: &Value, path: &str, out: &mut Vec<FieldError>) {
    if !v.as_f64().is_some_and(|n| (-60.0..=12.0).contains(&n)) {
        add(out, path, "must be number -60..12");
    }
}
fn pcm(v: &Value, path: &str, out: &mut Vec<FieldError>) {
    if !v.as_str().is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 512
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b":_=,.+-".contains(&c))
    }) {
        add(out, path, "invalid ALSA PCM");
    }
}
fn endpoint(v: &Value, path: &str, out: &mut Vec<FieldError>) {
    if !v
        .as_str()
        .is_some_and(|s| !s.is_empty() && s.chars().count() <= 160)
    {
        add(out, path, "must be string of 1..160 characters");
    }
}
pub fn choices(v: &Value) -> Result<(), Vec<FieldError>> {
    let mut errors = Vec::new();
    choice(v, "", true, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
fn choice(v: &Value, path: &str, public: bool, out: &mut Vec<FieldError>) {
    if public {
        shape(
            v,
            path,
            &[
                "revision",
                "normalize_matrix",
                "enabled",
                "inputs",
                "destinations",
                "confirm",
                "expect_impact",
            ],
            &["enabled", "inputs", "destinations"],
            out,
        );
    }
    boolean(&v["enabled"], &format!("{path}/enabled"), out);
    if let Some(revision) = v.get("revision") {
        integer(
            revision,
            &format!("{path}/revision"),
            0,
            9_007_199_254_740_991,
            out,
        );
    }
    for kind in ["inputs", "destinations"] {
        let p = format!("{path}/{kind}");
        let Some(entries) = v[kind].as_array() else {
            add(out, &p, "must be array");
            continue;
        };
        if entries.len() > 32 {
            add(out, &p, "must NOT have more than 32 items");
        }
        for (i, e) in entries.iter().enumerate() {
            let p = format!("{p}/{i}");
            if kind == "inputs" {
                shape(
                    e,
                    &p,
                    &["endpoint", "gain_db", "delay_ms"],
                    &["endpoint", "gain_db", "delay_ms"],
                    out,
                );
                gain(&e["gain_db"], &format!("{p}/gain_db"), out);
            } else {
                shape(
                    e,
                    &p,
                    &["endpoint", "delay_ms"],
                    &["endpoint", "delay_ms"],
                    out,
                );
            }
            endpoint(&e["endpoint"], &format!("{p}/endpoint"), out);
            integer(&e["delay_ms"], &format!("{p}/delay_ms"), 0, 5000, out);
        }
    }
    for key in ["normalize_matrix", "confirm"] {
        if let Some(v) = v.get(key) {
            boolean(v, &format!("{path}/{key}"), out);
        }
    }
}
pub fn legacy(v: &Value) -> Result<(), Vec<FieldError>> {
    let mut errors = Vec::new();
    let v2 = v["version"] == 2;
    let allowed = if v2 {
        vec![
            "version",
            "revision",
            "engine",
            "sources",
            "sinks",
            "outputs",
            "routes",
            "video_delay_ms",
        ]
    } else {
        vec![
            "version",
            "revision",
            "engine",
            "sources",
            "sinks",
            "video_delay_ms",
        ]
    };
    let required = if v2 {
        vec![
            "version",
            "engine",
            "sources",
            "sinks",
            "outputs",
            "routes",
            "video_delay_ms",
        ]
    } else {
        vec!["version", "engine", "sources", "sinks", "video_delay_ms"]
    };
    shape(v, "", &allowed, &required, &mut errors);
    if v["version"] != 1 && !v2 {
        add(&mut errors, "/version", "must be equal to constant 1 or 2");
    }
    if let Some(r) = v.get("revision") {
        integer(r, "/revision", 0, 9_007_199_254_740_991, &mut errors);
    }
    shape(
        &v["engine"],
        "/engine",
        &[
            "enabled",
            "rate",
            "period_frames",
            "master",
            "memory_budget_mib",
        ],
        &["enabled", "rate", "period_frames", "master"],
        &mut errors,
    );
    boolean(&v["engine"]["enabled"], "/engine/enabled", &mut errors);
    integer(
        &v["engine"]["rate"],
        "/engine/rate",
        48000,
        48000,
        &mut errors,
    );
    if !v["engine"]["period_frames"]
        .as_u64()
        .is_some_and(|n| [128, 256, 512, 1024, 2048].contains(&n))
    {
        add(
            &mut errors,
            "/engine/period_frames",
            "must be equal to one of the allowed values",
        );
    }
    if !v["engine"]["master"]
        .as_str()
        .is_some_and(|s| identifier(s) || (v2 && s.is_empty()))
    {
        add(&mut errors, "/engine/master", "invalid master ID");
    }
    if let Some(n) = v["engine"].get("memory_budget_mib") {
        integer(n, "/engine/memory_budget_mib", 1, 512, &mut errors);
    }
    integer(
        &v["video_delay_ms"],
        "/video_delay_ms",
        0,
        5000,
        &mut errors,
    );
    for kind in ["sources", "sinks"] {
        let Some(nodes) = v[kind].as_object() else {
            add(&mut errors, &format!("/{kind}"), "must be object");
            continue;
        };
        if nodes.len() > 32 {
            add(
                &mut errors,
                &format!("/{kind}"),
                "must NOT have more than 32 properties",
            );
        }
        let reserved = if kind == "sources" { "ndi" } else { "hdmi" };
        if !v2 && !nodes.contains_key(reserved) {
            add(
                &mut errors,
                &format!("/{kind}"),
                &format!("must have required property '{reserved}'"),
            );
        }
        for (id, n) in nodes {
            let p = format!("/{kind}/{id}");
            if !identifier(id) {
                add(&mut errors, &p, "invalid endpoint ID");
            }
            let (allowed, required) = if v2 {
                (
                    vec![
                        "type",
                        "enabled",
                        "channels",
                        "output",
                        "alsa_pcm",
                        "endpoint",
                        "resource",
                        "channel_start",
                        "channel_count",
                    ],
                    vec!["type", "enabled", "channels"],
                )
            } else if kind == "sources" {
                (
                    vec![
                        "type", "enabled", "channels", "delay_ms", "gain_db", "alsa_pcm",
                    ],
                    vec!["type", "enabled", "channels", "delay_ms", "gain_db"],
                )
            } else {
                (
                    vec!["type", "enabled", "channels", "delay_ms", "alsa_pcm", "mix"],
                    vec!["type", "enabled", "channels", "delay_ms", "alsa_pcm", "mix"],
                )
            };
            shape(n, &p, &allowed, &required, &mut errors);
            boolean(&n["enabled"], &format!("{p}/enabled"), &mut errors);
            integer(&n["channels"], &format!("{p}/channels"), 1, 16, &mut errors);
            let ndi = n["type"] == "ndi";
            if n["type"] != "alsa" && !(kind == "sources" && ndi) {
                add(
                    &mut errors,
                    &format!("{p}/type"),
                    "must be allowed endpoint type",
                );
            }
            if kind == "sources" && !v2 && ((id == "ndi" && !ndi) || (id != "ndi" && ndi)) {
                add(
                    &mut errors,
                    &format!("{p}/type"),
                    "only sources.ndi may be NDI",
                );
            }
            if ndi {
                if n.get("alsa_pcm").is_some() {
                    add(&mut errors, &format!("{p}/alsa_pcm"), "NDI has no ALSA PCM");
                }
                if v2 {
                    integer(&n["output"], &format!("{p}/output"), 1, 8, &mut errors);
                }
            } else {
                pcm(&n["alsa_pcm"], &format!("{p}/alsa_pcm"), &mut errors);
                if n.get("output").is_some() {
                    add(
                        &mut errors,
                        &format!("{p}/output"),
                        "ALSA has no NDI output",
                    );
                }
            }
            if v2 {
                for key in ["endpoint", "resource"] {
                    if let Some(v) = n.get(key) {
                        if !v.as_str().is_some_and(|s| s.chars().count() <= 160) {
                            add(
                                &mut errors,
                                &format!("{p}/{key}"),
                                "must be string of at most 160 characters",
                            );
                        }
                    }
                }
            }
            if !v2 {
                integer(
                    &n["delay_ms"],
                    &format!("{p}/delay_ms"),
                    0,
                    5000,
                    &mut errors,
                );
                if kind == "sources" {
                    gain(&n["gain_db"], &format!("{p}/gain_db"), &mut errors);
                } else if let Some(mix) = n["mix"].as_object() {
                    for (id, g) in mix {
                        if !identifier(id) {
                            add(&mut errors, &format!("{p}/mix/{id}"), "invalid source ID");
                        }
                        gain(g, &format!("{p}/mix/{id}"), &mut errors);
                    }
                } else {
                    add(&mut errors, &format!("{p}/mix"), "must be object");
                }
            }
        }
    }
    if v2 {
        if let Some(outputs) = v["outputs"].as_object() {
            if outputs.len() > 8 {
                add(&mut errors, "/outputs", "at most 8 outputs");
            }
            for (id, o) in outputs {
                if !id.parse::<u8>().is_ok_and(|n| (1..=8).contains(&n)) {
                    add(&mut errors, &format!("/outputs/{id}"), "output must be1..8");
                }
                boolean(
                    &o["enabled"],
                    &format!("/outputs/{id}/enabled"),
                    &mut errors,
                );
                let mut selected = o.clone();
                if let Some(selected) = selected.as_object_mut() {
                    selected
                        .entry("inputs")
                        .or_insert_with(|| serde_json::json!([]));
                    selected
                        .entry("destinations")
                        .or_insert_with(|| serde_json::json!([]));
                }
                choice(&selected, &format!("/outputs/{id}"), false, &mut errors);
                if let Some(v) = o.get("legacy_matrix") {
                    boolean(v, &format!("/outputs/{id}/legacy_matrix"), &mut errors);
                }
                if o.get("legacy").is_some_and(|v| !v.is_object()) {
                    add(
                        &mut errors,
                        &format!("/outputs/{id}/legacy"),
                        "must be object",
                    );
                }
                if let Some(settings) = o.pointer("/legacy/settings") {
                    if let Err(nested) = legacy(settings) {
                        errors.extend(nested);
                    }
                }
            }
        } else {
            add(&mut errors, "/outputs", "must be object");
        }
        if let Some(routes) = v["routes"].as_object() {
            for (id, r) in routes {
                let p = format!("/routes/{id}");
                if !identifier(id) {
                    add(&mut errors, &p, "invalid route ID");
                }
                shape(
                    r,
                    &p,
                    &[
                        "output", "source", "sink", "enabled", "gain_db", "delay_ms", "mapping",
                    ],
                    &["output", "source", "sink", "enabled", "gain_db", "delay_ms"],
                    &mut errors,
                );
                integer(&r["output"], &format!("{p}/output"), 1, 8, &mut errors);
                boolean(&r["enabled"], &format!("{p}/enabled"), &mut errors);
                gain(&r["gain_db"], &format!("{p}/gain_db"), &mut errors);
                integer(
                    &r["delay_ms"],
                    &format!("{p}/delay_ms"),
                    0,
                    5000,
                    &mut errors,
                );
                for key in ["source", "sink"] {
                    if !r[key].as_str().is_some_and(identifier) {
                        add(&mut errors, &format!("{p}/{key}"), "invalid endpoint ID");
                    }
                }
                if r.get("mapping").is_some_and(|v| v != "default") {
                    add(&mut errors, &format!("{p}/mapping"), "must be default");
                }
            }
        } else {
            add(&mut errors, "/routes", "must be object");
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
