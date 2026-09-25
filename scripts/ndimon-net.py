#!/usr/bin/env python3
"""NDIMon network helper. Invoked only by ndimon-priv, as root.

Reads and applies DHCP or static IPv4 on the primary wired interface.
Wi-Fi is out of scope. A change is applied a couple of seconds after
net-apply returns, and a systemd timer rolls it back unless net-confirm
runs first. Standard library plus PyYAML (netplan files only).
"""
import fcntl
import glob
import ipaddress
import json
import os
import re
import secrets
import shutil
import subprocess
import sys
import time

NET_DIR = "/var/lib/ndimon/net"
PENDING = os.path.join(NET_DIR, "pending")
STATE = os.path.join(PENDING, "state.json")
LOCK = os.path.join(PENDING, "lock")
BANNER = "/run/ndimon/net-pending"
HELPER = "/usr/local/lib/ndimon/ndimon-net"
PRIV = "/usr/local/sbin/ndimon-priv"

IPV4 = re.compile(r"^\d{1,3}(?:\.\d{1,3}){3}$")


class NetError(Exception):
    pass


def run(args, check=True, timeout=20):
    try:
        proc = subprocess.run(
            args, capture_output=True, text=True, timeout=timeout, check=False
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise NetError(str(exc)) from exc
    if check and proc.returncode != 0:
        err = (proc.stderr or proc.stdout or "").strip()
        raise NetError(err or f"{args[0]} failed ({proc.returncode})")
    return proc


def systemctl_active(unit):
    proc = run(["systemctl", "is-active", "--quiet", unit], check=False, timeout=8)
    return proc.returncode == 0


def read_text(path):
    try:
        with open(path, "r", encoding="utf-8") as fh:
            return fh.read()
    except OSError:
        return ""


def atomic_write(path, data, mode=0o600):
    directory = os.path.dirname(path)
    os.makedirs(directory, exist_ok=True)
    tmp = path + ".tmp"
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, mode)
    try:
        if isinstance(data, str):
            data = data.encode("utf-8")
        os.write(fd, data)
        os.fsync(fd)
    finally:
        os.close(fd)
    os.replace(tmp, path)
    os.chmod(path, mode)
    dirfd = os.open(directory, os.O_RDONLY)
    try:
        os.fsync(dirfd)
    finally:
        os.close(dirfd)


def wired(iface):
    if not iface or iface == "lo":
        return False
    if iface.startswith(("wlan", "wlx", "wlp", "wifi", "docker", "veth", "br-", "tun", "tap", "virbr")):
        return False
    base = f"/sys/class/net/{iface}"
    if not os.path.isdir(base):
        return False
    if os.path.exists(os.path.join(base, "wireless")):
        return False
    return True


def primary_interface():
    proc = run(["ip", "-j", "route", "show", "default"], check=False)
    try:
        routes = json.loads(proc.stdout or "[]")
    except json.JSONDecodeError:
        routes = []
    for route in routes:
        dev = route.get("dev")
        if wired(dev):
            return dev
    names = []
    try:
        names = sorted(os.listdir("/sys/class/net"))
    except OSError:
        return None
    up = []
    for name in names:
        if not wired(name):
            continue
        state = read_text(f"/sys/class/net/{name}/operstate").strip()
        if state == "up":
            up.append(name)
    if up:
        return up[0]
    for name in names:
        if wired(name):
            return name
    return None


def live_address(iface):
    proc = run(["ip", "-j", "addr", "show", "dev", iface], check=False)
    try:
        rows = json.loads(proc.stdout or "[]")
    except json.JSONDecodeError:
        rows = []
    for row in rows:
        for addr in row.get("addr_info") or []:
            if addr.get("family") == "inet" and addr.get("scope") == "global":
                return addr.get("local") or "", int(addr.get("prefixlen") or 0)
    return "", 0


def live_gateway(iface):
    proc = run(["ip", "-j", "route", "show", "default"], check=False)
    try:
        routes = json.loads(proc.stdout or "[]")
    except json.JSONDecodeError:
        routes = []
    for route in routes:
        if route.get("dev") == iface and route.get("gateway"):
            return route["gateway"]
    for route in routes:
        if route.get("gateway"):
            return route["gateway"]
    return ""


