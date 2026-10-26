//! Private durable upload/snapshot storage. Secret-bearing backups are 0600.
use serde_json::Value;
use serde_json::json;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
pub fn random(bytes: usize) -> Result<String, String> {
    let mut data = vec![0; bytes];
    getrandom::fill(&mut data).map_err(|e| e.to_string())?;
    Ok(data.iter().map(|v| format!("{v:02x}")).collect())
}
pub fn directory(path: &Path) -> Result<(), String> {
    if !path.exists() {
        fs::create_dir_all(path).map_err(|e| e.to_string())?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    let m = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err("unsafe backup directory".into());
    }
    Ok(())
}
pub fn write(path: &Path, value: &Value) -> Result<(), String> {
    write_bytes(path, &ndimon_render::node_json(value.clone())?)
}
pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("backup parent missing")?;
    directory(parent)?;
    let tmp = parent.join(format!(".backup-{}", random(16)?));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}
pub fn read_bytes(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > max {
        return Err("unsafe backup file".into());
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > max {
        return Err("backup file too large".into());
    }
    Ok(bytes)
}
pub fn read(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&read_bytes(path, 8 * 1024 * 1024)?)
        .map_err(|_| "snapshot is unreadable".into())
}
pub fn expired(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_none_or(|age| age > Duration::from_secs(900))
}
pub fn sweep(path: &Path) {
    if let Ok(entries) = fs::read_dir(path) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().ends_with(".json") && expired(&e.path()) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}
pub fn snapshots(path: &Path) -> Vec<Value> {
    let mut entries = Vec::new();
    if let Ok(dir) = fs::read_dir(path) {
        for e in dir.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with("pre-restore-") || !name.ends_with(".json") {
                continue;
            }
            if let Ok(m) = fs::symlink_metadata(e.path()) {
                if m.is_file() {
                    if let Ok(t) = m.modified() {
                        entries.push(json!({"name":name,"bytes":m.len(),"mtime":iso(t)}));
                    }
                }
            }
        }
    }
    entries.sort_by(|a, b| b["mtime"].as_str().cmp(&a["mtime"].as_str()));
    entries
}
pub fn prune(path: &Path) {
    // Keep filesystem timestamp precision; ISO review timestamps round to ms.
    let mut entries = snapshots(path)
        .into_iter()
        .filter_map(|v| {
            let name = v["name"].as_str()?.to_string();
            let modified = fs::metadata(path.join(&name)).ok()?.modified().ok()?;
            Some((modified, name))
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (_, name) in entries.into_iter().skip(5) {
        let _ = fs::remove_file(path.join(name));
    }
}
/// Gregorian UTC conversion; avoids invoking a program for timestamp formatting.
pub fn iso(t: SystemTime) -> String {
    let duration = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = duration.as_secs();
    let days = (seconds / 86400) as i64;
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60,
        duration.subsec_millis()
    )
}
pub fn stamp() -> String {
    iso(SystemTime::now())
        .chars()
        .filter(|c| c.is_ascii_digit())
        .take(12)
        .collect::<String>()
        .chars()
        .enumerate()
        .flat_map(|(i, c)| if i == 8 { vec!['-', c] } else { vec![c] })
        .collect()
}
