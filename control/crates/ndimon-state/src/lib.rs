//! One task owns the committed document. HTTP, plugins, migration and the
//! followed-file poller all enter the same bounded request queue.
pub mod apply;
pub mod backup;
pub mod commands;
pub mod domains;
pub use domains::audio;
pub use domains::audio::catalog;
pub use domains::display::assets;
pub mod follower;
pub mod import;
pub mod ipc;
pub mod network_authority;
mod observations;
pub mod observed_clock;
pub mod persist;
pub mod plan;

use apply::Applier;
use ndimon_render::Ownership;
use ndimon_types::*;
use persist::{Persistence, StateFile};
use plan::{Plan, internal};
use serde_json::{Value, json};
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::{broadcast, mpsc, oneshot};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone)]
pub struct StateHandle {
    tx: mpsc::Sender<Message>,
    activities: commands::Activities,
    events: broadcast::Sender<Event>,
    pub assets: domains::display::assets::AssetStore,
    backup_root: Arc<PathBuf>,
    backup_lock: Arc<tokio::sync::Mutex<()>>,
    pub(crate) backup_io: Arc<dyn backup::environment::BackupIo>,
    pub(crate) backup_plugins: Arc<dyn backup::plugins::PluginBackup>,
    applier: Arc<dyn Applier>,
}
#[derive(Debug, Clone)]
pub struct Submission {
    pub reply: CommandReply,
    pub audio_revision: u64,
    pub domain_revision: u64,
    pub apply: Value,
}
#[derive(Debug)]
enum Message {
    Observation(Box<Event>),
    Query(Query, oneshot::Sender<Result<Value, ApiError>>),
    Snapshot(oneshot::Sender<Result<StateFile, ApiError>>),
    Submit(
        Box<CommandRequest>,
        Origin,
        Option<Domain>,
        oneshot::Sender<Result<Submission, ApiError>>,
    ),
    Follow(Box<StateFile>, oneshot::Sender<Result<(), ApiError>>),
    Boot(oneshot::Sender<Result<(), ApiError>>),
    Replay(oneshot::Sender<Result<(), ApiError>>),
    Complete(Domain, Value, oneshot::Sender<Result<(), ApiError>>),
    Shutdown(oneshot::Sender<Result<(), ApiError>>),
}
#[derive(Debug)]
struct Actor {
    /// Set by `Shutdown`: nothing that writes starts after the process was
    /// told to stop.
    closing: bool,
    activities: commands::Activities,
    state: StateFile,
    assets: domains::display::assets::AssetStore,
    persistence: Persistence,
    applier: Arc<dyn Applier>,
    events: broadcast::Sender<Event>,
    observations: std::collections::BTreeMap<Domain, DomainObserved>,
    failed: std::collections::BTreeMap<Domain, String>,
    native_drift: std::collections::BTreeMap<(Domain, String), String>,
    replay_failed: std::collections::BTreeSet<Domain>,
    last_observed: Observed,
}