def live_dns(iface):
    proc = run(["resolvectl", "dns", iface], check=False, timeout=5)
    found = re.findall(r"\b(?:\d{1,3}\.){3}\d{1,3}\b", proc.stdout or "")
    if found:
        return found[:3]
    servers = []
    for line in read_text("/etc/resolv.conf").splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[0] == "nameserver" and IPV4.match(parts[1]):
            servers.append(parts[1])
    return servers[:3]


def nm_managed(iface):
    if not systemctl_active("NetworkManager.service") or not shutil.which("nmcli"):
        return False
    proc = run(["nmcli", "-g", "GENERAL.CONNECTION", "device", "show", iface], check=False)
    name = (proc.stdout or "").strip()
    return bool(name) and name != "--"


def nm_profile(iface):
    proc = run(["nmcli", "-g", "GENERAL.CONNECTION", "device", "show", iface], check=False)
    name = (proc.stdout or "").strip()
    if not name or name == "--":
        raise NetError(f"NetworkManager is not managing {iface}")
    return name


def nm_field(profile, field):
    proc = run(["nmcli", "-g", field, "connection", "show", profile], check=False)
    return (proc.stdout or "").strip()


def split_nm_list(value):
    if not value:
        return []
    return [part for part in re.split(r"[, ]+", value) if part]


def read_nm(iface):
    profile = nm_profile(iface)
    method = nm_field(profile, "ipv4.method") or "auto"
    addresses = split_nm_list(nm_field(profile, "ipv4.addresses"))
    gateway = nm_field(profile, "ipv4.gateway")
    dns = split_nm_list(nm_field(profile, "ipv4.dns"))
    mode = "dhcp"
    address, prefix = "", 0
    if method == "manual" and addresses:
        mode = "static"
        address, prefix = split_cidr(addresses[0])
    elif method not in ("auto", "dhcp"):
        mode = "unknown"
    return {
        "mode": mode,
        "address": address,
        "prefix": prefix,
        "gateway": gateway if mode == "static" else "",
        "dns": dns if mode == "static" else [],
        "snapshot": {
            "kind": "networkmanager",
            "profile": profile,
            "method": method,
            "addresses": addresses,
            "gateway": gateway,
            "dns": dns,
        },
    }


def netplan_files():
    return sorted(glob.glob("/etc/netplan/*.yaml"))


def networkctl_managed(iface):
    if not shutil.which("networkctl"):
        return False
    proc = run(["networkctl", "status", iface], check=False, timeout=8)
    text = (proc.stdout or "") + (proc.stderr or "")
    match = re.search(r"State:\s*(\S+)", text)
    if not match:
        return False
    return match.group(1).lower() != "unmanaged"


def read_networkd_file(iface):
    for path in sorted(glob.glob("/etc/systemd/network/*.network")):
        text = read_text(path)
        if re.search(rf"(?m)^Name\s*=\s*{re.escape(iface)}\s*$", text):
            return path, text
    return "", ""


def parse_networkd(text):
    address, prefix, gateway, dns = "", 0, "", []
    dhcp = False
    for raw in text.splitlines():
        line = raw.strip()
        if line.lower().startswith("dhcp=") and "yes" in line.lower():
            dhcp = True
        if line.startswith("Address="):
            address, prefix = split_cidr(line.split("=", 1)[1].strip())
        elif line.startswith("Gateway="):
            gateway = line.split("=", 1)[1].strip()
        elif line.startswith("DNS="):
            dns.append(line.split("=", 1)[1].strip())
    if address and not dhcp:
        return "static", address, prefix, gateway, dns
    return "dhcp", "", 0, "", []


def read_ifupdown(iface):
    path = f"/etc/network/interfaces.d/ndimon-{iface}"
    text = read_text(path)
    if not text:
        main = read_text("/etc/network/interfaces")
        block = ""
        capture = False
        for line in main.splitlines():
            if re.match(rf"iface\s+{re.escape(iface)}\s+", line):
                capture = True
                block = line + "\n"
                continue
            if capture:
                if line and not line[0].isspace() and not line.startswith("#"):
                    break
                block += line + "\n"
        text = block
    if re.search(r"\binet\s+static\b", text):
        address = value_of(text, "address")
        addr, prefix = split_cidr(address) if address else ("", 0)
        return "static", addr, prefix, value_of(text, "gateway"), value_of(text, "dns-nameservers").split()
    return "dhcp", "", 0, "", []


