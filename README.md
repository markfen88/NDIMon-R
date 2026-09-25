# NDIMon-R 1.1.1-beta

Dedicated **NDI receiver / HDMI decoder** for Linux appliances.

[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![Platform](https://img.shields.io/badge/arch-aarch64%20%7C%20x86--64-blue)](https://github.com/markfen88/NDIMon-R)

**1.1.1-beta** is a minor release on the 1.x line, not a 2.0. It keeps the 1.0 decoder and adds backup and restore, wired network settings with confirm-or-revert, and a scheduled reboot. The installer writes `1.1.1` to `/etc/ndimon-firmware-version`. Treat the network and backup flows as beta until they have been exercised on the board you ship.

It takes a live NDI stream off the network, decodes it, and scans it out to HDMI or DisplayPort with DRM/KMS. Audio goes to ALSA. A web UI on port 80 is the day-to-day control surface. The decoder core is C++ because the path is memory-bandwidth limited on ARM boards.

This is a **decoder**, not an encoder. It does not publish NDI.

NDI protocol notes used while building this tree: [docs/ndi/REFERENCE.md](docs/ndi/REFERENCE.md). Official docs: [docs.ndi.video](https://docs.ndi.video/all/).

---

## Contents

- [What it does](#what-it-does)
- [Requirements](#requirements)
- [Install](#install)
- [First boot](#first-boot)
- [Features](#features)
- [Receive path](#receive-path)
- [Hardware and codecs](#hardware-and-codecs)
- [Services](#services)
- [Recovery](#recovery)
- [Web UI](#web-ui)
- [HTTP API](#http-api)
- [Backup file](#backup-file)
- [Internal IPC](#internal-ipc)
- [Configuration](#configuration)
- [Update](#update)
- [Troubleshooting](#troubleshooting)
- [Layout](#layout)
- [Limits](#limits)
- [License](#license)

---

## What it does

NDIMon-R sits on a monitor or in a rack, remembers the last source, and comes back by itself after a reboot or a dropped sender.

| Incoming stream | What happens |
|-----------------|--------------|
| **Standard NDI** (SpeedHQ / UYVY / NV12) | The NDI SDK decodes SpeedHQ. FrameSync pulls video and audio to the local HDMI and ALSA clock. Colour conversion uses ARM NEON. Rockchip VOP2 can scan UYVY natively when rotation is 0. |
| **NDI HX** (H.264 / H.265) | The SDK is asked for a compressed bitstream (passthrough). NDIMon-R decodes it: Rockchip MPP, Pi 4 V4L2, Intel/AMD VAAPI, or FFmpeg. The Linux NDI SDK has **no** GPU decode of its own. |

If a hardware decoder fails to initialise, the worker falls back to FFmpeg so the output is not left black.

Each connector gets one worker: a DRM lease, an NDI receiver, an HX decoder, and a display thread. The first video frame chooses the pipeline. Compressed FourCCs (`H264`, `H265`, `HEVC`, `AVC1`) stay on the push capture path. Everything else switches to FrameSync.

---

## Requirements

- **OS:** Debian Bookworm/Trixie, Ubuntu 24.04, Armbian, or Raspberry Pi OS (64-bit).
- **CPU:** aarch64 or x86-64.
- **Display:** HDMI or DisplayPort. Do not run a desktop session that holds DRM master on that connector.
- **Network:** Avahi (`avahi-daemon`) for mDNS. An NDI Discovery Server is optional, for other subnets and for remote source control.
- **Privileges:** a full install is **root** (`sudo`). The decoder stays root (DRM). The API and finder run as user `ndimon`.
- **Build:** on the target. Do not cross-compile from Windows or macOS.

Installing accepts the [NDI SDK License Agreement](https://www.ndi.tv/license). The SDK tarball (~60 MB) is downloaded from NDI on the first `setup-deps`.

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

That pulls dependencies, the NDI SDK v6 line, a CMake build, binaries, systemd units, and the Node API. When it finishes, open `http://<device-ip>/`. The address is also drawn on the splash when the output is idle.

### Installer flags

| Command | When to use |
|---------|-------------|
| `sudo bash install.sh` | First install, or after a distro change |
| `sudo bash install.sh --no-deps` | Code update: rebuild and reinstall, keep packages |
| `sudo bash install.sh --no-build` | Units and API only; binaries already in `build/` |

The top-level `install.sh` always installs **system** units under `/etc/systemd/system`. Config files already in `/etc/` are never overwritten.

### What each step does

1. **`scripts/setup-deps.sh`** — build tools, libdrm, ALSA, Avahi, FFmpeg headers, Node.js 20, `python3`, `python3-yaml`, `iputils-arping`, and the NDI SDK into `/usr/local`. Rockchip MPP on RK boards. VAAPI drivers on x86 (Intel iHD/i965 and Mesa for AMD).
2. **`scripts/build.sh`** — `cmake -B build` and compile `ndimon-r` and `ndimon-finder`.
3. **`scripts/install.sh`** — `/usr/local/bin`, `/opt/ndimon-r/api`, user `ndimon`, `/usr/local/sbin/ndimon-priv`, `/usr/local/lib/ndimon/ndimon-net`, sudoers, `/var/lib/ndimon/.ndi` (shared NDI home), enable and start the services.

Optional checksum pin: `NDI_SDK_SHA256=<hex> sudo bash install.sh`.

CMake switches, set on the device: `ENABLE_MPP`, `ENABLE_V4L2`, `ENABLE_FFMPEG`, `ENABLE_VAAPI`.

```bash
sudo systemctl status ndimon-r ndimon-finder ndimon-api ndimon-watchdog
sudo journalctl -u ndimon-r -f
sudo bash scripts/status.sh
```

---

## First boot

1. Browse to `http://<device-ip>/`.
2. Log in. The default password is **`ndimon`**. Change it under **Settings → Security** before the box is on a real network. The UI keeps a warning up until you do.
3. Open **NDI** and pick a source. That choice is written to `/etc/ndimon-dec{N}-settings.json` and survives reboot.
4. Optional, on **NDI**: Discovery Server IP, NTP host, groups, extra IPs, transport, and HX decode mode.
5. Optional, on **Settings → Network**: leave DHCP, or set one static IPv4 address on the primary wired port. Wi-Fi is not configured here.
6. Optional, on **System**: scheduled reboot, watchdog mode, backup.

Factory watchdog mode is **active** (reconnect after about 30 s of stall). An existing `/etc/ndimon-device-settings.json` is not rewritten on update. Change an older box from `passive` under **System → Watchdog Mode**.

---

## Features

### Receive and display

- Dual-mode NDI: Standard (FrameSync) and HX (passthrough to a local decoder).
- One source per connector. HDMI-A-1, HDMI-A-2, and DP-1 keep channels 1, 2, and 3. Further connectors take the lowest free channel, up to 8.
- Scale: letterbox, stretch, crop. Rotation 0 / 90 / 180 / 270.
- HDMI mode auto-matched to the source, or a locked resolution from the UI.
- ALSA audio from NDI planar float, downmixed to stereo. One output owns audio.
- Custom splash and OSD when idle or disconnected.
- Monitor hotplug: the receiver still advertises if no display is present at boot. The picture starts when HDMI lands.
- Live address on the splash, refreshed about every 2 s. A short banner appears while a network change is waiting to be confirmed. The confirmation token is not drawn on HDMI.

### Appliance behaviour

- Last source auto-connects after reboot.
- Disconnect keeps the saved source so it can reconnect. Forget source is an explicit UI action (`None`).
- The API reconnects with exponential backoff (5 s → 30 s). Odd tries use the saved IP; even tries use the name only.
- Watchdog polls `/api/health`. **Active** mode disconnects a stalled output so the API reconnects the saved source. It does not restart the decoder process.
- `ndimon-r` is `Type=notify` with `WatchdogSec=30` and `Restart=always`.
- Optional NTP host writes a timesyncd or chrony drop-in and reapplies on API start. NDI has no time server of its own.
- Optional scheduled reboot (System page): local time and weekdays, as a systemd timer with `Persistent=false`. A missed window does not fire on the next boot.
- Backup and restore of user settings. Wired DHCP or static, with automatic rollback unless you confirm from the new address.

### Discovery and control

- mDNS via Avahi, plus an optional NDI Discovery Server. A blank IP turns it off. There is no separate enable flag.
- Receiver advertiser so the box shows up as a destination. `allow_controlling` lets the SDK switch sources. The app does **not** call `connect()` from a routing callback.
- NDI groups are case-sensitive (`Production` and `production` are different). Empty becomes `public`.
- Extra IPv4 addresses for senders off the local subnet.
- Transport: TCP on a new install (the UI default). RUDP, UDP, Multicast, and M-TCP are selectable. A change rewrites `ndi-config.v1.json` and recreates the receiver. RUDP is the NDI SDK’s own default if you pick it. Multicast also requires the sender to be multicasting.
- Named source presets (up to 32). Recall connects immediately, without a rescan.
- Device name is the OS hostname (`hostnamectl`). It is not written into the SDK config as `machinename`. That override causes mDNS name clashes.

### Security

This is a LAN appliance, not a public service.

- Password is scrypt in `/etc/ndimon-auth.json`. Until that file exists the password is `ndimon`.
- Session cookie (`HttpOnly`, `SameSite=Lax`, 7 days) or `Authorization: Bearer`. Sessions live in memory and disappear when `ndimon-api` restarts.
- Five failed logins per IP per minute returns 429.
- Same-origin only. Mutating requests must send an `Origin` whose host matches `Host`. CORS is not enabled.
- GET does not change state (`connectTo`, reboot, finder reset, and similar return 405 on GET).
- The API and finder run as `ndimon`. Reboot, hostname, NTP, the reboot timer, service restarts, and network changes go through `/usr/local/sbin/ndimon-priv`.
- IPC socket `/tmp/ndi-decoder.sock` is mode `0660`, group `ndimon`. Do not expose it on the LAN.
- `GET /api/health` is unauthenticated from loopback only, for the watchdog.
- `POST /api/net-confirm` is unauthenticated. The token is the credential, because the session cookie belongs to the old address.

---

## Receive path

```
NDI network
    │
    ├─ ndimon-finder  →  /etc/ndimon-sources.json
    │
    └─ ndimon-r (DRM master)
           NDIReceiver ─┬─ Standard: FrameSync → DRM (+ NEON UYVY→XRGB)
                        ├─ HX: bitstream → MPP | V4L2 | VAAPI | FFmpeg → DRM
                        └─ ALSA
           IPC  /tmp/ndi-decoder.sock
                        │
           ndimon-api   Web UI + REST  (port 80)
           ndimon-watchdog  →  GET /api/health
```

| | Standard NDI | NDI HX |
|--|--------------|--------|
| Payload | SpeedHQ, decoded by the SDK to UYVY or NV12 | H.264 or H.265 bitstream |
| Capture | `NDIlib_framesync` pull, about an 8 ms loop | `NDIlib_recv_capture_v3`, every frame kept |
| Why | A/V sync, duplicate, drop, silence insertion | Compressed frames cannot be dropped |
| Picture | CPU blit into an XRGB dumb buffer | Hardware decode to DMA-BUF, then a DRM plane |
| Copy | Frame is copied; the SDK frame is freed in the callback | SDK frame is held until the display thread frees it |

### HX passthrough

Before `NDIlib_initialize`, the process writes `codec.h264.passthrough` and `codec.h265.passthrough` into `/var/lib/ndimon/.ndi/ndi-config.v1.json`. The SDK rewrites that file during init, so the process writes it again and checks the keys. `passthrough_ok` is on `/api/status` and `/api/health`. A failed check is a red banner in the UI.

Those keys work on the pinned standard SDK v6 line. They are not the documented Advanced SDK per-receiver override. The backup does **not** contain `ndi-config.v1.json`. The decoder rewrites it on startup, including passthrough and transport.

### Decoder factory

`VideoDecoder::create()` reads `decode_mode` (`auto`, `hardware`, `software`) from device settings.

| Mode | Behaviour |
|------|-----------|
| `software` | FFmpeg only |
| `auto` / `hardware` | Rockchip MPP, then Pi 4 V4L2 M2M, then x86 VAAPI when a render node exists and the GPU is not NVIDIA, then FFmpeg |

`hardware` with no device logs and falls through to software. If hardware `init` fails on the first HX frame, the worker replaces the decoder with FFmpeg. The NDI page shows an `HW` / `SW` badge. **SATURATED** means an HX stream’s decoded FPS stayed under 85% of the source FPS for 3 seconds. Uncompressed FrameSync never trips that.

| Backend | Where | Scanout |
|---------|-------|---------|
| MPP | RK3588 / RK3399 | NV12 DMA-BUF into the plane |
| V4L2 M2M | Pi 4 H.264 (`/dev/video10`) | Exported DMA-BUF, or CPU if export fails |
| VAAPI | Intel iHD/i965, AMD Mesa | DRM PRIME with modifiers. CPU NV12 if export fails |
| FFmpeg | Pi 5, generic ARM, NVIDIA | CPU NV12. One low-latency thread on ARM; up to 16 on x86 |

### Picture and audio

DRM leases give each connector its own master. Queues hold two frames and drop the oldest. Idle or disconnected outputs draw a splash (logo, colours, text). An OSD string can sit on the live picture.

NDI audio is planar float. ALSA downmixes to interleaved S16LE stereo at 48 kHz. Layouts of 5.1 and above use an ITU-R BS.775 mix. The buffer is four periods of 1024 frames. FrameSync requests a sample count from elapsed time so the HDMI clock paces playback, not the sender.

---

## Hardware and codecs

| Board | HX H.264 | HX H.265 | Standard NDI |
|-------|----------|----------|----------------|
| Radxa Rock 5B (RK3588) | MPP | MPP | SDK + NEON, or native UYVY |
| Radxa Rock 4C+ / 4B+ (RK3399) | MPP | MPP | SDK + NEON, or native UYVY |
| Raspberry Pi 4 | V4L2 M2M | FFmpeg | SDK + NEON |
| Raspberry Pi 5 | FFmpeg | FFmpeg | SDK + NEON |
| Other aarch64 | FFmpeg | FFmpeg | SDK + NEON |
| Intel NUC | VAAPI | VAAPI | SDK + SSE |
| AMD | Mesa VAAPI | Mesa VAAPI | SDK + SSE |
| NVIDIA Linux | FFmpeg | FFmpeg | SDK + SSE |

Pi HEVC is stateless and is not used. NVIDIA is skipped because there is no NVDEC path yet. The x86 System page shows the VAAPI driver and decode profiles captured by `vainfo` at install time.

---

## Services

| Unit | User | Role |
|------|------|------|
| `ndimon-r` | root | Capture, decode, DRM, ALSA, IPC. `Type=notify`, `WatchdogSec=30`, `Nice=-10` |
| `ndimon-finder` | `ndimon` | Continuous NDI Find. Rewrites `/etc/ndimon-sources.json` when the list changes |
| `ndimon-api` | `ndimon` | Express on port 80, session auth, SSE, reconnect loops |
| `ndimon-watchdog` | root | Every 15 s: process liveness, then the health policy |
| `ndimon-net-boot` | root | Oneshot. If a network change was still unconfirmed at boot, roll it back |
| `ndimon-update` | root | Oneshot, started by the Update button. Not left running |

Finder and decoder pin `HOME` to `/var/lib/ndimon` so they share one official NDI config file, `/var/lib/ndimon/.ndi/ndi-config.v1.json`. Both emit the same transport keys from `Rxpm`. The SDK reads transport only when a receiver is created.

```bash
sudo systemctl restart ndimon-r ndimon-finder ndimon-api ndimon-watchdog
sudo journalctl -u ndimon-r -u ndimon-api -f
```

---

## Recovery

The C++ health check records state and resets a stuck DRM flip. It does not reconnect by itself. Node.js schedules retries. Active watchdog recovers a stalled output. systemd covers a dead or hung process.

Health is sampled every 500 ms in `DisplayWorker::tick()`, after a 10 s grace from connect.

| Layer | Trigger | Action |
|-------|---------|--------|
| Display flip reset | Frames arriving, no DRM commit for 3 s | `reset_flip_pending()` inside the worker |
| API reconnect | IPC says disconnected, and a source is saved | Backoff 5 s, 10 s, 20 s, then 30 s |
| Active watchdog | `stall_count` ≥ 60 (about 30 s), mode `active` | IPC disconnect. The saved source stays. The API connects again |
| systemd notify | No `WATCHDOG=1` within 30 s | Kills `ndimon-r`. `Restart=always` brings it back in 3 s |
| Process liveness | `ndimon-r`, the API, or the finder down for 5 watchdog passes | `systemctl restart`, then a 60 s cooldown |

| Signal | Unhealthy when |
|--------|----------------|
| Recv thread heartbeat | Older than 5 s |
| Video frames | None arrived, and the connect is older than 10 s |
| Decoder | Video younger than 2 s, decoded output older than 5 s |
| Display commit | Frames flowing, last commit older than 3 s |
| HX saturation | Decoded FPS under 85% of source FPS for 3 s |

`/api/health` reports `stalled` when `stall_count` is at least 60, `degraded` for any stall, and `idle` when not connected.

| Watchdog mode | Behaviour |
|---------------|-----------|
| `active` | Factory default. Disconnects a stalled output |
| `passive` | Logs counters only |
| `disabled` | Skips the health poll |

Process restarts still run in every mode.

---

## Web UI

Single page at `/` (`api/public/index.html`). Dark theme. Four sidebar pages. Live state comes from `EventSource /api/events` about every 1.5 s. The page closes the previous stream before opening another.

| Chrome | Behaviour |
|--------|-----------|
| Login overlay | Password field. `POST /api/login` |
| Top bar | LIVE / IDLE, source, resolution, sign out |
| Password warning | Shown while the password is still `ndimon` |
| Passthrough warning | `passthrough_ok` is false |
| Saturated warning | HX decode FPS under 85% of the source for 3 s |

### Dashboard

Stream status (LIVE/IDLE, source, resolutions, FPS, codec, platform, frame counters), system resources (CPU, memory, uptime, thermals), **Change Source**, **Refresh Sources**, and a read-only source list.

### NDI

| Control | Options | API |
|---------|---------|-----|
| Current connections | Per-output LIVE/IDLE, codec, HW/SW, SATURATED | SSE `/api/events` |
| Tally | `TallyOff`, `TallyOn`, `VideoMode` | `POST /v1/NDIDecode/decodesetup` |
| Audio | `NDIAudioEn`, `NDIAudioDis` | same |
| Presets | Save, recall, delete | `/v1/Presets/*` |
| Output name | Free text | `POST /v1/DeviceSettings/output-alias` |
| NDI source | None, discovered, or manual name and IP | `POST /v1/NDIDecode/connectTo` |
| Resolution | Auto, or a mode from the EDID list | `POST /v1/VideoOutput/resolution` |
| Scale | letterbox, stretch, crop | `POST /v1/VideoOutput/scaleMode` |
| Rotation | 0, 90, 180, 270 | `POST /v1/VideoOutput/rotation` |
| Discovery Server IP | Blank = off | `POST /v1/NDIFinder/NDIDisServer` |
| NTP | Hostname or IP. Blank keeps the OS default | `POST /v1/DeviceSettings/ntp` |
| NDI groups | Case-sensitive tags | `POST /v1/NDIFinder/NdiGrpName` |
| Off-subnet IPs | Comma-separated IPv4 | `POST /v1/NDIFinder/NdiOffSnSrc` |
| Device name | Receiver name. Also sets the hostname | `POST /v1/DeviceSettings/ndi-alias` |
| Screensaver | `BlackSS`, `SplashSS`, `CaptureSS` | `POST /v1/NDIDecode/decodesetup` |
| Colour space | `YUV`, `RGB` | same |
| Transport | TCP, UDP, Multicast, M-TCP, RUDP | `POST /v1/NDIDecode/decodeTransport` |
| HX decode | auto, hardware, software | `POST /v1/DeviceSettings/decode-mode` |

`Output` on connect and preset recall is **1–8**. VideoOutput query `output` is **0-based** (channel = index + 1).

Screensaver and colour space are stored and sent in connect metadata. `CaptureSS` is stored; the core does not freeze a frame for it. `TallyMode` `VideoMode` is stored; program tally is applied when the mode is `TallyOn`.

### Settings

| Control | API |
|---------|-----|
| Change password (current, new, confirm, minimum 6 characters) | `POST /api/password` |
| Wired network: DHCP or static, configured vs live address, gateway, DNS | `GET /v1/Network/status`, `POST /v1/Network/apply` |
| Splash colours, logo position, overlay text, visibility | `POST /v1/Splash/config` |
| Logo upload (PNG or JPEG) | `POST /v1/Splash/uploadLogo` |
| Splash preview | `POST /v1/Splash/preview` |
| OSD enable and text (max 128 characters) | `POST /v1/Splash/osd` |

A network apply shows the difference, warns if the new address is on another subnet or already answers ARP, then waits for confirmation. **Cancel and revert** is on that dialog and on the Network card while a change is pending. Wi-Fi is not changed.

### System

| Control | API |
|---------|-----|
| Device information | `GET /v1/AboutMe` and `/api/status` |
| GPU decode (x86) | `GET /v1/System/vaapi-info` |
| Check for updates / Update now | `GET /v1/System/version`, `POST /v1/System/update` |
| Backup and restore | `/v1/Backup/*` |
| Scheduled reboot | `GET` / `POST /v1/DeviceSettings/reboot-schedule` |
| Restart one service | `POST /api/services/:name/restart` |
| Restart decoder and finder | Restarts those two units. The API and watchdog stay up |
| Reboot | `POST /v1/System/reboot` |
| Watchdog mode and counters | `POST /v1/DeviceSettings/watchdog-mode`, `GET /api/health`, `GET /api/watchdog-stats` |

### Backup and restore

**System → Backup & Restore** downloads one JSON file. **Include network settings** is on by default. **Include admin password** is off. Turn the password on only when restoring this same device.

Restore shows where the file came from, then one row per section with a Changed or Unchanged badge. Unchanged sections start unticked. You choose what to import.

Network settings are mentioned only when they differ.

| This device | Backup file | What you see |
|-------------|-------------|--------------|
| DHCP | DHCP | Nothing. Network is left alone |
| Static A | The same static address, prefix, gateway, and DNS | Nothing |
| DHCP | Static B | A dialog naming B, the prefix, gateway, and DNS, and the current DHCP address |
| Static A | Static B | The same dialog, listing the fields that differ |
| Static A | DHCP | A dialog saying the new address will come from DHCP and will show on the display |
| Any | No network section | Nothing |
| Any | Network present, but this manager cannot apply it | An information line. No dialog. dhcpcd is read-only |

**Keep current network settings** is the default action. **Use backup network settings** applies the file. If another device already answers ARP for that address, Use stays disabled until you tick the acknowledgement.

Leave **Device name** unticked when cloning a second unit. It is pre-ticked only when the backup hostname matches this box and the name actually differs. Groups are restored with their original case.

A network change reverts after about two minutes (three for DHCP) unless you open the new address and confirm. Rebooting during that window also reverts it. The confirm link is `http://<new-address>/#netconfirm=<token>`. For DHCP the link fills in once the lease appears, and the splash shows the address. Copy is on the dialog. Closing the tab does not lose the change: the Network card still offers **Show link** and **Cancel and revert** until it is confirmed or the timer fires.

Undo last restore puts settings back to the automatic snapshot taken just before the import, including a network change if that address differs.

---

## HTTP API

Base URL `http://<device-ip>/`. Prefer `/v1/…`. The same routers are also mounted without `/v1` for `NDIDecode`, `NDIFinder`, `VideoOutput`, `DeviceSettings`, `System`, `AboutMe`, and `NDIEncoder`. Splash, Presets, Backup, Network, and ApiSettings are `/v1` only.

All `/v1/*` and `/api/*` routes need a session except:

- `POST /api/login`
- `GET /api/auth-status`
- `GET /api/health` from **loopback only**
- `POST /api/net-confirm` (the token is the credential)

Login:

```bash
curl -c cookies.txt -H 'Content-Type: application/json' \
  -d '{"password":"ndimon"}' http://DEVICE/api/login
```

Then send the cookie, or `Authorization: Bearer <token>` from the login JSON.

### Authentication

| Method | Path | Body | Response |
|--------|------|------|----------|
| POST | `/api/login` | `{ "password" }` | `{ ok, token, default_password }` and `Set-Cookie` |
| POST | `/api/logout` | — | Clears the session |
| GET | `/api/auth-status` | — | `{ auth_required, logged_in, default_password }` |
| POST | `/api/password` | `{ "current", "password" }` | Password at least 6 characters. Clears other sessions |

### Status and operations

| Method | Path | Notes |
|--------|------|-------|
| GET | `/api/status` | UI status: stream, system, sources, decoder, discovery, `passthrough_ok`, `outputs[]` |
| GET | `/api/health` | IPC health. 503 if the decoder socket is down |
| GET | `/api/watchdog-stats` | Counters from `/tmp/ndimon-watchdog-stats.json` |
| GET | `/api/events` | SSE. First event `connected`, then `type: status` about every 1.5 s. At most 20 clients |
| GET | `/api/services` | systemd state of the four long-running units |
| POST | `/api/services/:name/restart` | `ndimon-r`, `ndimon-finder`, `ndimon-api`, or `ndimon-watchdog` |
| POST | `/api/net-confirm` | `{ "token" }` — 64 hex characters. No session. 410 if it has expired. 429 after 5 failures in a minute |

`/api/health` includes `alive`, `uptime_s`, `passthrough_ok`, and `outputs[]` with `output`, `connected`, `fps`, `health` (`idle`, `ok`, `degraded`, `stalled`), stale-age fields, and `stall_count`.

### `/v1/NDIDecode`

| Method | Path | Body | Effect |
|--------|------|------|--------|
| GET | `/decodestatus` | — | Reads `/etc/ndimon-dec1-status.json` |
| GET | `/decodesetup` | — | Reads `/etc/ndimon-dec1-settings.json` |
| POST | `/decodesetup` | `NDIAudio`, `ScreenSaverMode`, `TallyMode`, `ColorSpace`, `ChNum` | Writes allowed fields, then `reload_config` |
| GET | `/decodeTransport` | — | `{ Rxpm }` |
| POST | `/decodeTransport` | `{ "Rxpm" }` | `TCP`, `UDP`, `Multicast`, `M-TCP`, or `RUDP`. Recreates the receiver |
| POST | `/connectTo` | `{ "SourceName", "SourceIP", "Output" }` | `Output` is 1–8. Empty or `None` forgets the source. Otherwise saves it and connects. Starts a 30 s grace so a Discovery Server route does not immediately undo a local pick |
| GET | `/connectTo` | — | 405 |
| GET | `/capture` | — | Touches a capture flag and returns text. Not a freeze-frame |

Allowed `decodesetup` values: `NDIAudioEn` or `NDIAudioDis`; `BlackSS`, `SplashSS`, or `CaptureSS`; `TallyOn`, `TallyOff`, or `VideoMode`; `ColorSpace` `YUV` or `RGB`.

### `/v1/NDIFinder`

| Method | Path | Body | Effect |
|--------|------|------|--------|
| POST | `/refresh` | — | Restarts the finder. Text `success` |
| GET | `/refresh` | — | 405 |
| POST | `/reset` | — | Clears the source cache to `None` |
| GET | `/List` | — | `{ count, list }` name → URL |
| GET | `/NdiOffSnSrc` | — | Plain text, comma-separated IPv4 |
| POST | `/NdiOffSnSrc` | text/plain IP list | Validates and writes `/etc/ndi-config.json` as a JSON string |
| GET | `/NdiGrpName` | — | `{ ndi_groups }` |
| POST | `/NdiGrpName` | string or `{ ndi_groups }` | Case preserved. `reload_config` |
| GET | `/NDIDisServer` | — | IP plus derived `NDIDisServEn` or `NDIDisServDis` |
| POST | `/NDIDisServer` | `{ "NDIDisServIP" }` | Blank disables. `reload_config` and a finder restart |

### `/v1/VideoOutput`

`output` is 0-based.

| Method | Path | Body | Effect |
|--------|------|------|--------|
| GET | `/modes?output=0` | — | `{ modes, current, auto_resolution, rotation }` |
| GET | `/resolution?output=0` | — | Current mode |
| POST | `/resolution` | `{ "auto": true, "output" }` or `{ "width", "height", "refresh_hz", "output" }` | Auto-match, or lock a mode |
| POST | `/scaleMode` | `{ "scale_mode", "output" }` | `letterbox`, `stretch`, or `crop` |
| POST | `/rotation` | `{ "degrees", "output" }` | 0, 90, 180, or 270 |

### `/v1/DeviceSettings`

| Method | Path | Body | Effect |
|--------|------|------|--------|
| GET, POST | `/operationmode` | plain `encode` or `decode` | Stored only. Nothing encodes. Not in the UI |
| GET | `/hostname` | — | `{ hostname }` |
| GET, POST | `/ndi-alias` | `{ "ndi_recv_name" }` | Saves the alias and sets the OS hostname |
| GET, POST | `/output-alias` | GET `?ch=1`. POST `{ "ch", "output_alias" }` | Per-output alias |
| GET, POST | `/decode-mode` | `{ "decode_mode" }` | `auto`, `hardware`, or `software`. Rebuilds the decoder |
| GET, POST | `/watchdog-mode` | `{ "watchdog_mode" }` | `disabled`, `passive`, or `active` |
| GET, POST | `/ntp` | `{ "ntp_server" }` | GET also returns `synchronized` and `active_server` |
| GET, POST | `/reboot-schedule` | `{ "enabled", "time": "HH:MM", "days": ["sun", …] }` | systemd timer, `Persistent=false` |

### `/v1/Splash`

| Method | Path | Body | Effect |
|--------|------|------|--------|
| GET, POST | `/config` | colours, logo percentages, text, `show_*` | `logo_path` is only an `/etc/ndi-splash-logo.*` path, or empty |
| POST | `/preview` | `{ "source_available": true/false }` | Splash on idle outputs |
| POST | `/uploadLogo` | `{ "data", "filename" }` | PNG or JPEG magic bytes. Writes `/etc/ndi-splash-logo.png` or `.jpg` |
| GET, POST | `/osd` | `{ "enabled", "text" }` | Text max 128 characters |

Splash defaults: idle background `#0D1B2A`, live background `#0D2B1A`, idle accent `#4488CC`, live accent `#22FF88`, logo at 50/50/50 percent, text “No Signal” / “Signal Available”, text height 4 percent of the screen.

### `/v1/Presets`

| Method | Path | Body | Effect |
|--------|------|------|--------|
| GET | `/list` | — | `{ presets: [{ name, source, ip }] }`, at most 32 |
| POST | `/save` | `{ "name", "source", "ip" }` | Upsert by name |
| POST | `/delete` | `{ "name" }` | Remove |
| POST | `/recall` | `{ "name", "output" }` | `output` is 1–8. Saves the source and connects |

### `/v1/System`

| Method | Path | Notes |
|--------|------|-------|
| GET | `/version` | `firmware`, `commit`, `build_date`, `ndi_version`, `update_supported`, `updates_available` |
| POST | `/update` | Detached `git pull --ff-only` and `install.sh --no-deps`. 409 if one is already running. 501 if this install has no source directory |
| POST | `/reboot` | Acknowledges, then reboots |
| POST | `/softreboot` | Restarts the stack. Not in the UI |
| GET | `/vaapi-info` | `{ available, driver, decode_profiles }` from the install-time `vainfo` capture |
| GET | `/status` | Raw IPC status. `/api/status` is the one the UI uses |

### `/v1/AboutMe` and `/v1/ApiSettings`

| Method | Path | Notes |
|--------|------|-------|
| GET | `/v1/AboutMe/` | Device name, hostname, IP, firmware, model, NDI version, build date, per-channel info |
| GET, POST | `/v1/ApiSettings/` | Legacy whitelist (`GroupName`, `MACAddress`, `device_name`, `host_name`). The UI does not call it |

### `/v1/Backup`

Session required. Inspect accepts an 8 MB JSON body. The global JSON limit stays 4 MB.

| Method | Path | Body | Effect |
|--------|------|------|--------|
| GET | `/export?network=1&password=0` | — | Downloads `ndimon-<alias>-<YYYYMMDD-HHMM>.json`. `network=0` omits network. `password=1` includes the scrypt hash |
| POST | `/inspect` | The backup document | Validates schema and checksum. Returns `import_id`, source device, per-section diffs, and the network comparison (`needs_prompt`, message, warnings, `duplicate`) |
| POST | `/apply` | `{ "import_id", "sections": [], "apply_network": false }` | Writes the ticked sections. Stops the decoder only when a file section is selected. Network is applied last, through the confirm-or-revert path |
| GET | `/snapshots` | — | Automatic pre-restore files, newest first. Five are kept |
| POST | `/undo` | `{ "name" }` | Restores that snapshot. Applies network only when it differs. Undo takes its own snapshot first |

`sections` may be `outputs`, `presets`, `discovery`, `transport`, `display`, `device`, `identity`, `auth`. Network is not a section name; it is `apply_network`.

A damaged checksum is refused. A newer `schema_version` is refused. Unknown keys are refused. Groups are not lowercased. Discovery Server is enabled only when the address is non-empty. The device name uses `hostnamectl`, never `machinename`. `ndi-config.v1.json` is not imported.

### `/v1/Network`

Primary wired interface only: the interface of the default route if it is wired, otherwise the first wired interface that is up, otherwise the first wired interface. Wi-Fi is out of scope.

| Method | Path | Body | Effect |
|--------|------|------|--------|
| GET | `/status` | — | Backend, interface, configured mode and fields, live address, gateway, DNS, `applicable`, and `pending` plus `pending_info` (token, address, deadline) while a change is open |
| POST | `/check` | `{ "address" }` | Duplicate-address probe (`arping`). Advisory |
| POST | `/apply` | `{ "mode", "address", "prefix", "gateway", "dns", "dry_run" }` | `mode` is `dhcp` or `static`. Prefix 8–30. Gateway must sit in the subnet. At most 3 DNS servers. `dry_run: true` returns the prompt and does not change anything |
| POST | `/rollback` | — | Restores the previous configuration and cancels the pending change |

Apply writes a staged file, schedules rollback (120 s static, 180 s DHCP), and applies about 2 seconds later so the HTTP response can leave before the address moves. Confirm with `POST /api/net-confirm` from the new address.

Supported managers, in detection order: NetworkManager, netplan (networkd renderer), systemd-networkd, ifupdown. dhcpcd is reported and not written. Anything else is `unsupported` and apply returns 409.

### Stubs

| Path | Response |
|------|----------|
| `/v1/NDIEncoder/encodesetup` | 404 `{ "status": "Command not supported" }` |
| `/v1/NDIEncoder/encodeTransport` | 404, same body |

### Control map

| UI control | HTTP | Then |
|------------|------|------|
| Login / logout / password | `/api/login`, `/api/logout`, `/api/password` | — |
| Live status | `GET /api/events` | IPC `subscribe` and `status` |
| Refresh sources | `POST /v1/NDIFinder/refresh` | Restarts the finder |
| Apply source | `POST /v1/NDIDecode/connectTo` | IPC `connect` |
| Source = None | `connectTo` with `None` | IPC `forget_source` |
| Recall preset | `POST /v1/Presets/recall` | IPC `connect` |
| Resolution auto / manual | `POST /v1/VideoOutput/resolution` | `auto_resolution` or `set_resolution` |
| Scale / rotation | `/v1/VideoOutput/scaleMode`, `/rotation` | `set_scale_mode`, `set_rotation` |
| Tally, audio, screensaver, colour | `POST /v1/NDIDecode/decodesetup` | `reload_config` |
| Transport | `POST /v1/NDIDecode/decodeTransport` | Recreate the receiver |
| HX decode mode | `POST /v1/DeviceSettings/decode-mode` | `reload_config` |
| Discovery Server | `POST /v1/NDIFinder/NDIDisServer` | `reload_config` and restart the finder |
| Groups | `POST /v1/NDIFinder/NdiGrpName` | `reload_config` |
| Device name | `POST /v1/DeviceSettings/ndi-alias` | `reload_config` and `hostnamectl` |
| NTP | `POST /v1/DeviceSettings/ntp` | `ndimon-priv set-ntp` |
| Splash / OSD | `POST /v1/Splash/config` or `/osd` | `reload_config` |
| Splash preview | `POST /v1/Splash/preview` | `show_splash` |
| Watchdog mode | `POST /v1/DeviceSettings/watchdog-mode` | The watchdog reads the file |
| Scheduled reboot | `POST /v1/DeviceSettings/reboot-schedule` | `ndimon-priv set-reboot-schedule` |
| Network apply | `POST /v1/Network/apply` | `ndimon-priv net-apply`, then `/api/net-confirm` |
| Update | `POST /v1/System/update` | Detached pull and rebuild |
| Reboot | `POST /v1/System/reboot` | `ndimon-priv reboot` |

---

## Backup file

One UTF-8 JSON document, pretty-printed.

| Field | Meaning |
|-------|---------|
| `format` | `ndimon-backup` |
| `schema_version` | `1`. A newer file is refused |
| `created_at` | ISO 8601 UTC |
| `app` | Firmware, build commit, NDI SDK version. Display only |
| `source_device` | Hostname, NDI alias, platform, last three MAC bytes. Display only. Never applied as a MAC |
| `sections` | The settings below. A section left out of the export is absent |
| `checksum` | SHA-256 of the canonical JSON of `sections` (keys sorted, no whitespace) |

| Section | Contents | Files |
|---------|----------|-------|
| `outputs` | Source name and IP, alias, scale, rotation, mode, audio, tally, screensaver, colour. Channels 1–8 that exist on disk | `/etc/ndimon-dec{N}-settings.json` |
| `presets` | Named sources | `/etc/ndimon-presets.json` |
| `discovery` | Discovery Server IP, groups, extra IPs | `ndimon-find-settings.json`, `ndi-group.json`, `ndi-config.json` |
| `transport` | `Rxpm` | `/etc/ndimon-rx-settings.json` |
| `display` | Splash, OSD, logo as base64 | splash and OSD JSON, `/etc/ndi-splash-logo.png` or `.jpg` |
| `device` | Watchdog mode, decode mode, NTP, reboot schedule | device settings, those keys only |
| `identity` | `ndi_recv_name` and hostname | device settings and `hostnamectl` |
| `network` | `dhcp` or `static`, plus address, prefix, gateway, DNS. Manager and interface names are informational | Applied to **this** device’s primary wired port, not by interface name |
| `auth` | scrypt salt and hash. Only when the password checkbox was ticked | `/etc/ndimon-auth.json` |

Not exported: the finder cache, status files, `ndi-config.v1.json`, build and version files, `device_ip`, and any other runtime key.

On import the API writes only allow-listed keys, merging into the current file so keys from sections you did not select stay. The logo is checked for PNG or JPEG magic bytes and written to a fixed path. A snapshot is saved under `/var/lib/ndimon/backups/` before anything changes. The decoder is stopped only while file sections are written, then started again so it reloads sources, groups, transport, and discovery. NTP and the reboot timer are reapplied. Network is last.

---

## Internal IPC

`/tmp/ndi-decoder.sock`, `AF_UNIX` `SOCK_STREAM`, mode `0660`, group `ndimon`. One JSON object per line. The API keeps a `subscribe` connection and uses short request/response calls (2 s timeout). The watchdog may send `disconnect` itself.

Do not expose this socket. Use the HTTP API.

| Action | Effect |
|--------|--------|
| `subscribe` | Keeps the connection. Replays the last routing and connection state. Pushes `connection` and `routing` events |
| `status` / `get_status_all` / `health` / `get_modes` | Queries |
| `connect` | `source_name`, `source_ip`, `output` (0-based) |
| `disconnect` | Drops the picture. Keeps the saved source |
| `forget_source` | Disconnects and clears the saved source |
| `reload_config` | Reread `/etc` JSON and apply it |
| `set_scale_mode`, `set_rotation`, `auto_resolution`, `set_resolution` | Picture controls. `output` is 0-based |
| `set_tally` | `tally_program`, `tally_preview` |
| `show_splash` | `source_available`. Idle outputs only |

`connection` events carry `output` (0-based), `connected`, `source`, and `drm_ready`. `routing` events carry `output`, `source`, and `url`. The API persists a Discovery Server route. It does not send `connect` for it.

Worker status includes `ch_num`, `connector`, `output_alias`, `connected`, `source_name`, input size, `fps`, `codec`, `decode_backend`, `hw_decode`, `decode_fps`, `decode_saturated`, `scale_mode`, `rotation`, and `stream_type` (`Standard`, `HX`, or `unknown`).

---

## Configuration

Templates in `config/` are copied to `/etc/` only when the file is missing. Writes are a temp file, `fsync`, rename, then `fsync` of the directory.

| File | Writer | Contents |
|------|--------|----------|
| `ndimon-dec{N}-settings.json` | API and `ndimon-r` | Source, scale, rotation, alias, audio, tally. `N` is 1–8 |
| `ndimon-device-settings.json` | API, plus alias init in C++ | Alias, watchdog mode, NTP, decode mode, reboot schedule |
| `ndimon-rx-settings.json` | API | Transport `Rxpm`. New installs ship `TCP` |
| `ndimon-find-settings.json` | API | Discovery Server IP. Non-empty means enabled |
| `ndi-group.json` | API | Groups, case-sensitive |
| `ndi-config.json` | API | Extra finder IPs, stored as a JSON string |
| `ndimon-presets.json` | API | Named presets |
| `ndimon-auth.json` | API | Password hash, mode `0600` |
| `ndimon-splash-settings.json`, `ndimon-osd-settings.json` | API | Splash and OSD |
| `ndimon-sources.json` | Finder | Name-to-URL cache |
| `/var/lib/ndimon/.ndi/ndi-config.v1.json` | Both C++ processes | SDK home: groups, discovery, transport, HX passthrough |
| `/var/lib/ndimon/backups/` | API | Pre-restore snapshots |
| Wired network | `ndimon-net` via `ndimon-priv` | Primary wired interface. The API only stages the request |

The splash IP is a runtime value. C++ does not write it back to disk.

Prefer the web UI. It writes JSON and tells the decoder to reload. If you edit `/etc/` by hand:

```bash
sudo systemctl restart ndimon-r ndimon-finder ndimon-api
```

---

## Update

**System → Check for Updates**, then **Update Now**. That runs from the checkout recorded in `/etc/ndimon-source-dir`:

```bash
cd /path/to/NDIMon-R
sudo bash install.sh --no-deps
```

Or `sudo bash scripts/update.sh`. `/etc` JSON is left alone.

---

## Troubleshooting

**No picture after connect.** `sudo journalctl -u ndimon-r -f`. Confirm a desktop does not own DRM. Standard NDI and HX H.264/H.265 are the supported payloads. HDR `P216` / `PA16` is logged and not converted.

**Empty source list.** `systemctl is-active ndimon-finder`. The Discovery Server IP must be reachable. Senders on another subnet need that server or extra IPs under **NDI**. Groups are case-sensitive.

**HX is black, or the passthrough banner is up.** `/api/status` → `passthrough_ok`. If false, `ndi-config.v1.json` lost the passthrough keys after SDK init. Look in `/var/lib/ndimon/.ndi/`. `decode_backend` on the NDI page shows what actually ran.

**Web UI down.** `systemctl status ndimon-api`. To use another port, set `Environment=PORT=8080` on the unit. Loopback health: `curl -s http://127.0.0.1/api/health`.

**Rockchip MPP missing.** `ldconfig -p | grep librockchip_mpp`, then `sudo bash scripts/setup-deps.sh` on an image that ships MPP.

**NDI library missing.** `ldconfig -p | grep libndi`, then `sudo bash scripts/setup-deps.sh`.

**Source jumps.** The saved source is in `ndimon-dec{N}-settings.json`. A Discovery Server with controlling enabled can change it. That is the SDK, not a reconnect bug. A local connect is protected for 30 seconds.

**Network change did not stick.** Open the confirm link from a host that can reach the new address before the countdown ends. If you rebooted first, `ndimon-net-boot` already restored the old settings. dhcpcd is reported and cannot be written; use NetworkManager, netplan, systemd-networkd, or ifupdown.

**Lost the confirm page.** **Settings → Network** still shows the pending change, with **Show link** and **Cancel and revert**, until the timer fires.

---

## Layout

| Path | Role |
|------|------|
| `src/main.cpp` | Process entry, one worker per connector, health tick, systemd notify, IPC wiring |
| `src/NDIReceiver.cpp` | SDK receive: advertiser, connect, FrameSync vs HX capture, tally, transport recreate |
| `src/VideoDecoder.cpp` | Factory: MPP, V4L2, VAAPI, FFmpeg, honoring `decode_mode` |
| `src/MppDecoder.cpp` | Rockchip H.264/H.265 to NV12 DMA-BUF |
| `src/V4L2Decoder.cpp` | Pi 4 M2M H.264 |
| `src/VAAPIDecoder.cpp` | Intel/AMD VAAPI, DRM PRIME modifiers |
| `src/SoftwareDecoder.cpp` | FFmpeg NV12 |
| `src/DRMDisplay.cpp` | KMS scanout, scale, rotation, colour convert, splash, OSD, hotplug |
| `src/AlsaAudio.cpp` | Planar float to stereo S16LE. One output |
| `src/IPCServer.cpp` | Unix socket JSON |
| `src/Config.cpp` | Reads `/etc` JSON. C++ writes alias init and per-output source/mode only |
| `finder/main.cpp` | NDI Find. Shares the NDI home with the decoder |
| `api/server.js` | Express, auth guard, route mounts |
| `api/auth.js` | Scrypt, sessions, login rate limit |
| `api/public/index.html` | The web UI |
| `api/routes/` | REST modules, including `Backup.js` and `Network.js` |
| `scripts/ndimon-priv.sh` | Root-only actions |
| `scripts/ndimon-net.py` | Wired DHCP/static helper, installed as `/usr/local/lib/ndimon/ndimon-net` |
| `scripts/ndimon-watchdog.sh` | Process liveness, and active-mode stall recovery |
| `systemd/` | Long-running units, the update oneshot, and boot rollback |
| `config/` | Templates copied to `/etc` only when missing |

---

## Limits

- There is no encoder. `/v1/NDIEncoder/*` returns 404.
- One output owns ALSA. The others are silent.
- Standard NDI frames are copied. At 1080p and 4K that is several megabytes per frame.
- HDR `P216` and `PA16` are not converted to SDR.
- Audio, tally, screensaver, and colour are stored on decoder 1 and applied as decoder-wide settings.
- Sessions do not survive an API restart.
- Wi-Fi, IPv6, VLANs, and more than one wired interface are not configured.
- dhcpcd (older Pi OS) can be read. It cannot be changed from this release.
- VAAPI has not been compile-tested in the laptop tree. Build it on the NUC.

---

## License

MIT — [LICENSE](LICENSE).

The NDI SDK is Vizrt’s, not MIT. Running `install.sh` means you accept the [NDI SDK License Agreement](https://www.ndi.tv/license).
