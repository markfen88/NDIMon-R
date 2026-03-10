#!/usr/bin/env python3
"""NDIMon-R - Rockchip NDI Display Appliance"""
import os, json, time, subprocess, threading, socket, re, glob, signal, ctypes, fcntl
import logging
from pathlib import Path
from functools import wraps
from datetime import datetime, timedelta

import zipfile
from flask import Flask, render_template, jsonify, request, redirect, url_for, session, flash, send_file
from PIL import Image
import psutil

# ── Config ────────────────────────────────────────────────────────────────────
BASE_DIR    = Path("/opt/ndimon-r")
CONFIG_FILE = BASE_DIR / "config.json"
UPLOAD_DIR  = BASE_DIR / "uploads"
LOG_DIR     = BASE_DIR / "logs"
STATIC_DIR  = BASE_DIR / "static"
BACKUP_DIR  = BASE_DIR / "backups"
BACKUP_TTL  = 15 * 60  # seconds — generated zips auto-deleted after this

logging.basicConfig(
    level=logging.DEBUG,
    format="%(asctime)s [%(levelname)s] %(message)s",
    handlers=[logging.FileHandler(LOG_DIR / "app.log"), logging.StreamHandler()]
)
log = logging.getLogger(__name__)

app = Flask(__name__, template_folder=str(BASE_DIR / "templates"),
            static_folder=str(STATIC_DIR))

# Persist secret key so sessions survive service restarts
_secret_key_file = BASE_DIR / ".secret_key"
if _secret_key_file.exists():
    app.secret_key = _secret_key_file.read_text().strip()
else:
    app.secret_key = os.urandom(32).hex()
    _secret_key_file.write_text(app.secret_key)
    _secret_key_file.chmod(0o600)

# ── Helpers ───────────────────────────────────────────────────────────────────
def load_config():
    with open(CONFIG_FILE) as f:
        return json.load(f)

def save_config(cfg):
    with open(CONFIG_FILE, "w") as f:
        json.dump(cfg, f, indent=2)
    _write_ndi_sdk_config(cfg)

def _write_ndi_sdk_config(cfg):
    """Write ~/.ndi/ndi-config.v1.json so the NDI SDK library uses our discovery servers/IPs.
    NDI v6 config requires a top-level 'ndi' key.
    networks.ips   = comma-separated machine IPs to query directly via port 5960
    networks.discovery = NDI Discovery Server address (port 5959 default)
    Both point to the same user-configured list since many setups run both services."""
    try:
        servers = [s.strip() for s in cfg["ndi"].get("discovery_servers", []) if s.strip()]
        groups  = cfg["ndi"].get("groups", ["Public"])
        group_str = ",".join(g.strip() for g in groups if g.strip()) or "Public"
        networks = {}
        if servers:
            ip_str = ",".join(servers)
            networks["ips"]       = ip_str   # direct per-machine source query (port 5960)
            networks["discovery"] = ip_str   # NDI discovery server (port 5959)
        ndi_cfg = {
            "ndi": {
                "networks": networks,
                "groups":   {"send": group_str, "recv": group_str},
            }
        }
        for home in ["/root", "/home/radxa"]:
            ndi_dir = Path(home) / ".ndi"
            try:
                ndi_dir.mkdir(parents=True, exist_ok=True)
                with open(ndi_dir / "ndi-config.v1.json", "w") as f:
                    json.dump(ndi_cfg, f, indent=2)
            except Exception as dir_err:
                log.warning(f"Could not write NDI config to {ndi_dir}: {dir_err}")
    except Exception as e:
        log.warning(f"Could not write NDI SDK config: {e}")

def get_ip():
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(("8.8.8.8", 80))
        ip = s.getsockname()[0]; s.close(); return ip
    except Exception:
        return "0.0.0.0"

def get_mac_suffix():
    """Return last 4 hex digits of the primary network interface MAC (upper-case, no colons)."""
    try:
        for iface_path in sorted(Path("/sys/class/net").iterdir()):
            if iface_path.name == "lo":
                continue
            addr_file = iface_path / "address"
            if addr_file.exists():
                mac = addr_file.read_text().strip().replace(":", "")
                if mac and mac != "000000000000":
                    return mac[-4:].upper()
    except Exception:
        pass
    return "0000"

def _migrate_alias():
    """One-time migration: replace old default 'NDI Monitor' alias with 'NDIMON-XXXX'."""
    try:
        cfg = load_config()
        if cfg.get("ndi", {}).get("alias") in ("NDI Monitor", "", None):
            cfg.setdefault("ndi", {})["alias"] = f"NDIMON-{get_mac_suffix()}"
            save_config(cfg)
            log.info(f"Migrated NDI alias to {cfg['ndi']['alias']}")
    except Exception as e:
        log.warning(f"Could not migrate alias: {e}")

def require_auth(f):
    @wraps(f)
    def decorated(*args, **kwargs):
        cfg = load_config()
        if cfg["auth"]["enabled"] and not session.get("authenticated"):
            return redirect(url_for("login"))
        return f(*args, **kwargs)
    return decorated

def get_system_uptime():
    try:
        with open("/proc/uptime") as f:
            secs = float(f.read().split()[0])
        return str(timedelta(seconds=int(secs)))
    except Exception:
        return "N/A"

def _hex_to_gst_argb(hex_color, alpha=0xFF):
    """Convert #RRGGBB to GStreamer ARGB uint32 (used for textoverlay color property)."""
    try:
        h = hex_color.lstrip("#")
        r, g, b = int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16)
        return (alpha << 24) | (r << 16) | (g << 8) | b
    except Exception:
        return 0xFFFFFFFF  # default white

def _overlay_props(cfg):
    """Return GStreamer textoverlay property string for color and keyed mode."""
    color_hex = cfg.get("video", {}).get("osd_color", "#ffffff")
    keyed     = cfg.get("video", {}).get("osd_keyed", False)
    fg        = _hex_to_gst_argb(color_hex)
    if keyed:
        # Keyed: no shaded box, just colored text with black outline for readability
        return f"color={fg} outline-color=0xFF000000 shaded-background=false"
    else:
        return f"color={fg} shaded-background=true"

# ── Splash Image Generator ───────────────────────────────────────────────────
def _generate_splash_image(cfg, display_w=1920, display_h=1080):
    """Compose splash PNG: solid color background + centered logo (PNG with alpha)."""
    splash_cfg = cfg.get("splash", {})
    bg_raw = splash_cfg.get("bg_color", "#1a1d23").lstrip("#")
    try:
        bg_color = tuple(int(bg_raw[i:i+2], 16) for i in (0, 2, 4))
    except Exception:
        bg_color = (26, 29, 35)

    img = Image.new("RGB", (display_w, display_h), bg_color)

    logo_path = splash_cfg.get("logo", "")
    # Migrate old install-dir path transparently if the file moved
    if logo_path and not Path(logo_path).exists():
        fallback = str(BASE_DIR / "static" / "img" / "logo.png")
        if Path(fallback).exists():
            logo_path = fallback
    if logo_path and Path(logo_path).exists():
        try:
            logo = Image.open(logo_path).convert("RGBA")
            lw, lh = logo.size
            # Scale so the LARGEST dimension fits within 60% of the screen
            max_w = int(display_w * 0.6)
            max_h = int(display_h * 0.6)
            scale = min(max_w / lw, max_h / lh, 1.0)  # never upscale
            new_w = max(1, int(lw * scale))
            new_h = max(1, int(lh * scale))
            logo = logo.resize((new_w, new_h), Image.LANCZOS)
            x = (display_w - new_w) // 2
            y = (display_h - new_h) // 2
            img.paste(logo, (x, y), logo)
        except Exception as e:
            log.warning(f"Splash logo load failed: {e}")

    out = STATIC_DIR / "splash_generated.png"
    img.save(str(out), "PNG")
    return str(out)

# ── Backup / Restore ─────────────────────────────────────────────────────────
def _cleanup_old_backups():
    """Delete backup zips older than BACKUP_TTL. Safe to call at any time."""
    try:
        BACKUP_DIR.mkdir(parents=True, exist_ok=True)
        now = time.time()
        for f in BACKUP_DIR.glob("*.zip"):
            try:
                if now - f.stat().st_mtime > BACKUP_TTL:
                    f.unlink()
                    log.info(f"Backup cleanup: removed {f.name}")
            except Exception:
                pass
    except Exception as e:
        log.warning(f"Backup cleanup error: {e}")

def _backup_cleanup_thread():
    """Sweep backup dir every 60 s so zips never outlast a restart."""
    while True:
        time.sleep(60)
        _cleanup_old_backups()

# ── NDI Source Discovery ──────────────────────────────────────────────────────
_ndi_sources_cache = []   # list of source names
_ndi_url_cache     = {}   # name -> url
_ndi_sources_lock  = threading.Lock()

class _NDIFindCreate(ctypes.Structure):
    _fields_ = [
        ("show_local_sources", ctypes.c_bool),
        ("p_groups",           ctypes.c_char_p),
        ("p_extra_ips",        ctypes.c_char_p),
    ]

class _NDISource(ctypes.Structure):
    _fields_ = [
        ("p_ndi_name",    ctypes.c_char_p),
        ("p_url_address", ctypes.c_char_p),
    ]

_ndi_lib = None
def _get_ndi_lib():
    global _ndi_lib
    if _ndi_lib is None:
        try:
            lib = ctypes.CDLL("/usr/local/lib/libndi.so.6")
            lib.NDIlib_initialize.restype = ctypes.c_bool
            lib.NDIlib_initialize()
            _ndi_lib = lib
        except Exception as e:
            log.warning(f"NDI lib load failed: {e}")
    return _ndi_lib

def _extra_ips_from_config():
    try:
        cfg = load_config()
        servers = [s.strip() for s in cfg["ndi"].get("discovery_servers", []) if s.strip()]
        return ",".join(servers).encode() if servers else None
    except Exception:
        return None

def _groups_from_config():
    try:
        cfg = load_config()
        groups = [g.strip() for g in cfg["ndi"].get("groups", []) if g.strip()]
        return ",".join(groups).encode() if groups else None
    except Exception:
        return None

