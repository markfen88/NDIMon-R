#pragma once
#include "VideoDecoder.h"
#include <string>
#include <vector>
#include <thread>
#include <atomic>
#include <linux/videodev2.h>

struct V4L2Buffer {
    void*    data   = nullptr;
    size_t   length = 0;
    int      dma_fd = -1;  // exported DMA-BUF fd (if device supports EXPBUF)
    uint32_t index  = 0;
};

class V4L2Decoder : public VideoDecoder {
public:
    V4L2Decoder();
    ~V4L2Decoder();

    bool init(VideoCodec codec) override;
    bool decode(const uint8_t* data, size_t size, int64_t pts_us) override;
    void flush() override;
    void destroy() override;
    void release_frame(DecodedFrame& f) override;

private:
    bool open_device(VideoCodec codec);
    bool configure_output(VideoCodec codec);
    bool configure_capture();
    bool alloc_buffers();
    void free_buffers();
    bool start_streaming();
    void stop_streaming();
    void capture_thread_fn();

    int fd_ = -1;

    std::vector<V4L2Buffer> out_bufs_;
    std::vector<V4L2Buffer> cap_bufs_;
    int out_buf_count_ = 4;
    int cap_buf_count_ = 8;

    uint32_t cap_width_  = 0;
    uint32_t cap_height_ = 0;
    uint32_t cap_stride_ = 0;
    bool     cap_dma_export_ = false;

    std::thread      cap_thread_;
    std::atomic<bool> streaming_{false};
    int next_out_buf_ = 0;
    bool initialized_ = false;
};
