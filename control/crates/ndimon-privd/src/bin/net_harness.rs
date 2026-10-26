//! Non-shipped NE-11 driver. Only install inside disposable nspawn images.
#[path = "net_harness/cross_handoff.rs"]
mod cross_handoff;
#[path = "net_harness/handoff.rs"]
mod handoff;
#[path = "net_harness/ports.rs"]
mod ports;
#[path = "net_harness/search.rs"]
mod search;
use ndimon_netengine::harness::ContainerProcesses;
use ndimon_netengine::store::{FileChange, Root, Secret, Store};
use ndimon_netengine::{
    Authority, Clock, CommandRunner, CommandSpec, Commit, LinuxClock, Phase, Result, Tool,
};
use ndimon_types::{
    AddressMode, ApiError, ConfirmPolicy, Domain, NetworkBackend, NetworkProfile, NetworkProfiles,
};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn error(reason: &str) -> ApiError {
    ApiError::ApplyFailed {
        domain: Domain::Network,
        step: "harness".into(),
        reason: reason.into(),
    }
}
fn redact(text: &str) -> String {
    let mut output = String::new();
    let mut run = String::new();
    for ch in text.chars().take(4096).chain(std::iter::once('\n')) {
        if ch.is_ascii_hexdigit() {
            run.push(ch);
        } else {
            if run.len() >= 64 {
                output.push_str("[redacted]");
            } else {
                output.push_str(&run);
            }
            run.clear();
            output.push(ch);
        }
    }
    output
}
fn ensure(value: bool, reason: &str) -> Result<()> {
    if value { Ok(()) } else { Err(error(reason)) }
}
fn serialized<T>(mut operation: impl FnMut() -> Result<T>) -> Result<T> {
    let start = Instant::now();
    loop {
        match operation() {
            Err(ApiError::Locked { by: Some(by), .. })
                if matches!(by.as_str(), "network_operation" | "state_transaction")
                    && start.elapsed() < Duration::from_secs(10) =>
            {
                std::thread::sleep(Duration::from_millis(100))
            }
            result => return result,
        }
    }
}
struct Engine {
    authority: Arc<dyn Authority>,
}
impl Engine {
    fn call<T: serde::de::DeserializeOwned>(
        &self,
        call: ndimon_privd_proto::NetworkCall,
    ) -> Result<T> {
        let host = ndimon_privd::Host::production().map_err(|_| error("privd_host"))?;
        let output = host
            .handle(ndimon_privd_proto::Request::NetworkNative { call })
            .map_err(|_| error("privd_dispatch"))?;
        let value = output.data.ok_or_else(|| error("privd_reply"))?;
        if let Some(error) = value.get("err") {
            return Err(
                serde_json::from_value(error.clone()).map_err(|_| crate::error("privd_error"))?
            );
        }
        serde_json::from_value(value["ok"].clone()).map_err(|_| error("privd_decode"))
    }
    fn status(&self) -> Result<ndimon_netengine::Status> {
        self.call(ndimon_privd_proto::NetworkCall::Status)
    }
    fn preview(&self, p: &NetworkProfiles, revision: u64) -> Result<ndimon_netengine::Preview> {
        self.call(ndimon_privd_proto::NetworkCall::Preview {
            request: ndimon_types::NetworkRequest::new(revision, p.clone()),
        })
    }
    fn stage(
        &self,
        p: &NetworkProfiles,
        revision: u64,
        session: Option<ndimon_types::NetworkSessionPath>,
    ) -> Result<ndimon_netengine::Stage> {
        self.call(ndimon_privd_proto::NetworkCall::Stage {
            request: ndimon_types::NetworkRequest::new(revision, p.clone()),
            session: session.map(Into::into),
        })
    }
    fn pending(&self, token: &str) -> Result<ndimon_netengine::Pending> {
        self.call(ndimon_privd_proto::NetworkCall::Pending {
            token: token.into(),
        })
    }
    fn confirm(&self, token: &str, local_address: Option<IpAddr>) -> Result<Commit> {
        self.call(ndimon_privd_proto::NetworkCall::Confirm {
            token: token.into(),
            local_address,
        })
    }
    fn revert(&self, token: &str) -> Result<()> {
        self.call::<serde_json::Value>(ndimon_privd_proto::NetworkCall::Revert {
            token: token.into(),
        })
        .map(|_| ())
    }
    fn job(&self, args: &[&str]) -> Result<()> {
        let output = Command::new(ndimon_netengine::PRIVD)
            .arg("network-job")
            .args(args)
            .output()
            .map_err(|_| error("privd_job"))?;
        if !output.status.success() {
            return Err(error(&format!(
                "privd_job_exit: {} {}",
                redact(&String::from_utf8_lossy(&output.stdout)),
                redact(&String::from_utf8_lossy(&output.stderr))
            )));
        }
        Ok(())
    }
    fn boot(&self) -> Result<()> {
        self.job(&["boot"])
    }
    fn run_apply_job(&self, id: &str) -> Result<()> {
        self.job(&["apply", id])
    }
    fn run_recovery(&self, id: &str) -> Result<()> {
        self.job(&["recover", id])
    }
}
fn engine() -> Engine {
    Engine {
        authority: Arc::new(ndimon_state::network_authority::StateAuthority::new(
            ndimon_state::persist::Persistence {
                directory: "/var/lib/ndimon/state".into(),
            },
        )),
    }
}

