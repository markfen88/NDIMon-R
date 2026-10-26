//! Render only native domains. Destinations and metadata are declared by the
//! pure renderer; caller-supplied paths/content never cross this boundary.
use crate::Host;
use base64::{Engine, engine::general_purpose::STANDARD};
use ndimon_privd_proto::{Error, LegacyConfigFile, LogoFile, MAX_CONFIG_BYTES};
use serde_json::Value;
#[derive(Debug, Clone)]
pub enum Artifact {
    State(&'static ndimon_render::PathSpec),
    Legacy(LegacyConfigFile),
    Logo(LogoFile),
    TesseraDevice,
    TesseraFlows,
    TesseraNetwork,
    Alsa,
    AlsaRanges,
    NdiDecoder,
    NdiFinder,
}
impl Artifact {
    pub fn target(&self) -> String {
        match self {
            Self::State(spec) => spec.path.into(),
            Self::Legacy(file) => format!("/etc/{}", file.filename()),
            Self::Logo(file) => format!("/etc/{}", file.filename()),
            Self::TesseraDevice => "/etc/tessera/config.d/60-ndimon.toml".into(),
            Self::TesseraFlows => "/etc/tessera/config.d/61-ndimon-flows.toml".into(),
            Self::TesseraNetwork => "/etc/tessera/config.d/62-ndimon-network.toml".into(),
            Self::Alsa => "/etc/alsa/conf.d/60-ndimon-tessera.conf".into(),
            Self::AlsaRanges => "/etc/alsa/conf.d/61-ndimon-tessera-ranges.conf".into(),
            Self::NdiDecoder => "/run/ndimon/ndi/decoder/ndi-config.v1.json".into(),
            Self::NdiFinder => "/run/ndimon/ndi/finder/ndi-config.v1.json".into(),
        }
    }
    pub fn mode(&self) -> u32 {
        match self {
            Self::State(spec) => spec.mode,
            Self::Legacy(_) | Self::Logo(_) | Self::NdiDecoder | Self::NdiFinder => 0o640,
            _ => 0o644,
        }
    }
    pub fn group(&self, host: &Host) -> u32 {
        match self {
            Self::State(spec) if spec.group == "ndimon" => host.ndimon_gid,
            Self::Legacy(_) | Self::Logo(_) | Self::NdiDecoder | Self::NdiFinder => host.ndimon_gid,
            _ => nix::unistd::getegid().as_raw(),
        }
    }
}
#[derive(Debug)]
pub struct RenderedArtifact {
    pub artifact: Artifact,
    pub bytes: Option<Vec<u8>>,
}
pub trait Renderer: std::fmt::Debug + Send + Sync {
    fn available(&self) -> bool {
        true
    }
    fn render(&self, state: &Value, revision: u64) -> Result<Vec<RenderedArtifact>, Error>;
}
#[derive(Debug)]
pub struct UnavailableRenderer;
impl Renderer for UnavailableRenderer {
    fn available(&self) -> bool {
        false
    }
    fn render(&self, _: &Value, _: u64) -> Result<Vec<RenderedArtifact>, Error> {
        Err(Error::Unsupported {
            what: "state artifact renderer (N2)".into(),
        })
    }
}
#[derive(Debug)]
pub struct StateRenderer;
impl Renderer for StateRenderer {
    fn render(&self, state: &Value, revision: u64) -> Result<Vec<RenderedArtifact>, Error> {
        let document: ndimon_types::DesiredState = serde_json::from_value(
            state
                .get("document")
                .cloned()
                .ok_or_else(|| Error::invalid("state", "document missing"))?,
        )
        .map_err(|e| Error::invalid("state", e.to_string()))?;
        if document.revision != revision {
            return Err(Error::invalid(
                "revision",
                "document revision does not match envelope",
            ));
        }
        ndimon_types::validate(&document).map_err(|errors| {
            Error::invalid(
                "state",
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        })?;
        let files: ndimon_render::LegacyFiles = serde_json::from_value(
            state
                .get("legacy_files")
                .cloned()
                .unwrap_or(serde_json::json!({})),
        )
        .map_err(|e| Error::invalid("legacy_files", e.to_string()))?;
        let revisions = state
            .get("domain_revisions")
            .cloned()
            .ok_or_else(|| Error::invalid("domain_revisions", "missing"))?;
        let revisions: std::collections::BTreeMap<ndimon_types::Domain, u64> =
            serde_json::from_value(revisions)
                .map_err(|e| Error::invalid("domain_revisions", e.to_string()))?;
        if !revisions.contains_key(&ndimon_types::Domain::Audio) {
            return Err(Error::invalid("domain_revisions.audio", "missing"));
        }
        ndimon_render::render_with_revisions(&document, &revisions, &files)
            .map_err(|e| Error::failed("render", e))?
            .into_iter()
            .map(|a| {
                let spec = a.spec();
                Ok(RenderedArtifact {
                    artifact: Artifact::State(spec),
                    bytes: Some(a.bytes),
                })
            })
            .collect()
    }
}
/// Drop what belongs to Tessera: its domain's artifacts and everything under
/// `/etc/tessera`, whichever domain renders it.
fn without_tessera(artifacts: &mut Vec<RenderedArtifact>) {
    artifacts.retain(|rendered| {
        let target = rendered.artifact.target();
        !target.starts_with("/etc/tessera/")
            && !ndimon_render::PATHS
                .iter()
                .any(|p| p.path == target && p.domain == ndimon_types::Domain::Tessera)
    });
}
pub fn render(host: &Host, revision: u64) -> Result<Value, Error> {
    if !host.renderer.available() {
        return Err(Error::Unsupported {
            what: "state artifact renderer (N2)".into(),
        });
    }
    let bytes = host
        .root
        .read("/var/lib/ndimon/state/state.json", MAX_CONFIG_BYTES)?
        .ok_or_else(|| Error::invalid("revision", "state document missing"))?;
    let mut state: Value =
        serde_json::from_slice(&bytes).map_err(|e| Error::invalid("state", e.to_string()))?;
    if state.get("revision").and_then(Value::as_u64) != Some(revision) {
        return Err(Error::invalid("revision", "state revision does not match"));
    }
    let etc = host.root.directory("/etc", true, true)?;
    let _lock = etc.lock(".ndimon-config.lock")?;
    // Legacy writers may still touch the rest of a mixed file. Read it under the
    // same lock as legacy writes; a two-second follower interval must not lose
    // its edits.
    for path in ndimon_render::MIXED_PATHS {
        if let Some(bytes) = host.root.read(path, MAX_CONFIG_BYTES)? {
            let raw = String::from_utf8(bytes)
                .map_err(|e| Error::invalid("legacy_files", e.to_string()))?;
            state["legacy_files"][*path] = Value::String(raw);
        }
    }
    let mut artifacts = crate::display::render(host, &state)?;
    artifacts.extend(host.renderer.render(&state, revision)?);
    // Render the whole set successfully before touching any destination.
    for rendered in &artifacts {
        let target = rendered.artifact.target();
        if matches!(&rendered.artifact, Artifact::Logo(_)) {
            continue;
        }
        let spec = ndimon_render::PATHS
            .iter()
            .find(|p| p.path == target)
            .ok_or_else(|| Error::invalid("artifact", "undeclared path"))?;
        if ndimon_render::ownership(spec.domain) != ndimon_render::Ownership::Native {
            return Err(Error::invalid(
                "artifact",
                "followed domain cannot be rendered",
            ));
        }
        if rendered
            .bytes
            .as_ref()
            .is_some_and(|b| b.len() > MAX_CONFIG_BYTES)
        {
            return Err(Error::invalid("artifact", "exceeds 4 MiB"));
        }
    }
    if crate::tessera::check_artifacts(host, &artifacts)?
        == crate::tessera::Checked::ValidatorMissing
    {
        // Control would otherwise stop at its first render, and every write of
        // any domain would fail with it. Tessera's files are neither written
        // nor removed; its commands fail on the absent daemon (NI-17).
        static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !REPORTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            tracing::warn!(
                "tesseractl is not installed: the Tessera configuration is not checked and its files are left as they are"
            );
        }
        without_tessera(&mut artifacts);
    }
    let mut manifest = Vec::new();
    for rendered in artifacts {
        let a = rendered.artifact;
        if let Some(bytes) = rendered.bytes {
            let target = a.target();
            let backup = format!("{target}.pre-control");
            if host.root.read(&backup, MAX_CONFIG_BYTES)?.is_none() {
                if let Some(original) = host.root.read(&target, MAX_CONFIG_BYTES)? {
                    host.root
                        .atomic(&backup, &original, a.group(host), a.mode())?;
                }
            }
            // Only what changed is replaced: the manifest below still lists every
            // artifact, with the digest of what is on disk.
            host.root
                .atomic_if_changed(&target, &bytes, a.group(host), a.mode())?;
            use sha2::{Digest, Sha256};
            let group = match &a {
                Artifact::State(spec) => spec.group,
                _ => "ndimon",
            };
            manifest.push(serde_json::json!({"path":target,"revision":revision,"sha256":format!("{:x}",Sha256::digest(&bytes)),"owner":"root","group":group,"mode":a.mode()}));
        } else {
            host.root.remove(&a.target())?;
        }
    }
    Ok(serde_json::json!(manifest))
}
pub fn legacy_json(host: &Host, file: LegacyConfigFile, json: &Value) -> Result<(), Error> {
    // Today's Python writer only validates JSON syntax, not an object schema.
    // Keep scalar/array acceptance during the transitional legacy phase.
    let etc = host.root.directory("/etc", true, true)?;
    let _lock = etc.lock(".ndimon-config.lock")?;
    let state = host
        .root
        .read("/var/lib/ndimon/state/state.json", MAX_CONFIG_BYTES)?;
    let native = state.is_some()
        && ndimon_render::ownership(ndimon_types::Domain::Audio)
            == ndimon_render::Ownership::Native;
    let path = format!("/etc/{}", file.filename());
    let domain = if file == LegacyConfigFile::Device {
        Some(ndimon_types::Domain::Device)
    } else {
        ndimon_render::PATHS
            .iter()
            .find(|p| p.path == path)
            .map(|p| p.domain)
    };
    // Decoder files contain outputs + NDI + audio. Native outputs owns the
    // artifact, so the audio-only rollback flag cannot reopen a legacy writer.
    if state.is_some()
        && domain.is_some_and(|domain| {
            ndimon_render::ownership(domain) == ndimon_render::Ownership::Native
                && (domain != ndimon_types::Domain::Audio || native)
        })
    {
        return Err(Error::invalid(
            "file",
            "domain is native; submit a control-plane command",
        ));
    }
    let mut json = json.clone();
    if native && file == LegacyConfigFile::Device {
        let state: Value = serde_json::from_slice(&state.ok_or(Error::PermissionDenied)?)
            .map_err(|e| Error::invalid("state", e.to_string()))?;
        if !json.is_object() {
            return Err(Error::invalid(
                "json",
                "device settings must preserve native audio_engine",
            ));
        }
        json["audio_engine"] = state
            .pointer("/document/audio/engine")
            .cloned()
            .ok_or_else(|| Error::invalid("state", "audio engine missing"))?;
    }
    let bytes = serde_json::to_vec(&json).map_err(|e| Error::invalid("json", e.to_string()))?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(Error::invalid("json", "config exceeds 4 MiB"));
    }
    host.root.atomic(
        &format!("/etc/{}", file.filename()),
        &bytes,
        host.ndimon_gid,
        0o640,
    )
}
pub fn legacy_logo(host: &Host, file: LogoFile, data: &str) -> Result<(), Error> {
    crate::display::guard_legacy(host)?;
    let bytes = STANDARD
        .decode(data)
        .map_err(|_| Error::invalid("base64", "invalid logo encoding"))?;
    if bytes.len() > MAX_CONFIG_BYTES
        || !(bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.starts_with(b"\xff\xd8\xff"))
    {
        return Err(Error::invalid(
            "logo",
            "PNG or JPEG, at most 4 MiB required",
        ));
    }
    host.root.atomic(
        &format!("/etc/{}", file.filename()),
        &bytes,
        host.ndimon_gid,
        0o640,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] // NI-17: what a device without tesseractl leaves alone.
    fn without_tessera_keeps_every_other_domains_artifacts() {
        let artifact = |artifact| RenderedArtifact {
            artifact,
            bytes: Some(Vec::new()),
        };
        let mut all = vec![
            artifact(Artifact::TesseraDevice),
            artifact(Artifact::TesseraFlows),
            artifact(Artifact::TesseraNetwork),
            artifact(Artifact::Alsa),
            artifact(Artifact::AlsaRanges),
            artifact(Artifact::NdiDecoder),
            artifact(Artifact::Legacy(LegacyConfigFile::Device)),
        ];
        without_tessera(&mut all);
        let left = all.iter().map(|a| a.artifact.target()).collect::<Vec<_>>();
        assert_eq!(
            left,
            [
                "/run/ndimon/ndi/decoder/ndi-config.v1.json",
                "/etc/ndimon-device-settings.json"
            ]
        );
    }
}
