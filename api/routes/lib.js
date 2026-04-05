'use strict';
const fs           = require('fs');
const net          = require('net');
const path         = require('path');
const EventEmitter = require('events');

const IPC_SOCKET = '/tmp/ndi-decoder.sock';

// EventEmitter for events pushed from C++ decoder
const ipcEvents = new EventEmitter();

// Send command to C++ decoder via Unix socket
function sendIPC(cmd) {
    return new Promise((resolve) => {
        const client = net.createConnection(IPC_SOCKET, () => {
            client.write(JSON.stringify(cmd));
        });
        let data = '';
        client.on('data', d => { data += d; });
        client.on('end', () => {
            try { resolve(JSON.parse(data)); } catch { resolve({ ok: true }); }
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
        sock.write(JSON.stringify({ action: 'subscribe' }));
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
    try {
        fs.writeFileSync(tmp, JSON.stringify(obj));
        fs.renameSync(tmp, file);
    } catch (e) {
        console.error('[lib] writeJson', file, e.message);
        try { fs.unlinkSync(tmp); } catch {}
    }
}

function corsHeaders(res) {
    res.header('Access-Control-Allow-Origin', '*');
    res.header('Access-Control-Allow-Methods', 'GET,HEAD,OPTIONS,POST,PUT');
    res.header('Access-Control-Allow-Headers', 'Origin, X-Requested-With, Content-Type, Accept, Authorization');
    res.header('Cache-Control', 'no-store');
    res.header('Connection', 'close');
}

module.exports = { sendIPC, readJson, writeJson, corsHeaders, ipcEvents };
