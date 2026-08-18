'use strict';
const express    = require('express');
const router     = express.Router();
const fs         = require('fs');
const path       = require('path');
const { execFile, spawn } = require('child_process');
const { sendIPC, corsHeaders, runPriv } = require('./lib');

router.use((req, res, next) => { corsHeaders(res); next(); });

function readTrim(p) {
    try { return fs.readFileSync(p, 'utf8').replace(/\0/g, '').trim(); } catch { return ''; }
}
function sourceDir() { return readTrim('/etc/ndimon-source-dir'); }

// Tracks an in-progress update so the UI can poll status.
let updateState = { running: false, started: 0, log: '' };

// GET /version — installed version info + whether a git update is available.
router.get('/version', (req, res) => {
    const dir = sourceDir();
    const info = {
        firmware:  readTrim('/etc/ndimon-firmware-version') || '1.0.0',
        commit:    readTrim('/etc/ndimon-build-commit'),
        build_date: readTrim('/etc/ndimon-build-date'),
        ndi_version: readTrim('/etc/ndimon-ndi-version') || '6.x',
        update_supported: !!dir,
        update_running: updateState.running,
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

// POST /update — git pull + rebuild + reinstall (detached so restarting
// ndimon-api mid-update doesn't kill it). Returns immediately; poll /version.
router.post('/update', (req, res) => {
    const dir = sourceDir();
    if (!dir) return res.status(501).json({ ok: false, error: 'no source checkout recorded' });
    if (!dir || !path.isAbsolute(dir) || dir.includes('\0') || dir.includes('..'))
        return res.status(501).json({ ok: false, error: 'invalid source checkout' });
    if (!fs.existsSync(path.join(dir, 'install.sh')))
        return res.status(501).json({ ok: false, error: 'install.sh missing' });
    if (updateState.running) return res.status(409).json({ ok: false, error: 'update already running' });

    updateState = { running: true, started: Date.now(), log: '' };
    const logFd = fs.openSync('/tmp/ndimon-update.log', 'w');
    // $1 is the source dir — never interpolate it into the shell script.
    const child = spawn('bash', ['-c',
        'git -C "$1" pull --ff-only && bash "$1/install.sh" --no-deps',
        'ndimon-update', dir],
        { detached: true, stdio: ['ignore', logFd, logFd] });
    child.unref();
    // Best-effort clear of the running flag (the API may be restarted before this).
    setTimeout(() => { updateState.running = false; }, 120000);
    res.json({ ok: true, message: 'update started — the device will rebuild and restart services' });
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
