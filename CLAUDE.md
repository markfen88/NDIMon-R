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

**Dependencies:** NDI SDK 6 (`/usr/local/lib/libndi.so`), libdrm, ALSA, Avahi, nlohmann_json (auto-fetched). Optional: Rockchip MPP, FFmpeg, LibRGA.

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
  DRMDisplay.h/cpp   — DRM/KMS display (scaling, splash, OSD, RGA)
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
- **`color_format_fastest`** — avoids NDI SDK internal color conversion
- **Factory pattern for decoders** — `VideoDecoder::create()` auto-selects MPP > V4L2 > Software
- **DRM leases** — each output connector gets independent DRM master rights
- **Node.js owns reconnect logic** — C++ pushes IPC events, API handles retry scheduling

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
- `ndimon-find-settings.json` — discovery server enable/IP
- `ndimon-device-settings.json` — device name, NDI receiver alias
- `ndi-config.json` — off-subnet source IPs
- `ndi-group.json` — NDI groups

## Coding Conventions

- C++17, `-O3` with ARM NEON (`-march=armv8-a+simd`)
- Log prefix: `[ComponentName]` (e.g. `[NDIRecv]`, `[mppdec]`, `[V4L2Dec]`)
- Config: `Config::instance()` singleton
- Callbacks: `std::function` for video/audio/connection/routing events
- Thread model: recv thread (NDI capture), capture thread (V4L2), display thread (uncompressed frames), IPC thread
- Mutex naming: `decoder_mutex_`, `source_mutex_`, `frame_mutex_` etc.
- DRM format constants: `kDrmFormatNV12`, `kDrmFormatUYVY`, etc.
