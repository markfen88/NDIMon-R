# NDIMon-R changelog

## 1.3.0 — 2026-10-20

### Plugins and network audio

- Install trusted plugin archives through Plugins, or update them from a
  configured feed. Enable, disable and remove plugins without replacing the
  decoder core. Plugins can contribute settings and backup extensions.
- Added the optional Tessera network-audio plugin with device/channel names,
  encoding and latency, RX/TX counts, clock controls, static flows, service
  controls and diagnostics. Install the archive matching your OS/architecture.
- Runtime updates are delivered in plugin bundles. Updating NDIMon core does
  not automatically replace an installed Tessera runtime.

### Per-output audio

- Added explicit opt-in audio engine v2 at 48 kHz; v1 remains the default.
- Each output can select multiple NDI/Tessera inputs and HDMI/Tessera
  destinations, allowing the same mix to play through HDMI and network audio.
- Inputs have gain and delay; destinations have independent delay. Each delay
  accepts 0–5000 ms, with at most 5000 ms combined per active route. Disabled
  outputs retain their saved choices.
- Shared destinations mix different sources through one physical writer.
  Duplicate source-to-destination contributions and overlapping writers are
  refused. Revision checks prevent one browser from overwriting newer changes.
- Saved routes do not switch engines. Selecting v2 or returning to v1 requires
  confirmation and restarts the decoder. Endpoint recovery preserves saved intent.

### Management and media networks

- Added named untagged/tagged networks on one wired port, with DHCP/static IPv4
  and independent management, shared NDI and Tessera role assignments.
- VLAN IDs are 1–4094, with up to eight tagged networks. Configure the switch
  trunk separately. The appliance lists its configuration, not all switch VLANs.
- Network settings shows read-only usage links. Assigned networks need a
  replacement before removal. Only management supplies a gateway and DNS.
- Changes stay in a draft until preview/apply. Once activation is ready, click
  Keep changes through the new management address within 60 seconds. Revert,
  expiry, failure or an unconfirmed reboot restores the previous settings.
- Added management/media traffic isolation and locked audio/NDI interface
  bindings. Backups restore networks and assignments in one protected transaction.

### Interface and receiver improvements

- Refreshed navigation, mobile layout, live output status and unsaved-edit feedback.
- Current Connections, Source Presets and full-width output cards collapse
  independently. Each output's blue Advanced control starts collapsed.
- Added Inter splash text, gradient/flat backgrounds, status pills, logo
  placement, OSD styling and live preview controls.
- Corrected shared SDK configuration and transport selection. RUDP is the
  default; transport changes restart the decoder so the SDK rereads settings.
- Improved queued-frame ownership, returning-sender endpoint lookup, bounded
  source retries, settings persistence, discovery and receive/drop reporting.
- Corrected active decode reporting, packed-YUV colours and rotation support.
  The Standard SDK software-decodes HX; allocating an unused hardware decoder
  does not enable hardware HX decoding.

### Known limitations

- Audio v2 and the current Ubuntu 24.04 arm64 Tessera bundle remain experimental.
  Sustained clock stability, external Dante hardware interoperability and
  hardware PTP support are not certified.
- Delays are manual. Automatic HDMI/Dante lip-sync, independent per-output video
  delay and HDMI ARC/eARC capture are unavailable.
- NDI transport, HX preference, tally and NDI network assignment remain global.
  Bluetooth routing/calibration and automatic vendor-controller subscriptions
  are not implemented.
- Standard SDK HX decoding remains on the CPU. Quarter-turn software rotation,
  HDR conversion, IPv6/Wi-Fi setup and multi-port network management are unavailable.

### Upgrade notes

1. Export a backup before updating. Existing appliance settings are preserved.
2. Network profiles require nftables; run the updated dependency setup when it
   is missing. An update using --no-deps does not install missing packages.
3. Upgrade plugins separately with matching OS/architecture bundles. Installing
   or viewing a plugin does not activate experimental audio v2.
4. Configure switch tagging and access to the new management network before
   applying a change, then explicitly Keep it when ready.
5. When NDIMon hosts Tessera, manage networking in NDIMon. Source publication
   does not update or restart an installed appliance.

## 1.2 beta

Included backup/restore, primary wired DHCP/static addressing with recovery,
scheduled reboot, NTP, authentication and source presets.
