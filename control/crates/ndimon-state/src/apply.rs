use crate::{BoxFuture, domains, ipc::Decoder, persist::StateFile, plan::Plan};
use ndimon_privd_proto::Request;
use serde_json::{Value, json};
use std::{fmt::Debug, sync::Arc};

pub trait Applier: Debug + Send + Sync + 'static {
    fn replay<'a>(
        &'a self,
        _state: &'a StateFile,
    ) -> BoxFuture<'a, Result<(), ndimon_types::ApiError>> {
        Box::pin(async { Ok(()) })
    }
    fn system(&self, query: ndimon_types::Query) -> BoxFuture<'_, Result<Value, String>> {
        Box::pin(crate::domains::device::system::observe(query))
    }
    fn ndi_sources(&self) -> BoxFuture<'_, Result<Value, String>> {
        Box::pin(crate::ipc::ndi_sources())
    }
    /// The newest release the update agent could verify and install, `None` when
    /// the device runs it already, or why the agent could not say.
    fn latest_release(&self) -> BoxFuture<'_, Result<Option<String>, String>> {
        Box::pin(async { Err("release lookup unavailable".into()) })
    }
    fn render<'a>(&'a self, state: &'a StateFile) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            let artifacts = ndimon_render::render_with_revisions(
                &state.document,
                &state.domain_revisions,
                &state.legacy_files,
            )?;
            serde_json::to_value(ndimon_render::manifest(&artifacts, state.revision))
                .map_err(|e| e.to_string())
        })
    }
    fn preflight<'a>(
        &'a self,
        previous: &'a StateFile,
        next: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<(), String>>;
    fn apply<'a>(
        &'a self,
        next: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<Value, String>>;
    fn rollback<'a>(
        &'a self,
        previous: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<(), String>>;
    fn decoder(&self, command: Value) -> BoxFuture<'_, Result<Value, String>>;
    fn apply_checked<'a>(
        &'a self,
        next: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<Value, ndimon_types::ApiError>> {
        Box::pin(async move {
            self.apply(next, plan)
                .await
                .map_err(|reason| ndimon_types::ApiError::ApplyFailed {
                    domain: plan
                        .domains
                        .first()
                        .copied()
                        .unwrap_or(ndimon_types::Domain::Audio),
                    step: "apply".into(),
                    reason,
                })
        })
    }
    /// The network status that guards, replay and the observer poll. It need
    /// not be a fresh probe: the production applier answers from what it
    /// learned recently unless something changed the network.
    fn network_observation(&self) -> BoxFuture<'_, Result<Value, ndimon_types::ApiError>> {
        self.network(ndimon_privd_proto::NetworkCall::Status)
    }
    fn tessera_query(
        &self,
        query: ndimon_tessera::wire::Query,
    ) -> BoxFuture<'_, Result<Value, ndimon_types::ApiError>> {
        Box::pin(async move {
            match query {
                ndimon_tessera::wire::Query::Status => self
                    .tessera_status()
                    .await
                    .ok_or_else(crate::domains::tessera::offline),
                _ => Err(crate::domains::tessera::offline()),
            }
        })
    }
    fn tessera_bundle_version(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(async { None })
    }
    fn network(
        &self,
        _call: ndimon_privd_proto::NetworkCall,
    ) -> BoxFuture<'_, Result<Value, ndimon_types::ApiError>> {
        Box::pin(async {
            Err(ndimon_types::ApiError::Unsupported {
                what: "network transport unavailable".into(),
            })
        })
    }
    fn plugins(&self, request: Value) -> BoxFuture<'_, Result<Value, String>> {
        Box::pin(crate::domains::plugins::host(request))
    }
    fn plugin_query<'a>(
        &'a self,
        _state: &'a StateFile,
        request: Value,
    ) -> BoxFuture<'a, Result<Value, String>> {
        self.plugins(request)
    }
    fn plugin_privd(&self, _request: Request) -> BoxFuture<'_, Result<Value, String>> {
        Box::pin(async { Err("plugin privileged client unavailable".into()) })
    }
    fn tessera_status(&self) -> BoxFuture<'_, Option<Value>> {
        Box::pin(async { None })
    }
}
/// Shared dispatcher, including injected appliers used by actor/router tests.
/// Live operations cannot change desired state; planned effects use apply/rollback.
pub async fn apply_command(
    applier: &dyn Applier,
    state: &StateFile,
    plan: &Plan,
    command: Option<&ndimon_types::Command>,
) -> Result<Value, ndimon_types::ApiError> {
    if plan.paths.is_empty() {
        if let Some(effect) = command.and_then(|command| {
            domains::command_domain(command)
                .and_then(|domain| domains::get(domain).ok())
                .and_then(|registration| {
                    registration
                        .module
                        .live_checked(applier, state, plan, command)
                })
        }) {
            return effect.await;
        }
        if plan.actions.is_empty() {
            return Ok(json!({"ok":true}));
        }
    }
    applier.apply_checked(state, plan).await
}
/// How long a quiet network observation answers the polling callers. A probe
/// runs a dozen backend tools; the network changes through this process's own
/// commands (which drop the observation), or rarely enough for half a minute.
pub const OBSERVATION_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30);
/// The last network observation of a quiet network, shared by every clone of
/// the applier. A command that may change the network bumps the generation, so
/// an observation that was in flight meanwhile is not kept.
#[derive(Debug, Clone, Default)]
pub struct ObservationCache(Arc<ObservationCell>);
#[derive(Debug, Default)]
struct ObservationCell {
    generation: std::sync::atomic::AtomicU64,
    last: std::sync::Mutex<Option<(std::time::Instant, Value)>>,
}
impl ObservationCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(std::time::Instant, Value)>> {
        self.0
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn generation(&self) -> u64 {
        self.0.generation.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn fresh(&self, max_age: std::time::Duration) -> Option<Value> {
        self.lock()
            .as_ref()
            .filter(|(at, _)| at.elapsed() < max_age)
            .map(|(_, value)| value.clone())
    }
    /// Only a settled network is kept. A pending change, a quiesced or an
    /// externally managed network is polled: each of those ends by itself.
    pub fn keep(&self, generation: u64, observation: &Value) {
        let settled = observation["pending"].is_null()
            && observation["quiesced"] != true
            && observation["externally_managed"] != true;
        let mut last = self.lock();
        *last = (settled && generation == self.generation())
            .then(|| (std::time::Instant::now(), observation.clone()));
    }
    pub fn invalidate(&self) {
        self.0
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.lock() = None;
    }
}
/// Privileged settings (hostname, NTP, reboot schedule) as last applied. A
/// replay sends a setting again only when it differs from what this process
/// last applied or never applied it: after a restart everything is asserted
/// once, and an unchanged system costs no privileged request.
#[derive(Debug, Clone, Default)]
pub struct AppliedSettings(Arc<std::sync::Mutex<std::collections::BTreeMap<&'static str, Value>>>);
impl AppliedSettings {
    fn lock(&self) -> std::sync::MutexGuard<'_, std::collections::BTreeMap<&'static str, Value>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub fn is_applied(&self, setting: &'static str, request: &Value) -> bool {
        self.lock().get(setting) == Some(request)
    }
    pub fn record(&self, setting: &'static str, request: Value) {
        self.lock().insert(setting, request);
    }
    /// A failed request leaves the system in an unknown state: send it again.
    pub fn forget(&self, setting: &'static str) {
        self.lock().remove(setting);
    }
}
/// How long the answer of a release lookup holds, and how long before a failed
/// one (offline, rate limited, nothing signed yet) is tried again. A lookup is
/// made only when a page asks for the version; GitHub allows an address 60
/// anonymous API requests an hour.
pub const RELEASE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);
pub const RELEASE_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);
type ReleaseAnswer = Result<Option<String>, String>;
/// The last answer of the update agent's release lookup, shared by every clone
/// of the applier. One lookup runs at a time: a caller that arrives meanwhile
/// waits for it (the agent bounds it) and takes its answer.
#[derive(Debug, Clone, Default)]
pub struct ReleaseCache(Arc<tokio::sync::Mutex<Option<(std::time::Instant, ReleaseAnswer)>>>);
impl ReleaseCache {
    pub async fn lookup<F, Fut>(&self, fetch: F) -> ReleaseAnswer
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ReleaseAnswer>,
    {
        self.lookup_within(RELEASE_MAX_AGE, RELEASE_RETRY_AFTER, fetch)
            .await
    }
    pub async fn lookup_within<F, Fut>(
        &self,
        max_age: std::time::Duration,
        retry_after: std::time::Duration,
        fetch: F,
    ) -> ReleaseAnswer
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ReleaseAnswer>,
    {
        let mut last = self.0.lock().await;
        if let Some((at, answer)) = last.as_ref() {
            let holds = if answer.is_ok() { max_age } else { retry_after };
            if at.elapsed() < holds {
                return answer.clone();
            }
        }
        let answer = fetch().await;
        *last = Some((std::time::Instant::now(), answer.clone()));
        answer
    }
}
/// What the applier has learned and keeps between calls.
#[derive(Debug, Clone, Default)]
pub struct Memory {
    pub network: ObservationCache,
    pub applied: AppliedSettings,
    pub tessera: crate::domains::tessera::Reloads,
    pub releases: ReleaseCache,
}
#[derive(Debug, Clone)]
pub struct ProductionApplier {
    pub privd: ndimon_privd_proto::client::Client,
    pub decoder: Arc<dyn Decoder>,
    pub finder: Arc<dyn Decoder>,
    pub tessera: ndimon_tessera::Client,
    pub memory: Memory,
}
impl ProductionApplier {
    async fn reconcile(
        &self,
        state: &StateFile,
        plan: &Plan,
        rollback: bool,
    ) -> Result<Value, ndimon_types::ApiError> {
        let rendered = if plan.paths.is_empty() {
            Value::Null
        } else {
            self.privd
                .execute(Request::RenderArtifacts {
                    revision: state.revision,
                })
                .await
                .map_err(|e| match e {
                    ndimon_privd_proto::Error::Invalid { field, reason } => {
                        ndimon_types::ApiError::invalid(vec![ndimon_types::FieldError::new(
                            field, reason,
                        )])
                    }
                    e => crate::plan::internal(e),
                })?
                .data
                .unwrap_or(Value::Null)
        };
        let mut reply = json!({"ok":true});
        for domain in &plan.domains {
            let module = domains::get(*domain)?.module;
            if rollback {
                module
                    .rollback(self, state, plan)
                    .await
                    .map_err(crate::plan::internal)?;
            } else {
                let value = module
                    .apply_checked(self, state, plan)
                    .await
                    .map_err(|error| match error {
                        ndimon_types::ApiError::ApplyFailed { step, reason, .. } => {
                            ndimon_types::ApiError::ApplyFailed {
                                domain: *domain,
                                step,
                                reason,
                            }
                        }
                        error => error,
                    })?;
                for (key, value) in value
                    .as_object()
                    .ok_or_else(|| crate::plan::internal("domain apply reply must be an object"))?
                {
                    reply[key] = value.clone();
                }
            }
        }
        if !plan.paths.is_empty() {
            self.replay(state).await?;
        }
        reply["_render_manifest"] = rendered;
        Ok(reply)
    }
}
impl Applier for ProductionApplier {
    fn replay<'a>(
        &'a self,
        state: &'a StateFile,
    ) -> BoxFuture<'a, Result<(), ndimon_types::ApiError>> {
        Box::pin(async move {
            for registration in domains::REGISTRY
                .iter()
                .filter(|r| r.ownership() == ndimon_render::Ownership::Native)
            {
                registration
                    .module
                    .replay(self, state)
                    .await
                    .map_err(|reason| ndimon_types::ApiError::ApplyFailed {
                        domain: registration.domain,
                        step: "replay".into(),
                        reason,
                    })?;
            }
            Ok(())
        })
    }
    fn render<'a>(&'a self, state: &'a StateFile) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            self.privd
                .execute(Request::RenderArtifacts {
                    revision: state.revision,
                })
                .await
                .map_err(|e| e.to_string())?
                .data
                .ok_or_else(|| "renderer did not return provenance".into())
        })
    }
    fn preflight<'a>(
        &'a self,
        previous: &'a StateFile,
        next: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            for domain in &plan.domains {
                domains::get(*domain)
                    .map_err(|e| e.to_string())?
                    .module
                    .preflight(self, previous, next, plan)
                    .await?;
            }
            Ok(())
        })
    }
    fn apply<'a>(
        &'a self,
        next: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            self.reconcile(next, plan, false)
                .await
                .map_err(|e| e.to_string())
        })
    }
    fn rollback<'a>(
        &'a self,
        previous: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.reconcile(previous, plan, true)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }
    fn decoder(&self, command: Value) -> BoxFuture<'_, Result<Value, String>> {
        self.decoder.request(command)
    }
    fn latest_release(&self) -> BoxFuture<'_, Result<Option<String>, String>> {
        Box::pin(self.memory.releases.lookup(move || async move {
            let output =
                self.privd
                    .execute(Request::CheckUpdate)
                    .await
                    .map_err(|error| match error {
                        ndimon_privd_proto::Error::Failed { reason, .. } => reason,
                        error => error.to_string(),
                    })?;
            let data = output.data.unwrap_or(Value::Null);
            match &data["latest_version"] {
                Value::String(version) => Ok(Some(version.clone())),
                Value::Null => Ok(None),
                _ => Err("the update agent gave no release answer".into()),
            }
        }))
    }
    fn apply_checked<'a>(
        &'a self,
        next: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<Value, ndimon_types::ApiError>> {
        Box::pin(self.reconcile(next, plan, false))
    }
    fn network(
        &self,
        call: ndimon_privd_proto::NetworkCall,
    ) -> BoxFuture<'_, Result<Value, ndimon_types::ApiError>> {
        Box::pin(async move {
            // Whatever may change the network, successful or not, makes what
            // was observed before it stale.
            let reads = call.is_read_only();
            if !reads {
                self.memory.network.invalidate();
            }
            let output = self
                .privd
                .execute(Request::NetworkNative { call })
                .await
                .map_err(|e| ndimon_types::ApiError::ApplyFailed {
                    domain: ndimon_types::Domain::Network,
                    step: "privd".into(),
                    reason: e.to_string(),
                });
            if !reads {
                self.memory.network.invalidate();
            }
            let value = output?
                .data
                .ok_or_else(|| crate::plan::internal("missing network reply"))?;
            if let Some(error) = value.get("err") {
                return Err(serde_json::from_value(error.clone()).map_err(crate::plan::internal)?);
            }
            value
                .get("ok")
                .cloned()
                .ok_or_else(|| crate::plan::internal("invalid network reply"))
        })
    }
    fn network_observation(&self) -> BoxFuture<'_, Result<Value, ndimon_types::ApiError>> {
        Box::pin(async move {
            if let Some(observation) = self.memory.network.fresh(OBSERVATION_MAX_AGE) {
                return Ok(observation);
            }
            let generation = self.memory.network.generation();
            let observation = self
                .network(ndimon_privd_proto::NetworkCall::Observe)
                .await?;
            self.memory.network.keep(generation, &observation);
            Ok(observation)
        })
    }
    fn tessera_query(
        &self,
        query: ndimon_tessera::wire::Query,
    ) -> BoxFuture<'_, Result<Value, ndimon_types::ApiError>> {
        Box::pin(async move {
            let reply = self
                .tessera
                .request(ndimon_tessera::wire::RequestBody::Query(query))
                .await
                .map_err(crate::domains::tessera::api_error)?;
            match reply {
                ndimon_tessera::wire::Reply::Status(s) => {
                    serde_json::to_value(s).map_err(crate::plan::internal)
                }
                ndimon_tessera::wire::Reply::Metrics(m) => {
                    serde_json::to_value(m).map_err(crate::plan::internal)
                }
                _ => Err(crate::plan::internal("unexpected Tessera query response")),
            }
        })
    }
    fn tessera_bundle_version(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(async {
            let path = "/opt/ndimon-r/plugins/tessera/bundle-info.json";
            if tokio::fs::metadata(path).await.ok()?.len() > 65536 {
                return None;
            }
            let bytes = tokio::fs::read(path).await.ok()?;
            let value: Value = serde_json::from_slice(&bytes).ok()?;
            value["version"].as_str().map(str::to_string)
        })
    }
    fn plugins(&self, request: Value) -> BoxFuture<'_, Result<Value, String>> {
        Box::pin(crate::domains::plugins::host(request))
    }

    fn plugin_query<'a>(
        &'a self,
        state: &'a StateFile,
        mut request: Value,
    ) -> BoxFuture<'a, Result<Value, String>> {
        request["_context"] = crate::domains::plugins::host::context(state);
        Box::pin(crate::domains::plugins::host(request))
    }
    fn plugin_privd(&self, request: Request) -> BoxFuture<'_, Result<Value, String>> {
        Box::pin(async move { crate::domains::plugins::execute(&self.privd, request).await })
    }
    fn tessera_status(&self) -> BoxFuture<'_, Option<Value>> {
        Box::pin(async move {
            self.tessera_query(ndimon_tessera::wire::Query::Status)
                .await
                .ok()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndimon_privd_proto::{ActionOutput, Envelope, NetworkCall, Reply, ReplyResult, codec};
    use std::sync::Mutex;

    /// A privd that answers every network call with the next canned reply.
    struct Privd {
        requests: Arc<Mutex<Vec<Request>>>,
        replies: Arc<Mutex<Vec<Value>>>,
        _directory: tempfile::TempDir,
        applier: ProductionApplier,
    }
    #[derive(Debug)]
    struct NoDecoder;
    impl Decoder for NoDecoder {
        fn request(&self, _: Value) -> BoxFuture<'_, Result<Value, String>> {
            Box::pin(async { Err("no decoder".into()) })
        }
    }
    fn privd() -> Privd {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("privd.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::UnixListener::from_std(listener).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let replies = Arc::new(Mutex::new(Vec::<Value>::new()));
        let (seen, canned) = (requests.clone(), replies.clone());
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let envelope: Envelope = codec::read_async(&mut stream).await.unwrap();
                seen.lock().unwrap().push(envelope.req);
                let body = {
                    let mut canned = canned.lock().unwrap();
                    if canned.is_empty() {
                        json!({"ok":{"pending":null,"drift":[]}})
                    } else {
                        canned.remove(0)
                    }
                };
                let reply = Reply {
                    v: 1,
                    id: envelope.id,
                    result: ReplyResult::Ok(ActionOutput {
                        data: Some(body),
                        ..Default::default()
                    }),
                };
                codec::write_async(&mut stream, &reply).await.unwrap();
            }
        });
        let applier = ProductionApplier {
            privd: ndimon_privd_proto::client::Client::new(
                socket,
                std::time::Duration::from_secs(2),
            ),
            decoder: Arc::new(NoDecoder),
            finder: Arc::new(NoDecoder),
            tessera: ndimon_tessera::Client::default(),
            memory: Default::default(),
        };
        Privd {
            requests,
            replies,
            _directory: directory,
            applier,
        }
    }
    fn calls(p: &Privd) -> Vec<NetworkCall> {
        p.requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| match r {
                Request::NetworkNative { call } => call.clone(),
                other => panic!("unexpected request {other:?}"),
            })
            .collect()
    }

    #[tokio::test]
    async fn a_settled_network_is_observed_once_until_something_changes_it() {
        let p = privd();
        for _ in 0..5 {
            p.applier.network_observation().await.unwrap();
        }
        assert_eq!(calls(&p), [NetworkCall::Observe]);
        // Reads of other kinds neither refresh nor drop what was learned.
        p.applier.network(NetworkCall::Status).await.unwrap();
        p.applier.network_observation().await.unwrap();
        assert_eq!(calls(&p), [NetworkCall::Observe, NetworkCall::Status]);
        // A command that may change the network does, failed or not.
        p.applier.network(NetworkCall::Rollback).await.unwrap();
        p.applier.network_observation().await.unwrap();
        p.applier.network_observation().await.unwrap();
        assert_eq!(
            calls(&p),
            [
                NetworkCall::Observe,
                NetworkCall::Status,
                NetworkCall::Rollback,
                NetworkCall::Observe
            ]
        );
        // Every clone of the applier shares one memory.
        let clone = p.applier.clone();
        clone.network_observation().await.unwrap();
        assert_eq!(calls(&p).len(), 4);
        clone
            .network(NetworkCall::Revert {
                token: "a".repeat(64),
            })
            .await
            .unwrap();
        p.applier.network_observation().await.unwrap();
        assert_eq!(calls(&p).len(), 6);
    }

    #[tokio::test]
    async fn a_network_in_transition_is_polled_every_time() {
        let p = privd();
        let pending = json!({"ok":{"pending":{"phase":"applied"},"drift":[]}});
        let quiesced = json!({"ok":{"pending":null,"quiesced":true,"drift":[]}});
        let external = json!({"ok":{"pending":null,"externally_managed":true,"drift":[]}});
        *p.replies.lock().unwrap() = vec![pending.clone(), pending, quiesced, external];
        for _ in 0..4 {
            p.applier.network_observation().await.unwrap();
        }
        assert_eq!(calls(&p).len(), 4);
        // ... and the end of the transition is seen at once, then cached.
        p.applier.network_observation().await.unwrap();
        p.applier.network_observation().await.unwrap();
        assert_eq!(calls(&p).len(), 5);
    }

    #[tokio::test]
    async fn an_observation_in_flight_during_a_change_is_not_kept() {
        let cache = ObservationCache::default();
        let generation = cache.generation();
        cache.invalidate();
        cache.keep(generation, &json!({"pending":null}));
        assert!(cache.fresh(OBSERVATION_MAX_AGE).is_none());
        cache.keep(cache.generation(), &json!({"pending":null}));
        assert!(cache.fresh(OBSERVATION_MAX_AGE).is_some());
        assert!(cache.fresh(std::time::Duration::ZERO).is_none());
    }

    #[tokio::test]
    async fn the_release_lookup_asks_the_update_agent_once_and_keeps_its_answer() {
        let p = privd();
        *p.replies.lock().unwrap() = vec![json!({"latest_version":"1.4.1-1"})];
        assert_eq!(p.applier.latest_release().await, Ok(Some("1.4.1-1".into())));
        // Every clone of the applier shares the answer.
        assert_eq!(
            p.applier.clone().latest_release().await,
            Ok(Some("1.4.1-1".into()))
        );
        assert_eq!(*p.requests.lock().unwrap(), [Request::CheckUpdate]);
        // A device that runs the newest release is told so.
        let p = privd();
        *p.replies.lock().unwrap() = vec![json!({"latest_version":null})];
        assert_eq!(p.applier.latest_release().await, Ok(None));
    }

    #[tokio::test]
    async fn an_unusable_answer_or_an_unreachable_agent_is_a_failed_lookup() {
        let p = privd();
        *p.replies.lock().unwrap() = vec![json!({"latest_version":5})];
        assert!(p.applier.latest_release().await.is_err());
        let applier = ProductionApplier {
            privd: ndimon_privd_proto::client::Client::new(
                "/nonexistent/privd.sock",
                std::time::Duration::from_secs(1),
            ),
            decoder: Arc::new(NoDecoder),
            finder: Arc::new(NoDecoder),
            tessera: ndimon_tessera::Client::default(),
            memory: Default::default(),
        };
        assert!(applier.latest_release().await.is_err());
    }

    #[tokio::test]
    async fn release_answers_age_out_and_failures_are_tried_again_sooner() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;
        let (hour, never) = (Duration::from_secs(3600), Duration::ZERO);
        let fetches = AtomicUsize::new(0);
        let ask = |cache: &ReleaseCache, max_age, retry_after, answer: ReleaseAnswer| {
            let fetches = &fetches;
            let cache = cache.clone();
            async move {
                cache
                    .lookup_within(max_age, retry_after, || async {
                        fetches.fetch_add(1, Ordering::SeqCst);
                        answer
                    })
                    .await
            }
        };
        let good: ReleaseAnswer = Ok(Some("1.4.1-1".into()));
        let bad: ReleaseAnswer = Err("offline".into());
        // A good answer holds for max_age, whatever the retry time.
        let cache = ReleaseCache::default();
        assert_eq!(ask(&cache, hour, never, good.clone()).await, good);
        assert_eq!(ask(&cache, hour, never, bad.clone()).await, good);
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        assert_eq!(ask(&cache, never, hour, bad.clone()).await, bad);
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
        // A failure holds for the retry time only, and does not replace itself early.
        assert_eq!(ask(&cache, never, hour, good.clone()).await, bad);
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
        assert_eq!(ask(&cache, hour, never, good.clone()).await, good);
        assert_eq!(fetches.load(Ordering::SeqCst), 3);
        // Callers that arrive while a lookup runs wait for it: one fetch.
        let cache = ReleaseCache::default();
        let slow_fetches = AtomicUsize::new(0);
        let slow = |cache: ReleaseCache| {
            let slow_fetches = &slow_fetches;
            async move {
                cache
                    .lookup_within(hour, hour, || async {
                        slow_fetches.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        Ok(None)
                    })
                    .await
            }
        };
        let answers = tokio::join!(slow(cache.clone()), slow(cache.clone()), slow(cache));
        assert_eq!(answers, (Ok(None), Ok(None), Ok(None)));
        assert_eq!(slow_fetches.load(Ordering::SeqCst), 1);
    }
}
