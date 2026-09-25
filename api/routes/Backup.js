'use strict';
// One-file backup and restore of user settings.
//
// NDI SDK notes this follows (docs/ndi/REFERENCE.md):
// - Groups are case-sensitive and are stored exactly as entered. Empty becomes
//   "public", which is the SDK default group.
// - Discovery Server is enabled by a non-empty address, not a separate toggle.
//   Addresses are IPv4, comma-separated, with an optional :port (default 5959).
// - The receiver name is the OS hostname (hostnamectl). This does not write
//   machinename into ndi-config.v1.json; the SDK docs discourage that because
//   it causes mDNS clashes.
// - ndi-config.v1.json is NOT part of the backup. The decoder rewrites it on
//   startup from the files restored here, including HX passthrough.
// - NTP is the OS clock. NDI has no time server of its own.
const express    = require('express');
const bodyParser = require('body-parser');
const crypto     = require('crypto');
const fs         = require('fs');
const os         = require('os');
const path       = require('path');
const Ajv        = require('ajv');
const router  = express.Router();
const { readJson, writeJson, corsHeaders, runPrivAsync } = require('./lib');
const auth    = require('../auth');
const network = require('./Network');
const device  = require('./DeviceSettings');

const IMPORT_DIR   = '/var/lib/ndimon/import';
const BACKUP_DIR   = '/var/lib/ndimon/backups';
const IMPORT_TTL   = 15 * 60 * 1000;
const MAX_SNAPSHOTS = 5;

const OUTPUT_KEYS = [
    'SourceName', 'SourceIP', 'output_alias', 'ScaleMode', 'rotation',
    'videooutput', 'NDIAudio', 'TallyMode', 'ScreenSaverMode', 'ColorSpace',
];
const DEVICE_KEYS = [
    'watchdog_mode', 'decode_mode', 'ntp_server',
    'reboot_schedule_enabled', 'reboot_schedule_time', 'reboot_schedule_days',
];
const SPLASH_KEYS = [
    'bg_idle', 'bg_live', 'accent_idle', 'accent_live', 'logo_path',
    'logo_x_pct', 'logo_y_pct', 'logo_w_pct', 'text_idle', 'text_live',
    'text_height_pct', 'show_box', 'show_signal_text', 'show_device_name',
    'show_device_url', 'show_sources_available',
];

router.use((req, res, next) => { corsHeaders(res); next(); });

