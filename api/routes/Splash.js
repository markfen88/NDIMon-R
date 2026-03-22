'use strict';
const express = require('express');
const router  = express.Router();
const fs      = require('fs');
const path    = require('path');
const { sendIPC } = require('./lib');

const LOGO_DIR  = '/etc';
const LOGO_BASE = 'ndi-splash-logo';

const SPLASH_FILE = '/etc/ndimon-splash-settings.json';

const DEFAULTS = {
    bg_idle:     '#0D1B2A',
    bg_live:     '#0D2B1A',
    accent_idle: '#4488CC',
    accent_live: '#22FF88',
    logo_path:   '',
    logo_x_pct:  50,
    logo_y_pct:  40,
    logo_w_pct:  30,
    text_idle:   'No Signal',
    text_live:   'Signal Available',
    text_x_pct:  50,
    text_y_pct:  62,
    text_scale:  3,
    show_box:    true,
};

function readSplash() {
    try {
        if (fs.existsSync(SPLASH_FILE)) {
            return Object.assign({}, DEFAULTS, JSON.parse(fs.readFileSync(SPLASH_FILE, 'utf8')));
        }
    } catch {}
    return Object.assign({}, DEFAULTS);
}

function writeSplash(cfg) {
    fs.writeFileSync(SPLASH_FILE, JSON.stringify(cfg, null, 2));
}

// GET /v1/Splash/config
router.get('/config', (req, res) => {
    res.json(readSplash());
});

// POST /v1/Splash/config  { ...fields }
router.post('/config', (req, res) => {
    const cur  = readSplash();
    const body = req.body || {};

    const strFields   = ['bg_idle','bg_live','accent_idle','accent_live','logo_path','text_idle','text_live'];
    const floatFields = ['logo_x_pct','logo_y_pct','logo_w_pct','text_x_pct','text_y_pct'];
    const intFields   = ['text_scale'];
    const boolFields  = ['show_box'];

    for (const f of strFields)   if (f in body) cur[f] = String(body[f]);
    for (const f of floatFields) if (f in body) cur[f] = Math.max(0, Math.min(100, parseFloat(body[f]) || 0));
    for (const f of intFields)   if (f in body) cur[f] = Math.max(1, Math.min(8, parseInt(body[f],10) || 1));
    for (const f of boolFields)  if (f in body) cur[f] = Boolean(body[f]);

    writeSplash(cur);

    // Tell decoder to reload config (so show_splash() picks up new values)
    sendIPC({ action: 'reload_config' });

    res.json({ ok: true, config: cur });
});

// POST /v1/Splash/preview  { source_available: bool }
router.post('/preview', (req, res) => {
    const src = !!(req.body && req.body.source_available);
    sendIPC({ action: 'show_splash', source_available: src });
    res.json({ ok: true });
});

// POST /v1/Splash/uploadLogo
// Body (JSON): { data: "<base64>", filename: "logo.png" }
// The browser reads the file with FileReader.readAsDataURL and strips the
// "data:image/...;base64," prefix before sending.
router.post('/uploadLogo', (req, res) => {
    const { data, filename } = req.body || {};
    if (!data) return res.status(400).json({ ok: false, error: 'no data' });

    const ext  = (path.extname(filename || '').toLowerCase() || '.png')
                    .replace(/[^.a-z0-9]/g, '');
    const dest = path.join(LOGO_DIR, LOGO_BASE + ext);

    try {
        const buf = Buffer.from(data, 'base64');
        if (buf.length < 4) throw new Error('file too small');
        fs.writeFileSync(dest, buf);

        // Auto-save logo_path in splash config
        const cur = readSplash();
        cur.logo_path = dest;
        writeSplash(cur);
        sendIPC({ action: 'reload_config' });

        res.json({ ok: true, path: dest, size: buf.length });
    } catch (e) {
        res.status(500).json({ ok: false, error: e.message });
    }
});

// GET  /v1/Splash/osd
// POST /v1/Splash/osd   { enabled: bool, text: "string" }
const OSD_FILE = '/etc/ndimon-osd-settings.json';
const OSD_DEFAULTS = { enabled: false, text: '' };

function readOsd() {
    try {
        if (fs.existsSync(OSD_FILE))
            return Object.assign({}, OSD_DEFAULTS, JSON.parse(fs.readFileSync(OSD_FILE, 'utf8')));
    } catch {}
    return Object.assign({}, OSD_DEFAULTS);
}

router.get('/osd', (req, res) => res.json(readOsd()));

router.post('/osd', (req, res) => {
    const body = req.body || {};
    const cfg  = readOsd();
    if ('enabled' in body) cfg.enabled = !!body.enabled;
    if ('text'    in body) cfg.text    = String(body.text).slice(0, 128);
    fs.writeFileSync(OSD_FILE, JSON.stringify(cfg, null, 2));
    sendIPC({ action: 'reload_config' });
    res.json({ ok: true, config: cfg });
});

module.exports = router;
