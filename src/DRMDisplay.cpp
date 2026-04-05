#include "DRMDisplay.h"
#include "Config.h"
#include <iostream>
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
#include <cerrno>
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

// ---------------------------------------------------------------------------
// Embedded 8×8 bitmap font (public domain, covers ASCII 32–127)
// Each glyph is 8 bytes; bit 7 of each byte is the leftmost pixel.
// ---------------------------------------------------------------------------
static const uint8_t kFont8x8[96][8] = {
    { 0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00 }, // ' '
    { 0x18,0x3C,0x3C,0x18,0x18,0x00,0x18,0x00 }, // '!'
    { 0x36,0x36,0x00,0x00,0x00,0x00,0x00,0x00 }, // '"'
    { 0x36,0x36,0x7F,0x36,0x7F,0x36,0x36,0x00 }, // '#'
    { 0x0C,0x3E,0x03,0x1E,0x30,0x1F,0x0C,0x00 }, // '$'
    { 0x00,0x63,0x33,0x18,0x0C,0x66,0x63,0x00 }, // '%'
    { 0x1C,0x36,0x1C,0x6E,0x3B,0x33,0x6E,0x00 }, // '&'
    { 0x06,0x06,0x03,0x00,0x00,0x00,0x00,0x00 }, // '\''
    { 0x18,0x0C,0x06,0x06,0x06,0x0C,0x18,0x00 }, // '('
    { 0x06,0x0C,0x18,0x18,0x18,0x0C,0x06,0x00 }, // ')'
    { 0x00,0x66,0x3C,0xFF,0x3C,0x66,0x00,0x00 }, // '*'
    { 0x00,0x0C,0x0C,0x3F,0x0C,0x0C,0x00,0x00 }, // '+'
    { 0x00,0x00,0x00,0x00,0x00,0x0C,0x0C,0x06 }, // ','
    { 0x00,0x00,0x00,0x3F,0x00,0x00,0x00,0x00 }, // '-'
    { 0x00,0x00,0x00,0x00,0x00,0x0C,0x0C,0x00 }, // '.'
    { 0x60,0x30,0x18,0x0C,0x06,0x03,0x01,0x00 }, // '/'
    { 0x3E,0x63,0x73,0x7B,0x6F,0x67,0x3E,0x00 }, // '0'
    { 0x0C,0x0E,0x0C,0x0C,0x0C,0x0C,0x3F,0x00 }, // '1'
    { 0x1E,0x33,0x30,0x1C,0x06,0x33,0x3F,0x00 }, // '2'
    { 0x1E,0x33,0x30,0x1C,0x30,0x33,0x1E,0x00 }, // '3'
    { 0x38,0x3C,0x36,0x33,0x7F,0x30,0x78,0x00 }, // '4'
    { 0x3F,0x03,0x1F,0x30,0x30,0x33,0x1E,0x00 }, // '5'
    { 0x1C,0x06,0x03,0x1F,0x33,0x33,0x1E,0x00 }, // '6'
    { 0x3F,0x33,0x30,0x18,0x0C,0x0C,0x0C,0x00 }, // '7'
    { 0x1E,0x33,0x33,0x1E,0x33,0x33,0x1E,0x00 }, // '8'
    { 0x1E,0x33,0x33,0x3E,0x30,0x18,0x0E,0x00 }, // '9'
    { 0x00,0x0C,0x0C,0x00,0x00,0x0C,0x0C,0x00 }, // ':'
    { 0x00,0x0C,0x0C,0x00,0x00,0x0C,0x0C,0x06 }, // ';'
    { 0x18,0x0C,0x06,0x03,0x06,0x0C,0x18,0x00 }, // '<'
    { 0x00,0x00,0x3F,0x00,0x00,0x3F,0x00,0x00 }, // '='
    { 0x06,0x0C,0x18,0x30,0x18,0x0C,0x06,0x00 }, // '>'
    { 0x1E,0x33,0x30,0x18,0x0C,0x00,0x0C,0x00 }, // '?'
    { 0x3E,0x63,0x7B,0x7B,0x7B,0x03,0x1E,0x00 }, // '@'
    { 0x0C,0x1E,0x33,0x33,0x3F,0x33,0x33,0x00 }, // 'A'
    { 0x3F,0x66,0x66,0x3E,0x66,0x66,0x3F,0x00 }, // 'B'
    { 0x3C,0x66,0x03,0x03,0x03,0x66,0x3C,0x00 }, // 'C'
    { 0x1F,0x36,0x66,0x66,0x66,0x36,0x1F,0x00 }, // 'D'
    { 0x7F,0x46,0x16,0x1E,0x16,0x46,0x7F,0x00 }, // 'E'
    { 0x7F,0x46,0x16,0x1E,0x16,0x06,0x0F,0x00 }, // 'F'
    { 0x3C,0x66,0x03,0x03,0x73,0x66,0x7C,0x00 }, // 'G'
    { 0x33,0x33,0x33,0x3F,0x33,0x33,0x33,0x00 }, // 'H'
    { 0x1E,0x0C,0x0C,0x0C,0x0C,0x0C,0x1E,0x00 }, // 'I'
    { 0x78,0x30,0x30,0x30,0x33,0x33,0x1E,0x00 }, // 'J'
    { 0x67,0x66,0x36,0x1E,0x36,0x66,0x67,0x00 }, // 'K'
    { 0x0F,0x06,0x06,0x06,0x46,0x66,0x7F,0x00 }, // 'L'
    { 0x63,0x77,0x7F,0x7F,0x6B,0x63,0x63,0x00 }, // 'M'
    { 0x63,0x67,0x6F,0x7B,0x73,0x63,0x63,0x00 }, // 'N'
    { 0x1C,0x36,0x63,0x63,0x63,0x36,0x1C,0x00 }, // 'O'
    { 0x3F,0x66,0x66,0x3E,0x06,0x06,0x0F,0x00 }, // 'P'
    { 0x1E,0x33,0x33,0x33,0x3B,0x1E,0x38,0x00 }, // 'Q'
    { 0x3F,0x66,0x66,0x3E,0x36,0x66,0x67,0x00 }, // 'R'
    { 0x1E,0x33,0x07,0x0E,0x38,0x33,0x1E,0x00 }, // 'S'
    { 0x3F,0x2D,0x0C,0x0C,0x0C,0x0C,0x1E,0x00 }, // 'T'
    { 0x33,0x33,0x33,0x33,0x33,0x33,0x3F,0x00 }, // 'U'
    { 0x33,0x33,0x33,0x33,0x33,0x1E,0x0C,0x00 }, // 'V'
    { 0x63,0x63,0x63,0x6B,0x7F,0x77,0x63,0x00 }, // 'W'
    { 0x63,0x63,0x36,0x1C,0x1C,0x36,0x63,0x00 }, // 'X'
    { 0x33,0x33,0x33,0x1E,0x0C,0x0C,0x1E,0x00 }, // 'Y'
    { 0x7F,0x63,0x31,0x18,0x4C,0x66,0x7F,0x00 }, // 'Z'
    { 0x1E,0x06,0x06,0x06,0x06,0x06,0x1E,0x00 }, // '['
    { 0x03,0x06,0x0C,0x18,0x30,0x60,0x40,0x00 }, // '\\'
    { 0x1E,0x18,0x18,0x18,0x18,0x18,0x1E,0x00 }, // ']'
    { 0x08,0x1C,0x36,0x63,0x00,0x00,0x00,0x00 }, // '^'
    { 0x00,0x00,0x00,0x00,0x00,0x00,0x00,0xFF }, // '_'
    { 0x0C,0x0C,0x18,0x00,0x00,0x00,0x00,0x00 }, // '`'
    { 0x00,0x00,0x1E,0x30,0x3E,0x33,0x6E,0x00 }, // 'a'
    { 0x07,0x06,0x06,0x3E,0x66,0x66,0x3B,0x00 }, // 'b'
    { 0x00,0x00,0x1E,0x33,0x03,0x33,0x1E,0x00 }, // 'c'
    { 0x38,0x30,0x30,0x3E,0x33,0x33,0x6E,0x00 }, // 'd'
    { 0x00,0x00,0x1E,0x33,0x3F,0x03,0x1E,0x00 }, // 'e'
    { 0x1C,0x36,0x06,0x0F,0x06,0x06,0x0F,0x00 }, // 'f'
    { 0x00,0x00,0x6E,0x33,0x33,0x3E,0x30,0x1F }, // 'g'
    { 0x07,0x06,0x36,0x6E,0x66,0x66,0x67,0x00 }, // 'h'
    { 0x0C,0x00,0x0E,0x0C,0x0C,0x0C,0x1E,0x00 }, // 'i'
    { 0x30,0x00,0x30,0x30,0x30,0x33,0x33,0x1E }, // 'j'
    { 0x07,0x06,0x66,0x36,0x1E,0x36,0x67,0x00 }, // 'k'
    { 0x0E,0x0C,0x0C,0x0C,0x0C,0x0C,0x1E,0x00 }, // 'l'
    { 0x00,0x00,0x33,0x7F,0x7F,0x6B,0x63,0x00 }, // 'm'
    { 0x00,0x00,0x1F,0x33,0x33,0x33,0x33,0x00 }, // 'n'
    { 0x00,0x00,0x1E,0x33,0x33,0x33,0x1E,0x00 }, // 'o'
    { 0x00,0x00,0x3B,0x66,0x66,0x3E,0x06,0x0F }, // 'p'
    { 0x00,0x00,0x6E,0x33,0x33,0x3E,0x30,0x78 }, // 'q'
    { 0x00,0x00,0x3B,0x6E,0x66,0x06,0x0F,0x00 }, // 'r'
    { 0x00,0x00,0x3E,0x03,0x1E,0x30,0x1F,0x00 }, // 's'
    { 0x08,0x0C,0x3E,0x0C,0x0C,0x2C,0x18,0x00 }, // 't'
    { 0x00,0x00,0x33,0x33,0x33,0x33,0x6E,0x00 }, // 'u'
    { 0x00,0x00,0x33,0x33,0x33,0x1E,0x0C,0x00 }, // 'v'
    { 0x00,0x00,0x63,0x6B,0x7F,0x7F,0x36,0x00 }, // 'w'
    { 0x00,0x00,0x63,0x36,0x1C,0x36,0x63,0x00 }, // 'x'
    { 0x00,0x00,0x33,0x33,0x33,0x3E,0x30,0x1F }, // 'y'
    { 0x00,0x00,0x3F,0x19,0x0C,0x26,0x3F,0x00 }, // 'z'
    { 0x38,0x0C,0x0C,0x07,0x0C,0x0C,0x38,0x00 }, // '{'
    { 0x18,0x18,0x18,0x00,0x18,0x18,0x18,0x00 }, // '|'
    { 0x07,0x0C,0x0C,0x38,0x0C,0x0C,0x07,0x00 }, // '}'
    { 0x6E,0x3B,0x00,0x00,0x00,0x00,0x00,0x00 }, // '~'
    { 0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF }, // DEL
};

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

