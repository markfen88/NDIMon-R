'use strict';
const express = require('express');
const router  = express.Router();
const fs      = require('fs');
const path    = require('path');
const { sendIPC, writeJson } = require('./lib');

const LOGO_DIR  = '/etc';
const LOGO_BASE = 'ndi-splash-logo';

const SPLASH_FILE = '/etc/ndimon-splash-settings.json';

const { normaliseSplash, applySplashUpdate } = require('../splash-config');

function readSplash() {
    try {
        if (fs.existsSync(SPLASH_FILE)) return normaliseSplash(JSON.parse(fs.readFileSync(SPLASH_FILE, 'utf8')));
    } catch {}
    return normaliseSplash({});
}

function writeSplash(cfg) {
    writeJson(SPLASH_FILE, cfg);
}

// GET /v1/Splash/config
router.get('/config', (req, res) => {
    res.json(readSplash());
});

// POST /v1/Splash/config  { ...fields }
// Validation, clamping and the logo_path restriction live in splash-config.js.
router.post('/config', (req, res) => {
    const cur = applySplashUpdate(readSplash(), req.body || {});
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

    try {
        const buf = Buffer.from(data, 'base64');
        const png = buf.subarray(0, 8).equals(Buffer.from([137,80,78,71,13,10,26,10]));
        const jpg = buf[0] === 0xff && buf[1] === 0xd8 && buf[2] === 0xff;
        if (!png && !jpg) return res.status(400).json({ ok: false, error: 'logo must be PNG or JPEG' });
        const ext = png ? '.png' : '.jpg';
        const dest = path.join(LOGO_DIR, LOGO_BASE + ext);
        require('../persistence').writeConfig(dest, buf);

        // Auto-save logo_path in splash config
        const cur = readSplash();
        cur.logo_path = dest;
        writeSplash(cur);
        for (const other of ['.png', '.jpg', '.jpeg']) {
            if (other !== ext) require('../persistence').removeLogo(path.join(LOGO_DIR, LOGO_BASE + other));
        }
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
    writeJson(OSD_FILE, cfg);
    sendIPC({ action: 'reload_config' });
    res.json({ ok: true, config: cfg });
});

module.exports = router;
