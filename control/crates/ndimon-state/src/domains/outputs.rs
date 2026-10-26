//! Native output intent, live video actions and observed display queries.
pub mod follower;
use crate::{
    BoxFuture,
    apply::{Applier, ProductionApplier},
    persist::StateFile,
    plan::{Action as DomainAction, DomainPlan, Plan, internal},
};
use ndimon_render::LegacyFiles;
use ndimon_types::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    OutputsConnect {
        output: OutputId,
    },
    OutputsForget {
        output: OutputId,
    },
    OutputsForgetNow {
        output: OutputId,
    },
    OutputsResolution {
        output: OutputId,
    },
    OutputsRotation {
        output: OutputId,
    },
    OutputsScale {
        output: OutputId,
    },
    OutputsReconnect {
        output: OutputId,
    },
    OutputsRetry {
        output: OutputId,
        name: String,
        endpoint: String,
    },
    OutputsDeliver,
}
fn invalid(reason: impl Into<String>) -> ApiError {
    ApiError::invalid(vec![FieldError::new("outputs", reason)])
}
fn output(state: &mut DesiredState, id: OutputId) -> Result<&mut Output, ApiError> {
    if !(1..=8).contains(&id) {
        return Err(invalid("output must be 1–8"));
    }
    Ok(state.outputs.entry(id).or_default())
}
#[derive(Debug)]
pub struct Module;
impl super::DomainModule for Module {
    fn imperative(&self, command: &Command) -> bool {
        matches!(
            command,
            Command::ReconnectSource { .. }
                | Command::RetrySource { .. }
                | Command::SelectSource { .. }
                | Command::ForgetSource { .. }
                | Command::SetOutputVideo { .. }
        )
    }
    fn reduce(&self, state: &mut DesiredState, command: &Command) -> Result<(), ApiError> {
        match command {
            Command::PutOutputSettings {
                outputs,
                ndi,
                audio_enabled,
            } => {
                for (id, out) in outputs {
                    *output(state, *id)? = out.clone();
                }
                if let Some(ndi) = ndi {
                    state.ndi = ndi.clone();
                }
                for (id, enabled) in audio_enabled {
                    output(state, *id)?;
                    if let Some(out) = state.audio.outputs.get_mut(id) {
                        out.enabled = *enabled;
                    } else if *enabled {
                        return Err(invalid(
                            "configure audio endpoints before enabling output audio",
                        ));
                    }
                }
            }
            Command::SelectSource { output: id, source } => {
                let out = output(state, *id)?;
                out.source = Some(source.clone());
                out.source_selection = "NDI".into();
            }
            Command::ForgetSource { output: id } => output(state, *id)?.source = None,
            Command::AdoptRemoteSource { output: id, source } => {
                if source
                    .as_ref()
                    .is_some_and(|s| s.name.is_empty() || s.name == "None")
                {
                    return Err(invalid("invalid source name"));
                }
                let out = output(state, *id)?;
                // Routed by name: the Discovery Server resolves the sender.
                out.source = source.as_ref().map(|s| NdiSourceRef::new(&s.name, ""));
                if out.source.is_some() {
                    out.source_selection = "NDI".into();
                }
            }
            Command::SetOutputAlias { output: id, alias } => {
                output(state, *id)?.alias = alias.trim().into()
            }
            Command::SetOutputVideo { output: id, video } => {
                output(state, *id)?.video = video.clone()
            }
            Command::SetScreenSaver {
                output: id,
                screensaver,
            } => output(state, *id)?.screensaver = *screensaver,
            Command::ReconnectSource { output: id } => {
                if !(1..=8).contains(id) {
                    return Err(invalid("output must be 1–8"));
                }
                if state
                    .outputs
                    .get(id)
                    .and_then(|o| o.source.as_ref())
                    .is_none()
                {
                    return Err(invalid("no saved source"));
                }
            }
            Command::RetrySource { output: id, source } => {
                let saved = state.outputs.get(id).and_then(|o| o.source.as_ref());
                if saved.is_none_or(|s| s.name != source.name) {
                    return Err(invalid("saved source changed; cancel stale reconnect"));
                }
                if !source.endpoint.is_empty() && !valid_endpoint(&source.endpoint) {
                    return Err(invalid("invalid source IPv4 endpoint"));
                }
            }
            Command::CaptureFrame { .. } => {
                return Err(ApiError::Unsupported {
                    what: "frame capture has no decoder consumer".into(),
                });
            }
            _ => return Err(invalid("unsupported output command")),
        }
        Ok(())
    }
    fn import(&self, state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String> {
        ndimon_render::outputs::import_json(state, files)
    }
    fn plan(
        &self,
        previous: &DesiredState,
        next: &DesiredState,
        command: &Command,
    ) -> Result<DomainPlan, ApiError> {
        let mut actions = Vec::new();
        let mut impact = Impact::default();
        for (id, out) in &next.outputs {
            let old = previous.outputs.get(id).cloned().unwrap_or_default();
            if old.source != out.source {
                actions.push(if out.source.is_some() {
                    Action::OutputsConnect { output: *id }
                } else {
                    Action::OutputsForget { output: *id }
                });
                // Source switching is brief and needs no disruptive confirmation.
                impact
                    .lines
                    .push(if matches!(command, Command::AdoptRemoteSource { .. }) {
                        format!("Save the Discovery Server's source for output {id}")
                    } else {
                        format!("Switch source on output {id}")
                    });
            }
            if old.video.resolution != out.video.resolution {
                actions.push(Action::OutputsResolution { output: *id });
            }
            if old.video.rotation != out.video.rotation {
                actions.push(Action::OutputsRotation { output: *id });
            }
            if old.video.scale_mode != out.video.scale_mode {
                actions.push(Action::OutputsScale { output: *id });
            }
        }
        if let Command::ReconnectSource { output } = command {
            actions.push(Action::OutputsReconnect { output: *output });
        }
        if let Command::RetrySource { output, source } = command {
            actions.push(Action::OutputsRetry {
                output: *output,
                name: source.name.clone(),
                endpoint: source.endpoint.clone(),
            });
        }
        match command {
            Command::SetOutputVideo { output, video }
                if previous
                    .outputs
                    .get(output)
                    .is_some_and(|o| o.video == *video) =>
            {
                actions.extend([
                    Action::OutputsResolution { output: *output },
                    Action::OutputsRotation { output: *output },
                    Action::OutputsScale { output: *output },
                ]);
            }
            Command::SelectSource { output, source }
                if previous.outputs.get(output).and_then(|o| o.source.as_ref()) == Some(source) =>
            {
                actions.push(Action::OutputsRetry {
                    output: *output,
                    name: source.name.clone(),
                    endpoint: source.endpoint.clone(),
                });
            }
            Command::ForgetSource { output }
                if previous
                    .outputs
                    .get(output)
                    .and_then(|o| o.source.as_ref())
                    .is_none() =>
            {
                actions.push(Action::OutputsForgetNow { output: *output });
            }
            _ => (),
        }
        if previous.outputs != next.outputs {
            actions.push(Action::OutputsDeliver);
        }
        Ok(DomainPlan {
            actions: actions.into_iter().map(DomainAction::Outputs).collect(),
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
            for action in &plan.actions {
                if let DomainAction::Outputs(Action::OutputsRotation { output }) = action {
                    let modes = applier
                        .decoder
                        .request(json!({"action":"get_modes","output":output - 1}))
                        .await?;
                    let rotation = next.document.outputs[output].video.rotation;
                    if !modes["supported_rotations"]
                        .as_array()
                        .is_some_and(|a| a.contains(&json!(rotation)))
                    {
                        return Err(json!({"ok":false,"error":"rotation_unsupported"}).to_string());
                    }
                }
            }
            // Establish IPC ownership before any setter can use the old C++ writer.
            follower::decoder(applier, previous, &[]).await
        })
    }
    fn apply<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            let reply = apply_actions(applier, state, plan).await?;
            follower::decoder(applier, state, &plan.impact.restarts).await?;
            Ok(reply)
        })
    }
    fn rollback<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
        plan: &'a Plan,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let mut video_plan = plan.clone();
            video_plan.actions.retain(|action| {
                matches!(
                    action,
                    DomainAction::Outputs(
                        Action::OutputsResolution { .. }
                            | Action::OutputsRotation { .. }
                            | Action::OutputsScale { .. }
                    )
                )
            });
            let mut previous = state.clone();
            for action in &video_plan.actions {
                let id = match action {
                    DomainAction::Outputs(
                        Action::OutputsResolution { output }
                        | Action::OutputsRotation { output }
                        | Action::OutputsScale { output },
                    ) => *output,
                    _ => continue,
                };
                previous.document.outputs.entry(id).or_default();
            }
            let runtime = apply_actions(applier, &previous, &video_plan).await;
            let delivered = follower::decoder(applier, state, &[]).await;
            runtime?;
            delivered
        })
    }
    fn replay<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { follower::decoder(applier, state, &[]).await })
    }
    fn query<'a>(
        &self,
        _state: &'a StateFile,
        applier: &'a dyn Applier,
        query: &'a Query,
    ) -> Option<BoxFuture<'a, Result<Value, ApiError>>> {
        match query {
            Query::Catalog {
                name: Catalog::DisplayModes,
                output,
            } => {
                let id = output.unwrap_or(1);
                Some(Box::pin(async move {
                    if !(1..=8).contains(&id) {
                        return Err(invalid("output must be 1–8"));
                    }
                    applier
                        .decoder(json!({"action":"get_modes","output":id - 1}))
                        .await
                        .map_err(internal)
                }))
            }
            _ => None,
        }
    }
}
fn deadline() -> Result<i64, String> {
    let time =
        nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC).map_err(|e| e.to_string())?;
    Ok(time.tv_sec() * 1000 + time.tv_nsec() / 1_000_000 + 1500)
}
pub async fn apply_actions(
    applier: &ProductionApplier,
    state: &StateFile,
    plan: &Plan,
) -> Result<Value, String> {
    let mut reply = json!({"ok":true});
    for action in &plan.actions {
        let DomainAction::Outputs(action) = action else {
            continue;
        };
        let (id, verb) = match action {
            Action::OutputsResolution { output } => (*output, "resolution"),
            Action::OutputsRotation { output } => (*output, "rotation"),
            Action::OutputsScale { output } => (*output, "scale"),
            Action::OutputsReconnect { output } => (*output, "reconnect"),
            Action::OutputsRetry { output, .. } => (*output, "retry"),
            Action::OutputsForgetNow { output } => (*output, "forget"),
            // apply_config performs connect/forget itself, exactly once.
            _ => continue,
        };
        let out = state.document.outputs.get(&id).ok_or("output missing")?;
        let mut command = match verb {
            "resolution" => match out.video.resolution {
                Resolution::Mode {
                    width,
                    height,
                    refresh_hz,
                } => {
                    json!({"action":"set_resolution","width":width,"height":height,"refresh":0,"refresh_hz":refresh_hz,"deadline_monotonic_ms":deadline()?})
                }
                _ => json!({"action":"auto_resolution","deadline_monotonic_ms":deadline()?}),
            },
            "rotation" => json!({"action":"set_rotation","rotation":out.video.rotation}),
            "scale" => json!({"action":"set_scale_mode","scale_mode":out.video.scale_mode}),
            "forget" => json!({"action":"forget_source"}),
            "retry" => {
                let Action::OutputsRetry { name, endpoint, .. } = action else {
                    unreachable!("retry action")
                };
                json!({"action":"connect","source_name":name,"source_ip":endpoint})
            }
            _ => {
                let source = out.source.as_ref().ok_or("no saved source")?;
                let sources = applier.ndi_sources().await.unwrap_or(Value::Null);
                let endpoint = sources["list"][&source.name]
                    .as_str()
                    .filter(|s| valid_endpoint(s))
                    .unwrap_or(&source.endpoint);
                json!({"action":"connect","source_name":source.name,"source_ip":endpoint})
            }
        };
        command["output"] = json!(id - 1);
        let result = applier.decoder.request(command).await?;
        if result["ok"] != true {
            return Err(result.to_string());
        }
        reply = result;
    }
    Ok(reply)
}
pub fn valid_endpoint(value: &str) -> bool {
    let mut state = DesiredState::default();
    state.outputs.get_mut(&1).expect("primary").source = Some(NdiSourceRef::new("source", value));
    !value.is_empty() && ndimon_types::validate(&state).is_ok()
}

#[cfg(test)]
mod follower_tests;
#[cfg(test)]
mod tests;