def discover_ndi_sources():
    """Discover NDI sources using SDK directly. Returns (names_list, url_map)."""
    sources = []
    urls    = {}
    lib = _get_ndi_lib()
    if lib:
        try:
            lib.NDIlib_find_create_v2.restype = ctypes.c_void_p
            settings = _NDIFindCreate(
                show_local_sources=True,
                p_groups=_groups_from_config(),
                p_extra_ips=_extra_ips_from_config(),
            )
            find = lib.NDIlib_find_create_v2(ctypes.byref(settings))
            if find:
                wait = lib.NDIlib_find_wait_for_sources
                wait.restype = ctypes.c_bool
                wait.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
                wait(find, 5000)

                get = lib.NDIlib_find_get_current_sources
                get.restype = ctypes.POINTER(_NDISource)
                get.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint32)]
                count = ctypes.c_uint32(0)
                ptr = get(find, ctypes.byref(count))
                for i in range(count.value):
                    name = ptr[i].p_ndi_name
                    url  = ptr[i].p_url_address
                    if name:
                        name = name.decode("utf-8", errors="replace")
                        if name not in sources:
                            sources.append(name)
                        if url:
                            urls[name] = url.decode("utf-8", errors="replace")
                lib.NDIlib_find_destroy.argtypes = [ctypes.c_void_p]
                lib.NDIlib_find_destroy(find)
        except Exception as e:
            log.warning(f"NDI SDK discovery error: {e}")
    # Fallback: avahi mDNS
    try:
        result = subprocess.run(
            ["avahi-browse", "-pt", "_ndi._tcp"],
            capture_output=True, text=True, timeout=5
        )
        for line in result.stdout.splitlines():
            parts = line.split(";")
            if len(parts) > 3 and parts[0] == "=":
                name = parts[3]
                if name and name not in sources:
                    sources.append(name)
    except Exception:
        pass
    return sorted(set(sources)), urls

def background_ndi_discovery():
    global _ndi_sources_cache, _ndi_url_cache
    while True:
        try:
            found, urls = discover_ndi_sources()
            with _ndi_sources_lock:
                _ndi_sources_cache = found
                _ndi_url_cache.update(urls)
            # Persist newly discovered URLs to config so they survive reboots
            if urls:
                cfg = load_config()
                stored = cfg.setdefault("ndi_source_urls", {})
                changed = False
                for name, url in urls.items():
                    if stored.get(name) != url:
                        stored[name] = url
                        changed = True
                if changed:
                    save_config(cfg)
        except Exception as e:
            log.error(f"Discovery thread error: {e}")
        time.sleep(5)

