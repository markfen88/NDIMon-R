#!/usr/bin/env python3
"""NDIMon network helper. Invoked only by ndimon-priv, as root.

Reads and applies DHCP or static IPv4 on the primary wired interface.
Wi-Fi is out of scope. A change is applied a couple of seconds after
net-apply returns, and a systemd timer rolls it back unless net-confirm
runs first. Standard library plus PyYAML (netplan files only).
"""
from contextlib import contextmanager
import fcntl
import fnmatch
import glob
import ipaddress
import json
import logging
import os
import re
import secrets
import shlex
import signal
import shutil
import stat
import subprocess
import tempfile
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
    """Invalid or unsupported network request."""
    pass


class NetExecutionError(NetError):
    """A validated request failed during operating-system execution."""
    pass


def run(args, check=True, timeout=20):
    try:
        proc = subprocess.run(
            args, capture_output=True, text=True, timeout=timeout, check=False
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise NetExecutionError(str(exc)) from exc
    if check and proc.returncode != 0:
        err = (proc.stderr or proc.stdout or "").strip()
        raise NetExecutionError(err or f"{args[0]} failed ({proc.returncode})")
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
    # VLANs can own the only default route but are never the physical base.
    if any(line.split('|', 1)[0].strip() == iface
           for line in read_text('/proc/net/vlan/config').splitlines() if '|' in line):
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
            "vlans": [], "vlan_error": "", "vlan_capable": False, "vlan_reason": "no wired interface",
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
        **vlan_list(iface, backend),
        "vlan_capable": applicable,
        "vlan_reason": "" if applicable else f"cannot apply vlans with {backend}",
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


# VLAN configuration is separate from the base interface's staged configuration.
VLAN_BACKENDS = ('networkmanager', 'netplan', 'networkd', 'ifupdown')
VLAN_DIR = os.path.join(NET_DIR, 'vlans')
VLAN_STATE = os.path.join(VLAN_DIR, 'vlans.json')
VLAN_OWNER_UID = 0
VLAN_OWNER_GID = 0


def open_vlan_directory(create=False):
    if create:
        try:
            os.mkdir(VLAN_DIR, 0o700)
        except FileExistsError:
            pass
    fd = os.open(VLAN_DIR, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        info = os.fstat(fd)
        # Do not repair user-owned or group-writable directories. A root-owned
        # private directory may inherit setgid/group from the staging parent.
        if info.st_uid != VLAN_OWNER_UID or stat.S_IMODE(info.st_mode) & 0o777 != 0o700:
            raise NetError('unsafe VLAN index directory ownership or permissions')
        os.fchown(fd, VLAN_OWNER_UID, VLAN_OWNER_GID)
        os.fchmod(fd, 0o700)
        info = os.fstat(fd)
        if (info.st_uid, info.st_gid, info.st_mode & 0o7777) != (VLAN_OWNER_UID, VLAN_OWNER_GID, 0o700):
            raise NetError('unsafe VLAN index directory ownership or permissions')
        return fd
    except Exception:
        os.close(fd)
        raise


def check_vlan_index_file(dirfd):
    try:
        info = os.lstat('vlans.json', dir_fd=dirfd)
    except FileNotFoundError:
        return False
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != VLAN_OWNER_UID
            or info.st_gid != VLAN_OWNER_GID or stat.S_IMODE(info.st_mode) != 0o600):
        raise NetError('unsafe VLAN index file (symlink, ownership or permissions)')
    return True


def read_vlan_index():
    dirfd = None
    try:
        try:
            dirfd = open_vlan_directory()
        except FileNotFoundError:
            return {}
        if not check_vlan_index_file(dirfd):
            return {}
        fd = os.open('vlans.json', os.O_RDONLY | os.O_NOFOLLOW, dir_fd=dirfd)
        with os.fdopen(fd, 'r', encoding='utf-8') as fh:
            return json.load(fh)
    except OSError as exc:
        raise NetError(f'cannot read VLAN index: {exc}') from exc
    finally:
        if dirfd is not None:
            os.close(dirfd)


def write_vlan_index(records):
    dirfd = None
    temporary = None
    try:
        dirfd = open_vlan_directory(create=True)
        check_vlan_index_file(dirfd)
        # Pin mkstemp to the open directory too: its writable parent cannot
        # redirect this write by renaming the directory and replacing its path.
        fd, temporary = tempfile.mkstemp(prefix='.vlans-', dir=f'/proc/self/fd/{dirfd}')
        with os.fdopen(fd, 'w', encoding='utf-8') as fh:
            json.dump(records, fh)
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(temporary, 'vlans.json', dst_dir_fd=dirfd)
        temporary = None
        os.fsync(dirfd)
    except OSError as exc:
        raise NetExecutionError(f'cannot write VLAN index: {exc}') from exc
    finally:
        if temporary is not None:
            os.unlink(temporary)
        if dirfd is not None:
            os.close(dirfd)


def clean_vlan_rows(iface, rows):
    if not isinstance(rows, list):
        raise NetError('VLAN index rows must be a list')
    clean = []
    for row in rows:
        try:
            if isinstance(row, dict) and any(isinstance(v, str) and re.search(r'[\x00-\x1f\x7f]', v)
                                             for v in row.values()):
                raise NetError('control character in saved settings')
            vlan = validate_vlan_request(row, {'interface': iface, 'vlans': clean})
            if any(v['id'] == vlan['id'] for v in clean):
                raise NetError('duplicate VLAN id')
            clean.append(vlan)
        except (NetError, ValueError, TypeError) as exc:
            logging.warning('Ignoring invalid saved VLAN row for %r: %s', iface, exc)
    return clean


def vlan_records():
    records = read_vlan_index()
    if not isinstance(records, dict):
        raise NetError('VLAN index must be an object')
    clean = {}
    for iface, rows in records.items():
        try:
            vlan_name(iface, 1)
            clean[iface] = clean_vlan_rows(iface, rows)
        except (NetError, ValueError, TypeError) as exc:
            logging.warning('Ignoring invalid saved VLAN rows for %r: %s', iface, exc)
    return clean


def vlan_id(value):
    if isinstance(value, bool) or not re.fullmatch(r'[0-9]+', str(value)) or not 1 <= int(value) <= 4094:
        raise NetError('VLAN id must be between 1 and 4094')
    return int(value)


def vlan_name(iface, vid):
    name = f'{iface}.{vlan_id(vid)}'
    if not re.fullmatch(r'[a-zA-Z0-9_-]+\.[0-9]+', name) or len(name) > 15:
        raise NetError('VLAN interface name must fit within 15 characters')
    return name


def vlan_list(iface, backend):
    # VLAN discovery is optional to base status/apply. Mutations separately
    # refuse an unreadable index; never replace it with an assumed empty index.
    try:
        return {'vlans': vlan_list_rows(iface, backend), 'vlan_error': ''}
    except Exception as exc:
        logging.warning('VLAN status unavailable: %s', exc)
        return {'vlans': [], 'vlan_error': str(exc) or type(exc).__name__}


def vlan_list_rows(iface, backend):
    records = vlan_records().get(iface, [])
    proc = run(['ip', '-d', '-j', 'link', 'show'], check=False)
    try:
        links = json.loads(proc.stdout or '[]')
    except json.JSONDecodeError:
        links = []
    live = {}
    for row in links:
        name = row.get('ifname', '').split('@')[0]
        info = row.get('linkinfo') or {}
        if info.get('info_kind') == 'vlan' and re.fullmatch(re.escape(iface) + r'\.[0-9]+', name):
            live[name] = row
    # Surface externally managed links, but do not take ownership of them.
    known = {v['id'] for v in records}
    records = records + [dict(id=int(name.rsplit('.', 1)[1]), mode='unknown')
                         for name in live if int(name.rsplit('.', 1)[1]) not in known]
    result = []
    for v in sorted(records, key=lambda v: v['id']):
        name = vlan_name(iface, v['id'])
        row = live.get(name, {})
        address, prefix = live_address(name) if row else ('', 0)
        result.append(dict(v, name=name, address=v.get('address') or address,
                           prefix=v.get('prefix') or prefix,
                           live_address=address, live_prefix=prefix,
                           up='UP' in row.get('flags', []),
                           carrier='LOWER_UP' in row.get('flags', []), managed=v['id'] in known))
    return result


def validate_vlan_request(obj, base_status):
    if not isinstance(obj, dict):
        raise NetError('settings must be an object')
    vid = vlan_id(obj.get('id'))
    vlan_name(base_status.get('interface') or '', vid)
    rows = base_status.get('vlans') or []
    if len(rows) >= 8 and not any(v['id'] == vid for v in rows):
        raise NetError('at most 8 VLANs are allowed')
    if any(v['id'] == vid and v.get('managed') is False for v in rows):
        raise NetError('VLAN is not managed by ndimon')
    allow = obj.get('allow_gateway', False)
    if not isinstance(allow, bool):
        raise NetError('allow_gateway must be a boolean')
    if obj.get('gateway') and not allow:
        raise NetError('gateway requires allow_gateway: true')
    if obj.get('mode') == 'static' and (isinstance(obj.get('prefix'), bool) or
            not re.fullmatch(r'[0-9]+', str(obj.get('prefix')))):
        raise NetError('prefix must be between 8 and 30')
    clean = validate_request(obj)
    clean.pop('dns')
    if clean['mode'] == 'static':
        subnet = ipaddress.ip_network(f"{clean['address']}/{clean['prefix']}", strict=False)
        for a, p in ((base_status.get('address'), base_status.get('prefix')),
                     (base_status.get('live_address'), base_status.get('live_prefix'))):
            if a and p and subnet.overlaps(ipaddress.ip_network(f'{a}/{p}', strict=False)):
                raise NetError('overlaps base subnet')
    return dict(id=vid, **clean, allow_gateway=allow)


def vlan_supported(backend):
    if backend not in VLAN_BACKENDS:
        raise NetError(f'cannot apply vlans with {backend}')


def vlan_networkd_base_file(iface):
    # Ask networkd which file matched: distro matches may use globs and may live
    # under /run or /usr/lib. Drop-ins always belong under /etc with that basename.
    proc = run(['networkctl', 'status', '--no-pager', iface], check=False)
    match = re.search(r'Network File:\s+(\S+\.network)', proc.stdout or '')
    fallback_path, fallback_text = read_networkd_file(iface)
    path = match.group(1) if match else fallback_path
    if not path:
        raise NetError('cannot find base networkd .network file')
    text = read_text(path) or (fallback_text if path == fallback_path else '')
    vlan_networkd_validate_base_match(text, iface, vlan_networkd_dropins(path))
    return '/etc/systemd/network/' + os.path.basename(path)


NETWORKD_DIRS = ('/usr/lib/systemd/network', '/usr/local/lib/systemd/network',
                 '/run/systemd/network', '/etc/systemd/network')


def vlan_networkd_dropins(path):
    """Same-name files prefer /etc, /run, /usr/local/lib, /usr/lib; then sort by name."""
    selected = {}
    for directory in NETWORKD_DIRS:
        for candidate in glob.glob(f'{directory}/{os.path.basename(path)}.d/*.conf'):
            selected[os.path.basename(candidate)] = candidate
    return [selected[name] for name in sorted(selected)]


def vlan_networkd_validate_base_match(text, iface, dropins=()):
    selectors = {'Name': [], 'MACAddress': []}
    for document in [text] + [read_text(path) for path in dropins]:
        section = ''
        for raw in document.splitlines():
            line = raw.strip()
            if line.startswith('[') and line.endswith(']'):
                section = line[1:-1]
            elif section == 'Match' and '=' in line and not line.startswith(('#', ';')):
                key, value = line.split('=', 1)
                if key.strip() in selectors:
                    # Non-empty assignments append to the systemd match list;
                    # only an empty assignment resets accumulated selectors.
                    tokens = shlex.split(value, comments=True)
                    if tokens:
                        selectors[key.strip()].extend(tokens)
                    else:
                        selectors[key.strip()] = []
    mac = read_text(f'/sys/class/net/{iface}/address').strip().lower()
    names, addresses = selectors['Name'], selectors['MACAddress']
    exact_name = bool(names) and set(names) == {iface}
    exact_mac = bool(mac) and bool(addresses) and {value.lower() for value in addresses} == {mac}
    # A broad Name selector must not be excused by a MAC selector. Every
    # effective selector must be absent or identify this one interface.
    if not ((exact_name or exact_mac) and (not names or exact_name) and (not addresses or exact_mac)):
        raise NetError(f'base .network must match only {iface}; narrow its effective [Match] to '
                       f'Name={iface} or this NIC\'s MACAddress, including drop-ins, before adding VLANs')


def vlan_networkd_owned_dropin(path, iface):
    text = read_text(path)
    marker = f'# Managed by ndimon-net VLANs for {iface}\n'
    if text.startswith(marker):
        return True
    return (os.path.basename(path) == '91-ndimon-vlans.conf'
            and (re.search(rf'(?m)^VLAN={re.escape(iface)}\.[0-9]+$', text)
                 or text.strip() == '[Network]'))


def vlan_networkd_attachment_files(iface, current):
    """Plan /etc attachments after existing drop-ins without modifying the base."""
    current_name = os.path.basename(current)
    future_name = f'05-ndimon-{iface}.network'
    bases = {current_name, future_name}
    for candidate in glob.glob('/etc/systemd/network/*.network.d/*.conf'):
        if vlan_networkd_owned_dropin(candidate, iface):
            bases.add(os.path.basename(os.path.dirname(candidate))[:-2])
    planned = []
    for base in sorted(bases):
        dropins = vlan_networkd_dropins(base)
        if base != current_name:
            # The base helper may create its exact-name file later. Existing
            # files/drop-ins for that name must be checked before attaching.
            main = f'[Match]\nName={iface}\n' if base == future_name else ''
            for directory in reversed(NETWORKD_DIRS):
                path = f'{directory}/{base}'
                if glob.glob(path):
                    main = read_text(path)
                    break
            vlan_networkd_validate_base_match(main, iface, dropins)
        owned = [path for path in glob.glob(f'/etc/systemd/network/{base}.d/*.conf')
                 if vlan_networkd_owned_dropin(path, iface)]
        others = [os.path.basename(path) for path in dropins if path not in owned]
        name = max(['91-ndimon-vlans.conf'] + [os.path.basename(path) for path in owned])
        if others and name <= max(others):
            name = max(others) + '.ndimon-vlans.conf'
        if len(os.fsencode(name)) > 255:
            raise NetError(f'cannot place VLAN drop-in after existing drop-ins for {base}: filename too long')
        planned.append((f'/etc/systemd/network/{base}.d/{name}', owned))
    return planned


def vlan_networkd_verify(name):
    expected = f'/etc/systemd/network/00-ndimon-{name}.network'
    deadline = time.monotonic() + 10
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise NetExecutionError(f'timed out waiting for networkd VLAN {name}')
        proc = run(['networkctl', 'status', '--no-pager', name], check=False, timeout=min(2, remaining))
        match = re.search(r'Network File:\s+(\S+\.network)', proc.stdout or '')
        if proc.returncode == 0 and match:
            selected = match.group(1)
            if selected != expected:
                raise NetExecutionError(f'another .network file captures this interface: {selected}')
            return
        # networkd creates and configures VLAN links asynchronously after reload.
        time.sleep(min(0.5, max(0, deadline - time.monotonic())))


def vlan_netplan_parent(iface):
    import yaml
    definitions = {}
    for path in netplan_files():
        try:
            doc = yaml.safe_load(read_text(path)) or {}
            entries = (doc.get('network') or {}).get('ethernets') or {}
            for key, entry in entries.items():
                entry = entry or {}
                previous = definitions.setdefault(key, {})
                merged_match = {**(previous.get('match') or {}), **(entry.get('match') or {})}
                previous.update(entry)
                if merged_match:
                    previous['match'] = merged_match
        except (yaml.YAMLError, AttributeError, TypeError, ValueError) as exc:
            raise NetError(f'cannot read netplan ethernet definitions: {path}') from exc
    mac = read_text(f'/sys/class/net/{iface}/address').strip().lower()
    matches = []
    for key, entry in definitions.items():
        match = entry.get('match') or {}
        pattern = match.get('name')
        configured_mac = match.get('macaddress')
        renamed = entry.get('set-name') == iface
        # match.name describes the pre-rename name. set-name identifies the
        # interface after rename, but must still agree with any MAC selector.
        name_matches = not pattern or (isinstance(pattern, str) and fnmatch.fnmatchcase(iface, pattern)) or renamed
        mac_matches = not configured_mac or (isinstance(configured_mac, str) and mac == configured_mac.lower())
        selected = bool(pattern or configured_mac) or renamed or key == iface
        if selected and name_matches and mac_matches:
            matches.append(key)
    if len(matches) > 1:
        raise NetError(f'ambiguous parent definition for {iface}: ' + ', '.join(matches))
    if matches:
        return matches[0]
    raise NetError(f'no netplan ethernet definition for {iface}')


def vlan_render(backend, iface, rows, write=None):
    """Write only ndimon's VLAN files. Never rewrite a base configuration file."""
    write = write or atomic_write
    rows = clean_vlan_rows(iface, rows)
    if backend == 'netplan':
        import yaml
        parent = vlan_netplan_parent(iface)
        vlans = {}
        for v in rows:
            entry = dict(id=v['id'], link=parent, dhcp4=v['mode'] == 'dhcp', optional=True)
            if v['mode'] == 'static':
                entry['addresses'] = [f"{v['address']}/{v['prefix']}"]
                if v['gateway']:
                    entry['routes'] = [dict(to='default', via=v['gateway'], metric=2048)]
            else:
                entry['dhcp4-overrides'] = {'use-routes': v['allow_gateway'], 'use-dns': False, 'route-metric': 2048}
            vlans[vlan_name(iface, v['id'])] = entry
        write('/etc/netplan/91-ndimon-vlans.yaml', yaml.safe_dump(
            {'network': {'version': 2, 'vlans': vlans}}, sort_keys=False), 0o600)
    elif backend == 'networkd':
        path = vlan_networkd_base_file(iface)
        marker = f'# Managed by ndimon-net VLANs for {iface}\n'
        dropin = marker + '[Network]\n' + ''.join(f"VLAN={vlan_name(iface, v['id'])}\n" for v in rows)
        for target, previous in vlan_networkd_attachment_files(iface, path):
            # Clear older owned attachments when a later administrator file
            # forces our attachment to move; leave every foreign file intact.
            for old in previous:
                if old != target:
                    write(old, marker + '[Network]\n', 0o644)
            write(target, dropin, 0o644)
        for v in rows:
            name = vlan_name(iface, v['id'])
            stem = f'/etc/systemd/network/00-ndimon-{name}'
            write(stem + '.netdev', f"[NetDev]\nName={name}\nKind=vlan\n\n[VLAN]\nId={v['id']}\n", 0o644)
            body = f'[Match]\nName={name}\n\n[Link]\nRequiredForOnline=no\n\n[Network]\n'
            if v['mode'] == 'dhcp':
                body += 'DHCP=ipv4\nIPv6AcceptRA=no\n\n[DHCPv4]\nUseDNS=no\n'
                body += f"UseRoutes={'yes' if v['allow_gateway'] else 'no'}\nRouteMetric=2048\n"
            else:
                body += f"Address={v['address']}/{v['prefix']}\n"
                if v['gateway']:
                    body += f"\n[Route]\nGateway={v['gateway']}\nMetric=2048\n"
            write(stem + '.network', body, 0o644)
    elif backend == 'ifupdown':
        if 'interfaces.d' not in read_text('/etc/network/interfaces'):
            raise NetError('/etc/network/interfaces does not include interfaces.d')
        body = ''
        guarded = []
        dhcp_names = []
        for v in rows:
            name = vlan_name(iface, v['id'])
            body += f"auto {name}\niface {name} inet {v['mode']}\n    vlan-raw-device {iface}\n"
            if v['mode'] == 'static':
                body += f"    address {v['address']}/{v['prefix']}\n"
                if v['gateway']:
                    body += f"    gateway {v['gateway']}\n    metric 2048\n"
            else:
                body += '    metric 2048\n'
                dhcp_names.append(name)
                if not v['allow_gateway']:
                    guarded.append(name)
            body += '\n'
        write('/etc/network/interfaces.d/ndimon-vlans', body, 0o644)
        hook = '# Managed by ndimon-net; sourced by dhclient.\n'
        if dhcp_names:
            hook += 'case "$interface" in\n    ' + '|'.join(dhcp_names) + ')\n'
            hook += '        unset new_domain_name_servers new_domain_name new_domain_search\n        ;;\nesac\n'
        if guarded:
            hook += 'case "$interface" in\n    ' + '|'.join(guarded) + ')\n'
            hook += '        unset new_routers new_static_routes new_classless_static_routes new_rfc3442_classless_static_routes\n'
            hook += '        ;;\nesac\n'
        write('/etc/dhcp/dhclient-enter-hooks.d/ndimon-vlans', hook, 0o644)


def guard_session_vlan(iface, vid, requester_ip):
    if not requester_ip:
        return
    try:
        requester = ipaddress.ip_address(requester_ip)
        if isinstance(requester, ipaddress.IPv6Address) and requester.ipv4_mapped:
            requester = requester.ipv4_mapped
    except ValueError as exc:
        raise NetError('invalid requester IP') from exc
    proc = run(['ip', '-j', 'addr', 'show', 'dev', vlan_name(iface, vid)], check=False)
    for row in json.loads(proc.stdout or '[]'):
        for addr in row.get('addr_info', []):
            if addr.get('local') and addr.get('prefixlen') is not None:
                subnet = ipaddress.ip_network(f"{addr['local']}/{addr['prefixlen']}", strict=False)
                if requester in subnet:
                    raise NetError('this VLAN carries your session')


def vlan_file_snapshot(path):
    try:
        info = os.stat(path, follow_symlinks=False)
    except FileNotFoundError:
        return None
    if not stat.S_ISREG(info.st_mode):
        raise NetError(f'unsafe VLAN configuration file: {path}')
    return read_text(path), stat.S_IMODE(info.st_mode)


class VlanFileChanges:
    def __init__(self):
        self.previous = {}

    def write(self, path, text, mode=0o600):
        if path not in self.previous:
            self.previous[path] = vlan_file_snapshot(path)
        atomic_write(path, text, mode)

    def remove(self, path):
        if path not in self.previous:
            self.previous[path] = vlan_file_snapshot(path)
        try:
            os.remove(path)
        except FileNotFoundError:
            pass

    def rollback(self):
        for path, previous in reversed(list(self.previous.items())):
            try:
                if previous is None:
                    try:
                        os.remove(path)
                    except FileNotFoundError:
                        pass
                else:
                    atomic_write(path, previous[0], previous[1])
            except Exception as exc:
                logging.error('VLAN configuration rollback failed for %s: %s', path, exc)


def vlan_nm_settings(profile, vlan):
    run(['nmcli', 'connection', 'modify', profile,
         'ipv4.method', 'auto' if vlan['mode'] == 'dhcp' else 'manual',
         'ipv4.addresses', f"{vlan['address']}/{vlan['prefix']}" if vlan['mode'] == 'static' else '',
         'ipv4.gateway', vlan['gateway'], 'ipv4.never-default', 'no' if vlan['allow_gateway'] else 'yes',
         'ipv4.route-metric', '2048', 'ipv6.method', 'disabled', 'connection.autoconnect', 'yes'])
    run(['nmcli', 'connection', 'modify', profile, 'ipv4.ignore-auto-dns', 'yes',
         'ipv4.ignore-auto-routes', 'yes' if vlan['mode'] == 'dhcp' and not vlan['allow_gateway'] else 'no',
         'ipv4.may-fail', 'yes', 'connection.wait-device-timeout', '0'])


def vlan_activate_backend(backend, iface, name, existing):
    if backend == 'netplan':
        run(['netplan', 'generate'], timeout=30)
        run(['netplan', 'apply'], timeout=40)
    elif backend == 'networkd':
        run(['networkctl', 'reload'])
        run(['networkctl', 'reconfigure', iface], timeout=30)
    else:
        if existing:
            run(['ifdown', name], check=False, timeout=30)
        run(['ifup', name], timeout=40)


def rollback_vlan_index(records, name):
    try:
        write_vlan_index(records)
    except Exception as cleanup:
        logging.error('VLAN index rollback failed for %s: %s', name, cleanup)


def vlan_apply(backend, iface, vlan, requester_ip=None):
    vlan_supported(backend)
    doc = status_document()
    inventory = vlan_list(iface, backend)
    if inventory['vlan_error']:
        raise NetError(f"VLAN index unavailable: {inventory['vlan_error']}")
    vlan = validate_vlan_request(vlan, dict(doc, interface=iface, vlans=inventory['vlans']))
    guard_session_vlan(iface, vlan['id'], requester_ip)
    records = vlan_records()
    previous_rows = records.get(iface, [])
    previous = next((v for v in previous_rows if v['id'] == vlan['id']), None)
    rows = sorted([v for v in previous_rows if v['id'] != vlan['id']] + [vlan], key=lambda v: v['id'])
    name = vlan_name(iface, vlan['id'])
    profile = f'ndimon-vlan-{name}'
    changes = VlanFileChanges()
    profile_created = False
    profile_modified = False
    activation_attempted = False
    index_attempted = False
    try:
        run(['modprobe', '8021q'])
        changes.write('/etc/modules-load.d/ndimon-8021q.conf', '8021q\n', 0o644)
        if backend == 'networkmanager':
            if previous is None:
                run(['nmcli', 'connection', 'add', 'type', 'vlan', 'con-name', profile,
                     'ifname', name, 'dev', iface, 'id', str(vlan['id'])])
                profile_created = True
            profile_modified = True
            vlan_nm_settings(profile, vlan)
            run(['nmcli', 'connection', 'up', profile], timeout=40)
        else:
            vlan_render(backend, iface, rows, write=changes.write)
            activation_attempted = True
            vlan_activate_backend(backend, iface, name, previous is not None)
            if backend == 'networkd':
                vlan_networkd_verify(name)
        updated = dict(records)
        updated[iface] = rows
        index_attempted = True
        write_vlan_index(updated)
    except Exception:
        if backend == 'ifupdown' and activation_attempted:
            # ifupdown must read the just-applied configuration while removing
            # its addresses and configured-state entry. Restoring the old file
            # first can make ifup skip a still-configured interface on rollback.
            try:
                run(['ifdown', '--force', name], timeout=30)
            except Exception as cleanup:
                logging.error('VLAN deactivation rollback failed for %s: %s', name, cleanup)
        changes.rollback()
        if index_attempted:
            rollback_vlan_index(records, name)
        try:
            if backend == 'networkmanager':
                if profile_created:
                    run(['nmcli', 'connection', 'delete', profile])
                elif previous is not None and profile_modified:
                    vlan_nm_settings(profile, previous)
                    run(['nmcli', 'connection', 'up', profile], timeout=40)
            elif activation_attempted:
                if previous is None:
                    run(['ip', 'link', 'delete', 'dev', name], check=False)
                # Reload the restored persistent files; updates retain their old
                # configuration rather than deleting an existing managed VLAN.
                if backend != 'ifupdown' or previous is not None:
                    vlan_activate_backend(backend, iface, name, False)
        except Exception as cleanup:
            logging.error('VLAN activation rollback failed for %s: %s', name, cleanup)
        raise


def vlan_remove(backend, iface, vlan_id, requester_ip=None):
    vlan_supported(backend)
    name = vlan_name(iface, vlan_id)
    guard_session_vlan(iface, vlan_id, requester_ip)
    records = vlan_records()
    previous_rows = records.get(iface, [])
    previous = next((v for v in previous_rows if v['id'] == int(vlan_id)), None)
    if previous is None:
        raise NetError('VLAN is not managed by ndimon')
    rows = [v for v in previous_rows if v['id'] != int(vlan_id)]
    profile = f'ndimon-vlan-{name}'
    changes = VlanFileChanges()
    backend_started = False
    index_attempted = False
    try:
        backend_started = True
        if backend == 'networkmanager':
            run(['nmcli', 'connection', 'delete', profile])
        else:
            if backend == 'ifupdown':
                run(['ifdown', '--force', name], timeout=30)
            vlan_render(backend, iface, rows, write=changes.write)
            if backend == 'networkd':
                for suffix in ('.netdev', '.network'):
                    changes.remove(f'/etc/systemd/network/00-ndimon-{name}{suffix}')
                run(['networkctl', 'reload'])
            elif backend == 'netplan':
                run(['netplan', 'generate'], timeout=30)
                run(['netplan', 'apply'], timeout=40)
            run(['ip', 'link', 'delete', 'dev', name], check=False)
        updated = dict(records)
        updated[iface] = rows
        index_attempted = True
        write_vlan_index(updated)
    except Exception:
        changes.rollback()
        # replace may already have succeeded before a directory fsync error or
        # SIGTERM. Restore the old index as well as the old backend files.
        if index_attempted:
            rollback_vlan_index(records, name)
        if backend_started:
            try:
                if backend == 'networkmanager':
                    exists = run(['nmcli', '-g', 'connection.id', 'connection', 'show', profile], check=False)
                    if exists.returncode != 0:
                        run(['nmcli', 'connection', 'add', 'type', 'vlan', 'con-name', profile,
                             'ifname', name, 'dev', iface, 'id', str(previous['id'])])
                    vlan_nm_settings(profile, previous)
                    run(['nmcli', 'connection', 'up', profile], timeout=40)
                else:
                    vlan_activate_backend(backend, iface, name, False)
                    if backend == 'networkd':
                        vlan_networkd_verify(name)
            except Exception as cleanup:
                logging.error('VLAN removal rollback failed for %s: %s', name, cleanup)
        raise


def cmd_vlan(cmd, args):
    doc = status_document()
    iface, backend = doc['interface'], doc['backend']
    if cmd == 'vlan-list' and not args:
        return {'ok': True, 'vlans': doc['vlans'], 'vlan_capable': doc['vlan_capable'], 'vlan_reason': doc['vlan_reason'], 'vlan_error': doc.get('vlan_error', '')}
    vlan_supported(backend)
    requester_ip = None
    if len(args) >= 2 and args[-2] == '--requester-ip':
        requester_ip = args[-1]
        args = args[:-2]
    if cmd == 'vlan-apply' and not args:
        try:
            obj = json.load(sys.stdin)
        except (ValueError, TypeError) as exc:
            raise NetError('settings must be valid JSON') from exc
        vlan_apply(backend, iface, validate_vlan_request(obj, doc), requester_ip=requester_ip)
    elif cmd == 'vlan-remove' and len(args) == 1:
        vlan_remove(backend, iface, vlan_id(args[0]), requester_ip=requester_ip)
    else:
        raise NetError('invalid VLAN arguments')
    return {'ok': True}


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
    properties = ['--property=RuntimeMaxSec=90s', '--property=TimeoutStopSec=90s'] if unit == 'ndimon-profiles-apply' else []
    run(["systemd-run", f"--unit={unit}", f"--on-active={int(seconds)}s", "--collect",
         '--timer-property=AccuracySec=100ms', '--timer-property=RandomizedDelaySec=0',
         *properties, *argv], timeout=15)


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
    if os.path.exists('/var/lib/ndimon-network/committed.json') or os.path.exists('/var/lib/ndimon-network/pending.json'):
        raise NetError('use the complete network profile transaction')
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


@contextmanager
def vlan_interruptible():
    previous = signal.getsignal(signal.SIGTERM)
    def terminate(signum, frame):
        # A second timeout signal must not interrupt rollback itself.
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        raise NetExecutionError('VLAN change interrupted by SIGTERM')
    signal.signal(signal.SIGTERM, terminate)
    try:
        yield
    finally:
        signal.signal(signal.SIGTERM, previous)


def main(argv):
    cmd = argv[1] if len(argv) > 1 else ""
    try:
        if cmd.startswith('profiles-'):
            import ndimon_network_profiles
            with vlan_interruptible():
                result = ndimon_network_profiles.dispatch(sys.modules[__name__], cmd, argv[2:])
            json.dump(result, sys.stdout)
            sys.stdout.write('\n')
        elif cmd in ("vlan-list", "vlan-apply", "vlan-remove"):
            if cmd != 'vlan-list':
                raise NetError('use the complete network profile transaction; immediate VLAN writes are disabled')
            def work():
                json.dump(cmd_vlan(cmd, argv[2:]), sys.stdout)
                sys.stdout.write("\n")
            if cmd in ("vlan-apply", "vlan-remove"):
                with vlan_interruptible():
                    locked(work)
            else:
                locked(work)
        elif cmd == "status":
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
    except NetExecutionError as exc:
        sys.stderr.write(str(exc) + "\n")
        return 3 if cmd.startswith("vlan-") else 1
    except NetError as exc:
        sys.stderr.write(str(exc) + "\n")
        return 2 if cmd.startswith("vlan-") else 1
    except Exception as exc:
        sys.stderr.write(f"network helper failed: {exc}\n")
        return 3 if cmd.startswith("vlan-") else 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
