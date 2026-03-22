'use strict';
const express = require('express');
const router  = express.Router();
const os      = require('os');
const fs      = require('fs');
const { readJson, corsHeaders } = require('./lib');

router.use((req, res, next) => { corsHeaders(res); next(); });

router.get('/', (req, res) => {
    const ifaces = os.networkInterfaces();
    let ip = '0.0.0.0';
    for (const [name, addrs] of Object.entries(ifaces)) {
        const a = addrs.find(a => !a.internal && a.family === 'IPv4');
        if (a) { ip = a.address; break; }
    }
    let firmware = '1.0.0';
    try { firmware = fs.readFileSync('/etc/birddog-firmware-version-common','utf8').trim(); } catch {}

    res.json({
        DeviceName: 'NDI Decoder',
        HostName: os.hostname(),
        IPAddress: ip,
        FirmwareVersion: firmware,
        Model: 'NDI Decoder (RK3588)',
        NDIVersion: '6.x',
        BuildDate: '2025-03-19',
        chInfo: [{
            ChNum: 1,
            Mode: 'decode',
            Status: 'active',
            Stream: ''
        }]
    });
});

module.exports = router;
