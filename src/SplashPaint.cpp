// SplashPaint — see SplashPaint.h.
#include "SplashPaint.h"

#include <algorithm>
#include <cmath>

namespace ndimon::paint {

uint32_t blend(uint32_t p, uint32_t rgb, uint32_t a) {
    if (a >= 255) return 0xFF000000u | (rgb & 0xFFFFFFu);
    if (a == 0) return p;
    uint32_t pr = (p >> 16) & 0xFF, pg = (p >> 8) & 0xFF, pb = p & 0xFF;
    uint32_t cr = (rgb >> 16) & 0xFF, cg = (rgb >> 8) & 0xFF, cb = rgb & 0xFF;
    uint32_t nr = (cr * a + pr * (255 - a)) / 255;
    uint32_t ng = (cg * a + pg * (255 - a)) / 255;
    uint32_t nb = (cb * a + pb * (255 - a)) / 255;
    return 0xFF000000u | (nr << 16) | (ng << 8) | nb;
}

uint32_t mix(uint32_t a, uint32_t b, float t) {
    t = std::clamp(t, 0.0f, 1.0f);
    auto ch = [&](int shift) {
        float va = (float)((a >> shift) & 0xFF), vb = (float)((b >> shift) & 0xFF);
        return (uint32_t)std::lround(va + (vb - va) * t) & 0xFF;
    };
    return (ch(16) << 16) | (ch(8) << 8) | ch(0);
}

uint32_t darken(uint32_t rgb, float f) { return mix(0x000000u, rgb, f); }

void fill_vertical_gradient(uint32_t* px, uint32_t stride_u32, uint32_t sw, uint32_t sh,
                            uint32_t top, uint32_t bottom) {
    if (!px || sw == 0 || sh == 0) return;
    for (uint32_t y = 0; y < sh; y++) {
        float t = sh > 1 ? (float)y / (float)(sh - 1) : 0.0f;
        uint32_t c = 0xFF000000u | mix(top, bottom, t);
        std::fill_n(px + (size_t)y * stride_u32, sw, c);
    }
}

void add_radial_glow(uint32_t* px, uint32_t stride_u32, uint32_t sw, uint32_t sh,
                     int cx, int cy, int radius, uint32_t rgb, uint8_t strength) {
    if (!px || radius <= 0 || strength == 0) return;
    int x0 = std::max(0, cx - radius), x1 = std::min((int)sw - 1, cx + radius);
    int y0 = std::max(0, cy - radius), y1 = std::min((int)sh - 1, cy + radius);
    float r2 = (float)radius * (float)radius;
    for (int y = y0; y <= y1; y++) {
        uint32_t* row = px + (size_t)y * stride_u32;
        for (int x = x0; x <= x1; x++) {
            float dx = (float)(x - cx), dy = (float)(y - cy);
            float d2 = dx * dx + dy * dy;
            if (d2 >= r2) continue;
            float f = 1.0f - d2 / r2;           // 1 at centre → 0 at the rim
            uint32_t a = (uint32_t)std::lround(strength * f * f);
            row[x] = blend(row[x], rgb, a);
        }
    }
}

// Signed distance from (fx, fy) to a rounded rectangle of half-size (hw, hh)
// centred at the origin with corner radius r. Negative inside.
static float rounded_rect_sdf(float fx, float fy, float hw, float hh, float r) {
    float qx = std::fabs(fx) - (hw - r), qy = std::fabs(fy) - (hh - r);
    float ox = std::max(qx, 0.0f), oy = std::max(qy, 0.0f);
    return std::sqrt(ox * ox + oy * oy) + std::min(std::max(qx, qy), 0.0f) - r;
}

void fill_rounded_rect(uint32_t* px, uint32_t stride_u32, uint32_t sw, uint32_t sh,
                       int x, int y, int w, int h, int r,
                       uint32_t fill_rgb, uint8_t fill_a,
                       uint32_t border_rgb, uint8_t border_a) {
    if (!px || w <= 0 || h <= 0) return;
    r = std::clamp(r, 0, std::min(w, h) / 2);
    float hw = w * 0.5f, hh = h * 0.5f;
    float ccx = x + hw, ccy = y + hh;
    int x0 = std::max(0, x), x1 = std::min((int)sw, x + w);
    int y0 = std::max(0, y), y1 = std::min((int)sh, y + h);
    for (int py = y0; py < y1; py++) {
        uint32_t* row = px + (size_t)py * stride_u32;
        for (int pxx = x0; pxx < x1; pxx++) {
            float d = rounded_rect_sdf(pxx + 0.5f - ccx, py + 0.5f - ccy, hw, hh, (float)r);
            float cover = std::clamp(0.5f - d, 0.0f, 1.0f);       // shape coverage
            if (cover <= 0.0f) continue;
            uint32_t p = row[pxx];
            if (fill_a) p = blend(p, fill_rgb, (uint32_t)std::lround(fill_a * cover));
            if (border_a) {
                float ring = std::clamp(1.0f - std::fabs(d + 0.5f), 0.0f, 1.0f); // 1 px band at the edge
                if (ring > 0.0f) p = blend(p, border_rgb, (uint32_t)std::lround(border_a * ring * cover));
            }
            row[pxx] = p;
        }
    }
}

void fill_circle(uint32_t* px, uint32_t stride_u32, uint32_t sw, uint32_t sh,
                 float cx, float cy, float radius, uint32_t rgb, uint8_t alpha) {
    if (!px || radius <= 0.0f || alpha == 0) return;
    int x0 = std::max(0, (int)std::floor(cx - radius - 1)), x1 = std::min((int)sw - 1, (int)std::ceil(cx + radius + 1));
    int y0 = std::max(0, (int)std::floor(cy - radius - 1)), y1 = std::min((int)sh - 1, (int)std::ceil(cy + radius + 1));
    for (int y = y0; y <= y1; y++) {
        uint32_t* row = px + (size_t)y * stride_u32;
        for (int x = x0; x <= x1; x++) {
            float d = std::sqrt((x + 0.5f - cx) * (x + 0.5f - cx) + (y + 0.5f - cy) * (y + 0.5f - cy)) - radius;
            float cover = std::clamp(0.5f - d, 0.0f, 1.0f);
            if (cover <= 0.0f) continue;
            row[x] = blend(row[x], rgb, (uint32_t)std::lround(alpha * cover));
        }
    }
}

} // namespace ndimon::paint