def value_of(text, key):
    match = re.search(rf"(?m)^\s*{re.escape(key)}\s+(\S+)", text)
    return match.group(1).strip() if match else ""


def split_cidr(value):
    if "/" not in value:
        return value, 24
    addr, bits = value.split("/", 1)
    try:
        return addr, int(bits)
    except ValueError:
        return addr, 24


def dhcpcd_mode(iface):
    text = read_text("/etc/dhcpcd.conf")
    capture = False
    static = ""
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("interface "):
            capture = stripped.split(None, 1)[1] == iface
            continue
        if capture and stripped.startswith("static ip_address="):
            static = stripped.split("=", 1)[1].strip()
    if static:
        addr, prefix = split_cidr(static)
        return "static", addr, prefix
    return "dhcp", "", 0


def detect_backend(iface):
    if nm_managed(iface):
        return "networkmanager"
    if netplan_files() and networkctl_managed(iface):
        return "netplan"
    if systemctl_active("systemd-networkd.service") and networkctl_managed(iface):
        return "networkd"
    # dhcpcd (older Pi OS) is read-only. Check it before ifupdown, because
    # those images also have a stub /etc/network/interfaces.
    if systemctl_active("dhcpcd.service"):
        return "dhcpcd"
    interfaces = read_text("/etc/network/interfaces")
    dropin = "/etc/network/interfaces.d"
    # Only claim ifupdown when this interface is actually defined there.
    # A stub that merely sources interfaces.d is not enough.
    if f"iface {iface} " in interfaces or os.path.exists(f"{dropin}/ndimon-{iface}"):
        return "ifupdown"
    return "unsupported"


def configured(backend, iface):
    if backend == "networkmanager":
        info = read_nm(iface)
        return info
    if backend == "netplan":
        path = "/etc/netplan/90-ndimon.yaml"
        text = read_text(path)
        mode, address, prefix, gateway, dns = ("dhcp", "", 0, "", [])
        if text:
            try:
                import yaml
                doc = yaml.safe_load(text) or {}
                eth = (((doc.get("network") or {}).get("ethernets") or {}).get(iface) or {})
                if eth.get("dhcp4") is False or eth.get("addresses"):
                    mode = "static"
                    addrs = eth.get("addresses") or []
                    if addrs:
                        address, prefix = split_cidr(str(addrs[0]))
                    routes = eth.get("routes") or []
                    for route in routes:
                        if route.get("to") in ("default", "0.0.0.0/0"):
                            gateway = str(route.get("via") or "")
                    dns = list(((eth.get("nameservers") or {}).get("addresses") or []))
            except Exception:
                mode = "unknown"
        return pack(mode, address, prefix, gateway, dns, {
            "kind": "netplan", "path": path, "existed": os.path.exists(path), "content": text,
        })
    if backend == "networkd":
        path, text = read_networkd_file(iface)
        if text:
            mode, address, prefix, gateway, dns = parse_networkd(text)
        else:
            mode, address, prefix, gateway, dns = "dhcp", "", 0, "", []
        return pack(mode, address, prefix, gateway, dns, {
            "kind": "networkd", "path": path, "existed": bool(path), "content": text,
        })
    if backend == "ifupdown":
        path = f"/etc/network/interfaces.d/ndimon-{iface}"
        mode, address, prefix, gateway, dns = read_ifupdown(iface)
        return pack(mode, address, prefix, gateway, dns, {
            "kind": "ifupdown", "path": path, "existed": os.path.exists(path),
            "content": read_text(path),
        })
    if backend == "dhcpcd":
        mode, address, prefix = dhcpcd_mode(iface)
        return pack(mode, address, prefix, "", [], {"kind": "dhcpcd"})
    return pack("unknown", "", 0, "", [], {"kind": "unsupported"})


def pack(mode, address, prefix, gateway, dns, snapshot):
    return {
        "mode": mode,
        "address": address or "",
        "prefix": int(prefix or 0),
        "gateway": gateway or "",
        "dns": [str(item) for item in dns if item],
        "snapshot": snapshot,
    }


