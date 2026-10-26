//! Fixed privileged network operations; the legacy request shapes stay intact.
use crate::Host;
use ndimon_netengine::{Engine, EngineConfig, Host as NetworkHost, Pending, Status};
use ndimon_privd_proto::{ActionOutput, Error, Request};
use ndimon_state::{network_authority::StateAuthority, persist::Persistence};
use ndimon_types::{ApiError, NetworkBackend, NetworkProfiles};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};

pub const CERTIFICATION: &str = "/etc/ndimon-network-certified-backends.json";
/// The backends the operator lists as validated (the packages ship all four as a
/// conffile) and why the list is empty, if it is. A missing, unreadable or
/// malformed file allows nothing: the engine still reads and previews, and refuses
/// every change with `backend_not_certified`. It must never fail the request, or
/// the boot job that shares this code and, with it, the services that need it.
pub fn certification(
    root: &ndimon_netengine::store::Root,
) -> (BTreeSet<NetworkBackend>, Option<String>) {
    let refused = |reason: &str| {
        (
            BTreeSet::new(),
            Some(format!(
                "{CERTIFICATION} {reason}; network changes are refused"
            )),
        )
    };
    match root.text(CERTIFICATION) {
        Ok(Some(text)) => match serde_json::from_str::<BTreeSet<NetworkBackend>>(&text) {
            Ok(backends) if backends.is_empty() => refused("lists no backend"),
            Ok(backends) => (backends, None),
            Err(_) => refused(
                "is not a JSON array of backend names (networkd, netplan, networkmanager, ifupdown)",
            ),
        },
        Ok(None) => refused("is missing"),
        Err(_) => refused(
            "is unreadable (it must be a regular file owned by root that others cannot write)",
        ),
    }
}
/// Say why changes are refused once per distinct reason, not on every request.
fn report(note: Option<&str>) {
    static LAST: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let mut last = LAST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if last.as_deref() != note {
        if let Some(note) = note {
            tracing::warn!("{note}");
        }
        *last = note.map(str::to_owned);
    }
}
fn error(e: ApiError) -> Error {
    Error::failed("network", e)
}
fn output(value: Value) -> Result<ActionOutput, Error> {
    Ok(ActionOutput {
        stdout: serde_json::to_string(&value).map_err(|e| Error::failed("network encode", e))?,
        data: Some(value),
        ..Default::default()
    })
}
#[derive(Debug)]
struct HostRunner(crate::process::SharedRunner);
impl ndimon_netengine::host::CommandRunner for HostRunner {
    fn run(
        &self,
        command: &ndimon_netengine::host::CommandSpec,
    ) -> ndimon_netengine::Result<ndimon_netengine::host::CommandOutput> {
        self.0.network(command)
    }
}
pub fn engine(host: &Host) -> Result<Engine, Error> {
    let root = ndimon_netengine::store::Root::new(host.root.0.clone()).map_err(error)?;
    let mut config = EngineConfig::default();
    let (backends, note) = certification(&root);
    config.certified_backends = backends;
    report(note.as_deref());
    let mut network = NetworkHost::default();
    network.root = root;
    network.runner = Arc::new(HostRunner(host.runner.clone()));
    #[cfg(feature = "harness")]
    {
        config.selected_interface = Some("end0".into());
    }
    let directory = host.root.path("/var/lib/ndimon/state");
    // Initialization/import belongs to the ordered state-import unit, never to
    // a socket request. The engine must not invent another authoritative store.
    if !directory.is_dir() {
        return Err(Error::failed(
            "network authority",
            "state import has not run",
        ));
    }
    let authority = StateAuthority::new(Persistence { directory });
    Ok(Engine::new(network, Arc::new(authority), config))
}
fn serialized_job(mut operation: impl FnMut() -> Result<(), ApiError>) -> Result<(), Error> {
    let start = std::time::Instant::now();
    loop {
        match operation() {
            Err(ApiError::Locked { by: Some(by), .. })
                if matches!(
                    by.as_str(),
                    "network_operation" | "legacy_network_writer" | "state_transaction"
                ) && start.elapsed() < std::time::Duration::from_secs(10) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            result => return result.map_err(error),
        }
    }
}
/// How long an operator's request waits for another network request to let go
/// of the network lock. A status poll holds it for milliseconds; an apply holds
/// it for the whole apply, and a Keep that arrives then is refused as before.
const OPERATOR_WAIT: std::time::Duration = std::time::Duration::from_secs(3);
/// Run an operator's network request, retrying while only a concurrent network
/// request holds the lock. Every such request takes the lock before it reads or
/// changes anything, so a retry repeats nothing, and the pending change's
/// deadline is still checked under the lock: waiting does not stretch the window
/// (VV-3 W3: one Keep in 25 lost to the page's own status poll).
fn waiting<T>(
    limit: std::time::Duration,
    mut operation: impl FnMut() -> Result<T, ApiError>,
) -> Result<T, ApiError> {
    let start = std::time::Instant::now();
    loop {
        match operation() {
            Err(ApiError::Locked { by: Some(by), .. })
                if by == "network_operation" && start.elapsed() < limit =>
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            result => return result,
        }
    }
}
pub fn job(host: &Host, args: &[String]) -> Result<(), Error> {
    match args {
        [action] if action == "boot" => {
            let engine = engine(host)?;
            serialized_job(|| {
                if let Some(note) = engine.boot_report()? {
                    // Not a failed boot (see `Engine::boot_report`): say so loudly.
                    tracing::error!(
                        %note,
                        "network boot left a problem: the stack starts anyway and network changes stay locked until it is resolved (a refused 1.3.0 store: repair it, dpkg --configure -a or a reboot retries; a recovery that failed: it retries every ten seconds, or accept the current network on the Network page)"
                    );
                }
                Ok(())
            })
        }
        [action] if action == "queue" => queue_stack(host),
        [action, id]
            if matches!(action.as_str(), "apply" | "recover")
                && ndimon_netengine::store::valid_id(id) =>
        {
            // A Rust journal must always be recoverable, including after a
            // fallback flag change. Jobs never invoke a legacy helper.
            let engine = engine(host)?;
            serialized_job(|| {
                if action == "apply" {
                    engine.run_apply_job(id)
                } else {
                    engine.run_recovery(id)
                }
            })
        }
        _ => Err(Error::invalid(
            "network job",
            "expected boot, queue, apply ID or recover ID; ID is 64 lowercase hex",
        )),
    }
}
/// What a successful boot job queues again (`ndimon-net-boot.service`): the
/// control plane, the finder, the decoder, and `tesserad` where it is
/// installed. `systemctl start` of a unit that is not installed says so in the
/// journal at every boot, and `--quiet` does not stop it, so the name is only
/// given where the unit file exists (NI-17).
fn queue_stack(host: &Host) -> Result<(), Error> {
    let mut args = vec![
        "--no-block",
        "start",
        "ndimon-control.service",
        "ndimon-finder.service",
        "ndimon-r.service",
    ];
    if ["/etc", "/usr/lib", "/lib"].iter().any(|prefix| {
        host.root
            .path(&format!("{prefix}/systemd/system/tesserad.service"))
            .exists()
    }) {
        args.push("tesserad.service");
    }
    crate::system::systemctl(host, &args).map(|_| ())
}
/// Tessera's network files follow the committed profiles. Plugin lifecycle jobs
/// call this while the control plane may still be finishing the transaction
/// that started them (its install or enable changes the plugin document): wait
/// for its decision lock like every independent job instead of failing.
pub fn sync_tessera(host: &Host) -> Result<(), Error> {
    let engine = engine(host)?;
    engine.reconcile_handoff().map_err(error)?;
    serialized_job(|| engine.sync_tessera())
}
pub fn legacy_profiles(profiles: &NetworkProfiles, revision: u64) -> Value {
    json!({"version":1,"revision":revision,"interface":profiles.interface,"backend":profiles.backend,
        "networks":profiles.networks,"management":profiles.roles.management,"ndi":profiles.roles.ndi,"tessera":profiles.roles.audio})
}
pub fn legacy_pending(pending: &Pending) -> Value {
    json!({"phase":pending.phase,"address":pending.management_address.map(|a| a.to_string()).unwrap_or_default(),
        "profiles":true,"remaining_seconds":pending.remaining_seconds,"confirm_seconds":60,
        "deadline":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs().saturating_add(pending.remaining_seconds)})
}
pub fn legacy_status(status: &Status) -> Value {
    let rows: Vec<_> = status
        .profiles
        .networks
        .iter()
        .map(|row| {
            let observed = status.networks.iter().find(|n| n.id == row.id);
            let (address, prefix) = observed
                .and_then(|n| n.addresses.first())
                .map(|(a, p)| (a.to_string(), *p))
                .unwrap_or_default();
            let mut value = serde_json::to_value(row).unwrap_or(Value::Null);
            value["interface"] = json!(ndimon_netengine::validation::ifname(&status.profiles, row));
            value["live_address"] = json!(address);
            value["live_prefix"] = json!(prefix);
            value["ready"] = json!(!address.is_empty());
            value["used_by"] = json!(
                observed
                    .map(|n| n
                        .used_by
                        .iter()
                        .map(|s| if s == "audio" { "tessera" } else { s.as_str() })
                        .collect::<Vec<_>>())
                    .unwrap_or_default()
            );
            value
        })
        .collect();
    json!({"ok":true,"settings":legacy_profiles(&status.profiles,status.revision),"networks":rows,
        "pending":status.pending.as_ref().map(legacy_pending)})
}
pub fn handle(host: &Host, request: Request) -> Result<ActionOutput, Error> {
    if let Request::NetworkNative { call } = request {
        // Preserve classified engine errors across the unchanged ActionOutput
        // envelope; legacy errors retain their previous transport behavior.
        return output(match native(host, call) {
            Ok(value) => json!({"ok":value}),
            Err(error) => json!({"err":error}),
        });
    }
    let engine = engine(host)?;
    engine.reconcile_handoff().map_err(error)?;
    use Request::*;
    let stage = matches!(&request, NetworkProfilesStage { .. });
    let value = match request {
        NetworkProfilesStatus => legacy_status(&engine.status().map_err(error)?),
        NetworkStatus | NetworkVlanList => {
            let s = engine.status().map_err(error)?;
            let mut v = json!({"ok":true,"applicable":s.writable,"reason":if s.writable { "" } else { "backend_not_certified" },
                "interface":s.profiles.interface,"backend":s.profiles.backend,"vlan_capable":s.writable,"vlan_reason":"","vlan_error":"",
                "vlans":s.profiles.networks.iter().filter_map(|n| n.vlan_id.map(|id| json!({"id":id,"mode":n.mode,"address":n.address,"prefix":n.prefix,"gateway":n.gateway,"allow_gateway":n.id == s.profiles.roles.management,"managed":true}))).collect::<Vec<_>>()});
            let profiles = legacy_status(&s);
            if let Some(row) = profiles["networks"]
                .as_array()
                .and_then(|rows| rows.iter().find(|n| n["id"] == s.profiles.roles.management))
            {
                for key in [
                    "mode",
                    "address",
                    "prefix",
                    "gateway",
                    "dns",
                    "live_address",
                    "live_prefix",
                ] {
                    v[key] = row[key].clone();
                }
            }
            v
        }
        NetworkCheckAddress { address } => {
            json!({"ok":true,"duplicate":engine.check_address(address).map_err(error)?})
        }
        NetworkProfilesPreview { profiles } | NetworkProfilesStage { profiles } => {
            let (revision, candidate) = ndimon_netengine::validation::import_legacy(
                &serde_json::to_vec(&profiles).map_err(|e| Error::failed("network encode", e))?,
            )
            .map_err(error)?;
            if stage {
                let s = engine.stage(&candidate, revision, None).map_err(error)?;
                if let Some(p) = &s.pending {
                    let mut v = legacy_pending(p);
                    v["ok"] = json!(true);
                    v["token"] = json!(s.token.as_ref().map(|t| t.expose()));
                    v
                } else {
                    json!({"ok":true,"unchanged":true})
                }
            } else {
                let p = engine.preview(&candidate, revision).map_err(error)?;
                json!({"ok":true,"settings":legacy_profiles(&p.profiles,revision),"unchanged":p.unchanged,
                    "affected":p.affected_roles.iter().map(|s| if s == "audio" {"tessera"} else {s}).collect::<Vec<_>>(),
                    "removed":p.removed_networks,"requires_confirmation":true,"confirm_seconds":60,
                    "confirm_policy":candidate.confirm_policy,
                    "message":ndimon_netengine::validation::confirm_message(candidate.confirm_policy)})
            }
        }
        NetworkConfirm { token } => {
            let c = engine.confirm(token.as_str(), None).map_err(error)?;
            json!({"ok":true,"revision":c.revision})
        }
        NetworkProfilesConfirm {
            token,
            local_address,
        } => {
            let c = engine
                .confirm(token.as_str(), Some(local_address.into()))
                .map_err(error)?;
            json!({"ok":true,"revision":c.revision})
        }
        NetworkProfilesRevert { token } => {
            engine.revert(token.as_str()).map_err(error)?;
            json!({"ok":true})
        }
        NetworkRollback | NetworkProfilesRollback => {
            engine.rollback().map_err(error)?;
            json!({"ok":true})
        }
        NetworkProfilesBoot => {
            engine.boot().map_err(error)?;
            json!({"ok":true})
        }
        NetworkProfilesSyncTessera => {
            engine.sync_tessera().map_err(error)?;
            json!({"ok":true})
        }
        NetworkApply { .. } | NetworkVlanApply { .. } | NetworkVlanRemove { .. } => {
            return Err(Error::Unsupported {
                what: "use the complete protected network profile transaction".into(),
            });
        }
        _ => return Err(Error::invalid("network", "not a network request")),
    };
    output(value)
}

