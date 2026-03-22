#include "V4L2Decoder.h"
#include <iostream>
#include <cstring>
#include <cerrno>
#include <fcntl.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <poll.h>
#include <drm_fourcc.h>

static int xioctl(int fd, unsigned long request, void* arg) {
    int r;
    do { r = ioctl(fd, request, arg); } while (r == -1 && errno == EINTR);
    return r;
}

V4L2Decoder::V4L2Decoder() = default;

V4L2Decoder::~V4L2Decoder() {
    destroy();
}

bool V4L2Decoder::open_device(VideoCodec codec) {
    // BCM2835 codec decode devices on Raspberry Pi
    // H.264:  /dev/video10   (bcm2835-codec-decode)
    // H.265:  /dev/video18   (bcm2835-codec-decode-hevc, RPi 4+)
    // Fallback: try /dev/video31 for hevc on some kernels
    const char* h264_devs[] = { "/dev/video10", "/dev/video19",
                                "/dev/video-dec2", "/dev/video-dec0",
                                nullptr };
    const char* hevc_devs[] = { "/dev/video18", "/dev/video31",
                                 nullptr };

    const char** devs = (codec == VideoCodec::H265) ? hevc_devs : h264_devs;

    for (int i = 0; devs[i]; i++) {
        int f = open(devs[i], O_RDWR | O_CLOEXEC | O_NONBLOCK);
        if (f < 0) continue;

        struct v4l2_capability cap = {};
        if (xioctl(f, VIDIOC_QUERYCAP, &cap) == 0) {
            bool is_m2m = (cap.capabilities & V4L2_CAP_VIDEO_M2M_MPLANE) ||
                          (cap.capabilities & V4L2_CAP_VIDEO_M2M);
            if (is_m2m) {
                fd_ = f;
                std::cout << "[V4L2Dec] Opened " << devs[i]
                          << " driver=" << cap.driver << "\n";
                return true;
            }
        }
        close(f);
    }
    return false;
}

bool V4L2Decoder::configure_output(VideoCodec codec) {
    struct v4l2_format fmt = {};
    fmt.type = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE;
    fmt.fmt.pix_mp.pixelformat = (codec == VideoCodec::H265)
        ? V4L2_PIX_FMT_HEVC : V4L2_PIX_FMT_H264;
    fmt.fmt.pix_mp.plane_fmt[0].sizeimage = 4 * 1024 * 1024; // 4MB input buffer
    fmt.fmt.pix_mp.num_planes = 1;

    if (xioctl(fd_, VIDIOC_S_FMT, &fmt) < 0) {
        std::cerr << "[V4L2Dec] VIDIOC_S_FMT OUTPUT failed errno=" << errno << "\n";
        return false;
    }
    return true;
}

bool V4L2Decoder::configure_capture() {
    // Get current CAPTURE format (decoder sets it after seeing SPS/PPS)
    struct v4l2_format fmt = {};
    fmt.type = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
    xioctl(fd_, VIDIOC_G_FMT, &fmt);

    cap_width_  = fmt.fmt.pix_mp.width  ? fmt.fmt.pix_mp.width  : 1920;
    cap_height_ = fmt.fmt.pix_mp.height ? fmt.fmt.pix_mp.height : 1080;
    cap_stride_ = fmt.fmt.pix_mp.plane_fmt[0].bytesperline
                    ? fmt.fmt.pix_mp.plane_fmt[0].bytesperline : cap_width_;

    // Request NV12 output
    fmt.fmt.pix_mp.pixelformat = V4L2_PIX_FMT_NV12;
    fmt.fmt.pix_mp.num_planes  = 1;
    xioctl(fd_, VIDIOC_S_FMT, &fmt);
    xioctl(fd_, VIDIOC_G_FMT, &fmt);

    cap_width_  = fmt.fmt.pix_mp.width;
    cap_height_ = fmt.fmt.pix_mp.height;
    cap_stride_ = fmt.fmt.pix_mp.plane_fmt[0].bytesperline;
    if (!cap_stride_) cap_stride_ = cap_width_;

    return true;
}

