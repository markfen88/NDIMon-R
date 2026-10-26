//! Typed, hardware-independent legacy audio import and projection.
use ndimon_types::*;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

pub fn primary(audio: &Audio) -> OutputId {
    audio
        .outputs
        .iter()
        .find(|(_, o)| o.primary)
        .map(|(id, _)| *id)
        .unwrap_or(1)
}
fn object(value: &Value) -> impl Iterator<Item = (&String, &Value)> {
    value.as_object().into_iter().flat_map(|o| o.iter())
}
fn text<'a>(v: &'a Value, key: &str, fallback: &'a str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or(fallback)
}
fn num(v: &Value, key: &str, default: u32) -> Result<u32, String> {
    match v.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| format!("{key} must be an unsigned integer")),
    }
}
fn enabled(v: &Value) -> bool {
    v.get("enabled").and_then(Value::as_bool).unwrap_or(true)
}

pub fn source(value: &Value, id: &str, p: OutputId) -> Result<AudioSource, String> {
    let channels = u16::try_from(num(value, "channels", 2)?).map_err(|e| e.to_string())?;
    let mut node = match text(value, "type", "alsa") {
        "ndi" => AudioSource::ndi(
            u8::try_from(num(value, "output", u32::from(p))?).map_err(|e| e.to_string())?,
        ),
        "alsa" => AudioSource::alsa(text(value, "alsa_pcm", "auto"), channels),
        _ => return Err("source type must be ndi or alsa".into()),
    };
    node.channels = channels;
    node.channel_count = u16::try_from(num(value, "channel_count", u32::from(channels))?)
        .map_err(|e| e.to_string())?;
    node.enabled = enabled(value);
    node.channel_start = num(value, "channel_start", 1)?;
    node.gain_db = value.get("gain_db").and_then(Value::as_f64).unwrap_or(0.0);
    node.delay_ms = num(value, "delay_ms", 0)?;
    if node.kind != EndpointType::Ndi {
        node.endpoint = id.into();
    }
    node.endpoint = text(value, "endpoint", &node.endpoint).into();
    node.resource = text(value, "resource", "").into();
    if node.alsa_pcm.as_deref() == Some("ndimon_net_in") {
        if value.get("resource").is_none() {
            node.resource = "tessera:main:rx".into();
        }
        if value.get("endpoint").is_none() {
            node.endpoint = format!("tessera-rx-1-{channels}");
        }
    }
    Ok(node)
}
pub fn sink(value: &Value, id: &str, p: OutputId) -> Result<AudioSink, String> {
    let channels = u16::try_from(num(value, "channels", 2)?).map_err(|e| e.to_string())?;
    let mut node = AudioSink::new(text(value, "alsa_pcm", "auto"), channels);
    node.enabled = enabled(value);
    node.delay_ms = num(value, "delay_ms", 0)?;
    node.channel_start = num(value, "channel_start", 1)?;
    node.channel_count = u16::try_from(num(value, "channel_count", u32::from(channels))?)
        .map_err(|e| e.to_string())?;
    node.endpoint = text(value, "endpoint", id).into();
    node.resource = text(value, "resource", "").into();
    if id == "hdmi" && node.alsa_pcm == "auto" {
        if value.get("endpoint").is_none() {
            node.endpoint = format!("hdmi-{p}");
        }
        if value.get("resource").is_none() {
            node.resource = format!("hdmi:{p}");
        }
    }
    if node.alsa_pcm == "ndimon_net_out" {
        if value.get("endpoint").is_none() {
            node.endpoint = format!("tessera-tx-1-{channels}");
        }
        if value.get("resource").is_none() {
            node.resource = "tessera:main:tx".into();
        }
    }
    if let Some(mix) = value.get("mix") {
        node.mix = serde_json::from_value(mix.clone()).map_err(|e| e.to_string())?;
    }
    Ok(node)
}

