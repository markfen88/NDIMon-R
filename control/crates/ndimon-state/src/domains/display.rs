//! Native display intent and non-disruptive decoder delivery.
pub mod assets;
use super::DomainModule;
use crate::{
    BoxFuture,
    apply::{Applier, ProductionApplier},
    persist::StateFile,
    plan::{DomainPlan, Plan},
};
use ndimon_render::LegacyFiles;
use ndimon_types::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    DisplayApplyConfig,
}
#[derive(Debug)]
pub struct Module;
fn invalid(reason: &str) -> ApiError {
    ApiError::invalid(vec![FieldError::new("display", reason)])
}

/// Equal revision retries are intentional. Refusals never count as application.
pub async fn deliver(
    applier: &dyn Applier,
    display: &Display,
    revision: u64,
) -> Result<Value, String> {
    let mut state = StateFile::new(DesiredState::default(), Default::default());
    state.revision = revision;
    state.document.revision = revision;
    let reply = crate::follower::deliver(
        &|command| applier.decoder(command),
        &state,
        json!({"display":display}),
    )
    .await?;
    if reply["restart_required"] == true {
        return Err(reply.to_string());
    }
    Ok(reply)
}
impl DomainModule for Module {
    fn reduce(&self, state: &mut DesiredState, command: &Command) -> Result<(), ApiError> {
        match command {
            Command::SetDisplay(display) => state.display = display.clone(),
            Command::SetLogo { asset } => state.display.splash.logo = asset.clone(),
            Command::PreviewSplash { .. } => (),
            Command::ImportBackup {
                document,
                scope: RestoreScope::Display,
            } => state.display = document.document.display.clone(),
            _ => return Err(invalid("unsupported display command")),
        }
        Ok(())
    }
    fn validate_assets(
        &self,
        state: &DesiredState,
        store: &assets::AssetStore,
    ) -> Result<(), String> {
        if let Some(asset) = &state.display.splash.logo {
            store.reconcile(asset)?;
        }
        Ok(())
    }
    fn plan(
        &self,
        _: &DesiredState,
        _: &DesiredState,
        _: &Command,
    ) -> Result<DomainPlan, ApiError> {
        Ok(DomainPlan {
            actions: vec![crate::plan::Action::Display(Action::DisplayApplyConfig)],
            impact: Impact::default(),
        })
    }
    fn import(&self, state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String> {
        ndimon_render::display::import_json(state, files)
    }
    fn live<'a>(
        &self,
        applier: &'a dyn Applier,
        command: &'a Command,
    ) -> Option<BoxFuture<'a, Result<Value, String>>> {
        if let Command::PreviewSplash { source_available } = command {
            Some(Box::pin(async move {
                let reply = applier
                    .decoder(json!({"action":"show_splash","source_available":source_available}))
                    .await?;
                if reply["ok"] != true {
                    return Err(reply.to_string());
                }
                Ok(reply)
            }))
        } else {
            None
        }
    }
    fn apply<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
        _: &'a Plan,
    ) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            deliver(applier, &state.document.display, state.revision)
                .await
                .map(|v| json!({"ok":true,"display":v}))
        })
    }
    fn replay<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            deliver(applier, &state.document.display, state.revision)
                .await
                .map(|_| ())
        })
    }

    fn rollback<'a>(
        &self,
        applier: &'a ProductionApplier,
        state: &'a StateFile,
        _: &'a Plan,
    ) -> BoxFuture<'a, Result<(), String>> {
        // A candidate can have reached the decoder before persistence failed.
        // Restore its previous payload at the same accepted revision watermark.
        Box::pin(async move {
            deliver(applier, &state.document.display, state.revision)
                .await
                .map(|_| ())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ipc::Decoder, plan};
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
        time::Duration,
    };
    #[derive(Debug, Default)]
    struct DecoderFake {
        replies: Mutex<VecDeque<Result<Value, String>>>,
        calls: Mutex<Vec<Value>>,
        revision: Mutex<Option<u64>>,
    }
    impl Decoder for DecoderFake {
        fn request(&self, command: Value) -> BoxFuture<'_, Result<Value, String>> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(command.clone());
                if command["action"] == "status" {
                    return Ok(json!({"revision":*self.revision.lock().unwrap()}));
                }
                let reply = self.replies.lock().unwrap().pop_front().unwrap_or_else(|| {
                    let revision = self.revision.lock().unwrap();
                    if revision.is_some_and(|r| Some(r) > command["revision"].as_u64())
                        && command["reset"] != true
                    {
                        Ok(json!({"ok":false,"errors":[crate::follower::OLDER_REVISION]}))
                    } else {
                        Ok(json!({"ok":true,"revision":command["revision"]}))
                    }
                });
                if let Ok(value) = &reply {
                    if value["ok"] == true {
                        *self.revision.lock().unwrap() = value["revision"].as_u64();
                    }
                }
                reply
            })
        }
    }
    fn applier(decoder: Arc<DecoderFake>) -> ProductionApplier {
        ProductionApplier {
            memory: Default::default(),
            tessera: ndimon_tessera::Client::default(),
            finder: decoder.clone(),
            decoder,
            privd: ndimon_privd_proto::client::Client::new("/unused", Duration::from_secs(1)),
        }
    }
    #[test]
    fn display_planner_is_non_disruptive_and_noops_have_no_actions() {
        let previous = DesiredState::default();
        for field in ["splash", "osd", "logo", "noop"] {
            let mut next = previous.clone();
            match field {
                "splash" => next.display.splash.text_idle = "Idle".into(),
                "osd" => next.display.osd.enabled = true,
                "logo" => {
                    next.display.splash.logo = Some(AssetRef::new("a".repeat(64), "image/png"))
                }
                _ => (),
            }
            let command = Command::SetDisplay(next.display.clone());
            let plan = plan::plan(&previous, &next, &command).unwrap();
            assert_eq!(plan.impact.scope, ImpactScope::None);
            assert!(!plan.impact.needs_confirmation());
            assert!(plan.impact.restarts.is_empty());
            assert!(plan.impact.interruptions.is_empty());
            if field == "noop" {
                assert!(plan.actions.is_empty());
                assert!(plan.paths.is_empty());
            } else {
                assert_eq!(plan.domains, vec![Domain::Display]);
                assert_eq!(
                    plan.actions,
                    vec![plan::Action::Display(Action::DisplayApplyConfig)]
                );
                assert!(plan.paths.iter().all(|p| p.starts_with("display.")));
            }
        }
    }
    #[tokio::test]
    async fn apply_retry_replay_and_rollback_send_only_complete_display() {
        let decoder = Arc::new(DecoderFake::default());
        decoder
            .replies
            .lock()
            .unwrap()
            .push_back(Err("timeout".into()));
        let applier = applier(decoder.clone());
        let mut state = StateFile::new(DesiredState::default(), Default::default());
        state.revision = 7;
        state.document.revision = 7;
        let plan = plan::plan(
            &DesiredState::default(),
            &state.document,
            &Command::SetDisplay(state.document.display.clone()),
        )
        .unwrap();
        Module.apply(&applier, &state, &plan).await.unwrap();
        Module.replay(&applier, &state).await.unwrap();
        *decoder.revision.lock().unwrap() = Some(8);
        Module.rollback(&applier, &state, &plan).await.unwrap();
        let all = decoder.calls.lock().unwrap();
        // One request per delivery; the follower is asked for its revision only
        // after it refused the state's (the rollback found it one ahead).
        assert_eq!(all.iter().filter(|c| c["action"] == "status").count(), 1);
        let calls: Vec<_> = all
            .iter()
            .filter(|c| c["action"] == "apply_config")
            .collect();
        assert_eq!(calls.len(), 5);
        assert_eq!(calls[0], calls[1]);
        for call in calls.iter() {
            assert_eq!(call["action"], "apply_config");
            assert_eq!(call["config"], json!({"display":state.document.display}));
        }
        assert_eq!(calls[2]["revision"], 7);
        assert_eq!(calls[3]["revision"], 7);
        assert_eq!(calls[4]["revision"], 8);
        assert!(calls.iter().all(|c| c.get("reset").is_none()));
    }
    #[tokio::test]
    async fn boot_recovery_replays_previous_payload_at_reserved_watermark() {
        let decoder = Arc::new(DecoderFake::default());
        *decoder.revision.lock().unwrap() = Some(8);
        let applier = applier(decoder.clone());
        let mut state = StateFile::new(DesiredState::default(), Default::default());
        state.revision = 7;
        state.document.revision = 7;
        Module.replay(&applier, &state).await.unwrap();
        let calls = decoder.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0]["revision"], 7);
        assert_eq!(calls[1]["action"], "status");
        assert_eq!(calls[2]["revision"], 8);
        assert_eq!(
            calls[2]["config"],
            json!({"display":state.document.display})
        );
        assert_eq!(state.revision, 7);
        assert_eq!(state.document.revision, 7);
    }
    #[tokio::test]
    async fn decoder_refusals_wrong_revision_and_restart_required_fail_closed() {
        for reply in [
            json!({"ok":false,"errors":["display: invalid"]}),
            json!({"ok":true,"revision":2}),
            json!({"ok":true,"revision":1,"restart_required":true}),
            json!({"ok":true,"revision":1,"refused":["display"]}),
        ] {
            let decoder = Arc::new(DecoderFake::default());
            decoder.replies.lock().unwrap().push_back(Ok(reply));
            assert!(
                deliver(&applier(decoder), &Display::default(), 1)
                    .await
                    .is_err()
            );
        }
    }
}
