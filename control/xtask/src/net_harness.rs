//! Root-only disposable nspawn lab. Never assigns an address in the host netns.
use super::Result;
use std::fs::{self, File};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BACKENDS: &[&str] = &["networkd", "netplan", "networkmanager", "ifupdown"];
/// NetworkManager with a Netplan-generated profile, the way Raspberry Pi OS ships
/// it (HW-2). Selected by name only: `all` stays the four backends.
const NM_NETPLAN: &str = "networkmanager-netplan";
/// The board's own file (HW-1), not a simplified one.
const ARMBIAN_NETPLAN: &str = include_str!(
    "../../crates/ndimon-netengine/tests/fixtures/armbian-10-dhcp-all-interfaces.yaml"
);
fn command(executable: &str, args: &[&str]) -> Result {
    let status = Command::new(executable).args(args).status()?;
    if !status.success() {
        return Err(format!("{executable} {args:?}: {status}").into());
    }
    Ok(())
}
fn output(executable: &str, args: &[&str]) -> Result<String> {
    let o = Command::new(executable).args(args).output()?;
    if !o.status.success() {
        return Err(format!(
            "{executable} {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(o.stdout)?)
}
fn write(root: &Path, path: &str, text: &str, mode: u32) -> Result {
    let path = root.join(path.trim_start_matches('/'));
    fs::create_dir_all(path.parent().ok_or("parent")?)?;
    fs::write(&path, text)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}
fn enable(root: &Path, unit: &str) -> Result {
    let path = root
        .join("etc/systemd/system/multi-user.target.wants")
        .join(unit);
    fs::create_dir_all(path.parent().ok_or("parent")?)?;
    if !path.symlink_metadata().is_ok() {
        symlink(format!("/usr/lib/systemd/system/{unit}"), path)?;
    }
    Ok(())
}
fn image_name(backend: &str) -> String {
    if backend == "netplan" {
        "ubuntu2604-netplan".into()
    } else if backend == NM_NETPLAN {
        "debian13-networkmanager-netplan".into()
    } else {
        format!("debian13-{backend}")
    }
}
pub fn run(workspace: &Path, args: &[String]) -> Result {
    if output("id", &["-u"])?.trim() != "0" {
        return Err("run the built xtask as root on the authorized build host".into());
    }
    let action = args.first().map(String::as_str).ok_or(
        "usage: net-harness <images|run> <images-directory> <work-directory> <backend|all>",
    )?;
    let images = PathBuf::from(args.get(1).ok_or("images directory")?);
    let work = PathBuf::from(args.get(2).ok_or("work directory")?);
    let selected = args.get(3).map(String::as_str).unwrap_or("all");
    if !images.is_absolute()
        || !work.is_absolute()
        || images == Path::new("/")
        || work == Path::new("/")
    {
        return Err("use absolute dedicated lab directories".into());
    }
    fs::create_dir_all(&images)?;
    fs::create_dir_all(&work)?;
    let backends = if selected == "all" {
        BACKENDS.to_vec()
    } else if BACKENDS.contains(&selected) || selected == NM_NETPLAN {
        vec![selected]
    } else {
        return Err("unknown backend".into());
    };
    for backend in backends {
        match action {
            "images" => image(&images, &work, backend)?,
            "run" => lab(
                workspace,
                &images,
                &work,
                backend,
                &args[4.min(args.len())..],
            )?,
            _ => return Err("expected images or run".into()),
        }
    }
    if action == "images" {
        images_readme(&images)?;
    }
    Ok(())
}
fn image(images: &Path, work: &Path, backend: &str) -> Result {
    let target = images.join(image_name(backend));
    if target.join(".n4-image-ready").exists() {
        println!("REUSE {}", target.display());
        return Ok(());
    }
    let common = "systemd-sysv,dbus,iproute2,nftables,iputils-arping,kmod,ca-certificates";
    let include = match backend {
        "netplan" => format!("{common},netplan.io"),
        "networkmanager" => format!("{common},network-manager"),
        NM_NETPLAN => format!("{common},network-manager,netplan.io"),
        "ifupdown" => format!("{common},ifupdown,isc-dhcp-client,vlan"),
        _ => common.into(),
    };
    let mut cmd = Command::new("mmdebstrap");
    cmd.args(["--variant=minbase", &format!("--include={include}")]);
    if backend == "netplan" {
        let deb = work.join("ubuntu-keyring.deb");
        let keys = work.join("ubuntu-keyring");
        command(
            "curl",
            &[
                "-fsSL",
                "https://archive.ubuntu.com/ubuntu/pool/main/u/ubuntu-keyring/ubuntu-keyring_2026.08.18_all.deb",
                "-o",
                deb.to_str().ok_or("path")?,
            ],
        )?;
        command(
            "dpkg-deb",
            &[
                "-x",
                deb.to_str().ok_or("path")?,
                keys.to_str().ok_or("path")?,
            ],
        )?;
        cmd.args([
            format!(
                "--keyring={}",
                keys.join("usr/share/keyrings/ubuntu-archive-keyring.gpg")
                    .display()
            ),
            "--components=main,universe".into(),
            "resolute".into(),
            target.to_string_lossy().into_owned(),
            "https://archive.ubuntu.com/ubuntu".into(),
        ]);
    } else {
        cmd.args([
            "trixie",
            target.to_str().ok_or("path")?,
            "https://deb.debian.org/debian",
        ]);
    }
    let status = cmd.status()?;
    if !status.success() {
        return Err(format!("bootstrap {backend}: {status}").into());
    }
    configure(&target, backend)?;
    fs::write(target.join(".n4-image-ready"), backend)?;
    Ok(())
}
fn images_readme(images: &Path) -> Result {
    fs::write(
        images.join("README.md"),
        "# Disposable network-engine images\n\nDebian 13 networkd / NetworkManager / ifupdown with ISC dhclient, and Ubuntu 26.04 Netplan with networkd. Built by `cargo xtask net-harness images`. Never boot with host networking. Clone these images into a work directory and boot only with isolated veth/namespace peers. Images contain no credentials, product driver or transaction state. Install each product's feature-gated driver into the clone.\n\nThe test NIC is end0. Networkd has a neutral e* DHCP fixture; Netplan retains the Armbian-style /etc/netplan/10-dhcp-all-interfaces.yaml with all-eth-interfaces matching e*. NM has the explicit fixture UUID 11111111-1111-4111-8111-111111111111 and metric 100. The ifupdown first-adoption fixture is /etc/network/interfaces.d/ndimon-end0, included from interfaces: a product with its own owned stanza contract, such as standalone Tessera, must replace or rename this disposable fixture before testing. Do not copy NDIMon journals or treat this as an ownership handoff. Ubuntu needs its private udev daemon with read-only sysfs; never bind host udev or writable host sysfs.\n\nCanonical directories: debian13-networkd, ubuntu2604-netplan, debian13-networkmanager, debian13-ifupdown. Keep these unbooted; work on clones.\n",
    )?;
    Ok(())
}
fn configure(root: &Path, backend: &str) -> Result {
    write(root, "/etc/machine-id", "", 0o644)?;
    write(root, "/etc/hostname", &format!("n4-{backend}\n"), 0o644)?;
    fs::create_dir_all(root.join("etc/tessera/config.d"))?;
    match backend {
        "networkd" => {
            write(
                root,
                "/etc/systemd/network/10-fixture.network",
                "[Match]\nName=e*\n[Link]\nRequiredForOnline=no\n[Network]\nDHCP=ipv4\nIPv6AcceptRA=no\nLinkLocalAddressing=no\n[DHCPv4]\nRouteMetric=100\nUseHostname=no\nUseNTP=no\n",
                0o644,
            )?;
            enable(root, "systemd-networkd.service")?;
        }
        "netplan" => {
            // Netplan requires udevadm control --reload, even in a container.
            // Start its PRIVATE daemon while retaining nspawn's read-only sysfs.
            for unit in [
                "systemd-udevd.service",
                "systemd-udevd-control.socket",
                "systemd-udevd-kernel.socket",
                "systemd-udevd-varlink.socket",
            ] {
                write(
                    root,
                    &format!("/etc/systemd/system/{unit}.d/10-harness.conf"),
                    "[Unit]\nConditionPathIsReadWrite=\n",
                    0o644,
                )?;
            }
            enable(root, "systemd-udevd.service")?;
            write(
                root,
                "/etc/netplan/10-dhcp-all-interfaces.yaml",
                ARMBIAN_NETPLAN,
                0o600,
            )?;
            enable(root, "systemd-networkd.service")?;
        }
        NM_NETPLAN => {
            // A profile the Netplan generator wrote into /run, as on Raspberry Pi OS:
            // `netplan-<id>`, no interface binding, DHCP, IPv6 auto with
            // `ip6-privacy=0` (the recorded keyfile). Pi OS's NetworkManager writes
            // its changes back into Netplan YAML, so regenerating gives the changed
            // profile; this image's NetworkManager has no such plugin, so every
            // `systemctl daemon-reload` would regenerate the original and shadow the
            // saved one. The profile is therefore placed once at boot and the
            // generator is masked (a /dev/null symlink in a higher-priority directory).
            write(
                root,
                "/etc/tmpfiles.d/ndimon-harness-netplan.conf",
                "d /run/NetworkManager/system-connections 0755 root root -\nf /run/NetworkManager/system-connections/netplan-end0.nmconnection 0600 root root - [connection]\\nid=netplan-end0\\ntype=ethernet\\nuuid=75a1216a-9d1a-30cd-8aca-ace5526ec021\\n\\n[ethernet]\\nwake-on-lan=0\\n\\n[ipv4]\\nmethod=auto\\n\\n[ipv6]\\nmethod=auto\\nip6-privacy=0\\n\\n[proxy]\\n\n",
                0o644,
            )?;
            let generators = root.join("etc/systemd/system-generators");
            fs::create_dir_all(&generators)?;
            if generators.join("netplan").symlink_metadata().is_err() {
                symlink("/dev/null", generators.join("netplan"))?;
            }
            write(
                root,
                "/etc/NetworkManager/conf.d/10-harness.conf",
                "[main]\nno-auto-default=*\n",
                0o644,
            )?;
            // The image started as a copy of the plain NetworkManager one, whose
            // explicit profile would be a second, competing connection.
            match fs::remove_file(
                root.join("etc/NetworkManager/system-connections/fixture.nmconnection"),
            ) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => (),
            }
            enable(root, "NetworkManager.service")?;
        }
        "networkmanager" => {
            write(
                root,
                "/etc/NetworkManager/system-connections/fixture.nmconnection",
                "[connection]\nid=fixture\nuuid=11111111-1111-4111-8111-111111111111\ntype=ethernet\ninterface-name=end0\nautoconnect=true\n[ethernet]\n[ipv4]\nmethod=auto\nroute-metric=100\n[ipv6]\nmethod=disabled\n",
                0o600,
            )?;
            enable(root, "NetworkManager.service")?;
        }
        "ifupdown" => {
            write(
                root,
                "/etc/network/interfaces",
                "auto lo\niface lo inet loopback\nsource /etc/network/interfaces.d/*\n",
                0o644,
            )?;
            write(
                root,
                "/etc/network/interfaces.d/ndimon-end0",
                "auto end0\niface end0 inet dhcp\n    metric 100\n",
                0o644,
            )?;
            enable(root, "networking.service")?;
        }
        _ => return Err("backend".into()),
    }
    Ok(())
}
/// NE-2.12a fixture for the `search-*` scenarios: the management network the way
/// a cloud image or installer leaves it, static with a gateway, a DNS server and
/// search domains, instead of the neutral DHCP fixture. The Netplan file has the
/// stock cloud-init layout of VV-1 but matches by name: nspawn's read-only sysfs
/// makes netplan's udev retrigger fail for a MAC match or set-name, so those are
/// covered by the unit tests, not here.
fn configure_search(root: &Path, backend: &str) -> Result {
    match backend {
        "networkd" => write(
            root,
            "/etc/systemd/network/10-fixture.network",
            "[Match]\nName=e*\n[Link]\nRequiredForOnline=no\n[Network]\nDHCP=no\nAddress=192.0.2.10/24\nGateway=192.0.2.1\nDNS=192.0.2.1\nDomains=lab.example corp.example\nIPv6AcceptRA=no\nLinkLocalAddressing=no\n",
            0o644,
        )?,
        "netplan" => {
            fs::remove_file(root.join("etc/netplan/10-dhcp-all-interfaces.yaml"))?;
            write(
                root,
                "/etc/netplan/50-cloud-init.yaml",
                "network:\n  version: 2\n  ethernets:\n    end0:\n      match:\n        name: 'e*'\n      addresses:\n      - 192.0.2.10/24\n      routes:\n      - to: default\n        via: 192.0.2.1\n      nameservers:\n        addresses:\n        - 192.0.2.1\n        search:\n        - lab.example\n        - corp.example\n",
                0o600,
            )?;
        }
        "networkmanager" => write(
            root,
            "/etc/NetworkManager/system-connections/fixture.nmconnection",
            "[connection]\nid=fixture\nuuid=11111111-1111-4111-8111-111111111111\ntype=ethernet\ninterface-name=end0\nautoconnect=true\n[ethernet]\n[ipv4]\nmethod=manual\naddress1=192.0.2.10/24,192.0.2.1\ndns=192.0.2.1;\ndns-search=lab.example;corp.example;\nroute-metric=100\n[ipv6]\nmethod=disabled\n",
            0o600,
        )?,
        "ifupdown" => write(
            root,
            "/etc/network/interfaces.d/ndimon-end0",
            "auto end0\niface end0 inet static\n    address 192.0.2.10/24\n    gateway 192.0.2.1\n    dns-nameservers 192.0.2.1\n    dns-search lab.example corp.example\n    metric 100\n",
            0o644,
        )?,
        _ => return Err("backend".into()),
    }
    Ok(())
}
struct Lab {
    machine: String,
    namespace: String,
    link: String,
    container: Option<Child>,
    dhcp: Option<Child>,
    probes: Vec<Child>,
}
impl Drop for Lab {
    fn drop(&mut self) {
        for child in &mut self.probes {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = Command::new("machinectl")
            .args(["terminate", &self.machine])
            .output();
        if let Some(child) = self.container.as_mut() {
            let deadline = Instant::now() + Duration::from_secs(15);
            while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(child) = self.dhcp.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = Command::new("ip")
            .args(["link", "delete", &self.link])
            .output();
        let _ = Command::new("ip")
            .args(["netns", "delete", &self.namespace])
            .output();
    }
}
fn inside(machine: &str, args: &[&str]) -> Result<String> {
    let mut all = vec!["--quiet", "--wait", "--pipe", "--machine", machine, "--"];
    all.extend_from_slice(args);
    output("systemd-run", &all)
}

/// Where the guest leaves requests for actions only the peer side can take. It
/// is a directory of the clone's root file system, so both sides read it.
const GATE: &str = "var/lib/ndimon-harness/gate";

/// Peer-side tools. The Tessera daemon and CLI are the unmodified binaries of a
/// separate build; they are optional, as the guest scenarios that need them.
struct Peer {
    namespace: String,
    tessera: Option<PathBuf>,
    /// Control socket of the peer daemon, started in the peer namespace.
    socket: PathBuf,
}

impl Peer {
    /// One request file: `{"ping":"ADDR"}` or `{"tessera_clock_locked":{"timeout":N}}`.
    fn answer(&self, request: &serde_json::Value) -> serde_json::Value {
        let result = if let Some(address) = request["ping"].as_str() {
            self.ping(address)
        } else if let Some(limit) = request["tessera_clock_locked"]["timeout"].as_u64() {
            self.clock_locked(limit)
        } else {
            Err("unknown peer request".into())
        };
        match result {
            Ok(detail) => serde_json::json!({"ok":true,"detail":detail}),
            Err(error) => serde_json::json!({"ok":false,"detail":error.to_string()}),
        }
    }
    fn ping(&self, address: &str) -> Result<String> {
        // The guest names only literal IPv4 addresses; nothing else is run.
        address.parse::<std::net::Ipv4Addr>()?;
        output(
            "ip",
            &[
                "netns",
                "exec",
                &self.namespace,
                "ping",
                "-c",
                "1",
                "-W",
                "2",
                address,
            ],
        )
        .map(|text| text.lines().last().unwrap_or_default().to_owned())
    }
    /// The peer daemon, a follower on the media VLAN, must lock to the guest's
    /// leader within `limit` seconds (the old Python test allowed 30).
    fn clock_locked(&self, limit: u64) -> Result<String> {
        let tools = self.tessera.as_ref().ok_or("no Tessera binaries")?;
        let socket = self.socket.to_str().ok_or("path")?;
        let start = Instant::now();
        let mut last = String::from("no reply");
        while start.elapsed() < Duration::from_secs(limit) {
            if let Ok(text) = output(
                tools.join("tesseractl").to_str().ok_or("path")?,
                &["--socket", socket, "--json", "clock", "show"],
            ) {
                let clocks = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v["clocks"].as_array().cloned())
                    .unwrap_or_default();
                if clocks.iter().any(|c| c["state"] == "locked") {
                    return Ok(text.trim().to_owned());
                }
                last = text.trim().to_owned();
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        Err(format!("peer clock never locked: {last}").into())
    }
    /// Serve every pending request in the gate, oldest name first.
    fn serve(&self, gate: &Path) -> Result {
        let mut pending: Vec<PathBuf> = fs::read_dir(gate)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "req"))
            .collect();
        pending.sort();
        for path in pending {
            let request = serde_json::from_slice(&fs::read(&path)?)?;
            let answer = self.answer(&request);
            let temporary = path.with_extension("tmp");
            fs::write(&temporary, serde_json::to_vec(&answer)?)?;
            fs::rename(&temporary, path.with_extension("res"))?;
            fs::remove_file(&path)?;
        }
        Ok(())
    }
}

/// `inside`, while answering the guest's peer requests until it exits. Output
/// goes to a file, as a full pipe would otherwise stall the guest.
fn inside_gated(
    machine: &str,
    args: &[&str],
    peer: &Peer,
    gate: &Path,
    log: &Path,
) -> Result<String> {
    let output_file = File::create(log)?;
    let mut all = vec!["--quiet", "--wait", "--pipe", "--machine", machine, "--"];
    all.extend_from_slice(args);
    let mut child = Command::new("systemd-run")
        .args(&all)
        .stdin(Stdio::null())
        .stdout(output_file.try_clone()?)
        .stderr(output_file)
        .spawn()?;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        peer.serve(gate)?;
        std::thread::sleep(Duration::from_millis(100));
    };
    let text = fs::read_to_string(log)?;
    if !status.success() {
        return Err(format!("systemd-run {all:?}: {status}: {text}").into());
    }
    Ok(text)
}
fn lab(workspace: &Path, images: &Path, work: &Path, backend: &str, selected: &[String]) -> Result {
    let source = images.join(image_name(backend));
    if !source.join("etc/os-release").exists() {
        return Err("build images first".into());
    }
    let suffix = match backend {
        "networkd" => "nd",
        "netplan" => "np",
        "networkmanager" => "nm",
        NM_NETPLAN => "nn",
        _ => "if",
    };
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let dir = work.join(format!("{backend}-{stamp}"));
    fs::create_dir(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    let root = dir.join("root");
    command(
        "cp",
        &[
            "-a",
            "--reflink=auto",
            source.to_str().ok_or("path")?,
            root.to_str().ok_or("path")?,
        ],
    )?;
    configure(&root, backend)?;
    let binary = workspace.join("target/debug/ndimon-net-harness");
    fs::write(
        dir.join("driver-sha256.txt"),
        output("sha256sum", &[binary.to_str().ok_or("path")?])?,
    )?;
    let destination = root.join("usr/libexec/ndimon/ndimon-net-harness");
    fs::create_dir_all(destination.parent().ok_or("parent")?)?;
    fs::copy(binary, &destination)?;
    fs::set_permissions(destination, fs::Permissions::from_mode(0o755))?;
    fs::copy(
        workspace.join("target/debug/ndimon-privd"),
        root.join("usr/libexec/ndimon/ndimon-privd"),
    )?;
    let gate = root.join(GATE);
    fs::create_dir_all(&gate)?;
    fs::set_permissions(&gate, fs::Permissions::from_mode(0o700))?;
    // Unmodified tesserad and tesseractl from a separate Tessera build. Without
    // them the Tessera daemon scenario is left out of the default list.
    let tessera = std::env::var_os("NDIMON_TESSERA_BIN_DIR").map(PathBuf::from);
    if let Some(tools) = &tessera {
        for binary in ["tesserad", "tesseractl"] {
            let destination = root.join("usr/bin").join(binary);
            fs::copy(tools.join(binary), &destination)
                .map_err(|e| format!("NDIMON_TESSERA_BIN_DIR needs {binary}: {e}"))?;
            fs::set_permissions(destination, fs::Permissions::from_mode(0o755))?;
        }
        fs::write(
            dir.join("tessera-daemon-sha256.txt"),
            output(
                "sha256sum",
                &[
                    tools.join("tesserad").to_str().ok_or("path")?,
                    tools.join("tesseractl").to_str().ok_or("path")?,
                ],
            )?,
        )?;
    } else if selected.iter().any(|s| s == "tessera-daemon") {
        return Err("tessera-daemon needs NDIMON_TESSERA_BIN_DIR (tesserad and tesseractl)".into());
    }
    if selected.iter().any(|s| s.starts_with("cross-")) {
        // Both inputs are built in the worker's private build directory. The
        // tool shim normalizes only veth; the Tessera CLI is the real binary.
        let driver = std::env::var("NDIMON_HANDOFF_CONTAINER_TOOLS")?;
        let binary = std::env::var("NDIMON_HANDOFF_TESSERA_BINARY")?;
        fs::copy(&binary, root.join("usr/bin/tessera-network"))?;
        for tool in ["ip", "modprobe"] {
            let target = root.join(format!("usr/sbin/{tool}"));
            fs::remove_file(&target)?;
            fs::copy(&driver, &target)?;
            fs::set_permissions(target, fs::Permissions::from_mode(0o755))?;
        }
        write(
            &root,
            "/etc/tessera/web.toml",
            "listen = \"0.0.0.0:80\"\n[network]\ninterface = \"end0\"\nconfirm = \"token_only\"\n",
            0o644,
        )?;
        fs::write(
            dir.join("tessera-sha256.txt"),
            output("sha256sum", &[&driver, &binary])?,
        )?;
        write(
            &root,
            "/usr/lib/systemd/system/tesseraaudio.slice",
            "[Unit]\nDescription=Tessera audio\n[Slice]\nCPUAccounting=yes\n",
            0o644,
        )?;
        write(
            &root,
            "/etc/systemd/system/tessera-network-boot.service",
            "[Unit]\nAfter=network.target\nBefore=ndimon-net-boot.service\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/usr/bin/tessera-network boot-recover\n[Install]\nWantedBy=multi-user.target\n",
            0o644,
        )?;
        enable(&root, "tessera-network-boot.service")?;
    }
    let unit = fs::read_to_string(
        workspace
            .parent()
            .ok_or("workspace root")?
            .join("systemd/ndimon-net-boot.service"),
    )?;
    write(
        &root,
        "/etc/systemd/system/ndimon-net-boot.service",
        &unit,
        0o644,
    )?;
    write(&root, "/etc/ndimon-network-engine", "rust\n", 0o644)?;
    write(
        &root,
        "/etc/ndimon-network-certified-backends.json",
        include_str!("../../../packaging/debian/etc/ndimon-network-certified-backends.json"),
        0o644,
    )?;
    let groups = root.join("etc/group");
    let mut text = fs::read_to_string(&groups)?;
    text.push_str("ndimon:x:951:\n");
    fs::write(groups, text)?;
    write(
        &root,
        "/etc/systemd/system/ndimon-state-import.service",
        "[Unit]\nDescription=Disposable state import\nBefore=ndimon-net-boot.service\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/usr/libexec/ndimon/ndimon-net-harness initialize\n",
        0o644,
    )?;
    write(
        &root,
        "/etc/systemd/system/ndimon-harness-portal.service",
        "[Unit]\nDescription=Harness listener\nRequires=ndimon-net-boot.service\nAfter=ndimon-net-boot.service\n[Service]\nExecStart=/usr/libexec/ndimon/ndimon-net-harness serve\nRestart=on-failure\n",
        0o644,
    )?;
    let wants = root.join("etc/systemd/system/multi-user.target.wants");
    fs::create_dir_all(&wants)?;
    for service in ["ndimon-net-boot.service", "ndimon-harness-portal.service"] {
        symlink(
            format!("/etc/systemd/system/{service}"),
            wants.join(service),
        )?;
    }
    for unit in [
        "ndimon-control.service",
        "ndimon-r.service",
        "ndimon-finder.service",
        "tesserad.service",
    ] {
        let slice =
            if unit == "tesserad.service" && selected.iter().any(|s| s.starts_with("cross-")) {
                "Slice=tesseraaudio.slice\n"
            } else {
                ""
            };
        write(
            &root,
            &format!("/etc/systemd/system/{unit}"),
            &format!(
                "[Unit]\nRequires=ndimon-net-boot.service\nAfter=ndimon-net-boot.service\n[Service]\n{slice}ExecStart=/usr/bin/sleep infinity\n"
            ),
            0o644,
        )?;
        symlink(format!("/etc/systemd/system/{unit}"), wants.join(unit))?;
    }
    let peer = format!("n4p{suffix}{}", std::process::id());
    let mut lab = Lab {
        machine: format!("n4-{suffix}-{}", std::process::id()),
        namespace: format!("n4-net-{suffix}-{}", std::process::id()),
        link: format!("n4c{suffix}{}", std::process::id()),
        container: None,
        dhcp: None,
        probes: Vec::new(),
    };
    println!(
        "LAB {backend} machine={} namespace={} link={} artifacts={}",
        lab.machine,
        lab.namespace,
        lab.link,
        dir.display()
    );
    fs::write(
        dir.join("lab.json"),
        serde_json::to_string_pretty(
            &serde_json::json!({"machine":lab.machine,"namespace":lab.namespace,"link":lab.link,"peer":peer}),
        )?,
    )?;
    command("ip", &["netns", "add", &lab.namespace])?;
    command(
        "ip",
        &[
            "link", "add", &lab.link, "type", "veth", "peer", "name", &peer,
        ],
    )?;
    command("ip", &["link", "set", &peer, "netns", &lab.namespace])?;
    if selected.iter().any(|s| s.starts_with("search-")) {
        configure_search(&root, backend)?;
    }
    command(
        "ip",
        &["-n", &lab.namespace, "link", "set", &peer, "name", "peer0"],
    )?;
    command("ip", &["-n", &lab.namespace, "link", "set", "lo", "up"])?;
    command("ip", &["-n", &lab.namespace, "link", "set", "peer0", "up"])?;
    for (tag, address) in [
        (None, "192.0.2.1/24"),
        (Some("20"), "198.51.20.1/24"),
        (Some("30"), "203.0.113.1/24"),
    ] {
        let name = tag.map(|t| format!("peer0.{t}")).unwrap_or("peer0".into());
        if let Some(tag) = tag {
            command(
                "ip",
                &[
                    "-n",
                    &lab.namespace,
                    "link",
                    "add",
                    "link",
                    "peer0",
                    "name",
                    &name,
                    "type",
                    "vlan",
                    "id",
                    tag,
                ],
            )?;
            command("ip", &["-n", &lab.namespace, "link", "set", &name, "up"])?;
        }
        command(
            "ip",
            &[
                "-n",
                &lab.namespace,
                "address",
                "add",
                address,
                "dev",
                &name,
            ],
        )?;
    }
    let log = File::create(dir.join("dhcp.log"))?;
    if selected.is_empty()
        || selected
            .iter()
            .any(|s| s == "packet-restrictions" || s == "guest-profiles")
    {
        for address in ["198.51.20.1:43001", "203.0.113.1:43001"] {
            lab.probes.push(
                Command::new("ip")
                    .args(["netns", "exec", &lab.namespace])
                    .arg(workspace.join("target/debug/ndimon-net-harness"))
                    .args(["udp-echo", address])
                    .stdout(Stdio::null())
                    .stderr(Stdio::inherit())
                    .spawn()?,
            );
        }
    }
    // The follower side of the media-VLAN clock test runs in the peer namespace
    // like the echo servers, and is reaped by Lab::drop.
    let peer_config = dir.join("tessera-peer");
    if let Some(tools) = &tessera {
        fs::create_dir_all(&peer_config)?;
        fs::write(
            peer_config.join("config.toml"),
            "[daemon]\nrealtime = false\n[network]\ninterfaces = [\"peer0.30\"]\nlocked = [\"primary\"]\n[clock]\nrole = \"follower\"\ntimestamping = \"software\"\n",
        )?;
        let daemon_log = File::create(dir.join("tessera-peer.log"))?;
        lab.probes.push(
            Command::new("ip")
                .args(["netns", "exec", &lab.namespace])
                .arg(tools.join("tesserad"))
                .arg("--config-dir")
                .arg(&peer_config)
                .arg("--state-dir")
                .arg(peer_config.join("state"))
                .arg("--runtime-dir")
                .arg(peer_config.join("run"))
                .stdout(Stdio::from(daemon_log.try_clone()?))
                .stderr(Stdio::from(daemon_log))
                .spawn()?,
        );
    }
    let peer = Peer {
        namespace: lab.namespace.clone(),
        tessera: tessera.clone(),
        socket: peer_config.join("run/control.sock"),
    };
    lab.dhcp = Some(
        Command::new("ip")
            .args([
                "netns",
                "exec",
                &lab.namespace,
                "dnsmasq",
                "--keep-in-foreground",
                "--user=root",
                "--port=0",
                "--no-resolv",
                "--bind-interfaces",
                "--interface=peer0",
                "--interface=peer0.20",
                "--interface=peer0.30",
                "--dhcp-authoritative",
                "--dhcp-range=set:native,192.0.2.100,192.0.2.180,255.255.255.0,5m",
                "--dhcp-range=set:v20,198.51.20.100,198.51.20.180,255.255.255.0,5m",
                "--dhcp-range=set:v30,203.0.113.100,203.0.113.180,255.255.255.0,5m",
                "--dhcp-option=tag:native,option:router,192.0.2.1",
                "--dhcp-option=tag:native,option:dns-server,192.0.2.1",
                "--dhcp-option=tag:v20,option:router,198.51.20.1",
                "--dhcp-option=tag:v20,option:dns-server,198.51.20.1",
                "--dhcp-option=tag:v30,option:router,203.0.113.1",
                "--dhcp-option=tag:v30,option:dns-server,203.0.113.1",
                "--dhcp-option=option:ntp-server,192.0.2.222",
                "--dhcp-option=option:domain-search,unwanted.test",
                &format!("--dhcp-leasefile={}", dir.join("leases").display()),
            ])
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()?,
    );
    command("modprobe", &["8021q"])?;
    command("udevadm", &["settle", "--timeout=10"])?;
    let boot = File::create(dir.join("boot.log"))?;
    lab.container = Some(
        Command::new("systemd-nspawn")
            .args([
                "--quiet",
                "--boot",
                "--register=yes",
                "--link-journal=no",
                "--resolv-conf=off",
                "--private-users=no",
                "--capability=CAP_NET_ADMIN,CAP_NET_RAW",
                &format!("--machine={}", lab.machine),
                &format!("--directory={}", root.display()),
                &format!("--network-interface={}:end0", lab.link),
            ])
            .stdout(Stdio::from(boot.try_clone()?))
            .stderr(Stdio::from(boot))
            .spawn()?,
    );
    let start = Instant::now();
    loop {
        if inside(
            &lab.machine,
            &["/usr/libexec/ndimon/ndimon-net-harness", "ready"],
        )
        .is_ok()
        {
            break;
        }
        if lab
            .container
            .as_mut()
            .ok_or("container")?
            .try_wait()?
            .is_some()
            || start.elapsed() > Duration::from_secs(90)
        {
            return Err(format!(
                "container not ready; inspect {}",
                dir.join("boot.log").display()
            )
            .into());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    let mut results = Vec::new();
    let defaults = [
        "first-adoption-revert",
        "timer-properties",
        "packet-restrictions",
        "guest-profiles",
        "untagged-dhcp",
        "untagged-static",
        "vlan-static",
        "vlan-dhcp",
        "mixed",
        "all-tagged",
        "management-move",
        "keep-address-policy",
        "keep-token-policy",
        "legacy-upgrade",
        "revert",
        "expiry",
        "apply-failure",
        "wildcard",
        "foreign-competition",
        "stale-job",
        "recovery-retry",
        "keep-expiry",
        "keep-revert",
        "storage-boundaries",
        "sigkill",
        "commit-sigkill",
        "reboot-stage",
    ];
    let scenarios: Vec<&str> = if selected.is_empty() {
        let mut all = defaults.to_vec();
        if tessera.is_some() {
            let after = all.iter().position(|s| *s == "guest-profiles").unwrap_or(0);
            all.insert(after + 1, "tessera-daemon");
        }
        all
    } else {
        selected.iter().map(String::as_str).collect()
    };
    for name in &scenarios {
        let result = inside_gated(
            &lab.machine,
            &["/usr/libexec/ndimon/ndimon-net-harness", "scenario", name],
            &peer,
            &gate,
            &dir.join(format!("{name}.out")),
        )
        .and_then(|text| {
            if name == &"guest-profiles" {
                for (address, allowed) in [
                    ("192.0.2.10", true),
                    ("198.51.20.10", false),
                    ("203.0.113.10", false),
                ] {
                    command(
                        "ip",
                        &[
                            "netns",
                            "exec",
                            &lab.namespace,
                            "ping",
                            "-c",
                            "1",
                            "-W",
                            "2",
                            address,
                        ],
                    )?;
                    let status = Command::new("ip")
                        .args([
                            "netns",
                            "exec",
                            &lab.namespace,
                            "curl",
                            "--noproxy",
                            "*",
                            "--fail",
                            "--silent",
                            "--max-time",
                            "3",
                            &format!("http://{address}/"),
                        ])
                        .output()?;
                    if status.status.success() != allowed {
                        return Err(format!("management HTTP policy failed for {address}").into());
                    }
                }
            }
            Ok(text)
        })
        .and_then(|text| {
            if text
                .lines()
                .any(|line| line.starts_with("PASS ") && line.ends_with(&format!(" {name}")))
            {
                Ok(text)
            } else {
                Err("scenario exited without its completion assertion".into())
            }
        });
        let pass = result.is_ok();
        let text = match result {
            Ok(s) => s,
            Err(e) => e.to_string(),
        };
        fs::write(dir.join(format!("{name}.log")), &text)?;
        println!(
            "{} {backend} {name}: {}",
            if pass { "PASS" } else { "FAIL" },
            text.trim()
        );
        results.push(serde_json::json!({"scenario":name,"pass":pass,"detail":text}));
        fs::write(
            dir.join("results.json"),
            serde_json::to_string_pretty(&results)?,
        )?;
        if !pass {
            if let Ok(journal) = inside(
                &lab.machine,
                &["/usr/bin/journalctl", "--no-pager", "-n", "400"],
            ) {
                fs::write(dir.join("failure-journal.log"), journal)?;
            }
            return Err(format!("scenario failed; artifacts {}", dir.display()).into());
        }
    }
    if scenarios.contains(&"reboot-stage") || scenarios.contains(&"cross-reboot-stage") {
        command("machinectl", &["reboot", &lab.machine])?;
        // nspawn reboot reexecs PID1 and retains the private veth. Wait for a changed
        // boot ID and the boot prerequisite, not a sleep treated as reboot evidence.
        let start = Instant::now();
        loop {
            match inside(
                &lab.machine,
                &[
                    "/usr/libexec/ndimon/ndimon-net-harness",
                    "scenario",
                    if scenarios.contains(&"cross-reboot-stage") {
                        "cross-reboot-verify"
                    } else {
                        "reboot-verify"
                    },
                ],
            ) {
                Ok(text) => {
                    println!("{text}");
                    fs::write(dir.join("reboot-verify.log"), &text)?;
                    results.push(
                        serde_json::json!({"scenario":"reboot-verify","pass":true,"detail":text}),
                    );
                    fs::write(
                        dir.join("results.json"),
                        serde_json::to_string_pretty(&results)?,
                    )?;
                    break;
                }
                Err(error) if start.elapsed() > Duration::from_secs(90) => {
                    let detail = error.to_string();
                    fs::write(dir.join("reboot-verify.log"), &detail)?;
                    results.push(serde_json::json!({"scenario":"reboot-verify","pass":false,"detail":detail}));
                    fs::write(
                        dir.join("results.json"),
                        serde_json::to_string_pretty(&results)?,
                    )?;
                    if let Ok(journal) = inside(
                        &lab.machine,
                        &["/usr/bin/journalctl", "--no-pager", "-n", "150"],
                    ) {
                        fs::write(dir.join("failure-journal.log"), journal)?;
                    }
                    return Err(error);
                }
                Err(_) => std::thread::sleep(Duration::from_secs(1)),
            }
        }
    }
    fs::write(
        dir.join("versions.txt"),
        inside(
            &lab.machine,
            &[
                "/usr/bin/dpkg-query",
                "-W",
                "systemd",
                "nftables",
                "iproute2",
                match backend {
                    "netplan" => "netplan.io",
                    "networkmanager" => "network-manager",
                    "ifupdown" => "ifupdown",
                    _ => "systemd",
                },
            ],
        )?,
    )?;
    fs::write(
        dir.join("journal.log"),
        inside(&lab.machine, &["/usr/bin/journalctl", "--no-pager"])?,
    )?;
    fs::write(dir.join("complete"), "all selected scenarios passed\n")?;
    Ok(())
}
