//! The pinned Bootstrap and Bootstrap Icons files the page loads. The lockfile
//! (`api/package-lock.json`) names each package's tarball and its sha512
//! `integrity`; both are fetched, verified and unpacked here, so a device that
//! builds its own packages needs neither npm, Node nor a `node_modules` tree.
use crate::{Result, install};
use base64::Engine;
use flate2::read::GzDecoder;
use sha2::{Digest, Sha512};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// Each package, and the files of its tarball the page loads, with where they
/// go below `api/public/vendor/<package>/`. Nothing else is extracted.
const PINNED: &[(&str, &[(&str, &str)])] = &[
    (
        "bootstrap",
        &[
            (
                "package/dist/css/bootstrap.min.css",
                "css/bootstrap.min.css",
            ),
            (
                "package/dist/js/bootstrap.bundle.min.js",
                "js/bootstrap.bundle.min.js",
            ),
            ("package/LICENSE", "LICENSE"),
        ],
    ),
    (
        "bootstrap-icons",
        &[
            (
                "package/font/bootstrap-icons.min.css",
                "font/bootstrap-icons.min.css",
            ),
            (
                "package/font/fonts/bootstrap-icons.woff2",
                "font/fonts/bootstrap-icons.woff2",
            ),
            (
                "package/font/fonts/bootstrap-icons.woff",
                "font/fonts/bootstrap-icons.woff",
            ),
            ("package/LICENSE", "LICENSE"),
        ],
    ),
];
/// Far above the real tarballs (about 3 MiB), far below anything abusive.
const MAX_TARBALL: usize = 32 * 1024 * 1024;
const MAX_FILE: u64 = 16 * 1024 * 1024;

struct Lock {
    url: String,
    integrity: String,
}
/// The tarball URL and integrity the lockfile records for `package`.
fn locked(lock: &serde_json::Value, package: &str) -> Result<Lock> {
    let entry = &lock["packages"][format!("node_modules/{package}")];
    let version = entry["version"].as_str().ok_or("lockfile version")?;
    let url = entry["resolved"].as_str().ok_or("lockfile URL")?;
    let integrity = entry["integrity"].as_str().ok_or("lockfile integrity")?;
    // Only the registry's own tarball for exactly this version.
    if url != format!("https://registry.npmjs.org/{package}/-/{package}-{version}.tgz") {
        return Err(format!("{package}: lockfile URL is not the registry tarball").into());
    }
    if !integrity.starts_with("sha512-") || integrity.contains(' ') {
        return Err(format!("{package}: lockfile integrity is not a single sha512").into());
    }
    Ok(Lock {
        url: url.to_owned(),
        integrity: integrity.to_owned(),
    })
}
/// Subresource-integrity form: `sha512-` and the base64 digest.
fn verify(package: &str, bytes: &[u8], integrity: &str) -> Result {
    let digest = base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes));
    if format!("sha512-{digest}") != integrity {
        return Err(format!("{package}: tarball does not match the lockfile integrity").into());
    }
    Ok(())
}
/// Read exactly the wanted members; every other member is skipped unread.
fn unpack(
    package: &str,
    tarball: &[u8],
    wanted: &[(&str, &str)],
    target: &Path,
) -> Result<Vec<PathBuf>> {
    let mut found: Vec<Option<Vec<u8>>> = vec![None; wanted.len()];
    let mut archive = tar::Archive::new(GzDecoder::new(tarball));
    for entry in archive.entries()? {
        let entry = entry?;
        let name = entry.path()?.to_string_lossy().into_owned();
        let Some(index) = wanted.iter().position(|(member, _)| *member == name) else {
            continue;
        };
        if !entry.header().entry_type().is_file() || entry.size() > MAX_FILE {
            return Err(format!("{package}: {name} is not a plain file").into());
        }
        if found[index].is_some() {
            return Err(format!("{package}: {name} appears twice").into());
        }
        let mut bytes = Vec::new();
        entry.take(MAX_FILE).read_to_end(&mut bytes)?;
        found[index] = Some(bytes);
    }
    let mut written = Vec::new();
    for ((member, relative), bytes) in wanted.iter().zip(found) {
        let bytes = bytes.ok_or_else(|| format!("{package}: {member} missing from the tarball"))?;
        let destination = target.join(relative);
        fs::create_dir_all(destination.parent().ok_or("vendor path")?)?;
        fs::write(&destination, bytes)?;
        written.push(destination);
    }
    Ok(written)
}
/// Fetch, verify and unpack every pinned package. `fetch` returns a tarball's
/// bytes; the real one is curl over https.
fn build_with(root: &Path, fetch: &dyn Fn(&str) -> Result<Vec<u8>>) -> Result {
    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("api/package-lock.json"))?)?;
    let output = root.join("api/public/vendor");
    // Verify everything before touching the tree: a bad download leaves the
    // previous files in place.
    let mut verified = Vec::new();
    for (package, wanted) in PINNED {
        let pin = locked(&lock, package)?;
        let tarball = fetch(&pin.url)?;
        verify(package, &tarball, &pin.integrity)?;
        verified.push((*package, *wanted, tarball));
    }
    for (package, wanted, tarball) in verified {
        let target = output.join(package);
        if target.exists() {
            fs::remove_dir_all(&target)?;
        }
        unpack(package, &tarball, wanted, &target)?;
    }
    Ok(())
}
fn download(url: &str) -> Result<Vec<u8>> {
    let temporary = tempfile::tempdir()?;
    let file = temporary.path().join("package.tgz");
    install::download(url, &file)?;
    let bytes = fs::read(&file)?;
    if bytes.len() > MAX_TARBALL {
        return Err("vendor tarball is larger than expected".into());
    }
    Ok(bytes)
}

