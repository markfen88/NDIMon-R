//! Ordered install/upgrade boundary. Migrations must succeed before retirement.
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Command,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
/// Units stopped before an upgrade replaces the files. The watchdog, updater,
/// Node API and its privd socket are retired; they are listed so an older
/// install or an intermediate build is quiesced and its manual copy preserved.
/// A socket is stopped before its service: systemd notes every service stopped
/// while its triggering socket is still active.
const SERVICES: &[&str] = &[
    "ndimon-watchdog.service",
    "ndimon-update.service",
    "ndimon-api.service",
    "ndimon-r.service",
    "ndimon-finder.service",
    "ndimon-control.socket",
    "ndimon-control.service",
    "ndimon-privd.socket",
    "ndimon-privd.service",
    "ndimon-privd-api.socket",
    "ndimon-net-boot.service",
    "ndimon-state-import.service",
];
/// Units that are disabled while their files may still exist.
const RETIRED_UNITS: &[&str] = &[
    "ndimon-watchdog.service",
    "ndimon-update.service",
    "ndimon-api.service",
    "ndimon-privd-api.socket",
];
const RETIRED: &[&str] = &[
    "/usr/lib/systemd/system/ndimon-update.service",
    "/lib/systemd/system/ndimon-update.service",
    "/etc/systemd/system/multi-user.target.wants/ndimon-update.service",
    "/usr/lib/systemd/system/ndimon-watchdog.service",
    "/lib/systemd/system/ndimon-watchdog.service",
    "/etc/systemd/system/multi-user.target.wants/ndimon-watchdog.service",
    "/etc/sudoers.d/ndimon",
    "/etc/ndimon-network-engine",
    "/usr/local/sbin/ndimon-priv",
    "/usr/local/lib/ndimon/ndimon-write-config",
    "/usr/local/lib/ndimon/ndimon-cpu-sampling",
    "/usr/local/lib/ndimon/ndimon-net",
    "/usr/local/lib/ndimon/ndimon_network_profiles.py",
    "/usr/local/bin/ndimon-control",
    "/usr/local/sbin/ndimon-state-import",
    "/usr/local/sbin/ndimon-privd",
    "/usr/local/bin/ndimon-r",
    "/usr/local/bin/ndimon-finder",
    // Before per-caller sockets there was one, shared by every member of the
    // old group. Nothing may find it again once the old units are gone.
    "/run/ndimon/privd.sock",
    "/opt/ndimon-r/scripts/ndimon-watchdog.sh",
    "/opt/ndimon-r/scripts/ndimon-update-run.sh",
    "/etc/systemd/system/ndimon-control.service.d/40-privd.conf",
    "/etc/systemd/system/ndimon-privd.service.d/45-audio-rollback.conf",
    "/etc/systemd/system/ndimon-r.service.d/45-control.conf",
    "/etc/systemd/system/ndimon-finder.service.d/45-control.conf",
    // The Node API: its enablement, unit and private transports.
    "/etc/systemd/system/multi-user.target.wants/ndimon-api.service",
    "/etc/systemd/system/ndimon-api.service",
    "/run/ndimon-control/internal.sock",
    "/run/ndimon-control/supervisor",
    // The API's own privd socket from intermediate builds: enablement, unit
    // files and the socket itself. Only the control plane's socket remains.
    "/etc/systemd/system/sockets.target.wants/ndimon-privd-api.socket",
    "/etc/systemd/system/ndimon-privd-api.socket",
    "/usr/lib/systemd/system/ndimon-privd-api.socket",
    "/lib/systemd/system/ndimon-privd-api.socket",
    "/run/ndimon-privd/api.sock",
];
/// Trees that belonged only to the Node API: its unit drop-ins (including the
/// per-plugin sandbox grants) and its runtime sockets. Plugin code under
/// /opt/ndimon-r/plugins and plugin data under /var/lib/ndimon/plugins are
/// operator data and are never listed here.
const RETIRED_TREES: &[&str] = &[
    "/etc/systemd/system/ndimon-api.service.d",
    "/run/ndimon-api",
    // The Python helpers of the 1.3.0 installer wrote their bytecode here.
    "/usr/local/lib/ndimon/__pycache__",
];
/// Where the 1.3.0 installer copied the Node API (with node_modules).
const INSTALLED_API: &str = "/opt/ndimon-r/api";
/// Locations a Node binary can have received the 1.3.0 installer's
/// `setcap cap_net_bind_service` (port 80 for the API).
const NODE_BINARIES: &[&str] = &["/usr/bin/node", "/usr/bin/nodejs", "/usr/local/bin/node"];

