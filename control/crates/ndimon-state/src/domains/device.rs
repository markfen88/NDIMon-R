//! Native appliance settings and explicitly owned System operations.
pub mod system;
use crate::{
    BoxFuture,
    apply::{Applier, ProductionApplier},
    persist::StateFile,
    plan::{Action as DomainAction, DomainPlan, Plan, internal},
};
use ndimon_privd_proto::{ManagedUnit, Request, UnitAction};
use ndimon_render::LegacyFiles;
use ndimon_types::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    DeviceHostname,
    DeviceTimeSync,
    DeviceRebootSchedule,
    DeviceApplyConfig,
    DeviceReboot,
    DeviceRestartStack,
    DeviceInstallUpdate {
        version: String,
        force: bool,
    },
    DeviceServiceControl {
        service: Service,
        action: ServiceAction,
    },
}
fn invalid(field: &str, reason: impl Into<String>) -> ApiError {
    ApiError::invalid(vec![FieldError::new(field, reason)])
}
fn unit(service: Service) -> Result<ManagedUnit, ApiError> {
    match service {
        Service::Control => Ok(ManagedUnit::Control),
        Service::Decoder => Ok(ManagedUnit::Decoder),
        Service::Finder => Ok(ManagedUnit::Finder),
        _ => Err(ApiError::Unsupported {
            what: "service is outside the appliance System surface".into(),
        }),
    }
}
fn unit_action(action: ServiceAction) -> Result<UnitAction, ApiError> {
    match action {
        ServiceAction::Start => Ok(UnitAction::Start),
        ServiceAction::Stop => Ok(UnitAction::Stop),
        ServiceAction::Restart => Ok(UnitAction::Restart),
        ServiceAction::Reload => Ok(UnitAction::Reload),
        _ => Err(invalid("action", "unsupported service action")),
    }
}
pub fn patch(device: &mut Device, patch: &DevicePatch) {
    macro_rules! assign {
        ($($field:ident),* $(,)?) => { $(if let Some(value) = &patch.$field { device.$field.clone_from(value); })* };
    }
    assign!(
        hostname,
        ndi_alias,
        mode,
        decode_mode,
        watchdog_mode,
        ntp_server,
        reboot_schedule,
        about,
        analog_audio_in_gain,
        analog_audio_out_gain,
        plugin_feeds,
        allow_insecure_feeds,
        web_allowed_hosts
    );
    device.ndi_alias = device.ndi_alias.trim().into();
    device.ntp_server = device.ntp_server.trim().into();
    for host in &mut device.web_allowed_hosts {
        host.make_ascii_lowercase();
    }
    if patch.ndi_alias.is_some() && !device.ndi_alias.is_empty() {
        device.hostname = alias_hostname(&device.ndi_alias);
    }
    device.reboot_schedule.days.sort();
    device.reboot_schedule.days.dedup();
}
fn actions(plan: &Plan) -> impl Iterator<Item = &Action> {
    plan.actions.iter().filter_map(|a| match a {
        DomainAction::Device(a) => Some(a),
        _ => None,
    })
}
fn time_sync(device: &Device) -> Result<Request, String> {
    serde_json::from_value(json!({"set_time_sync":{"server":if device.ntp_server.is_empty(){Value::Null}else{json!(device.ntp_server)}}}))
        .map_err(|e| e.to_string())
}
fn hostname(device: &Device) -> Result<Option<Request>, String> {
    if device.hostname.is_empty() {
        return Ok(None);
    }
    serde_json::from_value(json!({"set_hostname":{"name":device.hostname}}))
        .map(Some)
        .map_err(|e| e.to_string())
}
fn schedule(device: &Device) -> Result<Request, String> {
    let s = &device.reboot_schedule;
    let value = if !s.enabled {
        json!("disabled")
    } else if s.days.len() == 7 {
        json!({"daily":{"time":s.time}})
    } else {
        json!({"weekly":{"time":s.time,"days":s.days}})
    };
    serde_json::from_value(json!({"set_reboot_schedule":{"schedule":value}}))
        .map_err(|e| e.to_string())
}
/// Equal revisions are intentional: recovery and rollback must be retryable.
pub async fn deliver(decoder: &dyn crate::ipc::Decoder, state: &StateFile) -> Result<(), String> {
    let reply = crate::follower::deliver(
        &|command| decoder.request(command),
        state,
        json!({"device":state.document.device}),
    )
    .await?;
    if reply["restart_required"] == true {
        return Err(format!("device apply_config refused: {reply}"));
    }
    Ok(())
}
/// Send the selected privileged settings. `unchanged_are_skipped` is for replay,
/// which asserts the whole device on a schedule: a setting that this process
/// already applied, to the same value, never reaches privd again.
async fn configure(
    applier: &ProductionApplier,
    state: &StateFile,
    selected: &[Action],
    unchanged_are_skipped: bool,
) -> Result<(), String> {
    for action in selected {
        let (setting, request) = match action {
            Action::DeviceHostname => ("hostname", hostname(&state.document.device)?),
            Action::DeviceTimeSync => ("time_sync", Some(time_sync(&state.document.device)?)),
            Action::DeviceRebootSchedule => {
                ("reboot_schedule", Some(schedule(&state.document.device)?))
            }
            Action::DeviceApplyConfig => {
                deliver(applier.decoder.as_ref(), state).await?;
                continue;
            }
            _ => continue,
        };
        let Some(request) = request else { continue };
        let applied = &applier.memory.applied;
        let value = serde_json::to_value(&request).map_err(|e| e.to_string())?;
        if unchanged_are_skipped && applied.is_applied(setting, &value) {
            continue;
        }
        match applier.privd.execute(request).await {
            Ok(_) => applied.record(setting, value),
            Err(e) => {
                applied.forget(setting);
                return Err(e.to_string());
            }
        }
    }
    Ok(())
}
#[derive(Debug)]
pub struct Module;
impl super::DomainModule for Module {
    fn imperative(&self, command: &Command) -> bool {
        matches!(
            command,
            Command::Reboot
                | Command::RestartStack
                | Command::InstallUpdate { .. }
                | Command::ServiceControl { .. }
        )
    }
    /// A release lookup waits for the network: it must not hold the actor.
    fn query_outside_actor(&self, query: &Query) -> bool {
        matches!(query, Query::Version)
    }
    fn reduce(&self, state: &mut DesiredState, command: &Command) -> Result<(), ApiError> {
        match command {
            Command::SetDevice(p) => patch(&mut state.device, p),
            Command::ServiceControl { service, action } => {
                unit(*service)?;
                unit_action(*action)?;
            }
            Command::InstallUpdate { version, force, .. } => {
                system::check_signed_update(
                    version
                        .as_deref()
                        .ok_or_else(|| invalid("version", "signed update version required"))?,
                    *force,
                )?;
            }
            Command::Reboot | Command::RestartStack => (),
            _ => {
                return Err(ApiError::Unsupported {
                    what: "unsupported device command".into(),
                });
            }
        }
        Ok(())
    }
    fn import(&self, state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String> {
        ndimon_render::device::import_json(state, files)
    }
    fn plan(
        &self,
        previous: &DesiredState,
        next: &DesiredState,
        command: &Command,
    ) -> Result<DomainPlan, ApiError> {
        let mut selected = Vec::new();
        let mut impact = Impact::default();
        let old = &previous.device;
        let new = &next.device;
        if old.hostname != new.hostname {
            selected.push(Action::DeviceHostname);
        }
        if old.ntp_server != new.ntp_server {
            selected.push(Action::DeviceTimeSync);
        }
        if old.reboot_schedule != new.reboot_schedule {
            selected.push(Action::DeviceRebootSchedule);
        }
        if old != new {
            selected.push(Action::DeviceApplyConfig);
        }
        let effect = match command {
            Command::Reboot => Some((
                Action::DeviceReboot,
                ImpactScope::Device,
                "Reboot the device; all video, audio and control connections stop",
            )),
            Command::RestartStack => Some((
                Action::DeviceRestartStack,
                ImpactScope::Service,
                "Restart the NDIMon stack; video, audio and API connections pause",
            )),
            Command::InstallUpdate { version, force, .. } => Some((
                Action::DeviceInstallUpdate {
                    version: version
                        .clone()
                        .ok_or_else(|| invalid("version", "signed update version required"))?,
                    force: *force,
                },
                ImpactScope::System,
                "Install the signed NDIMon release; services restart and video/audio pause",
            )),
            Command::ServiceControl { service, action } => {
                if matches!(
                    action,
                    ServiceAction::Start | ServiceAction::Stop | ServiceAction::Restart
                ) {
                    impact.scope = ImpactScope::Service;
                    impact.restarts.push(*service);
                    impact.interruptions.push(Interruption::new(
                        Domain::Device,
                        format!("{service:?}"),
                        None,
                    ));
                    impact.lines.push(format!(
                        "{action:?} {service:?}; its connections and streams may pause"
                    ));
                }
                selected.push(Action::DeviceServiceControl {
                    service: *service,
                    action: *action,
                });
                None
            }
            _ => None,
        };
        if let Some((action, scope, line)) = effect {
            selected.push(action);
            impact.scope = scope;
            impact.restarts = vec![Service::Decoder, Service::Finder, Service::Control];
            impact.interruptions.push(Interruption::new(
                Domain::Device,
                "device video/audio/control",
                None,
            ));
            impact.lines.push(line.into());
        }
        Ok(DomainPlan {
            actions: selected.into_iter().map(DomainAction::Device).collect(),
            impact,
        })
    }
    fn preflight<'a>(
        &self,
        _applier: &'a ProductionApplier,
        _previous: &'a StateFile,
        next: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            for action in actions(plan) {
                let request = match action {
                    Action::DeviceHostname => hostname(&next.document.device)?,
                    Action::DeviceTimeSync => Some(time_sync(&next.document.device)?),
                    Action::DeviceRebootSchedule => Some(schedule(&next.document.device)?),
                    _ => None,
                };
                if let Some(request) = request {
                    request.validate().map_err(|e| e.to_string())?;
                }
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
        Box::pin(async move {
            let selected: Vec<_> = actions(plan).cloned().collect();
            configure(applier, state, &selected, false).await?;
            for action in selected {
                let request = match action {
                    Action::DeviceReboot | Action::DeviceRestartStack => {
                        let (request, delay) = if action == Action::DeviceReboot {
                            (Request::Reboot, 1000)
                        } else {
                            (Request::RestartStack, 500)
                        };
                        let client = applier.privd.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                            if let Err(error) = client.request(request).await {
                                tracing::error!(%error,"System action failed after acknowledgement");
                            }
                        });
                        None
                    }
                    Action::DeviceInstallUpdate { version, force } => Some(
                        serde_json::from_value(
                            json!({"install_update":{"version":version,"force":force}}),
                        )
                        .map_err(|e| e.to_string())?,
                    ),
                    Action::DeviceServiceControl { service, action } => {
                        Some(Request::ServiceControl {
                            unit: unit(service).map_err(|e| e.to_string())?,
                            action: unit_action(action).map_err(|e| e.to_string())?,
                        })
                    }
                    _ => None,
                };
                if let Some(request) = request {
                    let output = applier
                        .privd
                        .request(request)
                        .await
                        .map_err(system::priv_error)?;
                    return Ok(json!({"ok":true,"job":output.job}));
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
            let selected: Vec<_> = actions(plan).cloned().collect();
            configure(applier, state, &selected, false).await
        })
    }
    fn replay<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let mut selected = vec![Action::DeviceHostname];
            // Empty NTP means OS defaults; do not disable/reset it on every boot.
            if !state.document.device.ntp_server.is_empty() {
                selected.push(Action::DeviceTimeSync);
            }
            selected.push(Action::DeviceRebootSchedule);
            configure(applier, state, &selected, true).await?;
            deliver(applier.decoder.as_ref(), state).await
        })
    }
    fn query<'a>(
        &self,
        state: &'a StateFile,
        applier: &'a dyn Applier,
        query: &'a Query,
    ) -> Option<BoxFuture<'a, Result<Value, ApiError>>> {
        if !matches!(
            query,
            Query::Version
                | Query::VaapiInfo
                | Query::Hostname
                | Query::Ntp
                | Query::RebootSchedule
                | Query::AboutMe
                | Query::Status
        ) {
            return None;
        }
        Some(Box::pin(async move {
            if matches!(query, Query::Status) {
                return Ok(applier
                    .decoder(json!({"action":"status"}))
                    .await
                    .unwrap_or_else(|error| json!({"ok":false,"error":error})));
            }
            let mut value = applier.system(query.clone()).await.map_err(internal)?;
            if matches!(query, Query::Version) && value["update_supported"] == true {
                system::add_latest_release(&mut value, applier.latest_release().await);
            }
            if matches!(query, Query::Ntp) {
                value["ntp_server"] = json!(state.document.device.ntp_server);
            }
            if matches!(query, Query::RebootSchedule) {
                let schedule = &state.document.device.reboot_schedule;
                value["enabled"] = json!(schedule.enabled);
                value["time"] = json!(schedule.time);
                value["days"] = json!(if schedule.days.is_empty() {
                    RebootSchedule::default().days
                } else {
                    schedule.days.clone()
                });
                if !schedule.enabled {
                    value["next"] = Value::Null;
                }
            }
            Ok(value)
        }))
    }
}

#[cfg(test)]
mod tests;