pub fn build(root: &Path) -> Result {
    build_with(root, &download)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tarball(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        ));
        for (name, bytes) in members {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append_data(&mut header, name, *bytes).unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap()
    }
    fn integrity(bytes: &[u8]) -> String {
        format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes))
        )
    }
    fn sets() -> (Vec<u8>, Vec<u8>) {
        (
            tarball(&[
                ("package/package.json", b"{}"),
                ("package/LICENSE", b"MIT bootstrap"),
                ("package/dist/css/bootstrap.min.css", b"css"),
                ("package/dist/css/bootstrap.css", b"unminified, unused"),
                ("package/dist/css/bootstrap.min.css.map", b"map"),
                ("package/dist/js/bootstrap.bundle.min.js", b"js"),
                ("package/dist/js/bootstrap.js", b"unused"),
            ]),
            tarball(&[
                ("package/LICENSE", b"MIT icons"),
                ("package/font/bootstrap-icons.min.css", b"icons css"),
                ("package/font/bootstrap-icons.json", b"unused"),
                ("package/font/fonts/bootstrap-icons.woff2", b"woff2"),
                ("package/font/fonts/bootstrap-icons.woff", b"woff"),
            ]),
        )
    }
    fn repository(root: &Path, bootstrap: &[u8], icons: &[u8]) {
        fs::create_dir_all(root.join("api")).unwrap();
        let lock = serde_json::json!({"lockfileVersion":3,"packages":{
            "node_modules/bootstrap":{"version":"5.3.8",
                "resolved":"https://registry.npmjs.org/bootstrap/-/bootstrap-5.3.8.tgz",
                "integrity":integrity(bootstrap)},
            "node_modules/bootstrap-icons":{"version":"1.13.2",
                "resolved":"https://registry.npmjs.org/bootstrap-icons/-/bootstrap-icons-1.13.2.tgz",
                "integrity":integrity(icons)},
        }});
        fs::write(root.join("api/package-lock.json"), lock.to_string()).unwrap();
    }
    fn serve<'a>(bootstrap: &'a [u8], icons: &'a [u8]) -> impl Fn(&str) -> Result<Vec<u8>> + 'a {
        move |url: &str| {
            if url.ends_with("/bootstrap-5.3.8.tgz") {
                Ok(bootstrap.to_vec())
            } else if url.ends_with("/bootstrap-icons-1.13.2.tgz") {
                Ok(icons.to_vec())
            } else {
                Err(format!("unexpected URL {url}").into())
            }
        }
    }

    #[test]
    fn only_the_files_the_page_loads_are_unpacked_from_the_verified_tarballs() {
        let temp = tempfile::tempdir().unwrap();
        let (bootstrap, icons) = sets();
        repository(temp.path(), &bootstrap, &icons);
        build_with(temp.path(), &serve(&bootstrap, &icons)).unwrap();
        let vendor = temp.path().join("api/public/vendor");
        let mut files = Vec::new();
        fn walk(path: &Path, files: &mut Vec<String>, base: &Path) {
            for entry in fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, files, base);
                } else {
                    files.push(path.strip_prefix(base).unwrap().display().to_string());
                }
            }
        }
        walk(&vendor, &mut files, &vendor);
        files.sort();
        assert_eq!(
            files,
            [
                "bootstrap-icons/LICENSE",
                "bootstrap-icons/font/bootstrap-icons.min.css",
                "bootstrap-icons/font/fonts/bootstrap-icons.woff",
                "bootstrap-icons/font/fonts/bootstrap-icons.woff2",
                "bootstrap/LICENSE",
                "bootstrap/css/bootstrap.min.css",
                "bootstrap/js/bootstrap.bundle.min.js",
            ]
        );
        assert_eq!(
            fs::read(vendor.join("bootstrap/css/bootstrap.min.css")).unwrap(),
            b"css"
        );
        assert_eq!(
            fs::read(vendor.join("bootstrap-icons/LICENSE")).unwrap(),
            b"MIT icons"
        );
        // A second run replaces the packages' trees.
        fs::write(vendor.join("bootstrap/css/stale.css"), "old").unwrap();
        build_with(temp.path(), &serve(&bootstrap, &icons)).unwrap();
        assert!(!vendor.join("bootstrap/css/stale.css").exists());
    }

    #[test]
    fn a_tarball_that_differs_by_one_byte_changes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let (bootstrap, icons) = sets();
        repository(temp.path(), &bootstrap, &icons);
        let vendor = temp.path().join("api/public/vendor/bootstrap/css");
        fs::create_dir_all(&vendor).unwrap();
        fs::write(vendor.join("bootstrap.min.css"), "previous").unwrap();
        let mut tampered = bootstrap.clone();
        *tampered.last_mut().unwrap() ^= 1;
        let error = build_with(temp.path(), &serve(&tampered, &icons)).unwrap_err();
        assert!(error.to_string().contains("integrity"), "{error}");
        // The second package is checked too, and nothing is written until both pass.
        let mut tampered = icons.clone();
        tampered[10] ^= 1;
        assert!(build_with(temp.path(), &serve(&bootstrap, &tampered)).is_err());
        assert_eq!(
            fs::read(vendor.join("bootstrap.min.css")).unwrap(),
            b"previous"
        );
    }

    #[test]
    fn a_missing_member_or_a_foreign_lockfile_url_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let (_, icons) = sets();
        let incomplete = tarball(&[("package/LICENSE", b"MIT")]);
        repository(temp.path(), &incomplete, &icons);
        let error = build_with(temp.path(), &serve(&incomplete, &icons)).unwrap_err();
        assert!(
            error.to_string().contains("missing from the tarball"),
            "{error}"
        );

        let lock: serde_json::Value = serde_json::json!({"packages":{"node_modules/bootstrap":{
            "version":"5.3.8","integrity":"sha512-AAAA",
            "resolved":"https://registry.example.test/bootstrap/-/bootstrap-5.3.8.tgz"}}});
        assert!(locked(&lock, "bootstrap").is_err());
        let lock = serde_json::json!({"packages":{"node_modules/bootstrap":{
            "version":"5.3.8","integrity":"sha1-AAAA",
            "resolved":"https://registry.npmjs.org/bootstrap/-/bootstrap-5.3.8.tgz"}}});
        assert!(locked(&lock, "bootstrap").is_err());
        let lock = serde_json::json!({"packages":{"node_modules/bootstrap":{
            "version":"5.3.8","integrity":"sha512-AAAA",
            "resolved":"http://registry.npmjs.org/bootstrap/-/bootstrap-5.3.8.tgz"}}});
        assert!(locked(&lock, "bootstrap").is_err());
    }

    #[test]
    fn a_member_that_is_not_a_plain_file_is_refused() {
        let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        archive
            .append_link(&mut header, "package/LICENSE", "/etc/passwd")
            .unwrap();
        let bytes = archive.into_inner().unwrap().finish().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let error = unpack(
            "bootstrap",
            &bytes,
            &[("package/LICENSE", "LICENSE")],
            temp.path(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("plain file"), "{error}");
    }

    #[test]
    fn the_repository_lockfile_pins_both_packages_to_registry_tarballs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let lock: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("api/package-lock.json")).unwrap()).unwrap();
        for (package, wanted) in PINNED {
            let pin = locked(&lock, package).unwrap();
            assert!(
                pin.url.starts_with("https://registry.npmjs.org/"),
                "{package}"
            );
            // `sha512-` and the 88 base64 characters of a 64-byte digest.
            assert_eq!(pin.integrity.len(), 7 + 88, "{package}");
            assert!(
                wanted
                    .iter()
                    .all(|(member, _)| member.starts_with("package/"))
            );
        }
    }
}
