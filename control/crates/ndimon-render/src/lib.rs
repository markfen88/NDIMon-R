//! No I/O. JSON artifacts contain only the legacy reader's keys; provenance is
//! a separate manifest. See README.md for the mixed device-file merge rule.
pub mod audio;
pub mod device;
pub mod display;
pub mod legacy;
pub mod ndi;
pub mod network;
pub mod outputs;
pub mod plugins;
pub mod presets;
pub mod tessera;

use ndimon_types::{DesiredState, Domain};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ownership {
    Native,
    Followed,
}

/// The only domain ownership policy. A later phase changes an entry here.
pub const OWNERSHIP: &[(Domain, Ownership)] = &[
    (Domain::Audio, Ownership::Native),
    (Domain::Device, Ownership::Native),
    (Domain::Outputs, Ownership::Native),
    (Domain::Ndi, Ownership::Native),
    (Domain::Network, Ownership::Native),
    (Domain::Tessera, Ownership::Native),
    (Domain::Display, Ownership::Native),
    (Domain::Presets, Ownership::Native),
    (Domain::Plugins, Ownership::Native),
];
pub fn ownership(domain: Domain) -> Ownership {
    OWNERSHIP
        .iter()
        .find(|(d, _)| *d == domain)
        .map(|(_, o)| *o)
        .unwrap_or(Ownership::Followed)
}

#[derive(Debug, Clone, Copy)]
pub struct PathSpec {
    pub path_id: &'static str,
    pub path: &'static str,
    pub domain: Domain,
    pub owner: &'static str,
    pub group: &'static str,
    pub mode: u32,
}
macro_rules! path {
    ($id:literal, $path:literal, $domain:ident) => {
        PathSpec {
            path_id: $id,
            path: $path,
            domain: Domain::$domain,
            owner: "root",
            group: "ndimon",
            mode: 0o640,
        }
    };
}
pub const PATHS: &[PathSpec] = &[
    path!("audio", "/etc/ndimon-audio-settings.json", Audio),
    path!("audio-engine", "/etc/ndimon-device-settings.json", Audio),
    path!("device", "/etc/ndimon-device-settings.json", Device),
    path!("about", "/etc/ndimon-about-settings.json", Device),
    path!("dec1", "/etc/ndimon-dec1-settings.json", Outputs),
    path!("dec2", "/etc/ndimon-dec2-settings.json", Outputs),
    path!("dec3", "/etc/ndimon-dec3-settings.json", Outputs),
    path!("dec4", "/etc/ndimon-dec4-settings.json", Outputs),
    path!("dec5", "/etc/ndimon-dec5-settings.json", Outputs),
    path!("dec6", "/etc/ndimon-dec6-settings.json", Outputs),
    path!("dec7", "/etc/ndimon-dec7-settings.json", Outputs),
    path!("dec8", "/etc/ndimon-dec8-settings.json", Outputs),
    path!("receive", "/etc/ndimon-rx-settings.json", Ndi),
    path!("find", "/etc/ndimon-find-settings.json", Ndi),
    path!("groups", "/etc/ndi-group.json", Ndi),
    path!("extra-ips", "/etc/ndi-config.json", Ndi),
    path!("splash", "/etc/ndimon-splash-settings.json", Display),
    path!("osd", "/etc/ndimon-osd-settings.json", Display),
    path!("presets", "/etc/ndimon-presets.json", Presets),
    path!("plugins", "/etc/ndimon-plugins.json", Plugins),
    PathSpec {
        path_id: "network",
        path: "/var/lib/ndimon-network/committed.json",
        domain: Domain::Network,
        owner: "root",
        group: "root",
        mode: 0o600,
    },
    PathSpec {
        path_id: "network-bindings",
        path: "/etc/ndimon-network-interfaces.json",
        domain: Domain::Network,
        owner: "root",
        group: "root",
        mode: 0o644,
    },
    PathSpec {
        path_id: "network-banner",
        path: "/run/ndimon/net-pending",
        domain: Domain::Network,
        owner: "root",
        group: "root",
        mode: 0o644,
    },
    PathSpec {
        path_id: "network-ndi-slice",
        path: "/etc/systemd/system/ndimonndi.slice",
        domain: Domain::Network,
        owner: "root",
        group: "root",
        mode: 0o644,
    },
    PathSpec {
        path_id: "network-audio-slice",
        path: "/etc/systemd/system/ndimontessera.slice",
        domain: Domain::Network,
        owner: "root",
        group: "root",
        mode: 0o644,
    },
    PathSpec {
        path_id: "network-decoder-slice",
        path: "/etc/systemd/system/ndimon-r.service.d/70-ndimon-network.conf",
        domain: Domain::Network,
        owner: "root",
        group: "root",
        mode: 0o644,
    },
    PathSpec {
        path_id: "network-finder-slice",
        path: "/etc/systemd/system/ndimon-finder.service.d/70-ndimon-network.conf",
        domain: Domain::Network,
        owner: "root",
        group: "root",
        mode: 0o644,
    },
    PathSpec {
        path_id: "network-tessera-slice",
        path: "/etc/systemd/system/tesserad.service.d/70-ndimon-network.conf",
        domain: Domain::Network,
        owner: "root",
        group: "root",
        mode: 0o644,
    },
    PathSpec {
        path_id: "tessera-device",
        path: "/etc/tessera/config.d/60-ndimon.toml",
        domain: Domain::Tessera,
        owner: "root",
        group: "root",
        mode: 0o644,
    },
    PathSpec {
        path_id: "tessera-flows",
        path: "/etc/tessera/config.d/61-ndimon-flows.toml",
        domain: Domain::Tessera,
        owner: "root",
        group: "root",
        mode: 0o644,
    },
    PathSpec {
        path_id: "tessera-probe-flows",
        path: "/etc/tessera/config.d/90-probe-flows.toml",
        domain: Domain::Tessera,
        owner: "root",
        group: "root",
        mode: 0o644,
    },
    PathSpec {
        path_id: "tessera-network",
        path: "/etc/tessera/config.d/62-ndimon-network.toml",
        domain: Domain::Network,
        owner: "root",
        group: "root",
        mode: 0o644,
    },
    PathSpec {
        path_id: "alsa",
        path: "/etc/alsa/conf.d/60-ndimon-tessera.conf",
        domain: Domain::Tessera,
        owner: "root",
        group: "root",
        mode: 0o644,
    },
    PathSpec {
        path_id: "alsa-ranges",
        path: "/etc/alsa/conf.d/61-ndimon-tessera-ranges.conf",
        domain: Domain::Tessera,
        owner: "root",
        group: "root",
        mode: 0o644,
    },
];

