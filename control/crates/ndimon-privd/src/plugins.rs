//! Trusted plugin lifecycle, with archive snapshotting and a lock across hooks.
use crate::{
    Host,
    process::args,
    store::{Directory, io_error},
};
use ndimon_privd_proto::{
    ActionOutput, DebianVersion, Error, PluginArchive, PluginCommandName, PluginJob, PluginName,
    PluginUnit, RemoveMode, UnitAction,
};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs::{self, File, Permissions},
    io::Read,
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
};
pub mod runtime;
const BASE: &str = "/opt/ndimon-r/plugins";
const MAX_ARCHIVE: u64 = 200 * 1024 * 1024;
pub use ndimon_privd_proto::plugin::{Manifest, native_lifecycle, permitted_service};
const MAX_EXPANDED: u64 = 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 20000;
// Payload cap plus a header and up to one padding block per entry, and EOF.
// This budget includes metadata that tar consumes before yielding an entry.
const MAX_TAR_STREAM: u64 = MAX_EXPANDED + MAX_ENTRIES as u64 * 1024 + 1024;
/// Group ID lookup: NSS on the real root. A test root carries its own etc/group,
/// as it carries its own uploader and state owner.
fn group_id(host: &Host, group: &str) -> Result<Option<u32>, Error> {
    if host.root.0 == Path::new("/") {
        return Ok(nix::unistd::Group::from_name(group)
            .map_err(|e| Error::failed("group lookup", e))?
            .map(|g| g.gid.as_raw()));
    }
    let table = host
        .root
        .read("/etc/group", 1024 * 1024)?
        .unwrap_or_default();
    Ok(String::from_utf8_lossy(&table).lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next() == Some(group))
            .then(|| fields.nth(1).and_then(|gid| gid.parse().ok()))
            .flatten()
    }))
}
fn permitted_group(host: &Host, name: &str, group: &str) -> Result<bool, Error> {
    if ["tessera", "tessera-admin"].contains(&group) && name != "tessera" {
        return Ok(false);
    }
    // The retired privileged-socket groups stay reserved on upgraded devices.
    if [
        "root",
        "sudo",
        "wheel",
        "adm",
        "shadow",
        "disk",
        "docker",
        "lxd",
        "kmem",
        "systemd-journal",
        "ndimon",
        "ndimon-priv",
        "ndimon-priv-control",
        "ndimon-priv-api",
        "ndimon-control",
    ]
    .contains(&group)
    {
        return Ok(false);
    }
    if [
        "audio",
        "video",
        "render",
        "bluetooth",
        "tessera",
        "tessera-admin",
    ]
    .contains(&group)
    {
        return Ok(true);
    }
    Ok(group_id(host, group)?.is_none_or(|gid| gid >= 1000))
}
fn validate_groups(host: &Host, manifest: &Manifest) -> Result<(), Error> {
    for group in &manifest.supplementary_groups {
        if !permitted_group(host, manifest.name.as_str(), group)? {
            return Err(Error::PermissionDenied);
        }
    }
    Ok(())
}
/// The static admission rules of a staged or installed manifest.
pub fn check_manifest(
    host: &Host,
    manifest: &Manifest,
    name: &PluginName,
    expected: Option<&DebianVersion>,
) -> Result<(), Error> {
    manifest.validate(name, expected)?;
    validate_groups(host, manifest)
}
fn base(host: &Host) -> Result<Directory, Error> {
    host.root.directory(BASE, true, true)
}
pub fn with_lock(
    host: &Host,
    f: impl FnOnce() -> Result<ActionOutput, Error>,
) -> Result<ActionOutput, Error> {
    let dir = base(host)?;
    let _lock = dir.lock(".operation-lock")?;
    f()
}
fn destination(host: &Host, name: &PluginName) -> PathBuf {
    host.root.path(&format!("{BASE}/{name}"))
}
fn recover(base: &Directory, name: &PluginName) -> Result<(), Error> {
    let previous = format!(".previous-{name}");
    let dest = base.path().join(name.as_str());
    let old = base.path().join(&previous);
    if old.exists() && !dest.exists() {
        rustix::fs::renameat(&base.0, previous.as_str(), &base.0, name.as_str())
            .map_err(|e| Error::failed("recover plugin", e))?;
    }
    Ok(())
}
fn installed(host: &Host, name: &PluginName) -> Result<Manifest, Error> {
    let dir = host
        .root
        .directory(&format!("{BASE}/{name}"), false, true)?;
    let manifest_file = dir.open("plugin.json")?;
    let metadata = manifest_file.metadata().map_err(io_error)?;
    if !metadata.is_file()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.mode() & 0o022 != 0
    {
        return Err(Error::PermissionDenied);
    }
    let bytes = dir
        .read("plugin.json", 65536)?
        .ok_or_else(|| Error::invalid("manifest", "installed plugin.json missing"))?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|e| Error::invalid("manifest", e.to_string()))?;
    check_manifest(host, &manifest, name, None)?;
    Ok(manifest)
}
fn archive_file(host: &Host, archive: &PluginArchive) -> Result<File, Error> {
    let (directory, id) = match archive {
        PluginArchive::Download { id } => {
            ("/var/lib/ndimon/import/plugin-downloads".to_owned(), id)
        }
        PluginArchive::Import { directory, id } => {
            (format!("/var/lib/ndimon/import/{directory}"), id)
        }
    };
    let dir = host.root.directory(&directory, false, false)?;
    let file = dir.open(id.as_str())?;
    let m = file.metadata().map_err(io_error)?;
    let uid = if host.root.0 == Path::new("/") {
        nix::unistd::User::from_name("ndimon-control")
            .map_err(|e| Error::failed("uploader lookup", e))?
            .ok_or(Error::PermissionDenied)?
            .uid
            .as_raw()
    } else {
        nix::unistd::geteuid().as_raw()
    };
    if !m.is_file() || m.uid() != uid || m.len() > MAX_ARCHIVE {
        return Err(Error::invalid(
            "archive",
            "bundle must be a regular ndimon-control-owned file, at most 200 MiB",
        ));
    }
    Ok(file)
}
struct Prepared {
    _temp: tempfile::TempDir,
    stage: PathBuf,
    /// The same tree, pinned: root-private, so what is checked is what is moved.
    stage_directory: Directory,
    manifest: Manifest,
}
fn prepare(
    host: &Host,
    name: &PluginName,
    archive: &PluginArchive,
    expected: Option<&DebianVersion>,
) -> Result<Prepared, Error> {
    let source = archive_file(host, archive)?;
    let base = base(host)?;
    let temp = tempfile::Builder::new()
        .prefix(".archive-")
        .permissions(Permissions::from_mode(0o700))
        .tempdir_in(base.path().canonicalize().map_err(io_error)?)
        .map_err(io_error)?;
    let path = temp.path().canonicalize().map_err(io_error)?;
    let snapshot = path.join("bundle.tar.gz");
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&snapshot)
        .map_err(io_error)?;
    output
        .set_permissions(Permissions::from_mode(0o600))
        .map_err(io_error)?;
    let total = std::io::copy(&mut source.take(MAX_ARCHIVE + 1), &mut output).map_err(io_error)?;
    if total > MAX_ARCHIVE {
        return Err(Error::invalid("archive", "bundle exceeds 200 MiB"));
    }
    output.sync_all().map_err(io_error)?;
    let stage = path.join("stage");
    fs::DirBuilder::new()
        .mode(0o755)
        .create(&stage)
        .map_err(io_error)?;
    unpack(File::open(snapshot).map_err(io_error)?, &stage)?;
    let bytes = fs::read(stage.join("plugin.json")).map_err(io_error)?;
    if bytes.len() > 65536 {
        return Err(Error::invalid("manifest", "manifest exceeds 64KiB"));
    }
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|e| Error::invalid("manifest", e.to_string()))?;
    check_manifest(host, &manifest, name, expected)?;
    let staging = temp
        .path()
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::invalid("archive", "staging name"))?;
    let stage_directory = base.child(staging, false)?.child("stage", false)?;
    Ok(Prepared {
        _temp: temp,
        stage,
        stage_directory,
        manifest,
    })
}
/// Reject links, device nodes, paths, duplicates and bombs before writing each
/// entry into a newly created root-private staging tree. Archive metadata never
/// controls ownership, setuid bits, xattrs or destination paths.
pub fn unpack(source: impl Read, destination: &Path) -> Result<(), Error> {
    unpack_limited(source, destination, MAX_TAR_STREAM)
}
fn unpack_limited(source: impl Read, destination: &Path, stream_limit: u64) -> Result<(), Error> {
    let decoder = flate2::read::GzDecoder::new(source);
    // tar 0.4.46 reads GNU long-name/link and local PAX metadata into a Vec
    // before yielding the next entry. Bound those reads below the tar parser.
    let mut archive = tar::Archive::new(decoder.take(stream_limit));
    let mut seen = BTreeSet::new();
    let mut count = 0;
    let mut expanded = 0u64;
    for entry in archive.entries().map_err(io_error)? {
        let mut entry = entry.map_err(io_error)?;
        count += 1;
        let name = entry.path_bytes().into_owned();
        let text = std::str::from_utf8(&name)
            .map_err(|_| Error::invalid("archive", "non-UTF8 filename"))?;
        if text.chars().any(|c| c.is_control()) || text.starts_with('/') {
            return Err(Error::invalid("archive", "unsafe archive entry"));
        }
        let mut path = PathBuf::new();
        for component in Path::new(text).components() {
            match component {
                Component::Normal(p) => path.push(p),
                Component::CurDir => (),
                _ => return Err(Error::invalid("archive", "unsafe archive entry")),
            }
        }
        if !seen.insert(path.clone()) {
            return Err(Error::invalid("archive", "duplicate archive entry"));
        }
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(Error::invalid(
                "archive",
                "only files and directories allowed",
            ));
        }
        let size = entry.size();
        expanded = expanded
            .checked_add(size)
            .ok_or_else(|| Error::invalid("archive", "expanded size overflow"))?;
        if count > MAX_ENTRIES || expanded > MAX_EXPANDED {
            return Err(Error::invalid("archive", "archive too large"));
        }
        let target = destination.join(path);
        if kind.is_dir() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o755)
                .create(&target)
                .map_err(io_error)?;
            fs::set_permissions(&target, Permissions::from_mode(0o755)).map_err(io_error)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o755)
                    .create(parent)
                    .map_err(io_error)?;
            }
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(io_error)?;
            let copied = std::io::copy(&mut entry, &mut file).map_err(io_error)?;
            if copied != size {
                return Err(Error::invalid("archive", "truncated member"));
            }
            let mode = entry.header().mode().map_err(io_error)?;
            file.set_permissions(Permissions::from_mode(if mode & 0o111 != 0 {
                0o755
            } else {
                0o644
            }))
            .map_err(io_error)?;
        }
    }
    if archive.into_inner().limit() == 0 {
        return Err(Error::invalid("archive", "archive too large"));
    }
    Ok(())
}
/// The manifest of an archive, admitted by the static rules. Nothing changes.
pub fn inspect(
    host: &Host,
    name: &PluginName,
    archive: &PluginArchive,
    expected: Option<&DebianVersion>,
) -> Result<ActionOutput, Error> {
    inspect_archive(host, name, archive, expected, false)
}
/// What the control plane asks before it accepts an install: `inspect`, and for
/// Tessera everything the install would refuse about the runtime bundle. The
/// control plane renders Tessera's files for an accepted install, so a refusal
/// after that left them behind (VV-3 W2). `install` checks again; it is the same
/// code on the same tree.
pub fn admit(
    host: &Host,
    name: &PluginName,
    archive: &PluginArchive,
    expected: Option<&DebianVersion>,
) -> Result<ActionOutput, Error> {
    inspect_archive(host, name, archive, expected, true)
}
fn inspect_archive(
    host: &Host,
    name: &PluginName,
    archive: &PluginArchive,
    expected: Option<&DebianVersion>,
    bundle: bool,
) -> Result<ActionOutput, Error> {
    let dir = base(host)?;
    let _lock = dir.lock(".operation-lock")?;
    recover(&dir, name)?;
    let prepared = prepare(host, name, archive, expected)?;
    if bundle && name.as_str() == "tessera" && prepared.manifest.process.is_none() {
        crate::tessera::check_staged(host, &prepared.stage_directory)?;
    }
    let value = serde_json::to_value(prepared.manifest)
        .map_err(|e| Error::failed("manifest serialize", e))?;
    Ok(ActionOutput {
        stdout: format!("{value}\n"),
        data: Some(value),
        ..Default::default()
    })
}
fn state(host: &Host, name: &PluginName) -> Result<PathBuf, Error> {
    let directory = format!("/var/lib/ndimon/plugins/{name}");
    let dir = host.root.directory(&directory, true, false)?;
    dir.0
        .set_permissions(Permissions::from_mode(0o700))
        .map_err(io_error)?;
    if host.root.0 == Path::new("/") {
        let user = nix::unistd::User::from_name("ndimon")
            .map_err(|e| Error::failed("user lookup", e))?
            .ok_or(Error::PermissionDenied)?;
        nix::unistd::fchown(dir.0.as_raw_fd(), Some(user.uid), Some(user.gid))
            .map_err(|e| Error::failed("state ownership", e))?;
    }
    Ok(host.root.path(&directory))
}
fn hook(
    host: &Host,
    name: &PluginName,
    file: &str,
    argv: &[String],
) -> Result<ActionOutput, Error> {
    let dest = destination(host, name);
    let dir = host
        .root
        .directory(&format!("{BASE}/{name}"), false, true)?;
    let target = dir.open(file)?;
    let m = target.metadata().map_err(io_error)?;
    if !m.is_file()
        || m.uid() != nix::unistd::geteuid().as_raw()
        || m.mode() & 0o022 != 0
        || m.mode() & 0o111 == 0
    {
        return Err(Error::invalid("hook", "root-owned executable required"));
    }
    host.runner.hook(
        dest.join(file)
            .to_str()
            .ok_or_else(|| Error::invalid("hook", "non-UTF8 path"))?,
        argv,
        &dest,
        &host.root.path(&format!("/var/lib/ndimon/plugins/{name}")),
    )
}
fn dpkg(host: &Host, a: &str, op: &str, b: &str) -> bool {
    host.runner
        .run(
            "/usr/bin/dpkg",
            &args(&["--compare-versions", a, op, b]),
            None,
        )
        .is_ok()
}
fn remove_tree(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => fs::remove_file(path).map_err(io_error),
        Ok(_) => fs::remove_dir_all(path).map_err(io_error),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(e)),
    }
}
pub fn install(
    host: &Host,
    name: &PluginName,
    archive: &PluginArchive,
    expected: Option<&DebianVersion>,
    force: bool,
) -> Result<ActionOutput, Error> {
    let dir = base(host)?;
    let _lock = dir.lock(".operation-lock")?;
    recover(&dir, name)?;
    let prepared = prepare(host, name, archive, expected)?;
    let manifest = &prepared.manifest;
    let firmware = String::from_utf8(
        host.root
            .read("/etc/ndimon-firmware-version", 1024)?
            .ok_or_else(|| {
                Error::invalid("min_ndimon", "host_too_old: firmware version unavailable")
            })?,
    )
    .map_err(|_| Error::invalid("min_ndimon", "host_too_old: invalid firmware version"))?;
    let firmware: DebianVersion = firmware
        .trim()
        .to_owned()
        .try_into()
        .map_err(|_| Error::invalid("min_ndimon", "host_too_old: invalid firmware version"))?;
    if !dpkg(host, firmware.as_str(), "ge", manifest.min_ndimon.as_str()) {
        return Err(Error::invalid("min_ndimon", "host_too_old"));
    }
    let dest = destination(host, name);
    let previous = dir.path().join(format!(".previous-{name}"));
    if dest.exists() && !force {
        let old = installed(host, name)?;
        if dpkg(host, manifest.version.as_str(), "lt", old.version.as_str()) {
            return Err(Error::invalid("version", "downgrade requires --force"));
        }
    }
    // Nothing on the device has changed yet. Refuse a Tessera bundle the
    // installer would refuse (layout, lock, platform) here, so that it leaves no
    // plugin files and no broken marker behind, and the reason reaches the user.
    if name.as_str() == "tessera" && manifest.process.is_none() {
        crate::tessera::check_staged(host, &prepared.stage_directory)?;
    }
    let state = state(host, name)?;
    let _ = state;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(prepared.stage.join("priv"))
        .map_err(io_error)?;
    fs::write(
        prepared.stage.join("priv/ALLOWED"),
        manifest
            .priv_commands
            .iter()
            .map(|s| format!("{s}\n"))
            .collect::<String>(),
    )
    .map_err(io_error)?;
    fs::set_permissions(
        prepared.stage.join("priv/ALLOWED"),
        Permissions::from_mode(0o644),
    )
    .map_err(io_error)?;
    for file in [&manifest.install, &manifest.uninstall]
        .into_iter()
        .filter(|_| manifest.process.is_none() && !native_lifecycle(name.as_str()))
    {
        if !fs::symlink_metadata(prepared.stage.join(file))
            .map_err(io_error)?
            .is_file()
        {
            return Err(Error::invalid(
                "manifest",
                "install/uninstall executables required",
            ));
        }
    }
    remove_tree(&previous)?;
    if dest.exists() {
        fs::rename(&dest, &previous).map_err(io_error)?;
    }
    fs::write(
        prepared.stage.join(".broken"),
        b"installation in progress\n",
    )
    .map_err(io_error)?;
    File::open(prepared.stage.join(".broken"))
        .and_then(|f| f.sync_all())
        .map_err(io_error)?;
    fs::rename(&prepared.stage, &dest).map_err(io_error)?;
    dir.0.sync_all().map_err(io_error)?;
    // Tessera's old helper and scripts are replaced by these Rust handlers.
    let mut outcome = if manifest.process.is_some() {
        Ok(ActionOutput::default())
    } else if name.as_str() == "tessera" {
        crate::tessera::install(host)
    } else {
        hook(host, name, &manifest.install, &[])
    };
    if outcome.is_ok() {
        let groups = (|| -> Result<(), Error> {
            for group in &manifest.supplementary_groups {
                let exists = group_id(host, group)?.is_some();
                if !exists || !permitted_group(host, name.as_str(), group)? {
                    return Err(Error::invalid(
                        "supplementary_groups",
                        "group missing or unsafe after install",
                    ));
                }
            }
            Ok(())
        })();
        if let Err(error) = groups {
            outcome = Err(error);
        }
    }
    if let Err(error) = outcome {
        fs::write(dest.join(".broken"), b"install failed\n").map_err(io_error)?;
        if previous.exists() {
            remove_tree(&dest)?;
            fs::rename(&previous, &dest).map_err(io_error)?;
            fs::write(
                dest.join(".broken"),
                b"update failed; previous version restored\n",
            )
            .map_err(io_error)?;
        }
        return Err(error);
    }
    fs::remove_file(dest.join(".broken")).map_err(io_error)?;
    remove_tree(&previous)?;
    dir.0.sync_all().map_err(io_error)?;
    Ok(outcome.unwrap_or_default())
}
pub fn uninstall(host: &Host, name: &PluginName, mode: RemoveMode) -> Result<ActionOutput, Error> {
    let dir = base(host)?;
    let _lock = dir.lock(".operation-lock")?;
    recover(&dir, name)?;
    let dest = destination(host, name);
    if !dest.exists() {
        remove_tree(&dir.path().join(format!(".previous-{name}")))?;
        return Ok(ActionOutput::default());
    }
    if mode != RemoveMode::Force {
        let manifest = installed(host, name)?;
        let result = if manifest.process.is_some() {
            runtime::stop(host, name)
        } else if name.as_str() == "tessera" {
            crate::tessera::uninstall(host, mode == RemoveMode::Purge)
        } else {
            hook(
                host,
                name,
                &manifest.uninstall,
                &if mode == RemoveMode::Purge {
                    args(&["purge"])
                } else {
                    vec![]
                },
            )
        };
        if let Err(e) = result {
            fs::write(dest.join(".broken"), b"uninstall failed\n").map_err(io_error)?;
            return Err(e);
        }
    }
    remove_tree(&dest)?;
    if mode == RemoveMode::Purge {
        // Pin the registry before deletion, so a renamed API-writable ancestor
        // cannot redirect a root recursive removal to another tree.
        let registry = host
            .root
            .directory("/var/lib/ndimon/plugins", false, false)?;
        registry.trusted()?;
        remove_tree(&registry.path().join(name.as_str()))?;
    }
    Ok(ActionOutput::default())
}
pub fn execute(
    host: &Host,
    name: &PluginName,
    command: &PluginCommandName,
) -> Result<ActionOutput, Error> {
    let dir = base(host)?;
    let _lock = dir.lock(".operation-lock")?;
    recover(&dir, name)?;
    let manifest = installed(host, name)?;
    if !manifest.priv_commands.contains(command) {
        return Err(Error::PermissionDenied);
    }
    let priv_dir = host
        .root
        .directory(&format!("{BASE}/{name}/priv"), false, true)?;
    let allow = priv_dir.open("ALLOWED")?;
    let metadata = allow.metadata().map_err(io_error)?;
    if !metadata.is_file()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.mode() & 0o022 != 0
    {
        return Err(Error::PermissionDenied);
    }
    let bytes = priv_dir
        .read("ALLOWED", 65536)?
        .ok_or(Error::PermissionDenied)?;
    if !String::from_utf8_lossy(&bytes)
        .lines()
        .any(|s| s == command.as_str())
    {
        return Err(Error::PermissionDenied);
    }
    if name.as_str() == "tessera" {
        return match command.as_str() {
            "enable" => crate::tessera::enable(host),
            "disable" => crate::tessera::disable(host),
            _ => Err(Error::Unsupported {
                what: "Tessera command requires its typed request".into(),
            }),
        };
    }
    let target = priv_dir.open(command.as_str())?;
    let metadata = target.metadata().map_err(io_error)?;
    if !metadata.is_file()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
    {
        return Err(Error::PermissionDenied);
    }
    let dest = destination(host, name);
    let path = dest.join("priv").join(command.as_str());
    host.runner.hook(
        path.to_str().ok_or(Error::PermissionDenied)?,
        &[],
        &dest,
        &host.root.path(&format!("/var/lib/ndimon/plugins/{name}")),
    )
}
pub fn service(host: &Host, unit: &PluginUnit, action: UnitAction) -> Result<ActionOutput, Error> {
    if action == UnitAction::Reload {
        return Err(Error::invalid("action", "start/stop/restart only"));
    }
    let dir = base(host)?;
    let _lock = dir.lock(".operation-lock")?;
    let mut allowed = false;
    for entry in fs::read_dir(dir.path()).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        if !entry.file_type().map_err(io_error)?.is_dir() {
            continue;
        }
        let Ok(name) = PluginName::try_from(entry.file_name().to_string_lossy().into_owned())
        else {
            continue;
        };
        if entry.path().join(".broken").exists() {
            continue;
        }
        if let Ok(manifest) = installed(host, &name) {
            allowed |=
                permitted_service(name.as_str(), unit.as_str()) && manifest.services.contains(unit);
        }
    }
    if !allowed {
        return Err(Error::PermissionDenied);
    }
    crate::system::systemctl(host, &[action.as_str(), "--no-block", "--", unit.as_str()])
}
/// What a finished job left behind: its result file, as a durable status.
fn job_result(
    dir: &Directory,
    number: &str,
    progress: &Option<Value>,
) -> Result<Option<ActionOutput>, Error> {
    let Some(bytes) = dir.read(
        &format!("{number}.result.json"),
        ndimon_privd_proto::MAX_FRAME_BYTES,
    )?
    else {
        return Ok(None);
    };
    let result: ndimon_privd_proto::ReplyResult =
        serde_json::from_slice(&bytes).map_err(|e| Error::failed("job result", e))?;
    Ok(Some(ActionOutput {
        stdout: "inactive\n".into(),
        data: Some(serde_json::json!({"state":"inactive","result":result,"progress":progress})),
        ..Default::default()
    }))
}
pub fn job_status(host: &Host, job: &PluginJob) -> Result<ActionOutput, Error> {
    let number = job
        .as_str()
        .strip_suffix(".service")
        .and_then(|s| s.rsplit_once('-'))
        .map(|(_, id)| id)
        .ok_or_else(|| Error::invalid("job", "invalid job id"))?;
    // /run may have been lost since the control-plane decision was persisted.
    // Still query systemd so an absent result is observed as an unknown outcome.
    let dir = host.root.directory("/run/ndimon/privd-jobs", true, true)?;
    let progress = dir
        .read(&format!("{number}.progress.json"), 4096)?
        .map(|bytes| {
            serde_json::from_slice::<Value>(&bytes).map_err(|e| Error::failed("job progress", e))
        })
        .transpose()?;
    if let Some(done) = job_result(&dir, number, &progress)? {
        return Ok(done);
    }
    let result = crate::system::systemctl(
        host,
        &[
            "show",
            "--property=ActiveState",
            "--value",
            "--",
            job.as_str(),
        ],
    )?;
    let state = match result.stdout.trim() {
        "inactive" | "failed" => "inactive",
        "active" | "activating" | "deactivating" | "reloading" | "refreshing" => "active",
        _ => return Err(Error::failed("job status", "unknown job state")),
    };
    // The job writes its result before it exits and its unit is collected once
    // it has, so a job that finished between the file lookup above and systemd's
    // answer reads inactive with no result. Asking again settles it: an
    // inactive unit has nothing left to write. Without this a fast refusal
    // looked like a job that vanished (VV-3 W1).
    if state == "inactive" {
        if let Some(done) = job_result(&dir, number, &progress)? {
            return Ok(done);
        }
    }
    Ok(ActionOutput {
        stdout: format!("{state}\n"),
        data: Some(serde_json::json!({"state":state,"progress":progress})),
        ..Default::default()
    })
}
/// Stop a job's unit. A job that has ended is collected with its unit, which
/// systemctl cannot stop: cancelling it again is done, not a failure.
pub fn job_cancel(host: &Host, job: &PluginJob) -> Result<ActionOutput, Error> {
    match crate::system::systemctl(host, &["stop", "--no-block", "--", job.as_str()]) {
        Ok(output) => Ok(output),
        Err(error) => {
            let state = crate::system::systemctl(
                host,
                &[
                    "show",
                    "--property=ActiveState",
                    "--value",
                    "--",
                    job.as_str(),
                ],
            );
            match state {
                Ok(state) if state.stdout.trim() == "inactive" => Ok(ActionOutput::default()),
                _ => Err(error),
            }
        }
    }
}

