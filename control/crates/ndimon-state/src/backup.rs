//! Cross-domain backup operations. No domain is assigned ownership of a restore.
pub mod coordinator;
pub mod difference;
pub mod environment;
pub mod legacy;
pub mod plugins;
pub mod review;
pub mod schema;
mod semantic;
pub mod storage;
use crate::{domains, persist::StateFile, plan};
use ndimon_render::Ownership;
use ndimon_types::*;
use serde_json::{Value, json};
use std::path::Path;

pub fn handles(command: &Command) -> bool {
    matches!(
        command,
        Command::ImportBackup {
            scope: RestoreScope::All | RestoreScope::Settings | RestoreScope::Sources,
            ..
        }
    )
}
pub fn root(directory: &Path) -> &Path {
    if directory.file_name().is_some_and(|s| s == "state") {
        directory.parent().unwrap_or(directory)
    } else {
        directory
    }
}
pub fn reduce(state: &mut DesiredState, command: &Command) -> Result<(), ApiError> {
    let Command::ImportBackup { document, scope } = command else {
        return Err(ApiError::Unsupported {
            what: "undo must be resolved against the backup store".into(),
        });
    };
    ndimon_types::validate(&document.document).map_err(ApiError::invalid)?;
    for r in domains::REGISTRY {
        // Network has its own confirmed transaction, never an ordinary restore.
        let selected = match scope {
            RestoreScope::All => r.domain != Domain::Network,
            RestoreScope::Settings => !matches!(r.domain, Domain::Network | Domain::Plugins),
            RestoreScope::Sources => {
                matches!(r.domain, Domain::Presets | Domain::Outputs | Domain::Ndi)
            }
            _ => false,
        };
        if selected && r.ownership() == Ownership::Native {
            (r.copy)(state, &document.document);
        }
    }
    Ok(())
}
pub fn query(state: &StateFile, root: &Path, query: &Query) -> Option<Result<Value, ApiError>> {
    match query {
        Query::BackupSnapshots => Some(Ok(
            json!({"snapshots":storage::snapshots(&root.join("backups"))}),
        )),
        Query::ExportBackup { .. } => Some({
            let mut doc = BackupDocument::new(state.document.clone());
            if let Some(asset) = &state.document.display.splash.logo {
                doc.assets.push(asset.clone());
            }
            serde_json::to_value(doc).map_err(plan::internal)
        }),
        Query::InspectBackup { document } => Some((|| {
            let command = Command::ImportBackup {
                document: document.clone(),
                scope: RestoreScope::All,
            };
            let mut next = state.document.clone();
            reduce(&mut next, &command)?;
            ndimon_types::validate(&next).map_err(ApiError::invalid)?;
            let p = plan::plan(&state.document, &next, &command)?;
            Ok(json!({"revision":state.revision,"paths":p.paths,"impact":p.impact}))
        })()),
        _ => None,
    }
}

/// Merge restore-wide legacy service impact with the native domain contributions.
pub fn impact(command: &Command) -> Impact {
    let mut impact = Impact::default();
    if let Command::ImportBackup { document, .. } = command {
        if document.legacy_sections.iter().any(|s| {
            matches!(
                s.as_str(),
                "outputs" | "discovery" | "transport" | "display" | "device" | "identity"
            )
        }) {
            impact.scope = ImpactScope::Service;
            impact.restarts.push(Service::Decoder);
            impact.interruptions.push(Interruption::new(
                Domain::Outputs,
                "decoder video/audio",
                Some(3000),
            ));
            impact.lines.push(
                "Restart the decoder to restore followed settings; video and audio pause briefly"
                    .into(),
            );
        }
        if document.legacy_sections.iter().any(|s| s == "plugins") {
            impact.scope = impact.scope.max(ImpactScope::Service);
            impact.interruptions.push(Interruption::new(
                Domain::Plugins,
                "plugin services and audio routing",
                None,
            ));
            impact.lines.push("Run plugin restore hooks, which can restart plugin services and replace audio routing".into());
        }
    }
    if let Command::ImportBackup { document, .. } = command {
        if document.legacy_sections.iter().any(|s| s == "network") {
            let network = crate::domains::network::impact(
                &json!({"interruptions":["ndimon-r.service","ndimon-finder.service","tesserad.service"]}),
            );
            impact.scope = impact.scope.max(network.scope);
            for service in network.restarts {
                if !impact.restarts.contains(&service) {
                    impact.restarts.push(service);
                }
            }
            for interruption in network.interruptions {
                if !impact.interruptions.contains(&interruption) {
                    impact.interruptions.push(interruption);
                }
            }
            impact.lines.extend(network.lines);
        }
    }
    impact
}

pub fn context(previous: &DesiredState, command: &Command) -> Vec<String> {
    if handles(command) {
        vec![format!("backup_revision_context:{}", previous.revision)]
    } else {
        Vec::new()
    }
}