const ajv = new Ajv({ allErrors: true, strict: false });
const outputItem = {
    type: 'object', additionalProperties: false,
    properties: {
        SourceName: { type: 'string', maxLength: 256 },
        SourceIP: { type: 'string', maxLength: 64 },
        output_alias: { type: 'string', maxLength: 64 },
        ScaleMode: { enum: ['letterbox', 'stretch', 'crop'] },
        rotation: { enum: [0, 90, 180, 270] },
        videooutput: { type: 'string', maxLength: 32 },
        NDIAudio: { enum: ['NDIAudioEn', 'NDIAudioDis'] },
        TallyMode: { enum: ['TallyOn', 'TallyOff', 'VideoMode'] },
        ScreenSaverMode: { enum: ['SplashSS', 'BlackSS', 'CaptureSS'] },
        ColorSpace: { enum: ['RGB', 'YUV'] },
    },
};
const validateFile = ajv.compile({
    type: 'object',
    additionalProperties: false,
    required: ['format', 'schema_version', 'created_at', 'sections', 'checksum'],
    properties: {
        format: { const: 'ndimon-backup' },
        schema_version: { const: 1 },
        created_at: { type: 'string', maxLength: 40 },
        checksum: { type: 'string', pattern: '^[0-9a-f]{64}$' },
        app: { type: 'object' },
        source_device: { type: 'object' },
        sections: {
            type: 'object',
            additionalProperties: false,
            properties: {
                outputs: {
                    type: 'object', additionalProperties: false,
                    patternProperties: { '^[1-8]$': outputItem },
                },
                presets: {
                    type: 'object', additionalProperties: false, required: ['presets'],
                    properties: {
                        presets: {
                            type: 'array', maxItems: 32,
                            items: {
                                type: 'object', additionalProperties: false,
                                required: ['name', 'source'],
                                properties: {
                                    name: { type: 'string', minLength: 1, maxLength: 64 },
                                    source: { type: 'string', minLength: 1, maxLength: 256 },
                                    ip: { type: 'string', maxLength: 64 },
                                },
                            },
                        },
                    },
                },
                discovery: {
                    type: 'object', additionalProperties: false,
                    properties: {
                        NDIDisServIP: { type: 'string', maxLength: 512 },
                        ndi_groups: { type: 'string', maxLength: 512 },
                        off_subnet_ips: { type: 'string', maxLength: 1024 },
                    },
                },
                transport: {
                    type: 'object', additionalProperties: false,
                    properties: { Rxpm: { enum: ['TCP', 'UDP', 'Multicast', 'M-TCP', 'RUDP'] } },
                },
                display: {
                    type: 'object', additionalProperties: false,
                    properties: {
                        splash: { type: 'object' },
                        osd: {
                            type: 'object', additionalProperties: false,
                            properties: {
                                enabled: { type: 'boolean' },
                                text: { type: 'string', maxLength: 128 },
                            },
                        },
                        logo_base64: { type: 'string', maxLength: 6000000 },
                        logo_ext: { enum: ['png', 'jpg', 'jpeg', ''] },
                    },
                },
                device: {
                    type: 'object', additionalProperties: false,
                    properties: {
                        watchdog_mode: { enum: ['disabled', 'passive', 'active'] },
                        decode_mode: { enum: ['auto', 'hardware', 'software'] },
                        ntp_server: { type: 'string', maxLength: 253 },
                        reboot_schedule_enabled: { type: 'boolean' },
                        reboot_schedule_time: { type: 'string', pattern: '^([01][0-9]|2[0-3]):[0-5][0-9]$' },
                        reboot_schedule_days: {
                            type: 'array', maxItems: 7,
                            items: { enum: ['sun', 'mon', 'tue', 'wed', 'thu', 'fri', 'sat'] },
                        },
                    },
                },
                identity: {
                    type: 'object', additionalProperties: false,
                    properties: {
                        ndi_recv_name: { type: 'string', maxLength: 64 },
                        hostname: { type: 'string', maxLength: 63 },
                    },
                },
                network: {
                    type: 'object', additionalProperties: false,
                    required: ['mode'],
                    properties: {
                        mode: { enum: ['dhcp', 'static', 'unknown'] },
                        address: { type: 'string', maxLength: 32 },
                        prefix: { type: 'integer', minimum: 0, maximum: 32 },
                        gateway: { type: 'string', maxLength: 32 },
                        dns: { type: 'array', maxItems: 3, items: { type: 'string', maxLength: 32 } },
                        manager: { type: 'string', maxLength: 32 },
                        interface: { type: 'string', maxLength: 32 },
                        applicable: { type: 'boolean' },
                    },
                },
                auth: {
                    type: 'object', additionalProperties: false, required: ['salt', 'hash'],
                    properties: {
                        salt: { type: 'string', pattern: '^[0-9a-f]{32}$' },
                        hash: { type: 'string', pattern: '^[0-9a-f]{64}$' },
                    },
                },
            },
        },
    },
});

function canonicalize(value) {
    if (value === null || typeof value !== 'object') return JSON.stringify(value);
    if (Array.isArray(value)) return '[' + value.map(canonicalize).join(',') + ']';
    const keys = Object.keys(value).sort();
    return '{' + keys.map(k => JSON.stringify(k) + ':' + canonicalize(value[k])).join(',') + '}';
}

function checksumOf(sections) {
    return crypto.createHash('sha256').update(canonicalize(sections)).digest('hex');
}

function readTrim(file) {
    try { return fs.readFileSync(file, 'utf8').trim(); } catch { return ''; }
}

function isIpv4(ip) {
    const parts = String(ip || '').split('.');
    return parts.length === 4 && parts.every(o => /^\d+$/.test(o) && Number(o) >= 0 && Number(o) <= 255);
}

function cleanDiscovery(value) {
    const text = String(value || '').trim();
    if (!text) return '';
    const parts = text.split(',').map(p => p.trim()).filter(Boolean);
    for (const part of parts) {
        const match = part.match(/^(\d{1,3}(?:\.\d{1,3}){3})(?::(\d{1,5}))?$/);
        if (!match || !isIpv4(match[1]))
            throw new Error('Discovery Server must be an IPv4 address, with an optional :port');
        if (match[2] && (Number(match[2]) < 1 || Number(match[2]) > 65535))
            throw new Error('Discovery Server port is out of range');
    }
    return parts.join(',');
}

function cleanGroups(value) {
    const text = String(value == null ? '' : value).trim();
    if (/[\u0000-\u001f]/.test(text)) throw new Error('NDI groups contain control characters');
    // Case is significant to the SDK. Do not fold it.
    return text || 'public';
}

