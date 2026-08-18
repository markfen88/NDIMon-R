# NDIMon-R

ARM-powered NDI receiver appliance.

## What This Is

A dedicated NDI (Network Device Interface) decoder for ARM single-board computers that receives live NDI video streams over the network, decodes them using hardware video decoders, and outputs to HDMI/DisplayPort via DRM/KMS. Includes ALSA audio output, a REST API, and NDI Discovery Server integration.

## Target Platforms

**Primary (ARM):**
- Rockchip: Rock 4B+, Rock 4C+, Rock 5B, Rock 5B+ (MPP hardware decoder)
- Raspberry Pi: Pi 4, Pi 5 (V4L2 M2M hardware decoder)
- Generic aarch64 with NEON (FFmpeg software fallback)

**Secondary (planned):**
- Linux x86/x64

## Architecture

```
NDI Network -> [NDIReceiver] -> [VideoDecoder] -> [DRMDisplay] -> HDMI/DP
                    |                                  |
                    +-> [AlsaAudio] -> speakers        |
                    |                                  |
              [IPCServer] <-> [Node.js REST API] <-> Web UI
```

**Three systemd services:**
- `ndimon-r` — C++ decoder core (this codebase)
- `ndimon-finder` — NDI source discovery (writes /etc/ndimon-sources.json)
- `ndimon-api` — Node.js Express REST API on port 80

## Build

```bash
# On target machine (ARM):
mkdir build && cd build
cmake .. -DCMAKE_BUILD_TYPE=Release
make -j$(nproc)

# Artifacts: ndimon-r, ndimon-finder
```

**Do not build locally.** Building and testing happens on remote ARM machines.

**CMake options:**
- `ENABLE_MPP=ON/OFF` — Rockchip MPP hardware decoder
- `ENABLE_V4L2=ON/OFF` — V4L2 M2M decoder (RPi)
- `ENABLE_FFMPEG=ON/OFF` — FFmpeg software fallback

**Dependencies:** NDI SDK 6 (`/usr/local/lib/libndi.so`), libdrm, ALSA, Avahi, nlohmann_json (auto-fetched). Optional: Rockchip MPP, FFmpeg.

## Code Layout

```
src/
  main.cpp           — Entry point, DisplayWorker class, main loop
  Config.h/cpp       — JSON config singleton (/etc/ndimon-*.json)
  NDIReceiver.h/cpp  — NDI SDK wrapper (find, recv, tally, routing, advertiser)
  VideoDecoder.h/cpp — Abstract decoder + factory (create() selects best)
  MppDecoder.h/cpp   — Rockchip MPP (H.264/H.265, DMA-BUF zero-copy)
  V4L2Decoder.h/cpp  — V4L2 M2M (RPi BCM2835, DMA-BUF export)
  SoftwareDecoder.h/cpp — FFmpeg libavcodec fallback
  DRMDisplay.h/cpp   — DRM/KMS display (scaling, splash, OSD, NEON color conversion)
  AlsaAudio.h/cpp    — ALSA PCM output (planar float -> S16LE)
  IPCServer.h/cpp    — Unix socket IPC (JSON commands + event push)
  PlatformDetect.h/cpp — Auto-detect Rockchip/RPi/Generic
finder/
  main.cpp           — NDI source discovery service
api/
  server.js          — Express.js REST API
  routes/            — Route modules (NDIDecode, NDIFinder, VideoOutput, etc.)
config/              — Default JSON config templates
scripts/             — build.sh, install.sh, setup-deps.sh, deploy.sh
systemd/             — Service files
```

## Key Design Decisions

