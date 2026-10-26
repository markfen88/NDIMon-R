//! Native audio domain: graph compilation, planning, import and application.
pub mod catalog;
use crate::{
    BoxFuture,
    apply::{Applier, ProductionApplier},
    persist::StateFile,
    plan::{Action as DomainAction, DomainPlan, Plan, internal},
};
use ndimon_privd_proto::{Direction, ManagedUnit, Request, TesseraPcmSpec, UnitAction};
use ndimon_render::LegacyFiles;
use ndimon_types::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn invalid(reason: impl Into<String>) -> ApiError {
    ApiError::invalid(vec![FieldError::new("audio", reason)])
}
fn conflict(reason: impl Into<String>) -> ApiError {
    ApiError::Locked {
        field: reason.into(),
        by: None,
    }
}
fn source_id(id: &str, s: &AudioSource) -> String {
    if s.kind == EndpointType::Ndi {
        return format!("ndi-{}", s.output.unwrap_or(1));
    }
    resource_id(id, &s.resource, s.channel_start, s.channels)
}
fn sink_id(id: &str, s: &AudioSink) -> String {
    resource_id(id, &s.resource, s.channel_start, s.channels)
}
fn resource_id(id: &str, resource: &str, start: u32, count: u16) -> String {
    if let Some(p) = resource.strip_prefix("hdmi:") {
        return format!("hdmi-{p}");
    }
    if let Some(d) = resource.strip_prefix("tessera:main:") {
        return TesseraEndpoint::new(
            match d {
                "rx" => ndimon_types::Direction::Rx,
                "tx" => ndimon_types::Direction::Tx,
                _ => return format!("tessera-{d}-{start}-{count}"),
            },
            start,
            count,
        )
        .id();
    }
    id.into()
}
pub fn canonicalize(original: &Audio) -> Audio {
    let mut audio = original.clone();
    audio.sources.clear();
    audio.sinks.clear();
    let mut sources = BTreeMap::new();
    let mut sinks = BTreeMap::new();
    for (id, s) in &original.sources {
        let name = source_id(id, s);
        let mut node = s.clone();
        if node.alsa_pcm.as_deref() == Some("ndimon_net_in") {
            node.alsa_pcm = Some(
                TesseraEndpoint::new(
                    ndimon_types::Direction::Rx,
                    node.channel_start,
                    node.channels,
                )
                .pcm(),
            );
        }
        sources.insert(id.clone(), name.clone());
        audio.sources.insert(name, node);
    }
    for (id, s) in &original.sinks {
        let name = sink_id(id, s);
        let mut node = s.clone();
        if node.alsa_pcm == "ndimon_net_out" {
            node.alsa_pcm = TesseraEndpoint::new(
                ndimon_types::Direction::Tx,
                node.channel_start,
                node.channels,
            )
            .pcm();
        }
        sinks.insert(id.clone(), name.clone());
        audio.sinks.insert(name, node);
    }
    audio.routes = original
        .routes
        .values()
        .map(|r| {
            let mut r = r.clone();
            r.source = sources.get(&r.source).cloned().unwrap_or(r.source);
            r.sink = sinks.get(&r.sink).cloned().unwrap_or(r.sink);
            (
                ndimon_render::audio::route_id(r.output, &r.source, &r.sink),
                r,
            )
        })
        .collect();
    audio.settings.master = sinks
        .get(&original.settings.master)
        .cloned()
        .unwrap_or_default();
    audio
}
fn overlap(a: &Value, b: &Value) -> bool {
    let resource = a["resource"].as_str().unwrap_or("");
    let start = |v: &Value| v["channel_start"].as_u64().unwrap_or(1);
    let count = |v: &Value| {
        v["channel_count"]
            .as_u64()
            .or_else(|| v["channels"].as_u64())
            .unwrap_or(2)
    };
    !resource.is_empty()
        && a["resource"] == b["resource"]
        && start(a) < start(b) + count(b)
        && start(b) < start(a) + count(a)
}
fn identity(v: &Value) -> String {
    if v["provider"] == "ndi" {
        format!("ndi:{}", v["output"])
    } else if v["resource"].as_str().is_some_and(|r| !r.is_empty()) {
        format!(
            "{}:{}:{}",
            v["resource"],
            v["channel_start"].as_u64().unwrap_or(1),
            v["channels"]
        )
    } else {
        format!("alsa:{}", v["alsa_pcm"])
    }
}
fn resolve<'a>(catalog: &'a Value, direction: &str, id: &str) -> Result<&'a Value, ApiError> {
    catalog[direction]
        .as_array()
        .and_then(|a| a.iter().find(|v| v["id"] == id))
        .ok_or_else(|| invalid(format!("unknown audio endpoint: {id}")))
}
fn concrete(entry: &Value) -> Value {
    let mut v = json!({"type":if entry["provider"]=="ndi"{"ndi"}else{"alsa"},"enabled":true,"channels":entry["channels"],"endpoint":entry["id"]});
    if v["type"] == "ndi" {
        v["output"] = entry["output"].clone();
    } else {
        v["alsa_pcm"] = entry["alsa_pcm"].clone();
        v["resource"] = entry["resource"].clone();
        v["channel_start"] = entry.get("channel_start").cloned().unwrap_or(json!(1));
        v["channel_count"] = entry["channels"].clone();
    }
    v
}
pub fn compile_output(
    original: &Audio,
    output: OutputId,
    mut choice: OutputAudio,
    normalize: bool,
    catalog: &Value,
) -> Result<Audio, ApiError> {
    if !(1..=8).contains(&output) {
        return Err(invalid("output must be1..8"));
    }
    let mut audio = canonicalize(original);
    let before = audio.clone();
    choice.enabled = choice.enabled && !choice.inputs.is_empty() && !choice.destinations.is_empty();
    choice.primary = before.outputs.get(&output).is_some_and(|o| o.primary);
    let inputs = choice
        .inputs
        .iter()
        .map(|i| resolve(catalog, "inputs", &i.endpoint))
        .collect::<Result<Vec<_>, _>>()?;
    let destinations = choice
        .destinations
        .iter()
        .map(|i| resolve(catalog, "destinations", &i.endpoint))
        .collect::<Result<Vec<_>, _>>()?;
    for list in [&inputs, &destinations] {
        let mut seen = BTreeSet::new();
        for (i, a) in list.iter().enumerate() {
            if !seen.insert(identity(a)) {
                return Err(conflict(format!(
                    "duplicate canonical endpoint selection: {}",
                    a["label"].as_str().unwrap_or("")
                )));
            }
            for b in list.iter().take(i) {
                if overlap(a, b) {
                    return Err(conflict("overlapping channel selections"));
                }
            }
        }
    }
    let signature = |out: &OutputAudio| -> Result<Value, ApiError> {
        let mut inputs = out
            .inputs
            .iter()
            .map(|i| {
                resolve(catalog, "inputs", &i.endpoint).map(|v| {
                    (
                        v["node_id"].as_str().unwrap_or(&i.endpoint).to_string(),
                        i.gain_db,
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        inputs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)));
        let mut sinks = out
            .destinations
            .iter()
            .map(|i| {
                resolve(catalog, "destinations", &i.endpoint)
                    .map(|v| v["node_id"].as_str().unwrap_or(&i.endpoint).to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        sinks.sort();
        Ok(json!([inputs, sinks]))
    };
    let previous = before.outputs.get(&output);
    let same = previous
        .map(|p| signature(p).and_then(|a| signature(&choice).map(|b| a == b)))
        .transpose()?
        .unwrap_or(false);
    if previous.is_some_and(|o| o.legacy_matrix) && !same && !normalize {
        return Err(conflict("matrix_confirmation_required"));
    }
    audio.routes.retain(|_, r| r.output != output);
    for (kind, entries) in [("sources", &inputs), ("sinks", &destinations)] {
        for entry in entries {
            let id = entry["node_id"]
                .as_str()
                .or_else(|| entry["id"].as_str())
                .ok_or_else(|| invalid("endpoint identity missing"))?;
            let v = concrete(entry);
            if kind == "sources" {
                let mut node = ndimon_render::audio::source(&v, id, output).map_err(invalid)?;
                if let Some(existing) = audio.sources.get(id) {
                    if existing.channels != node.channels {
                        return Err(conflict(format!("shared endpoint geometry differs: {id}")));
                    }
                    node = existing.clone();
                    node.enabled = true;
                }
                choice.definitions.sources.insert(id.into(), node.clone());
                if choice.enabled && entry["provider"] != "unknown" {
                    audio.sources.insert(id.into(), node);
                }
            } else {
                let mut node = ndimon_render::audio::sink(&v, id, output).map_err(invalid)?;
                if let Some(existing) = audio.sinks.get(id) {
                    if existing.channels != node.channels {
                        return Err(conflict(format!("shared endpoint geometry differs: {id}")));
                    }
                    node = existing.clone();
                    node.enabled = true;
                }
                choice.definitions.sinks.insert(id.into(), node.clone());
                if choice.enabled && entry["provider"] != "unknown" {
                    audio.sinks.insert(id.into(), node);
                }
            }
        }
    }
    if choice.enabled {
        for (i, input) in choice.inputs.iter().zip(&inputs) {
            for (d, dest) in choice.destinations.iter().zip(&destinations) {
                if i.delay_ms.saturating_add(d.delay_ms) > 5000 {
                    return Err(invalid("effective delay exceeds5000ms"));
                }
                if input["provider"] == "unknown" || dest["provider"] == "unknown" {
                    continue;
                }
                let sid = input["node_id"]
                    .as_str()
                    .or_else(|| input["id"].as_str())
                    .ok_or_else(|| invalid("source id missing"))?;
                let tid = dest["node_id"]
                    .as_str()
                    .or_else(|| dest["id"].as_str())
                    .ok_or_else(|| invalid("sink id missing"))?;
                let mut route = AudioRoute::new(output, sid, tid);
                route.delay_ms = i.delay_ms + d.delay_ms;
                route.gain_db = i.gain_db;
                if same && previous.is_some_and(|p| p.legacy_matrix) {
                    if let Some(prior) = before
                        .routes
                        .values()
                        .find(|r| r.output == output && r.source == sid && r.sink == tid)
                    {
                        route.gain_db = prior.gain_db;
                    } else if let Some(prior) = previous {
                        let src = prior
                            .definitions
                            .sources
                            .values()
                            .find(|s| s.endpoint == i.endpoint);
                        let sink = prior
                            .definitions
                            .sinks
                            .values()
                            .find(|s| s.endpoint == d.endpoint);
                        if let (Some(s), Some(t)) = (src, sink) {
                            if let Some((alias, _)) = prior
                                .definitions
                                .sources
                                .iter()
                                .find(|(_, s)| s.endpoint == i.endpoint)
                            {
                                if let Some(mix) = t.mix.get(alias) {
                                    route.gain_db = if s.gain_db <= -60.0 || *mix <= -60.0 {
                                        -60.0
                                    } else {
                                        s.gain_db + mix
                                    };
                                }
                            }
                        }
                    }
                }
                audio
                    .routes
                    .insert(ndimon_render::audio::route_id(output, sid, tid), route);
            }
        }
    }
    if same {
        if let Some(previous) = previous {
            choice.legacy_matrix = previous.legacy_matrix;
            if previous.definitions.sources.contains_key("ndi") {
                choice.definitions = previous.definitions.clone();
                for s in choice.definitions.sources.values_mut() {
                    if let Some(i) = choice.inputs.iter().find(|i| i.endpoint == s.endpoint) {
                        s.delay_ms = i.delay_ms;
                    }
                }
                for s in choice.definitions.sinks.values_mut() {
                    if let Some(d) = choice
                        .destinations
                        .iter()
                        .find(|d| d.endpoint == s.endpoint)
                    {
                        s.delay_ms = d.delay_ms;
                    }
                }
            }
        }
    }
    audio.outputs.insert(output, choice);
    refresh(&mut audio);
    Ok(audio)
}
pub fn refresh(audio: &mut Audio) {
    audio
        .routes
        .retain(|_, r| r.enabled && audio.outputs.get(&r.output).is_some_and(|o| o.enabled));
    let sources: BTreeSet<_> = audio.routes.values().map(|r| r.source.clone()).collect();
    let sinks: BTreeSet<_> = audio.routes.values().map(|r| r.sink.clone()).collect();
    audio.sources.retain(|id, _| sources.contains(id));
    if audio.routes.is_empty() {
        audio.sources.clear();
        audio.sinks.clear();
        audio.settings.master.clear();
        return;
    }
    if !sinks.contains(&audio.settings.master) {
        audio.settings.master = audio
            .routes
            .values()
            .find(|r| {
                audio
                    .sinks
                    .get(&r.sink)
                    .is_some_and(|s| s.resource.starts_with("hdmi:"))
            })
            .or_else(|| audio.routes.values().next())
            .map(|r| r.sink.clone())
            .unwrap_or_default();
    }
    audio
        .sinks
        .retain(|id, _| sinks.contains(id) || *id == audio.settings.master);
    for s in audio.sources.values_mut() {
        s.enabled = true;
    }
    for s in audio.sinks.values_mut() {
        s.enabled = true;
    }
}

pub fn overlay(original: &Audio, settings: &Value) -> Result<Audio, ApiError> {
    let p = ndimon_render::audio::primary(original);
    let output = original.outputs.get(&p);
    if original.engine == AudioEngine::V2
        && output.is_some_and(|o| o.definitions.sources.contains_key("ndi"))
    {
        let output = output.expect("checked primary");
        for r in original.routes.values().filter(|r| r.output == p) {
            if original.sources.get(&r.source).is_none_or(|s| {
                !output
                    .definitions
                    .sources
                    .values()
                    .any(|d| d.endpoint == s.endpoint)
            }) || original.sinks.get(&r.sink).is_none_or(|s| {
                !output
                    .definitions
                    .sinks
                    .values()
                    .any(|d| d.endpoint == s.endpoint)
            }) {
                return Err(conflict("legacy_edit_unsupported"));
            }
        }
    }
    if original.engine == AudioEngine::V2
        && !output.is_some_and(|o| o.definitions.sources.contains_key("ndi"))
    {
        for r in original.routes.values().filter(|r| r.output == p) {
            if original.sources.get(&r.source).is_some_and(|s| {
                s.kind == EndpointType::Ndi && s.output != Some(p)
                    || s.resource.starts_with("tessera:") && s.channel_start != 1
            }) {
                return Err(conflict("legacy_edit_unsupported"));
            }
        }
        if output.is_some_and(|o| {
            o.definitions.sources.values().any(|s| {
                s.kind == EndpointType::Ndi && s.output != Some(p)
                    || s.resource.starts_with("tessera:") && s.channel_start != 1
            })
        }) {
            return Err(conflict("legacy_edit_unsupported"));
        }
    }
    let mut incoming =
        ndimon_render::audio::import(settings, original.engine, p).map_err(invalid)?;
    if settings["engine"].get("memory_budget_mib").is_none() {
        incoming.settings.memory_budget_mib = original.settings.memory_budget_mib;
    }
    if original.engine == AudioEngine::V1
        && original.outputs.len() == 1
        && original.sources.contains_key("ndi")
    {
        return Ok(incoming);
    }
    let mut incoming = canonicalize(&incoming);
    let mut audio = canonicalize(original);
    audio.routes.retain(|_, r| r.output != p);
    for (id, s) in &incoming.sources {
        if let Some(old) = audio.sources.get(id) {
            if audio.routes.values().any(|r| r.source == *id)
                && (old.channels != s.channels
                    || old.alsa_pcm != s.alsa_pcm
                    || old.output != s.output)
            {
                return Err(conflict(format!(
                    "legacy edit would reconfigure a shared endpoint: {id}"
                )));
            }
        }
    }
    for (id, s) in &incoming.sinks {
        if let Some(old) = audio.sinks.get(id) {
            if audio.routes.values().any(|r| r.sink == *id)
                && (old.channels != s.channels || old.alsa_pcm != s.alsa_pcm)
            {
                return Err(conflict(format!(
                    "legacy edit would reconfigure a shared endpoint: {id}"
                )));
            }
        }
    }
    // Keep disabled primary intent unless an endpoint was explicitly enabled.
    if output.is_some_and(|o| !o.enabled) {
        let before = ndimon_render::audio::project(original, 0);
        let explicit = ["sources", "sinks"].iter().any(|kind| {
            settings[*kind].as_object().is_some_and(|values| {
                values
                    .iter()
                    .any(|(id, v)| v["enabled"] == true && before[*kind][id]["enabled"] != true)
            })
        });
        if !explicit {
            if let Some(out) = incoming.outputs.get_mut(&p) {
                out.enabled = false;
            }
        }
    }
    audio.sources.extend(incoming.sources);
    audio.sinks.extend(incoming.sinks);
    audio.routes.extend(incoming.routes);
    audio.outputs.extend(incoming.outputs);
    let master = if audio
        .sinks
        .get(&original.settings.master)
        .is_some_and(|s| s.enabled)
    {
        original.settings.master.clone()
    } else {
        incoming.settings.master.clone()
    };
    audio.settings = incoming.settings;
    audio.settings.master = master;
    audio.settings.memory_budget_mib = original.settings.memory_budget_mib;
    audio.video_delay_ms = incoming.video_delay_ms;
    refresh(&mut audio);
    Ok(audio)
}

/// Endpoint replacements also update the retained draft. Under the v1 engine
/// the factorized matrix remains authoritative, so rebuild its derived routes.
pub fn put_source(audio: &mut Audio, id: &str, source: &AudioSource) -> Result<(), ApiError> {
    let old = audio.sources.insert(id.into(), source.clone());
    for out in audio.outputs.values_mut() {
        for (alias, definition) in &mut out.definitions.sources {
            if alias == id
                || old
                    .as_ref()
                    .is_some_and(|s| s.endpoint == definition.endpoint)
            {
                *definition = source.clone();
            }
        }
    }
    sync_v1(audio)
}
pub fn put_sink(audio: &mut Audio, id: &str, sink: &AudioSink) -> Result<(), ApiError> {
    let old = audio.sinks.insert(id.into(), sink.clone());
    for out in audio.outputs.values_mut() {
        for (alias, definition) in &mut out.definitions.sinks {
            if alias == id
                || old
                    .as_ref()
                    .is_some_and(|s| s.endpoint == definition.endpoint)
            {
                *definition = sink.clone();
            }
        }
    }
    sync_v1(audio)
}
pub(crate) fn sync_v1(audio: &mut Audio) -> Result<(), ApiError> {
    if audio.engine != AudioEngine::V1 || !audio.sources.contains_key("ndi") {
        return Ok(());
    }
    let p = ndimon_render::audio::primary(audio);
    let mut view = ndimon_render::audio::project(audio, 0);
    view["sources"] = json!(
        audio
            .sources
            .iter()
            .map(|(id, s)| (id.clone(), ndimon_render::audio::source_json(s, true)))
            .collect::<BTreeMap<_, _>>()
    );
    view["sinks"] = json!(
        audio
            .sinks
            .iter()
            .map(|(id, s)| (id.clone(), ndimon_render::audio::sink_json(s, true)))
            .collect::<BTreeMap<_, _>>()
    );
    let derived = ndimon_render::audio::import(&view, AudioEngine::V1, p).map_err(invalid)?;
    audio.outputs.insert(
        p,
        derived
            .outputs
            .get(&p)
            .cloned()
            .ok_or_else(|| invalid("primary audio output missing"))?,
    );
    audio.routes.retain(|_, r| r.output != p);
    audio.routes.extend(derived.routes);
    Ok(())
}

/// A legacy primary projection can represent only the ready full Tessera range.
/// Retained alias factors are already representable and use the earlier overlay
/// guard against foreign routes, matching the legacy metadata exception.
pub fn ensure_legacy_editable(audio: &Audio, catalog: &Value) -> Result<(), ApiError> {
    if audio.engine != AudioEngine::V2 {
        return Ok(());
    }
    let p = ndimon_render::audio::primary(audio);
    let Some(out) = audio.outputs.get(&p) else {
        return Ok(());
    };
    if out.definitions.sources.contains_key("ndi") {
        return Ok(());
    }
    let check = |node: &Value, kind: &str| -> Result<(), ApiError> {
        if !node["resource"]
            .as_str()
            .is_some_and(|s| s.starts_with("tessera:"))
        {
            return Ok(());
        }
        let full = catalog[kind]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| {
                e["provider"] == "tessera"
                    && e["resource"] == node["resource"]
                    && e["channel_start"] == 1
                    && e["ready"] == true
            })
            .filter_map(|e| e["channels"].as_u64())
            .max()
            .unwrap_or(0);
        if node["channel_start"] != 1 || node["channels"].as_u64() != Some(full) {
            return Err(conflict("legacy_edit_unsupported"));
        }
        Ok(())
    };
    for route in audio.routes.values().filter(|r| r.output == p) {
        if let Some(s) = audio.sources.get(&route.source) {
            check(&ndimon_render::audio::source_json(s, false), "inputs")?;
        }
        if let Some(s) = audio.sinks.get(&route.sink) {
            check(&ndimon_render::audio::sink_json(s, false), "destinations")?;
        }
    }
    for s in out.definitions.sources.values() {
        check(&ndimon_render::audio::source_json(s, false), "inputs")?;
    }
    for s in out.definitions.sinks.values() {
        check(&ndimon_render::audio::sink_json(s, false), "destinations")?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    CheckAudio,
    PreparePcms,
    Render,
    ReloadAudio,
    RestartDecoder,
}

#[derive(Debug)]
pub struct Module;
impl super::DomainModule for Module {
    fn reduce(&self, state: &mut DesiredState, command: &Command) -> Result<(), ApiError> {
        match command {
            Command::PutAudio(audio) => state.audio = audio.clone(),
            Command::SetAudioEngine { engine } => state.audio.engine = *engine,
            Command::PutAudioSource { id, source } => put_source(&mut state.audio, id, source)?,
            Command::PutAudioSink { id, sink } => put_sink(&mut state.audio, id, sink)?,
            Command::RemoveAudioEndpoints { ids } => {
                if ids.iter().any(|id| id == "ndi" || id == "hdmi") {
                    return Err(ApiError::invalid(vec![FieldError::new(
                        "ids",
                        "reserved audio ids cannot be removed",
                    )]));
                }
                state.audio.sources.retain(|id, node| {
                    !ids.contains(id)
                        && !(ids.iter().any(|id| id == "net-in")
                            && node.resource == "tessera:main:rx")
                });
                state.audio.sinks.retain(|id, node| {
                    !ids.contains(id)
                        && !(ids.iter().any(|id| id == "net-out")
                            && node.resource == "tessera:main:tx")
                });
                for sink in state.audio.sinks.values_mut() {
                    sink.mix.retain(|id, _| !ids.contains(id));
                }
                state.audio.routes.retain(|_, r| {
                    state.audio.sources.contains_key(&r.source)
                        && state.audio.sinks.contains_key(&r.sink)
                });
                if !state.audio.sinks.contains_key(&state.audio.settings.master) {
                    state.audio.settings.master =
                        state.audio.sinks.keys().next().cloned().unwrap_or_default();
                }
                sync_v1(&mut state.audio)?;
            }
            Command::ImportBackup {
                document,
                scope: RestoreScope::Audio,
            } => state.audio = document.document.audio.clone(),
            Command::PutOutputAudio {
                output,
                audio: choice,
                normalize_matrix,
            } => {
                let outputs = json!(
                    state
                        .outputs
                        .iter()
                        .map(|(id, o)| json!({"ch_num":id,"output_alias":o.alias}))
                        .collect::<Vec<_>>()
                );
                let catalog = crate::catalog::build(&outputs, None, false, &state.audio);
                state.audio = compile_output(
                    &state.audio,
                    *output,
                    choice.clone(),
                    *normalize_matrix,
                    &catalog,
                )?;
                // Native output commands own engine selection as part of the same
                // change. Legacy adapters may preserve an explicit v1 rollback.
                state.audio.engine = AudioEngine::V2;
            }
            _ => {
                return Err(ApiError::Unsupported {
                    what: "domain is followed; use its legacy writer until ownership moves".into(),
                });
            }
        }
        Ok(())
    }

    fn query<'a>(
        &self,
        state: &'a StateFile,
        applier: &'a dyn Applier,
        query: &'a Query,
    ) -> Option<BoxFuture<'a, Result<Value, ApiError>>> {
        if !matches!(
            query,
            Query::AudioSettings
                | Query::OutputAudio { .. }
                | Query::AudioStatus
                | Query::Catalog {
                    name: Catalog::AudioEndpoints,
                    ..
                }
        ) {
            return None;
        }
        Some(Box::pin(async move {
            match query {
                Query::AudioSettings => Ok(ndimon_render::audio::project(
                    &state.document.audio,
                    state.audio_revision(),
                )),
                Query::OutputAudio { output } => {
                    let a = state
                        .document
                        .audio
                        .outputs
                        .get(output)
                        .cloned()
                        .unwrap_or_default();
                    let mut value = json!({"enabled":a.enabled,"inputs":a.inputs,"destinations":a.destinations,"revision":state.audio_revision()});
                    if a.legacy_matrix {
                        value["legacy_matrix"] = json!(true);
                        value["warning"] = json!(
                            "Legacy per-destination gains and mutes are preserved. Changing this mix requires explicit normalization."
                        );
                    }
                    Ok(value)
                }
                Query::AudioStatus => {
                    let status = applier
                        .decoder(json!({"action":"status"}))
                        .await
                        .map_err(internal)?;
                    status
                        .get("audio")
                        .cloned()
                        .ok_or_else(|| internal("audio unavailable"))
                }
                Query::Catalog {
                    name: Catalog::AudioEndpoints,
                    ..
                } => {
                    let outputs = applier
                        .decoder(json!({"action":"get_status_all"}))
                        .await
                        .unwrap_or(json!([]));
                    // Public endpoint shaping belongs to compat; return observations.
                    let status = applier.tessera_status().await;
                    Ok(
                        json!({"outputs":outputs,"audio":state.document.audio,"revision":state.audio_revision(),"tessera_status":status,"tessera_enabled":state.document.plugins.get("tessera").is_some_and(|p|p.enabled)}),
                    )
                }
                _ => unreachable!("audio query route"),
            }
        }))
    }
    fn import(&self, state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String> {
        ndimon_render::audio::import_json(state, files)
    }

    fn plan(
        &self,
        previous: &DesiredState,
        next: &DesiredState,
        command: &Command,
    ) -> Result<DomainPlan, ApiError> {
        let mut actions = Vec::new();
        let mut impact = Impact::default();
        {
            if next.audio.video_delay_ms > 0
                && (previous.audio.video_delay_ms != next.audio.video_delay_ms
                    || previous.audio.settings.memory_budget_mib
                        != next.audio.settings.memory_budget_mib)
            {
                actions.push(Action::CheckAudio);
            }
            // RemoveAudioEndpoints may execute under privd's plugin lifecycle lock.
            // Never recursively acquire that lock to prepare PCM definitions.
            if !matches!(
                command,
                Command::RemoveAudioEndpoints { .. }
                    | Command::SetAudioEngine {
                        engine: AudioEngine::V1
                    }
            ) {
                actions.push(Action::PreparePcms);
            }
            actions.push(Action::Render);
            // The decoder can publish v2 topology live. Engine selection and v1
            // structural geometry cannot be changed safely with reload_audio.
            let restart = previous.audio.engine != next.audio.engine
                || (next.audio.engine == AudioEngine::V1
                    && (previous.audio.settings.period_frames
                        != next.audio.settings.period_frames
                        || previous.audio.settings.master != next.audio.settings.master));
            if restart {
                actions.push(Action::RestartDecoder);
                impact.scope = ImpactScope::Service;
                impact.restarts.push(Service::Decoder);
                impact.interruptions.push(Interruption::new(
                    Domain::Audio,
                    "decoder video/audio",
                    Some(3000),
                ));
                // Say why it restarts: saving enabled per-output routes selects v2.
                if previous.audio.engine != next.audio.engine {
                    impact.lines.push(match next.audio.engine {
                    AudioEngine::V1 => {
                        "Return to audio engine v1; per-output network audio routes become inactive"
                            .into()
                    }
                    _ => "Switch to audio engine v2, which per-output audio routes need".into(),
                });
                }
                impact
                    .lines
                    .push("Restart the decoder; video and audio pause briefly".into());
            } else {
                actions.push(Action::ReloadAudio);
            }
        }
        Ok(DomainPlan {
            actions: actions.into_iter().map(DomainAction::Audio).collect(),
            impact,
        })
    }

    fn preflight<'a>(
        &self,
        applier: &'a ProductionApplier,
        previous: &'a StateFile,
        next: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if plan
                .actions
                .contains(&DomainAction::Audio(Action::CheckAudio))
            {
                let cap = applier
                    .decoder
                    .request(json!({"action":"check_audio"}))
                    .await?;
                if cap["ok"] != true {
                    return Err(cap["error"]
                        .as_str()
                        .unwrap_or("decoder did not report video delay cap")
                        .into());
                }
                let cap = cap["video_delay_cap_ms"]
                    .as_u64()
                    .ok_or("decoder did not report video delay cap")?;
                if u64::from(next.document.audio.video_delay_ms) > cap {
                    return Err(format!("video delay capped at {cap} ms"));
                }
            }
            if plan
                .actions
                .contains(&DomainAction::Audio(Action::PreparePcms))
            {
                let mut ranges = BTreeSet::new();
                for a in [&previous.document.audio, &next.document.audio] {
                    for s in a.sources.values() {
                        if s.enabled && s.resource == "tessera:main:rx" {
                            ranges.insert((false, s.channel_start, s.channels));
                        }
                    }
                    for s in a.sinks.values() {
                        if s.enabled && s.resource == "tessera:main:tx" {
                            ranges.insert((true, s.channel_start, s.channels));
                        }
                    }
                }
                if !ranges.is_empty()
                    && ndimon_render::ownership(Domain::Tessera) != ndimon_render::Ownership::Native
                {
                    let ranges=ranges.into_iter().map(|(tx,start,count)|serde_json::from_value::<TesseraPcmSpec>(json!({"direction":if tx{Direction::Tx}else{Direction::Rx},"first_channel":start,"channels":count})).map_err(|e|e.to_string())).collect::<Result<Vec<_>,_>>()?;
                    applier
                        .privd
                        .execute(Request::TesseraWritePcms { ranges })
                        .await
                        .map_err(|e| e.to_string())?;
                }
            }
            Ok(())
        })
    }
    fn apply<'a>(
        &self,
        applier: &'a ProductionApplier,
        _state: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            if plan
                .actions
                .contains(&DomainAction::Audio(Action::RestartDecoder))
            {
                applier
                    .privd
                    .execute(Request::ServiceControl {
                        unit: ManagedUnit::Decoder,
                        action: UnitAction::Restart,
                    })
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!({"ok":true,"restarted":true}))
            } else {
                let reply = applier
                    .decoder
                    .request(json!({"action":"reload_audio"}))
                    .await?;
                if reply["ok"] != true {
                    return Err(serde_json::to_string(&reply).map_err(|e| e.to_string())?);
                }
                Ok(reply)
            }
        })
    }
}