pub fn import(doc: &Value, engine: AudioEngine, p: OutputId) -> Result<Audio, String> {
    let version = doc
        .get("version")
        .and_then(Value::as_u64)
        .ok_or("audio version missing")?;
    if version != 1 && version != 2 {
        return Err("unsupported audio version".into());
    }
    let mut audio = Audio::default();
    audio.engine = engine;
    let mut settings =
        serde_json::to_value(AudioEngineSettings::default()).map_err(|e| e.to_string())?;
    for (k, v) in object(&doc["engine"]) {
        settings[k] = v.clone();
    }
    audio.settings = serde_json::from_value(settings).map_err(|e| e.to_string())?;
    audio.video_delay_ms = num(doc, "video_delay_ms", 0)?;
    audio.sources.clear();
    audio.sinks.clear();
    audio.outputs.clear();
    audio.routes.clear();
    for (id, v) in object(&doc["sources"]) {
        audio.sources.insert(id.clone(), source(v, id, p)?);
    }
    for (id, v) in object(&doc["sinks"]) {
        let mut node = sink(v, id, p)?;
        // The schema-1 acceptance document retains the legacy primary alias's
        // default factor even when the v2 file omits inactive v1 mix metadata.
        if version == 2
            && id == "hdmi"
            && v.get("mix").is_none()
            && audio.sources.contains_key("ndi")
        {
            node.mix.insert("ndi".into(), 0.0);
        }
        audio.sinks.insert(id.clone(), node);
    }
    if version == 1 {
        let mut output = OutputAudio::default();
        output.enabled = true;
        output.primary = true;
        output.definitions.sources = audio.sources.clone();
        output.definitions.sinks = audio.sinks.clone();
        for (id, s) in &audio.sources {
            if s.enabled {
                output
                    .inputs
                    .push(AudioInput::new(&s.endpoint, s.gain_db, s.delay_ms));
            }
            let mut gains = Vec::new();
            for (sid, sink) in &audio.sinks {
                if let Some(mix) = sink.mix.get(id) {
                    let mut r = AudioRoute::new(p, id, sid);
                    r.enabled = s.enabled && sink.enabled;
                    r.gain_db = if s.gain_db <= -60.0 || *mix <= -60.0 {
                        -60.0
                    } else {
                        s.gain_db + mix
                    };
                    r.delay_ms = s.delay_ms + sink.delay_ms;
                    if r.enabled {
                        gains.push(r.gain_db);
                    }
                    audio.routes.insert(route_id(p, id, sid), r);
                }
            }
            if gains
                .first()
                .is_some_and(|first| gains.iter().any(|g| g != first))
            {
                output.legacy_matrix = true;
            }
        }
        for sink in audio.sinks.values() {
            if sink.enabled {
                output
                    .destinations
                    .push(AudioDestination::new(&sink.endpoint, sink.delay_ms));
            }
        }
        output.enabled =
            audio.settings.enabled && !output.inputs.is_empty() && !output.destinations.is_empty();
        audio.outputs.insert(p, output);
    } else {
        for (id, v) in object(&doc["outputs"]) {
            let id: OutputId = id.parse().map_err(|_| "invalid output")?;
            let mut out = OutputAudio::default();
            out.enabled = enabled(v);
            out.primary = v.get("primary").and_then(Value::as_bool).unwrap_or(id == p);
            out.inputs = serde_json::from_value(v.get("inputs").cloned().unwrap_or(json!([])))
                .map_err(|e| e.to_string())?;
            out.destinations =
                serde_json::from_value(v.get("destinations").cloned().unwrap_or(json!([])))
                    .map_err(|e| e.to_string())?;
            out.legacy_matrix = v
                .get("legacy_matrix")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            for (name, def) in object(&v["definitions"]["sources"]) {
                out.definitions
                    .sources
                    .insert(name.clone(), source(def, name, p)?);
            }
            for (name, def) in object(&v["definitions"]["sinks"]) {
                out.definitions
                    .sinks
                    .insert(name.clone(), sink(def, name, p)?);
            }
            // Fold the old duplicate legacy settings into typed alias definitions.
            if let Some(saved) = v.pointer("/legacy/settings") {
                for (alias, val) in object(&saved["sources"]) {
                    let mut node = source(val, alias, p)?;
                    if let Some(canonical) = v["legacy"]["sources"][alias]
                        .as_str()
                        .and_then(|id| audio.sources.get(id))
                    {
                        node.endpoint.clone_from(&canonical.endpoint);
                        node.resource.clone_from(&canonical.resource);
                        node.output = canonical.output;
                        node.channel_start = canonical.channel_start;
                        node.channel_count = canonical.channel_count;
                    }
                    out.definitions.sources.insert(alias.clone(), node);
                }
                for (alias, val) in object(&saved["sinks"]) {
                    let mut node = sink(val, alias, p)?;
                    if let Some(canonical) = v["legacy"]["sinks"][alias]
                        .as_str()
                        .and_then(|id| audio.sinks.get(id))
                    {
                        node.endpoint.clone_from(&canonical.endpoint);
                        node.resource.clone_from(&canonical.resource);
                        node.channel_start = canonical.channel_start;
                        node.channel_count = canonical.channel_count;
                    }
                    out.definitions.sinks.insert(alias.clone(), node);
                }
            }
            audio.outputs.insert(id, out);
        }
        for (id, v) in object(&doc["routes"]) {
            let mut val = v.clone();
            if val.get("mapping").is_none() {
                val["mapping"] = json!("default");
            }
            audio.routes.insert(
                id.clone(),
                serde_json::from_value(val).map_err(|e| e.to_string())?,
            );
        }
    }
    Ok(audio)
}