- **C/C++17 for performance** — memory-bandwidth-limited on ARM SBCs
- **Dual-mode NDI receiver** — auto-detects Standard vs HX streams, uses optimal pipeline for each
- **HX codec passthrough** — NDI SDK delivers compressed H.264/H.265 bitstreams directly; hardware decoders handle the rest (no SDK software decode)
- **FrameSync for Standard NDI** — uncompressed streams use `NDIlib_framesync` for A/V sync, display timing, automatic frame duplication/dropping, and silence insertion
- **Zero-copy pipeline** — DMA-BUF from decoder to DRM framebuffer (MPP/V4L2); uncompressed frames hold SDK reference and free from display thread
- **`color_format_fastest`** — SDK ignores `color_format_BGRX_BGRA` on ARM and always delivers UYVY. `color_format_fastest` avoids unnecessary conversion attempts. Single-threaded NEON UYVY→XRGB conversion (RowPool thread pool was removed — saturated memory bandwidth on RK3399)
- **Factory pattern for decoders** — `VideoDecoder::create()` auto-selects MPP > V4L2 > Software
- **DRM leases** — each output connector gets independent DRM master rights
- **Node.js owns reconnect logic** — C++ pushes IPC events, API handles retry scheduling
- **Splash screen on disconnect** — `disconnect_source()` drains the frame queue, clears `streaming_` flag, and renders splash. `show_splash()` always renders (caller responsible for stopping pipeline first)
- **NDI groups are case-sensitive** — SDK treats "Production" and "production" as distinct groups. Frontend preserves case as entered.
- **Atomic config writes** — all JSON config writes (C++ and Node.js) use write-to-tmp-then-rename to prevent corrupt reads during concurrent access
- **SSE connection management** — browser closes old EventSource before reconnecting to prevent connection leaks that exhaust the per-host connection limit

## Reliability / Appliance Operation

**Design target:** Set-it-and-forget-it. Source selection persists across reboots
and network failures. The device recovers from any failure without human intervention.

### Source persistence:
- Selected source saved per-output in `/etc/ndimon-dec{N}-settings.json`
- Survives reboots — auto-connect on startup to last source
- Survives network failures — exponential backoff reconnect (5s→30s cap)
- `disconnect_source()` preserves saved source for auto-reconnect
- `forget_source()` explicitly clears saved source (user action only)

### Recovery hierarchy (fastest to slowest):
1. **Targeted component restart** — decoder re-init, display flip reset (~100ms)
2. **Full pipeline reconnect** — disconnect + connect cycle (~3-5s)
3. **Systemd watchdog restart** — process killed and restarted (~5s)
4. **Systemd Restart=always** — covers crashes, OOM, unexpected exit

### Health monitoring (`DisplayWorker::tick()` at 500ms):
- Recv thread heartbeat — detect NDI SDK thread hang
- Video frame timestamp — detect source/network stall
- Decoder output timestamp — detect hardware decoder hang
- Display commit timestamp — detect HDMI output freeze
- FPS tracking — cosmetic + health assessment

### Thresholds:
- Recv stall: >5s since last heartbeat → full reconnect at 15s
- Decoder hang: >5s video arriving but no decoded output → decoder restart
- Display freeze: >3s decoded but no commit → flip reset + show_black
- Escalation: 30s continuous stall → full pipeline reconnect

### systemd integration:
- `Type=notify` with `sd_notify("READY=1")` on startup
- `WatchdogSec=30` with `sd_notify("WATCHDOG=1")` from main loop (500ms cadence)
- `Restart=always`, `RestartSec=3`
- Guarded by `#ifdef HAVE_SYSTEMD` — builds without libsystemd still work

## NDI SDK Usage Notes

- `ndi-config.v1.json` must be written **before** `NDIlib_initialize()` for discovery server and codec passthrough
- SDK rewrites this file during init — we write it again afterward to restore passthrough settings
- `NDIlib_recv_advertiser` registers the device as a receiver with the discovery server
- Routing metadata (`<ndi_routing>`) from DS is handled via `allow_controlling=true` — do NOT call `connect()` from routing callbacks (causes feedback loop / SEGV)
- Frame memory is SDK-owned; must call `free_video_v2`/`free_audio_v3`/`free_metadata` after use
- `line_stride_in_bytes` and `data_size_in_bytes` are a **union** — use stride for uncompressed, size for compressed
- Cross-thread `free_video_v2` is supported — display thread frees uncompressed frames after rendering (zero-copy path)
- `color_format_fastest` implies `allow_video_fields = true` — always set explicitly
- NDI audio is planar float: `p_data` is a single base pointer, channel N starts at `p_data + N * channel_stride_in_bytes`
- `find_source` pointers are owned by the finder — copy strings before destroying the finder instance

