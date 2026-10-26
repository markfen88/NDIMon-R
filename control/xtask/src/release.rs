//! Build-host signing tasks. No credentials are passed to subprocesses/logs.
use crate::Result;
use ndimon_release::{MAX_MANIFEST_BYTES, MAX_SIGNATURE_BYTES, Manifest};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

fn value(args: &[String], flag: &str) -> Result<String> {
    let indices: Vec<_> = args
        .iter()
        .enumerate()
        .filter(|(_, s)| s.as_str() == flag)
        .map(|(i, _)| i)
        .collect();
    if indices.len() != 1 {
        return Err(format!("expected exactly one {flag}").into());
    }
    args.get(indices[0] + 1)
        .filter(|s| !s.starts_with("--"))
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value").into())
}
fn password() -> Result<String> {
    std::env::var("NDIMON_RELEASE_KEY_PASSWORD")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "NDIMON_RELEASE_KEY_PASSWORD must be set and nonempty".into())
}
fn exclusive(path: &Path, mode: u32) -> Result<File> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    Ok(file)
}
fn read_bounded(path: &Path, max: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err("release metadata exceeds limit".into());
    }
    Ok(bytes)
}

/// Explicit input files avoid accidentally publishing source, tests or keys.
pub fn release(args: &[String]) -> Result {
    let key_path =
        std::env::var_os("NDIMON_RELEASE_KEY_FILE").ok_or("NDIMON_RELEASE_KEY_FILE must be set")?;
    let password = password()?;
    let version = value(args, "--version")?;
    let comment = ndimon_release::trusted_comment(&version)?;
    let output = PathBuf::from(value(args, "--out")?);
    let mut inputs = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--version" | "--out" => i += 2,
            flag if flag.starts_with('-') => return Err("unknown release option".into()),
            path => {
                inputs.push(PathBuf::from(path));
                i += 1;
            }
        }
    }
    if inputs.is_empty() {
        return Err("release requires explicit built artifact files".into());
    }
    let key_info =
        fs::symlink_metadata(&key_path).map_err(|_| "cannot inspect release signing key")?;
    if !key_info.is_file() || key_info.permissions().mode() & 0o077 != 0 {
        return Err("release signing key must be a private regular file (0600)".into());
    }
    let secret = minisign::SecretKey::from_file(&key_path, Some(password))
        .map_err(|_| "cannot unlock release signing key")?;
    let mut names = BTreeSet::new();
    let canonical_key =
        fs::canonicalize(&key_path).map_err(|_| "cannot inspect release signing key")?;
    for path in &inputs {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("artifact filename is not UTF-8")?;
        if !ndimon_release::valid_filename(name) || !names.insert(name.to_owned()) {
            return Err("invalid or duplicate artifact filename".into());
        }
        if !fs::symlink_metadata(path)?.is_file() {
            return Err("artifacts must be regular files".into());
        }
        if fs::canonicalize(path)? == canonical_key {
            return Err("refusing to publish signing key".into());
        }
    }
    fs::create_dir_all(&output)?;
    if !fs::symlink_metadata(&output)?.is_dir() || fs::read_dir(&output)?.next().is_some() {
        return Err("release output must be an empty directory".into());
    }
    inputs.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    let mut sums = String::new();
    for path in inputs {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("invalid filename")?;
        let mut input = File::open(&path)?;
        let mut out = exclusive(&output.join(name), 0o644)?;
        let mut hash = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            out.write_all(&buffer[..count])?;
        }
        out.sync_all()?;
        sums.push_str(&format!("{:x}  {name}\n", hash.finalize()));
    }
    Manifest::parse(sums.as_bytes())?;
    let signature = minisign::sign(
        None,
        &secret,
        Cursor::new(sums.as_bytes()),
        Some(&comment),
        None,
    )
    .map_err(|_| "release signing failed")?;
    let mut file = exclusive(&output.join("SHA256SUMS"), 0o644)?;
    file.write_all(sums.as_bytes())?;
    file.sync_all()?;
    let mut file = exclusive(&output.join("SHA256SUMS.minisig"), 0o644)?;
    file.write_all(signature.to_string().as_bytes())?;
    file.sync_all()?;
    File::open(&output)?.sync_all()?;
    println!("signed ndimon-r {version}: {} artifacts", names.len());
    Ok(())
}