pub fn route_id(output: OutputId, source: &str, sink: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("{source}\0{sink}").as_bytes());
    format!("o{output}-{}", &format!("{digest:x}")[..24])
}
pub fn source_json(s: &AudioSource, v1: bool) -> Value {
    let mut v = json!({"type":s.kind,"enabled":s.enabled,"channels":s.channels});
    if v1 {
        v["delay_ms"] = json!(s.delay_ms);
        v["gain_db"] = json!(s.gain_db);
    }
    if let Some(pcm) = &s.alsa_pcm {
        v["alsa_pcm"] = json!(pcm);
    }
    if !v1 {
        if let Some(output) = s.output {
            v["output"] = json!(output);
        }
        metadata(
            &mut v,
            &s.endpoint,
            &s.resource,
            s.channel_start,
            s.channel_count,
        );
    }
    v
}
pub fn sink_json(s: &AudioSink, v1: bool) -> Value {
    let mut v =
        json!({"type":"alsa","enabled":s.enabled,"channels":s.channels,"alsa_pcm":s.alsa_pcm});
    if v1 {
        v["delay_ms"] = json!(s.delay_ms);
        v["mix"] = json!(s.mix);
    } else {
        metadata(
            &mut v,
            &s.endpoint,
            &s.resource,
            s.channel_start,
            s.channel_count,
        );
    }
    v
}
fn metadata(v: &mut Value, endpoint: &str, resource: &str, start: u32, count: u16) {
    if !endpoint.is_empty() {
        v["endpoint"] = json!(endpoint);
    }
    if !resource.is_empty() {
        v["resource"] = json!(resource);
        v["channel_start"] = json!(start);
        v["channel_count"] = json!(count);
    }
}

pub fn render(audio: &Audio, revision: u64) -> Value {
    if audio.engine == AudioEngine::V1 {
        return project(audio, revision);
    }
    let sources: Map<String, Value> = audio
        .sources
        .iter()
        .map(|(id, s)| (id.clone(), source_json(s, false)))
        .collect();
    let sinks: Map<String, Value> = audio
        .sinks
        .iter()
        .map(|(id, s)| (id.clone(), sink_json(s, false)))
        .collect();
    let mut engine = json!(audio.settings);
    // v2 permits empty graphs; do not manufacture a master or endpoint.
    engine["master"] = json!(audio.settings.master);
    let mut outputs = Map::new();
    for (id, out) in &audio.outputs {
        let mut value = json!({"enabled":out.enabled,"primary":out.primary,"inputs":out.inputs,"destinations":out.destinations});
        if out.legacy_matrix {
            value["legacy_matrix"] = json!(true);
        }
        if !out.definitions.sources.is_empty() || !out.definitions.sinks.is_empty() {
            value["definitions"] = json!({"sources":out.definitions.sources.iter().map(|(id,s)|(id.clone(), source_json(s,false))).collect::<Map<_,_>>(),
                "sinks":out.definitions.sinks.iter().map(|(id,s)|(id.clone(), sink_json(s,false))).collect::<Map<_,_>>()});
        }
        // Preserve factorized matrices for legacy adapters without a second state model.
        if out.definitions.sources.contains_key("ndi") && out.definitions.sinks.contains_key("hdmi")
        {
            let mut draft = audio.clone();
            draft.engine = AudioEngine::V1;
            draft.sources = out.definitions.sources.clone();
            draft.sinks = out.definitions.sinks.clone();
            draft.outputs.clear();
            let mut settings = project(&draft, 0);
            settings.as_object_mut().expect("object").remove("revision");
            let source_ids: BTreeMap<_, _> = out
                .definitions
                .sources
                .iter()
                .map(|(alias, s)| {
                    (
                        alias,
                        audio
                            .sources
                            .iter()
                            .find(|(_, c)| c.endpoint == s.endpoint)
                            .map(|(id, _)| id.clone())
                            .unwrap_or_else(|| alias.clone()),
                    )
                })
                .collect();
            let sink_ids: BTreeMap<_, _> = out
                .definitions
                .sinks
                .iter()
                .map(|(alias, s)| {
                    (
                        alias,
                        audio
                            .sinks
                            .iter()
                            .find(|(_, c)| c.endpoint == s.endpoint)
                            .map(|(id, _)| id.clone())
                            .unwrap_or_else(|| alias.clone()),
                    )
                })
                .collect();
            value["legacy"] = json!({"settings":settings,"sources":source_ids,"sinks":sink_ids});
        }
        outputs.insert(id.to_string(), value);
    }
    json!({"version":2,"revision":revision,"engine":engine,"sources":sources,"sinks":sinks,"outputs":outputs,"routes":audio.routes,"video_delay_ms":audio.video_delay_ms})
}

