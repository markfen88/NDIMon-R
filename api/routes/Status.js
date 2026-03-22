'use strict';
const express = require('express');
const router  = express.Router();
const os      = require('os');
const fs      = require('fs');
const { sendIPC, readJson, writeJson, corsHeaders, ipcEvents } = require('./lib');

router.use((req, res, next) => { corsHeaders(res); next(); });

// SSE clients set
const sseClients = new Set();

// Reconnect state keyed by output index: { timer, attempts }
const reconnectState = {};

// Grace period after a user-initiated connectTo (30s).
// Suppresses: reconnect loop from intermediate disconnects during source switch,
// and DS routing events overwriting the user's manual source selection.
const manualConnectTime = {};  // output → timestamp (ms)
const MANUAL_CONNECT_GRACE_MS = 30000;

function notifyManualConnect(output) {
    manualConnectTime[output] = Date.now();
}

function isInGracePeriod(output) {
    return !!(manualConnectTime[output] &&
              (Date.now() - manualConnectTime[output] < MANUAL_CONNECT_GRACE_MS));
}

function startReconnectLoop(output, src, ip) {
    stopReconnectLoop(output);
    reconnectState[output] = { attempts: 0, src, ip };
    _scheduleAttempt(output, src, ip, 5000);
}

function _scheduleAttempt(output, src, ip, delayMs) {
    if (!reconnectState[output]) return;
    console.log(`[Events] Reconnect in ${delayMs}ms for output ${output} → "${src}"`);
    reconnectState[output].timer = setTimeout(async () => {
        if (!reconnectState[output]) return;
        reconnectState[output].attempts++;
        const n = reconnectState[output].attempts;
        // Odd attempts: use saved IP (direct / discovery-server URL)
        // Even attempts: name-only mDNS (works without discovery server)
        const useIp = (n % 2 === 1) ? ip : '';
        console.log(`[Events] Reconnect attempt ${n} for output ${output} → "${src}" ip="${useIp}"`);
        await sendIPC({ action: 'connect', source_name: src, source_ip: useIp, output });
        // Backoff: 5 → 10 → 20 → 30 → 30 → 30 … seconds
        const next = Math.min(5000 * Math.pow(2, n - 1), 30000);
        _scheduleAttempt(output, src, ip, next);
    }, delayMs);
}

function stopReconnectLoop(output) {
    if (reconnectState[output]?.timer) clearTimeout(reconnectState[output].timer);
    delete reconnectState[output];
}

// CPU usage tracking (delta-based)
let lastCpuTimes = null;

function getCpuPercent() {
    const cpus = os.cpus();
    const curr = cpus.reduce((a, c) => ({
        user: a.user + c.times.user,
        nice: a.nice + c.times.nice,
        sys:  a.sys  + c.times.sys,
        idle: a.idle + c.times.idle,
        irq:  a.irq  + c.times.irq,
    }), { user: 0, nice: 0, sys: 0, idle: 0, irq: 0 });

    if (!lastCpuTimes) { lastCpuTimes = curr; return 0; }

    const d = {
        user: curr.user - lastCpuTimes.user,
        nice: curr.nice - lastCpuTimes.nice,
        sys:  curr.sys  - lastCpuTimes.sys,
        idle: curr.idle - lastCpuTimes.idle,
        irq:  curr.irq  - lastCpuTimes.irq,
    };
    const total = d.user + d.nice + d.sys + d.idle + d.irq;
    lastCpuTimes = curr;
    return total > 0 ? Math.round((total - d.idle) * 100 / total) : 0;
}

function getMemInfo() {
    const total = os.totalmem();
    const free  = os.freemem();
    const used  = total - free;
    return {
        used_mb:  Math.round(used  / 1048576),
        total_mb: Math.round(total / 1048576),
        percent:  Math.round(used * 100 / total),
    };
}