# ── GStreamer Pipeline Manager ────────────────────────────────────────────────
class PipelineManager:
    def __init__(self):
        self.pipelines    = {}   # display_id -> {proc, source, start_time}
        self.splash_procs = {}   # display_id -> proc
        self.lock         = threading.Lock()
        self._start_locks = {}   # display_id -> threading.Lock (per-display start guard)
        self._detect_board()
        self._detect_displays()
        self._probe_kmssink_bus_id()
        self._probe_ndisrc_props()
        self._probe_rga()

    def _detect_board(self):
        compatible = ""
        try:
            with open("/proc/device-tree/compatible", "rb") as f:
                compatible = f.read().decode("utf-8", errors="ignore").replace("\x00", " ")
        except Exception:
            pass
        if "rk3588" in compatible:
            self.board = "rk3588"
        elif "rk3399" in compatible:
            self.board = "rk3399"
        elif "bcm2712" in compatible or "Raspberry Pi 5" in Path("/proc/cpuinfo").read_text(errors="ignore"):
            self.board = "rpi5"
        elif "bcm2711" in compatible:
            self.board = "rpi4"
        else:
            self.board = "generic"

        # Probe all available decoders — board-agnostic, works on any Linux SBC or x86
        self.hw_dec, self.mpp_avail, self.decoder_chain = self._probe_hw_decoder()
        log.info(f"Board: {self.board}, HW decoder: {self.hw_dec}")

    def _probe_hw_decoder(self):
        """Probe for the best available hardware video decoder by scanning GStreamer plugin .so files.
        Returns (primary_element, hw_available, decoder_chain) where decoder_chain is an ordered
        list of all discovered decoders (best first) for future HX/compressed-NDI support.

        Priority order:
          1. Rockchip MPP  — RK3399/RK3588 (mppvideodec / rkmppdec)
          2. gst-va        — VA-API via libva (Intel, AMD, Qualcomm Adreno, some Mali)
          3. gst-vaapi     — Legacy VA-API plugin (older distros / Mesa)
          4. V4L2 stateless— RPi 5, MediaTek, Allwinner H6+, modern Qualcomm (v4l2slh264dec)
          5. V4L2 M2M      — RPi 4/3, older SoCs (v4l2h264dec)
          6. avdec_h264    — Software fallback (libav/ffmpeg, always available)
        """
        plugin_dirs = [
            "/usr/lib/aarch64-linux-gnu/gstreamer-1.0",
            "/usr/lib/arm-linux-gnueabihf/gstreamer-1.0",
            "/usr/lib/x86_64-linux-gnu/gstreamer-1.0",
            "/usr/lib/gstreamer-1.0",
            "/lib/aarch64-linux-gnu/gstreamer-1.0",
        ]

        chain = []

        # 1. Rockchip MPP
        for d in plugin_dirs:
            if glob.glob(f"{d}/libgstrockchipmpp.so"):
                chain.append(("mppvideodec", "Rockchip MPP (mppvideodec)"))
                break
            if glob.glob(f"{d}/libgstrkmppdec.so") or glob.glob(f"{d}/libgstmpp.so"):
                chain.append(("rkmppdec", "Rockchip MPP (rkmppdec)"))
                break

        # 2. gst-va (new VA-API, GStreamer ≥1.20, libva2)
        va_device = glob.glob("/dev/dri/renderD1*")
        if va_device:
            for d in plugin_dirs:
                if glob.glob(f"{d}/libgstva.so"):
                    chain.append(("vah264dec", "VA-API/libva (gst-va)"))
                    break

        # 3. Legacy gst-vaapi plugin
        if va_device:
            for d in plugin_dirs:
                if glob.glob(f"{d}/libgstvaapi.so"):
                    chain.append(("vaapih264dec", "VA-API (gst-vaapi)"))
                    break

        # 4. V4L2 stateless codecs (RPi 5 / modern SoCs via libgstv4l2codecs.so)
        for d in plugin_dirs:
            if glob.glob(f"{d}/libgstv4l2codecs.so"):
                chain.append(("v4l2slh264dec", "V4L2 stateless (v4l2slh264dec)"))
                break

        # 5. V4L2 M2M stateful (RPi 4/3, older SoCs — present in libgstv4l2.so when M2M device found)
        v4l2_decoders = []
        for name_path in sorted(glob.glob("/sys/class/video4linux/video*/name")):
            try:
                dev_name = Path(name_path).read_text().strip().lower()
                if any(kw in dev_name for kw in ("h264", "h265", "hevc", "decoder", "codec", "m2m")):
                    v4l2_decoders.append(dev_name)
            except Exception:
                pass
        if v4l2_decoders:
            for d in plugin_dirs:
                if glob.glob(f"{d}/libgstv4l2.so"):
                    chain.append(("v4l2h264dec", f"V4L2 M2M ({', '.join(v4l2_decoders[:2])})"))
                    break

        # 6. Software fallback — avdec_h264 from gstreamer1.0-plugins-good (libav/ffmpeg)
        for d in plugin_dirs:
            if glob.glob(f"{d}/libgstlibav.so") or glob.glob(f"{d}/libgstavcodec.so"):
                chain.append(("avdec_h264", "Software/libav (avdec_h264)"))
                break
        if not any(el == "avdec_h264" for el, _ in chain):
            chain.append(("avdec_h264", "Software/libav (avdec_h264)"))  # always add as final fallback

        hw_available = len(chain) > 1 or (chain and chain[0][0] not in ("avdec_h264",))
        primary = chain[0][0] if chain else "avdec_h264"

        log.info(f"Decoder chain: {[desc for _, desc in chain]}")
        return primary, hw_available, [el for el, _ in chain]

    # Keep old name as alias for call sites that pass board-specific context
    def _probe_mpp_decoder(self):
        primary, avail, _ = self._probe_hw_decoder()
        return primary, avail

    def _probe_kmssink_bus_id(self):
        """Determine whether kmssink needs bus-id=card0.
        RK3588 requires it; RK3399/Armbian rejects it. Use board type to decide, no subprocess."""
        # Known board mappings (avoids hanging gst-launch probe at startup)
        if self.board == "rk3588":
            self.kmssink_bus_id = "card0"
            log.info("kmssink: bus-id=card0 (rk3588 known requirement)")
        else:
            self.kmssink_bus_id = None
            log.info(f"kmssink: no bus-id ({self.board})")

    def _probe_ndisrc_props(self):
        """Probe which optional properties ndisrc supports by inspecting the plugin .so directly."""
        self.ndisrc_has_app_name = False
        plugin_dirs = [
            "/usr/lib/aarch64-linux-gnu/gstreamer-1.0",
            "/lib/aarch64-linux-gnu/gstreamer-1.0",
            "/usr/lib/gstreamer-1.0",
        ]
        so_path = None
        for d in plugin_dirs:
            hits = glob.glob(f"{d}/libgstndi*.so")
            if hits:
                so_path = hits[0]
                break
        if so_path:
            try:
                data = open(so_path, "rb").read()
                self.ndisrc_has_app_name = b"ndi-app-name" in data
                log.info(f"ndisrc probe (so scan): ndi-app-name={'yes' if self.ndisrc_has_app_name else 'no'}")
            except Exception as e:
                log.warning(f"ndisrc so scan failed: {e}")
        else:
            log.warning("ndisrc probe: libgstndi*.so not found, ndi-app-name assumed no")

    def _probe_rga(self):
        """Detect Rockchip RGA hardware scaler/converter via sysfs + GStreamer registry."""
        self.rga_avail = False
        self.rga_dev   = None
        self.rga_el    = None

        # Step 1: find RGA device numbers from sysfs
        rga_nums = set()
        for name_path in sorted(glob.glob("/sys/class/video4linux/video*/name")):
            try:
                dev_name = Path(name_path).read_text().strip().lower()
                if "rga" in dev_name:
                    m = re.search(r'video(\d+)', name_path)
                    if m:
                        rga_nums.add(m.group(1))
            except Exception:
                pass

        if not rga_nums:
            log.info("No RGA hardware scaler detected")
            return

        # Step 2: find v4l2videoXconvert elements that exist in the GStreamer registry.
        # The element number may differ from the /dev/videoN number (depends on driver load order).
        registry_elements = set()
        for reg_path in glob.glob("/root/.cache/gstreamer-1.0/registry.*.bin") +                         glob.glob("/home/*/.cache/gstreamer-1.0/registry.*.bin"):
            try:
                data = Path(reg_path).read_bytes()
                for m in re.finditer(rb'v4l2video(\d+)convert', data):
                    registry_elements.add(m.group(1).decode())
            except Exception:
                pass

        # Step 3: prefer element whose number matches RGA device, else take first registry match
        candidate = None
        for n in rga_nums:
            if n in registry_elements:
                candidate = n
                break
        if candidate is None and registry_elements:
            candidate = sorted(registry_elements)[0]

        if candidate is not None:
            rga_n = sorted(rga_nums)[0]
            self.rga_dev   = f"/dev/video{rga_n}"
            self.rga_el    = f"v4l2video{candidate}convert"
            self.rga_avail = True
            log.info(f"RGA hardware scaler: {self.rga_dev} → element {self.rga_el}")
        else:
            log.info("No RGA hardware scaler detected")

    @staticmethod
    def _parse_gst_caps(line):
        """Extract incoming stream info from a GStreamer -v caps negotiation line."""
        if "video/x-raw" not in line or "caps =" not in line:
            return None
        m_w   = re.search(r'width=\(int\)(\d+)', line)
        m_h   = re.search(r'height=\(int\)(\d+)', line)
        m_fps = re.search(r'framerate=\(fraction\)(\d+)/(\d+)', line)
        m_fmt = re.search(r'format=\(string\)(\w+)', line)
        if not (m_w and m_h):
            return None
        fps = None
        if m_fps:
            num, den = int(m_fps.group(1)), int(m_fps.group(2))
            fps = round(num / den, 3) if den else 0
        return {
            "width":  int(m_w.group(1)),
            "height": int(m_h.group(1)),
            "format": m_fmt.group(1) if m_fmt else "RAW",
            "fps":    fps,
        }

    def _stderr_reader(self, display_name, proc):
        """Read gst-launch stderr, parse caps negotiation, store largest (input) caps."""
        try:
            for raw in proc.stderr:
                line = raw.decode("utf-8", errors="replace").rstrip()
                if not line:
                    continue
                log.debug(f"[gst:{display_name}] {line}")
                caps = self._parse_gst_caps(line)
                if caps:
                    with self.lock:
                        info = self.pipelines.get(display_name)
                        if info:
                            cur = info.get("in_caps")
                            # Keep the caps with largest resolution — that's the source, not scaled output
                            if not cur or caps["width"] > cur.get("width", 0):
                                info["in_caps"] = caps
        except Exception as e:
            log.debug(f"stderr reader for {display_name} exited: {e}")

    def _detect_displays(self):
        """Detect all DRM connectors from sysfs. Each entry includes connected status.
        Connector IDs come from modetest; connection state from /sys/class/drm."""
        self.displays = []
        self.drm_card = "card0"  # determined below

        # Build name→status map from sysfs (reliable, no driver detection needed)
        sysfs_status = {}
        for path in sorted(glob.glob("/sys/class/drm/card*-*")):
            base = os.path.basename(path)
            parts = base.split("-", 1)
            if len(parts) < 2:
                continue
            card, conn_name = parts[0], parts[1]
            if not re.match(r"HDMI|DP|eDP", conn_name):
                continue
            try:
                status = open(f"{path}/status").read().strip()
            except Exception:
                status = "unknown"
            sysfs_status[conn_name] = {"status": status, "card": card}
            if status == "connected":
                self.drm_card = card

        # Get connector IDs from modetest (runs fine as root/service)
        try:
            result = subprocess.run(["modetest", "-c"],
                                    capture_output=True, text=True, timeout=5)
            conn_re = re.compile(r'^(\d+)\t\d+\t\w+\t(HDMI-[A-Z]-\d+|DP-\d+|eDP-\d+)\s')
            for line in result.stdout.splitlines():
                m = conn_re.match(line)
                if m:
                    conn_id   = int(m.group(1))
                    name      = m.group(2)
                    connected = sysfs_status.get(name, {}).get("status") == "connected"
                    if name not in [d["name"] for d in self.displays]:
                        self.displays.append({
                            "id":        conn_id,
                            "name":      name,
                            "connected": connected,
                        })
        except Exception as e:
            log.warning(f"Display detection failed: {e}")

        connected = [d["name"] for d in self.displays if d.get("connected")]
        all_names  = [d["name"] for d in self.displays]
        if not connected:
            log.warning("No connected displays found — check cable connections")
        log.info(f"Detected displays: {all_names} (connected: {connected})")

    def get_supported_modes(self, display_name):
        """Return list of (WxH@fps) from modetest."""
        modes = []
        try:
            result = subprocess.run(["modetest", "-c"], capture_output=True, text=True, timeout=5)
            in_conn = False
            for line in result.stdout.splitlines():
                if display_name in line:
                    in_conn = True
                if in_conn and re.match(r'\s+\d+x\d+', line):
                    m = re.search(r'(\d+x\d+)\s+(\d+\.\d+)', line)
                    if m:
                        modes.append(f"{m.group(1)}@{float(m.group(2)):.2f}")
                if in_conn and line.strip() == "" and modes:
                    break
        except Exception:
            pass
        return modes or ["1920x1080@60.00", "1280x720@60.00"]

    def _build_pipeline(self, source, display, cfg_disp, chroma, scaling, resolution, framerate, osd=False, banner_text=None):
        """Build gst-launch-1.0 command for NDI → HDMI."""
        connector = display["id"]

        # Rotation: 90°/270° swap the content dimensions before scaling so that
        # after videoflip the output fills the display correctly.
        rotation  = int(cfg_disp.get("rotation", 0))
        flip_90   = rotation in (90, 270)
        flip_map  = {90: "clockwise", 180: "rotate-180", 270: "counterclockwise"}
        flip_method = flip_map.get(rotation)

        out_w, out_h = 1920, 1080
        if resolution and resolution != "auto":
            try:
                out_w, out_h = map(int, resolution.split("x"))
            except Exception:
                pass
        # Content is scaled to portrait dimensions for 90/270, then rotated to landscape
        scale_w = out_h if flip_90 else out_w
        scale_h = out_w if flip_90 else out_h

        # NDI source element — include url-address for direct connection (bypasses discovery delay)
        with _ndi_sources_lock:
            url = _ndi_url_cache.get(source, "")
        url_part = f'url-address="{url}" ' if url else ""
        ndi_src = f'ndisrc ndi-name="{source}" {url_part}connect-timeout=30000 '
        cfg = load_config()
        groups = ",".join(cfg["ndi"]["groups"])
        if groups and self.ndisrc_has_app_name:
            ndi_src += f'ndi-app-name="{groups}" '

        # Output format: RK3588 primary plane requires BGRx; everything else uses NV12
        if self.board == "rk3588":
            final_fmt = "video/x-raw,format=BGRx"
        else:
            final_fmt = "video/x-raw,format=NV12"

        par  = "pixel-aspect-ratio=1/1"
        size = f"width={scale_w},height={scale_h}"

        # Scaler selection. RGA (v4l2videoXconvert) only accepts NV12/RGB, not UYVY.
        # For raw UYVY NDI, hardware mode falls back to fast (method=0).
        scaler = cfg_disp.get("scaler", "auto")
        if scaler in ("hardware", "fast"):
            vs = "videoscale method=0 n-threads=6"
            if scaler == "hardware" and not self.rga_avail:
                log.warning("Hardware scaler requested but RGA not available — using fast SW")
        elif scaler == "quality":
            vs = "videoscale method=3 n-threads=4"
        else:
            vs = "videoscale method=1 n-threads=6"

        # aspectratiocrop can fail to link directly to videoscale for packed YUV (UYVY)
        # on some GStreamer builds. Insert videoconvert to normalise format first.
        if scaling == "stretch":
            scale_pipe = f"{vs} ! video/x-raw,{size},{par}"
        elif scaling == "crop":
            scale_pipe = (f"videoconvert ! "
                          f"aspectratiocrop aspect-ratio={scale_w}/{scale_h} ! "
                          f"{vs} ! video/x-raw,{size},{par}")
        else:  # letterbox / fit
            scale_pipe = f"{vs} add-borders=true ! video/x-raw,{size},{par}"

        flip_part = f"videoflip method={flip_method} ! " if flip_method else ""
        video_pipe = f"{scale_pipe} ! videoconvert ! {final_fmt}"

        # OSD / channel banner overlay
        osd_pipe = ""
        if banner_text:
            safe = banner_text.replace('"', "\'")
            osd_pipe = (f'textoverlay text="{safe}" valignment=center halignment=center '
                        f'font-desc="Sans Bold 52" shaded-background=true ! ')
        elif osd:
            cfg_full  = load_config()
            res_str   = f"{out_w}×{out_h}"
            fps_str   = framerate if framerate and framerate != "auto" else "auto"
            osd_text  = f"{res_str}  {fps_str}"
            ov_props  = _overlay_props(cfg_full)
            osd_pipe  = (f'textoverlay text="{osd_text}" valignment=top halignment=right '
                         f'font-desc="Sans Bold 11" {ov_props} ! ')

        bus_id_part = f"bus-id={self.kmssink_bus_id} " if self.kmssink_bus_id else ""
        sink = f"kmssink {bus_id_part}connector-id={connector} sync=false"
        audio_branch = "demux.audio ! queue ! audioconvert ! fakesink sync=false"

        pipeline = (
            f"nice -n -10 gst-launch-1.0 -e -v "
            f"{ndi_src}! queue ! "
            f"ndisrcdemux name=demux "
            f"demux.video ! queue max-size-bytes=0 max-size-buffers=2 max-size-time=0 leaky=downstream ! "
            f"{video_pipe} ! "
            f"{osd_pipe}"
            f"{flip_part}"
            f"{sink} "
            f"{audio_branch}"
        )
        return pipeline


    def _build_splash(self, display, cfg=None):
        """Build splash pipeline: PIL-generated PNG (logo + bg color) + small top-right overlay."""
        connector   = display["id"]
        bus_id_part = f"bus-id={self.kmssink_bus_id} " if self.kmssink_bus_id else ""
        if cfg is None:
            cfg = load_config()
        alias       = cfg["ndi"]["alias"]
        ip          = get_ip()

        # Query preferred resolution from EDID — first mode listed by modetest is preferred.
        # Retry up to 3× with a short delay because DRM mode enumeration can lag behind
        # sysfs hotplug detection by a second or two.
        splash_w, splash_h = 1920, 1080
        for _attempt in range(3):
            try:
                modes = self.get_supported_modes(display["name"])
                if modes:
                    wh = modes[0].split("@")[0]
                    splash_w, splash_h = map(int, wh.split("x"))
                    break
            except Exception:
                pass
            if _attempt < 2:
                time.sleep(1)

        splash_img  = _generate_splash_image(cfg, display_w=splash_w, display_h=splash_h)
        # imagefreeze holds the single decoded PNG frame as a live infinite stream
        src         = f'filesrc location="{splash_img}" ! pngdec ! imagefreeze ! videoconvert'
        fmt_part    = f"video/x-raw,format=BGRx,width={splash_w},height={splash_h}" if self.board == "rk3588" \
                      else f"video/x-raw,width={splash_w},height={splash_h}"
        sink        = f"kmssink {bus_id_part}connector-id={connector} sync=false"

        overlay_pipe = ""
        if cfg["splash"].get("show_overlay", True):
            label        = f"{alias}  {ip}:8080"
            ov_props     = _overlay_props(cfg)
            overlay_pipe = (f'textoverlay text="{label}" valignment=top halignment=right '
                            f'font-desc="Sans Bold 11" {ov_props} ! ')

        pipeline = (
            f"gst-launch-1.0 -e "
            f"{src} ! videoscale ! "
            f"{fmt_part} ! "
            f"{overlay_pipe}"
            f"{sink}"
        )
        return pipeline

    def start_stream(self, display_name, source, cfg_disp=None, show_banner=False):
        cfg = load_config()
        if cfg_disp is None:
            cfg_disp = cfg["displays"].get(display_name, {})
        chroma     = cfg_disp.get("chroma", "NV12")
        scaling    = cfg_disp.get("scaling", "letterbox")
        resolution = cfg_disp.get("resolution", "auto")
        framerate  = cfg_disp.get("framerate", "auto")
        osd        = cfg["video"]["enable_osd"]

        display = next((d for d in self.displays if d["name"] == display_name), None)
        if not display:
            log.error(f"Display not found: {display_name}")
            return False
        if not _sysfs_connected(display_name):
            log.warning(f"Skipping stream to disconnected display {display_name}")
            return False
        display["connected"] = True  # keep cache in sync

        # Validate resolution
        if resolution != "auto":
            modes = self.get_supported_modes(display_name)
            w, h  = resolution.split("x")
            if not any(m.startswith(f"{w}x{h}") for m in modes):
                log.error(f"Resolution {resolution} not supported on {display_name}")
                return False

        # Per-display lock prevents race between auto_recovery_thread and HTTP handlers.
        disp_lock = self._start_locks.setdefault(display_name, threading.Lock())
        if not disp_lock.acquire(blocking=False):
            log.warning(f"start_stream for {display_name} already in progress — skipping duplicate")
            return False
        try:
            return self._start_stream_locked(display_name, source, display, cfg_disp, chroma,
                                             scaling, resolution, framerate, osd, cfg, show_banner)
        finally:
            disp_lock.release()

    def _start_stream_locked(self, display_name, source, display, cfg_disp, chroma,
                              scaling, resolution, framerate, osd, cfg, show_banner=False):
        """Inner start_stream — called only while the per-display start lock is held."""
        # Guard: auto_recovery may pass a stale source that the user cleared while
        # the thread was iterating. Do a fresh config read and abort if source is gone.
        if source:
            _guard = load_config()
            if not _guard["displays"].get(display_name, {}).get("source", ""):
                log.info(f"Source for {display_name} was cleared — not starting pipeline")
                self.show_splash(display_name)
                return False
        # Kill existing stream, splash, and any orphaned NDI gst-launch processes.
        with self.lock:
            p = self.pipelines.pop(display_name, None)
        self._kill_pipeline_proc(p)
        self.stop_splash(display_name)
        subprocess.run(["pkill", "-TERM", "-f", "gst-launch-1.0.*ndisrc"], check=False)
        time.sleep(0.3)  # let killed processes actually exit

        # Build channel info banner text if requested
        banner_duration = cfg["video"].get("banner_duration", 5) if show_banner else 0
        banner_text = None
        if banner_duration > 0:
            res_label = resolution if resolution and resolution != "auto" else "auto"
            banner_text = f"{source}  |  {display_name}  |  {res_label}"

        cmd = self._build_pipeline(source, display, cfg_disp, chroma, scaling, resolution, framerate, osd,
                                   banner_text=banner_text)
        log.info(f"Starting pipeline: {cmd}")
        try:
            proc = subprocess.Popen(cmd, shell=True, preexec_fn=os.setsid,
                                    env=os.environ.copy(), stderr=subprocess.PIPE)
            with self.lock:
                self.pipelines[display_name] = {
                    "proc": proc, "source": source,
                    "start_time": time.time(), "pid": proc.pid, "cmd": cmd,
                    "in_caps": None, "has_banner": banner_text is not None,
                }
            threading.Thread(target=self._stderr_reader, args=(display_name, proc),
                             daemon=True).start()
            # Channel banner dismiss: restart clean pipeline after banner_duration seconds
            if banner_duration > 0:
                def _dismiss_banner(dname=display_name, src=source, dur=banner_duration):
                    time.sleep(dur)
                    with self.lock:
                        info = self.pipelines.get(dname)
                        if not info or info.get("source") != src or not info.get("has_banner"):
                            return  # source changed or already clean
                    log.info(f"Channel banner dismissed on {dname}, restarting clean")
                    self.start_stream(dname, src, show_banner=False)
                threading.Thread(target=_dismiss_banner, daemon=True).start()
            # Clear paused flag using a fresh config load so we never overwrite a
            # concurrent "source=''" written by api_stream_select or api_source_clear.
            _fresh = load_config()
            _fresh["displays"].setdefault(display_name, {}).pop("paused", None)
            save_config(_fresh)
            ok_msg = f"Stream started: {source} → {display_name}"
            log.info(ok_msg)
            ptz_mgr.advertise_receiver(source, display_name)
            return True
        except Exception as e:
            log.error(f"Failed to start stream: {e}")
            return False

    def _kill_pipeline_proc(self, p):
        """Kill a pipeline process dict {proc, ...}."""
        if not p:
            return
        try:
            os.killpg(os.getpgid(p["proc"].pid), signal.SIGTERM)
            p["proc"].wait(timeout=5)
        except Exception:
            try: p["proc"].kill()
            except Exception: pass

    def stop_stream(self, display_name, show_splash_after=True):
        with self.lock:
            p = self.pipelines.pop(display_name, None)
        self._kill_pipeline_proc(p)
        # Update NDI receiver advertisement — no longer connected to any source
        ptz_mgr.advertise_receiver("", display_name)
        if show_splash_after:
            self.show_splash(display_name)

    def show_splash(self, display_name, cfg=None):
        self.stop_splash(display_name)
        if cfg is None:
            cfg = load_config()
        display = next((d for d in self.displays if d["name"] == display_name), None)
        if not display:
            return
        cmd = self._build_splash(display, cfg)
        try:
            proc = subprocess.Popen(cmd, shell=True, preexec_fn=os.setsid, env=os.environ.copy())
            with self.lock:
                self.splash_procs[display_name] = proc
        except Exception as e:
            log.error(f"Splash failed: {e}")

    def stop_splash(self, display_name):
        with self.lock:
            p = self.splash_procs.pop(display_name, None)
        if p:
            try:
                os.killpg(os.getpgid(p.pid), signal.SIGTERM)
                p.wait(timeout=3)
            except Exception:
                try: p.kill()
                except Exception: pass

    def get_status(self):
        status = {}
        with self.lock:
            for name, info in self.pipelines.items():
                proc = info["proc"]
                uptime = int(time.time() - info["start_time"])
                status[name] = {
                    "active":   proc.poll() is None,
                    "source":   info["source"],
                    "uptime":   str(timedelta(seconds=uptime)),
                    "pid":      info["pid"],
                    "in_caps":  info.get("in_caps"),
                }
        for d in self.displays:
            name = d["name"]
            connected = _sysfs_connected(name)
            d["connected"] = connected  # keep cache in sync
            if name not in status:
                status[name] = {"active": False, "source": "", "uptime": "0:00:00"}
            status[name]["connected"] = connected
        return status

    def start_recording(self, display_name):
        cfg      = load_config()
        d_cfg    = cfg["displays"].get(display_name, {})
        rec_path = d_cfg.get("recording_path", "/opt/ndimon-r/recordings")
        source   = d_cfg.get("source", "")
        if not source:
            return False, "No source configured"
        ts      = datetime.now().strftime("%Y%m%d_%H%M%S")
        outfile = f"{rec_path}/{display_name}_{ts}.mp4"
        Path(rec_path).mkdir(parents=True, exist_ok=True)
        cmd = (f'gst-launch-1.0 -e ndisrc ndi-name="{source}" ! '
               f'ndisrcdemux name=d '
               f'd.video ! queue ! videoconvert ! x264enc ! mp4mux name=mux '
               f'd.audio ! queue ! audioconvert ! audioresample ! lamemp3enc ! mux. '
               f'mux. ! filesink location="{outfile}"')
        try:
            proc = subprocess.Popen(cmd, shell=True, preexec_fn=os.setsid, env=os.environ.copy())
            log.info(f"Recording started: {outfile}")
            return True, outfile
        except Exception as e:
            return False, str(e)

