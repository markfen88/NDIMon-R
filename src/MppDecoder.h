#pragma once
#include "VideoDecoder.h"
#include <rockchip/rk_mpi.h>
#include <rockchip/mpp_buffer.h>
#include <rockchip/mpp_frame.h>
#include <rockchip/mpp_packet.h>
#include <vector>

class MppDecoder : public VideoDecoder {
public:
    MppDecoder();
    ~MppDecoder();

    bool init(VideoCodec codec) override;
    bool decode(const uint8_t* data, size_t size, int64_t pts_us) override;
    void flush() override;
    void destroy() override;
    void release_frame(DecodedFrame& f) override;
    const char* backend_name() const override { return "mpp"; }
    bool is_hardware() const override { return true; }

private:
    void drain_frames();
    static uint32_t mpp_fmt_to_drm(MppFrameFormat fmt);

    MppCtx         ctx_       = nullptr;
    MppApi*        mpi_       = nullptr;
    MppBufferGroup buf_group_ = nullptr;
    VideoCodec     codec_     = VideoCodec::H264;
    bool           initialized_ = false;
    int64_t        pts_counter_ = 0;

    // mpp_packet_init wraps the caller's pointer; MPP's parser thread reads
    // the bitstream asynchronously after decode_put_packet returns. The caller
    // (NDIReceiver) frees the NDI frame as soon as decode() returns, so we
    // copy into a ring of decoder-owned buffers to keep the data alive until
    // MPP has consumed it.
    static constexpr int kPacketRingSize = 4;
    std::vector<uint8_t> packet_ring_[kPacketRingSize];
    int                  packet_ring_idx_ = 0;
};