bool V4L2Decoder::alloc_buffers() {
    // --- OUTPUT buffers ---
    struct v4l2_requestbuffers req = {};
    req.type   = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE;
    req.memory = V4L2_MEMORY_MMAP;
    req.count  = out_buf_count_;
    if (xioctl(fd_, VIDIOC_REQBUFS, &req) < 0) {
        std::cerr << "[V4L2Dec] REQBUFS OUTPUT failed\n";
        return false;
    }
    out_buf_count_ = req.count;
    out_bufs_.resize(out_buf_count_);

    for (int i = 0; i < out_buf_count_; i++) {
        struct v4l2_buffer buf = {};
        struct v4l2_plane  plane = {};
        buf.type     = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE;
        buf.memory   = V4L2_MEMORY_MMAP;
        buf.index    = i;
        buf.m.planes = &plane;
        buf.length   = 1;
        if (xioctl(fd_, VIDIOC_QUERYBUF, &buf) < 0) return false;

        out_bufs_[i].length = plane.length;
        out_bufs_[i].index  = i;
        out_bufs_[i].data   = mmap(nullptr, plane.length,
            PROT_READ | PROT_WRITE, MAP_SHARED, fd_, plane.m.mem_offset);
        if (out_bufs_[i].data == MAP_FAILED) {
            out_bufs_[i].data = nullptr;
            return false;
        }
    }

    // --- CAPTURE buffers ---
    req = {};
    req.type   = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
    req.memory = V4L2_MEMORY_MMAP;
    req.count  = cap_buf_count_;
    if (xioctl(fd_, VIDIOC_REQBUFS, &req) < 0) {
        std::cerr << "[V4L2Dec] REQBUFS CAPTURE failed\n";
        return false;
    }
    cap_buf_count_ = req.count;
    cap_bufs_.resize(cap_buf_count_);

    cap_dma_export_ = true; // optimistically try DMA-BUF export

    for (int i = 0; i < cap_buf_count_; i++) {
        struct v4l2_buffer buf = {};
        struct v4l2_plane  plane = {};
        buf.type     = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
        buf.memory   = V4L2_MEMORY_MMAP;
        buf.index    = i;
        buf.m.planes = &plane;
        buf.length   = 1;
        if (xioctl(fd_, VIDIOC_QUERYBUF, &buf) < 0) return false;

        cap_bufs_[i].length = plane.length;
        cap_bufs_[i].index  = i;
        cap_bufs_[i].data   = mmap(nullptr, plane.length,
            PROT_READ | PROT_WRITE, MAP_SHARED, fd_, plane.m.mem_offset);
        if (cap_bufs_[i].data == MAP_FAILED) cap_bufs_[i].data = nullptr;

        // Try to export DMA-BUF for zero-copy DRM import
        struct v4l2_exportbuffer expbuf = {};
        expbuf.type  = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
        expbuf.index = i;
        expbuf.plane = 0;
        expbuf.flags = O_CLOEXEC | O_RDONLY;
        if (xioctl(fd_, VIDIOC_EXPBUF, &expbuf) == 0) {
            cap_bufs_[i].dma_fd = expbuf.fd;
        } else {
            cap_dma_export_ = false;
            cap_bufs_[i].dma_fd = -1;
        }

        // Queue the capture buffer immediately
        buf = {};
        plane = {};
        buf.type     = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
        buf.memory   = V4L2_MEMORY_MMAP;
        buf.index    = i;
        buf.m.planes = &plane;
        buf.length   = 1;
        xioctl(fd_, VIDIOC_QBUF, &buf);
    }

    if (cap_dma_export_)
        std::cout << "[V4L2Dec] DMA-BUF export enabled (zero-copy DRM path)\n";
    else
        std::cout << "[V4L2Dec] DMA-BUF export not available, using memcpy path\n";

    return true;
}