### Discovery Server Telemetry
- DS routing accepted via `allow_controlling=true` and `allow_monitoring=true` on the receiver advertiser (NDI 6.2 API). This is the supported mechanism — the SDK switches sources internally; do NOT call connect() from a routing callback.
- IMPORTANT (verified against docs.ndi.video): `NDIlib_recv_send_metadata` sends metadata **upstream to the connected source (sender)**, NOT to the Discovery Server. The `<ndi_device_status>` telemetry and `<ndi_routing>` "ACKs" we send every 30s therefore reach the sender, not the DS. Senders ignore unknown XML, so this is harmless but does not drive DS state. Receiver visibility/state in the DS is managed by the advertiser/SDK itself.
- `<ndi_device_status>`, `<ndi_audio_setup>`, `<ndi_video_setup>`, `<ndi_general_setup>` are **custom (non-standard) XML elements**, not part of the documented NDI metadata vocabulary. `<ndi_product>` is standard.

### HX Passthrough (undocumented-config dependency)
- The `codec.h264/h265.passthrough` keys written into `ndi-config.v1.json` are NOT part of the publicly documented standard-SDK contract. The documented receive-side passthrough mechanism is per-codec override JSON at recv creation in the NDI **Advanced** SDK (6.3+). Passthrough works empirically with the v6 standard SDK we ship, but treat it as version-pinned, undocumented behavior.
- `verify_passthrough_config()` (main.cpp) reads ndi-config.v1.json back after `NDIlib_initialize()` rewrites it and sets `passthrough_ok`, surfaced in `/api/status` and `/api/health` and shown as a red banner in the web UI when false.
- The installed SDK version is pinned to the v6 line (setup-deps.sh) and recorded in `/etc/ndimon-ndi-version`.

### Dual-Mode Capture (Standard vs HX)

The receiver auto-detects stream type from the first video frame and uses the optimal capture mode:

**Standard NDI (SpeedHQ/UYVY/NV12):**
- SDK decodes SpeedHQ internally → delivers UYVY frames
- FrameSync enabled (`NDIlib_framesync`) — pull model at display refresh rate
- FrameSync handles A/V sync, frame duplication, silence insertion
- Video: `NDIlib_framesync_capture_video`. NOTE: FrameSync frames are NOT held zero-copy through the display queue — `on_video()` takes the memcpy fallback for FrameSync frames (the SDK frame is freed via `framesync_free_video` right after the callback). Only the HX/uncompressed `capture_v3` path uses the zero-copy SDK-frame-hold queue. This memcpy is a per-frame ~4–8 MB copy at 1080p/4K — a real memory-bandwidth cost on RK3399.
- Audio: `NDIlib_framesync_capture_audio` requesting native channel count (no_channels=0) → ALSA downmix to stereo

**HX NDI (H.264/H.265 compressed):**
- HX passthrough delivers compressed bitstream (no SDK decode)
- `capture_v3` push model — every compressed frame matters (no dropping)
- Video: compressed frame → VideoDecoder (MPP/V4L2/Software) → DMA-BUF → DRM
- Audio: planar float from `capture_v3` → ALSA

Stream type is reported in worker status as `stream_type: "Standard" | "HX" | "unknown"`.

## Config Files (in /etc/)

- `ndimon-dec1-settings.json` — audio, screensaver, tally, color space, source
- `ndimon-rx-settings.json` — transport mode (TCP/UDP/Multicast/M-TCP/RUDP)
- `ndimon-find-settings.json` — discovery server IP (enabled when IP is non-empty)
- `ndimon-device-settings.json` — device name, NDI receiver alias
- `ndi-config.json` — off-subnet source IPs
- `ndi-group.json` — NDI groups (case-sensitive)
- `ndimon-splash-settings.json` — splash screen appearance
- `ndimon-osd-settings.json` — on-screen display config

