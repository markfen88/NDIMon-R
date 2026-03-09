# NDIMon-R

Open-source NDI monitor appliance for Rockchip single-board computers.
Replicates and exceeds BirdDog Play functionality.

## Supported Hardware
- **Radxa Rock 5B** (RK3588) — 4K60 hardware decode
- **Radxa Rock 4B+** (RK3399) — 1080p60 hardware decode
- **Raspberry Pi 5** — v4l2 hardware decode
- Generic ARM64 — software decode

## Features
- NDI / NDI|HX2 / NDI|HX3 source discovery
- Direct HDMI output via KMS/DRM (no desktop required)
- Multi-display support (per-connector)
- Auto-failover between primary and backup sources
- Splash screen with custom image + IP/alias overlay
- CEC: Channel Up/Down cycles NDI sources
- Per-display recording to MP4
- GPIO tally lights
- PTZ virtual joystick + USB gamepad
- Quad 2×2 Multi-View
- Web UI on port 8080 (Bootstrap 5 dark theme)
- Static/DHCP network configuration via UI
- Password protection

## Management
- Web UI: `http://<ip>:8080`
- Service: `systemctl status ndi-monitor`
- Logs: `journalctl -u ndi-monitor -f`
- Update: `/opt/ndi-monitor/update.sh`

## License
Apache 2.0
