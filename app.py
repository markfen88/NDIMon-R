#!/usr/bin/env python3
"""NDI Monitor Appliance - Flask Application"""
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
BASE_DIR    = Path("/opt/ndi-monitor")
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
    """Write ~/.ndi/ndi-config.v1.json so the NDI SDK library uses our discovery server."""
    try:
        servers = [s.strip() for s in cfg["ndi"].get("discovery_servers", []) if s.strip()]
        groups  = cfg["ndi"].get("groups", ["Public"])
        group_str = ",".join(g.strip() for g in groups if g.strip()) or "Public"
        ndi_cfg = {
            "networks": {"discovery": ",".join(servers)} if servers else {},
            "groups":   {"send": group_str, "recv": group_str},
        }
        for home in ["/root", "/home/radxa"]:
            ndi_dir = Path(home) / ".ndi"
            try:
                ndi_dir.mkdir(parents=True, exist_ok=True)
                with open(ndi_dir / "ndi-config.v1.json", "w") as f:
                    json.dump(ndi_cfg, f, indent=2)
            except Exception:
                pass
    except Exception as e:
        log.warning(f"Could not write NDI SDK config: {e}")

def get_ip():
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(("8.8.8.8", 80))
        ip = s.getsockname()[0]; s.close(); return ip
    except Exception:
        return "0.0.0.0"

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
            self.board      = "rk3588"
            self.hw_dec, self.mpp_avail = self._probe_mpp_decoder()
        elif "rk3399" in compatible:
            self.board      = "rk3399"
            self.hw_dec, self.mpp_avail = self._probe_mpp_decoder()
        elif "bcm2712" in compatible or "Raspberry Pi 5" in Path("/proc/cpuinfo").read_text(errors="ignore"):
            self.board      = "rpi5"
            self.hw_dec     = "v4l2h264dec"
            self.mpp_avail  = False
        else:
            self.board      = "generic"
            self.hw_dec     = "avdec_h264"
            self.mpp_avail  = False
        log.info(f"Board: {self.board}, HW decoder: {self.hw_dec}")

    def _probe_mpp_decoder(self):
        """Return (element_name, available) for whichever Rockchip MPP decoder plugin is installed."""
        plugin_dirs = [
            "/usr/lib/aarch64-linux-gnu/gstreamer-1.0",
            "/usr/lib/gstreamer-1.0",
        ]
        for d in plugin_dirs:
            # gstreamer1.0-rockchip1 ships libgstrockchipmpp.so (mppvideodec)
            if glob.glob(f"{d}/libgstrockchipmpp.so"):
                return "mppvideodec", True
            # Older Radxa packages ship libgstrkmppdec.so (rkmppdec)
            if glob.glob(f"{d}/libgstrkmppdec.so") or glob.glob(f"{d}/libgstmpp.so"):
                return "rkmppdec", True
        return "avdec_h264", False

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
        """Detect Rockchip RGA hardware scaler/converter via sysfs V4L2 device names."""
        self.rga_avail = False
        self.rga_dev   = None
        self.rga_el    = None
        for name_path in sorted(glob.glob("/sys/class/video4linux/video*/name")):
            try:
                dev_name = Path(name_path).read_text().strip().lower()
                if "rga" in dev_name:
                    m = re.search(r'video(\d+)', name_path)
                    if m:
                        n = m.group(1)
                        self.rga_dev   = f"/dev/video{n}"
                        self.rga_el    = f"v4l2video{n}convert"
                        self.rga_avail = True
                        log.info(f"RGA hardware scaler: {self.rga_dev} ({dev_name})")
                        return
            except Exception:
                pass
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
        scale_w, scale_h = 1920, 1080
        if resolution and resolution != "auto":
            try:
                scale_w, scale_h = map(int, resolution.split("x"))
            except Exception:
                pass

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

        # IMPORTANT: force pixel-aspect-ratio=1/1 in all caps so GStreamer adds real border
        # pixels instead of encoding AR correction via PAR metadata (makes modes look identical).
        par = "pixel-aspect-ratio=1/1"
        size = f"width={scale_w},height={scale_h}"

        # Scaler quality/speed setting.
        # Scaler selection.
        # Hardware (RGA): UYVY→NV12 SW conversion first (at source res), then RGA scales
        # NV12→NV12 in hardware (cheap), then final colorspace convert at output res.
        # On RK3588, RGA3 supports NV12 input — much more capable than RK3399 RGA2.
        # Software paths: n-threads=6 uses all cores. Skip pre-scale videoconvert since
        # videoscale handles UYVY natively, saving ~12 MB/frame format conversion.
        scaler = cfg_disp.get("scaler", "auto")
        if scaler == "hardware" and self.rga_avail:
            # Hardware RGA path: SW convert UYVY→NV12, RGA scales, SW convert to final fmt
            rga = self.rga_el
            if scaling == "stretch":
                scale_pipe = (
                    f"videoconvert ! video/x-raw,format=NV12 ! "
                    f"{rga} ! video/x-raw,{size},format=NV12,{par}"
                )
            elif scaling == "crop":
                scale_pipe = (
                    f"aspectratiocrop aspect-ratio={scale_w}/{scale_h} ! "
                    f"videoconvert ! video/x-raw,format=NV12 ! "
                    f"{rga} ! video/x-raw,{size},format=NV12,{par}"
                )
            else:  # letterbox / fit
                scale_pipe = (
                    f"videoconvert ! video/x-raw,format=NV12 ! "
                    f"{rga} add-borders=true ! video/x-raw,{size},format=NV12,{par}"
                )
            video_pipe = f"{scale_pipe} ! videoconvert ! {final_fmt}"
            log.info(f"Using hardware RGA scaler ({self.rga_el})")
        else:
            if scaler == "hardware":
                log.warning("Hardware scaler requested but RGA not available — falling back to software")
            if scaler == "fast":
                vs = "videoscale method=0 n-threads=6"
            elif scaler == "quality":
                vs = "videoscale method=3 n-threads=4"
            else:  # auto / balanced / hardware fallback
                vs = "videoscale method=1 n-threads=6"

            if scaling == "stretch":
                scale_pipe = f"{vs} ! video/x-raw,{size},{par}"
            elif scaling == "crop":
                scale_pipe = (f"aspectratiocrop aspect-ratio={scale_w}/{scale_h} ! "
                              f"{vs} ! video/x-raw,{size},{par}")
            else:  # letterbox / fit
                scale_pipe = f"{vs} add-borders=true ! video/x-raw,{size},{par}"

            video_pipe = f"{scale_pipe} ! videoconvert ! {final_fmt}"

        # OSD / channel banner overlay
        osd_pipe = ""
        if banner_text:
            # Channel info banner: shown briefly on source switch, dismissed by restart
            safe = banner_text.replace('"', "'")
            osd_pipe = (f'textoverlay text="{safe}" valignment=center halignment=center '
                        f'font-desc="Sans Bold 52" shaded-background=true ! ')
        elif osd:
            ip       = get_ip()
            cfg_full = load_config()
            osd_text = f"{source}  |  {ip}:8080"
            ov_props = _overlay_props(cfg_full)
            osd_pipe = (f'textoverlay text="{osd_text}" valignment=top halignment=right '
                        f'font-desc="Sans Bold 11" {ov_props} ! ')

        bus_id_part = f"bus-id={self.kmssink_bus_id} " if self.kmssink_bus_id else ""
        sink = f"kmssink {bus_id_part}connector-id={connector} sync=false"

        # Audio branch: hardware audio sinks conflict with kmssink on RK3399.
        # Use fakesink with audioconvert to properly drain the audio pad.
        audio_branch = "demux.audio ! queue ! audioconvert ! fakesink sync=false"

        # Video queue: unlimited bytes (single 4K UYVY frame is ~12 MB, exceeds 10 MB default
        # causing deadlock). leaky=downstream drops oldest queued frame when full so the
        # pipeline never backs up — always shows the most recent live frame.
        pipeline = (
            f"nice -n -10 gst-launch-1.0 -e -v "
            f"{ndi_src}! queue ! "
            f"ndisrcdemux name=demux "
            f"demux.video ! queue max-size-bytes=0 max-size-buffers=2 max-size-time=0 leaky=downstream ! "
            f"{video_pipe} ! "
            f"{osd_pipe}"
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

        splash_img  = _generate_splash_image(cfg)
        # imagefreeze holds the single decoded PNG frame as a live infinite stream
        src         = f'filesrc location="{splash_img}" ! pngdec ! imagefreeze ! videoconvert'
        fmt_part    = "video/x-raw,format=BGRx,width=1920,height=1080" if self.board == "rk3588" \
                      else "video/x-raw,width=1920,height=1080"
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
        if not display.get("connected", True):
            log.warning(f"Skipping stream to disconnected display {display_name}")
            return False

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
            # Save last source
            cfg["displays"][display_name]["source"] = source
            cfg["displays"][display_name].pop("paused", None)
            save_config(cfg)
            ok_msg = f"Stream started: {source} → {display_name}"
            log.info(ok_msg)
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
            if d["name"] not in status:
                status[d["name"]] = {"active": False, "source": "", "uptime": "0:00:00"}
        return status

    def start_recording(self, display_name):
        cfg      = load_config()
        d_cfg    = cfg["displays"].get(display_name, {})
        rec_path = d_cfg.get("recording_path", "/opt/ndi-monitor/recordings")
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

class PTZManager:
    def __init__(self):
        self._recvs = {}   # source_name -> recv handle (c_void_p value)
        self._lock  = threading.Lock()
        try:
            self._lib = ctypes.CDLL("/usr/local/lib/libndi.so")
            # Ensure NDI is initialised (idempotent call)
            self._lib.NDIlib_initialize.restype = ctypes.c_bool
            self._lib.NDIlib_initialize()
            self._available = True
            log.info("PTZManager: libndi.so loaded successfully")
        except Exception as e:
            self._lib = None
            self._available = False
            log.warning(f"PTZManager: could not load libndi.so: {e}")

    @property
    def available(self):
        return self._available

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
            cfg = load_config()
            with _ndi_sources_lock:
                available = set(_ndi_sources_cache)
            for disp_name, d_cfg in cfg["displays"].items():
                if not d_cfg.get("enabled", True):
                    continue
                source        = d_cfg.get("source", "")
                backup_source = d_cfg.get("backup_source", "")
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
                    active_src = None
                    if source:
                        active_src = source
                    elif backup_source and backup_source in available:
                        active_src = backup_source
                    if active_src:
                        log.info(f"Auto-connecting {active_src} → {disp_name}")
                        pipeline_mgr.start_stream(disp_name, active_src)
                    else:
                        # Hotplug: refresh splash if display just reconnected
                        now_connected = _sysfs_connected(disp_name)
                        was_connected = _display_connected.get(disp_name, True)
                        if now_connected and not was_connected:
                            log.info(f"Display {disp_name} reconnected — refreshing splash")
                            pipeline_mgr.show_splash(disp_name)
                        _display_connected[disp_name] = now_connected
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
    return jsonify({"sources": sources})

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
    # Enrich stream status with configured output resolution
    for disp_name, info in status.items():
        dcfg = cfg["displays"].get(disp_name, {})
        info["output_res"] = dcfg.get("resolution", "auto")
    return jsonify({
        "streams": status,
        "system": {
            "cpu": cpu, "mem_used": mem.used, "mem_total": mem.total,
            "mem_pct": mem.percent, "temps": temps,
            "uptime": get_system_uptime(), "ip": get_ip(),
            "board": pipeline_mgr.board,
            "cec": _cec_status,
            "rga_avail": pipeline_mgr.rga_avail,
            "rga_dev":   pipeline_mgr.rga_dev,
        }
    })

@app.route("/api/displays")
@require_auth
def api_displays():
    return jsonify({"displays": pipeline_mgr.displays})

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
    save_config(new_cfg)
    return jsonify({"ok": True})

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
    zip_name = f"ndi-monitor-backup-{ts}.zip"
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
        sources = list(_ndi_sources_cache)
    status = pipeline_mgr.get_status()
    return render_template("ndi.html", cfg=cfg, displays=pipeline_mgr.displays,
                           modes=modes, sources=sources, status=status, ip=get_ip())

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
    # Background threads (only in the lock-holding worker)
    threading.Thread(target=background_ndi_discovery, daemon=True).start()
    threading.Thread(target=auto_recovery_thread,     daemon=True).start()
    _cleanup_old_backups()
    threading.Thread(target=_backup_cleanup_thread,   daemon=True).start()
    cfg = load_config()
    if cfg["cec"]["enabled"]:
        threading.Thread(target=cec_listener_thread, daemon=True).start()

startup()

if __name__ == "__main__":
    app.run(host="0.0.0.0", port=8080, debug=False, threaded=True)