function cleanIpList(value, label) {
    const text = String(value || '').trim();
    if (!text) return '';
    const parts = text.split(',').map(p => p.trim()).filter(Boolean);
    if (!parts.every(isIpv4)) throw new Error(label + ' must be comma-separated IPv4 addresses');
    return parts.join(',');
}

function pick(obj, keys) {
    const out = {};
    for (const key of keys) if (obj && obj[key] !== undefined) out[key] = obj[key];
    return out;
}

function macSuffix() {
    try {
        const names = fs.readdirSync('/sys/class/net');
        for (const name of names) {
            if (name === 'lo') continue;
            const mac = readTrim(`/sys/class/net/${name}/address`).replace(/:/g, '');
            if (mac.length >= 6 && mac !== '000000000000') return mac.slice(-6).toUpperCase();
        }
    } catch {}
    return '';
}

function readOutputs() {
    const outputs = {};
    for (let ch = 1; ch <= 8; ch++) {
        const file = `/etc/ndimon-dec${ch}-settings.json`;
        if (!fs.existsSync(file)) continue;
        const cfg = readJson(file);
        if (!cfg || typeof cfg !== 'object' || Array.isArray(cfg)) continue;
        outputs[String(ch)] = pick(cfg, OUTPUT_KEYS);
    }
    return outputs;
}

function readOffSubnet() {
    const value = readJson('/etc/ndi-config.json');
    return typeof value === 'string' ? value : '';
}

function buildSections(opts) {
    const deviceCfg = readJson('/etc/ndimon-device-settings.json');
    const splash = readJson('/etc/ndimon-splash-settings.json');
    const osd = readJson('/etc/ndimon-osd-settings.json');
    const sections = {
        outputs: readOutputs(),
        presets: { presets: (readJson('/etc/ndimon-presets.json').presets) || [] },
        discovery: {
            NDIDisServIP: readJson('/etc/ndimon-find-settings.json').NDIDisServIP || '',
            ndi_groups: readJson('/etc/ndi-group.json').ndi_groups || 'public',
            off_subnet_ips: readOffSubnet(),
        },
        transport: { Rxpm: readJson('/etc/ndimon-rx-settings.json').Rxpm || 'TCP' },
        display: {
            splash: pick(splash, SPLASH_KEYS),
            osd: { enabled: !!osd.enabled, text: String(osd.text || '').slice(0, 128) },
        },
        device: pick(deviceCfg, DEVICE_KEYS),
        identity: {
            ndi_recv_name: deviceCfg.ndi_recv_name || '',
            hostname: os.hostname(),
        },
    };
    const logo = findLogo();
    if (logo) {
        sections.display.logo_base64 = fs.readFileSync(logo.file).toString('base64');
        sections.display.logo_ext = logo.ext;
    }
    if (opts.network && opts.networkStatus) {
        const n = opts.networkStatus;
        sections.network = {
            mode: n.mode || 'unknown',
            address: n.address || '',
            prefix: Number(n.prefix) || 0,
            gateway: n.gateway || '',
            dns: n.dns || [],
            manager: n.backend || '',
            interface: n.interface || '',
            applicable: !!n.applicable,
        };
    }
    if (opts.password) {
        const stored = readJson('/etc/ndimon-auth.json');
        if (stored && stored.salt && stored.hash)
            sections.auth = { salt: stored.salt, hash: stored.hash };
    }
    return sections;
}

function findLogo() {
    for (const ext of ['png', 'jpg', 'jpeg']) {
        const file = `/etc/ndi-splash-logo.${ext}`;
        if (fs.existsSync(file)) return { file, ext: ext === 'jpeg' ? 'jpg' : ext };
    }
    return null;
}

function buildDocument(sections) {
    const alias = (sections.identity && sections.identity.ndi_recv_name) || 'ndimon';
    return {
        format: 'ndimon-backup',
        schema_version: 1,
        created_at: new Date().toISOString(),
        app: {
            firmware: readTrim('/etc/ndimon-firmware-version') || '1.0.0',
            commit: readTrim('/etc/ndimon-build-commit'),
            ndi_version: readTrim('/etc/ndimon-ndi-version') || '6.x',
        },
        source_device: {
            hostname: os.hostname(),
            ndi_alias: alias,
            platform: os.arch(),
            mac_suffix: macSuffix(),
        },
        sections,
        checksum: checksumOf(sections),
    };
}

function fileStamp() {
    const d = new Date();
    const p = n => String(n).padStart(2, '0');
    return `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}`;
}

function safeAlias(name) {
    return String(name || 'ndimon').replace(/[^A-Za-z0-9-]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 32) || 'ndimon';
}