def pending_info():
    if not os.path.exists(STATE):
        return {}
    try:
        state = load_state() or {}
        requested = state.get("requested") or {}
        return {
            "token": state.get("token") or "",
            "address": requested.get("address") or "",
            "mode": requested.get("mode") or "",
            "deadline": int(state.get("deadline") or 0),
            "phase": state.get("phase") or "",
        }
    except Exception:
        return {}


def status_document():
    iface = primary_interface()
    pending = os.path.exists(STATE)
    if not iface:
        return {
            "ok": True, "backend": "unsupported", "interface": "", "applicable": False,
            "mode": "unknown", "address": "", "prefix": 0, "gateway": "", "dns": [],
            "live_address": "", "live_prefix": 0, "live_gateway": "", "live_dns": [],
            "pending": pending, "pending_info": pending_info(), "reason": "no wired interface",
        }
    backend = detect_backend(iface)
    cfg = configured(backend, iface)
    live_addr, live_prefix = live_address(iface)
    applicable = backend in ("networkmanager", "netplan", "networkd", "ifupdown")
    reason = ""
    if backend == "dhcpcd":
        reason = "dhcpcd is read-only in this version"
    elif backend == "unsupported":
        reason = "network manager not recognized"
    return {
        "ok": True,
        "backend": backend,
        "interface": iface,
        "applicable": applicable,
        "reason": reason,
        "mode": cfg["mode"],
        "address": cfg["address"],
        "prefix": cfg["prefix"],
        "gateway": cfg["gateway"],
        "dns": cfg["dns"],
        "live_address": live_addr,
        "live_prefix": live_prefix,
        "live_gateway": live_gateway(iface),
        "live_dns": live_dns(iface),
        "pending": pending,
        "pending_info": pending_info(),
    }


def validate_request(obj):
    if not isinstance(obj, dict):
        raise NetError("settings must be an object")
    mode = obj.get("mode")
    if mode == "dhcp":
        return {"mode": "dhcp", "address": "", "prefix": 0, "gateway": "", "dns": []}
    if mode != "static":
        raise NetError("mode must be dhcp or static")
    try:
        addr = ipaddress.IPv4Address(str(obj.get("address") or ""))
    except ipaddress.AddressValueError as exc:
        raise NetError("address is not a valid IPv4 address") from exc
    try:
        prefix = int(obj.get("prefix"))
    except (TypeError, ValueError) as exc:
        raise NetError("prefix must be a number") from exc
    if prefix < 8 or prefix > 30:
        raise NetError("prefix must be between 8 and 30")
    network = ipaddress.ip_network(f"{addr}/{prefix}", strict=False)
    if addr == network.network_address or addr == network.broadcast_address:
        raise NetError("address is the network or broadcast address")
    if addr.is_loopback or addr.is_link_local or addr.is_multicast or addr.is_unspecified:
        raise NetError("address is not a usable unicast address")
    gateway = str(obj.get("gateway") or "")
    if gateway:
        try:
            gw = ipaddress.IPv4Address(gateway)
        except ipaddress.AddressValueError as exc:
            raise NetError("gateway is not a valid IPv4 address") from exc
        if gw not in network:
            raise NetError("gateway is not inside the address subnet")
    dns = obj.get("dns") or []
    if not isinstance(dns, list) or len(dns) > 3:
        raise NetError("dns must be a list of at most 3 addresses")
    clean_dns = []
    for item in dns:
        try:
            clean_dns.append(str(ipaddress.IPv4Address(str(item))))
        except ipaddress.AddressValueError as exc:
            raise NetError("a DNS server is not a valid IPv4 address") from exc
    return {
        "mode": "static",
        "address": str(addr),
        "prefix": prefix,
        "gateway": gateway,
        "dns": clean_dns,
    }


def same_config(current, requested):
    if current.get("mode") != requested.get("mode"):
        return False
    if requested["mode"] == "dhcp":
        return current.get("mode") == "dhcp"
    return (
        current.get("address") == requested["address"]
        and int(current.get("prefix") or 0) == requested["prefix"]
        and (current.get("gateway") or "") == requested["gateway"]
        and sorted(str(item) for item in (current.get("dns") or []))
            == sorted(str(item) for item in (requested.get("dns") or []))
    )


