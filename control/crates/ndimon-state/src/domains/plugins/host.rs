//! Native manifest/catalog/feed/archive host. Plugin code never runs here.
use super::compare_versions;
use crate::persist::StateFile;
use base64::Engine;
use ndimon_privd_proto::{PluginName, plugin::Manifest};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

pub const MAX_ARCHIVE: u64 = 200 * 1024 * 1024;
pub const MIGRATION: &str = "needs an update to the out-of-process format";
#[derive(Debug)]
pub struct NativeHost {
    pub root: PathBuf,
    cache: Mutex<Option<(Instant, Value, Value)>>,
    refreshing: Mutex<()>,
}
static HOST: OnceLock<NativeHost> = OnceLock::new();
pub fn native() -> &'static NativeHost {
    HOST.get_or_init(|| NativeHost::new("/"))
}
pub fn initialize(root: PathBuf) -> Result<(), String> {
    HOST.set(NativeHost::new(root))
        .map_err(|_| "plugin host already initialized".into())
}
impl NativeHost {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cache: Mutex::new(None),
            refreshing: Mutex::new(()),
        }
    }
    pub fn path(&self, absolute: &str) -> PathBuf {
        self.root.join(absolute.trim_start_matches('/'))
    }
    fn code_root(&self, name: &str) -> PathBuf {
        let installed = self.path("/opt/ndimon-r/plugins");
        // Installed bundles retain precedence, including broken/unsafe entries.
        // Only the first-party native fragment has an immutable package copy.
        if name == "tessera"
            && fs::symlink_metadata(installed.join(name))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            self.path("/usr/share/ndimon/plugins")
        } else {
            installed
        }
    }
    fn state(&self) -> Result<Option<StateFile>, String> {
        let file = self.path("/var/lib/ndimon/state/state.json");
        match fs::read(file) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| format!("plugins_state_corrupt: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
    pub fn manifest(&self, name: &str) -> Result<Manifest, String> {
        let name: PluginName = name.to_owned().try_into()?;
        let bytes = regular(
            &self.code_root(name.as_str()),
            &format!("{name}/plugin.json"),
            65536,
        )?;
        let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        manifest.validate(&name, None).map_err(|e| e.to_string())?;
        Ok(manifest)
    }
    fn entry(&self, name: &str, context: &Value) -> Option<Value> {
        let root = self.code_root(name);
        let dir = root.join(name);
        if fs::symlink_metadata(&dir).is_err() {
            return None;
        }
        // The package's Tessera manifest and UI are the native fragment of a
        // Tessera the device has accepted, not an installation: without it in
        // the accepted state (and no bundle under /opt) there is no plugin.
        if root == self.path("/usr/share/ndimon/plugins")
            && context["plugins"][name]["installed"] != true
        {
            return None;
        }
        let mut entry = json!({"name":name,"state":"error"});
        match self.manifest(name) {
            Err(error) => {
                entry["error"] = json!(error);
            }
            Ok(manifest) => {
                let accepted = &context["plugins"][name];
                let runtime = &context["runtime"]["plugins"][name];
                let matches = runtime["version"] == manifest.version.as_str();
                let enabled = accepted["enabled"] == true
                    && (!matches || runtime["enabled"] == true)
                    && !(matches && runtime["pending_enable"] == true);
                entry = json!({"name":name,"version":manifest.version,"title":manifest.title,"icon":manifest.icon,"manifest":manifest,"state":if enabled {"enabled"} else {"installed"}});
                if matches && runtime["pending_enable"] == true {
                    entry["pending_enable"] = json!(true);
                }
                if matches && runtime["enable_error"].is_string() {
                    entry["enable_error"] = runtime["enable_error"].clone();
                }
                if accepted["installed"] == true && accepted["version"] != manifest.version.as_str()
                {
                    entry["state"] = json!("error");
                    entry["error"] =
                        json!("installed version differs from accepted plugin version");
                }
                if enabled && manifest.process.is_some() && runtime["runtime_error"].is_string() {
                    entry["state"] = json!("error");
                    entry["error"] = runtime["runtime_error"].clone();
                }
                if self
                    .path(&format!("/opt/ndimon-r/plugins/{name}/.broken"))
                    .exists()
                {
                    entry["state"] = json!("broken");
                    entry["error"] = json!("plugin operation failed; retry or force remove");
                } else if manifest.process.is_none() && name != "tessera" {
                    // Manifest-v1 plugins ran inside the removed JavaScript API.
                    // They stay installed with their data but cannot be active.
                    entry["migration_warning"] = json!(MIGRATION);
                    if accepted["enabled"] == true {
                        entry["state"] = json!("error");
                        entry["error"] = json!(MIGRATION);
                    }
                }
            }
        }
        Some(entry)
    }
    pub async fn request(&self, request: Value) -> Result<Value, String> {
        let operation = request["operation"]
            .as_str()
            .or_else(|| request["action"].as_str())
            .unwrap_or("");
        let saved = self.state();
        let mut context = request.get("_context").cloned().unwrap_or_else(|| match &saved {
            Ok(Some(s)) => json!({"plugins":s.document.plugins,"device":s.document.device,"runtime":s.plugin_runtime,"revision":s.revision}),
            _ => json!({"plugins":{},"device":{},"runtime":{},"revision":0}),
        });
        if request["config"]["plugins"].is_object() {
            context["plugins"] = request["config"]["plugins"].clone();
        }
        match operation {
            "status" => Ok(json!({"ok":true,"revision":context["revision"]})),
            "apply_config" => Ok(
                json!({"ok":true,"revision":request["revision"],"refused":[],"restart_required":false}),
            ),
            "list" => {
                let mut plugins = vec![];
                let mut names = vec![];
                if let Ok(entries) = fs::read_dir(self.path("/opt/ndimon-r/plugins")) {
                    names = entries
                        .filter_map(Result::ok)
                        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .filter(|n| PluginName::try_from(n.clone()).is_ok())
                        .collect::<Vec<_>>();
                }
                if self.path("/usr/share/ndimon/plugins/tessera").is_dir() {
                    names.push("tessera".into());
                }
                names.sort();
                names.dedup();
                for name in names {
                    if let Some(entry) = self.entry(&name, &context) {
                        plugins.push(entry);
                    }
                }
                let feed = self
                    .cache
                    .lock()
                    .await
                    .as_ref()
                    .map(|(_, _, v)| v.clone())
                    .unwrap_or_else(empty_feed);
                Ok(
                    json!({"plugins":plugins,"plugins_state_error":saved.err().or_else(||context["runtime"]["state_error"].as_str().map(str::to_owned)),"feed":feed,"arch":architecture()}),
                )
            }
            "manifest" => Ok(self
                .entry(request["name"].as_str().unwrap_or(""), &context)
                .unwrap_or(Value::Null)),
            "check" => {
                saved?;
                if context["runtime"]["state_error"].is_string() {
                    return Err("plugins_state_corrupt".into());
                }
                Ok(json!({"ok":true}))
            }
            "feed" | "refresh" => {
                self.feed(context["device"].clone(), operation == "refresh")
                    .await
            }
            "prepare" => self.prepare(&request, &context["device"]).await,
            "upload_dir" => {
                Ok(json!({"path":self.path("/var/lib/ndimon/import/plugin-downloads")}))
            }
            "asset" => self.asset(&request, &context),
            // Lifecycle observations are returned to the actor, which persists
            // them with its receipt/completion. No second runtime file writer.
            "installed" => {
                let name = request["name"].as_str().ok_or("invalid plugin name")?;
                let error = self.manifest(name).err();
                if let Some(archive) = request["archive"]
                    .as_str()
                    .filter(|a| ndimon_privd_proto::ArchiveId::try_from(a.to_string()).is_ok())
                {
                    let _ = fs::remove_file(
                        self.path("/var/lib/ndimon/import/plugin-downloads")
                            .join(archive),
                    );
                }
                Ok(json!({"ok":true,"load_error":error}))
            }
            "removed" | "enabled" | "reset" => Ok(json!({"ok":true})),
            "job" => {
                Ok(json!({"job":context["runtime"]["job"],"error":context["runtime"]["job_error"]}))
            }
            _ => Err("unsupported native plugin host operation".into()),
        }
    }
    fn asset(&self, request: &Value, context: &Value) -> Result<Value, String> {
        let name = request["name"].as_str().ok_or("invalid plugin name")?;
        let manifest = self.manifest(name)?;
        if context["plugins"][name]["enabled"] != true
            || self
                .path(&format!("/opt/ndimon-r/plugins/{name}/.broken"))
                .exists()
        {
            return Err("plugin not enabled".into());
        }
        let path = request["path"].as_str().unwrap_or("ui.html");
        let file = if path == "ui.html" {
            manifest.ui.as_str()
        } else {
            path
        };
        if manifest.process.is_none() && path != "ui.html" {
            return Err("not found".into());
        }
        // Metadata, executable and private hooks are never a public UI bundle.
        if [
            "plugin.json",
            "routes.js",
            "backup.js",
            "install.sh",
            "uninstall.sh",
        ]
        .contains(&file)
            || [&manifest.routes, &manifest.install, &manifest.uninstall]
                .into_iter()
                .chain(manifest.backup.iter())
                .any(|entry| entry == file)
            || file.starts_with("priv/")
            || manifest.process.as_ref().is_some_and(|p| p.exec == file)
        {
            return Err("not found".into());
        }
        let mime = match Path::new(file)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
        {
            "html" => "text/html; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "js" => "text/javascript; charset=utf-8",
            "json" => "application/json",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "svg" => "image/svg+xml",
            "woff2" => "font/woff2",
            _ => return Err("unsupported UI asset".into()),
        };
        let bytes = regular(
            &self.code_root(name),
            &format!("{name}/{file}"),
            8 * 1024 * 1024,
        )?;
        Ok(json!({"mime":mime,"data":base64::engine::general_purpose::STANDARD.encode(bytes)}))
    }
    async fn feed(&self, device: Value, force: bool) -> Result<Value, String> {
        let _refresh = self.refreshing.lock().await;
        if let Some((at, policy, value)) = &*self.cache.lock().await {
            if !force && policy == &device && at.elapsed() < Duration::from_secs(900) {
                return Ok(value.clone());
            }
        }
        let urls = device["plugin_feeds"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if urls.is_empty() {
            let value = empty_feed();
            *self.cache.lock().await = Some((Instant::now(), device, value.clone()));
            return Ok(value);
        }
        if urls.len() > 32 {
            return Err("too many plugin feeds".into());
        }
        let insecure = device["allow_insecure_feeds"] == true;
        let mut tasks = vec![];
        for url in urls {
            tasks.push(tokio::task::spawn_blocking(move || {
                fetch_feed(url.as_str().unwrap_or(""), insecure)
            }));
        }
        let mut plugins = vec![];
        let mut errors = vec![];
        let mut available = false;
        for task in tasks {
            match task.await.map_err(|e| e.to_string())? {
                Ok(mut entries) => {
                    available = true;
                    plugins.append(&mut entries);
                }
                Err(e) => errors.push(e),
            }
        }
        plugins.sort_by(|a, b| {
            compare_versions(
                b["version"].as_str().unwrap_or(""),
                a["version"].as_str().unwrap_or(""),
            )
        });
        let mut value = json!({"available":available,"plugins":plugins});
        if !errors.is_empty() {
            value["error"] = json!(errors.join("; "));
        }
        *self.cache.lock().await = Some((Instant::now(), device, value.clone()));
        Ok(value)
    }
    async fn prepare(&self, request: &Value, device: &Value) -> Result<Value, String> {
        let name = request["name"].as_str().unwrap_or("");
        if PluginName::try_from(name.to_owned()).is_err() {
            return Ok(json!({"error":"invalid plugin name","status":400}));
        }
        let dir = self.path("/var/lib/ndimon/import/plugin-downloads");
        fs::create_dir_all(&dir).map_err(err)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(err)?;
        let entry = match request["source"].as_str() {
            Some("upload") => None,
            Some("feed") => {
                let feed = self.feed(device.clone(), false).await?;
                let entry = feed["plugins"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|p| {
                        p["name"] == name
                            && (request["version"].is_null() || request["version"] == p["version"])
                    })
                    .cloned();
                let Some(entry) = entry else {
                    return Ok(json!({"error":"plugin not in feed","status":404}));
                };
                Some(entry)
            }
            _ => return Ok(json!({"error":"invalid source","status":400})),
        };
        // Stable confirmation snapshots expire after fifteen minutes. Upload
        // aliases and in-flight private temporary files are not collected.
        if let Ok(files) = fs::read_dir(&dir) {
            for file in files.flatten() {
                let file_name = file.file_name().to_string_lossy().into_owned();
                let stable = file_name
                    .strip_suffix(".tar.gz")
                    .and_then(|s| s.rsplit_once('-'))
                    .is_some_and(|(_, hash)| {
                        hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
                    });
                if stable
                    && file.file_type().is_ok_and(|t| t.is_file())
                    && file
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > Duration::from_secs(900))
                {
                    let _ = fs::remove_file(file.path());
                }
            }
        }
        let name = name.to_owned();
        let insecure = device["allow_insecure_feeds"] == true;
        let result = tokio::task::spawn_blocking(move || {
            let mut source: Box<dyn Read> = if let Some(e) = &entry {
                Box::new(
                    fetch(
                        e["asset_url"].as_str().ok_or("invalid bundle URL")?,
                        insecure,
                        Duration::from_secs(300),
                    )?
                    .into_reader(),
                )
            } else {
                Box::new(open_regular(&dir, &format!("{name}.tar.gz"))?)
            };
            let mut temp = tempfile::NamedTempFile::new_in(&dir).map_err(err)?;
            let digest = copy_archive(&mut source, &mut temp, MAX_ARCHIVE)?;
            if entry.as_ref().is_some_and(|e| {
                !e["sha256"]
                    .as_str()
                    .unwrap_or("")
                    .eq_ignore_ascii_case(&digest)
            }) {
                return Err("SHA256 mismatch".to_string());
            }
            let archive = format!("{name}-{digest}.tar.gz");
            temp.as_file().sync_all().map_err(err)?;
            temp.persist(dir.join(&archive)).map_err(err)?;
            let mut result = json!({"archive":archive});
            if let Some(e) = entry {
                result["version"] = e["version"].clone();
            }
            Ok::<_, String>(result)
        })
        .await
        .map_err(err)??;
        let mut result = result;
        result["host_version"] = json!(
            fs::read_to_string(self.path("/etc/ndimon-firmware-version"))
                .unwrap_or_else(|_| "1.4.0".into())
                .trim()
        );
        Ok(result)
    }
}
pub fn context(state: &StateFile) -> Value {
    json!({"plugins":state.document.plugins,"device":state.document.device,"runtime":state.plugin_runtime,"revision":state.revision})
}
pub fn architecture() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "amd64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        std::env::consts::ARCH
    }
}
fn empty_feed() -> Value {
    json!({"available":false,"plugins":[],"error":"no feeds configured"})
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
/// Open every component with O_NOFOLLOW, retaining directory descriptors.
pub fn open_regular(root: &Path, relative: &str) -> Result<File, String> {
    use rustix::fs::{Mode, OFlags, openat};
    if !ndimon_privd_proto::plugin::relative_path(relative) {
        return Err("invalid plugin path".into());
    }
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_DIRECTORY)
        .open(root)
        .map_err(err)?;
    let owner = directory.metadata().map_err(err)?.uid();
    let components = relative.split('/').collect::<Vec<_>>();
    for (i, component) in components.iter().enumerate() {
        let last = i == components.len() - 1;
        let flags = OFlags::RDONLY
            | OFlags::NOFOLLOW
            | OFlags::CLOEXEC
            | OFlags::NONBLOCK
            | if last {
                OFlags::empty()
            } else {
                OFlags::DIRECTORY
            };
        let fd = openat(&directory, *component, flags, Mode::empty()).map_err(err)?;
        let file = File::from(fd);
        let metadata = file.metadata().map_err(err)?;
        if metadata.uid() != owner
            || metadata.mode() & 0o022 != 0
            || !last && !metadata.is_dir()
            || last && !metadata.is_file()
        {
            return Err("unsafe plugin file".into());
        }
        directory = file;
    }
    Ok(directory)
}
pub fn regular(root: &Path, relative: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = vec![];
    open_regular(root, relative)?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(err)?;
    if bytes.len() as u64 > limit {
        return Err("plugin file too large".into());
    }
    Ok(bytes)
}
pub fn copy_archive(
    source: &mut dyn Read,
    destination: &mut dyn Write,
    limit: u64,
) -> Result<String, String> {
    let mut hash = Sha256::new();
    let mut size = 0;
    let mut buf = [0; 65536];
    loop {
        let n = source.read(&mut buf).map_err(err)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > limit {
            return Err("archive exceeds 200mb".into());
        }
        hash.update(&buf[..n]);
        destination.write_all(&buf[..n]).map_err(err)?;
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn url(value: &str, insecure: bool) -> Result<url::Url, String> {
    let url = url::Url::parse(value).map_err(err)?;
    if (url.scheme() != "https" && !(insecure && url.scheme() == "http"))
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("HTTPS required for feed URLs".into());
    }
    Ok(url)
}
fn fetch(target: &str, insecure: bool, timeout: Duration) -> Result<ureq::Response, String> {
    let started = Instant::now();
    let mut target = url(target, insecure)?;
    for _ in 0..6 {
        let remaining = timeout
            .checked_sub(started.elapsed())
            .ok_or("feed timeout")?;
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout(remaining)
            .build();
        let response = agent.get(target.as_str()).call().map_err(|e| match e {
            ureq::Error::Status(s, _) => format!("feed HTTP {s}"),
            _ => e.to_string(),
        })?;
        if ![301, 302, 303, 307, 308].contains(&response.status()) {
            return Ok(response);
        }
        target = url(
            target
                .join(
                    response
                        .header("location")
                        .ok_or("redirect has no location")?,
                )
                .map_err(err)?
                .as_str(),
            insecure,
        )?;
    }
    Err("too many redirects".into())
}
fn fetch_feed(target: &str, insecure: bool) -> Result<Vec<Value>, String> {
    let start = Instant::now();
    let read = |target: &str, limit: u64| -> Result<Vec<u8>, String> {
        let remaining = Duration::from_secs(10)
            .checked_sub(start.elapsed())
            .ok_or("feed timeout")?;
        let mut bytes = vec![];
        fetch(target, insecure, remaining)?
            .into_reader()
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(err)?;
        if bytes.len() as u64 > limit {
            return Err("feed too large".into());
        }
        Ok(bytes)
    };
    let doc: Value =
        serde_json::from_slice(&read(target, 8 * 1024 * 1024)?).map_err(|_| "feed is not JSON")?;
    let mut plugins = vec![];
    if let Some(releases) = doc.as_array() {
        for release in releases {
            let Some(assets) = release["assets"].as_array() else {
                continue;
            };
            let Some(sums) = assets.iter().find(|a| a["name"] == "SHA256SUMS") else {
                continue;
            };
            let text = String::from_utf8(read(
                sums["browser_download_url"]
                    .as_str()
                    .ok_or("invalid checksum URL")?,
                1024 * 1024,
            )?)
            .map_err(err)?;
            let checksums = text
                .lines()
                .filter_map(|line| {
                    let mut parts = line.split_whitespace();
                    Some((
                        parts.next()?.to_owned(),
                        parts.next()?.trim_start_matches('*').to_owned(),
                    ))
                })
                .collect::<Vec<_>>();
            for asset in assets {
                let Some(file) = asset["name"].as_str() else {
                    continue;
                };
                let Some((name, version)) =
                    ndimon_privd_proto::plugin::parse_archive_name(file, architecture())
                else {
                    continue;
                };
                if let Some((hash, _)) = checksums.iter().find(|(_, n)| n == file) {
                    plugins.push(json!({"name":name,"version":version,"asset_url":asset["browser_download_url"],"sha256":hash.to_ascii_lowercase(),"size":asset.get("size").cloned().unwrap_or(json!(0))}));
                }
            }
        }
    } else if let Some(entries) = doc["plugins"].as_array() {
        for entry in entries {
            if entry.get("arch").is_none_or(|a| a == architecture()) {
                let mut entry = entry.clone();
                entry
                    .as_object_mut()
                    .ok_or("invalid feed entry")?
                    .remove("arch");
                plugins.push(entry);
            }
        }
    } else {
        return Err("invalid feed shape".into());
    }
    for p in &plugins {
        let name = p["name"].as_str().ok_or("invalid feed entry")?;
        let version = p["version"].as_str().ok_or("invalid feed entry")?;
        let hash = p["sha256"].as_str().ok_or("invalid feed entry")?;
        if PluginName::try_from(name.to_string()).is_err()
            || ndimon_privd_proto::DebianVersion::try_from(version.to_owned()).is_err()
            || hash.len() != 64
            || !hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("invalid feed entry".into());
        }
        url(
            p["asset_url"].as_str().ok_or("invalid feed entry")?,
            insecure,
        )?;
    }
    plugins.sort_by(|a, b| {
        compare_versions(
            b["version"].as_str().unwrap_or(""),
            a["version"].as_str().unwrap_or(""),
        )
    });
    Ok(plugins)
}