fn native(host: &Host, call: ndimon_privd_proto::NetworkCall) -> Result<Value, ApiError> {
    use ndimon_privd_proto::NetworkCall;
    let transport = |e: Error| ApiError::ApplyFailed {
        domain: ndimon_types::Domain::Network,
        step: "privd".into(),
        reason: e.to_string(),
    };
    let engine = engine(host).map_err(transport)?;
    engine.reconcile_handoff()?;
    fn encode(value: impl serde::Serialize) -> Result<Value, ApiError> {
        serde_json::to_value(value).map_err(|_| ApiError::Internal {
            message: "network encode".into(),
        })
    }
    match call {
        NetworkCall::Status => encode(engine.status()?),
        NetworkCall::Observe => encode(engine.observe()?),
        NetworkCall::HandoffPrepare => encode(engine.prepare_handoff()?),
        NetworkCall::HandoffCancel { id } => {
            engine.cancel_handoff(&id)?;
            Ok(json!({"ok":true}))
        }
        NetworkCall::HandoffImport { source } => encode(engine.import_handoff(&source)?),
        NetworkCall::HandoffCommit {
            source,
            expect_impact,
            session,
        } => encode(engine.commit_handoff(&source, &expect_impact, session.map(Into::into))?),
        NetworkCall::CheckAddress { address } => {
            let address = address.parse().map_err(|_| {
                ApiError::invalid(vec![ndimon_types::FieldError::new(
                    "address",
                    "invalid_ipv4_address",
                )])
            })?;
            Ok(json!({"ok":true,"duplicate":engine.check_address(address)?}))
        }
        NetworkCall::Preview { request } => {
            ndimon_netengine::validation::parse(&serde_json::to_vec(&request.profiles).map_err(
                |_| ApiError::Internal {
                    message: "network encode".into(),
                },
            )?)?;
            if request.version != 1 {
                return Err(ApiError::invalid(vec![ndimon_types::FieldError::new(
                    "version",
                    "invalid_version_revision",
                )]));
            }
            encode(waiting(OPERATOR_WAIT, || {
                engine.preview(&request.profiles, request.expect_revision)
            })?)
        }
        NetworkCall::Stage { request, session } => {
            ndimon_netengine::validation::parse(&serde_json::to_vec(&request.profiles).map_err(
                |_| ApiError::Internal {
                    message: "network encode".into(),
                },
            )?)?;
            if request.version != 1 {
                return Err(ApiError::invalid(vec![ndimon_types::FieldError::new(
                    "version",
                    "invalid_version_revision",
                )]));
            }
            serde_json::to_value(engine.stage(
                &request.profiles,
                request.expect_revision,
                session.map(Into::into),
            )?)
            .map_err(|_| ApiError::Internal {
                message: "network encode".into(),
            })
        }
        NetworkCall::Pending { token } => {
            serde_json::to_value(engine.pending(&token)?).map_err(|_| ApiError::Internal {
                message: "network encode".into(),
            })
        }
        NetworkCall::Confirm {
            token,
            local_address,
        } => serde_json::to_value(waiting(OPERATOR_WAIT, || {
            engine.confirm(&token, local_address)
        })?)
        .map_err(|_| ApiError::Internal {
            message: "network encode".into(),
        }),
        NetworkCall::Revert { token } => {
            waiting(OPERATOR_WAIT, || engine.revert(&token))?;
            Ok(json!({"ok":true}))
        }
        NetworkCall::Rollback => {
            waiting(OPERATOR_WAIT, || engine.rollback())?;
            Ok(json!({"ok":true}))
        }
        NetworkCall::AcceptCurrent => encode(waiting(OPERATOR_WAIT, || engine.accept_current())?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{artifacts::UnavailableRenderer, process::Runner, store::Root};
    use std::{fs, os::unix::fs::PermissionsExt, sync::Mutex};
    #[derive(Debug, Default)]
    struct Fake(Mutex<Vec<String>>);
    impl Runner for Fake {
        fn run(
            &self,
            program: &str,
            _args: &[String],
            _input: Option<&[u8]>,
        ) -> Result<ActionOutput, Error> {
            self.0.lock().unwrap().push(program.into());
            Ok(ActionOutput::default())
        }
    }
    fn fixture() -> (tempfile::TempDir, Host, Arc<Fake>) {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(dir.path().join("etc")).unwrap();
        fs::set_permissions(dir.path().join("etc"), fs::Permissions::from_mode(0o755)).unwrap();
        let runner = Arc::new(Fake::default());
        let host = Host {
            root: Root(dir.path().into()),
            runner: runner.clone(),
            ndimon_gid: nix::unistd::getegid().as_raw(),
            renderer: Arc::new(UnavailableRenderer),
        };
        (dir, host, runner)
    }
    /// Keeps the arguments, which `Fake` does not.
    #[derive(Debug, Default)]
    struct Arguments(Mutex<Vec<Vec<String>>>);
    impl Runner for Arguments {
        fn run(
            &self,
            program: &str,
            args: &[String],
            _input: Option<&[u8]>,
        ) -> Result<ActionOutput, Error> {
            let mut call = vec![program.to_owned()];
            call.extend(args.iter().cloned());
            self.0.lock().unwrap().push(call);
            Ok(ActionOutput::default())
        }
    }
    #[test] // NI-17: no "Unit tesserad.service not found" at every boot of a device without Tessera.
    fn the_boot_queue_names_tesserad_only_where_it_is_installed() {
        let (dir, mut host, _) = fixture();
        let runner = Arc::new(Arguments::default());
        host.runner = runner.clone();
        job(&host, &["queue".into()]).unwrap();
        let stack = |extra: &[&str]| {
            let mut call = vec![
                "/usr/bin/systemctl".to_owned(),
                "--no-block".into(),
                "start".into(),
                "ndimon-control.service".into(),
                "ndimon-finder.service".into(),
                "ndimon-r.service".into(),
            ];
            call.extend(extra.iter().map(|s| (*s).to_owned()));
            call
        };
        assert_eq!(runner.0.lock().unwrap().as_slice(), [stack(&[])]);
        let units = dir.path().join("usr/lib/systemd/system");
        fs::create_dir_all(&units).unwrap();
        fs::write(units.join("tesserad.service"), "[Unit]\n").unwrap();
        job(&host, &["queue".into()]).unwrap();
        assert_eq!(
            runner.0.lock().unwrap().last().unwrap(),
            &stack(&["tesserad.service"])
        );
    }
    #[test]
    fn retired_flag_and_invalid_job_argv_never_invoke_helpers() {
        let (dir, host, runner) = fixture();

        fs::write(dir.path().join("etc/ndimon-network-engine"), "python\n").unwrap();
        fs::set_permissions(
            dir.path().join("etc/ndimon-network-engine"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();

        fs::write(dir.path().join("etc/ndimon-network-engine"), "rust\n").unwrap();
        fs::set_permissions(
            dir.path().join("etc/ndimon-network-engine"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();

        for args in [
            vec![],
            vec!["apply".into()],
            vec!["apply".into(), "a".repeat(63)],
            vec!["recover".into(), "A".repeat(64)],
            vec!["boot".into(), "token".into()],
        ] {
            assert!(job(&host, &args).is_err());
        }
        assert!(runner.0.lock().unwrap().is_empty());
    }
    #[test]
    fn stale_jobs_use_the_actor_authority_and_never_python() {
        let (dir, host, runner) = fixture();
        let store = Persistence::new(dir.path().join("var/lib/ndimon/state")).unwrap();
        for path in ["var", "var/lib", "var/lib/ndimon"] {
            fs::set_permissions(dir.path().join(path), fs::Permissions::from_mode(0o755)).unwrap();
        }
        store
            .initialize(&ndimon_state::persist::StateFile::new(
                ndimon_types::DesiredState::default(),
                Default::default(),
            ))
            .unwrap();
        job(&host, &["apply".into(), "a".repeat(64)]).unwrap();
        job(&host, &["recover".into(), "b".repeat(64)]).unwrap();
        assert!(runner.0.lock().unwrap().is_empty());
        assert!(
            !dir.path()
                .join("var/lib/ndimon-network/committed.json")
                .exists()
        );
    }
    #[test]
    fn python_fallback_cannot_bypass_handoff_quiescence() {
        let (dir, host, runner) = fixture();
        let root = ndimon_netengine::store::Root::new(dir.path().into()).unwrap();
        root.replace(
            "/etc/ndimon-network-engine",
            Some(&ndimon_netengine::store::Root::image("python\n", 0o644)),
        )
        .unwrap();
        root.replace(
            ndimon_netengine::handoff::QUIESCE,
            Some(&ndimon_netengine::store::Root::image(
                &json!({"id":"a".repeat(32)}).to_string(),
                0o600,
            )),
        )
        .unwrap();
        fs::set_permissions(
            dir.path().join("var/lib/ndimon-network"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        assert!(handle(&host, Request::NetworkRollback).is_err());
        let store = Persistence::new(dir.path().join("var/lib/ndimon/state")).unwrap();
        store
            .initialize(&ndimon_state::persist::StateFile::new(
                ndimon_types::DesiredState::default(),
                Default::default(),
            ))
            .unwrap();
        job(&host, &["boot".into()]).unwrap();
        assert!(runner.0.lock().unwrap().is_empty());
    }
    #[test]
    fn independent_jobs_retry_only_writer_contention() {
        for owner in [
            "network_operation",
            "legacy_network_writer",
            "state_transaction",
        ] {
            let mut attempts = 0;
            serialized_job(|| {
                attempts += 1;
                if attempts < 3 {
                    Err(ApiError::Locked {
                        field: "network".into(),
                        by: Some(owner.into()),
                    })
                } else {
                    Ok(())
                }
            })
            .unwrap();
            assert_eq!(attempts, 3);
        }
        let mut attempts = 0;
        assert!(
            serialized_job(|| {
                attempts += 1;
                Err(ApiError::Expired)
            })
            .is_err()
        );
        assert_eq!(attempts, 1);
    }
    fn locked_by(owner: &str) -> ApiError {
        ApiError::Locked {
            field: "network".into(),
            by: Some(owner.into()),
        }
    }
    #[test] // VV-3 W3
    fn operator_requests_wait_for_a_concurrent_request_but_not_for_other_owners() {
        let limit = std::time::Duration::from_secs(5);
        let mut attempts = 0;
        let kept = waiting(limit, || {
            attempts += 1;
            if attempts < 4 {
                Err(locked_by("network_operation"))
            } else {
                Ok(7)
            }
        });
        assert_eq!(kept.unwrap(), 7);
        assert_eq!(attempts, 4);
        // A pending change, a state transaction or a legacy writer is a reason,
        // not a collision: the answer is immediate.
        for owner in [
            "pending_revision",
            "state_transaction",
            "legacy_network_writer",
        ] {
            let mut attempts = 0;
            assert!(
                waiting(limit, || {
                    attempts += 1;
                    Err::<(), _>(locked_by(owner))
                })
                .is_err()
            );
            assert_eq!(attempts, 1, "{owner}");
        }
        // A lock that stays is refused once the bound has passed.
        let started = std::time::Instant::now();
        let refused = waiting(std::time::Duration::from_millis(150), || {
            Err::<(), _>(locked_by("network_operation"))
        });
        assert!(matches!(refused, Err(ApiError::Locked { .. })));
        assert!(started.elapsed() >= std::time::Duration::from_millis(150));
        // Other errors are never retried.
        let mut attempts = 0;
        assert!(
            waiting(limit, || {
                attempts += 1;
                Err::<(), _>(ApiError::Expired)
            })
            .is_err()
        );
        assert_eq!(attempts, 1);
    }
    #[test] // VV-3 W3, through the privd call a Keep makes
    fn keep_waits_for_the_lock_a_status_poll_holds() {
        use ndimon_privd_proto::NetworkCall;
        let (dir, host, _) = fixture();
        initialize_state(&dir);
        let store = ndimon_netengine::store::Store {
            root: ndimon_netengine::store::Root::new(dir.path().into()).unwrap(),
        };
        let lock = store.lock().unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(lock);
        });
        let started = std::time::Instant::now();
        let answer = native(
            &host,
            NetworkCall::Confirm {
                token: "a".repeat(64),
                local_address: None,
            },
        );
        release.join().unwrap();
        assert!(started.elapsed() >= std::time::Duration::from_millis(250));
        // Past the lock the request gets the answer it deserves: nothing pending.
        assert!(
            !matches!(answer, Err(ApiError::Locked { .. })),
            "{answer:?}"
        );
    }
    fn certify(dir: &tempfile::TempDir, text: &str, mode: u32) {
        let file = dir.path().join(&CERTIFICATION[1..]);
        let _ = fs::remove_file(&file);
        fs::write(&file, text).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(mode)).unwrap();
    }
    fn initialize_state(dir: &tempfile::TempDir) {
        let store = Persistence::new(dir.path().join("var/lib/ndimon/state")).unwrap();
        for path in ["var", "var/lib", "var/lib/ndimon"] {
            fs::set_permissions(dir.path().join(path), fs::Permissions::from_mode(0o755)).unwrap();
        }
        store
            .initialize(&ndimon_state::persist::StateFile::new(
                ndimon_types::DesiredState::default(),
                Default::default(),
            ))
            .unwrap();
    }
    #[test]
    fn the_certification_file_lists_the_backends_that_may_change() {
        let (dir, _, _) = fixture();
        let root = ndimon_netengine::store::Root::new(dir.path().into()).unwrap();
        let (backends, note) = certification(&root);
        assert!(backends.is_empty());
        assert!(note.unwrap().contains("is missing"));
        // What the packages ship: every backend with real-tool acceptance.
        let shipped =
            include_str!("../../../../packaging/debian/etc/ndimon-network-certified-backends.json");
        certify(&dir, shipped, 0o644);
        let (backends, note) = certification(&root);
        assert_eq!(
            backends,
            BTreeSet::from([
                NetworkBackend::Networkd,
                NetworkBackend::Netplan,
                NetworkBackend::Networkmanager,
                NetworkBackend::Ifupdown,
            ])
        );
        assert_eq!(note, None);
        // An operator withdraws all but one.
        certify(&dir, "[\"netplan\"]\n", 0o644);
        let (backends, note) = certification(&root);
        assert_eq!(backends, BTreeSet::from([NetworkBackend::Netplan]));
        assert_eq!(note, None);
        // Anything else allows nothing and says why.
        for (text, mode, reason) in [
            ("[]\n", 0o644, "lists no backend"),
            ("[\"bridge\"]\n", 0o644, "not a JSON array of backend names"),
            ("networkd\n", 0o644, "not a JSON array of backend names"),
            ("[\"networkd\"]\n", 0o666, "is unreadable"),
        ] {
            certify(&dir, text, mode);
            let (backends, note) = certification(&root);
            assert!(backends.is_empty(), "{text}");
            let note = note.unwrap();
            assert!(note.contains(reason), "{note}");
            assert!(note.contains(CERTIFICATION), "{note}");
            assert!(note.contains("network changes are refused"), "{note}");
        }
    }
    #[test]
    fn a_bad_certification_file_refuses_changes_without_failing_the_boot_job() {
        let (dir, host, runner) = fixture();
        initialize_state(&dir);
        for (text, mode) in [
            ("not json", 0o644),
            ("[\"networkd\"]\n", 0o666),
            ("[]", 0o644),
        ] {
            certify(&dir, text, mode);
            assert!(engine(&host).unwrap().config.certified_backends.is_empty());
            job(&host, &["boot".into()]).unwrap();
        }
        fs::remove_file(dir.path().join(&CERTIFICATION[1..])).unwrap();
        assert!(engine(&host).unwrap().config.certified_backends.is_empty());
        job(&host, &["boot".into()]).unwrap();
        certify(&dir, "[\"ifupdown\"]\n", 0o644);
        assert_eq!(
            engine(&host).unwrap().config.certified_backends,
            BTreeSet::from([NetworkBackend::Ifupdown])
        );
        assert!(runner.0.lock().unwrap().is_empty());
    }
}
