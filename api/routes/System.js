'use strict';
const express    = require('express');
const router     = express.Router();
const { execSync } = require('child_process');
const { sendIPC, corsHeaders } = require('./lib');

router.use((req, res, next) => { corsHeaders(res); next(); });

// Reboot
router.post('/reboot', (req, res) => {
    res.json({ ok: true });
    setTimeout(() => {
        try { execSync('reboot'); } catch {}
    }, 1000);
});

router.get('/reboot', (req, res) => {
    res.json({ ok: true });
    setTimeout(() => {
        try { execSync('reboot'); } catch {}
    }, 1000);
});

// Soft reboot (restart services)
router.post('/softreboot', (req, res) => {
    res.json({ ok: true });
    setTimeout(() => {
        try { execSync('systemctl --user restart ndi-decoder.service ndi-finder.service ndi-api.service'); } catch {}
    }, 500);
});

// Status
router.get('/status', async (req, res) => {
    const status = await sendIPC({ action: 'status' });
    res.json(status);
});

module.exports = router;
