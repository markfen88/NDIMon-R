//! Native registry and lifecycle. Manifest-v1 plugins stay installed but inactive.
use crate::{
    BoxFuture,
    apply::{Applier, ProductionApplier},
    persist::StateFile,
    plan::{Action as DomainAction, DomainPlan, Plan, internal},
};
pub use ndimon_privd_proto::Request as PrivdRequest;
use ndimon_privd_proto::{Request, client::Client};
use ndimon_render::LegacyFiles;
use ndimon_types::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
mod backup;
pub mod host;
mod jobs;
pub mod runtime;
pub use jobs::{JobOutcome, JobResult, PendingJob, spawn_jobs, wait_result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    PluginsInstall {
        name: String,
        archive: String,
        version: String,
        enable: bool,
        force: bool,
    },
    PluginsEnable {
        name: String,
        enabled: bool,
    },
    PluginsRemove {
        name: String,
        purge: bool,
        force: bool,
    },
    PluginsCancel {
        job: String,
    },
    PluginsReset,
    PluginsRestore,
    PluginsComplete {
        pending: Box<PendingJob>,
        outcome: JobOutcome,
    },
}
#[derive(Debug)]
pub struct Module;
pub fn invalid(reason: impl ToString) -> ApiError {
    ApiError::invalid(vec![FieldError::new("plugins", reason.to_string())])
}
fn name(value: &str) -> Result<(), ApiError> {
    ndimon_privd_proto::PluginName::try_from(value.to_string()).map_err(invalid)?;
    Ok(())
}
impl super::DomainModule for Module {
    fn guard<'a>(
        &self,
        state: &'a StateFile,
        applier: &'a dyn Applier,
        command: Option<&'a Command>,
    ) -> BoxFuture<'a, Result<(), ApiError>> {
        Box::pin(async move {
            let competing = command.is_some_and(|c| {
                super::command_domain(c) == Some(Domain::Plugins)
                    && !matches!(c, Command::CancelPluginJob { .. })
                    || matches!(c, Command::ImportBackup { document, .. }
                        if document.document.plugins != state.document.plugins || document.legacy_sections.iter().any(|s| s == "plugins"))
            });
            if state.plugin_job.is_some() && competing {
                return Err(ApiError::StillApplying);
            }
            if let Some(Command::SetPluginEnabled {
                name,
                enabled: true,
                ..
            }) = command
            {
                let entry = applier
                    .plugin_query(state, json!({"operation":"manifest","name":name}))
                    .await
                    .map_err(internal)?;
                if let Some(warning) = entry["migration_warning"].as_str() {
                    return Err(invalid(warning));
                }
            }
            if let Some(Command::SetPluginData { name, state: data }) = command {
                let entry = applier
                    .plugin_query(state, json!({"operation":"manifest","name":name}))
                    .await
                    .map_err(internal)?;
                let manifest: ndimon_privd_proto::plugin::Manifest =
                    serde_json::from_value(entry["manifest"].clone()).map_err(invalid)?;
                if manifest.process.is_none() {
                    return Err(invalid("plugin has no process state"));
                }
                let schema = manifest
                    .state
                    .ok_or_else(|| invalid("plugin has no process state"))?;
                ndimon_privd_proto::plugin::validate_value(&schema.schema, &json!(data))
                    .map_err(invalid)?;
            }
            Ok(())
        })
    }
    fn prepare(
        &self,
        previous: &StateFile,
        next: &mut StateFile,
        command: &Command,
    ) -> Result<(), ApiError> {
        jobs::prepare(previous, next, command)
    }
    fn reservation(&self, previous: &StateFile, next: &StateFile) -> Option<StateFile> {
        if previous.plugin_job.is_none() && next.plugin_job.is_some() {
            let mut reserved = previous.clone();
            reserved.plugin_job = next.plugin_job.clone();
            Some(reserved)
        } else {
            None
        }
    }
    fn release_reservation(&self, state: &mut StateFile, reason: &str) {
        if state.plugin_job.as_ref().is_some_and(|p| p.id == "unknown")
            && !reason.contains("uncertain:")
        {
            state.plugin_job = None;
        }
    }
    fn accepted(
        &self,
        previous: &StateFile,
        next: &mut StateFile,
        command: &Command,
        apply: &mut Value,
    ) -> Result<bool, ApiError> {
        jobs::accepted(previous, next, command, apply)
    }
    fn completion(
        &self,
        state: &StateFile,
        result: &Value,
    ) -> Result<Option<(StateFile, Plan)>, ApiError> {
        if let Some(update) = result.get("runtime") {
            let name = update["name"]
                .as_str()
                .ok_or_else(|| invalid("missing runtime plugin"))?;
            if state
                .document
                .plugins
                .get(name)
                .is_none_or(|p| update["version"] != p.version)
            {
                return Ok(None);
            }
            let mut next = state.clone();
            let mut runtime = json!(next.plugin_runtime);
            if !runtime["plugins"].is_object() {
                runtime["plugins"] = json!({});
            }
            runtime["plugins"][name] = update.clone();
            next.plugin_runtime = runtime
                .as_object()
                .cloned()
                .ok_or_else(|| invalid("invalid runtime update"))?;
            return Ok(Some((
                next,
                Plan {
                    paths: vec![],
                    domains: vec![Domain::Plugins],
                    actions: vec![],
                    impact: Impact::default(),
                    confirmation: None,
                    external_confirmation: false,
                    session: None,
                },
            )));
        }
        jobs::completion(state, result)
    }
    fn completed(&self, state: &mut StateFile, apply: &Value) -> Result<(), ApiError> {
        if apply.get("_plugin_result").is_none() {
            return Ok(());
        }
        jobs::completed(state, apply)
    }
    fn observe(
        &self,
        handle: crate::StateHandle,
        _applier: &ProductionApplier,
    ) -> Option<BoxFuture<'static, ()>> {
        let runtime_handle = handle.clone();
        Some(Box::pin(async move {
            tokio::join!(jobs::observe(handle), runtime::observe(runtime_handle));
        }))
    }
    fn query_outside_actor(&self, query: &Query) -> bool {
        matches!(
            query,
            Query::Plugins
                | Query::PluginFeed
                | Query::RefreshPluginFeed
                | Query::PluginStatus { .. }
                | Query::PluginJob { .. }
                | Query::PreparePluginInstall { .. }
        )
    }
    fn project_observed(&self, state: &StateFile, observed: &mut DomainObserved) {
        if let Some(pending) = &state.plugin_job {
            observed.applied_revision = None;
            observed.status = if pending.investigation {
                SyncStatus::Failed
            } else {
                SyncStatus::Applying
            };
            observed.detail = Some(if pending.investigation {
                format!("plugin job {}; operator investigation required", pending.id)
            } else {
                format!("plugin job {}", pending.id)
            });
        }
    }
    fn imperative(&self, _: &Command) -> bool {
        true
    }
    fn reduce(&self, state: &mut DesiredState, command: &Command) -> Result<(), ApiError> {
        match command {
            Command::InstallPlugin {
                name: n,
                archive,
                version,
                enable,
                ..
            }
            | Command::UpdatePlugin {
                name: n,
                archive,
                version,
                enable,
                ..
            } => {
                name(n)?;
                ndimon_privd_proto::ArchiveId::try_from(archive.clone()).map_err(invalid)?;
                ndimon_privd_proto::DebianVersion::try_from(version.clone()).map_err(invalid)?;
                if matches!(command, Command::UpdatePlugin { .. }) {
                    let installed = state
                        .plugins
                        .get(n)
                        .ok_or_else(|| invalid("plugin not installed"))?;
                    if !compare_versions(version, &installed.version).is_gt() {
                        return Err(invalid("no newer version"));
                    }
                }
                let mut entry = PluginState::default();
                entry.installed = true;
                entry.enabled = *enable;
                entry.version = version.clone();
                if let Some(previous) = state.plugins.get(n) {
                    entry.state = previous.state.clone();
                }
                state.plugins.insert(n.clone(), entry);
            }
            Command::SetPluginEnabled {
                name: n,
                enabled,
                version,
            } => {
                name(n)?;
                if state.plugins.get(n).is_none_or(|p| !p.installed) {
                    let mut entry = PluginState::default();
                    entry.installed = true;
                    entry.version = version
                        .clone()
                        .ok_or_else(|| invalid("plugin not installed"))?;
                    state.plugins.insert(n.clone(), entry);
                }
                let entry = state
                    .plugins
                    .get_mut(n)
                    .ok_or_else(|| invalid("plugin not installed"))?;
                if !entry.installed {
                    return Err(invalid("plugin not installed"));
                }
                entry.enabled = *enabled;
            }
            Command::RemovePlugin { name: n, .. } => {
                name(n)?;
                state.plugins.remove(n);
            }
            Command::SetPluginData {
                name: n,
                state: data,
            } => {
                name(n)?;
                let entry = state
                    .plugins
                    .get_mut(n)
                    .ok_or_else(|| invalid("plugin not installed"))?;
                entry.state = Some(data.clone());
            }
            Command::ResetPluginState => state.plugins.clear(),
            Command::CancelPluginJob { job } => {
                // An acknowledgement that never named its unit is parked under
                // this id; cancelling it is the way out of that marker.
                if job != "unknown" {
                    ndimon_privd_proto::PluginJob::try_from(job.clone()).map_err(invalid)?;
                }
            }
            _ => {
                return Err(ApiError::Unsupported {
                    what: "unsupported plugin command".into(),
                });
            }
        }
        Ok(())
    }
    fn import(&self, state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String> {
        ndimon_render::plugins::import_json(state, files)
    }
    fn plan(
        &self,
        previous: &DesiredState,
        next: &DesiredState,
        command: &Command,
    ) -> Result<DomainPlan, ApiError> {
        if super::command_domain(command) == Some(Domain::Tessera) {
            return Ok(DomainPlan::default());
        }
        if crate::backup::handles(command) {
            let ordinary = |state: &DesiredState| {
                state
                    .plugins
                    .iter()
                    .filter(|(name, _)| name.as_str() != "tessera")
                    .map(|(n, p)| (n.clone(), p.clone()))
                    .collect::<std::collections::BTreeMap<_, _>>()
            };
            if ordinary(previous) == ordinary(next) {
                return Ok(DomainPlan::default());
            }
            let mut impact = Impact::new(ImpactScope::Service);
            impact
                .interruptions
                .push(Interruption::new(Domain::Plugins, "plugin loader", None));
            impact
                .lines
                .push("Restore accepted plugin intent to the plugin loader".into());
            return Ok(DomainPlan {
                actions: vec![DomainAction::Plugins(Action::PluginsRestore)],
                impact,
            });
        }
        let action = match command {
            Command::InstallPlugin {
                name,
                archive,
                version,
                enable,
                force,
            }
            | Command::UpdatePlugin {
                name,
                archive,
                version,
                enable,
                force,
            } => Action::PluginsInstall {
                name: name.clone(),
                archive: archive.clone(),
                version: version.clone(),
                enable: *enable,
                force: *force,
            },
            Command::SetPluginEnabled { name, enabled, .. } => Action::PluginsEnable {
                name: name.clone(),
                enabled: *enabled,
            },
            Command::RemovePlugin { name, purge, force } => Action::PluginsRemove {
                name: name.clone(),
                purge: *purge,
                force: *force,
            },
            Command::CancelPluginJob { job } => Action::PluginsCancel { job: job.clone() },
            Command::SetPluginData { .. } => return Ok(DomainPlan::default()),
            Command::ResetPluginState => Action::PluginsReset,
            _ => return Err(invalid("unsupported plugin plan")),
        };
        let mut impact = Impact::default();
        let interruption = match &action {
            Action::PluginsInstall { name, .. }
            | Action::PluginsEnable { name, .. }
            | Action::PluginsRemove { name, .. } => Some((
                format!("plugin {name} services and UI"),
                format!("Change plugin {name}; its services and UI may stop or restart"),
            )),
            Action::PluginsCancel { job } => Some((
                format!("plugin lifecycle job {job}"),
                format!("Cancel plugin lifecycle job {job}; its running hook may be interrupted"),
            )),
            Action::PluginsReset => Some((
                "all plugin UI panels".into(),
                "Reset plugin state and disable all plugin UI panels".into(),
            )),
            Action::PluginsComplete { .. } | Action::PluginsRestore => None,
        };
        if let Some((resource, line)) = interruption {
            impact.scope = ImpactScope::Service;
            impact
                .interruptions
                .push(Interruption::new(Domain::Plugins, resource, None));
            impact.lines.push(line);
        }
        Ok(DomainPlan {
            actions: vec![DomainAction::Plugins(action)],
            impact,
        })
    }
    fn preflight<'a>(
        &self,
        applier: &'a ProductionApplier,
        previous: &'a StateFile,
        next: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            for (name, plugin) in &next.document.plugins {
                if plugin.state.is_some()
                    && previous
                        .document
                        .plugins
                        .get(name)
                        .is_none_or(|p| p.state != plugin.state)
                {
                    let entry = applier
                        .plugin_query(next, json!({"operation":"manifest","name":name}))
                        .await?;
                    let manifest: ndimon_privd_proto::plugin::Manifest =
                        serde_json::from_value(entry["manifest"].clone())
                            .map_err(|e| e.to_string())?;
                    let schema = manifest.state.ok_or("plugin has no process state")?;
                    ndimon_privd_proto::plugin::validate_value(
                        &schema.schema,
                        &json!(plugin.state),
                    )?;
                }
            }
            if plan.actions.iter().any(|a| {
                matches!(
                    a,
                    DomainAction::Plugins(
                        Action::PluginsInstall { .. }
                            | Action::PluginsEnable { .. }
                            | Action::PluginsRemove { .. }
                    )
                )
            }) {
                bridge(applier, json!({"operation":"check"})).await?;
            }
            Ok(())
        })
    }
    fn apply<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(run(applier, state, plan))
    }
    fn rollback<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(restore(applier, state, plan))
    }
    fn replay<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if state.document.plugins.keys().all(|name| name == "tessera")
                || state.plugin_job.is_some()
            {
                return Ok(());
            }
            deliver(applier, state).await
        })
    }

    fn query<'a>(
        &self,
        state: &'a StateFile,
        applier: &'a dyn Applier,
        query: &'a Query,
    ) -> Option<BoxFuture<'a, Result<Value, ApiError>>> {
        let request = match query {
            Query::Plugins => json!({"operation":"list"}),
            Query::PluginFeed => json!({"operation":"feed"}),
            Query::RefreshPluginFeed => json!({"operation":"refresh"}),
            Query::PluginStatus { name } => json!({"operation":"manifest","name":name}),
            Query::PreparePluginInstall {
                name,
                source,
                version,
            } => json!({"operation":"prepare","name":name,"source":source,"version":version}),
            Query::PluginJob { job } => {
                let job = match ndimon_privd_proto::PluginJob::try_from(job.clone()) {
                    Ok(job) => job,
                    Err(e) => return Some(Box::pin(async { Err(invalid(e)) })),
                };
                return Some(Box::pin(async move {
                    applier
                        .plugin_privd(Request::PluginJobStatus { job })
                        .await
                        .map_err(internal)
                }));
            }
            _ => return None,
        };
        Some(Box::pin(async move {
            // Feed fetches (10 s) and archive preparation (300 s) use the host's
            // 310 s bound outside the actor, not its short acknowledgement bound.
            let mut result = applier
                .plugin_query(state, request)
                .await
                .map_err(internal)?;
            if result.get("error").is_some() {
                return Ok(result);
            }
            if let Query::PreparePluginInstall { name, .. } = query {
                let req: Request = serde_json::from_value(json!({"plugin_inspect":{"name":name,"archive":{"download":{"id":result["archive"]}},"expected_version":result.get("version")}})).map_err(internal)?;
                let inspected = match jobs::inspect(applier, req).await {
                    Ok(inspected) => inspected,
                    // The runtime bundle was refused before anything was accepted
                    // (NI-19 W2): no install is submitted, so nothing is rendered.
                    Err(refusal)
                        if refusal.starts_with("invalid bundle: ")
                            || refusal.starts_with("unsupported: runtime bundle") =>
                    {
                        return Ok(json!({"error":refusal,"status":400,"refused":true}));
                    }
                    Err(error) => return Err(internal(error)),
                };
                let manifest: Value =
                    serde_json::from_str(inspected["stdout"].as_str().unwrap_or(""))
                        .map_err(internal)?;
                if let (Some(host), Some(minimum)) = (
                    result["host_version"].as_str(),
                    manifest["min_ndimon"].as_str(),
                ) {
                    if compare_versions(host, minimum).is_lt() {
                        return Ok(json!({"error":"host_too_old","status":409}));
                    }
                }
                if let Some(plugin) = state
                    .document
                    .plugins
                    .get(name)
                    .filter(|p| p.state.is_some())
                {
                    if manifest["process"].is_object() {
                        ndimon_privd_proto::plugin::validate_value(
                            &manifest["state"]["schema"],
                            &json!(plugin.state),
                        )
                        .map_err(invalid)?;
                    }
                }
                result["version"] = manifest["version"].clone();
                result["manifest"] = manifest;
            }
            Ok(result)
        }))
    }
}
/// Exposed separately so injected appliers exercise the same lifecycle ordering.
pub async fn run(applier: &dyn Applier, state: &StateFile, plan: &Plan) -> Result<Value, String> {
    let mut result = json!({"ok":true});
    for action in &plan.actions {
        let DomainAction::Plugins(action) = action else {
            continue;
        };
        let mut log = String::new();
        match action {
            Action::PluginsInstall {
                name,
                archive,
                version,
                enable,
                force,
            } => {
                stop_process(applier, name).await?;
                let request = serde_json::from_value(json!({"plugin_install":{"name":name,"archive":{"download":{"id":archive}},"expected_version":version,"force":force}})).map_err(|e| e.to_string())?;
                let installed = start(applier, request).await?;
                if installed["job"].is_string() {
                    result =
                        json!({"ok":true,"job":installed["job"],"_plugin_job":installed["job"]});
                    continue;
                }
                log.push_str(installed["stdout"].as_str().unwrap_or(""));
                log.push_str(installed["stderr"].as_str().unwrap_or(""));
                let feedback = bridge(applier, json!({"operation":"installed","name":name,"version":version,"pending_enable":enable,"archive":archive}))
                    .await
                    .unwrap_or_else(|error| json!({"load_error":error}));
                result = installed_reply(
                    applier,
                    name,
                    *enable,
                    feedback.get("load_error").cloned().unwrap_or(Value::Null),
                    &log,
                )
                .await;
            }
            Action::PluginsEnable { name, enabled } => {
                let entry = bridge(applier, json!({"operation":"manifest","name":name})).await?;
                // A manifest-v1 plugin reports `error` while enabled; disabling it
                // is the one transition that stays possible.
                let inactive_legacy = !*enabled && entry["migration_warning"].is_string();
                if !inactive_legacy
                    && !["installed", "enabled"]
                        .contains(&entry["state"].as_str().unwrap_or("error"))
                {
                    return Err(entry["error"]
                        .as_str()
                        .unwrap_or("plugin not installed")
                        .into());
                }
                if state
                    .document
                    .plugins
                    .get(name)
                    .is_some_and(|p| entry["version"].as_str() != Some(p.version.as_str()))
                {
                    return Err("installed version differs from accepted plugin version".into());
                }
                let output = services(applier, name, *enabled, &entry, true).await?;
                if output["job"].is_string() {
                    result = json!({"ok":true,"job":output["job"],"_plugin_job":output["job"]});
                    continue;
                }
                log = output["stdout"].as_str().unwrap_or("").to_string();
                deliver(applier, state).await?;
                bridge(
                    applier,
                    json!({"operation":"enabled","name":name,"enabled":enabled}),
                )
                .await?;
                result =
                    json!({"ok":true,"state":if *enabled {"enabled"} else {"installed"},"log":log});
            }
            Action::PluginsRemove { name, purge, force } => {
                stop_process(applier, name).await?;
                let request = serde_json::from_value(json!({"plugin_uninstall":{"name":name,"mode":if *force {"force"} else if *purge {"purge"} else {"keep_state"}}})).map_err(|e|e.to_string())?;
                let output = start(applier, request).await?;
                if output["job"].is_string() {
                    result = json!({"ok":true,"job":output["job"],"_plugin_job":output["job"]});
                    continue;
                }
                let _ = bridge(applier, json!({"operation":"removed","name":name})).await;
                result = json!({"ok":true,"state":"removed","log":format!("{}{}",output["stdout"].as_str().unwrap_or(""),output["stderr"].as_str().unwrap_or(""))});
            }
            Action::PluginsCancel { job } => {
                // Nothing to stop for an acknowledgement that named no unit.
                if job != "unknown" {
                    privileged(
                        applier,
                        serde_json::from_value(json!({"plugin_job_cancel":{"job":job}}))
                            .map_err(|e| e.to_string())?,
                    )
                    .await?;
                }
            }
            Action::PluginsReset => {
                privileged(applier, Request::PluginResetState).await?;
                deliver(applier, state).await?;
                bridge(applier, json!({"operation":"reset"})).await?;
            }
            Action::PluginsRestore => {
                deliver(applier, state).await?;
            }
            Action::PluginsComplete { pending, outcome } => {
                result = jobs::finish(applier, state, pending, outcome).await?;
            }
        }
    }
    Ok(result)
}
/// Native host. This path never contacts the legacy plugin socket.
pub async fn host(request: Value) -> Result<Value, String> {
    host::native().request(request).await
}
/// One acknowledgement only. Long-job observation belongs to the supervisor.
pub async fn execute(client: &Client, request: Request) -> Result<Value, String> {
    serde_json::to_value(client.request(request).await.map_err(|e| {
        if matches!(&e, ndimon_privd_proto::Error::Failed { step, reason, .. } if ["send", "receive", "request"].contains(&step.as_str()) || reason.contains("NDIMON_PLUGIN_JOB=")) {
            format!("uncertain: {e}")
        } else { e.to_string() }
    })?).map_err(|e| e.to_string())
}

