//! Deterministic plugin transport; legacy archive bytes are frozen as fixtures.
use crate::Result;
use flate2::{Compression, GzBuilder, read::GzDecoder};
use ndimon_privd_proto::plugin::{ARCHIVE_ARCHITECTURES, archive_name, native_lifecycle};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

type Payload = BTreeMap<String, (Vec<u8>, u32)>;

#[derive(Debug)]
struct Options {
    plugin: String,
    bundle: Option<PathBuf>,
    output: PathBuf,
    release_tag: Option<String>,
    experimental: bool,
    /// The architecture the archive is named for; Tessera takes its bundle's.
    arch: Option<String>,
}

/// The Debian architecture this build host runs: a plugin that does not come
/// with a bundle is built for it unless `--arch` says otherwise.
fn host_architecture() -> Result<&'static str> {
    if cfg!(target_arch = "x86_64") {
        Ok("amd64")
    } else if cfg!(target_arch = "aarch64") {
        Ok("arm64")
    } else {
        Err("this build host is neither amd64 nor arm64: pass --arch".into())
    }
}

impl Options {
    fn parse(root: &Path, args: &[String]) -> Result<Self> {
        let mut options = Self {
            plugin: "tessera".into(),
            bundle: None,
            output: root.join("dist/plugins"),
            release_tag: None,
            experimental: false,
            arch: None,
        };
        let mut args = args.iter().peekable();
        if args.peek().is_some_and(|s| !s.starts_with('-')) {
            options.plugin = args.next().ok_or("missing plugin")?.clone();
        }
        while let Some(arg) = args.next() {
            let value = args.next().ok_or("option needs a value")?;
            match arg.as_str() {
                "--plugin" => options.plugin = value.clone(),
                "--bundle" => options.bundle = Some(value.into()),
                "--ubuntu-experimental-bundle" => {
                    options.bundle = Some(value.into());
                    options.experimental = true;
                }
                "--output" => options.output = value.into(),
                "--release-tag" => options.release_tag = Some(value.clone()),
                "--arch" => options.arch = Some(value.clone()),
                _ => return Err(format!("unknown build-plugin option: {arg}").into()),
            }
        }
        if !plugin_name(&options.plugin) {
            return Err("invalid plugin name".into());
        }
        // A release feed offers a device the archives named for its own
        // architecture and nothing else, so there is no `any`.
        if let Some(arch) = &options.arch {
            if options.plugin == "tessera" {
                return Err(
                    "--arch is not for Tessera: it takes the architecture of its bundle".into(),
                );
            }
            if !ARCHIVE_ARCHITECTURES.contains(&arch.as_str()) {
                return Err(format!(
                    "--arch must be {}: a feed matches the architecture of the device",
                    ARCHIVE_ARCHITECTURES.join(" or ")
                )
                .into());
            }
        }
        if options.experimental
            && !options
                .bundle
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .is_some_and(|s| {
                    s.starts_with("tessera-bundle_")
                        && s.ends_with("_arm64_ubuntu-noble-experimental.tar.gz")
                })
        {
            return Err("experimental Ubuntu noble arm64 bundle required".into());
        }
        Ok(options)
    }
}

pub fn build(root: &Path, args: &[String]) -> Result<PathBuf> {
    let options = Options::parse(root, args)?;
    let epoch = std::env::var("SOURCE_DATE_EPOCH")
        .unwrap_or_else(|_| "0".into())
        .parse::<u32>()?;
    build_with_epoch(root, &options, epoch)
}