router.get('/export', async (req, res) => {
    try {
        const wantNet = req.query.network !== '0';
        const wantPw = req.query.password === '1';
        let networkStatus = null;
        if (wantNet) {
            try { networkStatus = await network.readStatus(); }
            catch (e) { networkStatus = null; }
        }
        const sections = buildSections({ network: wantNet && networkStatus, networkStatus, password: wantPw });
        const doc = buildDocument(sections);
        const name = `ndimon-${safeAlias(doc.source_device.ndi_alias)}-${fileStamp()}.json`;
        res.setHeader('Content-Type', 'application/json');
        res.setHeader('Content-Disposition', `attachment; filename="${name}"`);
        res.send(JSON.stringify(doc, null, 2));
    } catch (e) {
        res.status(500).json({ ok: false, error: e.message });
    }
});

function sweepDir(dir, ttl) {
    let names = [];
    try { names = fs.readdirSync(dir); } catch { return; }
    const now = Date.now();
    for (const name of names) {
        const file = path.join(dir, name);
        try {
            if (ttl && now - fs.statSync(file).mtimeMs > ttl) fs.unlinkSync(file);
        } catch {}
    }
}

function semanticCheck(sections) {
    if (sections.discovery) {
        if (sections.discovery.NDIDisServIP != null)
            sections.discovery.NDIDisServIP = cleanDiscovery(sections.discovery.NDIDisServIP);
        if (sections.discovery.ndi_groups != null)
            sections.discovery.ndi_groups = cleanGroups(sections.discovery.ndi_groups);
        if (sections.discovery.off_subnet_ips != null)
            sections.discovery.off_subnet_ips = cleanIpList(sections.discovery.off_subnet_ips, 'Extra IPs');
    }
    if (sections.presets) {
        for (const preset of sections.presets.presets || []) {
            if (preset.ip && !isIpv4(preset.ip))
                throw new Error('a preset IP is not a valid IPv4 address');
        }
    }
    if (sections.outputs) {
        for (const item of Object.values(sections.outputs)) {
            if (item.SourceIP && !isIpv4(item.SourceIP))
                throw new Error('an output source IP is not a valid IPv4 address');
            if (item.SourceName && /[\u0000-\u001f]/.test(item.SourceName))
                throw new Error('a source name contains control characters');
        }
    }
    if (sections.display && sections.display.splash) {
        const splash = {};
        for (const key of SPLASH_KEYS) {
            if (sections.display.splash[key] !== undefined) splash[key] = sections.display.splash[key];
        }
        for (const key of ['bg_idle', 'bg_live', 'accent_idle', 'accent_live']) {
            if (splash[key] && !/^#[0-9A-Fa-f]{6}$/.test(String(splash[key])))
                throw new Error('splash colour ' + key + ' must be #RRGGBB');
        }
        sections.display.splash = splash;
    }
    if (sections.network && sections.network.mode === 'static')
        network.normalizeRequest(sections.network);
}

function diffOutputs(current, incoming) {
    const lines = [];
    const keys = new Set([...Object.keys(current || {}), ...Object.keys(incoming || {})]);
    for (const ch of [...keys].sort()) {
        const a = (current && current[ch]) || {};
        const b = (incoming && incoming[ch]) || {};
        const bits = [];
        if ((a.SourceName || '') !== (b.SourceName || ''))
            bits.push(`source ${a.SourceName || '(none)'} → ${b.SourceName || '(none)'}`);
        if ((a.ScaleMode || 'letterbox') !== (b.ScaleMode || 'letterbox'))
            bits.push(`scale ${a.ScaleMode || 'letterbox'} → ${b.ScaleMode || 'letterbox'}`);
        if (Number(a.rotation || 0) !== Number(b.rotation || 0))
            bits.push(`rotation ${a.rotation || 0} → ${b.rotation || 0}`);
        if (bits.length) lines.push(`Output ${ch}: ${bits.join(', ')}`);
    }
    return lines;
}

function diffDisplay(incoming) {
    const splash = readJson('/etc/ndimon-splash-settings.json');
    const osd = readJson('/etc/ndimon-osd-settings.json');
    const lines = [];
    const splashKeys = SPLASH_KEYS.filter(k => k !== 'logo_path');
    const curSplash = pick(splash, splashKeys);
    const nextSplash = pick(incoming.splash || {}, splashKeys);
    if (canonicalize(curSplash) !== canonicalize(nextSplash))
        lines.push('Splash colours or overlay text differ');
    const curOsd = { enabled: !!osd.enabled, text: String(osd.text || '').slice(0, 128) };
    const nextOsd = {
        enabled: !!(incoming.osd && incoming.osd.enabled),
        text: String((incoming.osd && incoming.osd.text) || '').slice(0, 128),
    };
    if (curOsd.enabled !== nextOsd.enabled || curOsd.text !== nextOsd.text)
        lines.push(`OSD ${curOsd.enabled ? 'on' : 'off'} → ${nextOsd.enabled ? 'on' : 'off'}`
            + (curOsd.text !== nextOsd.text ? `; text “${curOsd.text || '(none)'}” → “${nextOsd.text || '(none)'}”` : ''));
    const logo = findLogo();
    const haveLogo = !!logo;
    const incomingLogo = !!(incoming.logo_base64);
    if (incoming.logo_base64 === '' && haveLogo) lines.push('Logo will be removed');
    else if (incomingLogo && haveLogo) {
        try {
            if (fs.readFileSync(logo.file).toString('base64') !== incoming.logo_base64)
                lines.push('Logo image differs');
        } catch {
            lines.push('Logo image differs');
        }
    } else if (incomingLogo && !haveLogo) {
        lines.push('Logo will be added');
    }
    return lines;
}

function sectionRows(sections) {
    const currentOutputs = readOutputs();
    const deviceCfg = readJson('/etc/ndimon-device-settings.json');
    const rows = [];
    const add = (key, label, changed, summary) => {
        rows.push({ key, label, changed, summary, selected: !!changed });
    };
    if (sections.outputs) {
        const lines = diffOutputs(currentOutputs, sections.outputs);
        add('outputs', 'Outputs and sources', lines.length > 0, lines.join('\n') || 'No changes');
    }
    if (sections.presets) {
        const cur = JSON.stringify((readJson('/etc/ndimon-presets.json').presets) || []);
        const next = JSON.stringify(sections.presets.presets || []);
        add('presets', 'Source presets', cur !== next,
            `${(sections.presets.presets || []).length} preset(s) in the backup`);
    }
    if (sections.discovery) {
        const cur = {
            NDIDisServIP: readJson('/etc/ndimon-find-settings.json').NDIDisServIP || '',
            ndi_groups: readJson('/etc/ndi-group.json').ndi_groups || 'public',
            off_subnet_ips: readOffSubnet(),
        };
        const changed = cur.NDIDisServIP !== (sections.discovery.NDIDisServIP || '') ||
            cur.ndi_groups !== (sections.discovery.ndi_groups || 'public') ||
            cur.off_subnet_ips !== (sections.discovery.off_subnet_ips || '');
        add('discovery', 'Discovery, groups, extra IPs', changed,
            changed ? `Groups “${cur.ndi_groups}” → “${sections.discovery.ndi_groups || 'public'}”; Discovery Server “${cur.NDIDisServIP}” → “${sections.discovery.NDIDisServIP || ''}”` : 'No changes');
    }
    if (sections.transport) {
        const cur = readJson('/etc/ndimon-rx-settings.json').Rxpm || 'TCP';
        add('transport', 'Transport', cur !== sections.transport.Rxpm, `${cur} → ${sections.transport.Rxpm}`);
    }
    if (sections.display) {
        const lines = diffDisplay(sections.display);
        add('display', 'Splash and OSD', lines.length > 0, lines.join('\n') || 'No changes');
    }
    if (sections.device) {
        const cur = pick(deviceCfg, DEVICE_KEYS);
        const changed = canonicalize(cur) !== canonicalize(sections.device);
        add('device', 'Decoder, watchdog, NTP, reboot schedule', changed, changed ? 'Settings differ' : 'No changes');
    }
    if (sections.identity) {
        const same = (sections.identity.hostname || '') === os.hostname();
        const changed = (deviceCfg.ndi_recv_name || '') !== (sections.identity.ndi_recv_name || '') ||
            (sections.identity.hostname || '') !== os.hostname();
        rows.push({
            key: 'identity',
            label: 'Device name',
            changed,
            selected: same && changed,
            summary: `Backup “${sections.identity.ndi_recv_name || sections.identity.hostname || ''}”, this device “${deviceCfg.ndi_recv_name || os.hostname()}”.`
                + (same ? '' : ' Leave this off when setting up a second device from this backup.'),
        });
    }
    if (sections.auth) {
        rows.push({
            key: 'auth',
            label: 'Admin password',
            changed: true,
            selected: false,
            summary: 'Imports the password hash. You will be asked to log in again.',
        });
    }
    return rows;
}

router.post('/inspect', bodyParser.json({ limit: '8mb' }), async (req, res) => {
    try {
        const body = req.body;
        if (!body || typeof body !== 'object' || Buffer.isBuffer(body))
            return res.status(400).json({ ok: false, error: 'not a valid backup file' });
        if (body.schema_version && body.schema_version !== 1)
            return res.status(400).json({ ok: false, error: 'this backup is from a newer version; update this device first' });
        if (!validateFile(body)) {
            const msg = (validateFile.errors || []).map(e => `${e.instancePath || '/'} ${e.message}`).join('; ');
            return res.status(400).json({ ok: false, error: msg || 'backup file was rejected' });
        }
        const expect = checksumOf(body.sections);
        const got = Buffer.from(body.checksum, 'hex');
        const want = Buffer.from(expect, 'hex');
        if (got.length !== want.length || !crypto.timingSafeEqual(got, want))
            return res.status(400).json({ ok: false, error: 'this backup is damaged or was edited' });
        semanticCheck(body.sections);

        fs.mkdirSync(IMPORT_DIR, { recursive: true });
        sweepDir(IMPORT_DIR, IMPORT_TTL);
        const importId = crypto.randomBytes(16).toString('hex');
        if (!writeJson(path.join(IMPORT_DIR, importId + '.json'), body))
            return res.status(500).json({ ok: false, error: 'could not store the backup for review' });

        let currentNet = { applicable: false, mode: 'unknown', reason: 'status unavailable' };
        try { currentNet = await network.readStatus(); } catch (e) {
            currentNet.reason = (e.stderr || e.message || '').trim();
        }
        let netInfo = { present: false, needs_prompt: false, warnings: [] };
        if (body.sections.network) {
            netInfo = network.describeDifference(currentNet, body.sections.network, network.clientIp(req));
            if (netInfo.needs_prompt && body.sections.network.mode === 'static') {
                try {
                    const probe = await network.checkAddress(body.sections.network.address);
                    if (probe.duplicate === true) {
                        netInfo.duplicate = true;
                        netInfo.warnings = (netInfo.warnings || []).concat(
                            'Another device on this network is already using ' + body.sections.network.address + '. Applying this will cause an address conflict.'
                        );
                    }
                } catch {}
            }
        }
        const present = Object.keys(body.sections);
        const have = Object.keys(readOutputs());
        const extra = present.includes('outputs')
            ? Object.keys(body.sections.outputs).filter(ch => !have.includes(ch))
            : [];
        res.json({
            ok: true,
            import_id: importId,
            created_at: body.created_at,
            source_device: body.source_device || {},
            app: body.app || {},
            sections: sectionRows(body.sections),
            network: netInfo,
            extra_outputs: extra,
        });
    } catch (e) {
        res.status(400).json({ ok: false, error: e.message || 'could not read backup' });
    }
});

function mergeFile(file, patch) {
    const cur = readJson(file);
    const base = (cur && typeof cur === 'object' && !Array.isArray(cur)) ? cur : {};
    if (!writeJson(file, Object.assign(base, patch)))
        throw new Error('could not write ' + file);
}

function writeLogo(display) {
    if (display.logo_base64 == null) return;
    if (display.logo_base64 === '') {
        for (const ext of ['png', 'jpg', 'jpeg']) {
            try { fs.unlinkSync(`/etc/ndi-splash-logo.${ext}`); } catch {}
        }
        return;
    }
    const buf = Buffer.from(display.logo_base64, 'base64');
    if (buf.length < 8 || buf.length > 3 * 1024 * 1024)
        throw new Error('logo is empty or larger than 3 MB');
    const png = buf[0] === 0x89 && buf[1] === 0x50 && buf[2] === 0x4e && buf[3] === 0x47;
    const jpg = buf[0] === 0xff && buf[1] === 0xd8 && buf[2] === 0xff;
    if (!png && !jpg) throw new Error('logo is not a PNG or JPEG');
    const ext = png ? 'png' : 'jpg';
    const file = `/etc/ndi-splash-logo.${ext}`;
    const tmp = file + '.tmp';
    const fd = fs.openSync(tmp, 'w', 0o640);
    try {
        fs.writeSync(fd, buf);
        fs.fsyncSync(fd);
    } finally {
        fs.closeSync(fd);
    }
    fs.renameSync(tmp, file);
    for (const other of ['png', 'jpg', 'jpeg']) {
        if (other !== ext) { try { fs.unlinkSync(`/etc/ndi-splash-logo.${other}`); } catch {} }
    }
    return file;
}

function hostnameFor(identity) {
    const raw = (identity.hostname || identity.ndi_recv_name || '').trim();
    const cleaned = raw.replace(/[^A-Za-z0-9-]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 63);
    if (!/^[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?$/.test(cleaned)) return '';
    return cleaned;
}

const FILE_SECTIONS = ['outputs', 'presets', 'discovery', 'transport', 'display', 'device', 'identity', 'auth'];

async function applyDocument(doc, selected, applyNetwork) {
    const results = {};
    const sections = doc.sections || {};
    const want = new Set(selected || []);
    const writing = FILE_SECTIONS.some(key => want.has(key) && sections[key]);
    const changingNet = !!(applyNetwork && sections.network && sections.network.mode !== 'unknown');
    if (!writing && !changingNet)
        return { snapshot: null, results: {}, network: null, relogin: false, noop: true };

    fs.mkdirSync(BACKUP_DIR, { recursive: true });
    const snapSections = buildSections({
        network: true,
        networkStatus: await network.readStatus().catch(() => null),
        password: true,
    });
    if (snapSections.network == null) delete snapSections.network;
    const snap = buildDocument(snapSections);
    const snapName = `pre-restore-${fileStamp()}-${crypto.randomBytes(3).toString('hex')}.json`;
    if (!writeJson(path.join(BACKUP_DIR, snapName), snap))
        throw new Error('could not snapshot the current settings');
    pruneSnapshots();

    if (writing) await runPrivAsync(['stop-core'], 20000);
    try {
        if (want.has('outputs') && sections.outputs) {
            for (const [ch, item] of Object.entries(sections.outputs)) {
                mergeFile(`/etc/ndimon-dec${ch}-settings.json`, pick(item, OUTPUT_KEYS));
            }
            results.outputs = { ok: true };
        }
        if (want.has('presets') && sections.presets) {
            if (!writeJson('/etc/ndimon-presets.json', { presets: sections.presets.presets || [] }))
                throw new Error('could not write presets');
            results.presets = { ok: true };
        }
        if (want.has('discovery') && sections.discovery) {
            const ip = sections.discovery.NDIDisServIP || '';
            mergeFile('/etc/ndimon-find-settings.json', {
                NDIDisServIP: ip,
                NDIDisServ: ip ? 'NDIDisServEn' : 'NDIDisServDis',
            });
            if (sections.discovery.ndi_groups != null)
                mergeFile('/etc/ndi-group.json', { ndi_groups: sections.discovery.ndi_groups });
            if (sections.discovery.off_subnet_ips != null) {
                if (!writeJson('/etc/ndi-config.json', sections.discovery.off_subnet_ips))
                    throw new Error('could not write extra IPs');
            }
            results.discovery = { ok: true };
        }
        if (want.has('transport') && sections.transport) {
            mergeFile('/etc/ndimon-rx-settings.json', { Rxpm: sections.transport.Rxpm });
            results.transport = { ok: true };
        }
        if (want.has('display') && sections.display) {
            if (sections.display.splash) {
                const splash = Object.assign({}, sections.display.splash);
                delete splash.logo_path;
                mergeFile('/etc/ndimon-splash-settings.json', splash);
            }
            if (sections.display.osd)
                mergeFile('/etc/ndimon-osd-settings.json', {
                    enabled: !!sections.display.osd.enabled,
                    text: String(sections.display.osd.text || '').slice(0, 128),
                });
            const logo = writeLogo(sections.display);
            if (logo)
                mergeFile('/etc/ndimon-splash-settings.json', { logo_path: logo });
            else if (sections.display.logo_base64 === '')
                mergeFile('/etc/ndimon-splash-settings.json', { logo_path: '' });
            results.display = { ok: true };
        }
        if (want.has('device') && sections.device) {
            mergeFile('/etc/ndimon-device-settings.json', pick(sections.device, DEVICE_KEYS));
            results.device = { ok: true };
        }
        if (want.has('identity') && sections.identity) {
            const alias = String(sections.identity.ndi_recv_name || '').trim().slice(0, 64);
            mergeFile('/etc/ndimon-device-settings.json', { ndi_recv_name: alias });
            const host = hostnameFor(sections.identity);
            if (host) await runPrivAsync(['hostname', host], 10000);
            else results.identity = { ok: true, warning: 'device name is not a valid hostname; alias saved only' };
            if (!results.identity) results.identity = { ok: true };
        }
        if (want.has('auth') && sections.auth) {
            auth.importPasswordHash(sections.auth.salt, sections.auth.hash);
            results.auth = { ok: true, relogin: true };
        }
    } finally {
        if (writing) {
            try { await runPrivAsync(['start-core'], 25000); }
            catch (e) { results.services = { ok: false, error: (e.stderr || e.message || '').trim() }; }
        }
    }
    try { device.applySavedNtp(); } catch {}
    try { device.applySavedRebootSchedule(); } catch {}

    let pending = null;
    if (applyNetwork && sections.network && sections.network.mode !== 'unknown') {
        const current = await network.readStatus().catch(() => null);
        if (current && network.settingsEqual(current, sections.network)) {
            results.network = { ok: true, skipped: 'already matches' };
        } else if (current && current.applicable === false) {
            results.network = { ok: false, error: current.reason || 'not supported' };
        } else {
            try {
                pending = await network.stageAndApply(sections.network);
                results.network = { ok: true, pending: true };
            } catch (e) {
                results.network = { ok: false, error: (e.stderr || e.message || 'network change failed').toString().trim() };
            }
        }
    }
    return { snapshot: snapName, results, network: pending, relogin: !!(results.auth && results.auth.relogin) };
}

function pruneSnapshots() {
    let names = [];
    try { names = fs.readdirSync(BACKUP_DIR); } catch { return; }
    const files = names
        .filter(n => n.startsWith('pre-restore-') && n.endsWith('.json'))
        .map(n => ({ n, t: fs.statSync(path.join(BACKUP_DIR, n)).mtimeMs }))
        .sort((a, b) => b.t - a.t);
    for (const old of files.slice(MAX_SNAPSHOTS)) {
        try { fs.unlinkSync(path.join(BACKUP_DIR, old.n)); } catch {}
    }
}

router.post('/apply', async (req, res) => {
    try {
        const id = String((req.body && req.body.import_id) || '');
        if (!/^[0-9a-f]{32}$/.test(id))
            return res.status(400).json({ ok: false, error: 'unknown import' });
        const file = path.join(IMPORT_DIR, id + '.json');
        if (!fs.existsSync(file))
            return res.status(400).json({ ok: false, error: 'that restore expired; choose the file again' });
        const doc = readJson(file);
        const selected = Array.isArray(req.body.sections) ? req.body.sections.map(String) : [];
        const known = ['outputs', 'presets', 'discovery', 'transport', 'display', 'device', 'identity', 'auth'];
        if (selected.some(s => !known.includes(s)))
            return res.status(400).json({ ok: false, error: 'unknown section' });
        if (!selected.length && !req.body.apply_network)
            return res.status(400).json({ ok: false, error: 'choose at least one section' });
        const out = await applyDocument(doc, selected, !!req.body.apply_network);
        try { fs.unlinkSync(file); } catch {}
        res.json({ ok: true, ...out });
    } catch (e) {
        res.status(500).json({ ok: false, error: (e.stderr || e.message || 'restore failed').toString().trim() });
    }
});

router.get('/snapshots', (req, res) => {
    let names = [];
    try { names = fs.readdirSync(BACKUP_DIR); } catch { names = []; }
    const snapshots = names
        .filter(n => n.startsWith('pre-restore-') && n.endsWith('.json'))
        .map(n => {
            const st = fs.statSync(path.join(BACKUP_DIR, n));
            return { name: n, bytes: st.size, mtime: new Date(st.mtimeMs).toISOString() };
        })
        .sort((a, b) => (a.mtime < b.mtime ? 1 : -1));
    res.json({ snapshots });
});

router.post('/undo', async (req, res) => {
    try {
        const requested = String((req.body && req.body.name) || '');
        let names = [];
        try { names = fs.readdirSync(BACKUP_DIR); } catch {}
        const snaps = names.filter(n => n.startsWith('pre-restore-') && n.endsWith('.json')).sort().reverse();
        const name = requested || snaps[0];
        if (!name || !snaps.includes(name) || name.includes('..') || name.includes('/'))
            return res.status(404).json({ ok: false, error: 'no snapshot' });
        const doc = readJson(path.join(BACKUP_DIR, name));
        if (!doc || !doc.sections)
            return res.status(500).json({ ok: false, error: 'snapshot is unreadable' });
        const selected = Object.keys(doc.sections).filter(k => k !== 'network');
        let applyNetwork = false;
        if (doc.sections.network) {
            const current = await network.readStatus().catch(() => null);
            applyNetwork = !!(current && !network.settingsEqual(current, doc.sections.network));
        }
        const out = await applyDocument(doc, selected, applyNetwork);
        res.json({ ok: true, restored: name, ...out });
    } catch (e) {
        res.status(500).json({ ok: false, error: (e.stderr || e.message || 'undo failed').toString().trim() });
    }
});

module.exports = router;