// Draw a single character glyph at pixel origin (ox,oy), scaled by `scale`
static void draw_glyph(uint32_t* pixels, uint32_t stride_u32,
                       int ox, int oy, unsigned char ch,
                       uint32_t color, int scale,
                       uint32_t sw, uint32_t sh) {
    if (ch < 32 || ch > 127) ch = '?';
    const uint8_t* glyph = kFont8x8[ch - 32];
    for (int row = 0; row < 8; row++) {
        uint8_t bits = glyph[row];
        for (int col = 0; col < 8; col++) {
            if (!(bits & (1u << col))) continue;
            for (int sy = 0; sy < scale; sy++) {
                int py = oy + row*scale + sy;
                if (py < 0 || py >= (int)sh) continue;
                for (int sx = 0; sx < scale; sx++) {
                    int px = ox + col*scale + sx;
                    if (px >= 0 && px < (int)sw)
                        pixels[(uint32_t)py * stride_u32 + (uint32_t)px] = color;
                }
            }
        }
    }
}

// Draw text centred at (cx, cy)
static void draw_text_centred(uint32_t* pixels, uint32_t stride_u32,
                               const std::string& text,
                               uint32_t cx, uint32_t cy,
                               uint32_t color, int scale,
                               uint32_t sw, uint32_t sh) {
    if (text.empty() || scale < 1) return;
    int cw = 8 * scale;
    int ch = 8 * scale;
    int ox = (int)cx - (int)(text.size() * cw) / 2;
    int oy = (int)cy - ch / 2;
    for (size_t i = 0; i < text.size(); i++)
        draw_glyph(pixels, stride_u32, ox + (int)i*cw, oy,
                   (unsigned char)text[i], color, scale, sw, sh);
}

