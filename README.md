# NDIMon-R

Dedicated **NDI receiver / HDMI decoder** for Linux appliances.

It takes a live NDI stream off the network, decodes it, and scans it out to HDMI or DisplayPort with DRM/KMS. Audio goes to ALSA. A small web UI on port 80 is the day-to-day control surface.

This is a **decoder**, not an encoder. It does not send NDI.

[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![Platform](https://img.shields.io/badge/arch-aarch64%20%7C%20x86--64-blue)](https://github.com/markfen88/NDIMon-R)

---

## Contents

- [What it does](#what-it-does)
- [Requirements](#requirements)
- [Install](#install)
- [First boot](#first-boot)
- [Features](#features)
- [Hardware and codecs](#hardware-and-codecs)
- [Services](#services)
- [Configuration](#configuration)
- [Web UI and API](#web-ui-and-api)
- [Update](#update)
- [Troubleshooting](#troubleshooting)
- [Architecture](#architecture)
- [License](#license)

---

## What it does

NDIMon-R is meant to sit on an HDMI input, remember the last source, and come back by itself after a reboot or a dropped sender.

Two receive paths:

| Incoming stream | What happens |
|-----------------|--------------|
| **Standard NDI** (SpeedHQ / UYVY / NV12) | NDI SDK decodes SpeedHQ. Framesync pulls video/audio to the local HDMI/ALSA clock. Colour convert uses ARM NEON, or Rockchip VOP2 can scan UYVY natively. |
| **NDI HX** (H.264 / H.265) | SDK is asked for a compressed bitstream (passthrough). NDIMon-R decodes it: Rockchip MPP, Pi 4 V4L2, Intel/AMD VAAPI, or FFmpeg. Linux NDI SDK has **no** GPU decode of its own. |

If a hardware decoder fails to initialise, the worker falls back to FFmpeg so the output is not a black frame.

---

## Requirements

- **OS:** Debian Bookworm/Trixie, Ubuntu 24.04 Noble, Armbian, or Raspberry Pi OS (64-bit).
- **CPU:** aarch64 or x86-64.
- **Display:** HDMI or DisplayPort. Do not run a desktop session that holds DRM master (no gnome/weston on that connector).
- **Network:** Avahi/`avahi-daemon` for mDNS discovery. Optional NDI Discovery Server for other subnets.
- **Privileges:** full appliance install is **root** (`sudo`). The decoder stays root (DRM); the API and finder run as user `ndimon`.
- **Build:** happens **on the target**. Do not cross-compile from Windows/macOS and expect it to work.

By installing you accept the [NDI SDK License Agreement](https://www.ndi.tv/license). The SDK tarball is downloaded from NDI (~60 MB) on first `setup-deps`.

---

## Install

On the device:

```bash
sudo apt-get update
sudo apt-get install -y git
git clone https://github.com/markfen88/NDIMon-R.git
cd NDIMon-R
sudo bash install.sh
```

That is the whole path: dependencies, NDI SDK v6, CMake build, binaries, systemd units, Node API.

When it finishes, open `http://<device-ip>/`.

### Installer flags

| Command | When to use |
|---------|-------------|
| `sudo bash install.sh` | First install, or after a distro change |
| `sudo bash install.sh --no-deps` | Code update: rebuild and reinstall, keep packages |
| `sudo bash install.sh --no-build` | Units/API only; binaries already in `build/` |

Top-level `install.sh` always installs **system** units (`/etc/systemd/system`) via `sudo`. Config files already in `/etc/` are never overwritten.

### What each step does

1. **`scripts/setup-deps.sh`** (root) — build tools, libdrm, ALSA, Avahi, FFmpeg headers, Node.js 20, NDI SDK into `/usr/local`. Rockchip MPP on RK boards. VAAPI drivers on x86 (Intel iHD/i965 + Mesa for AMD).
2. **`scripts/build.sh`** — `cmake -B build` + compile `ndimon-r` and `ndimon-finder`.
3. **`scripts/install.sh`** (root) — `/usr/local/bin`, `/opt/ndimon-r/api`, user `ndimon`, `/usr/local/sbin/ndimon-priv`, sudoers, `/var/lib/ndimon/.ndi` (shared NDI config home), enable and start the four services.

Optional checksum pin for the NDI tarball: `NDI_SDK_SHA256=<hex> sudo bash install.sh`.

### After install — logs

```bash
sudo systemctl status ndimon-r ndimon-finder ndimon-api ndimon-watchdog
sudo journalctl -u ndimon-r -f
```

Or: `sudo bash scripts/status.sh`

---

## First boot

1. Browse to `http://<device-ip>/`.
2. Log in. Default password is **`ndimon`**. Change it under **Settings → Security** before the box is on a real network.
3. Open **NDI**, pick a source. That choice is written to `/etc/ndimon-dec{N}-settings.json` and survives reboot.
4. Optional, also on **NDI**: Discovery Server IP, NTP host, groups, extra IPs, transport, and HX decode mode.

Factory watchdog mode is **active** (reconnect after a ~30 s stall). Existing `/etc/ndimon-device-settings.json` is not rewritten on update — change it under **System → Watchdog Mode** if an older box is still `passive`.

---

## Features

### Receive and display

- Dual-mode NDI: Standard (Framesync) and HX (passthrough → local decoder).
- Independent source per connector (HDMI-A-1, HDMI-A-2, DP-1, then further connectors in a stable map).
- Scale: letterbox, stretch, crop. Rotation 0/90/180/270.
- HDMI mode auto-match to the source, or a locked resolution from the UI.
- ALSA audio from NDI planar float, downmixed to stereo.
- Custom splash + OSD when idle or disconnected.
- Monitor hotplug: receiver still advertises if no display at boot; picture starts when HDMI lands.

### Appliance behaviour

- Last source auto-connects after reboot.
- Disconnect keeps the saved source (reconnect). Forget source is an explicit UI action.
- Node.js reconnect with exponential backoff (5 s → 30 s).
- Watchdog polls `/api/health`; **active** mode disconnects a stalled output so the API reconnects it.
- systemd `Restart=always`; `ndimon-r` is `Type=notify` with `WatchdogSec=30`.
- Optional NTP host: writes timesyncd or chrony drop-ins, enables the daemon, reapplies on API start. NDI has no time server of its own.
- Optional scheduled reboot (System page): local time + weekdays, applied as a systemd timer. Missed windows do not fire on the next boot.

### Discovery and control

- mDNS via Avahi, plus optional NDI Discovery Server (**NDI → NDI Discovery**; blank IP = off).
- Receiver advertiser so the box shows up as a destination on the DS (`allow_controlling` — the SDK switches sources; we do not `connect()` from a routing callback).
- NDI groups (case-sensitive). Off-subnet extra IPs.
- Transport: TCP on a new install (the UI default). RUDP, UDP, Multicast, and M-TCP are selectable; a change rewrites `ndi-config.v1.json` and recreates the recv. RUDP is the NDI SDK’s own default if you pick it. Multicast also requires the sender to be multicasting.
- Named source presets (instant recall, no rescan).

### Security (LAN appliance)

- Session cookie (HttpOnly, SameSite=Lax) or `Authorization: Bearer`.
- Same-origin only; mutating requests checked against `Origin` / `Host`.
- GET is not used to change state (`connectTo`, reboot, finder reset, … return 405).
- API and finder run as `ndimon`. Reboot, hostname, NTP, and service restarts go through `/usr/local/sbin/ndimon-priv`.
- IPC socket `/tmp/ndi-decoder.sock` is `0660` `root:ndimon`.

---

## Hardware and codecs

| Board | HX H.264 | HX H.265 | Standard NDI |
|-------|----------|----------|----------------|
| Radxa Rock 5B (RK3588) | MPP | MPP | SDK + NEON or VOP2 UYVY |
| Radxa Rock 4C+ / 4B+ (RK3399) | MPP | MPP | SDK + NEON or VOP2 UYVY |
| Raspberry Pi 4 | V4L2 M2M (`/dev/video10`) | FFmpeg (Pi HEVC is stateless; not used) | SDK + NEON |
| Raspberry Pi 5 | FFmpeg (no H.264 HW) | FFmpeg (same) | SDK + NEON |
| Other aarch64 | FFmpeg | FFmpeg | SDK + NEON |
| Intel NUC (iHD / i965) | VAAPI | VAAPI | SDK + scalar/SSE |
| AMD (Mesa VAAPI) | VAAPI | VAAPI | SDK + scalar/SSE |
| NVIDIA Linux | FFmpeg (no NVDEC yet) | FFmpeg | SDK + scalar/SSE |

**NDI → Decoder → HX Decode:** `auto` (prefer HW, then FFmpeg), `hardware` (tries HW, falls back to FFmpeg if init fails), `software` (FFmpeg only). The NDI page shows a `HW`/`SW` badge and **SATURATED** if HX decode FPS stays below 85% of source FPS.

x86 System page also shows VAAPI driver + decode profiles from install-time `vainfo`.

---

## Services

| Unit | Role | User |
|------|------|------|
| `ndimon-r` | Capture, decode, DRM, ALSA, IPC | root |
| `ndimon-finder` | NDI Find → `/etc/ndimon-sources.json` | `ndimon` |
| `ndimon-api` | Web UI + REST, port 80 | `ndimon` |
| `ndimon-watchdog` | Process liveness + `/api/health` | root |

Finder and decoder share **`/var/lib/ndimon/.ndi/ndi-config.v1.json`**. That is the official NDI config location (`$HOME/.ndi`); both processes pin `HOME` there so groups, DS, transport, and HX passthrough stay in one file.

```bash
sudo systemctl restart ndimon-r ndimon-finder ndimon-api ndimon-watchdog
sudo journalctl -u ndimon-r -u ndimon-api -f
```

---

## Configuration

Templates in `config/` are copied to `/etc/` **only if the file is missing**.

| File | Purpose |
|------|---------|
| `ndimon-dec{N}-settings.json` | Per-output source, audio, tally, colour, alias (`N` = 1…8) |
| `ndimon-device-settings.json` | Device alias, `watchdog_mode`, `ntp_server`, `decode_mode`, scheduled reboot |
| `ndimon-find-settings.json` | Discovery Server IP (non-empty ⇒ enabled) |
| `ndimon-rx-settings.json` | Transport `Rxpm` |
| `ndi-group.json` | Groups (comma-separated, case-sensitive) |
| `ndi-config.json` | Extra finder IPs (comma-separated) |
| `ndimon-presets.json` | Named presets (API) |
| `ndimon-auth.json` | Password hash (created when you change the password) |
| `ndimon-splash-settings.json` / `ndimon-osd-settings.json` | Splash / OSD (API) |
| `ndimon-sources.json` | Finder cache |

Prefer the web UI — it writes JSON and tells the decoder to reload. If you edit `/etc/` by hand:

```bash
sudo systemctl restart ndimon-r ndimon-finder ndimon-api
```

Discovery Server and NTP live on **NDI → NDI Discovery**. Leave the DS IP blank to disable it. NTP sets the **OS clock** (timesyncd or chrony), not an NDI clock.

---

## Web UI and API

UI: `http://<device-ip>/`

All `/v1/*` and `/api/*` routes need a session except:

- `POST /api/login`, `GET /api/auth-status`
- `GET /api/health` **from loopback only** (watchdog)

Login: `POST /api/login` with `{ "password": "…" }` — cookie plus `token` for `Authorization: Bearer`.

### Useful routes

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/status` | GET | Decoder + system status |
| `/api/events` | GET | SSE (~1.5 s) |
| `/api/health` | GET | Watchdog payload (`stalled` / `degraded`) |
| `/v1/NDIFinder/List` | GET | Cached sources |
| `/v1/NDIFinder/refresh` | POST | Restart finder |
| `/v1/NDIDecode/connectTo` | POST | `{ SourceName, SourceIP, Output }` — `Output` is **1–8** |
| `/v1/NDIDecode/decodeTransport` | GET, POST | `{ Rxpm }` (`TCP`, `UDP`, `Multicast`, `M-TCP`, `RUDP`) |
| `/v1/Presets/list` | GET | Presets |
| `/v1/Presets/save` | POST | `{ name, source, ip }` |
| `/v1/Presets/recall` | POST | `{ name, output }` — `output` is **1–8** |
| `/v1/VideoOutput/modes` | GET | `?output=0` — `output` here is **0-based** |
| `/v1/VideoOutput/resolution` | GET, POST | POST `{ width, height, refresh_hz, output }` or `{ auto: true, output }` (`output` **0-based**) |
| `/v1/NDIFinder/NDIDisServer` | GET, POST | `{ NDIDisServIP }` |
| `/v1/DeviceSettings/ndi-alias` | GET, POST | Receiver name |
| `/v1/DeviceSettings/ntp` | GET, POST | `{ ntp_server }` |
| `/v1/DeviceSettings/reboot-schedule` | GET, POST | `{ enabled, time: "HH:MM", days: ["sun", …] }` |
| `/v1/DeviceSettings/decode-mode` | GET, POST | `auto` / `hardware` / `software` |
| `/v1/Splash/config` | GET, POST | Splash |
| `/v1/System/version` | GET | Firmware / git / updates |
| `/v1/System/update` | POST | `git pull --ff-only` + `install.sh --no-deps` |
| `/v1/System/reboot` | POST | Reboot (POST only) |

There is no encode API (`/v1/NDIEncoder/*` is 404).

NDI protocol notes used while building this tree: [docs/ndi/REFERENCE.md](docs/ndi/REFERENCE.md). Official docs: [docs.ndi.video](https://docs.ndi.video/all/).

---

## Update

On the device, from the checkout recorded in `/etc/ndimon-source-dir` (**System → Update** in the UI does this):

```bash
cd /path/to/NDIMon-R
sudo bash install.sh --no-deps
```

Or `sudo bash scripts/update.sh` (pull, rebuild, swap API tree, restart). `/etc/` JSON is left alone.

---

## Troubleshooting

**No picture after connect**  
`sudo journalctl -u ndimon-r -f`. Confirm DRM is not owned by a desktop. Standard NDI and HX H.264/H.265 are the supported payloads. HDR P216/PA16 is warned, not converted.

**Empty source list**  
`systemctl is-active ndimon-finder`. DS IP must be reachable. Other-subnet senders need DS or extra IPs on **NDI → NDI Discovery**. Groups are case-sensitive (`Production` ≠ `production`).

**HX is black / “passthrough” banner**  
`/api/status` → `passthrough_ok`. If false, `ndi-config.v1.json` lost `codec.h264/h265.passthrough` after SDK init. Check `/var/lib/ndimon/.ndi/ndi-config.v1.json`. Hardware init failure should log a software fallback; `decode_backend` on the NDI page shows what actually ran.

**Web UI down**  
`systemctl status ndimon-api`. To use 8080: set `Environment=PORT=8080` on the unit (and skip port 80). Loopback health: `curl -s http://127.0.0.1/api/health`.

**Rockchip MPP missing**  
`ldconfig -p | grep librockchip_mpp`. Re-run `sudo bash scripts/setup-deps.sh` on a Radxa/Armbian image that ships MPP.

**NDI library missing**  
`ldconfig -p | grep libndi` then `sudo bash scripts/setup-deps.sh`.

**Source jumps**  
Saved source is in `ndimon-dec{N}-settings.json`. A Discovery Server with controlling enabled can change the source; that is by design.

---

## Architecture

```
NDI network
    │
    ├─ ndimon-finder  →  /etc/ndimon-sources.json
    │
    └─ ndimon-r (DRM master)
           NDIReceiver ─┬─ Standard: Framesync → DRM (+ NEON UYVY→XRGB)
                        ├─ HX: bitstream → MPP | V4L2 | VAAPI | FFmpeg → DRM
                        └─ ALSA
           IPC  /tmp/ndi-decoder.sock
                        │
           ndimon-api   Web UI + /v1 REST  (port 80)
           ndimon-watchdog  →  GET /api/health
```

CMake switches (on-device): `ENABLE_MPP`, `ENABLE_V4L2`, `ENABLE_FFMPEG`, `ENABLE_VAAPI`.

---

## License

MIT — [LICENSE](LICENSE).

The NDI SDK is Vizrt’s, not MIT. Running `install.sh` means you accept the [NDI SDK License Agreement](https://www.ndi.tv/license).
