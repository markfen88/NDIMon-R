//! Domain registration and the sole command-to-domain routing table.
pub mod audio;
pub mod device;
pub mod display;
pub mod ndi;
pub mod network;
pub mod outputs;
pub mod plugins;
pub mod presets;
pub mod tessera;

use crate::{
    BoxFuture,
    apply::{Applier, ProductionApplier},
    persist::StateFile,
    plan::{DomainPlan, Plan},
};
use ndimon_render::{LegacyFiles, Ownership};
use ndimon_types::*;
use serde_json::{Value, json};

pub trait DomainModule: std::fmt::Debug + Sync {
    /// Admission runs before the shared durable-decision lock is acquired.
    fn guard<'a>(
        &self,
        _state: &'a StateFile,
        _applier: &'a dyn Applier,
        _command: Option<&'a Command>,
    ) -> BoxFuture<'a, Result<(), ApiError>> {
        Box::pin(async { Ok(()) })
    }
    fn observe_state<'a>(
        &self,
        _state: &'a StateFile,
        _applier: &'a dyn Applier,
        _previous: &'a Observed,
        _next: &'a mut Observed,
    ) -> BoxFuture<'a, Result<Vec<Event>, ApiError>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn operation_observed(
        &self,
        _observed: &mut Observed,
        _result: &Value,
    ) -> Result<Vec<Event>, ApiError> {
        Ok(Vec::new())
    }
    fn prepare(
        &self,
        _previous: &StateFile,
        _next: &mut StateFile,
        _command: &Command,
    ) -> Result<(), ApiError> {
        Ok(())
    }
    /// Optional metadata-only launch journal, durable before external effects.
    fn reservation(&self, _previous: &StateFile, _next: &StateFile) -> Option<StateFile> {
        None
    }
    fn release_reservation(&self, _state: &mut StateFile, _reason: &str) {}
    /// Retain operation receipts in the same envelope as accepted intent.
    /// Return whether the staged envelope needs updating before commit.
    fn accepted(
        &self,
        _previous: &StateFile,
        _next: &mut StateFile,
        _command: &Command,
        _apply: &mut Value,
    ) -> Result<bool, ApiError> {
        Ok(false)
    }
    /// Only an internal supervisor can submit an asynchronous completion.
    fn completion(
        &self,
        _state: &StateFile,
        _result: &Value,
    ) -> Result<Option<(StateFile, Plan)>, ApiError> {
        Ok(None)
    }
    fn completed(&self, _state: &mut StateFile, _apply: &Value) -> Result<(), ApiError> {
        Ok(())
    }
    fn query_outside_actor(&self, _query: &Query) -> bool {
        false
    }
    fn project_observed(&self, _state: &StateFile, _observed: &mut DomainObserved) {}
    /// Runtime-only operations execute without allocating a desired revision.
    fn imperative(&self, _command: &Command) -> bool {
        false
    }
    fn replay<'a>(
        &self,
        _applier: &'a ProductionApplier,
        _state: &'a StateFile,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn observe(
        &self,
        _handle: crate::StateHandle,
        _applier: &ProductionApplier,
    ) -> Option<BoxFuture<'static, ()>> {
        None
    }
    fn external_confirmation(&self, _command: &Command) -> bool {
        false
    }
    /// Optional, idempotent in-memory migration for older persisted envelopes.
    fn migrate(&self, _state: &mut DesiredState, _files: &LegacyFiles) -> Result<(), String> {
        Ok(())
    }
    fn reduce(&self, _state: &mut DesiredState, _command: &Command) -> Result<(), ApiError> {
        Err(followed_error())
    }
    fn plan(
        &self,
        _previous: &DesiredState,
        _next: &DesiredState,
        _command: &Command,
    ) -> Result<DomainPlan, ApiError> {
        Ok(DomainPlan::default())
    }
    fn import(&self, state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String>;
    fn validate_assets(
        &self,
        _state: &DesiredState,
        _assets: &crate::assets::AssetStore,
    ) -> Result<(), String> {
        Ok(())
    }
    fn preflight<'a>(
        &self,
        _applier: &'a ProductionApplier,
        _previous: &'a StateFile,
        _next: &'a StateFile,
        _plan: &'a Plan,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn apply<'a>(
        &self,
        _applier: &'a ProductionApplier,
        _state: &'a StateFile,
        _plan: &'a Plan,
    ) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async { Ok(json!({"ok":true})) })
    }
    fn apply_checked<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<Value, ApiError>> {
        let effect = self.apply(applier, state, plan);
        Box::pin(async move {
            effect.await.map_err(|reason| ApiError::ApplyFailed {
                domain: plan.domains.first().copied().unwrap_or(Domain::Audio),
                step: "apply".into(),
                reason,
            })
        })
    }
    fn rollback<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<(), String>> {
        let apply = self.apply(applier, state, plan);
        Box::pin(async move { apply.await.map(|_| ()) })
    }
    /// Imperative commands with no desired-state mutation run inside the actor.
    fn live<'a>(
        &self,
        _applier: &'a dyn Applier,
        _command: &'a Command,
    ) -> Option<BoxFuture<'a, Result<Value, String>>> {
        None
    }
    fn live_checked<'a>(
        &self,
        applier: &'a dyn Applier,
        _state: &'a StateFile,
        _plan: &'a Plan,
        command: &'a Command,
    ) -> Option<BoxFuture<'a, Result<Value, ApiError>>> {
        self.live(applier, command).map(|effect| {
            Box::pin(async move {
                effect.await.map_err(|reason| ApiError::ApplyFailed {
                    domain: command_domain(command).unwrap_or(Domain::Audio),
                    step: "apply".into(),
                    reason,
                })
            }) as BoxFuture<'a, Result<Value, ApiError>>
        })
    }
    fn query<'a>(
        &self,
        _state: &'a StateFile,
        _applier: &'a dyn Applier,
        _query: &'a Query,
    ) -> Option<BoxFuture<'a, Result<Value, ApiError>>> {
        None
    }
}
#[derive(Debug)]
pub struct Registration {
    pub domain: Domain,
    pub name: &'static str,
    pub module: &'static dyn DomainModule,
    pub copy: fn(&mut DesiredState, &DesiredState),
}
impl Registration {
    /// Default ownership comes from the same policy used by privd's write gate.
    pub fn ownership(&self) -> Ownership {
        ndimon_render::ownership(self.domain)
    }
}
macro_rules! domain {
    ($domain:ident, $field:ident, $module:expr) => {
        Registration {
            domain: Domain::$domain,
            name: stringify!($field),
            module: $module,
            copy: |next, imported| next.$field = imported.$field.clone(),
        }
    };
}
pub const REGISTRY: &[Registration] = &[
    domain!(Network, network, &network::Module),
    // Tessera must reload before audio consumers on apply and rollback.
    domain!(Tessera, tessera, &tessera::Module),
    domain!(Audio, audio, &audio::Module),
    domain!(Device, device, &device::Module),
    domain!(Outputs, outputs, &outputs::Module),
    domain!(Ndi, ndi, &ndi::Module),
    domain!(Display, display, &display::Module),
    domain!(Presets, presets, &presets::Module),
    domain!(Plugins, plugins, &plugins::Module),
];
pub fn get(domain: Domain) -> Result<&'static Registration, ApiError> {
    REGISTRY
        .iter()
        .find(|r| r.domain == domain)
        .ok_or_else(followed_error)
}
fn followed_error() -> ApiError {
    ApiError::Unsupported {
        what: "domain is followed; use its legacy writer until ownership moves".into(),
    }
}
pub fn reduce(state: &mut DesiredState, command: &Command) -> Result<(), ApiError> {
    if crate::backup::handles(command) {
        return crate::backup::reduce(state, command);
    }
    let registration = get(command_domain(command).ok_or_else(followed_error)?)?;
    if registration.ownership() != Ownership::Native {
        return Err(followed_error());
    }
    registration.module.reduce(state, command)
}
pub fn operation_query(
    state: &StateFile,
    root: &std::path::Path,
    query: &Query,
) -> Option<Result<Value, ApiError>> {
    crate::backup::query(state, root, query)
}
pub fn operation_context(previous: &DesiredState, command: &Command) -> Vec<String> {
    crate::backup::context(previous, command)
}
pub fn operation_impact(command: &Command) -> Impact {
    crate::backup::impact(command)
}
/// Restore uses the existing planners/hooks in the legacy dependency order.
pub fn planning_order(command: &Command) -> Vec<&'static Registration> {
    let mut order: Vec<_> = REGISTRY.iter().collect();
    if crate::backup::handles(command) {
        order.sort_by_key(|r| match r.domain {
            Domain::Audio => 0,
            Domain::Outputs => 1,
            Domain::Ndi => 2,
            Domain::Display => 3,
            Domain::Device => 4,
            Domain::Presets => 5,
            Domain::Tessera => 6,
            Domain::Plugins => 7,
            Domain::Network => 8,
            _ => 9,
        });
    }
    order
}
type CommandRoute = (Domain, fn(&Command) -> bool);
pub const COMMAND_ROUTES: &[CommandRoute] = &[
    (Domain::Audio, |command| {
        matches!(
            command,
            Command::PutAudio(_)
                | Command::SetAudioEngine { .. }
                | Command::PutAudioSource { .. }
                | Command::PutAudioSink { .. }
                | Command::RemoveAudioEndpoints { .. }
                | Command::PutOutputAudio { .. }
                | Command::ImportBackup {
                    scope: RestoreScope::Audio,
                    ..
                }
        )
    }),
    (Domain::Device, |command| {
        matches!(
            command,
            Command::SetDevice(_)
                | Command::Reboot
                | Command::RestartStack
                | Command::InstallUpdate { .. }
                | Command::ServiceControl { .. }
        )
    }),
    (Domain::Outputs, |command| {
        matches!(
            command,
            Command::SelectSource { .. }
                | Command::ForgetSource { .. }
                | Command::AdoptRemoteSource { .. }
                | Command::ReconnectSource { .. }
                | Command::RetrySource { .. }
                | Command::PutOutputSettings { .. }
                | Command::SetOutputAlias { .. }
                | Command::SetOutputVideo { .. }
                | Command::SetScreenSaver { .. }
                | Command::CaptureFrame { .. }
        )
    }),
    (Domain::Ndi, |command| {
        matches!(
            command,
            Command::SetNdi(_) | Command::RefreshNdi | Command::ResetNdiDiscovery
        )
    }),
    (Domain::Network, |command| {
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
    }),
    (Domain::Tessera, |command| {
        matches!(
            command,
            Command::TesseraService { .. }
                | Command::RestoreTessera { .. }
                | Command::SetTessera(_)
                | Command::PutTesseraFlows { .. }
                | Command::SetTesseraLabel { .. }
        )
    }),
    (Domain::Display, |command| {
        matches!(
            command,
            Command::SetDisplay(_)
                | Command::PreviewSplash { .. }
                | Command::SetLogo { .. }
                | Command::ImportBackup {
                    scope: RestoreScope::Display,
                    ..
                }
        )
    }),
    (Domain::Presets, |command| {
        matches!(
            command,
            Command::SavePreset { .. } | Command::DeletePreset { .. }
        )
    }),
    (Domain::Plugins, |command| {
        matches!(
            command,
            Command::InstallPlugin { .. }
                | Command::UpdatePlugin { .. }
                | Command::SetPluginEnabled { .. }
                | Command::RemovePlugin { .. }
                | Command::CancelPluginJob { .. }
                | Command::ResetPluginState
                | Command::SetPluginData { .. }
        )
    }),
];
pub fn command_domain(command: &Command) -> Option<Domain> {
    COMMAND_ROUTES
        .iter()
        .find(|(_, owns)| owns(command))
        .map(|(domain, _)| *domain)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registries_and_ownership_cover_each_domain_once() {
        let domains: std::collections::BTreeSet<_> = REGISTRY.iter().map(|r| r.domain).collect();
        assert_eq!(domains.len(), REGISTRY.len());
        assert_eq!(
            domains,
            ndimon_render::OWNERSHIP.iter().map(|(d, _)| *d).collect()
        );
        assert_eq!(
            domains,
            ndimon_render::REGISTRY.iter().map(|r| r.domain).collect()
        );
        for registration in REGISTRY {
            assert_eq!(
                registration.ownership(),
                ndimon_render::ownership(registration.domain)
            );
            assert_eq!(
                serde_json::to_value(registration.domain).unwrap(),
                registration.name
            );
        }
        assert_eq!(
            REGISTRY
                .iter()
                .filter(|r| r.ownership() == Ownership::Native)
                .map(|r| r.domain)
                .collect::<Vec<_>>(),
            vec![
                Domain::Network,
                Domain::Tessera,
                Domain::Audio,
                Domain::Device,
                Domain::Outputs,
                Domain::Ndi,
                Domain::Display,
                Domain::Presets,
                Domain::Plugins
            ]
        );
    }
    #[test]
    fn audio_and_followed_commands_have_one_owner() {
        for (domain, command) in [
            (Domain::Audio, Command::PutAudio(Audio::default())),
            (
                Domain::Audio,
                Command::PutOutputAudio {
                    output: 1,
                    audio: OutputAudio::default(),
                    normalize_matrix: false,
                },
            ),
            (
                Domain::Audio,
                Command::ImportBackup {
                    document: Box::new(BackupDocument::new(DesiredState::default())),
                    scope: RestoreScope::Audio,
                },
            ),
            (Domain::Device, Command::SetDevice(DevicePatch::default())),
            (Domain::Network, Command::RollbackNetwork),
            (Domain::Ndi, Command::RefreshNdi),
        ] {
            assert_eq!(command_domain(&command), Some(domain));
            assert_eq!(
                COMMAND_ROUTES
                    .iter()
                    .filter(|(_, owns)| owns(&command))
                    .count(),
                1
            );
            if ndimon_render::ownership(domain) != Ownership::Native {
                assert!(matches!(
                    reduce(&mut DesiredState::default(), &command),
                    Err(ApiError::Unsupported { .. })
                ));
            }
        }
    }
}