/// Readable v1 view of the primary bus. Per-output definitions retain aliases
/// and disabled factors. A compatibility overlay must refuse unrepresentable buses.
pub fn project(audio: &Audio, revision: u64) -> Value {
    let p = primary(audio);
    let canonical = audio.engine == AudioEngine::V2 || !audio.sources.contains_key("ndi");
    let out = audio.outputs.get(&p);
    let saved = out.filter(|o| {
        o.definitions.sources.contains_key("ndi") && o.definitions.sinks.contains_key("hdmi")
    });
    let source_nodes = saved
        .map(|o| &o.definitions.sources)
        .unwrap_or(&audio.sources);
    let sink_nodes = saved.map(|o| &o.definitions.sinks).unwrap_or(&audio.sinks);
    let mut sources = Map::new();
    let mut sinks = Map::new();
    let mut aliases = BTreeMap::new();
    for (id, node) in source_nodes {
        if node.kind == EndpointType::Ndi && node.output != Some(p) {
            continue;
        }
        if saved.is_none()
            && canonical
            && !audio
                .routes
                .values()
                .any(|r| r.output == p && r.source == *id)
        {
            continue;
        }
        let alias = if node.kind == EndpointType::Ndi {
            "ndi".to_string()
        } else {
            id.clone()
        };
        let mut value = source_json(node, true);
        if saved.is_none() && canonical {
            let choice = out.and_then(|o| o.inputs.iter().find(|c| c.endpoint == node.endpoint));
            value["gain_db"] = json!(choice.map_or(0.0, |c| c.gain_db));
            value["delay_ms"] = json!(choice.map_or(0, |c| c.delay_ms));
            value["enabled"] = json!(
                out.is_some_and(|o| o.enabled)
                    && audio
                        .routes
                        .values()
                        .any(|r| r.output == p && r.source == *id && r.enabled)
            );
        }
        aliases.insert(id.clone(), alias.clone());
        sources.insert(alias, value);
    }
    for (id, node) in sink_nodes {
        if saved.is_none()
            && canonical
            && !audio
                .routes
                .values()
                .any(|r| r.output == p && r.sink == *id)
            && node.resource != format!("hdmi:{p}")
        {
            continue;
        }
        let alias = if node.resource == format!("hdmi:{p}") {
            "hdmi"
        } else if node.resource == "tessera:main:tx" && node.channel_start == 1 {
            "net-out"
        } else {
            id
        };
        let mut value = sink_json(node, true);
        if saved.is_none() && canonical {
            let mut mix = Map::new();
            for r in audio
                .routes
                .values()
                .filter(|r| r.output == p && r.sink == *id)
            {
                if let Some(sid) = aliases.get(&r.source) {
                    mix.insert(
                        sid.clone(),
                        json!(if r.gain_db <= -60.0 {
                            -60.0
                        } else {
                            r.gain_db - sources[sid]["gain_db"].as_f64().unwrap_or(0.0)
                        }),
                    );
                }
            }
            value["mix"] = Value::Object(mix);
            value["delay_ms"] = json!(
                out.and_then(|o| o.destinations.iter().find(|c| c.endpoint == node.endpoint))
                    .map_or(0, |c| c.delay_ms)
            );
            value["enabled"] = json!(
                out.is_some_and(|o| o.enabled)
                    && audio
                        .routes
                        .values()
                        .any(|r| r.output == p && r.sink == *id && r.enabled)
            );
        }
        if alias == "net-out" {
            value["alsa_pcm"] = json!("ndimon_net_out");
        }
        sinks.insert(alias.into(), value);
    }
    sources.entry("ndi").or_insert_with(
        || json!({"type":"ndi","enabled":false,"channels":2,"delay_ms":0,"gain_db":0}),
    );
    sinks.entry("hdmi").or_insert_with(|| json!({"type":"alsa","enabled":true,"channels":2,"alsa_pcm":"auto","delay_ms":0,"mix":{"ndi":-60}}));
    if audio.engine == AudioEngine::V2 && sinks["hdmi"]["enabled"] == false {
        sinks["hdmi"]["enabled"] = json!(true);
        if let Some(mix) = sinks["hdmi"]["mix"].as_object_mut() {
            for gain in mix.values_mut() {
                *gain = json!(-60);
            }
        }
    }
    let master = if audio.engine == AudioEngine::V1 {
        if audio
            .sinks
            .get(&audio.settings.master)
            .is_some_and(|s| s.resource == format!("hdmi:{p}"))
        {
            "hdmi"
        } else if audio
            .sinks
            .get(&audio.settings.master)
            .is_some_and(|s| s.resource == "tessera:main:tx" && s.channel_start == 1)
        {
            "net-out"
        } else {
            audio.settings.master.as_str()
        }
    } else {
        "hdmi"
    };
    let master = if sinks.contains_key(master) {
        master
    } else {
        "hdmi"
    };
    json!({"version":1,"revision":revision,"engine":{"enabled":audio.settings.enabled,"rate":audio.settings.rate,"period_frames":audio.settings.period_frames,"master":master},"sources":sources,"sinks":sinks,"video_delay_ms":audio.video_delay_ms})
}

