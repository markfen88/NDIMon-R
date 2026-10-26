//! Catalog shaping mirrors api/audio-endpoints.js. Inputs are observations;
//! readiness never changes configured intent or removes a saved endpoint.
use ndimon_types::*;
use serde_json::{Map, Value, json};

fn mapping(channels: u64, input: bool, ndi: bool) -> &'static str {
    match channels {
        1 => {
            if input {
                "Mono → stereo: the same sample feeds both channels"
            } else {
                "Mono destination: default channel mapping"
            }
        }
        2 => {
            if ndi {
                "NDI stereo downmix"
            } else {
                "Stereo channel pair"
            }
        }
        _ => {
            if input {
                "Multichannel: matching channels; 6+ channels downmix when feeding stereo"
            } else {
                "Multichannel destination: matching channel indices"
            }
        }
    }
}
fn txt<'a>(v: &'a Value, key: &str, fallback: &'a str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or(fallback)
}
fn count(v: Option<u64>, default: u64) -> u64 {
    v.filter(|v| (1..=16).contains(v)).unwrap_or(default)
}
fn push(entries: &mut Vec<Value>, entry: Value) {
    if !entries.iter().any(|e| e["id"] == entry["id"]) {
        entries.push(entry);
    }
}
fn reason(entry: &mut Value, value: &str) {
    entry["ready"] = json!(value.is_empty());
    if value.is_empty() {
        entry.as_object_mut().expect("endpoint").remove("reason");
    } else {
        entry["reason"] = json!(value);
    }
}
pub fn build(
    outputs: &Value,
    tessera_status: Option<&Value>,
    tessera_enabled: bool,
    audio: &Audio,
) -> Value {
    let mut inputs = Vec::new();
    let mut destinations = Vec::new();
    let sources = audio
        .sources
        .iter()
        .chain(
            audio
                .outputs
                .values()
                .flat_map(|o| o.definitions.sources.iter()),
        )
        .collect::<Vec<_>>();
    let sinks = audio
        .sinks
        .iter()
        .chain(
            audio
                .outputs
                .values()
                .flat_map(|o| o.definitions.sinks.iter()),
        )
        .collect::<Vec<_>>();
    let main = tessera_status
        .and_then(|s| s["devices"].as_array())
        .and_then(|a| a.iter().find(|d| d["key"] == "main"));
    let channels_for =
        |d: &str| main.and_then(|m| m.get(format!("{d}_channels")).and_then(Value::as_array));
    let tessera_reason = if !tessera_enabled {
        "Tessera plugin is disabled"
    } else if tessera_status.is_none() {
        "Tessera daemon is offline"
    } else if main.is_none() {
        "Tessera main device is unavailable"
    } else if main.is_some_and(|m| m["sample_rate"] != 48000) {
        "Tessera main device must use 48 kHz"
    } else {
        ""
    };
    let range_reason = |d: &str, start: u64, n: u64| {
        if !tessera_reason.is_empty() {
            tessera_reason
        } else if !(1..=65535).contains(&start)
            || !(1..=16).contains(&n)
            || start + n - 1 > channels_for(d).map_or(0, |a| a.len() as u64)
        {
            "Saved channel range is unavailable on the Tessera main device"
        } else {
            ""
        }
    };
    if let Some(outputs) = outputs.as_array() {
        for out in outputs {
            let id = out["ch_num"]
                .as_u64()
                .or_else(|| out["output_index"].as_u64().map(|v| v + 1));
            let Some(id) = id.filter(|id| (1..=8).contains(id)) else {
                continue;
            };
            let label = txt(out, "output_alias", "");
            let label = if label.is_empty() {
                txt(out, "connector", "")
            } else {
                label
            };
            let label = if label.is_empty() {
                format!("Output {id}")
            } else {
                label.into()
            };
            let channels = sources
                .iter()
                .find(|(_, s)| s.kind == EndpointType::Ndi && s.output == Some(id as u8))
                .map_or(2, |(_, s)| u64::from(s.channels));
            let mut ndi = json!({"id":format!("ndi-{id}"),"node_id":format!("ndi-{id}"),"label":format!("{label} · NDI{}",out["source_name"].as_str().filter(|s|!s.is_empty()).map(|s|format!(" · {s}")).unwrap_or_default()),"provider":"ndi","channels":channels,"rate":48000,"ready":false,"mapping":mapping(channels,true,true),"output":id});
            reason(
                &mut ndi,
                if out["connected"] != true {
                    "NDI source is not connected"
                } else if out["audio_available"] != true {
                    "NDI audio capture is unavailable"
                } else {
                    ""
                },
            );
            push(&mut inputs, ndi);
            let mut hdmi = json!({"id":format!("hdmi-{id}"),"node_id":format!("hdmi-{id}"),"label":format!("{label} · HDMI/DisplayPort · Stereo"),"provider":"hdmi","channels":2,"rate":48000,"ready":false,"mapping":mapping(2,false,false),"alsa_pcm":"auto","resource":format!("hdmi:{id}"),"channel_start":1,"channel_count":2});
            reason(
                &mut hdmi,
                if out["drm_ready"] == true {
                    ""
                } else {
                    "Display connector is not ready"
                },
            );
            push(&mut destinations, hdmi);
        }
    }
    for direction in ["rx", "tx"] {
        let target = if direction == "rx" {
            &mut inputs
        } else {
            &mut destinations
        };
        let channels = channels_for(direction);
        let n = channels.map_or(0, Vec::len) as u64;
        let mut ranges = Vec::new();
        if (1..=16).contains(&n) {
            ranges.push((1, n));
        }
        if let Some(channels) = channels {
            for (i, ch) in channels.iter().enumerate() {
                let start = ch["channel"].as_u64().unwrap_or(i as u64 + 1);
                if (1..=65535).contains(&start) {
                    ranges.push((start, 1));
                }
            }
        }
        for (start, n) in ranges {
            let id = TesseraEndpoint::new(
                if direction == "rx" {
                    Direction::Rx
                } else {
                    Direction::Tx
                },
                start as u32,
                n as u16,
            )
            .id();
            let names = channels
                .map(|a| {
                    a.iter()
                        .skip(start as usize - 1)
                        .take(n as usize)
                        .enumerate()
                        .map(|(i, c)| {
                            c["label"]
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .map(str::to_string)
                                .unwrap_or_else(|| {
                                    format!(
                                        "{} {}",
                                        direction.to_uppercase(),
                                        c["channel"].as_u64().unwrap_or(start + i as u64)
                                    )
                                })
                        })
                        .collect::<Vec<_>>()
                        .join(" / ")
                })
                .unwrap_or_default();
            let range = if n == 1 {
                format!("channel {start}")
            } else {
                format!("channels {start}–{}", start + n - 1)
            };
            let mut entry = json!({"id":id,"node_id":id,"label":format!("{} · {}{range} · {names} · {}",main.map(|m|txt(m,"name","Tessera main")).filter(|s|!s.is_empty()).unwrap_or("Tessera main"),if direction=="rx"{"Receive "}else{"Transmit "},if n==1{"Mono"}else if n==2{"Stereo"}else{"Multichannel"}),"provider":"tessera","channels":n,"rate":48000,"ready":false,"mapping":mapping(n,direction=="rx",false),"alsa_pcm":format!("ndimon_tessera_{direction}_{start}_{n}"),"resource":format!("tessera:main:{direction}"),"channel_start":start,"channel_count":n});
            reason(&mut entry, range_reason(direction, start, n));
            push(target, entry);
        }
    }
    for (input, nodes) in [
        (
            true,
            sources
                .into_iter()
                .map(|(id, s)| (id, ndimon_render::audio::source_json(s, false)))
                .collect::<Vec<_>>(),
        ),
        (
            false,
            sinks
                .into_iter()
                .map(|(id, s)| (id, ndimon_render::audio::sink_json(s, false)))
                .collect::<Vec<_>>(),
        ),
    ] {
        for (node_id, node) in nodes {
            let id = txt(&node, "endpoint", node_id);
            let target = if input {
                &mut inputs
            } else {
                &mut destinations
            };
            if target.iter().any(|e| e["id"] == id) {
                continue;
            }
            let channels = count(
                node["channels"].as_u64(),
                count(node["channel_count"].as_u64(), 2),
            );
            let start = node["channel_start"]
                .as_u64()
                .filter(|s| (1..=65535).contains(s))
                .unwrap_or(1);
            let n = count(node["channel_count"].as_u64(), channels);
            let mut entry = json!({"id":id,"node_id":node_id,"label":format!("Saved endpoint · {id}"),"provider":"unknown","channels":channels,"rate":48000,"ready":false,"reason":"Saved endpoint provider is unavailable","mapping":mapping(channels,input,false)});
            let resource = txt(&node, "resource", "");
            if input && node["type"] == "ndi" && node["output"].is_u64() {
                let canonical = format!("ndi-{}", node["output"]);
                if let Some(live) = target.iter().find(|e| e["id"] == canonical) {
                    merge(&mut entry, live);
                } else {
                    entry["provider"] = json!("ndi");
                    reason(&mut entry, "Saved NDI output is unavailable");
                }
                entry["node_id"] = json!(canonical);
                entry["channels"] = json!(channels);
                entry["output"] = node["output"].clone();
                entry["mapping"] = json!(mapping(channels, true, true));
            } else if let Some(suffix) = resource.strip_prefix("hdmi:") {
                let canonical = format!("hdmi-{suffix}");
                if let Some(live) = target.iter().find(|e| e["id"] == canonical) {
                    merge(&mut entry, live);
                } else {
                    reason(&mut entry, "Saved display output is unavailable");
                }
                entry["node_id"] = json!(canonical);
                entry["provider"] = json!("hdmi");
                entry["channels"] = json!(channels);
                entry["channel_start"] = json!(start);
                entry["channel_count"] = json!(n);
            } else if resource == "tessera:main:rx" || resource == "tessera:main:tx" {
                let d = if resource.ends_with("rx") { "rx" } else { "tx" };
                let canonical = format!("tessera-{d}-{start}-{n}");
                if let Some(live) = target.iter().find(|e| e["id"] == canonical) {
                    merge(&mut entry, live);
                }
                entry["node_id"] = json!(canonical);
                entry["provider"] = json!("tessera");
                entry["channels"] = json!(channels);
                entry["channel_start"] = json!(start);
                entry["channel_count"] = json!(n);
                entry["mapping"] = json!(mapping(channels, input, false));
                reason(
                    &mut entry,
                    if input != (d == "rx") {
                        "Saved endpoint direction is invalid"
                    } else {
                        range_reason(d, start, n)
                    },
                );
            }
            entry["id"] = json!(id);
            for key in [
                "alsa_pcm",
                "resource",
                "channel_start",
                "channel_count",
                "output",
            ] {
                if let Some(v) = node.get(key) {
                    entry[key] = v.clone();
                }
            }
            push(target, entry);
        }
    }
    json!({"inputs":inputs,"destinations":destinations})
}
fn merge(a: &mut Value, b: &Value) {
    if let (Some(a), Some(b)) = (a.as_object_mut(), b.as_object()) {
        for (k, v) in b {
            a.insert(k.clone(), v.clone());
        }
    }
}
pub fn public(catalog: &Value, revision: u64) -> Value {
    let strip = |kind: &str| {
        catalog[kind]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|e| {
                        let mut result = Map::new();
                        for key in [
                            "id", "label", "provider", "channels", "rate", "ready", "reason",
                            "mapping", "output",
                        ] {
                            if let Some(v) = e.get(key) {
                                result.insert(key.into(), v.clone());
                            }
                        }
                        Value::Object(result)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    json!({"revision":revision,"inputs":strip("inputs"),"destinations":strip("destinations")})
}