const CALL_TIMEOUT: Duration = Duration::from_secs(2);
async fn bridge(applier: &dyn Applier, mut request: Value) -> Result<Value, String> {
    // Startup pending-enable hooks may take 300 s. Actor acknowledgements must
    // fail busy while those run rather than wait behind them in the plugin host.
    request["wait_ready"] = json!(false);
    tokio::time::timeout(CALL_TIMEOUT, applier.plugins(request))
        .await
        .map_err(|_| "plugin host timeout".to_string())?
}
async fn privileged(applier: &dyn Applier, request: Request) -> Result<Value, String> {
    tokio::time::timeout(CALL_TIMEOUT, applier.plugin_privd(request))
        .await
        .map_err(|_| "uncertain: plugin privileged acknowledgement timeout".to_string())?
}
/// The reply of an install that went through. A manifest-v1 plugin is kept
/// with its data but cannot run (its host is gone): nothing is pending a
/// restart and nothing will enable it, so the reply says what the plugin list
/// will show instead of promising an enable that cannot happen.
pub(super) async fn installed_reply(
    applier: &dyn Applier,
    name: &str,
    enable: bool,
    load_error: Value,
    log: &str,
) -> Value {
    let entry = bridge(applier, json!({"operation":"manifest","name":name}))
        .await
        .unwrap_or(Value::Null);
    let Some(warning) = entry["migration_warning"].as_str() else {
        return json!({"ok":true,"state":"installed","restart_pending":true,"load_error":load_error,"pending_enable":enable,"log":log});
    };
    let mut reply = json!({"ok":true,"state":"installed","restart_pending":false,"load_error":load_error,"pending_enable":false,"migration_warning":warning,"log":log});
    if enable {
        reply["enable_error"] = json!(warning);
    }
    reply
}
async fn start(applier: &dyn Applier, request: Request) -> Result<Value, String> {
    let journal = bridge(applier, json!({"operation":"job"})).await?;
    if journal["job"].is_string() {
        return Err("busy: plugin job requires observation or operator investigation".into());
    }
    privileged(applier, request).await
}

