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
- Device sends `<ndi_device_status>` metadata every 30s with FPS, resolution, codec, health state, SoC temperature, and stall count
- DS routing accepted via `allow_controlling=true` and `allow_monitoring=true`
- Routing ACKs sent every 30s to keep DS in sync
- Connection metadata includes `<ndi_product>`, `<ndi_routing>`, `<ndi_audio_setup>`, `<ndi_video_setup>`, `<ndi_general_setup>`

### Dual-Mode Capture (Standard vs HX)

The receiver auto-detects stream type from the first video frame and uses the optimal capture mode:

**Standard NDI (SpeedHQ/UYVY/NV12):**
- SDK decodes SpeedHQ internally → delivers UYVY frames
- FrameSync enabled (`NDIlib_framesync`) — pull model at display refresh rate
- FrameSync handles A/V sync, frame duplication, silence insertion
- Video: `NDIlib_framesync_capture_video` → zero-copy display queue → DRM
- Audio: `NDIlib_framesync_capture_audio` at 48kHz/2ch/1024 samples → ALSA

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

## Coding Conventions

- C++17, `-O3` with ARM NEON (`-march=armv8-a+simd`)
- Log prefix: `[ComponentName]` (e.g. `[NDIRecv]`, `[mppdec]`, `[V4L2Dec]`)
- Config: `Config::instance()` singleton
- Callbacks: `std::function` for video/audio/connection/routing events
- Thread model: recv thread (NDI capture), capture thread (V4L2), display thread (uncompressed frames), IPC thread
- Mutex naming: `decoder_mutex_`, `source_mutex_`, `frame_mutex_` etc.
- DRM format constants: `kDrmFormatNV12`, `kDrmFormatUYVY`, etc.
