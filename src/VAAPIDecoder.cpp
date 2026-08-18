#include "VAAPIDecoder.h"
#include <iostream>
#include <cstring>
#include <new>
#include <unistd.h>
#include <drm_fourcc.h>

extern "C" {
#include <libavutil/hwcontext_vaapi.h>
#include <libavutil/imgutils.h>
}

#include <va/va.h>
#include <va/va_drmcommon.h>

// FFmpeg get_format callback: insist on the VAAPI surface format so decoded
// frames stay on the GPU (AV_PIX_FMT_VAAPI) rather than being read back.
static enum AVPixelFormat get_vaapi_format(AVCodecContext* /*ctx*/,
                                           const enum AVPixelFormat* fmts) {
    for (const enum AVPixelFormat* p = fmts; *p != AV_PIX_FMT_NONE; p++)
        if (*p == AV_PIX_FMT_VAAPI) return *p;
    std::cerr << "[vaapi] decoder did not offer a VAAPI surface format\n";
    return AV_PIX_FMT_NONE;
}

VAAPIDecoder::VAAPIDecoder() = default;

VAAPIDecoder::~VAAPIDecoder() {
    destroy();
}

bool VAAPIDecoder::init(VideoCodec codec) {
    codec_ = codec;
    AVCodecID id = (codec == VideoCodec::H265) ? AV_CODEC_ID_HEVC : AV_CODEC_ID_H264;

    const AVCodec* dec = avcodec_find_decoder(id);
    if (!dec) { std::cerr << "[vaapi] avcodec_find_decoder failed\n"; return false; }

    // Create the VAAPI device. Default device first, then the common render node.
    if (av_hwdevice_ctx_create(&hw_device_ctx_, AV_HWDEVICE_TYPE_VAAPI,
                               nullptr, nullptr, 0) < 0 &&
        av_hwdevice_ctx_create(&hw_device_ctx_, AV_HWDEVICE_TYPE_VAAPI,
                               "/dev/dri/renderD128", nullptr, 0) < 0) {
        std::cerr << "[vaapi] av_hwdevice_ctx_create(VAAPI) failed\n";
        return false;
    }

    codec_ctx_ = avcodec_alloc_context3(dec);
    if (!codec_ctx_) { destroy(); return false; }

    codec_ctx_->hw_device_ctx = av_buffer_ref(hw_device_ctx_);
    codec_ctx_->get_format     = get_vaapi_format;
    codec_ctx_->flags2        |= AV_CODEC_FLAG2_FAST;

    if (avcodec_open2(codec_ctx_, dec, nullptr) < 0) {
        std::cerr << "[vaapi] avcodec_open2 failed\n";
        destroy();
        return false;
    }

    pkt_   = av_packet_alloc();
    frame_ = av_frame_alloc();
    if (!pkt_ || !frame_) { destroy(); return false; }

    initialized_ = true;
    std::cout << "[vaapi] initialized " << dec->name << " (zero-copy DMA-BUF)\n";
    return true;
}

bool VAAPIDecoder::decode(const uint8_t* data, size_t size, int64_t pts_us) {
    if (!initialized_) return false;

    pkt_->data = (uint8_t*)data;
    pkt_->size = (int)size;
    pkt_->pts  = pts_us;

    int ret = avcodec_send_packet(codec_ctx_, pkt_);
    if (ret < 0 && ret != AVERROR(EAGAIN)) return false;

    drain_frames();
    return true;
}

void VAAPIDecoder::drain_frames() {
    while (true) {
        int ret = avcodec_receive_frame(codec_ctx_, frame_);
        if (ret == AVERROR(EAGAIN) || ret == AVERROR_EOF) break;
        if (ret < 0) break;

        if (!frame_cb_) { av_frame_unref(frame_); continue; }

        bool ok = false;
        if (frame_->format == AV_PIX_FMT_VAAPI && zero_copy_)
            ok = emit_zero_copy(frame_);   // leaves frame_ intact on failure
        if (!ok) {
            if (frame_->format == AV_PIX_FMT_VAAPI && zero_copy_) {
                zero_copy_ = false;
                std::cerr << "[vaapi] zero-copy export unavailable — using CPU download\n";
            }
            emit_cpu_download(frame_);
        }

        av_frame_unref(frame_);
    }
}

