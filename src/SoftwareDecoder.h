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
};
