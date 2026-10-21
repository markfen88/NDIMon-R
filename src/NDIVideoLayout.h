#pragma once
#include <cstddef>
#include <Processing.NDI.Lib.h>
#include <cstdint>
#include <limits>

enum class NDIDisplayFormat { Unsupported, UYVY, BGRX, BGRA, NV12 };

inline NDIDisplayFormat ndi_display_format(NDIlib_FourCC_video_type_e fourcc) {
    switch (fourcc) {
        case NDIlib_FourCC_video_type_UYVY:
        case NDIlib_FourCC_video_type_UYVA: return NDIDisplayFormat::UYVY;
        case NDIlib_FourCC_video_type_BGRX: return NDIDisplayFormat::BGRX;
        case NDIlib_FourCC_video_type_BGRA: return NDIDisplayFormat::BGRA;
        case NDIlib_FourCC_video_type_NV12: return NDIDisplayFormat::NV12;
        default: return NDIDisplayFormat::Unsupported;
    }
}

struct NDIVideoLayout {
    int stride = 0;
    int bytes = 0;
};

// NDI planar formats store all planes consecutively. Keep copied frames large
// enough for chroma/alpha, and reject invalid dimensions/strides before copying.
inline NDIVideoLayout ndi_video_layout(NDIlib_FourCC_video_type_e fourcc,
                                      int width, int height, int stride) {
    if (width <= 0 || height <= 0 || stride < 0) return {};
    int pixel_bytes = 1;
    switch (fourcc) {
        case NDIlib_FourCC_video_type_UYVY:
        case NDIlib_FourCC_video_type_UYVA:
        case NDIlib_FourCC_video_type_P216:
        case NDIlib_FourCC_video_type_PA16:
            if (width % 2) return {};
            pixel_bytes = 2;
            break;
        case NDIlib_FourCC_video_type_BGRA:
        case NDIlib_FourCC_video_type_BGRX:
        case NDIlib_FourCC_video_type_RGBA:
        case NDIlib_FourCC_video_type_RGBX:
            pixel_bytes = 4;
            break;
        case NDIlib_FourCC_video_type_NV12:
        case NDIlib_FourCC_video_type_I420:
        case NDIlib_FourCC_video_type_YV12:
            if (width % 2 || height % 2) return {};
            break;
        default:
            return {};
    }
    const int64_t row = int64_t(width) * pixel_bytes;
    if (!stride) {
        if (row > std::numeric_limits<int>::max()) return {};
        stride = static_cast<int>(row);
    }
    if (stride < row) return {};
    const int64_t base = int64_t(stride) * height;
    if (base > std::numeric_limits<int>::max()) return {};
    int64_t bytes = base;
    switch (fourcc) {
        case NDIlib_FourCC_video_type_NV12: bytes += int64_t(stride) * (height / 2); break;
        case NDIlib_FourCC_video_type_I420:
        case NDIlib_FourCC_video_type_YV12: bytes += 2LL * (stride / 2) * (height / 2); break;
        case NDIlib_FourCC_video_type_UYVA: bytes += int64_t(stride / 2) * height; break;
        case NDIlib_FourCC_video_type_P216: bytes += base; break;
        case NDIlib_FourCC_video_type_PA16: bytes += 2 * base; break;
        default: break;
    }
    if (bytes > std::numeric_limits<int>::max()) return {};
    return {stride, static_cast<int>(bytes)};
}
