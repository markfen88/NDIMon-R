#pragma once
#include "VideoDecoder.h"

extern "C" {
#include <libavcodec/avcodec.h>
#include <libavutil/frame.h>
#include <libavutil/imgutils.h>
}

class SoftwareDecoder : public VideoDecoder {
public:
    SoftwareDecoder();
    ~SoftwareDecoder();

    bool init(VideoCodec codec) override;
    bool decode(const uint8_t* data, size_t size, int64_t pts_us) override;
    void flush() override;
    void destroy() override;
    void release_frame(DecodedFrame& f) override;

    // Latency vs throughput. Low-latency (default) pins to a single thread with
    // slice threading + LOW_DELAY — right for ARM fallback where latency matters
    // and this path is rarely used. Throughput mode uses all cores (frame+slice
    // threading) — right for x86 where software decode is a primary path and 4K
    // needs the cores. Must be called before init(). (~few frames extra latency
    // in throughput mode.)
    void set_low_latency(bool v) { low_latency_ = v; }

private:
    void drain_frames();
    // Convert AVFrame (YUV420P or NV12) to NV12 heap buffer.
    // Returns allocated buffer; caller must delete[].
    uint8_t* to_nv12(AVFrame* frame, size_t& out_size,
                     uint32_t& stride, uint32_t& height);

    AVCodecContext* codec_ctx_ = nullptr;
    AVPacket*       pkt_       = nullptr;
    AVFrame*        frame_     = nullptr;
    bool            initialized_ = false;
    bool            low_latency_ = true;
};
