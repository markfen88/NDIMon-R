'use strict';
const fs           = require('fs');
const net          = require('net');
const path         = require('path');
const { execFile } = require('child_process');
const EventEmitter = require('events');

const IPC_SOCKET = '/tmp/ndi-decoder.sock';
const MAX_OUTPUTS = 8;

function parseChannel(v, fallback = 1) {
    const n = parseInt(v, 10);
    if (!Number.isFinite(n) || n < 1 || n > MAX_OUTPUTS) return fallback;
    return n;
}

function parseOutputIndex(v, fallback = 0) {
    const n = parseInt(v, 10);
    if (!Number.isFinite(n) || n < 0 || n >= MAX_OUTPUTS) return fallback;
    return n;
}

// EventEmitter for events pushed from C++ decoder
const ipcEvents = new EventEmitter();

// Send command to C++ decoder via Unix socket
function sendIPC(cmd) {
    return new Promise((resolve) => {
        const client = net.createConnection(IPC_SOCKET, () => {
            client.write(JSON.stringify(cmd) + '\n');
        });
        let data = '';
        client.on('data', d => { data += d; });
        client.on('end', () => {
            try { resolve(JSON.parse(data)); }
            catch { resolve({ ok: false, error: 'bad_ipc' }); }
        });
        client.on('error', err => {
            const offline = err.code === 'ENOENT' || err.code === 'ECONNREFUSED';
            if (!offline) console.warn('[IPC] error:', err.message);
            resolve({ ok: false, error: offline ? 'decoder_offline' : err.message });
        });
        client.setTimeout(2000, () => {
            console.warn('[IPC] timeout:', cmd.action);
            client.destroy();
            resolve({ ok: false, error: 'timeout' });
        });
    });
}

// Persistent subscriber connection — C++ pushes events here
let _subSocket = null;
let _subBuf = '';
let _subReconnectTimer = null;

function connectSubscriber() {
    clearTimeout(_subReconnectTimer);
    const sock = net.createConnection(IPC_SOCKET);
    sock.on('connect', () => {
        _subSocket = sock;
        _subBuf = '';
        sock.write(JSON.stringify({ action: 'subscribe' }) + '\n');
        console.log('[IPC] event subscriber connected');
    });
    sock.on('data', chunk => {
        _subBuf += chunk.toString();
        const lines = _subBuf.split('\n');
        _subBuf = lines.pop();  // keep incomplete last line
        for (const line of lines) {
            if (!line.trim()) continue;
            try {
                const ev = JSON.parse(line);
                ipcEvents.emit(ev.type || 'unknown', ev);
            } catch {}
        }
    });
    sock.on('close', () => {
        _subSocket = null;
        _subReconnectTimer = setTimeout(connectSubscriber, 2000);
    });
    sock.on('error', () => {});  // handled by close
}
connectSubscriber();

function readJson(file) {
    try {
        return JSON.parse(fs.readFileSync(file, 'utf8'));
    } catch {
        return {};
    }
}

function writeJson(file, obj) {
    const tmp = file + '.tmp';
    let fd = null;
    try {
        fd = fs.openSync(tmp, 'w', 0o640);
        fs.writeSync(fd, JSON.stringify(obj));
        fs.fsyncSync(fd);
        fs.closeSync(fd);
        fd = null;
        fs.renameSync(tmp, file);
        // A rename is not durable until the directory entry itself is flushed.
        try {
            const dirfd = fs.openSync(path.dirname(file), 'r');
            try { fs.fsyncSync(dirfd); } finally { fs.closeSync(dirfd); }
        } catch (e) {
            console.warn('[lib] writeJson dir fsync', file, e.message);
        }
        return true;
    } catch (e) {
        if (fd != null) { try { fs.closeSync(fd); } catch {} }
        console.error('[lib] writeJson', file, e.message);
        try { fs.unlinkSync(tmp); } catch {}
        return false;
    }
}

function runPrivAsync(args, timeout) {
    return new Promise((resolve, reject) => {
        runPriv(args, { timeout: timeout || 15000 }, (err, stdout, stderr) => {
            if (err) {
                err.stdout = stdout;
                err.stderr = stderr;
                return reject(err);
            }
            resolve({ stdout: stdout || '', stderr: stderr || '' });
        });
    });
}

// Historically set CORS-allow-all headers; the API is now same-origin only
// (cross-origin access enabled CSRF against an authenticated appliance).
// Kept as the per-route response-header hook for cache control.
function corsHeaders(res) {
    res.header('Cache-Control', 'no-store');
    res.header('Connection', 'close');
}

// Root-only operations go through /usr/local/sbin/ndimon-priv (sudoers).
const PRIV = '/usr/local/sbin/ndimon-priv';

function runPriv(args, opts, cb) {
    if (typeof opts === 'function') { cb = opts; opts = {}; }
    const timeout = (opts && opts.timeout) || 10000;
    const done = cb || (() => {});
    const run = (bin, argv) => execFile(bin, argv, { timeout }, done);
    if (fs.existsSync(PRIV)) {
        if (process.getuid && process.getuid() === 0) return run(PRIV, args);
        return run('sudo', ['-n', PRIV, ...args]);
    }
    const cmd = args[0];
    const rest = args.slice(1);
    if (cmd === 'reboot') return run('reboot', []);
    if (cmd === 'hostname') return run('hostnamectl', ['set-hostname', rest[0] || 'ndimon']);
    if (cmd === 'restart-finder') {
        return execFile('systemctl', ['restart', 'ndimon-finder.service'], { timeout }, (err, stdout, stderr) => {
            if (!err) return done(null, stdout, stderr);
            execFile('systemctl', ['--user', 'restart', 'ndimon-finder.service'], { timeout }, done);
        });
    }
    if (cmd === 'restart-stack')
        return run('systemctl', ['restart', 'ndimon-r.service', 'ndimon-finder.service',
                                 'ndimon-api.service', 'ndimon-watchdog.service']);
    if (cmd === 'restart-service')
        return run('systemctl', ['restart', `${rest[0]}.service`]);
    if (cmd === 'set-ntp' || cmd === 'set-reboot-schedule' || cmd === 'start-update' ||
        cmd === 'stop-core' || cmd === 'start-core' ||
        cmd === 'net-status' || cmd === 'net-check-address' || cmd === 'net-apply' ||
        cmd === 'net-confirm' || cmd === 'net-rollback')
        return done(new Error('ndimon-priv not installed'));
    return done(new Error('unknown priv command'));
}

module.exports = { sendIPC, readJson, writeJson, corsHeaders, ipcEvents,
                   parseChannel, parseOutputIndex, MAX_OUTPUTS, runPriv, runPrivAsync };
