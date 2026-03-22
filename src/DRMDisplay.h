#pragma once
#include <cstdint>
#include <string>
#include <vector>
#include <atomic>
#include <mutex>
#include <xf86drm.h>
#include <xf86drmMode.h>

enum class ScaleMode { Stretch, Letterbox, Crop };

struct DRMBuffer {
    uint32_t bo_handle = 0;
    uint32_t fb_id     = 0;
    int      dma_fd    = -1;
    uint32_t width     = 0;
    uint32_t height    = 0;
    uint32_t stride    = 0;
    uint32_t size      = 0;
    void*    map       = nullptr;
};

struct DRMMode {
    uint32_t    width;
    uint32_t    height;
    uint32_t    refresh;      // integer vrefresh from DRM (e.g. 59 for 59.94)
    double      refresh_hz;   // actual refresh computed from pixel clock (e.g. 59.94)
    std::string name;
    bool        preferred;
};

struct ConnectorInfo {
    uint32_t    id;
    std::string name;       // e.g. "HDMI-A-1"
    bool        connected;
    std::vector<DRMMode> modes;
};

class DRMDisplay {
public:
    DRMDisplay();
    ~DRMDisplay();

    // Open device, find connector by name (empty = first connected/any)
    bool init(const std::string& device = "/dev/dri/card0",
              const std::string& connector_name = "",
              const std::string& preferred_mode = "");

    // Init with a pre-opened fd (e.g. a DRM lease). Skips open() and drmSetMaster().
    bool init(int fd, const std::string& connector_name, const std::string& preferred_mode);

    void destroy();

    // Create a DRM lease for a named connector. Returns a lease fd (or -1 on failure).
    // The caller must keep master_fd open until the lease fd is closed.
    static int create_lease(int master_fd, const std::string& connector_name);

    bool is_initialized() const { return initialized_; }
    int    width()          const { return width_; }
    int    height()         const { return height_; }
    int    refresh()        const { return vrefresh_; }
    double refresh_hz()     const { return vrefresh_hz_; }
    std::string conn_name() const { return connector_name_; }

    void      set_scale_mode(ScaleMode m) { scale_mode_ = m; }
    ScaleMode scale_mode()          const { return scale_mode_; }

    // Called by the video pipeline to block splash from interrupting live video
    void set_streaming(bool active) { streaming_ = active; }
    bool is_streaming()       const { return streaming_; }

    // OSD text overlaid on live video frames (empty = disabled)
    void set_osd_text(const std::string& text) { osd_text_ = text; }
    void set_osd_enabled(bool en)              { osd_enabled_ = en; }

    // Display content
    bool show_frame_memory(const uint8_t* data, size_t size,
                           uint32_t frame_w, uint32_t frame_h,
                           uint32_t stride, uint32_t drm_format);
    bool show_frame_dma(int dma_fd, uint32_t format,
                        uint32_t frame_w, uint32_t frame_h,
                        uint32_t stride_y, uint32_t stride_uv = 0);
    bool show_splash(bool source_available = false);
    bool show_black();

    // Mode management
    std::vector<DRMMode> list_modes() const { return available_modes_; }
    // refresh_hz = 0 → pick preferred/first mode for this resolution.
    // Pass exact fractional value (e.g. 59.94) to select NTSC rates precisely.
    bool set_mode(uint32_t w, uint32_t h, double refresh_hz = 0.0);

    // Hotplug: returns true if just reconnected (init was deferred)
    bool check_hotplug();

    void set_hdmi_enabled(bool enable);

    // Enumerate all connectors on a device (static utility)
    static std::vector<ConnectorInfo> enumerate_connectors(
        const std::string& device = "/dev/dri/card0");

private:
    struct Rect { uint32_t x = 0, y = 0, w = 0, h = 0; };

    bool setup_crtc(const std::string& connector_name,
                    const std::string& preferred_mode);
    bool alloc_fb(DRMBuffer& buf, uint32_t w, uint32_t h);
    void free_fb(DRMBuffer& buf);
    bool show_black_impl();  // show_black without locking frame_mutex_

    // Commit a framebuffer: SetCrtc on first call, PageFlip thereafter
    bool commit_fb(uint32_t fb_id);
    void wait_for_flip();
    static void flip_handler(int fd, unsigned seq,
                             unsigned tv_sec, unsigned tv_usec, void* user);

    Rect compute_dst_rect(uint32_t src_w, uint32_t src_h) const;

    void fill_bg(DRMBuffer& buf);   // fill with black respecting scale mode

    // Software conversion + scaled blit (always available)
    void sw_nv12_to_xrgb(const uint8_t* src,
                         uint32_t src_w, uint32_t src_h, uint32_t src_stride,
                         uint8_t* dst, uint32_t dst_stride,
                         const Rect& dr, uint32_t out_w, uint32_t out_h);
    void sw_uyvy_to_xrgb(const uint8_t* src,
                         uint32_t src_w, uint32_t src_h, uint32_t src_stride,
                         uint8_t* dst, uint32_t dst_stride,
                         const Rect& dr, uint32_t out_w, uint32_t out_h);

    // RGA hardware path (optional, HAVE_RGA)
    bool rga_blit(const void* src_va, int src_fd,
                  uint32_t src_w, uint32_t src_h, uint32_t src_stride,
                  uint32_t src_rga_fmt,
                  void* dst_va, uint32_t dst_stride,
                  const Rect& dr, uint32_t dst_w, uint32_t dst_h);

    mutable std::mutex frame_mutex_;  // serialises all FB alloc/write/flip ops

    int         drm_fd_   = -1;
    bool        owns_fd_  = true;
    std::string device_path_;
    std::string connector_name_;
    std::string preferred_mode_;
    bool        initialized_  = false;
    bool        crtc_active_  = false;
    std::atomic<bool> flip_pending_{false};

    uint32_t connector_id_ = 0;
    uint32_t crtc_id_      = 0;
    drmModeModeInfo mode_  = {};
    uint32_t width_     = 0;
    uint32_t height_    = 0;
    int      vrefresh_  = 0;
    double   vrefresh_hz_ = 0.0;

    ScaleMode scale_mode_ = ScaleMode::Letterbox;
    std::vector<DRMMode> available_modes_;

    static constexpr int kNumBuffers = 2;
    DRMBuffer fb_[kNumBuffers];
    int cur_buf_ = 0;

    // Per-buffer fill cache — prevents overwriting bars every frame,
    // but must be tracked independently for each buffer in the double-buffer pair.
    uint32_t  last_bg_fill_w_[kNumBuffers]   = {};
    uint32_t  last_bg_fill_h_[kNumBuffers]   = {};
    ScaleMode last_bg_scale_[kNumBuffers]    = { ScaleMode::Stretch, ScaleMode::Stretch };
    void invalidate_fill_cache();   // invalidates all buffers

    bool        streaming_   = false;    // true while video frames are being rendered
    bool        osd_enabled_ = false;
    std::string osd_text_;               // current OSD string, drawn after video blit

    // DMA-BUF import state (reused across frames)
    uint32_t import_fb_id_ = 0;
    uint32_t import_bo_    = 0;

    bool rga_available_ = false;
};
