//! Authenticate the manifest before trusting its names or checksums.
use minisign_verify::{PublicKey, Signature};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs::File, io::Read, path::Path};

pub mod trust;

/// A8/Q8: the owner provisions the first key. Never generate a project key here.
pub const TRUSTED_KEYS: &[&str] = &[];
pub const PRODUCT: &str = "ndimon-r";
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
pub const MAX_SIGNATURE_BYTES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("untrusted_key")]
    UntrustedKey,
    #[error("bad_signature")]
    BadSignature,
    #[error("wrong_product")]
    WrongProduct,
    #[error("wrong_version")]
    WrongVersion,
    #[error("missing_file: {name}")]
    MissingFile { name: String },
    #[error("checksum_mismatch: {name}")]
    ChecksumMismatch { name: String },
    #[error("malformed: {reason}")]
    Malformed { reason: String },
}
impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UntrustedKey => "untrusted_key",
            Self::BadSignature => "bad_signature",
            Self::WrongProduct => "wrong_product",
            Self::WrongVersion => "wrong_version",
            Self::MissingFile { .. } => "missing_file",
            Self::ChecksumMismatch { .. } => "checksum_mismatch",
            Self::Malformed { .. } => "malformed",
        }
    }
    pub fn malformed(reason: impl Into<String>) -> Self {
        Self::Malformed {
            reason: reason.into(),
        }
    }
}

/// Release assets are flat basenames. Refuse traversal, controls, GNU escaped
/// names and platform-dependent separators, rather than reinterpret them.
pub fn valid_filename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-~ ".contains(&b))
        && !name.starts_with(['.', '-', ' '])
        && !name.ends_with(' ')
        && name != "SHA256SUMS"
        && name != "SHA256SUMS.minisig"
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    entries: BTreeMap<String, [u8; 32]>,
}
impl Manifest {
    /// Strict GNU coreutils SHA-256 records: 64 hex digits, space, text/binary
    /// marker, safe basename, LF. No comments, blank lines or duplicate names.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.is_empty() || bytes.len() > MAX_MANIFEST_BYTES || !bytes.ends_with(b"\n") {
            return Err(Error::malformed(
                "empty, oversized or unterminated SHA256SUMS",
            ));
        }
        let text =
            std::str::from_utf8(bytes).map_err(|_| Error::malformed("SHA256SUMS is not UTF-8"))?;
        let mut entries = BTreeMap::new();
        for line in text.split_terminator('\n') {
            let b = line.as_bytes();
            if b.len() < 67
                || b[64] != b' '
                || !matches!(b[65], b' ' | b'*')
                || !b[..64].iter().all(u8::is_ascii_hexdigit)
            {
                return Err(Error::malformed("invalid GNU SHA256SUMS record"));
            }
            let name = &line[66..];
            if !valid_filename(name) {
                return Err(Error::malformed("unsafe release filename"));
            }
            let mut digest = [0; 32];
            for (i, byte) in digest.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&line[i * 2..i * 2 + 2], 16)
                    .map_err(|_| Error::malformed("invalid digest"))?;
            }
            if entries.insert(name.into(), digest).is_some() {
                return Err(Error::malformed("duplicate release filename"));
            }
        }
        Ok(Self { entries })
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }
    pub fn verify_reader(&self, name: &str, mut reader: impl Read) -> Result<(), Error> {
        let expected = self
            .entries
            .get(name)
            .ok_or_else(|| Error::MissingFile { name: name.into() })?;
        let mut hasher = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|_| Error::malformed("cannot read release file"))?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        let actual: [u8; 32] = hasher.finalize().into();
        if &actual != expected {
            return Err(Error::ChecksumMismatch { name: name.into() });
        }
        Ok(())
    }
    pub fn verify_file(&self, directory: &Path, name: &str) -> Result<(), Error> {
        if !self.entries.contains_key(name) {
            return Err(Error::MissingFile { name: name.into() });
        }
        let path = directory.join(name);
        use rustix::fs::{Mode, OFlags, open};
        let fd = open(
            &path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| {
            if error == rustix::io::Errno::NOENT {
                Error::MissingFile { name: name.into() }
            } else {
                Error::malformed("cannot open regular release asset")
            }
        })?;
        let file = File::from(fd);
        if !file
            .metadata()
            .map_err(|_| Error::malformed("cannot inspect release asset"))?
            .is_file()
        {
            return Err(Error::malformed("release asset is not a regular file"));
        }
        self.verify_reader(name, file)
    }
}

/// Canonical shared trusted-comment vocabulary; order is fixed for auditing.
pub fn trusted_comment(version: &str) -> Result<String, Error> {
    if version.is_empty()
        || version.len() > 128
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".+:~-".contains(&b))
    {
        return Err(Error::malformed("invalid release version"));
    }
    Ok(format!(
        "product:{PRODUCT}\tversion:{version}\tfile:SHA256SUMS"
    ))
}

pub fn verify_manifest(
    bytes: &[u8],
    signature: &str,
    keys: &[String],
    version: &str,
) -> Result<Manifest, Error> {
    // Empty trust always fails closed, including on otherwise malformed input.
    if keys.is_empty() {
        return Err(Error::UntrustedKey);
    }
    if bytes.len() > MAX_MANIFEST_BYTES
        || signature.len() > MAX_SIGNATURE_BYTES
        || signature.lines().count() != 4
    {
        return Err(Error::malformed(
            "oversized manifest or malformed signature envelope",
        ));
    }
    let signature =
        Signature::decode(signature).map_err(|_| Error::malformed("invalid minisign signature"))?;
    let mut matching_key = false;
    let mut verified = false;
    for key in keys {
        let key = PublicKey::from_base64(key)
            .map_err(|_| Error::malformed("invalid trusted public key"))?;
        // Contract §10: the prehashed ED variant only (Tessera enforces the same).
        match key.verify(bytes, &signature, false) {
            Ok(()) => {
                verified = true;
                break;
            }
            Err(minisign_verify::Error::UnexpectedKeyId) => (),
            Err(_) => matching_key = true,
        }
    }
    if !verified {
        return Err(if matching_key {
            Error::BadSignature
        } else {
            Error::UntrustedKey
        });
    }
    let fields: Vec<_> = signature.trusted_comment().split('\t').collect();
    if fields.len() != 3 {
        return Err(Error::malformed(
            "expected product, version and file in trusted comment",
        ));
    }
    if fields[0] != "product:ndimon-r" {
        return Err(Error::WrongProduct);
    }
    if fields[1] != format!("version:{version}") {
        return Err(Error::WrongVersion);
    }
    if fields[2] != "file:SHA256SUMS" {
        return Err(Error::malformed("trusted comment must name SHA256SUMS"));
    }
    trusted_comment(version)?;
    Manifest::parse(bytes)
}
