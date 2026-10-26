//! Observe root jobs outside the state actor; serialize only their decisions.
use super::*;
use crate::{StateHandle, domains::DomainModule};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingJob {
    pub id: String,
    pub action: Action,
    pub previous: Value,
    pub started_at: u64,
    #[serde(default)]
    pub investigation: bool,
    /// An operator cancelled the job. Whatever it left is then settled as
    /// cancelled, even if its unit is already gone and can never report.
    #[serde(default)]
    pub cancelled: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobOutcome {
    Success(Value),
    Failure(String),
    Unknown(String),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobResult {
    pub job: String,
    pub response: Result<Value, String>,
    pub investigation: bool,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(super) fn prepare(
    previous: &StateFile,
    next: &mut StateFile,
    command: &Command,
) -> Result<(), ApiError> {
    if matches!(command, Command::ResetPluginState) {
        next.legacy_files.remove("/etc/ndimon-plugins.json");
    }
    if !matches!(
        command,
        Command::InstallPlugin { .. }
            | Command::UpdatePlugin { .. }
            | Command::RemovePlugin { .. }
            | Command::SetPluginEnabled { .. }
    ) {
        return Ok(());
    }
    let plan = Module.plan(&previous.document, &next.document, command)?;
    let Some(DomainAction::Plugins(action)) = plan.actions.into_iter().next() else {
        return Err(internal("missing plugin job action"));
    };
    next.plugin_job = Some(PendingJob {
        id: "unknown".into(),
        action,
        previous: serde_json::to_value(&previous.document.plugins).map_err(internal)?,
        started_at: now(),
        investigation: false,
        cancelled: false,
    });
    Ok(())
}
pub(super) fn accepted(
    previous: &StateFile,
    next: &mut StateFile,
    _command: &Command,
    apply: &mut Value,
) -> Result<bool, ApiError> {
    if let Command::CancelPluginJob { job } = _command {
        // The cancel is kept with the job; the supervisor settles it, whether
        // its unit still reports or is already gone.
        next.plugin_job = previous.plugin_job.clone().map(|mut pending| {
            if pending.id == *job {
                pending.cancelled = true;
            }
            pending
        });
        return Ok(next.plugin_job != previous.plugin_job);
    }
    let Some(job) = apply.as_object_mut().and_then(|v| v.remove("_plugin_job")) else {
        if let Some(pending) = next.plugin_job.clone() {
            record_runtime(next, &pending.action)?;
        }
        if matches!(_command, Command::ResetPluginState) {
            next.plugin_runtime.clear();
            next.plugin_runtime
                .insert("native_host".into(), json!(true));
        }
        let prepared = next.plugin_job != previous.plugin_job
            || next.plugin_runtime != previous.plugin_runtime;
        next.plugin_job = previous.plugin_job.clone().filter(|p| p.id != "unknown");
        return Ok(prepared);
    };
    let id = job
        .as_str()
        .ok_or_else(|| internal("missing plugin job id"))?
        .to_string();
    ndimon_privd_proto::PluginJob::try_from(id.clone()).map_err(internal)?;
    next.plugin_job
        .as_mut()
        .ok_or_else(|| internal("missing prepared plugin job"))?
        .id = id;
    Ok(true)
}
pub(super) fn completion(
    state: &StateFile,
    result: &Value,
) -> Result<Option<(StateFile, Plan)>, ApiError> {
    let Some(pending) = &state.plugin_job else {
        return Ok(None);
    };
    // An investigation marker is left alone unless the settlement says it is
    // the operator's cancel or a restart looking at it again.
    let settling = result["settle"] == true;
    if result["job"].as_str() != Some(pending.id.as_str()) || (pending.investigation && !settling) {
        return Ok(None);
    }
    let outcome: JobOutcome =
        serde_json::from_value(result["outcome"].clone()).map_err(internal)?;
    let mut next = state.clone();
    if matches!(outcome, JobOutcome::Failure(_)) {
        // Restore this domain only; unrelated commands may have committed meanwhile.
        next.document.plugins =
            serde_json::from_value(pending.previous.clone()).map_err(internal)?;
    }
    if matches!(outcome, JobOutcome::Success(_)) {
        if let Action::PluginsInstall { name, .. } = &pending.action {
            if let Ok(manifest) = host::native().manifest(name) {
                if let Some(schema) = manifest.state {
                    if let Some(plugin) = next.document.plugins.get_mut(name) {
                        if plugin.state.is_none() {
                            plugin.state =
                                serde_json::from_value(schema.default).map_err(invalid)?;
                        }
                    }
                }
            }
        }
    }
    let plan = Plan {
        paths: if next.document.plugins == state.document.plugins {
            vec![]
        } else {
            vec!["plugins".into()]
        },
        domains: vec![Domain::Plugins],
        actions: vec![DomainAction::Plugins(Action::PluginsComplete {
            pending: Box::new(pending.clone()),
            outcome,
        })],
        impact: Impact::default(),
        confirmation: None,
        external_confirmation: false,
        session: None,
    };
    Ok(Some((next, plan)))
}
pub(super) fn completed(state: &mut StateFile, apply: &Value) -> Result<(), ApiError> {
    let result: JobResult =
        serde_json::from_value(apply["_plugin_result"].clone()).map_err(internal)?;
    if result.response.is_ok() {
        if let Some(pending) = state.plugin_job.clone() {
            record_runtime(state, &pending.action)?;
        }
    }

    if result.investigation {
        if let Some(pending) = &mut state.plugin_job {
            pending.investigation = true;
        }
    } else {
        state.plugin_job = None;
    }
    let id = result.job.clone();
    state.plugin_results.insert(id.clone(), result);
    while state.plugin_results.len() > 32 {
        let oldest = state
            .plugin_results
            .keys()
            .find(|key| **key != id)
            .cloned()
            .unwrap();
        state.plugin_results.remove(&oldest);
    }
    Ok(())
}
pub(super) async fn finish(
    applier: &dyn Applier,
    state: &StateFile,
    pending: &PendingJob,
    outcome: &JobOutcome,
) -> Result<Value, String> {
    let response = match outcome {
        JobOutcome::Success(output) => {
            let log = format!(
                "{}{}",
                output["stdout"].as_str().unwrap_or(""),
                output["stderr"].as_str().unwrap_or("")
            );
            match &pending.action {
                Action::PluginsInstall {
                    name,
                    version,
                    enable,
                    archive,
                    ..
                } => {
                    let feedback = bridge(applier, json!({"operation":"installed","name":name,"version":version,"pending_enable":enable,"archive":archive}))
                        .await
                        .unwrap_or_else(|error| json!({"load_error":error}));
                    Ok(installed_reply(
                        applier,
                        name,
                        *enable,
                        feedback.get("load_error").cloned().unwrap_or(Value::Null),
                        &log,
                    )
                    .await)
                }
                Action::PluginsEnable { name, enabled } => {
                    deliver(applier, state).await?;
                    bridge(
                        applier,
                        json!({"operation":"enabled","name":name,"enabled":enabled}),
                    )
                    .await?;
                    Ok(
                        json!({"ok":true,"state":if *enabled {"enabled"} else {"installed"},"log":log}),
                    )
                }
                Action::PluginsRemove { name, .. } => {
                    let _ = bridge(applier, json!({"operation":"removed","name":name})).await;
                    Ok(json!({"ok":true,"state":"removed","log":log}))
                }
                _ => return Err("invalid plugin completion action".into()),
            }
        }
        JobOutcome::Failure(error) => {
            // privd restores the previous package on failed install/update. Do
            // not rerun package or privilege hooks from a rollback transaction.
            let mut error = error.clone();
            if let Err(rollback) = deliver(applier, state).await {
                error.push_str(&format!("; loader rollback failed: {rollback}"));
            }
            if let Action::PluginsEnable { name, .. } = &pending.action {
                let enabled = state.document.plugins.get(name).is_some_and(|p| p.enabled);
                let rollback = async {
                    let entry =
                        bridge(applier, json!({"operation":"manifest","name":name})).await?;
                    services(applier, name, enabled, &entry, false).await?;
                    bridge(
                        applier,
                        json!({"operation":"enabled","name":name,"enabled":enabled}),
                    )
                    .await
                }
                .await;
                if let Err(rollback) = rollback {
                    error.push_str(&format!("; service rollback failed: {rollback}"));
                }
            }
            Err(error)
        }
        JobOutcome::Unknown(error) => Err(error.clone()),
    };
    let investigation = matches!(outcome, JobOutcome::Unknown(_));
    if investigation {
        bridge(
            applier,
            json!({"operation":"job","value":pending.id,"error":response.as_ref().err()}),
        )
        .await?;
    }
    Ok(
        json!({"_domain_failure":response.as_ref().err(),"_plugin_result":JobResult { job: pending.id.clone(), response, investigation }}),
    )
}
fn outcome(status: &Value) -> Option<JobOutcome> {
    if let Some(result) = status.get("data").and_then(|d| d.get("result")) {
        return Some(
            match serde_json::from_value::<ndimon_privd_proto::ReplyResult>(result.clone()) {
                Ok(ndimon_privd_proto::ReplyResult::Ok(output)) => {
                    match serde_json::to_value(output) {
                        Ok(value) => JobOutcome::Success(value),
                        Err(e) => JobOutcome::Unknown(e.to_string()),
                    }
                }
                Ok(ndimon_privd_proto::ReplyResult::Err(error)) => {
                    JobOutcome::Failure(error.to_string())
                }
                Err(error) => JobOutcome::Unknown(format!(
                    "invalid plugin result: {error}; operator investigation required"
                )),
            },
        );
    }
    (status["stdout"].as_str().unwrap_or("").trim() == "inactive").then(|| {
        JobOutcome::Unknown(
            "plugin job ended without a durable result; operator investigation required".into(),
        )
    })
}
/// The unit is gone and no result was found: the shape of a job that vanished.
fn ended_without_result(status: &Value) -> bool {
    status.get("data").and_then(|d| d.get("result")).is_none()
        && status["stdout"].as_str().unwrap_or("").trim() == "inactive"
}
/// Polls in a row that must find a job gone without a result before it is
/// called lost. privd closes the race that made this likely (VV-3 W1); the
/// repeat keeps a late rename or a slow disk from parking a job for good.
const LOST_POLLS: u32 = 3;
const CANCELLED: &str = "plugin job cancelled by an operator";
const ABANDONED: &str = "plugin job ended without a durable result and its unit is gone; the plugin registry is back as it was before the job";
/// Retire an investigation marker whose job can be asked about again: after
/// an operator's cancel, and once when the control plane starts. A result that
/// is there after all completes the job as it would have; a unit that is gone
/// without one ends the job as failed, with the registry as it was before it.
/// Returns false when privd could not be asked.
async fn settle(handle: &StateHandle, applier: &dyn Applier, pending: &PendingJob) -> bool {
    let reason = if pending.cancelled {
        CANCELLED
    } else {
        ABANDONED
    };
    let settled = match ndimon_privd_proto::PluginJob::try_from(pending.id.clone()) {
        // No acknowledgement ever named a unit: there is nothing to ask.
        Err(_) => JobOutcome::Failure(reason.into()),
        Ok(job) => {
            let Ok(status) = privileged(applier, Request::PluginJobStatus { job }).await else {
                return false;
            };
            if ended_without_result(&status) {
                JobOutcome::Failure(reason.into())
            } else {
                match outcome(&status) {
                    Some(JobOutcome::Unknown(error)) => JobOutcome::Failure(error),
                    Some(done) => done,
                    // Still running: the cancel is on its way.
                    None => return true,
                }
            }
        }
    };
    let message = json!({"job":pending.id,"outcome":settled,"settle":true});
    match handle.complete_domain(Domain::Plugins, message).await {
        Ok(()) => {
            // As after any completion: only now is the receipt of the job cleared.
            let _ = bridge(applier, json!({"operation":"job","value":null})).await;
        }
        Err(error) => tracing::warn!(%error, "plugin job settlement pending"),
    }
    true
}
/// Start once with the control runtime; abort on shutdown. Restarting resumes
/// the persisted job, including terminal receipts retained by privd.
pub fn spawn_jobs(handle: StateHandle) -> tokio::task::JoinHandle<()> {
    let applier = handle.applier.clone();
    spawn_with(handle, applier, Duration::from_millis(250))
}
fn spawn_with(
    handle: StateHandle,
    applier: Arc<dyn Applier>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(observe_with(handle, applier, interval))
}
pub(super) fn observe(handle: StateHandle) -> BoxFuture<'static, ()> {
    let applier = handle.applier.clone();
    observe_with(handle, applier, Duration::from_millis(250))
}
fn observe_with(
    handle: StateHandle,
    applier: Arc<dyn Applier>,
    interval: Duration,
) -> BoxFuture<'static, ()> {
    Box::pin(async move {
        let mut orphan_reported = None;
        let mut cancelled = None;
        // Consecutive polls that found the job gone without a result.
        let mut lost = 0;
        // True for the first look after the control plane started.
        let mut start = true;
        loop {
            let state = match handle.snapshot().await {
                Ok(state) => state,
                Err(_) => break,
            };
            let first = std::mem::take(&mut start);
            if !state.plugin_job.as_ref().is_some_and(|p| !p.investigation) {
                lost = 0;
            }
            if let Some(pending) = state
                .plugin_job
                .as_ref()
                .filter(|p| p.investigation && (first || p.cancelled))
            {
                // A restart and the documented cancel are the ways out of an
                // investigation marker; both ask the job's unit once more.
                if !settle(&handle, applier.as_ref(), pending).await {
                    start = first;
                }
                tokio::time::sleep(interval).await;
                continue;
            }
            if let Some(pending) = state.plugin_job.as_ref().filter(|p| !p.investigation) {
                let job = match ndimon_privd_proto::PluginJob::try_from(pending.id.clone()) {
                    Ok(job) => job,
                    Err(_) => {
                        let _ = handle.complete_domain(Domain::Plugins,json!({"job":pending.id,"outcome":JobOutcome::Unknown("plugin acknowledgement was lost; operator investigation required".into())})).await;
                        tokio::time::sleep(interval).await;
                        continue;
                    }
                };
                if let Ok(status) = privileged(
                    applier.as_ref(),
                    Request::PluginJobStatus { job: job.clone() },
                )
                .await
                {
                    let mut result = outcome(&status);
                    if !ended_without_result(&status) {
                        lost = 0;
                    } else if pending.cancelled {
                        // The unit is gone and cannot report: it was stopped.
                        result = Some(JobOutcome::Failure(CANCELLED.into()));
                    } else {
                        lost += 1;
                        if lost < LOST_POLLS {
                            result = None;
                        }
                    }
                    if result.is_none()
                        && now().saturating_sub(pending.started_at) >= 300
                        && cancelled.as_deref() != Some(pending.id.as_str())
                    {
                        // Cancellation itself never waits for systemd to stop a unit.
                        if privileged(applier.as_ref(), Request::PluginJobCancel { job })
                            .await
                            .is_ok()
                        {
                            cancelled = Some(pending.id.clone());
                        }
                    }
                    if let Some(outcome) = result.take() {
                        let message = json!({"job":pending.id,"outcome":outcome});
                        if let Err(error) = handle.complete_domain(Domain::Plugins, message).await {
                            // A loader failure after a successful enable must restore
                            // the prior accepted intent through the actor as well.
                            if matches!(pending.action, Action::PluginsEnable { .. })
                                && matches!(outcome, JobOutcome::Success(_))
                                && matches!(&error, ApiError::ApplyFailed { step, .. } if step == "completion")
                            {
                                let _ = handle.complete_domain(Domain::Plugins, json!({"job":pending.id,"outcome":JobOutcome::Failure(error.to_string())})).await;
                            }
                            tracing::warn!(%error, "plugin completion pending");
                        } else if !matches!(outcome, JobOutcome::Unknown(_)) {
                            // Clear only after the completion is durable; a failure
                            // here is retried by the next observation.
                            let _ =
                                bridge(applier.as_ref(), json!({"operation":"job","value":null}))
                                    .await;
                        }
                    }
                }
            } else if state.plugin_job.is_none() {
                // An interrupted acceptance can leave only the legacy journal.
                // Without accepted metadata we cannot guess a package outcome.
                if let Ok(journal) = bridge(applier.as_ref(), json!({"operation":"job"})).await {
                    if let Some(job) = journal["job"].as_str() {
                        if state
                            .plugin_results
                            .get(job)
                            .is_some_and(|result| !result.investigation)
                        {
                            let _ =
                                bridge(applier.as_ref(), json!({"operation":"job","value":null}))
                                    .await;
                        } else if orphan_reported.as_deref() != Some(job)
                            && handle
                                .snapshot()
                                .await
                                .is_ok_and(|s| s.plugin_job.is_none())
                        {
                            let _ = bridge(applier.as_ref(), json!({"operation":"job","value":job,"error":"plugin acknowledgement or acceptance was lost; operator investigation required"})).await;
                            orphan_reported = Some(job.to_string());
                        }
                    } else {
                        orphan_reported = None;
                    }
                }
            }
            tokio::time::sleep(interval).await;
        }
    })
}

#[cfg(test)]
mod tests;
/// The compatibility request waits here, never in the actor. Retained results
/// avoid a lost notification if completion precedes subscription or reconnect.
pub async fn wait_result(handle: &StateHandle, job: &str) -> Result<Value, ApiError> {
    tokio::time::timeout(Duration::from_secs(310), async {
        loop {
            let state = handle.snapshot().await?;
            if let Some(result) = state.plugin_results.get(job) {
                return result
                    .response
                    .clone()
                    .map_err(|reason| ApiError::ApplyFailed {
                        domain: Domain::Plugins,
                        step: "apply".into(),
                        reason,
                    });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| ApiError::ApplyFailed {
        domain: Domain::Plugins,
        step: "apply".into(),
        reason: "plugin job timed out; observation continues".into(),
    })?
}
/// Archive inspection is read-only and may wait, but only in a query caller.
pub(super) async fn inspect(applier: &dyn Applier, request: Request) -> Result<Value, String> {
    let output = privileged(applier, request).await?;
    let Some(job) = output["job"].as_str() else {
        return Ok(output);
    };
    wait_job(applier, job).await
}
pub(super) async fn wait_job(applier: &dyn Applier, job: &str) -> Result<Value, String> {
    let job =
        ndimon_privd_proto::PluginJob::try_from(job.to_string()).map_err(|e| e.to_string())?;
    tokio::time::timeout(Duration::from_secs(300), async {
        let mut lost = 0;
        loop {
            let status = privileged(applier, Request::PluginJobStatus { job: job.clone() }).await?;
            if ended_without_result(&status) {
                lost += 1;
                if lost < LOST_POLLS {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
            } else {
                lost = 0;
            }
            if let Some(result) = outcome(&status) {
                return match result {
                    JobOutcome::Success(output) => Ok(output),
                    JobOutcome::Failure(error) | JobOutcome::Unknown(error) => Err(error),
                };
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .map_err(|_| "plugin inspection timed out".to_string())?
}

fn record_runtime(state: &mut StateFile, action: &Action) -> Result<(), ApiError> {
    let mut runtime = json!(state.plugin_runtime);
    if !runtime["plugins"].is_object() {
        runtime["plugins"] = json!({});
    }
    match action {
        Action::PluginsInstall {
            name,
            version,
            enable,
            ..
        } => {
            runtime["plugins"][name] =
                json!({"version":version,"enabled":false,"pending_enable":enable});
        }
        Action::PluginsEnable { name, enabled } => {
            runtime["plugins"][name] = json!({"version":state.document.plugins.get(name).map(|p|&p.version),"enabled":enabled,"pending_enable":false});
        }
        Action::PluginsRemove { name, .. } => {
            runtime["plugins"].as_object_mut().unwrap().remove(name);
        }
        _ => (),
    }
    state.plugin_runtime = runtime
        .as_object()
        .cloned()
        .ok_or_else(|| invalid("invalid plugin runtime"))?;
    Ok(())
}