### Config File Ownership

Each config file has a single designated writer to prevent race conditions:

| File | Writer | Readers |
|------|--------|---------|
| `ndimon-find-settings.json` | ndimon-api | ndimon-r, ndimon-finder |
| `ndi-group.json` | ndimon-api | ndimon-r, ndimon-finder |
| `ndi-config.json` | ndimon-api | ndimon-r, ndimon-finder |
| `ndimon-rx-settings.json` | ndimon-api | ndimon-r |
| `ndimon-splash-settings.json` | ndimon-api | ndimon-r |
| `ndimon-osd-settings.json` | ndimon-api | ndimon-r |
| `ndimon-device-settings.json` | ndimon-api + ndimon-r (alias init) | both |
| `ndimon-dec{N}-settings.json` | ndimon-api + ndimon-r (source/mode) | both |
| `ndimon-sources.json` | ndimon-finder | ndimon-api |

**Rules:**
- C++ code must NOT write files owned by the API (no `Config::save()` — it was removed)
- C++ writes only: `save_device()` for alias init, `set_output()` for source/mode changes
- All writes use atomic write-to-tmp-then-rename to prevent partial reads
- Config reload is driven by explicit `reload_config` IPC from Node.js after writes

### Discovery Server Config

DS is enabled by IP presence — no separate toggle. If `NDIDisServIP` is non-empty,
DS is enabled (`NDIDisServ=NDIDisServEn`). If blank, DS is disabled. The API
derives the enabled state from the IP to eliminate toggle desync bugs.

### Transport Selection (Rxpm)

`ndimon-rx-settings.json:Rxpm` (TCP/UDP/Multicast/M-TCP/RUDP) is mapped to
`rudp.recv.enable` / `multicast.recv.enable` in `ndi-config.v1.json` by
`transport_json()` in BOTH main.cpp and finder/main.cpp (they share the file, so
both must emit identical keys). The SDK only reads transport settings at recv
creation, so a change rewrites the config and calls
`NDIReceiver::reload_transport()` → `recreate_recv_preserving_source()` to
destroy/recreate the recv instance and reconnect. RUDP is the SDK default;
Multicast also requires the sender to be multicasting.

### Authentication

`api/auth.js` guards all `/v1/*` and `/api/*` routes (session cookie or
`Authorization: Bearer`). Password hash (scrypt) lives in `/etc/ndimon-auth.json`;
default password is `ndimon` until changed (UI shows a warning banner). CORS
allow-all was removed — the API is same-origin only. `POST /v1/System/reboot`
(GET removed). All hand-built JSON config writes escape interpolated strings;
all shell-outs use `execFile`/`spawn` (no shell) — no more `hostnamectl` injection.

### Source Presets & Software Update

`api/routes/Presets.js` stores named source presets in `/etc/ndimon-presets.json`
(list/save/delete/recall). Recall sends a `connect` IPC with the pre-resolved
source+IP for instant switching (no rescan). `/v1/System/version` reports the
installed version + git update availability (recorded `/etc/ndimon-source-dir`,
`/etc/ndimon-build-commit`); `/v1/System/update` git-pulls + rebuilds detached.

## x86-64 Support (Phase 1 done; VAAPI decoder = Phase 2)

NUC/mini-PC appliances are supported. KEY FACT (verified at docs.ndi.video): the
NDI SDK has NO GPU decode on Linux — it's FFmpeg software-only. So HX hardware
decode is app-side via passthrough → our own decoder, which is NDI's documented
recommendation. NEON is ARM-only and fully guarded (`#if defined(__aarch64__) ||
defined(__ARM_NEON)`) with scalar `#else` paths that `-O3` auto-vectorises on x86.

- **decode_mode** (`auto|hardware|software`, device-level in ndimon-device-settings.json):
  `VideoDecoder::create()` consults it + platform. ARM `auto` = MPP/V4L2 unchanged.
  x86 `auto`/`hardware` = VAAPI if `HAVE_VAAPI` + render node, else software.
  "hardware" with no HW = best-effort fallback to software (logged; visible as
  `decode_backend` in status). Changing it at runtime rebuilds the worker decoder.
