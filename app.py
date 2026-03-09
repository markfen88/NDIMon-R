#!/usr/bin/env python3
"""NDI Monitor Appliance - Flask Application"""
import os, sys, json, time, subprocess, threading, socket, re, glob, signal
import logging
from pathlib import Path
from functools import wraps
from datetime import datetime, timedelta

from flask import (Flask, render_template, jsonify, request, redirect,
                   url_for, session, send_from_directory, flash)
from PIL import Image
import psutil

# ── Config ────────────────────────────────────────────────────────────────────
BASE_DIR    = Path("/opt/ndi-monitor")
CONFIG_FILE = BASE_DIR / "config.json"
UPLOAD_DIR  = BASE_DIR / "uploads"
LOG_DIR     = BASE_DIR / "logs"
STATIC_DIR  = BASE_DIR / "static"

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

# ── NDI Source Discovery ──────────────────────────────────────────────────────
_ndi_sources_cache = []   # list of source names
_ndi_url_cache     = {}   # name -> url
_ndi_sources_lock  = threading.Lock()

import ctypes as _ctypes

class _NDIFindCreate(_ctypes.Structure):
    _fields_ = [
        ("show_local_sources", _ctypes.c_bool),
        ("p_groups",           _ctypes.c_char_p),
        ("p_extra_ips",        _ctypes.c_char_p),
    ]

class _NDISource(_ctypes.Structure):
    _fields_ = [
        ("p_ndi_name",    _ctypes.c_char_p),
        ("p_url_address", _ctypes.c_char_p),
    ]

_ndi_lib = None
def _get_ndi_lib():
    global _ndi_lib
    if _ndi_lib is None:
        try:
            lib = _ctypes.CDLL("/usr/local/lib/libndi.so.6")
            lib.NDIlib_initialize.restype = _ctypes.c_bool
            lib.NDIlib_initialize()
            _ndi_lib = lib
        except Exception as e:
            log.warning(f"NDI lib load failed: {e}")
    return _ndi_lib

