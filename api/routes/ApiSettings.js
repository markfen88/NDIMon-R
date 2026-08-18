'use strict';
const express = require('express');
const router  = express.Router();
const { readJson, writeJson, corsHeaders } = require('./lib');

const API_SETTINGS = '/etc/ndimon-about-settings.json';

router.use((req, res, next) => { corsHeaders(res); next(); });

router.get('/', (req, res) => {
    res.json(readJson(API_SETTINGS));
});

router.post('/', (req, res) => {
    const cfg = readJson(API_SETTINGS);
    Object.assign(cfg, req.body || {});
    writeJson(API_SETTINGS, cfg);
    res.json(cfg);
});

module.exports = router;