bool V4L2Decoder::start_streaming() {
    enum v4l2_buf_type type = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE;
    if (xioctl(fd_, VIDIOC_STREAMON, &type) < 0) {
        std::cerr << "[V4L2Dec] STREAMON OUTPUT failed\n";
        return false;
    }
    type = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
    if (xioctl(fd_, VIDIOC_STREAMON, &type) < 0) {
        std::cerr << "[V4L2Dec] STREAMON CAPTURE failed\n";
        return false;
    }
    return true;
}

void V4L2Decoder::stop_streaming() {
    enum v4l2_buf_type type = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE;
    xioctl(fd_, VIDIOC_STREAMOFF, &type);
    type = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
    xioctl(fd_, VIDIOC_STREAMOFF, &type);
}

bool V4L2Decoder::init(VideoCodec codec) {
    if (!open_device(codec)) {
        std::cerr << "[V4L2Dec] No compatible V4L2 M2M decode device found\n";
        return false;
    }
    if (!configure_output(codec)) { close(fd_); fd_ = -1; return false; }
    if (!configure_capture())     { close(fd_); fd_ = -1; return false; }
    if (!alloc_buffers())         { close(fd_); fd_ = -1; return false; }
    if (!start_streaming())       { close(fd_); fd_ = -1; return false; }

    streaming_ = true;
    cap_thread_ = std::thread(&V4L2Decoder::capture_thread_fn, this);

    initialized_ = true;
    std::cout << "[V4L2Dec] Ready " << cap_width_ << "x" << cap_height_
              << " stride=" << cap_stride_ << "\n";
    return true;
}

bool V4L2Decoder::decode(const uint8_t* data, size_t size, int64_t pts_us) {
    if (!initialized_ || fd_ < 0) return false;
    if (size == 0) return true;

    // Rotate through output buffers; try dequeue first to reclaim used ones
    int idx = next_out_buf_;
    next_out_buf_ = (next_out_buf_ + 1) % out_buf_count_;

    if (!out_bufs_[idx].data) return false;
    if (size > out_bufs_[idx].length) {
        std::cerr << "[V4L2Dec] Frame too large " << size
                  << " > " << out_bufs_[idx].length << "\n";
        return false;
    }

    memcpy(out_bufs_[idx].data, data, size);

    struct v4l2_buffer buf = {};
    struct v4l2_plane  plane = {};
    buf.type     = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE;
    buf.memory   = V4L2_MEMORY_MMAP;
    buf.index    = idx;
    buf.m.planes = &plane;
    buf.length   = 1;
    plane.bytesused = size;
    plane.length    = out_bufs_[idx].length;
    buf.timestamp.tv_sec  = pts_us / 1000000;
    buf.timestamp.tv_usec = pts_us % 1000000;

    if (xioctl(fd_, VIDIOC_QBUF, &buf) < 0) {
        // Buffer may still be queued from last time; try dequeueing
        struct v4l2_buffer dq = {};
        struct v4l2_plane  dqp = {};
        dq.type     = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE;
        dq.memory   = V4L2_MEMORY_MMAP;
        dq.m.planes = &dqp;
        dq.length   = 1;
        xioctl(fd_, VIDIOC_DQBUF, &dq);
        xioctl(fd_, VIDIOC_QBUF, &buf);
    }
    return true;
}

