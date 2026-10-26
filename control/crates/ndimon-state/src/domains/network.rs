//! The engine owns pending effects; only its Authority commits desired network.
use super::DomainModule;
use crate::{
    BoxFuture,
    apply::Applier,
    persist::StateFile,
    plan::{DomainPlan, Plan},
};
use ndimon_privd_proto::NetworkCall;
use ndimon_render::LegacyFiles;
use ndimon_types::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug)]
pub struct Module;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    NetworkStage,
    NetworkConfirm,
    NetworkRecover,
}
fn internal(e: impl ToString) -> ApiError {
    crate::plan::internal(e)
}
fn follower_error(reason: impl Into<String>) -> ApiError {
    ApiError::ApplyFailed {
        domain: Domain::Network,
        step: "follower".into(),
        reason: reason.into(),
    }
}
async fn deliver(state: &StateFile, applier: &dyn Applier) -> Result<(), ApiError> {
    let response = crate::follower::deliver(
        &|request| applier.decoder(request),
        state,
        json!({"network":state.document.network}),
    )
    .await
    .map_err(follower_error)?;
    if response["restart_required"] == true {
        return Err(follower_error(response.to_string()));
    }
    Ok(())
}
/// The engine refuses every network operation while a legacy store waits to be
/// taken over; nothing else depends on it.
fn legacy_recovery(error: &ApiError) -> bool {
    matches!(error, ApiError::Locked { by: Some(by), .. } if by == "legacy_network_recovery")
}
pub fn impact(preview: &Value) -> Impact {
    if preview["unchanged"] == true {
        return Impact::default();
    }
    let mut impact = Impact::new(ImpactScope::Network);
    if let Some(services) = preview["interruptions"].as_array() {
        for service in services.iter().filter_map(Value::as_str) {
            let typed = match service {
                "tesserad.service" => Some(Service::Tessera),
                "ndimon-finder.service" => Some(Service::Finder),
                "ndimon-r.service" => Some(Service::Decoder),
                _ => None,
            };
            if let Some(service) = typed {
                impact.restarts.push(service);
            }
            impact
                .interruptions
                .push(Interruption::new(Domain::Network, service, Some(90_000)));
            let name = match service {
                "tesserad.service" => "Tessera network audio",
                "ndimon-finder.service" => "NDI source discovery",
                "ndimon-r.service" => "the decoder",
                other => other,
            };
            impact
                .lines
                .push(format!("Restart {name} to use the changed network."));
        }
    }
    impact.lines.push("Apply the network settings. Keep them within 60 seconds after they are active, or the previous settings are restored.".into());
    impact
}
/// What the operator confirms before the engine gives up a failed recovery
/// (NI-17): why it failed, what is live on the port, and what the engine finds
/// foreign there, so the decision is made on the device's own account.
pub fn accept_impact(status: &Value) -> Impact {
    let mut impact = Impact::new(ImpactScope::Network);
    impact.restarts = vec![Service::Tessera, Service::Finder, Service::Decoder];
    for unit in [
        "tesserad.service",
        "ndimon-finder.service",
        "ndimon-r.service",
    ] {
        impact
            .interruptions
            .push(Interruption::new(Domain::Network, unit, Some(10_000)));
    }
    impact.lines.push(
        "Stop retrying the failed recovery and keep the network as it is now. The previous settings are not restored.".into(),
    );
    if let Ok(error) =
        serde_json::from_value::<ApiError>(status["pending"]["recovery_error"].clone())
    {
        impact.lines.push(format!("The recovery failed: {error}."));
    }
    for network in status["networks"].as_array().into_iter().flatten() {
        let addresses = network["addresses"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| Some(format!("{}/{}", a[0].as_str()?, a[1].as_u64()?)))
            .collect::<Vec<_>>();
        impact.lines.push(format!(
            "{} is live with {}.",
            network["interface"].as_str().unwrap_or("A network"),
            if addresses.is_empty() {
                "no IPv4 address".into()
            } else {
                addresses.join(", ")
            }
        ));
    }
    for drift in status["drift"].as_array().into_iter().flatten() {
        impact.lines.push(format!(
            "Foreign configuration found: {}{}.",
            drift["path"].as_str().unwrap_or("unknown"),
            drift["artifact"]
                .as_str()
                .map(|a| format!(" ({a})"))
                .unwrap_or_default()
        ));
    }
    impact.lines.push(
        "What the failed change created is removed, the media services restart, and the network can be changed again.".into(),
    );
    impact
}
impl DomainModule for Module {
    fn guard<'a>(
        &self,
        state: &'a StateFile,
        applier: &'a dyn Applier,
        command: Option<&'a Command>,
    ) -> BoxFuture<'a, Result<(), ApiError>> {
        Box::pin(async move {
            if state.document.network.backend.is_none() {
                return Ok(());
            }
            let status = match applier.network_observation().await {
                Ok(status) => status,
                // A legacy network store the engine would not take over (VV-2
                // N1) locks network changes only; the rest stays configurable.
                Err(error)
                    if legacy_recovery(&error)
                        && !command.is_some_and(|command| Module.imperative(command)) =>
                {
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            if !status["pending"].is_null()
                && !command.is_some_and(|command| Module.imperative(command))
            {
                return Err(ApiError::Locked {
                    field: "network".into(),
                    by: Some("pending_revision".into()),
                });
            }
            Ok(())
        })
    }
    fn imperative(&self, command: &Command) -> bool {
        matches!(
            command,
            Command::StageNetwork { .. }
                | Command::ConfirmNetwork { .. }
                | Command::RevertNetwork { .. }
                | Command::RollbackNetwork
                | Command::AcceptNetworkRecovery
                | Command::PrepareNetworkHandoff
                | Command::CancelNetworkHandoff { .. }
                | Command::ImportNetworkHandoff { .. }
                | Command::CommitNetworkHandoff { .. }
        )
    }
    fn external_confirmation(&self, _command: &Command) -> bool {
        true
    }
    fn operation_observed(
        &self,
        observed: &mut Observed,
        result: &Value,
    ) -> Result<Vec<Event>, ApiError> {
        if let Some(value) = result.get("pending") {
            observed.network_pending = serde_json::from_value(value.clone()).map_err(internal)?;
            if let Some(p) = &observed.network_pending {
                return Ok(vec![Event::NetworkPending {
                    revision: p.revision,
                    expires_at: p.expires_at.clone(),
                    phase: p.phase,
                }]);
            }
        }
        Ok(Vec::new())
    }
    fn reduce(&self, state: &mut DesiredState, command: &Command) -> Result<(), ApiError> {
        if let Command::StageNetwork { profiles } = command {
            // Nothing committed yet (a fresh install, or a device that never kept
            // a change): the port and backend are whatever the engine adopts, and
            // the engine compares the candidate with them in `prepare`. The first
            // Keep commits them; from then on they are fixed.
            let previous = if state.network.backend.is_none() {
                profiles
            } else {
                &state.network
            };
            ndimon_netengine::validation::validate(profiles, previous)?;
        }
        // The engine commits only on Keep through the shared StateAuthority.
        Ok(())
    }
    fn plan(
        &self,
        _previous: &DesiredState,
        _next: &DesiredState,
        command: &Command,
    ) -> Result<DomainPlan, ApiError> {
        let action = match command {
            Command::StageNetwork { .. } => Action::NetworkStage,
            Command::ConfirmNetwork { .. } => Action::NetworkConfirm,
            _ => Action::NetworkRecover,
        };
        Ok(DomainPlan {
            actions: vec![crate::plan::Action::Network(action)],
            impact: Impact::default(),
        })
    }
    fn rollback<'a>(
        &self,
        _applier: &'a crate::apply::ProductionApplier,
        _state: &'a StateFile,
        _plan: &'a Plan,
    ) -> BoxFuture<'a, Result<(), String>> {
        // Failed/rejected lifecycle requests leave recovery to the durable engine.
        Box::pin(async { Ok(()) })
    }
    fn import(&self, state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String> {
        ndimon_render::network::import_json(state, files)
    }
    fn live_checked<'a>(
        &self,
        applier: &'a dyn Applier,
        state: &'a StateFile,
        plan: &'a Plan,
        command: &'a Command,
    ) -> Option<BoxFuture<'a, Result<Value, ApiError>>> {
        Some(Box::pin(async move {
            let mut contribution = Impact::default();
            let mut context = vec![format!(
                "network_revision_context:{}",
                state.domain_revision(Domain::Network)
            )];
            let call = match command {
                Command::StageNetwork { profiles } => {
                    let request = NetworkRequest::new(state.revision, profiles.clone());
                    let preview = applier
                        .network(NetworkCall::Preview {
                            request: request.clone(),
                        })
                        .await?;
                    if preview["writable"] != true && preview["unchanged"] != true {
                        return Err(ApiError::Unsupported {
                            what: "backend_not_certified".into(),
                        });
                    }
                    contribution = impact(&preview);
                    NetworkCall::Stage {
                        request,
                        session: plan.session.map(Into::into),
                    }
                }
                Command::PrepareNetworkHandoff => NetworkCall::HandoffPrepare,
                Command::CancelNetworkHandoff { id } => {
                    NetworkCall::HandoffCancel { id: id.clone() }
                }
                Command::ImportNetworkHandoff { source } => NetworkCall::HandoffImport {
                    source: source.clone(),
                },
                Command::CommitNetworkHandoff { source } => {
                    let review = applier
                        .network(NetworkCall::HandoffImport {
                            source: source.clone(),
                        })
                        .await?;
                    contribution =
                        serde_json::from_value(review["impact"].clone()).map_err(internal)?;
                    context = serde_json::from_value(review["impact_context"].clone())
                        .map_err(internal)?;
                    NetworkCall::HandoffCommit {
                        source: source.clone(),
                        expect_impact: contribution.token.clone(),
                        session: plan.session.map(Into::into),
                    }
                }
                Command::ConfirmNetwork { token } => NetworkCall::Confirm {
                    token: token.clone(),
                    local_address: plan.session.map(|s| s.local),
                },
                Command::RevertNetwork { token } => NetworkCall::Revert {
                    token: token.clone(),
                },
                Command::RollbackNetwork => {
                    contribution = impact(
                        &json!({"interruptions":["ndimon-r.service","ndimon-finder.service","tesserad.service"]}),
                    );
                    NetworkCall::Rollback
                }
                Command::AcceptNetworkRecovery => {
                    // The drift is shown first: what the device found, then the
                    // confirmation seals exactly that.
                    let status = applier.network(NetworkCall::Status).await?;
                    contribution = accept_impact(&status);
                    NetworkCall::AcceptCurrent
                }
                _ => return Err(internal("network command ownership")),
            };
            let sealed = contribution.sealed(command, &context).map_err(internal)?;
            if plan.confirmation.as_ref().is_some_and(|token| {
                token
                    .as_ref()
                    .is_some_and(|token| token != sealed.token.as_str())
            }) || (sealed.needs_confirmation() && plan.confirmation.is_none())
            {
                return Err(ApiError::ConfirmationRequired { impact: sealed });
            }
            let mut reply = applier.network(call).await?;
            if matches!(
                command,
                Command::RevertNetwork { .. }
                    | Command::RollbackNetwork
                    | Command::AcceptNetworkRecovery
                    | Command::CancelNetworkHandoff { .. }
            ) {
                if let Err(error) = deliver(state, applier).await {
                    tracing::warn!(%error, "network recovery follower delivery will retry");
                }
            }
            reply["_impact"] = serde_json::to_value(sealed).map_err(internal)?;
            Ok(reply)
        }))
    }
    fn replay<'a>(
        &self,
        applier: &'a crate::apply::ProductionApplier,
        state: &'a StateFile,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if state.document.network.backend.is_some() {
                let status = match applier.network_observation().await {
                    Ok(status) => status,
                    Err(ApiError::Locked { by: Some(by), .. }) if by == "state_transaction" => {
                        return Ok(());
                    }
                    // Nothing to deliver until the legacy store is taken over; the
                    // domains after this one must still be replayed.
                    Err(error) if legacy_recovery(&error) => return Ok(()),
                    Err(error) => return Err(error.to_string()),
                };
                if status["externally_managed"] == true || status["quiesced"] == true {
                    return Ok(());
                }
                deliver(state, applier)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })
    }
    fn observe(
        &self,
        handle: crate::StateHandle,
        _applier: &crate::apply::ProductionApplier,
    ) -> Option<BoxFuture<'static, ()>> {
        Some(Box::pin(async move {
            let mut timer = tokio::time::interval(std::time::Duration::from_secs(1));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                if handle.query(Query::Observed).await.is_err() {
                    break;
                }
            }
        }))
    }
    fn query<'a>(
        &self,
        state: &'a StateFile,
        applier: &'a dyn Applier,
        query: &'a Query,
    ) -> Option<BoxFuture<'a, Result<Value, ApiError>>> {
        let call = match query {
            Query::NetworkStatus | Query::NetworkProfiles => NetworkCall::Status,
            Query::PreviewNetwork { profiles } => NetworkCall::Preview {
                request: NetworkRequest::new(state.revision, profiles.clone()),
            },
            Query::CheckNetworkAddress { address } => NetworkCall::CheckAddress {
                address: address.clone(),
            },
            Query::NetworkPending { token } => NetworkCall::Pending {
                token: token.clone(),
            },
            _ => return None,
        };
        Some(applier.network(call))
    }
    fn observe_state<'a>(
        &self,
        state: &'a StateFile,
        applier: &'a dyn Applier,
        previous: &'a Observed,
        next: &'a mut Observed,
    ) -> BoxFuture<'a, Result<Vec<Event>, ApiError>> {
        Box::pin(async move {
            let status = match applier.network_observation().await {
                Ok(value) => value,
                Err(error) => {
                    next.network_pending = previous.network_pending.clone();
                    if let Some(domain) = next.domains.get_mut(&Domain::Network) {
                        domain.status = SyncStatus::Failed;
                        domain.detail = Some(error.to_string());
                    }
                    return Ok(Vec::new());
                }
            };
            next.network_pending =
                serde_json::from_value(status["pending"].clone()).map_err(internal)?;
            if next.network_pending.is_none()
                && state.document.network.backend.is_some()
                && status["externally_managed"] != true
                && status["quiesced"] != true
            {
                if let Err(error) = deliver(state, applier).await {
                    if let Some(domain) = next.domains.get_mut(&Domain::Network) {
                        domain.status = SyncStatus::Failed;
                        domain.detail = Some(error.to_string());
                    }
                }
            }
            let mut events = Vec::new();
            let before = previous.network_pending.as_ref();
            let pending = next.network_pending.as_ref();
            if before.map(|p| (&p.job_id, p.phase)) != pending.map(|p| (&p.job_id, p.phase)) {
                if let Some(pending) = pending {
                    events.push(Event::NetworkPending {
                        revision: pending.revision,
                        expires_at: pending.expires_at.clone(),
                        phase: pending.phase,
                    });
                } else if let Some(before) = before {
                    if let Ok(phase) = serde_json::from_value(status["last_phase"].clone()) {
                        events.push(Event::NetworkPending {
                            revision: before.revision,
                            expires_at: before.expires_at.clone(),
                            phase,
                        });
                    }
                }
            }
            let drift: Vec<Drift> =
                serde_json::from_value(status["drift"].clone()).map_err(internal)?;
            for drift in &drift {
                if !previous.drift.contains(drift) {
                    events.push(Event::Drift {
                        domain: drift.domain,
                        path: drift.path.clone(),
                        artifact: drift.artifact.clone(),
                        foreign: drift.foreign.clone(),
                        selector: drift.selector.clone(),
                        interface: drift.interface.clone(),
                        neutralized: drift.neutralized,
                        blocking: drift.blocking,
                    });
                }
            }
            next.drift.extend(drift);
            Ok(events)
        })
    }
}

#[cfg(test)]
mod tests;
