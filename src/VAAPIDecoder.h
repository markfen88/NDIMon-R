#pragma once
#include "VideoDecoder.h"

extern "C" {
#include <libavcodec/avcodec.h>
#include <libavutil/frame.h>
#include <libavutil/buffer.h>
#include <libavutil/hwcontext.h>
}

// Intel/AMD hardware H.264/H.265 decoder via FFmpeg's VAAPI hwaccel.
//
// Zero-copy path: the decoded VA surface is exported as a DMA-BUF
// (vaExportSurfaceHandle, DRM_PRIME_2) and handed to DRMDisplay with its plane
// offsets/pitches and tiling modifier — no CPU touch of pixel data. If export
// or import is not possible on a given GPU, the decoder transparently falls
// back to downloading the surface to CPU NV12 (still hardware-decoded).
class VAAPIDecoder : public VideoDecoder {
public:
    VAAPIDecoder();
    ~VAAPIDecoder();

    bool init(VideoCodec codec) override;
    bool decode(const uint8_t* data, size_t size, int64_t pts_us) override;
    void flush() override;
    void destroy() override;
    void release_frame(DecodedFrame& f) override;
    const char* backend_name() const override { return "vaapi"; }
    bool is_hardware() const override { return true; }

private:
    void drain_frames();
    bool emit_zero_copy(AVFrame* frame);     // export VA surface as DMA-BUF
    bool emit_cpu_download(AVFrame* frame);   // transfer/pack to NV12 heap buffer

    AVCodecContext* codec_ctx_     = nullptr;
    AVBufferRef*    hw_device_ctx_ = nullptr;
    AVPacket*       pkt_           = nullptr;
    AVFrame*        frame_         = nullptr;
    bool            initialized_   = false;
    bool            zero_copy_     = true;   // disabled after an export failure
    VideoCodec      codec_         = VideoCodec::H264;
};