def _ndi_env():
    """Build environment dict for gst-launch pipelines.
    Auto-injects Wayland vars if a weston socket is present (RK3399 headless)."""
    env = os.environ.copy()
    xdg_rt = "/run/user/0"
    for n in range(5):
        if os.path.exists(f"{xdg_rt}/wayland-{n}"):
            env["XDG_RUNTIME_DIR"] = xdg_rt
            env["WAYLAND_DISPLAY"] = f"wayland-{n}"
            break
    return env

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
            lib.NDIlib_find_create_v2.restype = _ctypes.c_void_p
            settings = _NDIFindCreate(
                show_local_sources=True,
                p_groups=_groups_from_config(),
                p_extra_ips=_extra_ips_from_config(),
            )
            find = lib.NDIlib_find_create_v2(_ctypes.byref(settings))
            if find:
                wait = lib.NDIlib_find_wait_for_sources
                wait.restype = _ctypes.c_bool
                wait.argtypes = [_ctypes.c_void_p, _ctypes.c_uint32]
                wait(find, 5000)

                get = lib.NDIlib_find_get_current_sources
                get.restype = _ctypes.POINTER(_NDISource)
                get.argtypes = [_ctypes.c_void_p, _ctypes.POINTER(_ctypes.c_uint32)]
                count = _ctypes.c_uint32(0)
                ptr = get(find, _ctypes.byref(count))
                for i in range(count.value):
                    name = ptr[i].p_ndi_name
                    url  = ptr[i].p_url_address
                    if name:
                        name = name.decode("utf-8", errors="replace")
                        if name not in sources:
                            sources.append(name)
                        if url:
                            urls[name] = url.decode("utf-8", errors="replace")
                lib.NDIlib_find_destroy.argtypes = [_ctypes.c_void_p]
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
        self.pipelines   = {}   # display_id -> {proc, source, start_time}
        self.splash_procs = {}  # display_id -> proc
        self.lock        = threading.Lock()
        self._detect_board()
        self._detect_displays()
        self._probe_kmssink_bus_id()
        self._probe_ndisrc_props()

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
            self.use_wayland = False  # kmssink works fine on rk3588
        elif "rk3399" in compatible:
            self.board      = "rk3399"
            self.hw_dec, self.mpp_avail = self._probe_mpp_decoder()
            self.use_wayland = True   # RK3399 dumb-buffer alloc fails; use Weston+waylandsink
        elif "bcm2712" in compatible or "Raspberry Pi 5" in open("/proc/cpuinfo").read():
            self.board      = "rpi5"
            self.hw_dec     = "v4l2h264dec"
            self.mpp_avail  = False
            self.use_wayland = False
        else:
            self.board      = "generic"
            self.hw_dec     = "avdec_h264"
            self.mpp_avail  = False
            self.use_wayland = False
        log.info(f"Board: {self.board}, HW decoder: {self.hw_dec}, Wayland: {self.use_wayland}")

    def _probe_mpp_decoder(self):
        """Return (element_name, available) for whichever Rockchip MPP decoder plugin is installed."""
        import glob as _glob
        plugin_dirs = [
            "/usr/lib/aarch64-linux-gnu/gstreamer-1.0",
            "/usr/lib/gstreamer-1.0",
        ]
        for d in plugin_dirs:
            # gstreamer1.0-rockchip1 ships libgstrockchipmpp.so (mppvideodec)
            if _glob.glob(f"{d}/libgstrockchipmpp.so"):
                return "mppvideodec", True
            # Older Radxa packages ship libgstrkmppdec.so (rkmppdec)
            if _glob.glob(f"{d}/libgstrkmppdec.so") or _glob.glob(f"{d}/libgstmpp.so"):
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
        import glob as _glob
        plugin_dirs = [
            "/usr/lib/aarch64-linux-gnu/gstreamer-1.0",
            "/lib/aarch64-linux-gnu/gstreamer-1.0",
            "/usr/lib/gstreamer-1.0",
        ]
        so_path = None
        for d in plugin_dirs:
            hits = _glob.glob(f"{d}/libgstndi*.so")
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

    def _detect_displays(self):
        """Detect all DRM connectors from sysfs. Each entry includes connected status.
        Connector IDs come from modetest; connection state from /sys/class/drm."""
        import glob as _glob
        self.displays = []
        self.drm_card = "card0"  # determined below

        # Build name→status map from sysfs (reliable, no driver detection needed)
        sysfs_status = {}
        for path in sorted(_glob.glob("/sys/class/drm/card*-*")):
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
        log.info(f"Detected displays: {[d['name'] for d in self.displays]}")

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

    def _build_pipeline(self, source, display, cfg_disp, chroma, scaling, resolution, framerate, osd=False):
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

        # Chroma format
        fmt_map = {"NV12": "NV12", "YUY2": "YUY2", "RGB": "BGR"}
        fmt = fmt_map.get(chroma, "NV12")

        # Scaling pipeline element
        if scaling == "stretch":
            scale_pipe = f"videoscale ! video/x-raw,width={scale_w},height={scale_h}"
        elif scaling == "crop":
            scale_pipe = (f"videoscale method=0 add-borders=false ! "
                          f"video/x-raw,width={scale_w},height={scale_h}")
        else:  # letterbox / fit
            scale_pipe = (f"videoscale add-borders=true ! "
                          f"video/x-raw,width={scale_w},height={scale_h}")

        # HW conversion — RK3588 primary plane (Cluster) only supports RGB, not NV12
        if self.board == "rk3588":
            conv = "videoconvert ! video/x-raw,format=BGRx"
        elif self.mpp_avail:
            conv = "videoconvert ! video/x-raw,format=NV12"
        else:
            conv = f"videoconvert ! video/x-raw,format={fmt}"

        # OSD overlay
        osd_pipe = ""
        if osd:
            ip = get_ip()
            osd_text = f"{source} | {ip}:8080"
            osd_pipe = (f'textoverlay text="{osd_text}" valignment=top halignment=center '
                        f'font-desc="Sans Bold 24" ! ')

        # Sink — Wayland (RK3399 headless) or KMS (RK3588/others)
        if self.use_wayland:
            sink = "waylandsink sync=false"
        else:
            bus_id_part = f"bus-id={self.kmssink_bus_id} " if self.kmssink_bus_id else ""
            sink = (f"kmssink {bus_id_part}connector-id={connector} "
                    f"sync=false render-rectangle=\"<0,0,{scale_w},{scale_h}>\"")

        # Audio
        audio_pipe = ""
        if cfg["audio"]["enabled"]:
            audio_pipe = " ! queue ! audioconvert ! audioresample ! autoaudiosink"

        pipeline = (
            f"gst-launch-1.0 -e "
            f"{ndi_src}! queue ! "
            f"ndisrcdemux name=demux "
            f"demux.video ! queue ! videoconvert ! {scale_pipe} ! {conv} ! "
            f"{osd_pipe}"
            f"{sink} "
            f"demux.audio ! queue{audio_pipe}"
        )
        return pipeline

    def _build_splash(self, display, image_path=None):
        """Build splash screen pipeline."""
        connector  = display["id"]
        bus_id_part = f"bus-id={self.kmssink_bus_id} " if self.kmssink_bus_id else ""
        cfg        = load_config()
        alias      = cfg["ndi"]["alias"]
        ip         = get_ip()
        overlay    = f"{alias}  |  {ip}:8080"

        if image_path and Path(image_path).exists():
            src = f'filesrc location="{image_path}" ! decodebin'
        else:
            src = f'videotestsrc pattern=black'

        fmt_part = "video/x-raw,format=BGRx,width=1920,height=1080" if self.board == "rk3588" \
                   else "video/x-raw,width=1920,height=1080"

        if self.use_wayland:
            sink = "waylandsink sync=false"
        else:
            sink = f"kmssink {bus_id_part}connector-id={connector} sync=false"

        pipeline = (
            f"gst-launch-1.0 -e "
            f"{src} ! videoconvert ! videoscale ! "
            f"{fmt_part} ! "
            f'textoverlay text="{overlay}" valignment=bottom halignment=center '
            f'font-desc="Sans Bold 32" shaded-background=true ! '
            f"{sink}"
        )
        return pipeline

    def start_stream(self, display_name, source, cfg_disp=None):
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

        # Kill existing stream and splash without starting a new splash
        with self.lock:
            p = self.pipelines.pop(display_name, None)
        self._kill_pipeline_proc(p)
        self.stop_splash(display_name)

        cmd = self._build_pipeline(source, display, cfg_disp, chroma, scaling, resolution, framerate, osd)
        log.info(f"Starting pipeline: {cmd}")
        try:
            proc = subprocess.Popen(cmd, shell=True, preexec_fn=os.setsid, env=_ndi_env())
            with self.lock:
                self.pipelines[display_name] = {
                    "proc": proc, "source": source,
                    "start_time": time.time(), "pid": proc.pid, "cmd": cmd
                }
            # OSD auto-dismiss
            if osd:
                osd_timeout = cfg["video"].get("osd_timeout", 8)
                def dismiss_osd():
                    time.sleep(osd_timeout)
                    # Would need proper overlay toggle — log for now
                    log.info(f"OSD dismissed after {osd_timeout}s on {display_name}")
                threading.Thread(target=dismiss_osd, daemon=True).start()
            # Save last source, clear paused flag
            cfg["displays"][display_name]["source"] = source
            cfg["displays"][display_name]["paused"] = False
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

    def show_splash(self, display_name):
        self.stop_splash(display_name)
        cfg        = load_config()
        img        = cfg["splash"].get("custom_image", "")
        display    = next((d for d in self.displays if d["name"] == display_name), None)
        if not display:
            return
        cmd = self._build_splash(display, img if img else None)
        try:
            proc = subprocess.Popen(cmd, shell=True, preexec_fn=os.setsid, env=_ndi_env())
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
                    "active": proc.poll() is None,
                    "source": info["source"],
                    "uptime": str(timedelta(seconds=uptime)),
                    "pid": info["pid"]
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
            proc = subprocess.Popen(cmd, shell=True, preexec_fn=os.setsid, env=_ndi_env())
            log.info(f"Recording started: {outfile}")
            return True, outfile
        except Exception as e:
            return False, str(e)