def apply_nm(iface, requested, snapshot=None):
    profile = (snapshot or {}).get("profile") or nm_profile(iface)
    if requested["mode"] == "dhcp":
        run(["nmcli", "connection", "modify", profile,
             "ipv4.method", "auto", "ipv4.addresses", "", "ipv4.gateway", "", "ipv4.dns", ""])
    else:
        dns = " ".join(requested["dns"])
        args = ["nmcli", "connection", "modify", profile,
                "ipv4.method", "manual",
                "ipv4.addresses", f"{requested['address']}/{requested['prefix']}",
                "ipv4.gateway", requested["gateway"] or "",
                "ipv4.dns", dns]
        run(args)
    run(["nmcli", "connection", "up", profile], timeout=40)


def apply_netplan(iface, requested, snapshot=None):
    try:
        import yaml
    except ImportError as exc:
        raise NetError("python3-yaml is not installed") from exc
    if requested["mode"] == "dhcp":
        eth = {"dhcp4": True}
    else:
        eth = {
            "dhcp4": False,
            "addresses": [f"{requested['address']}/{requested['prefix']}"],
        }
        if requested["gateway"]:
            eth["routes"] = [{"to": "default", "via": requested["gateway"]}]
        if requested["dns"]:
            eth["nameservers"] = {"addresses": requested["dns"]}
    doc = {"network": {"version": 2, "ethernets": {iface: eth}}}
    path = "/etc/netplan/90-ndimon.yaml"
    atomic_write(path, yaml.safe_dump(doc, sort_keys=False), 0o600)
    run(["netplan", "generate"], timeout=30)
    run(["netplan", "apply"], timeout=40)


def apply_networkd(iface, requested, snapshot=None):
    path = f"/etc/systemd/network/05-ndimon-{iface}.network"
    lines = ["[Match]", f"Name={iface}", "", "[Network]"]
    if requested["mode"] == "dhcp":
        lines.append("DHCP=yes")
    else:
        lines.append(f"Address={requested['address']}/{requested['prefix']}")
        if requested["gateway"]:
            lines.append(f"Gateway={requested['gateway']}")
        for server in requested["dns"]:
            lines.append(f"DNS={server}")
    atomic_write(path, "\n".join(lines) + "\n", 0o644)
    run(["networkctl", "reload"], timeout=20)
    run(["networkctl", "reconfigure", iface], timeout=30)


def ifupdown_block(iface):
    main = read_text("/etc/network/interfaces")
    if f"iface {iface} " in main:
        raise NetError(
            f"{iface} is defined in /etc/network/interfaces; refusing to edit the distro file"
        )
    if "interfaces.d" not in main:
        raise NetError("/etc/network/interfaces does not include interfaces.d")


def apply_ifupdown(iface, requested, snapshot=None):
    ifupdown_block(iface)
    path = f"/etc/network/interfaces.d/ndimon-{iface}"
    if requested["mode"] == "dhcp":
        body = f"auto {iface}\niface {iface} inet dhcp\n"
    else:
        body = (
            f"auto {iface}\niface {iface} inet static\n"
            f"    address {requested['address']}/{requested['prefix']}\n"
        )
        if requested["gateway"]:
            body += f"    gateway {requested['gateway']}\n"
        if requested["dns"]:
            body += "    dns-nameservers " + " ".join(requested["dns"]) + "\n"
    atomic_write(path, body, 0o644)
    run(["ifdown", iface], check=False, timeout=30)
    run(["ifup", iface], timeout=40)


def apply_backend(backend, iface, requested, snapshot=None):
    if backend == "networkmanager":
        apply_nm(iface, requested, snapshot)
    elif backend == "netplan":
        apply_netplan(iface, requested, snapshot)
    elif backend == "networkd":
        apply_networkd(iface, requested, snapshot)
    elif backend == "ifupdown":
        apply_ifupdown(iface, requested, snapshot)
    else:
        raise NetError(f"cannot apply settings with {backend}")


