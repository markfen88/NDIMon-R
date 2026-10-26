//! NE-6/7: serialized write-ahead transactions with one authoritative commit.
use crate::backend::{self, Foreign, Metadata, Plan};
use crate::host::{Host, SessionPath, Tool};
use crate::store::{Root, Secret, Store, random_id, valid_id};
use crate::validation::{self, assigned, ifname, mapped};
use crate::{
    APPLY_MS, BANNER, CONFIRM_MS, GUARD_MS, PRIVD, Result, failed, invalid, locked, unsupported,
};
use ndimon_types::{ApiError, ConfirmPolicy, NetworkBackend, NetworkProfiles};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

/// The state actor/privd must implement this against ONE durable state decision.
/// compare_and_commit atomically replaces the full network section, metadata,
/// transaction identity and global revision, or leaves them all unchanged.
/// Do not implement this as a second write after updating state.json. See README.
pub trait Authority: std::fmt::Debug + Send + Sync {
    fn load(&self) -> Result<Commit>;
    fn compare_and_commit(&self, expected: &Commit, replacement: &Commit) -> Result<()>;
}
/// Thin persistence extension to the shared document. No duplicate row model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commit {
    pub version: u32,
    pub revision: u64,
    pub profiles: NetworkProfiles,
    pub metadata: Metadata,
    pub transaction: Option<String>,
    /// Preserve this decision revision across unrelated desired-state writes.
    #[serde(default)]
    pub network_commit_revision: Option<u64>,
}
impl Default for Commit {
    fn default() -> Self {
        Self {
            version: 1,
            revision: 0,
            profiles: NetworkProfiles::default(),
            metadata: Metadata::default(),
            transaction: None,
            network_commit_revision: None,
        }
    }
}

