# NDI documentation distill (NDIMon-R)

Local working notes distilled from the public NDI Docs & Guides dump
(`https://docs.ndi.video/all/llms.txt` and `llms-full.txt`, fetched 18 Aug 2026).
Vizrt owns the NDI docs; the full dump is kept in `docs/ndi/vendor/` (gitignored)
and is **not** for republishing. Prefer the live pages when something may have
changed: [docs.ndi.video](https://docs.ndi.video/all/).

NDIMon-R is a **Linux receiver / decoder appliance** using the **standard NDI SDK
v6** (not Advanced). Anything marked Advanced below is unavailable to us.

---

## What NDIMon-R already implements

| Topic | How we do it | NDI docs |
|-------|----------------|----------|
| Discovery (mDNS) | `ndimon-finder` Find instance | Discovery & Registration |
| Discovery Server | `networks.discovery` in `/var/lib/ndimon/.ndi/ndi-config.v1.json`; advertiser when IP set | Discovery Server |
| Off-subnet IPs | `networks.ips` | Manual Connection |
| Groups | `ndi.groups.recv` | NDI Groups (case-sensitive) |
| Transport | `rudp` / `multicast` / `tcp` keys | Configuration Files |
| HX passthrough | `codec.h264/h265.passthrough` in ndi-config | **Undocumented on standard SDK**; Advanced documents per-recv override JSON |
| Standard vs HX | FrameSync pull vs `capture_v3` push | Framesync API; recv |
| Receiver advertiser | `NDIlib_recv_advertiser` when DS is set | NDI 6.2 Discovery Tool / receivers |
| A/V at HDMI | ALSA + DRM paced from local output | Framesync (Standard); HX decoded locally |

---

## Time sync — there is no NDI time server

NDI **does not** ship a time-sync server, PTP grandmaster, or clock-distribution
service. Discovery Server is **source registration**, not time.

Official model:

1. **Sync-free by default.** Senders and receivers do not need a shared clock
   to connect. NDIMon-R as a single HDMI decoder works without NTP.
2. **Timestamps follow the OS clock.** NDI timestamp is Unix epoch, 100 ns units.
   Senders should NTP/PTP/GPS-lock the system clock. On Linux through SDK 6.3
   the SDK still anchors `high_resolution_clock` via `gettimeofday()` at init
   (a free-running clock); **versions after 6.3 switch to `CLOCK_REALTIME`**,
   which *does* track NTP/PTP. Pin/upgrade awareness matters.
3. **NDI Genlock** (`NDIlib_genlock_*`) is **sender-side**, Advanced SDK, for
   software sources with no native timebase. It cannot genlock a camera, capture
   card, or this HDMI decoder. Not applicable to NDIMon-R.
4. **Framesync** is the **receiver** API we already use on Standard NDI: pull
   video/audio on the *local* HDMI/ALSA timebase, duplicate/drop/resample.
   HX compressed frames must **not** go through Framesync (docs: H.264/H.265
   and AAC/Opus do not work through framesync on Advanced; we decode HX
   ourselves and pace to DRM).
5. **AVSync API** is for sample-accurate A/V without resampling (e.g. recording).
   Not the HDMI playback path.

**NDIMon-R option:** `NDI → NDI Discovery → NTP Time Server` writes a
timesyncd drop-in and/or a chrony source file via `ndimon-priv set-ntp`, then
enables the matching daemon. Blank = OS default. This is OS clock sync, which
is what NDI actually uses.

PTP (hardware/NIC) is out of scope unless we add `ptp4l` later; NDI still would
not speak PTP itself.

---

## Discovery Server vs NTP (do not confuse)

| | Discovery Server | NTP / PTP |
|---|---|---|
| Purpose | Find sources/receivers without mDNS multicast | Align OS clocks |
| Config | `networks.discovery` (default port **5959**) | systemd-timesyncd / chrony / ptp4l |
| Redundancy | Comma-separated DS IPs (NDI 5+) | Multiple NTP sources |
| NDIMon-R | `disc-ip` / `NDIDisServIP` | `ntp-server` / `ntp_server` |

When DS is set: **senders skip mDNS**; **receivers still combine DS + mDNS**.
Multiple DS IPs are supported. Only devices registered on a given DS see each
other on that DS.

---

## `ndi-config.v1.json` keys we care about

Written by `ndimon-r` and `ndimon-finder` before `NDIlib_initialize()`. The SDK
rewrites the file; we write passthrough keys again afterward.

| Key | Role |
|-----|------|
| `networks.discovery` | DS IP(s), optional `:port` (default 5959) |
| `networks.ips` | Extra finder IPs (off-subnet / no mDNS) |
| `groups.recv` / `groups.send` | Default `public`; **case-sensitive** |
| `rudp.recv.enable` | Preferred transport (NDI 5+ default) |
| `multicast.recv.enable` | Multicast receive |
| `tcp.recv.enable` | Multi-TCP |
| `unicast.recv.enable` | UDP+FEC unicast |
| `adapters.allowed` | NIC pin (Advanced / NDI 5+; we do not expose yet) |
| `codec.h264.passthrough` / `codec.h265.passthrough` | HX bitstream to our decoder (empirical on standard v6) |

`machinename` override is discouraged (mDNS name clashes). We set hostname via
`hostnamectl` instead.

On the appliance this file lives at **`/var/lib/ndimon/.ndi/ndi-config.v1.json`**
(systemd `HOME=` + `pin_ndi_home()`). Finder and decoder must share that
directory. A process that falls back to `/root/.ndi` or `~/.ndi` will desync
groups, DS, transport, and HX passthrough.

---

## Receiver / decode notes

- Frame memory is SDK-owned; `free_video_v2` / `free_audio_v3` after use.
- `line_stride_in_bytes` and `data_size_in_bytes` are a **union** — stride for
  uncompressed, size for compressed.
- `color_format_fastest` on ARM often yields UYVY; SDK may ignore BGRX.
- Routing metadata (`<ndi_routing>`) from DS: `allow_controlling=true`; **do not**
  `connect()` from the routing callback (feedback / crash). NDIMon-R follows this.
- `NDIlib_recv_send_metadata` goes **upstream to the sender**, not to the DS.
- Linux NDI SDK has **no GPU decode**; HX HW decode is app-side (our MPP/V4L2/VAAPI).

---

## Ports (typical)

| Port | Use |
|------|-----|
| 5353 UDP | mDNS |
| 5959 | Discovery Server |
| 5960 | Per-machine NDI service / extra-IP probe |
| 5961+ | Media sessions (dynamic) |

---

## Index of official pages (bookmark these)

- [Time, Timecode, and Sync](https://docs.ndi.video/all/getting-started/white-paper/time-timecode-and-sync-for-ndi.md)
- [Synchronization / Genlock](https://docs.ndi.video/all/getting-started/white-paper/synchronization.md)
- [Discovery Server](https://docs.ndi.video/all/getting-started/white-paper/discovery-and-registration/discovery-server.md)
- [Configuration Files](https://docs.ndi.video/all/getting-started/white-paper/configuration-files.md)
- [Framesync / AV Sync (Advanced)](https://docs.ndi.video/all/developing-with-ndi/advanced-sdk/av-sync.md)
- [Genlock (Advanced)](https://docs.ndi.video/all/developing-with-ndi/advanced-sdk/genlock.md)
- [Full index](https://docs.ndi.video/all/llms.txt)

Ask the live docs: `GET <page.md>?ask=<question>&goal=<endgoal>`.
