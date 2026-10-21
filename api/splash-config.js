'use strict';
// Splash settings: defaults, the exported key list, and the two pure
// transforms the Splash route and Backup share. No file access here.

const BACKDROPS = ['gradient', 'flat'];

const DEFAULTS = Object.freeze({
    bg_idle:     '#0D1B2A',
    bg_live:     '#0D2B1A',
    accent_idle: '#4488CC',
    accent_live: '#22FF88',
    logo_path:   '',
    logo_x_pct:  50,
    logo_y_pct:  50,
    logo_w_pct:  50,
    text_idle:   'No Signal',
    text_live:   'Signal Available',
    text_height_pct: 4,
    backdrop:    'gradient',
    show_signal_text: true,
    show_device_name: true,
    show_device_url:  true,
    show_sources_available: true,
});

const SPLASH_KEYS = Object.keys(DEFAULTS);

const STR_FIELDS   = ['bg_idle', 'bg_live', 'accent_idle', 'accent_live', 'text_idle', 'text_live'];
const FLOAT_FIELDS = ['logo_x_pct', 'logo_y_pct', 'logo_w_pct', 'text_height_pct'];
const BOOL_FIELDS  = ['show_signal_text', 'show_device_name', 'show_device_url', 'show_sources_available'];

// The decoder renders logo_path with stb_image as root, so it is restricted to
// the managed upload location (or empty).
const LOGO_PATH_RE = /^\/etc\/ndi-splash-logo\.(png|jpe?g)$/;

const backdropOrDefault = (v) => (BACKDROPS.includes(v) ? v : DEFAULTS.backdrop);

// Stored file → complete settings object. Keys from before the redesign
// (show_box) are dropped; unknown backdrop values fall back to the default.
function normaliseSplash(stored) {
    const src = (stored && typeof stored === 'object') ? stored : {};
    const out = {};
    for (const k of SPLASH_KEYS) out[k] = (k in src) ? src[k] : DEFAULTS[k];
    out.backdrop = backdropOrDefault(out.backdrop);
    return out;
}

// Current settings + request body → new settings. Only known keys are
// written; values are coerced and clamped.
function applySplashUpdate(cur, body) {
    const next = normaliseSplash(cur);
    const b = (body && typeof body === 'object') ? body : {};
    for (const f of STR_FIELDS)   if (f in b) next[f] = String(b[f]);
    for (const f of FLOAT_FIELDS) if (f in b) next[f] = Math.max(0, Math.min(100, parseFloat(b[f]) || 0));
    for (const f of BOOL_FIELDS)  if (f in b) next[f] = Boolean(b[f]);
    if ('backdrop' in b && BACKDROPS.includes(b.backdrop)) next.backdrop = b.backdrop;
    if ('logo_path' in b) {
        const p = String(b.logo_path || '');
        if (p === '' || LOGO_PATH_RE.test(p)) next.logo_path = p;
    }
    return next;
}

module.exports = { DEFAULTS, SPLASH_KEYS, BACKDROPS, normaliseSplash, applySplashUpdate };