/// Debian verrevcmp, including arbitrary-length epochs and digit runs.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    fn number(a: &[u8], b: &[u8]) -> Ordering {
        let a = a
            .iter()
            .position(|c| *c != b'0')
            .map_or(&a[a.len()..], |i| &a[i..]);
        let b = b
            .iter()
            .position(|c| *c != b'0')
            .map_or(&b[b.len()..], |i| &b[i..]);
        a.len().cmp(&b.len()).then_with(|| a.cmp(b))
    }
    fn order(c: Option<u8>) -> i32 {
        match c {
            Some(b'~') => -1,
            None | Some(b'0'..=b'9') => 0,
            Some(c) if c.is_ascii_alphabetic() => i32::from(c),
            Some(c) => i32::from(c) + 256,
        }
    }
    fn part(mut a: &[u8], mut b: &[u8]) -> Ordering {
        while !a.is_empty() || !b.is_empty() {
            while a.first().is_some_and(|c| !c.is_ascii_digit())
                || b.first().is_some_and(|c| !c.is_ascii_digit())
            {
                let cmp = order(a.first().copied()).cmp(&order(b.first().copied()));
                if cmp != Ordering::Equal {
                    return cmp;
                }
                a = a.get(1..).unwrap_or_default();
                b = b.get(1..).unwrap_or_default();
            }
            let an = a
                .iter()
                .position(|c| !c.is_ascii_digit())
                .unwrap_or(a.len());
            let bn = b
                .iter()
                .position(|c| !c.is_ascii_digit())
                .unwrap_or(b.len());
            let cmp = number(&a[..an], &b[..bn]);
            if cmp != Ordering::Equal {
                return cmp;
            }
            a = &a[an..];
            b = &b[bn..];
        }
        Ordering::Equal
    }
    let (ae, a) = a.split_once(':').unwrap_or(("0", a));
    let (be, b) = b.split_once(':').unwrap_or(("0", b));
    let (av, ar) = a.rsplit_once('-').unwrap_or((a, "0"));
    let (bv, br) = b.rsplit_once('-').unwrap_or((b, "0"));
    number(ae.as_bytes(), be.as_bytes())
        .then_with(|| part(av.as_bytes(), bv.as_bytes()))
        .then_with(|| part(ar.as_bytes(), br.as_bytes()))
}