/// Mixed files must be reread under the legacy writer lock before rendering.
pub const MIXED_PATHS: &[&str] = &[
    "/etc/ndimon-plugins.json", // retained unknown fields and imported observations
    "/etc/ndimon-device-settings.json",
    "/etc/ndimon-dec1-settings.json",
    "/etc/ndimon-dec2-settings.json",
    "/etc/ndimon-dec3-settings.json",
    "/etc/ndimon-dec4-settings.json",
    "/etc/ndimon-dec5-settings.json",
    "/etc/ndimon-dec6-settings.json",
    "/etc/ndimon-dec7-settings.json",
    "/etc/ndimon-dec8-settings.json",
];
pub fn is_mixed(spec: &PathSpec) -> bool {
    MIXED_PATHS.contains(&spec.path)
}

/// Exact input text, including unknown keys, ordering, omissions and whitespace.
/// Auth, journals, credentials and observed catalogs are never in this map.
pub type LegacyFiles = BTreeMap<String, String>;

#[derive(Debug, Clone)]
pub struct Artifact {
    pub path_id: &'static str,
    pub bytes: Vec<u8>,
    pub owner: &'static str,
    pub group: &'static str,
    pub mode: u32,
}
impl Artifact {
    pub fn spec(&self) -> &'static PathSpec {
        PATHS
            .iter()
            .find(|p| p.path_id == self.path_id)
            .expect("declared artifact")
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct ManifestEntry {
    pub path: String,
    pub revision: u64,
    pub sha256: String,
    pub owner: String,
    pub group: String,
    pub mode: u32,
}
pub fn manifest(artifacts: &[Artifact], revision: u64) -> Vec<ManifestEntry> {
    artifacts
        .iter()
        .map(|a| ManifestEntry {
            path: a.spec().path.into(),
            revision,
            sha256: format!("{:x}", Sha256::digest(&a.bytes)),
            owner: a.owner.into(),
            group: a.group.into(),
            mode: a.mode,
        })
        .collect()
}

pub fn render(
    state: &DesiredState,
    audio_revision: u64,
    files: &LegacyFiles,
) -> Result<Vec<Artifact>, String> {
    let revisions = BTreeMap::from([(Domain::Audio, audio_revision)]);
    render_with_revisions(state, &revisions, files)
}

/// Also available for previewing the followed-domain renderers. `render` is the
/// write gate and never returns followed artifacts.
pub fn render_path(
    spec: &PathSpec,
    state: &DesiredState,
    revision: u64,
    files: &LegacyFiles,
) -> Result<Vec<u8>, String> {
    let renderer = REGISTRY
        .iter()
        .find(|r| r.domain == spec.domain)
        .ok_or_else(|| format!("unregistered renderer: {:?}", spec.domain))?;
    (renderer.render)(spec, state, revision, files)
}

/// Domain order is stable for artifacts and import adapters.
#[derive(Debug)]
pub struct DomainRenderer {
    pub domain: Domain,
    pub render: RenderDomain,
    pub import: Option<ImportDomain>,
    pub drift: Option<DriftProjection>,
    pub include: Option<fn(&PathSpec, &DesiredState) -> bool>,
}
pub type RenderDomain = fn(&PathSpec, &DesiredState, u64, &LegacyFiles) -> Result<Vec<u8>, String>;
pub type ImportDomain = fn(&mut DesiredState, &LegacyFiles) -> Result<(), String>;
pub type DriftProjection =
    fn(&Artifact, &DesiredState, &LegacyFiles) -> Result<(Vec<u8>, Option<Vec<u8>>), String>;
pub const REGISTRY: &[DomainRenderer] = &[
    DomainRenderer {
        domain: Domain::Device,
        render: device::render_path,
        import: Some(device::import_json),
        drift: Some(device::drift_bytes),
        include: Some(device::include),
    },
    DomainRenderer {
        domain: Domain::Outputs,
        render: outputs::render_path,
        import: Some(outputs::import_json),
        drift: Some(outputs::drift_bytes),
        include: Some(outputs::include),
    },
    DomainRenderer {
        domain: Domain::Ndi,
        render: ndi::render_path,
        import: Some(ndi::import_json),
        drift: None,
        include: None,
    },
    DomainRenderer {
        domain: Domain::Audio,
        render: audio::render_path,
        import: Some(audio::import_json),
        drift: Some(audio::drift_bytes),
        include: None,
    },
    DomainRenderer {
        domain: Domain::Display,
        render: display::render_path,
        import: Some(display::import_json),
        drift: None,
        include: None,
    },
    DomainRenderer {
        domain: Domain::Presets,
        render: presets::render_path,
        import: Some(presets::import_json),
        drift: None,
        include: None,
    },
    DomainRenderer {
        domain: Domain::Plugins,
        render: plugins::render_path,
        import: Some(plugins::import_json),
        drift: Some(plugins::drift_bytes),
        include: None,
    },
    DomainRenderer {
        domain: Domain::Network,
        render: network::render_path,
        import: Some(network::import_json),
        drift: None,
        include: Some(network::include_artifact),
    },
    DomainRenderer {
        domain: Domain::Tessera,
        render: tessera::render_path,
        import: None,
        drift: None,
        include: Some(tessera::include),
    },
];
pub fn render_with_revisions(
    state: &DesiredState,
    revisions: &BTreeMap<Domain, u64>,
    files: &LegacyFiles,
) -> Result<Vec<Artifact>, String> {
    let mut artifacts = Vec::new();
    let mut composed = files.clone();
    for renderer in REGISTRY
        .iter()
        .filter(|r| ownership(r.domain) == Ownership::Native)
    {
        for spec in PATHS.iter().filter(|p| {
            p.domain == renderer.domain && renderer.include.is_none_or(|include| include(p, state))
        }) {
            let bytes = (renderer.render)(
                spec,
                state,
                revisions
                    .get(&spec.domain)
                    .copied()
                    .unwrap_or(state.revision),
                &composed,
            )?;
            if is_mixed(spec) {
                composed.insert(
                    spec.path.into(),
                    String::from_utf8(bytes.clone()).map_err(|e| e.to_string())?,
                );
            }
            artifacts.push(Artifact {
                path_id: spec.path_id,
                bytes,
                owner: spec.owner,
                group: spec.group,
                mode: spec.mode,
            });
        }
    }
    // Shared projections compose in REGISTRY order. Every artifact for a mixed
    // path carries the final bytes, so publication order cannot erase a peer.
    for artifact in &mut artifacts {
        if is_mixed(artifact.spec()) {
            artifact.bytes = composed[artifact.spec().path].as_bytes().to_vec();
        }
    }
    artifacts.sort_by_key(|a| PATHS.iter().position(|p| p.path_id == a.path_id));
    Ok(artifacts)
}

pub fn drift_bytes(
    artifact: &Artifact,
    state: &DesiredState,
    files: &LegacyFiles,
) -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
    let renderer = REGISTRY
        .iter()
        .find(|r| r.domain == artifact.spec().domain)
        .ok_or_else(|| "unregistered drift renderer".to_string())?;
    if let Some(project) = renderer.drift {
        project(artifact, state, files)
    } else {
        Ok((
            artifact.bytes.clone(),
            files
                .get(artifact.spec().path)
                .map(|s| s.as_bytes().to_vec()),
        ))
    }
}

