'use strict';
const express = require('express');
const router  = express.Router();
const os      = require('os');
const fs      = require('fs');
const { corsHeaders } = require('./lib');

router.use((req, res, next) => { corsHeaders(res); next(); });

function readTrim(path) {
    try { return fs.readFileSync(path, 'utf8').replace(/\0/g, '').trim(); } catch { return ''; }
}

// Board model from the device tree (e.g. "Radxa ROCK 5B", "Raspberry Pi 5").
function boardModel() {
    return readTrim('/proc/device-tree/model') || `aarch64 (${os.arch()})`;
}

router.get('/', (req, res) => {
    const ifaces = os.networkInterfaces();
    let ip = '0.0.0.0';
    for (const [name, addrs] of Object.entries(ifaces)) {
        const a = addrs.find(a => !a.internal && a.family === 'IPv4');
        if (a) { ip = a.address; break; }
    }

    const firmware   = readTrim('/etc/ndimon-firmware-version') || '1.0.0';
    const buildDate  = readTrim('/etc/ndimon-build-date');
    // NDI SDK version is recorded by setup-deps.sh at install time.
    const ndiVersion = readTrim('/etc/ndimon-ndi-version') || '6.x';

    res.json({
        DeviceName: 'NDI Decoder',
        HostName: os.hostname(),
        IPAddress: ip,
        FirmwareVersion: firmware,
        Model: boardModel(),
        NDIVersion: ndiVersion,
        BuildDate: buildDate,
        chInfo: [{
            ChNum: 1,
            Mode: 'decode',
            Status: 'active',
            Stream: ''
        }]
    });
});

module.exports = router;