# ── PTZ Manager ───────────────────────────────────────────────────────────────
class _NDIRecvCreate(ctypes.Structure):
    _fields_ = [
        ("source_to_connect_to", _NDISource),
        ("color_format",         ctypes.c_int),
        ("bandwidth",            ctypes.c_int),
        ("allow_video_fields",   ctypes.c_bool),
        ("_pad",                 ctypes.c_uint8 * 7),
        ("p_ndi_recv_name",      ctypes.c_char_p),
    ]

class _NDIRecvAdvertiserCreate(ctypes.Structure):
    _fields_ = [("p_url_address", ctypes.c_char_p)]

class PTZManager:
    def __init__(self):
        self._recvs      = {}   # source_name -> recv handle (c_void_p value)
        self._adv_recvs  = {}   # source_name -> recv handle advertised for discovery
        self._advertiser = None # NDIlib_recv_advertiser_instance_t
        self._lock  = threading.Lock()
        self._watch_stop = {}   # disp_name -> threading.Event (signals watch thread to exit)
        try:
            self._lib = ctypes.CDLL("/usr/local/lib/libndi.so")
            # Ensure NDI is initialised (idempotent call)
            self._lib.NDIlib_initialize.restype = ctypes.c_bool
            self._lib.NDIlib_initialize()
            self._available = True
            log.info("PTZManager: libndi.so loaded successfully")
            self._init_advertiser()
        except Exception as e:
            self._lib = None
            self._available = False
            log.warning(f"PTZManager: could not load libndi.so: {e}")

    def _init_advertiser(self):
        """Create an NDI receiver advertiser so this device appears on the discovery server."""
        try:
            cfg = load_config()
            servers = [s.strip() for s in cfg["ndi"].get("discovery_servers", []) if s.strip()]
            url = ",".join(servers).encode() if servers else None
            self._lib.NDIlib_recv_advertiser_create.restype  = ctypes.c_void_p
            self._lib.NDIlib_recv_advertiser_create.argtypes = [ctypes.c_void_p]
            adv_cfg = _NDIRecvAdvertiserCreate(p_url_address=url)
            adv = self._lib.NDIlib_recv_advertiser_create(ctypes.byref(adv_cfg))
            if adv:
                self._advertiser = adv
                log.info(f"PTZManager: recv advertiser created (server={url})")
            else:
                log.warning("PTZManager: recv advertiser returned NULL (no discovery server?)")
        except Exception as e:
            log.warning(f"PTZManager: could not create recv advertiser: {e}")

    def advertise_receiver(self, source_name, disp_name, alias=None):
        """Create/update a metadata-only receiver for disp_name and advertise it.
        Keyed by display name so the receiver identity stays stable across source changes.
        alias overrides the NDI receiver name shown in discovery; falls back to config then disp_name."""
        if not self._lib or not self._advertiser:
            return
        with self._lock:
            if disp_name in self._adv_recvs:
                return  # already advertised for this display
        # Resolve alias: explicit arg > config > display name
        if alias is None:
            try:
                alias = load_config().get("displays", {}).get(disp_name, {}).get("alias", "")
            except Exception:
                alias = ""
        recv_name = (alias.strip() if alias else disp_name)
        try:
            lib = self._lib
            lib.NDIlib_recv_create_v3.restype  = ctypes.c_void_p
            lib.NDIlib_recv_create_v3.argtypes = [ctypes.c_void_p]
            enc_name = source_name.encode("utf-8")
            with _ndi_sources_lock:
                url = _ndi_url_cache.get(source_name, "")
            enc_url = url.encode("utf-8") if url else None
            src = _NDISource(p_ndi_name=enc_name, p_url_address=enc_url)
            cfg_s = _NDIRecvCreate(
                source_to_connect_to=src,
                color_format=0,
                bandwidth=-10,
                allow_video_fields=False,
                p_ndi_recv_name=recv_name.encode("utf-8"),
            )
            recv = lib.NDIlib_recv_create_v3(ctypes.byref(cfg_s))
            if recv:
                lib.NDIlib_recv_advertiser_add_receiver.restype  = ctypes.c_bool
                lib.NDIlib_recv_advertiser_add_receiver.argtypes = [
                    ctypes.c_void_p, ctypes.c_void_p,
                    ctypes.c_bool, ctypes.c_bool, ctypes.c_char_p
                ]
                ok = lib.NDIlib_recv_advertiser_add_receiver(
                    self._advertiser, recv, True, True, None
                )
                with self._lock:
                    self._adv_recvs[disp_name] = recv
                log.info(f"NDI advertiser registered receiver '{disp_name}': ok={ok}")
                self.start_source_watch(disp_name, recv)
        except Exception as e:
            log.warning(f"PTZManager: advertise_receiver error for {disp_name}: {e}")

    def unadvertise_receiver(self, source_name, disp_name):
        """Remove receiver advertisement for disp_name and destroy the receiver."""
        if not self._lib or not self._advertiser:
            return
        with self._lock:
            recv = self._adv_recvs.pop(disp_name, None)
        if not recv:
            return
        try:
            lib = self._lib
            lib.NDIlib_recv_advertiser_del_receiver.restype  = ctypes.c_bool
            lib.NDIlib_recv_advertiser_del_receiver.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
            self.stop_source_watch(disp_name)
            lib.NDIlib_recv_advertiser_del_receiver(self._advertiser, recv)
            lib.NDIlib_recv_destroy.argtypes = [ctypes.c_void_p]
            lib.NDIlib_recv_destroy(recv)
            log.info(f"NDI advertiser unregistered receiver '{disp_name}'")
        except Exception as e:
            log.warning(f"PTZManager: unadvertise_receiver error for {disp_name}: {e}")

    @property
    def available(self):
        return self._available


    def start_source_watch(self, disp_name, recv):
        """Spawn a daemon thread that blocks on NDIlib_recv_get_source_name waiting for a
        management-tool connect command, then switches the display to the assigned source."""
        if not self._lib:
            return
        # Stop any existing watch for this display
        self.stop_source_watch(disp_name)
        stop_evt = threading.Event()
        with self._lock:
            self._watch_stop[disp_name] = stop_evt

        lib = self._lib
        try:
            lib.NDIlib_recv_get_source_name.restype  = ctypes.c_bool
            lib.NDIlib_recv_get_source_name.argtypes = [
                ctypes.c_void_p,
                ctypes.POINTER(ctypes.c_char_p),
                ctypes.c_uint32,
            ]
            lib.NDIlib_recv_free_string.restype  = None
            lib.NDIlib_recv_free_string.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
        except Exception as e:
            log.warning(f"NDI source-watch: could not bind SDK functions: {e}")
            return

        def _watch():
            log.info(f"NDI source-watch started for {disp_name}")
            name_ptr = ctypes.c_char_p(None)
            while not stop_evt.is_set():
                try:
                    changed = lib.NDIlib_recv_get_source_name(
                        ctypes.c_void_p(recv),
                        ctypes.byref(name_ptr),
                        ctypes.c_uint32(2000),   # 2 s block
                    )
                    if changed:
                        if name_ptr.value:
                            source_name = name_ptr.value.decode("utf-8", errors="replace")
                            lib.NDIlib_recv_free_string(ctypes.c_void_p(recv), name_ptr)
                            name_ptr = ctypes.c_char_p(None)
                            log.info(f"NDI remote: assign '{source_name}' -> {disp_name}")
                            cfg = load_config()
                            disp = cfg["displays"].setdefault(disp_name, {})
                            disp["source"] = source_name
                            disp.pop("paused", None)
                            save_config(cfg)
                            _pipeline_backoff.pop(disp_name, None)
                            _pipeline_failures.pop(disp_name, None)
                            pipeline_mgr.start_stream(disp_name, source_name, show_banner=True)
                        else:
                            # Discovery server assigned no source — stop stream
                            log.info(f"NDI remote: clear source -> {disp_name}")
                            cfg = load_config()
                            disp = cfg["displays"].setdefault(disp_name, {})
                            disp["source"] = ""
                            disp["paused"] = True
                            save_config(cfg)
                            pipeline_mgr.stop_stream(disp_name)
                except Exception as exc:
                    log.warning(f"NDI source-watch error on {disp_name}: {exc}")
                    stop_evt.wait(2)
            log.info(f"NDI source-watch stopped for {disp_name}")

        t = threading.Thread(target=_watch, daemon=True, name=f"ndi-watch-{disp_name}")
        t.start()

    def stop_source_watch(self, disp_name):
        """Signal the source-watch thread for disp_name to exit."""
        with self._lock:
            evt = self._watch_stop.pop(disp_name, None)
        if evt:
            evt.set()

    def _get_recv(self, source_name):
        """Return (or create) a metadata-only NDI receiver for the given source."""
        with self._lock:
            if source_name in self._recvs:
                return self._recvs[source_name]
        if not self._lib:
            return None
        try:
            lib = self._lib
            lib.NDIlib_recv_create_v3.restype  = ctypes.c_void_p
            lib.NDIlib_recv_create_v3.argtypes = [ctypes.c_void_p]
            enc_name = source_name.encode("utf-8")
            # Look up URL from cache if available
            with _ndi_sources_lock:
                url = _ndi_url_cache.get(source_name, "")
            enc_url = url.encode("utf-8") if url else None
            src = _NDISource(p_ndi_name=enc_name, p_url_address=enc_url)
            cfg_s = _NDIRecvCreate(
                source_to_connect_to=src,
                color_format=0,
                bandwidth=-10,
                allow_video_fields=False,
                p_ndi_recv_name=b"NDIMonitorPTZ",
            )
            recv = lib.NDIlib_recv_create_v3(ctypes.byref(cfg_s))
            if recv:
                with self._lock:
                    self._recvs[source_name] = recv
                log.info(f"PTZ receiver created for: {source_name}")
                return recv
            log.warning(f"PTZ: NDIlib_recv_create_v3 returned NULL for {source_name}")
        except Exception as e:
            log.warning(f"PTZ: _get_recv error for {source_name}: {e}")
        return None

    def _call(self, fn_name, source_name, *args):
        """Call an NDI PTZ function; returns bool result or False on failure."""
        if not self._lib:
            return False
        recv = self._get_recv(source_name)
        if not recv:
            return False
        try:
            fn = getattr(self._lib, fn_name)
            fn.restype = ctypes.c_bool
            result = fn(ctypes.c_void_p(recv), *args)
            return bool(result)
        except Exception as e:
            log.warning(f"PTZ: {fn_name} failed for {source_name}: {e}")
            return False

    def pan_tilt(self, source, pan, tilt):
        return self._call("NDIlib_recv_ptz_pan_tilt", source,
                          ctypes.c_float(pan), ctypes.c_float(tilt))

    def pan_tilt_speed(self, source, pan_speed, tilt_speed):
        return self._call("NDIlib_recv_ptz_pan_tilt_speed", source,
                          ctypes.c_float(pan_speed), ctypes.c_float(tilt_speed))

    def zoom(self, source, value):
        return self._call("NDIlib_recv_ptz_zoom", source, ctypes.c_float(value))

    def zoom_speed(self, source, speed):
        return self._call("NDIlib_recv_ptz_zoom_speed", source, ctypes.c_float(speed))

    def focus(self, source, value):
        return self._call("NDIlib_recv_ptz_focus", source, ctypes.c_float(value))

    def focus_speed(self, source, speed):
        return self._call("NDIlib_recv_ptz_focus_speed", source, ctypes.c_float(speed))

    def auto_focus(self, source):
        return self._call("NDIlib_recv_ptz_auto_focus", source)

    def store_preset(self, source, idx):
        return self._call("NDIlib_recv_ptz_store_preset", source, ctypes.c_int(idx))

    def recall_preset(self, source, idx, speed=1.0):
        return self._call("NDIlib_recv_ptz_recall_preset", source,
                          ctypes.c_int(idx), ctypes.c_float(speed))

    def is_supported(self, source):
        return self._call("NDIlib_recv_ptz_is_supported", source)

    def expose(self, source, auto=True, level=0.0, iris=0.0, gain=0.0, shutter=0.0):
        if not self._lib:
            return False
        recv = self._get_recv(source)
        if not recv:
            return False
        try:
            if auto:
                fn = self._lib.NDIlib_recv_ptz_exposure_auto
                fn.restype = ctypes.c_bool
                return bool(fn(ctypes.c_void_p(recv)))
            else:
                fn = self._lib.NDIlib_recv_ptz_exposure_manual_v2
                fn.restype = ctypes.c_bool
                return bool(fn(ctypes.c_void_p(recv),
                               ctypes.c_float(iris),
                               ctypes.c_float(gain),
                               ctypes.c_float(shutter)))
        except Exception as e:
            log.warning(f"PTZ expose error for {source}: {e}")
            return False

    def white_balance(self, source, mode, red=0.0, blue=0.0):
        if not self._lib:
            return False
        recv = self._get_recv(source)
        if not recv:
            return False
        try:
            fn_map = {
                "auto":    "NDIlib_recv_ptz_white_balance_auto",
                "indoor":  "NDIlib_recv_ptz_white_balance_indoor",
                "outdoor": "NDIlib_recv_ptz_white_balance_outdoor",
                "oneshot": "NDIlib_recv_ptz_white_balance_oneshot",
            }
            if mode in fn_map:
                fn = getattr(self._lib, fn_map[mode])
                fn.restype = ctypes.c_bool
                return bool(fn(ctypes.c_void_p(recv)))
            else:  # manual
                fn = self._lib.NDIlib_recv_ptz_white_balance_manual
                fn.restype = ctypes.c_bool
                return bool(fn(ctypes.c_void_p(recv),
                               ctypes.c_float(red), ctypes.c_float(blue)))
        except Exception as e:
            log.warning(f"PTZ white_balance error for {source}: {e}")
            return False

