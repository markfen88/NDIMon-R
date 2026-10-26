use crate::*;
use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

/// Validate all portable document invariants, collecting independent failures.
/// Runtime capabilities, locks and adoption/CAS checks belong to the planner.
pub fn validate(state: &DesiredState) -> Result<(), Vec<FieldError>> {
    let mut v = Validator::default();
    v.check(
        state.schema_version == SCHEMA_VERSION,
        "schema_version",
        "unsupported schema version",
    );
    v.device(&state.device);
    v.check(state.outputs.len() <= 8, "outputs", "at most 8 outputs");
    for (id, output) in &state.outputs {
        let path = format!("outputs.{id}");
        v.output_id(*id, &path);
        v.check(
            output.alias.trim() == output.alias,
            &format!("{path}.alias"),
            "must be trimmed",
        );
        v.check(
            output.source_selection == "NDI",
            &format!("{path}.source_selection"),
            "only NDI is supported",
        );
        if let Some(source) = &output.source {
            v.source_ref(source, &format!("{path}.source"));
        }
        // Rotation policy (RotationPolicy.h): no swapped canvas yet.
        v.check(
            matches!(output.video.rotation, 0 | 180),
            &format!("{path}.video.rotation"),
            "must be 0 or 180; planner must also check plane support",
        );
        // VideoOutput.js and DRMDisplay::set_resolution: zero refresh picks
        // preferred EDID mode for this size; only observed EDID can verify it.
        if let Resolution::Mode {
            width,
            height,
            refresh_hz,
        } = output.video.resolution
        {
            v.check(
                width > 0,
                &format!("{path}.video.resolution.mode.width"),
                "must be positive",
            );
            v.check(
                height > 0,
                &format!("{path}.video.resolution.mode.height"),
                "must be positive",
            );
            v.check(
                refresh_hz.is_finite() && refresh_hz >= 0.0,
                &format!("{path}.video.resolution.mode.refresh_hz"),
                "must be finite and nonnegative",
            );
        }
    }
    // NDIFinder.js preserves group case. A comma separates group names; do not
    // normalize case, de-duplicate or sort them during validation.
    v.check(
        !state.ndi.groups.is_empty(),
        "ndi.groups",
        "at least one group",
    );
    for (i, group) in state.ndi.groups.iter().enumerate() {
        v.check(
            !group.trim().is_empty() && !group.contains(','),
            &format!("ndi.groups[{i}]"),
            "must be a nonempty individual group",
        );
    }
    for (i, ip) in state.ndi.off_subnet_sources.iter().enumerate() {
        v.check(
            ip.parse::<Ipv4Addr>().is_ok(),
            &format!("ndi.off_subnet_sources[{i}]"),
            "must be IPv4",
        );
    }
    // NDI SDK discovery accepts an IP or hostname (see finder/main.cpp).
    for (i, host) in state.ndi.discovery_servers.iter().enumerate() {
        v.check(
            valid_host(host),
            &format!("ndi.discovery_servers[{i}]"),
            "must be an IP address or hostname",
        );
    }
    v.audio(&state.audio);
    for id in state.audio.outputs.keys() {
        v.check(
            state.outputs.contains_key(id),
            &format!("audio.outputs.{id}"),
            "output is not configured",
        );
    }
    v.network(&state.network);
    if let Some(tessera) = &state.tessera {
        v.tessera(tessera, &state.audio);
    }
    v.display(&state.display);
    // Presets.js: 32 entries; trimmed nonempty names, 64 characters.
    v.check(state.presets.len() <= 32, "presets", "at most 32 presets");
    for (name, preset) in &state.presets {
        let path = format!("presets.{name}");
        v.check(
            !name.is_empty() && name.trim() == name && name.encode_utf16().count() <= 64,
            &path,
            "name must be trimmed, nonempty and at most 64 UTF-16 units",
        );
        v.source_ref(&preset.source, &format!("{path}.source"));
    }
    // plugins.js / plugin-manifest.js: names are two to 32 ASCII characters.
    for (name, plugin) in &state.plugins {
        let path = format!("plugins.{name}");
        v.check(
            identifier(name) && name.len() >= 2,
            &path,
            "invalid plugin name",
        );
        v.check(
            !plugin.enabled || plugin.installed,
            &format!("{path}.enabled"),
            "enabled plugin must be installed",
        );
        v.check(
            serde_json::to_vec(&plugin.state).is_ok_and(|bytes| bytes.len() <= 1024 * 1024),
            &format!("{path}.state"),
            "plugin data exceeds 1 MiB",
        );
        v.check(
            plugin.version.is_empty() || valid_version(&plugin.version),
            &format!("{path}.version"),
            "invalid plugin version",
        );
    }
    if v.errors.is_empty() {
        Ok(())
    } else {
        Err(v.errors)
    }
}

