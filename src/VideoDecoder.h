#pragma once
#include <cstdint>
#include <cstddef>
#include <functional>
#include <memory>

enum class VideoCodec { H264, H265, MJPEG };

// DRM_FORMAT_NV12 numeric value (fourcc_code('N','V','1','2'))
static constexpr uint32_t kDrmFormatNV12  = 0x3231564e;
static constexpr uint32_t kDrmFormatNV16  = 0x3631564e;
static constexpr uint32_t kDrmFormatNV15  = 0x3531564e;
static constexpr uint32_t kDrmFormatYUYV  = 0x56595559;
static constexpr uint32_t kDrmFormatUYVY  = 0x59565955;

struct DecodedFrame {
    void*    opaque     = nullptr; // decoder-specific handle for release
    uint8_t* data       = nullptr; // CPU memory (may be null if fd valid)
    size_t   data_size  = 0;
    int      fd         = -1;      // DMA-BUF fd (-1 if unavailable)
    uint32_t width      = 0;
    uint32_t height     = 0;
    uint32_t hor_stride = 0;
    uint32_t ver_stride = 0;
    uint32_t drm_format = kDrmFormatNV12;

    // Explicit multi-plane DMA-BUF layout. MPP/V4L2 leave these at defaults and
    // the display uses its implicit contiguous-NV12 import. VAAPI sets
    // prime_explicit=true and provides per-plane offsets/pitches plus the DRM
    // format modifier (decode surfaces are tiled on Intel, so the modifier is
    // required to import them correctly).
    bool     prime_explicit  = false;
    int      num_planes      = 0;
    uint32_t plane_offset[4] = {0, 0, 0, 0};
    uint32_t plane_pitch[4]  = {0, 0, 0, 0};
    uint64_t drm_modifier    = 0;   // DRM_FORMAT_MOD_LINEAR
};

class VideoDecoder {
public:
    using FrameCallback = std::function<void(DecodedFrame&)>;

    virtual ~VideoDecoder() = default;

    virtual bool init(VideoCodec codec) = 0;
    virtual bool decode(const uint8_t* data, size_t size, int64_t pts_us) = 0;
    virtual void flush() = 0;
    virtual void destroy() = 0;
    virtual void release_frame(DecodedFrame& f) = 0;

    // Short backend id for status/UI: "mpp", "v4l2", "vaapi", or "software".
    virtual const char* backend_name() const { return "software"; }
    // True if this backend decodes on dedicated/GPU hardware (not the CPU).
    virtual bool is_hardware() const { return false; }

    virtual void set_frame_callback(FrameCallback cb) { frame_cb_ = std::move(cb); }

    // Factory: creates best decoder for the current platform
    static std::unique_ptr<VideoDecoder> create();

protected:
    FrameCallback frame_cb_;
};