def restore_snapshot(state):
    snap = state.get("previous") or {}
    kind = snap.get("kind")
    iface = state.get("interface")
    if kind == "networkmanager":
        requested = {
            "mode": "dhcp" if snap.get("method") in ("auto", "dhcp") else "static",
            "address": "",
            "prefix": 24,
            "gateway": snap.get("gateway") or "",
            "dns": snap.get("dns") or [],
        }
        addresses = snap.get("addresses") or []
        if requested["mode"] == "static" and addresses:
            requested["address"], requested["prefix"] = split_cidr(addresses[0])
        elif requested["mode"] != "dhcp":
            requested["mode"] = "dhcp"
        apply_nm(iface, requested, snap)
        return
    path = snap.get("path") or ""
    if snap.get("existed") and path:
        atomic_write(path, snap.get("content") or "", 0o600 if kind == "netplan" else 0o644)
    elif path and os.path.exists(path):
        os.remove(path)
    if kind == "netplan":
        run(["netplan", "generate"], timeout=30)
        run(["netplan", "apply"], timeout=40)
    elif kind == "networkd":
        run(["networkctl", "reload"], timeout=20)
        if iface:
            run(["networkctl", "reconfigure", iface], timeout=30)
    elif kind == "ifupdown" and iface:
        run(["ifdown", iface], check=False, timeout=30)
        run(["ifup", iface], timeout=40)


def schedule(unit, seconds, argv):
    run(["systemctl", "reset-failed", f"{unit}.timer", f"{unit}.service"], check=False, timeout=8)
    run(["systemctl", "stop", f"{unit}.timer", f"{unit}.service"], check=False, timeout=8)
    run(["systemd-run", f"--unit={unit}", f"--on-active={int(seconds)}s", "--collect", *argv], timeout=15)


def write_banner(requested):
    os.makedirs("/run/ndimon", exist_ok=True)
    if requested.get("mode") == "static" and requested.get("address"):
        text = f"Pending network change: http://{requested['address']}/"
    else:
        text = "Pending network change — the new address will appear above"
    atomic_write(BANNER, text + "\n", 0o644)


def clear_banner():
    try:
        os.remove(BANNER)
    except OSError:
        pass


def load_state():
    if not os.path.exists(STATE):
        return None
    with open(STATE, "r", encoding="utf-8") as fh:
        return json.load(fh)


def save_state(state):
    os.makedirs(PENDING, exist_ok=True)
    os.chmod(PENDING, 0o700)
    atomic_write(STATE, json.dumps(state), 0o600)


def locked(func):
    os.makedirs(PENDING, exist_ok=True)
    os.chmod(PENDING, 0o700)
    with open(LOCK, "a", encoding="utf-8") as fh:
        fcntl.flock(fh, fcntl.LOCK_EX)
        try:
            return func()
        finally:
            fcntl.flock(fh, fcntl.LOCK_UN)


def cmd_status():
    json.dump(status_document(), sys.stdout)
    sys.stdout.write("\n")


def cmd_check(address):
    iface = primary_interface()
    if not iface:
        raise NetError("no wired interface")
    try:
        ipaddress.IPv4Address(address)
    except ipaddress.AddressValueError as exc:
        raise NetError("address is not a valid IPv4 address") from exc
    live, _prefix = live_address(iface)
    if address == live:
        result = {"ok": True, "duplicate": False, "skipped": "already ours"}
    elif not shutil.which("arping"):
        result = {"ok": True, "duplicate": None, "error": "arping not installed"}
    else:
        proc = run(["arping", "-D", "-I", iface, "-c", "2", "-w", "3", address], check=False, timeout=10)
        result = {"ok": True, "duplicate": proc.returncode != 0}
    json.dump(result, sys.stdout)
    sys.stdout.write("\n")