pub use ndimon_types::{NetworkPhase as Phase, PendingNetwork as Pending};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Operation {
    Network,
    Handoff,
    SyncTessera,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    operation: Operation,
    job_id: String,
    token: Secret,
    base: Commit,
    plan: Plan,
    phase: Phase,
    boot_id: String,
    deadline_ms: u64,
    applied_ms: Option<u64>,
    management_address: Option<Ipv4Addr>,
    recovery_pending: bool,
    outcome: Phase,
    apply_error: Option<ApiError>,
    recovery_error: Option<ApiError>,
    #[serde(with = "crate::host::session_journal")]
    session: Option<SessionPath>,
    timer_generation: u32,
    #[serde(default)]
    handoff: Option<crate::handoff::Transfer>,
    #[serde(default)]
    handoff_restored: bool,
    /// A failed recovery started the media units again (NI-17 D14). The retry
    /// that follows must not stop them every ten seconds; the one that finally
    /// succeeds restarts them once, on the restored network.
    #[serde(default)]
    media_running: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Last {
    token: Secret,
    phase: Phase,
    apply_error: Option<ApiError>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkObserved {
    pub id: String,
    pub interface: String,
    pub addresses: Vec<(Ipv4Addr, u8)>,
    pub used_by: Vec<String>,
}
/// Why first adoption was refused (NE-3). `reason` is the stable code, `detail`
/// the offending key and file where the backend could name them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Refusal {
    pub reason: String,
    pub detail: String,
}
impl Refusal {
    fn from_unsupported(what: &str) -> Self {
        let (reason, detail) = what.split_once(": ").unwrap_or((what, ""));
        Self {
            reason: reason.into(),
            detail: detail.into(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub revision: u64,
    pub domain_revision: u64,
    pub profiles: NetworkProfiles,
    pub networks: Vec<NetworkObserved>,
    pub pending: Option<Pending>,
    pub last_phase: Option<Phase>,
    pub last_error: Option<ApiError>,
    pub drift: Vec<Foreign>,
    pub writable: bool,
    pub externally_managed: bool,
    pub quiesced: bool,
    pub stale_done: Vec<String>,
    /// Set when the host cannot be adopted: `profiles` is then unadopted, and
    /// `networks` holds only the live addressing of the port, read-only.
    #[serde(default)]
    pub refusal: Option<Refusal>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preview {
    pub profiles: NetworkProfiles,
    pub unchanged: bool,
    pub affected_roles: Vec<String>,
    pub removed_networks: Vec<String>,
    pub interruptions: Vec<String>,
    pub confirm_policy: ConfirmPolicy,
    pub confirm_seconds: u64,
    pub drift: Vec<Foreign>,
    pub changed_paths: Vec<String>,
    pub writable: bool,
}
/// What `accept_current` gave up: the errors of the failed change and of its
/// failed recovery, for the operator's record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Accepted {
    pub apply_error: Option<ApiError>,
    pub recovery_error: Option<ApiError>,
    /// What could not be put back. It still holds what the failed change wrote,
    /// so the operator has to see it (VV-3 W5).
    #[serde(default)]
    pub unrestored: Vec<Unrestored>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unrestored {
    pub path: String,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stage {
    pub unchanged: bool,
    pub pending: Option<Pending>,
    pub token: Option<Secret>,
}
#[derive(Debug, Clone, Default)]
pub struct EngineConfig {
    /// Populate only after this backend/host combination passes NE-11's live
    /// lifecycle. Empty by default; mocked tests are not certification.
    pub certified_backends: BTreeSet<NetworkBackend>,
    pub selected_interface: Option<String>,
}
#[derive(Debug, Clone)]
pub struct Engine {
    pub host: Host,
    pub authority: Arc<dyn Authority>,
    pub config: EngineConfig,
    store: Store,
}
impl Engine {
    pub fn new(host: Host, authority: Arc<dyn Authority>, config: EngineConfig) -> Self {
        let store = Store {
            root: host.root.clone(),
        };
        Self {
            host,
            authority,
            config,
            store,
        }
    }
    fn current(&self) -> Result<Commit> {
        self.current_with(true)
    }
    /// `adopt` runs first-adoption discovery for a network nothing has been
    /// committed for. Discovery is the expensive part of a status (a dozen
    /// backend tools, `netplan get` among them); an observation of an
    /// uncommitted network has no use for it.
    fn current_with(&self, adopt: bool) -> Result<Commit> {
        if crate::migration::needed(&self.host)? {
            return Err(locked("legacy_network_recovery"));
        }
        let mut c = self.authority.load()?;
        if c.version != 1 {
            return Err(invalid("network.version", "invalid_version_revision"));
        }
        if c.profiles.backend.is_none() {
            if let Some(j) = self.store.read::<Journal>("pending")? {
                c.profiles = j.plan.previous;
                c.metadata = j.plan.previous_metadata;
                return Ok(c);
            }
            if !adopt {
                return Ok(c);
            }
            let (profiles, metadata) =
                self.host.adopt(self.config.selected_interface.as_deref())?;
            c.profiles = profiles;
            c.metadata = metadata;
        } else {
            validation::validate(&c.profiles, &c.profiles)?;
        }
        Ok(c)
    }
    fn idle(&self) -> Result<()> {
        crate::handoff::writable(self)?;
        if self.store.read::<serde_json::Value>("handoff")?.is_some() {
            return Err(locked("pending_transaction"));
        }
        if crate::migration::needed(&self.host)? {
            return Err(locked("legacy_network_recovery"));
        }
        if self.store.read::<Journal>("pending")?.is_some() {
            return Err(locked("pending_transaction"));
        }
        // Legacy journals must be explicitly recovered by the migration worker;
        // never run either Python helper or silently reinterpret their state.
        if self
            .host
            .root
            .path("/var/lib/ndimon/net/pending/state.json")?
            .exists()
        {
            return Err(locked("legacy_network_recovery"));
        }
        Ok(())
    }
    fn certified(&self, p: &NetworkProfiles) -> bool {
        p.backend
            .is_some_and(|b| self.config.certified_backends.contains(&b))
    }
    fn public(&self, j: &Journal) -> Result<Pending> {
        let remaining = if self.host.clock.boot_id()? == j.boot_id {
            j.deadline_ms.saturating_sub(self.host.clock.now_ms()?)
        } else {
            0
        };
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| failed("status", "deadline_format"))?
            .as_secs()
            .saturating_add(remaining.div_ceil(1000));
        let expires = chrono::DateTime::<chrono::Utc>::from_timestamp(
            i64::try_from(wall).map_err(|_| failed("status", "deadline_format"))?,
            0,
        )
        .ok_or_else(|| failed("status", "deadline_format"))?
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut pending = Pending::new(j.base.revision.saturating_add(1), expires, j.phase);
        pending.job_id = j.job_id.clone();
        pending.remaining_seconds = remaining.div_ceil(1000);
        pending.confirm_policy = j.plan.candidate.confirm_policy;
        pending.management_address = j.management_address;
        pending.management_port = j.plan.portal_port;
        pending.recovery_pending = j.recovery_pending;
        pending.apply_error = j.apply_error.clone();
        pending.recovery_error = j.recovery_error.clone();
        Ok(pending)
    }

    pub fn status(&self) -> Result<Status> {
        self.observe_with(true)
    }
    /// What the control plane polls: the same as `status`, except that a
    /// network no one has committed is neither adopted nor probed for a
    /// standalone Tessera (so no tool runs for it). The profiles of such a
    /// status are the empty default, and nothing that polls acts on them.
    pub fn observe(&self) -> Result<Status> {
        self.observe_with(false)
    }
    fn observe_with(&self, adopt: bool) -> Result<Status> {
        let mut release = crate::handoff::release_status(self)?;
        if release.externally_managed && release.quiesced {
            let _lock = self.store.lock()?;
            release = crate::handoff::complete_release(self)?;
        }
        let j = self.store.read::<Journal>("pending")?;
        let saved = self.authority.load()?;
        // After handoff recovery, R owns the live VLANs again and A has no
        // committed network. Observe that ownership instead of attempting an
        // ordinary first-adoption discovery over R's restored configuration.
        let source_owned = if adopt && j.is_none() && saved.profiles.backend.is_none() {
            match self.host.ownership() {
                Err(ApiError::Locked { by: Some(by), .. }) if by == "tessera_standalone" => true,
                Ok(()) => false,
                Err(error) => return Err(error),
            }
        } else {
            false
        };
        let c = match j.as_ref() {
            // Use the same journal snapshot throughout this observation. Its
            // cleanup may race a second read inside first-adoption discovery.
            Some(j) if saved.profiles.backend.is_none() => Commit {
                profiles: j.plan.previous.clone(),
                metadata: j.plan.previous_metadata.clone(),
                ..saved
            },
            _ if release.externally_managed || source_owned => saved,
            _ => match self.current_with(adopt) {
                Ok(c) => c,
                // Reads stay available when adoption is refused: show what is
                // live, read-only, with the reason. Mutations still fail in
                // `prepare` with the same refusal.
                Err(ApiError::Unsupported { what }) => {
                    return Ok(self.refused(&what, saved, release.quiesced, release.stale_done));
                }
                Err(error) => return Err(error),
            },
        };
        let last = self.store.read::<Last>("last")?;
        if release.externally_managed || source_owned {
            return Ok(Status {
                revision: c.revision,
                domain_revision: c.network_commit_revision.unwrap_or(0),
                profiles: c.profiles,
                networks: Vec::new(),
                pending: None,
                last_phase: last.as_ref().map(|l| l.phase),
                last_error: last.and_then(|l| l.apply_error),
                drift: Vec::new(),
                writable: false,
                externally_managed: true,
                quiesced: release.quiesced,
                stale_done: release.stale_done,
                refusal: None,
            });
        }
        let observed = if let Some(j) = &j {
            &j.plan.candidate
        } else {
            &c.profiles
        };
        let observed_metadata = if let Some(j) = &j {
            j.plan.transitional_metadata()
        } else {
            c.metadata.clone()
        };
        // Nothing to report addresses for without a network.
        let live = if observed.networks.is_empty() {
            serde_json::Value::Null
        } else {
            self.host.json(Tool::Ip, &["-j", "-4", "address", "show"])?
        };
        let mut networks = Vec::new();
        for n in &observed.networks {
            let name = ifname(observed, n);
            let addresses = live
                .as_array()
                .into_iter()
                .flatten()
                .filter(|v| v["ifname"] == name)
                .filter_map(|v| v["addr_info"].as_array())
                .flatten()
                .filter(|a| a["family"] == "inet" && a["scope"] == "global")
                .filter_map(|a| {
                    Some((
                        a["local"].as_str()?.parse().ok()?,
                        u8::try_from(a["prefixlen"].as_u64()?).ok()?,
                    ))
                })
                .collect();
            let used_by = [
                ("management", &observed.roles.management),
                ("ndi", &observed.roles.ndi),
                ("audio", &observed.roles.audio),
            ]
            .into_iter()
            .filter(|(_, id)| **id == n.id)
            .map(|(r, _)| r.into())
            .collect();
            networks.push(NetworkObserved {
                id: n.id.clone(),
                interface: name,
                addresses,
                used_by,
            });
        }
        let (drift, refusal) = match self.drift(observed, &observed_metadata) {
            Ok(drift) => (drift, None),
            // The OS configuration cannot be read safely (a writable directory on
            // the way to a networkd file, say). Keep the document and the live
            // addressing readable with the reason, as for a refused adoption;
            // every mutation fails in `prepare` with the same refusal.
            Err(ApiError::Unsupported { what }) => {
                (Vec::new(), Some(Refusal::from_unsupported(&what)))
            }
            Err(error) => return Err(error),
        };
        Ok(Status {
            revision: c.revision,
            domain_revision: c.network_commit_revision.unwrap_or(0),
            writable: self.certified(&c.profiles) && !release.quiesced && refusal.is_none(),
            externally_managed: false,
            quiesced: release.quiesced,
            stale_done: release.stale_done,
            profiles: c.profiles,
            networks,
            pending: j.as_ref().map(|j| self.public(j)).transpose()?,
            last_phase: last.as_ref().map(|l| l.phase),
            last_error: last.and_then(|l| l.apply_error),
            drift,
            refusal,
        })
    }
    fn drift(&self, observed: &NetworkProfiles, metadata: &Metadata) -> Result<Vec<Foreign>> {
        Ok(match observed.backend {
            Some(NetworkBackend::Networkd) => self.host.networkd_drift(observed)?,
            Some(NetworkBackend::Netplan) => {
                let mut drift = self.host.networkd_drift(observed)?;
                drift.extend(self.host.netplan_drift(observed)?);
                drift
            }
            Some(NetworkBackend::Networkmanager) => self.host.nm_drift(observed, metadata)?,
            Some(NetworkBackend::Ifupdown) => self.host.ifupdown_drift(observed)?,
            _ => Vec::new(),
        })
    }
    /// Unadopted status for a refused host (NE-3): no profiles, nothing writable,
    /// the port's live addressing, and the refusal that blocks every mutation.
    fn refused(
        &self,
        what: &str,
        saved: Commit,
        quiesced: bool,
        stale_done: Vec<String>,
    ) -> Status {
        let last = self.store.read::<Last>("last").ok().flatten();
        let mut profiles = NetworkProfiles::default();
        let mut networks = Vec::new();
        if let Some((interface, addresses)) = self
            .host
            .observe_port(self.config.selected_interface.as_deref())
        {
            profiles.interface = interface.clone();
            networks.push(NetworkObserved {
                id: "untagged".into(),
                interface,
                addresses,
                used_by: vec!["management".into()],
            });
        }
        Status {
            revision: saved.revision,
            domain_revision: saved.network_commit_revision.unwrap_or(0),
            profiles,
            networks,
            pending: None,
            last_phase: last.as_ref().map(|l| l.phase),
            last_error: last.and_then(|l| l.apply_error),
            drift: Vec::new(),
            writable: false,
            externally_managed: false,
            quiesced,
            stale_done,
            refusal: Some(Refusal::from_unsupported(what)),
        }
    }
    fn prepare(
        &self,
        candidate: &NetworkProfiles,
        revision: u64,
    ) -> Result<(Commit, Preview, Option<Plan>)> {
        self.idle()?;
        let old = self.current()?;
        if revision != old.revision {
            return Err(ApiError::RevisionConflict {
                current_revision: old.revision,
            });
        }
        let p = validation::validate(candidate, &old.profiles)?;
        let unchanged = p == old.profiles;
        let mut preview = Preview {
            profiles: p.clone(),
            unchanged,
            affected_roles: backend::affected_roles(&old.profiles, &p),
            removed_networks: old
                .profiles
                .networks
                .iter()
                .filter(|n| !p.networks.iter().any(|m| m.id == n.id))
                .map(|n| n.id.clone())
                .collect(),
            interruptions: Vec::new(),
            confirm_policy: p.confirm_policy,
            confirm_seconds: 60,
            drift: Vec::new(),
            changed_paths: Vec::new(),
            writable: self.certified(&p),
        };
        if unchanged {
            return Ok((old, preview, None));
        }
        if revision == u64::MAX {
            return Err(invalid("network.revision", "revision_overflow"));
        }
        let mut plan = self
            .host
            .plan(old.profiles.clone(), p, old.metadata.clone(), 80)?;
        // Reserve deterministic connection UUIDs before the journal. Created
        // connections are discoverable after an interrupted add/activation.
        for c in &plan.nm {
            if c.tag.is_some() && !c.remove_after_commit {
                plan.metadata
                    .nm_owned
                    .insert(c.interface.clone(), c.uuid.clone());
            }
            if c.remove_after_commit {
                plan.metadata.nm_owned.remove(&c.interface);
            }
        }
        preview.interruptions = plan.affected_services.clone();
        preview.drift = plan.drift.clone();
        preview.changed_paths = plan.files.iter().map(|c| c.path.clone()).collect();
        Ok((old, preview, Some(plan)))
    }
    /// Preflight only; no lock/store directories, backend files or timers created.
    pub fn preview(&self, candidate: &NetworkProfiles, expect_revision: u64) -> Result<Preview> {
        Ok(self.prepare(candidate, expect_revision)?.1)
    }
    pub fn stage(
        &self,
        candidate: &NetworkProfiles,
        expect_revision: u64,
        session: Option<SessionPath>,
    ) -> Result<Stage> {
        let (_, preview, _) = self.prepare(candidate, expect_revision)?;
        if preview.unchanged {
            return Ok(Stage {
                unchanged: true,
                pending: None,
                token: None,
            });
        }
        if !preview.writable {
            return Err(unsupported("backend_not_certified"));
        }
        // Before lock: a consumer start must never replay the boot oneshot
        // while a transaction is live (NE-6.14).
        if !self.host.active("ndimon-net-boot.service")? {
            self.host
                .run(Tool::Systemctl, &["start", "ndimon-net-boot.service"], 90)?;
        }
        let _lock = self.store.lock()?;
        let (_, preview, plan) = self.prepare(candidate, expect_revision)?;
        if preview.unchanged {
            return Ok(Stage {
                unchanged: true,
                pending: None,
                token: None,
            });
        }
        let plan = plan.ok_or_else(|| failed("stage", "missing_plan"))?;
        let base = self.authority.load()?;
        if base.revision != expect_revision {
            return Err(ApiError::RevisionConflict {
                current_revision: base.revision,
            });
        }
        self.stage_transaction(base, plan, session, None)
    }
    pub fn commit_handoff(
        &self,
        source: &ndimon_types::NetworkHandoff,
        expect_impact: &ndimon_types::ImpactToken,
        session: Option<SessionPath>,
    ) -> Result<Stage> {
        let _lock = self.store.lock()?;
        let _source_lock = crate::handoff::source_lock(&self.host, source)?;
        let (review, base, plan, transfer) = self.handoff_plan(source)?;
        if &review.impact.token != expect_impact {
            return Err(ApiError::ConfirmationRequired {
                impact: review.impact,
            });
        }
        self.stage_transaction(base, plan, session, Some(transfer))
    }
    fn stage_transaction(
        &self,
        base: Commit,
        plan: Plan,
        session: Option<SessionPath>,
        handoff: Option<crate::handoff::Transfer>,
    ) -> Result<Stage> {
        let token = Secret::generate()?;
        let j = Journal {
            version: 1,
            operation: if handoff.is_some() {
                Operation::Handoff
            } else {
                Operation::Network
            },
            job_id: random_id()?,
            token: token.clone(),
            base,
            plan,
            phase: Phase::Staged,
            boot_id: self.host.clock.boot_id()?,
            deadline_ms: self.host.clock.now_ms()?.saturating_add(GUARD_MS),
            applied_ms: None,
            management_address: None,
            recovery_pending: false,
            outcome: Phase::Expired,
            apply_error: None,
            recovery_error: None,
            session,
            timer_generation: 0,
            handoff,
            handoff_restored: false,
            media_running: false,
        };
        self.store.write("pending", &j)?;
        if let Some(t) = &j.handoff {
            if let Err(error) = crate::handoff::reserve(&self.host, t, &j.job_id) {
                self.finish_unmodified(&j, Phase::Failed)?;
                return Err(error);
            }
        }
        // A repeating recovery timer is armed BEFORE the separate apply timer;
        // it also supplies independent retries if recovery is killed or fails.
        if let Err(e) = self.schedule(&j, "guard", 180_000, true) {
            self.finish_unmodified(&j, Phase::Failed)?;
            return Err(e);
        }
        if let Err(e) = self.schedule(&j, "apply", 3000, false) {
            self.finish_unmodified(&j, Phase::Failed)?;
            return Err(e);
        }
        Ok(Stage {
            unchanged: false,
            pending: Some(self.public(&j)?),
            token: Some(token),
        })
    }
    fn matching(&self, token: &str) -> Result<Journal> {
        if !valid_id(token) {
            return Err(invalid("token", "invalid_token"));
        }
        let Some(j) = self.store.read::<Journal>("pending")? else {
            if self
                .store
                .read::<Last>("last")?
                .is_some_and(|l| l.phase == Phase::Expired && l.token.matches(token))
            {
                return Err(ApiError::Expired);
            }
            return Err(ApiError::NotPending);
        };
        if !j.token.matches(token) {
            return Err(ApiError::TokenMismatch);
        }
        Ok(j)
    }
    /// Token-scoped observer. A correct token is not a Keep operation.
    pub fn pending(&self, token: &str) -> Result<Pending> {
        self.public(&self.matching(token)?)
    }
    pub fn confirm(&self, token: &str, local_address: Option<IpAddr>) -> Result<Commit> {
        // API applies its shared per-peer rate limiter BEFORE entering here.
        if !valid_id(token) {
            return Err(invalid("token", "invalid_token"));
        }
        let _lock = self.store.lock()?;
        let mut j = self.matching(token)?;
        let _source_lock = j
            .handoff
            .as_ref()
            .map(|t| crate::handoff::source_lock(&self.host, &t.source))
            .transpose()?;
        if self.committed(&j)? {
            self.finish_committed(&j)?;
            return Err(ApiError::NotPending);
        }
        if j.recovery_pending {
            return Err(recovery_failure(&j));
        }
        if j.boot_id != self.host.clock.boot_id()? || self.host.clock.now_ms()? >= j.deadline_ms {
            self.recover_locked(&mut j, Phase::Expired)?;
            return Err(ApiError::Expired);
        }
        if j.phase != Phase::Applied {
            return Err(ApiError::StillApplying);
        }
        if j.plan.candidate.confirm_policy == ConfirmPolicy::RequireManagementAddress {
            let local = local_address.and_then(mapped).filter(|a| !a.is_loopback());
            let expected = j.management_address;
            if local.is_none() || local != expected {
                return Err(ApiError::WrongAddress);
            }
            let current = self.host.management_address(&j.plan.candidate)?;
            if local != current {
                return Err(ApiError::WrongAddress);
            }
        }
        let next = self.next_commit(&j)?;
        // Atomic state-actor replacement is the sole decision. If an fsync
        // reports an error after rename, re-read before considering recovery.
        match self.authority.compare_and_commit(&j.base, &next) {
            Ok(()) => (),
            Err(e) => {
                if !self.committed(&j)? {
                    return Err(e);
                }
            }
        }
        j.phase = Phase::Confirmed;
        // Everything below is cleanup. Failure is retained for boot/retry and
        // can never roll back an authoritative committed identity.
        let _ = self.store.write("pending", &j);
        let _ = self.finish_committed(&j);
        Ok(next)
    }
    pub fn revert(&self, token: &str) -> Result<()> {
        if !valid_id(token) {
            return Err(invalid("token", "invalid_token"));
        }
        let _lock = self.store.lock()?;
        let mut j = self.matching(token)?;
        let _source_lock = j
            .handoff
            .as_ref()
            .map(|t| crate::handoff::source_lock(&self.host, &t.source))
            .transpose()?;
        if self.committed(&j)? {
            self.finish_committed(&j)?;
            return Err(ApiError::NotPending);
        }
        self.recover_locked(&mut j, Phase::Reverted)
    }
    /// Explicit privileged recovery, without accepting a public job identity.
    pub fn rollback(&self) -> Result<()> {
        let _lock = self.store.lock()?;
        if let Some(mut j) = self.store.read::<Journal>("pending")? {
            let _source_lock = j
                .handoff
                .as_ref()
                .map(|t| crate::handoff::source_lock(&self.host, &t.source))
                .transpose()?;
            self.recover_locked(&mut j, Phase::Reverted)?;
        }
        Ok(())
    }
    /// The operator's way out of a recovery that cannot finish (NI-17 D14): stop
    /// retrying and keep the network as it is now. Only a transaction whose
    /// recovery has failed qualifies. The committed document stays what it was
    /// (the candidate was never kept), the engine's own files and guards go
    /// back to it, the media run, and the live addressing is left alone: the
    /// status shows it, with any drift, for the next decision. A handoff is
    /// excluded; its ownership markers have their own recovery.
    pub fn accept_current(&self) -> Result<Accepted> {
        let _lock = self.store.lock()?;
        let Some(j) = self.store.read::<Journal>("pending")? else {
            return Err(ApiError::NotPending);
        };
        if j.handoff.is_some() {
            return Err(unsupported("handoff_recovery_cannot_be_accepted"));
        }
        if self.committed(&j)? {
            self.finish_committed(&j)?;
            return Err(ApiError::NotPending);
        }
        if !(j.recovery_pending && j.phase == Phase::Failed) {
            return Err(locked("recovery_not_failed"));
        }
        let host = self.host.budget(APPLY_MS - 2000)?;
        // From here on nothing may keep the journal: an exit that can fail is
        // not one. Each step is the one a finished recovery takes, best effort;
        // what cannot be put back is kept to be told (the journal is gone after).
        let mut unrestored = Vec::new();
        let mut note = |path: &str, result: Result<()>| {
            if let Err(error) = result {
                unrestored.push(Unrestored {
                    path: path.into(),
                    reason: error.to_string(),
                });
            }
        };
        let _ = host.stop_consumers(&j.plan, true);
        if j.plan.old_table.is_some() {
            let _ = host.slices();
        }
        for c in j
            .plan
            .files
            .iter()
            .rev()
            .filter(|c| !runtime(&c.path) && !c.path.ends_with(".slice"))
        {
            note(&c.path, host.root.replace(&c.path, c.before.as_ref()));
        }
        note(
            "nftables table ndimon_management",
            host.set_table(j.plan.old_table.as_deref()),
        );
        for c in j
            .plan
            .files
            .iter()
            .rev()
            .filter(|c| runtime(&c.path) || c.path.ends_with(".slice"))
        {
            note(&c.path, host.root.replace(&c.path, c.before.as_ref()));
        }
        host.discard_created(&j.plan);
        let _ = host.resume_consumers(&j.plan);
        self.store.write(
            "last",
            &Last {
                token: j.token.clone(),
                phase: Phase::Failed,
                apply_error: j.recovery_error.clone().or_else(|| j.apply_error.clone()),
            },
        )?;
        self.store.remove("pending")?;
        self.stop_timers(&j);
        Ok(Accepted {
            apply_error: j.apply_error,
            recovery_error: j.recovery_error,
            unrestored,
        })
    }
    pub fn run_apply_job(&self, job_id: &str) -> Result<()> {
        if !valid_id(job_id) {
            return Err(invalid("job_id", "invalid_job_id"));
        }
        let _lock = self.store.lock()?;
        let Some(mut j) = self.store.read::<Journal>("pending")? else {
            return Ok(());
        };
        if j.job_id != job_id {
            return Ok(());
        }
        let _source_lock = j
            .handoff
            .as_ref()
            .map(|t| crate::handoff::source_lock(&self.host, &t.source))
            .transpose()?;
        if self.committed(&j)? {
            return self.finish_committed(&j);
        }
        if j.boot_id != self.host.clock.boot_id()? || self.host.clock.now_ms()? >= j.deadline_ms {
            return self.recover_locked(&mut j, Phase::Expired);
        }
        if j.phase != Phase::Staged {
            return Ok(());
        }
        let result: Result<()> = (|| {
            if j.handoff.is_none() {
                self.host.ownership()?;
            }
            if self.authority.load()? != j.base {
                return Err(ApiError::RevisionConflict {
                    current_revision: self.authority.load()?.revision,
                });
            }
            if !self.certified(&j.plan.candidate) {
                return Err(unsupported("backend_not_certified"));
            }
            let mut preflight = self.host.clone();
            if let Some(t) = &j.handoff {
                crate::handoff::check(&preflight, t, &j.job_id)?;
                preflight.handoff = true;
                preflight.handoff_paths = t
                    .document
                    .artifacts
                    .iter()
                    .map(|a| a.path.clone())
                    .collect();
            }
            preflight.check_plan(&j.plan)?;
            j.phase = Phase::Applying;
            self.store.write("pending", &j)?;
            let host = self.host.budget(APPLY_MS - 2000)?;
            if j.handoff.is_none() && host.settle_nm_files(&mut j.plan.nm)? {
                // The before-images are what NetworkManager keeps (NI-17 D14);
                // the journal holds them before anything changes.
                self.store.write("pending", &j)?;
            }
            host.stop_consumers(&j.plan, false)?;
            if j.plan
                .candidate
                .networks
                .iter()
                .any(|n| n.vlan_id.is_some())
            {
                host.run(Tool::Modprobe, &["8021q"], 20)?;
            }
            // Journaled slice/drop-in artifacts precede nft evaluation; backend
            // addressing and public bindings are still unchanged at this point.
            for change in j.plan.files.iter().filter(|c| c.path.ends_with(".slice")) {
                host.root.replace(&change.path, change.after.as_ref())?;
            }
            host.slices()?;
            let rules = backend::firewall(&j.plan.candidate, j.plan.portal_port)?;
            host.run_owned(
                Tool::Nft,
                &["--check".into(), "-f".into(), "-".into()],
                20,
                Some(&rules),
            )
            .map_err(|_| unsupported("guard_unavailable"))?;
            if let Some(t) = &j.handoff {
                crate::handoff::replace_tables(
                    &host,
                    &t.document.nftables_table,
                    Some(&rules),
                    None,
                )?;
            } else {
                host.set_table(Some(&rules))?;
            }
            if j.plan.candidate.backend == Some(NetworkBackend::Ifupdown) {
                host.deconfigure(&j.plan.previous, false)?;
            }
            for change in j
                .plan
                .files
                .iter()
                .filter(|c| !c.path.ends_with(".slice") && !runtime(&c.path))
            {
                host.root.replace(&change.path, change.after.as_ref())?;
            }
            if j.handoff.is_some()
                && j.plan.candidate.backend == Some(NetworkBackend::Networkmanager)
            {
                host.run(Tool::Nmcli, &["connection", "reload"], 20)?;
            }
            host.activate(
                &j.plan.candidate,
                &j.plan.previous,
                &j.plan.metadata,
                &j.plan.nm,
                false,
            )?;
            if let Some(t) = &j.handoff {
                // NM persists property changes itself; remove R's disabled
                // connection files after activation as well as before it.
                for artifact in &t.document.artifacts {
                    host.root.replace(&artifact.path, None)?;
                }
            }
            let address = host.readiness(&j.plan.candidate, &j.plan.metadata, false, true)?;
            host.arp(&j.plan.candidate)?;
            for change in j.plan.files.iter().filter(|c| runtime(&c.path)) {
                host.root.replace(&change.path, change.after.as_ref())?;
            }
            let banner = Root::image(
                &format!("Pending network change: http://{address}/\n"),
                0o644,
            );
            let change = j
                .plan
                .files
                .iter_mut()
                .find(|c| c.path == BANNER)
                .ok_or_else(|| failed("publish", "missing_banner_snapshot"))?;
            change.after = Some(banner.clone());
            self.store.write("pending", &j)?;
            host.root.replace(BANNER, Some(&banner))?;
            host.restore_consumers(&j.plan, false)?;
            j.management_address = Some(address);
            j.phase = Phase::Applied;
            // Persist readiness first. The initial repeating guard remains armed
            // throughout this transition, including the timer-arm failure path.
            j.applied_ms = Some(self.host.clock.now_ms()?);
            j.deadline_ms = j.applied_ms.unwrap_or(0).saturating_add(CONFIRM_MS);
            self.store.write("pending", &j)?;
            self.arm_window(&mut j)?;
            Ok(())
        })();
        if let Err(error) = result {
            // An observation or actor turn may briefly hold the decision lock.
            // Before mutation, keep the reservation intact for the independent
            // job's bounded retry rather than publishing a failed transaction.
            if j.phase == Phase::Staged
                && matches!(&error, ApiError::Locked { by: Some(by), .. } if by == "state_transaction")
            {
                return Err(error);
            }
            j.apply_error = Some(error.clone());
            j.outcome = Phase::Failed;
            if j.phase == Phase::Staged {
                self.finish_unmodified(&j, Phase::Failed)?;
                return Err(error);
            }
            self.store.write("pending", &j)?;
            self.recover_locked(&mut j, Phase::Failed)?;
            return Err(error);
        }
        Ok(())
    }
    /// A timer may fire early. It always rechecks identity/deadline under lock.
    pub fn run_recovery(&self, job_id: &str) -> Result<()> {
        if !valid_id(job_id) {
            return Err(invalid("job_id", "invalid_job_id"));
        }
        let _lock = self.store.lock()?;
        if crate::migration::run(
            &self.host,
            self.authority.as_ref(),
            self.config.selected_interface.as_deref(),
            Some(job_id),
        )? {
            return Ok(());
        }
        let Some(mut j) = self.store.read::<Journal>("pending")? else {
            return Ok(());
        };
        if j.job_id != job_id {
            return Ok(());
        }
        let _source_lock = j
            .handoff
            .as_ref()
            .map(|t| crate::handoff::source_lock(&self.host, &t.source))
            .transpose()?;
        if self.committed(&j)? {
            return self.finish_committed(&j);
        }
        if j.boot_id == self.host.clock.boot_id()?
            && self.host.clock.now_ms()? < j.deadline_ms
            && !j.recovery_pending
        {
            return self.arm_window(&mut j);
        }
        let outcome = if j.recovery_pending {
            j.outcome
        } else {
            Phase::Expired
        };
        self.recover_locked(&mut j, outcome)
    }
    fn recover_locked(&self, j: &mut Journal, outcome: Phase) -> Result<()> {
        if self.committed(j)? {
            return self.finish_committed(j);
        }
        if j.phase == Phase::Staged || j.handoff_restored {
            return self.finish_unmodified(j, outcome);
        }
        j.phase = Phase::Reverting;
        j.recovery_pending = true;
        j.outcome = outcome;
        self.store.write("pending", j)?;
        // Initial guard repeats every 10s independently; also arm a fresh retry
        // on boot, where transient timers from the previous boot do not exist.
        let result: Result<()> = (|| {
            self.arm_retry(j)?;
            let host = self.host.budget(APPLY_MS - 2000)?;
            if j.operation == Operation::SyncTessera {
                for c in j.plan.files.iter().rev() {
                    host.root.replace(&c.path, c.before.as_ref())?;
                }
                host.run(Tool::Systemctl, &["daemon-reload"], 20)?;
                return self.finish_unmodified(j, outcome);
            }
            // A retry finds the media running again (see the failure path
            // below); they are not stopped every ten seconds for an attempt
            // that may fail again.
            if !j.media_running {
                host.stop_consumers(&j.plan, true)?;
            }
            if j.plan.candidate.backend == Some(NetworkBackend::Ifupdown) {
                host.deconfigure(&j.plan.candidate, true)?;
            }
            // Restore the saved nft table while candidate slices still exist.
            // A before-image referencing slices requires stable cgroups at boot.
            if j.plan.old_table.is_some() {
                host.slices()?;
            }
            // Keep candidate guards in place while restoring old addressing.
            // Consumers remain stopped. Restore public bindings after backend
            // and management verification, then the exact old nft table.
            for c in j.plan.files.iter().rev().filter(|c| {
                !runtime(&c.path) && (j.handoff.is_some() || !c.path.ends_with(".slice"))
            }) {
                host.root.replace(&c.path, c.before.as_ref())?;
            }
            if j.handoff.is_some()
                && j.plan.previous.backend == Some(NetworkBackend::Networkmanager)
            {
                host.run(Tool::Nmcli, &["connection", "reload"], 20)?;
            }
            host.activate(
                &j.plan.previous,
                &j.plan.candidate,
                &j.plan.previous_metadata,
                &j.plan.nm,
                true,
            )?;
            let previously_owned = j.base.profiles.backend.is_some();
            if let Some(t) = &j.handoff {
                crate::handoff::source_readiness(&host, t, &j.plan)?;
            } else {
                host.readiness(
                    &j.plan.previous,
                    &j.plan.previous_metadata,
                    true,
                    previously_owned,
                )?;
            }
            if let Some(t) = &j.handoff {
                // Tessera packages this slice under /usr/lib; it is never a
                // handoff artifact. Recreate its cgroup before loading R's
                // saved guard, including boot before consumers can start it.
                host.run(Tool::Systemctl, &["daemon-reload"], 20)?;
                host.run(Tool::Systemctl, &["start", "tesseraaudio.slice"], 20)?;
                crate::handoff::replace_tables(
                    &host,
                    &t.document.nftables_table,
                    j.plan.old_table.as_deref(),
                    t.source_table.as_deref(),
                )?;
            } else {
                host.set_table(j.plan.old_table.as_deref())?;
            }
            for c in j
                .plan
                .files
                .iter()
                .rev()
                .filter(|c| runtime(&c.path) || c.path.ends_with(".slice"))
            {
                host.root.replace(&c.path, c.before.as_ref())?;
            }
            if j.plan.previous.backend == Some(NetworkBackend::Networkmanager)
                && j.handoff.is_some()
            {
                for c in j
                    .plan
                    .files
                    .iter()
                    .rev()
                    .filter(|c| c.path.ends_with(".nmconnection"))
                {
                    host.root.replace(&c.path, c.before.as_ref())?;
                }
            }
            if j.media_running {
                // Left running by a failed attempt: they restart once, now that
                // the network they run on is the restored one.
                host.stop_consumers(&j.plan, true)?;
            }
            host.restore_consumers(&j.plan, true)?;
            self.store.write(
                "last",
                &Last {
                    token: j.token.clone(),
                    phase: outcome,
                    apply_error: j.apply_error.clone(),
                },
            )?;
            if let Some(t) = &j.handoff {
                // After releasing R's marker, R may start another transaction.
                // Retry cleanup only; never replay old snapshots over that work.
                let transfer = t.clone();
                j.handoff_restored = true;
                self.store.write("pending", j)?;
                crate::handoff::restored(&host, &transfer)?;
            }
            self.store.remove("pending")?;
            self.stop_timers(j);
            Ok(())
        })();
        if let Err(e) = &result {
            // NI-17 D14: a recovery that cannot finish must not leave the
            // appliance silent. The network may stay failed (and say why), but
            // whatever this transaction stopped runs again. Best effort: each
            // unit is tried, and a unit that does not start is systemd's to
            // report.
            if !j.handoff_restored {
                let _ = self.host.resume_consumers(&j.plan);
                j.media_running = true;
            }
            j.phase = Phase::Failed;
            j.recovery_error = Some(e.clone());
            j.recovery_pending = true;
            self.store.write("pending", j)?;
            return Err(recovery_failure(j));
        }
        result
    }
    fn next_commit(&self, j: &Journal) -> Result<Commit> {
        Ok(Commit {
            version: 1,
            revision: j
                .base
                .revision
                .checked_add(1)
                .ok_or_else(|| invalid("network.revision", "revision_overflow"))?,
            profiles: j.plan.candidate.clone(),
            metadata: j.plan.metadata.clone(),
            transaction: Some(j.job_id.clone()),
            network_commit_revision: Some(
                j.base
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| invalid("network.revision", "revision_overflow"))?,
            ),
        })
    }
    fn committed(&self, j: &Journal) -> Result<bool> {
        if j.operation == Operation::SyncTessera {
            return Ok(j.phase == Phase::Confirmed);
        }
        let actual = self.authority.load()?;
        let mut expected = self.next_commit(j)?;
        // Unrelated actor writes may advance the global revision after commit.
        // The immutable network decision revision and exact content/job remain
        // mandatory proof; a greater global revision alone proves nothing.
        if actual.revision < expected.revision {
            return Ok(false);
        }
        expected.revision = actual.revision;
        Ok(actual == expected)
    }
    fn finish_unmodified(&self, j: &Journal, outcome: Phase) -> Result<()> {
        if let Some(t) = &j.handoff {
            crate::handoff::restored(&self.host, t)?;
        }
        self.store.write(
            "last",
            &Last {
                token: j.token.clone(),
                phase: outcome,
                apply_error: j.apply_error.clone(),
            },
        )?;
        self.store.remove("pending")?;
        self.stop_timers(j);
        Ok(())
    }
    fn finish_committed(&self, j: &Journal) -> Result<()> {
        if let Some(t) = &j.handoff {
            crate::handoff::kept(&self.host, t, &j.job_id)?;
        }
        self.host.prune_nm(&j.plan)?;
        // Banner is the only runtime lifecycle file removed at commit.
        if matches!(j.operation, Operation::Network | Operation::Handoff) {
            self.host.root.replace(BANNER, None)?;
        }
        self.store.write(
            "last",
            &Last {
                token: j.token.clone(),
                phase: Phase::Confirmed,
                apply_error: j.apply_error.clone(),
            },
        )?;
        self.store.remove("pending")?;
        self.stop_timers(j);
        Ok(())
    }
    fn schedule(&self, j: &Journal, kind: &str, delay_ms: u64, repeat: bool) -> Result<()> {
        let generation = if kind == "window" {
            format!("-{}", j.timer_generation)
        } else {
            String::new()
        };
        let unit = format!("ndimon-network-{kind}-{}{generation}", j.job_id);
        let mut args = vec![
            "--quiet".into(),
            "--collect".into(),
            format!("--unit={unit}"),
            format!("--on-active={}ms", delay_ms.max(1)),
            "--timer-property=AccuracySec=100ms".into(),
            "--timer-property=RandomizedDelaySec=0".into(),
            "--property=RuntimeMaxSec=90s".into(),
            "--property=TimeoutStopSec=2s".into(),
            "--property=KillMode=control-group".into(),
            "--property=NoNewPrivileges=yes".into(),
            "--property=ProtectClock=yes".into(),
            "--property=CapabilityBoundingSet=~CAP_SYS_TIME".into(),
        ];
        if repeat {
            args.push("--on-unit-inactive=10s".into());
        }
        args.extend([
            "--".into(),
            PRIVD.into(),
            "network-job".into(),
            if kind == "apply" {
                "apply".into()
            } else {
                "recover".into()
            },
            j.job_id.clone(),
        ]);
        self.host.run_owned(Tool::SystemdRun, &args, 15, None)?;
        Ok(())
    }
    fn arm_window(&self, j: &mut Journal) -> Result<()> {
        j.timer_generation = j
            .timer_generation
            .checked_add(1)
            .ok_or_else(|| failed("timer", "generation_overflow"))?;
        // Persist the generation before arming, so cleanup can find every timer.
        self.store.write("pending", j)?;
        let delay = j.deadline_ms.saturating_sub(self.host.clock.now_ms()?);
        self.schedule(j, "window", delay, true)
    }
    fn arm_retry(&self, j: &mut Journal) -> Result<()> {
        // Exactly one repeating retry timer per transaction/boot. Repeated
        // recovery failures must not multiply active recurring timers.
        let unit = format!("ndimon-network-retry-{}.timer", j.job_id);
        if self.host.active(&unit)? {
            return Ok(());
        }
        self.schedule(j, "retry", 10_000, true)
    }
    fn stop_timers(&self, j: &Journal) {
        for kind in ["apply", "guard", "retry"] {
            let timer = format!("ndimon-network-{kind}-{}.timer", j.job_id);
            let _ = self
                .host
                .run(Tool::Systemctl, &["stop", "--no-block", &timer], 8);
        }
        // A bounded transaction can create only a bounded number of generations;
        // stop exact recorded names, never a wildcard or a later transaction.
        for generation in 1..=j.timer_generation {
            let timer = format!("ndimon-network-window-{}-{generation}.timer", j.job_id);
            let _ = self
                .host
                .run(Tool::Systemctl, &["stop", "--no-block", &timer], 8);
        }
    }
    pub fn boot(&self) -> Result<()> {
        self.boot_report().map(|_| ())
    }
    /// `boot`, returning what it could not do. A recovery that fails at boot
    /// (NI-17) is reported the same way: its journal stays and retries, and the
    /// device starts. A legacy network store the engine
    /// refuses to take over (VV-2 N1) is reported, not failed: every unit of the
    /// stack waits for the boot job, so failing it would leave the device down
    /// on its own, unchanged network configuration. The store stays in place,
    /// every network change stays locked (`legacy_network_recovery`), and the
    /// next boot tries again.
    pub fn boot_report(&self) -> Result<Option<String>> {
        let _lock = self.store.lock()?;
        let mut boot = self.clone();
        boot.host.boot_recovery = true;
        boot.boot_locked()
    }
    fn boot_locked(&self) -> Result<Option<String>> {
        let release = crate::handoff::complete_release(self)?;
        if release.externally_managed {
            return Ok(None);
        }
        if release.quiesced {
            // Prepared R is still the owner. Rebuild its guards at boot unless
            // A has reserved the transfer and owns the recovery ordering.
            if self
                .store
                .read::<crate::handoff::Ownership>("external")?
                .is_none()
            {
                let c = self.authority.load()?;
                if c.profiles.backend.is_some() {
                    self.host.slices()?;
                    self.host
                        .set_table(Some(&backend::firewall(&c.profiles, 80)?))?;
                }
            }
            return Ok(None);
        }
        let legacy = crate::migration::needed(&self.host)?;
        match crate::migration::run(
            &self.host,
            self.authority.as_ref(),
            self.config.selected_interface.as_deref(),
            None,
        ) {
            Err(ApiError::Unsupported { what }) if legacy => return Ok(Some(what)),
            result => {
                result?;
            }
        }
        if self
            .host
            .root
            .path("/var/lib/ndimon/net/pending/state.json")?
            .exists()
        {
            return Err(locked("legacy_network_recovery"));
        }
        if let Some(mut j) = self.store.read::<Journal>("pending")? {
            let _source_lock = j
                .handoff
                .as_ref()
                .map(|t| crate::handoff::source_lock(&self.host, &t.source))
                .transpose()?;
            if let Err(error) = self.recover_locked(&mut j, Phase::Expired) {
                // Not the recovery's own failure (contention, a store that cannot
                // be written): fail the job, which is retried in five seconds.
                if !(j.recovery_pending && j.phase == Phase::Failed) {
                    return Err(error);
                }
                // NI-17 D14: the recovery failed and its journal is kept; it
                // retries every ten seconds, the media were started again, and
                // the page says why. Failing the boot job would keep the control
                // plane, the finder and the decoder from starting at all, so it
                // reports instead, with the committed network's guards in place
                // as far as they can be.
                let c = self.authority.load()?;
                if c.profiles.backend.is_some() {
                    let _ = self.host.slices();
                    if let Ok(rules) = backend::firewall(&c.profiles, 80) {
                        let _ = self.host.set_table(Some(&rules));
                    }
                }
                return Ok(Some(format!("recovery_pending: {error}")));
            }
        }
        let c = self.authority.load()?;
        if c.profiles.backend.is_some() {
            self.host.ownership()?;
            self.host.slices()?;
            self.host
                .set_table(Some(&backend::firewall(&c.profiles, 80)?))?;
        }
        Ok(None)
    }
    /// Explicit upgrade entry point; packaging must quiesce the old helpers
    /// before changing the boot service. Boot also resumes interrupted upgrades.
    pub fn migrate_legacy(&self) -> Result<()> {
        crate::handoff::writable(self)?;
        let _lock = self.store.lock()?;
        crate::migration::run(
            &self.host,
            self.authority.as_ref(),
            self.config.selected_interface.as_deref(),
            None,
        )?;
        Ok(())
    }
    pub fn sync_tessera(&self) -> Result<()> {
        let _lock = self.store.lock()?;
        self.idle()?;
        self.host.ownership()?;
        let c = self.authority.load()?;
        if c.profiles.backend.is_none() {
            return Ok(());
        }
        let mut files = Vec::new();
        self.host.tessera_files(&c.profiles, &mut files)?;
        if files.is_empty() {
            return Ok(());
        }
        let mut j = Journal {
            version: 1,
            operation: Operation::SyncTessera,
            job_id: random_id()?,
            token: Secret::generate()?,
            base: c.clone(),
            plan: Plan {
                files,
                nm: Vec::new(),
                previous: c.profiles.clone(),
                candidate: c.profiles.clone(),
                previous_metadata: c.metadata.clone(),
                metadata: c.metadata,
                old_table: None,
                active_services: Vec::new(),
                affected_services: Vec::new(),
                portal_port: 80,
                drift: Vec::new(),
            },
            phase: Phase::Applying,
            boot_id: self.host.clock.boot_id()?,
            deadline_ms: self.host.clock.now_ms()?.saturating_add(30_000),
            applied_ms: None,
            management_address: None,
            recovery_pending: false,
            outcome: Phase::Failed,
            apply_error: None,
            recovery_error: None,
            session: None,
            timer_generation: 0,
            handoff: None,
            handoff_restored: false,
            media_running: false,
        };
        self.store.write("pending", &j)?;
        if let Err(e) = self.arm_retry(&mut j) {
            self.finish_unmodified(&j, Phase::Failed)?;
            return Err(e);
        }
        let result: Result<()> = (|| {
            for change in &j.plan.files {
                self.host
                    .root
                    .replace(&change.path, change.after.as_ref())?;
            }
            self.host.run(Tool::Systemctl, &["daemon-reload"], 20)?;
            j.phase = Phase::Confirmed;
            self.store.write("pending", &j)?;
            self.finish_committed(&j)
        })();
        if let Err(e) = result {
            if !self.committed(&j)? {
                j.apply_error = Some(e.clone());
                self.recover_locked(&mut j, Phase::Failed)?;
            }
            return Err(e);
        }
        Ok(())
    }
    /// Legacy read-only ARP probe; callers never supply argv or filesystem paths.
    pub fn check_address(&self, address: Ipv4Addr) -> Result<bool> {
        let c = self.current()?;
        if !validation::unicast(address) {
            return Err(invalid("address", "unusable_unicast_address"));
        }
        let name = ifname(
            &c.profiles,
            assigned(&c.profiles, &c.profiles.roles.management)?,
        );
        let args = [
            "-D".into(),
            "-I".into(),
            name,
            "-c".into(),
            "2".into(),
            "-w".into(),
            "3".into(),
            address.to_string(),
        ];
        let output = self.host.output(Tool::Arping, &args, 10_000, None)?;
        match output.code {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(failed("arp", "probe_failed")),
        }
    }
    /// Immediate VLAN writes remain disabled. Trusted socket context only;
    /// protected moves use stage and its captured confirmation policy (U14).
    pub fn guard_immediate_session(
        &self,
        candidate: &NetworkProfiles,
        session: &SessionPath,
    ) -> Result<()> {
        crate::handoff::writable(self)?;
        let old = self.current()?;
        let local = mapped(session.local);
        let peer = mapped(session.peer);
        for n in &old.profiles.networks {
            let a = n.address.parse::<Ipv4Addr>().ok();
            let carrying = a == local && a.is_some()
                || a.zip(peer).is_some_and(|(a, peer)| {
                    (8..=30).contains(&n.prefix)
                        && validation::subnet(a, n.prefix) == validation::subnet(peer, n.prefix)
                });
            if carrying && !candidate.networks.iter().any(|c| c == n) {
                return Err(locked("session_path"));
            }
        }
        Err(unsupported("unsupported_operation"))
    }
}
fn runtime(path: &str) -> bool {
    path == crate::BINDING
        || path == crate::TESSERA_BINDING
        || path == BANNER
        || path.ends_with("70-ndimon-network.conf")
}
fn recovery_failure(j: &Journal) -> ApiError {
    // A retained recovery is the transaction's state, not a fault of the
    // request (counterpart contract §3, NI-17 D15): `apply_failed` at step
    // `recover`, which `http_status` answers with 409, naming what the recovery
    // could not do. Original and recovery typed errors are also separately
    // visible in Pending.
    ApiError::ApplyFailed {
        domain: ndimon_types::Domain::Network,
        step: "recover".into(),
        reason: match &j.recovery_error {
            Some(error) => error.to_string(),
            None => "recovery_pending".into(),
        },
    }
}
