use crate::domains;
use ndimon_types::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Action {
    Audio(domains::audio::Action),
    Network(domains::network::Action),
    Display(domains::display::Action),
    Outputs(domains::outputs::Action),
    Ndi(domains::ndi::Action),
    Device(domains::device::Action),
    Tessera(domains::tessera::Action),
    Presets(domains::presets::Action),
    Plugins(domains::plugins::Action),
}
#[derive(Debug, Clone, Default)]
pub struct DomainPlan {
    pub actions: Vec<Action>,
    pub impact: Impact,
}
#[derive(Debug, Clone)]
pub struct Plan {
    pub paths: Vec<String>,
    pub domains: Vec<Domain>,
    pub actions: Vec<Action>,
    pub impact: Impact,
    /// Confirmation forwarded to a counterpart after its files are staged.
    pub confirmation: Option<Option<String>>,
    pub external_confirmation: bool,
    /// Trusted connection context, never part of confirmation digests.
    pub session: Option<NetworkSessionPath>,
}
pub fn plan(
    previous: &DesiredState,
    next: &DesiredState,
    command: &Command,
) -> Result<Plan, ApiError> {
    let a = serde_json::to_value(previous).map_err(internal)?;
    let b = serde_json::to_value(next).map_err(internal)?;
    let mut paths = Vec::new();
    let mut changed = Vec::new();
    let mut actions = Vec::new();
    let mut impact = Impact::default();
    for registration in domains::planning_order(command) {
        let start = paths.len();
        diff(
            registration.name,
            &a[registration.name],
            &b[registration.name],
            &mut paths,
        );
        if paths.len() == start
            && !(domains::command_domain(command) == Some(registration.domain)
                && registration.module.imperative(command))
        {
            continue;
        }
        changed.push(registration.domain);
        let contribution = registration.module.plan(previous, next, command)?;
        actions.extend(contribution.actions);
        merge_impact(&mut impact, contribution.impact);
    }
    let external_confirmation = changed
        .iter()
        .any(|domain| domains::get(*domain).is_ok_and(|r| r.module.external_confirmation(command)));
    merge_impact(&mut impact, domains::operation_impact(command));
    let mut context = paths.clone();
    context.extend(domains::operation_context(previous, command));
    if let Some(domain) = domains::command_domain(command) {
        context.push(format!(
            "{}_revision_context:{}",
            domains::get(domain)?.name,
            previous.revision
        ));
    }
    context.push(serde_json::to_string(&actions).map_err(internal)?);
    impact = impact.sealed(command, &context).map_err(internal)?;
    Ok(Plan {
        paths,
        domains: changed,
        actions,
        impact,
        confirmation: None,
        external_confirmation,
        session: None,
    })
}
fn merge_impact(impact: &mut Impact, contribution: Impact) {
    impact.scope = impact.scope.max(contribution.scope);
    for restart in contribution.restarts {
        if !impact.restarts.contains(&restart) {
            impact.restarts.push(restart);
        }
    }
    for interruption in contribution.interruptions {
        if !impact.interruptions.contains(&interruption) {
            impact.interruptions.push(interruption);
        }
    }
    impact.lines.extend(contribution.lines);
}
fn diff(path: &str, a: &serde_json::Value, b: &serde_json::Value, out: &mut Vec<String>) {
    if a == b {
        return;
    }
    if let (Some(a), Some(b)) = (a.as_object(), b.as_object()) {
        let keys: std::collections::BTreeSet<_> = a.keys().chain(b.keys()).collect();
        for key in keys {
            diff(
                &format!("{path}.{key}"),
                a.get(key).unwrap_or(&serde_json::Value::Null),
                b.get(key).unwrap_or(&serde_json::Value::Null),
                out,
            );
        }
    } else {
        out.push(path.into());
    }
}
pub(crate) fn internal(e: impl ToString) -> ApiError {
    ApiError::Internal {
        message: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::audio::Action as AudioAction;

    #[test]
    fn audio_planner_table_and_confirmation_context_stay_compatible() {
        for (engine, change, command, expected, scope) in [
            (
                AudioEngine::V1,
                "none",
                Command::PutAudio(Audio::default()),
                vec![],
                ImpactScope::None,
            ),
            (
                AudioEngine::V1,
                "delay",
                Command::PutAudio(Audio::default()),
                vec![
                    AudioAction::CheckAudio,
                    AudioAction::PreparePcms,
                    AudioAction::Render,
                    AudioAction::ReloadAudio,
                ],
                ImpactScope::None,
            ),
            (
                AudioEngine::V1,
                "period",
                Command::PutAudio(Audio::default()),
                vec![
                    AudioAction::PreparePcms,
                    AudioAction::Render,
                    AudioAction::RestartDecoder,
                ],
                ImpactScope::Service,
            ),
            (
                AudioEngine::V2,
                "period",
                Command::PutAudio(Audio::default()),
                vec![
                    AudioAction::PreparePcms,
                    AudioAction::Render,
                    AudioAction::ReloadAudio,
                ],
                ImpactScope::None,
            ),
            (
                AudioEngine::V1,
                "engine",
                Command::SetAudioEngine {
                    engine: AudioEngine::V2,
                },
                vec![
                    AudioAction::PreparePcms,
                    AudioAction::Render,
                    AudioAction::RestartDecoder,
                ],
                ImpactScope::Service,
            ),
            (
                AudioEngine::V1,
                "period",
                Command::RemoveAudioEndpoints { ids: vec![] },
                vec![AudioAction::Render, AudioAction::RestartDecoder],
                ImpactScope::Service,
            ),
        ] {
            let mut previous = DesiredState::default();
            previous.revision = 42;
            previous.audio.engine = engine;
            let mut next = previous.clone();
            match change {
                "delay" => next.audio.video_delay_ms = 1,
                "period" => next.audio.settings.period_frames = 1024,
                "engine" => next.audio.engine = AudioEngine::V2,
                _ => (),
            }
            let plan = plan(&previous, &next, &command).unwrap();
            assert_eq!(
                plan.actions,
                expected
                    .iter()
                    .cloned()
                    .map(Action::Audio)
                    .collect::<Vec<_>>(),
                "{engine:?}/{change}"
            );
            assert_eq!(plan.impact.scope, scope);
            let mut context = plan.paths.clone();
            context.push("audio_revision_context:42".into());
            // Audio's old action list was unwrapped; its digest must survive.
            context.push(serde_json::to_string(&expected).unwrap());
            assert_eq!(
                plan.impact.token,
                plan.impact
                    .clone()
                    .sealed(&command, &context)
                    .unwrap()
                    .token
            );
            assert_eq!(
                plan.domains,
                if change == "none" {
                    vec![]
                } else {
                    vec![Domain::Audio]
                }
            );
        }
    }

    #[test]
    fn diff_discovers_domains_and_paths_in_registry_order() {
        let previous = DesiredState::default();
        let mut next = previous.clone();
        next.device.ndi_alias = "Unit".into();
        next.audio.video_delay_ms = 1;
        let plan = plan(&previous, &next, &Command::PutAudio(next.audio.clone())).unwrap();
        assert_eq!(plan.domains, vec![Domain::Audio, Domain::Device]);
        assert_eq!(plan.paths, vec!["audio.video_delay_ms", "device.ndi_alias"]);
    }

    #[test]
    fn impact_merge_unions_effects_and_keeps_domain_line_order() {
        let interruption = Interruption::new(Domain::Audio, "decoder", Some(3000));
        let mut merged = Impact::default();
        for (scope, services, lines) in [
            (ImpactScope::Service, vec![Service::Decoder], vec!["audio"]),
            (
                ImpactScope::Network,
                vec![Service::Decoder, Service::Finder],
                vec!["network", "confirm"],
            ),
            (ImpactScope::Output, vec![], vec!["display"]),
        ] {
            let mut contribution = Impact::new(scope);
            contribution.restarts = services;
            contribution.interruptions.push(interruption.clone());
            contribution.lines = lines.into_iter().map(str::to_string).collect();
            contribution.token = ImpactToken::new("must not be copied");
            merge_impact(&mut merged, contribution);
        }
        assert_eq!(merged.scope, ImpactScope::Network);
        assert_eq!(merged.restarts, vec![Service::Decoder, Service::Finder]);
        assert_eq!(merged.interruptions, vec![interruption]);
        assert_eq!(merged.lines, vec!["audio", "network", "confirm", "display"]);
        assert_eq!(merged.token, ImpactToken::default());
    }
}