impl crate::StateHandle {
    pub async fn plugin_asset(
        &self,
        name: String,
        path: String,
    ) -> Result<Value, ndimon_types::ApiError> {
        let snapshot = self.snapshot().await?;
        self.applier
            .plugin_query(
                &snapshot,
                json!({"operation":"asset","name":name,"path":path}),
            )
            .await
            .map_err(crate::plan::internal)
    }
    pub async fn plugin_upload_directory(&self) -> Result<PathBuf, ndimon_types::ApiError> {
        let result = self
            .applier
            .plugins(json!({"operation":"upload_dir"}))
            .await
            .map_err(crate::plan::internal)?;
        result["path"]
            .as_str()
            .map(PathBuf::from)
            .ok_or_else(|| crate::plan::internal("upload directory unavailable"))
    }
}

/// Import the former observation file once. The native marker prevents a
/// cleared/reset journal from being re-imported.
pub fn migrate_runtime(state: &mut StateFile, root: &Path) -> Result<(), String> {
    if state.plugin_runtime.get("native_host") == Some(&json!(true)) {
        return Ok(());
    }
    let file = root.join("var/lib/ndimon/plugin-runtime.json");
    match File::open(file).and_then(|file| {
        let mut bytes = vec![];
        file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        Ok(bytes)
    }) {
        Ok(bytes) if bytes.len() <= 1024 * 1024 => {
            match serde_json::from_slice::<serde_json::Map<String, Value>>(&bytes) {
                Ok(runtime) => state.plugin_runtime = runtime,
                Err(_) => {
                    state
                        .plugin_runtime
                        .insert("state_error".into(), json!("plugins_state_corrupt"));
                }
            }
        }
        Ok(_) => {
            state
                .plugin_runtime
                .insert("state_error".into(), json!("plugins_state_corrupt"));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.to_string()),
    }
    if state
        .legacy_files
        .get("/etc/ndimon-plugins.json")
        .is_some_and(|raw| serde_json::from_str::<Value>(raw).map_or(true, |v| !v.is_object()))
    {
        state
            .plugin_runtime
            .insert("state_error".into(), json!("plugins_state_corrupt"));
    }
    // A legacy journal has no original intent/action metadata. Preserve it as
    // an explicit investigation reservation; never guess or replay its effects.
    if let Some(job) = state
        .plugin_runtime
        .remove("job")
        .and_then(|v| v.as_str().map(str::to_owned))
    {
        if state.plugin_job.is_none()
            && !state
                .plugin_results
                .get(&job)
                .is_some_and(|r| !r.investigation)
        {
            state.plugin_job = Some(super::PendingJob {
                id: job.clone(),
                action: super::Action::PluginsCancel { job },
                previous: json!(state.document.plugins),
                started_at: 0,
                investigation: true,
                cancelled: false,
            });
        }
    }
    state
        .plugin_runtime
        .insert("native_host".into(), json!(true));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn packaged_tessera_assets_keep_native_guards_and_installed_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let host = NativeHost::new(dir.path());
        let packaged = host.path("/usr/share/ndimon/plugins/tessera");
        fs::create_dir_all(&packaged).unwrap();
        fs::set_permissions(&packaged, fs::Permissions::from_mode(0o755)).unwrap();
        let manifest: Value = serde_json::from_str(include_str!(
            "../../../../../../plugins/tessera/plugin.json"
        ))
        .unwrap();
        fs::write(packaged.join("plugin.json"), manifest.to_string()).unwrap();
        fs::write(packaged.join("ui.html"), "packaged fragment").unwrap();
        for file in ["plugin.json", "ui.html"] {
            fs::set_permissions(packaged.join(file), fs::Permissions::from_mode(0o644)).unwrap();
        }
        let context = json!({"plugins":{"tessera":{"installed":true,"enabled":true}}});
        let request =
            json!({"operation":"asset", "name":"tessera", "path":"ui.html", "_context":context});
        assert_eq!(
            host.request(request.clone()).await.unwrap()["data"],
            base64::engine::general_purpose::STANDARD.encode("packaged fragment")
        );
        // The package alone is not an installation: a device that never had
        // Tessera lists nothing and has no entry to enable or remove.
        for never in [
            json!({"plugins":{}}),
            json!({"plugins":{"tessera":{"enabled":false}}}),
        ] {
            let list = host
                .request(json!({"operation":"list", "_context":never}))
                .await
                .unwrap();
            assert_eq!(list["plugins"], json!([]));
            let manifest = host
                .request(json!({"operation":"manifest", "name":"tessera", "_context":never}))
                .await
                .unwrap();
            assert!(manifest.is_null());
        }
        let list = host
            .request(json!({"operation":"list", "_context":context}))
            .await
            .unwrap();
        assert_eq!(list["plugins"].as_array().unwrap().len(), 1);
        assert_eq!(list["plugins"][0]["name"], "tessera");
        for path in ["plugin.json", "../ui.html"] {
            assert!(
                host.asset(&json!({"name":"tessera", "path":path}), &context)
                    .is_err()
            );
        }
        assert!(
            host.asset(&json!({"name":"tessera"}), &json!({"plugins":{}}))
                .is_err()
        );
        let installed = host.path("/opt/ndimon-r/plugins/tessera");
        fs::create_dir_all(&installed).unwrap();
        fs::set_permissions(&installed, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(installed.join("plugin.json"), manifest.to_string()).unwrap();
        fs::write(installed.join("ui.html"), "installed fragment").unwrap();
        for file in ["plugin.json", "ui.html"] {
            fs::set_permissions(installed.join(file), fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert_eq!(
            host.request(request.clone()).await.unwrap()["data"],
            base64::engine::general_purpose::STANDARD.encode("installed fragment")
        );
        let list = host
            .request(json!({"operation":"list", "_context":context}))
            .await
            .unwrap();
        assert_eq!(list["plugins"].as_array().unwrap().len(), 1);
        fs::remove_file(installed.join("plugin.json")).unwrap();
        assert!(host.request(request.clone()).await.is_err());
        fs::remove_dir_all(&installed).unwrap();
        std::os::unix::fs::symlink(&packaged, &installed).unwrap();
        assert!(host.request(request).await.is_err());
    }
    async fn server(
        routes: std::collections::BTreeMap<String, (u16, String, Vec<u8>)>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 8192];
                let n = stream.read(&mut request).await.unwrap();
                let path = String::from_utf8_lossy(&request[..n])
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .to_owned();
                let (status, extra, body) =
                    routes
                        .get(&path)
                        .cloned()
                        .unwrap_or((404, String::new(), vec![]));
                let header = format!(
                    "HTTP/1.1 {status} response\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes()).await;
                let _ = stream.write_all(&body).await;
            }
        });
        (base, task)
    }
    #[tokio::test]
    async fn directory_and_release_feeds_prepare_integrity_redirect_policy_and_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let host = NativeHost::new(dir.path());
        let bundle = b"archive fixture".to_vec();
        let hash = format!("{:x}", Sha256::digest(&bundle));
        let (bundle_base, bundle_server) = server(std::collections::BTreeMap::from([(
            "/hello.tar.gz".into(),
            (200, String::new(), bundle.clone()),
        )]))
        .await;
        let directory = json!({"plugins":[{"name":"hello","version":"1.0-1","arch":architecture(),"asset_url":format!("{bundle_base}/hello.tar.gz"),"sha256":hash,"size":bundle.len()}]});
        let release = json!([{"assets":[{"name":"SHA256SUMS","browser_download_url":format!("{bundle_base}/SHA256SUMS")},{"name":format!("hello-plugin_1.0-1_{}.tar.gz",architecture()),"browser_download_url":format!("{bundle_base}/hello.tar.gz"),"size":bundle.len()}]}]);
        let (base, feed_server) = server(std::collections::BTreeMap::from([
            (
                "/index.json".into(),
                (200, String::new(), serde_json::to_vec(&directory).unwrap()),
            ),
            (
                "/large".into(),
                (200, String::new(), vec![b' '; 8 * 1024 * 1024 + 1]),
            ),
            (
                "/redirect".into(),
                (
                    302,
                    "Location: http://user:password@localhost/\r\n".into(),
                    vec![],
                ),
            ),
            (
                "/release.json".into(),
                (200, String::new(), serde_json::to_vec(&release).unwrap()),
            ),
        ]))
        .await;
        let policy =
            json!({"plugin_feeds":[format!("{base}/index.json")],"allow_insecure_feeds":true});
        let feed = host.feed(policy.clone(), false).await.unwrap();
        assert_eq!(feed["available"], true);
        assert_eq!(feed["plugins"][0]["name"], "hello");
        let prepared = host
            .prepare(&json!({"name":"hello","source":"feed"}), &policy)
            .await
            .unwrap();
        let archive = prepared["archive"].as_str().unwrap();
        assert!(archive.contains(&hash));
        assert_eq!(
            fs::read(
                host.path("/var/lib/ndimon/import/plugin-downloads")
                    .join(archive)
            )
            .unwrap(),
            bundle
        );
        assert_eq!(
            host.prepare(&json!({"name":"hello","source":"feed"}), &policy)
                .await
                .unwrap()["archive"],
            archive
        );
        let base2 = base.clone();
        assert!(
            tokio::task::spawn_blocking(move || fetch_feed(&format!("{base2}/index.json"), false))
                .await
                .unwrap()
                .is_err()
        );
        let base2 = base.clone();
        assert!(
            tokio::task::spawn_blocking(move || fetch_feed(&format!("{base2}/large"), true))
                .await
                .unwrap()
                .unwrap_err()
                .contains("too large")
        );
        let base2 = base.clone();
        assert!(
            tokio::task::spawn_blocking(move || fetch_feed(&format!("{base2}/redirect"), true))
                .await
                .unwrap()
                .is_err()
        );
        // Release entries without an exact checksum asset never become usable.
        let release_result =
            tokio::task::spawn_blocking(move || fetch_feed(&format!("{base}/release.json"), true))
                .await
                .unwrap();
        assert!(release_result.is_err());
        let mut copy = vec![];
        assert!(copy_archive(&mut &b"12345"[..], &mut copy, 4).is_err());
        feed_server.abort();
        bundle_server.abort();
    }
    /// What `cargo xtask build-plugin` names its archives is what a release feed
    /// offers: this device's architecture, and not `any`.
    #[tokio::test]
    async fn a_release_feed_offers_the_archives_build_plugin_names_for_this_device() {
        use ndimon_privd_proto::plugin::{ARCHIVE_ARCHITECTURES, archive_name};
        assert!(ARCHIVE_ARCHITECTURES.contains(&architecture()));
        let hash = "a".repeat(64);
        let mine = archive_name("hello", "1.2.0~beta", architecture());
        let other = ARCHIVE_ARCHITECTURES
            .iter()
            .find(|arch| **arch != architecture())
            .unwrap();
        let theirs = archive_name("hello", "1.2.0~beta", other);
        let any = "hello-plugin_1.2.0-beta_any.tar.gz";
        let sums = format!("{hash}  {mine}\n{hash}  {theirs}\n{hash}  {any}\n");
        let (base, sums_server) = server(std::collections::BTreeMap::from([(
            "/SHA256SUMS".into(),
            (200, String::new(), sums.into_bytes()),
        )]))
        .await;
        let asset = |name: &str| json!({"name":name,"browser_download_url":format!("{base}/{name}"),"size":1});
        let release = json!([{"assets":[
            {"name":"SHA256SUMS","browser_download_url":format!("{base}/SHA256SUMS")},
            asset(&mine), asset(&theirs), asset(any)]}]);
        let (feed, feed_server) = server(std::collections::BTreeMap::from([(
            "/release.json".into(),
            (200, String::new(), serde_json::to_vec(&release).unwrap()),
        )]))
        .await;
        let plugins =
            tokio::task::spawn_blocking(move || fetch_feed(&format!("{feed}/release.json"), true))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(plugins.len(), 1, "{plugins:?}");
        assert_eq!(plugins[0]["name"], "hello");
        assert_eq!(plugins[0]["version"], "1.2.0-beta");
        assert!(plugins[0]["asset_url"].as_str().unwrap().ends_with(&mine));
        feed_server.abort();
        sums_server.abort();
    }
    #[test]
    fn migration_is_once_only_and_retains_corruption_and_orphan_reservations() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("var/lib/ndimon")).unwrap();
        let path = root.path().join("var/lib/ndimon/plugin-runtime.json");
        fs::write(&path,r#"{"job":"ndimon-plugin-hello-123.service","plugins":{"hello":{"version":"1.0-1","pending_enable":true}}}"#).unwrap();
        let mut state = StateFile::new(ndimon_types::DesiredState::default(), Default::default());
        state
            .legacy_files
            .insert("/etc/ndimon-plugins.json".into(), "invalid".into());
        migrate_runtime(&mut state, root.path()).unwrap();
        assert!(state.plugin_job.as_ref().unwrap().investigation);
        assert_eq!(state.plugin_runtime["state_error"], "plugins_state_corrupt");
        assert_eq!(
            state.plugin_runtime["plugins"]["hello"]["pending_enable"],
            true
        );
        state.plugin_runtime.clear();
        state
            .plugin_runtime
            .insert("native_host".into(), json!(true));
        state.plugin_job = None;
        migrate_runtime(&mut state, root.path()).unwrap();
        assert!(state.plugin_job.is_none());
        assert_eq!(state.plugin_runtime.len(), 1);
    }
    #[tokio::test]
    async fn native_catalog_never_contacts_the_node_plugin_socket() {
        let dir = tempfile::tempdir().unwrap();
        let host = NativeHost::new(dir.path());
        let api = host.path("/run/ndimon-api");
        fs::create_dir_all(&api).unwrap();
        let poison = tokio::net::UnixListener::bind(api.join("plugins.sock")).unwrap();
        for request in [
            json!({"operation":"list"}),
            json!({"operation":"feed"}),
            json!({"operation":"check"}),
            json!({"operation":"manifest","name":"missing"}),
            json!({"operation":"apply_config","revision":0,"config":{"plugins":{}}}),
            json!({"operation":"reset"}),
            json!({"operation":"job"}),
        ] {
            host.request(request).await.unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(20), poison.accept())
                .await
                .is_err()
        );
        assert!(!include_str!("../../apply.rs").contains("plugins.sock"));
        assert!(!include_str!("../plugins.rs").contains("plugins.sock"));
    }
    // Replaces tests/install_with_plugin.sh 39-46: the former `curl --proto`
    // argument is the native scheme policy, applied to feeds, redirects and the
    // archive URL of every feed entry alike.
    #[test]
    fn url_policy_requires_https_unless_insecure_feeds_are_enabled() {
        assert!(url("https://forge.example/hello.tar.gz", false).is_ok());
        assert!(url("http://forge.example/hello.tar.gz", false).is_err());
        assert!(url("http://forge.example/hello.tar.gz", true).is_ok());
        assert!(url("https://user:secret@forge.example/hello.tar.gz", true).is_err());
        for other in ["file:///etc/passwd", "ftp://forge.example/hello.tar.gz"] {
            assert!(url(other, true).is_err(), "{other}");
        }
    }
    // Replaces tests/install_with_plugin.sh 15, 17-31: a local archive is copied
    // as the control user into a private directory under a content-addressed
    // name. A planted link at that name is replaced, never written through, and a
    // linked source or an invalid plugin name is refused.
    #[tokio::test]
    async fn upload_preparation_copies_privately_and_never_writes_through_links() {
        let root = tempfile::tempdir().unwrap();
        let host = NativeHost::new(root.path());
        let uploads = host.path("/var/lib/ndimon/import/plugin-downloads");
        fs::create_dir_all(&uploads).unwrap();
        let source = uploads.join("hello.tar.gz");
        fs::write(&source, b"plugin archive").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
        let digest = format!("{:x}", Sha256::digest(b"plugin archive"));
        let published = uploads.join(format!("hello-{digest}.tar.gz"));
        let protected = root.path().join("protected");
        fs::write(&protected, "do not overwrite").unwrap();
        std::os::unix::fs::symlink(&protected, &published).unwrap();
        let prepared = host
            .prepare(&json!({"name":"hello","source":"upload"}), &json!({}))
            .await
            .unwrap();
        assert_eq!(prepared["archive"], format!("hello-{digest}.tar.gz"));
        assert_eq!(fs::read_to_string(&protected).unwrap(), "do not overwrite");
        assert!(fs::symlink_metadata(&published).unwrap().is_file());
        assert_eq!(fs::read(&published).unwrap(), b"plugin archive");
        assert_eq!(
            fs::metadata(&uploads).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let refused = host
            .prepare(&json!({"name":"../bad","source":"upload"}), &json!({}))
            .await
            .unwrap();
        assert_eq!(refused["status"], 400);
        fs::remove_file(&source).unwrap();
        std::os::unix::fs::symlink(&protected, &source).unwrap();
        assert!(
            host.prepare(&json!({"name":"hello","source":"upload"}), &json!({}))
                .await
                .is_err()
        );
    }
}
