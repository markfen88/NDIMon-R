'use strict';
// Source presets — named NDI source assignments for fast, one-click recall
// (parity with Kiloview's 0-9 presets and BirdDog's fast source switching).
// Recall sends a connect IPC with the pre-resolved source+IP so switching is
// immediate and does not wait for a network rescan.
const express = require('express');
const router  = express.Router();
const { readJson, writeJson, sendIPC, corsHeaders } = require('./lib');
const { notifyManualConnect, stopReconnectLoop } = require('./Status');

const PRESETS_FILE = '/etc/ndimon-presets.json';
const MAX_PRESETS  = 32;

router.use((req, res, next) => { corsHeaders(res); next(); });

function loadPresets() {
    const j = readJson(PRESETS_FILE);
    return Array.isArray(j.presets) ? j.presets : [];
}

function savePresets(presets) {
    writeJson(PRESETS_FILE, { presets });
}

// GET /v1/Presets/list
router.get('/list', (req, res) => {
    res.json({ presets: loadPresets() });
});

// POST /v1/Presets/save  { name, source, ip }
// Adds a new preset or updates the existing one with the same name.
router.post('/save', (req, res) => {
    const body = req.body || {};
    const name   = String(body.name || '').trim().slice(0, 64);
    const source = String(body.source || '').trim();
    const ip     = String(body.ip || '').trim();
    if (!name)   return res.status(400).json({ ok: false, error: 'name required' });
    if (!source) return res.status(400).json({ ok: false, error: 'source required' });

    const presets = loadPresets();
    const idx = presets.findIndex(p => p.name === name);
    const entry = { name, source, ip };
    if (idx >= 0) {
        presets[idx] = entry;
    } else {
        if (presets.length >= MAX_PRESETS)
            return res.status(400).json({ ok: false, error: `max ${MAX_PRESETS} presets` });
        presets.push(entry);
    }
    savePresets(presets);
    res.json({ ok: true, presets });
});

// POST /v1/Presets/delete  { name }
router.post('/delete', (req, res) => {
    const name = String((req.body && req.body.name) || '').trim();
    const presets = loadPresets().filter(p => p.name !== name);
    savePresets(presets);
    res.json({ ok: true, presets });
});

// POST /v1/Presets/recall  { name, output }
// Connects the given output (1-based ch) to the preset's source immediately.
router.post('/recall', (req, res) => {
    const body = req.body || {};
    const name = String(body.name || '').trim();
    const output = (parseInt(body.output, 10) || 1) - 1;   // 0-based index
    const ch = output + 1;

    const preset = loadPresets().find(p => p.name === name);
    if (!preset) return res.status(404).json({ ok: false, error: 'preset not found' });

    // Treat recall as an explicit user action: cancel pending auto-reconnect and
    // start the manual-connect grace period so DS routing won't override it.
    stopReconnectLoop(output);
    notifyManualConnect(output);

    // Persist selection so it survives reboot, then connect immediately.
    const settingsFile = `/etc/ndimon-dec${ch}-settings.json`;
    const cfg = readJson(settingsFile);
    cfg.SourceName = preset.source;
    cfg.SourceIP   = preset.ip || '';
    cfg.SourceSelection = 'NDI';
    writeJson(settingsFile, cfg);

    sendIPC({ action: 'connect', source_name: preset.source,
              source_ip: preset.ip || '', output });
    res.json({ ok: true, recalled: preset.name, source: preset.source });
});

module.exports = router;
