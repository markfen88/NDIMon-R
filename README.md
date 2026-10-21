# NDIMon-R 1.3.0

Dedicated **NDI receiver / HDMI decoder** for Linux appliances.

[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![Platform](https://img.shields.io/badge/arch-aarch64%20%7C%20x86--64-blue)](https://github.com/markfen88/NDIMon-R)

**1.3.0** adds installable plugins, experimental Tessera network audio,
per-output audio routing and delays, recoverable management/media VLAN
assignments, and refreshed output and splash controls. It retains backup/restore
and scheduled reboot from the 1.2 beta line. The installer writes
`1.3.0` to `/etc/ndimon-firmware-version`. See [the changelog](CHANGELOG.md) for changes
since the last documented 1.2 beta baseline and [the documentation index](docs/README.md)
for operator guides. Network audio remains experimental.

It takes a live NDI stream off the network, decodes it, and scans it out to HDMI or DisplayPort with DRM/KMS. Audio goes to ALSA. A web UI on port 80 is the day-to-day control surface. The decoder core is C++ because the path is memory-bandwidth limited on ARM boards.

This is a **decoder**, not an encoder. It does not publish NDI.

Official NDI documentation: [docs.ndi.video](https://docs.ndi.video/all/).

---

## Contents

- [Changes since 1.2 beta](CHANGELOG.md)
- [Documentation index](docs/README.md)
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
- [Plugins](#plugins)
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
| **NDI HX** (H.264 / H.265) | The Standard SDK decodes HX in software and hands the application uncompressed frames, where the SDK's codec runtime (libavcodec) is available. Hardware HX decode is not available; NDIMon-R does not use the Advanced SDK. |

Each connector gets its own DRM lease, NDI receiver and display thread. With
the Standard SDK, both Standard NDI and HX arrive as uncompressed frames and use
FrameSync. The retained compressed-frame decoder factory can fall back to
FFmpeg, but MPP/V4L2/VAAPI HX decoding is dormant on this SDK path.

---

## Requirements

- **OS:** Debian Bookworm/Trixie, Ubuntu 24.04, Armbian, or Raspberry Pi OS (64-bit).
- **CPU:** aarch64 or x86-64.
- **Display:** HDMI or DisplayPort. Do not run a desktop session that holds DRM master on that connector.
- **Network:** Avahi (`avahi-daemon`) for mDNS. An NDI Discovery Server is optional, for other subnets and for remote source control.
- **Privileges:** a full install is **root** (`sudo`). The decoder stays root (DRM). The API and finder run as user `ndimon`.
- **Build:** on the target. Do not cross-compile from Windows or macOS.

These are NDIMon's host requirements. Tessera plugin bundles have their own
OS/architecture requirements; the current Ubuntu ARM bundle is experimental.
See [Tessera setup](docs/plugins/tessera.md) before installing it.

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

1. **`scripts/setup-deps.sh`** — build tools, libdrm, ALSA, Avahi, FFmpeg headers, Node.js 20, `python3`, `python3-yaml`, `iputils-arping`, nftables, and the NDI SDK into `/usr/local`. Rockchip MPP on RK boards. VAAPI drivers on x86 (Intel iHD/i965 and Mesa for AMD).
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
5. Optional, on **Settings → Network**: define named untagged/tagged networks,
   choose management, and review/apply the draft. Select the shared NDI network
   on **NDI** and the audio network inside **Plugins → Tessera**. Keep the change
   explicitly at the new management address within 60 seconds once it is ready.
6. Optional, on **System**: scheduled reboot, watchdog mode, backup.
7. For network audio, install/enable the Tessera plugin, configure its device,
   flows and channel names, then open each **Output → Advanced → Audio** to
   choose inputs, destinations, gains and delays. Saving routes leaves the
   current audio engine selected; switching to experimental v2 requires a
   separate confirmation and restarts the decoder. See [the setup guide](docs/plugins/tessera.md).

Factory watchdog mode is **active** (reconnect after about 30 s of stall). An existing `/etc/ndimon-device-settings.json` is not rewritten on update. Change an older box from `passive` under **System → Watchdog Mode**.

---

## Features

### Receive and display

- Dual-mode NDI: Standard and SDK-decoded HX, both received through FrameSync.
- One source per connector. HDMI-A-1, HDMI-A-2, and DP-1 keep channels 1, 2, and 3. Further connectors take the lowest free channel, up to 8.
- Scale: letterbox, stretch, crop. Rotation follows `/modes.supported_rotations` (0; 180 on compatible planes).
- HDMI mode auto-matched to the source, or a locked resolution from the UI.
- Default audio engine v1 plays NDI audio through one ALSA owner.
- Opt-in v2 audio engine: multiple inputs and destinations per output, input
  gains, separate input/destination delays, shared physical sink mixing,
  drift compensation and memory-capped primary video delay. Routes can send
  the same mix to HDMI and Tessera together. The default remains v1 pending soak.
- Full-width output cards, Current Connections and Source Presets collapse
  independently. Each output's blue **Advanced** control starts collapsed.
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
- Named management, shared NDI and Tessera network assignments on one wired port,
  with untagged or 802.1Q VLANs, read-only **Used by** links, deletion guards,
  and a device-side 60-second Keep/Revert window once activation is ready.
- Installable plugin archives, feed updates, enable/disable/remove controls and
  plugin backup extensions. Tessera is optional; its runtime lives in its own repo.

### Discovery and control

- mDNS via Avahi, plus an optional NDI Discovery Server. A blank IP turns it off. There is no separate enable flag.
- Receiver advertiser so the box shows up as a destination. `allow_controlling` lets the SDK switch sources. The app does **not** call `connect()` from a routing callback.
- NDI groups are case-sensitive (`Production` and `production` are different). Empty becomes `public`.
- Extra IPv4 addresses for senders off the local subnet.
- Transport: RUDP by default (the NDI SDK’s own default and the one the NDI docs recommend). TCP, Multi-TCP, UDP and Multicast are selectable; each enables exactly one of the SDK's `rudp`/`tcp`/`unicast`/`multicast` receive keys (Multicast also keeps RUDP as its fallback, and needs a multicasting sender). A change rewrites `ndi-config.v1.json` and restarts the decoder service, because the SDK only reads transport settings at startup (a few seconds of outage; the saved source reconnects). A saved `TCP` that was never chosen in the UI (no `Rxpm_user_set` marker) is migrated to RUDP once at API start.
- Named source presets (up to 32). Recall connects immediately, without a rescan.
- Device name is the OS hostname (`hostnamectl`). It is not written into the SDK config as `machinename`. That override causes mDNS name clashes.

### Security

This is a LAN appliance, not a public service.

- Password is scrypt in `/etc/ndimon-auth.json`. Until that file exists the password is `ndimon`.
- Settings are atomically replaced by a root helper limited to named appliance files. The API and finder retain their unprivileged accounts. Config files are `root:ndimon`, mode `0640`.
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
    ├─ ndimon-finder  →  /var/lib/ndimon/ndimon-sources.json
    │
    └─ ndimon-r (DRM master)
           NDIReceiver → SDK-decoded frames (Standard or HX)
                        ├─ FrameSync → DRM (+ CPU conversion/native UYVY)
                        ├─ audio v1 → one ALSA output
                        └─ audio v2 → central graph → HDMI / Tessera PCM
                                                             │
                                                   optional tesserad
           IPC  /tmp/ndi-decoder.sock
                        │
           ndimon-api   Web UI + REST  (port 80)
           ndimon-watchdog  →  GET /api/health
```

| | Standard NDI | NDI HX |
|--|--------------|--------|
| Payload | SpeedHQ decoded inside the SDK to uncompressed frames | H.264/H.265 decoded inside the Standard SDK to uncompressed frames |
| Capture | FrameSync pull | FrameSync pull after SDK software decode |
| Timing | A/V sync, duplication/drop and silence insertion | Same uncompressed timing path |
| Picture | CPU conversion or supported native UYVY scanout | Same display path; allocating a hardware decoder does not make it active |
| Copy | Application-owned frame planes survive SDK receiver replacement | Same ownership rule |

### HX on the Standard SDK

The build links the Standard SDK and creates v3 receivers in `fastest` mode. On SDK 6.3.2, NDI Screen Capture HX arrives as uncompressed UYVY: the SDK decodes the H.264/H.265 itself, in software, on the appliance CPU. The old global `codec.*.passthrough` keys (which exist as strings in `libndi` 6.3.2) did not enable compressed receive on that SDK and are no longer written; whether any earlier SDK build honoured them is unverified.

Per-output `compressed_frame_observed` reports whether a compressed frame has
actually reached the application (it stays false on the Standard SDK). Use
`decode_backend` and `hw_decode` for the active backend. SDK-decoded UYVY does
not use the compressed-stream saturation detector. HX consumes CPU inside the
SDK, but the historical drop bursts did not establish a decode-speed ceiling;
inspect source continuity and receiver diagnostics when investigating drops.

### Decoder factory

`VideoDecoder::create()` reads `decode_mode` (`auto`, `hardware`, `software`) from device settings.

This table describes the retained compressed-frame path. It does not select
hardware HX decoding when the Standard SDK returns uncompressed frames.

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

DRM leases give each connector its own master. Queues hold two frames and drop the oldest. Idle or disconnected outputs draw a splash: a gradient or flat backdrop in the chosen colours, a status pill with a glowing dot, the source count, device name and URL, and the logo. Text is anti-aliased Inter (`/usr/local/share/ndimon/fonts`, SIL OFL); if the font files are missing the embedded 8x8 bitmap font is used. An OSD string can sit on the live picture in a translucent pill.

In the default v1 path, NDI planar float is downmixed to interleaved S16LE stereo
at 48 kHz. Layouts of 5.1 and above use an ITU-R BS.775 mix. The buffer is four
periods of 1024 frames. FrameSync requests samples from elapsed time so the
HDMI clock paces playback. Experimental v2 uses a central graph and its selected
master, with drift compensation for other clocks; see [per-output audio](#per-output-audio-experimental).

### Receive status and renderer limits

Per-output status includes `total_frames` and `dropped_frames` from the NDI SDK receive counters, `stream_type`, `rotation`, and the receiver's live `audio_enabled` state. Receive counters reset when a receiver is recreated; they do not measure display-side frame loss.

With v1, `audio_available` identifies the worker with the legacy ALSA writer.
With v2 it can report the shared engine attachment; `audio_enabled` reports the
receiver's current capture state. Use `/v1/Audio/status` and endpoint readiness
for the graph, rather than treating this legacy field as physical sink status.

An uncompressed SDK receive path reports `decode_backend: ndi-sdk` and `hw_decode: false`; allocating an unused hardware decoder does not count as active hardware decoding.
NDI packed YUV software conversion uses video range and the source-size matrix specified by the NDI SDK (BT.601 SD, BT.709 HD, BT.2020 UHD). Selection uses original dimensions before cropping/scaling. The native UYVY path is used only when the actual scanout plane already reports the required encoding and limited range; otherwise software conversion preserves colour correctness. This can reduce native acceleration on drivers whose plane colour defaults do not match. UHD matrix selection does not add HDMI gamut signalling or HDR tone mapping. NV12/HX metadata handling is unchanged by this fix.


Rotation depends on the renderer and selected DRM plane. The modes API exposes the angles this renderer supports, rather than the raw driver enum. Quarter turns (90/270) are unavailable until the renderer supports a swapped logical canvas. Rockchip Cluster planes and unidentified Rockchip planes expose only 0 degrees: their linear framebuffers cannot safely use the AFBC transform path, even when a BSP kernel accepts TEST_ONLY. Other planes may offer 180 degrees when the driver advertises the required rotation or reflection bits; TEST_ONLY can still reject a format/scaling combination. Unsupported requests return HTTP 400 and preserve the output; busy requests return HTTP 409 without applying later. Unsupported saved angles are reset to the effective angle on startup, reload and hotplug. Software rotation is not implemented. See the [Rockchip driver format/rotation check](https://github.com/rockchip-linux/kernel/blob/develop-6.1/drivers/gpu/drm/rockchip/rockchip_drm_vop2.c).


Source freshness follows incoming SDK frame-counter progress, so FrameSync duplicates cannot hide a frozen sender. In active watchdog mode a source that stays connected but sends no video for roughly 35 seconds is reconnected; paused/on-change sources may prefer passive or disabled watchdog mode. Receiver counter resets report zero FPS for the reset sample.

---

## Hardware and codecs

| Board | HX H.264 | HX H.265 | Standard NDI |
|-------|----------|----------|----------------|
| Radxa Rock 5B (RK3588) | SDK software | SDK software | SDK + NEON, or native UYVY |
| Radxa Rock 4C+ / 4B+ (RK3399) | SDK software | SDK software | SDK + NEON, or native UYVY |
| Raspberry Pi 4 / 5 | SDK software | SDK software | SDK + NEON |
| Other aarch64 | SDK software | SDK software | SDK + NEON |
| Intel / AMD / NVIDIA Linux | SDK software | SDK software | SDK + CPU conversion |

HX requires the SDK's compatible codec runtime. These describe the current
receive paths, not certification of every board/image. MPP, V4L2 and VAAPI
implementations remain in the source but are not active HX decoding with the
Standard SDK. Pi HEVC's stateless path and NVIDIA NVDEC are not implemented.
The x86 System page's install-time `vainfo` report does not prove that NDI is
using hardware decoding.

---

## Services

| Unit | User | Role |
|------|------|------|
| `ndimon-r` | root | Capture, decode, DRM, ALSA, IPC. `Type=notify`, `WatchdogSec=30`, `Nice=-10` |
| `ndimon-finder` | `ndimon` | Continuous NDI Find. Rewrites `/var/lib/ndimon/ndimon-sources.json` when the list changes |
| `ndimon-api` | `ndimon` | Express on port 80, session auth, SSE, reconnect loops |
| `ndimon-watchdog` | root | Every 15 s: process liveness, then the health policy |
| `ndimon-net-boot` | root | Oneshot. If a network change was still unconfirmed at boot, roll it back |
| `ndimon-update` | root | Oneshot, started by the Update button. Not left running |
| `tesserad` | `tessera` | Optional plugin runtime; network audio, flows and its virtual PTP media clock |

Finder and decoder share one NDI config file, `/var/lib/ndimon/.ndi/ndi-config.v1.json`, selected with `NDI_CONFIG_DIR` (set in both units and before SDK initialization). Pinning `HOME` is not enough. Both processes emit the same transport keys from `Rxpm`. Transport is read at SDK/process startup, so applying a transport change restarts the decoder service; recreating just its receiver is insufficient.

On RK3588 boards with the global `ondemand` governor interface, the root decoder service reduces `sampling_rate` to 10 ms while it runs. This shortens CPU demand sampling for bursty decoding work and affects **all CPU policies and processes**. The governor and frequency limits stay as configured. Stop/crash cleanup restores the saved interval when its applied value is still current; a shorter existing interval or a later operator change is preserved. Other boards, other governors and kernels exposing only per-policy tunables are skipped, with the reason recorded in the service journal.

To disable this tuning, run `sudo systemctl edit ndimon-r` and add:

```ini
[Service]
Environment=NDIMON_CPU_SAMPLING=off
```

Then restart `ndimon-r`. Before rolling back to older service units, stop the receiver with the current unit so its cleanup restores the interval. If the unit was already replaced, run `sudo /usr/bin/python3 -I /usr/local/lib/ndimon/ndimon-cpu-sampling restore`, or reboot.

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

Single page at `/` (`api/public/index.html`). Dark theme, Inter typeface served
from `/fonts/`. Sidebar pages are Dashboard, Outputs, NDI, Settings, System and
Plugins; phones use a bottom tab bar. Page hashes preserve navigation on reload.
Unsaved edits are marked, and live state comes from `EventSource /api/events`
about every 1.5 s. The page closes an old stream before reconnecting. Bootstrap
and icons are served locally from `/vendor/...`, with a CDN fallback.

| Chrome | Behaviour |
|--------|-----------|
| Login overlay | Password field. `POST /api/login` |
| Top bar | Device name, live count across outputs (or LIVE / IDLE with one output), sources, display mode, live-feed indicator, sign out |
| Password warning | Shown while the password is still `ndimon` |
| Saturated warning | HX decode FPS under 85% of the source for 3 s |
| Connection warning | The event stream dropped; values may be stale until it reconnects |

Warnings stack in the page flow under the top bar. Destructive actions (reboot, restart, delete, update) confirm in a modal.

### Dashboard

One tile per output (LIVE/IDLE, source, input and output resolution, frame rate, codec, HW/SW decode, SATURATED), a system card (CPU, memory, thermals, platform, uptime, audio, frame counters), **Rescan sources**, **Change source**, and the source list with a **Connect** button per source. With more than one output, a selector in the card header picks which output the button connects.

### Outputs

Current Connections is a compact full-width card with output summaries side by
side. Source Presets and every full-width output card collapse independently,
so one output can stay open while the others are hidden. Within an output,
**Advanced** is bright blue and starts collapsed after a page load; it contains
video settings and the audio route editor. Live status does not replace unsaved
audio drafts.

| Control | Options | API |
|---------|---------|-----|
| Current connections | Per-output LIVE/IDLE, codec, HW/SW, SATURATED | SSE `/api/events` |
| Presets | Save, recall, delete | `/v1/Presets/*` |
| Output name | Free text | `POST /v1/DeviceSettings/output-alias` |
| NDI source | None, discovered, or manual name and IP. **Connect** / **Disconnect** | `POST /v1/NDIDecode/connectTo` |
| Resolution | Auto, or a mode from the EDID list | `POST /v1/VideoOutput/resolution` |
| Scale | letterbox, stretch, crop | `POST /v1/VideoOutput/scaleMode` |
| Rotation | 0; 180 on compatible planes | `POST /v1/VideoOutput/rotation` |
| Audio inputs | Multiple NDI/Tessera endpoints, each with gain and delay | `GET /v1/Audio/endpoints`, `GET/PUT /v1/Audio/outputs/:ch` |
| Audio destinations | Multiple HDMI/Tessera endpoints, each with delay | Same output audio API |
| Output audio enabled | Off retains selections; no active contributions | Same output audio API |
| Audio engine | Explicit v1/v2 confirmation; decoder restarts | `GET/POST /v1/Audio/engine` |

### Per-output audio (experimental)

Open **Outputs → an output → Advanced → Audio**. Add any available NDI or
Tessera inputs and HDMI or Tessera destinations. Each input feeds every selected
destination: for example, NDI audio can feed both the TV's HDMI and a Tessera
network output. Tessera supplies reusable full-device and named mono endpoints;
configure their labels and network flows in its plugin. Two mono selections are
mixed together, rather than interpreted as a left/right pair.

Input gain is −60 to +12 dB (−60 mutes). Input and destination delays are whole
milliseconds from 0–5000; the sum for an active input/destination pair must fit
5000 ms. These are manual delays. There is no automatic HDMI/Dante latency
measurement or per-output video delay.

**Save output audio** affects that output and preserves other outputs. It does
not select v2 or restart the decoder. Explicitly choose **Use experimental audio
engine v2…** to activate saved routes; confirmation restarts the decoder and may
briefly interrupt video. Fresh installs default to v1. **Return to audio engine
v1…** restores the legacy path and retains saved choices. Disabling an output
retains its choices without contributing audio. Known offline endpoints stay
visible and are retried by the control side.

One physical destination has one writer and can mix different receiver sources.
Duplicate active contributions from the same physical source to the same sink,
and overlapping channel writers, are refused. If another browser has saved
audio, reload the saved output before retrying a revision conflict. Resource or
busy errors preserve the previous graph; an explicit `restart_required` error
may require returning to v1, saving, then selecting v2 again.

See [the API contract](docs/api/output-audio.md). Audio v2 remains experimental; sustained clock stability and external hardware interoperability are not certified.

### NDI

Device-wide receive settings.

| Control | Options | API |
|---------|---------|-----|
| Discovery Server IP | Blank = off | `POST /v1/NDIFinder/NDIDisServer` |
| NDI groups | Case-sensitive tags | `POST /v1/NDIFinder/NdiGrpName` |
| Off-subnet IPs | Comma-separated IPv4 | `POST /v1/NDIFinder/NdiOffSnSrc` |
| Device name | Receiver name. Also sets the hostname | `POST /v1/DeviceSettings/ndi-alias` |
| Transport | TCP, UDP, Multicast, M-TCP, RUDP | `POST /v1/NDIDecode/decodeTransport` |
| NDI network | One configured network shared by discovery and every receiver | Complete `/v1/Network/profiles/*` transaction |
| HX decode | auto, hardware, software | `POST /v1/DeviceSettings/decode-mode` |
| Tally | `TallyOff`, `TallyOn`, `VideoMode` | `POST /v1/NDIDecode/decodesetup` |
| Audio | `NDIAudioEn`, `NDIAudioDis` (applies on change) | same |

`Output` on connect and preset recall is **1–8**. VideoOutput query `output` is **0-based** (channel = index + 1).

Transport, HX decode preference and tally remain global. Independent transport
or NDI VLAN selection per output is deferred with receiver-process work. Per-output audio
routing is independent of those settings.

Screensaver and colour space are stored and sent in connect metadata. `CaptureSS` is stored; the core does not freeze a frame for it. `TallyMode` `VideoMode` is stored; program tally is applied when the mode is `TallyOn`.

### Settings

| Control | API |
|---------|-----|
| Change password (current, new, confirm, minimum 6 characters) | `POST /api/password` |
| Named wired networks: untagged/VLAN, DHCP/static, management assignment, read-only usage | `GET /v1/Network/profiles`, `POST /v1/Network/profiles/preview`, `/apply` |
| Splash colours (with one-click palettes), gradient or flat backdrop, logo position, text, visibility, with an in-page preview that follows the controls | `POST /v1/Splash/config` |
| Logo upload (PNG or JPEG) | `POST /v1/Splash/uploadLogo` |
| Splash preview | `POST /v1/Splash/preview` |
| OSD enable and text (max 128 characters) | `POST /v1/Splash/osd` |

Define networks here and select **Management / Control**. The NDI page and
Tessera plugin select their roles from the same network IDs. The table's
read-only **Used by** links show assignments. An assigned network cannot be
removed until a replacement is selected. A blank tag means the switch port's
untagged/native network; tags are 1–4094, with at most eight tagged networks on
one wired port. This lists appliance configuration, not discovered switch VLANs.

Edits are drafts until **Apply network settings**. Review the impact, then open
the displayed management address and click **Keep changes** within 60 seconds
once activation is ready. All applies use this protection, including media-role
changes. Page loads never confirm. Revert, expiry, failure or boot recovery
restores the prior settings; timers run independently of the browser/API.
Only management supplies gateway/DNS. Wi-Fi is unchanged. See
[network profiles and VLANs](docs/network-vlans.md) for switch, backend and recovery details.

### System

| Control | API |
|---------|-----|
| Device information | `GET /v1/AboutMe` and `/api/status` |
| GPU decode (x86) | `GET /v1/System/vaapi-info` |
| Check for updates / Update now | `GET /v1/System/version`, `POST /v1/System/update` |
| Backup and restore | `/v1/Backup/*` |
| NTP server (hostname or IP; blank keeps the OS default) | `GET` / `POST /v1/DeviceSettings/ntp` |
| Scheduled reboot | `GET` / `POST /v1/DeviceSettings/reboot-schedule` |
| Restart one service | `POST /api/services/:name/restart` |
| Restart decoder and finder | Restarts those two units. The API and watchdog stay up |
| Reboot | `POST /v1/System/reboot` |
| Watchdog mode and counters | `POST /v1/DeviceSettings/watchdog-mode`, `GET /api/health`, `GET /api/watchdog-stats` |

### Backup and restore

**System → Backup & Restore** downloads one JSON file. **Include network settings** is on by default. **Include admin password** is off. Turn the password on only when restoring this same device.

Restore shows where the file came from, then one row per section with a Changed or Unchanged badge. Unchanged sections start unticked. You choose what to import.

Network settings are mentioned when addressing, configured networks or role
assignments differ. A profile comparison includes every network and the
Management/NDI/Tessera assignments, even when the management IP stays the same.
The following table describes older backups containing only primary addressing.

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

Restored networks and role assignments use one protected profile transaction,
with 60 seconds to Keep once activation is ready. Open the management address
shown in the dialog and click **Keep changes**; its transaction token works
without the old login cookie. Revert, expiry or an unconfirmed reboot restores
the prior configuration. For DHCP the address appears after a lease is acquired;
if you lose contact before seeing it, let the device revert.

Audio graphs and enabled plugin backup extensions are included. Restore skips
missing plugins with a warning; it does not install their runtimes. Restoring
**Device** settings can also restore `audio_engine` and restart the decoder, so
review that selection before applying a backup from a v2 appliance.

Undo last restore puts settings back to the automatic snapshot taken just before the import, including a network change if that address differs.

---

## Plugins

Open **Plugins** to upload a trusted plugin archive or choose a release from a
configured feed. Enter the plugin name, choose its `.tar.gz` file, and select
**Upload and install**. Installation restarts the API briefly, then enables the plugin; its tab opens
on the Plugins page. Use **Disable**, **Update**, or **Remove**
to manage it. A failed removal keeps files for **Retry remove** or **Force remove**;
force removal skips the plugin's cleanup script. Normal removal keeps configuration.

The installer accepts repeatable options:

```bash
sudo bash install.sh --with-plugin hello=/path/to/hello-plugin_1.0.0-1_amd64.tar.gz
sudo bash install.sh --plugin-feed https://forge.example/team/plugins/index.json --with-plugin hello
```

`--with-plugin NAME=URL` also accepts an HTTP(S) archive URL. A bare name uses
configured `plugin_feeds` in `/etc/ndimon-device-settings.json`. Feed downloads
select the appliance architecture and check SHA256; an empty feed list shows
upload only. Install only publishers you trust: plugin scripts run as root.
Backups include enabled plugin settings; restore skips absent plugins with a
warning. The framework installs no tessera packages by itself.
See [the plugin authoring guide](docs/plugins.md) for manifests, feed formats,
privileged hooks, purge and API details.

For Tessera, use a matching plugin archive containing its precompiled bundle.
Installing/enabling the plugin exposes its page; selecting experimental network
audio is separate. Configure device/clock settings, static flows, reusable
channel names and the Tessera network there. Configure each output's audio mix
under **Outputs → Advanced → Audio**. [Tessera setup](docs/plugins/tessera.md)
describes compatibility, ownership and current limitations.

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
| GET | `/api/status` | UI status: stream, system, sources, decoder, discovery, `outputs[]` |
| GET | `/api/health` | IPC health. 503 if the decoder socket is down |
| GET | `/api/watchdog-stats` | Counters from `/tmp/ndimon-watchdog-stats.json` |
| GET | `/api/events` | SSE. First event `connected`, then `type: status` about every 1.5 s. At most 20 clients |
| GET | `/api/services` | systemd state of the four long-running units |
| POST | `/api/services/:name/restart` | `ndimon-r`, `ndimon-finder`, `ndimon-api`, or `ndimon-watchdog` |
| POST | `/api/net-pending` | `{ "token" }` — pending profile status without an old login cookie |
| POST | `/api/net-confirm` | `{ "token", "profiles": true }` for profile changes; explicit Keep through the new management socket address |
| POST | `/api/net-revert` | `{ "token" }` — revert that profile transaction without an old login cookie |

`/api/health` includes `alive`, `uptime_s`, and `outputs[]` with `output`, `connected`, `fps`, `health` (`idle`, `ok`, `degraded`, `stalled`), stale-age fields, and `stall_count`.

### `/v1/NDIDecode`

| Method | Path | Body | Effect |
|--------|------|------|--------|
| GET | `/decodestatus` | — | Reads `/etc/ndimon-dec1-status.json` |
| GET | `/decodesetup` | — | Reads `/etc/ndimon-dec1-settings.json` |
| POST | `/decodesetup` | `NDIAudio`, `ScreenSaverMode`, `TallyMode`, `ColorSpace`, `ChNum` | Writes allowed fields, then `reload_config` |
| GET | `/decodeTransport` | — | `{ Rxpm }` |
| POST | `/decodeTransport` | `{ "Rxpm" }` | `TCP`, `UDP`, `Multicast`, `M-TCP`, or `RUDP`. Restarts the decoder service so the SDK rereads transport |
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
| GET | `/modes?output=0` | — | `{ modes, current, auto_resolution, rotation, supported_rotations }` |
| GET | `/resolution?output=0` | — | Current mode |
| POST | `/resolution` | `{ "auto": true, "output" }` or `{ "width", "height", "refresh_hz", "output" }` | Auto-match, or lock a mode |
| POST | `/scaleMode` | `{ "scale_mode", "output" }` | `letterbox`, `stretch`, or `crop` |
| POST | `/rotation` | `{ "degrees", "output" }` | Use `supported_rotations` from `/modes`; unsupported angles return 400 |

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

### `/v1/Audio`

| Method | Path | Notes |
|--------|------|-------|
| GET | `/endpoints` | Endpoint catalog with labels, channel/rate metadata and readiness |
| GET, PUT | `/outputs/:ch` | Stable one-based output 1–8; enabled, inputs, destinations and global revision |
| GET, POST | `/engine` | Read engine; explicitly select `v1`/`v2` with `confirm:true`, restarting the decoder |
| GET, PUT | `/settings` | Full audio document; legacy projection is limited to the primary output |
| PATCH | `/sources/:id`, `/sinks/:id` | Legacy endpoint overlay preserving other outputs; ambiguous edits refused |
| GET | `/status` | Runtime master, period timing, endpoint readiness, xruns and drift compensation |

Output writes require the current revision; stale writes return 409
`revision_conflict`. Saving routes never switches the engine. See
[the detailed output-audio API](docs/api/output-audio.md) for request shapes,
delay limits, resource conflicts and legacy migration behavior.

### `/v1/Splash`

| Method | Path | Body | Effect |
|--------|------|------|--------|
| GET, POST | `/config` | colours, `backdrop` (`gradient` or `flat`), logo percentages, text, `show_*` | `logo_path` is only an `/etc/ndi-splash-logo.*` path, or empty. A stored `show_box` from older versions is ignored |
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

`sections` may be `outputs`, `presets`, `discovery`, `transport`, `display`,
`device`, `identity`, `auth`, `audio`, `plugins`. Network is selected separately
with `apply_network` and restored through a protected profile transaction.

A damaged checksum is refused. A newer `schema_version` is refused. Unknown keys are refused. Groups are not lowercased. Discovery Server is enabled only when the address is non-empty. The device name uses `hostnamectl`, never `machinename`. `ndi-config.v1.json` is not imported.

### `/v1/Network`

One physical wired port hosts named networks and role assignments. Wi-Fi and
switch trunk configuration are outside this API. The full profile document has
`version`, `revision`, `interface`, `backend`, `networks`, and network IDs for
`management`, `ndi`, `tessera`. See [the profile API](docs/network-vlans.md#api).

| Method | Path | Body | Effect |
|--------|------|------|--------|
| GET | `/profiles` | — | Committed profiles, live status, assignments and pending transaction |
| POST | `/profiles/preview` | Complete candidate document | Validate revision and impact without changing the OS |
| POST | `/profiles/apply` | Complete candidate document | Stage one recoverable change to networks and assignments |
| POST | `/profiles/rollback` | — | Revert the pending profile transaction |
| GET | `/status` | — | Compatibility status view: backend, configured/live addressing and pending information |
| POST | `/check` | `{ "address" }` | Duplicate-address probe (`arping`). Advisory |
| POST | `/apply` | `{ "mode", "address", "prefix", "gateway", "dns", "dry_run" }` | Compatibility wrapper for management addressing; non-dry-run applies use profile transactions |
| GET | `/vlans` | — | Compatibility VLAN status view |
| PUT, DELETE | `/vlans/:id` | — | Refused; use a complete profile transaction |

Before activation the helper journals previous settings and arms a 150-second
guard. The apply worker is limited to 90 seconds. Once required addresses and
guards are ready, a separate 60-second Keep window begins. Confirm using
`POST /api/net-confirm` with `{ "token": "...", "profiles": true }` through
the new management address. Status polls do not confirm. Use
`/profiles/rollback` or token-scoped `/api/net-revert` to revert this transaction;
the retained `/rollback` endpoint belongs to the older primary-network helper.

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
| Transport | `POST /v1/NDIDecode/decodeTransport` | Restart decoder service and reconnect saved source |
| Output audio | `PUT /v1/Audio/outputs/:ch` | Validate revision, compile and apply graph; retain selected engine |
| Audio engine | `POST /v1/Audio/engine` | Confirm and restart decoder |
| HX decode mode | `POST /v1/DeviceSettings/decode-mode` | `reload_config` |
| Discovery Server | `POST /v1/NDIFinder/NDIDisServer` | `reload_config` and restart the finder |
| Groups | `POST /v1/NDIFinder/NdiGrpName` | `reload_config` |
| Device name | `POST /v1/DeviceSettings/ndi-alias` | `reload_config` and `hostnamectl` |
| NTP | `POST /v1/DeviceSettings/ntp` | `ndimon-priv set-ntp` |
| Splash / OSD | `POST /v1/Splash/config` or `/osd` | `reload_config` |
| Splash preview | `POST /v1/Splash/preview` | `show_splash` |
| Watchdog mode | `POST /v1/DeviceSettings/watchdog-mode` | The watchdog reads the file |
| Scheduled reboot | `POST /v1/DeviceSettings/reboot-schedule` | `ndimon-priv set-reboot-schedule` |
| Network apply | `POST /v1/Network/profiles/apply` | Privileged profile transaction, then explicit `/api/net-confirm` |
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
| `device` | Watchdog mode, decode mode, audio engine, NTP, reboot schedule | device settings, those keys only |
| `identity` | `ndi_recv_name` and hostname | device settings and `hostnamectl` |
| `network` | Complete network profiles and Management/NDI/Tessera assignments, plus compatibility addressing fields | Applied together through protected network recovery; revision/interface/backend adapted to this device |
| `audio` | Audio settings/graph, gains, delays and output choices | `/etc/ndimon-audio-settings.json` |
| `plugins` | Enabled plugins' versioned backup extensions | Installed plugin handlers; absent plugins skipped with a warning |
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
| `ndimon-device-settings.json` | API, plus alias init in C++ | Alias, watchdog mode, NTP, decode/audio engine modes, reboot schedule, plugin feeds |
| `ndimon-rx-settings.json` | API | Transport `Rxpm` (default `RUDP`) and the `Rxpm_user_set` marker |
| `ndimon-find-settings.json` | API | Discovery Server IP. Non-empty means enabled |
| `ndi-group.json` | API | Groups, case-sensitive |
| `ndi-config.json` | API | Extra finder IPs, stored as a JSON string |
| `ndimon-presets.json` | API | Named presets |
| `ndimon-audio-settings.json` | API | Audio graph, output choices, gains and delays |
| `ndimon-plugins.json` | API | Plugin enabled/version state |
| `ndimon-auth.json` | API | Password hash, mode `0600` |
| `ndimon-splash-settings.json`, `ndimon-osd-settings.json` | API | Splash and OSD |
| `ndimon-sources.json` | Finder | Name-to-URL cache |
| `/var/lib/ndimon/.ndi/ndi-config.v1.json` | Both C++ processes | SDK home: groups, discovery and transport |
| `/var/lib/ndimon/backups/` | API | Pre-restore snapshots |
| `/var/lib/ndimon-network/` | Root network helper | Private profiles, journal and recovery state |
| `/etc/ndimon-network-interfaces.json` | Root network helper | Public interface mapping for the selected roles |
| Wired network | `ndimon-net` via `ndimon-priv` | Management/media networks and bindings; API stages complete transactions |
| Tessera flow/PCM files | Declared Tessera privileged hooks | Device flows and ALSA bridges; see the plugin guide |

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

Core updates and plugin updates are separate. Update a Tessera plugin with its
matching precompiled bundle through **Plugins**; a change in the Tessera source
repository does not automatically replace the installed daemon or archive.
The current host manifest checks `min_ndimon`; it does not yet show a tested-build
compatibility matrix. See [release/upgrade notes](CHANGELOG.md#upgrade-notes).
Network-profile upgrades also require the new nftables dependency; run the
updated dependency setup if the host does not already have it.

---

## Troubleshooting

**No picture after connect.** `sudo journalctl -u ndimon-r -f`. Confirm a desktop does not own DRM. Standard NDI and HX H.264/H.265 are the supported payloads. HDR `P216` / `PA16` is logged and not converted.

**Empty source list.** `systemctl is-active ndimon-finder`. The Discovery Server IP must be reachable. Senders on another subnet need that server or extra IPs under **NDI**. Groups are case-sensitive.

**HX is black.** The Standard SDK decodes HX inside the SDK and needs its codec runtime (`libavcodec.so.61` on the tested builds). Check `journalctl -u ndimon-r` for the first-frame line and confirm the runtime is installed. Hardware HX decode is not available.

**Web UI down.** `systemctl status ndimon-api`. To use another port, set `Environment=PORT=8080` on the unit. Loopback health: `curl -s http://127.0.0.1/api/health`.

**Rockchip MPP missing.** `ldconfig -p | grep librockchip_mpp`, then `sudo bash scripts/setup-deps.sh` on an image that ships MPP.

**NDI library missing.** `ldconfig -p | grep libndi`, then `sudo bash scripts/setup-deps.sh`.

**Source jumps.** The saved source is in `ndimon-dec{N}-settings.json`. A Discovery Server with controlling enabled can change it. That is the SDK, not a reconnect bug. A local connect is protected for 30 seconds.

**Network change did not stick.** Open the displayed management address directly
and click Keep within the ready-state 60-second window. Proxy headers and
opening the page alone do not confirm. An unconfirmed reboot or failed apply
restores the old settings. dhcpcd is read-only; check
[supported backends and adoption limits](docs/network-vlans.md#operating-system-integration).

**Lost the confirm page.** Reopen the pending transaction's management link
before its countdown ends. If the new address is unknown or unreachable, leave
it unconfirmed and let the device restore the previous configuration. Opening
the link alone never keeps the change.

**Saved audio is silent.** Check the selected engine and endpoint readiness in
**Outputs → Advanced → Audio**. Saving choices leaves v1 active until you
explicitly select v2. A disabled output retains its routes without playing them.
Check Tessera flows, 48 kHz agreement and the selected audio network; review
`GET /v1/Audio/status` for endpoint errors/xruns. Returning to v1 restores the
legacy path. Persistent drift pinned at its limit needs investigation.

**Plugin settings are missing.** Installing/enabling the plugin exposes its
tab on Plugins; network-audio activation is a second action. A core-only update
does not install Tessera. Check the archive's OS/architecture and `min_ndimon`.

---

## Layout

| Path | Role |
|------|------|
| `src/main.cpp` | Process entry, one worker per connector, health tick, systemd notify, IPC wiring |
| `src/NDIReceiver.cpp` | SDK receive: advertiser, connect, FrameSync vs retained compressed capture, tally |
| `src/VideoDecoder.cpp` | Factory: MPP, V4L2, VAAPI, FFmpeg, honoring `decode_mode` |
| `src/MppDecoder.cpp` | Rockchip H.264/H.265 to NV12 DMA-BUF |
| `src/V4L2Decoder.cpp` | Pi 4 M2M H.264 |
| `src/VAAPIDecoder.cpp` | Intel/AMD VAAPI, DRM PRIME modifiers |
| `src/SoftwareDecoder.cpp` | FFmpeg NV12 |
| `src/DRMDisplay.cpp` | KMS scanout, scale, rotation, colour convert, splash, OSD, hotplug |
| `src/AlsaAudio.cpp` | Planar float to stereo S16LE. One output |
| `src/audio/` | Optional central v2 audio graph, route delays, ALSA endpoints and drift compensation |
| `src/IPCServer.cpp` | Unix socket JSON |
| `src/Config.cpp` | Reads `/etc` JSON. C++ writes alias init and per-output source/mode only |
| `finder/main.cpp` | NDI Find. Shares the NDI home with the decoder |
| `api/server.js` | Express, auth guard, route mounts |
| `api/auth.js` | Scrypt, sessions, login rate limit |
| `api/public/index.html` | The web UI |
| `api/routes/` | REST modules, including `Backup.js` and `Network.js` |
| `api/audio-settings.js`, `api/network-profiles.js` | Revision-guarded audio graphs and typed network transactions |
| `api/plugins.js`, `plugins/tessera/` | Host plugin loader/lifecycle and Tessera adapter/UI |
| `scripts/ndimon-priv.sh` | Root-only actions |
| `scripts/ndimon-net.py` | Wired DHCP/static helper, installed as `/usr/local/lib/ndimon/ndimon-net` |
| `scripts/ndimon_network_profiles.py` | Management/media profile activation, isolation and recovery |
| `scripts/ndimon-watchdog.sh` | Process liveness, and active-mode stall recovery |
| `systemd/` | Long-running units, the update oneshot, and boot rollback |
| `config/` | Templates copied to `/etc` only when missing |

---

## Limits

- There is no encoder. `/v1/NDIEncoder/*` returns 404.
- With default engine v1, one output owns ALSA. Experimental v2 adds the
  independently configured audio routes described above.
- Standard NDI frames are copied. At 1080p and 4K that is several megabytes per frame.
- HDR `P216` and `PA16` are not converted to SDR.
- Tally, screensaver, colour and the legacy NDI-audio switch remain decoder-wide.
  V2 audio choices are per output. Transport, HX decode preference and the NDI
  network assignment remain global; separate receiver processes are deferred.
- Sessions do not survive an API restart.
- VLANs are supported on one physical wired port. Wi-Fi, IPv6 configuration and
  multiple physical port management remain outside this feature.
- dhcpcd (older Pi OS) can be read. It cannot be changed from this release.
- The Standard SDK uses software HX decoding even when VAAPI drivers are installed.
- Audio v2 and the current Ubuntu ARM Tessera bundle remain experimental. Sustained clock stability, external hardware interoperability, HDMI/Dante delay
  matching and hardware PTP support are not certified. No automatic lip-sync is provided.
- Bluetooth speaker routing/latency calibration and HDMI ARC/eARC capture are
  planned research, with no implementation in this release.
- This integration configures static flows and exposes no automatic
  vendor-controller subscription setup. Use the established controller workflow
  where supported; ordinary Dante Controller discovery/routing interoperability
  was not established by the local PCM checks.

---

## License

MIT — [LICENSE](LICENSE).

The NDI SDK is Vizrt’s, not MIT. Running `install.sh` means you accept the [NDI SDK License Agreement](https://www.ndi.tv/license).