/// Reset only the rendered native registry after the actor accepted empty intent.
/// This deliberately cannot write arbitrary content or reset a nonempty registry.
pub fn reset_state(host: &Host) -> Result<ndimon_privd_proto::ActionOutput, Error> {
    let bytes = host
        .root
        .read(
            "/var/lib/ndimon/state/state.json",
            ndimon_privd_proto::MAX_CONFIG_BYTES,
        )?
        .ok_or(Error::PermissionDenied)?;
    let state: Value =
        serde_json::from_slice(&bytes).map_err(|e| Error::invalid("state", e.to_string()))?;
    if !state["document"]["plugins"]
        .as_object()
        .is_some_and(|p| p.is_empty())
    {
        return Err(Error::invalid(
            "plugins",
            "accepted registry must be empty before reset",
        ));
    }
    let etc = host.root.directory("/etc", true, true)?;
    let _lock = etc.lock(".ndimon-config.lock")?;
    host.root
        .atomic("/etc/ndimon-plugins.json", b"{}", host.ndimon_gid, 0o640)?;
    Ok(ndimon_privd_proto::ActionOutput::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_metadata_is_bounded_before_tar_yields_a_file() {
        for kind in [
            tar::EntryType::GNULongName,
            tar::EntryType::GNULongLink,
            tar::EntryType::XHeader,
        ] {
            let mut metadata = if kind == tar::EntryType::XHeader {
                let record = format!(" comment={}\n", "a".repeat(256 * 1024));
                let mut size = record.len();
                loop {
                    let next = record.len() + size.to_string().len();
                    if next == size {
                        break;
                    }
                    size = next;
                }
                format!("{size}{record}").into_bytes()
            } else {
                // A valid, large name that normalizes to a small filename.
                let mut name = b"./".repeat(128 * 1024);
                name.extend_from_slice(b"file\0");
                name
            };
            let encoder = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(metadata.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(kind);
            header.set_cksum();
            builder
                .append_data(&mut header, "metadata", metadata.as_slice())
                .unwrap();
            metadata.clear();
            header.set_size(2);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder
                .append_data(&mut header, "file", &b"ok"[..])
                .unwrap();
            let bytes = builder.into_inner().unwrap().finish().unwrap();
            let allowed = tempfile::tempdir().unwrap();
            unpack_limited(bytes.as_slice(), allowed.path(), 1024 * 1024).unwrap();
            assert_eq!(fs::read(allowed.path().join("file")).unwrap(), b"ok");
            let limited = tempfile::tempdir().unwrap();
            assert!(unpack_limited(bytes.as_slice(), limited.path(), 64 * 1024).is_err());
            assert_eq!(fs::read_dir(limited.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn host_services_are_never_plugin_scoped() {
        for unit in ["ssh.service", "ndimon-api.service", "ndimon-r.service"] {
            assert!(!permitted_service("hello", unit));
        }
        assert!(permitted_service("tessera", "tesserad.service"));
        assert!(!permitted_service("other", "tesserad.service"));
    }
    #[test]
    fn archive_rejects_symlinks_and_duplicates() {
        for duplicate in [false, true] {
            let t = tempfile::tempdir().unwrap();
            let encoder = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o755);
            header.set_entry_type(if duplicate {
                tar::EntryType::Regular
            } else {
                tar::EntryType::Symlink
            });
            if !duplicate {
                header.set_link_name("/etc/passwd").unwrap();
            }
            header.set_cksum();
            builder
                .append_data(&mut header, "bad", std::io::empty())
                .unwrap();
            if duplicate {
                builder
                    .append_data(&mut header, "bad", std::io::empty())
                    .unwrap();
            }
            let bytes = builder.into_inner().unwrap().finish().unwrap();
            assert!(unpack(std::io::Cursor::new(bytes), t.path()).is_err());
        }
    }
}
