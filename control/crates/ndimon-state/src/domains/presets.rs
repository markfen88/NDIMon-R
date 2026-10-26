//! Source preset mutations have no runtime effects; recall belongs to outputs.
use super::DomainModule;
use crate::plan::{Action as DomainAction, DomainPlan};
use ndimon_render::LegacyFiles;
use ndimon_types::*;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    PresetsRender,
}
#[derive(Debug)]
pub struct Module;
impl DomainModule for Module {
    fn migrate(&self, state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String> {
        if state.presets.values().all(|p| p.position.is_some()) {
            return Ok(());
        }
        let mut imported = DesiredState::default();
        ndimon_render::presets::import_json(&mut imported, files)?;
        for (name, preset) in &mut state.presets {
            if preset.position.is_none() {
                preset.position = imported.presets.get(name).and_then(|p| p.position);
            }
        }
        Ok(())
    }
    fn import(&self, state: &mut DesiredState, files: &LegacyFiles) -> Result<(), String> {
        ndimon_render::presets::import_json(state, files)
    }
    fn reduce(&self, state: &mut DesiredState, command: &Command) -> Result<(), ApiError> {
        match command {
            Command::SavePreset { name, preset } => {
                let mut preset = preset.clone();
                preset.position = state
                    .presets
                    .get(name)
                    .and_then(|p| p.position)
                    .or_else(|| {
                        Some(
                            state
                                .presets
                                .values()
                                .filter_map(|p| p.position)
                                .max()
                                .map_or(0, |p| p.saturating_add(1)),
                        )
                    });
                state.presets.insert(name.clone(), preset);
            }
            Command::DeletePreset { name } => {
                state.presets.remove(name);
            }
            _ => {
                return Err(ApiError::Unsupported {
                    what: "not a preset mutation".into(),
                });
            }
        }
        Ok(())
    }
    fn plan(
        &self,
        _previous: &DesiredState,
        _next: &DesiredState,
        _command: &Command,
    ) -> Result<DomainPlan, ApiError> {
        Ok(DomainPlan {
            actions: vec![DomainAction::Presets(Action::PresetsRender)],
            impact: Impact::default(),
        })
    }
}
