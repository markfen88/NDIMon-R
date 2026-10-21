# Tessera network audio

Install and enable the Tessera plugin in **Plugins**, then select its network
audio tab. Plugin enablement makes the settings page available. It does not
activate experimental audio v2. The preferred route editor is now
**Outputs → an output → Advanced → Audio**; device, clock, flow and channel-name
configuration stays here. **Enable network audio** is a separate legacy-primary
action that selects v2 and restarts the decoder, briefly interrupting video.
Disable network audio to return to v1; saved choices are retained. Advanced
primary graphs can refuse legacy edits rather than silently replace a mix.

## Bundle and ownership

The optional plugin contains the host adapter, its settings UI and a precompiled
Tessera runtime bundle. The current plugin is `0.1.0-63~experimental1` with an
experimental Ubuntu 24.04 arm64 bundle, compatible with NDIMon `1.3.0`.
Install the matching plugin archive through Plugins. A core update does not
automatically download, compile or replace the Tessera runtime.

## Setup and routing

1. Define the required networks under **Settings → Network**. Select the NDI
   role on NDI and Tessera's audio role in its plugin. Network settings shows
   read-only usage; all assignments apply through the same protected
   [network transaction](../network-vlans.md). NDIMon owns OS networking here.
2. Configure the Tessera device at 48 kHz, channel counts, RX/TX channel names,
   clock and matching static flows. Labels support individual save/reset.
3. Open an output's blue **Advanced** control (collapsed by default). Add NDI
   or Tessera inputs and HDMI or Tessera destinations. Select both HDMI and
   Tessera to send that output mix to both. Full-device and named mono ranges
   are offered; two mono selections mix rather than forming a stereo pair.
4. Set input gain/input delay and separate destination delay, then **Save output
   audio**. Every selected input feeds every selected destination. Whole-ms
   delays are 0–5000 each with at most 5000 ms combined per active pair. A
   disabled output retains its selections. Saving does not select v2.
5. Explicitly select experimental v2 in the output audio editor and confirm its
   decoder restart. To recover the original path, select v1 there. Readiness,
   endpoint errors and runtime timing are available through the
   [audio API](../api/output-audio.md).

When NDIMon hosts Tessera, configure VLANs through NDIMon and select the audio
network inside this plugin. Standalone Tessera networking is separate.

The integration uses a 48 kHz audio engine and supports 1–16 device channels per
direction. Stereo is the initial supported configuration; other channel layouts,
sustained clock stability and external hardware interoperability are not certified.

Set the device name, encoding, latency and channel counts in Device. Names may contain up to 31 UTF-8 bytes. Device latency is in microseconds; input synchronization delay is separately set in milliseconds. Disruptive device changes display the daemon's impact information and require confirmation. Clock role and priority changes occur only when their Save buttons are pressed. The page reads clock state, offset, interface and software/hardware timestamping from the daemon.

Flows are static. Automatic vendor-controller subscriptions are unavailable. Configure a matching Tessera peer or an externally prepared hardware peer: direction, peer unicast IPv4 address and UDP port (1024–65535). Every flow uses every device channel in its direction, so both ends must agree on channel count. Only one RX flow can occupy those RX channels; multiple TX flows may send them to different peers. RX can optionally accept any source instead of limiting reception to a peer. Save flows validates the entire set before applying it. Removing a row takes effect only after Save flows.

The plugin's legacy Network input delay (0–2000 ms) and four-gain routing
controls project only the primary output. Prefer Outputs for independent
multi-input/destination routes and delay. Gains are −60 to +12 dB; 0 preserves
level and −60 mutes. Save routing applies its four gains together; edits are not
saved until their Save button is pressed. A primary graph that cannot be safely
represented returns `legacy_edit_unsupported`; a legacy matrix change can
require explicit `normalize_matrix:true` confirmation. These refusals preserve
the saved graph. No automatic HDMI/Dante delay measurement is provided.

Start, stop and restart control only the Tessera daemon. Stop and restart ask before interrupting network audio. If the daemon goes offline, the status strip reports it; the rest of the page stays available for diagnostics and recovery. Download diagnostics exports the backend's status and metrics as JSON. The Advanced section displays the generated flow configuration and daemon warnings. Bundle updates use the host Plugins list.
