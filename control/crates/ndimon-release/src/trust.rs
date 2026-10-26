//! Public configuration only: pin every ancestor and refuse unsafe ownership.
use crate::{Error, TRUSTED_KEYS};
use rustix::fs::{Mode, OFlags, open, openat};
use std::{
    fs::File,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Component, Path},
};

pub const OVERRIDE_FILE: &str = "/etc/ndimon/trusted-release-keys";

pub fn validate_metadata(uid: u32, mode: u32, regular: bool) -> Result<(), Error> {
    if uid != 0 || mode & 0o7777 != 0o644 || !regular {
        return Err(Error::malformed(
            "public configuration must be a root-owned regular file with mode 0644",
        ));
    }
    Ok(())
}

/// Read a small, root-owned 0644 file without following symlinks. A missing
/// final component is optional; an unsafe ancestor is always an error.
pub fn read_config(path: &Path, limit: usize) -> Result<Option<String>, Error> {
    if !path.is_absolute() {
        return Err(Error::malformed("configuration path must be absolute"));
    }
    let mut dir = File::from(
        open(
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| Error::malformed("cannot open root"))?,
    );
    let mut parts = path
        .components()
        .filter(|c| *c != Component::RootDir)
        .peekable();
    while let Some(part) = parts.next() {
        let Component::Normal(part) = part else {
            return Err(Error::malformed("invalid configuration path"));
        };
        let m = dir
            .metadata()
            .map_err(|_| Error::malformed("cannot inspect configuration parent"))?;
        if m.uid() != 0 || m.mode() & 0o022 != 0 {
            return Err(Error::malformed("unsafe configuration parent"));
        }
        let last = parts.peek().is_none();
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let fd = match openat(
            &dir,
            part,
            if last {
                flags
            } else {
                flags | OFlags::DIRECTORY
            },
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(_) => return Err(Error::malformed("cannot open public configuration")),
        };
        let file = File::from(fd);
        if !last {
            dir = file;
            continue;
        }
        let m = file
            .metadata()
            .map_err(|_| Error::malformed("cannot inspect public configuration"))?;
        validate_metadata(m.uid(), m.mode(), m.is_file())?;
        if m.len() > limit as u64 {
            return Err(Error::malformed("public configuration exceeds limit"));
        }
        let mut text = String::new();
        file.take(limit as u64 + 1)
            .read_to_string(&mut text)
            .map_err(|_| Error::malformed("cannot read public configuration"))?;
        if text.len() > limit {
            return Err(Error::malformed("public configuration exceeds limit"));
        }
        return Ok(Some(text));
    }
    Err(Error::malformed("configuration file missing"))
}

pub fn load_keys(path: &Path) -> Result<Vec<String>, Error> {
    let mut keys: Vec<String> = TRUSTED_KEYS.iter().map(|s| (*s).into()).collect();
    if let Some(text) = read_config(path, 16 * 1024)? {
        for line in text.lines().filter(|s| !s.is_empty()) {
            minisign_verify::PublicKey::from_base64(line)
                .map_err(|_| Error::malformed("invalid override public key"))?;
            if !keys.iter().any(|k| k == line) {
                keys.push(line.into());
            }
        }
    }
    Ok(keys)
}
