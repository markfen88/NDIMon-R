#pragma once
#include "VideoDecoder.h"
#include <rockchip/rk_mpi.h>
#include <rockchip/mpp_buffer.h>
#include <rockchip/mpp_frame.h>
#include <rockchip/mpp_packet.h>

class MppDecoder : public VideoDecoder {
public:
    MppDecoder();
    ~MppDecoder();

    bool init(VideoCodec codec) override;
    bool decode(const uint8_t* data, size_t size, int64_t pts_us) override;
    void flush() override;
    void destroy() override;
    void release_frame(DecodedFrame& f) override;

private:
    void drain_frames();
    static uint32_t mpp_fmt_to_drm(MppFrameFormat fmt);

    MppCtx         ctx_       = nullptr;
    MppApi*        mpi_       = nullptr;
    MppBufferGroup buf_group_ = nullptr;
    VideoCodec     codec_     = VideoCodec::H264;
    bool           initialized_ = false;
    int64_t        pts_counter_ = 0;
};