# Write NDI SDK config before NDIlib_initialize() is called by PTZManager
_write_ndi_sdk_config(load_config())
ptz_mgr = PTZManager()
pipeline_mgr = PipelineManager()

# ── Auto-Recovery Thread ──────────────────────────────────────────────────────
_pipeline_failures  = {}  # disp_name -> (fail_count, last_start_time)
_pipeline_backoff   = {}  # disp_name -> retry_after (unix timestamp)
_display_connected  = {}  # disp_name -> bool (last known state for hotplug detection)
MAX_FAST_FAILURES   = 3   # clear url cache + backoff after this many crashes within window
FAST_FAIL_WINDOW    = 30  # seconds
BACKOFF_SECS        = 120 # seconds to wait after fast-failure before retrying

def _sysfs_connected(disp_name):
    """Read display connected state from sysfs (reliable, no subprocess)."""
    for path in glob.glob(f"/sys/class/drm/card*-{disp_name}/status"):
        try:
            return open(path).read().strip() == "connected"
        except Exception:
            pass
    return False

def auto_recovery_thread():
    """Every 8s: check active streams, restart dead ones, auto-connect whenever source is set."""
    time.sleep(15)  # startup grace period before first cycle
    while True:
        try:
            # ── Phase 1: Hotplug scan across ALL DRM connectors ──────────────
            # Iterates pipeline_mgr.displays (all connectors, set at startup via modetest)
            # so newly connected displays are caught even if cfg["displays"] is empty.
            for _d in list(pipeline_mgr.displays):
                _name = _d["name"]
                _now  = _sysfs_connected(_name)
                _was  = _display_connected.get(_name, _now)
                _display_connected[_name] = _now
                _d["connected"] = _now
                if _now and not _was:
                    log.info(f"Display {_name} hotplugged — starting splash and advertising receiver")
                    _hcfg = load_config()
                    if _name not in _hcfg["displays"]:
                        _hcfg["displays"][_name] = {
                            "enabled": True, "source": "", "resolution": "auto",
                            "framerate": "auto", "chroma": "NV12", "scaling": "letterbox",
                            "scaler": "auto", "rotation": 0, "backup_source": "",
                            "ndi_lock": False,
                        }
                        save_config(_hcfg)
                    ptz_mgr.advertise_receiver("", _name)
                    pipeline_mgr.show_splash(_name)

            # ── Phase 2: Per-display recovery and auto-connect ────────────────
            cfg = load_config()
            with _ndi_sources_lock:
                available = set(_ndi_sources_cache)
            for disp_name, d_cfg in cfg["displays"].items():
                if not d_cfg.get("enabled", True):
                    continue
                source        = d_cfg.get("source", "")
                backup_source = d_cfg.get("backup_source", "")
                now_connected = _display_connected.get(disp_name, False)
                if not now_connected:
                    continue  # skip — Phase 1 will handle reconnect when it happens

                with pipeline_mgr.lock:
                    pipe_info = pipeline_mgr.pipelines.get(disp_name)
                if pipe_info:
                    proc       = pipe_info["proc"]
                    start_time = pipe_info.get("start_time", time.time())
                    if proc.poll() is not None:
                        uptime = time.time() - start_time
                        # Clean up the dead pipeline entry
                        with pipeline_mgr.lock:
                            pipeline_mgr.pipelines.pop(disp_name, None)
                        # Skip restart if in backoff
                        if time.time() < _pipeline_backoff.get(disp_name, 0):
                            remain = int(_pipeline_backoff[disp_name] - time.time())
                            log.debug(f"Pipeline dead on {disp_name}, backoff {remain}s remaining")
                            continue
                        # Track fast failures
                        fc, last_t = _pipeline_failures.get(disp_name, (0, 0))
                        if time.time() - last_t < FAST_FAIL_WINDOW:
                            fc += 1
                        else:
                            fc = 1
                        _pipeline_failures[disp_name] = (fc, time.time())

                        if fc >= MAX_FAST_FAILURES:
                            # Clear stale URL cache for this source (port may have changed)
                            with _ndi_sources_lock:
                                _ndi_url_cache.pop(source, None)
                            _pipeline_failures.pop(disp_name, None)
                            _pipeline_backoff[disp_name] = time.time() + BACKOFF_SECS
                            log.warning(f"Pipeline on {disp_name} crashed {fc}x in {FAST_FAIL_WINDOW}s "
                                        f"— cleared URL cache, backing off {BACKOFF_SECS}s")
                            pipeline_mgr.show_splash(disp_name)
                        else:
                            log.warning(f"Pipeline died on {disp_name} (exit={proc.poll()}, "
                                        f"uptime={uptime:.1f}s, fail #{fc}), restarting...")
                            pipeline_mgr.start_stream(disp_name, source)
                else:
                    # No active stream — try to connect if source is configured
                    if time.time() < _pipeline_backoff.get(disp_name, 0):
                        continue  # Still in backoff after repeated fast failures
                    with _ndi_sources_lock:
                        src_url = _ndi_url_cache.get(source, "")
                        bkp_url = _ndi_url_cache.get(backup_source, "")
                    # Connect if discoverable OR if we have a cached direct URL (survives
                    # mDNS gaps — GStreamer will use url-address for direct connection).
                    # Splash only when neither source has any known address.
                    active_src = None
                    if source and (source in available or src_url):
                        active_src = source
                    elif backup_source and (backup_source in available or bkp_url):
                        active_src = backup_source
                    if active_src:
                        log.info(f"Auto-connecting {active_src} → {disp_name}")
                        pipeline_mgr.start_stream(disp_name, active_src)
                    else:
                        # Splash watchdog: restart if it died (prevents blank screen)
                        with pipeline_mgr.lock:
                            splash_proc = pipeline_mgr.splash_procs.get(disp_name)
                        if splash_proc is None or splash_proc.poll() is not None:
                            log.info(f"Splash died on {disp_name} — restarting to prevent blank screen")
                            pipeline_mgr.show_splash(disp_name)
        except Exception as e:
            log.error(f"Auto-recovery error: {e}")
        time.sleep(8)

