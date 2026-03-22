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

    virtual void set_frame_callback(FrameCallback cb) { frame_cb_ = std::move(cb); }

    // Factory: creates best decoder for the current platform
    static std::unique_ptr<VideoDecoder> create();

protected:
    FrameCallback frame_cb_;
};