pub fn verify(args: &[String]) -> Result {
    if args.len() != 6 {
        return Err(
            "usage: cargo xtask verify-release --dir DIR --version VERSION --public-key FILE"
                .into(),
        );
    }
    let directory = PathBuf::from(value(args, "--dir")?);
    let version = value(args, "--version")?;
    let public = fs::read_to_string(value(args, "--public-key")?)?;
    let key = if public.starts_with("untrusted comment: ") {
        minisign_verify_key(&public)?
    } else {
        public.trim().into()
    };
    let sums = read_bounded(&directory.join("SHA256SUMS"), MAX_MANIFEST_BYTES)?;
    let signature = String::from_utf8(read_bounded(
        &directory.join("SHA256SUMS.minisig"),
        MAX_SIGNATURE_BYTES,
    )?)?;
    let manifest = ndimon_release::verify_manifest(&sums, &signature, &[key], &version)?;
    for name in manifest.names() {
        manifest.verify_file(&directory, name)?;
    }
    println!("verified ndimon-r {version}");
    Ok(())
}
fn minisign_verify_key(text: &str) -> Result<String> {
    let lines: Vec<_> = text.lines().collect();
    if lines.len() != 2 {
        return Err("expected minisign public key file".into());
    }
    Ok(lines[1].into())
}

/// Exercise real package bytes with an ephemeral in-memory key. This never
/// creates project trust, contacts a release host, or uses the owner's key.
pub fn smoke(args: &[String]) -> Result {
    if args.len() != 4 {
        return Err("release-smoke --dir PACKAGE_DIRECTORY --version VERSION".into());
    }
    let directory = PathBuf::from(value(args, "--dir")?);
    let version = value(args, "--version")?;
    let mut files: Vec<_> = fs::read_dir(&directory)?
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "deb"))
        .collect();
    files.sort();
    if files.len() != crate::package::PACKAGES.len() {
        return Err("smoke requires exactly one complete architecture package set".into());
    }
    // Network changes are refused without the certification list: the privd
    // package must carry it as a conffile.
    let privd = files
        .iter()
        .find(|file| {
            file.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|name| name.starts_with("ndimon-privd_"))
        })
        .ok_or("ndimon-privd package missing")?;
    crate::package::verify_privd_conffiles(privd)?;
    let mut sums = String::new();
    for file in &files {
        let name = file
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("artifact filename")?;
        sums.push_str(&format!("{:x}  {name}\n", Sha256::digest(fs::read(file)?)));
    }
    let key = minisign::KeyPair::generate_unencrypted_keypair()
        .map_err(|_| "ephemeral signing key failed")?;
    let signature = minisign::sign(
        Some(&key.pk),
        &key.sk,
        Cursor::new(sums.as_bytes()),
        Some(&ndimon_release::trusted_comment(&version)?),
        None,
    )
    .map_err(|_| "smoke signing failed")?
    .to_string();
    let manifest = ndimon_release::verify_manifest(
        sums.as_bytes(),
        &signature,
        &[key.pk.to_base64()],
        &version,
    )?;
    for file in &files {
        manifest.verify_file(
            &directory,
            file.file_name()
                .and_then(|s| s.to_str())
                .ok_or("filename")?,
        )?;
    }
    let mut tampered = sums.into_bytes();
    tampered[0] = if tampered[0] == b'0' { b'1' } else { b'0' };
    if ndimon_release::verify_manifest(&tampered, &signature, &[key.pk.to_base64()], &version)
        .is_ok()
    {
        return Err("tampered manifest accepted".into());
    }
    println!(
        "release smoke: {} real packages signed and verified; tampering refused",
        files.len()
    );
    Ok(())
}

/// Owner-only operation. Implementation is shipped; never invoked by CI or an
/// implementation worker. Refuse existing directories/files to prevent loss.
pub fn keygen(args: &[String]) -> Result {
    if args.len() != 2 {
        return Err("usage: cargo xtask release-keygen --out NEW_PRIVATE_DIRECTORY".into());
    }
    let output = PathBuf::from(value(args, "--out")?);
    let password = password()?;
    fs::DirBuilder::new().mode(0o700).create(&output)?;
    fs::set_permissions(&output, fs::Permissions::from_mode(0o700))?;
    let mut secret = exclusive(&output.join("ndimon-release.key"), 0o600)?;
    let mut public = exclusive(&output.join("ndimon-release.pub"), 0o644)?;
    let pair = minisign::KeyPair::generate_and_write_encrypted_keypair(
        &mut public,
        &mut secret,
        None,
        Some(password),
    )
    .map_err(|_| "key generation failed")?;
    secret.sync_all()?;
    public.sync_all()?;
    File::open(&output)?.sync_all()?;
    println!("{}", pair.pk.to_base64());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn options_refuse_missing_and_duplicate_values() {
        for input in [
            vec![],
            vec!["--version"],
            vec!["--version", "--out"],
            vec!["--version", "1", "--version", "2"],
        ] {
            let args: Vec<String> = input.into_iter().map(String::from).collect();
            assert!(value(&args, "--version").is_err());
        }
    }
}
