'use strict';
const express = require('express');
const router  = express.Router();
const { sendIPC, corsHeaders } = require('./lib');

router.use((req, res, next) => { corsHeaders(res); next(); });

// GET /modes?output=0  — list available display modes for an output (0-based index)
router.get('/modes', async (req, res) => {
    const output = parseInt(req.query.output || 0, 10);
    const data = await sendIPC({ action: 'get_modes', output });
    res.json(data);
});

// GET /resolution?output=0  — get current resolution
router.get('/resolution', async (req, res) => {
    const output = parseInt(req.query.output || 0, 10);
    const data = await sendIPC({ action: 'get_modes', output });
    res.json(data.current || {});
});

// POST /resolution  — set resolution
// Body: { "width": 1920, "height": 1080, "refresh_hz": 59.94, "output": 0 }
router.post('/resolution', async (req, res) => {
    const { width, height, refresh, refresh_hz, output = 0 } = req.body || {};
    if (!width || !height) {
        return res.status(400).json({ ok: false, error: 'width and height required' });
    }
    const data = await sendIPC({
        action:     'set_resolution',
        width:      parseInt(width, 10),
        height:     parseInt(height, 10),
        refresh:    parseInt(refresh || 0, 10),
        refresh_hz: parseFloat(refresh_hz || 0),
        output:     parseInt(output, 10),
    });
    res.json(data);
});

// POST /scaleMode  — set display scale mode (letterbox/stretch/crop)
// Body: { "scale_mode": "letterbox", "output": 0 }
router.post('/scaleMode', async (req, res) => {
    const { scale_mode = 'letterbox', output = 0 } = req.body || {};
    const allowed = ['letterbox','stretch','crop'];
    if (!allowed.includes(scale_mode))
        return res.status(400).json({ ok: false, error: 'invalid scale_mode' });
    await sendIPC({ action: 'set_scale_mode', scale_mode, output: parseInt(output, 10) });
    res.json({ ok: true, scale_mode });
});

// GET /setresolution via query string (BirdDog-style legacy)
router.get('/setresolution', async (req, res) => {
    const { width, height, refresh, output = 0 } = req.query;
    if (!width || !height) {
        return res.json(await sendIPC({ action: 'get_modes', output: parseInt(output, 10) }));
    }
    const data = await sendIPC({
        action:  'set_resolution',
        width:   parseInt(width, 10),
        height:  parseInt(height, 10),
        refresh: parseInt(refresh || 0, 10),
        output:  parseInt(output, 10),
    });
    res.json(data);
});

module.exports = router;