fn run(tool: Tool, args: &[&str]) -> Result<String> {
    let out = ContainerProcesses.run(&CommandSpec {
        tool,
        args: args.iter().map(|s| (*s).into()).collect(),
        input: None,
        timeout_ms: 30_000,
        output_limit: 2 * 1024 * 1024,
    })?;
    ensure(out.code == 0, "probe_exit")?;
    Ok(out.stdout)
}
fn wait(e: &Engine, applied: bool) -> Result<()> {
    wait_limit(e, applied, if applied { 110 } else { 155 })
}
fn wait_limit(e: &Engine, applied: bool, seconds: u64) -> Result<()> {
    let start = Instant::now();
    loop {
        let status = serialized(|| e.status())?;
        match status.pending {
            Some(p) if applied && p.phase == Phase::Applied => return Ok(()),
            None if !applied => return Ok(()),
            Some(p) if applied && p.phase == Phase::Failed => {
                return Err(error(&format!(
                    "apply_failed:{:?}/{:?}",
                    p.apply_error, p.recovery_error
                )));
            }
            None if applied => return Err(error(&format!("apply_ended:{:?}", status.last_error))),
            _ => (),
        }
        if start.elapsed() > Duration::from_secs(seconds) {
            return Err(error("wait_timeout"));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
fn parent(dhcp: bool) -> NetworkProfiles {
    let mut p = NetworkProfiles::default();
    p.interface = "end0".into();
    let n = row(None, dhcp, true);
    p.networks.push(n);
    p.roles.management = "untagged".into();
    p.roles.ndi = "untagged".into();
    p.roles.audio = "untagged".into();
    p
}
fn row(tag: Option<u16>, dhcp: bool, management: bool) -> NetworkProfile {
    let mut n = NetworkProfile::dhcp(
        tag.map(|v| format!("vlan-{v}"))
            .unwrap_or("untagged".into()),
        "Harness network",
    );
    n.vlan_id = tag;
    if !dhcp {
        n.mode = AddressMode::Static;
        n.prefix = 24;
        let prefix = match tag {
            Some(20) => "198.51.20",
            Some(30) => "203.0.113",
            _ => "192.0.2",
        };
        n.address = format!("{prefix}.10");
        if management {
            n.gateway = format!("{prefix}.1");
            n.dns = vec![format!("{prefix}.1")];
        }
    }
    n
}
fn stage(e: &Engine, p: &NetworkProfiles) -> Result<(Secret, String, Vec<FileChange>)> {
    let revision = e.authority.load()?.revision;
    let s = serialized(|| e.stage(p, revision, None))?;
    let token = s.token.ok_or_else(|| error("unexpected_noop"))?;
    let job = s.pending.ok_or_else(|| error("missing_pending"))?.job_id;
    let value: serde_json::Value = Store {
        root: Root::production(),
    }
    .read("pending")?
    .ok_or_else(|| error("missing_journal"))?;
    let mut files: Vec<FileChange> = serde_json::from_value(value["plan"]["files"].clone())
        .map_err(|_| error("journal_format"))?;
    // The files of a Netplan-owned NetworkManager profile (NE-4.9) are restored
    // byte for byte too, so the same checks cover them.
    for change in value["plan"]["nm"].as_array().into_iter().flatten() {
        let persisted: Vec<ndimon_netengine::backend::NmFile> = serde_json::from_value(
            change
                .get("files")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([])),
        )
        .map_err(|_| error("journal_format"))?;
        files.extend(persisted.into_iter().map(|f| FileChange {
            path: f.path,
            before: f.before,
            after: None,
        }));
    }
    Ok((token, job, files))
}
fn restored(files: &[FileChange]) -> Result<()> {
    for file in files {
        ensure(
            Root::production().snapshot(&file.path)? == file.before,
            &format!("snapshot_mismatch:{}", file.path),
        )?;
    }
    Ok(())
}
fn keep(e: &Engine, p: &NetworkProfiles, local: Option<IpAddr>) -> Result<()> {
    let revision = e.authority.load()?.revision;
    let (token, _, _) = stage(e, p)?;
    wait(e, true)?;
    serialized(|| e.confirm(token.expose(), local))?;
    ensure(
        e.authority.load()?.revision == revision + 1,
        "revision_increment",
    )
}
// Test-only cookie-less transport. The server obtains the destination from the
// accepted socket; the token appears only in the HTTP body, never argv/logs.
fn socket_confirm(token: &Secret, destination: IpAddr) -> Result<u16> {
    let mut stream = TcpStream::connect((destination, 80)).map_err(|_| error("connect"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|_| error("socket_timeout"))?;
    let body = serde_json::to_string(token).map_err(|_| error("token_format"))?;
    write!(stream,"POST /confirm HTTP/1.1\r\nHost: harness\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).map_err(|_| error("http_write"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|_| error("http_read"))?;
    response
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| error("http_status"))
}
fn serve(e: &Engine) -> Result<()> {
    let listener = TcpListener::bind("0.0.0.0:80").map_err(|_| error("listener"))?;
    for stream in listener.incoming() {
        let mut stream = stream.map_err(|_| error("accept"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|_| error("socket_timeout"))?;
        let mut request = Vec::new();
        let mut buf = [0; 1024];
        let mut header = None;
        while request.len() < 8192 {
            let n = stream.read(&mut buf).map_err(|_| error("http_read"))?;
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buf[..n]);
            if let Some(offset) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                let length = String::from_utf8_lossy(&request[..offset])
                    .lines()
                    .find_map(|s| {
                        s.strip_prefix("Content-Length: ")
                            .and_then(|s| s.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                header = Some((offset + 4, length));
                if request.len() >= offset + 4 + length {
                    break;
                }
            }
        }
        let code = if request.starts_with(b"POST /confirm ") {
            let result: Result<()> = (|| {
                let (offset, length) = header.ok_or_else(|| error("http_header"))?;
                let token: Secret = serde_json::from_slice(
                    request
                        .get(offset..offset + length)
                        .ok_or_else(|| error("http_body"))?,
                )
                .map_err(|_| error("token_format"))?;
                let local = stream
                    .local_addr()
                    .map_err(|_| error("local_address"))?
                    .ip();
                serialized(|| e.confirm(token.expose(), Some(local))).map(|_| ())
            })();
            result.err().map_or(200, |e| e.http_status())
        } else {
            200
        };
        let _ = write!(
            stream,
            "HTTP/1.1 {code} Result\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
        );
    }
    Ok(())
}
#[derive(Debug, Serialize, Deserialize)]
struct RebootCase {
    token: Secret,
    boot: String,
    base: Commit,
    files: Vec<FileChange>,
}
fn fault(backend: NetworkBackend, count: u32) -> Result<()> {
    let (tool, arg) = match backend {
        NetworkBackend::Networkd | NetworkBackend::Netplan => ("Networkctl", "reconfigure"),
        NetworkBackend::Networkmanager => ("Nmcli", "up"),
        NetworkBackend::Ifupdown => ("Ifup", "end0"),
        _ => return Err(error("backend")),
    };
    Root::production().replace(
        "/run/ndimon-harness-fault.json",
        Some(&Root::image(
            &serde_json::to_string(&serde_json::json!({"tool":tool,"arg":arg,"remaining":count}))
                .map_err(|_| error("fault_format"))?,
            0o600,
        )),
    )
}
fn scenario(e: &Engine, name: &str) -> Result<()> {
    if name == "cross-reboot-verify" {
        return cross_handoff::verify_reboot(e);
    }
    let status = e.status()?;
    // A refused adoption has no backend; say why instead of a bare "backend".
    let backend = status
        .profiles
        .backend
        .ok_or_else(|| match &status.refusal {
            Some(r) => error(&format!("adoption_refused:{}:{}", r.reason, r.detail)),
            None => error("backend"),
        })?;
    let mut p = status.profiles.clone();
    match name {
        "packet-restrictions" => ports::restrictions(e, backend)?,
        "timer-properties" => ports::timers(e)?,
        "guest-profiles" => ports::profiles(e, backend)?,
        "tessera-daemon" => ports::tessera(e, backend)?,
        // NE-2.12a; run against the fixture xtask writes for `search-*` runs.
        name if name.starts_with("search-") => search::scenario(e, backend, name)?,
        name if name.starts_with("cross-") => cross_handoff::scenario(e, backend, name)?,
        "handoff-keep" | "handoff-revert" | "handoff-expire" => {
            handoff::scenario(e, backend, name.trim_start_matches("handoff-"))?;
        }
        "first-adoption-revert" => {
            let base = e.authority.load()?;
            ensure(base.profiles.backend.is_none(), "requires_fresh_adoption")?;
            let selected = if matches!(backend, NetworkBackend::Networkd | NetworkBackend::Netplan)
            {
                Some(
                    run(Tool::Networkctl, &["status", "--no-pager", "--", "end0"])?
                        .lines()
                        .find(|s| s.contains("Network File:"))
                        .ok_or_else(|| error("original_source"))?
                        .trim()
                        .to_owned(),
                )
            } else {
                None
            };
            p = parent(true);
            p.backend = Some(backend);
            p.networks.push(row(Some(20), false, false));
            p.roles.audio = "vlan-20".into();
            p.roles.ndi = "vlan-20".into();
            let (token, _, files) = stage(e, &p)?;
            wait(e, true)?;
            serialized(|| e.revert(token.expose()))?;
            ensure(e.authority.load()? == base, "adoption_revert_committed")?;
            restored(&files)?;
            if let Some(selected) = selected {
                let current = run(Tool::Networkctl, &["status", "--no-pager", "--", "end0"])?;
                ensure(
                    current.lines().any(|s| s.trim() == selected),
                    "original_source_not_restored",
                )?;
            }
        }
        "commit-sigkill" => {
            for point in ["rename", "directory_fsync"] {
                let base = e.authority.load()?;
                p.networks[0].name = format!("Commit fault {point}");
                let (token, _, files) = stage(e, &p)?;
                wait(e, true)?;
                let root = Root::production();
                root.replace(
                    "/run/ndimon-harness-token.json",
                    Some(&Root::image(
                        &serde_json::to_string(&token).map_err(|_| error("token_format"))?,
                        0o600,
                    )),
                )?;
                root.replace("/run/ndimon-harness-storage-fault.json",Some(&Root::image(&serde_json::json!({"path":"/var/lib/ndimon/state/state.json","point":point,"kill":true}).to_string(),0o600)))?;
                let status = Command::new("/usr/libexec/ndimon/ndimon-net-harness")
                    .arg("confirm-saved")
                    .status()
                    .map_err(|_| error("confirm_child"))?;
                ensure(status.signal() == Some(9), "confirm_not_killed")?;
                root.replace("/run/ndimon-harness-token.json", None)?;
                // Independent boot entry resolves exact authority identity on
                // either side of rename; no assumption based on revision alone.
                e.boot()?;
                let current = e.authority.load()?;
                if point == "rename" {
                    ensure(current == base, "pre_commit_kill_committed")?;
                    restored(&files)?;
                } else {
                    ensure(
                        current.revision == base.revision + 1 && current.profiles == p,
                        "post_commit_kill_rolled_back",
                    )?;
                }
                ensure(e.status()?.pending.is_none(), "commit_kill_pending")?;
            }
        }
        "foreign-competition" => {
            if matches!(
                backend,
                NetworkBackend::Networkmanager | NetworkBackend::Ifupdown
            ) {
                let base = e.authority.load()?;
                p.networks[0].name = "Foreign refusal".into();
                let path = "/etc/network/interfaces.d/foreign-test";
                let uuid = "22222222-2222-4222-8222-222222222222";
                if backend == NetworkBackend::Networkmanager {
                    run(
                        Tool::Nmcli,
                        &[
                            "connection",
                            "add",
                            "type",
                            "ethernet",
                            "ifname",
                            "end0",
                            "con-name",
                            "foreign-test",
                            "connection.uuid",
                            uuid,
                            "ipv4.method",
                            "disabled",
                            "ipv6.method",
                            "disabled",
                        ],
                    )?;
                } else {
                    Root::production().replace(
                        path,
                        Some(&Root::image("auto end0\niface end0 inet dhcp\n", 0o644)),
                    )?;
                }
                let rejected = matches!(
                    serialized(|| e.stage(&p, base.revision, None)),
                    Err(ApiError::Unsupported { .. })
                );
                if backend == NetworkBackend::Networkmanager {
                    run(Tool::Nmcli, &["connection", "delete", "uuid", uuid])?;
                } else {
                    Root::production().replace(path, None)?;
                }
                ensure(rejected, "foreign_competition_accepted")?;
                ensure(
                    e.authority.load()? == base && e.status()?.pending.is_none(),
                    "foreign_refusal_mutated",
                )?;
            }
        }
        "storage-boundaries" | "sigkill" => {
            p.roles.audio = if p.roles.audio == "untagged" {
                p.roles.ndi.clone()
            } else {
                "untagged".into()
            };
            ensure(
                p.roles.audio != status.profiles.roles.audio,
                "binding_change_required",
            )?;
            let base = e.authority.load()?;
            for point in if name == "sigkill" {
                vec!["directory_fsync"]
            } else {
                vec!["write", "file_fsync", "rename", "directory_fsync"]
            } {
                p.networks[0].name = format!("{name} {point}");
                Root::production().replace("/run/ndimon-harness-storage-fault.json",Some(&Root::image(&serde_json::json!({"path":ndimon_netengine::BINDING,"point":point,"kill":name=="sigkill"}).to_string(),0o600)))?;
                let (_, _, files) = stage(e, &p)?;
                wait_limit(e, false, if name == "sigkill" { 240 } else { 155 })?;
                ensure(e.authority.load()? == base, "fault_changed_commit")?;
                restored(&files)?;
                ensure(
                    !Root::production().exists("/run/ndimon-harness-storage-fault.json")?,
                    "fault_not_reached",
                )?;
            }
        }
        "untagged-dhcp" => {
            p = parent(true);
            keep(e, &with_backend(p, backend), None)?;
        }
        "untagged-static" => {
            p = parent(false);
            keep(e, &with_backend(p, backend), None)?;
        }
        "vlan-static" | "wildcard" => {
            p = parent(true);
            p.networks.push(row(Some(20), false, false));
            p.roles.audio = "vlan-20".into();
            p.roles.ndi = "vlan-20".into();
            p.backend = Some(backend);
            p.networks[0].name = format!("Harness {name}");
            let preview = e.preview(&p, e.authority.load()?.revision)?;
            if name == "wildcard"
                && matches!(backend, NetworkBackend::Networkd | NetworkBackend::Netplan)
            {
                ensure(!preview.drift.is_empty(), "wildcard_drift_missing")?;
            }
            keep(e, &p, None)?;
            if name == "wildcard"
                && matches!(backend, NetworkBackend::Networkd | NetworkBackend::Netplan)
            {
                for interface in ["end0", "end0.20"] {
                    let selected =
                        run(Tool::Networkctl, &["status", "--no-pager", "--", interface])?;
                    ensure(
                        selected.contains(&format!(
                            "/etc/systemd/network/00-ndimon-{interface}.network"
                        )),
                        "pin_not_selected",
                    )?;
                }
                let routes: serde_json::Value = serde_json::from_str(&run(
                    Tool::Ip,
                    &["-j", "-4", "route", "show", "default"],
                )?)
                .map_err(|_| error("route_format"))?;
                ensure(
                    routes.as_array().is_some_and(|a| {
                        a.len() == 1 && a[0]["dev"] == "end0" && a[0]["metric"] == 100
                    }),
                    "metric_100_not_preserved",
                )?;
            }
        }
        "vlan-dhcp" => {
            p = parent(false);
            p.networks.push(row(Some(20), true, false));
            p.roles.audio = "vlan-20".into();
            p.roles.ndi = "vlan-20".into();
            keep(e, &with_backend(p, backend), None)?;
        }
        "mixed" => {
            p = parent(true);
            p.networks.push(row(Some(20), false, false));
            p.networks.push(row(Some(30), true, false));
            p.roles.audio = "vlan-20".into();
            p.roles.ndi = "vlan-30".into();
            keep(e, &with_backend(p, backend), None)?;
        }
        "all-tagged" => {
            p = parent(false);
            p.networks = vec![row(Some(20), false, true), row(Some(30), true, false)];
            p.roles.management = "vlan-20".into();
            p.roles.ndi = "vlan-30".into();
            p.roles.audio = "vlan-30".into();
            keep(e, &with_backend(p, backend), None)?;
            let live: serde_json::Value = serde_json::from_str(&run(
                Tool::Ip,
                &["-j", "-4", "address", "show", "dev", "end0"],
            )?)
            .map_err(|_| error("ip_format"))?;
            ensure(
                live.as_array().is_some_and(|a| {
                    a.iter().all(|v| {
                        v["addr_info"]
                            .as_array()
                            .is_none_or(|a| a.iter().all(|n| n["scope"] != "global"))
                    })
                }),
                "all_tagged_parent_address",
            )?;
            // Restore an untagged path for the following policy/recovery cases.
            keep(e, &with_backend(parent(true), backend), None)?;
        }
        "management-move" => {
            p = parent(false);
            p.networks[0].gateway.clear();
            p.networks[0].dns.clear();
            p.networks.push(row(Some(20), false, true));
            p.roles.management = "vlan-20".into();
            p.backend = Some(backend);
            let old = e.authority.load()?;
            let (token, _, files) = stage(e, &p)?;
            wait(e, true)?;
            ensure(e.authority.load()? == old, "early_commit")?;
            ensure(
                e.pending(token.expose())?.management_address
                    == Some("198.51.20.10".parse().map_err(|_| error("ip"))?),
                "management_move",
            )?;
            serialized(|| e.revert(token.expose()))?;
            restored(&files)?;
        }
        "keep-address-policy" => {
            p.confirm_policy = ConfirmPolicy::RequireManagementAddress;
            p.networks[0].name = "Strict Keep".into();
            let (token, _, _) = stage(e, &p)?;
            wait(e, true)?;
            ensure(
                socket_confirm(&token, "127.0.0.1".parse().map_err(|_| error("ip"))?)? == 409,
                "wrong_address_kind",
            )?;
            let address = e
                .pending(token.expose())?
                .management_address
                .ok_or_else(|| error("management_address"))?;
            ensure(
                socket_confirm(&token, IpAddr::V4(address))? == 200,
                "socket_keep",
            )?;
        }
        "keep-token-policy" => {
            p.confirm_policy = ConfirmPolicy::TokenOnly;
            p.networks[0].name = "Token Keep".into();
            let revision = e.authority.load()?.revision;
            let (token, _, _) = stage(e, &p)?;
            wait(e, true)?;
            ensure(
                socket_confirm(&token, "127.0.0.1".parse().map_err(|_| error("ip"))?)? == 200,
                "token_only_socket_keep",
            )?;
            ensure(
                e.authority.load()?.revision == revision + 1,
                "socket_revision",
            )?;
        }
        // NI-17 (HW-3 D14, D15): a second change from a state the engine produced
        // itself (the first one was kept), then left to expire or reverted by hand.
        // Whatever NetworkManager and its Netplan writer do to the profile's files
        // meanwhile, the recovery must verify, and the media must run afterwards.
        "keep-expiry" | "keep-revert" => {
            // The guest's media are stand-ins that nothing starts; the engine
            // restores only what ran when it planned the change.
            for unit in ndimon_netengine::backend::MEDIA {
                run(Tool::Systemctl, &["start", unit])?;
            }
            let management = p
                .networks
                .iter()
                .position(|n| n.id == p.roles.management)
                .ok_or_else(|| error("management_row"))?;
            let first = vec![format!("first-{name}.example")];
            p.networks[management].dns_search = first.clone();
            keep(e, &p, None)?;
            let mut next = e.status()?.profiles;
            next.networks[management].dns_search = vec![format!("second-{name}.example")];
            let (token, _, files) = stage(e, &next)?;
            wait(e, true)?;
            if name == "keep-expiry" {
                wait(e, false)?;
                ensure(
                    matches!(e.pending(token.expose()), Err(ApiError::Expired)),
                    "expired_kind",
                )?;
            } else {
                serialized(|| e.revert(token.expose()))?;
            }
            ensure(e.status()?.pending.is_none(), "recovery_pending")?;
            ensure(
                e.authority.load()?.profiles.networks[management].dns_search == first,
                "second_change_committed",
            )?;
            restored(&files)?;
            for unit in ndimon_netengine::backend::MEDIA {
                run(Tool::Systemctl, &["is-active", unit])?;
            }
        }
        "revert" | "expiry" | "apply-failure" | "recovery-retry" | "stale-job" => {
            p.networks[0].name = format!("Harness {name}");
            let base = e.authority.load()?;
            if name == "recovery-retry" {
                // See keep-expiry: start the stand-ins so there is something to keep running.
                for unit in ndimon_netengine::backend::MEDIA {
                    run(Tool::Systemctl, &["start", unit])?;
                }
            }
            if name == "apply-failure" {
                fault(backend, 1)?;
            }
            let (token, job, files) = stage(e, &p)?;
            if name == "apply-failure" {
                wait(e, false)?;
                ensure(e.status()?.last_error.is_some(), "missing_apply_error")?;
            } else {
                wait(e, true)?;
                match name {
                    "expiry" => {
                        let journal: serde_json::Value = Store {
                            root: Root::production(),
                        }
                        .read("pending")?
                        .ok_or_else(|| error("missing_journal"))?;
                        let applied = journal["applied_ms"]
                            .as_u64()
                            .ok_or_else(|| error("applied_time"))?;
                        let deadline = journal["deadline_ms"]
                            .as_u64()
                            .ok_or_else(|| error("deadline"))?;
                        ensure(
                            deadline.checked_sub(applied) == Some(60_000),
                            "window_not_60000ms",
                        )?;
                        wait(e, false)?;
                        let recovered = LinuxClock.now_ms()?;
                        ensure(recovered >= deadline, "expiry_too_early")?;
                        println!(
                            "durable applied_ms={applied} deadline_ms={deadline} recovered_ms={recovered}"
                        );
                        ensure(
                            matches!(e.pending(token.expose()), Err(ApiError::Expired)),
                            "expired_kind",
                        )?;
                    }
                    "recovery-retry" => {
                        fault(backend, 2)?;
                        ensure(
                            serialized(|| e.revert(token.expose())).is_err(),
                            "recovery_fault_not_observed",
                        )?;
                        ensure(
                            e.status()?.pending.is_some_and(|p| p.recovery_pending),
                            "missing_recovery_pending",
                        )?;
                        // NI-17 D14: a recovery that has not finished leaves the
                        // media running, and the retries do not stop them.
                        for unit in ndimon_netengine::backend::MEDIA {
                            run(Tool::Systemctl, &["is-active", unit])?;
                        }
                        wait(e, false)?;
                        for unit in ndimon_netengine::backend::MEDIA {
                            run(Tool::Systemctl, &["is-active", unit])?;
                        }
                    }
                    "stale-job" => {
                        serialized(|| e.revert(token.expose()))?;
                        p.networks[0].name = "Fresh transaction".into();
                        let (new, _, _) = stage(e, &p)?;
                        wait(e, true)?;
                        let before = e.pending(new.expose())?;
                        // A timer can race another short operation. Exercise
                        // the actual worker's bounded lock retry, then verify
                        // that the stale identity cannot touch this journal.
                        let held = Store {
                            root: Root::production(),
                        }
                        .lock()?;
                        let mut worker = Command::new(ndimon_netengine::PRIVD)
                            .args(["network-job", "apply", &job])
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn()
                            .map_err(|_| error("contention_worker"))?;
                        std::thread::sleep(Duration::from_millis(250));
                        let waiting = worker
                            .try_wait()
                            .map_err(|_| error("contention_worker"))?
                            .is_none();
                        drop(held);
                        let status = worker.wait().map_err(|_| error("contention_worker"))?;
                        ensure(
                            waiting && status.success(),
                            "worker_did_not_retry_contention",
                        )?;
                        e.run_apply_job(&job)?;
                        e.run_recovery(&job)?;
                        ensure(
                            e.pending(new.expose())?.job_id == before.job_id,
                            "stale_job_mutated_new",
                        )?;
                        serialized(|| e.revert(new.expose()))?;
                    }
                    _ => serialized(|| e.revert(token.expose()))?,
                }
            }
            ensure(e.authority.load()? == base, "recovery_changed_revision")?;
            restored(&files)?;
        }
        "legacy-upgrade" | "python-upgrade" => {
            let base = e.authority.load()?;
            let root = Root::production();
            let previous = ndimon_privd::network::legacy_profiles(&base.profiles, 77);
            root.replace(
                "/var/lib/ndimon-network/committed.json",
                Some(&Root::image(&previous.to_string(), 0o600)),
            )?;
            let mut requested = previous.clone();
            requested["networks"][0]["name"] = serde_json::json!("Python upgrade candidate");
            // Historical v1 applied journal, constructed from exact before-images.
            // The retired interpreter is not needed to verify Rust recovery.
            let token = Secret::generate()?;
            let mut snapshots = serde_json::Map::new();
            for path in [ndimon_netengine::BINDING, ndimon_netengine::TESSERA_BINDING] {
                let before = root.snapshot(path)?;
                let saved = match before {
                    Some(image) => serde_json::json!([
                        String::from_utf8(image.bytes).map_err(|_| error("snapshot_utf8"))?,
                        image.mode
                    ]),
                    None => serde_json::Value::Null,
                };
                snapshots.insert(path.into(), saved);
            }
            let pending = serde_json::json!({"version":1,"token":token,"phase":"applied","expires":0.0,"boot":"legacy-boot",
                "previous":previous,"requested":requested,"files":snapshots,"nm":{},"address":"192.0.2.10","running":[],"nft_present":false,"nft_previous":""});
            Store { root: root.clone() }.write("pending", &pending)?;
            root.replace(
                ndimon_netengine::BINDING,
                Some(&Root::image("legacy candidate", 0o640)),
            )?;
            e.boot()?;
            let current = e.authority.load()?;
            ensure(
                current.profiles == base.profiles && current.revision == base.revision + 1,
                "upgrade_decision",
            )?;
            ensure(e.status()?.pending.is_none(), "upgrade_pending")?;
            ensure(
                matches!(
                    e.confirm(token.expose(), None),
                    Err(ApiError::NotPending) | Err(ApiError::Expired)
                ),
                "python_token_retired",
            )?;
            ensure(
                !root.exists("/var/lib/ndimon-network/committed.json")?,
                "legacy_authority_retired",
            )?;
            for (path, before) in snapshots {
                let actual = root.snapshot(&path)?;
                if before.is_null() {
                    ensure(actual.is_none(), "upgrade_prior_absence")?;
                } else {
                    let actual = actual.ok_or_else(|| error("upgrade_snapshot_missing"))?;
                    ensure(
                        actual.bytes
                            == before[0]
                                .as_str()
                                .ok_or_else(|| error("snapshot_format"))?
                                .as_bytes()
                            && u64::from(actual.mode)
                                == before[1].as_u64().ok_or_else(|| error("snapshot_format"))?,
                        "upgrade_snapshot_mismatch",
                    )?;
                }
            }
        }
        "reboot-stage" => {
            p.networks[0].name = "Unconfirmed reboot".into();
            let base = e.authority.load()?;
            let (token, _, files) = stage(e, &p)?;
            wait(e, true)?;
            let saved = RebootCase {
                token,
                boot: LinuxClock.boot_id()?,
                base,
                files,
            };
            Root::production().replace(
                "/var/lib/ndimon-harness-reboot.json",
                Some(&Root::image(
                    &serde_json::to_string(&saved).map_err(|_| error("reboot_format"))?,
                    0o600,
                )),
            )?;
        }
        "reboot-verify" => {
            let saved: RebootCase = serde_json::from_str(
                &Root::production()
                    .text("/var/lib/ndimon-harness-reboot.json")?
                    .ok_or_else(|| error("reboot_case_missing"))?,
            )
            .map_err(|_| error("reboot_format"))?;
            ensure(saved.boot != LinuxClock.boot_id()?, "boot_id_unchanged")?;
            ensure(e.authority.load()? == saved.base, "reboot_changed_commit")?;
            ensure(e.status()?.pending.is_none(), "reboot_left_pending")?;
            restored(&saved.files)?;
            boot_order()?;
            run(Tool::Nft, &["list", "table", "inet", "ndimon_management"])?;
            ensure(
                matches!(
                    e.confirm(saved.token.expose(), None),
                    Err(ApiError::Expired)
                ),
                "stale_boot_keep",
            )?;
        }
        _ => return Err(error("unknown_scenario")),
    }
    println!("PASS {backend:?} {name}");
    Ok(())
}
fn boot_order() -> Result<()> {
    let boot = run(
        Tool::Systemctl,
        &[
            "show",
            "--property=ActiveEnterTimestampMonotonic",
            "--value",
            "ndimon-net-boot.service",
        ],
    )?;
    let boot = boot
        .trim()
        .parse::<u64>()
        .map_err(|_| error("boot_timestamp"))?;
    ensure(boot > 0, "boot_not_active")?;
    for unit in [
        "ndimon-control.service",
        "ndimon-r.service",
        "ndimon-finder.service",
        "tesserad.service",
        "ndimon-harness-portal.service",
    ] {
        run(Tool::Systemctl, &["is-active", "--quiet", unit])?;
        let entered = run(
            Tool::Systemctl,
            &[
                "show",
                "--property=ActiveEnterTimestampMonotonic",
                "--value",
                unit,
            ],
        )?;
        ensure(
            entered
                .trim()
                .parse::<u64>()
                .map_err(|_| error("consumer_timestamp"))?
                >= boot,
            "consumer_preceded_recovery",
        )?;
    }
    Ok(())
}
fn with_backend(mut p: NetworkProfiles, b: NetworkBackend) -> NetworkProfiles {
    p.backend = Some(b);
    p
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let e = engine();
    let result = match args.first().map(String::as_str) {
        Some("initialize") => (|| {
            let p = ndimon_state::persist::Persistence::new("/var/lib/ndimon/state")
                .map_err(|_| error("state_init"))?;
            if p.load().map_err(|_| error("state_init"))?.is_none() {
                p.initialize(&ndimon_state::persist::StateFile::new(
                    ndimon_types::DesiredState::default(),
                    Default::default(),
                ))
                .map_err(|_| error("state_init"))?;
            }
            Ok(())
        })(),
        Some("network-job") => match args.get(1).map(String::as_str) {
            Some("boot") => e.boot(),
            Some("apply") => e.run_apply_job(args.get(2).map(String::as_str).unwrap_or("")),
            Some("recover") => e.run_recovery(args.get(2).map(String::as_str).unwrap_or("")),
            _ => Err(error("job_usage")),
        },
        Some("scenario") => scenario(&e, args.get(1).map(String::as_str).unwrap_or("")),
        Some("udp-echo") => ports::echo(args.get(1).map(String::as_str).unwrap_or("")),
        Some("udp-probe") => ports::probe(
            args.get(1).map(String::as_str).unwrap_or(""),
            args.get(2).map(String::as_str).unwrap_or(""),
        ),
        Some("confirm-saved") => (|| {
            let token: Secret = serde_json::from_str(
                &Root::production()
                    .text("/run/ndimon-harness-token.json")?
                    .ok_or_else(|| error("token_missing"))?,
            )
            .map_err(|_| error("token_format"))?;
            serialized(|| e.confirm(token.expose(), None)).map(|_| ())
        })(),
        Some("status") => e.status().and_then(|s| {
            println!(
                "{}",
                serde_json::to_string(&s).map_err(|_| error("status_format"))?
            );
            Ok(())
        }),
        Some("ready") => (|| {
            run(
                Tool::Systemctl,
                &["is-active", "--quiet", "ndimon-net-boot.service"],
            )?;
            let _stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, 80))
                .map_err(|_| error("listener_not_ready"))?;
            e.status()?;
            Ok(())
        })(),
        Some("serve") => serve(&e),
        _ => Err(error("usage")),
    };
    if let Err(error) = result {
        eprintln!("FAIL {error:?}");
        // A failed transaction keeps why it failed and why recovery did not finish.
        if let Some(pending) = e.status().ok().and_then(|s| s.pending) {
            eprintln!(
                "PENDING phase={:?} apply_error={:?} recovery_error={:?}",
                pending.phase, pending.apply_error, pending.recovery_error
            );
        }
        // The host as it is: what NetworkManager, the kernel and the files say.
        for (program, args) in [
            (
                "nmcli",
                &[
                    "-t",
                    "-f",
                    "NAME,UUID,DEVICE,FILENAME",
                    "connection",
                    "show",
                ][..],
            ),
            ("nmcli", &["-t", "device", "status"][..]),
            ("ip", &["-4", "-brief", "address"][..]),
            (
                "ls",
                &[
                    "-l",
                    "/etc/netplan",
                    "/etc/NetworkManager/system-connections",
                    "/run/NetworkManager/system-connections",
                ][..],
            ),
            (
                "grep",
                &[
                    "-r",
                    "-H",
                    "-E",
                    "^(id|uuid|method|address1|interface-name)=",
                    "/etc/NetworkManager/system-connections",
                    "/run/NetworkManager/system-connections",
                ][..],
            ),
        ] {
            if let Ok(output) = Command::new(program).args(args).output() {
                eprintln!(
                    "DIAG {program} {args:?}\n{}",
                    String::from_utf8_lossy(&output.stdout)
                );
            }
        }
        std::process::exit(1);
    }
}
