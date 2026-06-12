'use strict';
const express = require('express');
const router  = express.Router();
const { readJson, writeJson, sendIPC, corsHeaders } = require('./lib');
const { execFile } = require('child_process');

const DEVICE_SETTINGS = '/etc/ndimon-device-settings.json';
const TUNING_SETTINGS = '/etc/ndimon-tuning.json';

router.use((req, res, next) => { corsHeaders(res); next(); });

// Latency tuning. All keys optional; defaults preserve current behaviour.
const TUNING_DEFAULTS = {
    display_queue_depth: 2,
    decode_low_latency: false,
    vaapi_low_delay: false,
    framesync_bypass: false,
    realtime_threads: false,
    cpu_performance_governor: false,
    audio_periods: 4,
    audio_period_frames: 1024,
};

function clampInt(v, lo, hi, dflt) {
    const n = parseInt(v, 10);
    if (Number.isNaN(n)) return dflt;
    return Math.max(lo, Math.min(hi, n));
}

// GET /tuning
router.get('/tuning', (req, res) => {
    res.json(Object.assign({}, TUNING_DEFAULTS, readJson(TUNING_SETTINGS)));
});

// POST /tuning — merge provided keys, validate, persist, reload.
router.post('/tuning', (req, res) => {
    const cur  = Object.assign({}, TUNING_DEFAULTS, readJson(TUNING_SETTINGS));
    const body = req.body || {};
    if ('display_queue_depth' in body) cur.display_queue_depth = clampInt(body.display_queue_depth, 1, 4, cur.display_queue_depth);
    if ('audio_periods' in body)       cur.audio_periods       = clampInt(body.audio_periods, 2, 8, cur.audio_periods);
    if ('audio_period_frames' in body) cur.audio_period_frames = clampInt(body.audio_period_frames, 128, 8192, cur.audio_period_frames);
    for (const k of ['decode_low_latency','vaapi_low_delay','framesync_bypass','realtime_threads','cpu_performance_governor'])
        if (k in body) cur[k] = Boolean(body[k]);
    writeJson(TUNING_SETTINGS, cur);
    sendIPC({ action: 'reload_config' });
    res.json({ ok: true, tuning: cur });
});

// GET|POST /operationmode

// GET|POST /operationmode
router.get('/operationmode', (req, res) => {
    res.header('Content-Type','text/plain');
    const cfg = readJson(DEVICE_SETTINGS);
    const { mode } = req.query;
    if (mode && ['encode','decode'].includes(mode) && mode !== cfg.mode) {
        cfg.mode = mode;
        writeJson(DEVICE_SETTINGS, cfg);
        sendIPC({ action: 'reload_config' });
    }
    res.send(cfg.mode || 'decode');
});

router.post('/operationmode', (req, res) => {
    res.header('Content-Type','text/plain');
    const mode = req.body;
    const cfg = readJson(DEVICE_SETTINGS);
    if (mode && ['encode','decode'].includes(mode) && mode !== cfg.mode) {
        cfg.mode = mode;
        writeJson(DEVICE_SETTINGS, cfg);
        sendIPC({ action: 'reload_config' });
    }
    res.send(cfg.mode || 'decode');
});

// GET /hostname
router.get('/hostname', (req, res) => {
    const os = require('os');
    res.json({ hostname: os.hostname() });
});

// GET /ndi-alias  — returns the current NDI receiver name
router.get('/ndi-alias', (req, res) => {
    const cfg = readJson(DEVICE_SETTINGS);
    res.json({ ndi_recv_name: cfg.ndi_recv_name || '' });
});

// POST /ndi-alias  — body: { ndi_recv_name: "NDIMON-A1B2" }
router.post('/ndi-alias', (req, res) => {
    const name = ((req.body && req.body.ndi_recv_name) || '').trim();
    const cfg = readJson(DEVICE_SETTINGS);
    cfg.ndi_recv_name = name;
    writeJson(DEVICE_SETTINGS, cfg);
    // Update OS hostname so NDI discovery shows the right device name.
    // execFile (no shell) prevents command injection via the alias, and the
    // hostname itself is restricted to RFC-952-ish safe characters.
    if (name) {
        const hostname = name.replace(/[^A-Za-z0-9-]+/g, '-')
                             .replace(/^-+|-+$/g, '')
                             .slice(0, 63) || 'ndimon';
        execFile('hostnamectl', ['set-hostname', hostname], err => {
            if (err) console.warn('[DeviceSettings] hostnamectl:', err.message);
        });
    }
    sendIPC({ action: 'reload_config' });
    res.json({ ndi_recv_name: cfg.ndi_recv_name });
});

// GET /output-alias?ch=1  — returns output alias for given channel
router.get('/output-alias', (req, res) => {
    const ch = parseInt(req.query.ch || 1, 10);
    const path = `/etc/ndimon-dec${ch}-settings.json`;
    const cfg = readJson(path);
    res.json({ output_alias: cfg.output_alias || '', ch });
});

// POST /output-alias  — body: { ch: 1, output_alias: "Main Screen" }
router.post('/output-alias', (req, res) => {
    const { ch = 1, output_alias = '' } = req.body || {};
    const chNum = parseInt(ch, 10);
    const path = `/etc/ndimon-dec${chNum}-settings.json`;
    const cfg = readJson(path) || {};
    cfg.output_alias = (output_alias || '').trim();
    writeJson(path, cfg);
    sendIPC({ action: 'reload_config' });
    res.json({ output_alias: cfg.output_alias, ch: chNum });
});

// GET /decode-mode  — HX decode path: auto | hardware | software
router.get('/decode-mode', (req, res) => {
    const cfg = readJson(DEVICE_SETTINGS);
    res.json({ decode_mode: cfg.decode_mode || 'auto' });
});

// POST /decode-mode  — body: { decode_mode: "auto"|"hardware"|"software" }
router.post('/decode-mode', (req, res) => {
    const mode = ((req.body && req.body.decode_mode) || '').trim();
    const allowed = ['auto', 'hardware', 'software'];
    if (!allowed.includes(mode))
        return res.status(400).json({ ok: false, error: 'must be auto, hardware, or software' });
    const cfg = readJson(DEVICE_SETTINGS);
    cfg.decode_mode = mode;
    writeJson(DEVICE_SETTINGS, cfg);
    sendIPC({ action: 'reload_config' });
    res.json({ ok: true, decode_mode: mode });
});

// GET /watchdog-mode
router.get('/watchdog-mode', (req, res) => {
    const cfg = readJson(DEVICE_SETTINGS);
    res.json({ watchdog_mode: cfg.watchdog_mode || 'passive' });
});

// POST /watchdog-mode  — body: { watchdog_mode: "disabled"|"passive"|"active" }
router.post('/watchdog-mode', (req, res) => {
    const mode = ((req.body && req.body.watchdog_mode) || '').trim();
    const allowed = ['disabled', 'passive', 'active'];
    if (!allowed.includes(mode))
        return res.status(400).json({ ok: false, error: 'must be disabled, passive, or active' });
    const cfg = readJson(DEVICE_SETTINGS);
    cfg.watchdog_mode = mode;
    writeJson(DEVICE_SETTINGS, cfg);
    sendIPC({ action: 'reload_config' });
    res.json({ ok: true, watchdog_mode: mode });
});

module.exports = router;
