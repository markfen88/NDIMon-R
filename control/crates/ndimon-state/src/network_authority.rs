//! The engine's commit is one replacement of the actor's state envelope.
//! Actor writers must hold `DecisionLock` over their complete transaction. An
//! external job never waits inside the authority (it is also called from within
//! an actor transaction): it is refused while a writer holds the lock and
//! retries. With the lock free, a staged publication belongs to a writer that
//! died, and is rolled back as `Persistence::load` would.
use crate::persist::{Persistence, StateFile};
use ndimon_netengine::{Authority, Commit};
use ndimon_types::{ApiError, Domain};
use nix::{
    fcntl::{Flock, FlockArg},
    unistd::{Gid, Uid, fchown, geteuid},
};
use std::{
    fs::{self, File, OpenOptions, Permissions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
};

pub type DecisionLock = Flock<File>;
fn failed(reason: impl ToString) -> ApiError {
    ApiError::ApplyFailed {
        domain: Domain::Network,
        step: "authority".into(),
        reason: reason.to_string(),
    }
}
/// Shared with every actor turn. Root jobs preserve the actor directory owner.
pub fn lock(store: &Persistence) -> Result<DecisionLock, ApiError> {
    lock_with(store, FlockArg::LockExclusive)
}
fn lock_with(store: &Persistence, mode: FlockArg) -> Result<DecisionLock, ApiError> {
    let info = fs::symlink_metadata(&store.directory).map_err(failed)?;
    if !info.is_dir() || info.mode() & 0o022 != 0 {
        return Err(failed("unsafe_state_directory"));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(store.directory.join("state.lock"))
        .map_err(failed)?;
    let m = file.metadata().map_err(failed)?;
    if !m.is_file()
        || m.nlink() != 1
        || m.mode() & 0o077 != 0
        || (m.uid() != info.uid() && m.uid() != geteuid().as_raw())
    {
        return Err(failed("unsafe_state_lock"));
    }
    if geteuid().is_root() {
        fchown(
            file.as_raw_fd(),
            Some(Uid::from_raw(info.uid())),
            Some(Gid::from_raw(info.gid())),
        )
        .map_err(failed)?;
    }
    Flock::lock(file, mode).map_err(|(_, e)| {
        if e == nix::errno::Errno::EWOULDBLOCK {
            ApiError::Locked {
                field: "network".into(),
                by: Some("state_transaction".into()),
            }
        } else {
            failed(e)
        }
    })
}
#[derive(Debug, Clone)]
pub struct StateAuthority {
    pub persistence: Persistence,
}
impl StateAuthority {
    pub fn new(persistence: Persistence) -> Self {
        Self { persistence }
    }
    /// Callers hold the decision lock, and an actor transaction holds it from
    /// `stage` to `commit`: a marker that is still there was left by a writer
    /// that died in between. Roll it back exactly as `Persistence::load` does
    /// when control starts, keeping the owner of the files control reads.
    fn recover(&self) -> Result<(), ApiError> {
        let dir = &self.persistence.directory;
        if !dir.join("state.pending").try_exists().map_err(failed)? {
            return Ok(());
        }
        let previous = fs::read(dir.join("state.prev.json")).map_err(failed)?;
        serde_json::from_slice::<StateFile>(&previous)
            .map_err(failed)?
            .check()
            .map_err(failed)?;
        self.publish(&previous)?;
        match fs::remove_file(dir.join("state.pending")) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(failed(e)),
        }
        File::open(dir).and_then(|d| d.sync_all()).map_err(failed)
    }
    fn read(&self) -> Result<StateFile, ApiError> {
        self.recover()?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .open(self.persistence.directory.join("state.json"))
            .map_err(failed)?;
        let metadata = file.metadata().map_err(failed)?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.len() > 4 * 1024 * 1024
            || metadata.mode() & 0o022 != 0
        {
            return Err(failed("unsafe_state_file"));
        }
        let mut bytes = Vec::new();
        file.take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(failed)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(failed("state_too_large"));
        }
        let state: StateFile = serde_json::from_slice(&bytes).map_err(failed)?;
        state.check().map_err(failed)?;
        Ok(state)
    }
    pub fn decision(state: &StateFile) -> Commit {
        Commit {
            version: 1,
            revision: state.revision,
            profiles: state.document.network.clone(),
            metadata: state.network_backend_metadata.clone().unwrap_or_default(),
            transaction: state.network_commit_job_id.clone(),
            network_commit_revision: state.network_commit_revision,
        }
    }
    fn replace(&self, state: &StateFile) -> Result<(), ApiError> {
        self.publish(&serde_json::to_vec(state).map_err(failed)?)
    }
    fn publish(&self, bytes: &[u8]) -> Result<(), ApiError> {
        let dir = &self.persistence.directory;
        let owner = fs::metadata(dir).map_err(failed)?;
        let temporary = dir.join(format!(
            ".network-{}.tmp",
            ndimon_netengine::store::random_id()?
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(nix::libc::O_NOFOLLOW)
                .open(&temporary)
                .map_err(failed)?;
            if geteuid().is_root() {
                fchown(
                    file.as_raw_fd(),
                    Some(Uid::from_raw(owner.uid())),
                    Some(Gid::from_raw(owner.gid())),
                )
                .map_err(failed)?;
            }
            file.set_permissions(Permissions::from_mode(0o640))
                .map_err(failed)?;
            file.write_all(bytes).map_err(failed)?;
            file.sync_all().map_err(failed)?;
            #[cfg(feature = "harness")]
            if dir == std::path::Path::new("/var/lib/ndimon/state") {
                ndimon_netengine::store::Root::production()
                    .checkpoint("/var/lib/ndimon/state/state.json", "rename")?;
            }
            fs::rename(&temporary, dir.join("state.json")).map_err(failed)?;
            #[cfg(feature = "harness")]
            if dir == std::path::Path::new("/var/lib/ndimon/state") {
                ndimon_netengine::store::Root::production()
                    .checkpoint("/var/lib/ndimon/state/state.json", "directory_fsync")?;
            }
            File::open(dir).and_then(|d| d.sync_all()).map_err(failed)
        })();
        let _ = fs::remove_file(temporary);
        result
    }
}
impl Authority for StateAuthority {
    fn load(&self) -> Result<Commit, ApiError> {
        // Status can be requested by replay inside an actor transaction. A
        // reader must refuse that reservation, never wait on its RPC caller.
        let _lock = lock_with(&self.persistence, FlockArg::LockExclusiveNonblock)?;
        Ok(Self::decision(&self.read()?))
    }
    fn compare_and_commit(&self, expected: &Commit, replacement: &Commit) -> Result<(), ApiError> {
        let _lock = lock(&self.persistence)?;
        let mut state = self.read()?;
        if Self::decision(&state) != *expected {
            return Err(ApiError::RevisionConflict {
                current_revision: state.revision,
            });
        }
        if replacement.version != 1
            || expected.revision.checked_add(1) != Some(replacement.revision)
            || replacement.revision > 9_007_199_254_740_991
            || replacement.network_commit_revision != Some(replacement.revision)
            || replacement
                .transaction
                .as_ref()
                .is_none_or(|id| !ndimon_netengine::store::valid_id(id))
        {
            return Err(failed("invalid_network_decision"));
        }
        state.document.network = replacement.profiles.clone();
        state.revision = replacement.revision;
        state.document.revision = replacement.revision;
        state
            .domain_revisions
            .insert(Domain::Network, replacement.revision);
        state.network_commit_job_id = replacement.transaction.clone();
        state.network_commit_revision = replacement.network_commit_revision;
        state.network_backend_metadata = Some(replacement.metadata.clone());
        state.check().map_err(failed)?;
        self.replace(&state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndimon_types::DesiredState;
    fn fixture() -> (tempfile::TempDir, StateAuthority) {
        let temp = tempfile::tempdir().unwrap();
        let p = Persistence::new(temp.path()).unwrap();
        p.initialize(&StateFile::new(DesiredState::default(), Default::default()))
            .unwrap();
        (temp, StateAuthority::new(p))
    }
    #[test]
    fn exact_cas_persists_one_complete_decision_and_preserves_other_domains() {
        let (_temp, a) = fixture();
        let before = a.load().unwrap();
        let mut next = before.clone();
        next.revision = 1;
        next.network_commit_revision = Some(1);
        next.transaction = Some("a".repeat(64));
        next.profiles.interface = "eth0".into();
        next.profiles.backend = Some(ndimon_types::NetworkBackend::Networkd);
        next.profiles
            .networks
            .push(ndimon_types::NetworkProfile::dhcp("base", "Management"));
        next.profiles.roles.management = "base".into();
        next.profiles.roles.ndi = "base".into();
        next.profiles.roles.audio = "base".into();
        next.metadata.dhcp_metric = 300;
        a.compare_and_commit(&before, &next).unwrap();
        assert_eq!(a.load().unwrap(), next);
        let state = a.read().unwrap();
        assert_eq!(state.domain_revision(Domain::Network), 1);
        assert_eq!(state.document.audio, DesiredState::default().audio);
        assert!(!a.persistence.directory.join("committed.json").exists());
        assert!(matches!(
            a.compare_and_commit(&before, &next),
            Err(ApiError::RevisionConflict {
                current_revision: 1
            })
        ));
        let mut unrelated = state;
        unrelated.revision = 2;
        unrelated.document.revision = 2;
        a.persistence.initialize(&unrelated).unwrap();
        let decision = a.load().unwrap();
        assert_eq!(decision.network_commit_revision, Some(1));
        assert_eq!(decision.transaction, next.transaction);
    }
    #[test]
    fn load_refuses_the_actor_lock_without_waiting_or_recovering() {
        let (_temp, a) = fixture();
        let state = a.read().unwrap();
        let decision_lock = lock(&a.persistence).unwrap();
        assert!(
            matches!(a.load(), Err(ApiError::Locked { by: Some(by), .. }) if by == "state_transaction")
        );
        a.persistence.stage(&state, &state).unwrap();
        assert!(matches!(a.load(), Err(ApiError::Locked { .. })));
        assert!(a.persistence.directory.join("state.pending").exists());
        a.persistence.rollback(&state).unwrap();
        drop(decision_lock);
        assert_eq!(a.load().unwrap(), StateAuthority::decision(&state));
    }
    #[test]
    fn a_marker_left_by_a_dead_writer_is_rolled_back_like_persistence_load() {
        let (_temp, a) = fixture();
        let state = a.read().unwrap();
        let mut next = state.clone();
        next.revision = 1;
        next.document.revision = 1;
        // The writer staged revision 1 and died before its commit: no lock is
        // held, the marker and the staged envelope remain.
        a.persistence.stage(&state, &next).unwrap();
        let before = fs::metadata(a.persistence.directory.join("state.json")).unwrap();
        assert_eq!(a.load().unwrap(), StateAuthority::decision(&state));
        assert!(!a.persistence.directory.join("state.pending").exists());
        let after = fs::metadata(a.persistence.directory.join("state.json")).unwrap();
        assert_eq!(a.read().unwrap(), state);
        assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
        assert_eq!(after.mode() & 0o777, 0o640);
        // The same recovery as the control plane's own start-up, so nothing
        // is left for it to roll back again.
        assert_eq!(a.persistence.load().unwrap().unwrap(), state);
    }
    #[test]
    fn a_marker_without_its_previous_envelope_stays_and_fails_closed() {
        let (_temp, a) = fixture();
        let state = a.read().unwrap();
        a.persistence.stage(&state, &state).unwrap();
        fs::remove_file(a.persistence.directory.join("state.prev.json")).unwrap();
        assert!(matches!(a.load(), Err(ApiError::ApplyFailed { .. })));
        assert!(a.persistence.directory.join("state.pending").exists());
    }
    #[test]
    fn a_live_writer_keeps_its_marker_until_it_releases_the_lock() {
        let (_temp, a) = fixture();
        let state = a.read().unwrap();
        a.persistence.stage(&state, &state).unwrap();
        let held = lock(&a.persistence).unwrap();
        assert!(matches!(a.load(), Err(ApiError::Locked { .. })));
        assert!(a.persistence.directory.join("state.pending").exists());
        drop(held);
        a.load().unwrap();
        assert!(!a.persistence.directory.join("state.pending").exists());
    }
}
