#pragma once
#include <cstdint>

struct YuvCoefficients {
    int y, rv, gu, gv, bu;
    bool limited;
    const char* name;
};
static constexpr YuvCoefficients kYuvLegacy{256,359,88,183,453,false,"legacy BT.601 full"};
static constexpr YuvCoefficients kYuv601{298,409,100,208,516,true,"BT.601"};
static constexpr YuvCoefficients kYuv709{298,459,55,136,541,true,"BT.709"};
static constexpr YuvCoefficients kYuv2020{298,430,48,167,548,true,"BT.2020"};

// NDI's documented source-size rules, before crop, rotation or output scaling.
static inline const YuvCoefficients& ndi_yuv_coefficients(uint32_t width,uint32_t height) {
    if (width>1920 || height>1080) return kYuv2020;
    if (width>720 || height>576) return kYuv709;
    return kYuv601;
}
static inline uint32_t yuv_to_xrgb(int y,int u,int v,const YuvCoefficients& c=kYuvLegacy) {
    u-=128;v-=128;
    int r,g,b;
    if(c.limited) {
        int l=c.y*(y-16);
        r=(l+c.rv*v+128)>>8;g=(l-c.gu*u-c.gv*v+128)>>8;b=(l+c.bu*u+128)>>8;
    } else {
        r=y+((c.rv*v)>>8);g=y-((c.gu*u+c.gv*v)>>8);b=y+((c.bu*u)>>8);
    }
    auto clamp=[](int n){return n<0?0:n>255?255:n;};
    return 0xff000000u|(uint32_t(clamp(r))<<16)|(uint32_t(clamp(g))<<8)|uint32_t(clamp(b));
}
#if defined(__aarch64__) || defined(__ARM_NEON)
#include <arm_neon.h>
static inline void neon_yuv_to_xrgb_8px(const uint8_t* yp,const uint8_t* up,const uint8_t* vp,
                                      uint32_t* dst,const YuvCoefficients& c=kYuvLegacy) {
    const auto y=vreinterpretq_s16_u16(vmovl_u8(vld1_u8(yp)));
    const auto u=vsubq_s16(vreinterpretq_s16_u16(vmovl_u8(vld1_u8(up))),vdupq_n_s16(128));
    const auto v=vsubq_s16(vreinterpretq_s16_u16(vmovl_u8(vld1_u8(vp))),vdupq_n_s16(128));
    int16x8_t r,g,b;
    auto ulo=vget_low_s16(u),uhi=vget_high_s16(u),vlo=vget_low_s16(v),vhi=vget_high_s16(v);
    if(c.limited) {
        const auto yl=vmull_n_s16(vsub_s16(vget_low_s16(y),vdup_n_s16(16)),c.y);
        const auto yh=vmull_n_s16(vsub_s16(vget_high_s16(y),vdup_n_s16(16)),c.y);
        const auto round=vdupq_n_s32(128);
        r=vcombine_s16(vmovn_s32(vshrq_n_s32(vaddq_s32(vmlal_n_s16(yl,vlo,c.rv),round),8)),
                       vmovn_s32(vshrq_n_s32(vaddq_s32(vmlal_n_s16(yh,vhi,c.rv),round),8)));
        g=vcombine_s16(vmovn_s32(vshrq_n_s32(vaddq_s32(vmlsl_n_s16(vmlsl_n_s16(yl,ulo,c.gu),vlo,c.gv),round),8)),
                       vmovn_s32(vshrq_n_s32(vaddq_s32(vmlsl_n_s16(vmlsl_n_s16(yh,uhi,c.gu),vhi,c.gv),round),8)));
        b=vcombine_s16(vmovn_s32(vshrq_n_s32(vaddq_s32(vmlal_n_s16(yl,ulo,c.bu),round),8)),
                       vmovn_s32(vshrq_n_s32(vaddq_s32(vmlal_n_s16(yh,uhi,c.bu),round),8)));
    } else {
        r=vaddq_s16(y,vcombine_s16(vmovn_s32(vshrq_n_s32(vmull_n_s16(vlo,c.rv),8)),vmovn_s32(vshrq_n_s32(vmull_n_s16(vhi,c.rv),8))));
        g=vsubq_s16(y,vcombine_s16(vmovn_s32(vshrq_n_s32(vmlal_n_s16(vmull_n_s16(ulo,c.gu),vlo,c.gv),8)),vmovn_s32(vshrq_n_s32(vmlal_n_s16(vmull_n_s16(uhi,c.gu),vhi,c.gv),8))));
        b=vaddq_s16(y,vcombine_s16(vmovn_s32(vshrq_n_s32(vmull_n_s16(ulo,c.bu),8)),vmovn_s32(vshrq_n_s32(vmull_n_s16(uhi,c.bu),8))));
    }
    uint8x8x4_t rgba;rgba.val[0]=vqmovun_s16(b);rgba.val[1]=vqmovun_s16(g);
    rgba.val[2]=vqmovun_s16(r);rgba.val[3]=vdup_n_u8(255);vst4_u8(reinterpret_cast<uint8_t*>(dst),rgba);
}
#endif
