#include "SoftwareDecoder.h"
#include <iostream>
#include <cstring>
#include <drm_fourcc.h>

SoftwareDecoder::SoftwareDecoder() = default;

SoftwareDecoder::~SoftwareDecoder() {
    destroy();
}

bool SoftwareDecoder::init(VideoCodec codec) {
    AVCodecID id = (codec == VideoCodec::H265) ? AV_CODEC_ID_HEVC : AV_CODEC_ID_H264;

    const AVCodec* av_codec = avcodec_find_decoder(id);
    if (!av_codec) {
        std::cerr << "[SoftDec] avcodec_find_decoder failed\n";
        return false;
    }

    codec_ctx_ = avcodec_alloc_context3(av_codec);
    if (!codec_ctx_) return false;

    // Single-threaded: FF_THREAD_FRAME buffers multiple frames (multi-frame latency).
    // thread_count=1 with FF_THREAD_SLICE is intra-frame only — no buffering delay.
    codec_ctx_->thread_count = 1;
    codec_ctx_->thread_type  = FF_THREAD_SLICE;
    codec_ctx_->flags  |= AV_CODEC_FLAG_LOW_DELAY;
    codec_ctx_->flags2 |= AV_CODEC_FLAG2_FAST;

    if (avcodec_open2(codec_ctx_, av_codec, nullptr) < 0) {
        std::cerr << "[SoftDec] avcodec_open2 failed\n";
        avcodec_free_context(&codec_ctx_);
        return false;
    }

    pkt_   = av_packet_alloc();
    frame_ = av_frame_alloc();
    if (!pkt_ || !frame_) { destroy(); return false; }

    initialized_ = true;
    std::cout << "[SoftDec] Initialized " << av_codec->name
              << " threads=" << codec_ctx_->thread_count << "\n";
    return true;
}

bool SoftwareDecoder::decode(const uint8_t* data, size_t size, int64_t pts_us) {
    if (!initialized_) return false;

    pkt_->data = (uint8_t*)data;
    pkt_->size = (int)size;
    pkt_->pts  = pts_us;

    int ret = avcodec_send_packet(codec_ctx_, pkt_);
    if (ret < 0 && ret != AVERROR(EAGAIN)) return false;

    drain_frames();
    return true;
}

void SoftwareDecoder::drain_frames() {
    while (true) {
        int ret = avcodec_receive_frame(codec_ctx_, frame_);
        if (ret == AVERROR(EAGAIN) || ret == AVERROR_EOF) break;
        if (ret < 0) break;

        if (!frame_cb_) { av_frame_unref(frame_); continue; }

        size_t   out_size = 0;
        uint32_t stride   = 0;
        uint32_t height   = 0;
        uint8_t* nv12 = to_nv12(frame_, out_size, stride, height);

        if (nv12) {
            DecodedFrame df;
            df.opaque     = nv12;
            df.data       = nv12;
            df.data_size  = out_size;
            df.fd         = -1;
            df.width      = (uint32_t)frame_->width;
            df.height     = (uint32_t)frame_->height;
            df.hor_stride = stride;
            df.ver_stride = height;
            df.drm_format = DRM_FORMAT_NV12;

            frame_cb_(df);
            if (df.opaque) release_frame(df);
        }

        av_frame_unref(frame_);
    }
}

uint8_t* SoftwareDecoder::to_nv12(AVFrame* f, size_t& out_size,
                                    uint32_t& stride, uint32_t& height) {
    int w = f->width;
    int h = f->height;
    stride = ((uint32_t)w + 15) & ~15u; // align to 16
    height = (uint32_t)h;
    out_size = stride * h * 3 / 2;

    uint8_t* buf = new uint8_t[out_size]();
    if (!buf) return nullptr;

    uint8_t* dst_y  = buf;
    uint8_t* dst_uv = buf + stride * h;

    if (f->format == AV_PIX_FMT_YUV420P) {
        // Copy Y
        for (int row = 0; row < h; row++)
            memcpy(dst_y + row * stride,
                   f->data[0] + row * f->linesize[0], w);
        // Interleave UV
        for (int row = 0; row < h / 2; row++) {
            uint8_t*       d = dst_uv + row * stride;
            const uint8_t* u = f->data[1] + row * f->linesize[1];
            const uint8_t* v = f->data[2] + row * f->linesize[2];
            for (int col = 0; col < w / 2; col++) {
                d[col * 2]     = u[col];
                d[col * 2 + 1] = v[col];
            }
        }
    } else if (f->format == AV_PIX_FMT_NV12) {
        for (int row = 0; row < h; row++)
            memcpy(dst_y + row * stride,
                   f->data[0] + row * f->linesize[0], w);
        for (int row = 0; row < h / 2; row++)
            memcpy(dst_uv + row * stride,
                   f->data[1] + row * f->linesize[1], w);
    } else {
        // Unsupported; return black NV12
        memset(buf, 16, stride * h);         // Y=16 (black in BT.601)
        memset(dst_uv, 128, stride * h / 2); // UV=128 (neutral)
    }

    return buf;
}

void SoftwareDecoder::release_frame(DecodedFrame& f) {
    if (f.opaque) {
        delete[] (uint8_t*)f.opaque;
        f.opaque = nullptr;
    }
}

void SoftwareDecoder::flush() {
    if (!initialized_) return;
    avcodec_send_packet(codec_ctx_, nullptr);
    drain_frames();
    avcodec_flush_buffers(codec_ctx_);
}

void SoftwareDecoder::destroy() {
    if (!initialized_) return;
    if (frame_)    { av_frame_free(&frame_);     frame_    = nullptr; }
    if (pkt_)      { av_packet_free(&pkt_);      pkt_      = nullptr; }
    if (codec_ctx_){ avcodec_free_context(&codec_ctx_); codec_ctx_ = nullptr; }
    initialized_ = false;
}
