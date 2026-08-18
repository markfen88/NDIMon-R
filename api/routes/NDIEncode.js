'use strict';
// Encode routes - stub implementation for API compatibility
const express = require('express');
const router  = express.Router();
const { corsHeaders } = require('./lib');

router.use((req, res, next) => { corsHeaders(res); next(); });

router.get('/encodesetup',     (req, res) => res.status(404).json({ status: 'Command not supported' }));
router.post('/encodesetup',    (req, res) => res.status(404).json({ status: 'Command not supported' }));
router.get('/encodeTransport', (req, res) => res.status(404).json({ status: 'Command not supported' }));
router.post('/encodeTransport',(req, res) => res.status(404).json({ status: 'Command not supported' }));

module.exports = router;
