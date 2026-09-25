'use strict';
// Wired network settings. DHCP or a single static IPv4 address on the
// primary wired interface. Applying a change goes through ndimon-priv →
// ndimon-net, which rolls the change back unless it is confirmed from the
// new address. Wi-Fi is not handled.
const express = require('express');
const crypto  = require('crypto');
const fs      = require('fs');
const router  = express.Router();
const { corsHeaders, runPrivAsync, writeJson } = require('./lib');

const STAGE_DIR = '/var/lib/ndimon/net';

router.use((req, res, next) => { corsHeaders(res); next(); });

const confirmFailures = new Map();

function clientIp(req) {
    let ip = (req.socket && req.socket.remoteAddress) || '';
    if (ip.startsWith('::ffff:')) ip = ip.slice(7);
    return ip;
}

function ipv4ToInt(ip) {
    const parts = String(ip || '').split('.');
    if (parts.length !== 4) return null;
    let value = 0;
    for (const part of parts) {
        if (!/^\d+$/.test(part)) return null;
        const n = Number(part);
        if (n > 255) return null;
        value = ((value << 8) | n) >>> 0;
    }
    return value;
}

function subnetMask(prefix) {
    const bits = Number(prefix);
    if (!Number.isInteger(bits) || bits < 0 || bits > 32) return null;
    if (bits === 0) return 0;
    return bits === 32 ? 0xffffffff : (~0 << (32 - bits)) >>> 0;
}

function unicastProblem(ip, prefix) {
    const value = ipv4ToInt(ip);
    if (value == null) return 'address is not a valid IPv4 address';
    if (value === 0 || (value >>> 24) === 127 || (value >>> 16) === 0xa9fe || (value >>> 28) === 0xe)
        return 'address is not a usable unicast address';
    const mask = subnetMask(prefix);
    if (mask == null || prefix >= 31) return null;
    const network = (value & mask) >>> 0;
    const broadcast = (network | (~mask >>> 0)) >>> 0;
    if (value === network || value === broadcast)
        return 'address is the network or broadcast address';
    return null;
}
function sameSubnet(a, b, prefix) {
    const ia = ipv4ToInt(a);
    const ib = ipv4ToInt(b);
    const mask = subnetMask(prefix);
    if (ia == null || ib == null || mask == null || prefix < 1) return false;
    return (ia & mask) === (ib & mask);
}

function dnsKey(list) {
    return (Array.isArray(list) ? list : []).map(String).slice().sort().join(',');
}

function settingsEqual(a, b) {
    if (!a || !b) return false;
    if (a.mode !== b.mode) return false;
    if (a.mode === 'dhcp' || b.mode === 'dhcp') return a.mode === 'dhcp' && b.mode === 'dhcp';
    return a.address === b.address &&
           Number(a.prefix) === Number(b.prefix) &&
           (a.gateway || '') === (b.gateway || '') &&
           dnsKey(a.dns) === dnsKey(b.dns);
}

function normalizeRequest(body) {
    const mode = body && body.mode;
    if (mode === 'dhcp')
        return { mode: 'dhcp', address: '', prefix: 0, gateway: '', dns: [] };
    if (mode !== 'static') {
        const err = new Error('mode must be dhcp or static');
        err.status = 400;
        throw err;
    }
    const address = String(body.address || '').trim();
    const prefix = Number(body.prefix);
    const gateway = String(body.gateway || '').trim();
    const dns = Array.isArray(body.dns) ? body.dns.map(d => String(d).trim()).filter(Boolean) : [];
    if (ipv4ToInt(address) == null) {
        const err = new Error('address is not a valid IPv4 address');
        err.status = 400;
        throw err;
    }
    if (!Number.isInteger(prefix) || prefix < 8 || prefix > 30) {
        const err = new Error('prefix must be between 8 and 30');
        err.status = 400;
        throw err;
    }
    const problem = unicastProblem(address, prefix);
    if (problem) {
        const err = new Error(problem);
        err.status = 400;
        throw err;
    }
    if (gateway && ipv4ToInt(gateway) == null) {
        const err = new Error('gateway is not a valid IPv4 address');
        err.status = 400;
        throw err;
    }
    if (gateway && !sameSubnet(address, gateway, prefix)) {
        const err = new Error('gateway is not inside the address subnet');
        err.status = 400;
        throw err;
    }
    if (dns.length > 3 || dns.some(d => ipv4ToInt(d) == null)) {
        const err = new Error('dns must be at most 3 IPv4 addresses');
        err.status = 400;
        throw err;
    }
    return { mode: 'static', address, prefix, gateway, dns };
}

