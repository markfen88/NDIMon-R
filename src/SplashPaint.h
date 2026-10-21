// SplashPaint — small software-rendering helpers for the splash screen:
// gradients, anti-aliased rounded rectangles and circles, radial glows.
// All functions write XRGB8888 pixels (`stride_u32` words per row) and clip
// to the `sw` x `sh` visible area. Colours are 0xRRGGBB.
#pragma once

#include <cstdint>

namespace ndimon::paint {

// Blend colour `rgb` over pixel `p` with coverage `a` (0..255).
uint32_t blend(uint32_t p, uint32_t rgb, uint32_t a);

// Linear mix of two colours: t=0 → a, t=1 → b.
uint32_t mix(uint32_t a, uint32_t b, float t);
// Scale a colour toward black: f=1 keeps it, f=0 is black.
uint32_t darken(uint32_t rgb, float f);

// Fill the whole surface with a top→bottom gradient.
void fill_vertical_gradient(uint32_t* px, uint32_t stride_u32, uint32_t sw, uint32_t sh,
                            uint32_t top, uint32_t bottom);

// Add a soft radial glow centred at (cx, cy): `strength` (0..255) at the
// centre, fading quadratically to nothing at `radius`.
void add_radial_glow(uint32_t* px, uint32_t stride_u32, uint32_t sw, uint32_t sh,
                     int cx, int cy, int radius, uint32_t rgb, uint8_t strength);

// Rounded rectangle at (x, y) of w x h with corner radius r: a translucent
// fill (`fill_a`) and a 1 px border (`border_a`; 0 for none). Edges are
// anti-aliased.
void fill_rounded_rect(uint32_t* px, uint32_t stride_u32, uint32_t sw, uint32_t sh,
                       int x, int y, int w, int h, int r,
                       uint32_t fill_rgb, uint8_t fill_a,
                       uint32_t border_rgb, uint8_t border_a);

// Anti-aliased filled circle.
void fill_circle(uint32_t* px, uint32_t stride_u32, uint32_t sw, uint32_t sh,
                 float cx, float cy, float radius, uint32_t rgb, uint8_t alpha = 255);

} // namespace ndimon::paint
