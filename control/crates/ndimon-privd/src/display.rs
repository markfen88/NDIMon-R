//! Native logo publication and the retired legacy-logo writer gate.
use crate::{
    Host,
    artifacts::{Artifact, RenderedArtifact},
};
use ndimon_privd_proto::{Error, LogoFile, MAX_CONFIG_BYTES};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn guard_legacy(host: &Host) -> Result<(), Error> {
    if ndimon_render::ownership(ndimon_types::Domain::Display) == ndimon_render::Ownership::Native
        && host
            .root
            .read("/var/lib/ndimon/state/state.json", MAX_CONFIG_BYTES)?
            .is_some()
    {
        return Err(Error::invalid(
            "logo",
            "display is native; submit a control-plane command",
        ));
    }
    Ok(())
}
pub fn render(host: &Host, state: &Value) -> Result<Vec<RenderedArtifact>, Error> {
    if ndimon_render::ownership(ndimon_types::Domain::Display) != ndimon_render::Ownership::Native {
        return Ok(vec![]);
    }
    let logo = &state["document"]["display"]["splash"]["logo"];
    let selected = if logo.is_null() {
        None
    } else {
        let asset: ndimon_types::AssetRef = serde_json::from_value(logo.clone())
            .map_err(|e| Error::invalid("logo", e.to_string()))?;
        if asset.sha256.len() != 64
            || !asset
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::invalid("logo", "invalid hash"));
        }
        let bytes = host
            .root
            .read(
                &format!("/var/lib/ndimon/assets/{}", asset.sha256),
                3 * 1024 * 1024,
            )?
            .ok_or_else(|| Error::invalid("logo", "asset missing"))?;
        let file = match asset.media_type.as_str() {
            "image/png" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => LogoFile::Png,
            "image/jpeg" if bytes.starts_with(b"\xff\xd8\xff") => LogoFile::Jpg,
            _ => return Err(Error::invalid("logo", "asset media type mismatch")),
        };
        if format!("{:x}", Sha256::digest(&bytes)) != asset.sha256 {
            return Err(Error::invalid("logo", "asset digest mismatch"));
        }
        Some((file, bytes))
    };
    Ok([LogoFile::Png, LogoFile::Jpg, LogoFile::Jpeg]
        .into_iter()
        .map(|file| RenderedArtifact {
            artifact: Artifact::Logo(file),
            bytes: selected
                .as_ref()
                .filter(|(selected, _)| *selected == file)
                .map(|(_, bytes)| bytes.clone()),
        })
        .collect())
}
