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

This repository vendors the released [Tessera plugin 0.2.1](https://github.com/markfen88/NDIMon-Plugins/releases/tag/tessera-v0.2.1)
in `plugins/tessera/`. It requires NDIMon 1.3.0 or later and embeds the pinned
Tessera runtime 0.2.1-1. Available native bundles are Debian 13 amd64 and
experimental Ubuntu 24.04 arm64; they are not interchangeable with other
OS/architecture combinations. Runtime source lives in the
[Tessera repository](https://github.com/markfen88/Tessera).

The base NDIMon installer does not fetch or compile Tessera automatically.
Install the matching plugin archive through Plugins. Updating NDIMon or Tessera
source alone does not replace an installed plugin or its embedded runtime.
Dante Controller interoperability is enabled by default for the first logical
device, services appear in the Tessera portal by default, and the default portal
password is `tessera`. Automatic Controller subscriptions and lipsync remain
unsupported; flows are configured statically.

**After upgrading from 0.2.0-beta or the older experimental plugin, reboot the
appliance before using network audio.** This loads the clock overlay 2.0 runtime
in both the daemon and local clients. Rebooting interrupts audio and video.
Installation does not automatically restart active audio services.

To assemble an archive from this checkout, download the matching runtime bundle
from the [Tessera 0.2.1 release](https://github.com/markfen88/Tessera/releases/tag/v0.2.1)
and keep its original filename, then run:

```sh
scripts/build-plugin.sh tessera --bundle /path/to/tessera-bundle_0.2.1-1_amd64.tar.gz
# Or the experimental Ubuntu 24.04 ARM bundle:
scripts/build-plugin.sh tessera --bundle /path/to/tessera-bundle_0.2.1-1_arm64_ubuntu-noble-experimental.tar.gz
```

The builder checks the bundle checksum, source commit, platform and integration
contract against `bundles.lock.json` and `compatibility.json`. It writes the
plugin archive, catalog-entry JSON and `SHA256SUMS` under `dist/plugins/`.
Use a clean `dist/plugins/` directory for a release; `SHA256SUMS` includes all
plugin archives present there. The original `--ubuntu-experimental-bundle`
invocation remains supported. Output now uses names such as `tessera-plugin_0.2.1_arm64.tar.gz`, a shared `SHA256SUMS` and a printed path relative to the checkout.
Per-archive `.sha256` files are no longer generated.
Generated catalog entries use the canonical NDIMon-Plugins release URLs.
This plugin update does not change the NDIMon host version.

The host enforces `min_ndimon`, not an exact tested-build matrix. Compatibility
metadata does not certify external hardware or every host build.

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
