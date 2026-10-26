//! Native NDI intent and finder observation; catalogs are never desired state.
use super::outputs::follower;
use crate::{
    BoxFuture,
    apply::{Applier, ProductionApplier},
    persist::StateFile,
    plan::{Action as DomainAction, DomainPlan, Plan, internal},
};
use ndimon_privd_proto::ManagedUnit;
use ndimon_render::LegacyFiles;
use ndimon_types::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    NdiDeliver,
    NdiRestartDecoder,
    NdiRefresh,
    NdiReset,
}
#[derive(Debug)]
pub struct Module;
impl super::DomainModule for Module {
    fn imperative(&self, command: &Command) -> bool {
        matches!(command, Command::RefreshNdi | Command::ResetNdiDiscovery)
    }
    fn reduce(&self, state: &mut DesiredState, command: &Command) -> Result<(), ApiError> {
        match command {
            Command::SetNdi(ndi) => state.ndi = ndi.clone(),
            Command::RefreshNdi | Command::ResetNdiDiscovery => (),
            _ => {
                return Err(ApiError::Unsupported {
                    what: "unsupported NDI command".into(),
                });
            }
        }
        Ok(())
    }
    fn import(&self, state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String> {
        ndimon_render::ndi::import_json(state, files)
    }
    fn plan(
        &self,
        previous: &DesiredState,
        next: &DesiredState,
        command: &Command,
    ) -> Result<DomainPlan, ApiError> {
        let mut actions = Vec::new();
        let mut impact = Impact::default();
        if previous.ndi != next.ndi {
            actions.push(Action::NdiDeliver);
        }
        // The SDK reads transport and Discovery Server settings only when the
        // decoder starts: a receiver it routes by name is resolved through the
        // server it started with, so both changes restart the decoder.
        let transport = previous.ndi.receive != next.ndi.receive;
        let discovery = previous.ndi.discovery_servers != next.ndi.discovery_servers;
        if transport || discovery {
            actions.push(Action::NdiRestartDecoder);
            impact.scope = ImpactScope::Service;
            impact.restarts.push(Service::Decoder);
            impact.interruptions.push(Interruption::new(
                Domain::Ndi,
                "decoder video/audio",
                Some(3000),
            ));
        }
        if transport {
            impact.lines.push("Restart the decoder to change NDI receive transport; video and audio pause briefly".into());
        }
        if discovery {
            impact.lines.push(
                "Restart the decoder to change the Discovery Server; video and audio pause briefly"
                    .into(),
            );
        }
        if matches!(command, Command::RefreshNdi) {
            // A rescan the operator asked for interrupts only source discovery
            // (about 2 s, never media), and the page rescans after every
            // discovery save: it applies without a confirmation round. The
            // finder restart stays declared so privd may perform exactly it.
            actions.push(Action::NdiRefresh);
            impact.restarts.push(Service::Finder);
            impact.interruptions.push(Interruption::new(
                Domain::Ndi,
                "source discovery",
                Some(2000),
            ));
            impact
                .lines
                .push("Restart the finder to refresh NDI source discovery".into());
        }
        if matches!(command, Command::ResetNdiDiscovery) {
            actions.push(Action::NdiReset);
        }
        Ok(DomainPlan {
            actions: actions.into_iter().map(DomainAction::Ndi).collect(),
            impact,
        })
    }
    fn apply<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            follower::finder(applier, state).await?;
            follower::decoder(applier, state, &plan.impact.restarts).await?;
            for action in &plan.actions {
                match action {
                    DomainAction::Ndi(Action::NdiRestartDecoder) => {
                        follower::restart(applier, ManagedUnit::Decoder).await?
                    }
                    DomainAction::Ndi(Action::NdiRefresh) => {
                        follower::restart(applier, ManagedUnit::Finder).await?
                    }
                    DomainAction::Ndi(Action::NdiReset) => {
                        tokio::fs::write("/run/ndimon/ndi_src_find_param", "-rs")
                            .await
                            .map_err(|e| e.to_string())?
                    }
                    _ => (),
                }
            }
            Ok(json!({"ok":true}))
        })
    }
    fn rollback<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            follower::finder(applier, state).await?;
            follower::decoder(applier, state, &plan.impact.restarts).await?;
            if plan.impact.restarts.contains(&Service::Decoder) {
                follower::restart(applier, ManagedUnit::Decoder).await?;
            }
            Ok(())
        })
    }
    fn replay<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { follower::finder(applier, state).await })
    }
    fn query<'a>(
        &self,
        _state: &'a StateFile,
        applier: &'a dyn Applier,
        query: &'a Query,
    ) -> Option<BoxFuture<'a, Result<Value, ApiError>>> {
        if !matches!(
            query,
            Query::Catalog {
                name: Catalog::NdiSources,
                ..
            }
        ) {
            return None;
        }
        Some(Box::pin(async move {
            applier.ndi_sources().await.map_err(internal)
        }))
    }
}