trait Execute {
    fn run(&self, program: &str, args: &[&str]) -> Result;
}
struct System;
impl Execute for System {
    fn run(&self, program: &str, args: &[&str]) -> Result {
        let status = Command::new(program).args(args).status()?;
        if !status.success() {
            return Err(format!("{program} {}: {status}", args.join(" ")).into());
        }
        Ok(())
    }
}
fn path(root: &Path, value: &str) -> PathBuf {
    root.join(value.trim_start_matches('/'))
}
fn remove(file: &Path) -> Result {
    match fs::remove_file(file) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
/// Remove a directory tree without following a symlink at its root.
fn remove_tree(tree: &Path) -> Result {
    match fs::symlink_metadata(tree) {
        Ok(metadata) if metadata.is_dir() => Ok(fs::remove_dir_all(tree)?),
        Ok(_) => remove(tree),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
/// Whether a tree holds anything worth a recovery copy: a file with content or
/// a link. Directories the package has just created, and the empty lock files
/// in them, are not data.
fn has_data(tree: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(tree) else {
        return false;
    };
    if metadata.is_dir() {
        return fs::read_dir(tree)
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| has_data(&entry.path()));
    }
    !metadata.is_file() || metadata.len() > 0
}
/// Preserve only once, including original permissions, without replacing an
/// earlier recovery image. Directory trees are copied by cp with no shell.
/// There is nothing to preserve in what was not there before this install.
fn preserve(root: &Path, value: &str, exec: &dyn Execute) -> Result {
    let source = path(root, value);
    let backup = path(root, &format!("{value}.pre-p8"));
    if has_data(&source) && fs::symlink_metadata(&backup).is_err() {
        exec.run(
            "/usr/bin/cp",
            &[
                "-a",
                "--",
                source.to_str().ok_or("source path")?,
                backup.to_str().ok_or("backup path")?,
            ],
        )?;
    }
    Ok(())
}
/// Where a unit file of a retired unit can be.
const UNIT_DIRECTORIES: &[&str] = &[
    "/etc/systemd/system",
    "/usr/lib/systemd/system",
    "/lib/systemd/system",
];
fn disable_retired(root: &Path, exec: &dyn Execute) {
    // Only units that exist: systemctl complains, in red, about every other one,
    // on every fresh install.
    let present: Vec<_> = RETIRED_UNITS
        .iter()
        .filter(|unit| {
            UNIT_DIRECTORIES.iter().any(|directory| {
                fs::symlink_metadata(path(root, &format!("{directory}/{unit}"))).is_ok()
            })
        })
        .copied()
        .collect();
    if present.is_empty() {
        return;
    }
    let mut args = vec!["disable", "--quiet"];
    args.extend(present);
    // Best effort: a unit that is already half gone is retired by file removal.
    let _ = exec.run("/usr/bin/systemctl", &args);
}
/// Units the old manual installs placed in /etc, shadowing the packaged ones:
/// disabled while their files still exist, kept for a rollback, then removed.
fn retire_manual_units(root: &Path, exec: &dyn Execute) -> Result {
    disable_retired(root, exec);
    for unit in SERVICES {
        let value = format!("/etc/systemd/system/{unit}");
        preserve(root, &value, exec)?;
        remove(&path(root, &value))?;
    }
    Ok(())
}
/// The directories the migration writes after the old stack is stopped: the state
/// and the settings. A write that fails there (a full disk, a read-only mount, a
/// file where a directory belongs) used to stop the postinst with nothing
/// running (NI-17). The preinst writes and syncs a probe in each first, before it
/// stops anything; a failing preinst aborts the unpack and the old version keeps
/// running. A directory that does not exist yet (a first install) is judged by
/// its nearest existing parent.
const MIGRATION_DIRECTORIES: &[&str] = &["/var/lib/ndimon", "/etc"];
fn check_writable(root: &Path) -> Result {
    for directory in MIGRATION_DIRECTORIES {
        let wanted = path(root, directory);
        if wanted.exists() && !wanted.is_dir() {
            return Err(format!(
                "{} is not a directory. Nothing was stopped; move it away and install again.",
                wanted.display()
            )
            .into());
        }
        let mut target = wanted.as_path();
        while !target.is_dir() {
            target = target.parent().ok_or("no directory to write to")?;
        }
        let probe = target.join(".ndimon-package-write-check");
        let written = (|| -> std::io::Result<()> {
            let mut file = fs::File::create(&probe)?;
            file.write_all(&[0; 256 * 1024])?;
            file.sync_all()
        })();
        let _ = fs::remove_file(&probe);
        written.map_err(|error| {
            format!(
                "cannot write to {}: {error}. Nothing was stopped; free space or make the directory writable, then install again.",
                target.display()
            )
        })?;
    }
    Ok(())
}
fn quiesce() -> Result {
    // Earlier packages of the same run have already replaced unit files; every
    // systemctl call below would otherwise warn once per changed unit that it
    // changed on disk.
    let _ = System.run("/usr/bin/systemctl", &["daemon-reload"]);
    for unit in SERVICES {
        let output = Command::new("/usr/bin/systemctl")
            .args(["show", "--property=LoadState", "--value", unit])
            .output()?;
        let state = String::from_utf8(output.stdout)?;
        if !state.trim().is_empty() && state.trim() != "not-found" {
            System.run("/usr/bin/systemctl", &["stop", unit])?;
        }
    }
    Ok(())
}
/// Undo what the Node API alone needed. This is best effort: a leftover file
/// must never keep the native stack from starting, so failures are reported.
fn retire_node(root: &Path, exec: &dyn Execute) {
    let attempt = |what: &str, result: Result| {
        if let Err(error) = result {
            eprintln!("ndimon-package: could not retire {what}: {error}");
        }
    };
    for value in RETIRED_TREES {
        attempt(value, remove_tree(&path(root, value)));
    }
    // A source checkout recorded at /opt/ndimon-r would own that api directory.
    let source_is_install_dir = fs::read_to_string(path(root, "/etc/ndimon-source-dir"))
        .is_ok_and(|dir| Path::new(dir.trim()) == Path::new("/opt/ndimon-r"));
    if !source_is_install_dir {
        attempt(INSTALLED_API, remove_tree(&path(root, INSTALLED_API)));
    }
    // Only an empty download directory goes: it may hold an archive in use.
    let _ = fs::remove_dir(path(root, "/var/lib/ndimon/plugins/downloads"));
    // The helper directory of the old installer, once nothing is left in it.
    let _ = fs::remove_dir(path(root, "/usr/local/lib/ndimon"));
    // Its scripts directory, empty once the watchdog and updater scripts went.
    let _ = fs::remove_dir(path(root, "/opt/ndimon-r/scripts"));
    let mut binaries = std::collections::BTreeSet::new();
    for value in NODE_BINARIES {
        if let Ok(binary) = fs::canonicalize(path(root, value)) {
            binaries.insert(binary);
        }
    }
    for binary in binaries {
        if let Some(binary) = binary.to_str() {
            // The capability is absent from most Node installs; that is fine.
            let _ = exec.run("/usr/sbin/setcap", &["-r", binary]);
        }
    }
}
fn refresh_rga(root: &Path, exec: &dyn Execute) -> Result {
    let devices = path(root, "/sys/class/video4linux");
    if !devices.exists() {
        return Ok(());
    }
    let mut matches = Vec::new();
    for entry in fs::read_dir(devices)? {
        let entry = entry?;
        if fs::read_to_string(entry.path().join("name"))
            .is_ok_and(|name| name.trim() == "rockchip-rga")
        {
            matches.push(entry.file_name().into_string().map_err(|_| "device name")?);
        }
    }
    if !matches.is_empty() {
        exec.run("/usr/bin/udevadm", &["control", "--reload-rules"])?;
        matches.sort();
        for name in matches {
            exec.run(
                "/usr/bin/udevadm",
                &[
                    "trigger",
                    "--action=add",
                    "--subsystem-match=video4linux",
                    &format!("--sysname-match={name}"),
                ],
            )?;
        }
    }
    Ok(())
}
/// sysusers adds memberships but never removes obsolete ones. Quiesce first,
/// then retire the former account-wide `ndimon-priv` grant and, once nobody
/// else is in it, the group itself, and the Node API's `ndimon-priv-api` group
/// from intermediate builds on the same terms. The control plane holds
/// `ndimon-priv-control` (created by sysusers) through its unit only.
fn migrate_service_groups(root: &Path, exec: &dyn Execute) -> Result {
    // Earlier manual installs shipped a second vendor declaration. Leaving it
    // behind would regrant account-wide privileges at the next sysusers boot.
    for value in [
        "/usr/lib/sysusers.d/ndimon-privd.conf",
        "/usr/lib/tmpfiles.d/ndimon-privd.conf",
    ] {
        preserve(root, value, exec)?;
        remove(&path(root, value))?;
    }
    let groups = fs::read_to_string(path(root, "/etc/group"))?;
    for line in groups.lines() {
        let fields: Vec<_> = line.split(':').collect();
        if fields.len() == 4 && fields[0] == "ndimon-priv" {
            let service_accounts = ["ndimon", "ndimon-control"];
            for user in service_accounts {
                if fields[3].split(',').any(|member| member == user) {
                    exec.run("/usr/bin/gpasswd", &["--delete", user, "ndimon-priv"])?;
                }
            }
            // An operator-added member keeps the group; it grants nothing now
            // because no socket uses it.
            let others = fields[3]
                .split(',')
                .any(|member| !member.is_empty() && !service_accounts.contains(&member));
            if !others {
                // Cosmetic: a failure here leaves an empty, unused group.
                let _ = exec.run("/usr/sbin/groupdel", &["ndimon-priv"]);
            }
        }
        // The API's socket group was never a persistent member's; any member
        // was added by an operator, and then the group is left alone.
        if fields.len() == 4 && fields[0] == "ndimon-priv-api" && fields[3].is_empty() {
            let _ = exec.run("/usr/sbin/groupdel", &["ndimon-priv-api"]);
        }
    }
    Ok(())
}

/// Copy the packaged default settings next to the ones the device has.
fn install_defaults(root: &Path) -> Result {
    let defaults = path(root, "/usr/share/ndimon/defaults");
    for entry in fs::read_dir(defaults)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "ndimon-sources.json" {
            continue;
        }
        let dest = path(root, "/etc").join(name);
        if fs::symlink_metadata(&dest).is_err() {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o640)
                .open(&dest)?;
            file.write_all(&fs::read(entry.path())?)?;
            file.sync_all()?;
        }
    }
    Ok(())
}

