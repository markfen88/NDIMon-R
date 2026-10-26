//! NE-6.2/6.4: exact bytes/metadata/absence, no-follow reads, durable writes.
use crate::{MAX_PRIVATE, Result, failed, invalid, locked, unsupported};
use nix::fcntl::{Flock, FlockArg};
use nix::unistd::{Gid, Uid, fchown, getegid, geteuid};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use subtle::ConstantTimeEq;

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}
impl Secret {
    pub fn generate() -> Result<Self> {
        Ok(Self(random_id()?))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn matches(&self, value: &str) -> bool {
        value.len() == 64 && bool::from(self.0.as_bytes().ct_eq(value.as_bytes()))
    }
}
pub fn random_id() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| failed("random", "entropy_unavailable"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub fn valid_id(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Image {
    pub bytes: Vec<u8>,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileChange {
    pub path: String,
    pub before: Option<Image>,
    pub after: Option<Image>,
}

/// Production anchor is /. A fixture anchor is explicitly trusted; checks stop
/// there, allowing a private tempfile under /tmp without trusting other /tmp data.
#[derive(Debug, Clone)]
pub struct Root {
    anchor: PathBuf,
}
impl Root {
    #[cfg(any(test, feature = "harness"))]
    pub fn checkpoint(&self, path: &str, point: &str) -> Result<()> {
        const CONTROL: &str = "/run/ndimon-harness-storage-fault.json";
        if path == CONTROL {
            return Ok(());
        }
        let Some(text) = self.text(CONTROL)? else {
            return Ok(());
        };
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fault {
            path: String,
            point: String,
            kill: bool,
        }
        let fault: Fault =
            serde_json::from_str(&text).map_err(|_| failed("harness", "fault_format"))?;
        if fault.path == path && fault.point == point {
            self.replace(CONTROL, None)?;
            if fault.kill {
                nix::sys::signal::kill(nix::unistd::getpid(), nix::sys::signal::Signal::SIGKILL)
                    .map_err(|_| failed("harness", "kill_failed"))?;
                std::process::abort();
            }
            return Err(failed("filesystem", &format!("injected_{point}")));
        }
        Ok(())
    }
    #[cfg(not(any(test, feature = "harness")))]
    fn checkpoint(&self, _path: &str, _point: &str) -> Result<()> {
        Ok(())
    }
    pub fn new(anchor: PathBuf) -> Result<Self> {
        if !anchor.is_absolute()
            || anchor
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(unsupported("unsafe_configuration"));
        }
        let m = fs::symlink_metadata(&anchor).map_err(|_| unsupported("unsafe_configuration"))?;
        if !m.is_dir() || m.uid() != geteuid().as_raw() || m.mode() & 0o022 != 0 {
            return Err(unsupported("unsafe_configuration"));
        }
        Ok(Self { anchor })
    }
    pub fn production() -> Self {
        Self {
            anchor: PathBuf::from("/"),
        }
    }
    pub fn path(&self, absolute: &str) -> Result<PathBuf> {
        let p = Path::new(absolute);
        if !p.is_absolute()
            || p.components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(unsupported("unsafe_configuration"));
        }
        Ok(self.anchor.join(
            p.strip_prefix("/")
                .map_err(|_| unsupported("unsafe_configuration"))?,
        ))
    }
    /// Walk the ancestors of `path` and refuse any that is not a directory owned
    /// by us and closed to group and others. Returns false when a component does
    /// not exist and `create` is off. An ancestor of a path that does not exist
    /// is never trusted with anything, so it is only judged when the whole chain
    /// exists (a networkd search path that is absent must not make a writable
    /// `/usr/local/lib` fail every status). Creating always judges first. The
    /// refusal names the directory and why: `unsafe_configuration: <detail>`.
    fn parents(&self, path: &Path, create: bool) -> Result<bool> {
        let parent = path
            .parent()
            .ok_or_else(|| unsupported("unsafe_configuration"))?;
        let relative = parent
            .strip_prefix(&self.anchor)
            .map_err(|_| unsupported("unsafe_configuration"))?;
        let mut current = self.anchor.clone();
        let mut untrusted = None;
        for part in std::iter::once(None).chain(relative.components().map(Some)) {
            if let Some(part) = part {
                current.push(part.as_os_str());
            }
            let m = match fs::symlink_metadata(&current) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
                    fs::DirBuilder::new()
                        .mode(0o755)
                        .create(&current)
                        .map_err(|_| failed("filesystem", "create_directory"))?;
                    sync_dir(
                        current
                            .parent()
                            .ok_or_else(|| unsupported("unsafe_configuration"))?,
                    )?;
                    fs::symlink_metadata(&current).map_err(|_| failed("filesystem", "metadata"))?
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(_) => return Err(failed("filesystem", "metadata")),
            };
            if untrusted.is_none() {
                if let Some(why) = untrusted_directory(&m) {
                    let shown =
                        Path::new("/").join(current.strip_prefix(&self.anchor).unwrap_or(&current));
                    let error =
                        unsupported(&format!("unsafe_configuration: {} {why}", shown.display()));
                    if create {
                        return Err(error);
                    }
                    untrusted = Some(error);
                }
            }
        }
        untrusted.map_or(Ok(true), Err)
    }
    pub fn exists(&self, path: &str) -> Result<bool> {
        let p = self.path(path)?;
        if !self.parents(&p, false)? {
            return Ok(false);
        }
        match fs::symlink_metadata(p) {
            Ok(m) if m.file_type().is_symlink() => Err(unsupported("unsafe_configuration")),
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(failed("filesystem", "metadata")),
        }
    }
    pub fn snapshot(&self, path: &str) -> Result<Option<Image>> {
        for attempt in 0..8 {
            if let Some(result) = self.snapshot_attempt(path)? {
                return Ok(result);
            }
            std::thread::sleep(std::time::Duration::from_millis(1 << attempt));
        }
        Err(failed("filesystem", "snapshot_busy"))
    }
    // None means our opened inode was unlinked by an atomic replacement before
    // fstat. Reopen the path instead of mistaking a normal writer for a hardlink
    // attack. Every accepted inode still satisfies the exact single-link check.
    fn snapshot_attempt(&self, path: &str) -> Result<Option<Option<Image>>> {
        let p = self.path(path)?;
        if !self.parents(&p, false)? {
            return Ok(Some(None));
        }
        let f = match OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .open(&p)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Some(None)),
            Err(_) => return Err(unsupported("unsafe_configuration")),
        };
        let m = f.metadata().map_err(|_| failed("filesystem", "metadata"))?;
        if m.nlink() == 0 {
            return Ok(None);
        }
        if !m.is_file()
            || m.uid() != geteuid().as_raw()
            || m.mode() & 0o022 != 0
            || m.len() > MAX_PRIVATE as u64
            || m.nlink() != 1
        {
            #[cfg(feature = "harness")]
            eprintln!(
                "UNSAFE snapshot path={path} mode={:o} uid={} nlink={}",
                m.mode(),
                m.uid(),
                m.nlink()
            );
            return Err(unsupported("unsafe_configuration"));
        }
        let mut bytes = Vec::new();
        f.take(MAX_PRIVATE as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| failed("filesystem", "read"))?;
        if bytes.len() > MAX_PRIVATE {
            return Err(unsupported("unsafe_configuration"));
        }
        Ok(Some(Some(Image {
            bytes,
            mode: m.mode() & 0o7777,
            uid: m.uid(),
            gid: m.gid(),
        })))
    }
    pub fn text(&self, path: &str) -> Result<Option<String>> {
        self.snapshot(path)?
            .map(|i| String::from_utf8(i.bytes).map_err(|_| unsupported("unsafe_configuration")))
            .transpose()
    }
    pub fn image(text: &str, mode: u32) -> Image {
        Image {
            bytes: text.as_bytes().to_vec(),
            mode,
            uid: geteuid().as_raw(),
            gid: getegid().as_raw(),
        }
    }
    pub fn replace(&self, path: &str, image: Option<&Image>) -> Result<()> {
        let target = self.path(path)?;
        self.parents(&target, image.is_some())?;
        // Validate the old target before either replacement or deletion.
        let _ = self.snapshot(path)?;
        let Some(image) = image else {
            self.checkpoint(path, "remove")?;
            return match fs::remove_file(&target) {
                Ok(()) => {
                    self.checkpoint(path, "remove_directory_fsync")?;
                    sync_dir(
                        target
                            .parent()
                            .ok_or_else(|| unsupported("unsafe_configuration"))?,
                    )
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(_) => Err(failed("filesystem", "remove")),
            };
        };
        let temporary = target.with_file_name(format!(".ndimon-{}", random_id()?));
        let result = (|| {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(nix::libc::O_NOFOLLOW)
                .open(&temporary)
                .map_err(|_| failed("filesystem", "create_temporary"))?;
            self.checkpoint(path, "write")?;
            f.write_all(&image.bytes)
                .map_err(|_| failed("filesystem", "write"))?;
            fchown(
                f.as_raw_fd(),
                Some(Uid::from_raw(image.uid)),
                Some(Gid::from_raw(image.gid)),
            )
            .map_err(|_| failed("filesystem", "ownership"))?;
            f.set_permissions(Permissions::from_mode(image.mode))
                .map_err(|_| failed("filesystem", "permissions"))?;
            self.checkpoint(path, "file_fsync")?;
            f.sync_all()
                .map_err(|_| failed("filesystem", "file_fsync"))?;
            self.checkpoint(path, "rename")?;
            fs::rename(&temporary, &target).map_err(|_| failed("filesystem", "rename"))?;
            self.checkpoint(path, "directory_fsync")?;
            sync_dir(
                target
                    .parent()
                    .ok_or_else(|| unsupported("unsafe_configuration"))?,
            )
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
    pub fn list(&self, directory: &str, extension: Option<&str>) -> Result<Vec<String>> {
        let p = self.path(directory)?;
        if !self.parents(&p.join("entry"), false)? {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for e in fs::read_dir(p).map_err(|_| failed("filesystem", "read_directory"))? {
            let e = e.map_err(|_| failed("filesystem", "read_directory"))?;
            let name = e
                .file_name()
                .into_string()
                .map_err(|_| unsupported("unsafe_configuration"))?;
            if extension.is_none_or(|s| name.ends_with(s)) {
                out.push(format!("{directory}/{name}"));
            }
        }
        out.sort();
        Ok(out)
    }
    pub fn change(&self, path: &str, text: Option<&str>, mode: u32) -> Result<Option<FileChange>> {
        let before = self.snapshot(path)?;
        let after = text.map(|s| Self::image(s, mode));
        if before == after {
            Ok(None)
        } else {
            Ok(Some(FileChange {
                path: path.into(),
                before,
                after,
            }))
        }
    }
}
use std::os::fd::AsRawFd;
/// Why a directory on the way to a configuration file is not trusted, in words
/// an operator can act on; `None` when it is.
fn untrusted_directory(m: &fs::Metadata) -> Option<String> {
    if !m.is_dir() {
        Some("is not a directory".into())
    } else if m.uid() != geteuid().as_raw() {
        Some(format!(
            "is owned by uid {}, not {}",
            m.uid(),
            geteuid().as_raw()
        ))
    } else if m.mode() & 0o022 != 0 {
        Some(format!(
            "is writable by its group or others (mode {:04o})",
            m.mode() & 0o7777
        ))
    } else {
        None
    }
}
fn sync_dir(p: &Path) -> Result<()> {
    File::open(p)
        .and_then(|f| f.sync_all())
        .map_err(|_| failed("filesystem", "directory_fsync"))
}

#[derive(Debug, Clone)]
pub struct Store {
    pub root: Root,
}
impl Store {
    fn prepare(&self) -> Result<()> {
        let path = self.root.path(crate::STATE_DIRECTORY)?;
        self.root.parents(&path, true)?;
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => sync_dir(
                path.parent()
                    .ok_or_else(|| unsupported("unsafe_configuration"))?,
            )?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err(failed("journal", "create_directory")),
        }
        let m = fs::symlink_metadata(path).map_err(|_| failed("journal", "metadata"))?;
        if !m.is_dir()
            || m.uid() != geteuid().as_raw()
            || m.gid() != getegid().as_raw()
            || m.mode() & 0o7777 != 0o700
        {
            return Err(unsupported("unsafe_configuration"));
        }
        Ok(())
    }
    pub(crate) fn private_directory(&self, path: &str) -> Result<()> {
        self.prepare()?;
        let target = self.root.path(path)?;
        self.root.parents(&target, true)?;
        match fs::DirBuilder::new().mode(0o700).create(&target) {
            Ok(()) => sync_dir(
                target
                    .parent()
                    .ok_or_else(|| unsupported("unsafe_configuration"))?,
            )?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err(failed("handoff", "create_directory")),
        }
        let m = fs::symlink_metadata(target).map_err(|_| failed("handoff", "metadata"))?;
        if !m.is_dir()
            || m.uid() != geteuid().as_raw()
            || m.gid() != getegid().as_raw()
            || m.mode() & 0o7777 != 0o700
        {
            return Err(unsupported("unsafe_configuration"));
        }
        Ok(())
    }
    pub fn lock(&self) -> Result<Flock<File>> {
        self.prepare()?;
        let path = self
            .root
            .path(&format!("{}/lock", crate::STATE_DIRECTORY))?;
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(path)
            .map_err(|_| unsupported("unsafe_configuration"))?;
        let m = f.metadata().map_err(|_| failed("journal", "metadata"))?;
        if !m.is_file()
            || m.uid() != geteuid().as_raw()
            || m.gid() != getegid().as_raw()
            || m.mode() & 0o7777 != 0o600
            || m.nlink() != 1
        {
            return Err(unsupported("unsafe_configuration"));
        }
        Flock::lock(f, FlockArg::LockExclusiveNonblock).map_err(|_| locked("network_operation"))
    }
    fn path(name: &str) -> Result<String> {
        if !matches!(
            name,
            "pending" | "last" | "committed" | "upgrade" | "handoff" | "quiesce" | "external"
        ) {
            return Err(invalid("journal", "invalid_type"));
        }
        Ok(format!("{}/{name}.json", crate::STATE_DIRECTORY))
    }
    pub fn read<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        // Read-only observers never create the store.
        let path = Self::path(name)?;
        let Some(i) = self.root.snapshot(&path)? else {
            return Ok(None);
        };
        self.prepare()?;
        if i.mode != 0o600 || i.gid != getegid().as_raw() {
            return Err(unsupported("unsafe_configuration"));
        }
        serde_json::from_slice(&i.bytes)
            .map(Some)
            .map_err(|_| failed("journal", "invalid_state"))
    }
    pub fn write(&self, name: &str, value: &impl Serialize) -> Result<()> {
        self.prepare()?;
        let bytes = serde_json::to_vec(value).map_err(|_| failed("journal", "serialize"))?;
        if bytes.len() > MAX_PRIVATE {
            return Err(failed("journal", "snapshot_too_large"));
        }
        let text = String::from_utf8(bytes).map_err(|_| failed("journal", "serialize"))?;
        self.root
            .replace(&Self::path(name)?, Some(&Root::image(&text, 0o600)))
    }
    pub fn remove(&self, name: &str) -> Result<()> {
        self.root.replace(&Self::path(name)?, None)
    }
}