async function readStatus() {
    const { stdout } = await runPrivAsync(['net-status'], 8000);
    const doc = JSON.parse(stdout);
    if (!doc || doc.ok === false) {
        const err = new Error((doc && doc.error) || 'network status failed');
        throw err;
    }
    return doc;
}

async function checkAddress(address) {
    const { stdout } = await runPrivAsync(['net-check-address', address], 12000);
    return JSON.parse(stdout);
}

async function stageAndApply(settings) {
    const requested = normalizeRequest(settings);
    fs.mkdirSync(STAGE_DIR, { recursive: true });
    const stage = `${STAGE_DIR}/stage-${crypto.randomBytes(8).toString('hex')}.json`;
    if (!writeJson(stage, requested)) {
        const err = new Error('could not stage network settings');
        err.status = 500;
        throw err;
    }
    try {
        const { stdout } = await runPrivAsync(['net-apply', stage], 20000);
        return JSON.parse(stdout);
    } finally {
        try { fs.unlinkSync(stage); } catch {}
    }
}

// What the restore screen should say. needs_prompt is false when both sides
// are DHCP, or both are the same static settings — the UI stays quiet then.
function describeDifference(current, backup, browserIp) {
    const cur = current || {};
    const bak = backup || {};
    if (!backup) return { present: false, needs_prompt: false, warnings: [] };
    if (bak.mode === 'unknown' || cur.applicable === false) {
        return {
            present: true,
            applicable: false,
            needs_prompt: false,
            warnings: [],
            message: 'Network settings in this backup cannot be applied on this system'
                + (cur.reason ? ` (${cur.reason})` : '') + '. They will be skipped.',
        };
    }
    if (settingsEqual(cur, bak)) {
        return {
            present: true,
            applicable: true,
            needs_prompt: false,
            warnings: [],
            message: '',
        };
    }
    const warnings = [];
    let message;
    if (bak.mode === 'dhcp') {
        message = 'The backup uses DHCP. This device is currently set to a static address'
            + (cur.address ? ` (${cur.address})` : '')
            + '. The new address will come from the DHCP server and is not known in advance.'
            + ' It will be shown on the connected display.';
    } else if (cur.mode === 'dhcp' || cur.mode === 'unknown') {
        message = 'The network configuration in this backup is different. The backup uses a static address: '
            + `${bak.address}/${bak.prefix}`
            + (bak.gateway ? `, gateway ${bak.gateway}` : '')
            + (bak.dns && bak.dns.length ? `, DNS ${bak.dns.join(', ')}` : '')
            + `. This device currently uses DHCP`
            + (cur.live_address ? ` (current address ${cur.live_address})` : '')
            + '.';
    } else {
        const bits = [];
        if (cur.address !== bak.address || Number(cur.prefix) !== Number(bak.prefix))
            bits.push(`address ${cur.address}/${cur.prefix} → ${bak.address}/${bak.prefix}`);
        if ((cur.gateway || '') !== (bak.gateway || ''))
            bits.push(`gateway ${cur.gateway || '(none)'} → ${bak.gateway || '(none)'}`);
        if (dnsKey(cur.dns) !== dnsKey(bak.dns))
            bits.push(`DNS ${(cur.dns || []).join(', ') || '(none)'} → ${(bak.dns || []).join(', ') || '(none)'}`);
        message = 'The network configuration in this backup is different. The backup uses a static address. '
            + (bits.join('; ') || `${bak.address}/${bak.prefix}`)
            + '. Overwrite the current network settings?';
    }
    if (bak.mode === 'static' && browserIp && !sameSubnet(browserIp, bak.address, bak.prefix)) {
        warnings.push(`Your computer (${browserIp}) may not be able to reach the device at ${bak.address}.`);
    }
    return {
        present: true,
        applicable: true,
        needs_prompt: true,
        current_mode: cur.mode || 'unknown',
        backup_mode: bak.mode,
        message,
        warnings,
    };
}

