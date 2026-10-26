#include "DRMDisplay.h"
#include "Config.h"
#include "RotationPolicy.h"
#include "SplashPaint.h"
#include "SplashStyle.h"
#include <iostream>
#include <chrono>
#include <cstring>
#include <fcntl.h>
#include <unistd.h>
#include <poll.h>
#include <sys/mman.h>
#include <drm_fourcc.h>
#include <xf86drm.h>
#include <xf86drmMode.h>
#include <algorithm>
#include <cstdint>
#include <fstream>
#include <sstream>
#include <nlohmann/json.hpp>
#include <cerrno>
#include <cstdlib>
#include <stdexcept>
#include <cmath>
#include <thread>
#include <functional>

// ---------------------------------------------------------------------------
// Compute actual refresh rate from DRM pixel clock / totals.
// Returns the rounded-to-0.01Hz value, e.g. 59.94, 60.00, 29.97, 23.98.
// ---------------------------------------------------------------------------
static double calc_refresh_hz(const drmModeModeInfo& m) {
    if (m.htotal == 0 || m.vtotal == 0) return (double)m.vrefresh;
    double hz = (double)m.clock * 1000.0 / ((double)m.htotal * (double)m.vtotal);
    return std::round(hz * 100.0) / 100.0;
}

// Collect and deduplicate modes from a connector.
// Modes with the same resolution and refresh within 0.02 Hz are collapsed
// (the preferred one wins; otherwise the first is kept).
static std::vector<DRMMode> collect_modes(drmModeConnector* conn) {
    std::vector<DRMMode> raw;
    for (int m = 0; m < conn->count_modes; m++) {
        const auto& mi = conn->modes[m];
        DRMMode dm;
        dm.width      = mi.hdisplay;
        dm.height     = mi.vdisplay;
        dm.refresh    = mi.vrefresh;
        dm.refresh_hz = calc_refresh_hz(mi);
        dm.name       = mi.name;
        dm.preferred  = (mi.type & DRM_MODE_TYPE_PREFERRED) != 0;
        raw.push_back(dm);
    }

    // Deduplicate: same (width, height, refresh_hz within 0.02 Hz)
    // Preferred mode wins over non-preferred; first seen wins otherwise.
    std::vector<DRMMode> result;
    for (auto& m : raw) {
        bool merged = false;
        for (auto& r : result) {
            if (r.width == m.width && r.height == m.height &&
                std::abs(r.refresh_hz - m.refresh_hz) < 0.02) {
                if (m.preferred && !r.preferred) r = m;  // preferred wins
                merged = true;
                break;
            }
        }
        if (!merged) result.push_back(m);
    }
    return result;
}

#define STB_IMAGE_IMPLEMENTATION
#define STBI_NO_SIMD
#define STBI_FAILURE_USERMSG
#include "stb_image.h"

// Parse "#RRGGBB" → 0xFFRRGGBB (XRGB8888)
static uint32_t hex_to_u32(const std::string& hex) {
    if (hex.size() < 7 || hex[0] != '#') return 0xFF808080u;
    try {
        uint32_t r = std::stoul(hex.substr(1,2), nullptr, 16);
        uint32_t g = std::stoul(hex.substr(3,2), nullptr, 16);
        uint32_t b = std::stoul(hex.substr(5,2), nullptr, 16);
        return 0xFF000000u | (r<<16) | (g<<8) | b;
    } catch (...) { return 0xFF808080u; }
}

// Draw an RGBA/RGB image (from stb_image) centred at (cx,cy), scaled to w pixels wide
static void draw_logo(uint32_t* pixels, uint32_t stride_u32,
                      const uint8_t* img, int img_w, int img_h, int img_c,
                      uint32_t cx, uint32_t cy, uint32_t draw_w,
                      uint32_t sw, uint32_t sh) {
    if (!img || img_w <= 0 || img_h <= 0 || draw_w == 0) return;
    uint32_t draw_h = (uint32_t)((float)img_h * draw_w / img_w);
    int x0 = (int)cx - (int)draw_w / 2;
    int y0 = (int)cy - (int)draw_h / 2;
    for (uint32_t dy = 0; dy < draw_h; dy++) {
        int py = y0 + (int)dy;
        if (py < 0 || py >= (int)sh) continue;
        int src_y = (int)((float)dy * img_h / draw_h);
        for (uint32_t dx = 0; dx < draw_w; dx++) {
            int px = x0 + (int)dx;
            if (px < 0 || px >= (int)sw) continue;
            int src_x = (int)((float)dx * img_w / draw_w);
            const uint8_t* p = img + (src_y * img_w + src_x) * img_c;
            uint8_t r = p[0], g = (img_c>1?p[1]:p[0]), b = (img_c>2?p[2]:p[0]);
            uint8_t a = (img_c==4 ? p[3] : 255);
            if (a < 8) continue;  // skip transparent pixels
            if (a == 255) {
                pixels[(uint32_t)py * stride_u32 + (uint32_t)px] =
                    0xFF000000u | ((uint32_t)r<<16) | ((uint32_t)g<<8) | b;
            } else {
                // Alpha blend over existing pixel
                uint32_t bg = pixels[(uint32_t)py * stride_u32 + (uint32_t)px];
                uint32_t br = (bg>>16)&0xFF, bgg = (bg>>8)&0xFF, bb = bg&0xFF;
                uint32_t nr = (r*a + br*(255-a)) / 255;
                uint32_t ng = (g*a + bgg*(255-a)) / 255;
                uint32_t nb = (b*a + bb*(255-a)) / 255;
                pixels[(uint32_t)py * stride_u32 + (uint32_t)px] =
                    0xFF000000u | (nr<<16) | (ng<<8) | nb;
            }
        }
    }
}

// RGA hardware color conversion removed — mainline kernels use V4L2 M2M
// driver which is incompatible with librga. NEON software path is used instead.

#include "YuvToXrgb.h"

#if defined(__aarch64__) || defined(__ARM_NEON)
// NEON row converter: UYVY source → XRGB destination (1:1, no scaling)
// Fully NEON — no scalar extraction loops inside the hot path.
// UYVY layout: [U0 Y0 V0 Y1] [U2 Y2 V2 Y3] ... (4 bytes per macro-pixel, 2 pixels each)
// vld4_u8 loads 32 bytes and deinterleaves into 4 x uint8x8 registers:
//   val[0]=U, val[1]=Y_even, val[2]=V, val[3]=Y_odd   (8 macro-pixels = 16 pixels)
static void neon_uyvy_row_to_xrgb(const uint8_t* src, uint32_t* dst, uint32_t width, const YuvCoefficients& colour) {
    uint32_t x = 0;
    // Process 16 pixels (32 bytes UYVY = 8 macro-pixels) per iteration
    for (; x + 16 <= width; x += 16) {
        uint8x8x4_t uyvy = vld4_u8(src + x * 2);
        // uyvy.val[0] = U0 U2 U4 U6 U8 U10 U12 U14  (8 U values, one per macro-pixel)
        // uyvy.val[1] = Y0 Y2 Y4 Y6 Y8 Y10 Y12 Y14  (8 even Y values)
        // uyvy.val[2] = V0 V2 V4 V6 V8 V10 V12 V14  (8 V values)
        // uyvy.val[3] = Y1 Y3 Y5 Y7 Y9 Y11 Y13 Y15  (8 odd Y values)

        // Interleave even/odd Y to get per-pixel Y order
        uint8x8x2_t y_zip = vzip_u8(uyvy.val[1], uyvy.val[3]);
        // y_zip.val[0] = Y0 Y1 Y2 Y3 Y4 Y5 Y6 Y7      (first 8 pixels)
        // y_zip.val[1] = Y8 Y9 Y10 Y11 Y12 Y13 Y14 Y15 (next 8 pixels)

        // Duplicate U/V so each pixel in the pair gets the same chroma
        uint8x8x2_t u_zip = vzip_u8(uyvy.val[0], uyvy.val[0]);
        uint8x8x2_t v_zip = vzip_u8(uyvy.val[2], uyvy.val[2]);

        // Convert first 8 pixels
        neon_yuv_to_xrgb_8px((const uint8_t*)&y_zip.val[0],
                              (const uint8_t*)&u_zip.val[0],
                              (const uint8_t*)&v_zip.val[0], dst + x, colour);
        // Convert next 8 pixels
        neon_yuv_to_xrgb_8px((const uint8_t*)&y_zip.val[1],
                              (const uint8_t*)&u_zip.val[1],
                              (const uint8_t*)&v_zip.val[1], dst + x + 8, colour);
    }
    // Process remaining 8 pixels if any
    if (x + 8 <= width) {
        // Partial load — need exactly 4 macro-pixels (16 bytes)
        uint8_t ubuf[8], vbuf[8], ybuf[8];
        const uint8_t* p = src + x * 2;
        for (int i = 0; i < 4; i++) {
            ubuf[i*2] = ubuf[i*2+1] = p[i*4];
            ybuf[i*2] = p[i*4+1];
            vbuf[i*2] = vbuf[i*2+1] = p[i*4+2];
            ybuf[i*2+1] = p[i*4+3];
        }
        neon_yuv_to_xrgb_8px(ybuf, ubuf, vbuf, dst + x, colour);
        x += 8;
    }
    // Scalar tail (< 8 pixels)
    for (; x < width; x++) {
        uint32_t pair = x & ~1u;
        int u = src[pair * 2 + 0];
        int y = src[pair * 2 + 1 + (x & 1u) * 2];
        int v = src[pair * 2 + 2];
        dst[x] = yuv_to_xrgb(y, u, v, colour);
    }
}

// NEON row converter: NV12 Y+UV → XRGB (1:1, no scaling)
// NV12 UV plane is interleaved: U0 V0 U1 V1 ... each pair covers 2 pixels.
static void neon_nv12_row_to_xrgb(const uint8_t* y_row, const uint8_t* uv_row,
                                    uint32_t* dst, uint32_t width) {
    uint32_t x = 0;
    for (; x + 16 <= width; x += 16) {
        // Load 16 UV bytes → 8 U/V pairs for 16 pixels
        uint8x8x2_t uv = vld2_u8(uv_row + (x & ~1u));
        // uv.val[0] = U0 U1 U2 U3 U4 U5 U6 U7 (one per pair)
        // uv.val[1] = V0 V1 V2 V3 V4 V5 V6 V7

        // Duplicate each U/V for both pixels in the pair
        uint8x8x2_t u_zip = vzip_u8(uv.val[0], uv.val[0]);
        uint8x8x2_t v_zip = vzip_u8(uv.val[1], uv.val[1]);

        neon_yuv_to_xrgb_8px(y_row + x, (const uint8_t*)&u_zip.val[0],
                              (const uint8_t*)&v_zip.val[0], dst + x);
        neon_yuv_to_xrgb_8px(y_row + x + 8, (const uint8_t*)&u_zip.val[1],
                              (const uint8_t*)&v_zip.val[1], dst + x + 8);
    }
    for (; x + 8 <= width; x += 8) {
        uint8_t ubuf[8], vbuf[8];
        for (int i = 0; i < 8; i++) {
            ubuf[i] = uv_row[(x + i) & ~1u];
            vbuf[i] = uv_row[((x + i) & ~1u) + 1];
        }
        neon_yuv_to_xrgb_8px(y_row + x, ubuf, vbuf, dst + x);
    }
    for (; x < width; x++) {
        int y = y_row[x];
        int u = uv_row[x & ~1u];
        int v = uv_row[(x & ~1u) + 1];
        dst[x] = yuv_to_xrgb(y, u, v);
    }
}
// NEON-accelerated scaling UYVY→XRGB: convert source row with NEON, then
// horizontal nearest-neighbor resample. Much faster than scalar per-pixel YUV math.
static void neon_uyvy_scale_row_to_xrgb(const uint8_t* src_row, uint32_t src_w,
                                          uint32_t* dst_row, uint32_t dst_w,
                                          uint32_t x_step_fp16, const YuvCoefficients& colour) {
    // Convert full source row to XRGB via NEON into a temp buffer
    // Use thread_local to avoid per-call allocation
    thread_local std::vector<uint32_t> line_buf;
    if (line_buf.size() < src_w) line_buf.resize(src_w);

    neon_uyvy_row_to_xrgb(src_row, line_buf.data(), src_w, colour);

    // Horizontal nearest-neighbor resample (just uint32_t lookups — very fast)
    uint32_t sx_fp = 0;
    for (uint32_t dx = 0; dx < dst_w; dx++, sx_fp += x_step_fp16) {
        dst_row[dx] = line_buf[sx_fp >> 16];
    }
}

// NEON-accelerated scaling NV12→XRGB: same two-pass approach
static void neon_nv12_scale_row_to_xrgb(const uint8_t* y_row, const uint8_t* uv_row,
                                          uint32_t src_w,
                                          uint32_t* dst_row, uint32_t dst_w,
                                          uint32_t x_step_fp16) {
    thread_local std::vector<uint32_t> line_buf;
    if (line_buf.size() < src_w) line_buf.resize(src_w);

    neon_nv12_row_to_xrgb(y_row, uv_row, line_buf.data(), src_w);

    uint32_t sx_fp = 0;
    for (uint32_t dx = 0; dx < dst_w; dx++, sx_fp += x_step_fp16) {
        dst_row[dx] = line_buf[sx_fp >> 16];
    }
}
#endif // __aarch64__ || __ARM_NEON

DRMDisplay::DRMDisplay() = default;

DRMDisplay::~DRMDisplay() {
    destroy();
}

