//! NE-3.2/6.14: bounded adapters for the two shipped Python journals and index.
//! The old helpers are never executed. Upgrade recovery has its own durable job.
use std::collections::BTreeMap;
use std::fs::{File, Permissions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use ndimon_types::{AddressMode, NetworkBackend, NetworkProfile, NetworkProfiles};
use nix::fcntl::{Flock, FlockArg};
use nix::unistd::{Gid, Group, Uid, User, fchown, getegid, geteuid};
use rustix::fs::{AtFlags, Mode, OFlags, openat, unlinkat};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::backend::{self, Metadata, NmChange, Plan};
use crate::engine::{Authority, Commit};
use crate::host::{Host, Tool};
use crate::store::{FileChange, Image, Root, Secret, Store, random_id, valid_id};
use crate::validation;
use crate::{BANNER, MAX_PRIVATE, PRIVD, Result, failed, locked, unsupported};

const BASE_DIR: &str = "/var/lib/ndimon/net/pending";
const INDEX_DIR: &str = "/var/lib/ndimon/net/vlans";
const LEGACY_UNITS: &[&str] = &[
    "ndimon-net-apply",
    "ndimon-net-rollback",
    "ndimon-profiles-apply",
    "ndimon-profiles-rollback",
];

/// Pins each ancestor with openat(NO_FOLLOW). Only the two historical staging
/// ancestors may be writable; the private final directory and file may not be.
/// This exception never applies to ordinary engine storage or configuration.
#[derive(Debug)]
pub(crate) struct PrivateDirectory(File, String);
impl PrivateDirectory {
    pub(crate) fn open(root: &Root, path: &str) -> Result<Option<Self>> {
        let mut dir =
            File::open(root.path("/")?).map_err(|_| unsupported("unsafe_configuration"))?;
        let mut walked = String::new();
        for component in path.trim_start_matches('/').split('/') {
            if component.is_empty() || component == "." || component == ".." {
                return Err(unsupported("unsafe_configuration"));
            }
            walked.push('/');
            walked.push_str(component);
            let fd = match openat(
                &dir,
                component,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => fd,
                Err(rustix::io::Errno::NOENT) => return Ok(None),
                Err(_) => return Err(unsupported("unsafe_configuration")),
            };
            dir = File::from(fd);
            let m = dir
                .metadata()
                .map_err(|_| failed("migration", "metadata"))?;
            let staging = walked == "/var/lib/ndimon" || walked == "/var/lib/ndimon/net";
            if m.uid() != geteuid().as_raw()
                || (!staging && m.mode() & 0o022 != 0)
                || (walked == path && (m.mode() & 0o7777 != 0o700 || m.gid() != getegid().as_raw()))
            {
                return Err(unsupported("unsafe_configuration"));
            }
        }
        Ok(Some(Self(dir, path.into())))
    }
    pub(crate) fn read(&self, name: &str) -> Result<Option<Image>> {
        if name.contains('/') {
            return Err(unsupported("unsafe_configuration"));
        }
        let fd = match openat(
            &self.0,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(_) => return Err(unsupported("unsafe_configuration")),
        };
        let file = File::from(fd);
        let m = file
            .metadata()
            .map_err(|_| failed("migration", "metadata"))?;
        if !m.is_file()
            || m.nlink() != 1
            || m.uid() != geteuid().as_raw()
            || m.gid() != getegid().as_raw()
            || m.mode() & 0o7777 != 0o600
            || m.len() > MAX_PRIVATE as u64
        {
            return Err(unsupported("unsafe_configuration"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_PRIVATE as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| failed("migration", "read"))?;
        if bytes.len() > MAX_PRIVATE {
            return Err(unsupported("unsafe_configuration"));
        }
        Ok(Some(Image {
            bytes,
            mode: 0o600,
            uid: m.uid(),
            gid: m.gid(),
        }))
    }
    pub(crate) fn lock(&self) -> Result<Flock<File>> {
        self.lock_as(legacy_service_ids())
    }
    /// The lock holds no data. 1.3.0 created it with the default umask (0644),
    /// as root, and the finder's `StateDirectory=ndimon` then re-owned the tree
    /// to `service` (VV-2 N1): exactly those shapes are taken over and made
    /// private in place (the inode, and so any flock on it, stays). Any other
    /// shape is refused, naming the file and what was found.
    fn lock_as(&self, service: Option<(u32, u32)>) -> Result<Flock<File>> {
        let fd = openat(
            &self.0,
            "lock",
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| {
            unsupported(&format!(
                "unsafe_configuration: {}/lock cannot be opened without following links",
                self.1
            ))
        })?;
        let file = File::from(fd);
        let m = file
            .metadata()
            .map_err(|_| failed("migration", "metadata"))?;
        let engine = (geteuid().as_raw(), getegid().as_raw());
        let found = (m.uid(), m.gid(), m.mode() & 0o7777);
        if !m.is_file() || m.nlink() != 1 {
            return Err(unsupported(&format!(
                "unsafe_configuration: {}/lock is not a regular file with a single link",
                self.1
            )));
        }
        if found != (engine.0, engine.1, 0o600) {
            if !legacy_lock(found, engine, service) {
                return Err(unsupported(&format!(
                    "unsafe_configuration: {}/lock is owned by {}:{} with mode {:04o}, which is not a lock 1.3.0 created (root or ndimon, mode 0600 or 0644); it holds no data, remove it and retry",
                    self.1, found.0, found.1, found.2
                )));
            }
            fchown(
                file.as_raw_fd(),
                Some(Uid::from_raw(engine.0)),
                Some(Gid::from_raw(engine.1)),
            )
            .map_err(|_| failed("migration", "lock_ownership"))?;
            file.set_permissions(Permissions::from_mode(0o600))
                .map_err(|_| failed("migration", "lock_permissions"))?;
            file.sync_all()
                .map_err(|_| failed("migration", "lock_fsync"))?;
        }
        Flock::lock(file, FlockArg::LockExclusiveNonblock)
            .map_err(|_| locked("legacy_network_writer"))
    }
    pub(crate) fn remove(&self, name: &str) -> Result<()> {
        let _ = self.read(name)?;
        match unlinkat(&self.0, name, AtFlags::empty()) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => self
                .0
                .sync_all()
                .map_err(|_| failed("migration", "directory_fsync")),
            Err(_) => Err(failed("migration", "remove")),
        }
    }
}

/// The account and group 1.3.0's services ran as: its finder unit re-owned
/// `/var/lib/ndimon` to them at every start.
fn legacy_service_ids() -> Option<(u32, u32)> {
    let user = User::from_name("ndimon").ok().flatten()?;
    let group = Group::from_name("ndimon").ok().flatten()?;
    Some((user.uid.as_raw(), group.gid.as_raw()))
}
/// A lock 1.3.0 left: owned by root (the engine) or by its `ndimon` service,
/// mode 0600 or 0644. `found` is (uid, gid, mode).
fn legacy_lock(found: (u32, u32, u32), engine: (u32, u32), service: Option<(u32, u32)>) -> bool {
    let (uid, gid, mode) = found;
    matches!(mode, 0o600 | 0o644) && ((uid, gid) == engine || Some((uid, gid)) == service)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PythonProfiles {
    version: u32,
    token: Secret,
    phase: String,
    expires: f64,
    boot: String,
    previous: Value,
    requested: Value,
    files: BTreeMap<String, Option<(String, u32)>>,
    nm: BTreeMap<String, Option<BTreeMap<String, String>>>,
    address: String,
    running: Vec<String>,
    #[serde(default)]
    nft_present: bool,
    #[serde(default)]
    nft_previous: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PythonBase {
    token: Secret,
    phase: String,
    deadline: u64,
    backend: NetworkBackend,
    interface: String,
    previous: Value,
    requested: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Vlan {
    id: u16,
    mode: AddressMode,
    address: String,
    prefix: u8,
    gateway: String,
    allow_gateway: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Upgrade {
    version: u32,
    job_id: String,
    base: Commit,
    profiles: Option<NetworkProfiles>,
    legacy_revision: u64,
    restore: Option<Plan>,
    base_restore: Option<PythonBase>,
    profile_pending: bool,
    base_pending: bool,
    index: bool,
    recovered: bool,
    replacement: Option<Commit>,
    recovery_error: Option<ndimon_types::ApiError>,
}

fn decode<T: serde::de::DeserializeOwned>(image: &Image) -> Result<T> {
    serde_json::from_slice(&image.bytes).map_err(|_| failed("migration", "invalid_legacy_state"))
}
fn profiles(value: &Value) -> Result<(u64, NetworkProfiles)> {
    validation::import_legacy(
        &serde_json::to_vec(value).map_err(|_| failed("migration", "serialize"))?,
    )
}
fn phase(value: &str) -> Result<()> {
    if !["scheduled", "applying", "applied", "reverting", "confirmed"].contains(&value) {
        return Err(failed("migration", "invalid_legacy_phase"));
    }
    Ok(())
}
fn private_json(host: &Host, name: &str) -> Result<Option<Value>> {
    Store {
        root: host.root.clone(),
    }
    .read(name)
}
pub(crate) fn needed(host: &Host) -> Result<bool> {
    if private_json(host, "upgrade")?.is_some() {
        return Ok(true);
    }
    if private_json(host, "pending")?.is_some_and(|v| v.get("job_id").is_none()) {
        return Ok(true);
    }
    if private_json(host, "committed")?.is_some_and(|v| v.get("networks").is_some()) {
        return Ok(true);
    }
    for (path, name) in [(BASE_DIR, "state.json"), (INDEX_DIR, "vlans.json")] {
        if PrivateDirectory::open(&host.root, path)?
            .map(|d| d.read(name))
            .transpose()?
            .flatten()
            .is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn artifact(path: &str, p: &NetworkProfiles, q: &NetworkProfiles) -> bool {
    let names = backend::names(p)
        .into_iter()
        .chain(backend::names(q))
        .collect::<Vec<_>>();
    if [
        crate::BINDING,
        crate::TESSERA_BINDING,
        BANNER,
        "/etc/modules-load.d/ndimon-8021q.conf",
        "/etc/netplan/90-ndimon.yaml",
        "/etc/netplan/91-ndimon-vlans.yaml",
        "/etc/network/interfaces.d/ndimon-vlans",
        "/etc/dhcp/dhclient-enter-hooks.d/ndimon-vlans",
        "/etc/systemd/system/ndimonndi.slice",
        "/etc/systemd/system/ndimontessera.slice",
    ]
    .contains(&path)
    {
        return true;
    }
    if backend::MEDIA
        .iter()
        .any(|u| path == format!("/etc/systemd/system/{u}.d/70-ndimon-network.conf"))
    {
        return true;
    }
    if path == format!("/etc/network/interfaces.d/ndimon-{}", p.interface) {
        return true;
    }
    if names.iter().any(|n| {
        path == format!("/etc/systemd/network/00-ndimon-{n}.network")
            || path == format!("/etc/systemd/network/00-ndimon-{n}.netdev")
    }) {
        return true;
    }
    path == format!("/etc/systemd/network/05-ndimon-{}.network", p.interface)
        || path.starts_with("/etc/systemd/network/")
            && path.ends_with(&format!(".network.d/50-ndimon-vlans-{}.conf", p.interface))
}
fn restore_plan(host: &Host, pending: &PythonProfiles) -> Result<Plan> {
    let (_, previous) = profiles(&pending.previous)?;
    let (_, candidate) = profiles(&pending.requested)?;
    validation::validate(&candidate, &previous)?;
    let mut files = Vec::new();
    for (path, old) in &pending.files {
        if !artifact(path, &previous, &candidate) {
            return Err(unsupported("unsafe_legacy_artifact"));
        }
        let before = old.as_ref().map(|(text, mode)| Root::image(text, *mode));
        if before
            .as_ref()
            .is_some_and(|i| i.mode & !0o777 != 0 || i.mode & 0o022 != 0)
        {
            return Err(unsupported("unsafe_legacy_artifact"));
        }
        files.push(FileChange {
            path: path.clone(),
            before,
            after: host.root.snapshot(path)?,
        });
    }
    if !pending
        .running
        .iter()
        .all(|u| backend::MEDIA.contains(&u.as_str()))
    {
        return Err(unsupported("unsafe_legacy_artifact"));
    }
    if pending.nft_present
        && !pending
            .nft_previous
            .trim_start()
            .starts_with("table inet ndimon_management {")
    {
        return Err(unsupported("unsafe_legacy_artifact"));
    }
    // Only fields the shipped Python helper actually modified are restored.
    const FIELDS: &[&str] = &[
        "ipv4.method",
        "ipv4.addresses",
        "ipv4.gateway",
        "ipv4.dns",
        "ipv4.never-default",
        "ipv4.ignore-auto-dns",
        "ipv4.ignore-auto-routes",
        "ipv4.may-fail",
        "ipv6.method",
        "connection.autoconnect",
        "connection.wait-device-timeout",
    ];
    let mut nm = Vec::new();
    for (name, before) in &pending.nm {
        if name.is_empty()
            || name.len() > 256
            || name.chars().any(char::is_control)
            || before
                .as_ref()
                .is_some_and(|m| m.keys().any(|k| !FIELDS.contains(&k.as_str())))
        {
            return Err(unsupported("unsafe_legacy_artifact"));
        }
        let out = host.output(
            Tool::Nmcli,
            &[
                "-g".into(),
                "connection.uuid".into(),
                "connection".into(),
                "show".into(),
                "id".into(),
                name.clone(),
            ],
            20_000,
            None,
        )?;
        if out.code == 10 && before.is_none() {
            continue;
        }
        if out.code != 0 {
            return Err(failed("migration", "missing_nm_snapshot_connection"));
        }
        let uuid = out.stdout.trim().to_owned();
        if uuid.len() != 36 || !uuid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return Err(unsupported("unsafe_legacy_artifact"));
        }
        nm.push(NmChange {
            uuid,
            interface: String::new(),
            tag: None,
            before: before.clone(),
            active: before.is_some(),
            after: BTreeMap::new(),
            remove_after_commit: false,
            files: Vec::new(),
        });
    }
    Ok(Plan {
        files,
        nm,
        previous,
        candidate,
        previous_metadata: Metadata::default(),
        metadata: Metadata::default(),
        old_table: pending.nft_present.then(|| pending.nft_previous.clone()),
        active_services: pending.running.clone(),
        affected_services: backend::MEDIA.iter().map(|s| (*s).into()).collect(),
        portal_port: 80,
        drift: Vec::new(),
    })
}

fn quiesce(host: &Host) -> Result<()> {
    for name in LEGACY_UNITS {
        for suffix in ["timer", "service"] {
            let unit = format!("{name}.{suffix}");
            if host.active(&unit)? {
                host.run(Tool::Systemctl, &["stop", "--no-block", &unit], 8)?;
            }
        }
    }
    Ok(())
}
fn retry(host: &Host, id: &str) -> Result<()> {
    let unit = format!("ndimon-network-upgrade-{id}");
    if !host.active(&format!("{unit}.timer"))? {
        host.run_owned(
            Tool::SystemdRun,
            &[
                "--quiet".into(),
                "--collect".into(),
                format!("--unit={unit}"),
                "--on-active=10s".into(),
                "--on-unit-inactive=10s".into(),
                "--property=RuntimeMaxSec=90s".into(),
                "--property=TimeoutStopSec=2s".into(),
                "--property=KillMode=control-group".into(),
                "--property=NoNewPrivileges=yes".into(),
                "--property=ProtectClock=yes".into(),
                "--property=CapabilityBoundingSet=~CAP_SYS_TIME".into(),
                "--".into(),
                PRIVD.into(),
                "network-job".into(),
                "recover".into(),
                id.into(),
            ],
            15,
            None,
        )?;
    }
    Ok(())
}
fn make(
    host: &Host,
    authority: &dyn Authority,
    base_dir: Option<&PrivateDirectory>,
    index_dir: Option<&PrivateDirectory>,
) -> Result<Upgrade> {
    let committed = private_json(host, "committed")?;
    let pending = private_json(host, "pending")?;
    if pending.as_ref().is_some_and(|v| v.get("job_id").is_some()) {
        return Err(locked("pending_transaction"));
    }
    let mut u = Upgrade {
        version: 1,
        job_id: random_id()?,
        base: authority.load()?,
        profiles: None,
        legacy_revision: 0,
        restore: None,
        base_restore: None,
        profile_pending: pending.is_some(),
        base_pending: false,
        index: false,
        recovered: false,
        replacement: None,
        recovery_error: None,
    };
    if let Some(saved) = committed.as_ref() {
        (u.legacy_revision, u.profiles) = {
            let (r, p) = profiles(saved)?;
            (r, Some(p))
        };
    }
    if let Some(value) = pending {
        let p: PythonProfiles = serde_json::from_value(value)
            .map_err(|_| failed("migration", "invalid_legacy_state"))?;
        phase(&p.phase)?;
        if p.version != 1 || !valid_id(p.token.expose()) || !p.expires.is_finite() {
            return Err(failed("migration", "invalid_legacy_state"));
        }
        let (revision, requested) = profiles(&p.requested)?;
        let completed = committed.as_ref().is_some_and(|v| {
            profiles(v).is_ok_and(|(r, doc)| revision.checked_add(1) == Some(r) && requested == doc)
        });
        if p.phase == "confirmed" && !completed {
            return Err(failed("migration", "legacy_commit_missing"));
        }
        if !completed {
            let (r, old) = profiles(&p.previous)?;
            if u.profiles.as_ref().is_some_and(|saved| *saved != old) {
                return Err(failed("migration", "legacy_commit_conflict"));
            }
            u.profiles = Some(old);
            u.legacy_revision = r;
            if p.phase != "scheduled" {
                u.restore = Some(restore_plan(host, &p)?);
            }
        }
    }
    if let Some(image) = base_dir
        .map(|d| d.read("state.json"))
        .transpose()?
        .flatten()
    {
        if u.profile_pending {
            return Err(locked("conflicting_legacy_journals"));
        }
        let b: PythonBase = decode(&image)?;
        phase(&b.phase)?;
        if !valid_id(b.token.expose()) || !validation::physical(&b.interface) {
            return Err(failed("migration", "invalid_legacy_state"));
        }
        u.base_pending = true;
        if b.phase != "confirmed" && b.phase != "scheduled" {
            u.base_restore = Some(b);
        }
    }
    u.index = index_dir
        .map(|d| d.read("vlans.json"))
        .transpose()?
        .flatten()
        .is_some();
    Ok(u)
}

fn restore_base(host: &Host, b: &PythonBase) -> Result<()> {
    let kind = b.previous["kind"]
        .as_str()
        .ok_or_else(|| failed("migration", "invalid_legacy_state"))?;
    if kind == "networkmanager" {
        let name = b.previous["profile"]
            .as_str()
            .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
            .ok_or_else(|| unsupported("unsafe_legacy_artifact"))?;
        let addresses: Vec<String> = serde_json::from_value(b.previous["addresses"].clone())
            .map_err(|_| failed("migration", "invalid_legacy_state"))?;
        let dns: Vec<String> = serde_json::from_value(b.previous["dns"].clone())
            .map_err(|_| failed("migration", "invalid_legacy_state"))?;
        let method = b.previous["method"]
            .as_str()
            .filter(|s| ["auto", "manual", "dhcp"].contains(s))
            .ok_or_else(|| unsupported("unrepresentable_configuration"))?;
        let gateway = b.previous["gateway"]
            .as_str()
            .ok_or_else(|| failed("migration", "invalid_legacy_state"))?;
        host.run(
            Tool::Nmcli,
            &[
                "connection",
                "modify",
                "id",
                name,
                "ipv4.method",
                method,
                "ipv4.addresses",
                &addresses.join(","),
                "ipv4.gateway",
                gateway,
                "ipv4.dns",
                &dns.join(" "),
            ],
            20,
        )?;
        host.run(Tool::Nmcli, &["connection", "up", "id", name], 40)?;
    } else {
        let path = b.previous["path"]
            .as_str()
            .ok_or_else(|| failed("migration", "invalid_legacy_state"))?;
        let allowed = match kind {
            "networkd" => {
                path.starts_with("/etc/systemd/network/")
                    && path.ends_with(".network")
                    && !path.contains("..")
            }
            "netplan" => path == "/etc/netplan/90-ndimon.yaml",
            "ifupdown" => path == format!("/etc/network/interfaces.d/ndimon-{}", b.interface),
            _ => false,
        };
        if !allowed || path.matches('/').count() != if kind == "netplan" { 3 } else { 4 } {
            return Err(unsupported("unsafe_legacy_artifact"));
        }
        let existed = b.previous["existed"]
            .as_bool()
            .ok_or_else(|| failed("migration", "invalid_legacy_state"))?;
        let text = b.previous["content"]
            .as_str()
            .ok_or_else(|| failed("migration", "invalid_legacy_state"))?;
        if kind == "ifupdown" {
            let _ = host.output(
                Tool::Ifdown,
                &["--force".into(), b.interface.clone()],
                30_000,
                None,
            )?;
        }
        host.root.replace(
            path,
            existed
                .then(|| Root::image(text, if kind == "netplan" { 0o600 } else { 0o644 }))
                .as_ref(),
        )?;
        match kind {
            "netplan" => {
                host.run(Tool::Netplan, &["generate"], 30)?;
                host.run(Tool::Netplan, &["apply"], 40)?;
                host.run(Tool::Networkctl, &["reload"], 20)?;
                host.run(Tool::Networkctl, &["reconfigure", "--", &b.interface], 30)?;
            }
            "networkd" => {
                host.run(Tool::Networkctl, &["reload"], 20)?;
                host.run(Tool::Networkctl, &["reconfigure", "--", &b.interface], 30)?;
            }
            "ifupdown" => {
                host.run(Tool::Ifup, &[&b.interface], 40)?;
            }
            _ => return Err(unsupported("unrepresentable_configuration")),
        }
    }
    Ok(())
}

fn import_index(
    host: &Host,
    p: &mut NetworkProfiles,
    dir: Option<&PrivateDirectory>,
) -> Result<()> {
    let Some(image) = dir.map(|d| d.read("vlans.json")).transpose()?.flatten() else {
        return Ok(());
    };
    let map: BTreeMap<String, Vec<Vlan>> = decode(&image)?;
    for (base, rows) in map {
        if !validation::physical(&base) || base != p.interface && !rows.is_empty() {
            return Err(unsupported("unrepresentable_configuration"));
        }
        for row in rows {
            if row.allow_gateway || !row.gateway.is_empty() {
                return Err(unsupported("unrepresentable_configuration"));
            }
            let mut n =
                NetworkProfile::dhcp(format!("vlan-{}", row.id), format!("VLAN {}", row.id));
            n.vlan_id = Some(row.id);
            n.mode = row.mode;
            n.address = row.address;
            n.prefix = row.prefix;
            n.gateway = row.gateway;
            if let Some(old) = p.networks.iter().find(|n| n.vlan_id == Some(row.id)) {
                if old.mode != n.mode
                    || old.address != n.address
                    || old.prefix != n.prefix
                    || old.gateway != n.gateway
                {
                    return Err(failed("migration", "legacy_index_conflict"));
                }
            } else {
                p.networks.push(n);
            }
        }
    }
    let _ = host;
    *p = validation::validate(p, p)?;
    Ok(())
}

/// Caller holds the modern/Python-profile lock. Both historical writers are
/// quiesced, and the old base lock remains pinned throughout recovery/import.
pub(crate) fn run(
    host: &Host,
    authority: &dyn Authority,
    selected: Option<&str>,
    only_job: Option<&str>,
) -> Result<bool> {
    let store = Store {
        root: host.root.clone(),
    };
    let saved = store.read::<Upgrade>("upgrade")?;
    if let Some(id) = only_job {
        if saved.as_ref().is_none_or(|u| u.job_id != id) {
            return Ok(false);
        }
    } else if saved.is_none() && !needed(host)? {
        return Ok(false);
    }
    quiesce(host)?;
    let base_dir = PrivateDirectory::open(&host.root, BASE_DIR)?;
    let _base_lock = base_dir.as_ref().map(PrivateDirectory::lock).transpose()?;
    let index_dir = PrivateDirectory::open(&host.root, INDEX_DIR)?;
    let mut u = match saved {
        Some(u) => u,
        None => make(host, authority, base_dir.as_ref(), index_dir.as_ref())?,
    };
    if u.version != 1 || !valid_id(&u.job_id) {
        return Err(failed("migration", "invalid_state"));
    }
    store.write("upgrade", &u)?;
    let result: Result<()> = (|| {
        retry(host, &u.job_id)?;
        let actual = authority.load()?;
        if u.replacement
            .as_ref()
            .is_none_or(|c| !decision_matches(&actual, c))
        {
            if !u.recovered {
                if let Some(p) = &u.restore {
                    host.stop_consumers(p, true)?;
                    if p.previous.backend == Some(NetworkBackend::Ifupdown) {
                        host.deconfigure(&p.candidate, true)?;
                    }
                    for file in &p.files {
                        host.root.replace(&file.path, file.before.as_ref())?;
                    }
                    host.activate(&p.previous, &p.candidate, &p.previous_metadata, &p.nm, true)?;
                    let metadata = host.imported_metadata(&p.previous)?;
                    host.readiness(&p.previous, &metadata, true, false)?;
                    if p.old_table.is_some() {
                        host.slices()?;
                    }
                    host.set_table(p.old_table.as_deref())?;
                    host.restore_consumers(p, true)?;
                }
                if let Some(b) = &u.base_restore {
                    restore_base(host, b)?;
                }
                u.recovered = true;
                store.write("upgrade", &u)?;
            }
            let mut p = match &u.profiles {
                Some(p) => p.clone(),
                None if u.base.profiles.backend.is_some() => u.base.profiles.clone(),
                None => {
                    let records: BTreeMap<String, Vec<Vlan>> = index_dir
                        .as_ref()
                        .map(|d| d.read("vlans.json"))
                        .transpose()?
                        .flatten()
                        .map(|i| decode(&i))
                        .transpose()?
                        .unwrap_or_default();
                    let bases = records
                        .iter()
                        .filter(|(_, rows)| !rows.is_empty())
                        .collect::<Vec<_>>();
                    if bases.len() > 1 {
                        return Err(unsupported("unrepresentable_configuration"));
                    }
                    let base = selected.or_else(|| bases.first().map(|(base, _)| base.as_str()));
                    let owned = bases
                        .first()
                        .map(|(base, rows)| {
                            rows.iter()
                                .map(|r| format!("{base}.{}", r.id))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    host.adopt_known(base, &owned)?.0
                }
            };
            import_index(host, &mut p, index_dir.as_ref())?;
            let metadata = host.imported_metadata(&p)?;
            host.readiness(&p, &metadata, true, false)?;
            if u.base.profiles.backend.is_some() && u.base.profiles != p {
                return Err(locked("legacy_desired_conflict"));
            }
            let next = match &u.replacement {
                Some(c) => c.clone(),
                None => {
                    let revision = u
                        .base
                        .revision
                        .checked_add(1)
                        .ok_or_else(|| failed("migration", "revision_overflow"))?;
                    Commit {
                        version: 1,
                        revision,
                        profiles: p,
                        metadata,
                        transaction: Some(u.job_id.clone()),
                        network_commit_revision: Some(revision),
                    }
                }
            };
            u.replacement = Some(next.clone());
            store.write("upgrade", &u)?;
            if let Err(error) = authority.compare_and_commit(&u.base, &next) {
                if !decision_matches(&authority.load()?, &next) {
                    return Err(error);
                }
            }
        }
        // Source records stay intact until the one atomic desired-state import.
        if u.profile_pending {
            store.remove("pending")?;
        }
        if u.base_pending {
            if let Some(dir) = &base_dir {
                dir.remove("state.json")?;
            }
        }
        if u.index {
            if let Some(dir) = &index_dir {
                dir.remove("vlans.json")?;
            }
        }
        store.remove("committed")?;
        host.root.replace(BANNER, None)?;
        store.remove("upgrade")?;
        host.run(
            Tool::Systemctl,
            &[
                "stop",
                "--no-block",
                &format!("ndimon-network-upgrade-{}.timer", u.job_id),
            ],
            8,
        )?;
        Ok(())
    })();
    if let Err(error) = result {
        u.recovery_error = Some(error.clone());
        store.write("upgrade", &u)?;
        return Err(error);
    }
    Ok(true)
}
pub(crate) fn decision_matches(actual: &Commit, expected: &Commit) -> bool {
    let mut expected = expected.clone();
    if actual.revision < expected.revision {
        return false;
    }
    expected.revision = actual.revision;
    *actual == expected
}

#[cfg(test)]
mod tests {
    use super::legacy_lock;
    #[test] // VV-2 N1: exactly root or ndimon, mode 0600 or 0644, as 1.3.0 left it.
    fn legacy_lock_shapes_are_exactly_those_of_1_3_0() {
        let (root, ndimon) = ((0, 0), Some((998, 997)));
        for found in [
            (0, 0, 0o644),
            (0, 0, 0o600),
            (998, 997, 0o644),
            (998, 997, 0o600),
        ] {
            assert!(legacy_lock(found, root, ndimon), "{found:?}");
        }
        for found in [
            (0, 0, 0o666),
            (0, 0, 0o664),
            (0, 0, 0o640),
            (0, 0, 0o755),
            (0, 0, 0o4644),
            (998, 997, 0o666),
            (998, 0, 0o644),
            (0, 997, 0o644),
            (1000, 1000, 0o644),
            (1000, 1000, 0o600),
        ] {
            assert!(!legacy_lock(found, root, ndimon), "{found:?}");
        }
        // No ndimon account on the device: only the engine's own shape.
        assert!(!legacy_lock((998, 997, 0o644), root, None));
        assert!(legacy_lock((0, 0, 0o644), root, None));
    }
}