pipeline_mgr = PipelineManager()

# ── Auto-Recovery Thread ──────────────────────────────────────────────────────
_pipeline_failures = {}  # disp_name -> (fail_count, last_start_time)
MAX_FAST_FAILURES   = 3  # pause after this many crashes within FAST_FAIL_WINDOW seconds
FAST_FAIL_WINDOW    = 30 # seconds

def auto_recovery_thread():
    """Every 8s: check active streams, restart dead ones, auto-connect when source appears."""
    while True:
        try:
            cfg = load_config()
            with _ndi_sources_lock:
                available = set(_ndi_sources_cache)
            for disp_name, d_cfg in cfg["displays"].items():
                if not d_cfg.get("enabled", True):
                    continue
                if d_cfg.get("paused", False):
                    continue  # User manually stopped this stream
                source        = d_cfg.get("source", "")
                backup_source = d_cfg.get("backup_source", "")
                with pipeline_mgr.lock:
                    pipe_info = pipeline_mgr.pipelines.get(disp_name)
                if pipe_info:
                    proc      = pipe_info["proc"]
                    start_time = pipe_info.get("start_time", time.time())
                    if proc.poll() is not None:
                        uptime = time.time() - start_time
                        # Track fast failures
                        fc, last_t = _pipeline_failures.get(disp_name, (0, 0))
                        if time.time() - last_t < FAST_FAIL_WINDOW:
                            fc += 1
                        else:
                            fc = 1
                        _pipeline_failures[disp_name] = (fc, time.time())

                        if fc >= MAX_FAST_FAILURES:
                            log.error(f"Pipeline on {disp_name} crashed {fc}x in {FAST_FAIL_WINDOW}s "
                                      f"(last uptime {uptime:.1f}s) — pausing auto-recovery. "
                                      f"Check logs for pipeline errors.")
                            cfg2 = load_config()
                            cfg2["displays"].setdefault(disp_name, {})["paused"] = True
                            save_config(cfg2)
                            _pipeline_failures.pop(disp_name, None)
                        else:
                            log.warning(f"Pipeline died on {disp_name} (exit={proc.poll()}, "
                                        f"uptime={uptime:.1f}s, fail #{fc}), restarting...")
                            pipeline_mgr.start_stream(disp_name, source)
                else:
                    # No active stream — try to connect if source is visible
                    if source and source in available:
                        log.info(f"Auto-connecting {source} → {disp_name}")
                        pipeline_mgr.start_stream(disp_name, source)
                    elif backup_source and backup_source in available:
                        log.info(f"Failover: {backup_source} → {disp_name}")
                        pipeline_mgr.start_stream(disp_name, backup_source)
        except Exception as e:
            log.error(f"Auto-recovery error: {e}")
        time.sleep(8)

