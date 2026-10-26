//! A pending marker makes the temporary publication required by privd
//! recoverable. state.prev.json always contains the last committed revision.
use ndimon_render::LegacyFiles;
use ndimon_types::{DesiredState, Domain, NetworkBackendMetadata};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions, Permissions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateFile {
    pub schema_version: u32,
    pub revision: u64,
    pub document: DesiredState,
    pub domain_revisions: BTreeMap<Domain, u64>,
    pub legacy_files: LegacyFiles,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_commit_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_commit_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_backend_metadata: Option<NetworkBackendMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_job: Option<crate::domains::plugins::PendingJob>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub plugin_runtime: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugin_results: BTreeMap<String, crate::domains::plugins::JobResult>,
}
impl StateFile {
    pub fn new(document: DesiredState, legacy_files: LegacyFiles) -> Self {
        Self {
            schema_version: 1,
            revision: document.revision,
            domain_revisions: ndimon_render::OWNERSHIP
                .iter()
                .map(|(d, _)| (*d, document.revision))
                .collect(),
            document,
            legacy_files,
            network_commit_job_id: None,
            network_commit_revision: None,
            network_backend_metadata: None,
            plugin_job: None,
            plugin_runtime: serde_json::Map::new(),
            plugin_results: BTreeMap::new(),
        }
    }
    pub fn domain_revision(&self, domain: Domain) -> u64 {
        self.domain_revisions.get(&domain).copied().unwrap_or(0)
    }
    pub fn audio_revision(&self) -> u64 {
        self.domain_revision(Domain::Audio)
    }
    pub fn check(&self) -> Result<(), String> {
        if self.schema_version != 1
            || self.document.revision != self.revision
            || self.domain_revisions.values().any(|r| *r > self.revision)
            || self
                .network_commit_revision
                .is_some_and(|r| r > self.revision)
        {
            return Err("inconsistent state envelope".into());
        }
        ndimon_types::validate(&self.document).map_err(|errors| {
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        })
    }
}
#[derive(Debug, Clone)]
pub struct Persistence {
    pub directory: PathBuf,
}
static TEMP: AtomicU64 = AtomicU64::new(1);
impl Persistence {
    /// The calling process owns this directory/files. Production runs as
    /// ndimon-control; root import must hand ownership over before startup.
    pub fn new(directory: impl Into<PathBuf>) -> Result<Self, String> {
        let directory = directory.into();
        fs::create_dir_all(&directory).map_err(err)?;
        if fs::symlink_metadata(&directory)
            .map_err(err)?
            .file_type()
            .is_symlink()
        {
            return Err("linked state directory".into());
        }
        fs::set_permissions(&directory, Permissions::from_mode(0o750)).map_err(err)?;
        Ok(Self { directory })
    }
    pub fn load(&self) -> Result<Option<StateFile>, String> {
        self.remove_orphans();
        if self.directory.join("state.pending").exists() {
            let previous = fs::read(self.directory.join("state.prev.json")).map_err(err)?;
            let state: StateFile = serde_json::from_slice(&previous).map_err(err)?;
            state.check()?;
            self.write("state.json", &previous)?;
            self.remove_pending()?;
        }
        match fs::read(self.directory.join("state.json")) {
            Ok(bytes) => {
                let state: StateFile = serde_json::from_slice(&bytes).map_err(err)?;
                state.check()?;
                Ok(Some(state))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(err(e)),
        }
    }
    pub fn initialize(&self, state: &StateFile) -> Result<(), String> {
        self.write("state.json", &serde_json::to_vec(state).map_err(err)?)
    }
    pub fn stage(&self, previous: &StateFile, next: &StateFile) -> Result<(), String> {
        self.write(
            "state.prev.json",
            &serde_json::to_vec(previous).map_err(err)?,
        )?;
        self.write("state.pending", next.revision.to_string().as_bytes())?;
        self.write("state.json", &serde_json::to_vec(next).map_err(err)?)
    }
    /// Removing and fsyncing the pending marker is the commit point.
    pub fn commit(&self) -> Result<(), String> {
        self.remove_pending()
    }
    pub fn rollback(&self, previous: &StateFile) -> Result<(), String> {
        self.initialize(previous)?;
        self.remove_pending()
    }
    /// A writer that died between creating its temporary file and renaming it
    /// leaves `.state-<pid>-<n>.tmp` behind. The pid in the name says whose it
    /// is: only files of processes that no longer exist are removed, never this
    /// process's own (another thread may be writing) or a live writer's.
    fn remove_orphans(&self) {
        if !Path::new("/proc/self").exists() {
            return;
        }
        let Ok(entries) = fs::read_dir(&self.directory) else {
            return;
        };
        for entry in entries.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(temporary_pid) else {
                continue;
            };
            if pid != std::process::id() && !Path::new("/proc").join(pid.to_string()).exists() {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    fn remove_pending(&self) -> Result<(), String> {
        match fs::remove_file(self.directory.join("state.pending")) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(err(e)),
        }
        File::open(&self.directory)
            .and_then(|f| f.sync_all())
            .map_err(err)
    }
    pub fn write(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        atomic(&self.directory, name, bytes)
    }
}
/// The writer's pid of a name made by `atomic`.
fn temporary_pid(name: &str) -> Option<u32> {
    name.strip_prefix(".state-")?
        .strip_suffix(".tmp")?
        .split_once('-')?
        .0
        .parse()
        .ok()
}
pub fn atomic(directory: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    if name.contains('/') || name == "." || name == ".." {
        return Err("invalid state basename".into());
    }
    let target = directory.join(name);
    if let Ok(info) = fs::symlink_metadata(&target) {
        if !info.is_file() || info.file_type().is_symlink() {
            return Err("nonregular state destination".into());
        }
    }
    let temporary = directory.join(format!(
        ".state-{}-{}.tmp",
        std::process::id(),
        TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(err)?;
        f.set_permissions(Permissions::from_mode(0o640))
            .map_err(err)?;
        f.write_all(bytes)
            .and_then(|()| f.sync_all())
            .map_err(err)?;
        fs::rename(&temporary, &target).map_err(err)?;
        File::open(directory)
            .and_then(|f| f.sync_all())
            .map_err(err)
    })();
    let _ = fs::remove_file(temporary);
    result
}
pub(crate) fn err(e: impl ToString) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_envelopes_accept_missing_network_decision_fields() {
        let state = StateFile::new(DesiredState::default(), LegacyFiles::new());
        let value = serde_json::to_value(&state).unwrap();
        assert!(value.get("network_commit_job_id").is_none());
        assert_eq!(serde_json::from_value::<StateFile>(value).unwrap(), state);
        let mut state = state;
        state.network_commit_revision = Some(1);
        assert!(state.check().is_err());
    }
    #[test]
    fn load_removes_the_temporary_files_of_dead_writers_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = Persistence::new(dir.path()).unwrap();
        store
            .initialize(&StateFile::new(DesiredState::default(), LegacyFiles::new()))
            .unwrap();
        // No process has this pid (it exceeds the kernel's pid limit).
        let dead = dir.path().join(format!(".state-{}-3.tmp", u32::MAX));
        let own = dir
            .path()
            .join(format!(".state-{}-9.tmp", std::process::id()));
        let parent = dir.path().join(format!(
            ".state-{}-2.tmp",
            std::os::unix::process::parent_id()
        ));
        let foreign = dir.path().join(".state-orphan.tmp");
        let unrelated = dir.path().join("rendered.json");
        for file in [&dead, &own, &parent, &foreign, &unrelated] {
            fs::write(file, b"incomplete").unwrap();
        }
        store.load().unwrap();
        assert!(!dead.exists());
        for kept in [&own, &parent, &foreign, &unrelated] {
            assert!(kept.exists(), "{kept:?}");
        }
    }
    #[test]
    fn temporary_names_carry_the_writer_pid() {
        assert_eq!(temporary_pid(".state-12-3.tmp"), Some(12));
        assert_eq!(temporary_pid(".state-orphan.tmp"), None);
        assert_eq!(temporary_pid(".network-12-3.tmp"), None);
        assert_eq!(temporary_pid(".state-12.tmp"), None);
        assert_eq!(temporary_pid("state-12-3.tmp"), None);
    }
    #[test]
    fn crash_with_temp_or_pending_keeps_committed_revision_and_prev() {
        let dir = tempfile::tempdir().unwrap();
        let store = Persistence::new(dir.path()).unwrap();
        let previous = StateFile::new(DesiredState::default(), LegacyFiles::new());
        store.initialize(&previous).unwrap();
        fs::write(dir.path().join(".state-orphan.tmp"), b"incomplete").unwrap();
        assert_eq!(store.load().unwrap().unwrap(), previous);
        let mut next = previous.clone();
        next.revision = 1;
        next.document.revision = 1;
        store.stage(&previous, &next).unwrap();
        assert_eq!(store.load().unwrap().unwrap(), previous);
        let kept: StateFile =
            serde_json::from_slice(&fs::read(dir.path().join("state.prev.json")).unwrap()).unwrap();
        assert_eq!(kept, previous);
        assert_eq!(
            fs::metadata(dir.path().join("state.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert_eq!(
            fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }
}