fn build_with_epoch(root: &Path, options: &Options, epoch: u32) -> Result<PathBuf> {
    let source = root.join("plugins").join(&options.plugin);
    let manifest = read_json(&source.join("plugin.json"))?;
    let validated: ndimon_privd_proto::plugin::Manifest =
        serde_json::from_slice(&fs::read(source.join("plugin.json"))?)?;
    validated.validate(&options.plugin.clone().try_into()?, None)?;
    let version = manifest["version"]
        .as_str()
        .ok_or("missing plugin version")?;
    if manifest["name"] != options.plugin
        || !version.as_bytes().first().is_some_and(u8::is_ascii_digit)
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".+:~-".contains(&b))
    {
        return Err("invalid plugin manifest identity".into());
    }
    let mut payload = Payload::new();
    collect_source(&source, &source, &mut payload)?;
    for (field, default) in [
        ("routes", "routes.js"),
        ("ui", "ui.html"),
        ("install", "install.sh"),
        ("uninstall", "uninstall.sh"),
        ("backup", "backup.js"),
    ] {
        if validated.process.is_some() && field != "ui" {
            continue;
        }
        // First-party Tessera is a native domain with typed privd handlers: its
        // archive ships no lifecycle executables, router or backup hook.
        if native_lifecycle(&options.plugin)
            && matches!(field, "install" | "uninstall" | "routes" | "backup")
        {
            continue;
        }
        if field == "backup" && manifest.get(field).is_some_and(Value::is_null) {
            continue;
        }
        let filename = manifest
            .get(field)
            .map(|v| v.as_str())
            .unwrap_or(Some(default))
            .ok_or("invalid host entry filename")?;
        let (stem, ext) = filename
            .split_once('.')
            .ok_or("invalid host entry filename")?;
        if stem.is_empty()
            || ext.is_empty()
            || !stem
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || !ext.bytes().all(|b| b.is_ascii_alphanumeric())
            || !payload.contains_key(filename)
        {
            return Err(format!("missing or invalid host entry file: {field}").into());
        }
    }
    if let Some(process) = &validated.process {
        if !payload
            .get(&process.exec)
            .is_some_and(|(_, mode)| mode & 0o111 != 0)
        {
            return Err("missing process executable".into());
        }
    }
    if let Some(commands) = manifest
        .get("priv_commands")
        .filter(|_| !native_lifecycle(&options.plugin))
    {
        for command in commands
            .as_array()
            .ok_or("priv_commands must be an array")?
        {
            let command = command.as_str().ok_or("invalid privileged command")?;
            if !plugin_name(command)
                || !payload
                    .get(&format!("priv/{command}"))
                    .is_some_and(|(_, mode)| mode & 0o111 != 0)
            {
                return Err(
                    format!("privileged command must exist and be executable: {command}").into(),
                );
            }
        }
    }
    let arch: String;
    let mut platform = None;
    if options.plugin == "tessera" {
        let archive = fs::canonicalize(
            options
                .bundle
                .as_ref()
                .ok_or("Tessera requires --bundle with a pinned native runtime archive")?,
        )?;
        let filename = archive
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("invalid bundle filename")?;
        let bytes = fs::read(&archive)?;
        let digest = sha256(&bytes);
        let locks = read_json(&root.join("bundles.lock.json"))?;
        let matches: Vec<_> = locks["bundles"]
            .as_array()
            .ok_or("missing bundles list")?
            .iter()
            .filter(|entry| entry["sha256"] == digest && entry["asset"] == filename)
            .collect();
        if matches.len() != 1 {
            return Err("runtime is not in bundles.lock.json; review and pin it first".into());
        }
        let lock = matches[0];
        let (runtime, contract) = runtime_documents(&bytes)?;
        for key in ["name", "version", "arch", "commit", "os"] {
            if runtime.get(key).is_none() || runtime[key] != lock[key] {
                return Err(format!("runtime manifest differs from lock: {key}").into());
            }
        }
        let compatibility = read_json(&root.join("compatibility.json"))?;
        let compatibility = &compatibility["tessera"];
        if manifest["version"] != compatibility["plugin_version"]
            || runtime["version"] != compatibility["runtime_package"]
        {
            return Err(
                "plugin/runtime version changed: update compatibility before building".into(),
            );
        }
        if contract.get("control_wire_version").is_none()
            || contract["control_wire_version"] != compatibility["control_wire_version"]
        {
            return Err("control API changed: update plugin compatibility before building".into());
        }
        if manifest.get("min_ndimon").is_none()
            || manifest["min_ndimon"] != compatibility["min_ndimon"]
        {
            return Err("host minimum differs from compatibility record".into());
        }
        platform = Some(lock["os"].clone());
        arch = lock["arch"]
            .as_str()
            .filter(|arch| ARCHIVE_ARCHITECTURES.contains(arch))
            .ok_or("missing or unsupported bundle architecture")?
            .into();
        let mut embedded = serde_json::Map::new();
        for key in ["name", "version", "arch", "commit", "sha256", "os"] {
            embedded.insert(key.into(), lock[key].clone());
        }
        let info = pretty(&Value::Object(embedded))?;
        payload.insert("bundle-lock.json".into(), (info.clone(), 0o644));
        payload.insert("bundle-info.json".into(), (info, 0o644));
        payload.insert("compatibility.json".into(), (pretty(compatibility)?, 0o644));
        payload.insert(format!("bundle/{filename}"), (bytes, 0o644));
        payload.insert(
            "bundle/SHA256SUMS".into(),
            (format!("{digest}  {filename}\n").into_bytes(), 0o644),
        );
    } else if options.bundle.is_some() {
        return Err("--bundle is only supported for Tessera".into());
    } else {
        arch = match &options.arch {
            Some(arch) => arch.clone(),
            None => host_architecture()?.into(),
        };
    }
    fs::create_dir_all(&options.output)?;
    let filename = archive_name(&options.plugin, version, &arch);
    let destination = options.output.join(&filename);
    let archive = archive_bytes(&payload, epoch)?;
    fs::write(&destination, &archive)?;
    let tag = options
        .release_tag
        .clone()
        .unwrap_or_else(|| format!("{}-v{version}", options.plugin));
    let mut catalog = json!({"name":options.plugin,"version":version,"arch":arch,
        "asset_url":format!("https://github.com/markfen88/NDIMon-Plugins/releases/download/{}/{}", quote(&tag), quote(&filename)),
        "sha256":sha256(&archive),"size":archive.len()});
    if let Some(platform) = platform {
        catalog["platform"] = platform;
    }
    fs::write(
        options.output.join(format!("{filename}.json")),
        pretty(&catalog)?,
    )?;
    let mut archives = Vec::new();
    for entry in fs::read_dir(&options.output)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.contains("-plugin_") && name.ends_with(".tar.gz") && entry.file_type()?.is_file() {
            archives.push((name, entry.path()));
        }
    }
    archives.sort_by(|a, b| a.0.cmp(&b.0));
    let mut checksums = String::new();
    for (name, path) in archives {
        checksums.push_str(&format!("{}  {name}\n", sha256(&fs::read(path)?)));
    }
    fs::write(options.output.join("SHA256SUMS"), checksums)?;
    println!(
        "{}",
        destination
            .strip_prefix(root)
            .unwrap_or(&destination)
            .display()
    );
    Ok(destination)
}