/// DeviceSettings.js alias-to-hostname conversion. It preserves alias case;
/// replaces each run of non-ASCII hostname characters and truncates at 63 bytes.
pub fn alias_hostname(alias: &str) -> String {
    let mut out = String::new();
    let mut separator = false;
    for ch in alias.trim().chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            if separator && !out.is_empty() {
                out.push('-');
            }
            separator = false;
            out.push(ch);
        } else {
            separator = true;
        }
    }
    let value: String = out.trim_matches('-').chars().take(63).collect();
    if value.is_empty() {
        "ndimon".into()
    } else {
        value
    }
}

#[derive(Default)]
struct Validator {
    errors: Vec<FieldError>,
}

impl Validator {
    fn check(&mut self, condition: bool, path: &str, reason: &str) {
        if !condition {
            self.errors.push(FieldError::new(path, reason));
        }
    }
    fn range(&mut self, value: u64, min: u64, max: u64, path: &str) {
        self.check(
            (min..=max).contains(&value),
            path,
            &format!("must be {min}..={max}"),
        );
    }
    fn gain(&mut self, value: f64, path: &str) {
        self.check(
            value.is_finite() && (-60.0..=12.0).contains(&value),
            path,
            "must be finite and -60..=12 dB",
        );
    }
    fn output_id(&mut self, id: OutputId, path: &str) {
        self.range(u64::from(id), 1, 8, path);
    }
    fn source_ref(&mut self, source: &NdiSourceRef, path: &str) {
        // NDIDecode.js / Presets.js / ndi-endpoint.js: empty endpoint permits
        // name-only recovery; `None` is the explicit forget action.
        self.check(
            !source.name.is_empty() && source.name != "None",
            &format!("{path}.name"),
            "must name a source (use null to forget)",
        );
        self.check(
            source.endpoint.is_empty() || ndi_endpoint(&source.endpoint),
            &format!("{path}.endpoint"),
            "must be IPv4 with optional port 1..=65535",
        );
    }
    fn device(&mut self, device: &Device) {
        self.check(
            device.web_allowed_hosts.len() <= 16,
            "device.web_allowed_hosts",
            "at most 16 additional host names",
        );
        for (i, host) in device.web_allowed_hosts.iter().enumerate() {
            self.check(
                valid_web_hostname(host) && *host == host.to_ascii_lowercase(),
                &format!("device.web_allowed_hosts[{i}]"),
                "must be a lower-case DNS hostname without wildcards, ports or IP literals",
            );
        }
        // DeviceSettings.js trims aliases; the RFC-style hostname is a separate
        // derived OS setting and may be blank to leave the OS default alone.
        self.check(
            device.ndi_alias.trim() == device.ndi_alias,
            "device.ndi_alias",
            "must be trimmed",
        );
        self.check(
            device.hostname.is_empty()
                || (device.hostname.len() <= 63 && valid_device_label(&device.hostname)),
            "device.hostname",
            "must be a hostname label of at most 63 bytes",
        );
        self.check(
            device.ntp_server.is_empty() || valid_device_ntp(&device.ntp_server),
            "device.ntp_server",
            "invalid NTP host",
        );
        let schedule = &device.reboot_schedule;
        let bytes = schedule.time.as_bytes();
        self.check(
            bytes.len() == 5
                && bytes.get(2) == Some(&b':')
                && bytes
                    .iter()
                    .enumerate()
                    .all(|(i, b)| i == 2 || b.is_ascii_digit())
                && schedule
                    .time
                    .get(..2)
                    .and_then(|s| s.parse::<u8>().ok())
                    .is_some_and(|h| h < 24)
                && schedule
                    .time
                    .get(3..)
                    .and_then(|s| s.parse::<u8>().ok())
                    .is_some_and(|m| m < 60),
            "device.reboot_schedule.time",
            "must be HH:MM (24 hour)",
        );
        self.check(
            !schedule.enabled || !schedule.days.is_empty(),
            "device.reboot_schedule.days",
            "enabled schedule requires a weekday",
        );
        self.check(
            schedule.days.iter().copied().collect::<BTreeSet<_>>().len() == schedule.days.len(),
            "device.reboot_schedule.days",
            "weekdays must be distinct",
        );
        // config/ndimon-device-settings.json legacy percentage knobs.
        self.range(
            u64::from(device.analog_audio_in_gain),
            0,
            100,
            "device.analog_audio_in_gain",
        );
        self.range(
            u64::from(device.analog_audio_out_gain),
            0,
            100,
            "device.analog_audio_out_gain",
        );
        // api/plugin-feed.js refuses credentials and insecure URLs unless opted in.
        for (i, url) in device.plugin_feeds.iter().enumerate() {
            let body = url.strip_prefix("https://").or_else(|| {
                device
                    .allow_insecure_feeds
                    .then(|| url.strip_prefix("http://"))
                    .flatten()
            });
            let valid = body.is_some_and(|s| {
                !s.is_empty()
                    && !s.starts_with('/')
                    && !s.split('/').next().unwrap_or("").contains('@')
                    && !s.chars().any(|c| c.is_whitespace() || c.is_control())
            });
            self.check(
                valid,
                &format!("device.plugin_feeds[{i}]"),
                "feed URL requires HTTPS without credentials (HTTP needs explicit opt-in)",
            );
        }
    }
    fn node_geometry(
        &mut self,
        geometry: (u16, u32, u16),
        endpoint: &str,
        resource: &str,
        path: &str,
    ) {
        let (channels, start, count) = geometry;
        // audio-graph.js / AudioSettings.cpp / write-pcms: exact channel geometry.
        self.range(u64::from(channels), 1, 16, &format!("{path}.channels"));
        self.range(u64::from(start), 1, 65535, &format!("{path}.channel_start"));
        self.check(
            count == channels,
            &format!("{path}.channel_count"),
            "must equal channels",
        );
        self.check(
            u64::from(start) + u64::from(channels) <= 65536,
            &format!("{path}.channel_start"),
            "channel range overflows",
        );
        self.check(
            endpoint.chars().count() <= 160,
            &format!("{path}.endpoint"),
            "at most 160 characters",
        );
        self.check(
            resource.chars().count() <= 160,
            &format!("{path}.resource"),
            "at most 160 characters",
        );
    }
    fn source(&mut self, source: &AudioSource, path: &str) {
        self.node_geometry(
            (source.channels, source.channel_start, source.channel_count),
            &source.endpoint,
            &source.resource,
            path,
        );
        match source.kind {
            EndpointType::Ndi => {
                self.check(
                    source.alsa_pcm.is_none(),
                    &format!("{path}.alsa_pcm"),
                    "NDI has no ALSA PCM",
                );
                if let Some(output) = source.output {
                    self.output_id(output, &format!("{path}.output"));
                } else {
                    self.check(false, &format!("{path}.output"), "NDI requires an output");
                }
            }
            EndpointType::Alsa => {
                self.check(
                    source.output.is_none(),
                    &format!("{path}.output"),
                    "ALSA has no NDI output",
                );
                self.check(
                    source.alsa_pcm.as_deref().is_some_and(valid_pcm),
                    &format!("{path}.alsa_pcm"),
                    "invalid ALSA PCM",
                );
            }
        }
        // v1 factors remain subject to api/audio-settings.js bounds in all engines.
        self.gain(source.gain_db, &format!("{path}.gain_db"));
        self.range(
            u64::from(source.delay_ms),
            0,
            5000,
            &format!("{path}.delay_ms"),
        );
    }
    fn sink(&mut self, sink: &AudioSink, path: &str) {
        self.node_geometry(
            (sink.channels, sink.channel_start, sink.channel_count),
            &sink.endpoint,
            &sink.resource,
            path,
        );
        self.check(
            valid_pcm(&sink.alsa_pcm),
            &format!("{path}.alsa_pcm"),
            "invalid ALSA PCM",
        );
        self.range(
            u64::from(sink.delay_ms),
            0,
            5000,
            &format!("{path}.delay_ms"),
        );
        for (id, gain) in &sink.mix {
            self.check(
                identifier(id),
                &format!("{path}.mix.{id}"),
                "invalid source ID",
            );
            self.gain(*gain, &format!("{path}.mix.{id}"));
            // AudioSettings.cpp keeps unknown mix IDs as a warning, not refusal.
        }
    }
    fn audio(&mut self, audio: &Audio) {
        // api/audio-settings.js and audio-graph.js AJV schemas / AudioSettings.cpp.
        self.range(
            u64::from(audio.settings.rate),
            48000,
            48000,
            "audio.settings.rate",
        );
        self.check(
            [128, 256, 512, 1024, 2048].contains(&audio.settings.period_frames),
            "audio.settings.period_frames",
            "must be 128, 256, 512, 1024 or 2048",
        );
        self.range(
            u64::from(audio.settings.memory_budget_mib),
            1,
            512,
            "audio.settings.memory_budget_mib",
        );
        self.range(
            u64::from(audio.video_delay_ms),
            0,
            5000,
            "audio.video_delay_ms",
        );
        self.check(
            audio.sources.len() <= 32,
            "audio.sources",
            "at most 32 sources",
        );
        self.check(audio.sinks.len() <= 32, "audio.sinks", "at most 32 sinks");
        self.check(
            audio.outputs.len() <= 8,
            "audio.outputs",
            "at most 8 outputs",
        );
        let empty = audio.sources.is_empty() && audio.sinks.is_empty() && audio.routes.is_empty();
        self.check(
            if empty {
                audio.settings.master.is_empty()
            } else {
                audio
                    .sinks
                    .get(&audio.settings.master)
                    .is_some_and(|s| s.enabled)
            },
            "audio.settings.master",
            "must name an enabled sink, or be empty for an empty graph",
        );
        for (id, source) in &audio.sources {
            let path = format!("audio.sources.{id}");
            self.check(identifier(id), &path, "invalid source ID");
            self.source(source, &path);
        }
        for (id, sink) in &audio.sinks {
            let path = format!("audio.sinks.{id}");
            self.check(identifier(id), &path, "invalid sink ID");
            self.sink(sink, &path);
        }
        // audio-graph.js: one physical writer per PCM/resource. V1 did not reject
        // duplicate PCM names; migration preserves those under v1 until normalized.
        if audio.engine == AudioEngine::V2 {
            let sinks: Vec<_> = audio.sinks.iter().collect();
            for (i, (id, a)) in sinks.iter().enumerate() {
                for (other, b) in sinks.iter().skip(i + 1) {
                    let same = a.alsa_pcm == b.alsa_pcm
                        && (a.alsa_pcm != "auto" || a.resource == b.resource);
                    self.check(
                        !same
                            && !overlap(
                                &a.resource,
                                a.channel_start,
                                a.channel_count,
                                &b.resource,
                                b.channel_start,
                                b.channel_count,
                            ),
                        &format!("audio.sinks.{id}"),
                        &format!("overlapping physical writer with {other}"),
                    );
                }
            }
        }
        let mut contributions = BTreeSet::new();
        let mut inputs: BTreeMap<OutputId, BTreeSet<String>> = BTreeMap::new();
        for (id, route) in &audio.routes {
            let path = format!("audio.routes.{id}");
            self.check(identifier(id), &path, "invalid route ID");
            self.output_id(route.output, &format!("{path}.output"));
            let output = audio.outputs.get(&route.output);
            self.check(
                output.is_some(),
                &format!("{path}.output"),
                "missing output",
            );
            let source = audio.sources.get(&route.source);
            let sink = audio.sinks.get(&route.sink);
            self.check(
                source.is_some(),
                &format!("{path}.source"),
                "missing source",
            );
            self.check(sink.is_some(), &format!("{path}.sink"), "missing sink");
            // api/audio-legacy.js preserves v1 components even when their sum
            // exceeds the v2 graph limits; activation of v2 revalidates strictly.
            if audio.engine == AudioEngine::V2 {
                self.gain(route.gain_db, &format!("{path}.gain_db"));
            } else {
                self.check(
                    route.gain_db.is_finite() && (-120.0..=24.0).contains(&route.gain_db),
                    &format!("{path}.gain_db"),
                    "v1 effective gain must be -120..=24",
                );
            }
            self.range(
                u64::from(route.delay_ms),
                0,
                if audio.engine == AudioEngine::V1 {
                    10000
                } else {
                    5000
                },
                &format!("{path}.delay_ms"),
            );
            if let (Some(source), Some(sink), Some(output)) = (source, sink, output) {
                if route.enabled && output.enabled && source.enabled && sink.enabled {
                    let a = source_identity(source);
                    let b = sink_identity(sink);
                    self.check(
                        contributions.insert((a, b)),
                        &path,
                        "duplicate canonical source/sink contribution",
                    );
                    inputs
                        .entry(route.output)
                        .or_default()
                        .insert(route.source.clone());
                }
            }
        }
        // audio-graph.js selected sources cannot overlap within an active bus.
        for (output, selected) in inputs {
            let selected: Vec<_> = selected
                .iter()
                .filter_map(|id| audio.sources.get(id))
                .collect();
            for (i, a) in selected.iter().enumerate() {
                for b in selected.iter().skip(i + 1) {
                    self.check(
                        source_identity(a) != source_identity(b)
                            && !overlap(
                                &a.resource,
                                a.channel_start,
                                a.channel_count,
                                &b.resource,
                                b.channel_start,
                                b.channel_count,
                            ),
                        &format!("audio.outputs.{output}.inputs"),
                        "overlapping source selections",
                    );
                }
            }
        }
        for (id, output) in &audio.outputs {
            let path = format!("audio.outputs.{id}");
            self.output_id(*id, &path);
            self.check(
                output.inputs.len() <= 32,
                &format!("{path}.inputs"),
                "at most 32 inputs",
            );
            self.check(
                output.destinations.len() <= 32,
                &format!("{path}.destinations"),
                "at most 32 destinations",
            );
            for (i, input) in output.inputs.iter().enumerate() {
                let p = format!("{path}.inputs[{i}]");
                self.check(
                    !input.endpoint.is_empty() && input.endpoint.chars().count() <= 160,
                    &format!("{p}.endpoint"),
                    "endpoint must be 1..=160 characters",
                );
                self.gain(input.gain_db, &format!("{p}.gain_db"));
                self.range(u64::from(input.delay_ms), 0, 5000, &format!("{p}.delay_ms"));
            }
            for (i, dest) in output.destinations.iter().enumerate() {
                let p = format!("{path}.destinations[{i}]");
                self.check(
                    !dest.endpoint.is_empty() && dest.endpoint.chars().count() <= 160,
                    &format!("{p}.endpoint"),
                    "endpoint must be 1..=160 characters",
                );
                self.range(u64::from(dest.delay_ms), 0, 5000, &format!("{p}.delay_ms"));
                if audio.engine == AudioEngine::V2 && output.enabled {
                    for input in &output.inputs {
                        self.check(
                            u64::from(input.delay_ms) + u64::from(dest.delay_ms) <= 5000,
                            &format!("{p}.delay_ms"),
                            "combined input/destination delay exceeds 5000 ms",
                        );
                    }
                }
            }
            for (kind, ids) in [
                (
                    "inputs",
                    output
                        .inputs
                        .iter()
                        .map(|i| &i.endpoint)
                        .collect::<Vec<_>>(),
                ),
                (
                    "destinations",
                    output
                        .destinations
                        .iter()
                        .map(|d| &d.endpoint)
                        .collect::<Vec<_>>(),
                ),
            ] {
                if output.enabled {
                    self.check(
                        ids.iter().collect::<BTreeSet<_>>().len() == ids.len(),
                        &format!("{path}.{kind}"),
                        "duplicate endpoint choice",
                    );
                }
            }
            for (node, source) in &output.definitions.sources {
                self.check(
                    identifier(node),
                    &format!("{path}.definitions.sources.{node}"),
                    "invalid source ID",
                );
                self.source(source, &format!("{path}.definitions.sources.{node}"));
            }
            for (node, sink) in &output.definitions.sinks {
                self.check(
                    identifier(node),
                    &format!("{path}.definitions.sinks.{node}"),
                    "invalid sink ID",
                );
                self.sink(sink, &format!("{path}.definitions.sinks.{node}"));
            }
        }
    }
    fn network(&mut self, network: &NetworkProfiles) {
        // ndimon_network_profiles.py::validate + ndimon-net.py::validate_request.
        if network.backend.is_none() {
            self.check(
                network.interface.is_empty()
                    && network.networks.is_empty()
                    && network.roles == NetworkRoles::default(),
                "network",
                "unadopted network must have empty interface, networks and roles",
            );
            return;
        }
        self.check(
            !network.interface.is_empty()
                && network.interface.len() <= 15
                && network
                    .interface
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "network.interface",
            "invalid physical interface",
        );
        self.check(
            (1..=9).contains(&network.networks.len()),
            "network.networks",
            "configure 1..=9 networks",
        );
        let mut ids = BTreeSet::new();
        let mut tags = BTreeSet::new();
        let mut subnets = Vec::new();
        for (i, row) in network.networks.iter().enumerate() {
            let path = format!("network.networks[{i}]");
            self.check(
                identifier(&row.id) && ids.insert(row.id.clone()),
                &format!("{path}.id"),
                "must be a unique identifier",
            );
            self.check(
                !row.name.trim().is_empty()
                    && row.name.chars().count() <= 64
                    && !row.name.chars().any(|c| (c as u32) < 32),
                &format!("{path}.name"),
                "nonempty name of at most 64 characters without controls",
            );
            self.check(
                tags.insert(row.vlan_id),
                &format!("{path}.vlan_id"),
                "duplicate VLAN/untagged network",
            );
            if let Some(tag) = row.vlan_id {
                self.range(u64::from(tag), 1, 4094, &format!("{path}.vlan_id"));
                self.check(
                    format!("{}.{tag}", network.interface).len() <= 15,
                    &format!("{path}.vlan_id"),
                    "derived interface exceeds 15 bytes",
                );
            }
            self.check(
                row.id == network.roles.management
                    || (row.gateway.is_empty() && row.dns.is_empty() && row.dns_search.is_empty()),
                &path,
                "gateway, DNS and search domains belong only to management",
            );
            self.check(
                row.dns_search.len() <= MAX_DNS_SEARCH,
                &format!("{path}.dns_search"),
                "at most 6 DNS search domains",
            );
            for (j, domain) in row.dns_search.iter().enumerate() {
                self.check(
                    valid_dns_search_domain(domain),
                    &format!("{path}.dns_search[{j}]"),
                    "must be a host name",
                );
            }
            self.check(
                row.dns.len() <= 3,
                &format!("{path}.dns"),
                "at most 3 DNS servers",
            );
            for (j, ip) in row.dns.iter().enumerate() {
                self.check(
                    ip.parse::<Ipv4Addr>().is_ok(),
                    &format!("{path}.dns[{j}]"),
                    "must be IPv4",
                );
            }
            if row.mode == AddressMode::Dhcp {
                self.check(
                    row.address.is_empty()
                        && row.prefix == 0
                        && row.gateway.is_empty()
                        && row.dns.is_empty(),
                    &path,
                    "DHCP settings must be normalized (empty static fields)",
                );
                continue;
            }
            self.range(u64::from(row.prefix), 8, 30, &format!("{path}.prefix"));
            let ip = row.address.parse::<Ipv4Addr>().ok();
            self.check(ip.is_some(), &format!("{path}.address"), "must be IPv4");
            let gateway = row.gateway.parse::<Ipv4Addr>().ok();
            self.check(
                row.gateway.is_empty() || gateway.is_some(),
                &format!("{path}.gateway"),
                "must be IPv4",
            );
            if let Some(ip) = ip {
                self.check(
                    !ip.is_loopback()
                        && !ip.is_link_local()
                        && !ip.is_multicast()
                        && !ip.is_unspecified(),
                    &format!("{path}.address"),
                    "must be usable unicast IPv4",
                );
                if (8..=30).contains(&row.prefix) {
                    let mask = u32::MAX << (32 - u32::from(row.prefix));
                    let first = u32::from(ip) & mask;
                    let last = first | !mask;
                    self.check(
                        u32::from(ip) != first && u32::from(ip) != last,
                        &format!("{path}.address"),
                        "network/broadcast addresses are not usable",
                    );
                    if let Some(gw) = gateway {
                        self.check(
                            u32::from(gw) & mask == first,
                            &format!("{path}.gateway"),
                            "gateway must be in the same subnet",
                        );
                    }
                    self.check(
                        !subnets.iter().any(|(a, b)| first <= *b && *a <= last),
                        &format!("{path}.address"),
                        "static subnets overlap",
                    );
                    subnets.push((first, last));
                }
            }
        }
        self.check(
            tags.iter().filter(|t| t.is_some()).count() <= 8,
            "network.networks",
            "at most 8 tagged VLANs",
        );
        for (role, id) in [
            ("management", &network.roles.management),
            ("ndi", &network.roles.ndi),
            ("audio", &network.roles.audio),
        ] {
            self.check(
                ids.contains(id),
                &format!("network.roles.{role}"),
                "role must name an existing network",
            );
        }
    }
    fn tessera(&mut self, tessera: &Tessera, audio: &Audio) {
        // plugins/tessera/routes.js: byte lengths, ranges and legacy plugin limits.
        let device = &tessera.device;
        self.check(
            !device.name.is_empty() && device.name.len() <= 31 && !controls(&device.name),
            "tessera.device.name",
            "must be 1..=31 UTF-8 bytes without controls",
        );
        self.range(
            u64::from(device.sample_rate),
            8000,
            384000,
            "tessera.device.sample_rate",
        );
        self.check(
            [16, 24, 32].contains(&device.encoding),
            "tessera.device.encoding",
            "must be 16, 24 or 32",
        );
        self.range(
            u64::from(device.latency_us),
            1,
            1_000_000,
            "tessera.device.latency_us",
        );
        self.range(
            u64::from(device.rx_channels),
            1,
            16,
            "tessera.device.rx_channels",
        );
        self.range(
            u64::from(device.tx_channels),
            1,
            16,
            "tessera.device.tx_channels",
        );
        for (direction, count, labels) in [
            ("rx", device.rx_channels, &device.rx_labels),
            ("tx", device.tx_channels, &device.tx_labels),
        ] {
            for (channel, label) in labels {
                let path = format!("tessera.device.{direction}_labels.{channel}");
                self.range(u64::from(*channel), 1, u64::from(count), &path);
                self.check(
                    label.len() <= 31 && !controls(label),
                    &path,
                    "label must be at most 31 UTF-8 bytes without controls",
                );
            }
        }
        // audio-endpoints.js: 48 kHz is required when a Tessera endpoint is used.
        let active = audio
            .sources
            .values()
            .any(|s| s.enabled && s.resource.starts_with("tessera:"))
            || audio
                .sinks
                .values()
                .any(|s| s.enabled && s.resource.starts_with("tessera:"));
        self.check(
            !active || device.sample_rate == 48000,
            "tessera.device.sample_rate",
            "active NDIMon audio endpoints require 48000 Hz",
        );
        // flows.js + the retired priv/helper.py (now ndimon-privd's tessera.rs):
        // all channels implicit, one RX, unicast peer.
        let mut ids = BTreeSet::new();
        let mut targets = BTreeSet::new();
        let mut rx = 0;
        for (i, flow) in tessera.flows.iter().enumerate() {
            let path = format!("tessera.flows[{i}]");
            self.check(
                !flow.id.is_empty()
                    && flow.id.len() <= 32
                    && flow
                        .id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    && ids.insert(&flow.id),
                &format!("{path}.id"),
                "unique ID of 1..=32 ASCII letters/digits/_/- required",
            );
            self.range(u64::from(flow.port), 1024, 65535, &format!("{path}.port"));
            // Design D10 / incident #7 strengthens legacy flows.js: IDs alone
            // must not permit a second writer for the same peer/port/direction.
            self.check(
                targets.insert((
                    flow.direction,
                    flow.peer.clone(),
                    flow.port,
                    flow.any_source,
                )),
                &path,
                "duplicate peer/port/direction flow",
            );
            if flow.direction == Direction::Rx {
                rx += 1;
            }
            self.check(
                !flow.any_source
                    || (flow.direction == Direction::Rx
                        && flow.peer.as_deref().unwrap_or("").is_empty()),
                &format!("{path}.any_source"),
                "RX only and excludes peer",
            );
            if !flow.any_source {
                let peer = flow
                    .peer
                    .as_deref()
                    .and_then(|s| s.parse::<Ipv4Addr>().ok());
                self.check(
                    peer.is_some_and(|ip| {
                        let first = ip.octets()[0];
                        first != 0 && first != 127 && first < 224
                    }),
                    &format!("{path}.peer"),
                    "must be canonical unicast IPv4",
                );
            }
            if let Some(frames) = flow.frames_per_packet {
                self.check(
                    flow.direction == Direction::Tx,
                    &format!("{path}.frames_per_packet"),
                    "TX only",
                );
                self.range(
                    u64::from(frames),
                    1,
                    1024,
                    &format!("{path}.frames_per_packet"),
                );
            }
        }
        self.check(
            rx <= 1,
            "tessera.flows",
            "only one RX flow; it carries all channels",
        );
        // flows.js caps compact JSON at 60 KB. Count the legacy write shape,
        // omitting unset options, equally with and without the serde feature.
        self.check(
            flow_write_size(&tessera.flows) <= 60000,
            "tessera.flows",
            "flow set exceeds 60 KB",
        );
    }
    fn display(&mut self, display: &Display) {
        // splash-config.js clamps all four percentages to 0..100. Color strings
        // are free-form in that API; do not add a CSS-only compatibility limit.
        let splash = &display.splash;
        for (name, value) in [
            ("logo_x_pct", splash.logo_x_pct),
            ("logo_y_pct", splash.logo_y_pct),
            ("logo_w_pct", splash.logo_w_pct),
            ("text_height_pct", splash.text_height_pct),
        ] {
            self.check(
                value.is_finite() && (0.0..=100.0).contains(&value),
                &format!("display.splash.{name}"),
                "must be finite and 0..=100",
            );
        }
        if let Some(logo) = &splash.logo {
            self.check(
                logo.sha256.len() == 64
                    && logo
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "display.splash.logo.sha256",
                "must be lowercase SHA-256",
            );
            self.check(
                matches!(logo.media_type.as_str(), "image/png" | "image/jpeg"),
                "display.splash.logo.media_type",
                "logo must be PNG or JPEG (Splash.js)",
            );
        }
        // Splash.js uses JavaScript slice(0,128), i.e. UTF-16 units.
        self.check(
            display.osd.text.encode_utf16().count() <= 128,
            "display.osd.text",
            "at most 128 UTF-16 units",
        );
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn valid_version(value: &str) -> bool {
    value.as_bytes().first().is_some_and(u8::is_ascii_digit)
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".+:~-".contains(&b))
}
fn valid_pcm(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b":_=,.+-".contains(&b))
}
fn controls(value: &str) -> bool {
    value.chars().any(|c| (c as u32) < 32 || c == '\u{7f}')
}
fn valid_host(value: &str) -> bool {
    if value.is_empty() || value.len() > 253 {
        return false;
    }
    if value.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return value.parse::<Ipv4Addr>().is_ok();
    }
    value.split('.').all(|part| {
        !part.is_empty()
            && part.len() <= 63
            && part.as_bytes()[0].is_ascii_alphanumeric()
            && part.as_bytes()[part.len() - 1].is_ascii_alphanumeric()
            && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}
fn ndi_endpoint(value: &str) -> bool {
    let mut parts = value.split(':');
    let ip = parts.next().unwrap_or("").parse::<Ipv4Addr>();
    let port = parts.next();
    ip.is_ok()
        && parts.next().is_none()
        && port.is_none_or(|p| !p.starts_with('0') && p.parse::<u16>().is_ok_and(|n| n > 0))
}
fn overlap(ar: &str, a: u32, ac: u16, br: &str, b: u32, bc: u16) -> bool {
    !ar.is_empty()
        && ar == br
        && u64::from(a) < u64::from(b) + u64::from(bc)
        && u64::from(b) < u64::from(a) + u64::from(ac)
}
fn source_identity(source: &AudioSource) -> String {
    if source.kind == EndpointType::Ndi {
        format!("ndi:{:?}", source.output)
    } else if source.resource.is_empty() {
        format!("alsa:{}", source.alsa_pcm.as_deref().unwrap_or(""))
    } else {
        format!(
            "{}:{}:{}",
            source.resource, source.channel_start, source.channel_count
        )
    }
}
fn sink_identity(sink: &AudioSink) -> String {
    if sink.resource.is_empty() {
        format!("alsa:{}", sink.alsa_pcm)
    } else {
        format!(
            "{}:{}:{}",
            sink.resource, sink.channel_start, sink.channel_count
        )
    }
}

fn json_string_size(value: &str) -> usize {
    2 + value
        .chars()
        .map(|ch| match ch {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            ch if (ch as u32) < 32 => 6,
            ch => ch.len_utf8(),
        })
        .sum::<usize>()
}
fn flow_write_size(flows: &[TesseraFlow]) -> usize {
    2 + flows
        .iter()
        .map(|flow| {
            let direction = match flow.direction {
                Direction::Rx => "rx",
                Direction::Tx => "tx",
            };
            let mut size = 2
                + "\"id\":".len()
                + json_string_size(&flow.id)
                + ",\"direction\":".len()
                + json_string_size(direction)
                + ",\"port\":".len()
                + flow.port.to_string().len();
            if let Some(peer) = &flow.peer {
                size += ",\"peer\":".len() + json_string_size(peer);
            }
            if flow.any_source {
                size += ",\"any_source\":true".len();
            }
            if let Some(frames) = flow.frames_per_packet {
                size += ",\"frames_per_packet\":".len() + frames.to_string().len();
            }
            size
        })
        .sum::<usize>()
        + flows.len().saturating_sub(1)
}

/// Strict ASCII DNS hostname, without a trailing dot, wildcard, port or IP.
pub fn valid_web_hostname(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.split('.').all(valid_device_label)
        && value.parse::<std::net::IpAddr>().is_err()
        // Browsers also interpret all-numeric final labels as IPv4 forms
        // (including abbreviated and hexadecimal literals).
        && value.rsplit('.').next().is_some_and(|label| {
            label.bytes().any(|b| b.is_ascii_alphabetic())
                && !label.to_ascii_lowercase().strip_prefix("0x").is_some_and(|hex| {
                    !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit())
                })
        })
}

/// NE-2.12a: a DNS search domain is a host name of 1..253 ASCII characters made
/// of dot-separated labels (no trailing dot). Unlike `valid_web_hostname` it
/// may be entirely numeric and may be a single label (`local`).
pub fn valid_dns_search_domain(value: &str) -> bool {
    !value.is_empty() && value.len() <= 253 && value.split('.').all(valid_device_label)
}

/// Longest search list a network row may carry (NE-2.12a).
pub const MAX_DNS_SEARCH: usize = 6;

// Appliance labels may be entirely numeric; other network validators retain
// their existing IPv4-or-DNS policy.
fn valid_device_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn valid_device_ntp(value: &str) -> bool {
    if value.split('.').count() == 4 && value.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return value.parse::<Ipv4Addr>().is_ok();
    }
    value.len() <= 253 && value.split('.').all(valid_device_label)
}
