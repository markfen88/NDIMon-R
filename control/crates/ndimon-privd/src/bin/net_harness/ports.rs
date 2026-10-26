//! Live replacements for the retired guest_network_* Python scripts: packet
//! restrictions, timers, profile lifecycle and the Tessera daemon on a media VLAN.
use super::*;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicU32, Ordering};

/// Requests for actions only the peer side can take are files in a directory
/// of the guest's root file system; the outer driver answers them from the peer
/// namespace (see xtask's net_harness). The answer is `{"ok":bool,"detail":..}`.
const GATE: &str = "/var/lib/ndimon-harness/gate";
pub fn peer(request: serde_json::Value, seconds: u64) -> Result<serde_json::Value> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let id = format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let gate = std::path::Path::new(GATE);
    let temporary = gate.join(format!("{id}.tmp"));
    std::fs::write(
        &temporary,
        serde_json::to_vec(&request).map_err(|_| error("peer_request_format"))?,
    )
    .map_err(|_| error("peer_request_write"))?;
    std::fs::rename(&temporary, gate.join(format!("{id}.req")))
        .map_err(|_| error("peer_request_publish"))?;
    let answer = gate.join(format!("{id}.res"));
    let start = Instant::now();
    loop {
        match std::fs::read(&answer) {
            Ok(bytes) => {
                let _ = std::fs::remove_file(&answer);
                let value: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|_| error("peer_answer_format"))?;
                ensure(
                    value["ok"] == true,
                    &format!(
                        "peer_refused:{}",
                        redact(value["detail"].as_str().unwrap_or(""))
                    ),
                )?;
                return Ok(value);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(error("peer_answer_read")),
        }
        ensure(
            start.elapsed() < Duration::from_secs(seconds),
            "peer_timeout",
        )?;
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn echo(address: &str) -> Result<()> {
    let socket = UdpSocket::bind(address).map_err(|_| error("echo_bind"))?;
    let mut data = [0; 256];
    loop {
        let (length, peer) = socket
            .recv_from(&mut data)
            .map_err(|_| error("echo_receive"))?;
        socket
            .send_to(&data[..length], peer)
            .map_err(|_| error("echo_reply"))?;
    }
}
pub fn probe(mode: &str, address: &str) -> Result<()> {
    ensure(matches!(mode, "allow" | "deny"), "probe_usage")?;
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|_| error("probe_bind"))?;
    socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(|_| error("probe_timeout"))?;
    let allowed = socket
        .send_to(b"interface-proof", address)
        .and_then(|_| {
            let mut data = [0; 256];
            let (length, _) = socket.recv_from(&mut data)?;
            Ok(&data[..length] == b"interface-proof")
        })
        .unwrap_or(false);
    ensure(
        allowed == (mode == "allow"),
        "interface_restriction_not_enforced",
    )
}
fn restricted_probe(mode: &str, destination: &str) -> Result<()> {
    run(
        Tool::SystemdRun,
        &[
            "--quiet",
            "--wait",
            "--pipe",
            "--collect",
            "--slice=ndimonndi.slice",
            &format!(
                "--unit=ndimon-packet-proof-{}-{}",
                std::process::id(),
                destination.replace(['.', ':'], "-")
            ),
            "/usr/libexec/ndimon/ndimon-net-harness",
            "udp-probe",
            mode,
            destination,
        ],
    )?;
    ensure(
        std::path::Path::new("/sys/fs/cgroup/ndimonndi.slice").exists(),
        "parent_slice_disappeared",
    )
}
fn rules(text: &str) -> Result<()> {
    let mut child = Command::new("nft")
        .args(["-f", "-"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|_| error("nft_spawn"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| error("nft_stdin"))?
        .write_all(text.as_bytes())
        .map_err(|_| error("nft_stdin"))?;
    ensure(
        child.wait().map_err(|_| error("nft_wait"))?.success(),
        "nft_rules",
    )
}
pub fn restrictions(e: &Engine, backend: NetworkBackend) -> Result<()> {
    let previous = e.status()?.profiles;
    let mut p = parent(false);
    p.backend = Some(backend);
    p.networks
        .extend([row(Some(20), false, false), row(Some(30), false, false)]);
    p.roles.ndi = "vlan-20".into();
    p.roles.audio = "vlan-30".into();
    keep(e, &p, None)?;
    let normal = ndimon_netengine::backend::firewall(&p, 80)?;
    let result = (|| {
        restricted_probe("allow", "198.51.20.1:43001")?;
        restricted_probe("deny", "203.0.113.1:43001")?;
        // Keep egress allowed while denying only ingress on the selected VLAN.
        let needle = "iifname != { \"lo\", \"end0.20\" }";
        ensure(normal.contains(needle), "missing_input_guard")?;
        rules(&format!(
            "delete table inet ndimon_management\n{}",
            normal.replace(needle, "iifname != { \"lo\" }")
        ))?;
        restricted_probe("deny", "198.51.20.1:43001")
    })();
    let restore = rules(&format!("delete table inet ndimon_management\n{normal}"));
    result?;
    restore?;
    keep(e, &previous, None)
}
pub fn timers(e: &Engine) -> Result<()> {
    let mut p = e.status()?.profiles;
    p.networks[0].name = "Timer property proof".into();
    let (token, job, _) = stage(e, &p)?;
    let result = (|| {
        let properties = run(
            Tool::Systemctl,
            &[
                "show",
                &format!("ndimon-network-apply-{job}.service"),
                "--property=RuntimeMaxUSec",
                "--property=TimeoutStopUSec",
            ],
        )?;
        ensure(
            properties
                .lines()
                .any(|s| matches!(s, "RuntimeMaxUSec=1min 30s" | "RuntimeMaxUSec=90s")),
            "apply_runtime_not_90s",
        )?;
        // NE-6 replaced the Python helper's 90-second stop grace with a bounded
        // two-second stop, leaving the independent guard free to recover.
        ensure(
            properties.lines().any(|s| s == "TimeoutStopUSec=2s"),
            "apply_stop_timeout_not_2s",
        )?;
        wait(e, true)
    })();
    let revert = serialized(|| e.revert(token.expose()));
    result?;
    revert
}

pub fn profiles(e: &Engine, backend: NetworkBackend) -> Result<()> {
    let mut p = parent(false);
    p.backend = Some(backend);
    p.networks
        .extend([row(Some(20), false, false), row(Some(30), false, false)]);
    p.roles.ndi = "vlan-20".into();
    p.roles.audio = "vlan-30".into();
    keep(e, &p, None)?;
    let root = Root::production();
    let binding: serde_json::Value = serde_json::from_str(
        &root
            .text(ndimon_netengine::BINDING)?
            .ok_or_else(|| error("binding_missing"))?,
    )
    .map_err(|_| error("binding_format"))?;
    ensure(
        binding["ndi"] == "end0.20" && binding["tessera"] == "end0.30",
        "role_binding",
    )?;
    for address in ["198.51.20.1:43001", "203.0.113.1:43001"] {
        probe("allow", address)?;
    }
    let accepted = e.authority.load()?;
    let mut changed = p.clone();
    changed.networks[1].address = "198.51.20.11".into();
    let (token, _, files) = stage(e, &changed)?;
    wait(e, true)?;
    // The peer, not the guest, initiates: the replacement address is reachable
    // from the media VLAN while the change is still unconfirmed.
    peer(serde_json::json!({"ping":"198.51.20.11"}), 30)?;
    let addresses = || -> Result<Vec<String>> {
        let value: serde_json::Value = serde_json::from_str(&run(
            Tool::Ip,
            &["-j", "-4", "address", "show", "dev", "end0.20"],
        )?)
        .map_err(|_| error("address_format"))?;
        Ok(value
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|row| row["addr_info"].as_array().into_iter().flatten())
            .filter_map(|row| row["local"].as_str().map(str::to_owned))
            .collect())
    };
    ensure(
        addresses()? == ["198.51.20.11"],
        "stale_vlan_address_retained",
    )?;
    serialized(|| e.revert(token.expose()))?;
    ensure(
        addresses()? == ["198.51.20.10"],
        "prior_vlan_address_not_restored",
    )?;
    restored(&files)?;
    ensure(e.authority.load()? == accepted, "revert_changed_authority")?;
    let (_token, _, _) = stage(e, &changed)?;
    wait(e, true)?;
    e.boot()?;
    ensure(
        e.status()?.pending.is_none() && e.authority.load()? == accepted,
        "boot_did_not_restore_accepted_profiles",
    )
}

/// Apply `p` and Keep it, unless it is already the accepted profile set.
fn ensure_profiles(e: &Engine, p: &NetworkProfiles) -> Result<()> {
    let revision = e.authority.load()?.revision;
    let staged = serialized(|| e.stage(p, revision, None))?;
    let Some(token) = staged.token else {
        return Ok(());
    };
    wait(e, true)?;
    serialized(|| e.confirm(token.expose(), None))?;
    ensure(
        e.authority.load()?.revision == revision + 1,
        "revision_increment",
    )
}

/// A Tessera daemon from the unmodified binaries of a separate Tessera build,
/// killed when dropped.
struct Daemon {
    child: std::process::Child,
    socket: String,
    log: String,
}
impl Daemon {
    fn start(directory: &str, interface: &str, role: &str) -> Result<Self> {
        let io = |_: std::io::Error| error("tessera_directory");
        std::fs::create_dir_all(directory).map_err(io)?;
        std::fs::write(
            format!("{directory}/config.toml"),
            format!(
                "[daemon]\nrealtime = false\n[network]\ninterfaces = [\"{interface}\"]\nlocked = [\"primary\"]\n[clock]\nrole = \"{role}\"\ntimestamping = \"software\"\n"
            ),
        )
        .map_err(io)?;
        let log = format!("{directory}/daemon.log");
        let file = std::fs::File::create(&log).map_err(io)?;
        let child = Command::new("/usr/bin/tesserad")
            .args(["--config-dir", directory])
            .args(["--state-dir", &format!("{directory}/state")])
            .args(["--runtime-dir", &format!("{directory}/run")])
            .stdout(file.try_clone().map_err(io)?)
            .stderr(file)
            .spawn()
            .map_err(|_| error("tessera_spawn"))?;
        Ok(Self {
            child,
            socket: format!("{directory}/run/control.sock"),
            log,
        })
    }
    /// `tesseractl --json ARGS`, retried while the daemon is still starting.
    fn control(&mut self, args: &[&str], seconds: u64) -> Result<String> {
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().map_err(|_| error("tessera_wait"))? {
                let log = std::fs::read_to_string(&self.log).unwrap_or_default();
                let tail = log
                    .char_indices()
                    .rev()
                    .nth(399)
                    .map_or(log.as_str(), |(at, _)| &log[at..]);
                return Err(error(&format!("tessera_exited:{status}:{}", redact(tail))));
            }
            let output = Command::new("/usr/bin/tesseractl")
                .args(["--socket", &self.socket, "--json"])
                .args(args)
                .output()
                .map_err(|_| error("tessera_cli"))?;
            if output.status.success() {
                return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
            }
            ensure(
                start.elapsed() < Duration::from_secs(seconds),
                &format!(
                    "tessera_control:{}",
                    redact(&String::from_utf8_lossy(&output.stderr))
                ),
            )?;
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The actual Tessera daemon, pinned to the audio role's media VLAN, reports
/// that interface as locked; a second daemon on the matching tagged network in
/// the peer namespace becomes a locked clock follower. (A UDP echo would not
/// exercise either.)
pub fn tessera(e: &Engine, backend: NetworkBackend) -> Result<()> {
    let mut p = parent(false);
    p.backend = Some(backend);
    p.networks
        .extend([row(Some(20), false, false), row(Some(30), false, false)]);
    p.roles.ndi = "vlan-20".into();
    p.roles.audio = "vlan-30".into();
    ensure_profiles(e, &p)?;
    let mut local = Daemon::start("/run/ndimon-harness/tessera-local", "end0.30", "leader")?;
    let network = local.control(&["network", "show"], 30)?;
    ensure(
        network.contains("end0.30"),
        &format!("tessera_vlan_not_reported:{}", redact(&network)),
    )?;
    ensure(
        network.to_lowercase().contains("locked"),
        &format!("tessera_interface_not_locked:{}", redact(&network)),
    )?;
    println!("tessera network: {}", network.trim());
    let clock = peer(
        serde_json::json!({"tessera_clock_locked":{"timeout":30}}),
        60,
    )?;
    println!(
        "tessera peer clock: {}",
        clock["detail"].as_str().unwrap_or("")
    );
    Ok(())
}