impl StateHandle {
    /// Read existing state or import every declared input. Privileged inputs
    /// require the root migration/bootstrap described in README.md.
    pub async fn start(
        directory: impl Into<PathBuf>,
        legacy_root: PathBuf,
        applier: Arc<dyn Applier>,
    ) -> Result<Self, ApiError> {
        let directory = directory.into();
        let assets = domains::display::assets::AssetStore::new(
            directory
                .parent()
                .ok_or_else(|| internal("state parent missing"))?
                .join("assets"),
        );
        let initial_assets = assets.clone();
        let (persistence, state) = tokio::task::spawn_blocking(move || {
            let p = Persistence::new(directory)?;
            let mut state = match p.load()? {
                Some(s) => s,
                None => {
                    let (mut s, skipped) =
                        import::import_tolerant(import::read_files(&legacy_root, true)?)?;
                    for item in skipped {
                        tracing::warn!(%item, "legacy import left a domain at its defaults");
                    }
                    if let Err(reason) = initial_assets.import_legacy(&mut s, &legacy_root) {
                        tracing::warn!(%reason, "legacy splash logo not imported");
                    }
                    p.initialize(&s)?;
                    s
                }
            };
            domains::plugins::host::migrate_runtime(&mut state, &legacy_root)?;
            p.initialize(&state)?;
            Ok::<_, String>((p, state))
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;
        let handle = Self::spawn_with_assets(state, persistence, applier, assets)?;
        handle.render_boot().await?;
        Ok(handle)
    }
    pub fn spawn(
        state: StateFile,
        persistence: Persistence,
        applier: Arc<dyn Applier>,
    ) -> Result<Self, ApiError> {
        let assets = domains::display::assets::AssetStore::new(
            backup::root(&persistence.directory).join("assets"),
        );
        Self::spawn_with_assets(state, persistence, applier, assets)
    }
    fn spawn_with_assets(
        mut state: StateFile,
        persistence: Persistence,
        applier: Arc<dyn Applier>,
        assets: domains::display::assets::AssetStore,
    ) -> Result<Self, ApiError> {
        let before_migration = state.document.clone();
        for registration in domains::REGISTRY {
            if registration.ownership() == Ownership::Native {
                registration
                    .module
                    .migrate(&mut state.document, &state.legacy_files)
                    .map_err(internal)?;
            }
        }
        state.check().map_err(internal)?;
        if state.document != before_migration {
            // privd renders the persisted envelope during boot. Publish migrated
            // ordering metadata before that render, retaining every revision.
            persistence
                .write("state.json", &serde_json::to_vec(&state).map_err(internal)?)
                .map_err(internal)?;
        }
        for warning in import::warnings(&state.legacy_files) {
            tracing::warn!(%warning,"legacy import retained an unknown key");
        }
        let (tx, mut rx) = mpsc::channel(64);
        let (events, _) = broadcast::channel(256);
        let backup_root = Arc::new(backup::root(&persistence.directory).to_path_buf());
        let activities = commands::Activities::new();
        let mut actor = Actor {
            closing: false,
            activities: activities.clone(),
            state,
            assets: assets.clone(),
            persistence,
            applier: applier.clone(),
            events: events.clone(),
            observations: Default::default(),
            failed: Default::default(),
            native_drift: Default::default(),
            replay_failed: Default::default(),
            last_observed: Observed::default(),
        };
        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                match message {
                    Message::Observation(event) => actor.record_observation(*event),
                    Message::Snapshot(reply) => {
                        let result = actor.refresh().await.map(|_lock| actor.state.clone());
                        let _ = reply.send(result);
                    }
                    Message::Query(query, reply) => {
                        let result = actor.query(query).await;
                        let _ = reply.send(result);
                    }
                    Message::Submit(_, _, _, reply) if actor.closing => {
                        let _ = reply.send(Err(stopping()));
                    }
                    Message::Submit(request, origin, cas_domain, reply) => {
                        let result = actor.submit(*request, origin, cas_domain).await;
                        let _ = reply.send(result);
                    }
                    Message::Follow(_, reply)
                    | Message::Boot(reply)
                    | Message::Replay(reply)
                    | Message::Complete(_, _, reply)
                        if actor.closing =>
                    {
                        let _ = reply.send(Err(stopping()));
                    }
                    Message::Follow(imported, reply) => {
                        let result = actor.follow(*imported).await;
                        let _ = reply.send(result);
                    }
                    Message::Boot(reply) => {
                        let result = actor.boot().await;
                        let _ = reply.send(result);
                    }
                    Message::Replay(reply) => {
                        let result = actor.replay().await;
                        let _ = reply.send(result);
                    }
                    Message::Complete(domain, result, reply) => {
                        let result = actor.complete(domain, result).await;
                        let _ = reply.send(result);
                    }
                    Message::Shutdown(reply) => {
                        actor.closing = true;
                        let result = actor.finish().await;
                        let _ = reply.send(result);
                    }
                }
            }
        });
        Ok(Self {
            tx,
            activities,
            assets,
            events,
            backup_root,
            backup_io: Arc::new(backup::environment::SystemIo::default()),
            backup_plugins: Arc::new(backup::plugins::Unavailable),
            applier,
            backup_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }
    pub fn backup_root(&self) -> &std::path::Path {
        &self.backup_root
    }
    pub async fn lock_backup(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.backup_lock.lock().await
    }
    pub fn with_backup_io(mut self, io: Arc<dyn backup::environment::BackupIo>) -> Self {
        self.backup_io = io;
        self
    }
    pub fn with_plugin_backups(mut self, plugins: Arc<dyn backup::plugins::PluginBackup>) -> Self {
        self.backup_plugins = plugins;
        self
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }
    pub fn subscribe_commands(&self) -> broadcast::Receiver<commands::CommandActivity> {
        self.activities.subscribe()
    }
    pub fn command_sequence(&self) -> u64 {
        self.activities.sequence()
    }
    pub async fn snapshot(&self) -> Result<StateFile, ApiError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Message::Snapshot(tx))
            .await
            .map_err(internal)?;
        rx.await.map_err(internal)?
    }
    pub async fn query(&self, query: Query) -> Result<Value, ApiError> {
        for registration in domains::REGISTRY {
            if registration.module.query_outside_actor(&query) {
                let snapshot = self.snapshot().await?;
                if let Some(result) =
                    registration
                        .module
                        .query(&snapshot, self.applier.as_ref(), &query)
                {
                    return result.await;
                }
            }
        }
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Message::Query(query, tx))
            .await
            .map_err(internal)?;
        rx.await.map_err(internal)?
    }
    pub(crate) async fn complete_domain(
        &self,
        domain: Domain,
        result: Value,
    ) -> Result<(), ApiError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Message::Complete(domain, result, tx))
            .await
            .map_err(internal)?;
        rx.await.map_err(internal)?
    }
    pub async fn submit(
        &self,
        request: CommandRequest,
        origin: Origin,
    ) -> Result<CommandReply, ApiError> {
        self.submit_with_result(request, origin)
            .await
            .map(|s| s.reply)
    }
    /// Legacy audio revision is domain-local. The comparison happens inside the
    /// actor, never in an HTTP read/modify/write window.
    pub async fn submit_audio(
        &self,
        request: CommandRequest,
        origin: Origin,
    ) -> Result<Submission, ApiError> {
        self.submit_domain(request, origin, Domain::Audio).await
    }
    pub async fn submit_with_result(
        &self,
        request: CommandRequest,
        origin: Origin,
    ) -> Result<Submission, ApiError> {
        if matches!(request.command, Command::UndoRestore { .. }) {
            let restored = backup::coordinator::undo(self, request, origin).await?;
            let state = self.snapshot().await?;
            return Ok(Submission {
                reply: restored.reply,
                audio_revision: state.audio_revision(),
                domain_revision: state.revision,
                apply: restored.legacy,
            });
        }
        self.send(request, origin, None).await
    }
    pub async fn submit_domain(
        &self,
        request: CommandRequest,
        origin: Origin,
        domain: Domain,
    ) -> Result<Submission, ApiError> {
        if domains::command_domain(&request.command) != Some(domain) {
            return Err(internal("command does not belong to CAS domain"));
        }
        self.send(request, origin, Some(domain)).await
    }
    async fn send(
        &self,
        request: CommandRequest,
        origin: Origin,
        cas_domain: Option<Domain>,
    ) -> Result<Submission, ApiError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Message::Submit(Box::new(request), origin, cas_domain, tx))
            .await
            .map_err(internal)?;
        rx.await.map_err(internal)?
    }
    pub async fn follow(&self, state: StateFile) -> Result<(), ApiError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Message::Follow(Box::new(state), tx))
            .await
            .map_err(internal)?;
        rx.await.map_err(internal)?
    }
    pub async fn render_boot(&self) -> Result<(), ApiError> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Message::Boot(tx)).await.map_err(internal)?;
        rx.await.map_err(internal)?
    }
    /// Finish for exit: wait for the transaction in flight, refuse everything
    /// that writes after it, and leave no `state.pending` marker behind. The
    /// actor owns the commit, so an exiting process must not abandon it: the
    /// next reader of the state (privd's boot job) would find the marker.
    pub async fn shutdown(&self, limit: Duration) -> Result<(), ApiError> {
        let (tx, rx) = oneshot::channel();
        tokio::time::timeout(limit, async {
            self.tx
                .send(Message::Shutdown(tx))
                .await
                .map_err(internal)?;
            rx.await.map_err(internal)?
        })
        .await
        .map_err(|_| internal("the state actor did not finish before the stop timeout"))?
    }
    pub async fn replay(&self) -> Result<(), ApiError> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Message::Replay(tx)).await.map_err(internal)?;
        rx.await.map_err(internal)?
    }
    pub fn spawn_observers(
        &self,
        applier: &apply::ProductionApplier,
    ) -> tokio::task::JoinHandle<()> {
        let effects: Vec<_> = domains::REGISTRY
            .iter()
            .filter(|r| r.ownership() == Ownership::Native)
            .filter_map(|r| r.module.observe(self.clone(), applier))
            .collect();
        tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            for effect in effects {
                tasks.spawn(effect);
            }
            while tasks.join_next().await.is_some() {}
        })
    }
    /// Replay through the actor queue: an old boot snapshot cannot overtake a
    /// command. Keep retrying missing/restarted sockets without blocking READY.
    pub fn spawn_replay(&self) -> tokio::task::JoinHandle<()> {
        self.spawn_replay_with(
            Duration::from_millis(100),
            Duration::from_secs(10),
            Duration::from_secs(30),
        )
    }
    fn spawn_replay_with(
        &self,
        initial: Duration,
        maximum: Duration,
        periodic: Duration,
    ) -> tokio::task::JoinHandle<()> {
        let handle = self.clone();
        tokio::spawn(async move {
            let mut delay = initial;
            loop {
                let (tx, rx) = oneshot::channel();
                if handle.tx.send(Message::Replay(tx)).await.is_err() {
                    break;
                }
                match rx.await {
                    Ok(Ok(())) => delay = periodic,
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "follower replay pending");
                        delay = (delay * 2).min(maximum);
                    }
                    Err(_) => break,
                }
                tokio::time::sleep(delay).await;
            }
        })
    }
    /// Abort this task before dropping the control runtime. Polling uses hashes
    /// (complete byte comparisons), so atomic replacement and coarse mtimes work.
    pub fn spawn_follower(&self, root: PathBuf) -> tokio::task::JoinHandle<()> {
        let handle = self.clone();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(2));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut refused = Repeats::default();
            loop {
                timer.tick().await;
                let root = root.clone();
                let snapshot = match handle.snapshot().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let imported = tokio::task::spawn_blocking(move || {
                    import::followed_document(
                        import::read_files_with_fallback(
                            &root,
                            false,
                            Some(&snapshot.legacy_files),
                        )?,
                        &snapshot.document,
                    )
                })
                .await;
                match imported {
                    Ok(Ok(state)) => match handle.follow(state).await {
                        Ok(()) => {
                            refused.news(None);
                        }
                        // A refusal that lasts (a network change waiting for its
                        // Keep) is one fact, said once, not every two seconds.
                        Err(e) => {
                            if refused.news(Some(e.to_string())) {
                                tracing::warn!(error=%e,"followed state import failed");
                            } else {
                                tracing::debug!(error=%e,"followed state import still failing");
                            }
                        }
                    },
                    Ok(Err(e)) => tracing::warn!(error=%e,"legacy import failed"),
                    Err(e) => tracing::warn!(error=%e,"legacy reader failed"),
                }
            }
        })
    }
}
fn stopping() -> ApiError {
    internal("the control plane is shutting down")
}
/// A failure that repeats is news only the first time (VV-3 W4).
#[derive(Debug, Default)]
struct Repeats(Option<String>);
impl Repeats {
    /// Whether this failure differs from the last one. A success (`None`)
    /// forgets it, so the same failure after a success is news again.
    fn news(&mut self, failure: Option<String>) -> bool {
        let news = failure.is_some() && self.0 != failure;
        self.0 = failure;
        news
    }
}
impl Actor {
    /// Every transaction ends in its own commit or rollback, and the actor ran
    /// them one at a time, so the one before `Shutdown` is complete. Only a
    /// failed rollback leaves its marker: roll it back here, under the
    /// decision lock, exactly as the next start would.
    async fn finish(&mut self) -> Result<(), ApiError> {
        drop(self.refresh().await?);
        Ok(())
    }
    async fn refresh(&mut self) -> Result<network_authority::DecisionLock, ApiError> {
        let store = self.persistence.clone();
        let (lock, state) = tokio::task::spawn_blocking(move || {
            let lock = network_authority::lock(&store)?;
            let state = store.load().map_err(internal)?;
            Ok::<_, ApiError>((lock, state))
        })
        .await
        .map_err(internal)??;
        if let Some(state) = state {
            if state != self.state {
                let previous = serde_json::to_value(&self.state.document).map_err(internal)?;
                let next = serde_json::to_value(&state.document).map_err(internal)?;
                let domains: Vec<_> = domains::REGISTRY
                    .iter()
                    .filter(|r| previous[r.name] != next[r.name])
                    .map(|r| r.domain)
                    .collect();
                self.state = state;
                if !domains.is_empty() {
                    let _ = self.events.send(Event::StateChanged {
                        revision: self.state.revision,
                        domains,
                        origin: Origin::Migration,
                    });
                }
            }
        }
        Ok(lock)
    }
    async fn replay(&mut self) -> Result<(), ApiError> {
        drop(self.refresh().await?);
        match self.applier.replay(&self.state).await {
            Ok(()) => {
                for domain in std::mem::take(&mut self.replay_failed) {
                    self.failed.remove(&domain);
                }
                Ok(())
            }
            Err(error) => {
                if let ApiError::ApplyFailed { domain, .. } = &error {
                    self.failed.insert(*domain, error.to_string());
                    self.replay_failed.insert(*domain);
                }
                Err(error)
            }
        }
    }
    fn collect_assets(&self) {
        if let Err(error) = self
            .assets
            .collect_persisted(&self.state, &self.persistence.directory)
        {
            tracing::warn!(%error, "asset collection deferred");
        }
    }
    async fn boot(&mut self) -> Result<(), ApiError> {
        drop(self.refresh().await?);
        for registration in domains::REGISTRY
            .iter()
            .filter(|r| r.ownership() == Ownership::Native)
        {
            registration
                .module
                .validate_assets(&self.state.document, &self.assets)
                .map_err(internal)?;
        }
        let manifest = self.applier.render(&self.state).await.map_err(internal)?;
        self.collect_assets();
        let bytes = serde_json::to_vec(&manifest).map_err(internal)?;
        let store = self.persistence.clone();
        tokio::task::spawn_blocking(move || store.write("rendered.json", &bytes))
            .await
            .map_err(internal)?
            .map_err(internal)?;
        Ok(())
    }
    async fn query(&mut self, query: Query) -> Result<Value, ApiError> {
        drop(self.refresh().await?);
        if let Some(reply) = domains::operation_query(
            &self.state,
            backup::root(&self.persistence.directory),
            &query,
        ) {
            return reply;
        }
        for registration in domains::REGISTRY {
            if let Some(reply) =
                registration
                    .module
                    .query(&self.state, self.applier.as_ref(), &query)
            {
                return reply.await;
            }
        }
        match query {
            Query::State => serde_json::to_value(&self.state.document).map_err(internal),
            Query::StateDomain { domain } => {
                let value = serde_json::to_value(&self.state.document).map_err(internal)?;
                let key = serde_json::to_value(domain).map_err(internal)?;
                Ok(
                    json!({"revision":self.state.revision,"domain_revision":self.state.domain_revisions.get(&domain),"state":value[key.as_str().ok_or_else(||internal("domain key"))?]}),
                )
            }
            Query::Status => self
                .applier
                .decoder(json!({"action":"status"}))
                .await
                .map_err(internal),
            Query::Observed => {
                let mut observed = Observed::default();
                observed.revision = self.state.revision;
                for registration in domains::REGISTRY
                    .iter()
                    .filter(|r| r.ownership() == Ownership::Native)
                {
                    let mut domain = self
                        .observations
                        .get(&registration.domain)
                        .cloned()
                        .unwrap_or_default();
                    domain.applied_revision = Some(self.state.domain_revision(registration.domain));
                    domain.status = if self.failed.contains_key(&registration.domain) {
                        SyncStatus::Failed
                    } else if self
                        .native_drift
                        .keys()
                        .any(|(d, _)| *d == registration.domain)
                    {
                        SyncStatus::Drift
                    } else {
                        domain.status
                    };
                    if let Some(detail) = self.failed.get(&registration.domain) {
                        domain.detail = Some(detail.clone());
                    }
                    registration
                        .module
                        .project_observed(&self.state, &mut domain);
                    observed.domains.insert(registration.domain, domain);
                }
                observed.drift = self
                    .native_drift
                    .keys()
                    .map(|(domain, path)| Drift::new(*domain, path))
                    .collect();
                for registration in domains::REGISTRY
                    .iter()
                    .filter(|r| r.ownership() == Ownership::Native)
                {
                    for event in registration
                        .module
                        .observe_state(
                            &self.state,
                            self.applier.as_ref(),
                            &self.last_observed,
                            &mut observed,
                        )
                        .await?
                    {
                        let _ = self.events.send(event);
                    }
                }
                self.last_observed = observed.clone();
                serde_json::to_value(observed).map_err(internal)
            }
            _ => Err(ApiError::Unsupported {
                what: "query is owned by another control-plane module".into(),
            }),
        }
    }
    async fn submit(
        &mut self,
        request: CommandRequest,
        origin: Origin,
        cas_domain: Option<Domain>,
    ) -> Result<Submission, ApiError> {
        drop(self.refresh().await?);
        if request
            .expect_activity
            .is_some_and(|n| n != self.activities.sequence())
        {
            return Err(internal("command superseded by newer activity"));
        }
        for registration in domains::REGISTRY
            .iter()
            .filter(|r| r.ownership() == Ownership::Native)
        {
            registration
                .module
                .guard(&self.state, self.applier.as_ref(), Some(&request.command))
                .await?;
        }
        let decision_lock = self.refresh().await?;
        let revision = cas_domain.map_or(self.state.revision, |domain| {
            self.state.domain_revision(domain)
        });
        if request.expect_revision.is_some_and(|r| r != revision) {
            return Err(ApiError::RevisionConflict {
                current_revision: revision,
            });
        }
        let mut next = self.state.clone();
        domains::reduce(&mut next.document, &request.command)?;
        let owner = domains::command_domain(&request.command);
        ndimon_types::validate(&next.document).map_err(ApiError::invalid)?;
        let mut context = self.state.document.clone();
        if let Some(domain) = cas_domain {
            context.revision = self.state.domain_revision(domain);
        }
        let mut plan = plan::plan(&context, &next.document, &request.command)?;
        plan.session = request.network_session;
        plan.confirmation = request
            .expect_impact
            .as_ref()
            .map(|t| Some(t.as_str().to_string()))
            .or_else(|| (request.confirm || !origin.requires_confirmation()).then_some(None));
        if !plan.external_confirmation
            && ((request
                .expect_impact
                .as_ref()
                .is_some_and(|token| token != &plan.impact.token))
                || (plan.impact.needs_confirmation()
                    && origin.requires_confirmation()
                    && request.expect_impact.is_none()
                    && !request.confirm))
        {
            return Err(ApiError::ConfirmationRequired {
                impact: plan.impact,
            });
        }
        // The restore impact is sealed locally once. Counterpart hooks receive
        // the explicit confirmation after that complete impact has been accepted.
        if backup::handles(&request.command) {
            plan.confirmation = Some(None);
        }
        let mut report = ApplyReport::default();
        report.impact = plan.impact.clone();
        for domain in &plan.domains {
            domains::get(*domain)?
                .module
                .prepare(&self.state, &mut next, &request.command)?;
        }
        self.activities.record(&request.command);
        if plan.paths.is_empty() {
            // Runtime hooks may use the same authority through privd. Durable
            // writes below retain this lock through stage, apply and commit.
            let has_actions = !plan.actions.is_empty();
            let effect_owner = owner
                .or_else(|| plan.domains.first().copied())
                .unwrap_or(Domain::Presets);
            let name = domains::get(effect_owner)?.name;
            let mut apply = {
                self.applier
                    .preflight(&self.state, &next, &plan)
                    .await
                    .map_err(|reason| ApiError::ApplyFailed {
                        domain: effect_owner,
                        step: "preflight".into(),
                        reason,
                    })?;
                self.reserve(&next, &plan).await?;
                drop(decision_lock);
                if has_actions {
                    self.progress(self.state.revision, name, ApplyStatus::Started, None);
                }
                match apply::apply_command(
                    self.applier.as_ref(),
                    &next,
                    &plan,
                    Some(&request.command),
                )
                .await
                {
                    Ok(value) => {
                        if has_actions {
                            self.progress(self.state.revision, name, ApplyStatus::Done, None);
                            report.steps.push(ApplyStep::new(name, ApplyStatus::Done));
                        }
                        value
                    }
                    Err(error) => {
                        let reason = error.to_string();
                        self.release_reservation(&plan, &reason).await?;
                        let rollback = self.applier.rollback(&self.state, &plan).await;
                        if has_actions {
                            self.progress(
                                self.state.revision,
                                name,
                                ApplyStatus::Failed,
                                Some(reason.clone()),
                            );
                        }
                        if let Err(rollback) = rollback {
                            return Err(ApiError::ApplyFailed {
                                domain: effect_owner,
                                step: "rollback".into(),
                                reason: format!("{reason}; rollback failed: {rollback}"),
                            });
                        }
                        return Err(error);
                    }
                }
            };
            if let Some(object) = apply.as_object_mut() {
                object.remove("_render_manifest");
                if let Some(impact) = object.remove("_impact") {
                    report.impact = serde_json::from_value(impact).map_err(internal)?;
                }
            }
            if let Some(domain) = owner {
                for event in domains::get(domain)?
                    .module
                    .operation_observed(&mut self.last_observed, &apply)?
                {
                    let _ = self.events.send(event);
                }
            }
            let revision = self.state.revision;
            drop(self.refresh().await?);
            if self.state.revision != revision {
                // The durable decision already succeeded; record a failed
                // follower replay for the existing retry loop, never undo it.
                if let Err(error) = self.replay().await {
                    tracing::warn!(%error, "committed operation replay pending");
                }
            }
            {
                let _decision_lock = self.refresh().await?;
                next = self.state.clone();
                let mut metadata_changed = false;
                for domain in &plan.domains {
                    let module = domains::get(*domain)?.module;
                    module.prepare(&self.state, &mut next, &request.command)?;
                    metadata_changed |=
                        module.accepted(&self.state, &mut next, &request.command, &mut apply)?;
                }
                if metadata_changed {
                    let store = self.persistence.clone();
                    let candidate = next.clone();
                    tokio::task::spawn_blocking(move || store.initialize(&candidate))
                        .await
                        .map_err(internal)?
                        .map_err(internal)?;
                    self.state = next;
                }
            }
            for domain in &plan.domains {
                let mut observed = DomainObserved::default();
                domains::get(*domain)?
                    .module
                    .project_observed(&self.state, &mut observed);
                if observed.status == SyncStatus::Applying {
                    let mut step =
                        ApplyStep::new(domains::get(*domain)?.name, ApplyStatus::Started);
                    step.detail = observed.detail;
                    report.steps.retain(|s| s.step != step.step);
                    report.steps.push(step);
                }
            }
            return Ok(Submission {
                reply: CommandReply::new(self.state.revision, report),
                audio_revision: self.state.audio_revision(),
                domain_revision: owner
                    .map_or(self.state.revision, |d| self.state.domain_revision(d)),
                apply,
            });
        }
        let effect_owner = owner
            .or_else(|| plan.domains.first().copied())
            .ok_or_else(|| internal("empty restore plan"))?;
        next.revision = self
            .state
            .revision
            .checked_add(1)
            .filter(|n| *n <= 9_007_199_254_740_991)
            .ok_or_else(|| internal("revision exhausted"))?;
        next.document.revision = next.revision;
        for domain in &plan.domains {
            next.domain_revisions.insert(*domain, next.revision);
        }
        let artifacts = ndimon_render::render_with_revisions(
            &next.document,
            &next.domain_revisions,
            &next.legacy_files,
        )
        .map_err(internal)?;
        let fallback_manifest =
            serde_json::to_value(ndimon_render::manifest(&artifacts, next.revision))
                .map_err(internal)?;
        for domain in &plan.domains {
            let module = domains::get(*domain)?.module;
            module
                .validate_assets(&next.document, &self.assets)
                .map_err(|reason| ApiError::ApplyFailed {
                    domain: *domain,
                    step: "preflight".into(),
                    reason,
                })?;
        }
        self.applier
            .preflight(&self.state, &next, &plan)
            .await
            .map_err(|reason| ApiError::ApplyFailed {
                domain: effect_owner,
                step: "preflight".into(),
                reason,
            })?;
        self.reserve(&next, &plan).await?;
        let store = self.persistence.clone();
        let previous = self.state.clone();
        let candidate = next.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = store.stage(&previous, &candidate) {
                return match store.rollback(&previous) {
                    Ok(()) => Err(e),
                    Err(rollback) => Err(format!("{e}; persistence rollback failed: {rollback}")),
                };
            }
            Ok(())
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;
        for domain in &plan.domains {
            self.progress(
                next.revision,
                domains::get(*domain)?.name,
                ApplyStatus::Started,
                None,
            );
        }
        let applied =
            apply::apply_command(self.applier.as_ref(), &next, &plan, Some(&request.command)).await;
        let mut apply = match applied {
            Ok(v) => v,
            Err(error) => {
                let original = error.to_string();
                let restored = self.restore(&plan, &original).await?;
                if restored != original {
                    return Err(ApiError::ApplyFailed {
                        domain: effect_owner,
                        step: "rollback".into(),
                        reason: restored,
                    });
                }
                if matches!(error, ApiError::ConfirmationRequired { .. }) {
                    for domain in &plan.domains {
                        self.failed.remove(domain);
                    }
                }
                return Err(error);
            }
        };
        // Manifest describes rendered bytes, never embeds provenance in JSON.
        let actual_manifest = apply
            .as_object_mut()
            .and_then(|a| a.remove("_render_manifest"))
            .filter(|v| !v.is_null())
            .unwrap_or(fallback_manifest);
        let manifest = serde_json::to_vec(&actual_manifest).map_err(internal)?;
        let mut metadata_changed = false;
        for domain in &plan.domains {
            metadata_changed |= domains::get(*domain)?.module.accepted(
                &self.state,
                &mut next,
                &request.command,
                &mut apply,
            )?;
        }
        let store = self.persistence.clone();
        let candidate = metadata_changed.then(|| next.clone());
        let committed = tokio::task::spawn_blocking(move || {
            if let Some(candidate) = candidate {
                store.initialize(&candidate)?;
            }
            store.write("rendered.json", &manifest)?;
            store.commit()
        })
        .await
        .map_err(internal)?;
        if let Err(reason) = committed {
            let reason = self.restore(&plan, &reason).await?;
            return Err(ApiError::ApplyFailed {
                domain: effect_owner,
                step: "commit".into(),
                reason,
            });
        }
        self.state = next;
        self.collect_assets();
        for domain in &plan.domains {
            self.failed.remove(domain);
        }
        self.native_drift
            .retain(|(domain, _), _| !plan.domains.contains(domain));
        for domain in &plan.domains {
            let name = domains::get(*domain)?.name;
            let mut observed = DomainObserved::default();
            domains::get(*domain)?
                .module
                .project_observed(&self.state, &mut observed);
            let status = if observed.status == SyncStatus::Applying {
                ApplyStatus::Started
            } else {
                ApplyStatus::Done
            };
            let mut step = ApplyStep::new(name, status);
            step.detail = observed.detail.clone();
            report.steps.push(step);
            self.progress(self.state.revision, name, status, observed.detail);
        }
        let _ = self.events.send(Event::StateChanged {
            revision: self.state.revision,
            domains: plan.domains.clone(),
            origin,
        });
        for domain in &plan.domains {
            let mut observed = DomainObserved::default();
            observed.applied_revision = Some(self.state.domain_revision(*domain));
            domains::get(*domain)?
                .module
                .project_observed(&self.state, &mut observed);
            let _ = self.events.send(Event::Observed {
                domain: *domain,
                delta: observed,
            });
        }
        Ok(Submission {
            reply: CommandReply::new(self.state.revision, report),
            audio_revision: self.state.audio_revision(),
            domain_revision: owner.map_or(self.state.revision, |d| self.state.domain_revision(d)),
            apply,
        })
    }
    fn progress(&self, revision: u64, step: &str, status: ApplyStatus, detail: Option<String>) {
        let _ = self.events.send(Event::ApplyProgress {
            revision,
            step: step.into(),
            status,
            detail,
        });
    }
    /// A supervisor supplies observations; only the actor publishes the decision.
    async fn complete(&mut self, domain: Domain, result: Value) -> Result<(), ApiError> {
        drop(self.refresh().await?);
        for registration in domains::REGISTRY
            .iter()
            .filter(|r| r.ownership() == Ownership::Native)
        {
            registration
                .module
                .guard(&self.state, self.applier.as_ref(), None)
                .await?;
        }
        let _decision_lock = self.refresh().await?;
        let registration = domains::get(domain)?;
        let Some((mut next, plan)) = registration.module.completion(&self.state, &result)? else {
            return Ok(());
        };
        ndimon_types::validate(&next.document).map_err(ApiError::invalid)?;
        self.applier
            .preflight(&self.state, &next, &plan)
            .await
            .map_err(internal)?;
        let changed = next.document != self.state.document;
        if changed {
            next.revision = self
                .state
                .revision
                .checked_add(1)
                .filter(|n| *n <= 9_007_199_254_740_991)
                .ok_or_else(|| internal("revision exhausted"))?;
            next.document.revision = next.revision;
            next.domain_revisions.insert(domain, next.revision);
        }
        let store = self.persistence.clone();
        let previous = self.state.clone();
        let candidate = next.clone();
        tokio::task::spawn_blocking(move || {
            store
                .stage(&previous, &candidate)
                .map_err(|reason| match store.rollback(&previous) {
                    Ok(()) => reason,
                    Err(error) => format!("{reason}; persistence rollback failed: {error}"),
                })
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;
        let apply = match apply::apply_command(self.applier.as_ref(), &next, &plan, None).await {
            Ok(value) => value,
            Err(reason) => {
                let reason = self.restore(&plan, &reason.to_string()).await?;
                return Err(ApiError::ApplyFailed {
                    domain,
                    step: "completion".into(),
                    reason,
                });
            }
        };
        if let Err(error) = registration.module.completed(&mut next, &apply) {
            return Err(internal(self.restore(&plan, &error.to_string()).await?));
        }
        let manifest = match apply.get("_render_manifest").filter(|v| !v.is_null()) {
            Some(manifest) => manifest.clone(),
            None => match self.applier.render(&next).await {
                Ok(manifest) => manifest,
                Err(reason) => return Err(internal(self.restore(&plan, &reason).await?)),
            },
        };
        let bytes = serde_json::to_vec(&manifest).map_err(internal)?;
        let store = self.persistence.clone();
        let candidate = next.clone();
        let saved = tokio::task::spawn_blocking(move || {
            store.initialize(&candidate)?;
            store.write("rendered.json", &bytes)?;
            store.commit()
        })
        .await
        .map_err(internal)?;
        if let Err(reason) = saved {
            let reason = self.restore(&plan, &reason).await?;
            return Err(internal(reason));
        }
        self.state = next;
        if let Some(reason) = apply["_domain_failure"].as_str() {
            self.failed.insert(domain, reason.to_string());
        } else {
            self.failed.remove(&domain);
        }
        if changed {
            let _ = self.events.send(Event::StateChanged {
                revision: self.state.revision,
                domains: vec![domain],
                origin: Origin::Migration,
            });
        }
        let mut observed = DomainObserved::default();
        observed.applied_revision = Some(self.state.domain_revision(domain));
        if let Some(reason) = self.failed.get(&domain) {
            observed.status = SyncStatus::Failed;
            observed.detail = Some(reason.clone());
        }
        registration
            .module
            .project_observed(&self.state, &mut observed);
        self.progress(
            self.state.revision,
            registration.name,
            if observed.status == SyncStatus::Failed {
                ApplyStatus::Failed
            } else {
                ApplyStatus::Done
            },
            observed.detail.clone(),
        );
        let _ = self.events.send(Event::Observed {
            domain,
            delta: observed,
        });
        Ok(())
    }
    async fn reserve(&mut self, next: &StateFile, plan: &Plan) -> Result<(), ApiError> {
        for domain in &plan.domains {
            if let Some(reserved) = domains::get(*domain)?.module.reservation(&self.state, next) {
                let store = self.persistence.clone();
                let candidate = reserved.clone();
                tokio::task::spawn_blocking(move || store.initialize(&candidate))
                    .await
                    .map_err(internal)?
                    .map_err(internal)?;
                self.state = reserved;
            }
        }
        Ok(())
    }
    async fn release_reservation(&mut self, plan: &Plan, reason: &str) -> Result<(), ApiError> {
        let mut next = self.state.clone();
        for domain in &plan.domains {
            domains::get(*domain)?
                .module
                .release_reservation(&mut next, reason);
        }
        if next != self.state {
            let store = self.persistence.clone();
            let candidate = next.clone();
            tokio::task::spawn_blocking(move || store.initialize(&candidate))
                .await
                .map_err(internal)?
                .map_err(internal)?;
            self.state = next;
        }
        Ok(())
    }
    async fn restore(&mut self, plan: &Plan, reason: &str) -> Result<String, ApiError> {
        self.release_reservation(plan, reason).await?;
        let store = self.persistence.clone();
        let previous = self.state.clone();
        tokio::task::spawn_blocking(move || store.rollback(&previous))
            .await
            .map_err(internal)?
            .map_err(internal)?;
        let rollback = self.applier.rollback(&self.state, plan).await;
        let mut detail = match rollback {
            Ok(()) => reason.into(),
            Err(e) => format!("{reason}; rollback failed: {e}"),
        };
        match self.applier.render(&self.state).await {
            Ok(manifest) => {
                let bytes = serde_json::to_vec(&manifest).map_err(internal)?;
                let store = self.persistence.clone();
                if let Err(e) =
                    tokio::task::spawn_blocking(move || store.write("rendered.json", &bytes))
                        .await
                        .map_err(internal)?
                {
                    detail.push_str(&format!("; rollback manifest failed: {e}"));
                }
            }
            Err(e) => detail.push_str(&format!("; rollback render failed: {e}")),
        }
        for domain in &plan.domains {
            self.failed.insert(*domain, detail.clone());
        }
        for domain in &plan.domains {
            self.progress(
                self.state.revision + 1,
                domains::get(*domain)?.name,
                ApplyStatus::Failed,
                Some(detail.clone()),
            );
        }
        Ok(detail)
    }
    async fn follow(&mut self, imported: StateFile) -> Result<(), ApiError> {
        drop(self.refresh().await?);
        for registration in domains::REGISTRY
            .iter()
            .filter(|r| r.ownership() == Ownership::Native)
        {
            registration
                .module
                .guard(&self.state, self.applier.as_ref(), None)
                .await?;
        }
        let _decision_lock = self.refresh().await?;
        self.check_native_drift(&imported.legacy_files)?;
        let mut next = self.state.clone();
        let mut domains = Vec::new();
        let before = serde_json::to_value(&next.document).map_err(internal)?;
        let after = serde_json::to_value(&imported.document).map_err(internal)?;
        for (domain, _) in ndimon_render::OWNERSHIP {
            let registration = domains::get(*domain)?;
            if registration.ownership() == Ownership::Followed
                && before[registration.name] != after[registration.name]
            {
                (registration.copy)(&mut next.document, &imported.document);
                domains.push(registration.domain);
            }
        }
        next.legacy_files = imported.legacy_files;
        // Native exclusive files retain their import text. Mixed-file native
        // projections always merge with the latest followed bytes.
        for path in ndimon_render::PATHS
            .iter()
            .filter(|p| ndimon_render::ownership(p.domain) == Ownership::Native)
        {
            if ndimon_render::is_mixed(path) {
                continue;
            }
            next.legacy_files.remove(path.path);
            if let Some(raw) = self.state.legacy_files.get(path.path) {
                next.legacy_files.insert(path.path.into(), raw.clone());
            }
        }
        let text_changed = next.legacy_files != self.state.legacy_files;
        if text_changed {
            for warning in import::warnings(&next.legacy_files) {
                tracing::warn!(%warning,"followed import retained an unknown key");
            }
        }
        if domains.is_empty() && !text_changed {
            return Ok(());
        }
        ndimon_types::validate(&next.document).map_err(ApiError::invalid)?;
        if !domains.is_empty() {
            next.revision = next
                .revision
                .checked_add(1)
                .filter(|n| *n <= 9_007_199_254_740_991)
                .ok_or_else(|| internal("revision exhausted"))?;
            next.document.revision = next.revision;
            for d in &domains {
                next.domain_revisions.insert(*d, next.revision);
            }
        }
        let store = self.persistence.clone();
        let previous = self.state.clone();
        let candidate = next.clone();
        let changed_revision = !domains.is_empty();
        tokio::task::spawn_blocking(move || {
            if !changed_revision {
                return store.initialize(&candidate);
            }
            if let Err(reason) = store
                .stage(&previous, &candidate)
                .and_then(|()| store.commit())
            {
                return match store.rollback(&previous) {
                    Ok(()) => Err(reason),
                    Err(e) => Err(format!("{reason}; followed-state rollback failed: {e}")),
                };
            }
            Ok(())
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;
        self.state = next;
        drop(_decision_lock);
        if !domains.is_empty() {
            if let Err(error) = self.replay().await {
                tracing::warn!(%error, "follower replay pending after followed commit");
            }
            let _ = self.events.send(Event::StateChanged {
                revision: self.state.revision,
                domains,
                origin: Origin::Migration,
            });
        }
        Ok(())
    }
    fn check_native_drift(&mut self, files: &ndimon_render::LegacyFiles) -> Result<(), ApiError> {
        use sha2::{Digest, Sha256};
        for artifact in ndimon_render::render_with_revisions(
            &self.state.document,
            &self.state.domain_revisions,
            &self.state.legacy_files,
        )
        .map_err(internal)?
        {
            let path = artifact.spec().path;
            let key = (artifact.spec().domain, path.to_string());
            let (expected, actual) =
                ndimon_render::drift_bytes(&artifact, &self.state.document, files)
                    .map_err(internal)?;
            if actual.as_ref() == Some(&expected) {
                self.native_drift.remove(&key);
                continue;
            }
            let expected = format!("{:x}", Sha256::digest(&expected));
            let actual = actual
                .map(|a| format!("{:x}", Sha256::digest(&a)))
                .unwrap_or_else(|| "missing".into());
            if self.native_drift.get(&key) != Some(&actual) {
                self.native_drift.insert(key, actual.clone());
                let _ = self.events.send(Event::Drift {
                    domain: artifact.spec().domain,
                    path: path.into(),
                    artifact: Some(expected),
                    foreign: Some(actual),
                    selector: String::new(),
                    interface: String::new(),
                    neutralized: false,
                    blocking: false,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod replay_tests;