fn collect_source(root: &Path, directory: &Path, payload: &mut Payload) -> Result {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        let relative = path.strip_prefix(root)?;
        if relative.components().any(|p| {
            matches!(
                p.as_os_str().to_str(),
                Some("__pycache__" | "node_modules" | "tests" | ".git")
            )
        }) {
            continue;
        }
        if kind.is_symlink() {
            return Err("plugin source contains a symbolic link".into());
        }
        if kind.is_dir() {
            collect_source(root, &path, payload)?;
        } else if kind.is_file() {
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                if entry.metadata()?.permissions().mode() & 0o111 != 0 {
                    0o755
                } else {
                    0o644
                }
            };
            #[cfg(not(unix))]
            let mode = 0o644;
            payload.insert(
                relative.to_string_lossy().replace('\\', "/"),
                (fs::read(&path)?, mode),
            );
        }
    }
    Ok(())
}

fn runtime_documents(bytes: &[u8]) -> Result<(Value, Value)> {
    let mut archive = tar::Archive::new(GzDecoder::new(bytes));
    let mut manifests = Vec::new();
    let mut contracts = Vec::new();
    let mut installers = 0;
    for entry in archive.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?.to_string_lossy().into_owned();
        if path.ends_with("/tessera-bundle") && entry.header().mode()? & 0o111 != 0 {
            installers += 1;
        }
        if path.ends_with("/manifest.json") || path.ends_with("/integration-contract.json") {
            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;
            let value = serde_json::from_slice(&content)?;
            if path.ends_with("/manifest.json") {
                manifests.push(value);
            } else {
                contracts.push(value);
            }
        }
    }
    if manifests.len() != 1 || contracts.len() != 1 {
        return Err("runtime manifest/integration contract missing or ambiguous".into());
    }
    // privd installs the runtime with the bundle's own tessera-bundle; a bundle
    // that only ships shell entry points would build but never install.
    if installers != 1 {
        return Err("runtime bundle must ship exactly one executable tessera-bundle".into());
    }
    Ok((manifests.remove(0), contracts.remove(0)))
}

