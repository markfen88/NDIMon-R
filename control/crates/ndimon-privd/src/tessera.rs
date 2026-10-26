//! Fixed Tessera operations. The renderers keep the bytes of the retired Python
//! helper (see tests/golden); the runtime is installed by Tessera's own
//! `tessera-bundle` from the pinned, checksummed bundle.
use crate::{
    Host,
    process::args,
    store::{Directory, io_error},
};
use ndimon_privd_proto::{
    ActionOutput, BundleAction, ChannelCount, Error, Request, TesseraFlowSpec, TesseraPcmSpec,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
/// The installer shipped at the root of every Tessera runtime bundle.
const INSTALLER: &str = "tessera-bundle";
/// Where a runtime bundle is unpacked, because its installer is executed from
/// there. Not below `/run`: Debian 13 and Ubuntu mount it `noexec`, so the
/// installer failed with "Permission denied" on a stock device. Root-private
/// (tmpfiles.d, 0700) on the persistent root filesystem; every extraction is
/// removed when its job ends and tmpfiles ages out what a killed job left.
pub const BUNDLE_STAGE: &str = "/var/lib/ndimon-privd/bundles";
pub const FLOW: &str = "/etc/tessera/config.d/61-ndimon-flows.toml";
pub const ALSA: &str = "/etc/alsa/conf.d/60-ndimon-tessera.conf";
pub const RANGES: &str = "/etc/alsa/conf.d/61-ndimon-tessera-ranges.conf";
pub fn native(host: &Host) -> Result<bool, Error> {
    Ok(
        ndimon_render::ownership(ndimon_types::Domain::Tessera) == ndimon_render::Ownership::Native
            && host
                .root
                .read(
                    "/var/lib/ndimon/state/state.json",
                    ndimon_privd_proto::MAX_CONFIG_BYTES,
                )?
                .is_some(),
    )
}
fn legacy_write(host: &Host) -> Result<(), Error> {
    if native(host)? {
        Err(Error::invalid(
            "tessera",
            "domain is native; submit a control-plane command",
        ))
    } else {
        Ok(())
    }
}
fn atomic(host: &Host, path: &str, text: &str) -> Result<(), Error> {
    let (dir, name) = host.root.parent(path, true, true)?;
    dir.root_owned_target(&name)?;
    dir.atomic(
        &name,
        text.as_bytes(),
        nix::unistd::geteuid().as_raw(),
        nix::unistd::getegid().as_raw(),
        0o644,
    )
}

pub fn render_flows(flows: &[TesseraFlowSpec]) -> Result<String, Error> {
    Request::TesseraWriteFlows {
        flows: flows.to_vec(),
    }
    .validate()?;
    let mut text = String::from("# NDIMon-R Tessera plugin; static flows, all device channels\n");
    for flow in flows {
        text.push_str(&format!(
            "[[flow]]\nid = {}\ndirection = \"{}\"\ndevice = \"main\"\nchannels = []\n",
            serde_json::to_string(flow.id.as_str()).map_err(|e| Error::failed("flow render", e))?,
            flow.direction.as_str()
        ));
        if flow.any_source {
            text.push_str("any_source = true\n");
        } else {
            text.push_str(&format!(
                "peer = \"{}\"\n",
                flow.peer
                    .ok_or_else(|| Error::invalid("peer", "required"))?
            ));
        }
        text.push_str(&format!("port = {}\n", flow.port.get()));
        if let Some(frames) = flow.frames_per_packet {
            text.push_str(&format!("frames_per_packet = {}\n", frames.get()));
        }
    }
    Ok(text)
}
pub fn render_alsa(rx: ChannelCount, tx: ChannelCount) -> String {
    format!(
        "# NDIMon-R Tessera plugin\npcm.ndimon_net_in {{ type tessera device \"main\" first_channel 1 channels {} client \"ndimon-r\" }}\npcm.ndimon_net_out {{ type tessera device \"main\" first_channel 1 channels {} client \"ndimon-r\" }}\n",
        rx.get(),
        tx.get()
    )
}
pub fn render_pcms(ranges: &[TesseraPcmSpec]) -> Result<String, Error> {
    Request::TesseraWritePcms {
        ranges: ranges.to_vec(),
    }
    .validate()?;
    let mut text =
        String::from("# NDIMon-R Tessera endpoint ranges; generated from the audio graph\n");
    for range in ranges.iter().collect::<BTreeSet<_>>() {
        let d = range.direction.as_str();
        let f = range.first_channel.get();
        let c = range.channels.get();
        text.push_str(&format!("pcm.ndimon_tessera_{d}_{f}_{c} {{ type tessera device \"main\" first_channel {f} channels {c} client \"ndimon-r\" }}\n"));
    }
    Ok(text)
}
fn copy_tree(source: &Path, dest: &Path) -> Result<(), Error> {
    let m = fs::symlink_metadata(source).map_err(io_error)?;
    if m.file_type().is_symlink() {
        return Err(Error::PermissionDenied);
    }
    if m.is_dir() {
        fs::create_dir_all(dest).map_err(io_error)?;
        for entry in fs::read_dir(source).map_err(io_error)? {
            let e = entry.map_err(io_error)?;
            copy_tree(&e.path(), &dest.join(e.file_name()))?;
        }
    } else if m.is_file() {
        fs::copy(source, dest).map_err(io_error)?;
    } else {
        return Err(Error::PermissionDenied);
    }
    Ok(())
}
/// What `check_artifacts` could say about the Tessera configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checked {
    /// Nothing of Tessera's was rendered, or `tesseractl` approved it.
    Valid,
    /// `tesseractl` is not installed: nothing can vouch for the configuration.
    ValidatorMissing,
}
/// Validate the complete candidate Tessera config before publishing any file.
/// All content comes from RenderArtifacts, never from an HTTP/privd caller.
/// A device with a Tessera configuration and no `tesseractl` (the 0.1.0 plugin
/// never shipped one) is not a bad request: the caller leaves Tessera's files
/// alone and renders the rest (NI-17).
pub fn check_artifacts(
    host: &Host,
    artifacts: &[crate::artifacts::RenderedArtifact],
) -> Result<Checked, Error> {
    if !artifacts
        .iter()
        .any(|a| a.artifact.target() == "/etc/tessera/config.d/60-ndimon.toml")
    {
        return Ok(Checked::Valid);
    }
    let runtime = host.root.directory("/run", false, true)?;
    let stage = tempfile::Builder::new()
        .prefix("ndimon-tessera-config-")
        .tempdir_in(runtime.path())
        .map_err(io_error)?;
    let stage_path = stage.path().canonicalize().map_err(io_error)?;
    copy_tree(&host.root.path("/etc/tessera"), &stage_path)?;
    fs::create_dir_all(stage_path.join("config.d")).map_err(io_error)?;
    for artifact in artifacts {
        let target = artifact.artifact.target();
        if let Some(name) = target.strip_prefix("/etc/tessera/config.d/") {
            if name.contains('/') {
                return Err(Error::invalid("artifact", "invalid Tessera artifact"));
            }
            if let Some(bytes) = &artifact.bytes {
                fs::write(stage_path.join("config.d").join(name), bytes).map_err(io_error)?;
            }
        }
    }
    match host.runner.run(
        "/usr/bin/tesseractl",
        &args(&[
            "config",
            "check",
            "--config-dir",
            stage_path
                .to_str()
                .ok_or_else(|| Error::invalid("stage", "non-UTF8 path"))?,
        ]),
        None,
    ) {
        Ok(_) => Ok(Checked::Valid),
        // `timeout` reports a program it could not start as 127.
        Err(Error::Failed {
            exit_code: Some(127),
            ..
        }) => Ok(Checked::ValidatorMissing),
        Err(e) => Err(Error::invalid("tessera", e.to_string())),
    }
}
pub fn write_flows(host: &Host, flows: &[TesseraFlowSpec]) -> Result<ActionOutput, Error> {
    legacy_write(host)?;
    let text = render_flows(flows)?;
    host.root.parent(FLOW, true, true)?;
    let runtime = host.root.directory("/run", false, true)?;
    let stage = tempfile::Builder::new()
        .prefix("ndimon-tessera-config-")
        .tempdir_in(runtime.path())
        .map_err(io_error)?;
    // The real path is passed across exec; /proc/self/fd descriptors are CLOEXEC.
    let stage_path = stage.path().canonicalize().map_err(io_error)?;
    copy_tree(&host.root.path("/etc/tessera"), &stage_path)?;
    fs::create_dir_all(stage_path.join("config.d")).map_err(io_error)?;
    fs::write(
        stage_path.join("config.d/61-ndimon-flows.toml"),
        text.as_bytes(),
    )
    .map_err(io_error)?;
    host.runner.run(
        "/usr/bin/tesseractl",
        &args(&[
            "config",
            "check",
            "--config-dir",
            stage_path
                .to_str()
                .ok_or_else(|| Error::failed("stage", "non-UTF8 path"))?,
        ]),
        None,
    )?;
    atomic(host, FLOW, &text)?;
    Ok(ActionOutput::default())
}
pub fn write_alsa(host: &Host, rx: ChannelCount, tx: ChannelCount) -> Result<ActionOutput, Error> {
    legacy_write(host)?;
    atomic(host, ALSA, &render_alsa(rx, tx))?;
    Ok(ActionOutput::default())
}
pub fn write_pcms(host: &Host, ranges: &[TesseraPcmSpec]) -> Result<ActionOutput, Error> {
    legacy_write(host)?;
    atomic(host, RANGES, &render_pcms(ranges)?)?;
    Ok(ActionOutput::default())
}
pub fn enable(host: &Host) -> Result<ActionOutput, Error> {
    crate::network::sync_tessera(host)?;
    crate::system::systemctl(host, &["start", "tesserad.service"])
}
pub fn disable(host: &Host) -> Result<ActionOutput, Error> {
    crate::system::systemctl(host, &["stop", "tesserad.service"])
}
pub fn install(host: &Host) -> Result<ActionOutput, Error> {
    crate::network::sync_tessera(host)?;
    bundle(host, &BundleAction::Install { restart: false })?;
    if !native(host)? {
        write_alsa(
            host,
            2.try_into().map_err(|e| Error::invalid("rx", e))?,
            2.try_into().map_err(|e| Error::invalid("tx", e))?,
        )?;
    }
    Ok(ActionOutput::default())
}
pub fn uninstall(host: &Host, purge: bool) -> Result<ActionOutput, Error> {
    bundle(host, &BundleAction::Uninstall { purge })?;
    for path in [
        "/etc/tessera/config.d/60-ndimon.toml",
        "/etc/tessera/config.d/90-probe-flows.toml",
        FLOW,
        ALSA,
        RANGES,
        "/etc/tessera/config.d/62-ndimon-network.toml",
        "/etc/systemd/system/tesserad.service.d/70-ndimon-network.conf",
    ] {
        if host.root.path(path).parent().is_some_and(|p| p.exists()) {
            let (dir, name) = host.root.parent(path, false, true)?;
            dir.root_owned_target(&name)?;
            dir.remove(&name)?;
        }
    }
    crate::system::systemctl(host, &["daemon-reload"])
}
/// A runtime bundle that passed every check the installer relies on. The
/// extraction directory lives as long as this value.
struct Verified {
    // The extraction is addressed through `/proc/self/fd/<stage>`, so the pinned
    // staging directory must outlive it. Closed first, the extraction could not
    // be removed and every job left 41 MB behind, until a 384 MB /run was full
    // and a job could no longer write its result (NI-21). Fields drop in order.
    _work: tempfile::TempDir,
    _stage: Directory,
    directory: PathBuf,
    lock: Value,
}
/// The bundle of a plugin tree (`bundle/`, `bundle-lock.json`): exactly one
/// archive, the reviewed SHA-256, the layout (one executable `tessera-bundle`)
/// and the manifest against the lock. Nothing is installed or changed; the
/// installed tree and a staged one are checked by the same code.
fn verify(host: &Host, plugin: &Directory) -> Result<Verified, Error> {
    let dir = plugin.child("bundle", false)?;
    dir.trusted()?;
    let mut archives = vec![];
    for e in fs::read_dir(dir.path()).map_err(io_error)? {
        let e = e.map_err(io_error)?;
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("tessera-bundle_") && name.ends_with(".tar.gz") {
            archives.push(name);
        }
    }
    if archives.len() != 1 {
        return Err(Error::invalid(
            "bundle",
            "exactly one verified local bundle is required",
        ));
    }
    let name = &archives[0];
    let source = dir.open(name)?;
    let lock: Value = serde_json::from_slice(
        &plugin
            .read("bundle-lock.json", 65536)?
            .ok_or_else(|| Error::invalid("bundle", "lock missing"))?,
    )
    .map_err(|e| Error::invalid("bundle", e.to_string()))?;
    let sums = String::from_utf8(
        dir.read("SHA256SUMS", 65536)?
            .ok_or_else(|| Error::invalid("bundle", "checksums missing"))?,
    )
    .map_err(|e| Error::invalid("bundle", e.to_string()))?;
    let expected: Vec<_> = sums.split_whitespace().collect();
    if expected.len() != 2
        || expected[1].trim_start_matches('*') != name
        || Some(expected[0]) != lock["sha256"].as_str()
    {
        return Err(Error::invalid("bundle", "bundle checksum mismatch"));
    }
    // Snapshot the reviewed, root-owned archive; hash and extract the same bytes.
    let work = host.root.directory(BUNDLE_STAGE, true, true)?;
    let temp = tempfile::Builder::new()
        .prefix("bundle-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(work.path())
        .map_err(io_error)?;
    let path = temp.path().canonicalize().map_err(io_error)?;
    let mut bytes = vec![];
    source
        .take(200 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > 200 * 1024 * 1024 || format!("{:x}", Sha256::digest(&bytes)) != expected[0] {
        return Err(Error::invalid("bundle", "bundle checksum mismatch"));
    }
    crate::plugins::unpack(std::io::Cursor::new(bytes), &path)?;
    let mut roots = vec![];
    find_installer(&path, &mut roots)?;
    if roots.is_empty() {
        return Err(Error::invalid(
            "bundle",
            "bundle has no tessera-bundle installer",
        ));
    }
    if roots.len() != 1 {
        return Err(Error::invalid("bundle", "invalid bundle layout"));
    }
    let directory = roots.remove(0);
    let manifest: Value =
        serde_json::from_slice(&fs::read(directory.join("manifest.json")).map_err(io_error)?)
            .map_err(|e| Error::invalid("bundle", e.to_string()))?;
    if ["name", "version", "arch", "commit"]
        .iter()
        .any(|k| manifest[*k] != lock[*k])
        || lock["os"]
            .as_object()
            .is_none_or(|os| os.iter().any(|(k, v)| manifest["os"][k] != *v))
    {
        return Err(Error::invalid(
            "bundle",
            "bundle manifest differs from reviewed lock",
        ));
    }
    if fs::metadata(directory.join(INSTALLER))
        .map_err(io_error)?
        .permissions()
        .mode()
        & 0o111
        == 0
    {
        return Err(Error::invalid(
            "bundle",
            "bundle installer is not executable",
        ));
    }
    Ok(Verified {
        _work: temp,
        _stage: work,
        directory,
        lock,
    })
}
/// The platform flags of `tessera-bundle install` for a locked bundle.
fn install_flags(lock: &Value) -> Result<Vec<String>, Error> {
    // The reviewed SHA-256 lock above authenticates the archive; Tessera's
    // own release-key list is empty until its owner provisions a key.
    let mut argv = args(&[
        "install",
        "--profile",
        "ndimon",
        "--admin-user",
        "ndimon",
        "--allow-unsigned",
        "--yes",
    ]);
    if lock["os"]["id"] == "ubuntu"
        && lock["os"]["version_id"] == "24.04"
        && lock["arch"] == "arm64"
    {
        argv.push("--allow-ubuntu-24.04".into());
    } else if !(lock["os"]["id"] == "debian"
        && lock["os"]["version_id"] == "13"
        && (lock["arch"] == "amd64" || lock["arch"] == "arm64"))
    {
        return Err(Error::Unsupported {
            what: "runtime bundle platform".into(),
        });
    }
    Ok(argv)
}
/// Everything `bundle(.., Install)` would refuse, asked of a staged plugin tree
/// before it replaces anything: a refused bundle then leaves no plugin files
/// and no broken marker, and the caller gets the reason.
pub fn check_staged(host: &Host, stage: &Directory) -> Result<(), Error> {
    let verified = verify(host, stage)?;
    install_flags(&verified.lock).map(|_| ())
}
pub fn bundle(host: &Host, action: &BundleAction) -> Result<ActionOutput, Error> {
    let plugin = host
        .root
        .directory("/opt/ndimon-r/plugins/tessera", false, true)?;
    let verified = verify(host, &plugin)?;
    let argv = match action {
        BundleAction::Install { restart } => {
            let mut argv = install_flags(&verified.lock)?;
            if *restart {
                argv.push("--restart".into());
            }
            argv
        }
        BundleAction::Uninstall { purge } => {
            let mut argv = args(&["uninstall", "--yes"]);
            if *purge {
                argv.push("--purge".into());
            }
            argv
        }
    };
    // This is a reviewed root-owned executable from the pinned archive, not
    // caller text; the subcommand and flags are fixed above.
    host.runner.run(
        verified
            .directory
            .join(INSTALLER)
            .to_str()
            .ok_or_else(|| Error::invalid("bundle", "non-UTF8 executable"))?,
        &argv,
        None,
    )
}
fn find_installer(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Error> {
    for e in fs::read_dir(dir).map_err(io_error)? {
        let e = e.map_err(io_error)?;
        let kind = e.file_type().map_err(io_error)?;
        if kind.is_dir() {
            find_installer(&e.path(), out)?;
        } else if kind.is_file() && e.file_name() == INSTALLER {
            out.push(dir.into());
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn python_flow_golden() {
        let f:Vec<TesseraFlowSpec>=serde_json::from_value(json!([{"id":"rx_1","direction":"rx","port":5004,"any_source":true},{"id":"tx-1","direction":"tx","peer":"192.0.2.8","port":5006,"frames_per_packet":48}])).unwrap();
        assert_eq!(
            render_flows(&f).unwrap(),
            include_str!("../tests/golden/flows.toml")
        );
        assert_eq!(
            render_flows(&[]).unwrap(),
            "# NDIMon-R Tessera plugin; static flows, all device channels\n"
        );
    }
    #[test]
    fn python_alsa_golden() {
        assert_eq!(
            render_alsa(2.try_into().unwrap(), 16.try_into().unwrap()),
            include_str!("../tests/golden/alsa.conf")
        );
    }
    #[test]
    fn python_pcm_golden() {
        let ranges:Vec<TesseraPcmSpec>=serde_json::from_value(json!([{"direction":"tx","first_channel":2,"channels":2},{"direction":"rx","first_channel":1,"channels":1},{"direction":"tx","first_channel":2,"channels":2}])).unwrap();
        assert_eq!(
            render_pcms(&ranges).unwrap(),
            include_str!("../tests/golden/pcms.conf")
        );
    }
    /// Fails every program the way `timeout` reports one it cannot start.
    #[derive(Debug)]
    struct Absent;
    impl crate::process::Runner for Absent {
        fn run(
            &self,
            program: &str,
            _: &[String],
            _: Option<&[u8]>,
        ) -> Result<ActionOutput, Error> {
            Err(Error::Failed {
                step: program.into(),
                reason: format!(
                    "timeout: failed to run command '{program}': No such file or directory"
                ),
                exit_code: Some(127),
            })
        }
    }
    #[test] // NI-17: no tesseractl is not a bad request, and a validator that says no still is.
    fn a_missing_validator_is_reported_and_a_refusing_one_still_refuses() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mode = |path: std::path::PathBuf, mode| {
            std::fs::create_dir_all(&path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        mode(dir.path().into(), 0o700);
        for part in ["etc", "etc/tessera", "etc/tessera/config.d", "run"] {
            mode(dir.path().join(part), 0o755);
        }
        let host = Host {
            root: crate::store::Root(dir.path().into()),
            runner: std::sync::Arc::new(Absent),
            ndimon_gid: nix::unistd::getegid().as_raw(),
            renderer: std::sync::Arc::new(crate::artifacts::UnavailableRenderer),
        };
        let device = vec![crate::artifacts::RenderedArtifact {
            artifact: crate::artifacts::Artifact::TesseraDevice,
            bytes: Some(b"[[device]]\nid = \"main\"\n".to_vec()),
        }];
        assert_eq!(
            check_artifacts(&host, &device).unwrap(),
            Checked::ValidatorMissing
        );
        // Nothing of Tessera's: nothing to check, so nothing is missing.
        assert_eq!(check_artifacts(&host, &[]).unwrap(), Checked::Valid);
        // A validator that ran and refused is the caller's bad configuration.
        #[derive(Debug)]
        struct Refuses;
        impl crate::process::Runner for Refuses {
            fn run(
                &self,
                program: &str,
                _: &[String],
                _: Option<&[u8]>,
            ) -> Result<ActionOutput, Error> {
                Err(Error::Failed {
                    step: program.into(),
                    reason: "device: name is required".into(),
                    exit_code: Some(1),
                })
            }
        }
        let host = Host {
            runner: std::sync::Arc::new(Refuses),
            ..host
        };
        assert!(matches!(
            check_artifacts(&host, &device),
            Err(Error::Invalid { .. })
        ));
    }
}
