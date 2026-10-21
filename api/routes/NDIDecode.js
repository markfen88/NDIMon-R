'use strict';
const express = require('express');
const { isNdiEndpoint, resolveNdiEndpoint } = require('../ndi-endpoint');
const router  = express.Router();
const { readJson, writeJson, sendIPC, corsHeaders, parseChannel } = require('./lib');
const { stopReconnectLoop: cancelReconnect, notifyManualConnect } = require('./Status');

const DEC1_SETTINGS = '/etc/ndimon-dec1-settings.json';
const DEC1_STATUS   = '/etc/ndimon-dec1-status.json';
const RX_SETTINGS   = '/etc/ndimon-rx-settings.json';
const NDI_SRC       = '/var/lib/ndimon/ndimon-sources.json';
const { KNOWN_RXPM } = require('../transport');

router.use((req, res, next) => { corsHeaders(res); next(); });

// GET /decodestatus
router.get('/decodestatus', (req, res) => {
    res.json(readJson(DEC1_STATUS));
});

// GET|POST /decodesetup
router.get('/decodesetup', (req, res) => {
    res.json(readJson(DEC1_SETTINGS));
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
    res.json(readJson(RX_SETTINGS));
});

router.post('/decodeTransport', (req, res) => {
    const cfg = readJson(RX_SETTINGS);
    const { Rxpm } = req.body || {};
    if (Rxpm && KNOWN_RXPM.includes(Rxpm)) {
        const changed = Rxpm !== cfg.Rxpm;
        // An explicit choice (even TCP) must survive the default migration.
        if (changed || cfg.Rxpm_user_set !== true) {
            cfg.Rxpm = Rxpm;
            cfg.Rxpm_user_set = true;
            writeJson(RX_SETTINGS, cfg);
        }
        if (changed) sendIPC({ action: 'reload_config' });
    }
    res.json(cfg);
});

// GET|POST /connectTo - select NDI source to decode
// Output param (1-based ch_num) selects which display output to connect; defaults to 1.
router.get('/connectTo', (req, res) => {
    res.status(405).json({ ok: false, error: 'use POST' });
});

router.post('/connectTo', (req, res) => {
    const { SourceName, SourceIP, Output = 1 } = req.body || {};
    if (SourceIP != null && SourceIP !== '' && !isNdiEndpoint(SourceIP)) return res.status(400).json({ ok: false, error: 'invalid source IPv4 endpoint' });
    const ch = parseChannel(Output, 0);
    if (!ch) return res.status(400).json({ ok: false, error: 'output must be 1–8' });
    const outputIdx = ch - 1;
    const settingsFile = `/etc/ndimon-dec${ch}-settings.json`;
    cancelReconnect(outputIdx);  // explicit user action — cancel pending auto-reconnect
    if (SourceName && SourceName !== 'None') notifyManualConnect(outputIdx);
    const cfg = readJson(settingsFile);
    if (SourceName && SourceName !== 'None') {
        const sourceIp = resolveNdiEndpoint(SourceName, SourceIP, SourceIP ? null : readJson(NDI_SRC));
        cfg.SourceName = SourceName;
        cfg.SourceIP   = sourceIp;
        cfg.SourceSelection = 'NDI';
        writeJson(settingsFile, cfg);
        sendIPC({ action: 'connect', source_name: SourceName, source_ip: sourceIp,
                  output: outputIdx });
    } else {
        cfg.SourceName = '';
        cfg.SourceIP   = '';
        writeJson(settingsFile, cfg);
        // Use forget_source to clear saved source — prevents auto-reconnect
        sendIPC({ action: 'forget_source', output: outputIdx });
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
