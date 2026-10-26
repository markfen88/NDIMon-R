//! Control-owned, immutable assets. Pins protect uploads awaiting actor admission.
use crate::persist::{StateFile, atomic};
use ndimon_types::{AssetRef, DesiredState};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions, Permissions},
    io::Read,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

// The old public JSON body is bounded to 4 MiB; base64 fits at most 3 MiB.
pub const MAX_LOGO_BYTES: usize = 3 * 1024 * 1024;
#[derive(Debug, Clone)]
pub struct AssetStore {
    pub directory: PathBuf,
    pins: Arc<Mutex<BTreeMap<String, usize>>>,
}
#[derive(Debug)]
pub struct StoredAsset {
    pub asset: AssetRef,
    store: AssetStore,
}
impl Drop for StoredAsset {
    fn drop(&mut self) {
        let mut pins = self.store.pins.lock().expect("asset pins");
        if let Some(count) = pins.get_mut(&self.asset.sha256) {
            *count -= 1;
            if *count == 0 {
                pins.remove(&self.asset.sha256);
            }
        }
    }
}
pub fn media_type(bytes: &[u8]) -> Result<&'static str, String> {
    if bytes.len() > MAX_LOGO_BYTES {
        return Err("logo is larger than 3 MB".into());
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Ok("image/jpeg")
    } else {
        Err("logo must be PNG or JPEG".into())
    }
}
fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn extension(media: &str) -> Result<&'static str, String> {
    match media {
        "image/png" => Ok("png"),
        "image/jpeg" => Ok("jpg"),
        _ => Err("logo must be PNG or JPEG".into()),
    }
}
impl AssetStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            pins: Default::default(),
        }
    }
    fn prepare(&self) -> Result<(), String> {
        fs::create_dir_all(&self.directory).map_err(|e| e.to_string())?;
        let info = fs::symlink_metadata(&self.directory).map_err(|e| e.to_string())?;
        if !info.is_dir() || info.file_type().is_symlink() {
            return Err("nonregular asset directory".into());
        }
        fs::set_permissions(&self.directory, Permissions::from_mode(0o750))
            .map_err(|e| e.to_string())
    }
    pub fn put(&self, bytes: &[u8]) -> Result<StoredAsset, String> {
        let media = media_type(bytes)?;
        let hash = format!("{:x}", Sha256::digest(bytes));
        // Publication and GC use this same lock. Retain a pin until submission ends.
        let mut pins = self.pins.lock().map_err(|e| e.to_string())?;
        self.prepare()?;
        atomic(&self.directory, &hash, bytes)?;
        atomic(
            &self.directory,
            &format!("{hash}.{}", extension(media)?),
            bytes,
        )?;
        *pins.entry(hash.clone()).or_default() += 1;
        Ok(StoredAsset {
            asset: AssetRef::new(hash, media),
            store: self.clone(),
        })
    }
    pub fn read(&self, asset: &AssetRef) -> Result<Vec<u8>, String> {
        if !valid_hash(&asset.sha256) {
            return Err("invalid asset hash".into());
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .open(self.directory.join(&asset.sha256))
            .map_err(|e| e.to_string())?;
        let info = file.metadata().map_err(|e| e.to_string())?;
        if !info.is_file() || info.len() > MAX_LOGO_BYTES as u64 {
            return Err("invalid asset file".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_LOGO_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if media_type(&bytes)? != asset.media_type
            || format!("{:x}", Sha256::digest(&bytes)) != asset.sha256
        {
            return Err("asset digest or media type mismatch".into());
        }
        Ok(bytes)
    }
    pub fn reconcile(&self, asset: &AssetRef) -> Result<(), String> {
        let bytes = self.read(asset)?;
        let _pins = self.pins.lock().map_err(|e| e.to_string())?;
        self.prepare()?;
        atomic(
            &self.directory,
            &format!("{}.{}", asset.sha256, extension(&asset.media_type)?),
            &bytes,
        )
    }
    /// Preserve all current/previous document assets, including other domains.
    pub fn collect(
        &self,
        current: &DesiredState,
        previous: Option<&DesiredState>,
    ) -> Result<(), String> {
        let pins = self.pins.lock().map_err(|e| e.to_string())?;
        if !self.directory.exists() {
            return Ok(());
        }
        let mut keep: BTreeSet<String> = pins.keys().cloned().collect();
        fn references(value: &Value, keep: &mut BTreeSet<String>) {
            match value {
                Value::Object(map) => {
                    if let Some(hash) = map
                        .get("sha256")
                        .and_then(Value::as_str)
                        .filter(|h| valid_hash(h))
                    {
                        keep.insert(hash.into());
                    }
                    for v in map.values() {
                        references(v, keep);
                    }
                }
                Value::Array(values) => {
                    for v in values {
                        references(v, keep);
                    }
                }
                _ => (),
            }
        }
        references(
            &serde_json::to_value(current).map_err(|e| e.to_string())?,
            &mut keep,
        );
        if let Some(previous) = previous {
            references(
                &serde_json::to_value(previous).map_err(|e| e.to_string())?,
                &mut keep,
            );
        }
        for entry in fs::read_dir(&self.directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let hash = name
                .strip_suffix(".png")
                .or_else(|| name.strip_suffix(".jpg"))
                .unwrap_or(name);
            if valid_hash(hash)
                && !keep.contains(hash)
                && entry.file_type().map_err(|e| e.to_string())?.is_file()
            {
                fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
            }
        }
        File::open(&self.directory)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    }
    pub fn collect_persisted(&self, state: &StateFile, state_dir: &Path) -> Result<(), String> {
        let previous = match fs::read(state_dir.join("state.prev.json")) {
            Ok(bytes) => {
                Some(serde_json::from_slice::<StateFile>(&bytes).map_err(|e| e.to_string())?)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.to_string()),
        };
        self.collect(&state.document, previous.as_ref().map(|s| &s.document))
    }
    /// Bootstrap only: adopt the exact configured regular legacy image.
    pub fn import_legacy(&self, state: &mut StateFile, legacy_root: &Path) -> Result<(), String> {
        if state.document.display.splash.logo.is_some() {
            return Ok(());
        }
        let path = state
            .legacy_files
            .get(ndimon_render::display::SPLASH_PATH)
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .and_then(|s| s["logo_path"].as_str().map(str::to_string));
        let Some(path) = path.filter(|p| {
            matches!(
                p.as_str(),
                "/etc/ndi-splash-logo.png"
                    | "/etc/ndi-splash-logo.jpg"
                    | "/etc/ndi-splash-logo.jpeg"
            )
        }) else {
            return Ok(());
        };
        let path = legacy_root.join(path.trim_start_matches('/'));
        let info = match fs::symlink_metadata(&path) {
            Ok(info) => info,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.to_string()),
        };
        if !info.is_file() || info.len() > MAX_LOGO_BYTES as u64 {
            return Err("invalid configured legacy logo".into());
        }
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        state.document.display.splash.logo = Some(self.put(&bytes)?.asset.clone());
        Ok(())
    }
}
