'use strict';
const express = require('express');
const router  = express.Router();
const { readJson, writeJson, corsHeaders } = require('./lib');

const API_SETTINGS = '/etc/ndimon-about-settings.json';
const ALLOWED = new Set(['GroupName', 'MACAddress', 'device_name', 'host_name']);

router.use((req, res, next) => { corsHeaders(res); next(); });

router.get('/', (req, res) => {
    res.json(readJson(API_SETTINGS));
});

router.post('/', (req, res) => {
    const cfg = readJson(API_SETTINGS);
    const body = req.body || {};
    for (const key of Object.keys(body)) {
        if (!ALLOWED.has(key)) continue;
        if (key === '__proto__' || key === 'constructor' || key === 'prototype') continue;
        cfg[key] = body[key];
    }
    writeJson(API_SETTINGS, cfg);
    res.json(cfg);
});

module.exports = router;