# ── CEC Listener Thread ───────────────────────────────────────────────────────
_cec_status = "disabled"  # "ok", "unavailable", "disabled", or "error: ..."

def cec_listener_thread():
    """Listen for CEC keypresses via cec-client subprocess → cycle NDI sources."""
    global _cec_status
    import shutil
    if not shutil.which("cec-client"):
        _cec_status = "unavailable"
        log.warning("CEC: cec-client not found")
        return
    if not os.path.exists("/dev/cec0"):
        _cec_status = "unavailable"
        log.warning("CEC: /dev/cec0 not found")
        return

    cmd = ["cec-client", "Linux", "-t", "r", "-o", "NDIMonitor", "-d", "8"]
    try:
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                text=True, bufsize=1)
    except Exception as e:
        _cec_status = f"error: {e}"
        log.warning(f"CEC: failed to start cec-client: {e}")
        return

    _cec_status = "ok"
    log.info("CEC: cec-client started on /dev/cec0")

    for line in proc.stdout:
        line = line.strip()
        if "key pressed:" not in line:
            continue
        if "channel up" in line.lower():
            direction = 1
        elif "channel down" in line.lower():
            direction = -1
        else:
            continue
        cfg = load_config()
        with _ndi_sources_lock:
            sources = list(_ndi_sources_cache)
        if not sources:
            continue
        for disp_name, d_cfg in cfg["displays"].items():
            if not d_cfg.get("enabled"):
                continue
            current = d_cfg.get("source", "")
            idx = sources.index(current) if current in sources else -1
            next_src = sources[(idx + direction) % len(sources)]
            pipeline_mgr.start_stream(disp_name, next_src, show_banner=True)

    _cec_status = "error: cec-client exited"
    log.warning("CEC: cec-client process exited")

# ── Routes ────────────────────────────────────────────────────────────────────
@app.route("/")
@require_auth
def index():
    cfg    = load_config()
    status = pipeline_mgr.get_status()
    with _ndi_sources_lock:
        sources = list(_ndi_sources_cache)
    displays = pipeline_mgr.displays
    return render_template("index.html", cfg=cfg, status=status,
                           sources=sources, displays=displays,
                           board=pipeline_mgr.board,
                           ip=get_ip(), uptime=get_system_uptime())