/// The rule for every step below: the old stack is stopped before the first of
/// them runs, so a step that cannot migrate something must not leave the
/// appliance down. Either the step is essential to starting the new stack (the
/// accounts, the directories, the state, the network boot job) and its content
/// refusals are contained where they are made (the importer leaves a refused
/// domain at its defaults, the network migration reports a store it will not
/// take over, and both say so), or it is not, and a failure is reported at the
/// end while the rest is configured and started (HW-1 H1, VV-2 N1).
fn migrate(root: &Path, exec: &dyn Execute) -> Result<Vec<String>> {
    let mut left = Vec::new();
    let mut step = |what: &str, result: Result| {
        if let Err(error) = result {
            eprintln!("ndimon-package: NOT MIGRATED, {what}: {error}");
            left.push(what.to_owned());
        }
    };
    exec.run(
        "/usr/bin/systemd-sysusers",
        &["/usr/lib/sysusers.d/ndimon.conf"],
    )?;
    step(
        "old service group memberships",
        migrate_service_groups(root, exec),
    );
    exec.run("/usr/bin/systemd-tmpfiles", &["--create", "ndimon.conf"])?;
    step("packaged default settings", install_defaults(root));
    // N4 removes old journals only after its atomic import. Keep the originals
    // independently for a release rollback, including root-private tokens.
    for value in [
        "/var/lib/ndimon-network",
        "/var/lib/ndimon/net/pending",
        "/var/lib/ndimon/net/vlans",
    ] {
        step(
            &format!("recovery copy of {value}"),
            preserve(root, value, exec),
        );
    }
    exec.run("/usr/bin/ndimon-state-import", &[])?;
    exec.run("/usr/libexec/ndimon/ndimon-privd", &["network-job", "boot"])?;
    step("video device rules", refresh_rga(root, exec));
    for value in RETIRED {
        step(&format!("removal of {value}"), remove(&path(root, value)));
    }
    retire_node(root, exec);
    disable_retired(root, exec);
    // The enablement links of the 1.3.0 units point at files that moved to
    // *.pre-p8; enabling the packaged units below makes the right ones.
    for package in ["ndimon-control", "ndimon-privd", "ndimon-runtime"] {
        step(
            &format!("stale unit links of {package}"),
            remove_enablement(root, package),
        );
    }
    // State import sets private ownership. The renderer owns /etc projections;
    // the auth store retains its existing password and migrates on login.
    exec.run("/usr/bin/systemctl", &["daemon-reload"])?;
    exec.run(
        "/usr/bin/systemctl",
        &[
            "enable",
            "ndimon-privd.socket",
            "ndimon-control.socket",
            "ndimon-control.service",
            "ndimon-net-boot.service",
            "ndimon-r.service",
            "ndimon-finder.service",
        ],
    )?;
    exec.run(
        "/usr/bin/systemctl",
        &[
            "start",
            "ndimon-privd.socket",
            "ndimon-net-boot.service",
            "ndimon-control.socket",
            "ndimon-control.service",
        ],
    )?;
    // Finder can run without a display; decoder readiness still requires DRM.
    exec.run(
        "/usr/bin/systemctl",
        &[
            "start",
            "--no-block",
            "ndimon-finder.service",
            "ndimon-r.service",
        ],
    )?;
    fs::write(
        path(root, "/etc/ndimon-firmware-version"),
        fs::read(path(root, "/usr/share/ndimon/version"))?,
    )?;
    Ok(left)
}

/// The units each package ships and `migrate` enables.
fn shipped_units(package: &str) -> &'static [&'static str] {
    match package {
        "ndimon-control" => &[
            "ndimon-control.service",
            "ndimon-control.socket",
            "ndimon-state-import.service",
        ],
        "ndimon-privd" => &[
            "ndimon-privd.socket",
            "ndimon-privd.service",
            "ndimon-net-boot.service",
        ],
        "ndimon-runtime" => &["ndimon-r.service", "ndimon-finder.service"],
        _ => &[],
    }
}
/// What `deb-systemd-helper` does when a package is removed: the enablement
/// links of its units go with the unit files. Only dangling links of this
/// package's own units are removed, and only links.
fn remove_enablement(root: &Path, package: &str) -> Result {
    let system = path(root, "/etc/systemd/system");
    let Ok(directories) = fs::read_dir(&system) else {
        return Ok(());
    };
    for directory in directories.flatten() {
        let name = directory.file_name();
        let name = name.to_string_lossy();
        if !(name.ends_with(".wants") || name.ends_with(".requires") || name.ends_with(".upholds"))
        {
            continue;
        }
        for unit in shipped_units(package) {
            let link = directory.path().join(unit);
            let dangling = fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink())
                && fs::metadata(&link).is_err();
            if dangling {
                remove(&link)?;
            }
        }
    }
    Ok(())
}

