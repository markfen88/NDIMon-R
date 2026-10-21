// TextRender — anti-aliased TrueType text for the splash and OSD.
//
// Rasterises a string into an 8-bit coverage bitmap with stb_truetype, then
// blit_text() blends it onto an XRGB8888 framebuffer. If the font files are
// missing the renderer falls back to the embedded 8x8 bitmap font so the
// appliance always draws something.
#pragma once

#include <cstdint>
#include <memory>
#include <string>
#include <vector>

namespace ndimon {

struct TextBitmap {
    int w = 0, h = 0;
    int baseline = 0;              // rows from the top to the text baseline
    std::vector<uint8_t> alpha;    // w*h coverage, 0..255, row-major
};

class TextRenderer {
public:
    TextRenderer();
    ~TextRenderer();
    TextRenderer(const TextRenderer&) = delete;
    TextRenderer& operator=(const TextRenderer&) = delete;

    // Loads the regular face and, if given, the semibold face. Returns true
    // when the regular face loaded; semibold falls back to regular.
    bool load(const std::string& regular_path, const std::string& semibold_path);
    bool loaded() const;

    // Rasterise `text` (ASCII / Latin-1) so the line height is about px_height.
    // Without a loaded font this returns render_fallback().
    TextBitmap render(const std::string& text, int px_height, bool semibold = false) const;
    int measure(const std::string& text, int px_height, bool semibold = false) const;

    // Embedded 8x8 bitmap font scaled by an integer factor (px_height / 8).
    static TextBitmap render_fallback(const std::string& text, int px_height);

private:
    struct Face;
    std::unique_ptr<Face> regular_;
    std::unique_ptr<Face> semibold_;
};

// Blend `bm` at (x, y) onto `pixels` (XRGB8888, `stride_u32` words per row,
// `sw` x `sh` visible) in colour 0xRRGGBB. `alpha` scales the coverage.
void blit_text(uint32_t* pixels, uint32_t stride_u32, uint32_t sw, uint32_t sh,
               const TextBitmap& bm, int x, int y, uint32_t rgb, uint8_t alpha = 255);

} // namespace ndimon
