'use strict';
const express = require('express');
const router  = express.Router();
const { readJson, writeJson, sendIPC, corsHeaders } = require('./lib');
const { exec } = require('child_process');

const DEVICE_SETTINGS = '/etc/ndimon-device-settings.json';

router.use((req, res, next) => { corsHeaders(res); next(); });

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
    // Update OS hostname so NDI discovery shows the right device name
    if (name) {
        exec(`hostnamectl set-hostname "${name.replace(/"/g, '')}"`, err => {
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