pub fn hook(name: &str, package: &str, action: &str) -> Result {
    if !matches!(
        package,
        "ndimon-control" | "ndimon-privd" | "ndimon-runtime" | "ndimon-ui"
    ) {
        return Err("unrecognized Debian package".into());
    }
    let root = Path::new("/");
    match (name, action) {
        ("preinst", "install" | "upgrade") => {
            // Before anything is stopped: a failure here leaves the old stack up.
            check_writable(root)?;
            quiesce()?;
            // Disable while the manual unit still exists, before removing the
            // file that shadows packaged units. No new watchdog is installed.
            retire_manual_units(root, &System)?;
        }
        ("postinst", "configure") if package == "ndimon-runtime" => {
            let left = migrate(root, &System)?;
            if !left.is_empty() {
                eprintln!(
                    "ndimon-package: the appliance is configured and running; these were not migrated: {}",
                    left.join("; ")
                );
            }
        }
        ("prerm", "remove" | "deconfigure") => quiesce()?,
        ("postrm", "remove" | "purge") => {
            // The unit files are gone; so are the links that enabled them.
            remove_enablement(root, package)?;
            let _ = System.run("/usr/bin/systemctl", &["daemon-reload"]);
        }
        _ => (), // dpkg's abort/failed-upgrade hooks must preserve recovery data.
    }
    Ok(())
}

