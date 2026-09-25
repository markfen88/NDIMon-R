'use strict';
const express    = require('express');
const router     = express.Router();
const fs         = require('fs');
const path       = require('path');
const { execFile } = require('child_process');
const { sendIPC, corsHeaders, runPriv } = require('./lib');

router.use((req, res, next) => { corsHeaders(res); next(); });

function readTrim(p) {
    try { return fs.readFileSync(p, 'utf8').replace(/\0/g, '').trim(); } catch { return ''; }
}
function sourceDir() { return readTrim('/etc/ndimon-source-dir'); }

// GET /version — installed version info + whether a git update is available.
router.get('/version', (req, res) => {
    const dir = sourceDir();
    execFile('systemctl', ['is-active', '--quiet', 'ndimon-update.service'],
        { timeout: 2000 }, (activeErr) => {
        const info = {
            firmware:  readTrim('/etc/ndimon-firmware-version') || '1.0.0',
            commit:    readTrim('/etc/ndimon-build-commit'),
            build_date: readTrim('/etc/ndimon-build-date'),
            ndi_version: readTrim('/etc/ndimon-ndi-version') || '6.x',
            update_supported: !!dir,
            update_running: !activeErr,
        };
        if (!dir) return res.json(info);
        // Compare local HEAD with origin without modifying the tree.
        execFile('git', ['-C', dir, 'fetch', '--quiet'], { timeout: 15000 }, () => {
            execFile('git', ['-C', dir, 'rev-list', '--count', 'HEAD..@{u}'],
                { timeout: 5000 }, (err, stdout) => {
                    info.updates_available = err ? null : parseInt(stdout.trim(), 10) || 0;
                    res.json(info);
                });
        });
    });
});

// POST /update — pid1 runs the oneshot so the API sandbox cannot block the
// checkout write. Returns immediately; poll /version.
router.post('/update', (req, res) => {
    const dir = sourceDir();
    if (!dir) return res.status(501).json({ ok: false, error: 'no source checkout recorded' });
    if (!path.isAbsolute(dir) || dir.includes('\0') || dir.includes('..'))
        return res.status(501).json({ ok: false, error: 'invalid source checkout' });
    if (!fs.existsSync(path.join(dir, 'install.sh')))
        return res.status(501).json({ ok: false, error: 'install.sh missing' });
    runPriv(['start-update'], { timeout: 15000 }, (err, stdout, stderr) => {
        if (err) {
            const msg = `${stderr || ''} ${err.message || ''}`;
            if (err.code === 3 || /already running/.test(msg))
                return res.status(409).json({ ok: false, error: 'update already running' });
            return res.status(500).json({ ok: false, error: (stderr || err.message || 'update failed to start').toString().trim() });
        }
        res.json({ ok: true, message: 'update started — the device will rebuild and restart services' });
    });
});

// Reboot — POST only. A GET reboot is trivially triggerable cross-site
// (e.g. an <img> tag) and state-changing GETs violate HTTP semantics.
router.post('/reboot', (req, res) => {
    res.json({ ok: true });
    setTimeout(() => {
        runPriv(['reboot'], () => {});
    }, 1000);
});

// Soft reboot (restart the NDIMon services).
router.post('/softreboot', (req, res) => {
    res.json({ ok: true });
    setTimeout(() => {
        runPriv(['restart-stack'], () => {});
    }, 500);
});

// GET /vaapi-info — VAAPI driver + supported decode profiles (x86 only).
// Parsed from /etc/ndimon-vaapi-info (vainfo output recorded at install).
router.get('/vaapi-info', (req, res) => {
    const raw = readTrim('/etc/ndimon-vaapi-info');
    if (!raw) return res.json({ available: false });
    let driver = '';
    const profiles = [];
    for (const line of raw.split('\n')) {
        const d = line.match(/Driver version:\s*(.+)/);
        if (d) driver = d[1].trim();
        // e.g. "VAProfileH264High           : VAEntrypointVLD"
        const p = line.match(/VAProfile(\S+)\s*:\s*VAEntrypointVLD/);
        if (p) profiles.push(p[1]);
    }
    const decode = [...new Set(profiles)].filter(p => /H264|HEVC|VP9|AV1/i.test(p));
    res.json({ available: true, driver, decode_profiles: decode });
});

// Status
router.get('/status', async (req, res) => {
    const status = await sendIPC({ action: 'status' });
    res.json(status);
});

module.exports = router;