// ---------------------------------------------------------------------------
// Static utility: enumerate all connectors
// ---------------------------------------------------------------------------
std::vector<ConnectorInfo> DRMDisplay::enumerate_connectors(const std::string& device) {
    std::vector<ConnectorInfo> result;

    int fd = open(device.c_str(), O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        std::cerr << "[DRM] enumerate_connectors: cannot open " << device << "\n";
        return result;
    }

    // Extract "cardN" basename for sysfs lookups (/dev/dri/card1 → card1)
    std::string card_basename = "card0";
    {
        auto pos = device.rfind('/');
        if (pos != std::string::npos && pos + 1 < device.size())
            card_basename = device.substr(pos + 1);
    }

    drmSetClientCap(fd, DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1);

    drmModeRes* res = drmModeGetResources(fd);
    if (!res) {
        close(fd);
        return result;
    }

    for (int i = 0; i < res->count_connectors; i++) {
        drmModeConnector* conn = drmModeGetConnector(fd, res->connectors[i]);
        if (!conn) continue;

        ConnectorInfo info;
        info.id = conn->connector_id;

        // Build connector name
        const char* type_names[] = {
            "Unknown", "VGA", "DVII", "DVID", "DVIA",
            "Composite", "SVIDEO", "LVDS", "Component",
            "9PinDIN", "DisplayPort", "HDMIA", "HDMIB",
            "TV", "eDP", "VIRTUAL", "DSI", "DPI", "WRITEBACK", "SPI", "USB"
        };
        const char* type_str = "Unknown";
        if (conn->connector_type < (sizeof(type_names)/sizeof(type_names[0])))
            type_str = type_names[conn->connector_type];

        // Map to standard names
        std::string type_name;
        switch (conn->connector_type) {
            case DRM_MODE_CONNECTOR_HDMIA:  type_name = "HDMI-A"; break;
            case DRM_MODE_CONNECTOR_HDMIB:  type_name = "HDMI-B"; break;
            case DRM_MODE_CONNECTOR_DisplayPort: type_name = "DP"; break;
            case DRM_MODE_CONNECTOR_VGA:    type_name = "VGA"; break;
            case DRM_MODE_CONNECTOR_LVDS:   type_name = "LVDS"; break;
            case DRM_MODE_CONNECTOR_eDP:    type_name = "eDP"; break;
            default: type_name = std::string(type_str); break;
        }
        info.name = type_name + "-" + std::to_string(conn->connector_type_id);

        // Check connection via sysfs first (more reliable at boot)
        {
            std::string sysfs_path = "/sys/class/drm/" + card_basename + "-" + info.name + "/status";
            std::ifstream sf(sysfs_path);
            if (sf.is_open()) {
                std::string status;
                sf >> status;
                info.connected = (status == "connected");
            } else {
                info.connected = (conn->connection == DRM_MODE_CONNECTED);
            }
        }

        info.modes = collect_modes(conn);
        result.push_back(info);
        drmModeFreeConnector(conn);
    }

    drmModeFreeResources(res);
    close(fd);
    return result;
}

// ---------------------------------------------------------------------------
// create_lease — open a DRM lease for a specific connector
// The lease gives the holder DRM master rights for its connector/CRTC/planes.
// master_fd must remain open as long as the lease fd is in use.
// ---------------------------------------------------------------------------
int DRMDisplay::create_lease(int master_fd, const std::string& connector_name,
                             const std::set<uint32_t>& excluded_crtcs,
                             uint32_t* selected_crtc_out) {
    // Need to see all planes
    drmSetClientCap(master_fd, DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1);

    drmModeRes* res = drmModeGetResources(master_fd);
    if (!res) {
        std::cerr << "[DRM] create_lease: drmModeGetResources failed\n";
        return -1;
    }

    uint32_t conn_id = 0, crtc_id = 0;
    int crtc_index = -1;

    for (int i = 0; i < res->count_connectors; i++) {
        drmModeConnector* conn = drmModeGetConnector(master_fd, res->connectors[i]);
        if (!conn) continue;

        std::string type_name;
        switch (conn->connector_type) {
            case DRM_MODE_CONNECTOR_HDMIA:       type_name = "HDMI-A"; break;
            case DRM_MODE_CONNECTOR_HDMIB:       type_name = "HDMI-B"; break;
            case DRM_MODE_CONNECTOR_DisplayPort: type_name = "DP";     break;
            default:                             type_name = "Unknown"; break;
        }
        std::string this_name = type_name + "-" + std::to_string(conn->connector_type_id);

        if (this_name.find(connector_name) == std::string::npos &&
            connector_name.find(this_name) == std::string::npos) {
            drmModeFreeConnector(conn);
            continue;
        }

        conn_id = conn->connector_id;

        // 1. Try current encoder's CRTC (only if not excluded by another lease)
        if (conn->encoder_id) {
            drmModeEncoder* enc = drmModeGetEncoder(master_fd, conn->encoder_id);
            if (enc) {
                if (enc->crtc_id && !excluded_crtcs.count(enc->crtc_id))
                    crtc_id = enc->crtc_id;
                drmModeFreeEncoder(enc);
            }
        }
        // 2. Try possible encoders — skip CRTCs already claimed by another lease
        for (int j = 0; j < conn->count_encoders && !crtc_id; j++) {
            drmModeEncoder* enc = drmModeGetEncoder(master_fd, conn->encoders[j]);
            if (!enc) continue;
            for (int k = 0; k < res->count_crtcs; k++) {
                if (!(enc->possible_crtcs & (1u << k))) continue;
                if (excluded_crtcs.count(res->crtcs[k])) continue;
                crtc_id = res->crtcs[k];
                crtc_index = k;
                break;
            }
            drmModeFreeEncoder(enc);
        }
        drmModeFreeConnector(conn);
        break;
    }

    // Find crtc_index (position in res->crtcs[]) if not set
    if (crtc_id && crtc_index < 0) {
        for (int k = 0; k < res->count_crtcs; k++) {
            if (res->crtcs[k] == crtc_id) { crtc_index = k; break; }
        }
    }
    drmModeFreeResources(res);

    if (!conn_id || !crtc_id) {
        std::cerr << "[DRM] create_lease: no connector/CRTC found for '"
                  << connector_name << "'\n";
        return -1;
    }

    // The kernel's validate_lease() requires at least one plane in every lease.
    // To avoid EBUSY conflicts between leases, prefer planes that exclusively
    // belong to this CRTC (possible_crtcs has exactly one bit set).
    // On RK3588 VOP2, shared overlay planes set multiple bits — skip those.
    std::vector<uint32_t> objects = { conn_id, crtc_id };
    {
        drmModePlaneRes* pres = drmModeGetPlaneResources(master_fd);
        if (pres) {
            uint32_t crtc_bit = (crtc_index >= 0) ? (1u << crtc_index) : 0;
            // Pass 1: exclusive planes (possible_crtcs == crtc_bit only)
            for (uint32_t p = 0; p < pres->count_planes && objects.size() < 3; p++) {
                drmModePlane* plane = drmModeGetPlane(master_fd, pres->planes[p]);
                if (!plane) continue;
                if (crtc_bit && plane->possible_crtcs == crtc_bit)
                    objects.push_back(plane->plane_id);
                drmModeFreePlane(plane);
            }
            // Pass 2: if no exclusive plane found, take the first compatible one
            if (objects.size() < 3) {
                for (uint32_t p = 0; p < pres->count_planes && objects.size() < 3; p++) {
                    drmModePlane* plane = drmModeGetPlane(master_fd, pres->planes[p]);
                    if (!plane) continue;
                    bool ok = (crtc_bit == 0) || (plane->possible_crtcs & crtc_bit);
                    if (ok) objects.push_back(plane->plane_id);
                    drmModeFreePlane(plane);
                }
            }
            drmModeFreePlaneResources(pres);
        }
    }

    uint32_t lessee_id = 0;
    int lease_fd = drmModeCreateLease(master_fd, objects.data(),
                                      (int)objects.size(), 0, &lessee_id);
    if (lease_fd < 0) {
        std::cerr << "[DRM] drmModeCreateLease failed for '" << connector_name
                  << "': " << strerror(errno) << "\n";
    } else {
        std::cout << "[DRM] Lease created for " << connector_name
                  << " (conn=" << conn_id << " crtc=" << crtc_id
                  << " planes=" << (objects.size()-2) << ")\n";
        if (selected_crtc_out) *selected_crtc_out = crtc_id;
    }
    return lease_fd;
}

