#include "MppDecoder.h"
#include <rockchip/rk_mpi.h>
#include <rockchip/rk_mpi_cmd.h>
#include <rockchip/mpp_frame.h>
#include <rockchip/mpp_packet.h>
#include <drm_fourcc.h>
#include <iostream>
#include <cstring>

MppDecoder::MppDecoder() = default;

MppDecoder::~MppDecoder() {
    destroy();
}

uint32_t MppDecoder::mpp_fmt_to_drm(MppFrameFormat fmt) {
    switch (fmt) {
        case MPP_FMT_YUV420SP:       return DRM_FORMAT_NV12;
        case MPP_FMT_YUV420SP_10BIT: return DRM_FORMAT_NV15;
        case MPP_FMT_YUV422SP:       return DRM_FORMAT_NV16;
        default:                     return DRM_FORMAT_NV12;
    }
}

bool MppDecoder::init(VideoCodec codec) {
    codec_ = codec;

    MppCodingType coding;
    switch (codec) {
        case VideoCodec::H264:  coding = MPP_VIDEO_CodingAVC;  break;
        case VideoCodec::H265:  coding = MPP_VIDEO_CodingHEVC; break;
        case VideoCodec::MJPEG: coding = MPP_VIDEO_CodingMJPEG; break;
        default: return false;
    }

    MPP_RET ret = mpp_create(&ctx_, &mpi_);
    if (ret != MPP_OK) {
        std::cerr << "[mppdec] mpp_create failed ret " << ret << "\n";
        return false;
    }

    // Split mode: 0 = no split (NDI HX delivers complete frames)
    int split_mode = 0;
    if (mpi_->control(ctx_, MPP_DEC_SET_PARSER_SPLIT_MODE, &split_mode) != MPP_OK)
        std::cerr << "[mppdec]  mpi->control MPP_DEC_SET_PARSER_SPLIT_MODE  failed\n";
    else
        std::cout << "[mppdec]  MPP_DEC_SET_PARSER_SPLIT_MODE ok\n";

    // Fast mode for low-latency decoding
    int fast_mode = 1;
    if (mpi_->control(ctx_, MPP_DEC_SET_PARSER_FAST_MODE, &fast_mode) != MPP_OK)
        std::cerr << "[mppdec]  mpi->control MPP_DEC_SET_PARSER_FAST_MODE failed\n";
    else
        std::cout << "[mppdec]  MPP_DEC_SET_PARSER_FAST_MODE ok\n";

    ret = mpp_init(ctx_, MPP_CTX_DEC, coding);
    if (ret != MPP_OK) {
        std::cerr << "[mppdec]  mpp_init failed ret " << ret << "\n";
        mpp_destroy(ctx_);
        ctx_ = nullptr;
        return false;
    }

    int immediate = 1;
    if (mpi_->control(ctx_, MPP_DEC_SET_IMMEDIATE_OUT, &immediate) != MPP_OK)
        std::cerr << "[mppdec]  mpi->control MPP_DEC_SET_IMMEDIATE_OUT failed\n";

    ret = mpp_buffer_group_get_external(&buf_group_, MPP_BUFFER_TYPE_DRM);
    if (ret != MPP_OK) {
        std::cerr << "[mppdec] mpp_buffer_group_get_external failed ret " << ret << "\n";
        buf_group_ = nullptr;
    }

    initialized_ = true;
    std::cout << "[mppdec] initialized codec=" << (int)codec << "\n";
    return true;
}

void MppDecoder::destroy() {
    if (!initialized_) return;
    if (ctx_) {
        mpi_->reset(ctx_);
        mpp_destroy(ctx_);
        ctx_ = nullptr;
        mpi_ = nullptr;
    }
    if (buf_group_) {
        mpp_buffer_group_put(buf_group_);
        buf_group_ = nullptr;
    }
    initialized_ = false;
}