def cmd_apply(path):
    if not re.fullmatch(r"/var/lib/ndimon/net/stage-[a-z0-9]+\.json", path or ""):
        raise NetError("staged file path is not allowed")
    with open(path, "r", encoding="utf-8") as fh:
        requested = validate_request(json.load(fh))
    doc = status_document()
    if not doc.get("applicable"):
        raise NetError(doc.get("reason") or "network settings cannot be applied on this system")
    current = {
        "mode": doc["mode"], "address": doc["address"], "prefix": doc["prefix"],
        "gateway": doc["gateway"], "dns": doc["dns"],
    }
    if same_config(current, requested):
        json.dump({"ok": True, "unchanged": True}, sys.stdout)
        sys.stdout.write("\n")
        return
    cfg = configured(doc["backend"], doc["interface"])
    seconds = 180 if requested["mode"] == "dhcp" else 120
    token = secrets.token_hex(32)
    deadline = int(time.time()) + seconds

    def stage():
        if os.path.exists(STATE):
            raise NetError("a network change is already pending")
        save_state({
            "token": token,
            "phase": "scheduled",
            "deadline": deadline,
            "backend": doc["backend"],
            "interface": doc["interface"],
            "previous": cfg["snapshot"],
            "requested": requested,
        })

    locked(stage)
    try:
        schedule("ndimon-net-rollback", seconds, [PRIV, "net-rollback"])
        schedule("ndimon-net-apply", 2, [HELPER, "apply-now"])
    except Exception:
        def cleanup():
            try:
                os.remove(STATE)
            except OSError:
                pass
        locked(cleanup)
        run(["systemctl", "stop", "ndimon-net-rollback.timer"], check=False, timeout=8)
        raise
    json.dump({
        "ok": True,
        "token": token,
        "address": requested["address"] or None,
        "mode": requested["mode"],
        "deadline": deadline,
        "backend": doc["backend"],
        "interface": doc["interface"],
    }, sys.stdout)
    sys.stdout.write("\n")


def cmd_apply_now():
    def work():
        state = load_state()
        phase = state.get("phase") if state else ""
        if not state or phase in ("applied", "reverting"):
            return None
        apply_backend(state["backend"], state["interface"], state["requested"], state.get("previous"))
        # Re-read: confirm or rollback may have won the lock before we saved.
        latest = load_state() or state
        if latest.get("phase") == "confirmed":
            try:
                os.remove(STATE)
            except OSError:
                pass
            clear_banner()
            return None
        if latest.get("phase") == "reverting":
            return None
        state["phase"] = "applied"
        save_state(state)
        write_banner(state["requested"])
        return None
    locked(work)


def cmd_confirm(token):
    if not re.fullmatch(r"[0-9a-f]{64}", token or ""):
        raise NetError("bad token")

    def work():
        state = load_state()
        if not state:
            raise NetError("no network change is pending")
        if not secrets.compare_digest(state.get("token") or "", token):
            raise NetError("token does not match")
        if state.get("phase") == "reverting":
            raise NetError("no network change is pending")
        # Already on the new address: drop the pending record so it cannot
        # be rolled back. Not applied yet: mark confirmed and let apply-now run.
        if state.get("phase") == "applied":
            try:
                os.remove(STATE)
            except OSError:
                pass
            clear_banner()
            return
        state["phase"] = "confirmed"
        save_state(state)

    locked(work)
    run(["systemctl", "stop", "ndimon-net-rollback.timer"], check=False, timeout=8)
    run(["systemctl", "stop", "ndimon-net-rollback.service"], check=False, timeout=8)
    json.dump({"ok": True}, sys.stdout)
    sys.stdout.write("\n")


def cmd_rollback():
    # Hold the lock across the restore. Confirm and apply-now wait, so a
    # change the user already accepted cannot be undone afterwards.
    def work():
        state = load_state()
        if not state or state.get("phase") == "confirmed":
            clear_banner()
            return
        state["phase"] = "reverting"
        save_state(state)
        restore_snapshot(state)
        try:
            os.remove(STATE)
        except OSError:
            pass
        clear_banner()

    locked(work)
    run(["systemctl", "stop", "ndimon-net-apply.timer"], check=False, timeout=8)
    run(["systemctl", "stop", "ndimon-net-apply.service"], check=False, timeout=8)


def main(argv):
    cmd = argv[1] if len(argv) > 1 else ""
    try:
        if cmd == "status":
            cmd_status()
        elif cmd == "check-address" and len(argv) == 3:
            cmd_check(argv[2])
        elif cmd == "apply" and len(argv) == 3:
            cmd_apply(argv[2])
        elif cmd == "apply-now":
            cmd_apply_now()
        elif cmd == "confirm" and len(argv) == 3:
            cmd_confirm(argv[2])
        elif cmd == "rollback":
            cmd_rollback()
        else:
            sys.stderr.write("usage: ndimon-net status|check-address|apply|apply-now|confirm|rollback\n")
            return 2
    except NetError as exc:
        sys.stderr.write(str(exc) + "\n")
        return 1
    except Exception as exc:
        sys.stderr.write(f"network helper failed: {exc}\n")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