void V4L2Decoder::capture_thread_fn() {
    while (streaming_) {
        struct pollfd pfd = { fd_, POLLIN | POLLOUT, 0 };
        int r = poll(&pfd, 1, 100);
        if (r < 0 && errno != EINTR) break;
        if (r == 0) continue;

        // Dequeue consumed OUTPUT buffer (free it for reuse)
        if (pfd.revents & POLLOUT) {
            struct v4l2_buffer dq = {};
            struct v4l2_plane  dqp = {};
            dq.type     = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE;
            dq.memory   = V4L2_MEMORY_MMAP;
            dq.m.planes = &dqp;
            dq.length   = 1;
            xioctl(fd_, VIDIOC_DQBUF, &dq); // ignore errors; non-blocking
        }

        // Dequeue decoded CAPTURE buffer
        if (pfd.revents & POLLIN) {
            struct v4l2_buffer buf = {};
            struct v4l2_plane  plane = {};
            buf.type     = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
            buf.memory   = V4L2_MEMORY_MMAP;
            buf.m.planes = &plane;
            buf.length   = 1;

            if (xioctl(fd_, VIDIOC_DQBUF, &buf) < 0) continue;

            int idx = buf.index;
            if (idx < 0 || idx >= cap_buf_count_) {
                xioctl(fd_, VIDIOC_QBUF, &buf);
                continue;
            }

            // Check for resolution change event
            if (buf.flags & V4L2_BUF_FLAG_LAST) {
                // EOS or resolution change; requeue and continue
                xioctl(fd_, VIDIOC_QBUF, &buf);
                continue;
            }

            if (frame_cb_ && (cap_bufs_[idx].data || cap_bufs_[idx].dma_fd >= 0)) {
                DecodedFrame df;
                df.opaque     = (void*)(uintptr_t)(idx + 1); // 1-based to distinguish from null
                df.data       = (uint8_t*)cap_bufs_[idx].data;
                df.data_size  = plane.bytesused ? plane.bytesused : cap_bufs_[idx].length;
                df.fd         = cap_dma_export_ ? cap_bufs_[idx].dma_fd : -1;
                df.width      = cap_width_;
                df.height     = cap_height_;
                df.hor_stride = cap_stride_;
                df.ver_stride = cap_height_;
                df.drm_format = DRM_FORMAT_NV12;

                frame_cb_(df);

                // If callback didn't call release_frame, requeue now
                if (df.opaque) {
                    xioctl(fd_, VIDIOC_QBUF, &buf);
                }
            } else {
                xioctl(fd_, VIDIOC_QBUF, &buf);
            }
        }
    }
}

void V4L2Decoder::release_frame(DecodedFrame& f) {
    if (f.opaque && fd_ >= 0) {
        int idx = (int)(uintptr_t)f.opaque - 1;
        if (idx >= 0 && idx < cap_buf_count_) {
            struct v4l2_buffer buf = {};
            struct v4l2_plane  plane = {};
            buf.type     = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
            buf.memory   = V4L2_MEMORY_MMAP;
            buf.index    = idx;
            buf.m.planes = &plane;
            buf.length   = 1;
            xioctl(fd_, VIDIOC_QBUF, &buf);
        }
        f.opaque = nullptr;
    }
}

void V4L2Decoder::flush() {
    if (!initialized_ || fd_ < 0) return;
    // Send zero-length packet to signal EOS
    struct v4l2_buffer buf = {};
    struct v4l2_plane  plane = {};
    buf.type     = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE;
    buf.memory   = V4L2_MEMORY_MMAP;
    buf.index    = 0;
    buf.m.planes = &plane;
    buf.length   = 1;
    plane.bytesused = 0;
    xioctl(fd_, VIDIOC_QBUF, &buf);
}

void V4L2Decoder::free_buffers() {
    for (auto& b : out_bufs_) {
        if (b.data && b.data != MAP_FAILED) { munmap(b.data, b.length); b.data = nullptr; }
    }
    out_bufs_.clear();
    for (auto& b : cap_bufs_) {
        if (b.data && b.data != MAP_FAILED) { munmap(b.data, b.length); b.data = nullptr; }
        if (b.dma_fd >= 0) { close(b.dma_fd); b.dma_fd = -1; }
    }
    cap_bufs_.clear();
}

void V4L2Decoder::destroy() {
    if (!initialized_) return;
    streaming_ = false;
    if (cap_thread_.joinable()) cap_thread_.join();
    stop_streaming();
    free_buffers();
    if (fd_ >= 0) { close(fd_); fd_ = -1; }
    initialized_ = false;
}