/// Match Python tarfile's PAX headers, checksum spelling and 20-block padding.
/// The generic tar writer uses a different checksum field and GNU long names;
/// emitting this small regular-file subset preserves existing archive digests.
fn archive_bytes(payload: &Payload, epoch: u32) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for (path, (content, mode)) in payload {
        let needs_path = !path.is_ascii() || path.len() > 100;
        if needs_path {
            let record = pax_record("path", path);
            append_member(
                &mut bytes,
                "././@PaxHeader",
                record.as_bytes(),
                0,
                0,
                false,
                b'x',
            )?;
        }
        append_member(&mut bytes, path, content, *mode, epoch, true, b'0')?;
    }
    bytes.resize(bytes.len() + 1024, 0);
    let padded = bytes.len().div_ceil(10240) * 10240;
    bytes.resize(padded, 0);
    // zlib is also used by Python gzip.GzipFile, with level 9 and no filename.
    let mut zipped = GzBuilder::new()
        .mtime(epoch)
        .operating_system(255)
        .write(Vec::new(), Compression::best());
    std::io::Write::write_all(&mut zipped, &bytes)?;
    Ok(zipped.finish()?)
}

fn pax_record(key: &str, value: &str) -> String {
    let body = format!(" {key}={value}\n");
    let mut length = body.len() + 1;
    loop {
        let next = body.len() + length.to_string().len();
        if next == length {
            return format!("{length}{body}");
        }
        length = next;
    }
}

