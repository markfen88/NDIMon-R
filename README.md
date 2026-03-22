# NDIMon-R

**Hardware-accelerated NDI decoder appliance for ARM single-board computers**

[![Platform](https://img.shields.io/badge/platform-aarch64-blue)](https://github.com/markfen88/NDIMon-R)
[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)

---

## Overview

NDIMon-R turns a low-cost ARM single-board computer into a dedicated NDI decoder that outputs live video over HDMI or DisplayPort. It uses the hardware video engine on Rockchip and Raspberry Pi SoCs for zero-CPU H.264/H.265 decode, and falls back to software (FFmpeg) decode on any aarch64 board.

A Node.js REST API and web UI run alongside the C++ decoder core. The API is compatible with the BirdDog management protocol, so NDIMon-R can be managed by existing BirdDog-aware control systems without modification.

---

## Features

- **Hardware H.264/H.265 decode** — Rockchip MPP (RK3588/RK3399) and V4L2 M2M (Raspberry Pi 4/5)
- **Multi-output support** — independent NDI sources on HDMI-A-1, HDMI-A-2, and DP-1 simultaneously
- **BirdDog API compatibility** — all `/v1/` REST routes match the BirdDog Play 4K protocol
- **Web UI** — source selection, output config, resolution control, status monitoring
- **NDI Discovery Server support** — works with centralised NDI routing
- **Auto-reconnect** — exponential backoff reconnect on signal loss
- **Splash screen** — customisable idle/live backgrounds with logo and OSD overlay
- **Hotplug** — display workers initialise on hotplug when no monitor is connected at boot

---

## Supported Hardware

| Board | SoC | Decode Engine | Status |
|-------|-----|---------------|--------|
| Radxa Rock 5B | RK3588 | Rockchip MPP | Tested |
| Radxa Rock 4C | RK3399 | Rockchip MPP | Tested |
| Raspberry Pi 4 | BCM2711 | V4L2 M2M | Tested |
| Raspberry Pi 5 | BCM2712 | V4L2 M2M | Tested |
| Any aarch64 board | — | FFmpeg (software) | Fallback |

Runs on Debian Bookworm/Trixie, Ubuntu Noble (24.04), Armbian, and Raspberry Pi OS.

---

## Quick Install

```bash
git clone https://github.com/markfen88/NDIMon-R.git
cd NDIMon-R
bash install.sh
```

This single command installs dependencies, builds the binaries, and enables the systemd services. On first run it downloads the NDI SDK (~60 MB) from NDI's servers.

> **Note:** By running this installer you accept the [NDI SDK License Agreement](https://www.ndi.tv/license).

---

## Installation Details

### Options

```bash
bash install.sh              # Full install: deps + build + install
bash install.sh --no-deps    # Skip dependency install (already done)
bash install.sh --no-build   # Skip build (binary already compiled)
```

### What the installer does

1. **`scripts/setup-deps.sh`** — Installs system packages, Rockchip MPP (if applicable), NDI SDK v6, and Node.js v20
2. **`scripts/build.sh`** — Runs CMake and compiles `ndimon-r` and `ndimon-finder`
3. **`scripts/install.sh`** — Installs binaries to `/usr/local/bin`, copies default config files to `/etc/`, installs npm packages, and enables three systemd services:
   - `ndimon-r` — the main decoder process (C++)
   - `ndimon-finder` — NDI source discovery helper
   - `ndimon-api` — web UI and REST API (Node.js, port 80)

### Root vs user install

Running `install.sh` as root installs system-wide services (`/etc/systemd/system`). Running as a regular user installs user-scoped services (`~/.config/systemd/user`) and enables systemd linger so they persist after logout.

---

## Configuration

Config files live in `/etc/`. They are created from `config/` defaults on first install and are **never overwritten** by subsequent installs or updates.

| File | Purpose |
|------|---------|
| `/etc/ndimon-dec1-settings.json` | Audio, screensaver, tally, color space |
| `/etc/ndimon-find-settings.json` | NDI Discovery Server IP and enable/disable |
| `/etc/ndimon-device-settings.json` | Device alias (NDI receiver name shown in discovery) |
| `/etc/ndimon-rx-settings.json` | Transport mode (TCP / UDP / Multicast / RUDP) |
| `/etc/ndi-group.json` | NDI groups to subscribe to (default: `public`) |
| `/etc/ndi-config.json` | Off-subnet source IPs (comma-separated) |
| `/etc/ndimon-sources.json` | NDI source list cache (written by ndimon-finder) |
| `/etc/ndimon-dec1-status.json` | Runtime decoder status (written by ndimon-r) |

### Discovery Server

To use an NDI Discovery Server, edit `/etc/ndimon-find-settings.json`:

```json
{
  "NDIDisServ": "NDIDisServEn",
  "NDIDisServIP": "192.168.1.x"
}
```

Then reload:

```bash
sudo systemctl restart ndimon-r ndimon-finder ndimon-api
# or for user services:
systemctl --user restart ndimon-r ndimon-finder ndimon-api
```

---

## Web UI & API

The web UI is served at `http://<device-ip>/` once the `ndimon-api` service is running.

From the web UI you can:
- Select an NDI source to display on each output
- Switch between connected HDMI/DP outputs
- Change output resolution and refresh rate
- Adjust scale mode (letterbox / stretch / crop)
- Monitor connection status, FPS, codec, CPU, memory, and temperatures

### Key API endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/status` | GET | Full decoder and system status (JSON) |
| `/api/events` | GET | Server-Sent Events stream (1.5 s updates) |
| `/v1/NDIFinder/List` | GET | Available NDI sources |
| `/v1/NDIDecode/connectTo` | GET/POST | Connect to a source |
| `/v1/VideoOutput/modes` | GET | Available display modes |
| `/v1/VideoOutput/setresolution` | POST | Set output resolution |
| `/v1/NDIFinder/NDIDisServer` | GET/POST | Discovery server config |
| `/v1/DeviceSettings/ndi-alias` | GET/POST | NDI receiver name |
| `/v1/Splash/config` | GET/POST | Splash screen appearance |

All `/v1/` routes are compatible with the BirdDog Play 4K management protocol.

---

## Service Management

```bash
# System-wide install (root)
sudo systemctl status ndimon-r ndimon-finder ndimon-api
sudo systemctl restart ndimon-r
sudo journalctl -u ndimon-r -f
sudo journalctl -u ndimon-finder -f
sudo journalctl -u ndimon-api -f

# User install (non-root)
systemctl --user status ndimon-r ndimon-finder ndimon-api
systemctl --user restart ndimon-r
journalctl --user -u ndimon-r -f
```

Or use the convenience script:

```bash
bash scripts/status.sh
```

---

## Updating

```bash
cd NDIMon-R
git pull
bash install.sh --no-deps
```

This rebuilds the binaries and reinstalls services without touching existing config files in `/etc/`.

---

## Troubleshooting

**No video on HDMI after connecting a source**
- Check logs: `journalctl -u ndimon-r -f`
- Verify the display was connected before the service started, or wait for hotplug detection
- Confirm the NDI source is sending H.264, H.265, or an uncompressed format

**Source list is empty**
- Check that `ndimon-finder` is running: `systemctl status ndimon-finder`
- If using a Discovery Server, verify the IP in `/etc/ndimon-find-settings.json` and that the server is reachable
- NDI sources on other subnets require a Discovery Server or manual IPs in `/etc/ndi-config.json`

**Web UI unreachable**
- Check `ndimon-api` is running: `systemctl status ndimon-api`
- If port 80 is blocked, set `PORT=8080` in the `ndimon-api` service environment

**Source reverts unexpectedly**
- Source selection is persisted in `/etc/ndimon-dec1-settings.json`
- An NDI Discovery Server can override routing — if you use DS-managed routing, let the DS control source selection

**MPP hardware decode not working (Rockchip)**
- Verify MPP: `ldconfig -p | grep librockchip_mpp`
- If missing, re-run `sudo bash scripts/setup-deps.sh`
- Ensure the service user is in the `video` and `render` groups

**NDI SDK not found**
- Re-run `sudo bash scripts/setup-deps.sh` — it will download and install the SDK
- Verify: `ldconfig -p | grep libndi`

---

## Architecture

```
┌─────────────────────────────────────────────────────┐
│                    NDIMon-R process                  │
│  ┌──────────────┐   ┌──────────────┐                │
│  │ NDIReceiver  │──▶│ VideoDecoder │                │
│  │ (NDI SDK)    │   │ MPP/V4L2/SW  │                │
│  └──────────────┘   └──────┬───────┘                │
│                            │ DecodedFrame            │
│                     ┌──────▼───────┐                │
│                     │  DRMDisplay  │──▶ HDMI/DP      │
│                     └──────────────┘                │
│                                                      │
│  ┌──────────────┐                                   │
│  │  IPCServer   │◀──── /tmp/ndi-decoder.sock        │
│  └──────────────┘                                   │
└─────────────────────────────────────────────────────┘
         ▲ JSON commands / events
         │
┌────────┴──────────────────────┐
│  ndimon-api (Node.js, port 80) │
│  Express + BirdDog /v1/ routes │
│  Web UI (public/)              │
└────────────────────────────────┘

┌─────────────────────────┐
│  ndimon-finder           │
│  Writes /etc/ndimon-    │
│  sources.json           │
└─────────────────────────┘
```

---

## License

MIT — see [LICENSE](LICENSE) for details.

The NDI SDK is licensed separately by NDI (Vizrt). By running `install.sh` you accept the [NDI SDK License Agreement](https://www.ndi.tv/license).