function getTemps() {
    const temps = {};
    try {
        const zones = fs.readdirSync('/sys/class/thermal');
        for (const z of zones) {
            if (!z.startsWith('thermal_zone')) continue;
            try {
                const t = parseInt(fs.readFileSync(`/sys/class/thermal/${z}/temp`, 'utf8').trim()) / 1000;
                let type = z;
                try { type = fs.readFileSync(`/sys/class/thermal/${z}/type`, 'utf8').trim(); } catch {}
                if (!(type in temps) || t > temps[type]) temps[type] = Math.round(t * 10) / 10;
            } catch {}
        }
    } catch {}
    return temps;
}

function getUptime() {
    const s = Math.floor(os.uptime());
    const d = Math.floor(s / 86400);
    const h = Math.floor((s % 86400) / 3600);
    const m = Math.floor((s % 3600) / 60);
    if (d > 0) return `${d}d ${h}h ${m}m`;
    if (h > 0) return `${h}h ${m}m`;
    return `${m}m`;
}

async function buildStatus() {
    const ipc     = await sendIPC({ action: 'status' });
    const sources = readJson('/etc/ndimon-sources.json');
    const decCfg  = readJson('/etc/ndimon-dec1-settings.json');
    const findCfg = readJson('/etc/ndimon-find-settings.json');

    const sourceList = sources.list
        ? Object.keys(sources.list).filter(k => k !== 'None')
        : [];

    // Flatten per-output fields so the UI can use outputs[].output_index for routing
    const outputs = (ipc.outputs || []).map(o => ({
        ch_num:       o.ch_num       || 1,
        output_index: o.output_index !== undefined ? o.output_index : (o.ch_num - 1),
        connector:    o.connector    || '',
        output_alias: o.output_alias || '',
        recv_name:    o.recv_name    || '',
        device_name:  o.device_name  || '',
        connected:    o.connected    || false,
        source_name:  o.source_name  || '',
        width:        o.width        || 0,
        height:       o.height       || 0,
        refresh:      o.refresh      || 0,
        refresh_hz:   o.refresh_hz   || 0,
        input_width:  o.input_width  || 0,
        input_height: o.input_height || 0,
        fps:          o.fps          || 0,
        codec:        o.codec        || 'none',
        scale_mode:   o.scale_mode   || 'letterbox',
        drm_ready:    o.drm_ready    || false,
    }));

    return {
        stream: {
            connected:       ipc.connected      || false,
            source_name:     ipc.source_name     || '',
            platform:        ipc.platform        || 'Unknown',
            total_frames:    ipc.total_frames    || 0,
            dropped_frames:  ipc.dropped_frames  || 0,
            display_width:      ipc.width           || 0,
            display_height:     ipc.height          || 0,
            display_refresh:    ipc.refresh         || 0,
            display_refresh_hz: ipc.refresh_hz      || 0,
            input_width:     ipc.input_width     || 0,
            input_height:    ipc.input_height    || 0,
            codec:           ipc.codec           || 'none',
            fps:             ipc.fps             || 0,
            audio_enabled:   ipc.audio_enabled   !== false,
            connector:       ipc.connector       || '',
            output_alias:    ipc.output_alias    || '',
            recv_name:       ipc.recv_name       || '',
            device_name:     ipc.device_name     || '',
        },
        system: {
            cpu_percent: getCpuPercent(),
            memory:      getMemInfo(),
            temps:       getTemps(),
            uptime:      getUptime(),
        },
        sources: sourceList,
        decoder: {
            audio:       decCfg.NDIAudio        || 'NDIAudioEn',
            screensaver: decCfg.ScreenSaverMode || 'BlackSS',
            tally:       decCfg.TallyMode       || 'TallyOff',
            color_space: decCfg.ColorSpace      || 'YUV',
        },
        discovery: {
            enabled:   findCfg.NDIDisServ   === 'NDIDisServEn',
            server_ip: findCfg.NDIDisServIP || '',
        },
        outputs,
    };
}