async fn deliver(applier: &dyn Applier, state: &StateFile) -> Result<(), String> {
    let reply = crate::follower::deliver(
        &|request| Box::pin(bridge(applier, request)),
        state,
        json!({"plugins":state.document.plugins}),
    )
    .await?;
    if reply["restart_required"] == true {
        return Err("legacy plugin follower requested an unplanned restart".into());
    }
    Ok(())
}

async fn services(
    applier: &dyn Applier,
    name: &str,
    enabled: bool,
    entry: &Value,
    hooks: bool,
) -> Result<Value, String> {
    let mut log = String::new();
    let manifest = &entry["manifest"];
    if !enabled && manifest["process"].is_object() {
        stop_process(applier, name).await?;
    }
    let command = if enabled { "enable" } else { "disable" };
    let requests: Vec<Request> = if hooks
        && manifest["priv_commands"]
            .as_array()
            .is_some_and(|v| v.iter().any(|v| v == command))
    {
        vec![
            serde_json::from_value(json!({"plugin_exec":{"name":name,"command":command}}))
                .map_err(|e| e.to_string())?,
        ]
    } else {
        manifest["services"].as_array().into_iter().flatten().map(|unit| serde_json::from_value(json!({"plugin_service_control":{"unit":unit,"action":if enabled {"start"} else {"stop"}}})).map_err(|e|e.to_string())).collect::<Result<_,_>>()?
    };
    for request in requests {
        let output = if matches!(request, Request::PluginExec { .. }) {
            start(applier, request).await?
        } else {
            privileged(applier, request).await?
        };
        if output["job"].is_string() {
            return Ok(output);
        }
        log.push_str(output["stdout"].as_str().unwrap_or(""));
        log.push_str(output["stderr"].as_str().unwrap_or(""));
    }
    Ok(json!({"stdout":log,"stderr":""}))
}

async fn stop_process(applier: &dyn Applier, name: &str) -> Result<(), String> {
    let entry = bridge(applier, json!({"operation":"manifest","name":name})).await?;
    if entry["manifest"]["process"].is_object() {
        runtime::native().quiesce(name).await;
        privileged(
            applier,
            Request::PluginRuntimeStop {
                name: name.to_owned().try_into()?,
            },
        )
        .await?;
    }
    Ok(())
}

/// Restore only reversible service/loader effects, never rerun package hooks.
pub async fn restore(applier: &dyn Applier, state: &StateFile, plan: &Plan) -> Result<(), String> {
    if !plan
        .actions
        .iter()
        .any(|a| matches!(a, DomainAction::Plugins(_)))
    {
        return Ok(());
    }
    deliver(applier, state).await?;
    for action in plan.actions.iter().rev() {
        if let DomainAction::Plugins(Action::PluginsEnable { name, .. }) = action {
            let entry = bridge(applier, json!({"operation":"manifest","name":name})).await?;
            let enabled = state.document.plugins.get(name).is_some_and(|p| p.enabled);
            services(applier, name, enabled, &entry, false).await?;
            bridge(
                applier,
                json!({"operation":"enabled","name":name,"enabled":enabled}),
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