// ---------------------------------------------------------------------------
// init with pre-opened fd (DRM lease)
// ---------------------------------------------------------------------------
bool DRMDisplay::init(int fd, const std::string& connector_name,
                      const std::string& preferred_mode,
                      const std::string& device_path) {
    device_path_    = device_path;
    connector_name_ = connector_name;
    preferred_mode_ = preferred_mode;
    drm_fd_         = fd;
    owns_fd_        = false;

    std::cout << "[DRM] Using lease fd for " << connector_name << "\n";

    drmSetClientCap(drm_fd_, DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1);
    drmSetClientCap(drm_fd_, DRM_CLIENT_CAP_ATOMIC, 1);

    if (!setup_crtc(connector_name, preferred_mode)) {
        std::cerr << "[DRM] No connected display found for connector '"
                  << connector_name << "' — will retry on hotplug\n";
        return false;
    }

    initialized_ = true;
    std::cout << "[DRM] Initialized " << width_ << "x" << height_
              << "@" << vrefresh_ << " on " << connector_name_ << "\n";
    return true;
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------
bool DRMDisplay::init(const std::string& device,
                      const std::string& connector_name,
                      const std::string& preferred_mode) {
    device_path_    = device;
    connector_name_ = connector_name;
    preferred_mode_ = preferred_mode;

    drm_fd_ = open(device.c_str(), O_RDWR | O_CLOEXEC);
    if (drm_fd_ < 0) {
        std::cerr << "[DRM] Open " << device << " failed: " << strerror(errno) << "\n";
        return false;
    }
    std::cout << "[DRM] Opened " << device << "\n";

    if (drmSetMaster(drm_fd_) != 0) {
        std::cerr << "[DRM] drmSetMaster failed — may not have exclusive display access\n";
    }

    drmSetClientCap(drm_fd_, DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1);
    drmSetClientCap(drm_fd_, DRM_CLIENT_CAP_ATOMIC, 1);

    if (!setup_crtc(connector_name, preferred_mode)) {
        std::cerr << "[DRM] No connected display found for connector '"
                  << (connector_name.empty() ? "(any)" : connector_name)
                  << "' — will retry on hotplug\n";
        // Do NOT close drm_fd_ — keep it open for check_hotplug()
        return false;
    }

    initialized_ = true;
    std::cout << "[DRM] Initialized " << width_ << "x" << height_
              << "@" << vrefresh_ << " on " << connector_name_ << "\n";
    return true;
}

// Forward declaration — defined below atomic_plane_commit
static uint32_t get_prop_id(int fd, uint32_t obj_id, uint32_t obj_type, const char* name);

// ---------------------------------------------------------------------------
// setup_crtc
// ---------------------------------------------------------------------------
bool DRMDisplay::setup_crtc(const std::string& connector_name,
                              const std::string& preferred_mode) {
    drmModeRes* res = drmModeGetResources(drm_fd_);
    if (!res) {
        std::cerr << "[DRM] drmModeGetResources failed\n";
        return false;
    }

    bool found = false;

    for (int i = 0; i < res->count_connectors && !found; i++) {
        drmModeConnector* conn = drmModeGetConnector(drm_fd_, res->connectors[i]);
        if (!conn) continue;

        // Build this connector's name
        std::string type_name;
        switch (conn->connector_type) {
            case DRM_MODE_CONNECTOR_HDMIA:  type_name = "HDMI-A"; break;
            case DRM_MODE_CONNECTOR_HDMIB:  type_name = "HDMI-B"; break;
            case DRM_MODE_CONNECTOR_DisplayPort: type_name = "DP"; break;
            case DRM_MODE_CONNECTOR_VGA:    type_name = "VGA"; break;
            case DRM_MODE_CONNECTOR_LVDS:   type_name = "LVDS"; break;
            case DRM_MODE_CONNECTOR_eDP:    type_name = "eDP"; break;
            default: type_name = "Unknown"; break;
        }
        std::string this_name = type_name + "-" + std::to_string(conn->connector_type_id);

        // Filter by connector name if specified
        if (!connector_name.empty()) {
            if (this_name.find(connector_name) == std::string::npos &&
                connector_name.find(this_name) == std::string::npos) {
                drmModeFreeConnector(conn);
                continue;
            }
        }

        // Check connection (sysfs is more reliable)
        bool is_connected = false;
        {
            std::string card_base = device_path_.empty() ? "card0"
                : device_path_.substr(device_path_.rfind('/') + 1);
            std::string sysfs = "/sys/class/drm/" + card_base + "-" + this_name + "/status";
            std::ifstream sf(sysfs);
            if (sf.is_open()) {
                std::string st;
                sf >> st;
                is_connected = (st == "connected");
            } else {
                is_connected = (conn->connection == DRM_MODE_CONNECTED);
            }
        }

        if (!is_connected || conn->count_modes == 0) {
            drmModeFreeConnector(conn);
            continue;
        }

        // Collect and deduplicate modes
        available_modes_ = collect_modes(conn);

        // Find encoder -> CRTC
        drmModeEncoder* enc = nullptr;
        // First try the current encoder
        for (int j = 0; j < res->count_encoders; j++) {
            enc = drmModeGetEncoder(drm_fd_, res->encoders[j]);
            if (enc && enc->encoder_id == conn->encoder_id) break;
            drmModeFreeEncoder(enc);
            enc = nullptr;
        }
        // If no current encoder, find a compatible one
        if (!enc) {
            for (int j = 0; j < res->count_encoders; j++) {
                enc = drmModeGetEncoder(drm_fd_, res->encoders[j]);
                if (!enc) continue;
                if (enc->possible_crtcs) break;
                drmModeFreeEncoder(enc);
                enc = nullptr;
            }
        }
        // On a lease fd, encoders are not leased — fall back to the leased CRTC directly
        uint32_t crtc_id = 0;
        if (enc) {
            crtc_id = enc->crtc_id;
            if (!crtc_id) {
                for (int k = 0; k < res->count_crtcs; k++) {
                    if (enc->possible_crtcs & (1u << k)) {
                        crtc_id = res->crtcs[k];
                        break;
                    }
                }
            }
            if (!crtc_id) {
                drmModeFreeEncoder(enc);
                drmModeFreeConnector(conn);
                continue;
            }
        } else if (res->count_crtcs > 0) {
            // Lease fd: no encoder visible, use the single leased CRTC
            crtc_id = res->crtcs[0];
        } else {
            drmModeFreeConnector(conn);
            continue;
        }

        // Pick mode
        drmModeModeInfo best_mode = conn->modes[0];
        bool matched = false;

        if (!preferred_mode.empty()) {
            for (int m = 0; m < conn->count_modes; m++) {
                std::string mname = conn->modes[m].name;
                if (mname.find(preferred_mode) != std::string::npos ||
                    preferred_mode == mname) {
                    best_mode = conn->modes[m];
                    matched   = true;
                    break;
                }
            }
        }
        if (!matched) {
            for (int m = 0; m < conn->count_modes; m++) {
                if (conn->modes[m].type & DRM_MODE_TYPE_PREFERRED) {
                    best_mode = conn->modes[m];
                    break;
                }
            }
        }

        connector_id_   = conn->connector_id;
        crtc_id_        = crtc_id;
        mode_           = best_mode;
        width_          = best_mode.hdisplay;
        height_         = best_mode.vdisplay;
        vrefresh_       = best_mode.vrefresh;
        vrefresh_hz_    = calc_refresh_hz(best_mode);
        connector_name_ = this_name;

        // Find the primary plane for this CRTC (needed for atomic plane commits)
        plane_id_ = 0;
        atomic_plane_state_ = 0;
        unsafe_linear_rotation_ = true;
        rotation_supported_mask_ = 0;
        rotation_prop_id_ = 0;
        {
            drmModePlaneRes* pres = drmModeGetPlaneResources(drm_fd_);
            if (pres) {
                // Find CRTC index for possible_crtcs bitmask
                int crtc_index = -1;
                for (int k = 0; k < res->count_crtcs; k++) {
                    if (res->crtcs[k] == crtc_id) { crtc_index = k; break; }
                }
                uint32_t crtc_bit = (crtc_index >= 0) ? (1u << crtc_index) : 0;

                // Prefer a plane that supports NV12 (for zero-copy DMA-BUF path)
                for (uint32_t p = 0; p < pres->count_planes; p++) {
                    drmModePlane* plane = drmModeGetPlane(drm_fd_, pres->planes[p]);
                    if (!plane) continue;
                    if (crtc_bit && !(plane->possible_crtcs & crtc_bit)) {
                        drmModeFreePlane(plane);
                        continue;
                    }
                    bool has_nv12 = false;
                    for (uint32_t f = 0; f < plane->count_formats; f++) {
                        if (plane->formats[f] == DRM_FORMAT_NV12) { has_nv12 = true; break; }
                    }
                    if (has_nv12) {
                        plane_id_ = plane->plane_id;
                        drmModeFreePlane(plane);
                        break;
                    }
                    // Remember first compatible plane as fallback
                    if (!plane_id_) plane_id_ = plane->plane_id;
                    drmModeFreePlane(plane);
                }
                drmModeFreePlaneResources(pres);
            }
            if (plane_id_) {
                std::cout << "[DRM] Primary plane " << plane_id_ << " for CRTC " << crtc_id << "\n";
                // Cache plane / crtc / connector property IDs once. atomic_plane_commit
                // would otherwise enumerate them on every frame at 60 fps.
                prop_plane_fb_id_   = get_prop_id(drm_fd_, plane_id_,     DRM_MODE_OBJECT_PLANE,     "FB_ID");
                prop_plane_crtc_id_ = get_prop_id(drm_fd_, plane_id_,     DRM_MODE_OBJECT_PLANE,     "CRTC_ID");
                prop_plane_src_x_   = get_prop_id(drm_fd_, plane_id_,     DRM_MODE_OBJECT_PLANE,     "SRC_X");
                prop_plane_src_y_   = get_prop_id(drm_fd_, plane_id_,     DRM_MODE_OBJECT_PLANE,     "SRC_Y");
                prop_plane_src_w_   = get_prop_id(drm_fd_, plane_id_,     DRM_MODE_OBJECT_PLANE,     "SRC_W");
                prop_plane_src_h_   = get_prop_id(drm_fd_, plane_id_,     DRM_MODE_OBJECT_PLANE,     "SRC_H");
                prop_plane_crtc_x_  = get_prop_id(drm_fd_, plane_id_,     DRM_MODE_OBJECT_PLANE,     "CRTC_X");
                prop_plane_crtc_y_  = get_prop_id(drm_fd_, plane_id_,     DRM_MODE_OBJECT_PLANE,     "CRTC_Y");
                prop_plane_crtc_w_  = get_prop_id(drm_fd_, plane_id_,     DRM_MODE_OBJECT_PLANE,     "CRTC_W");
                prop_plane_crtc_h_  = get_prop_id(drm_fd_, plane_id_,     DRM_MODE_OBJECT_PLANE,     "CRTC_H");
                prop_conn_crtc_id_  = get_prop_id(drm_fd_, connector_id_, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID");
                prop_crtc_active_   = get_prop_id(drm_fd_, crtc_id_,      DRM_MODE_OBJECT_CRTC,      "ACTIVE");
                prop_crtc_mode_id_  = get_prop_id(drm_fd_, crtc_id_,      DRM_MODE_OBJECT_CRTC,      "MODE_ID");
                rotation_prop_id_   = get_prop_id(drm_fd_, plane_id_,     DRM_MODE_OBJECT_PLANE,     "rotation");
                rotation_supported_mask_ = 0;
                if (rotation_prop_id_) {
                    auto* prop = drmModeGetProperty(drm_fd_, rotation_prop_id_);
                    if (prop) {
                        for (int i=0;i<prop->count_enums;++i)
                            if (prop->enums[i].value < 64)
                                rotation_supported_mask_ |= uint64_t(1) << prop->enums[i].value;
                        drmModeFreeProperty(prop);
                    }
                }
                if (rotation_prop_id_)
                    std::cout << "[DRM] Rotation property available on plane " << plane_id_ << "\n";

                // Rockchip Cluster transforms require AFBC, whereas our splash
                // and software conversion buffers are linear. Some BSP kernels
                // accept TEST_ONLY yet fault the IOMMU on the actual scanout.
                // NAME is an immutable bitmask: decode its active enum bit.
                bool rockchip = true; // Unknown driver identity fails closed.
                if (auto* version = drmGetVersion(drm_fd_)) {
                    rockchip = !version->name || version->name_len <= 0 ||
                        std::string(version->name, version->name_len) == "rockchip";
                    drmFreeVersion(version);
                }
                std::string plane_name;
                if (auto* props = drmModeObjectGetProperties(drm_fd_, plane_id_, DRM_MODE_OBJECT_PLANE)) {
                    for (uint32_t p = 0; p < props->count_props; ++p) {
                        auto* prop = drmModeGetProperty(drm_fd_, props->props[p]);
                        if (!prop) continue;
                        if (std::strcmp(prop->name, "NAME") == 0 &&
                            (prop->flags & DRM_MODE_PROP_BITMASK)) {
                            for (int e = 0; e < prop->count_enums; ++e) {
                                const auto bit = prop->enums[e].value;
                                if (bit < 64 && props->prop_values[p] == (uint64_t(1) << bit))
                                    plane_name = prop->enums[e].name;
                            }
                        }
                        drmModeFreeProperty(prop);
                    }
                    drmModeFreeObjectProperties(props);
                }
                const bool known_smart = plane_name.rfind("Esmart", 0) == 0 ||
                                         plane_name.rfind("Smart", 0) == 0;
                unsafe_linear_rotation_ = rockchip && !known_smart;
                std::cout << "[DRM] Rotation policy plane=" << plane_id_
                          << " name=" << (plane_name.empty() ? "unknown" : plane_name)
                          << " advertised_mask=" << rotation_supported_mask_
                          << " linear_transforms=" << !unsafe_linear_rotation_ << "\n";
            }
        }
        if (!rotation_allowed(rotation_degrees_)) {
            rotation_degrees_ = 0;
            rotation_drm_ = 1;
            rotation_dirty_ = true;
        }

        std::cout << "[DRM] Connector " << this_name
                  << " mode " << best_mode.name
                  << " " << best_mode.hdisplay << "x" << best_mode.vdisplay
                  << "@" << best_mode.vrefresh << "\n";

        // Allocate both framebuffers
        for (auto& b : fb_) {
            free_fb(b);
            if (!alloc_fb(b, width_, height_)) {
                std::cerr << "[DRM] Failed to allocate framebuffer\n";
                drmModeFreeEncoder(enc);
                drmModeFreeConnector(conn);
                drmModeFreeResources(res);
                return false;
            }
        }

        drmModeFreeEncoder(enc);
        drmModeFreeConnector(conn);
        found = true;
    }

    drmModeFreeResources(res);
    return found;
}

// ---------------------------------------------------------------------------
// check_hotplug
// ---------------------------------------------------------------------------
bool DRMDisplay::check_hotplug() {
    if (initialized_) return false;
    if (drm_fd_ < 0) return false;

    // Check sysfs status
    std::string check_name = connector_name_.empty() ? "HDMI-A-1" : connector_name_;
    std::string card_base = device_path_.empty() ? "card0"
        : device_path_.substr(device_path_.rfind('/') + 1);
    std::string sysfs = "/sys/class/drm/" + card_base + "-" + check_name + "/status";
    std::ifstream sf(sysfs);
    if (!sf.is_open()) return false;

    std::string status;
    sf >> status;
    if (status != "connected") return false;

    std::cout << "[DRM] Hotplug detected on " << check_name << ", initializing...\n";
    if (!setup_crtc(connector_name_, preferred_mode_)) {
        std::cerr << "[DRM] Hotplug setup_crtc failed\n";
        return false;
    }

    initialized_ = true;
    crtc_active_ = false;
    std::cout << "[DRM] Hotplug init complete: " << width_ << "x" << height_
              << "@" << vrefresh_ << "\n";
    show_splash(false);
    return true;
}

// ---------------------------------------------------------------------------
// alloc_fb / free_fb
// ---------------------------------------------------------------------------
bool DRMDisplay::alloc_fb(DRMBuffer& buf, uint32_t w, uint32_t h) {
    struct drm_mode_create_dumb create_dumb = {};
    create_dumb.width  = w;
    create_dumb.height = h;
    create_dumb.bpp    = 32;
    if (drmIoctl(drm_fd_, DRM_IOCTL_MODE_CREATE_DUMB, &create_dumb) < 0) {
        std::cerr << "[DRM] DRM_IOCTL_MODE_CREATE_DUMB failed: " << strerror(errno) << "\n";
        return false;
    }

    buf.bo_handle = create_dumb.handle;
    buf.stride    = create_dumb.pitch;
    buf.size      = create_dumb.size;
    buf.width     = w;
    buf.height    = h;

    uint32_t handles[4] = { buf.bo_handle };
    uint32_t strides[4] = { buf.stride };
    uint32_t offsets[4] = { 0 };

    if (drmModeAddFB2(drm_fd_, w, h, DRM_FORMAT_XRGB8888,
                      handles, strides, offsets, &buf.fb_id, 0) != 0) {
        std::cerr << "[DRM] drmModeAddFB2 failed: " << strerror(errno) << "\n";
        struct drm_mode_destroy_dumb d = {};
        d.handle = buf.bo_handle;
        drmIoctl(drm_fd_, DRM_IOCTL_MODE_DESTROY_DUMB, &d);
        buf.bo_handle = 0;
        return false;
    }

    struct drm_mode_map_dumb map_dumb = {};
    map_dumb.handle = buf.bo_handle;
    if (drmIoctl(drm_fd_, DRM_IOCTL_MODE_MAP_DUMB, &map_dumb) == 0) {
        buf.map = mmap(nullptr, buf.size, PROT_READ | PROT_WRITE, MAP_SHARED,
                       drm_fd_, map_dumb.offset);
        if (buf.map == MAP_FAILED) {
            buf.map = nullptr;
            std::cerr << "[DRM] mmap of dumb buffer failed: " << strerror(errno) << "\n";
        }
    }

    return true;
}

void DRMDisplay::free_fb(DRMBuffer& buf) {
    if (buf.map && buf.map != MAP_FAILED) {
        munmap(buf.map, buf.size);
        buf.map = nullptr;
    }
    if (buf.fb_id) {
        drmModeRmFB(drm_fd_, buf.fb_id);
        buf.fb_id = 0;
    }
    if (buf.bo_handle) {
        struct drm_mode_destroy_dumb d = {};
        d.handle = buf.bo_handle;
        drmIoctl(drm_fd_, DRM_IOCTL_MODE_DESTROY_DUMB, &d);
        buf.bo_handle = 0;
    }
    if (buf.dma_fd >= 0) { close(buf.dma_fd); buf.dma_fd = -1; }
    buf.width = buf.height = buf.stride = buf.size = 0;
}

// ---------------------------------------------------------------------------
// commit_fb / wait_for_flip / flip_handler
// ---------------------------------------------------------------------------
void DRMDisplay::flip_handler(int /*fd*/, unsigned /*seq*/,
                               unsigned /*tv_sec*/, unsigned /*tv_usec*/,
                               void* user) {
    DRMDisplay* self = static_cast<DRMDisplay*>(user);
    self->flip_pending_ = false;
}

void DRMDisplay::wait_for_flip() {
    if (!flip_pending_.load()) return;

    drmEventContext ev = {};
    ev.version = DRM_EVENT_CONTEXT_VERSION;
    ev.page_flip_handler = flip_handler;

    // Wait up to 50ms for the vsync flip event — must be longer than one
    // full frame period (16.6ms at 60Hz, 33.3ms at 30Hz, 40ms at 25Hz).
    // A shorter timeout (like 5ms) can consistently miss the vsync under
    // load, leaving flip_pending_ stuck true and freezing the display.
    struct pollfd pfd = { drm_fd_, POLLIN, 0 };
    int r = poll(&pfd, 1, 50);
    if (r > 0 && (pfd.revents & POLLIN))
        drmHandleEvent(drm_fd_, &ev);
}

bool DRMDisplay::commit_fb(uint32_t fb_id) {
    // Atomic modesetting is required when: (a) rotation is active, or
    // (b) a previous atomic_plane_commit established the CRTC in atomic mode
    // (legacy SetCrtc/PageFlip fail after atomic has been used).
    if (plane_id_ && (rotation_drm_ != 1 || rotation_dirty_ || atomic_plane_state_ == 1)) {
        Rect full_src = {0, 0, width_, height_};
        Rect full_dst = {0, 0, width_, height_};
        switch (atomic_plane_commit(fb_id, full_src, full_dst)) {
            case CommitResult::Ok:      return true;
            case CommitResult::Dropped: return false;
            case CommitResult::Failed:  return false;
        }
        return false;
    }

    if (!crtc_active_) {
        // First commit: use SetCrtc
        int ret = drmModeSetCrtc(drm_fd_, crtc_id_, fb_id,
                                  0, 0, &connector_id_, 1, &mode_);
        if (ret != 0) {
            std::cerr << "[DRM] drmModeSetCrtc failed: " << strerror(-ret) << "\n";
            return false;
        }
        crtc_active_ = true;
        return true;
    }

    // Subsequent commits: use PageFlip
    // First, drain any completed flip event (non-blocking).
    wait_for_flip();
    if (flip_pending_.load()) {
        // Previous flip still in progress — drop this frame.
        // Callers must NOT advance the buffer index: the "back" buffer
        // we just wrote to may actually be the displayed front buffer
        // when using double-buffering, so we need to overwrite it again
        // next frame rather than switching to the real front buffer.
        return false;
    }

    flip_pending_ = true;
    int ret = drmModePageFlip(drm_fd_, crtc_id_, fb_id,
                               DRM_MODE_PAGE_FLIP_EVENT, this);
    if (ret != 0) {
        flip_pending_ = false;
        // Fall back to SetCrtc on error
        ret = drmModeSetCrtc(drm_fd_, crtc_id_, fb_id,
                              0, 0, &connector_id_, 1, &mode_);
        if (ret != 0) {
            std::cerr << "[DRM] SetCrtc fallback failed: " << strerror(-ret) << "\n";
            return false;
        }
        return true;
    }

    return true;
}

// ---------------------------------------------------------------------------
// atomic_plane_commit — zero-copy DMA-BUF with hardware scaling via VOP
// Uses atomic modesetting to set plane SRC/CRTC rects, letting the display
// controller handle NV12→RGB conversion and scaling in hardware.
// ---------------------------------------------------------------------------
static uint32_t get_prop_id(int fd, uint32_t obj_id, uint32_t obj_type, const char* name) {
    drmModeObjectProperties* props = drmModeObjectGetProperties(fd, obj_id, obj_type);
    if (!props) return 0;
    uint32_t id = 0;
    for (uint32_t i = 0; i < props->count_props; i++) {
        drmModePropertyRes* prop = drmModeGetProperty(fd, props->props[i]);
        if (!prop) continue;
        if (strcmp(prop->name, name) == 0) id = prop->prop_id;
        drmModeFreeProperty(prop);
        if (id) break;
    }
    drmModeFreeObjectProperties(props);
    return id;
}

DRMDisplay::CommitResult DRMDisplay::atomic_plane_commit_impl(uint32_t fb_id, const Rect& src, const Rect& dst) {
    if (!plane_id_) return CommitResult::Failed;

    if (!prop_plane_fb_id_ || !prop_plane_crtc_id_ || !prop_plane_src_x_ ||
        !prop_plane_src_y_ || !prop_plane_src_w_   || !prop_plane_src_h_ ||
        !prop_plane_crtc_x_ || !prop_plane_crtc_y_ || !prop_plane_crtc_w_ ||
        !prop_plane_crtc_h_)
        return CommitResult::Failed;

    // First atomic commit must also set the CRTC mode
    drmModeAtomicReq* req = drmModeAtomicAlloc();
    if (!req) return CommitResult::Failed;

    uint32_t flags = DRM_MODE_ATOMIC_NONBLOCK | DRM_MODE_PAGE_FLIP_EVENT;

    if (!crtc_active_) {
        // First commit: need ALLOW_MODESET and connector/CRTC properties
        flags |= DRM_MODE_ATOMIC_ALLOW_MODESET;
        if (prop_conn_crtc_id_)
            drmModeAtomicAddProperty(req, connector_id_, prop_conn_crtc_id_, crtc_id_);
        if (prop_crtc_active_)
            drmModeAtomicAddProperty(req, crtc_id_, prop_crtc_active_, 1);
        if (prop_crtc_mode_id_) {
            // Destroy any previous mode blob to avoid leaking kernel blobs on
            // every modeset (re-init / set_mode / hotplug).
            if (mode_blob_id_) {
                drmModeDestroyPropertyBlob(drm_fd_, mode_blob_id_);
                mode_blob_id_ = 0;
            }
            drmModeCreatePropertyBlob(drm_fd_, &mode_, sizeof(mode_), &mode_blob_id_);
            if (mode_blob_id_)
                drmModeAtomicAddProperty(req, crtc_id_, prop_crtc_mode_id_, mode_blob_id_);
        }
    }

    // Drain any pending flip
    wait_for_flip();
    if (flip_pending_.load() && crtc_active_) {
        drmModeAtomicFree(req);
        return CommitResult::Dropped;
    }

    // SRC rect is in 16.16 fixed point
    drmModeAtomicAddProperty(req, plane_id_, prop_plane_fb_id_,   fb_id);
    drmModeAtomicAddProperty(req, plane_id_, prop_plane_crtc_id_, crtc_id_);
    drmModeAtomicAddProperty(req, plane_id_, prop_plane_src_x_,   (uint64_t)src.x << 16);
    drmModeAtomicAddProperty(req, plane_id_, prop_plane_src_y_,   (uint64_t)src.y << 16);
    drmModeAtomicAddProperty(req, plane_id_, prop_plane_src_w_,   (uint64_t)src.w << 16);
    drmModeAtomicAddProperty(req, plane_id_, prop_plane_src_h_,   (uint64_t)src.h << 16);
    drmModeAtomicAddProperty(req, plane_id_, prop_plane_crtc_x_,  dst.x);
    drmModeAtomicAddProperty(req, plane_id_, prop_plane_crtc_y_,  dst.y);
    drmModeAtomicAddProperty(req, plane_id_, prop_plane_crtc_w_,  dst.w);
    drmModeAtomicAddProperty(req, plane_id_, prop_plane_crtc_h_,  dst.h);

    // Hardware rotation (VOP2 Smart/Esmart planes)
    if (rotation_prop_id_)
        drmModeAtomicAddProperty(req, plane_id_, rotation_prop_id_, rotation_drm_);

    flip_pending_ = true;
    int ret = drmModeAtomicCommit(drm_fd_, req, flags, this);
    const int commit_error = errno;
    drmModeAtomicFree(req);

    if (ret != 0) {
        last_commit_errno_ = commit_error;
        flip_pending_ = false;
        if (rotation_drm_ != 1 && (commit_error == EINVAL || commit_error == ERANGE)) {
            // Formats can change after TEST_ONLY; preserve output if the real
            // commit becomes incompatible with the requested rotation.
            std::cerr << "[DRM] Rotation commit failed; reverting display to 0 degrees\n";
            rotation_degrees_=0;
            rotation_drm_=1;
            rotation_dirty_=true;
            invalidate_fill_cache();
            return atomic_plane_commit_impl(fb_id,src,dst);
        }
        if (!crtc_active_) {
            // Can't use atomic — caller should fall back to legacy path
            return CommitResult::Failed;
        }
        // Non-first frame failed: log once and return
        return CommitResult::Failed;
    }

    rotation_dirty_ = false;
    crtc_active_ = true;
    return CommitResult::Ok;
}

static int64_t drm_mono_ms() {
    return std::chrono::duration_cast<std::chrono::milliseconds>(
               std::chrono::steady_clock::now().time_since_epoch()).count();
}

// HW-1 H6: a failed atomic commit was silent, and nothing recorded how the
// commits went after a modeset. Count each result; log a failure with its errno,
// the first one after a modeset and then at most once in five seconds.
DRMDisplay::CommitResult DRMDisplay::atomic_plane_commit(uint32_t fb_id, const Rect& src, const Rect& dst) {
    const CommitResult result = atomic_plane_commit_impl(fb_id, src, dst);
    const int64_t now = drm_mono_ms();
    switch (result) {
        case CommitResult::Ok:
            commits_ok_++;
            last_ok_ms_ = now;
            break;
        case CommitResult::Dropped:
            commits_dropped_++;
            break;
        case CommitResult::Failed: {
            const uint64_t n = ++commits_failed_;
            if (n == 1 || now - last_failure_log_ms_.load() >= 5000) {
                last_failure_log_ms_ = now;
                const int e = last_commit_errno_.load();
                std::cerr << "[DRM] atomic plane commit failed (" << n
                          << " since the last modeset): "
                          << (e ? strerror(e) : "no errno") << ", crtc_active=" << crtc_active_
                          << " flip_pending=" << flip_pending_.load() << "\n";
            }
            break;
        }
    }
    return result;
}

// `plain`: the caller owns frame_mutex_, so the plain fields can be read.
std::string DRMDisplay::state_line(bool plain) const {
    const int64_t now = drm_mono_ms();
    std::ostringstream o;
    if (plain) {
        o << "mode=" << width_ << "x" << height_ << "@" << vrefresh_hz_
          << " crtc_active=" << crtc_active_ << " streaming=" << streaming_
          << " atomic_plane_state=" << atomic_plane_state_ << " ";
    }
    o << "flip_pending=" << flip_pending_.load()
      << " commits(ok/dropped/failed)=" << commits_ok_.load() << "/"
      << commits_dropped_.load() << "/" << commits_failed_.load()
      << " last_errno=" << last_commit_errno_.load();
    if (modeset_ms_.load() > 0) o << " since_modeset_ms=" << now - modeset_ms_.load();
    if (last_ok_ms_.load() > 0) o << " since_last_ok_commit_ms=" << now - last_ok_ms_.load();
    return o.str();
}

std::string DRMDisplay::stall_report() const {
    // try_lock: the report must not wait on the thread it is diagnosing.
    std::unique_lock<std::mutex> lk(frame_mutex_, std::try_to_lock);
    return std::string("frame_mutex=") + (lk.owns_lock() ? "free " : "HELD ") +
           state_line(lk.owns_lock());
}

// ---------------------------------------------------------------------------
// set_rotation — hardware rotation via VOP2 plane property
// ---------------------------------------------------------------------------
std::vector<uint32_t> DRMDisplay::supported_rotations() const {
    std::vector<uint32_t> result{0};
    if (safe_rotation_value(180, rotation_supported_mask_, unsafe_linear_rotation_))
        result.push_back(180);
    return result;
}

bool DRMDisplay::rotation_allowed(uint32_t degrees) const {
    return safe_rotation_value(degrees, rotation_supported_mask_, unsafe_linear_rotation_) != 0;
}

bool DRMDisplay::set_rotation(uint32_t degrees) {
    rotation_busy_=false;
    const auto drm_val = safe_rotation_value(degrees, rotation_supported_mask_, unsafe_linear_rotation_);
    if (!drm_val) {
        std::cerr << "[DRM] Rotation " << degrees << " unavailable for linear renderer on plane "
                  << plane_id_ << " (quarter-turn geometry or plane transform restriction)\n";
        return false;
    }
    if (degrees==rotation_degrees_.load()) return true;
    rotation_waiting_=true;
    struct WaitingGuard {
        std::atomic<bool>& flag;
        ~WaitingGuard() { flag=false; }
    } guard{rotation_waiting_};
    std::unique_lock<std::mutex> lock(frame_mutex_,std::defer_lock);
    const auto deadline=std::chrono::steady_clock::now()+std::chrono::milliseconds(250);
    while(!lock.try_lock()) {
        if(std::chrono::steady_clock::now()>=deadline) { rotation_busy_=true;return false; }
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    if (degrees==rotation_degrees_) return true;
    if (!rotation_prop_id_ || (rotation_supported_mask_ & drm_val)!=drm_val) {
        std::cerr << "[DRM] Rotation " << degrees << " unsupported on plane " << plane_id_ << "\n";
        return false;
    }
    // Test against the current plane format/scaling without changing scanout.
    auto* req=drmModeAtomicAlloc();
    if (!req) return false;
    int add=drmModeAtomicAddProperty(req,plane_id_,rotation_prop_id_,drm_val);
    int ret=add<0?-1:drmModeAtomicCommit(drm_fd_,req,DRM_MODE_ATOMIC_TEST_ONLY,nullptr);
    drmModeAtomicFree(req);
    if (ret) {
        std::cerr << "[DRM] Rotation test rejected: " << std::strerror(errno) << "\n";
        return false;
    }
    rotation_degrees_=degrees;
    rotation_drm_=drm_val;
    rotation_dirty_=true; // One atomic reset is required before legacy flips resume.
    invalidate_fill_cache();
    // Keep scanout buffers alive until normal mode teardown.
    yuv_fb_state_ = degrees? -1:0;
    uyvy_colour_cached_ = false;
    std::cout << "[DRM] Rotation accepted: " << degrees << " degrees\n";
    return true;
}

// ---------------------------------------------------------------------------
// compute_dst_rect
// ---------------------------------------------------------------------------
DRMDisplay::Rect DRMDisplay::compute_dst_rect(uint32_t src_w, uint32_t src_h) const {
    Rect r;
    if (scale_mode_ == ScaleMode::Stretch || src_w == 0 || src_h == 0) {
        r.x = 0; r.y = 0; r.w = width_; r.h = height_;
        return r;
    }

    // Aspect-ratio calculation (shared by Letterbox and Crop)
    double src_ar = (double)src_w / (double)src_h;
    double dst_ar = (double)width_ / (double)height_;

    if (scale_mode_ == ScaleMode::Letterbox) {
        // Fit inside — add black bars
        if (src_ar > dst_ar) {
            r.w = width_;
            r.h = (uint32_t)(width_ / src_ar);
            r.x = 0;
            r.y = (height_ - r.h) / 2;
        } else {
            r.h = height_;
            r.w = (uint32_t)(height_ * src_ar);
            r.y = 0;
            r.x = (width_ - r.w) / 2;
        }
    } else { // Crop
        // Fill display — destination rect is the full display
        r.x = 0; r.y = 0; r.w = width_; r.h = height_;
    }
    return r;
}

// ---------------------------------------------------------------------------
// compute_src_rect — Crop only. Letterbox and Stretch keep the full frame.
// ---------------------------------------------------------------------------
DRMDisplay::Rect DRMDisplay::compute_src_rect(uint32_t src_w, uint32_t src_h) const {
    Rect full{0, 0, src_w, src_h};
    if (scale_mode_ != ScaleMode::Crop || src_w < 2 || src_h < 2 ||
        width_ < 2 || height_ < 2)
        return full;

    double src_ar = (double)src_w / (double)src_h;
    double dst_ar = (double)width_ / (double)height_;
    Rect r = full;
    if (src_ar > dst_ar) {
        r.h = src_h & ~1u;
        r.w = ((uint32_t)((double)r.h * dst_ar)) & ~1u;
        r.x = ((src_w - r.w) / 2u) & ~1u;
        r.y = 0;
    } else {
        r.w = src_w & ~1u;
        r.h = ((uint32_t)((double)r.w / dst_ar)) & ~1u;
        r.y = ((src_h - r.h) / 2u) & ~1u;
        r.x = 0;
    }
    if (r.w < 2 || r.h < 2 || r.x + r.w > src_w || r.y + r.h > src_h)
        return full;
    return r;
}

// ---------------------------------------------------------------------------
// fill_bg
// ---------------------------------------------------------------------------
// Fills the black letterbox/crop bars in the CURRENT buffer (fb_[cur_buf_]).
// Cache is tracked per-buffer so double-buffering doesn't cause stale bar
// content from a prior show_splash() call to bleed through.
void DRMDisplay::fill_bg(DRMBuffer& buf, const Rect& painted) {
    int idx = cur_buf_;
    if (scale_mode_ == ScaleMode::Stretch) {
        last_bg_fill_w_[idx] = 0;
        return;
    }
    const Rect& previous = last_bg_painted_[idx];
    if (buf.width  == last_bg_fill_w_[idx] &&
        buf.height == last_bg_fill_h_[idx] &&
        scale_mode_ == last_bg_scale_[idx] &&
        painted.x == previous.x && painted.y == previous.y &&
        painted.w == previous.w && painted.h == previous.h) {
        return;
    }

    if (buf.map) memset(buf.map, 0, buf.size);

    last_bg_fill_w_[idx]  = buf.width;
    last_bg_fill_h_[idx]  = buf.height;
    last_bg_scale_[idx]   = scale_mode_;
    last_bg_painted_[idx] = painted;
}

void DRMDisplay::invalidate_fill_cache() {
    for (int i = 0; i < kNumBuffers; i++) {
        last_bg_fill_w_[i] = 0;
        last_bg_fill_h_[i] = 0;
    }
}

// ---------------------------------------------------------------------------
// sw_nv12_to_xrgb
// ---------------------------------------------------------------------------
void DRMDisplay::sw_nv12_to_xrgb(const uint8_t* src,
                                   uint32_t src_w, uint32_t src_h, uint32_t src_stride,
                                   uint8_t* dst, uint32_t dst_stride,
                                   const Rect& dr, uint32_t /*out_w*/, uint32_t /*out_h*/,
                                   uint32_t crop_x, uint32_t crop_y, uint32_t full_h) {
    uint32_t plane_h = full_h ? full_h : src_h;
    const uint8_t* y_plane  = src + (size_t)crop_y * src_stride + crop_x;
    const uint8_t* uv_plane = src + (size_t)src_stride * plane_h
                            + (size_t)(crop_y / 2) * src_stride + crop_x;

    if (dr.w == 0 || dr.h == 0) return;

    uint32_t y_step = (src_h << 16) / dr.h;

#if defined(__aarch64__) || defined(__ARM_NEON)
    if (src_w == dr.w && src_h == dr.h) {
        // No scaling — single-threaded NEON 1:1
        for (uint32_t sy = 0; sy < src_h; sy++) {
            const uint8_t* yr  = y_plane  + (size_t)sy * src_stride;
            const uint8_t* uvr = uv_plane + (size_t)(sy >> 1) * src_stride;
            uint32_t* d = (uint32_t*)(dst + (size_t)(dr.y + sy) * dst_stride) + dr.x;
            neon_nv12_row_to_xrgb(yr, uvr, d, src_w);
        }
        return;
    }

    // Scaling path — single-threaded NEON convert + horizontal resample
    uint32_t x_step = (src_w << 16) / dr.w;
    for (uint32_t row = 0; row < dr.h; row++) {
        uint32_t sy = ((uint64_t)row * y_step) >> 16;
        const uint8_t* yr  = y_plane  + (size_t)sy * src_stride;
        const uint8_t* uvr = uv_plane + (size_t)(sy >> 1) * src_stride;
        uint32_t* d = (uint32_t*)(dst + (size_t)(dr.y + row) * dst_stride) + dr.x;
        neon_nv12_scale_row_to_xrgb(yr, uvr, src_w, d, dr.w, x_step);
    }
#else
    uint32_t x_step = (src_w << 16) / dr.w;
    uint32_t sy_fp = 0;
    for (uint32_t dy = dr.y; dy < dr.y + dr.h; dy++, sy_fp += y_step) {
        uint32_t sy        = sy_fp >> 16;
        const uint8_t* yr  = y_plane  + (size_t)sy * src_stride;
        const uint8_t* uvr = uv_plane + (size_t)(sy >> 1) * src_stride;
        uint32_t* d        = (uint32_t*)(dst + (size_t)dy * dst_stride) + dr.x;

        uint32_t sx_fp = 0;
        for (uint32_t dx = 0; dx < dr.w; dx++, sx_fp += x_step) {
            uint32_t sx = sx_fp >> 16;
            int y = yr[sx];
            int u = uvr[sx & ~1u];
            int v = uvr[(sx & ~1u) + 1];
            d[dx] = yuv_to_xrgb(y, u, v);
        }
    }
#endif
}

// ---------------------------------------------------------------------------
// sw_uyvy_to_xrgb
// ---------------------------------------------------------------------------
void DRMDisplay::sw_uyvy_to_xrgb(const uint8_t* src,
                                   uint32_t src_w, uint32_t src_h, uint32_t src_stride,
                                   uint8_t* dst, uint32_t dst_stride,
                                   const Rect& dr, uint32_t /*out_w*/, uint32_t /*out_h*/, const YuvCoefficients& colour) {
    if (dr.w == 0 || dr.h == 0) return;

    uint32_t y_step = (src_h << 16) / dr.h;

#if defined(__aarch64__) || defined(__ARM_NEON)
    if (src_w == dr.w && src_h == dr.h) {
        // No scaling — single-threaded NEON 1:1
        for (uint32_t sy = 0; sy < src_h; sy++) {
            const uint8_t* s = src + (size_t)sy * src_stride;
            uint32_t* d = (uint32_t*)(dst + (size_t)(dr.y + sy) * dst_stride) + dr.x;
            neon_uyvy_row_to_xrgb(s, d, src_w, colour);
        }
        return;
    }

    // Scaling path — single-threaded NEON convert + horizontal resample
    uint32_t x_step = (src_w << 16) / dr.w;
    for (uint32_t row = 0; row < dr.h; row++) {
        uint32_t sy = ((uint64_t)row * y_step) >> 16;
        const uint8_t* s = src + (size_t)sy * src_stride;
        uint32_t* d = (uint32_t*)(dst + (size_t)(dr.y + row) * dst_stride) + dr.x;
        neon_uyvy_scale_row_to_xrgb(s, src_w, d, dr.w, x_step, colour);
    }
#else
    uint32_t x_step = (src_w << 16) / dr.w;
    uint32_t sy_fp = 0;
    for (uint32_t dy = dr.y; dy < dr.y + dr.h; dy++, sy_fp += y_step) {
        uint32_t sy      = sy_fp >> 16;
        const uint8_t* s = src + (size_t)sy * src_stride;
        uint32_t* d      = (uint32_t*)(dst + (size_t)dy * dst_stride) + dr.x;

        uint32_t sx_fp = 0;
        for (uint32_t dx = 0; dx < dr.w; dx++, sx_fp += x_step) {
            uint32_t sx   = sx_fp >> 16;
            uint32_t pair = sx & ~1u;
            int u = s[pair * 2 + 0];
            int y = s[pair * 2 + 1 + (sx & 1u) * 2];
            int v = s[pair * 2 + 2];
            d[dx] = yuv_to_xrgb(y, u, v, colour);
        }
    }
#endif
}

// ---------------------------------------------------------------------------
// UYVY native plane path — VOP2 Cluster handles YUV→RGB during scanout.
// ---------------------------------------------------------------------------

static bool plane_colour_matches(int fd,uint32_t plane,const char* matrix) {
    auto* props=drmModeObjectGetProperties(fd,plane,DRM_MODE_OBJECT_PLANE);
    if(!props)return false;
    bool encoding=false,range=false;
    for(uint32_t i=0;i<props->count_props;++i) {
        auto* p=drmModeGetProperty(fd,props->props[i]);if(!p)continue;
        for(int n=0;n<p->count_enums;++n) if(p->enums[n].value==props->prop_values[i]) {
            if(std::strcmp(p->name,"COLOR_ENCODING")==0 && std::strcmp(p->enums[n].name,matrix)==0)encoding=true;
            if(std::strcmp(p->name,"COLOR_RANGE")==0 && std::strcmp(p->enums[n].name,"YCbCr limited range")==0)range=true;
        }
        drmModeFreeProperty(p);
    }
    drmModeFreeObjectProperties(props);return encoding&&range;
}

bool DRMDisplay::native_uyvy_colour_matches(uint32_t w,uint32_t h) {
    const auto& colour=ndi_yuv_coefficients(w,h);
    const char* matrix=&colour==&kYuv601?"ITU-R BT.601 YCbCr":&colour==&kYuv709?"ITU-R BT.709 YCbCr":"ITU-R BT.2020 YCbCr";
    const uint32_t requested_plane=(atomic_plane_state_==1 || rotation_drm_!=1)?plane_id_:0;
    if(uyvy_colour_cached_ && uyvy_colour_request_plane_==requested_plane && uyvy_colour_class_==&colour)
        return uyvy_colour_compatible_;
    uint32_t scanout=requested_plane;
    if(!requested_plane) {
        auto* planes=drmModeGetPlaneResources(drm_fd_);
        if(planes) {
            for(uint32_t i=0;i<planes->count_planes;++i) {
                auto* plane=drmModeGetPlane(drm_fd_,planes->planes[i]);
                if(!plane)continue;
                if(plane->crtc_id==crtc_id_) {
                    auto* props=drmModeObjectGetProperties(drm_fd_,plane->plane_id,DRM_MODE_OBJECT_PLANE);
                    if(props) {
                        for(uint32_t j=0;j<props->count_props;++j) {
                            auto* p=drmModeGetProperty(drm_fd_,props->props[j]);
                            if(p && std::strcmp(p->name,"type")==0 && props->prop_values[j]==DRM_PLANE_TYPE_PRIMARY)scanout=plane->plane_id;
                            if(p)drmModeFreeProperty(p);
                        }
                        drmModeFreeObjectProperties(props);
                    }
                }
                drmModeFreePlane(plane);
            }
            drmModeFreePlaneResources(planes);
        }
    }
    bool compatible=scanout && plane_colour_matches(drm_fd_,scanout,matrix);
    std::cout << "[DRM] UYVY colour plane=" << scanout << " matrix=" << colour.name
              << " range=limited native-compatible=" << compatible << "\n";
    uyvy_colour_cached_=true;
    uyvy_colour_request_plane_=requested_plane;
    uyvy_colour_class_=&colour;
    uyvy_colour_compatible_=compatible;
    if (!compatible) {
        // Keep old buffers until mode teardown: one may still be scanned out.
        yuv_fb_state_=-1; // Re-probed after rotation/mode reinitialisation.
    }
    return compatible;
}

bool DRMDisplay::alloc_yuv_fb_if_needed() {
    if (yuv_fb_state_ == 1) {
        if (yuv_fb_[0].width == width_ && yuv_fb_[0].height == height_)
            return true;
        // Wrong size — free and reallocate
        for (auto& b : yuv_fb_) free_fb(b);
        yuv_fb_state_ = 0;
        uyvy_colour_cached_ = false;
    }
    if (yuv_fb_state_ == -1) return false;

    for (int i = 0; i < kNumBuffers; i++) {
        free_fb(yuv_fb_[i]);

        struct drm_mode_create_dumb cd = {};
        cd.width  = width_;
        cd.height = height_;
        cd.bpp    = 16;
        if (drmIoctl(drm_fd_, DRM_IOCTL_MODE_CREATE_DUMB, &cd) < 0) {
            std::cerr << "[DRM] UYVY alloc: CREATE_DUMB(bpp=16) failed: "
                      << strerror(errno) << "\n";
            for (int j = 0; j < i; j++) free_fb(yuv_fb_[j]);
            yuv_fb_state_ = -1;
            return false;
        }
        yuv_fb_[i].bo_handle = cd.handle;
        yuv_fb_[i].stride    = cd.pitch;
        yuv_fb_[i].size      = cd.size;
        yuv_fb_[i].width     = width_;
        yuv_fb_[i].height    = height_;

        uint32_t handles[4] = { yuv_fb_[i].bo_handle };
        uint32_t strides[4] = { yuv_fb_[i].stride };
        uint32_t offsets[4] = { 0 };
        if (drmModeAddFB2(drm_fd_, width_, height_, DRM_FORMAT_UYVY,
                          handles, strides, offsets, &yuv_fb_[i].fb_id, 0) != 0) {
            std::cerr << "[DRM] UYVY alloc: drmModeAddFB2(UYVY) failed: "
                      << strerror(errno) << " — plane does not support UYVY\n";
            struct drm_mode_destroy_dumb d = {};
            d.handle = yuv_fb_[i].bo_handle;
            drmIoctl(drm_fd_, DRM_IOCTL_MODE_DESTROY_DUMB, &d);
            yuv_fb_[i].bo_handle = 0;
            for (int j = 0; j < i; j++) free_fb(yuv_fb_[j]);
            yuv_fb_state_ = -1;
            return false;
        }

        struct drm_mode_map_dumb md = {};
        md.handle = yuv_fb_[i].bo_handle;
        if (drmIoctl(drm_fd_, DRM_IOCTL_MODE_MAP_DUMB, &md) == 0) {
            yuv_fb_[i].map = mmap(nullptr, yuv_fb_[i].size,
                                  PROT_READ | PROT_WRITE, MAP_SHARED,
                                  drm_fd_, md.offset);
            if (yuv_fb_[i].map == MAP_FAILED) {
                yuv_fb_[i].map = nullptr;
                std::cerr << "[DRM] UYVY alloc: mmap failed\n";
            }
        }
    }

    yuv_fb_state_ = 1;
    cur_yuv_buf_  = 0;
    std::cout << "[DRM] UYVY native plane active (VOP2 hardware YUV→RGB)\n";
    return true;
}

// Fill entire UYVY buffer with black (U=128, Y=16, V=128, Y=16).
void DRMDisplay::fill_bg_yuv(DRMBuffer& buf) {
    if (!buf.map) return;
    // UYVY black pattern as uint32: [U=0x80][Y=0x10][V=0x80][Y=0x10]
    uint32_t* p   = static_cast<uint32_t*>(buf.map);
    uint32_t  n   = buf.size / 4;
    uint32_t  pat = 0x10801080u;
    for (uint32_t i = 0; i < n; i++) p[i] = pat;
}

// Scale UYVY src into the dst rectangle dr without any colour conversion.
// src_stride and dst_stride are in bytes. dr.x must be even.
void DRMDisplay::sw_uyvy_scale(const uint8_t* src,
                                uint32_t src_w, uint32_t src_h, uint32_t src_stride,
                                uint8_t* dst, uint32_t dst_stride,
                                const Rect& dr) {
    if (dr.w == 0 || dr.h == 0) return;

    uint32_t x_step = (src_w << 16) / dr.w;
    uint32_t y_step = (src_h << 16) / dr.h;

    // dr.x must be even for UYVY pair alignment
    uint32_t dst_x_bytes = (dr.x & ~1u) * 2u;
    uint32_t out_pairs   = dr.w / 2;

    uint32_t sy_fp = 0;
    for (uint32_t dy = dr.y; dy < dr.y + dr.h; dy++, sy_fp += y_step) {
        uint32_t sy = sy_fp >> 16;
        if (sy >= src_h) sy = src_h - 1;
        const uint8_t* srow = src + (size_t)sy * src_stride;
        uint8_t*       drow = dst + (size_t)dy * dst_stride + dst_x_bytes;

        uint32_t sx_fp = 0;
        for (uint32_t p = 0; p < out_pairs; p++) {
            uint32_t sx0 = sx_fp >> 16;
            sx_fp += x_step;
            uint32_t sx1 = sx_fp >> 16;
            sx_fp += x_step;

            if (sx0 >= src_w) sx0 = src_w - 1;
            if (sx1 >= src_w) sx1 = src_w - 1;

            uint32_t sp0 = sx0 >> 1;
            uint32_t sp1 = sx1 >> 1;

            // UYVY pair layout: [U0][Y0][V0][Y1]
            uint8_t u  = srow[sp0 * 4 + 0];
            uint8_t y0 = srow[sp0 * 4 + 1 + (sx0 & 1u) * 2u];
            uint8_t v  = srow[sp0 * 4 + 2];
            uint8_t y1 = srow[sp1 * 4 + 1 + (sx1 & 1u) * 2u];

            drow[p * 4 + 0] = u;
            drow[p * 4 + 1] = y0;
            drow[p * 4 + 2] = v;
            drow[p * 4 + 3] = y1;
        }
    }
}

// ---------------------------------------------------------------------------
// show_frame_memory
// ---------------------------------------------------------------------------
bool DRMDisplay::show_frame_memory(const uint8_t* data, size_t /*size*/,
                                    uint32_t frame_w, uint32_t frame_h,
                                    uint32_t stride, uint32_t drm_format) {
    if (!initialized_) return false;
    if (rotation_waiting_.load(std::memory_order_relaxed) || mode_waiting_.load(std::memory_order_relaxed)) return false;
    std::lock_guard<std::mutex> lk(frame_mutex_);

    // BGRX/BGRA direct path: SDK already converted to RGB — just copy to DRM buffer.
    // No color conversion, no staging buffer, no NEON threads.
    if ((drm_format == DRM_FORMAT_XRGB8888 || drm_format == DRM_FORMAT_ARGB8888) && data) {
        DRMBuffer& buf = fb_[cur_buf_];
        if (!buf.fb_id || buf.width != width_ || buf.height != height_) {
            free_fb(buf);
            if (!alloc_fb(buf, width_, height_)) return false;
        }
        if (!buf.map) return false;

        Rect dr = compute_dst_rect(frame_w, frame_h);
        Rect sr = compute_src_rect(frame_w, frame_h);
        uint32_t src_stride_bytes = stride ? stride : frame_w * 4u;
        const uint8_t* win = data + (size_t)sr.y * src_stride_bytes + (size_t)sr.x * 4u;
        uint32_t copy_w = std::min(sr.w, dr.w) * 4u;
        uint32_t copy_h = std::min(sr.h, dr.h);
        fill_bg(buf, Rect{dr.x, dr.y, copy_w / 4u, copy_h});

        for (uint32_t y = 0; y < copy_h; y++) {
            memcpy((uint8_t*)buf.map + (size_t)(dr.y + y) * buf.stride + dr.x * 4u,
                   win + (size_t)y * src_stride_bytes,
                   copy_w);
        }

        streaming_ = true;
        if (osd_enabled_ && !osd_text_.empty()) {
            uint32_t* px = (uint32_t*)buf.map;
            draw_osd(px, buf.stride / 4);
        }
        bool ok = commit_fb(buf.fb_id);
        if (ok) cur_buf_ = (cur_buf_ + 1) % kNumBuffers;
        return true;
    }

    // UYVY native path: feed 16bpp UYVY dumb buffer to the VOP2 Cluster plane.
    // The display controller handles YUV→RGB during scanout — zero CPU colour math.
    // If commit_fb fails (plane doesn't support UYVY in this lease), fall back
    // to the software XRGB conversion path and mark UYVY as unsupported.
    if (drm_format == DRM_FORMAT_UYVY && data && yuv_fb_state_ != -1 && native_uyvy_colour_matches(frame_w,frame_h) && alloc_yuv_fb_if_needed()) {
        DRMBuffer& ybuf = yuv_fb_[cur_yuv_buf_];
        if (ybuf.map) {
            fill_bg_yuv(ybuf);
            Rect dr = compute_dst_rect(frame_w, frame_h);
            Rect sr = compute_src_rect(frame_w, frame_h);
            uint32_t src_stride_bytes = stride ? stride : frame_w * 2u;
            const uint8_t* win = data + (size_t)sr.y * src_stride_bytes + (size_t)sr.x * 2u;
            sw_uyvy_scale(win, sr.w, sr.h, src_stride_bytes,
                          static_cast<uint8_t*>(ybuf.map), ybuf.stride, dr);
            streaming_ = true;
            bool ok = commit_fb(ybuf.fb_id);
            if (ok || flip_pending_.load()) {
                // A busy flip is not "this plane cannot do UYVY". Keep the
                // path and overwrite this buffer next frame.
                if (ok) cur_yuv_buf_ = (cur_yuv_buf_ + 1) % kNumBuffers;
                return true;
            }
            // commit_fb failed — this plane doesn't support UYVY natively.
            // Free UYVY buffers, mark path as unsupported, fall through to
            // software XRGB conversion below.
            std::cerr << "[DRM] UYVY native commit failed — disabling UYVY path, "
                         "falling back to software conversion\n";
            for (auto& b : yuv_fb_) free_fb(b);
            yuv_fb_state_ = -1;
        }
    }

    DRMBuffer& buf = fb_[cur_buf_];

    if (!buf.fb_id || buf.width != width_ || buf.height != height_) {
        free_fb(buf);
        if (!alloc_fb(buf, width_, height_)) return false;
    }

    if (!buf.map) return false;

    Rect dr = compute_dst_rect(frame_w, frame_h);
    Rect sr = compute_src_rect(frame_w, frame_h);

    uint32_t src_stride = stride ? stride : frame_w;

    if (data) {
        if (drm_format == DRM_FORMAT_UYVY || drm_format == DRM_FORMAT_YUYV) {
            const auto& colour=ndi_yuv_coefficients(frame_w,frame_h);
            if (colour_path_!=colour.name) {
                colour_path_=colour.name;
                std::cout << "[DRM] NDI colour path=yuv422-sw " << colour.name << " range=limited\n";
            }
        }
        // NEON software convert directly to DRM back buffer.
        // Triple buffering (3 buffers) ensures this buffer is never being
        // scanned out, so direct writes are tear-free.
        fill_bg(buf, dr);

        if (drm_format == DRM_FORMAT_NV12 ||
            drm_format == DRM_FORMAT_NV15 ||
            drm_format == DRM_FORMAT_NV16) {
            sw_nv12_to_xrgb(data, sr.w, sr.h,
                             src_stride,
                             (uint8_t*)buf.map, buf.stride,
                             dr, width_, height_,
                             sr.x, sr.y, frame_h);
        } else if (drm_format == DRM_FORMAT_UYVY) {
            uint32_t uyvy_stride = stride ? stride : frame_w * 2;
            const uint8_t* win = data + (size_t)sr.y * uyvy_stride + (size_t)sr.x * 2u;
            sw_uyvy_to_xrgb(win, sr.w, sr.h, uyvy_stride,
                             (uint8_t*)buf.map, buf.stride,
                             dr, width_, height_, ndi_yuv_coefficients(frame_w,frame_h));
        } else {
            uint32_t src_row_bytes = stride ? stride : frame_w * 2u;
            const uint8_t* win = data + (size_t)sr.y * src_row_bytes + (size_t)sr.x * 2u;
            sw_uyvy_to_xrgb(win, sr.w, sr.h, src_row_bytes,
                             (uint8_t*)buf.map, buf.stride,
                             dr, width_, height_, ndi_yuv_coefficients(frame_w,frame_h));
        }
    }

    if (osd_enabled_ && !osd_text_.empty()) {
        uint32_t* px = (uint32_t*)buf.map;
        draw_osd(px, buf.stride / 4);
    }

    streaming_ = true;

    bool ok = commit_fb(buf.fb_id);
    if (ok) cur_buf_ = (cur_buf_ + 1) % kNumBuffers;
    return true;  // frame rendered (even if flip was dropped)
}

// ---------------------------------------------------------------------------
// show_frame_dma
// ---------------------------------------------------------------------------
bool DRMDisplay::show_frame_dma(int dma_fd, uint32_t format,
                                 uint32_t frame_w, uint32_t frame_h,
                                 uint32_t stride_y, uint32_t stride_uv) {
    if (!initialized_) return false;
    if (rotation_waiting_.load(std::memory_order_relaxed) || mode_waiting_.load(std::memory_order_relaxed)) return false;
    std::lock_guard<std::mutex> lk(frame_mutex_);
    if (stride_uv == 0) stride_uv = stride_y;

    Rect dr = compute_dst_rect(frame_w, frame_h);
    bool need_scale = (dr.x != 0 || dr.y != 0 ||
                       dr.w != width_ || dr.h != height_ ||
                       frame_w != width_ || frame_h != height_);

    // For letterbox/crop modes or when format requires conversion, go through dumb buffer
    if (need_scale || scale_mode_ != ScaleMode::Stretch) {
        // Zero-copy path: use atomic plane commit to let VOP handle NV12→RGB + scaling
        // in hardware. No CPU work at all — the display controller does everything.
        if (atomic_plane_state_ >= 0 && plane_id_ &&
            (format == DRM_FORMAT_NV12 || format == DRM_FORMAT_NV16)) {
            // Import DMA-BUF as framebuffer
            uint32_t a_bo = 0, a_fb = 0;
            if (drmPrimeFDToHandle(drm_fd_, dma_fd, &a_bo) == 0) {
                uint32_t handles[4] = {};
                uint32_t strides[4] = {};
                uint32_t offsets[4] = {};
                handles[0] = a_bo; strides[0] = stride_y;  offsets[0] = 0;
                handles[1] = a_bo; strides[1] = stride_uv; offsets[1] = stride_y * frame_h;

                if (drmModeAddFB2(drm_fd_, frame_w, frame_h, format,
                                  handles, strides, offsets, &a_fb, 0) == 0) {
                    Rect src_rect = compute_src_rect(frame_w, frame_h);
                    CommitResult cr = atomic_plane_commit(a_fb, src_rect, dr);
                    if (cr == CommitResult::Dropped) {
                        drmModeRmFB(drm_fd_, a_fb);
                        struct drm_gem_close c = {}; c.handle = a_bo;
                        drmIoctl(drm_fd_, DRM_IOCTL_GEM_CLOSE, &c);
                        return true;
                    }
                    if (cr == CommitResult::Ok) {
                        if (import_fb_id_) drmModeRmFB(drm_fd_, import_fb_id_);
                        if (import_bo_) {
                            struct drm_gem_close c = {}; c.handle = import_bo_;
                            drmIoctl(drm_fd_, DRM_IOCTL_GEM_CLOSE, &c);
                        }
                        import_fb_id_ = a_fb;
                        import_bo_ = a_bo;
                        streaming_ = true;
                        if (atomic_plane_state_ == 0) {
                            atomic_plane_state_ = 1;
                            std::cout << "[DRM] Atomic plane commit OK — zero-copy NV12 path active\n";
                        }
                        return true;
                    }
                    drmModeRmFB(drm_fd_, a_fb);
                }
                struct drm_gem_close c = {}; c.handle = a_bo;
                drmIoctl(drm_fd_, DRM_IOCTL_GEM_CLOSE, &c);
            }
            // Atomic failed — mark as unsupported and fall through to software
            if (atomic_plane_state_ == 0) {
                atomic_plane_state_ = -1;
                std::cerr << "[DRM] Atomic plane commit failed — falling back to software conversion\n";
            }
        }

        DRMBuffer& buf = fb_[cur_buf_];
        if (!buf.fb_id || buf.width != width_ || buf.height != height_) {
            free_fb(buf);
            if (!alloc_fb(buf, width_, height_)) return false;
        }
        if (!buf.map) return false;

        fill_bg(buf, dr);

        {
            // NEON convert directly to DRM back buffer.
            // Triple buffering ensures this buffer is not being scanned out.
            if (format == DRM_FORMAT_NV12 || format == DRM_FORMAT_NV16) {
                size_t uv_rows = (format == DRM_FORMAT_NV12) ? (frame_h / 2) : frame_h;
                size_t map_size = (size_t)stride_y * frame_h
                                + (size_t)stride_uv * uv_rows;
                auto it = dma_mmap_cache_.find(dma_fd);
                void* mapped = nullptr;
                if (it != dma_mmap_cache_.end() && it->second.size == map_size) {
                    mapped = it->second.ptr;
                } else {
                    if (it != dma_mmap_cache_.end()) {
                        ::munmap(it->second.ptr, it->second.size);
                        dma_mmap_cache_.erase(it);
                    }
                    if (dma_mmap_cache_.size() >= 8) clear_dma_mmap_cache();
                    mapped = ::mmap(nullptr, map_size, PROT_READ, MAP_SHARED, dma_fd, 0);
                    if (mapped != MAP_FAILED)
                        dma_mmap_cache_[dma_fd] = { mapped, map_size };
                }
                if (mapped && mapped != MAP_FAILED) {
                    Rect sr = compute_src_rect(frame_w, frame_h);
                    sw_nv12_to_xrgb((const uint8_t*)mapped,
                                    sr.w, sr.h, stride_y,
                                    (uint8_t*)buf.map, buf.stride,
                                    dr, width_, height_,
                                    sr.x, sr.y, frame_h);
                } else {
                    if (buf.map) memset(buf.map, 0, buf.size);
                }
            } else {
                if (buf.map) memset(buf.map, 0, buf.size);
            }
        }

        if (osd_enabled_ && !osd_text_.empty()) {
            uint32_t* px = (uint32_t*)buf.map;
            draw_osd(px, buf.stride / 4);
        }

        streaming_ = true;

        bool ok = commit_fb(buf.fb_id);
        if (ok) cur_buf_ = (cur_buf_ + 1) % kNumBuffers;
        return true;
    }

    streaming_ = true;
    // Stretch mode: keep the FB on screen until the new commit succeeds.
    uint32_t prev_fb = import_fb_id_;
    uint32_t prev_bo = import_bo_;
    import_fb_id_ = 0;
    import_bo_ = 0;

    if (drmPrimeFDToHandle(drm_fd_, dma_fd, &import_bo_) != 0) {
        std::cerr << "[DRM] drmPrimeFDToHandle failed: " << strerror(errno) << "\n";
        import_fb_id_ = prev_fb;
        import_bo_ = prev_bo;
        return false;
    }

    uint32_t handles[4] = {};
    uint32_t strides[4] = {};
    uint32_t offsets[4] = {};

    if (format == DRM_FORMAT_NV12 || format == DRM_FORMAT_NV16 || format == DRM_FORMAT_NV15) {
        handles[0] = import_bo_; strides[0] = stride_y;  offsets[0] = 0;
        handles[1] = import_bo_; strides[1] = stride_uv; offsets[1] = stride_y * frame_h;
    } else {
        handles[0] = import_bo_; strides[0] = stride_y; offsets[0] = 0;
    }

    if (drmModeAddFB2(drm_fd_, frame_w, frame_h, format,
                      handles, strides, offsets, &import_fb_id_, 0) != 0) {
        std::cerr << "[DRM] drmModeAddFB2 for DMA import failed: " << strerror(errno) << "\n";
        struct drm_gem_close c = {};
        c.handle = import_bo_;
        drmIoctl(drm_fd_, DRM_IOCTL_GEM_CLOSE, &c);
        import_fb_id_ = prev_fb;
        import_bo_ = prev_bo;
        return false;
    }

    if (!commit_fb(import_fb_id_)) {
        drmModeRmFB(drm_fd_, import_fb_id_);
        struct drm_gem_close c = {};
        c.handle = import_bo_;
        drmIoctl(drm_fd_, DRM_IOCTL_GEM_CLOSE, &c);
        import_fb_id_ = prev_fb;
        import_bo_ = prev_bo;
        return false;
    }

    if (prev_fb) drmModeRmFB(drm_fd_, prev_fb);
    if (prev_bo) {
        struct drm_gem_close c = {};
        c.handle = prev_bo;
        drmIoctl(drm_fd_, DRM_IOCTL_GEM_CLOSE, &c);
    }
    return true;
}

// ---------------------------------------------------------------------------
// show_frame_dma_explicit — zero-copy import of a DMA-BUF with explicit plane
// offsets/pitches and a DRM format modifier (VAAPI). Decode surfaces on Intel
// are tiled, so importing them as LINEAR produces garbage — we must pass the
// modifier via drmModeAddFB2WithModifiers. Scaling is done by the VOP/display
// engine through the atomic plane path; falls back to full-screen scanout if
// the atomic scaler is unavailable.
// ---------------------------------------------------------------------------
bool DRMDisplay::show_frame_dma_explicit(int dma_fd, uint32_t format,
                                         uint32_t frame_w, uint32_t frame_h,
                                         const uint32_t offsets[4], const uint32_t pitches[4],
                                         int num_planes, uint64_t modifier) {
    if (!initialized_) return false;
    if (rotation_waiting_.load(std::memory_order_relaxed) || mode_waiting_.load(std::memory_order_relaxed)) return false;
    std::lock_guard<std::mutex> lk(frame_mutex_);
    if (num_planes <= 0) num_planes = 1;
    if (num_planes > 4)  num_planes = 4;

    uint32_t bo = 0;
    if (drmPrimeFDToHandle(drm_fd_, dma_fd, &bo) != 0) {
        std::cerr << "[DRM] VAAPI drmPrimeFDToHandle failed: " << strerror(errno) << "\n";
        return false;
    }

    uint32_t handles[4] = {}, pitch[4] = {}, off[4] = {};
    uint64_t mods[4] = {};
    const bool use_mod = (modifier != 0 && modifier != DRM_FORMAT_MOD_INVALID);
    for (int i = 0; i < num_planes; i++) {
        handles[i] = bo;
        pitch[i]   = pitches[i];
        off[i]     = offsets[i];
        mods[i]    = modifier;
    }

    auto gem_close = [this](uint32_t handle) {
        struct drm_gem_close c = {}; c.handle = handle;
        drmIoctl(drm_fd_, DRM_IOCTL_GEM_CLOSE, &c);
    };

    uint32_t fb = 0;
    int r = use_mod
        ? drmModeAddFB2WithModifiers(drm_fd_, frame_w, frame_h, format,
                                     handles, pitch, off, mods, &fb, DRM_MODE_FB_MODIFIERS)
        : drmModeAddFB2(drm_fd_, frame_w, frame_h, format, handles, pitch, off, &fb, 0);
    if (r != 0) {
        std::cerr << "[DRM] VAAPI drmModeAddFB2"
                  << (use_mod ? "WithModifiers" : "") << " failed: " << strerror(errno) << "\n";
        gem_close(bo);
        return false;
    }

    Rect dr  = compute_dst_rect(frame_w, frame_h);
    Rect src = compute_src_rect(frame_w, frame_h);

    if (atomic_plane_state_ >= 0 && plane_id_) {
        CommitResult cr = atomic_plane_commit(fb, src, dr);
        if (cr == CommitResult::Dropped) {
            drmModeRmFB(drm_fd_, fb);
            gem_close(bo);
            return true;
        }
        if (cr == CommitResult::Ok) {
            if (atomic_plane_state_ == 0) {
                atomic_plane_state_ = 1;
                std::cout << "[DRM] VAAPI atomic plane commit OK — zero-copy decode path active\n";
            }
            if (import_fb_id_) drmModeRmFB(drm_fd_, import_fb_id_);
            if (import_bo_)    gem_close(import_bo_);
            import_fb_id_ = fb;
            import_bo_    = bo;
            streaming_    = true;
            return true;
        }
        if (atomic_plane_state_ == 0) {
            atomic_plane_state_ = -1;
            std::cerr << "[DRM] VAAPI atomic plane commit failed — full-screen scanout fallback\n";
        }
    }
    if (!commit_fb(fb)) {
        drmModeRmFB(drm_fd_, fb);
        gem_close(bo);
        return false;
    }
    if (import_fb_id_) drmModeRmFB(drm_fd_, import_fb_id_);
    if (import_bo_)    gem_close(import_bo_);
    import_fb_id_ = fb;
    import_bo_    = bo;
    streaming_    = true;
    return true;
}

// ---------------------------------------------------------------------------
// show_splash  — reads layout/colour settings from Config::instance().splash
// ---------------------------------------------------------------------------
bool DRMDisplay::show_splash(bool source_available) {
    if (!initialized_) return false;
    // Use try_lock to avoid blocking the main-loop tick() (which handles
    // watchdog notifications). If the FrameSync thread holds the mutex,
    // we'll retry on the next tick (500ms).
    std::unique_lock<std::mutex> lk(frame_mutex_, std::try_to_lock);
    if (!lk.owns_lock()) return false;
    // Clear streaming flag — caller has already stopped the video pipeline.
    // This ensures splash actually renders rather than silently returning.
    streaming_ = false;

    DRMBuffer& buf = fb_[cur_buf_];
    if (!buf.fb_id || buf.width != width_ || buf.height != height_) {
        free_fb(buf);
        if (!alloc_fb(buf, width_, height_)) return false;
    }
    if (!buf.map) return false;

    using namespace ndimon;
    using namespace ndimon::paint;
    uint32_t* pixels = (uint32_t*)buf.map;
    const uint32_t stride_u32 = buf.stride / 4;
    const uint32_t W = width_, H = height_;

    const SplashConfig& sc = Config::instance().splash;
    const uint32_t bg     = hex_to_u32(source_available ? sc.bg_live     : sc.bg_idle)     & 0xFFFFFFu;
    const uint32_t accent = hex_to_u32(source_available ? sc.accent_live : sc.accent_idle) & 0xFFFFFFu;
    const uint32_t white  = 0xFFFFFFu;

    // ── Backdrop: gradient with a soft accent glow, or the flat colour ───────
    if (backdrop_from_string(sc.backdrop) == Backdrop::Gradient) {
        fill_vertical_gradient(pixels, stride_u32, W, H, bg, darken(bg, 0.55f));
        add_radial_glow(pixels, stride_u32, W, H, (int)(W * 0.12f), 0, (int)(H * 0.9f), accent, 56);
    } else {
        for (uint32_t y = 0; y < H; y++)
            std::fill_n(pixels + (size_t)y * stride_u32, W, 0xFF000000u | bg);
    }

    // ── Logo image ───────────────────────────────────────────────────────────
    if (!sc.logo_path.empty()) {
        int iw = 0, ih = 0, ic = 0;
        uint8_t* img = stbi_load(sc.logo_path.c_str(), &iw, &ih, &ic, 0);
        if (img) {
            uint32_t cx = (uint32_t)(sc.logo_x_pct / 100.0f * W);
            uint32_t cy = (uint32_t)(sc.logo_y_pct / 100.0f * H);
            uint32_t dw = (uint32_t)(sc.logo_w_pct / 100.0f * W);
            draw_logo(pixels, stride_u32, img, iw, ih, ic, cx, cy, dw, W, H);
            stbi_image_free(img);
        } else {
            std::cerr << "[DRM] splash logo load failed: " << sc.logo_path
                      << " — " << stbi_failure_reason() << "\n";
        }
    }

    // ── Type scale from text_height_pct; secondary text at 60% ──────────────
    ensure_text_renderer();
    const float pct = std::max(1.0f, std::min(sc.text_height_pct, 20.0f));
    const int status_px = std::max(12, (int)(pct / 100.0f * H));
    const int sub_px    = std::max(10, (int)(status_px * 0.6f));
    const int margin_x  = (int)(W * 0.05f);   // 5% title-safe margin
    const int margin_y  = (int)(H * 0.05f);

    // ── Top-left: status pill with a glowing dot, then the source count ─────
    int left_x = margin_x;
    int ty = margin_y;
    if (sc.show_signal_text) {
        const std::string& label = source_available ? sc.text_live : sc.text_idle;
        if (!label.empty()) {
            TextBitmap t = text_.render(label, status_px, true);
            const int pad_x = (int)(status_px * 0.6f), pad_y = (int)(status_px * 0.3f);
            const int dot_r = std::max(3, (int)(status_px * 0.18f));
            const int gap   = (int)(status_px * 0.4f);
            const int pill_h = t.h + 2 * pad_y;
            const int pill_w = pad_x + 2 * dot_r + gap + t.w + pad_x;
            fill_rounded_rect(pixels, stride_u32, W, H, margin_x, margin_y, pill_w, pill_h, pill_h / 2,
                              white, 18, mix(white, accent, 0.5f), 48);
            const float dcx = margin_x + pad_x + dot_r, dcy = margin_y + pill_h * 0.5f;
            add_radial_glow(pixels, stride_u32, W, H, (int)dcx, (int)dcy, dot_r * 3, accent, 140);
            fill_circle(pixels, stride_u32, W, H, dcx, dcy, (float)dot_r, accent);
            blit_text(pixels, stride_u32, W, H, t, margin_x + pad_x + 2 * dot_r + gap, margin_y + pad_y, white);
            left_x = margin_x + pad_x;
            ty = margin_y + pill_h + (int)(sub_px * 0.6f);
        }
    }
    if (sc.show_sources_available) {
        // Read source count from ndimon-sources.json, subtract 1 for "None"
        int src_count = 0;
        try {
            std::ifstream f("/run/ndimon/ndimon-sources.json");
            if (!f.is_open()) f.open("/var/lib/ndimon/ndimon-sources.json");
            if (f.is_open()) {
                nlohmann::json j;
                f >> j;
                src_count = std::max(0, j.value("count", 0) - 1);
            }
        } catch (...) {}
        std::string src_text = src_count == 0 ? "No sources found"
                             : std::to_string(src_count) + (src_count == 1 ? " source available" : " sources available");
        TextBitmap t = text_.render(src_text, sub_px, false);
        blit_text(pixels, stride_u32, W, H, t, left_x, ty, white, 150);
    }

    // ── Top-right: NDI name and URL ──────────────────────────────────────────
    {
        const auto& dev = Config::instance().device;
        const int rx = (int)W - margin_x;
        int ry = margin_y;
        const std::string& ndi_name = dev.ndi_recv_name.empty() ? dev.device_name : dev.ndi_recv_name;
        if (sc.show_device_name && !ndi_name.empty()) {
            TextBitmap t = text_.render(ndi_name, (int)(status_px * 0.8f), true);
            blit_text(pixels, stride_u32, W, H, t, rx - t.w, ry, white);
            ry += t.h + (int)(sub_px * 0.3f);
        }
        std::string ip = Config::instance().runtime_ip();
        if (sc.show_device_url && !ip.empty()) {
            TextBitmap t = text_.render("http://" + ip, sub_px, false);
            blit_text(pixels, stride_u32, W, H, t, rx - t.w, ry, white, 160);
        }
    }

    // ── Bottom-centre: banner while a network change waits to be confirmed ──
    // Written by the network helper; the confirmation token is never drawn.
    {
        std::ifstream pending("/run/ndimon/net-pending");
        std::string line;
        if (pending && std::getline(pending, line) && !line.empty()) {
            if (line.size() > 80) line.resize(80);
            TextBitmap t = text_.render(line, sub_px, false);
            const int pad_x = (int)(sub_px * 0.8f), pad_y = (int)(sub_px * 0.4f);
            const int pill_w = t.w + 2 * pad_x, pill_h = t.h + 2 * pad_y;
            const int px0 = (int)W / 2 - pill_w / 2, py0 = (int)H - margin_y - pill_h;
            fill_rounded_rect(pixels, stride_u32, W, H, px0, py0, pill_w, pill_h, pill_h / 2,
                              white, 18, white, 40);
            blit_text(pixels, stride_u32, W, H, t, px0 + pad_x, py0 + pad_y, white, 220);
        }
    }

    // Invalidate fill cache for ALL buffers so the next video frame in every
    // slot re-fills the letterbox bars. Only invalidating cur_buf_ is not
    // enough — the other buffer slot still holds old video data in the bar
    // areas (e.g. when switching from a 16:9 to a 4:3 source).
    invalidate_fill_cache();

    bool ok = commit_fb(buf.fb_id);
    if (ok) cur_buf_ = (cur_buf_ + 1) % kNumBuffers;
    return true;  // frame rendered (even if flip was dropped)
}

// ---------------------------------------------------------------------------
// Text engine: fonts load on first use. NDIMON_FONT_DIR overrides the install
// location (handy for the diagnostics build); missing files mean the embedded
// bitmap font, logged once.
// ---------------------------------------------------------------------------
void DRMDisplay::ensure_text_renderer() {
    if (text_load_tried_) return;
    text_load_tried_ = true;
    const char* env = getenv("NDIMON_FONT_DIR");
    std::string dir = (env && *env) ? env : "/usr/share/ndimon/fonts";
    if (text_.load(dir + "/Inter-Regular.ttf", dir + "/Inter-SemiBold.ttf"))
        std::cerr << "[DRM] Text: Inter loaded from " << dir << "\n";
    else
        std::cerr << "[DRM] Text: no TrueType font in " << dir << " — using the embedded bitmap font\n";
}

// OSD line over live video: white text in a translucent dark pill, top centre.
// Rasterised once per text/size; per frame this is a small fill and a blit.
void DRMDisplay::draw_osd(uint32_t* px, uint32_t stride_u32) {
    if (!osd_enabled_ || osd_text_.empty() || !px) return;
    using namespace ndimon;
    using namespace ndimon::paint;
    ensure_text_renderer();
    const int osd_px = std::max(14, (int)(height_ * 0.03f));
    if (osd_bitmap_text_ != osd_text_ || osd_bitmap_px_ != osd_px) {
        osd_bitmap_ = text_.render(osd_text_, osd_px, true);
        osd_bitmap_text_ = osd_text_;
        osd_bitmap_px_ = osd_px;
    }
    const TextBitmap& t = osd_bitmap_;
    if (t.w <= 0) return;
    const int pad_x = (int)(osd_px * 0.7f), pad_y = (int)(osd_px * 0.35f);
    const int pill_w = t.w + 2 * pad_x, pill_h = t.h + 2 * pad_y;
    const int x0 = (int)width_ / 2 - pill_w / 2, y0 = (int)(height_ * 0.03f);
    fill_rounded_rect(px, stride_u32, width_, height_, x0, y0, pill_w, pill_h, pill_h / 2,
                      0x000000u, 140, 0xFFFFFFu, 40);
    blit_text(px, stride_u32, width_, height_, t, x0 + pad_x, y0 + pad_y, 0xFFFFFFu);
}

// ---------------------------------------------------------------------------
// show_black_impl  (called with frame_mutex_ already held)
// ---------------------------------------------------------------------------
bool DRMDisplay::show_black_impl() {
    if (!initialized_) return false;

    DRMBuffer& buf = fb_[cur_buf_];
    if (!buf.fb_id || buf.width != width_ || buf.height != height_) {
        free_fb(buf);
        if (!alloc_fb(buf, width_, height_)) return false;
    }
    if (buf.map) memset(buf.map, 0, buf.size);
    streaming_ = false;
    invalidate_fill_cache();

    bool ok = commit_fb(buf.fb_id);
    if (ok) cur_buf_ = (cur_buf_ + 1) % kNumBuffers;
    return true;  // frame rendered (even if flip was dropped)
}

// ---------------------------------------------------------------------------
// show_black
// ---------------------------------------------------------------------------
bool DRMDisplay::show_black() {
    std::lock_guard<std::mutex> lk(frame_mutex_);
    return show_black_impl();
}

// ---------------------------------------------------------------------------
// set_mode
// ---------------------------------------------------------------------------
ResolutionResult DRMDisplay::set_mode(uint32_t w, uint32_t h, double refresh_hz,
                                       int64_t deadline_monotonic_ms) {
    if (!initialized_) return ResolutionResult::Failed;
    mode_waiting_ = true;
    struct WaitingGuard {
        std::atomic<bool>& flag;
        ~WaitingGuard() { flag = false; }
    } guard{mode_waiting_};
    std::unique_lock<std::mutex> lock(frame_mutex_, std::defer_lock);
    auto acquired = lock_resolution_control(lock, deadline_monotonic_ms);
    if (acquired != ResolutionResult::Applied) return acquired;

    // Match against available_modes_ using refresh_hz for fractional rate support.
    // refresh_hz == 0 → pick preferred/first mode for this resolution.
    const DRMMode* match = nullptr;
    for (const auto& m : available_modes_) {
        if (m.width == w && m.height == h) {
            if (refresh_hz == 0.0) {
                match = &m;   // first (preferred) mode
                break;
            }
            if (std::abs(m.refresh_hz - refresh_hz) < 0.02) {
                match = &m;
                break;
            }
        }
    }
    if (!match) {
        std::cerr << "[DRM] Mode " << w << "x" << h << "@" << refresh_hz << " not available\n";
        return ResolutionResult::Failed;
    }

    // Find the exact drmModeModeInfo by pixel-clock match against available_modes_ entry
    drmModeRes* res = drmModeGetResources(drm_fd_);
    if (!res) return ResolutionResult::Failed;

    drmModeConnector* conn = nullptr;
    for (int i = 0; i < res->count_connectors; i++) {
        conn = drmModeGetConnector(drm_fd_, res->connectors[i]);
        if (conn && conn->connector_id == connector_id_) break;
        if (conn) { drmModeFreeConnector(conn); conn = nullptr; }
    }
    drmModeFreeResources(res);
    if (!conn) return ResolutionResult::Failed;

    bool found = false;
    drmModeModeInfo next_mode{};
    for (int m = 0; m < conn->count_modes; m++) {
        if ((uint32_t)conn->modes[m].hdisplay == w &&
            (uint32_t)conn->modes[m].vdisplay == h) {
            double hz = calc_refresh_hz(conn->modes[m]);
            if (std::abs(hz - match->refresh_hz) < 0.02) {
                next_mode = conn->modes[m];
                found        = true;
                break;
            }
        }
    }
    drmModeFreeConnector(conn);
    if (!found) return ResolutionResult::Failed;

    if (resolution_expired(deadline_monotonic_ms)) return ResolutionResult::Expired;
    if (crtc_active_ && std::memcmp(&mode_, &next_mode, sizeof(mode_)) == 0)
        return ResolutionResult::Applied;
    mode_ = next_mode;
    width_ = w;
    height_ = h;
    vrefresh_ = next_mode.vrefresh;
    vrefresh_hz_ = calc_refresh_hz(next_mode);

    // Free existing FBs (sized for old mode)
    for (auto& b : fb_)     free_fb(b);
    for (auto& b : yuv_fb_) free_fb(b);
    // A modeset must preserve the rotated renderer's software-only YUV path.
    yuv_fb_state_ = rotation_drm_ != 1 ? -1 : 0;
    uyvy_colour_cached_ = false;
    atomic_plane_state_ = 0;  // re-probe on mode change
    if (import_fb_id_) { drmModeRmFB(drm_fd_, import_fb_id_); import_fb_id_ = 0; }
    if (import_bo_) {
        struct drm_gem_close c = {};
        c.handle = import_bo_;
        drmIoctl(drm_fd_, DRM_IOCTL_GEM_CLOSE, &c);
        import_bo_ = 0;
    }

    crtc_active_ = false;
    streaming_   = false;
    invalidate_fill_cache();
    clear_dma_mmap_cache();

    // Reallocate at new size and show black to activate mode
    for (auto& b : fb_) {
        if (!alloc_fb(b, width_, height_)) {
            std::cerr << "[DRM] set_mode: alloc_fb failed\n";
            return ResolutionResult::Failed;
        }
    }

    if (!show_black_impl()) {
        std::cerr << "[DRM] set_mode: failed to activate new mode\n";
        return ResolutionResult::Failed;
    }
    // show_black_impl() reports "rendered" even when its commit failed. A mode
    // that never reached the CRTC is not a changed mode: report it (the policy
    // retries once a second) instead of leaving the display dark behind a
    // "Mode changed" line (HW-1 H6).
    if (!crtc_active_) {
        std::cerr << "[DRM] set_mode: " << w << "x" << h << "@" << vrefresh_hz_
                  << " is not active after the commit; " << state_line(true) << "\n";
        return ResolutionResult::Failed;
    }
    commits_ok_ = commits_dropped_ = commits_failed_ = 0;
    last_commit_errno_ = 0;
    modeset_ms_ = drm_mono_ms();

    std::cout << "[DRM] Mode changed to " << w << "x" << h << "@" << vrefresh_hz_ << "\n";
    return ResolutionResult::Applied;
}

// ---------------------------------------------------------------------------
// set_hdmi_enabled
// ---------------------------------------------------------------------------
void DRMDisplay::set_hdmi_enabled(bool enable) {
    // Try the connector-specific sysfs path
    std::string card_base = device_path_.empty() ? "card0"
        : device_path_.substr(device_path_.rfind('/') + 1);
    std::string path = "/sys/class/drm/" + card_base + "-" + connector_name_ + "/status";
    FILE* f = fopen(path.c_str(), "w");
    if (!f) {
        // Fallback to HDMI-A-1 on same card
        f = fopen(("/sys/class/drm/" + card_base + "-HDMI-A-1/status").c_str(), "w");
    }
    if (f) {
        fputs(enable ? "on" : "off", f);
        fclose(f);
    }
}

// ---------------------------------------------------------------------------
// destroy
// ---------------------------------------------------------------------------
void DRMDisplay::clear_dma_mmap_cache() {
    for (auto& [fd, entry] : dma_mmap_cache_) {
        if (entry.ptr && entry.ptr != MAP_FAILED)
            ::munmap(entry.ptr, entry.size);
    }
    dma_mmap_cache_.clear();
}

void DRMDisplay::destroy() {
    clear_dma_mmap_cache();
    if (import_fb_id_) {
        drmModeRmFB(drm_fd_, import_fb_id_);
        import_fb_id_ = 0;
    }
    if (import_bo_) {
        struct drm_gem_close c = {};
        c.handle = import_bo_;
        if (drm_fd_ >= 0) drmIoctl(drm_fd_, DRM_IOCTL_GEM_CLOSE, &c);
        import_bo_ = 0;
    }

    if (drm_fd_ >= 0) {
        for (auto& b : fb_)     free_fb(b);
        for (auto& b : yuv_fb_) free_fb(b);
        if (mode_blob_id_)
            drmModeDestroyPropertyBlob(drm_fd_, mode_blob_id_);
        if (owns_fd_) {
            std::cout << "[DRM] Closing DRM device\n";
            close(drm_fd_);
        } else {
            std::cout << "[DRM] Releasing DRM lease fd\n";
            close(drm_fd_);
        }
        drm_fd_ = -1;
    }

    mode_blob_id_       = 0;
    prop_plane_fb_id_   = 0;
    prop_plane_crtc_id_ = 0;
    prop_plane_src_x_   = 0;
    prop_plane_src_y_   = 0;
    prop_plane_src_w_   = 0;
    prop_plane_src_h_   = 0;
    prop_plane_crtc_x_  = 0;
    prop_plane_crtc_y_  = 0;
    prop_plane_crtc_w_  = 0;
    prop_plane_crtc_h_  = 0;
    prop_conn_crtc_id_  = 0;
    prop_crtc_active_   = 0;
    prop_crtc_mode_id_  = 0;

    initialized_  = false;
    crtc_active_  = false;
    flip_pending_ = false;
    available_modes_.clear();
    width_ = height_ = vrefresh_ = 0;
    connector_id_ = crtc_id_ = 0;
    cur_buf_ = 0;
    cur_yuv_buf_  = 0;
    // Effective rotation survives teardown until the next plane is validated.
    yuv_fb_state_ = rotation_drm_ != 1 ? -1 : 0;
    uyvy_colour_cached_ = false;
    invalidate_fill_cache();
}