/// JSON.stringify's numeric representation for integral floating point values.
pub fn node_json(mut value: Value) -> Result<Vec<u8>, String> {
    fn numbers(v: &mut Value) {
        match v {
            Value::Number(n) if n.is_f64() => {
                if let Some(f) = n
                    .as_f64()
                    .filter(|f| f.fract() == 0.0 && f.abs() <= 9_007_199_254_740_991.0)
                {
                    *v = json!(f as i64);
                }
            }
            Value::Object(m) => m.values_mut().for_each(numbers),
            Value::Array(a) => a.iter_mut().for_each(numbers),
            _ => (),
        }
    }
    numbers(&mut value);
    serde_json::to_vec(&value).map_err(|e| e.to_string())
}

/// Replace just a top-level JSON value. All bytes outside its value span survive,
/// including other keys, escape spellings and whitespace. Duplicate keys refuse.
pub fn replace_key(raw: &str, key: &str, value: &Value) -> Result<Vec<u8>, String> {
    let parsed: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    if !parsed.is_object() {
        return Err("expected JSON object".into());
    }
    let bytes = raw.as_bytes();
    let mut i = bytes
        .iter()
        .position(|b| *b == b'{')
        .ok_or("expected object")?
        + 1;
    let mut found = None;
    let mut count = 0;
    loop {
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if bytes.get(i) == Some(&b'}') {
            break;
        }
        let start = i;
        i = string_end(bytes, i)?;
        let name: String = serde_json::from_slice(&bytes[start..i]).map_err(|e| e.to_string())?;
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if bytes.get(i) != Some(&b':') {
            return Err("expected colon".into());
        }
        i += 1;
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        let start = i;
        i = value_end(bytes, i)?;
        if name == key {
            if found.is_some() {
                return Err(format!("duplicate key {key}"));
            }
            found = Some((start, i));
        }
        count += 1;
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        match bytes.get(i) {
            Some(b',') => i += 1,
            Some(b'}') => break,
            _ => return Err("expected object separator".into()),
        }
    }
    let replacement = node_json(value.clone())?;
    let mut out = Vec::new();
    if let Some((start, end)) = found {
        out.extend_from_slice(&bytes[..start]);
        out.extend(replacement);
        out.extend_from_slice(&bytes[end..]);
    } else {
        out.extend_from_slice(&bytes[..i]);
        if count > 0 {
            out.push(b',');
        }
        out.extend(serde_json::to_vec(key).map_err(|e| e.to_string())?);
        out.push(b':');
        out.extend(replacement);
        out.extend_from_slice(&bytes[i..]);
    }
    Ok(out)
}
fn string_end(b: &[u8], start: usize) -> Result<usize, String> {
    if b.get(start) != Some(&b'"') {
        return Err("expected string".into());
    }
    let mut i = start + 1;
    while let Some(c) = b.get(i) {
        match c {
            b'\\' => i += 2,
            b'"' => return Ok(i + 1),
            _ => i += 1,
        }
    }
    Err("unterminated string".into())
}
fn value_end(b: &[u8], mut i: usize) -> Result<usize, String> {
    if b.get(i) == Some(&b'"') {
        return string_end(b, i);
    }
    if matches!(b.get(i), Some(b'{' | b'[')) {
        let mut depth = 0;
        while let Some(c) = b.get(i) {
            match c {
                b'"' => {
                    i = string_end(b, i)?;
                    continue;
                }
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(i + 1);
                    }
                }
                _ => (),
            }
            i += 1;
        }
        return Err("unterminated value".into());
    }
    let start = i;
    while b
        .get(i)
        .is_some_and(|c| !c.is_ascii_whitespace() && *c != b',' && *c != b'}')
    {
        i += 1;
    }
    if i == start {
        Err("missing value".into())
    } else {
        Ok(i)
    }
}