bool VAAPIDecoder::emit_zero_copy(AVFrame* frame) {
    if (!frame->hw_frames_ctx) return false;
    AVHWFramesContext* fctx = (AVHWFramesContext*)frame->hw_frames_ctx->data;
    AVHWDeviceContext* dctx = (AVHWDeviceContext*)fctx->device_ref->data;
    AVVAAPIDeviceContext* vactx = (AVVAAPIDeviceContext*)dctx->hwctx;
    VADisplay dpy = vactx->display;
    VASurfaceID surf = (VASurfaceID)(uintptr_t)frame->data[3];

    // Make sure decode into this surface is complete before exporting.
    vaSyncSurface(dpy, surf);

    VADRMPRIMESurfaceDescriptor desc;
    memset(&desc, 0, sizeof(desc));
    VAStatus st = vaExportSurfaceHandle(
        dpy, surf, VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
        VA_EXPORT_SURFACE_READ_ONLY | VA_EXPORT_SURFACE_COMPOSED_LAYERS, &desc);
    if (st != VA_STATUS_SUCCESS) return false;

    // We import a single DMA-BUF object with one composed layer (NV12, 2 planes).
    // Anything else (multi-object) we don't handle in the zero-copy path.
    if (desc.num_objects != 1 || desc.num_layers < 1) {
        for (uint32_t i = 0; i < desc.num_objects; i++) close(desc.objects[i].fd);
        return false;
    }

    DecodedFrame df;
    df.fd            = desc.objects[0].fd;
    df.drm_modifier  = desc.objects[0].drm_format_modifier;
    df.width         = desc.width;
    df.height        = desc.height;
    df.drm_format    = desc.layers[0].drm_format;   // DRM fourcc (e.g. NV12)
    df.num_planes    = (int)desc.layers[0].num_planes;
    if (df.num_planes > 4) df.num_planes = 4;
    for (int i = 0; i < df.num_planes; i++) {
        df.plane_offset[i] = desc.layers[0].offset[i];
        df.plane_pitch[i]  = desc.layers[0].pitch[i];
    }
    df.hor_stride     = df.plane_pitch[0];
    df.ver_stride     = desc.height;
    df.prime_explicit = true;

    // Hold the AVFrame until the display is done so VAAPI won't reuse the
    // surface (release_frame frees it and closes the exported fd).
    AVFrame* held = av_frame_alloc();
    if (!held) { close(df.fd); return false; }
    av_frame_move_ref(held, frame);
    df.opaque = held;

    frame_cb_(df);
    if (df.opaque) release_frame(df);   // safety if no consumer released it
    return true;
}

bool VAAPIDecoder::emit_cpu_download(AVFrame* frame) {
    AVFrame* sw  = nullptr;
    AVFrame* src = frame;

    if (frame->hw_frames_ctx) {
        sw = av_frame_alloc();
        if (!sw) return false;
        sw->format = AV_PIX_FMT_NV12;
        if (av_hwframe_transfer_data(sw, frame, 0) < 0) {
            std::cerr << "[vaapi] av_hwframe_transfer_data failed\n";
            av_frame_free(&sw);
            return false;
        }
        src = sw;
    }

    const int w = src->width, h = src->height;
    const uint32_t stride = (uint32_t)((w + 15) & ~15);   // 16-byte aligned
    const size_t   size   = (size_t)stride * h * 3 / 2;
    uint8_t* buf = new (std::nothrow) uint8_t[size];
    if (!buf) { if (sw) av_frame_free(&sw); return false; }

    uint8_t* dst_y  = buf;
    uint8_t* dst_uv = buf + (size_t)stride * h;

    if (src->format == AV_PIX_FMT_NV12) {
        for (int row = 0; row < h; row++)
            memcpy(dst_y + (size_t)row * stride, src->data[0] + (size_t)row * src->linesize[0], w);
        for (int row = 0; row < h / 2; row++)
            memcpy(dst_uv + (size_t)row * stride, src->data[1] + (size_t)row * src->linesize[1], w);
    } else if (src->format == AV_PIX_FMT_YUV420P) {
        for (int row = 0; row < h; row++)
            memcpy(dst_y + (size_t)row * stride, src->data[0] + (size_t)row * src->linesize[0], w);
        for (int row = 0; row < h / 2; row++) {
            uint8_t*       d = dst_uv + (size_t)row * stride;
            const uint8_t* u = src->data[1] + (size_t)row * src->linesize[1];
            const uint8_t* v = src->data[2] + (size_t)row * src->linesize[2];
            for (int col = 0; col < w / 2; col++) { d[col*2] = u[col]; d[col*2+1] = v[col]; }
        }
    } else {
        memset(dst_y, 16, (size_t)stride * h);
        memset(dst_uv, 128, (size_t)stride * h / 2);
    }

    if (sw) av_frame_free(&sw);

    DecodedFrame df;
    df.data        = buf;
    df.opaque      = buf;
    df.data_size   = size;
    df.fd          = -1;
    df.width       = (uint32_t)w;
    df.height      = (uint32_t)h;
    df.hor_stride  = stride;
    df.ver_stride  = (uint32_t)h;
    df.drm_format  = DRM_FORMAT_NV12;
    df.prime_explicit = false;

    frame_cb_(df);
    if (df.opaque) release_frame(df);
    return true;
}

void VAAPIDecoder::release_frame(DecodedFrame& f) {
    if (f.fd >= 0) { close(f.fd); f.fd = -1; }
    if (f.opaque) {
        if (f.prime_explicit) {
            AVFrame* held = (AVFrame*)f.opaque;
            av_frame_free(&held);
        } else {
            delete[] (uint8_t*)f.opaque;
        }
        f.opaque = nullptr;
    }
}

void VAAPIDecoder::flush() {
    if (!initialized_) return;
    avcodec_send_packet(codec_ctx_, nullptr);
    drain_frames();
    avcodec_flush_buffers(codec_ctx_);
}

void VAAPIDecoder::destroy() {
    if (frame_)         { av_frame_free(&frame_);            frame_ = nullptr; }
    if (pkt_)           { av_packet_free(&pkt_);             pkt_ = nullptr; }
    if (codec_ctx_)     { avcodec_free_context(&codec_ctx_); codec_ctx_ = nullptr; }
    if (hw_device_ctx_) { av_buffer_unref(&hw_device_ctx_);  hw_device_ctx_ = nullptr; }
    initialized_ = false;
}