// Draw text right-aligned: right edge at rx, top at ty
static void draw_text_right(uint32_t* pixels, uint32_t stride_u32,
                             const std::string& text,
                             uint32_t rx, uint32_t ty,
                             uint32_t color, int scale,
                             uint32_t sw, uint32_t sh) {
    if (text.empty() || scale < 1) return;
    int cw = 8 * scale;
    int ox = (int)rx - (int)(text.size() * cw);
    for (size_t i = 0; i < text.size(); i++)
        draw_glyph(pixels, stride_u32, ox + (int)i * cw, (int)ty,
                   (unsigned char)text[i], color, scale, sw, sh);
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

#ifdef HAVE_RGA
#include <rga/im2d.hpp>
#include <rga/RgaUtils.h>
#include <atomic>
// libRGA uses a global singleton that can only be safely initialised once.
// Gate all init checks behind this flag so multiple DRMDisplay instances
// don't race and corrupt the singleton.
static std::atomic<int> g_rga_state{0};  // 0=unknown, 1=ok, -1=unavailable
static constexpr int kRgaMaxRetries = 3; // retry blit before giving up
#endif

// BT.601 YUV->RGB, clamped to [0,255]
static inline uint32_t yuv_to_xrgb(int y, int u, int v) {
    int r = y + ((359 * (v - 128)) >> 8);
    int g = y - ((88  * (u - 128) + 183 * (v - 128)) >> 8);
    int b = y + ((453 * (u - 128)) >> 8);
    if (r < 0) r = 0; if (r > 255) r = 255;
    if (g < 0) g = 0; if (g > 255) g = 255;
    if (b < 0) b = 0; if (b > 255) b = 255;
    return (0xFF000000u) | ((uint32_t)r << 16) | ((uint32_t)g << 8) | (uint32_t)b;
}

// ---------------------------------------------------------------------------
// ARM NEON accelerated YUV→XRGB conversion (BT.601)
// Processes 8 pixels at a time — ~6-8x faster than scalar on A72/A53.
// ---------------------------------------------------------------------------
#if defined(__aarch64__) || defined(__ARM_NEON)
#include <arm_neon.h>

// Convert 8 Y values + 8 U/V values to 8 XRGB pixels using NEON.
// BT.601: R = Y + 1.402*(V-128),  G = Y - 0.344*(U-128) - 0.714*(V-128),  B = Y + 1.772*(U-128)
// Fixed-point: R = Y + (359*(V-128))>>8,  G = Y - (88*(U-128) + 183*(V-128))>>8,  B = Y + (453*(U-128))>>8
static inline void neon_yuv_to_xrgb_8px(const uint8_t* y_ptr, const uint8_t* u_ptr,
                                         const uint8_t* v_ptr, uint32_t* dst) {
    // Load 8 Y/U/V values and widen to 16-bit signed
    int16x8_t y16 = vreinterpretq_s16_u16(vmovl_u8(vld1_u8(y_ptr)));
    int16x8_t u16 = vreinterpretq_s16_u16(vmovl_u8(vld1_u8(u_ptr)));
    int16x8_t v16 = vreinterpretq_s16_u16(vmovl_u8(vld1_u8(v_ptr)));

    // Subtract 128 from U and V
    int16x8_t u_off = vsubq_s16(u16, vdupq_n_s16(128));
    int16x8_t v_off = vsubq_s16(v16, vdupq_n_s16(128));

    // R = Y + (359*V')>>8
    int16x8_t r16 = vaddq_s16(y16, vshrq_n_s16(vmulq_n_s16(v_off, 359), 8));
    // G = Y - (88*U' + 183*V')>>8
    int16x8_t g16 = vsubq_s16(y16, vshrq_n_s16(vaddq_s16(vmulq_n_s16(u_off, 88),
                                                            vmulq_n_s16(v_off, 183)), 8));
    // B = Y + (453*U')>>8
    int16x8_t b16 = vaddq_s16(y16, vshrq_n_s16(vmulq_n_s16(u_off, 453), 8));

    // Clamp to [0,255] and narrow to u8
    uint8x8_t r8 = vqmovun_s16(r16);
    uint8x8_t g8 = vqmovun_s16(g16);
    uint8x8_t b8 = vqmovun_s16(b16);
    uint8x8_t a8 = vdup_n_u8(0xFF);

    // Interleave to XRGB (stored as BGRA in memory = 0xFFRRGGBB little-endian)
    // Actually stored as B, G, R, A bytes in memory → XRGB when read as uint32_t LE
    uint8x8x4_t rgba;
    rgba.val[0] = b8;
    rgba.val[1] = g8;
    rgba.val[2] = r8;
    rgba.val[3] = a8;
    vst4_u8((uint8_t*)dst, rgba);
}

// NEON row converter: UYVY source → XRGB destination (1:1, no scaling)
// Fully NEON — no scalar extraction loops inside the hot path.
// UYVY layout: [U0 Y0 V0 Y1] [U2 Y2 V2 Y3] ... (4 bytes per macro-pixel, 2 pixels each)
// vld4_u8 loads 32 bytes and deinterleaves into 4 x uint8x8 registers:
//   val[0]=U, val[1]=Y_even, val[2]=V, val[3]=Y_odd   (8 macro-pixels = 16 pixels)
static void neon_uyvy_row_to_xrgb(const uint8_t* src, uint32_t* dst, uint32_t width) {
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
                              (const uint8_t*)&v_zip.val[0], dst + x);
        // Convert next 8 pixels
        neon_yuv_to_xrgb_8px((const uint8_t*)&y_zip.val[1],
                              (const uint8_t*)&u_zip.val[1],
                              (const uint8_t*)&v_zip.val[1], dst + x + 8);
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
        neon_yuv_to_xrgb_8px(ybuf, ubuf, vbuf, dst + x);
        x += 8;
    }
    // Scalar tail (< 8 pixels)
    for (; x < width; x++) {
        uint32_t pair = x & ~1u;
        int u = src[pair * 2 + 0];
        int y = src[pair * 2 + 1 + (x & 1u) * 2];
        int v = src[pair * 2 + 2];
        dst[x] = yuv_to_xrgb(y, u, v);
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
                                          uint32_t x_step_fp16) {
    // Convert full source row to XRGB via NEON into a temp buffer
    // Use thread_local to avoid per-call allocation
    thread_local std::vector<uint32_t> line_buf;
    if (line_buf.size() < src_w) line_buf.resize(src_w);

    neon_uyvy_row_to_xrgb(src_row, line_buf.data(), src_w);

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

#ifdef HAVE_RGA
    {
        int state = g_rga_state.load();
        if (state == 0) {
            // First instance: probe RGA once and cache result globally
            if (access("/dev/rga", F_OK) == 0 && imcheckHeader() == IM_STATUS_SUCCESS) {
                g_rga_state = 1;
                rga_available_ = true;
                std::cout << "[DRM] RGA hardware color conversion available\n";
            } else {
                g_rga_state = -1;
                if (access("/dev/rga", F_OK) != 0)
                    std::cout << "[DRM] /dev/rga not found, using software color conversion\n";
                else
                    std::cerr << "[DRM] RGA header version mismatch, falling back to software\n";
            }
        } else if (state == 1) {
            rga_available_ = true;
            // No log — already reported by first instance
        }
        // state == -1: leave rga_available_ = false (default)
    }
#endif

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

#ifdef HAVE_RGA
    {
        int state = g_rga_state.load();
        if (state == 0) {
            if (access("/dev/rga", F_OK) == 0 && imcheckHeader() == IM_STATUS_SUCCESS) {
                g_rga_state = 1;
                rga_available_ = true;
                std::cout << "[DRM] RGA hardware color conversion available\n";
            } else {
                g_rga_state = -1;
                if (access("/dev/rga", F_OK) != 0)
                    std::cout << "[DRM] /dev/rga not found, using software color conversion\n";
                else
                    std::cerr << "[DRM] RGA header version mismatch, falling back to software\n";
            }
        } else if (state == 1) {
            rga_available_ = true;
        }
    }
#endif

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
                // Cache rotation property ID for hardware rotation support
                rotation_prop_id_ = get_prop_id(drm_fd_, plane_id_,
                                                 DRM_MODE_OBJECT_PLANE, "rotation");
                if (rotation_prop_id_)
                    std::cout << "[DRM] Rotation property available on plane " << plane_id_ << "\n";
            }
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
    if (plane_id_ && (rotation_drm_ != 1 || atomic_plane_state_ == 1)) {
        Rect full_src = {0, 0, width_, height_};
        Rect full_dst = {0, 0, width_, height_};
        return atomic_plane_commit(fb_id, full_src, full_dst);
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

bool DRMDisplay::atomic_plane_commit(uint32_t fb_id, const Rect& src, const Rect& dst) {
    if (!plane_id_) return false;

    uint32_t p_fb   = get_prop_id(drm_fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "FB_ID");
    uint32_t p_cid  = get_prop_id(drm_fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "CRTC_ID");
    uint32_t p_sx   = get_prop_id(drm_fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "SRC_X");
    uint32_t p_sy   = get_prop_id(drm_fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "SRC_Y");
    uint32_t p_sw   = get_prop_id(drm_fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "SRC_W");
    uint32_t p_sh   = get_prop_id(drm_fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "SRC_H");
    uint32_t p_cx   = get_prop_id(drm_fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "CRTC_X");
    uint32_t p_cy   = get_prop_id(drm_fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "CRTC_Y");
    uint32_t p_cw   = get_prop_id(drm_fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "CRTC_W");
    uint32_t p_ch   = get_prop_id(drm_fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "CRTC_H");

    if (!p_fb || !p_cid || !p_sx || !p_sy || !p_sw || !p_sh ||
        !p_cx || !p_cy || !p_cw || !p_ch)
        return false;

    // First atomic commit must also set the CRTC mode
    drmModeAtomicReq* req = drmModeAtomicAlloc();
    if (!req) return false;

    uint32_t flags = DRM_MODE_ATOMIC_NONBLOCK | DRM_MODE_PAGE_FLIP_EVENT;

    if (!crtc_active_) {
        // First commit: need ALLOW_MODESET and connector/CRTC properties
        flags |= DRM_MODE_ATOMIC_ALLOW_MODESET;
        uint32_t c_crtc = get_prop_id(drm_fd_, connector_id_, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID");
        uint32_t crtc_active = get_prop_id(drm_fd_, crtc_id_, DRM_MODE_OBJECT_CRTC, "ACTIVE");
        uint32_t crtc_mode = get_prop_id(drm_fd_, crtc_id_, DRM_MODE_OBJECT_CRTC, "MODE_ID");
        if (c_crtc) drmModeAtomicAddProperty(req, connector_id_, c_crtc, crtc_id_);
        if (crtc_active) drmModeAtomicAddProperty(req, crtc_id_, crtc_active, 1);
        if (crtc_mode) {
            uint32_t blob_id = 0;
            drmModeCreatePropertyBlob(drm_fd_, &mode_, sizeof(mode_), &blob_id);
            if (blob_id)
                drmModeAtomicAddProperty(req, crtc_id_, crtc_mode, blob_id);
        }
    }

    // Drain any pending flip
    wait_for_flip();
    if (flip_pending_.load() && crtc_active_) {
        drmModeAtomicFree(req);
        return true; // drop frame rather than block
    }

    // SRC rect is in 16.16 fixed point
    drmModeAtomicAddProperty(req, plane_id_, p_fb,  fb_id);
    drmModeAtomicAddProperty(req, plane_id_, p_cid, crtc_id_);
    drmModeAtomicAddProperty(req, plane_id_, p_sx,  (uint64_t)src.x << 16);
    drmModeAtomicAddProperty(req, plane_id_, p_sy,  (uint64_t)src.y << 16);
    drmModeAtomicAddProperty(req, plane_id_, p_sw,  (uint64_t)src.w << 16);
    drmModeAtomicAddProperty(req, plane_id_, p_sh,  (uint64_t)src.h << 16);
    drmModeAtomicAddProperty(req, plane_id_, p_cx,  dst.x);
    drmModeAtomicAddProperty(req, plane_id_, p_cy,  dst.y);
    drmModeAtomicAddProperty(req, plane_id_, p_cw,  dst.w);
    drmModeAtomicAddProperty(req, plane_id_, p_ch,  dst.h);

    // Hardware rotation (VOP2 Smart/Esmart planes)
    if (rotation_prop_id_ && rotation_drm_ != 1)
        drmModeAtomicAddProperty(req, plane_id_, rotation_prop_id_, rotation_drm_);

    flip_pending_ = true;
    int ret = drmModeAtomicCommit(drm_fd_, req, flags, this);
    drmModeAtomicFree(req);

    if (ret != 0) {
        flip_pending_ = false;
        if (!crtc_active_) {
            // Can't use atomic — caller should fall back to legacy path
            return false;
        }
        // Non-first frame failed: log once and return
        return false;
    }

    crtc_active_ = true;
    return true;
}

// ---------------------------------------------------------------------------
// set_rotation — hardware rotation via VOP2 plane property
// ---------------------------------------------------------------------------
bool DRMDisplay::set_rotation(uint32_t degrees) {
    uint32_t drm_val;
    switch (degrees) {
        case 0:   drm_val = 1;    break;  // DRM_MODE_ROTATE_0
        case 90:  drm_val = 2;    break;  // DRM_MODE_ROTATE_90
        case 180: drm_val = 0x30; break;  // reflect-x | reflect-y
        case 270: drm_val = 8;    break;  // DRM_MODE_ROTATE_270
        default:
            std::cerr << "[DRM] Invalid rotation: " << degrees << "\n";
            return false;
    }

    if (!rotation_prop_id_) {
        std::cerr << "[DRM] Rotation not supported on this plane\n";
        return false;
    }

    rotation_degrees_ = degrees;
    rotation_drm_     = drm_val;
    invalidate_fill_cache();  // force bg redraw with new rotation

    // Cluster planes (UYVY-capable) only support rotate-0 and reflect-y.
    // Disable UYVY native path for any non-zero rotation so we fall through to
    // software XRGB conversion on Smart/Esmart planes which support full rotation.
    if (degrees != 0) {
        for (auto& b : yuv_fb_) free_fb(b);
        yuv_fb_state_ = -1;  // mark UYVY unsupported
    } else if (yuv_fb_state_ == -1) {
        yuv_fb_state_ = 0;   // re-enable UYVY probing at 0°
    }

    std::cout << "[DRM] Rotation set to " << degrees << "° (DRM value 0x"
              << std::hex << drm_val << std::dec << ")\n";
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
// fill_bg
// ---------------------------------------------------------------------------
// Fills the black letterbox/crop bars in the CURRENT buffer (fb_[cur_buf_]).
// Cache is tracked per-buffer so double-buffering doesn't cause stale bar
// content from a prior show_splash() call to bleed through.
void DRMDisplay::fill_bg(DRMBuffer& buf) {
    if (scale_mode_ == ScaleMode::Stretch) return;

    int idx = cur_buf_;  // which slot in the double-buffer ring
    if (buf.width  == last_bg_fill_w_[idx] &&
        buf.height == last_bg_fill_h_[idx] &&
        scale_mode_ == last_bg_scale_[idx]) {
        return;
    }

    if (buf.map) memset(buf.map, 0, buf.size);

    last_bg_fill_w_[idx]  = buf.width;
    last_bg_fill_h_[idx]  = buf.height;
    last_bg_scale_[idx]   = scale_mode_;
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
                                   const Rect& dr, uint32_t /*out_w*/, uint32_t /*out_h*/) {
    const uint8_t* y_plane  = src;
    const uint8_t* uv_plane = src + (size_t)src_stride * src_h;

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
                                   const Rect& dr, uint32_t /*out_w*/, uint32_t /*out_h*/) {
    if (dr.w == 0 || dr.h == 0) return;

    uint32_t y_step = (src_h << 16) / dr.h;

#if defined(__aarch64__) || defined(__ARM_NEON)
    if (src_w == dr.w && src_h == dr.h) {
        // No scaling — single-threaded NEON 1:1
        for (uint32_t sy = 0; sy < src_h; sy++) {
            const uint8_t* s = src + (size_t)sy * src_stride;
            uint32_t* d = (uint32_t*)(dst + (size_t)(dr.y + sy) * dst_stride) + dr.x;
            neon_uyvy_row_to_xrgb(s, d, src_w);
        }
        return;
    }

    // Scaling path — single-threaded NEON convert + horizontal resample
    uint32_t x_step = (src_w << 16) / dr.w;
    for (uint32_t row = 0; row < dr.h; row++) {
        uint32_t sy = ((uint64_t)row * y_step) >> 16;
        const uint8_t* s = src + (size_t)sy * src_stride;
        uint32_t* d = (uint32_t*)(dst + (size_t)(dr.y + row) * dst_stride) + dr.x;
        neon_uyvy_scale_row_to_xrgb(s, src_w, d, dr.w, x_step);
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
            d[dx] = yuv_to_xrgb(y, u, v);
        }
    }
#endif
}

// ---------------------------------------------------------------------------
// UYVY native plane path — VOP2 Cluster handles YUV→RGB during scanout.
// ---------------------------------------------------------------------------

bool DRMDisplay::alloc_yuv_fb_if_needed() {
    if (yuv_fb_state_ == 1) {
        if (yuv_fb_[0].width == width_ && yuv_fb_[0].height == height_)
            return true;
        // Wrong size — free and reallocate
        for (auto& b : yuv_fb_) free_fb(b);
        yuv_fb_state_ = 0;
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
// rga_blit (RGA hardware path)
// ---------------------------------------------------------------------------
bool DRMDisplay::rga_blit(const void* src_va, int src_fd,
                           uint32_t src_w, uint32_t src_h, uint32_t src_stride,
                           uint32_t src_rga_fmt,
                           void* dst_va, uint32_t dst_stride,
                           const Rect& dr, uint32_t dst_w, uint32_t dst_h) {
#ifdef HAVE_RGA
    if (!rga_available_) return false;

    rga_buffer_t src_buf, dst_buf;
    if (src_fd >= 0) {
        src_buf = wrapbuffer_fd(src_fd, src_w, src_h, (int)src_rga_fmt);
    } else if (src_va) {
        src_buf = wrapbuffer_virtualaddr(const_cast<void*>(src_va),
                                          src_w, src_h, (int)src_rga_fmt);
    } else {
        return false;
    }
    // wstride is in PIXELS, not bytes.
    // Packed formats (UYVY/YUYV): 2 bytes per pixel → pixels = bytes/2
    // Planar formats (NV12/NV16): Y plane is 1 byte per pixel → pixels = bytes
    bool is_packed = (src_rga_fmt == (uint32_t)RK_FORMAT_UYVY_422 ||
                      src_rga_fmt == (uint32_t)RK_FORMAT_YUYV_422);
    src_buf.wstride = is_packed ? (int)(src_stride / 2) : (int)src_stride;

    // Output is XRGB8888 (RK_FORMAT_BGRA_8888 in RGA terminology)
    dst_buf = wrapbuffer_virtualaddr(dst_va, dst_w, dst_h, RK_FORMAT_BGRA_8888);
    dst_buf.wstride = (int)(dst_stride / 4); // BGRA8888: 4 bytes per pixel

    im_rect src_rect = { 0, 0, (int)src_w, (int)src_h };
    im_rect dst_rect = { (int)dr.x, (int)dr.y, (int)dr.w, (int)dr.h };

    IM_STATUS status = improcess(src_buf, dst_buf, {}, src_rect, dst_rect, {}, IM_SYNC);
    if (status != IM_STATUS_SUCCESS) {
        rga_fail_count_++;
        if (rga_fail_count_ >= kRgaMaxRetries) {
            std::cerr << "[DRM] RGA blit failed " << rga_fail_count_
                      << " times: " << imStrError(status) << " — disabling RGA\n";
            rga_available_ = false;
        } else {
            std::cerr << "[DRM] RGA blit failed (attempt " << rga_fail_count_
                      << "/" << kRgaMaxRetries << "): " << imStrError(status) << "\n";
        }
        return false;
    }
    rga_fail_count_ = 0;  // reset on success
    return true;
#else
    (void)src_va; (void)src_fd; (void)src_w; (void)src_h; (void)src_stride;
    (void)src_rga_fmt; (void)dst_va; (void)dst_stride; (void)dr; (void)dst_w; (void)dst_h;
    return false;
#endif
}

// ---------------------------------------------------------------------------
// show_frame_memory
// ---------------------------------------------------------------------------
bool DRMDisplay::show_frame_memory(const uint8_t* data, size_t /*size*/,
                                    uint32_t frame_w, uint32_t frame_h,
                                    uint32_t stride, uint32_t drm_format) {
    if (!initialized_) return false;
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

        fill_bg(buf);
        Rect dr = compute_dst_rect(frame_w, frame_h);
        uint32_t src_stride_bytes = stride ? stride : frame_w * 4u;
        uint32_t copy_w = std::min(frame_w, dr.w) * 4u;
        uint32_t copy_h = std::min(frame_h, dr.h);

        for (uint32_t y = 0; y < copy_h; y++) {
            memcpy((uint8_t*)buf.map + (size_t)(dr.y + y) * buf.stride + dr.x * 4u,
                   data + (size_t)y * src_stride_bytes,
                   copy_w);
        }

        streaming_ = true;
        if (osd_enabled_ && !osd_text_.empty()) {
            uint32_t* px = (uint32_t*)buf.map;
            draw_text_centred(px, buf.stride / 4, osd_text_,
                              width_ / 2, (uint32_t)(0.04f * height_),
                              0xFFFFFFFFu, 2, width_, height_);
        }
        bool ok = commit_fb(buf.fb_id);
        if (ok) cur_buf_ = (cur_buf_ + 1) % kNumBuffers;
        return true;
    }

    // UYVY native path: feed 16bpp UYVY dumb buffer to the VOP2 Cluster plane.
    // The display controller handles YUV→RGB during scanout — zero CPU colour math.
    // If commit_fb fails (plane doesn't support UYVY in this lease), fall back
    // to the software XRGB conversion path and mark UYVY as unsupported.
    if (drm_format == DRM_FORMAT_UYVY && data && alloc_yuv_fb_if_needed()) {
        DRMBuffer& ybuf = yuv_fb_[cur_yuv_buf_];
        if (ybuf.map) {
            fill_bg_yuv(ybuf);
            Rect dr = compute_dst_rect(frame_w, frame_h);
            uint32_t src_stride_bytes = stride ? stride : frame_w * 2u;
            sw_uyvy_scale(data, frame_w, frame_h, src_stride_bytes,
                          static_cast<uint8_t*>(ybuf.map), ybuf.stride, dr);
            streaming_ = true;
            bool ok = commit_fb(ybuf.fb_id);
            if (ok) {
                cur_yuv_buf_ = (cur_yuv_buf_ + 1) % kNumBuffers;
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

    uint32_t src_stride = stride ? stride : frame_w;

    // Try RGA first (writes directly to DRM buffer — single-threaded, no tearing)
    bool rga_ok = false;
#ifdef HAVE_RGA
    if (rga_available_ && data) {
        fill_bg(buf);  // RGA writes directly to DRM buffer, needs letterbox bars
        uint32_t rga_fmt = 0;
        bool rga_supported = true;
        if (drm_format == DRM_FORMAT_NV12)
            rga_fmt = RK_FORMAT_YCbCr_420_SP;
        else if (drm_format == DRM_FORMAT_NV16)
            rga_fmt = RK_FORMAT_YCbCr_422_SP;
        else if (drm_format == DRM_FORMAT_UYVY)
            rga_fmt = RK_FORMAT_UYVY_422;
        else if (drm_format == DRM_FORMAT_YUYV)
            rga_fmt = RK_FORMAT_YUYV_422;
        else
            rga_supported = false;

        if (rga_supported) {
            rga_ok = rga_blit(data, -1, frame_w, frame_h, src_stride, rga_fmt,
                              buf.map, buf.stride, dr, width_, height_);
        }
    }
#endif

    if (!rga_ok && data) {
        // Software fallback — NEON convert directly to DRM back buffer.
        // Triple buffering (3 buffers) ensures this buffer is never being
        // scanned out, so direct writes are tear-free.
        fill_bg(buf);

        if (drm_format == DRM_FORMAT_NV12 ||
            drm_format == DRM_FORMAT_NV15 ||
            drm_format == DRM_FORMAT_NV16) {
            sw_nv12_to_xrgb(data, frame_w, frame_h,
                             src_stride,
                             (uint8_t*)buf.map, buf.stride,
                             dr, width_, height_);
        } else if (drm_format == DRM_FORMAT_UYVY) {
            sw_uyvy_to_xrgb(data, frame_w, frame_h,
                             stride ? stride : frame_w * 2,
                             (uint8_t*)buf.map, buf.stride,
                             dr, width_, height_);
        } else {
            uint32_t src_row_bytes = stride ? stride : frame_w * 2u;
            sw_uyvy_to_xrgb(data, frame_w, frame_h, src_row_bytes,
                             (uint8_t*)buf.map, buf.stride,
                             dr, width_, height_);
        }
    }

    if (osd_enabled_ && !osd_text_.empty()) {
        uint32_t* px = (uint32_t*)buf.map;
        draw_text_centred(px, buf.stride / 4, osd_text_,
                          width_ / 2, (uint32_t)(0.04f * height_),
                          0xFFFFFFFFu, 2, width_, height_);
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
                    Rect src_rect = { 0, 0, frame_w, frame_h };
                    if (atomic_plane_commit(a_fb, src_rect, dr)) {
                        // Clean up previous import state
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
            // Atomic failed — mark as unsupported and fall through to RGA/software
            if (atomic_plane_state_ == 0) {
                atomic_plane_state_ = -1;
                std::cerr << "[DRM] Atomic plane commit failed — falling back to RGA/software\n";
            }
        }

        DRMBuffer& buf = fb_[cur_buf_];
        if (!buf.fb_id || buf.width != width_ || buf.height != height_) {
            free_fb(buf);
            if (!alloc_fb(buf, width_, height_)) return false;
        }
        if (!buf.map) return false;

        fill_bg(buf);

        bool rga_ok = false;
#ifdef HAVE_RGA
        if (rga_available_) {
            uint32_t rga_fmt = 0;
            bool rga_supported = true;
            if (format == DRM_FORMAT_NV12)
                rga_fmt = RK_FORMAT_YCbCr_420_SP;
            else if (format == DRM_FORMAT_NV16)
                rga_fmt = RK_FORMAT_YCbCr_422_SP;
            else if (format == DRM_FORMAT_UYVY)
                rga_fmt = RK_FORMAT_UYVY_422;
            else if (format == DRM_FORMAT_YUYV)
                rga_fmt = RK_FORMAT_YUYV_422;
            else
                rga_supported = false;

            if (rga_supported) {
                rga_ok = rga_blit(nullptr, dma_fd, frame_w, frame_h, stride_y, rga_fmt,
                                  buf.map, buf.stride, dr, width_, height_);
            }
        }
#endif
        if (!rga_ok) {
            // RGA unavailable — NEON convert directly to DRM back buffer.
            // Triple buffering ensures this buffer is not being scanned out.
            fill_bg(buf);

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
                    sw_nv12_to_xrgb((const uint8_t*)mapped,
                                    frame_w, frame_h, stride_y,
                                    (uint8_t*)buf.map, buf.stride,
                                    dr, width_, height_);
                } else {
                    if (buf.map) memset(buf.map, 0, buf.size);
                }
            } else {
                if (buf.map) memset(buf.map, 0, buf.size);
            }
        }

        if (osd_enabled_ && !osd_text_.empty()) {
            uint32_t* px = (uint32_t*)buf.map;
            draw_text_centred(px, buf.stride / 4, osd_text_,
                              width_ / 2, (uint32_t)(0.04f * height_),
                              0xFFFFFFFFu, 2, width_, height_);
        }

        streaming_ = true;

        bool ok = commit_fb(buf.fb_id);
        if (ok) cur_buf_ = (cur_buf_ + 1) % kNumBuffers;
        return true;
    }

    streaming_ = true;
    // Stretch mode: try direct import and flip
    if (import_fb_id_) {
        drmModeRmFB(drm_fd_, import_fb_id_);
        import_fb_id_ = 0;
    }
    if (import_bo_) {
        struct drm_gem_close c = {};
        c.handle = import_bo_;
        drmIoctl(drm_fd_, DRM_IOCTL_GEM_CLOSE, &c);
        import_bo_ = 0;
    }

    if (drmPrimeFDToHandle(drm_fd_, dma_fd, &import_bo_) != 0) {
        std::cerr << "[DRM] drmPrimeFDToHandle failed: " << strerror(errno) << "\n";
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
        import_bo_ = 0;
        return false;
    }

    return commit_fb(import_fb_id_);
}

// ---------------------------------------------------------------------------
// show_splash  — reads layout/colour settings from Config::instance().splash
// ---------------------------------------------------------------------------
bool DRMDisplay::show_splash(bool source_available) {
    if (!initialized_) return false;
    std::lock_guard<std::mutex> lk(frame_mutex_);
    // Clear streaming flag — caller has already stopped the video pipeline.
    // This ensures splash actually renders rather than silently returning.
    streaming_ = false;

    DRMBuffer& buf = fb_[cur_buf_];
    if (!buf.fb_id || buf.width != width_ || buf.height != height_) {
        free_fb(buf);
        if (!alloc_fb(buf, width_, height_)) return false;
    }
    if (!buf.map) return false;

    uint32_t* pixels = (uint32_t*)buf.map;
    uint32_t stride_u32 = buf.stride / 4;

    const SplashConfig& sc = Config::instance().splash;

    uint32_t bg_color     = hex_to_u32(source_available ? sc.bg_live    : sc.bg_idle);
    uint32_t accent_color = hex_to_u32(source_available ? sc.accent_live : sc.accent_idle);
    // Darken accent for the box fill (50% blend toward bg)
    uint32_t box_r = (((bg_color>>16)&0xFF) + ((accent_color>>16)&0xFF)) / 2;
    uint32_t box_g = (((bg_color>> 8)&0xFF) + ((accent_color>> 8)&0xFF)) / 2;
    uint32_t box_b = (( bg_color     &0xFF) + ( accent_color     &0xFF)) / 2;
    uint32_t box_color = 0xFF000000u | (box_r<<16) | (box_g<<8) | box_b;

    // ── Fill background ──────────────────────────────────────────────────────
    for (uint32_t y = 0; y < height_; y++)
        for (uint32_t x = 0; x < width_; x++)
            pixels[y * stride_u32 + x] = bg_color;

    // ── Decorative centre box ────────────────────────────────────────────────
    if (sc.show_box) {
        uint32_t bx0 = width_  / 3,  bx1 = width_  * 2 / 3;
        uint32_t by0 = height_ / 3,  by1 = height_ * 2 / 3;

        for (uint32_t y = by0; y < by1; y++)
            for (uint32_t x = bx0; x < bx1; x++)
                pixels[y * stride_u32 + x] = box_color;

        for (uint32_t b = 0; b < 2; b++) {
            uint32_t top = by0+b, bot = by1-1-b;
            uint32_t lft = bx0+b, rgt = bx1-1-b;
            for (uint32_t x = bx0; x < bx1; x++) {
                pixels[top * stride_u32 + x] = accent_color;
                pixels[bot * stride_u32 + x] = accent_color;
            }
            for (uint32_t y = by0; y < by1; y++) {
                pixels[y * stride_u32 + lft] = accent_color;
                pixels[y * stride_u32 + rgt] = accent_color;
            }
        }
    }

    // ── Logo image ───────────────────────────────────────────────────────────
    if (!sc.logo_path.empty()) {
        int iw = 0, ih = 0, ic = 0;
        uint8_t* img = stbi_load(sc.logo_path.c_str(), &iw, &ih, &ic, 0);
        if (img) {
            uint32_t cx = (uint32_t)(sc.logo_x_pct / 100.0f * width_);
            uint32_t cy = (uint32_t)(sc.logo_y_pct / 100.0f * height_);
            uint32_t dw = (uint32_t)(sc.logo_w_pct / 100.0f * width_);
            draw_logo(pixels, stride_u32, img, iw, ih, ic, cx, cy, dw, width_, height_);
            stbi_image_free(img);
        } else {
            std::cerr << "[DRM] splash logo load failed: " << sc.logo_path
                      << " — " << stbi_failure_reason() << "\n";
        }
    }

    // ── Status label (centre) ────────────────────────────────────────────────
    const std::string& label = source_available ? sc.text_live : sc.text_idle;
    if (!label.empty() && sc.text_scale >= 1) {
        int scale = std::min(sc.text_scale, 8);
        draw_text_centred(pixels, stride_u32, label,
                          width_ / 2, height_ * 2 / 3,
                          accent_color, scale, width_, height_);
    }

    // ── Device info — top-right: NDI alias + IP ──────────────────────────────
    {
        const auto& dev = Config::instance().device;
        int info_scale = std::max(1, std::min(sc.text_scale, 8));
        int line_h = 8 * info_scale + 4;  // glyph height + small gap
        uint32_t margin = (uint32_t)(8 * info_scale);
        uint32_t rx = width_ - margin;
        uint32_t ty = margin;

        if (!dev.device_name.empty())
            draw_text_right(pixels, stride_u32, dev.device_name,
                            rx, ty, accent_color, info_scale, width_, height_);
        if (!dev.device_ip.empty())
            draw_text_right(pixels, stride_u32, dev.device_ip,
                            rx, ty + (uint32_t)line_h, accent_color, info_scale, width_, height_);
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
bool DRMDisplay::set_mode(uint32_t w, uint32_t h, double refresh_hz) {
    if (!initialized_) return false;

    std::lock_guard<std::mutex> lk(frame_mutex_);

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
        return false;
    }

    // Find the exact drmModeModeInfo by pixel-clock match against available_modes_ entry
    drmModeRes* res = drmModeGetResources(drm_fd_);
    if (!res) return false;

    drmModeConnector* conn = nullptr;
    for (int i = 0; i < res->count_connectors; i++) {
        conn = drmModeGetConnector(drm_fd_, res->connectors[i]);
        if (conn && conn->connector_id == connector_id_) break;
        if (conn) { drmModeFreeConnector(conn); conn = nullptr; }
    }
    drmModeFreeResources(res);
    if (!conn) return false;

    bool found = false;
    for (int m = 0; m < conn->count_modes; m++) {
        if ((uint32_t)conn->modes[m].hdisplay == w &&
            (uint32_t)conn->modes[m].vdisplay == h) {
            double hz = calc_refresh_hz(conn->modes[m]);
            if (std::abs(hz - match->refresh_hz) < 0.02) {
                mode_        = conn->modes[m];
                width_       = w;
                height_      = h;
                vrefresh_    = conn->modes[m].vrefresh;
                vrefresh_hz_ = hz;
                found        = true;
                break;
            }
        }
    }
    drmModeFreeConnector(conn);
    if (!found) return false;

    // Free existing FBs (sized for old mode)
    for (auto& b : fb_)     free_fb(b);
    for (auto& b : yuv_fb_) free_fb(b);
    yuv_fb_state_ = 0;
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

#ifdef HAVE_RGA
    // Reset RGA state on mode change — the initial failure may have been
    // transient (lazy kernel driver init on RK3399).
    rga_fail_count_ = 0;
    if (!rga_available_ && g_rga_state.load() != -1) {
        rga_available_ = true;
    }
#endif

    // Reallocate at new size and show black to activate mode
    for (auto& b : fb_) {
        if (!alloc_fb(b, width_, height_)) {
            std::cerr << "[DRM] set_mode: alloc_fb failed\n";
            return false;
        }
    }

    if (!show_black_impl()) {
        std::cerr << "[DRM] set_mode: failed to activate new mode\n";
        return false;
    }

    std::cout << "[DRM] Mode changed to " << w << "x" << h << "@" << vrefresh_hz_ << "\n";
    return true;
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
        if (owns_fd_) {
            std::cout << "[DRM] Closing DRM device\n";
            close(drm_fd_);
        } else {
            std::cout << "[DRM] Releasing DRM lease fd\n";
            close(drm_fd_);
        }
        drm_fd_ = -1;
    }

    initialized_  = false;
    crtc_active_  = false;
    flip_pending_ = false;
    available_modes_.clear();
    width_ = height_ = vrefresh_ = 0;
    connector_id_ = crtc_id_ = 0;
    cur_buf_ = 0;
    cur_yuv_buf_  = 0;
    yuv_fb_state_ = 0;
    invalidate_fill_cache();
}
