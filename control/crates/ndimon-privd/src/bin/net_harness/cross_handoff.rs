//! NE-9.10 acceptance between both real engines in disposable guests.
use super::*;
use ndimon_netengine::handoff::{Document, Intent, Ownership, Review};
use ndimon_types::{NetworkHandoff, NetworkProduct};
use serde_json::{Value, json};
use std::process::Stdio;

fn tessera_raw(command: Value, api: u32) -> Result<Value> {
    let mut child = Command::new("/usr/bin/tessera-network")
        .arg("request")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| error("tessera_spawn"))?;
    writeln!(
        child.stdin.take().ok_or_else(|| error("tessera_stdin"))?,
        "{}",
        json!({"api":api,"command":command})
    )
    .map_err(|_| error("tessera_write"))?;
    let output = child
        .wait_with_output()
        .map_err(|_| error("tessera_wait"))?;
    // The real CLI deliberately exits nonzero for a typed refusal. Preserve
    // that JSON so acceptance identifies the exact failing contract step.
    let reply: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        error(&format!(
            "tessera_process:{}",
            redact(&String::from_utf8_lossy(&output.stderr))
        ))
    })?;
    ensure(
        output.status.success() || !reply["error"].is_null(),
        "tessera_process_exit",
    )?;
    Ok(reply)
}
fn tessera(command: Value, api: u32) -> Result<Value> {
    let kind = command["kind"].as_str().unwrap_or("unknown").to_owned();
    let start = Instant::now();
    loop {
        let reply = tessera_raw(command.clone(), api)?;
        // Applied can be observed just before the apply process releases its
        // lock. Tessera explicitly classifies this early Keep as StillApplying.
        if kind == "confirm"
            && reply["error_kind"] == "still_applying"
            && start.elapsed() < Duration::from_secs(45)
        {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        ensure(
            reply["error"].is_null(),
            &format!("tessera_refused:{kind}:{}", redact(&reply.to_string())),
        )?;
        return Ok(reply);
    }
}
fn t_wait(applied: bool) -> Result<Value> {
    let start = Instant::now();
    loop {
        let reply = tessera(json!({"kind":"status"}), 1)?;
        if (applied && reply["pending"]["phase"] == "applied")
            || (!applied && reply["pending"].is_null())
        {
            return Ok(reply);
        }
        ensure(
            reply["pending"]["phase"] != "failed",
            &format!("tessera_apply:{}", redact(&reply.to_string())),
        )?;
        ensure(
            start.elapsed() < Duration::from_secs(110),
            "tessera_timeout",
        )?;
        std::thread::sleep(Duration::from_millis(250));
    }
}
fn desired(backend: NetworkBackend) -> NetworkProfiles {
    let mut p = with_backend(parent(false), backend);
    p.networks.push(row(Some(20), false, false));
    p.roles.audio = "vlan-20".into();
    p.roles.ndi = "vlan-20".into();
    p
}
fn t_owner(backend: NetworkBackend) -> Result<Document> {
    let root = Root::production();
    ensure(
        !root.exists("/var/lib/ndimon-network/quiesce.json")?,
        "ndimon_prepared_before_tessera",
    )?;
    if backend == NetworkBackend::Ifupdown {
        let from = "/etc/network/interfaces.d/ndimon-end0";
        let to = "/etc/network/interfaces.d/tessera-end0";
        let mut image = root.snapshot(from)?.ok_or_else(|| error("ifup_fixture"))?;
        // Tessera requires explicit administrator consent for first adoption.
        // This fixture is prepared before either product owns networking.
        image
            .bytes
            .splice(..0, b"# Handed off to tessera-network.\n".iter().copied());
        root.replace(to, Some(&image))?;
        root.replace(from, None)?;
    }
    let initial = tessera(json!({"kind":"status"}), 1)?;
    let mut document = serde_json::to_value(Intent::from_profiles(&desired(backend), 0)?)
        .map_err(|_| error("intent"))?;
    document
        .as_object_mut()
        .ok_or_else(|| error("intent"))?
        .remove("ndi");
    document["revision"] = json!(
        initial["profiles"]["revision"]
            .as_u64()
            .ok_or_else(|| error("initial_revision"))?
            .to_string()
    );
    let staged = tessera(
        json!({"kind":"apply_document","document":document,"session":null}),
        2,
    )?;
    let token = staged["token"]
        .as_str()
        .ok_or_else(|| error("tessera_token"))?;
    t_wait(true)?;
    tessera(
        json!({"kind":"confirm","token":token,"local_address":"192.0.2.10"}),
        2,
    )?;
    let d = tessera(json!({"kind":"handoff_prepare","receiver":"ndimon"}), 2)?;
    serde_json::from_value(d["handoff"].clone()).map_err(|_| error("tessera_offer"))
}
fn owner_table(owner: NetworkProduct) -> &'static str {
    if owner == NetworkProduct::Ndimon {
        "ndimon_management"
    } else {
        "tessera_network"
    }
}
fn one_owner(owner: NetworkProduct) -> Result<()> {
    let tables = run(Tool::Nft, &["list", "tables"])?;
    ensure(tables.contains(owner_table(owner)), "owner_guard_missing")?;
    let other = if owner == NetworkProduct::Ndimon {
        NetworkProduct::Tessera
    } else {
        NetworkProduct::Ndimon
    };
    ensure(!tables.contains(owner_table(other)), "dual_owners")?;
    for (name, address) in [("end0", "192.0.2.10"), ("end0.20", "198.51.20.10")] {
        let live = run(Tool::Ip, &["-j", "-4", "address", "show", name])?;
        let live: Value = serde_json::from_str(&live).map_err(|_| error("addresses"))?;
        let addresses = live[0]["addr_info"]
            .as_array()
            .ok_or_else(|| error("addresses"))?;
        ensure(
            addresses.iter().filter(|a| a["scope"] == "global").count() == 1
                && addresses
                    .iter()
                    .any(|a| a["local"] == address && a["prefixlen"] == 24),
            "handoff_changed_addresses",
        )?;
    }
    Ok(())
}
fn source_path(d: &Document, suffix: &str) -> String {
    format!("{}/handoff/{}.{suffix}", d.product.state_directory(), d.id)
}
fn verify_done(d: &Document) -> Result<()> {
    let root = Root::production();
    let done = root
        .snapshot(&source_path(d, "done"))?
        .ok_or_else(|| error("missing_done"))?;
    let external = root
        .snapshot(&format!("{}/external.json", d.product.state_directory()))?
        .ok_or_else(|| error("missing_external"))?;
    let o: Ownership = serde_json::from_slice(&done.bytes).map_err(|_| error("done_shape"))?;
    let offered = root
        .snapshot(&source_path(d, "json"))?
        .ok_or_else(|| error("missing_offer"))?;
    ensure(
        done.bytes == external.bytes
            && done.mode == 0o600
            && external.mode == 0o600
            && offered.mode == 0o600
            && o.id == d.id
            && o.document_sha256 == ndimon_netengine::handoff::sha256(&offered.bytes),
        "done_does_not_match_reservation",
    )
}
fn restored_source(e: &Engine, d: &Document, before: &Commit) -> Result<()> {
    let root = Root::production();
    ensure(e.authority.load()? == *before, "recovery_changed_authority")?;
    for a in &d.artifacts {
        let image = root
            .snapshot(&a.path)?
            .ok_or_else(|| error("source_artifact_missing"))?;
        ensure(
            ndimon_netengine::handoff::sha256(&image.bytes) == a.sha256,
            "source_artifact_bytes",
        )?;
    }
    for name in ["external", "quiesce"] {
        ensure(
            !root.exists(&format!("{}/{name}.json", d.product.state_directory()))?,
            "source_marker_remains",
        )?;
    }
    ensure(
        !root.exists(&source_path(d, "done"))? && !root.exists(ndimon_netengine::BINDING)?,
        "receiver_artifact_remains",
    )?;
    one_owner(NetworkProduct::Tessera)?;
    let status = tessera(json!({"kind":"status"}), 1)?;
    ensure(
        status["externally_managed"] == false && status["profiles"]["audio"] == d.network.audio,
        "tessera_did_not_resume",
    )?;
    println!(
        "Restored Tessera's exact artifact hashes, original authority, role bindings and sole guard"
    );
    Ok(())
}
#[derive(Serialize, Deserialize)]
struct Reboot {
    boot: String,
    document: Document,
    before: Commit,
    token: Secret,
}
pub(super) fn verify_reboot(e: &Engine) -> Result<()> {
    let saved: Reboot = serde_json::from_str(
        &Root::production()
            .text("/var/lib/nh2-reboot.json")?
            .ok_or_else(|| error("reboot_record"))?,
    )
    .map_err(|_| error("reboot_record"))?;
    ensure(LinuxClock.boot_id()? != saved.boot, "guest_did_not_reboot")?;
    run(
        Tool::Systemctl,
        &["is-active", "--quiet", "ndimon-net-boot.service"],
    )?;
    ensure(e.status()?.pending.is_none(), "boot_left_pending")?;
    restored_source(e, &saved.document, &saved.before)?;
    ensure(
        matches!(
            serialized(|| e.confirm(saved.token.expose(), None)),
            Err(ApiError::Expired)
        ),
        "reboot_token_not_expired",
    )?;
    println!("PASS cross-reboot-verify: changed boot ID, boot recovery restored releasing owner");
    Ok(())
}
pub(super) fn scenario(e: &Engine, backend: NetworkBackend, name: &str) -> Result<()> {
    let root = Root::production();
    if name == "cross-ndimon-keep" {
        keep(e, &desired(backend), None)?;
        let d: Document = serialized(|| e.call(ndimon_privd_proto::NetworkCall::HandoffPrepare))?;
        let before = root.snapshot(&source_path(&d, "json"))?;
        let review = tessera(
            json!({"kind":"handoff_import","product":"ndimon","id":d.id}),
            2,
        )?;
        ensure(
            !root.exists("/etc/tessera/config.d/59-standalone-network.toml")?
                && root.snapshot(&source_path(&d, "json"))? == before,
            "tessera_import_mutated",
        )?;
        let staged = tessera(
            json!({"kind":"handoff_commit","product":"ndimon","id":d.id,"expect_impact":review["review"]["impact"]["token"],"confirm":false}),
            2,
        )?;
        let token = staged["token"]
            .as_str()
            .ok_or_else(|| error("tessera_token"))?;
        t_wait(true)?;
        tessera(
            json!({"kind":"confirm","token":token,"local_address":"192.0.2.10"}),
            2,
        )?;
        verify_done(&d)?;
        let status = e.status()?;
        ensure(
            status.externally_managed && !status.quiesced && !status.writable,
            "ndimon_release_incomplete",
        )?;
        one_owner(NetworkProduct::Tessera)?;
        println!(
            "Real NDIMon prepare -> Tessera read-only import/commit/Keep -> NDIMon archived and externally managed"
        );
        return Ok(());
    }
    let d = t_owner(backend)?;
    let source = NetworkHandoff::new(NetworkProduct::Tessera, &d.id);
    let before = e.authority.load()?;
    let review: Review = e.call(ndimon_privd_proto::NetworkCall::HandoffImport {
        source: source.clone(),
    })?;
    ensure(
        !root.exists(ndimon_netengine::BINDING)?
            && e.authority.load()? == before
            && !root.exists("/var/lib/ndimon-network/quiesce.json")?,
        "ndimon_import_mutated",
    )?;
    ensure(
        review.profiles.roles.audio == d.network.audio
            && review.profiles.roles.ndi == d.network.audio,
        "role_substitution",
    )?;
    if name == "cross-drift" {
        let artifact = d.artifacts.first().ok_or_else(|| error("artifact"))?;
        let mut image = root
            .snapshot(&artifact.path)?
            .ok_or_else(|| error("artifact"))?;
        image.bytes.extend_from_slice(b"\n# NH2 drift\n");
        root.replace(&artifact.path, Some(&image))?;
        let result: Result<ndimon_netengine::Stage> =
            e.call(ndimon_privd_proto::NetworkCall::HandoffCommit {
                source,
                expect_impact: review.impact.token,
                session: None,
            });
        ensure(
            matches!(result, Err(ApiError::Drift { .. })),
            "drift_not_refused",
        )?;
        ensure(
            root.snapshot(&artifact.path)? == Some(image)
                && !root.exists(ndimon_netengine::BINDING)?
                && !root.exists("/var/lib/tessera-network/external.json")?
                && e.authority.load()? == before,
            "drift_abort_mutated",
        )?;
        one_owner(NetworkProduct::Tessera)?;
        tessera(json!({"kind":"handoff_cancel","id":d.id}), 2)?;
        println!(
            "Modified listed artifact after prepare/review; NDIMon commit returned Drift before reservation or mutation"
        );
        return Ok(());
    }
    let staged: ndimon_netengine::Stage = serialized(|| {
        e.call(ndimon_privd_proto::NetworkCall::HandoffCommit {
            source: source.clone(),
            expect_impact: review.impact.token.clone(),
            session: None,
        })
    })?;
    let token = staged.token.ok_or_else(|| error("handoff_token"))?;
    wait(e, true)?;
    ensure(e.authority.load()? == before, "premature_authority_commit")?;
    one_owner(NetworkProduct::Ndimon)?;
    match name {
        "cross-tessera-keep" => {
            serialized(|| e.confirm(token.expose(), Some("192.0.2.10".parse().unwrap())))?;
            verify_done(&d)?;
            let release = tessera(json!({"kind":"handoff_status"}), 2)?;
            ensure(
                release["handoff"]["externally_managed"] == true
                    && release["handoff"]["quiesced"] == false,
                "tessera_release_incomplete",
            )?;
            ensure(
                e.authority.load()?.profiles == review.profiles,
                "accepted_intent_changed",
            )?;
            one_owner(NetworkProduct::Ndimon)?;
            println!(
                "Real standalone Tessera prepare -> NDIMon read-only import/commit/Keep -> Tessera archived and externally managed"
            );
        }
        "cross-revert" => {
            serialized(|| e.revert(token.expose()))?;
            restored_source(e, &d, &before)?;
        }
        "cross-expire" => {
            wait(e, false)?;
            ensure(
                matches!(
                    serialized(|| e.confirm(token.expose(), None)),
                    Err(ApiError::Expired)
                ),
                "expiry_kind",
            )?;
            restored_source(e, &d, &before)?;
        }
        "cross-reboot-stage" => {
            let pending = e.pending(token.expose())?;
            ensure(
                pending.phase == Phase::Applied && pending.remaining_seconds > 0,
                "outside_commit_window",
            )?;
            let saved = Reboot {
                boot: LinuxClock.boot_id()?,
                document: d,
                before,
                token,
            };
            root.replace(
                "/var/lib/nh2-reboot.json",
                Some(&Root::image(
                    &serde_json::to_string(&saved).map_err(|_| error("reboot_record"))?,
                    0o600,
                )),
            )?;
            println!(
                "Receiver applied inside Keep window; outer lab must restart guest and verify recovery"
            );
        }
        _ => return Err(error("cross_scenario")),
    }
    Ok(())
}