fn append_member(
    out: &mut Vec<u8>,
    path: &str,
    content: &[u8],
    mode: u32,
    epoch: u32,
    root_names: bool,
    kind: u8,
) -> Result {
    let mut header = [0_u8; 512];
    let ascii: Vec<u8> = path
        .chars()
        .map(|ch| if ch.is_ascii() { ch as u8 } else { b'?' })
        .take(100)
        .collect();
    header[..ascii.len()].copy_from_slice(&ascii);
    for (range, value) in [
        (100..108, u64::from(mode)),
        (108..116, 0),
        (116..124, 0),
        (124..136, content.len() as u64),
        (136..148, u64::from(epoch)),
    ] {
        let field = &mut header[range];
        let octal = format!("{value:0width$o}\0", width = field.len() - 1);
        if octal.len() != field.len() {
            return Err("archive member numeric field exceeds PAX implementation limit".into());
        }
        field.copy_from_slice(octal.as_bytes());
    }
    header[148..156].fill(b' ');
    header[156] = kind;
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    if root_names {
        header[265..269].copy_from_slice(b"root");
        header[297..301].copy_from_slice(b"root");
    }
    let checksum: u32 = header.iter().map(|b| u32::from(*b)).sum();
    let checksum = format!("{checksum:06o}\0 ");
    header[148..156].copy_from_slice(checksum.as_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(content);
    out.resize(out.len().div_ceil(512) * 512, 0);
    Ok(())
}

fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn pretty(value: &Value) -> Result<Vec<u8>> {
    Ok(format!("{}\n", serde_json::to_string_pretty(value)?).into_bytes())
}
fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn plugin_name(name: &str) -> bool {
    (2..=32).contains(&name.len())
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn quote(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(root: &Path) -> Options {
        let directory = root.join("plugins/hello");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("plugin.json"),
            r#"{"name":"hello","version":"1.2.0~beta","title":"Hello","min_ndimon":"1.3.0","backup":null}"#,
        )
        .unwrap();
        for name in ["routes.js", "ui.html", "install.sh", "uninstall.sh"] {
            fs::write(directory.join(name), name).unwrap();
        }
        Options {
            plugin: "hello".into(),
            bundle: None,
            output: root.join("dist/plugins"),
            release_tag: None,
            experimental: false,
            arch: Some("amd64".into()),
        }
    }

    #[test]
    fn plugin_layout_identity_checksums_and_determinism() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let options = source(root);
        let dest = build_with_epoch(root, &options, 123).unwrap();
        assert_eq!(
            dest.file_name().unwrap(),
            "hello-plugin_1.2.0-beta_amd64.tar.gz"
        );
        let first = fs::read(&dest).unwrap();
        assert_eq!(first[3], 0, "no gzip filename or extra metadata");
        assert_eq!(&first[4..8], &123_u32.to_le_bytes());
        build_with_epoch(root, &options, 123).unwrap();
        assert_eq!(first, fs::read(&dest).unwrap());
        let checksums = fs::read_to_string(options.output.join("SHA256SUMS")).unwrap();
        assert_eq!(
            checksums,
            format!("{}  hello-plugin_1.2.0-beta_amd64.tar.gz\n", sha256(&first))
        );
        let catalog = read_json(
            &options
                .output
                .join("hello-plugin_1.2.0-beta_amd64.tar.gz.json"),
        )
        .unwrap();
        assert_eq!(catalog["version"], "1.2.0~beta");
        assert_eq!(catalog["sha256"], sha256(&first));
        let mut archive = tar::Archive::new(GzDecoder::new(first.as_slice()));
        let mut names = Vec::new();
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            let header = entry.header();
            assert_eq!(header.uid().unwrap(), 0);
            assert_eq!(header.gid().unwrap(), 0);
            assert_eq!(header.mtime().unwrap(), 123);
            assert_eq!(header.username().unwrap(), Some("root"));
            names.push(entry.path().unwrap().to_string_lossy().into_owned());
        }
        assert_eq!(
            names,
            [
                "install.sh",
                "plugin.json",
                "routes.js",
                "ui.html",
                "uninstall.sh"
            ]
        );
        assert_eq!(
            quote("release/tag with~space"),
            "release%2Ftag%20with~space"
        );
    }

    #[test]
    fn transport_is_byte_identical_to_the_legacy_python_builder() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let options = source(root);
        let nested = root.join("plugins/hello").join("a".repeat(110));
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("unicode-é.txt"), "PAX path fixture").unwrap();
        let rust = build_with_epoch(root, &options, 123).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugin-legacy");
        let filename = rust.file_name().unwrap();
        assert_eq!(
            fs::read(&rust).unwrap(),
            fs::read(fixture.join(filename)).unwrap()
        );
        assert_eq!(
            fs::read(options.output.join("SHA256SUMS")).unwrap(),
            fs::read(fixture.join("SHA256SUMS")).unwrap()
        );
        let catalog = format!("{}.json", filename.to_string_lossy());
        assert_eq!(
            fs::read(options.output.join(&catalog)).unwrap(),
            fs::read(fixture.join(&catalog)).unwrap()
        );
    }

    fn runtime_bundle(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        ));
        for (name, bytes, mode) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(*mode);
            header.set_cksum();
            archive.append_data(&mut header, name, *bytes).unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap()
    }

    /// A first-party Tessera tree with no lifecycle files, a pinned runtime and
    /// matching lock and compatibility records. Returns the options to build it.
    fn tessera(root: &Path, runtime: &[(&str, &[u8], u32)]) -> Options {
        let directory = root.join("plugins/tessera");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("plugin.json"),
            r#"{"name":"tessera","version":"0.2.1","title":"Tessera","min_ndimon":"1.3.0","priv_commands":["enable","disable","write-flows"]}"#,
        )
        .unwrap();
        fs::write(directory.join("ui.html"), "ui.html").unwrap();
        let bytes = runtime_bundle(runtime);
        let filename = "tessera-bundle_0.2.1-1_amd64.tar.gz";
        let bundle = root.join(filename);
        fs::write(&bundle, &bytes).unwrap();
        let platform = json!({"id":"debian","version_id":"13"});
        fs::write(
            root.join("bundles.lock.json"),
            json!({"schema_version":1,"bundles":[{
                "name":"tessera","version":"0.2.1-1","arch":"amd64","commit":"fixture",
                "os":platform,"asset":filename,"sha256":sha256(&bytes)}]})
            .to_string(),
        )
        .unwrap();
        fs::write(
            root.join("compatibility.json"),
            json!({"schema_version":1,"tessera":{
                "plugin_version":"0.2.1","min_ndimon":"1.3.0",
                "runtime_package":"0.2.1-1","control_wire_version":1}})
            .to_string(),
        )
        .unwrap();
        Options {
            plugin: "tessera".into(),
            bundle: Some(bundle),
            output: root.join("dist/plugins"),
            release_tag: None,
            experimental: false,
            arch: None,
        }
    }

    type Member = (&'static str, &'static [u8], u32);
    fn runtime_files(installer: Option<(&'static str, u32)>) -> Vec<Member> {
        let manifest: &'static [u8] = br#"{"name":"tessera","version":"0.2.1-1","arch":"amd64","commit":"fixture","os":{"id":"debian","version_id":"13"}}"#;
        let contract: &'static [u8] = br#"{"control_wire_version":1}"#;
        let binary: &'static [u8] = b"ELF fixture";
        let mut files: Vec<Member> = vec![
            ("runtime/manifest.json", manifest, 0o644),
            ("runtime/integration-contract.json", contract, 0o644),
        ];
        if let Some((name, mode)) = installer {
            files.push((name, binary, mode));
        }
        files
    }

    #[test]
    fn tessera_archive_needs_no_lifecycle_entry_points_but_a_native_installer() {
        let temp = tempfile::tempdir().unwrap();
        let options = tessera(
            temp.path(),
            &runtime_files(Some(("runtime/tessera-bundle", 0o755))),
        );
        let archive = build_with_epoch(temp.path(), &options, 0).unwrap();
        let mut names = Vec::new();
        let bytes = fs::read(&archive).unwrap();
        let mut tar = tar::Archive::new(GzDecoder::new(bytes.as_slice()));
        for entry in tar.entries().unwrap() {
            names.push(
                entry
                    .unwrap()
                    .path()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        assert!(names.iter().any(|n| n == "plugin.json"));
        assert!(
            names
                .iter()
                .any(|n| n == "bundle/tessera-bundle_0.2.1-1_amd64.tar.gz")
        );
        assert!(names.iter().any(|n| n == "ui.html"));
        for forbidden in ["install.sh", "uninstall.sh", "routes.js", "backup.js"] {
            assert!(!names.iter().any(|n| n == forbidden), "{forbidden}");
        }
        assert!(!names.iter().any(|n| n.starts_with("priv/")));
        // A runtime that only ships shell entry points, or a non-executable
        // installer, would build here but could never be installed by privd.
        for runtime in [
            runtime_files(None),
            runtime_files(Some(("runtime/install.sh", 0o755))),
            runtime_files(Some(("runtime/tessera-bundle", 0o644))),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let options = tessera(temp.path(), &runtime);
            let error = build_with_epoch(temp.path(), &options, 0).unwrap_err();
            assert!(error.to_string().contains("tessera-bundle"), "{error}");
        }
    }

    /// A release feed offers a device the archives named for its architecture, so
    /// the names `build-plugin` writes must parse the way the feed parses them.
    #[test]
    fn archives_are_named_for_an_architecture_a_release_feed_accepts() {
        use ndimon_privd_proto::plugin::parse_archive_name;
        for arch in ARCHIVE_ARCHITECTURES {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            let mut options = source(root);
            options.arch = Some(arch.into());
            let archive = build_with_epoch(root, &options, 0).unwrap();
            let file = archive.file_name().unwrap().to_str().unwrap();
            assert_eq!(
                parse_archive_name(file, arch),
                Some(("hello", "1.2.0-beta"))
            );
            let catalog = read_json(&options.output.join(format!("{file}.json"))).unwrap();
            assert_eq!(catalog["arch"], arch);
            assert!(catalog["asset_url"].as_str().unwrap().ends_with(file));
            let sums = fs::read_to_string(options.output.join("SHA256SUMS")).unwrap();
            assert!(
                sums.lines()
                    .any(|line| line.ends_with(&format!("  {file}")))
            );
        }
        // Without --arch the plugin is built for this host, which is one of them.
        let temp = tempfile::tempdir().unwrap();
        let mut options = source(temp.path());
        options.arch = None;
        let archive = build_with_epoch(temp.path(), &options, 0).unwrap();
        let file = archive.file_name().unwrap().to_str().unwrap();
        assert!(
            ARCHIVE_ARCHITECTURES
                .iter()
                .any(|arch| parse_archive_name(file, arch).is_some()),
            "{file}"
        );
        assert!(!file.contains("_any"), "{file}");
    }

    #[test]
    fn the_architecture_option_names_a_device_architecture_and_not_tessera() {
        let root = Path::new("/nonexistent");
        let parse = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|a| (*a).into()).collect();
            Options::parse(root, &args)
        };
        for arch in ARCHIVE_ARCHITECTURES {
            let options = parse(&["--plugin", "hello", "--arch", arch]).unwrap();
            assert_eq!(options.arch.as_deref(), Some(arch));
        }
        assert_eq!(parse(&["--plugin", "hello"]).unwrap().arch, None);
        for arch in ["any", "armhf", "x86_64", "", "amd64/arm64"] {
            assert!(
                parse(&["--plugin", "hello", "--arch", arch]).is_err(),
                "{arch}"
            );
        }
        let error = parse(&["tessera", "--arch", "amd64"]).unwrap_err();
        assert!(error.to_string().contains("Tessera"), "{error}");
    }

    #[test]
    fn other_plugins_still_need_their_lifecycle_entry_points() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let options = source(root);
        fs::remove_file(root.join("plugins/hello/install.sh")).unwrap();
        assert!(build_with_epoch(root, &options, 0).is_err());
        fs::write(root.join("plugins/hello/install.sh"), "install").unwrap();
        fs::write(
            root.join("plugins/hello/plugin.json"),
            r#"{"name":"hello","version":"1.2.0","title":"Hello","min_ndimon":"1.3.0","backup":null,"priv_commands":["ping"]}"#,
        )
        .unwrap();
        let error = build_with_epoch(root, &options, 0).unwrap_err();
        assert!(error.to_string().contains("ping"), "{error}");
    }

    #[test]
    fn builder_refuses_missing_entries_wrong_identity_and_source_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let options = source(root);
        fs::remove_file(root.join("plugins/hello/routes.js")).unwrap();
        assert!(build_with_epoch(root, &options, 0).is_err());
        fs::write(root.join("plugins/hello/routes.js"), "route").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("routes.js", root.join("plugins/hello/link")).unwrap();
            assert!(build_with_epoch(root, &options, 0).is_err());
        }
    }
}