bool MppDecoder::decode(const uint8_t* data, size_t size, int64_t pts_us) {
    if (!initialized_) return false;

    MppPacket packet = nullptr;
    MPP_RET ret = mpp_packet_init(&packet, (void*)data, size);
    if (ret != MPP_OK) {
        std::cerr << "[mppdec] mpp_packet_init failed " << ret << "\n";
        return false;
    }

    mpp_packet_set_pts(packet, pts_us > 0 ? pts_us : pts_counter_++);

    ret = mpi_->decode_put_packet(ctx_, packet);
    mpp_packet_deinit(&packet);

    if (ret != MPP_OK && ret != MPP_ERR_BUFFER_FULL) {
        std::cerr << "[mppdec] decode_put_packet failed " << ret << "\n";
        return ret != MPP_NOK;
    }

    drain_frames();
    return true;
}

void MppDecoder::drain_frames() {
    MPP_RET ret;
    do {
        MppFrame frame = nullptr;
        ret = mpi_->decode_get_frame(ctx_, &frame);
        if (ret != MPP_OK || !frame) break;

        if (mpp_frame_get_info_change(frame)) {
            int width      = mpp_frame_get_width(frame);
            int height     = mpp_frame_get_height(frame);
            int hor_stride = mpp_frame_get_hor_stride(frame);
            int ver_stride = mpp_frame_get_ver_stride(frame);
            size_t buf_size = mpp_frame_get_buf_size(frame);

            std::cout << "[mppdec] info change w:" << width << " h:" << height
                      << " hor:" << hor_stride << " ver:" << ver_stride
                      << " buf:" << buf_size << "\n";

            if (buf_group_) {
                mpi_->control(ctx_, MPP_DEC_SET_EXT_BUF_GROUP, buf_group_);
                mpp_buffer_group_clear(buf_group_);
            }
            mpi_->control(ctx_, MPP_DEC_SET_INFO_CHANGE_READY, nullptr);
            mpp_frame_deinit(&frame);
            continue;
        }

        if (mpp_frame_get_discard(frame) || mpp_frame_get_errinfo(frame)) {
            mpp_frame_deinit(&frame);
            continue;
        }

        if (mpp_frame_get_eos(frame)) {
            mpp_frame_deinit(&frame);
            break;
        }

        if (frame_cb_) {
            MppBuffer buf = mpp_frame_get_buffer(frame);
            MppFrameFormat fmt = mpp_frame_get_fmt(frame);

            DecodedFrame df;
            df.opaque     = (void*)frame;
            df.width      = mpp_frame_get_width(frame);
            df.height     = mpp_frame_get_height(frame);
            df.hor_stride = mpp_frame_get_hor_stride(frame);
            df.ver_stride = mpp_frame_get_ver_stride(frame);
            df.drm_format = mpp_fmt_to_drm(fmt);

            if (buf) {
                df.fd        = mpp_buffer_get_fd(buf);
                df.data      = (uint8_t*)mpp_buffer_get_ptr(buf);
                df.data_size = mpp_buffer_get_size(buf);
            }

            frame_cb_(df);
            // If callback didn't call release_frame, release now
            if (df.opaque) {
                MppFrame mf = (MppFrame)df.opaque;
                mpp_frame_deinit(&mf);
            }
        } else {
            mpp_frame_deinit(&frame);
        }
    } while (ret == MPP_OK);
}

void MppDecoder::flush() {
    if (!initialized_) return;
    MppPacket packet = nullptr;
    mpp_packet_init(&packet, nullptr, 0);
    mpp_packet_set_eos(packet);
    mpi_->decode_put_packet(ctx_, packet);
    mpp_packet_deinit(&packet);
    drain_frames();
    mpi_->reset(ctx_);
}

void MppDecoder::release_frame(DecodedFrame& f) {
    if (f.opaque) {
        MppFrame mf = (MppFrame)f.opaque;
        mpp_frame_deinit(&mf);
        f.opaque = nullptr;
    }
}