@app.route("/login", methods=["GET", "POST"])
def login():
    if request.method == "POST":
        cfg = load_config()
        pw  = request.form.get("password", "")
        if pw == cfg["auth"].get("password", ""):
            session["authenticated"] = True
            return redirect(url_for("index"))
        flash("Invalid password", "danger")
    return render_template("login.html")

@app.route("/logout")
def logout():
    session.clear()
    return redirect(url_for("login"))

@app.route("/api/sources")
@require_auth
def api_sources():
    with _ndi_sources_lock:
        sources = list(_ndi_sources_cache)
        urls    = dict(_ndi_url_cache)
    return jsonify({"sources": sources, "urls": urls})

@app.route("/api/status")
@require_auth
def api_status():
    status  = pipeline_mgr.get_status()
    cpu     = psutil.cpu_percent(interval=0.5)
    mem     = psutil.virtual_memory()
    temps   = {}
    try:
        for name, entries in psutil.sensors_temperatures().items():
            for e in entries:
                temps[f"{name}/{e.label}"] = e.current
    except Exception:
        pass
    cfg = load_config()
    with _ndi_sources_lock:
        avail_set = set(_ndi_sources_cache)
        url_map   = dict(_ndi_url_cache)
    # Enrich stream status with configured source info and availability
    for disp_name, info in status.items():
        dcfg = cfg["displays"].get(disp_name, {})
        info["output_res"] = dcfg.get("resolution", "auto")
        csrc = dcfg.get("source", "")
        cbkp = dcfg.get("backup_source", "")
        info["configured_source"]  = csrc
        info["configured_backup"]  = cbkp
        info["source_available"]   = csrc in avail_set
        info["backup_available"]   = cbkp in avail_set
        info["source_url"]         = url_map.get(csrc, "")
        info["backup_url"]         = url_map.get(cbkp, "")
    return jsonify({
        "streams": status,
        "system": {
            "cpu": cpu, "mem_used": mem.used, "mem_total": mem.total,
            "mem_pct": mem.percent, "temps": temps,
            "uptime": get_system_uptime(), "ip": get_ip(),
            "board":         pipeline_mgr.board,
            "hw_dec":        pipeline_mgr.hw_dec,
            "hw_available":  pipeline_mgr.mpp_avail,
            "decoder_chain": getattr(pipeline_mgr, "decoder_chain", [pipeline_mgr.hw_dec]),
            "cec":           _cec_status,
            "rga_avail":     pipeline_mgr.rga_avail,
            "rga_dev":       pipeline_mgr.rga_dev,
        }
    })

@app.route("/api/displays")
@require_auth
def api_displays():
    return jsonify({"displays": pipeline_mgr.displays})

@app.route("/api/display/alias", methods=["POST"])
@require_auth
def api_display_alias():
    """Save display alias to config and re-advertise receiver with new NDI name."""
    data = request.get_json(force=True) or {}
    disp_name = data.get("display", "").strip()
    alias     = data.get("alias", "").strip()
    if not disp_name:
        return jsonify({"ok": False, "error": "missing display"})
    cfg = load_config()
    cfg.setdefault("displays", {}).setdefault(disp_name, {})["alias"] = alias
    save_config(cfg)
    # Re-advertise receiver with updated alias
    source = cfg["displays"][disp_name].get("source", "")
    ptz_mgr.unadvertise_receiver(source, disp_name)
    ptz_mgr.advertise_receiver(source, disp_name, alias=alias or None)
    return jsonify({"ok": True})

@app.route("/api/modes/<display_name>")
@require_auth
def api_modes(display_name):
    modes = pipeline_mgr.get_supported_modes(display_name)
    return jsonify({"modes": modes})

@app.route("/api/stream/start", methods=["POST"])
@require_auth
def api_stream_start():
    data    = request.get_json()
    display = data.get("display", "HDMI-A-1")
    source  = data.get("source", "")
    if not source:
        return jsonify({"ok": False, "error": "No source specified"}), 400
    ok_flag = pipeline_mgr.start_stream(display, source)
    return jsonify({"ok": ok_flag})

@app.route("/api/stream/stop", methods=["POST"])
@require_auth
def api_stream_stop():
    data    = request.get_json()
    display = data.get("display", "HDMI-A-1")
    pipeline_mgr.stop_stream(display)
    return jsonify({"ok": True})

@app.route("/api/stream/select", methods=["POST"])
@require_auth
def api_stream_select():
    """Set the NDI source for a display and trigger connect/disconnect immediately."""
    data    = request.get_json()
    display = data.get("display", "HDMI-A-1")
    source  = data.get("source", "")
    # Save source to config (clears paused if present)
    cfg = load_config()
    disp = cfg["displays"].setdefault(display, {})
    disp["source"] = source
    disp.pop("paused", None)
    save_config(cfg)
    # Clear any backoff so auto-recovery picks it up immediately
    _pipeline_backoff.pop(display, None)
    _pipeline_failures.pop(display, None)
    if source:
        ok_flag = pipeline_mgr.start_stream(display, source, show_banner=True)
    else:
        pipeline_mgr.stop_stream(display)
        ok_flag = True
    return jsonify({"ok": ok_flag})

@app.route("/api/source/clear", methods=["POST"])
@require_auth
def api_source_clear():
    """Clear a configured source (primary or backup) from a display and stop the stream."""
    data    = request.get_json(force=True) or {}
    display = data.get("display", "").strip()
    which   = data.get("which", "source")   # "source" or "backup_source"
    if which not in ("source", "backup_source"):
        return jsonify({"ok": False, "error": "which must be 'source' or 'backup_source'"}), 400
    cfg  = load_config()
    disp = cfg["displays"].setdefault(display, {})
    disp[which] = ""
    save_config(cfg)
    if which == "source":
        _pipeline_backoff.pop(display, None)
        _pipeline_failures.pop(display, None)
        pipeline_mgr.stop_stream(display)
    return jsonify({"ok": True})

@app.route("/api/stream/record", methods=["POST"])
@require_auth
def api_stream_record():
    data    = request.get_json()
    display = data.get("display", "HDMI-A-1")
    ok_flag, result = pipeline_mgr.start_recording(display)
    return jsonify({"ok": ok_flag, "file": result if ok_flag else "", "error": "" if ok_flag else result})

@app.route("/api/config", methods=["GET"])
@require_auth
def api_config_get():
    return jsonify(load_config())

@app.route("/api/config", methods=["POST"])
@require_auth
def api_config_set():
    new_cfg = request.get_json()
    old_cfg = load_config()
    save_config(new_cfg)
    _restart_changed_streams(old_cfg, new_cfg)
    return jsonify({"ok": True})

def _restart_changed_streams(old_cfg, new_cfg):
    """Restart active streams whose pipeline-affecting display settings changed."""
    pipeline_keys = {"scaler", "scaling", "resolution", "framerate", "chroma", "osd", "rotation"}
    for disp_name, new_disp in new_cfg.get("displays", {}).items():
        old_disp = old_cfg.get("displays", {}).get(disp_name, {})
        if not any(new_disp.get(k) != old_disp.get(k) for k in pipeline_keys):
            continue
        with pipeline_mgr.lock:
            pipe_info = pipeline_mgr.pipelines.get(disp_name)
        if pipe_info and pipe_info["proc"].poll() is None:
            source = new_disp.get("source", "")
            if source:
                log.info(f"Display settings changed for {disp_name} — restarting stream")
                pipeline_mgr.start_stream(disp_name, source)

@app.route("/api/config/section", methods=["POST"])
@require_auth
def api_config_section():
    data    = request.get_json()
    section = data.get("section")
    values  = data.get("values", {})
    cfg     = load_config()
    if section in cfg:
        cfg[section].update(values)
        save_config(cfg)
        return jsonify({"ok": True})
    return jsonify({"ok": False, "error": "Unknown section"}), 400

@app.route("/api/splash/upload", methods=["POST"])
@require_auth
def api_splash_upload():
    """Upload a PNG logo for the splash screen. Alpha channel preserved; stored raw."""
    if "file" not in request.files:
        return jsonify({"ok": False, "error": "No file"}), 400
    f = request.files["file"]
    if not f.filename.lower().endswith(".png"):
        return jsonify({"ok": False, "error": "Only PNG files are supported (alpha channel required)"}), 400
    UPLOAD_DIR.mkdir(parents=True, exist_ok=True)
    outpath = UPLOAD_DIR / "logo.png"
    try:
        img = Image.open(f).convert("RGBA")  # preserve alpha
        img.save(str(outpath), "PNG")
    except Exception as e:
        return jsonify({"ok": False, "error": str(e)}), 500
    cfg = load_config()
    cfg["splash"]["logo"] = str(outpath)
    save_config(cfg)
    _refresh_idle_splashes(cfg)
    return jsonify({"ok": True, "path": str(outpath)})

@app.route("/api/splash/settings", methods=["POST"])
@require_auth
def api_splash_settings():
    """Save splash bg_color, show_overlay, osd_color, osd_keyed."""
    data = request.get_json()
    cfg  = load_config()
    if "bg_color"     in data: cfg["splash"]["bg_color"]      = data["bg_color"]
    if "show_overlay" in data: cfg["splash"]["show_overlay"]  = bool(data["show_overlay"])
    if "osd_color"    in data: cfg["video"]["osd_color"]      = data["osd_color"]
    if "osd_keyed"    in data: cfg["video"]["osd_keyed"]      = bool(data["osd_keyed"])
    save_config(cfg)
    _refresh_idle_splashes(cfg)
    return jsonify({"ok": True})

def _refresh_idle_splashes(cfg=None):
    """Regenerate and display splash on all idle displays."""
    if cfg is None:
        cfg = load_config()
    for d in pipeline_mgr.displays:
        with pipeline_mgr.lock:
            active = d["name"] in pipeline_mgr.pipelines
        if not active:
            pipeline_mgr.show_splash(d["name"], cfg)

