# NDIMon-R 1.4.0

Dedicated **NDI receiver / HDMI decoder** for Linux appliances.

[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![Platform](https://img.shields.io/badge/arch-aarch64%20%7C%20x86--64-blue)](https://github.com/markfen88/NDIMon-R)

**1.4.0** replaces everything that ran beside the decoder with one Rust control
plane. The Node.js REST API, the shell and Python helpers, the installer scripts,
the standalone watchdog and the sudo rule are gone. `ndimon-control` serves the
web UI and the API on port 80 and keeps the whole configuration as one versioned
document; `ndimon-privd` is the only root helper and accepts a closed set of typed
requests. The decoder and the NDI finder stay C++. The release also adds a
native network engine with recoverable changes, out-of-process plugins, signed
updates, native backup and restore, and a TypeScript web UI that updates live.
It installs as four Debian packages, and a 1.3.x appliance upgrades in place.
Read [Upgrading from 1.3.x](#upgrading-from-13x) before you upgrade. The packages
write `1.4.0-1` to `/etc/ndimon-firmware-version`. See [the changelog](CHANGELOG.md)
for the release notes and [the documentation index](docs/README.md) for operator
and developer guides. Network audio remains experimental.

It takes a live NDI stream off the network, decodes it, and scans it out to HDMI or DisplayPort with DRM/KMS. Audio goes to ALSA. A web UI on port 80 is the day-to-day control surface. The decoder core is C++ because the path is memory-bandwidth limited on ARM boards.

This is a **decoder**, not an encoder. It does not publish NDI.

Official NDI documentation: [docs.ndi.video](https://docs.ndi.video/all/).

---

## Contents

- [Changelog and release notes](CHANGELOG.md)
- [Documentation index](docs/README.md)
- [What it does](#what-it-does)
- [Requirements](#requirements)
- [Install](#install)
- [Upgrading from 1.3.x](#upgrading-from-13x)
- [First boot](#first-boot)
- [Features](#features)
- [Architecture](#architecture)
- [Receive path](#receive-path)
- [Hardware and codecs](#hardware-and-codecs)
- [Services](#services)
- [Security](#security)
- [Recovery](#recovery)
- [Web UI](#web-ui)
- [Network](#network)
- [Plugins](#plugins)
- [Backup and restore](#backup-and-restore)
- [Updates](#updates)
- [HTTP API](#http-api)
- [Internal IPC](#internal-ipc)
- [Files and paths](#files-and-paths)
- [Troubleshooting](#troubleshooting)
- [Source layout](#source-layout)
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

- **OS:** Debian 13, Ubuntu 24.04 or 26.04, Armbian, or Raspberry Pi OS (64-bit), with systemd 244 or later. The packages were installed and exercised on Debian 13 and Ubuntu 26.04, and on an Armbian noble (Ubuntu 24.04) Radxa ROCK 5B; the Network page works with Armbian's stock Netplan file and with Raspberry Pi OS's Netplan-generated NetworkManager profile (see [network VLANs](docs/network-vlans.md#operating-system-integration)).
- **CPU:** aarch64 or x86-64.
- **Display:** HDMI or DisplayPort. Do not run a desktop session that holds DRM master on that connector.
- **Network:** Avahi (`avahi-daemon`) for mDNS. An NDI Discovery Server is optional, for other subnets and for remote source control. Network changes also need `nftables`, `iproute2` and `iputils-arping`, which the packages depend on.
- **Privileges:** a full install is **root** (`sudo`). The decoder stays root (DRM). `ndimon-control`, the finder and the plugin processes run as their own unprivileged users.
- **Build:** on the target, or on a machine of the same distribution and architecture. Do not cross-compile from Windows or macOS. Building needs Rust stable (edition 2024, 1.87 or later), a C++17 compiler and CMake. `zlib1g-dev` is optional (the Rust build compiles its bundled zlib without it).
- **No Node.js, Python or shell** runs on the device, and no package depends on them.

These are NDIMon's host requirements. Tessera network audio needs **Tessera 0.3.0 or later**, which has its own OS/architecture requirements.
See [Tessera setup](docs/plugins/tessera.md) before installing it.

Installing accepts the [NDI SDK License Agreement](https://www.ndi.tv/license). The SDK tarball (~60 MB) is downloaded from NDI on the first `cargo xtask setup-deps`.

**Ubuntu 24.04 (noble) and Armbian noble.** The build compiles against noble's own
FFmpeg 6.1 development packages and `ndimon-runtime` depends on noble's runtime
libraries (`libavcodec60`, `libavutil58`, `libswresample4`). Only the NDI SDK's own
HX decoder needs FFmpeg 7's `libavcodec.so.61`, which noble does not ship;
`cargo xtask setup-deps` installs just `libavcodec61` and `libavutil59` from Ubuntu's
plucky archive through a temporary source pinned below the default priority, so no
package noble provides (above all the `-dev` packages) is replaced, then removes the
source. If that fails, the build and everything but HX decoding still work. After an
interrupted run, `setup-deps` removes a leftover source before it installs anything.
On a board that carries the earlier installer's plucky `-dev` packages, reinstall noble's:
`sudo apt-get install --allow-downgrades libavcodec-dev libavformat-dev libswscale-dev libavutil-dev libswresample-dev`.
If `apt-get update` refuses a changed repository (the Radxa noble repository changed
its Origin and Label), accept it once with `sudo apt-get update --allow-releaseinfo-change`.

---

## Install

Build on the appliance (Debian/Ubuntu, ARM or x86-64) with Rust stable
(edition 2024, Rust 1.87 or later). Install Rust through your normal trusted
provisioning process, then run:

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config git
git clone https://github.com/markfen88/NDIMon-R.git
cd NDIMon-R/control
RUSTFLAGS="-D warnings" cargo xtask install
```

The Rust installer installs dependencies, downloads the licensed NDI SDK v6,
builds Rust and C++, builds four Debian packages, and installs them with apt. It
enables the Rust control plane on port 80 and its typed root daemon; no sudoers
rule or Python/shell helper is installed. No Node.js is installed, run or needed
on the device, even when it builds its own packages: the browser bundle is the
committed one, and the pinned Bootstrap and Bootstrap Icons files are fetched
from the npm registry tarballs named in `api/package-lock.json`, verified against
their sha512 `integrity` and unpacked by the installer itself. Node is a
developer and CI tool only, for rebuilding the browser bundle. Open `http://<device-ip>/` when installation ends.

| Command (from `control/`) | Purpose |
|---|---|
| `cargo xtask install` | Dependencies, build, package and install on this appliance |
| `cargo xtask install --no-deps` | Rebuild and install with existing dependencies |
| `cargo xtask install --no-deps --no-build` | Install already built packages/assets |
| `cargo xtask setup-deps` | Provision build/runtime dependencies and the NDI SDK |
| `cargo xtask build-deb --version 1.4.0-1` | Build the complete package set in `dist/debian/` |

Optional download pin: `NDI_SDK_SHA256=<64 hex digits> cargo xtask install`.
New SDK installs record the exact version and archive hash in
`/etc/ndimon-ndi-version` and `/etc/ndimon-ndi-sha256`. Existing v6 installs are
retained and their version marker is repaired. Rockchip MPP, Pi V4L2 and x86
VAAPI/NVIDIA detection preserve their software fallbacks. Pass CMake options
after `--`, for example
`cargo xtask build-deb -- -DENABLE_MPP=OFF`.

For a prebuilt release, provision the SDK/media dependencies with
`cargo xtask setup-deps`, verify its signature and checksums as described in
[releases](docs/releases.md), then install the complete package set:

```sh
sudo apt install ./ndimon-*_1.4.0-1_amd64.deb
sudo systemctl status ndimon-control ndimon-privd.socket ndimon-finder ndimon-r
sudo journalctl -u ndimon-control -u ndimon-privd -f
```

The four packages are `ndimon-control`, `ndimon-privd`, `ndimon-runtime` and
`ndimon-ui`; install all of them at the same version. Use the package
architecture and version matching your appliance. The packages depend on each
other at an exact version, and `ndimon-runtime` carries the C++ decoder with
distribution-specific library dependencies, so build or download the one made for
your distribution release (see [releases](docs/releases.md)). Users and groups
come from `sysusers.d`, shared directories and permissions from `tmpfiles.d`. The
package hooks are native Rust executables, not shell scripts.

For opt-in packet and CPU diagnostics, name the sender and receiving interface:

```text
sudo ndimonctl diag collect --out /tmp/ndimon-diag --secs 60 --peer 192.0.2.20 --nic end0
ndimonctl diag analyze /tmp/ndimon-diag 2
```

Collection writes `sys.csv` and `pkt.csv`. Analysis also needs the matching
channel telemetry at `tele/t-ch2.csv` under the same directory. `--proc UNIT`
selects another systemd service to sample (default `ndimon-r`). `ndimonctl
status` prints the state of the NDIMon-R services and the loopback health reply.

---

## Upgrading from 1.3.x

1.4.0 installs over a 1.3.x appliance. The first start imports the settings,
the password, the network configuration and the Tessera configuration, then
retires the 1.3.x services and helpers. Plan for a short outage: the decoder, the
finder and the web UI restart.

**Before you upgrade**

1. Export a backup (**System → Backup & Restore**) and keep console or SSH
   access to the device.
2. Resolve any pending network change with **Keep changes** or **Revert**.
3. Get the four 1.4.0 packages (built for your distribution release and
   architecture) or a 1.4.0 checkout, and install them as described under
   [Install](#install). The 1.3.x **Update now** button cannot perform this
   upgrade: it updates from a source checkout, which 1.4.0 no longer does.

**What the upgrade imports** (once, with every original kept as
`<file>.pre-control`)

- Output, device, NDI discovery, transport, display (splash, OSD, logo), preset
  and audio settings, the plugin registry and the output aliases.
- The administrator password. The hash stays valid; it is replaced by Argon2id
  at the next successful login, and backups keep both formats.
- The effective network configuration: the 1.3.x profiles, VLAN index and base
  transaction are adopted by the native engine. A change that was still pending
  is rolled back first. The private network journals are also copied to
  `*.pre-p8` for a rollback.
- Tessera's effective device settings (name, latency, channel counts and labels),
  its static flows and clock role, read from `/etc/tessera/config.d` and its
  runtime state. A Tessera 0.1.0 device that never had a name takes the host
  name, which is Tessera's own default.

The import does not repair what it cannot read, and it does not leave the device
down for it. The old services are already stopped when the import runs, so a
setting group whose legacy files cannot be imported (a malformed file, or values
that do not validate) is left at its defaults and reported, and everything else
is imported: the package output ends with `state migration: NOT IMPORTED, left at
its defaults: <group>: <reason>`, and the original files stay as `<file>.pre-control`
for you to copy back through the UI. A legacy network store the engine will not
take over (for example a lock file with an unexpected owner or mode) is reported
in the journal of `ndimon-net-boot`; the device keeps its own network
configuration, network changes stay locked, and the next boot tries again. Steps
of the package configuration that only tidy up (old group memberships, recovery
copies, stale links) are reported at the end and do not stop it.

**What the upgrade retires**

- The Node.js API (`ndimon-api`, its sockets, drop-ins and the installed `api`
  tree), the standalone `ndimon-watchdog` service, the `ndimon-update` unit and
  its runner, `install.sh` and the `scripts/` helpers, the sudo rule
  `/etc/sudoers.d/ndimon`, the Python network engine and its selector
  `/etc/ndimon-network-engine`, and the privileged shell helpers.
- Manual units in `/etc/systemd/system` that shadow the packaged ones (kept as
  `*.pre-p8`), the port-80 capability that the old installer gave the `node`
  binary, and the `ndimon` account's membership of the old privileged group.
- Watchdog behaviour continues inside `ndimon-control` and still follows your
  `watchdog_mode`.

**What you have to do**

| Item | Action |
|---|---|
| JavaScript plugins (manifest v1) | They stay installed with their files and data but do not run: the page shows **needs an update to the out-of-process format** and they cannot be enabled. Install the plugin's manifest v2 release over it, or remove it. See [Plugins](#plugins). |
| Tessera | 1.4.0 needs **Tessera 0.3.0 or later**. Install the matching plugin archive from Plugins after the core upgrade. A 0.2.x runtime that is already installed keeps running, but NDIMon-R cannot lock its flows against controller writes (it reports an ownership conflict), and the 0.2.x bundles can no longer be installed. |
| Node.js | The upgrade does not remove the `nodejs` and `npm` packages. Nothing on the device uses them any more; remove them with `apt remove nodejs npm` if you installed them only for NDIMon-R. |
| Names other than the device's own | The web UI answers only to the device's IP addresses, `localhost`, its hostname, `hostname.local` and `hostname.<search domain>`. A request under any other name (a DNS alias, a proxy name) is refused with HTTP 421 `misdirected_host`. Open the UI by IP address once and add the name under **Settings → Security → Additional host names**. |
| Network changes | Nothing to do: the packages list all four backends as validated (a conffile of `ndimon-privd`), so VLAN and address changes work as they did in 1.3.x. To withdraw a backend you have not validated on your hardware, edit that file. See [Network](#network). |
| Release keys | No release key ships with 1.4.0. Without one the device installs updates only by hand (packages or `cargo xtask install`); with one, **System → Update now** finds and installs the newest signed release. See [Updates](#updates). |
| Sessions and scripts | Sessions end at the upgrade; sign in again. Scripts that log in with `POST /api/login` and send `Authorization: Bearer` keep working; scripts that use the session cookie must also send `X-CSRF-Token` on every change. The cookie is `SameSite=Strict`. |

Backups made by 1.3.x restore into 1.4.0, including the **Undo last restore**
snapshots. If the upgrade fails, see [Rollback](#rollback).

---

## First boot

1. Browse to `http://<device-ip>/`.
2. Log in. The default password is **`ndimon`**. Change it under **Settings → Security** before the box is on a real network. The UI keeps a warning up until you do.
3. Open **NDI** and pick a source. That choice is saved in the device configuration and survives reboot.
4. Optional, on **NDI**: Discovery Server IP, receiver advertising and control, groups, extra IPs, transport, and HX decode mode. Optional, on **System**: NTP host.
5. Optional, on **Settings → Network**: define named untagged/tagged networks,
   choose management, and review/apply the draft. Select the shared NDI network
   on **NDI** and the audio network inside **Plugins → Tessera**. Keep the change
   explicitly within 60 seconds once it is ready. See [Network](#network).
6. Optional, on **Settings → Security**: add the other names you open the UI by.
7. Optional, on **System**: scheduled reboot, watchdog mode, backup.
8. For network audio, install/enable the Tessera plugin, configure its device,
   flows and channel names, then open each **Output → Advanced → Audio** to
   choose inputs, destinations, gains and delays. Saving an output's routes
   selects the experimental v2 engine in the same change; while v1 is running the
   device first shows that this restarts the decoder, and you confirm it. See
   [the setup guide](docs/plugins/tessera.md).

Factory watchdog mode is **active** (reconnect after about 30 s of stall). An existing device configuration keeps its mode across updates. Change an older box from `passive` under **System → Watchdog Mode**.

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
  the same mix to HDMI and Tessera together. The default remains v1.
- Full-width output cards, Current Connections and Source Presets collapse
  independently. Each output's blue **Advanced** control starts collapsed.
- Custom splash and OSD when idle or disconnected.
- Monitor hotplug: the receiver still advertises if no display is present at boot. The picture starts when HDMI lands.
- Live address on the splash, refreshed about every 2 s. A short banner appears while a network change is waiting to be confirmed. The confirmation token is not drawn on HDMI.

### Appliance behaviour

- Last source auto-connects after reboot.
- Disconnect keeps the saved source so it can reconnect. Forget source is an explicit UI action (`None`).
- The control supervisor reconnects with exponential backoff (5 s → 30 s). Odd tries use the saved IP; even tries use the name only.
- The supervisor reads decoder health over IPC. **Active** watchdog mode disconnects a stalled output so the saved source reconnects. It does not restart the decoder process; systemd does that.
- `ndimon-r` is `Type=notify` with `WatchdogSec=30` and `Restart=always`, and so is `ndimon-control`.
- Optional NTP host writes a timesyncd or chrony drop-in and is reapplied when the control plane starts. NDI has no time server of its own.
- Optional scheduled reboot (System page): local time and weekdays, as a systemd timer with `Persistent=false`. A missed window does not fire on the next boot.
- Backup and restore of user settings, with a one-step undo.
- Named management, shared NDI and Tessera network assignments on one wired port,
  with untagged or 802.1Q VLANs, read-only **Used by** links, deletion guards,
  and a device-side 60-second Keep/Revert window once activation is ready.
- Installable out-of-process plugins with feed updates, enable/disable/remove
  controls and backup of their state. Tessera is optional; its runtime lives in its own repo.
- Signed-only updates, applied as the same package set that a manual install uses; started through the API with an explicit version.

### Discovery and control

- mDNS via Avahi, plus an optional NDI Discovery Server. A blank IP turns it off. There is no separate enable flag. Changing the address restarts the decoder (video and audio pause briefly; the page asks first): the NDI SDK only reads it when the decoder starts.
- With a Discovery Server set, **Allow Receiver Advertising** (on by default) lists each output as a receiver, with its connection state, source and stream details. **Allow Receiver Control** lets a Discovery Server controller (for example NDI Tools Discovery 6.2 or later) choose an output's source. It is off on new installs and stays on after an upgrade from 1.3.x. A routed source is saved like a local pick; whichever comes last wins, and the output page shows "set by Discovery Server". NDI control has no authentication: anyone who can reach the Discovery Server can route the outputs.
- Receiver advertiser so the box shows up as a destination, retried until the Discovery Server answers. With control allowed the SDK switches the source itself; the app saves the new source and does **not** call `connect()` for it.
- NDI groups are case-sensitive (`Production` and `production` are different). Empty becomes `public`.
- Extra IPv4 addresses for senders off the local subnet.
- Transport: RUDP by default (the NDI SDK’s own default and the one the NDI docs recommend). TCP, Multi-TCP, UDP and Multicast are selectable; each enables exactly one of the SDK's `rudp`/`tcp`/`unicast`/`multicast` receive keys (Multicast also keeps RUDP as its fallback, and needs a multicasting sender). A change rewrites the SDK configuration and restarts the decoder service, because the SDK only reads transport settings at startup (a few seconds of outage; the saved source reconnects). A saved `TCP` that was never chosen in the UI is migrated to RUDP once, when the 1.3.x settings are imported.
- Named source presets (up to 32). Recall connects immediately, without a rescan.
- Device name is the OS hostname (`hostnamectl`). It is not written into the SDK config as `machinename`. That override causes mDNS name clashes.

---

## Architecture

```
 browser (TypeScript UI)        ndimonctl / scripts        plugin processes
        │  HTTP + SSE                 │                          │ framed socket
        ▼                             ▼                          ▼
 ┌────────────────────────── ndimon-control (Rust, user ndimon-control) ────────────────────────┐
 │ sessions, CSRF, Host allowlist, security headers · /api/v1 · legacy /v1 routes · SSE events   │
 │ state actor: validate → plan impact → confirm → apply → persist (one document, one revision)  │
 │ supervisor: source recovery, watchdog modes · observers · replay after restarts               │
 └──────┬───────────────────────┬──────────────────────────┬───────────────────────────┬───────┘
        │ JSON-lines IPC        │ control.sock             │ typed requests            │ systemctl
        ▼                       ▼                          ▼                           ▼
  ndimon-r, ndimon-finder    tesserad                 ndimon-privd (root)         systemd
  (C++ decoder and finder)   (optional)               renders artifacts, runs the
                                                      network engine, plugin jobs,
                                                      hostname, NTP, updates
```

- **One document.** All configuration is one desired-state document at
  `/var/lib/ndimon/state/state.json` with a single revision, owned by the state
  actor in `ndimon-control`. Nothing else writes it. Every change from the UI, a
  script, a plugin or a restore is a typed **command** that is validated as a
  whole, planned (what restarts, what is interrupted), confirmed when it is
  disruptive, applied and saved. A failed apply rolls back to the previous
  document and artifacts.
- **Generated files.** The files that the decoder, the finder, Tessera and the
  OS read (the `/etc/ndimon-*.json` family, Tessera's `config.d` files, the
  ALSA PCM files, network and service drop-ins) are rendered from the document
  by `ndimon-privd`. Do not edit them: an edit is detected as drift and replaced
  at the next write. The decoder and the finder also receive the same
  configuration over their IPC sockets (`apply_config`), and the control plane
  replays it after either restarts.
- **Privilege boundary.** `ndimon-privd` is socket-activated, root, and exits
  when idle. Its requests are a closed set of typed values: no path, shell text
  or argument list comes from a caller. Only `ndimon-control` can connect.
- **Live UI.** The UI keeps one store of state and observations fed by a single
  server-sent event stream, so open pages update themselves. A save is made
  against the revision the page loaded; a stale save gets a conflict and offers
  a reload.
- **Languages.** Rust for everything that runs on the device except the C++
  decoder and finder; TypeScript only in the browser. The repository holds no
  Python and no shell.

---

## Receive path

```
NDI network
    │
    ├─ ndimon-finder  →  /run/ndimon/finder/ndimon-sources.json
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
       ndimon-control   Web UI + REST  (port 80)
           supervisor  →  decoder IPC health/events
```

| | Standard NDI | NDI HX |
|--|--------------|--------|
| Payload | SpeedHQ decoded inside the SDK to uncompressed frames | H.264/H.265 decoded inside the Standard SDK to uncompressed frames |
| Capture | FrameSync pull | FrameSync pull after SDK software decode |
| Timing | A/V sync, duplication/drop and silence insertion | Same uncompressed timing path |
| Picture | CPU conversion or supported native UYVY scanout | Same display path; allocating a hardware decoder does not make it active |
| Copy | Application-owned frame planes survive SDK receiver replacement | Same ownership rule |

### HX on the Standard SDK

The build links the Standard SDK and creates v3 receivers in `fastest` mode. On the tested SDK 6.3.2, NDI Screen Capture HX arrives as uncompressed UYVY: the SDK decodes the H.264/H.265 itself, in software, on the appliance CPU. The old global `codec.*.passthrough` keys (which exist as strings in `libndi` 6.3.2) did not enable compressed receive in our test and are no longer written; whether any earlier SDK build honoured them is unverified.

Per-output `compressed_frame_observed` reports whether a compressed frame has
actually reached the application (it stays false on the Standard SDK). Use
`decode_backend` and `hw_decode` for the active backend. SDK-decoded UYVY does
not use the compressed-stream saturation detector. HX consumes CPU inside the
SDK, but the historical drop bursts did not establish a decode-speed ceiling.

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

DRM leases give each connector its own master. Queues hold two frames and drop the oldest. Idle or disconnected outputs draw a splash: a gradient or flat backdrop in the chosen colours, a status pill with a glowing dot, the source count, device name and URL, and the logo. Text is anti-aliased Inter (`/usr/share/ndimon/fonts`, SIL OFL); if the font files are missing the embedded 8x8 bitmap font is used. An OSD string can sit on the live picture in a translucent pill.

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
| `ndimon-finder` | `ndimon-finder` | Continuous NDI Find, isolated from the other accounts and from privd. Writes the source list below `/run/ndimon/finder` when it changes |
| `ndimon-control` (+ `.socket`) | `ndimon-control` | Rust API, authentication, state actor, supervisor and the port 80 socket-activated listeners (IPv4 and IPv6). `Type=notify`, `WatchdogSec=30`, `NoNewPrivileges` |
| `ndimon-privd` (+ `.socket`) | root | Typed privileged operations, native network engine, plugin and update jobs. Socket-activated; exits after 60 idle seconds; only `ndimon-control` may connect |
| `ndimon-state-import` | root | Oneshot before the control plane. On the first start after an upgrade it imports the 1.3.x configuration; when the state already exists it imports nothing |
| `ndimon-net-boot` | root | Oneshot. Rolls back a network change that was never confirmed, then queues the control plane, finder, decoder and `tesserad` again. It retries on failure |
| `tesserad` | `tessera` | Optional plugin runtime; network audio, flows and its virtual PTP media clock |

`ndimon-control`, `ndimon-r` and `ndimon-finder` restart without a start-rate
limit, so the device recovers without an operator. Finder and decoder use separate SDK homes under `/run/ndimon/ndi/{finder,decoder}`, selected with `NDI_CONFIG_DIR` (set in both units and before SDK initialization). Pinning `HOME` is not enough. Both processes emit the same transport keys from `Rxpm`. Transport is read at SDK/process startup, so applying a transport change restarts the decoder service; recreating just its receiver is insufficient.

On RK3588 boards with the global `ondemand` governor interface, the root decoder service reduces `sampling_rate` to 10 ms while it runs. This shortens CPU demand sampling for bursty decoding work and affects **all CPU policies and processes**. The governor and frequency limits stay as configured. Stop/crash cleanup restores the saved interval when its applied value is still current; a shorter existing interval or a later operator change is preserved. Other boards, other governors and kernels exposing only per-policy tunables are skipped, with the reason recorded in the service journal.

To disable this tuning, run `sudo systemctl edit ndimon-r` and add:

```ini
[Service]
Environment=NDIMON_CPU_SAMPLING=off
```

Then restart `ndimon-r`. Before rolling back to older service units, stop the receiver with the current unit so its cleanup restores the interval. If needed, run `sudo /usr/libexec/ndimon/ndimon-privd --cpu restore` before replacing it.

```bash
sudo systemctl restart ndimon-control ndimon-r ndimon-finder
sudo journalctl -u ndimon-r -u ndimon-control -u ndimon-privd -f
```

---

## Security

This is a LAN appliance, not a public service.

- **Passwords and sessions.** One operator password, stored as Argon2id in
  `/etc/ndimon-auth.json`; existing scrypt hashes migrate after a successful
  login. Until that file exists the password is `ndimon`. A session is a random
  256-bit token in the `ndimon_session` cookie (`HttpOnly`, `SameSite=Strict`,
  `Path=/`, `Secure` when the deployment uses HTTPS) or an
  `Authorization: Bearer` header. Sessions live in memory (at most 64), expire
  after eight idle hours and disappear when `ndimon-control` restarts. Changing
  or restoring the password ends every other session.
- **Login limiter.** Failed logins and wrong current-password checks are counted
  per source (an IPv4 address, or an IPv6 /64) over a sliding 60-second window.
  From the fifth failure the source is refused with 429 and a `Retry-After` of
  1 second that doubles with every further failure, up to 30 seconds, counted
  from its last failure; the correct password does not skip the wait, and a
  successful login clears the count. Many failures from many sources only add
  a short delay for everyone, never a refusal.
- **Same origin, CSRF, Host.** CORS is not enabled. A request with an `Origin`
  must match its `Host`. Every change made with the session cookie must send the
  session's CSRF token in `X-CSRF-Token`; Bearer requests do not need it. The
  `Host` must name the device itself: its IP addresses, `localhost`, its
  hostname, `hostname.local`, `hostname.<search domain>` for its resolver's
  search domains, or one of up to 16 names added under **Settings → Security →
  Additional host names**. Anything else gets HTTP 421 `misdirected_host`, which
  stops DNS-rebinding attacks. No DNS answer and no forwarding header is trusted.
- **Unauthenticated routes** are only `POST /api/login`, `POST /api/v1/session`,
  the sign-in status probes (`GET /api/auth-status`, `GET /api/v1/session`),
  `GET /api/health` from loopback (read-only), and the three token-only network
  confirmation routes. GET never changes state.
- **No sudo.** `ndimon-control` and the finder run unprivileged, and
  `ndimon-control` runs with `NoNewPrivileges` and a read-only file system apart
  from its state, import, backup and runtime directories. Reboot, hostname, NTP,
  the reboot timer, service control, artifacts under `/etc`, network changes,
  plugin lifecycle and updates are typed requests to `ndimon-privd`.
- **Privileged daemon.** `ndimon-privd` listens on
  `/run/ndimon-privd/control.sock` (mode 0660, group `ndimon-priv-control`, which
  only `ndimon-control` receives through its unit). It identifies its caller by
  the socket that accepted the connection, on every kernel, and on Linux 6.5 and
  later it also checks the caller's cgroup. It keeps a minimal capability set,
  logs every request kind and outcome (never bodies, passwords or tokens), and
  runs long operations as independent systemd jobs that survive a crash of the
  control plane.
- **Isolated finder.** The finder parses LAN traffic with its own account, no
  privileged group and no access to the state, the control sockets or privd.
- **Plugins** are separate processes in their own sandboxed transient units; the
  control plane runs no third-party code. Archive extraction is bounded in size,
  entry count and metadata, and a plugin's install and remove hooks run as root,
  so install only archives from publishers you trust. A checksum is not a
  publisher signature.
- **Signed updates.** The device installs a release only when its checksum list
  carries a valid signature from a key it trusts, and every package matches the
  list. See [Updates](#updates).
- IPC socket `/tmp/ndi-decoder.sock` is mode `0660`, group `ndimon`. Do not expose it on the LAN.
- `POST /api/net-confirm`, `/api/net-pending` and `/api/net-revert` are unauthenticated. The token is the credential, because the session cookie belongs to the old address. Wrong tokens, and only wrong tokens, count toward a per-client limit of five a minute.

---

## Recovery

The C++ health check records state and resets a stuck DRM flip. It does not reconnect by itself. The control supervisor schedules retries and, in active mode, recovers a stalled output. systemd covers a dead or hung process.

Health is sampled every 500 ms in `DisplayWorker::tick()`, after a 10 s grace from connect.

| Layer | Trigger | Action |
|-------|---------|--------|
| Display flip reset | Frames arriving, no DRM commit for 3 s | `reset_flip_pending()` inside the worker |
| Supervisor reconnect | IPC says disconnected, and a source is saved | Backoff 5 s, 10 s, 20 s, then 30 s |
| Active watchdog | `stall_count` ≥ 60 (about 30 s), mode `active` | IPC disconnect. The saved source stays. The supervisor connects again |
| systemd notify | No `WATCHDOG=1` within 30 s | Kills `ndimon-r` (or `ndimon-control`). `Restart=always` brings it back in 3 s |
| Process liveness | `ndimon-r`, `ndimon-control` or the finder exits | systemd `Restart=always` brings it back after 3–5 s, with no start-rate limit |
| Network | A change is not kept within the window, fails, or the box reboots first | The previous network configuration is restored by the device, independent of the browser and the control plane |
| State | The control plane stops in the middle of a change | The state transaction is finished or rolled back before it exits, and recovered at the next start |

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

Single page at `/`. Dark theme, Inter typeface served
from `/fonts/`. Sidebar pages are Dashboard, Outputs, NDI, Settings, System and
Plugins; phones use a bottom tab bar. Page hashes preserve navigation on reload.
Unsaved edits are marked, and live state comes from one `EventSource` on
`/api/v1/events`. The page closes an old stream before reconnecting. Every page
reads desired state and observations from that store and saves through typed
commands (`POST /api/v1/commands`) at the revision it loaded, so a page that was
changed elsewhere asks you to reload instead of overwriting; the `/v1/...`
routes named in the tables below remain available to scripts. Bootstrap
and icons are served locally from `/vendor/...`, with a CDN fallback.

| Chrome | Behaviour |
|--------|-----------|
| Login overlay | Password field. `POST /api/v1/session` |
| Top bar | Device name, live count across outputs (or LIVE / IDLE with one output), sources, display mode, live-feed indicator, sign out |
| Password warning | Shown while the password is still `ndimon` |
| Saturated warning | HX decode FPS under 85% of the source for 3 s |
| Connection warning | The event stream dropped; values may be stale until it reconnects |

Warnings stack in the page flow under the top bar. Destructive actions (reboot, restart, delete, update) confirm in a modal. The modal shows the device's own statement of what the exact change will restart or interrupt.

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
| Current connections | Per-output LIVE/IDLE, codec, HW/SW, SATURATED | SSE `/api/v1/events` |
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

**Save output audio** affects that output and preserves other outputs. Routes
and engine are one change, so a save also selects the experimental v2 engine.
Fresh installs run v1: the first save then shows that the decoder restarts and
may briefly interrupt video, and applies only after you confirm; once v2 runs, a
save is applied live. **Use experimental audio engine v2…** and **Return to audio
engine v1…** remain as explicit actions; returning to v1 restores the legacy path
and retains saved choices. Disabling an output retains its choices without
contributing audio. Known offline endpoints stay visible and are retried by the
control side.

One physical destination has one writer and can mix different receiver sources.
Duplicate active contributions from the same physical source to the same sink,
and overlapping channel writers, are refused. If another browser has saved
audio, reload the saved output before retrying a revision conflict. Resource or
busy errors preserve the previous graph.

See [the API contract](docs/api/output-audio.md). Audio v2 remains experimental;
sustained clock stability and external hardware interoperability are not certified.

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
or NDI VLAN selection per output is deferred with receiver-process work.
Per-output audio routing is independent of those settings.

Screensaver and colour space are stored and sent in connect metadata. `CaptureSS` is stored; the core does not freeze a frame for it. `TallyMode` `VideoMode` is stored; program tally is applied when the mode is `TallyOn`.

### Settings

| Control | API |
|---------|-----|
| Change password (current, new, confirm, minimum 6 characters) | `POST /api/v1/session/password` |
| Additional host names (up to 16 DNS names) | `set_device` command; `GET/POST /v1/DeviceSettings/web-allowed-hosts` |
| Named wired networks: untagged/VLAN, DHCP/static, DNS search domains, management assignment, read-only usage | `GET /v1/Network/profiles`, `POST /v1/Network/profiles/preview`, `/apply` |
| Splash colours (with one-click palettes), gradient or flat backdrop, logo position, text, visibility, with an in-page preview that follows the controls | `POST /v1/Splash/config` |
| Logo upload (PNG or JPEG) | `POST /v1/Splash/uploadLogo` |
| Splash preview | `POST /v1/Splash/preview` |
| OSD enable and text (max 128 characters) | `POST /v1/Splash/osd` |

Define networks here and select **Management / Control**. The NDI page and
Tessera plugin select their roles from the same network IDs. See [Network](#network).

### System

| Control | API |
|---------|-----|
| Device information | `GET /v1/AboutMe` and `/api/status` |
| GPU decode (x86) | `GET /api/v1/system/vaapi-info` |
| Check for updates / Update now | `GET /api/v1/system/version`; the signed `install_update` command (`POST /v1/System/update` for scripts). See [Updates](#updates) |
| Backup and restore | `GET /v1/Backup/export`, `POST /v1/Backup/inspect`, `/apply` (at the revision the file was inspected at); the `undo_restore` command |
| NTP server (hostname or IP; blank keeps the OS default) | `set_device` command; clock state `GET /v1/DeviceSettings/ntp` |
| Scheduled reboot | `set_device` command; next reboot `GET /v1/DeviceSettings/reboot-schedule` |
| Restart one service | `service_control` command (`POST /api/services/:name/restart` for scripts) |
| Restart decoder and finder | Restarts those two units. The control plane stays up |
| Reboot | `reboot` command (`POST /v1/System/reboot` for scripts) |
| Watchdog mode and counters | `set_device` command, `GET /api/health`, `GET /api/watchdog-stats` |

Restart, reboot and update keep their own confirmation dialog. The device then
reports the impact of the exact command, and the page applies that impact; if
it changed in between, the new impact is shown first.

---

## Network

Settings → Network defines named networks on the primary wired port: one
untagged network and up to eight 802.1Q VLANs, each with DHCP or static IPv4.
One network is the **management** network (web UI, gateway, DNS and optional DNS
search domains); the NDI page picks the network for discovery and every receiver,
and the Tessera plugin picks its audio network. An assigned network cannot be
removed until another is selected. Wi-Fi is out of scope.

- **Four backends.** The engine adopts and changes the effective wired
  configuration of NetworkManager, Netplan (networkd renderer), systemd-networkd
  and ifupdown (ISC dhclient only). Pin files that sort before the
  distribution's own (for example an Armbian `Name=e*` file) make a static VLAN
  keep its address. A host managed by dhcpcd or another unsupported manager is
  refused. A configuration the engine cannot represent exactly (mixed DHCP and
  static overrides, several static addresses, custom routes, routing-only search
  domains) is refused, never guessed, and the Network page then shows the refusal
  and the live address read-only.
- **Recoverable changes.** An apply is a journaled transaction: the device arms
  a recovery timer before it changes anything, applies the change in a separate
  bounded job, and after the new addresses and guards are ready opens a
  **60-second** window to press **Keep changes**. **Revert**, an expired window,
  a failed apply or a reboot restores the previous configuration, independent of
  the browser and the control plane. A recovery that cannot finish leaves the
  media services running and shows a banner on the Network card that says why the
  previous settings were not restored. Network changes are paused, and **Keep
  changes** and **Revert** answer HTTP 409. The banner's **Accept current
  network** button (`POST /v1/Network/recovery/accept`) is the way out, after it
  shows what the device found; see
  [Troubleshooting](#troubleshooting).
- **Confirm policy.** By default **Keep changes** is accepted from any address
  that holds the random transaction token (`token_only`); the stricter
  `require_management_address` accepts it only from the new management address.
  The button is never disabled; the device answers with a typed reason instead.
  Only a wrong token counts toward the limit of five a minute. A recovery that
  has failed is not a token problem: Keep and Revert answer `409`
  (`apply_failed` at step `recover`, with the reason) and do not count.
- **Isolation.** The engine owns an nftables table (`inet ndimon_management`)
  that limits web access to management, blocks forwarding between the configured
  networks and keeps NDI and Tessera traffic on their selected interfaces.
- **Tessera.** When NDIMon-R hosts Tessera it owns the network. Moving a
  network that a standalone Tessera install manages needs an explicit handoff.
- **Backend validation.** A network change is only applied for a backend that
  is listed in `/etc/ndimon-network-certified-backends.json`, a root-owned JSON
  array of backend names (`networkd`, `netplan`, `networkmanager`, `ifupdown`).
  All four passed the real-tool acceptance runs (on disposable guests, not mocks),
  so the `ndimon-privd` package installs the file with all four listed, on a
  fresh install and when upgrading a 1.3.x appliance. It is a Debian conffile:
  your edits survive upgrades, and a deleted file is not restored. Edit it to
  withdraw a backend you have not validated on your hardware, for example
  `["networkd"]`: for a withdrawn backend status and preview still work, the
  page shows the live configuration read-only, and every change is refused with
  `backend_not_certified`. A file that is missing, unreadable (it must be a
  regular file owned by root that others cannot write) or not a JSON array of
  those names allows no backend, and the `ndimon-privd` journal says why
  (`journalctl -u ndimon-privd`); it never stops the page or the boot.

See [network profiles and VLANs](docs/network-vlans.md) for switch, backend,
recovery and handoff details.

---

## Plugins

Plugins run as **separate sandboxed processes**; the control plane runs no
plugin code and the device has no JavaScript runtime. A plugin is a manifest
(version 2) with a prebuilt executable, a bounded JSON state schema, typed
commands and events, and a static UI panel. Its state lives in the NDIMon-R
document, so it gets revisions, events and backup for free; a plugin asks the
host for privileged work by naming one of the commands its manifest declares.

Open **Plugins** to upload a trusted plugin archive or choose a release from a
configured feed. Enter the plugin name, choose its `.tar.gz` file, and select
**Upload and install**. Installation does not restart the control plane; the plugin is enabled afterwards and its tab opens
on the Plugins page. Use **Disable**, **Update**, or **Remove**
to manage it. A failed removal keeps files for **Retry remove** or **Force remove**;
force removal skips the plugin's cleanup script. Normal removal keeps configuration.

Feed downloads (`plugin_feeds` in the device settings) select the appliance
architecture and check SHA256; an empty feed list shows upload only. Install
only publishers you trust: install and remove hooks run as root. Backups include
the state of installed plugins; restore skips absent plugins with a warning.
See [the plugin authoring guide](docs/plugins.md) for the manifest, the process
protocol, feeds, privileged commands and packaging.

**JavaScript plugins (manifest v1) from 1.3.x** stay installed with their files
and data but never run: they are reported as **needs an update to the
out-of-process format**, cannot be enabled, and their backup section reads
"restorable after plugin support is available". Install a manifest v2 release
of the same name to migrate, or remove them (**Force remove** if their cleanup
script no longer works).

**Tessera** is a first-party plugin whose settings are part of the NDIMon-R
document. Use a plugin archive built from a **Tessera 0.3.0 or later** runtime
bundle; this release pins the Tessera 0.3.0 runtime (`0.3.0-1`). Installing/enabling the plugin exposes its page; selecting experimental
network audio is separate. Configure device/clock settings, static flows,
reusable channel names and the Tessera network there. Configure each output's
audio mix under **Outputs → Advanced → Audio**. [Tessera setup](docs/plugins/tessera.md)
describes compatibility, ownership and the current limitations.

---

## Backup and restore

**System → Backup & Restore** downloads one JSON document (`ndimon-backup`, schema 1,
checksummed). **Include network settings** is on by default. **Include admin password** is off. Turn the password on only when restoring this same device.

Restore shows where the file came from, then one row per section with a Changed or Unchanged badge. Unchanged sections start unticked. You choose what to import. The ticked settings sections are validated and applied as one confirmed change to the device document, which a failed apply rolls back; the network section is applied last as its own confirmed transaction, and the plugin sections after the settings. Backups made by 1.3.x restore into 1.4.0.

Network settings are mentioned when addressing, configured networks or role
assignments differ. A profile comparison includes every network and the
Management/NDI/Tessera assignments, even when the management IP stays the same.
**Keep current network settings** is the default action; **Use backup network settings** applies the file as a normal protected network transaction. If another device already answers ARP for that address, Use stays disabled until you tick the acknowledgement. The review marks the network section as not applicable when the engine cannot change the network on this host (see [Network](#network)).

Leave **Device name** unticked when cloning a second unit. It is pre-ticked only when the backup hostname matches this box and the name actually differs. Groups are restored with their original case.

Restored networks and role assignments use one protected profile transaction,
with 60 seconds to Keep once activation is ready. Open the management address
shown in the dialog and click **Keep changes**; its transaction token works
without the old login cookie. Revert, expiry or an unconfirmed reboot restores
the prior configuration. For DHCP the address appears after a lease is acquired;
if you lose contact before seeing it, let the device revert.

Audio graphs, Tessera's flows and audio endpoints, and the state of installed
plugins are included. Restore skips missing plugins with a warning; it does not install
their runtimes. Restoring **Device** settings can also restore `audio_engine`
and restart the decoder, so review that selection before applying a backup from a
v2 appliance.

Undo last restore puts settings back to the automatic snapshot taken just before the import (five are kept, under `/var/lib/ndimon/backups/`), including a network change if that address differs.

### Backup file

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

| Section | Contents |
|---------|----------|
| `outputs` | Source name and IP, alias, scale, rotation, mode, audio, tally, screensaver, colour. Channels 1–8 |
| `presets` | Named sources |
| `discovery` | Discovery Server IP, groups, extra IPs |
| `transport` | `Rxpm` |
| `display` | Splash, OSD, logo as base64 |
| `device` | Watchdog mode, decode mode, audio engine, NTP, reboot schedule, additional host names |
| `identity` | `ndi_recv_name` and hostname |
| `network` | Complete network profiles and Management/NDI/Tessera assignments (adapted to this device's interface and backend) |
| `audio` | Audio settings/graph, gains, delays and output choices |
| `plugins` | The state of installed plugins (at most 1 MiB each), by plugin name; the `tessera` entry holds Tessera's flows and audio endpoints |
| `auth` | Password hash (Argon2id, with the scrypt fields when present). Only when the password checkbox was ticked |

Not exported: the finder cache, status files, the NDI SDK configuration, build and version files, `device_ip`, and any other runtime key.

A damaged checksum is refused. A newer `schema_version` is refused. Unknown keys are refused. Groups are not lowercased. Discovery Server is enabled only when the address is non-empty. The device name uses `hostnamectl`, never `machinename`. A snapshot is saved before anything changes.

---

## Updates

1.4.0 installs updates only from **signed releases**. A release is a set of the
four Debian packages with `SHA256SUMS` and `SHA256SUMS.minisig` (minisign,
Ed25519). The device checks the signature against the public keys it trusts,
checks every package against the signed list and the package identity, version
and architecture, and refuses downgrades unless forced. A killed update never
advances the firmware marker. No project key ships in the packages: the owner
of a deployment provisions the first public key as a root-owned
`/etc/ndimon/trusted-release-keys`, as described in [releases](docs/releases.md).

- **With a trusted key,** **System → Check for updates** looks up the newest
  release and **Update now** installs it. The lookup asks the release source for
  the release the project marks as *latest* (GitHub's `releases/latest` of
  `owner/repository`, over the same HTTPS host rules as an install) and reports
  it only when its checksum list carries a valid signature from a trusted key
  naming this product and that version, it holds the four packages for this
  architecture, and its version is newer than the installed one. Unverified
  data is never offered. The lookup is bounded (about 20 seconds), runs only when
  a page asks for the version, and its answer is kept for an hour (ten minutes
  after a failed lookup, such as an offline device, which then says so instead
  of offering anything). The version query reports `latest_version` and
  `updates_available` (`1`, or `0` when nothing newer is signed) with an
  `update_message`; **Update now** installs `latest_version`. The install job
  verifies everything again. To install a specific version, send the
  `install_update` command (`POST /api/v1/commands`) or `POST /v1/System/update`
  with `{"version":"1.4.0-1","force":false}`. Either starts a detached job that
  survives a restart of the control plane; poll the job for its durable result.
  Do not run two updaters together. An update never overwrites your edit of
  `/etc/ndimon-network-certified-backends.json`.
- **Without a key,** the page says "updates need a signed release; no release
  key is configured" and refuses. Update by hand: install the complete,
  verified package set with apt, or check out the new release and run
  `RUSTFLAGS="-D warnings" cargo xtask install --no-deps` from `control/`.

The release source is the repository named in the optional root-owned
`/etc/ndimon/release-source` (`owner/repository`, default `markfen88/NDIMon-R`).
Plugin updates are separate from core updates.

### Rollback

Rollback means reinstalling the previous release, with local console or SSH
access available. Keep that release's artifacts or its original 1.3.0 checkout.
First resolve any pending network transaction through Keep/Revert and save a
backup. Stop the stack before replacing state:

```sh
sudo systemctl stop ndimon-r ndimon-finder ndimon-control.service ndimon-control.socket ndimon-privd.service ndimon-privd.socket
```

For a previous **native package release** (1.4.0 or later), restore the previous
accepted state only if that is the intended recovery point, then reinstall its
entire verified package set (use its actual version and architecture):

```sh
sudo cp -a /var/lib/ndimon/state/state.json /var/lib/ndimon/state/state.before-rollback.json
sudo cp -a /var/lib/ndimon/state/state.prev.json /var/lib/ndimon/state/state.json
sudo apt install --allow-downgrades /path/to/previous-release/ndimon-*.deb
```

For **1.3.x**, remove the four new packages (data is retained), restore the
pre-import projections, and run the previous release's installer. 1.3.x ran
the Node.js API, so it needs Node.js 20 or newer (`sudo apt install nodejs npm`);
the native packages neither install nor remove Node. These commands copy
originals without deleting the recovery copies:

```sh
sudo apt remove ndimon-runtime ndimon-ui ndimon-control ndimon-privd
sudo find /etc -maxdepth 1 -type f -name '*.pre-control' -exec sh -c 'cp -a -- "$1" "${1%.pre-control}"' sh {} \;
sudo find /etc/tessera/config.d /etc/alsa/conf.d -maxdepth 1 -type f -name '*.pre-control' -exec sh -c 'cp -a -- "$1" "${1%.pre-control}"' sh {} \;
# If network journals were imported, preserve the native journal then restore:
sudo mv /var/lib/ndimon-network /var/lib/ndimon-network.before-rollback
sudo cp -a /var/lib/ndimon-network.pre-p8 /var/lib/ndimon-network
# Restore each existing private leaf snapshot in the same way:
sudo cp -a /var/lib/ndimon/net/pending.pre-p8/. /var/lib/ndimon/net/pending/
sudo cp -a /var/lib/ndimon/net/vlans.pre-p8/. /var/lib/ndimon/net/vlans/
cd /path/to/saved-1.3.0-checkout
sudo bash install.sh --no-deps
```

Run only snapshot commands whose source exists; a fresh install has no legacy
configuration snapshots. The Tessera directories are optional. The former
manual units are also preserved as `/etc/systemd/system/*.pre-p8`; reinstalling
1.3.x recreates its units and helpers. `state.prev.json` is one revision, not a
complete release backup. Newer configuration may be absent in an older release.
JavaScript plugins that stayed installed run again under 1.3.x. Verify login,
saved outputs/network and service health after either rollback.

---

## HTTP API

Base URL `http://<device-ip>/`. The native API is `/api/v1`; the 1.3.x `/v1/…`
routes remain as compatibility routes for scripts and are served by the same
process, under the same authentication. The same routers are also mounted without
`/v1` for `NDIDecode`, `NDIFinder`, `VideoOutput`, `DeviceSettings`, `System`,
`AboutMe`, and `NDIEncoder`. Splash, Presets, Backup, Network, Plugins, Audio and
ApiSettings are `/v1` only. A protected path that no router claims answers a
local 404; nothing is proxied.

All `/v1/*`, `/api/*` and plugin routes need a session except the routes under
[Security](#security). The `Host` header must name the device (HTTP 421
otherwise), and a change made with the session cookie must carry `X-CSRF-Token`.

Log in with the native session route, keep the cookie and send the returned CSRF
token on changes:

```bash
curl -c cookies.txt -H 'Content-Type: application/json' \
  -d '{"password":"ndimon"}' http://DEVICE/api/v1/session
# => {"csrf":"..."}
curl -b cookies.txt -H 'X-CSRF-Token: <csrf>' -H 'Content-Type: application/json' \
  -d '{"command":{"set_device":{"watchdog_mode":"passive"}}}' \
  http://DEVICE/api/v1/commands
```

Scripts can also log in with `POST /api/login` and send the returned `token` as
`Authorization: Bearer <token>`; Bearer requests need no CSRF token. A Bearer
credential is the session token, so it ends with the session.

### Native API (`/api/v1`)

| Method | Path | Notes |
|--------|------|-------|
| POST | `/session` | `{ "password" }` → `{ csrf }` and the `ndimon_session` cookie |
| GET | `/session` | `{ logged_in:false }`, or `{ logged_in, csrf, default_password }` for a signed-in caller |
| DELETE | `/session` | Sign out (204) |
| POST | `/session/password` | `{ current_password, new_password }` (at least 6 characters) → `{ csrf }`; every older session ends |
| GET | `/state`, `/state/{domain}` | The desired-state document and its revision, or one domain (`device`, `outputs`, `ndi`, `audio`, `network`, `tessera`, `display`, `presets`, `plugins`) with its domain revision |
| GET | `/observed` | Observed state per domain: sync status, pending network change, drift |
| GET | `/status` | Live status snapshot |
| GET | `/catalog/{name}` | Editor lists: `audio_endpoints`, `ndi_sources`, `network_interfaces`, `display_modes` |
| POST | `/commands` | `{ command, expect_revision?, expect_impact?, confirm? }` → `{ ok, revision, report, result }` |
| GET | `/events` | Server-sent events: `event` frames (`state_changed`, `apply_progress`, `observed`, `drift`, `network_pending`, `status`), a `resync` frame with empty data after lost events, and a keep-alive comment every 15 s |
| GET | `/system/status`, `/system/version`, `/system/vaapi-info` | System reads |
| POST | `/system/reboot`, `/system/softreboot`, `/system/restart-stack`, `/system/update` | System actions; they follow the same confirmation rule as commands |

`Command` is a JSON object with one snake_case key, for example `set_device`,
`select_source`, `put_output_audio`, `set_audio_engine`, `stage_network`,
`confirm_network`, `revert_network`, `set_tessera`, `put_tessera_flows`,
`set_display`, `save_preset`, `recall_preset`, `import_backup`, `undo_restore`,
`install_plugin`, `set_plugin_enabled`, `set_plugin_data`, `service_control`,
`reboot` and `install_update`. The full set is in the generated TypeScript types
(`ui/src/generated/`) and JSON Schemas (`control/schema/`).

Rules for commands:

- **Revisions.** The document has one `revision`. `expect_revision` is a
  compare-and-set: if the document moved, the answer is `409 revision_conflict`
  with `current_revision`, and nothing changes. Every response that changes
  state carries the new revision. A command that changes nothing keeps its
  revision.
- **Impact and confirmation.** A change that interrupts something (a decoder
  restart, a network change, a reboot) is refused with `409
  confirmation_required` and an `impact` (what restarts, what is interrupted, a
  `token`). Send the command again with `expect_impact: <token>` after showing
  the impact, or `confirm: true` for a script that accepts whatever the impact
  is. A stale token is refused even with `confirm: true`.
- **Errors.** A refusal is `{ "error": { "<kind>": { … } }, "message": "…" }`.
  Branch on the kind, never on `message`. Kinds: `invalid` (every field error at
  once), `locked`, `confirmation_required`, `revision_conflict`, `not_found`,
  `permission_denied`, `unauthorized`, `rate_limited`, `unsupported`,
  `apply_failed` (`domain`, `step`, `reason`) and `internal`. The network
  confirmation routes add `not_pending`, `expired`, `still_applying`,
  `token_mismatch` and `wrong_address`.
- **Apply.** A change is applied in place when it can be (audio graph reload,
  source switch), otherwise through the smallest restart. If an apply fails the
  previous document and generated files are restored and `apply_failed` is
  returned; the revision does not move.

### Compatibility routes (`/v1`, `/api`)

The compatibility routes keep the 1.3.x request and reply shapes; each one is a
thin translation to a command or query. Prefer `/api/v1` for new scripts.

#### Status and operations

| Method | Path | Notes |
|--------|------|-------|
| POST | `/api/login`, `/api/logout` | Legacy login: `{ ok, token, default_password, csrf }` and `Set-Cookie`; logout clears the session |
| GET | `/api/auth-status` | `{ auth_required, logged_in, default_password, csrf? }` |
| POST | `/api/password` | `{ "current", "password" }`; at least 6 characters; clears other sessions |
| GET | `/api/status` | Flattened status: stream, system, sources, decoder, discovery, `outputs[]`, plugins |
| GET | `/api/health` | Decoder health. 503 if the decoder socket is down. Loopback only without a session |
| GET | `/api/watchdog-stats` | Supervisor counters from `/tmp/ndimon-watchdog-stats.json` |
| GET | `/api/events` | Legacy SSE status stream (the UI uses `/api/v1/events`) |
| GET | `/api/services` | systemd state of the long-running units |
| POST | `/api/services/:name/restart` | `ndimon-r`, `ndimon-finder`, or `ndimon-control` |
| POST | `/api/net-pending` | `{ "token" }` — pending profile status without an old login cookie |
| POST | `/api/net-confirm` | `{ "token", "profiles": true }` — explicit Keep, from any address (the token is the credential) |
| POST | `/api/net-revert` | `{ "token" }` — revert that transaction without an old login cookie |

`/api/health` includes `alive`, `uptime_s`, and `outputs[]` with `output`, `connected`, `fps`, `health` (`idle`, `ok`, `degraded`, `stalled`), stale-age fields, and `stall_count`.

#### `/v1/NDIDecode`

| Method | Path | Body | Effect |
|--------|------|------|--------|
| GET | `/decodestatus` | — | Decoder status for the outputs |
| GET | `/decodesetup` | — | Output settings |
| POST | `/decodesetup` | `NDIAudio`, `ScreenSaverMode`, `TallyMode`, `ColorSpace`, `ChNum` | Saves the allowed fields |
| GET | `/decodeTransport` | — | `{ Rxpm }` |
| POST | `/decodeTransport` | `{ "Rxpm" }` | `TCP`, `UDP`, `Multicast`, `M-TCP`, or `RUDP`. Restarts the decoder service so the SDK rereads transport |
| POST | `/connectTo` | `{ "SourceName", "SourceIP", "Output" }` | `Output` is 1–8. Empty or `None` forgets the source. Otherwise saves it and connects. A later Discovery Server route replaces it (last writer wins) |
| GET | `/connectTo` | — | 405 |
| GET | `/capture` | — | Touches a capture flag and returns text. Not a freeze-frame |

Allowed `decodesetup` values: `NDIAudioEn` or `NDIAudioDis`; `BlackSS`, `SplashSS`, or `CaptureSS`; `TallyOn`, `TallyOff`, or `VideoMode`; `ColorSpace` `YUV` or `RGB`.

#### `/v1/NDIFinder`

| Method | Path | Body | Effect |
|--------|------|------|--------|
| POST | `/refresh` | — | Rescans sources. Text `success` |
| GET | `/refresh` | — | 405 |
| POST | `/reset` | — | Clears the source cache to `None` |
| GET | `/List` | — | `{ count, list }` name → URL |
| GET | `/NdiOffSnSrc` | — | Plain text, comma-separated IPv4 |
| POST | `/NdiOffSnSrc` | text/plain IP list | Validates and saves the list |
| GET | `/NdiGrpName` | — | `{ ndi_groups }` |
| POST | `/NdiGrpName` | string or `{ ndi_groups }` | Case preserved |
| GET | `/NDIDisServer` | — | IP plus derived `NDIDisServEn` or `NDIDisServDis` |
| POST | `/NDIDisServer` | `{ "NDIDisServIP" }` | Blank disables |

#### `/v1/VideoOutput`

`output` is 0-based.

| Method | Path | Body | Effect |
|--------|------|------|--------|
| GET | `/modes?output=0` | — | `{ modes, current, auto_resolution, rotation, supported_rotations }` |
| GET | `/resolution?output=0` | — | Current mode |
| POST | `/resolution` | `{ "auto": true, "output" }` or `{ "width", "height", "refresh_hz", "output" }` | Auto-match, or lock a mode |
| POST | `/scaleMode` | `{ "scale_mode", "output" }` | `letterbox`, `stretch`, or `crop` |
| POST | `/rotation` | `{ "degrees", "output" }` | Use `supported_rotations` from `/modes`; unsupported angles return 400 |

#### `/v1/DeviceSettings`

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
| GET, POST | `/web-allowed-hosts` | `{ "web_allowed_hosts": ["studio.lan"], "revision" }` | Up to 16 lower-case DNS names; no IP literals or wildcards |

#### `/v1/Audio`

| Method | Path | Notes |
|--------|------|-------|
| GET | `/endpoints` | Endpoint catalog with labels, channel/rate metadata and readiness |
| GET, PUT | `/outputs/:ch` | Stable one-based output 1–8; enabled, inputs, destinations and revision |
| GET, POST | `/engine` | Read engine; explicitly select `v1`/`v2` with `confirm:true`, restarting the decoder |
| GET, PUT | `/settings` | Full audio document; legacy projection is limited to the primary output |
| PATCH | `/sources/:id`, `/sinks/:id` | Legacy endpoint overlay preserving other outputs; ambiguous edits refused |
| GET | `/status` | Runtime master, period timing, endpoint readiness, xruns and drift compensation |

Output writes require the current revision; stale writes return 409
`revision_conflict`. Saving an output also selects engine v2 (confirmed first
while v1 runs). See
[the detailed output-audio API](docs/api/output-audio.md) for request shapes,
delay limits, resource conflicts and legacy migration behavior.

#### `/v1/Splash`

| Method | Path | Body | Effect |
|--------|------|------|--------|
| GET, POST | `/config` | colours, `backdrop` (`gradient` or `flat`), logo percentages, text, `show_*` | `logo_path` is only an `/etc/ndi-splash-logo.*` path, or empty. A stored `show_box` from older versions is ignored |
| POST | `/preview` | `{ "source_available": true/false }` | Splash on idle outputs |
| POST | `/uploadLogo` | `{ "data", "filename" }` | PNG or JPEG magic bytes. Stored content-addressed and served as the splash logo |
| GET, POST | `/osd` | `{ "enabled", "text" }` | Text max 128 characters |

Splash defaults: idle background `#0D1B2A`, live background `#0D2B1A`, idle accent `#4488CC`, live accent `#22FF88`, logo at 50/50/50 percent, text “No Signal” / “Signal Available”, text height 4 percent of the screen.

#### `/v1/Presets`

| Method | Path | Body | Effect |
|--------|------|------|--------|
| GET | `/list` | — | `{ presets: [{ name, source, ip }] }`, at most 32 |
| POST | `/save` | `{ "name", "source", "ip" }` | Upsert by name |
| POST | `/delete` | `{ "name" }` | Remove |
| POST | `/recall` | `{ "name", "output" }` | `output` is 1–8. Saves the source and connects |

#### `/v1/System`

| Method | Path | Notes |
|--------|------|-------|
| GET | `/version` | `firmware`, `commit`, `build_date`, `ndi_version`, `update_supported`, `update_running`, `update_message`; with a release key also `updates_available` and `latest_version` (see [Updates](#updates)) |
| POST | `/update` | Signed package update; body `{"version":"…","force"?}`; refused while no release key is configured |
| POST | `/reboot` | Acknowledges, then reboots |
| POST | `/softreboot` | Restarts the stack. Not in the UI |
| GET | `/vaapi-info` | `{ available, driver, decode_profiles }` from the install-time `vainfo` capture |
| GET | `/status` | Raw decoder status. `/api/status` is the one the UI uses |

#### `/v1/AboutMe` and `/v1/ApiSettings`

| Method | Path | Notes |
|--------|------|-------|
| GET | `/v1/AboutMe/` | Device name, hostname, IP, firmware, model, NDI version, build date, per-channel info |
| GET, POST | `/v1/ApiSettings/` | Legacy whitelist (`GroupName`, `MACAddress`, `device_name`, `host_name`). The UI does not call it |

#### `/v1/Backup`

Session required. Inspect accepts an 8 MB JSON body. The global JSON limit is 4 MB.

| Method | Path | Body | Effect |
|--------|------|------|--------|
| GET | `/export?network=1&password=0` | — | Downloads `ndimon-<alias>-<YYYYMMDD-HHMM>.json`. `network=0` omits network. `password=1` includes the password hash |
| POST | `/inspect` | The backup document | Validates schema and checksum. Returns `import_id`, source device, per-section diffs, and the network comparison (`needs_prompt`, message, warnings, `duplicate`) |
| POST | `/apply` | `{ "import_id", "sections": [], "apply_network": false }` | Applies the ticked sections as one confirmed change. Network is applied last, through the confirm-or-revert path |
| GET | `/snapshots` | — | Automatic pre-restore files, newest first. Five are kept |
| POST | `/undo` | `{ "name" }` | Restores that snapshot. Applies network only when it differs. Undo takes its own snapshot first |

`sections` may be `outputs`, `presets`, `discovery`, `transport`, `display`,
`device`, `identity`, `auth`, `audio`, `plugins`. Network is selected separately
with `apply_network` and restored through a protected profile transaction.

#### `/v1/Network`

One physical wired port hosts named networks and role assignments. Wi-Fi and
switch trunk configuration are outside this API. The full profile document has
`interface`, `backend`, `networks`, the network IDs for `management`, `ndi` and
`tessera`, and the `confirm_policy`. See [the profile API](docs/network-vlans.md#api).

| Method | Path | Body | Effect |
|--------|------|------|--------|
| GET | `/profiles` | — | Committed profiles, live status, assignments, any pending transaction and, when the configuration cannot be adopted, `refusal` |
| POST | `/profiles/preview` | Complete candidate document | Validate revision and impact without changing the OS |
| POST | `/profiles/apply` | Complete candidate document | Stage one recoverable change to networks and assignments |
| POST | `/profiles/rollback` | — | Revert the pending profile transaction |
| POST | `/recovery/accept` | `{ "expect_impact" }` | Give up a recovery that cannot finish and keep the network as it is. The first call answers `confirmation_required` with what the device found (the failure, the live addresses, foreign configuration); repeat it with the impact token |
| GET | `/status` | — | Compatibility status view: backend, configured/live addressing and pending information |
| POST | `/check` | `{ "address" }` | Duplicate-address probe (`arping`). Advisory |
| POST | `/apply` | `{ "mode", "address", "prefix", "gateway", "dns", "dry_run" }` | Compatibility wrapper for management addressing; non-dry-run applies use profile transactions |
| GET | `/vlans` | — | Compatibility VLAN status view |
| PUT, DELETE | `/vlans/:id` | — | Refused; use a complete profile transaction |
| POST | `/handoff/{prepare,cancel,import,commit}` | See [docs/network-vlans.md](docs/network-vlans.md#ownership-handoff) | Move network ownership between a standalone Tessera install and NDIMon-R |

Confirm using `POST /api/net-confirm` with `{ "token": "...", "profiles": true }`.
Status polls do not confirm. Use `/profiles/rollback` or token-scoped
`/api/net-revert` to revert this transaction. When a recovery has failed (the
helper keeps its journal and retries), Keep and Revert answer `409` with
`apply_failed` at step `recover` and the reason; that is not a token failure and
does not count toward the limit. The media services keep running, and **Accept
current network** (`/recovery/accept`) is the operator's exit.

#### `/v1/plugins` and `/v1/plugins/tessera`

`GET /v1/plugins`, `/feed`, `POST /feed/refresh`, `/install`, `/upload?name=`,
`/{name}/{enable,disable,update,uninstall}`, `/{name}/commands/{command}` and
`/jobs/{job}/cancel` manage and drive plugins; see [the plugin guide](docs/plugins.md).
The Tessera adapter routes are under `/v1/plugins/tessera`
([Tessera setup](docs/plugins/tessera.md)).

#### Stubs

| Path | Response |
|------|----------|
| `/v1/NDIEncoder/encodesetup` | 404 `{ "status": "Command not supported" }` |
| `/v1/NDIEncoder/encodeTransport` | 404, same body |

### Control map

| UI control | HTTP | Then |
|------------|------|------|
| Login / logout / password | `/api/v1/session`, `/api/v1/session/password` | — |
| Live status | `GET /api/v1/events` | Decoder IPC `subscribe` and `status` |
| Refresh sources | `POST /v1/NDIFinder/refresh` | Finder rescan |
| Apply source | `POST /v1/NDIDecode/connectTo` | IPC `connect` |
| Source = None | `connectTo` with `None` | IPC `forget_source` |
| Recall preset | `POST /v1/Presets/recall` | IPC `connect` |
| Resolution auto / manual | `POST /v1/VideoOutput/resolution` | `auto_resolution` or `set_resolution` |
| Scale / rotation | `/v1/VideoOutput/scaleMode`, `/rotation` | `set_scale_mode`, `set_rotation` |
| Tally, audio, screensaver, colour | `POST /v1/NDIDecode/decodesetup` | `apply_config` |
| Transport | `POST /v1/NDIDecode/decodeTransport` | Restart decoder service and reconnect saved source |
| Output audio | `PUT /v1/Audio/outputs/:ch` | Validate revision, compile and apply graph; retain selected engine |
| Audio engine | `POST /v1/Audio/engine` | Confirm and restart decoder |
| HX decode mode | `POST /v1/DeviceSettings/decode-mode` | `apply_config` |
| Discovery Server | `POST /v1/NDIFinder/NDIDisServer` | `apply_config` to decoder and finder, then a decoder restart (confirmed) |
| Groups | `POST /v1/NDIFinder/NdiGrpName` | `apply_config` |
| Device name | `POST /v1/DeviceSettings/ndi-alias` | `apply_config` and `hostnamectl` |
| NTP | `POST /v1/DeviceSettings/ntp` | privd `SetTimeSync` |
| Splash / OSD | `POST /v1/Splash/config` or `/osd` | `apply_config` |
| Splash preview | `POST /v1/Splash/preview` | `show_splash` |
| Watchdog mode | `POST /v1/DeviceSettings/watchdog-mode` | The supervisor reads the document |
| Scheduled reboot | `POST /v1/DeviceSettings/reboot-schedule` | privd `SetRebootSchedule` |
| Network apply | `POST /v1/Network/profiles/apply` | Privileged profile transaction, then explicit `/api/net-confirm` |
| Update | `POST /v1/System/update` (body `{"version":"1.4.0-1"}`) | Detached signed-release install job; refused while no release key is configured |
| Reboot | `POST /v1/System/reboot` | privd `Reboot` |

---

## Internal IPC

The decoder listens on `/tmp/ndi-decoder.sock` (`AF_UNIX` `SOCK_STREAM`, mode `0660`, group `ndimon`) and the finder on `/run/ndimon/finder.sock` (mode `0660`, group `ndimon`). One JSON object per line. The control plane keeps a `subscribe` connection to the decoder and uses short request/response calls (2 s timeout).

Do not expose these sockets. Use the HTTP API.

| Action (decoder) | Effect |
|--------|--------|
| `subscribe` | Keeps the connection. Replays a Discovery Server route still in effect and the connection state. Pushes `connection` and `remote_source` events |
| `status` / `get_status_all` / `health` / `get_modes` | Queries |
| `apply_config` | `revision` and a `config` object holding whole domains (`device`, `outputs`, `ndi`, `audio`, `display`, `network`). The decoder validates every domain before installing any, refuses a revision older than the last accepted one and replies `{ok, revision}` or `{ok:false, errors}` |
| `reload_audio`, `check_audio` | Apply or pre-check the audio graph |
| `reload_config` | Reread the generated `/etc` JSON (the control plane sends it after delivering output settings, to refresh the audio projection) |
| `connect` | `source_name`, `source_ip`, `output` (0-based) |
| `disconnect` | Drops the picture. Keeps the saved source |
| `forget_source` | Disconnects and clears the saved source |
| `set_scale_mode`, `set_rotation`, `auto_resolution`, `set_resolution` | Picture controls. `output` is 0-based |
| `set_tally` | `tally_program`, `tally_preview` |
| `show_splash` | `source_available`. Idle outputs only |

The finder accepts `apply_config` with the complete `ndi` domain only. `connection` events carry `output` (0-based), `connected`, `source`, and `drm_ready`. `remote_source` events carry `output` and `source` (empty when the controller disconnected the output). With remote control on, the control plane saves the route as the output's source by name. It never sends `connect` for it: the SDK has already switched the receiver.

Worker status includes `ch_num`, `connector`, `output_alias`, `connected`, `source_name`, input size, `fps`, `codec`, `decode_backend`, `hw_decode`, `decode_fps`, `decode_saturated`, `scale_mode`, `rotation`, and `stream_type` (`Standard`, `HX`, or `unknown`).

---

## Files and paths

The source of truth is `/var/lib/ndimon/state/state.json`. Everything else in this
table is either generated from it by `ndimon-privd` (do not edit), a secret, or
runtime data. Prefer the web UI; to change a setting from a script use the API.

| Path | Owner / writer | Contents |
|------|----------------|----------|
| `/var/lib/ndimon/state/state.json`, `state.prev.json`, `rendered.json` | `ndimon-control` | The desired-state document (previous revision, and the manifest of generated files). Atomic writes; a change in flight is marked `state.pending` and recovered at start |
| `/var/lib/ndimon/assets/` | `ndimon-control` | Content-addressed uploads (the splash logo) |
| `/var/lib/ndimon/backups/` | `ndimon-control` | Pre-restore snapshots (five kept) |
| `/etc/ndimon-auth.json` | `ndimon-privd` for `ndimon-control` | Password hash, mode `0640` `root:ndimon` |
| `/etc/ndimon-dec{N}-settings.json`, `ndimon-device-settings.json`, `ndimon-rx-settings.json`, `ndimon-find-settings.json`, `ndi-group.json`, `ndi-config.json`, `ndimon-audio-settings.json`, `ndimon-splash-settings.json`, `ndimon-osd-settings.json`, `ndimon-presets.json`, `ndimon-plugins.json`, `ndimon-about-settings.json` | Generated by `ndimon-privd` | Projections of the document that the decoder and finder read at boot and after a restart (`root:ndimon`, `0640`) |
| `<file>.pre-control` | `ndimon-state-import` | The 1.3.x original of each imported file, kept for rollback |
| `/run/ndimon/ndi/decoder/`, `/run/ndimon/ndi/finder/` | The decoder and the finder, each its own | Private SDK homes (`NDI_CONFIG_DIR`): groups, discovery and transport |
| `/run/ndimon/finder/`, `/run/ndimon/ndimon-sources.json` | `ndimon-finder` | Discovered sources and finder control socket |
| `/var/lib/ndimon-network/` | Network engine in `ndimon-privd` | Private journal, committed copy and recovery state (root only) |
| `/etc/ndimon-network-interfaces.json` | Network engine | Public interface mapping for the selected roles |
| `/etc/ndimon-network-certified-backends.json` | Root administrator (a conffile of `ndimon-privd`) | The network backends validated for changes; the packages list all four, edit it to withdraw one |
| `/etc/tessera/config.d/60-ndimon.toml`, `61-ndimon-flows.toml`, `62-ndimon-network.toml`, `90-probe-flows.toml`; `/etc/alsa/conf.d/60-ndimon-tessera*.conf` | Generated by `ndimon-privd` | Tessera device, flows, network binding and ALSA PCM files |
| `/opt/ndimon-r/plugins/<name>/`, `/var/lib/ndimon/plugins/<name>/` | Root (install jobs), the plugin's process | Installed plugin code and its private state |
| `/etc/ndimon/trusted-release-keys`, `/etc/ndimon/release-source` | Root administrator | Release trust and source override |
| `/etc/ndimon-firmware-version`, `-ndi-version`, `-ndi-sha256`, `-vaapi-info` | Packages / installer | Version metadata and install-time captures |
| `/usr/share/ndimon/build-info` (`commit=`, `date=`) | `ndimon-runtime` package | The commit and date the package was built from; `/etc/ndimon-build-commit` and `-build-date` are only read when this file is absent (a source install) |

The splash IP is a runtime value. The decoder does not write it back to disk.

---

## Troubleshooting

**No picture after connect.** `sudo journalctl -u ndimon-r -f`. Confirm a desktop does not own DRM. Standard NDI and HX H.264/H.265 are the supported payloads. HDR `P216` / `PA16` is logged and not converted.

**Empty source list.** `systemctl is-active ndimon-finder`. The Discovery Server IP must be reachable. Senders on another subnet need that server or extra IPs under **NDI**. Groups are case-sensitive.

**HX is black.** The Standard SDK decodes HX inside the SDK and needs its codec runtime (`libavcodec.so.61` on the tested builds). Check `journalctl -u ndimon-r` for the first-frame line and confirm the runtime is installed. Hardware HX decode is not available.

**Web UI down.** `systemctl status ndimon-control ndimon-control.socket ndimon-privd.socket ndimon-net-boot`. The public listener is configured by `ListenStream=` in the `ndimon-control.socket` unit. Loopback health: `curl -s http://127.0.0.1/api/health`. If the control plane and the decoder are all down after a restart, check `ndimon-net-boot`: they start after it, and it retries a failed boot job by itself.

**"This device is not reachable under …" (HTTP 421).** You opened the UI by a name the device does not know. Use its IP address, or add the name under **Settings → Security → Additional host names**.

**Login is refused with 429.** Too many wrong passwords from your address. Wait for the `Retry-After` interval (at most 30 seconds, growing with each further failure).

**Rockchip MPP missing.** `ldconfig -p | grep librockchip_mpp`, then `cargo xtask setup-deps` (from `control/`) on an image that ships MPP.

**NDI library missing.** `ldconfig -p | grep libndi`, then `cargo xtask setup-deps` (from `control/`). The decoder and finder also need `libavahi-client3` and `libavahi-common3`, which `ndimon-runtime` depends on.

**Source jumps.** The saved source is in the output's settings. With **Allow Receiver Control** on, a Discovery Server controller can change it, and the output then shows "set by Discovery Server". Turn control off on the NDI page to stop that.

**Network change did not stick.** Click **Keep changes** within the 60-second ready-state window. Opening the page alone, a proxy header or a status poll does not confirm. An unconfirmed reboot or failed apply restores the old settings. Check
[supported backends and adoption limits](docs/network-vlans.md#operating-system-integration).

**The Network card says the previous network settings could not be restored.** A change was not kept (or failed) and the device could not put the old configuration back exactly, so it keeps retrying. The decoder, finder and Tessera keep running, network changes are paused, and **Keep changes** and **Revert** answer HTTP 409 (`apply_failed`, step `recover`) with the reason; that is not a wrong token. If the cause named in the banner goes away, the retry finishes by itself. Otherwise press **Accept current network** (or `POST /v1/Network/recovery/accept`): the device first shows its live addresses and any foreign configuration, then keeps the network as it is and removes what the failed change created. The live addressing of the port is not touched, and the Network page shows any difference from the saved networks. A failed ownership handoff cannot be accepted this way. See [Apply and recovery](docs/network-vlans.md#apply-and-recovery).

**Network changes are refused with `backend_not_certified`.** The host's backend is not listed in `/etc/ndimon-network-certified-backends.json`: it was withdrawn, or the file is missing, unreadable or malformed. `journalctl -u ndimon-privd` names the reason. Restore the list from the package with `sudo dpkg -i --force-confmiss` of the same `ndimon-privd` package, or write the four names back by hand. See [Network](#network).

**The Network page is read-only and shows a refusal.** The device's current network configuration uses a setting the engine cannot represent exactly (it names the key and the file, never the value). Change that file by hand, or leave networking to the operating system.

**Lost the confirm page.** Reopen the pending transaction's management link
before its countdown ends. If the new address is unknown or unreachable, leave
it unconfirmed and let the device restore the previous configuration. Opening
the link alone never keeps the change.

**Saved audio is silent.** Check the selected engine and endpoint readiness in
**Outputs → Advanced → Audio**. Routes play only under engine v2; saving an
output selects it, but until you confirm the decoder restart v1 stays active.
A disabled output retains its routes without playing them.
Check Tessera flows, 48 kHz agreement and the selected audio network; review
`GET /v1/Audio/status` for endpoint errors/xruns. Returning to v1 restores the
legacy path. Persistent drift pinned at its limit needs investigation.

**Plugin settings are missing.** Installing/enabling the plugin exposes its
tab on Plugins; network-audio activation is a second action. A core-only update
does not install Tessera. Check the archive's OS/architecture and `min_ndimon`.
A plugin marked **needs an update to the out-of-process format** is a 1.3.x
JavaScript plugin; install its manifest v2 release.

**Update now does nothing.** Without a release key the page says so and refuses (provision one as described in [releases](docs/releases.md)). With one, the page says "Up to date" when nothing newer is signed, or says why the lookup failed (offline, rate limited, a release whose signature or packages do not verify); a failed lookup is retried after ten minutes. You can always start an update with an explicit version through the API, or install the packages by hand (see [Updates](#updates)).

---

## Source layout

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
| `src/Config.cpp` | Reads the generated `/etc` JSON at boot and applies `apply_config` snapshots. Writes alias init and per-output source/mode only |
| `finder/main.cpp` | NDI Find. Receives the NDI domain over its socket |
| `control/` | The Rust workspace: control plane, privileged daemon, network engine, release tooling and `cargo xtask`. See [control/README.md](control/README.md) |
| `control/crates/ndimon-control/` | HTTP front door: authentication, sessions, CSRF, Host allowlist, SSE, supervisor |
| `control/crates/ndimon-state/`, `ndimon-compat/` | State actor and domains; the legacy `/v1` URL adapters |
| `control/crates/ndimon-privd/`, `ndimon-privd-proto/` | Typed privileged actions and their wire protocol |
| `control/crates/ndimon-netengine/` | Network engine: four backends, journal, recovery, handoff |
| `control/crates/ndimon-render/`, `ndimon-types/` | Generated-file renderers; the shared document, command and event types |
| `control/crates/ndimon-release/` | Signed release verification |
| `control/crates/ndimon-package/` | Native package maintainer hooks |
| `control/crates/ndimonctl/` | `status` and opt-in `diag collect\|analyze` diagnostics |
| `ui/` | TypeScript sources of the web UI; Node.js is a build tool only |
| `api/public/` | The committed UI build the packages ship (the directory name is historical); `api/` otherwise holds only the pinned Bootstrap manifest |
| `plugins/tessera/` | Tessera plugin manifest and UI fragment |
| `systemd/`, `packaging/` | Packaged units; sysusers.d, tmpfiles.d and udev declarations |
| `config/` | Default settings templates installed by the package for the first import |

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
- Sessions do not survive a control restart.
- Network changes are applied only for backends recorded as validated; none is by default.
- VLANs are supported on one physical wired port. Wi-Fi, IPv6 configuration and
  multiple physical port management remain outside this feature.
- Hosts managed by dhcpcd (older Pi OS) or another unsupported network manager are refused.
- No release key ships: in-UI updates need the owner to provision one.
- JavaScript (manifest v1) plugins do not run; only manifest v2 plugins do.
- VAAPI decoding is built only where libva is present; check it on your hardware.
- Audio v2 and the Ubuntu ARM Tessera bundle remain experimental. Sustained
  clock stability, external Dante hardware interoperability, HDMI/Dante delay
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

The NDI SDK is Vizrt’s, not MIT. Running the dependency setup means you accept the [NDI SDK License Agreement](https://www.ndi.tv/license).