async function broadcastStatus() {
    if (sseClients.size === 0) return;
    try {
        const data = await buildStatus();
        const msg  = `data: ${JSON.stringify({ type: 'status', ...data })}\n\n`;
        for (const client of [...sseClients]) {
            try { client.write(msg); } catch { sseClients.delete(client); }
        }
    } catch {}
}

// Background SSE broadcast every 1.5s
setInterval(broadcastStatus, 1500);

// Handle routing events from DS via C++ push
// DS assignment is persisted to local config so reconnect-after-loss uses DS source.
// The NDI SDK handles the actual connection switch via allow_controlling=true —
// we do NOT send a connect IPC back, which would create a feedback loop:
//   DS routing → connect → routing ACK → DS routing → connect → SEGV.
ipcEvents.on('routing', async ev => {
    const { source, url: ip, output = 0 } = ev;
    const ch = output + 1;
    console.log(`[Events] DS routing: output=${output} source="${source}" ip="${ip}"`);

    // Don't let DS routing events overwrite a user's manual source selection
    // during the grace period after a connectTo. The user's choice takes precedence
    // — the DS will eventually sync once it receives our routing ACK.
    if (isInGracePeriod(output)) {
        console.log(`[Events] DS routing ignored (manual connect grace period active)`);
        broadcastStatus();
        return;
    }

    const cfg = readJson(`/etc/ndimon-dec${ch}-settings.json`);
    if (source && source !== 'None') {
        // Persist DS assignment — reconnect loop will use this source if it drops
        writeJson(`/etc/ndimon-dec${ch}-settings.json`, { ...cfg, SourceName: source, SourceIP: ip });
    } else {
        // DS cleared this receiver — clear saved source so reconnect loop stays idle
        writeJson(`/etc/ndimon-dec${ch}-settings.json`, { ...cfg, SourceName: '', SourceIP: '' });
    }
    // Do NOT sendIPC connect/disconnect — SDK already switched via allow_controlling=true
    broadcastStatus();
});

// Handle connection state changes from C++ push
ipcEvents.on('connection', async ev => {
    const { output = 0, connected, drm_ready } = ev;
    console.log(`[Events] Connection: output=${output} connected=${connected} drm_ready=${drm_ready}`);
    if (!connected) {
        // No display attached — don't start reconnect loop, cancel any existing one
        if (drm_ready === false) {
            stopReconnectLoop(output);
            broadcastStatus();
            return;
        }
        // Suppress reconnect loop during the grace period after an explicit
        // connectTo — the intermediate disconnect is expected during a source
        // switch and the connect IPC is already in flight.
        if (isInGracePeriod(output)) {
            broadcastStatus();
            return;
        }
        const ch = output + 1;
        const cfg = readJson(`/etc/ndimon-dec${ch}-settings.json`);
        const src = cfg.SourceName || '';
        const ip  = cfg.SourceIP  || '';
        if (src && src !== 'None') {
            // Don't reset backoff if already retrying this exact source —
            // probe timeout fires every 5s and would otherwise keep resetting to attempt 0.
            const existing = reconnectState[output];
            if (!existing || existing.src !== src) {
                startReconnectLoop(output, src, ip);
            }
        }
    } else {
        // Successful connection — clear grace period and stop reconnect loop
        delete manualConnectTime[output];
        stopReconnectLoop(output);
    }
    broadcastStatus();
});

// GET /api/status
router.get('/status', async (req, res) => {
    res.json(await buildStatus());
});

// GET /api/events  — Server-Sent Events for real-time updates
router.get('/events', (req, res) => {
    res.setHeader('Content-Type',  'text/event-stream');
    res.setHeader('Cache-Control', 'no-cache');
    res.setHeader('Connection',    'keep-alive');
    res.setHeader('X-Accel-Buffering', 'no');
    res.flushHeaders();
    res.write('data: {"type":"connected"}\n\n');

    sseClients.add(res);
    req.on('close', () => sseClients.delete(res));
});

module.exports = { router, stopReconnectLoop, notifyManualConnect };
