//! Fake privileged lifecycle with the real durable StateAuthority commit.
use ndimon_privd_proto::NetworkCall;
use ndimon_state::{network_authority::StateAuthority, persist::Persistence};
use ndimon_types::*;
use serde_json::{Value, json};
use std::{fs, io::Write, path::PathBuf};

#[derive(Debug)]
pub struct Engine {
    root: PathBuf,
    pending: Option<(PendingNetwork, NetworkProfiles)>,
    sequence: u64,
    handoff: Option<String>,
    plugin_polls: std::collections::BTreeMap<String, u8>,
}
impl Engine {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            pending: None,
            sequence: 0,
            handoff: None,
            plugin_polls: Default::default(),
        }
    }
    pub fn handle(&mut self, call: NetworkCall) -> Value {
        match self.execute(call) {
            Ok(value) => json!({"ok":value}),
            Err(error) => json!({"err":error}),
        }
    }
    fn execute(&mut self, call: NetworkCall) -> Result<Value, ApiError> {
        use ndimon_state::network_authority::StateAuthority as Authority;
        let store = Persistence {
            directory: self.root.join("var/lib/ndimon/state"),
        };
        if store.directory.join("state.pending").exists() {
            return Err(ApiError::Locked {
                field: "network".into(),
                by: Some("state_transaction".into()),
            });
        }
        let state: ndimon_state::persist::StateFile = serde_json::from_slice(
            &fs::read(store.directory.join("state.json")).map_err(internal)?,
        )
        .map_err(internal)?;
        state.check().map_err(internal)?;
        let audit = match &call {
            NetworkCall::Stage { .. } => "stage",
            NetworkCall::Confirm { .. } => "keep",
            NetworkCall::Revert { .. } => "revert",
            _ => "read",
        };
        if audit != "read" {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.root.join("network-requests.jsonl"))
                .map_err(internal)?;
            writeln!(
                file,
                "{}",
                json!({"operation":audit,"revision":state.revision})
            )
            .map_err(internal)?;
        }
        match call {
            NetworkCall::Status | NetworkCall::Observe => Ok(
                json!({"revision":state.revision,"domain_revision":state.domain_revision(Domain::Network),"profiles":state.document.network,"networks":state.document.network.networks.iter().map(|n| json!({"id":n.id,"interface":state.document.network.interface,"addresses":[[if n.address.is_empty(){"192.0.2.2"}else{&n.address},24]],"used_by":["management","ndi","audio"]})).collect::<Vec<_>>(),"pending":self.pending.as_ref().map(|p| &p.0),"drift":[],"last_phase":"reverted","writable":true,"quiesced":self.handoff.is_some(),"externally_managed":false}),
            ),
            NetworkCall::Preview { request } => Ok(
                json!({"unchanged":request.profiles == state.document.network,"affected_roles":["management","ndi","audio"],"removed_networks":[],"interruptions":["ndimon-r.service","ndimon-finder.service","tesserad.service"],"writable":true}),
            ),
            NetworkCall::Stage { request, .. } => {
                if self.handoff.is_some() {
                    return Err(ApiError::Locked {
                        field: "network".into(),
                        by: Some("handoff".into()),
                    });
                }
                if self.pending.is_some() {
                    return Err(ApiError::Locked {
                        field: "network".into(),
                        by: Some("pending_revision".into()),
                    });
                }
                if request.expect_revision != state.revision {
                    return Err(ApiError::RevisionConflict {
                        current_revision: state.revision,
                    });
                }
                if request.profiles == state.document.network {
                    return Ok(json!({"unchanged":true}));
                }
                self.sequence += 1;
                let mut pending = PendingNetwork::new(
                    state.revision + 1,
                    "2099-01-01T00:00:00Z",
                    NetworkPhase::Applied,
                );
                pending.job_id = format!("{:064x}", self.sequence);
                pending.management_address = Some("192.0.2.2".parse().unwrap());
                pending.management_port = 80;
                pending.remaining_seconds = 60;
                self.pending = Some((pending.clone(), request.profiles));
                Ok(json!({"unchanged":false,"pending":pending,"token":"c".repeat(64)}))
            }
            NetworkCall::Pending { token } => {
                self.check(&token)?;
                Ok(json!(self.pending.as_ref().unwrap().0))
            }
            NetworkCall::Confirm { token, .. } => {
                self.check(&token)?;
                let (pending, profiles) = self.pending.as_ref().unwrap();
                let previous = Authority::decision(&state);
                let mut replacement = previous.clone();
                replacement.revision += 1;
                replacement.network_commit_revision = Some(replacement.revision);
                replacement.transaction = Some(pending.job_id.clone());
                replacement.profiles = profiles.clone();
                commit(&StateAuthority::new(store), &previous, &replacement)?;
                self.pending = None;
                Ok(json!({"revision":replacement.revision}))
            }
            NetworkCall::Revert { token } => {
                self.check(&token)?;
                self.pending = None;
                Ok(json!({"ok":true}))
            }
            NetworkCall::Rollback | NetworkCall::AcceptCurrent => {
                self.pending = None;
                Ok(json!({"ok":true}))
            }
            NetworkCall::CheckAddress { .. } => Ok(json!({"ok":true,"duplicate":false})),
            NetworkCall::HandoffPrepare => {
                if self.pending.is_some() || self.handoff.is_some() {
                    return Err(ApiError::Locked {
                        field: "network".into(),
                        by: Some("handoff".into()),
                    });
                }
                self.sequence += 1;
                let id = format!("{:064x}", self.sequence);
                self.handoff = Some(id.clone());
                Ok(json!({"id":id,"product":"ndimon","revision":state.revision}))
            }
            NetworkCall::HandoffCancel { id } => {
                if self.handoff.as_deref() != Some(id.as_str()) {
                    return Err(ApiError::NotPending);
                }
                self.handoff = None;
                Ok(json!({"ok":true}))
            }
            NetworkCall::HandoffImport { source } => {
                let command = Command::CommitNetworkHandoff {
                    source: source.clone(),
                };
                let context = vec![format!("fixture-peer:{}:{}", source.id, state.revision)];
                let mut impact = Impact::new(ImpactScope::Network);
                impact
                    .lines
                    .push("Import the reviewed peer network intent.".into());
                let impact = impact.sealed(&command, &context).map_err(internal)?;
                Ok(
                    json!({"source":source,"impact":impact,"impact_context":context,"write_files":["/etc/ndimon-network-interfaces.json"]}),
                )
            }
            NetworkCall::HandoffCommit { .. } => Err(ApiError::Unsupported {
                what: "fixture handoff commit".into(),
            }),
        }
    }
    pub fn plugin(
        &mut self,
        request: ndimon_privd_proto::Request,
    ) -> Result<ndimon_privd_proto::ActionOutput, ndimon_privd_proto::Error> {
        use ndimon_privd_proto::{ActionOutput, Error, PluginJob, ReplyResult, Request};
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("plugin-requests.jsonl"))
            .map_err(|e| Error::failed("fixture audit", e))?;
        writeln!(
            file,
            "{}",
            serde_json::to_string(&request).map_err(|e| Error::failed("fixture audit", e))?
        )
        .map_err(|e| Error::failed("fixture audit", e))?;
        match request {
            Request::PluginInspect { .. } => Ok(ActionOutput { stdout: json!({"name":"hello","version":"1.0.0","services":[],"priv_commands":["enable","disable"]}).to_string(), ..Default::default() }),
            Request::PluginInstall { .. } | Request::PluginUninstall { .. } | Request::PluginExec { .. } => {
                self.sequence += 1;
                let id = format!("ndimon-plugin-hello-{}.service", self.sequence);
                self.plugin_polls.insert(id.clone(), 0);
                Ok(ActionOutput { job: Some(PluginJob::try_from(id).map_err(|e| Error::failed("fixture job", e))?), ..Default::default() })
            }
            Request::PluginJobStatus { job } => {
                let polls = self.plugin_polls.get_mut(job.as_str()).ok_or(Error::PermissionDenied)?;
                *polls = polls.saturating_add(1);
                if *polls < 3 {
                    return Ok(ActionOutput { stdout: "active".into(), ..Default::default() });
                }
                let result = ReplyResult::Ok(ActionOutput { stdout: "fixture lifecycle hook completed\n".into(), ..Default::default() });
                Ok(ActionOutput { stdout: "inactive".into(), data: Some(json!({"result":result})), ..Default::default() })
            }
            Request::PluginServiceControl { .. } => Ok(ActionOutput::default()),
            _ => Err(Error::PermissionDenied),
        }
    }
    fn check(&self, token: &str) -> Result<(), ApiError> {
        if self.pending.is_none() {
            return Err(ApiError::NotPending);
        }
        if token != "c".repeat(64) {
            return Err(ApiError::TokenMismatch);
        }
        Ok(())
    }
}
fn internal(error: impl ToString) -> ApiError {
    ApiError::Internal {
        message: error.to_string(),
    }
}

// Keep the engine dependency behind the fixture's already non-default feature.
fn commit(
    authority: &StateAuthority,
    previous: &ndimon_netengine::Commit,
    next: &ndimon_netengine::Commit,
) -> Result<(), ApiError> {
    use ndimon_netengine::Authority;
    authority.compare_and_commit(previous, next)
}