# ── CEC Listener Thread ───────────────────────────────────────────────────────
def cec_listener_thread():
    """Listen for CEC Channel Up/Down → cycle NDI sources."""
    try:
        import cec
        cec.init()
        log.info("CEC initialized")
    except Exception as e:
        log.warning(f"CEC not available: {e}")
        return

    def on_keypress(key, duration):
        if duration > 0:
            return
        cfg = load_config()
        with _ndi_sources_lock:
            sources = list(_ndi_sources_cache)
        if not sources:
            return
        for disp_name, d_cfg in cfg["displays"].items():
            if not d_cfg.get("enabled"):
                continue
            current = d_cfg.get("source", "")
            idx = sources.index(current) if current in sources else -1
            if key == cec.CEC_USER_CONTROL_CODE_CHANNEL_UP:
                next_src = sources[(idx + 1) % len(sources)]
            elif key == cec.CEC_USER_CONTROL_CODE_CHANNEL_DOWN:
                next_src = sources[(idx - 1) % len(sources)]
            else:
                return
            pipeline_mgr.start_stream(disp_name, next_src)

    try:
        cec.add_callback(on_keypress, cec.EVENT_KEYPRESS)
        while True:
            time.sleep(1)
    except Exception as e:
        log.error(f"CEC listener error: {e}")

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
    return jsonify({
        "streams": status,
        "system": {
            "cpu": cpu, "mem_used": mem.used, "mem_total": mem.total,
            "mem_pct": mem.percent, "temps": temps,
            "uptime": get_system_uptime(), "ip": get_ip(),
            "board": pipeline_mgr.board
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
    # Mark as manually paused so auto-recovery doesn't restart it
    cfg = load_config()
    cfg["displays"].setdefault(display, {})["paused"] = True
    save_config(cfg)
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
    if "file" not in request.files:
        return jsonify({"ok": False, "error": "No file"}), 400
    f       = request.files["file"]
    outpath = UPLOAD_DIR / "splash.jpg"
    img     = Image.open(f)
    img     = img.convert("RGB")
    img     = img.resize((1920, 1080), Image.LANCZOS)
    img.save(str(outpath), "JPEG", quality=95)
    cfg     = load_config()
    cfg["splash"]["custom_image"] = str(outpath)
    save_config(cfg)
    # Refresh splash on all idle displays
    for d in pipeline_mgr.displays:
        with pipeline_mgr.lock:
            active = d["name"] in pipeline_mgr.pipelines
        if not active:
            pipeline_mgr.show_splash(d["name"])
    return jsonify({"ok": True, "path": str(outpath)})

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

@app.route("/settings")
@require_auth
def settings():
    cfg   = load_config()
    modes = {}
    for d in pipeline_mgr.displays:
        modes[d["name"]] = pipeline_mgr.get_supported_modes(d["name"])
    return render_template("settings.html", cfg=cfg, displays=pipeline_mgr.displays,
                           modes=modes, ip=get_ip())

def get_system_uptime():
    try:
        with open("/proc/uptime") as f:
            secs = float(f.read().split()[0])
        return str(timedelta(seconds=int(secs)))
    except Exception:
        return "N/A"

# ── Startup ───────────────────────────────────────────────────────────────────
_worker_lock_fd = None  # module-level keeps fd alive so fcntl lock isn't released on return

def startup():
    """Run pipeline management only in the first gunicorn worker (lock file guard)."""
    global _worker_lock_fd
    import fcntl
    lock_path = BASE_DIR / "worker.lock"
    try:
        _worker_lock_fd = open(lock_path, "w")
        fcntl.flock(_worker_lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        log.info(f"Worker {os.getpid()}: standby (pipeline manager already running)")
        # Still run NDI discovery so all workers can serve the /api/sources endpoint
        threading.Thread(target=background_ndi_discovery, daemon=True).start()
        return

    log.info(f"Worker {os.getpid()}: pipeline manager active")
    # Show splash on connected displays only
    for d in pipeline_mgr.displays:
        if d.get("connected"):
            pipeline_mgr.show_splash(d["name"])
    # Background threads (only in the lock-holding worker)
    threading.Thread(target=background_ndi_discovery, daemon=True).start()
    threading.Thread(target=auto_recovery_thread,     daemon=True).start()
    cfg = load_config()
    if cfg["cec"]["enabled"]:
        threading.Thread(target=cec_listener_thread, daemon=True).start()

startup()

if __name__ == "__main__":
    app.run(host="0.0.0.0", port=8080, debug=False)
