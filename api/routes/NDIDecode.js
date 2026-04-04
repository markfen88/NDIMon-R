'use strict';
const express = require('express');
const router  = express.Router();
const { readJson, writeJson, sendIPC, corsHeaders } = require('./lib');
const { stopReconnectLoop: cancelReconnect, notifyManualConnect } = require('./Status');

const DEC1_SETTINGS = '/etc/ndimon-dec1-settings.json';
const DEC1_STATUS   = '/etc/ndimon-dec1-status.json';
const RX_SETTINGS   = '/etc/ndimon-rx-settings.json';
const NDI_SRC       = '/etc/ndimon-sources.json';

router.use((req, res, next) => { corsHeaders(res); next(); });

// GET /decodestatus
router.get('/decodestatus', (req, res) => {
    res.json(readJson(DEC1_STATUS));
});

// GET|POST /decodesetup
router.get('/decodesetup', (req, res) => {
    const cfg = readJson(DEC1_SETTINGS);
    const { NDIAudio, ScreenSaverMode, TallyMode, ColorSpace, ChNum } = req.query;
    let write = false;

    if (NDIAudio && ['NDIAudioEn','NDIAudioDis'].includes(NDIAudio) && NDIAudio !== cfg.NDIAudio) {
        cfg.NDIAudio = NDIAudio; write = true;
    }
    if (ScreenSaverMode && ['SplashSS','BlackSS','CaptureSS'].includes(ScreenSaverMode) && ScreenSaverMode !== cfg.ScreenSaverMode) {
        cfg.ScreenSaverMode = ScreenSaverMode; write = true;
    }
    if (TallyMode && ['TallyOn','TallyOff','VideoMode'].includes(TallyMode) && TallyMode !== cfg.TallyMode) {
        cfg.TallyMode = TallyMode; write = true;
    }
    if (ColorSpace && ['RGB','YUV'].includes(ColorSpace) && ColorSpace !== cfg.ColorSpace) {
        cfg.ColorSpace = ColorSpace; write = true;
    }

    if (write) {
        writeJson(DEC1_SETTINGS, cfg);
        sendIPC({ action: 'reload_config' });
    }
    res.json(cfg);
});

router.post('/decodesetup', (req, res) => {
    const cfg = readJson(DEC1_SETTINGS);
    const body = req.body || {};
    let write = false;

    const fields = {
        NDIAudio: ['NDIAudioEn','NDIAudioDis'],
        ScreenSaverMode: ['SplashSS','BlackSS','CaptureSS'],
        TallyMode: ['TallyOn','TallyOff','VideoMode'],
        ColorSpace: ['RGB','YUV'],
    };

    for (const [key, allowed] of Object.entries(fields)) {
        if (body[key] && allowed.includes(body[key]) && body[key] !== cfg[key]) {
            cfg[key] = body[key]; write = true;
        }
    }
    if (body.ChNum != null) { cfg.ChNum = body.ChNum; write = true; }

    if (write) {
        writeJson(DEC1_SETTINGS, cfg);
        sendIPC({ action: 'reload_config' });
    }
    res.json(cfg);
});

// GET|POST /decodeTransport
router.get('/decodeTransport', (req, res) => {
    const cfg = readJson(RX_SETTINGS);
    const { Rxpm } = req.query;
    if (Rxpm && ['Multicast','TCP','M-TCP','RUDP','UDP'].includes(Rxpm) && Rxpm !== cfg.Rxpm) {
        cfg.Rxpm = Rxpm;
        writeJson(RX_SETTINGS, cfg);
        sendIPC({ action: 'reload_config' });
    }
    res.json(cfg);
});

router.post('/decodeTransport', (req, res) => {
    const cfg = readJson(RX_SETTINGS);
    const { Rxpm } = req.body || {};
    if (Rxpm && ['Multicast','TCP','M-TCP','RUDP','UDP'].includes(Rxpm) && Rxpm !== cfg.Rxpm) {
        cfg.Rxpm = Rxpm;
        writeJson(RX_SETTINGS, cfg);
        sendIPC({ action: 'reload_config' });
    }
    res.json(cfg);
});

// GET|POST /connectTo - select NDI source to decode
// Output param (1-based ch_num) selects which display output to connect; defaults to 1.
router.get('/connectTo', (req, res) => {
    const { SourceName, SourceIP, Output = 1 } = req.query;
    const outputIdx = (parseInt(Output, 10) || 1) - 1;
    cancelReconnect(outputIdx);  // explicit user action — cancel pending auto-reconnect
    if (SourceName && SourceName !== 'None') notifyManualConnect(outputIdx);
    const cfg = readJson(DEC1_SETTINGS);
    if (SourceName && SourceName !== 'None') {
        cfg.SourceName = SourceName;
        cfg.SourceIP   = SourceIP || '';
        cfg.SourceSelection = 'NDI';
        writeJson(DEC1_SETTINGS, cfg);
        sendIPC({ action: 'connect', source_name: SourceName, source_ip: SourceIP || '',
                  output: outputIdx });
    } else {
        cfg.SourceName = '';
        cfg.SourceIP   = '';
        writeJson(DEC1_SETTINGS, cfg);
        sendIPC({ action: 'disconnect', output: outputIdx });
    }
    res.json({ ok: true, SourceName });
});

router.post('/connectTo', (req, res) => {
    const { SourceName, SourceIP, Output = 1 } = req.body || {};
    const outputIdx = (parseInt(Output, 10) || 1) - 1;
    cancelReconnect(outputIdx);  // explicit user action — cancel pending auto-reconnect
    if (SourceName && SourceName !== 'None') notifyManualConnect(outputIdx);
    const cfg = readJson(DEC1_SETTINGS);
    if (SourceName && SourceName !== 'None') {
        cfg.SourceName = SourceName;
        cfg.SourceIP   = SourceIP || '';
        cfg.SourceSelection = 'NDI';
        writeJson(DEC1_SETTINGS, cfg);
        sendIPC({ action: 'connect', source_name: SourceName, source_ip: SourceIP || '',
                  output: outputIdx });
    } else {
        cfg.SourceName = '';
        cfg.SourceIP   = '';
        writeJson(DEC1_SETTINGS, cfg);
        sendIPC({ action: 'disconnect', output: outputIdx });
    }
    res.json({ ok: true, SourceName });
});

// GET /capture
router.get('/capture', (req, res) => {
    res.header('Content-Type', 'text/plain');
    // Trigger a frame capture (write flag file)
    const fs = require('fs');
    try { fs.writeFileSync('/tmp/ndimon-dec1-cap', ''); } catch {}
    res.send('Capture Success');
});

module.exports = router;