function rateLimited(ip) {
    const now = Date.now();
    const hits = (confirmFailures.get(ip) || []).filter(t => now - t < 60000);
    confirmFailures.set(ip, hits);
    return hits.length >= 5;
}

function recordFailure(ip) {
    const hits = confirmFailures.get(ip) || [];
    hits.push(Date.now());
    confirmFailures.set(ip, hits);
}

// Unauthenticated. The browser lands on the new address without the old cookie.
async function confirm(req, res) {
    const ip = clientIp(req);
    if (rateLimited(ip))
        return res.status(429).json({ ok: false, error: 'too many attempts, wait a minute' });
    const token = String((req.body && req.body.token) || '');
    if (!/^[0-9a-f]{64}$/.test(token)) {
        recordFailure(ip);
        return res.status(400).json({ ok: false, error: 'bad token' });
    }
    try {
        await runPrivAsync(['net-confirm', token], 60000);
        res.json({ ok: true });
    } catch (e) {
        recordFailure(ip);
        const msg = `${e.stderr || ''} ${e.message || ''}`;
        const expired = /no network change is pending|does not match|bad token/.test(msg);
        res.status(expired ? 410 : 500).json({
            ok: false,
            error: expired
                ? 'This confirmation has expired. The device has returned to its previous settings.'
                : ((e.stderr || e.message || 'confirm failed') + '').trim(),
        });
    }
}

router.get('/status', async (req, res) => {
    try {
        res.json(await readStatus());
    } catch (e) {
        res.status(500).json({ ok: false, error: (e.stderr || e.message || 'status failed').toString().trim() });
    }
});

router.post('/check', async (req, res) => {
    const address = String((req.body && req.body.address) || '').trim();
    if (ipv4ToInt(address) == null)
        return res.status(400).json({ ok: false, error: 'address is not a valid IPv4 address' });
    try {
        res.json(await checkAddress(address));
    } catch (e) {
        res.status(500).json({ ok: false, error: (e.stderr || e.message || 'check failed').toString().trim() });
    }
});

router.post('/apply', async (req, res) => {
    try {
        const requested = normalizeRequest(req.body || {});
        const current = await readStatus();
        if (!current.applicable)
            return res.status(409).json({ ok: false, error: current.reason || 'not supported on this system' });
        let duplicate = false;
        if (requested.mode === 'static' && requested.address !== (current.live_address || current.address)) {
            try {
                const probe = await checkAddress(requested.address);
                duplicate = probe.duplicate === true;
            } catch {}
        }
        const diff = describeDifference(current, requested, clientIp(req));
        if (req.body && req.body.dry_run) {
            return res.json({ ok: true, dry_run: true, duplicate, ...diff });
        }
        const result = await stageAndApply(requested);
        res.json({ ok: true, duplicate, ...result });
    } catch (e) {
        res.status(e.status || 500).json({ ok: false, error: (e.stderr || e.message || 'apply failed').toString().trim() });
    }
});

router.post('/rollback', async (req, res) => {
    try {
        await runPrivAsync(['net-rollback'], 30000);
        res.json({ ok: true });
    } catch (e) {
        res.status(500).json({ ok: false, error: (e.stderr || e.message || 'rollback failed').toString().trim() });
    }
});

module.exports = {
    router, confirm, readStatus, checkAddress, stageAndApply,
    settingsEqual, describeDifference, clientIp, normalizeRequest,
};