pub fn import_json(state: &mut DesiredState, files: &crate::LegacyFiles) -> Result<(), String> {
    let read = |name: &str| crate::legacy::read(files, name);
    let d = read("ndimon-device-settings.json")?;
    let engine = if d["audio_engine"] == "v2" {
        AudioEngine::V2
    } else {
        AudioEngine::V1
    };
    if files.contains_key("/etc/ndimon-audio-settings.json") {
        state.audio = crate::audio::import(&read("ndimon-audio-settings.json")?, engine, 1)?;
    } else {
        state.audio.engine = engine;
    }
    let dec1 = read("ndimon-dec1-settings.json")?;
    if state.audio.engine == AudioEngine::V1 && dec1["NDIAudio"] == "NDIAudioDis" {
        if let Some(out) = state.audio.outputs.get_mut(&1) {
            out.enabled = false;
        }
    }
    Ok(())
}

pub fn render_path(
    spec: &crate::PathSpec,
    state: &DesiredState,
    revision: u64,
    files: &crate::LegacyFiles,
) -> Result<Vec<u8>, String> {
    if spec.path_id == "audio-engine" {
        return crate::replace_key(
            files.get(spec.path).map(String::as_str).unwrap_or("{}"),
            "audio_engine",
            &serde_json::to_value(state.audio.engine).map_err(|e| e.to_string())?,
        );
    }
    if spec.path_id == "audio" {
        if let Some(raw) = files.get(spec.path) {
            let value: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
            if import(&value, state.audio.engine, primary(&state.audio))? == state.audio {
                return crate::replace_key(raw, "revision", &json!(revision));
            }
        }
        return crate::node_json(render(&state.audio, revision));
    }
    Err(format!("unknown audio artifact: {}", spec.path_id))
}

pub fn drift_bytes(
    artifact: &crate::Artifact,
    state: &DesiredState,
    files: &crate::LegacyFiles,
) -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
    if artifact.path_id != "audio-engine" {
        return Ok((
            artifact.bytes.clone(),
            files
                .get(artifact.spec().path)
                .map(|s| s.as_bytes().to_vec()),
        ));
    }
    let expected = serde_json::to_vec(&state.audio.engine).map_err(|e| e.to_string())?;
    let actual = files
        .get(artifact.spec().path)
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.get("audio_engine").cloned())
        .map(|v| serde_json::to_vec(&v))
        .transpose()
        .map_err(|e| e.to_string())?;
    Ok((expected, actual))
}