@app.route("/api/network/apply", methods=["POST"])
@require_auth
def api_network_apply():
    data = request.get_json()
    cfg  = load_config()
    cfg["network"].update(data)
    save_config(cfg)
    iface = data.get("interface", "eth0")
    try:
        if data.get("mode") == "static":
            addr = data["address"]; mask = data["netmask"]; gw = data["gateway"]
            dns  = " ".join(data.get("dns", ["8.8.8.8"]))
            script = f"""ip addr flush dev {iface}
ip addr add {addr}/{mask} dev {iface}
ip link set {iface} up
ip route add default via {gw}
echo "nameserver {data['dns'][0]}" > /etc/resolv.conf"""
            subprocess.run(script, shell=True, check=True)
        else:
            subprocess.run(f"dhclient -r {iface}; dhclient {iface}", shell=True)
        return jsonify({"ok": True})
    except Exception as e:
        return jsonify({"ok": False, "error": str(e)}), 500

@app.route("/api/system/reboot", methods=["POST"])
@require_auth
def api_system_reboot():
    threading.Timer(2, lambda: subprocess.run(["reboot"])).start()
    return jsonify({"ok": True, "message": "Rebooting in 2 seconds..."})

@app.route("/api/system/shutdown", methods=["POST"])
@require_auth
def api_system_shutdown():
    threading.Timer(2, lambda: subprocess.run(["shutdown", "-h", "now"])).start()
    return jsonify({"ok": True, "message": "Shutting down in 2 seconds..."})

@app.route("/api/backup/download")
@require_auth
def api_backup_download():
    BACKUP_DIR.mkdir(parents=True, exist_ok=True)
    ts       = datetime.now().strftime("%Y%m%d_%H%M%S")
    zip_name = f"ndimon-r-backup-{ts}.zip"
    zip_path = BACKUP_DIR / zip_name
    try:
        with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED) as zf:
            zf.write(CONFIG_FILE, "config.json")
            for f in UPLOAD_DIR.glob("*"):
                if f.is_file():
                    zf.write(f, f"uploads/{f.name}")
        log.info(f"Backup created: {zip_name}")
        return send_file(str(zip_path), as_attachment=True,
                         download_name=zip_name, mimetype="application/zip")
    except Exception as e:
        log.error(f"Backup failed: {e}")
        return jsonify({"ok": False, "error": str(e)}), 500

@app.route("/api/backup/restore", methods=["POST"])
@require_auth
def api_backup_restore():
    if "file" not in request.files:
        return jsonify({"ok": False, "error": "No file uploaded"}), 400
    f = request.files["file"]
    if not f.filename.lower().endswith(".zip"):
        return jsonify({"ok": False, "error": "Expected a .zip backup file"}), 400
    BACKUP_DIR.mkdir(parents=True, exist_ok=True)
    tmp = BACKUP_DIR / f"restore_tmp_{int(time.time())}.zip"
    try:
        f.save(str(tmp))
        with zipfile.ZipFile(tmp, "r") as zf:
            names = zf.namelist()
            if "config.json" not in names:
                return jsonify({"ok": False, "error": "Invalid backup: config.json not found"}), 400
            raw = zf.read("config.json")
            cfg = json.loads(raw)          # validate JSON before writing anything
            CONFIG_FILE.write_bytes(raw)
            _write_ndi_sdk_config(cfg)
            for name in names:
                if name.startswith("uploads/") and not name.endswith("/"):
                    dest = BASE_DIR / name
                    dest.parent.mkdir(parents=True, exist_ok=True)
                    dest.write_bytes(zf.read(name))
        log.info("Config restored from backup upload")
        return jsonify({"ok": True, "message": "Config restored — page will reload"})
    except json.JSONDecodeError:
        return jsonify({"ok": False, "error": "Invalid backup: config.json contains bad JSON"}), 400
    except Exception as e:
        log.error(f"Restore failed: {e}")
        return jsonify({"ok": False, "error": str(e)}), 500
    finally:
        try: tmp.unlink()
        except Exception: pass

@app.route("/api/ptz/control", methods=["POST"])
@require_auth
def api_ptz_control():
    data   = request.get_json()
    source = data.get("source", "")
    action = data.get("action", "")
    if not source or not action:
        return jsonify({"ok": False, "error": "source and action required"}), 400

    ok = False
    if action == "pan_tilt":
        ok = ptz_mgr.pan_tilt(source, float(data.get("pan", 0)), float(data.get("tilt", 0)))
    elif action == "pan_tilt_speed":
        ok = ptz_mgr.pan_tilt_speed(source, float(data.get("pan_speed", 0)), float(data.get("tilt_speed", 0)))
    elif action == "zoom":
        ok = ptz_mgr.zoom(source, float(data.get("value", 0.5)))
    elif action == "zoom_speed":
        ok = ptz_mgr.zoom_speed(source, float(data.get("speed", 0)))
    elif action == "focus":
        ok = ptz_mgr.focus(source, float(data.get("value", 0.5)))
    elif action == "focus_speed":
        ok = ptz_mgr.focus_speed(source, float(data.get("speed", 0)))
    elif action == "auto_focus":
        ok = ptz_mgr.auto_focus(source)
    elif action == "store_preset":
        idx  = int(data.get("preset", 0))
        name = data.get("name", f"Preset {idx+1}")
        ok   = ptz_mgr.store_preset(source, idx)
        if ok:
            cfg = load_config()
            src_ptz = cfg.setdefault("ptz", {}).setdefault(source, {})
            presets = src_ptz.setdefault("presets", {})
            presets[str(idx)] = name
            save_config(cfg)
    elif action == "recall_preset":
        ok = ptz_mgr.recall_preset(source, int(data.get("preset", 0)), float(data.get("speed", 1.0)))
    elif action == "exposure":
        if data.get("auto"):
            ok = ptz_mgr.expose(source, auto=True)
        else:
            ok = ptz_mgr.expose(source, auto=False,
                                level=float(data.get("level", 0)),
                                iris=float(data.get("iris", 0)),
                                gain=float(data.get("gain", 0)),
                                shutter=float(data.get("shutter", 0)))
    elif action == "white_balance":
        ok = ptz_mgr.white_balance(source, data.get("mode", "auto"),
                                   float(data.get("red", 0)), float(data.get("blue", 0)))
    return jsonify({"ok": ok, "ptz_available": ptz_mgr.available})

@app.route("/api/ptz/presets/<path:source>")
@require_auth
def api_ptz_presets(source):
    cfg     = load_config()
    presets = cfg.get("ptz", {}).get(source, {}).get("presets", {})
    return jsonify({"presets": presets})

@app.route("/ndi")
@require_auth
def ndi_page():
    cfg   = load_config()
    modes = {}
    for d in pipeline_mgr.displays:
        modes[d["name"]] = pipeline_mgr.get_supported_modes(d["name"])
    with _ndi_sources_lock:
        sources     = list(_ndi_sources_cache)
        source_urls = dict(_ndi_url_cache)
    status = pipeline_mgr.get_status()
    return render_template("ndi.html", cfg=cfg, displays=pipeline_mgr.displays,
                           modes=modes, sources=sources, source_urls=source_urls,
                           status=status, ip=get_ip())

@app.route("/settings")
@require_auth
def settings():
    cfg   = load_config()
    return render_template("settings.html", cfg=cfg, displays=pipeline_mgr.displays,
                           ip=get_ip())

# ── Startup ───────────────────────────────────────────────────────────────────
_worker_lock_fd = None  # module-level keeps fd alive so fcntl lock isn't released on return

def startup():
    """Initialise pipeline management. Lock file prevents a second instance."""
    global _worker_lock_fd

    # Kill any gst-launch orphans left by a previous crash before acquiring the lock.
    subprocess.run(["pkill", "-9", "-f", "gst-launch-1.0"], check=False)
    time.sleep(0.5)

    lock_path = BASE_DIR / "worker.lock"
    try:
        _worker_lock_fd = open(lock_path, "w")
        fcntl.flock(_worker_lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        log.error("Another instance is already running — exiting.")
        raise SystemExit(1)

    log.info(f"PID {os.getpid()}: pipeline manager active")
    # Migrate old alias default and write NDI SDK config at startup.
    _migrate_alias()
    _write_ndi_sdk_config(load_config())
    # Wake up displays: set DPMS On via modetest before kmssink takes DRM master.
    # This ensures monitors don't sleep between service restarts.
    try:
        result = subprocess.run(
            ["modetest", "-M", "rockchip", "-c"],
            capture_output=True, text=True, timeout=5
        )
        connector_ids = []
        for line in result.stdout.splitlines():
            parts = line.split()
            if len(parts) >= 3 and parts[2] in ("connected", "disconnected"):
                try:
                    connector_ids.append(int(parts[0]))
                except ValueError:
                    pass
        for cid in connector_ids:
            subprocess.run(
                ["modetest", "-M", "rockchip", "-w", f"{cid}:DPMS:0"],
                capture_output=True, timeout=3
            )
        if connector_ids:
            log.info(f"DPMS On set for connectors: {connector_ids}")
    except Exception as e:
        log.debug(f"DPMS pre-enable skipped: {e}")
    # Set CPU governor to performance — eliminates frequency-scaling latency during 4K decode.
    try:
        gov_path = "/sys/devices/system/cpu/cpu{}/cpufreq/scaling_governor"
        count = int(Path("/sys/devices/system/cpu/present").read_text().strip().split("-")[-1]) + 1
        for i in range(count):
            Path(gov_path.format(i)).write_text("performance")
        log.info(f"CPU governor set to performance on {count} cores")
    except Exception as e:
        log.warning(f"Could not set CPU governor: {e}")
    # Show splash on connected displays only
    for d in pipeline_mgr.displays:
        if d.get("connected"):
            pipeline_mgr.show_splash(d["name"])
    # Pre-advertise receivers for ALL connected displays on the NDI discovery server.
    # This makes them visible immediately at boot, even if no source is configured.
    for d in pipeline_mgr.displays:
        if d.get("connected"):
            ptz_mgr.advertise_receiver("", d["name"])
    # Background threads (only in the lock-holding worker)
    threading.Thread(target=background_ndi_discovery, daemon=True).start()
    threading.Thread(target=auto_recovery_thread,     daemon=True).start()
    _cleanup_old_backups()
    threading.Thread(target=_backup_cleanup_thread,   daemon=True).start()
    cfg = load_config()
    # Pre-seed URL cache from persisted config so direct connections work immediately
    # (before discovery has run) and survive reboots.
    with _ndi_sources_lock:
        _ndi_url_cache.update(cfg.get("ndi_source_urls", {}))
    if cfg["cec"]["enabled"]:
        threading.Thread(target=cec_listener_thread, daemon=True).start()

startup()

if __name__ == "__main__":
    app.run(host="0.0.0.0", port=8080, debug=False, threaded=True)
