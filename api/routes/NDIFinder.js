'use strict';
const express = require('express');
const router  = express.Router();
const fs      = require('fs');
const { readJson, writeJson, sendIPC, corsHeaders } = require('./lib');
const { exec } = require('child_process');

const PARAM_FILE    = '/etc/ndi_src_find_param';
const SRC_LIST      = '/etc/ndimon-sources.json';
const FIND_SETTINGS = '/etc/ndimon-find-settings.json';
const NDI_CONFIG    = '/etc/ndi-config.json';
const NDI_GROUP     = '/etc/ndi-group.json';

router.use((req, res, next) => { corsHeaders(res); next(); });

function triggerRefresh() {
    try { fs.writeFileSync(PARAM_FILE, '-run'); } catch {}
    exec('systemctl --user restart ndimon-finder.service 2>/dev/null || systemctl restart ndimon-finder.service 2>/dev/null', { timeout: 2000 }, err => {
        if (err) console.warn('[NDIFinder] restart ndimon-finder:', err.message);
    });
}

// GET|POST /refresh
router.get('/refresh',  (req, res) => { triggerRefresh(); res.header('Content-Type','text/plain').send('success'); });
router.post('/refresh', (req, res) => { triggerRefresh(); res.header('Content-Type','text/plain').send('success'); });

// GET|POST /reset
router.get('/reset',  (req, res) => {
    try { fs.writeFileSync(PARAM_FILE, '-rs'); } catch {}
    writeJson(SRC_LIST, { count: 1, list: { None: 'None' } });
    res.header('Content-Type','text/plain').send('success');
});
router.post('/reset', (req, res) => {
    try { fs.writeFileSync(PARAM_FILE, '-rs'); } catch {}
    writeJson(SRC_LIST, { count: 1, list: { None: 'None' } });
    res.header('Content-Type','text/plain').send('success');
});

// GET /List
router.get('/List', (req, res) => {
    res.json(readJson(SRC_LIST));
});

// GET|POST /NdiOffSnSrc (off-subnet IPs)
router.get('/NdiOffSnSrc', (req, res) => {
    res.header('Content-Type','text/plain');
    try { res.send(fs.readFileSync(NDI_CONFIG, 'utf8')); } catch { res.send(''); }
});
router.post('/NdiOffSnSrc', (req, res) => {
    res.header('Content-Type','text/plain');
    const ips = (req.body || '').replace(/['"]/g,'').replace(/ /g,'');
    // Validate IPs
    const valid = ips.split(',').every(ip =>
        ip === '' || /^(\d{1,3}\.){3}\d{1,3}$/.test(ip)
    );
    if (!valid) return res.send('Bad request - Invalid IP');
    fs.writeFileSync(NDI_CONFIG, JSON.stringify(ips));
    res.send('success');
});

// GET|POST /NdiGrpName (NDI groups — comma-separated, stored as {"ndi_groups":"..."})
router.get('/NdiGrpName', (req, res) => {
    const grp = readJson(NDI_GROUP);
    const groups = (grp && grp.ndi_groups) ? grp.ndi_groups : 'public';
    res.json({ ndi_groups: groups });
});
router.post('/NdiGrpName', (req, res) => {
    let groups;
    if (typeof req.body === 'string') {
        groups = req.body.replace(/['"]/g, '').trim();
    } else if (req.body && req.body.ndi_groups != null) {
        groups = String(req.body.ndi_groups).trim();
    } else {
        groups = 'public';
    }
    if (!groups) groups = 'public';
    writeJson(NDI_GROUP, { ndi_groups: groups });
    sendIPC({ action: 'reload_config' });
    res.json({ ndi_groups: groups });
});

// GET|POST /NDIDisServer (discovery server)
router.get('/NDIDisServer', (req, res) => {
    const cfg = readJson(FIND_SETTINGS);
    const { NDIDisServ, NDIDisServIP } = req.query;
    let write = false;
    if (NDIDisServ && ['NDIDisServEn','NDIDisServDis'].includes(NDIDisServ) && NDIDisServ !== cfg.NDIDisServ) {
        cfg.NDIDisServ = NDIDisServ; write = true;
    }
    if (NDIDisServIP && NDIDisServIP !== cfg.NDIDisServIP) {
        cfg.NDIDisServIP = NDIDisServIP; write = true;
    }
    if (write) {
        writeJson(FIND_SETTINGS, cfg);
        sendIPC({ action: 'reload_config' });  // pick up new DS address in C++
    }
    res.json(cfg);
});

router.post('/NDIDisServer', (req, res) => {
    const cfg = readJson(FIND_SETTINGS);
    const body = req.body || {};
    let write = false;
    if (body.NDIDisServ && ['NDIDisServEn','NDIDisServDis'].includes(body.NDIDisServ) && body.NDIDisServ !== cfg.NDIDisServ) {
        cfg.NDIDisServ = body.NDIDisServ; write = true;
    }
    if (body.NDIDisServIP != null && body.NDIDisServIP !== cfg.NDIDisServIP) {
        cfg.NDIDisServIP = body.NDIDisServIP; write = true;
    }
    if (write) {
        writeJson(FIND_SETTINGS, cfg);
        sendIPC({ action: 'reload_config' });  // pick up new DS address in C++
    }
    res.json(body);
});

module.exports = router;
