//! NE-9 fixture peer, built from NDIMon intent/output, not Tessera code/binaries.
use super::*;
use ndimon_netengine::handoff::{Artifact, Document, Review};
use ndimon_types::{NetworkHandoff, NetworkProduct};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;

fn private(root: &Root, directory: &str) -> Result<()> {
    let directory = root.path(directory)?;
    std::fs::create_dir_all(&directory).map_err(|_| error("peer_directory"))?;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| error("peer_mode"))
}
fn activate(p: &NetworkProfiles, d: &Document) -> Result<()> {
    let names = ndimon_netengine::backend::names(p);
    match p.backend {
        Some(NetworkBackend::Networkd | NetworkBackend::Netplan) => {
            if p.backend == Some(NetworkBackend::Netplan) {
                run(Tool::Netplan, &["generate"])?;
                run(Tool::Netplan, &["apply"])?;
            }
            run(Tool::Networkctl, &["reload"])?;
            let mut args = vec!["reconfigure", "--"];
            args.extend(names.iter().map(String::as_str));
            run(Tool::Networkctl, &args)?;
        }
        Some(NetworkBackend::Networkmanager) => {
            run(Tool::Nmcli, &["connection", "reload"])?;
            for uuid in d
                .adoption
                .nm_parent_uuid
                .iter()
                .chain(d.adoption.nm_owned.values())
            {
                run(Tool::Nmcli, &["connection", "up", "uuid", uuid])?;
            }
        }
        Some(NetworkBackend::Ifupdown) => {
            for name in &names {
                run(Tool::Ifup, &["--force", name])?;
            }
        }
        _ => return Err(error("backend")),
    }
    Ok(())
}
fn fixture(e: &Engine, backend: NetworkBackend) -> Result<(NetworkHandoff, Document)> {
    let mut p = with_backend(parent(false), backend);
    p.networks.push(row(Some(20), false, false));
    p.roles.audio = "vlan-20".into();
    p.roles.ndi = "vlan-20".into();
    p.networks[0].name = "Handoff management".into();
    keep(e, &p, None)?;
    let mut d: Document = serialized(|| e.call(ndimon_privd_proto::NetworkCall::HandoffPrepare))?;
    let again: Document = serialized(|| e.call(ndimon_privd_proto::NetworkCall::HandoffPrepare))?;
    ensure(d == again, "prepare_not_idempotent")?;
    ensure(
        matches!(e.stage(&p,e.authority.load()?.revision,None),Err(ApiError::Locked {by:Some(by),..}) if by=="handoff"),
        "quiesce_did_not_lock",
    )?;
    e.call::<Value>(ndimon_privd_proto::NetworkCall::HandoffCancel { id: d.id.clone() })?;
    let root = Root::production();
    for unit in [
        "tesserad.service",
        "ndimon-finder.service",
        "ndimon-r.service",
    ] {
        let _ = run(Tool::Systemctl, &["stop", unit]);
    }
    if backend == NetworkBackend::Ifupdown {
        for name in ndimon_netengine::backend::names(&p) {
            run(Tool::Ifdown, &["--force", &name])?;
        }
    }
    let old_table = run(Tool::Nft, &["list", "table", "inet", "ndimon_management"])?;
    let mut files = BTreeMap::new();
    let mut netplan: Option<Value> = None;
    for artifact in &d.artifacts {
        let before = root
            .snapshot(&artifact.path)?
            .ok_or_else(|| error("fixture_artifact_missing"))?;
        root.replace(&artifact.path, None)?;
        if artifact.path == ndimon_netengine::BINDING
            || artifact.path.ends_with("/ndimonndi.slice")
            || artifact.path.contains("/ndimon-r.service.d/")
            || artifact.path.contains("/ndimon-finder.service.d/")
            || artifact.path.ends_with("/70-ndimon-network.conf")
        {
            continue;
        }
        if artifact.path.ends_with("/ndimontessera.slice") {
            // The standalone slice is a package asset, outside the release set.
            root.replace("/usr/lib/systemd/system/tesseraaudio.slice", Some(&before))?;
            continue;
        }
        if artifact.path.starts_with("/etc/netplan/") {
            let value: Value =
                serde_json::from_slice(&before.bytes).map_err(|_| error("fixture_netplan_json"))?;
            if let Some(combined) = netplan.as_mut() {
                let additions = value["network"]
                    .as_object()
                    .ok_or_else(|| error("fixture_netplan"))?;
                combined["network"]
                    .as_object_mut()
                    .ok_or_else(|| error("fixture_netplan"))?
                    .extend(additions.clone());
            } else {
                netplan = Some(value);
            }
            continue;
        }
        let to = if artifact.path == ndimon_netengine::TESSERA_BINDING {
            "/etc/tessera/config.d/59-standalone-network.toml".into()
        } else {
            artifact.path.replace("ndimon", "tessera")
        };
        let mut after = before;
        if to.ends_with("70-tessera-network.conf") {
            after.bytes = b"[Service]\nSlice=tesseraaudio.slice\n".to_vec();
        } else if to.ends_with(".nmconnection") {
            let text = String::from_utf8(after.bytes).map_err(|_| error("fixture_nm"))?;
            after.bytes = text.replace("ndimon-vlan-", "tessera-vlan-").into_bytes();
        }
        root.replace(&to, Some(&after))?;
        files.insert(to, after);
    }
    if let Some(value) = netplan {
        let path = "/etc/netplan/90-tessera-network.yaml";
        let image = Root::image(
            &serde_json::to_string_pretty(&value).map_err(|_| error("fixture_netplan"))?,
            0o600,
        );
        root.replace(path, Some(&image))?;
        files.insert(path.into(), image);
    }
    run(Tool::Systemctl, &["daemon-reload"])?;
    run(Tool::Systemctl, &["start", "tesseraaudio.slice"])?;
    let source_table = old_table
        .replace("ndimon_management", "tessera_network")
        .replace("ndimonndi.slice", "tesseraaudio.slice")
        .replace("ndimontessera.slice", "tesseraaudio.slice");
    // A real atomic table replacement establishes the fixture's releasing owner.
    let runner = ContainerProcesses;
    let output = runner.run(&CommandSpec {
        tool: Tool::Nft,
        args: vec!["-f".into(), "-".into()],
        input: Some(format!("delete table inet ndimon_management\n{source_table}\n").into_bytes()),
        timeout_ms: 20_000,
        output_limit: ndimon_netengine::MAX_PRIVATE,
    })?;
    ensure(output.code == 0, "fixture_nft_replace")?;
    d.product = NetworkProduct::Tessera;
    d.network.ndi = None;
    d.bindings.remove("ndi");
    d.nftables_table = "tessera_network".into();
    d.version = "ndimon-fixture-ne-9.5".into();
    activate(&p, &d)?;
    d.artifacts.clear();
    for path in files.keys() {
        let actual = root
            .snapshot(path)?
            .ok_or_else(|| error("fixture_artifact_disappeared"))?;
        d.artifacts.push(Artifact {
            path: path.clone(),
            sha256: ndimon_netengine::handoff::sha256(&actual.bytes),
        });
    }
    private(&root, "/var/lib/tessera-network")?;
    private(&root, "/var/lib/tessera-network/handoff")?;
    for (path, value) in [
        (
            "/var/lib/tessera-network/committed.json".into(),
            json!({"fixture":true,"network":d.network}),
        ),
        (
            "/var/lib/tessera-network/quiesce.json".into(),
            json!({"id":d.id}),
        ),
        (
            format!("/var/lib/tessera-network/handoff/{}.json", d.id),
            json!(d),
        ),
    ] {
        root.replace(&path, Some(&Root::image(&value.to_string(), 0o600)))?;
    }
    let persistence = ndimon_state::persist::Persistence::new("/var/lib/ndimon/state")
        .map_err(|_| error("fixture_state"))?;
    let mut state = persistence
        .load()
        .map_err(|_| error("fixture_state"))?
        .ok_or_else(|| error("fixture_state"))?;
    state.document.network = NetworkProfiles::default();
    state.network_backend_metadata = None;
    state.network_commit_job_id = None;
    state.network_commit_revision = None;
    persistence
        .initialize(&state)
        .map_err(|_| error("fixture_state_reset"))?;
    Ok((NetworkHandoff::new(NetworkProduct::Tessera, &d.id), d))
}
pub(super) fn scenario(e: &Engine, backend: NetworkBackend, action: &str) -> Result<()> {
    let (source, d) = fixture(e, backend)?;
    let before = e.authority.load()?;
    let review: Review = e.call(ndimon_privd_proto::NetworkCall::HandoffImport {
        source: source.clone(),
    })?;
    ensure(
        !Root::production().exists(ndimon_netengine::BINDING)?,
        "import_mutated_binding",
    )?;
    ensure(
        review.profiles.roles.ndi == review.profiles.roles.audio,
        "audio_role_mapping",
    )?;
    let staged: ndimon_netengine::Stage = serialized(|| {
        e.call(ndimon_privd_proto::NetworkCall::HandoffCommit {
            source: source.clone(),
            expect_impact: review.impact.token.clone(),
            session: None,
        })
    })?;
    let token = staged.token.ok_or_else(|| error("handoff_token"))?;
    wait(e, true)?;
    ensure(
        e.authority.load()? == before,
        "handoff_committed_before_keep",
    )?;
    for artifact in &d.artifacts {
        ensure(
            !Root::production().exists(&artifact.path)?,
            "releasing_artifact_remains",
        )?;
    }
    let tables = run(Tool::Nft, &["list", "tables"])?;
    ensure(
        tables.contains("ndimon_management") && !tables.contains("tessera_network"),
        "dual_guard_owners",
    )?;
    let done = format!("/var/lib/tessera-network/handoff/{}.done", source.id);
    match action {
        "keep" => {
            serialized(|| e.confirm(token.expose(), None))?;
            let committed = e.authority.load()?;
            ensure(
                committed.revision == before.revision + 1,
                "handoff_revision",
            )?;
            ensure(
                committed
                    .metadata
                    .handoff
                    .as_ref()
                    .is_some_and(|h| h.source == source),
                "handoff_receipt",
            )?;
            ensure(Root::production().exists(&done)?, "missing_done")?;
            e.boot()?;
            ensure(e.authority.load()? == committed, "kept_handoff_reverted")?;
        }
        "revert" => serialized(|| e.revert(token.expose()))?,
        "expire" => {
            wait_limit(e, false, 90)?;
            // Pending is removed before timer cleanup releases the writer lock.
            // Use the harness's ordinary bounded contention retry here too.
            let result = serialized(|| e.confirm(token.expose(), None));
            ensure(
                matches!(result, Err(ApiError::Expired)),
                &format!("expiry_kind:{result:?}"),
            )?;
        }
        _ => return Err(error("handoff_action")),
    }
    if action != "keep" {
        ensure(e.authority.load()? == before, "handoff_recovery_committed")?;
        let root = Root::production();
        for a in &d.artifacts {
            let i = root
                .snapshot(&a.path)?
                .ok_or_else(|| error("source_not_restored"))?;
            ensure(
                ndimon_netengine::handoff::sha256(&i.bytes) == a.sha256,
                "source_hash_not_restored",
            )?;
        }
        for name in ["quiesce.json", "external.json"] {
            ensure(
                !root.exists(&format!("/var/lib/tessera-network/{name}"))?,
                "source_marker_not_restored",
            )?;
        }
        ensure(
            !root.exists(&done)? && !root.exists(ndimon_netengine::BINDING)?,
            "receiver_artifact_remains",
        )?;
        let tables = run(Tool::Nft, &["list", "tables"])?;
        ensure(
            tables.contains("tessera_network") && !tables.contains("ndimon_management"),
            "recovery_dual_owners",
        )?;
        ensure(
            run(Tool::Ip, &["-j", "-4", "address", "show", "end0.20"])?.contains("198.51.20.10"),
            "recovered_media_address",
        )?;
    }
    println!(
        "NE-9.5-9.9 fixture handoff {action}: one owner, real backend/timer, no Tessera binary"
    );
    Ok(())
}