- **Backend reporting**: `VideoDecoder::backend_name()`/`is_hardware()` →
  status `decode_backend`/`hw_decode` per output → UI badge (HW/SW) + Decoder hint.
- **SoftwareDecoder threading**: `set_low_latency(bool)`. ARM fallback = 1 thread +
  LOW_DELAY (latency). x86 = all cores (FF_THREAD_SLICE|FRAME, capped 16) for 4K.
- **PlatformDetect**: `is_x86()`, `cpu_vendor()` (Intel/AMD), `has_render_node()`.
- **Installer** (`setup-deps.sh`): arch-normalised (IS_X86/MULTIARCH/NDI_LIB_MATCH);
  picks the x86_64-linux-gnu NDI lib (the tarball ships all arches — was the one
  real breakage); installs libva + intel-media/i965/mesa-va drivers + vainfo on
  x86; skips MPP; adds service user to render group; records vainfo to
  /etc/ndimon-vaapi-info. CMake: x86 flags `-msse4.2 -mtune=generic` (no -march=
  native, so binaries run across a mixed NUC fleet); `HAVE_VAAPI` via libva+libva-drm,
  VAAPIDecoder.cpp added only when the file exists (Phase 2).
- **Connector→ch mapping** in main.cpp is now a stable map: keeps HDMI-A-1→1,
  HDMI-A-2→2, DP-1→3 (backward compat) and assigns DP-2/eDP-1/etc the lowest free
  ch deterministically. `get_primary_mac_suffix()` enumerates /sys/class/net.
- **PHASE 2 (done)**: `src/VAAPIDecoder.{h,cpp}` — FFmpeg `AV_HWDEVICE_TYPE_VAAPI`,
  `vaExportSurfaceHandle(DRM_PRIME_2, COMPOSED_LAYERS)` → `DecodedFrame.prime_explicit`
  with per-plane offsets/pitches + `drm_modifier`. New
  `DRMDisplay::show_frame_dma_explicit()` imports via `drmModeAddFB2WithModifiers`
  (decode surfaces are tiled on Intel — LINEAR import = garbage) and scales through
  `atomic_plane_commit`, with a full-screen scanout fallback. If VA export or DRM
  import fails the decoder transparently downloads to CPU NV12 (`zero_copy_=false`,
  still HW-decoded). `DecodedFrame` gained prime_explicit/num_planes/plane_offset[4]/
  plane_pitch[4]/drm_modifier (defaults keep MPP/V4L2 on the legacy path).
  NOT compile-tested (no libva/ffmpeg headers in dev env) — MUST build on a NUC.
- **PHASE 3 (done)**: decode saturation indicator — worker tracks decoded fps
  (`decoded_count_`→`decode_fps_`); `decode_saturated_` trips when an HX stream's
  decode fps stays <85% of source fps for ≥3s (`hx_stream_` gates it; uncompressed
  FrameSync never trips). Surfaced as status `decode_fps`/`decode_saturated` →
  per-output SATURATED badge + top banner. `/v1/System/vaapi-info` parses
  /etc/ndimon-vaapi-info (vainfo) → System page shows driver + decode profiles.
- **Roadmap (Phase 4+)**: NVIDIA NVDEC, Intel oneVPL/QSV. Known caveat: surface
  released right after import (parity with MPP) → possible tearing under load.

## Coding Conventions

- C++17, `-O3` with ARM NEON (`-march=armv8-a+simd`)
- Log prefix: `[ComponentName]` (e.g. `[NDIRecv]`, `[mppdec]`, `[V4L2Dec]`)
- Config: `Config::instance()` singleton
- Callbacks: `std::function` for video/audio/connection/routing events
- Thread model: recv thread (NDI capture), capture thread (V4L2), display thread (uncompressed frames), IPC thread
- Mutex naming: `decoder_mutex_`, `source_mutex_`, `frame_mutex_` etc.
- DRM format constants: `kDrmFormatNV12`, `kDrmFormatUYVY`, etc.
