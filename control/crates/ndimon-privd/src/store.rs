//! All writes use pinned directory descriptors. No untrusted ancestor is reopened.
use fs2::FileExt;
use ndimon_privd_proto::Error;
use nix::unistd::{Gid, Uid, fchown};
use rustix::fs::{AtFlags, Mode, OFlags, mkdirat, openat, renameat, unlinkat};
use std::{
    fs::{File, Permissions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(1);
pub fn next_id() -> u64 {
    // Numeric job IDs fit the legacy job-unit grammar.
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64)
        .wrapping_add(NEXT.fetch_add(1, Ordering::Relaxed))
}
#[derive(Debug)]
pub struct Directory(pub File);
impl Directory {
    pub fn child(&self, name: &str, create: bool) -> Result<Self, Error> {
        component(name)?;
        if create {
            match mkdirat(&self.0, name, Mode::from_raw_mode(0o755)) {
                Ok(()) => (),
                Err(e) if e == rustix::io::Errno::EXIST => (),
                Err(e) => return Err(Error::failed("mkdir", e)),
            }
        }
        openat(
            &self.0,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(|fd| Self(File::from(fd)))
        .map_err(|e| Error::failed("open directory", e))
    }
    pub fn trusted(&self) -> Result<(), Error> {
        let m = self
            .0
            .metadata()
            .map_err(|e| Error::failed("stat directory", e))?;
        if !m.is_dir() || m.uid() != nix::unistd::geteuid().as_raw() || m.mode() & 0o022 != 0 {
            return Err(Error::PermissionDenied);
        }
        Ok(())
    }
    pub fn open(&self, name: &str) -> Result<File, Error> {
        component(name)?;
        openat(
            &self.0,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|e| Error::failed("open file", e))
    }
    pub fn read(&self, name: &str, max: usize) -> Result<Option<Vec<u8>>, Error> {
        component(name)?;
        let fd = match openat(
            &self.0,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(e) if e == rustix::io::Errno::NOENT => return Ok(None),
            Err(e) => return Err(Error::failed("read", e)),
        };
        let file = File::from(fd);
        let m = file.metadata().map_err(|e| Error::failed("stat", e))?;
        if !m.is_file() || m.len() > max as u64 {
            return Err(Error::invalid("file", "not regular or exceeds limit"));
        }
        let mut bytes = vec![];
        file.take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| Error::failed("read", e))?;
        if bytes.len() > max {
            return Err(Error::invalid("file", "exceeds limit"));
        }
        Ok(Some(bytes))
    }
    pub fn atomic(
        &self,
        name: &str,
        bytes: &[u8],
        uid: u32,
        gid: u32,
        mode: u32,
    ) -> Result<(), Error> {
        self.trusted()?;
        component(name)?;
        self.regular_target(name)?;
        let temp = format!(".ndimon-{}-{}", std::process::id(), next_id());
        let result = (|| {
            let fd = openat(
                &self.0,
                temp.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|e| Error::failed("create temporary", e))?;
            let mut file = File::from(fd);
            fchown(
                file.as_raw_fd(),
                Some(Uid::from_raw(uid)),
                Some(Gid::from_raw(gid)),
            )
            .map_err(|e| Error::failed("chown", e))?;
            file.set_permissions(Permissions::from_mode(mode))
                .map_err(|e| Error::failed("chmod", e))?;
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(|e| Error::failed("write/fsync", e))?;
            renameat(&self.0, temp.as_str(), &self.0, name)
                .map_err(|e| Error::failed("rename", e))?;
            self.0
                .sync_all()
                .map_err(|e| Error::failed("directory fsync", e))
        })();
        let _ = unlinkat(&self.0, temp.as_str(), AtFlags::empty());
        result
    }
    /// `atomic`, unless the file already is exactly this: same bytes, owner,
    /// group and mode. Replacing a file costs two fsyncs; a render that
    /// rewrites every artifact for a change of one made each write take
    /// 300 ms on a small board. Returns whether the file was written.
    pub fn atomic_if_changed(
        &self,
        name: &str,
        bytes: &[u8],
        uid: u32,
        gid: u32,
        mode: u32,
    ) -> Result<bool, Error> {
        if let Ok(file) = self.open(name) {
            if let Ok(m) = file.metadata() {
                if m.is_file()
                    && m.uid() == uid
                    && m.gid() == gid
                    && m.mode() & 0o7777 == mode
                    && m.len() == bytes.len() as u64
                {
                    let mut current = Vec::with_capacity(bytes.len());
                    if file
                        .take(bytes.len() as u64 + 1)
                        .read_to_end(&mut current)
                        .is_ok()
                        && current == bytes
                    {
                        return Ok(false);
                    }
                }
            }
        }
        self.atomic(name, bytes, uid, gid, mode)?;
        Ok(true)
    }
    pub fn regular_target(&self, name: &str) -> Result<(), Error> {
        match rustix::fs::statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) if st.st_mode & 0o170000 == 0o100000 => Ok(()),
            Ok(_) => Err(Error::PermissionDenied),
            Err(e) if e == rustix::io::Errno::NOENT => Ok(()),
            Err(e) => Err(Error::failed("stat target", e)),
        }
    }
    pub fn root_owned_target(&self, name: &str) -> Result<(), Error> {
        component(name)?;
        match rustix::fs::statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st)
                if st.st_mode & 0o170000 == 0o100000
                    && st.st_uid == nix::unistd::geteuid().as_raw() =>
            {
                Ok(())
            }
            Ok(_) => Err(Error::PermissionDenied),
            Err(e) if e == rustix::io::Errno::NOENT => Ok(()),
            Err(e) => Err(Error::failed("stat root target", e)),
        }
    }
    pub fn remove(&self, name: &str) -> Result<(), Error> {
        component(name)?;
        self.regular_target(name)?;
        match unlinkat(&self.0, name, AtFlags::empty()) {
            Ok(()) => (),
            Err(e) if e == rustix::io::Errno::NOENT => (),
            Err(e) => return Err(Error::failed("unlink", e)),
        }
        self.0
            .sync_all()
            .map_err(|e| Error::failed("directory fsync", e))
    }
    pub fn lock(&self, name: &str) -> Result<File, Error> {
        self.trusted()?;
        component(name)?;
        let fd = openat(
            &self.0,
            name,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|e| Error::failed("open lock", e))?;
        let file = File::from(fd);
        let m = file.metadata().map_err(|e| Error::failed("stat lock", e))?;
        if !m.is_file() || m.uid() != nix::unistd::geteuid().as_raw() || m.mode() & 0o022 != 0 {
            return Err(Error::PermissionDenied);
        }
        file.lock_exclusive()
            .map_err(|e| Error::failed("lock", e))?;
        Ok(file)
    }
    pub fn try_lock(&self, name: &str) -> Result<File, Error> {
        self.trusted()?;
        component(name)?;
        let fd = openat(
            &self.0,
            name,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|e| Error::failed("open lock", e))?;
        let file = File::from(fd);
        let m = file.metadata().map_err(|e| Error::failed("stat lock", e))?;
        if !m.is_file() || m.uid() != nix::unistd::geteuid().as_raw() || m.mode() & 0o077 != 0 {
            return Err(Error::PermissionDenied);
        }
        FileExt::try_lock_exclusive(&file).map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                Error::Busy
            } else {
                Error::failed("lock", e)
            }
        })?;
        Ok(file)
    }
    pub fn path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.0.as_raw_fd()))
    }
}
fn component(s: &str) -> Result<(), Error> {
    if s.is_empty() || s == "." || s == ".." || s.contains('/') || s.contains('\0') {
        Err(Error::invalid("component", "invalid basename"))
    } else {
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct Root(pub PathBuf);
impl Root {
    pub fn production() -> Self {
        Self("/".into())
    }
    pub fn path(&self, path: &str) -> PathBuf {
        self.0.join(path.trim_start_matches('/'))
    }
    pub fn directory(&self, path: &str, create: bool, trusted: bool) -> Result<Directory, Error> {
        let root = rustix::fs::open(
            &self.0,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| Error::failed("open root", e))?;
        let mut dir = Directory(File::from(root));
        if trusted {
            dir.trusted()?;
        }
        let mut walked = String::new();
        for part in path.trim_matches('/').split('/').filter(|s| !s.is_empty()) {
            dir = dir.child(part, create)?;
            walked.push('/');
            walked.push_str(part);
            // The uploader owns /var/lib/ndimon. Pin and check its registry
            // child on both archive and state traversal (legacy check_state_parent).
            if trusted || walked == "/var/lib/ndimon/plugins" {
                dir.trusted()?;
            }
        }
        Ok(dir)
    }
    pub fn parent(
        &self,
        path: &str,
        create: bool,
        trusted: bool,
    ) -> Result<(Directory, String), Error> {
        let p = Path::new(path);
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::invalid("path", "missing basename"))?;
        Ok((
            self.directory(
                p.parent().and_then(|p| p.to_str()).unwrap_or("/"),
                create,
                trusted,
            )?,
            name.into(),
        ))
    }
    pub fn atomic(&self, path: &str, bytes: &[u8], gid: u32, mode: u32) -> Result<(), Error> {
        let (dir, name) = self.parent(path, true, true)?;
        dir.atomic(&name, bytes, nix::unistd::geteuid().as_raw(), gid, mode)
    }
    /// `atomic` for a file that may already be exactly this; see `Directory`.
    pub fn atomic_if_changed(
        &self,
        path: &str,
        bytes: &[u8],
        gid: u32,
        mode: u32,
    ) -> Result<bool, Error> {
        let (dir, name) = self.parent(path, true, true)?;
        dir.atomic_if_changed(&name, bytes, nix::unistd::geteuid().as_raw(), gid, mode)
    }
    pub fn read(&self, path: &str, max: usize) -> Result<Option<Vec<u8>>, Error> {
        let path = Path::new(path);
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| Error::invalid("path", "missing basename"))?;
        let parent = path.parent().and_then(|p| p.to_str()).unwrap_or("/");
        let mut dir = self.directory("/", false, false)?;
        let mut walked = String::new();
        for part in parent
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
        {
            component(part)?;
            let fd = match openat(
                &dir.0,
                part,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => fd,
                Err(e) if e == rustix::io::Errno::NOENT => return Ok(None),
                Err(e) => return Err(Error::failed("open directory", e)),
            };
            dir = Directory(File::from(fd));
            walked.push('/');
            walked.push_str(part);
            if walked == "/var/lib/ndimon/plugins" {
                dir.trusted()?;
            }
        }
        dir.read(name, max)
    }
    pub fn remove(&self, path: &str) -> Result<(), Error> {
        let (dir, name) = self.parent(path, false, true)?;
        dir.remove(&name)
    }
}
pub fn io_error(e: std::io::Error) -> Error {
    Error::failed("filesystem", e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn an_unchanged_file_is_not_replaced() {
        let temp = tempfile::Builder::new()
            .permissions(Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let root = Root(temp.path().into());
        let gid = nix::unistd::getegid().as_raw();
        let path = temp.path().join("etc/a.json");
        let inode = || fs::metadata(&path).unwrap().ino();
        let write = |bytes: &[u8], mode| root.atomic_if_changed("/etc/a.json", bytes, gid, mode);
        assert!(write(b"one", 0o640).unwrap(), "a new file is written");
        let first = inode();
        assert!(!write(b"one", 0o640).unwrap());
        assert_eq!(inode(), first, "the same file, not a copy");
        // Another content, another length, another mode: each replaces it.
        assert!(write(b"two", 0o640).unwrap());
        assert_ne!(inode(), first);
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert!(write(b"three", 0o640).unwrap());
        assert!(write(b"three", 0o600).unwrap());
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
        let kept = inode();
        assert!(!write(b"three", 0o600).unwrap());
        assert_eq!(inode(), kept);
        // Drift of the mode is corrected even though the bytes agree.
        fs::set_permissions(&path, Permissions::from_mode(0o666)).unwrap();
        assert!(write(b"three", 0o600).unwrap());
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
        // A symlink in its place is never followed or kept.
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", &path).unwrap();
        assert!(write(b"three", 0o600).is_err());
    }
    #[test]
    fn optional_read_handles_missing_ancestors_but_rejects_links() {
        let temp = tempfile::Builder::new()
            .permissions(Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let root = Root(temp.path().into());
        assert_eq!(
            root.read("/var/lib/ndimon/state/state.json", 1024).unwrap(),
            None
        );
        std::os::unix::fs::symlink("/tmp", temp.path().join("var")).unwrap();
        assert!(root.read("/var/lib/ndimon/state/state.json", 1024).is_err());
    }
    #[test]
    fn durable_writer_and_links() {
        let t = tempfile::Builder::new()
            .permissions(Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let root = Root(t.path().into());
        let gid = nix::unistd::getegid().as_raw();
        root.atomic("/etc/test", b"{\"a\":1}", gid, 0o640).unwrap();
        assert_eq!(fs::read(t.path().join("etc/test")).unwrap(), b"{\"a\":1}");
        assert_eq!(
            fs::metadata(t.path().join("etc/test")).unwrap().mode() & 0o777,
            0o640
        );
        std::os::unix::fs::symlink("test", t.path().join("etc/link")).unwrap();
        assert!(root.atomic("/etc/link", b"bad", gid, 0o640).is_err());
    }
    #[test]
    fn trusted_writes_reject_writable_roots_and_ancestors() {
        let t = tempfile::Builder::new()
            .permissions(Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let root = Root(t.path().into());
        let gid = nix::unistd::getegid().as_raw();
        root.directory("/etc", true, true).unwrap();
        fs::set_permissions(t.path().join("etc"), Permissions::from_mode(0o775)).unwrap();
        assert!(matches!(
            root.atomic("/etc/test", b"bad", gid, 0o640),
            Err(Error::PermissionDenied)
        ));
        fs::set_permissions(t.path().join("etc"), Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(t.path(), Permissions::from_mode(0o775)).unwrap();
        assert!(matches!(
            root.atomic("/etc/test", b"bad", gid, 0o640),
            Err(Error::PermissionDenied)
        ));
    }
    #[test]
    fn registry_parent_is_checked_even_below_writable_ancestor() {
        let t = tempfile::Builder::new()
            .permissions(Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let root = Root(t.path().into());
        fs::create_dir_all(t.path().join("var/lib/ndimon/plugins/downloads")).unwrap();
        fs::set_permissions(
            t.path().join("var/lib/ndimon/plugins"),
            Permissions::from_mode(0o777),
        )
        .unwrap();
        assert!(
            root.directory("/var/lib/ndimon/plugins/downloads", false, false)
                .is_err()
        );
    }
}