pub fn console(release: bool) -> Result {
    for name in [
        "/sys/class/vtconsole/vtcon0/bind",
        "/sys/class/vtconsole/vtcon1/bind",
    ] {
        if Path::new(name).exists() {
            fs::write(name, if release { b"1" } else { b"0" })?;
        }
    }
    if !release && Path::new("/sys/class/graphics/fb0/blank").exists() {
        fs::write("/sys/class/graphics/fb0/blank", b"1")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::os::unix::fs::PermissionsExt;
    struct Fake {
        calls: RefCell<Vec<String>>,
        fail: &'static str,
    }
    impl Execute for Fake {
        fn run(&self, program: &str, args: &[&str]) -> Result {
            self.calls
                .borrow_mut()
                .push(format!("{program} {}", args.join(" ")));
            if program == self.fail {
                return Err("injected migration failure".into());
            }
            Ok(())
        }
    }
    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("etc")).unwrap();
        fs::write(
            root.path().join("etc/group"),
            "ndimon:x:999:\nndimon-priv:x:998:\n",
        )
        .unwrap();
        fs::create_dir_all(root.path().join("usr/share/ndimon/defaults")).unwrap();
        fs::write(
            root.path()
                .join("usr/share/ndimon/defaults/ndimon-device-settings.json"),
            b"{}\n",
        )
        .unwrap();
        fs::write(root.path().join("usr/share/ndimon/version"), "1.4.0-1\n").unwrap();
        root
    }
    #[test] // NI-17: the unwritable state directory is found before the old stack is stopped.
    fn the_state_directory_is_probed_before_anything_is_stopped() {
        let root = fixture();
        check_writable(root.path()).unwrap();
        assert!(
            !root.path().join(".ndimon-package-write-check").exists(),
            "the probe is removed"
        );
        // A file where the state directory belongs.
        fs::create_dir_all(root.path().join("var/lib")).unwrap();
        fs::write(root.path().join("var/lib/ndimon"), b"not a directory").unwrap();
        let error = check_writable(root.path()).unwrap_err().to_string();
        assert!(
            error.contains("is not a directory") && error.contains("Nothing was stopped"),
            "{error}"
        );
        fs::remove_file(root.path().join("var/lib/ndimon")).unwrap();
        // A directory nobody can write to (skipped where permissions do not bind, as for root).
        let state = root.path().join("var/lib/ndimon");
        fs::create_dir_all(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o500)).unwrap();
        if fs::File::create(state.join("x")).is_err() {
            let error = check_writable(root.path()).unwrap_err().to_string();
            assert!(
                error.contains("cannot write to") && error.contains("Nothing was stopped"),
                "{error}"
            );
        }
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    }
    #[test]
    fn migrations_precede_retirement_and_service_activation() {
        for fail in [
            "/usr/bin/ndimon-state-import",
            "/usr/libexec/ndimon/ndimon-privd",
            "",
        ] {
            let root = fixture();
            let helper = root.path().join("usr/local/sbin/ndimon-priv");
            fs::create_dir_all(helper.parent().unwrap()).unwrap();
            fs::write(&helper, b"old helper").unwrap();
            let exec = Fake {
                calls: RefCell::new(Vec::new()),
                fail,
            };
            assert_eq!(migrate(root.path(), &exec).is_ok(), fail.is_empty());
            assert_eq!(helper.exists(), !fail.is_empty());
            let calls = exec.calls.borrow();
            let activation = calls.iter().any(|c| c.contains("systemctl enable"));
            assert_eq!(activation, fail.is_empty());
        }
    }
    #[test]
    fn a_step_that_cannot_migrate_something_does_not_keep_the_stack_down() {
        // The shapes of a 1.3.0 device: a group with members, journals with
        // content, an RGA video device. Each of these steps fails in turn.
        for (program, what) in [
            ("/usr/bin/gpasswd", "old service group memberships"),
            ("/usr/bin/cp", "recovery copy of /var/lib/ndimon-network"),
            ("/usr/bin/udevadm", "video device rules"),
        ] {
            let root = fixture();
            fs::write(
                root.path().join("etc/group"),
                "ndimon-priv:x:998:ndimon,ndimon-control\nndimon:x:999:ndimon-control\n",
            )
            .unwrap();
            touch(&root, "var/lib/ndimon-network/committed.json");
            touch(&root, "sys/class/video4linux/video0/name");
            fs::write(
                root.path().join("sys/class/video4linux/video0/name"),
                "rockchip-rga\n",
            )
            .unwrap();
            let exec = Fake {
                calls: RefCell::new(Vec::new()),
                fail: program,
            };
            let left = migrate(root.path(), &exec).unwrap();
            assert!(left.iter().any(|l| l == what), "{program}: {left:?}");
            // Everything else was configured, and the new stack was started.
            let calls = exec.calls.borrow();
            for expected in [
                "ndimon-state-import",
                "network-job boot",
                "systemctl enable",
                "systemctl start ndimon-privd.socket",
                "systemctl start --no-block ndimon-finder.service",
            ] {
                assert!(
                    calls.iter().any(|c| c.contains(expected)),
                    "{program}: {expected}"
                );
            }
            assert!(root.path().join("etc/ndimon-firmware-version").exists());
        }
    }
    #[test]
    fn upgrade_removes_only_obsolete_service_memberships() {
        let root = fixture();
        fs::write(
            root.path().join("etc/group"),
            "ndimon-priv:x:998:ndimon,operator,ndimon-control\nndimon:x:999:ndimon-control\n",
        )
        .unwrap();
        let exec = Fake {
            calls: RefCell::new(vec![]),
            fail: "",
        };
        migrate(root.path(), &exec).unwrap();
        let calls = exec.calls.borrow();
        let removals: Vec<_> = calls
            .iter()
            .filter(|call| call.contains("gpasswd"))
            .map(String::as_str)
            .collect();
        assert_eq!(
            removals,
            [
                "/usr/bin/gpasswd --delete ndimon ndimon-priv",
                "/usr/bin/gpasswd --delete ndimon-control ndimon-priv"
            ]
        );
        let removed = calls
            .iter()
            .rposition(|call| call.contains("gpasswd"))
            .unwrap();
        let activation = calls
            .iter()
            .position(|call| call.contains("systemctl enable"))
            .unwrap();
        assert!(removed < activation);
        // An operator still belongs to it: the group is left alone.
        assert!(calls.iter().all(|call| !call.contains("groupdel")));
    }

    #[test]
    fn upgrade_retires_the_old_group_when_only_service_accounts_were_in_it() {
        let root = fixture();
        fs::write(
            root.path().join("etc/group"),
            "ndimon-priv:x:998:ndimon,ndimon-control\nndimon:x:999:ndimon-control\n",
        )
        .unwrap();
        let exec = Fake {
            calls: RefCell::new(vec![]),
            fail: "",
        };
        migrate(root.path(), &exec).unwrap();
        let calls = exec.calls.borrow();
        let position = |needle: &str| calls.iter().position(|call| call.contains(needle));
        let last_removal = calls.iter().rposition(|c| c.contains("gpasswd")).unwrap();
        let delete = position("/usr/sbin/groupdel ndimon-priv").unwrap();
        assert!(last_removal < delete);
        assert!(delete < position("systemctl enable").unwrap());
        // An already-absent group (a fresh or already migrated host) is no-op.
        let fresh = fixture();
        fs::write(fresh.path().join("etc/group"), "ndimon:x:999:\n").unwrap();
        let exec = Fake {
            calls: RefCell::new(vec![]),
            fail: "",
        };
        migrate(fresh.path(), &exec).unwrap();
        assert!(exec.calls.borrow().iter().all(|c| !c.contains("groupdel")));
    }

    #[test]
    fn the_control_socket_is_made_enabled_and_started_before_its_caller() {
        let root = fixture();
        let stale = path(root.path(), "/run/ndimon/privd.sock");
        fs::create_dir_all(stale.parent().unwrap()).unwrap();
        fs::write(&stale, b"old shared socket").unwrap();
        let exec = Fake {
            calls: RefCell::new(vec![]),
            fail: "",
        };
        migrate(root.path(), &exec).unwrap();
        assert!(!stale.exists(), "the shared socket path must be retired");
        let calls = exec.calls.borrow();
        let position = |needle: &str| calls.iter().position(|call| call.contains(needle)).unwrap();
        // Accounts and groups exist before tmpfiles and units refer to them.
        assert!(position("systemd-sysusers") < position("systemd-tmpfiles"));
        assert!(position("systemd-tmpfiles") < position("systemctl daemon-reload"));
        let enable = &calls[position("systemctl enable")];
        let start = &calls[position("systemctl start ndimon-privd.socket")];
        for list in [enable, start] {
            assert!(list.contains("ndimon-privd.socket"), "{list}");
            assert!(!list.contains("ndimon-privd-api.socket"), "{list}");
        }
        // The socket listens before control (its caller) starts.
        assert!(
            start.find("ndimon-privd.socket").unwrap()
                < start.find("ndimon-control.service").unwrap()
        );
    }

    #[test]
    fn upgrade_retires_the_api_privd_socket_unit_and_group_of_an_intermediate_build() {
        let root = fixture();
        for file in [
            "etc/systemd/system/sockets.target.wants/ndimon-privd-api.socket",
            "usr/lib/systemd/system/ndimon-privd-api.socket",
            "run/ndimon-privd/api.sock",
        ] {
            touch(&root, file);
        }
        touch(&root, "run/ndimon-privd/control.sock");
        fs::write(
            root.path().join("etc/group"),
            "ndimon:x:999:\nndimon-priv-api:x:997:\nndimon-priv-control:x:996:\n",
        )
        .unwrap();
        let exec = Fake {
            calls: RefCell::new(vec![]),
            fail: "",
        };
        migrate(root.path(), &exec).unwrap();
        for file in [
            "etc/systemd/system/sockets.target.wants/ndimon-privd-api.socket",
            "usr/lib/systemd/system/ndimon-privd-api.socket",
            "run/ndimon-privd/api.sock",
        ] {
            assert!(!root.path().join(file).exists(), "{file}");
        }
        // The control plane's own socket is not touched.
        assert!(root.path().join("run/ndimon-privd/control.sock").exists());
        let calls = exec.calls.borrow();
        let position = |needle: &str| calls.iter().position(|call| call.contains(needle));
        assert!(position("/usr/sbin/groupdel ndimon-priv-api").is_some());
        assert!(position("groupdel ndimon-priv-control").is_none());
        // Its files are removed before the disable would run: nothing to disable.
        assert!(position("systemctl disable").is_none());
        // An operator in the group keeps it.
        let kept = fixture();
        fs::write(
            kept.path().join("etc/group"),
            "ndimon:x:999:\nndimon-priv-api:x:997:operator\n",
        )
        .unwrap();
        let exec = Fake {
            calls: RefCell::new(vec![]),
            fail: "",
        };
        migrate(kept.path(), &exec).unwrap();
        assert!(exec.calls.borrow().iter().all(|c| !c.contains("groupdel")));
    }

    #[test]
    fn upgrade_retires_manual_privilege_declarations() {
        let root = fixture();
        for value in [
            "/usr/lib/sysusers.d/ndimon-privd.conf",
            "/usr/lib/tmpfiles.d/ndimon-privd.conf",
        ] {
            let file = path(root.path(), value);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, b"obsolete declaration").unwrap();
        }
        let exec = Fake {
            calls: RefCell::new(vec![]),
            fail: "",
        };
        migrate(root.path(), &exec).unwrap();
        assert!(!path(root.path(), "/usr/lib/sysusers.d/ndimon-privd.conf").exists());
        assert!(!path(root.path(), "/usr/lib/tmpfiles.d/ndimon-privd.conf").exists());
        let calls = exec.calls.borrow();
        assert!(
            calls
                .iter()
                .any(|call| call.contains("/usr/lib/sysusers.d/ndimon-privd.conf.pre-p8"))
        );
        assert!(
            calls
                .iter()
                .any(|call| call.contains("/usr/lib/tmpfiles.d/ndimon-privd.conf.pre-p8"))
        );
    }

    #[test]
    fn reinstall_preserves_existing_configuration() {
        let root = fixture();
        let dest = root.path().join("etc/ndimon-device-settings.json");
        fs::write(&dest, b"{\"alias\":\"existing\"}\n").unwrap();
        migrate(
            root.path(),
            &Fake {
                calls: RefCell::new(Vec::new()),
                fail: "",
            },
        )
        .unwrap();
        assert_eq!(fs::read(dest).unwrap(), b"{\"alias\":\"existing\"}\n");
    }

    /// The list of certified network backends is a dpkg conffile of ndimon-privd:
    /// dpkg creates it on a fresh install and on the upgrade of a 1.3.x install,
    /// keeps an operator's edit and does not restore a deleted file. The hooks run
    /// around that and must neither create, replace nor retire it.
    #[test]
    fn install_and_upgrade_hooks_leave_the_certification_conffile_alone() {
        const FILE: &str = "etc/ndimon-network-certified-backends.json";
        let packaged =
            include_str!("../../../../packaging/debian/etc/ndimon-network-certified-backends.json");
        assert!(!RETIRED.contains(&"/etc/ndimon-network-certified-backends.json"));
        for before in [Some(packaged), Some("[\"netplan\"]\n"), Some("[]\n"), None] {
            let root = fixture();
            if let Some(text) = before {
                fs::write(root.path().join(FILE), text).unwrap();
            }
            let exec = Fake {
                calls: RefCell::new(Vec::new()),
                fail: "",
            };
            retire_manual_units(root.path(), &exec).unwrap();
            migrate(root.path(), &exec).unwrap();
            assert_eq!(
                fs::read_to_string(root.path().join(FILE)).ok().as_deref(),
                before
            );
        }
    }

    #[test]
    fn upgrade_retires_watchdog_unit_script_and_enablement() {
        let root = fixture();
        for file in RETIRED.iter().filter(|file| file.contains("watchdog")) {
            let file = path(root.path(), file);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, b"legacy watchdog").unwrap();
        }
        // The manual unit of an old install, as the preinst hook finds it.
        touch(&root, "etc/systemd/system/ndimon-watchdog.service");
        let exec = Fake {
            calls: RefCell::new(Vec::new()),
            fail: "",
        };
        retire_manual_units(root.path(), &exec).unwrap();
        migrate(root.path(), &exec).unwrap();
        for file in RETIRED.iter().filter(|file| file.contains("watchdog")) {
            assert!(!path(root.path(), file).exists(), "{file}");
        }
        assert!(
            !root
                .path()
                .join("etc/systemd/system/ndimon-watchdog.service")
                .exists()
        );
        let calls = exec.calls.borrow();
        let disable = calls
            .iter()
            .position(|call| call == "/usr/bin/systemctl disable --quiet ndimon-watchdog.service")
            .unwrap();
        let enable = calls
            .iter()
            .position(|call| call.contains("systemctl enable"))
            .unwrap();
        assert!(disable < enable);
        assert!(calls.iter().filter(|call| call.contains("systemctl start") || call.contains("systemctl enable")).all(|call| !call.contains("watchdog")));
    }

    #[test]
    fn a_fresh_install_disables_nothing_and_keeps_no_copy_of_what_it_just_created() {
        let root = fixture();
        // What tmpfiles creates before migrate preserves anything.
        for directory in [
            "var/lib/ndimon-network",
            "var/lib/ndimon/net/pending",
            "var/lib/ndimon/net/vlans",
        ] {
            fs::create_dir_all(root.path().join(directory)).unwrap();
        }
        // The network engine's lock file is created empty on its first run.
        fs::write(root.path().join("var/lib/ndimon-network/lock"), b"").unwrap();
        let exec = Fake {
            calls: RefCell::new(Vec::new()),
            fail: "",
        };
        retire_manual_units(root.path(), &exec).unwrap();
        migrate(root.path(), &exec).unwrap();
        let calls = exec.calls.borrow();
        assert!(
            calls.iter().all(|call| !call.contains("systemctl disable")),
            "{calls:?}"
        );
        assert!(
            calls.iter().all(|call| !call.contains("/usr/bin/cp")),
            "{calls:?}"
        );
    }

    #[test]
    fn existing_network_data_is_copied_once_for_a_rollback() {
        let root = fixture();
        touch(&root, "var/lib/ndimon-network/committed.json");
        touch(&root, "var/lib/ndimon/net/pending/state.json");
        fs::create_dir_all(root.path().join("var/lib/ndimon/net/vlans")).unwrap();
        let exec = Fake {
            calls: RefCell::new(Vec::new()),
            fail: "",
        };
        migrate(root.path(), &exec).unwrap();
        let copies: Vec<_> = exec
            .calls
            .borrow()
            .iter()
            .filter(|call| call.contains("/usr/bin/cp"))
            .cloned()
            .collect();
        assert_eq!(copies.len(), 2, "{copies:?}");
        assert!(copies[0].ends_with("var/lib/ndimon-network.pre-p8"));
        assert!(copies[1].ends_with("var/lib/ndimon/net/pending.pre-p8"));
        // An earlier recovery image is never replaced.
        fs::create_dir_all(root.path().join("var/lib/ndimon-network.pre-p8")).unwrap();
        fs::create_dir_all(root.path().join("var/lib/ndimon/net/pending.pre-p8")).unwrap();
        let again = Fake {
            calls: RefCell::new(Vec::new()),
            fail: "",
        };
        migrate(root.path(), &again).unwrap();
        assert!(
            again
                .calls
                .borrow()
                .iter()
                .all(|call| !call.contains("/usr/bin/cp"))
        );
    }

    #[test]
    fn the_old_helper_directory_goes_with_its_bytecode_when_nothing_else_is_in_it() {
        for other in [false, true] {
            let root = fixture();
            touch(
                &root,
                "usr/local/lib/ndimon/__pycache__/ndimon_network_profiles.cpython-311.pyc",
            );
            touch(&root, "usr/local/lib/ndimon/ndimon_network_profiles.py");
            if other {
                touch(&root, "usr/local/lib/ndimon/operator-tool");
            }
            let exec = Fake {
                calls: RefCell::new(Vec::new()),
                fail: "",
            };
            migrate(root.path(), &exec).unwrap();
            assert!(
                !root
                    .path()
                    .join("usr/local/lib/ndimon/__pycache__")
                    .exists()
            );
            assert!(
                !root
                    .path()
                    .join("usr/local/lib/ndimon/ndimon_network_profiles.py")
                    .exists()
            );
            // Nothing the operator put there is touched.
            assert_eq!(root.path().join("usr/local/lib/ndimon").exists(), other);
        }
    }

    #[test]
    fn an_upgrade_removes_the_dangling_links_of_the_old_units_and_the_empty_scripts_directory() {
        let root = fixture();
        let system = root.path().join("etc/systemd/system");
        // 1.3.0 enabled its units from /etc/systemd/system; they moved to *.pre-p8.
        let wants = system.join("multi-user.target.wants");
        fs::create_dir_all(&wants).unwrap();
        for unit in [
            "ndimon-r.service",
            "ndimon-finder.service",
            "ndimon-net-boot.service",
        ] {
            std::os::unix::fs::symlink(system.join(unit), wants.join(unit)).unwrap();
        }
        // Not ours, not dangling: stays.
        std::os::unix::fs::symlink("/etc/hostname", wants.join("ndimon-other.service")).unwrap();
        touch(&root, "opt/ndimon-r/scripts/ndimon-watchdog.sh");
        touch(&root, "opt/ndimon-r/scripts/ndimon-update-run.sh");
        let exec = Fake {
            calls: RefCell::new(Vec::new()),
            fail: "",
        };
        migrate(root.path(), &exec).unwrap();
        let dangling: Vec<_> = fs::read_dir(&wants)
            .unwrap()
            .flatten()
            .filter(|e| fs::metadata(e.path()).is_err())
            .collect();
        assert!(dangling.is_empty(), "{dangling:?}");
        assert!(fs::symlink_metadata(wants.join("ndimon-other.service")).is_ok());
        assert!(!root.path().join("opt/ndimon-r/scripts").exists());
        // Something the operator put there keeps the directory.
        let root = fixture();
        touch(&root, "opt/ndimon-r/scripts/ndimon-watchdog.sh");
        touch(&root, "opt/ndimon-r/scripts/my-own-tool");
        migrate(root.path(), &exec).unwrap();
        assert!(
            root.path()
                .join("opt/ndimon-r/scripts/my-own-tool")
                .exists()
        );
    }

    #[test]
    fn removal_takes_the_dangling_enablement_links_of_its_own_units_only() {
        let root = fixture();
        let system = root.path().join("etc/systemd/system");
        let units = root.path().join("usr/lib/systemd/system");
        fs::create_dir_all(&units).unwrap();
        // Links into the test root, so that only its files decide what dangles.
        let link = |directory: &str, unit: &str| {
            fs::create_dir_all(system.join(directory)).unwrap();
            std::os::unix::fs::symlink(units.join(unit), system.join(directory).join(unit))
                .unwrap();
        };
        link("multi-user.target.wants", "ndimon-r.service");
        link("multi-user.target.wants", "ndimon-finder.service");
        link("multi-user.target.wants", "ndimon-control.service");
        link("sockets.target.wants", "ndimon-control.socket");
        link("sockets.target.wants", "ndimon-privd.socket");
        link("multi-user.target.wants", "ssh.service");
        // A link whose unit is still installed is not dangling.
        fs::write(units.join("ndimon-net-boot.service"), b"[Unit]\n").unwrap();
        link("multi-user.target.wants", "ndimon-net-boot.service");
        let present = |directory: &str, unit: &str| {
            fs::symlink_metadata(system.join(directory).join(unit)).is_ok()
        };
        remove_enablement(root.path(), "ndimon-runtime").unwrap();
        assert!(!present("multi-user.target.wants", "ndimon-r.service"));
        assert!(!present("multi-user.target.wants", "ndimon-finder.service"));
        assert!(present("multi-user.target.wants", "ndimon-control.service"));
        assert!(present("sockets.target.wants", "ndimon-privd.socket"));
        remove_enablement(root.path(), "ndimon-control").unwrap();
        assert!(!present(
            "multi-user.target.wants",
            "ndimon-control.service"
        ));
        assert!(!present("sockets.target.wants", "ndimon-control.socket"));
        remove_enablement(root.path(), "ndimon-privd").unwrap();
        assert!(!present("sockets.target.wants", "ndimon-privd.socket"));
        // Still installed, and foreign, links stay.
        assert!(present(
            "multi-user.target.wants",
            "ndimon-net-boot.service"
        ));
        assert!(present("multi-user.target.wants", "ssh.service"));
        remove_enablement(root.path(), "ndimon-ui").unwrap();
        remove_enablement(&root.path().join("missing"), "ndimon-privd").unwrap();
    }

    fn touch(root: &tempfile::TempDir, file: &str) {
        let file = root.path().join(file);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, b"fixture").unwrap();
    }
    const NODE_API_FILES: &[&str] = &[
        "etc/systemd/system/multi-user.target.wants/ndimon-api.service",
        "etc/systemd/system/ndimon-api.service",
        "etc/systemd/system/ndimon-api.service.d/plugin-hello.conf",
        "etc/systemd/system/ndimon-api.service.d/45-control.conf",
        "run/ndimon-api/legacy.sock",
        "run/ndimon-api/plugins.sock",
        "run/ndimon-control/internal.sock",
        "run/ndimon-control/supervisor",
        "opt/ndimon-r/api/server.js",
        "opt/ndimon-r/api/node_modules/express/index.js",
    ];
    const OPERATOR_DATA: &[&str] = &[
        "opt/ndimon-r/plugins/hello/plugin.json",
        "opt/ndimon-r/plugins/hello/routes.js",
        "var/lib/ndimon/plugins/hello/state.json",
        "var/lib/ndimon/plugins/downloads/pending.tar.gz",
        "etc/ndimon-auth.json",
    ];

    #[test]
    fn upgrade_retires_the_node_api_and_keeps_plugins_and_operator_data() {
        let root = fixture();
        for file in NODE_API_FILES.iter().chain(OPERATOR_DATA) {
            touch(&root, file);
        }
        touch(&root, "usr/bin/node");
        let exec = Fake {
            calls: RefCell::new(Vec::new()),
            fail: "",
        };
        retire_manual_units(root.path(), &exec).unwrap();
        migrate(root.path(), &exec).unwrap();
        for file in NODE_API_FILES {
            assert!(!root.path().join(file).exists(), "{file}");
        }
        for file in OPERATOR_DATA {
            assert!(root.path().join(file).exists(), "{file}");
        }
        let calls = exec.calls.borrow();
        let node = fs::canonicalize(root.path().join("usr/bin/node")).unwrap();
        assert!(calls.contains(&format!("/usr/sbin/setcap -r {}", node.display())));
        // Disabled once, while its unit file still existed, and only that unit.
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.contains("systemctl disable"))
                .collect::<Vec<_>>(),
            ["/usr/bin/systemctl disable --quiet ndimon-api.service"]
        );
        // The retired unit is neither enabled nor started again.
        for call in calls
            .iter()
            .filter(|call| call.contains("systemctl enable") || call.contains("systemctl start"))
        {
            assert!(!call.contains("ndimon-api"), "{call}");
        }
    }

    #[test]
    fn empty_download_directory_goes_but_one_holding_an_archive_stays() {
        for archive in [false, true] {
            let root = fixture();
            let downloads = root.path().join("var/lib/ndimon/plugins/downloads");
            fs::create_dir_all(&downloads).unwrap();
            if archive {
                fs::write(downloads.join("hello.tar.gz"), b"archive").unwrap();
            }
            let exec = Fake {
                calls: RefCell::new(Vec::new()),
                fail: "",
            };
            migrate(root.path(), &exec).unwrap();
            assert_eq!(downloads.exists(), archive);
        }
    }

    #[test]
    fn a_source_checkout_at_the_install_directory_keeps_its_api_directory() {
        let root = fixture();
        touch(&root, "opt/ndimon-r/api/public/index.html");
        touch(&root, "etc/ndimon-source-dir");
        fs::write(root.path().join("etc/ndimon-source-dir"), "/opt/ndimon-r").unwrap();
        let exec = Fake {
            calls: RefCell::new(Vec::new()),
            fail: "",
        };
        migrate(root.path(), &exec).unwrap();
        assert!(
            root.path()
                .join("opt/ndimon-r/api/public/index.html")
                .exists()
        );
    }

    #[test]
    fn node_artifacts_survive_a_failed_migration_for_recovery() {
        let root = fixture();
        for file in NODE_API_FILES {
            touch(&root, file);
        }
        let exec = Fake {
            calls: RefCell::new(Vec::new()),
            fail: "/usr/bin/ndimon-state-import",
        };
        assert!(migrate(root.path(), &exec).is_err());
        for file in NODE_API_FILES {
            assert!(root.path().join(file).exists(), "{file}");
        }
        assert!(
            exec.calls
                .borrow()
                .iter()
                .all(|call| !call.contains("gpasswd") && !call.contains("setcap"))
        );
    }

    #[test]
    fn upgrade_removes_unsigned_update_units() {
        let root = fixture();
        for file in RETIRED.iter().filter(|file| file.contains("ndimon-update")) {
            let file = path(root.path(), file);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, b"legacy updater").unwrap();
        }
        migrate(
            root.path(),
            &Fake {
                calls: RefCell::new(vec![]),
                fail: "",
            },
        )
        .unwrap();
        for file in RETIRED.iter().filter(|file| file.contains("ndimon-update")) {
            assert!(!path(root.path(), file).exists(), "{file}");
        }
    }

    #[test]
    fn rga_refresh_only_triggers_matching_video_devices() {
        let root = tempfile::tempdir().unwrap();
        let exec = Fake {
            calls: RefCell::new(Vec::new()),
            fail: "",
        };
        refresh_rga(root.path(), &exec).unwrap();
        assert!(exec.calls.borrow().is_empty());
        for (device, name) in [("video0", "camera"), ("video7", "rockchip-rga\n")] {
            let directory = root.path().join("sys/class/video4linux").join(device);
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("name"), name).unwrap();
        }
        refresh_rga(root.path(), &exec).unwrap();
        let calls = exec.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], "/usr/bin/udevadm control --reload-rules");
        assert!(calls[1].ends_with("--sysname-match=video7"));
        assert!(!calls[1].contains("video0"));
    }

    fn unique_tmpfiles_paths(configs: &[(String, String)]) -> std::result::Result<(), String> {
        let mut paths = std::collections::BTreeMap::new();
        for (name, text) in configs {
            for line in text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
            {
                let target = line
                    .split_whitespace()
                    .nth(1)
                    .ok_or_else(|| format!("invalid tmpfiles line in {name}"))?;
                if let Some(previous) = paths.insert(target, name) {
                    return Err(format!(
                        "duplicate tmpfiles path {target}: {previous}, {name}"
                    ));
                }
            }
        }
        Ok(())
    }
    #[test]
    fn packaged_tmpfiles_have_no_duplicate_paths() {
        let directory =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../packaging/debian/tmpfiles.d");
        let configs: Vec<_> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|file| file.extension().is_some_and(|ext| ext == "conf"))
            .map(|file| {
                (
                    file.display().to_string(),
                    fs::read_to_string(file).unwrap(),
                )
            })
            .collect();
        assert!(!configs.is_empty());
        unique_tmpfiles_paths(&configs).unwrap();
        let conflict = unique_tmpfiles_paths(&[
            (
                "ndimon.conf".into(),
                "d /run/ndimon 3770 root ndimon -".into(),
            ),
            (
                "ndimon-privd.conf".into(),
                "d /run/ndimon 0755 root root -".into(),
            ),
        ])
        .unwrap_err();
        assert!(conflict.contains("duplicate tmpfiles path /run/ndimon"));
        let all = configs
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let declaration: Vec<_> = all
            .lines()
            .find(|line| line.split_whitespace().nth(1) == Some("/run/ndimon"))
            .unwrap()
            .split_whitespace()
            .collect();
        assert_eq!(
            &declaration[..5],
            ["d", "/run/ndimon", "0755", "root", "root"]
        );
    }

    #[test]
    fn tmpfiles_rerun_clears_inherited_setgid_and_preserves_private_files() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("var/lib/ndimon/net/vlans");
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o2700)).unwrap();
        let file = directory.join("vlans.json");
        fs::write(&file, b"{\"end0\":[]}").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let uid = nix::unistd::geteuid().as_raw().to_string();
        let gid = nix::unistd::getegid().as_raw().to_string();
        // Map the identities to the current user in the isolated root. Live
        // installation tests also check distinct root/API/control ownership.
        let mut declarations = String::new();
        for line in include_str!("../../../../packaging/debian/tmpfiles.d/ndimon.conf").lines() {
            if !line.starts_with("d ") {
                continue;
            }
            let mut columns: Vec<_> = line.split_whitespace().collect();
            columns[3] = &uid;
            columns[4] = &gid;
            declarations.push_str(&columns.join(" "));
            declarations.push('\n');
        }
        let conf = root.path().join("test.conf");
        fs::write(&conf, declarations).unwrap();
        for _ in 0..2 {
            let status = Command::new("systemd-tmpfiles")
                .arg("--create")
                .arg("--root")
                .arg(root.path())
                .arg(&conf)
                .status()
                .unwrap();
            assert!(status.success());
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o7777,
                0o700
            );
            assert_eq!(
                fs::metadata(&file).unwrap().permissions().mode() & 0o7777,
                0o600
            );
            assert_eq!(fs::read(&file).unwrap(), b"{\"end0\":[]}");
            assert_eq!(
                fs::metadata(root.path().join("var/lib/ndimon"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777,
                0o3770
            );
        }
    }
}
