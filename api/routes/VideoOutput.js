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

// POST /resolution  — set resolution (or auto mode)
// Body: { "auto": true, "output": 0 }  — enable auto mode
// Body: { "width": 1920, "height": 1080, "refresh_hz": 59.94, "output": 0 }  — manual
router.post('/resolution', async (req, res) => {
    const body = req.body || {};
    const output = parseInt(body.output || 0, 10);

    if (body.auto) {
        await sendIPC({ action: 'auto_resolution', output });
        const data = await sendIPC({ action: 'get_modes', output });
        data.ok = true;
        return res.json(data);
    }

    const { width, height, refresh, refresh_hz } = body;
    if (!width || !height) {
        return res.status(400).json({ ok: false, error: 'width and height required' });
    }
    const data = await sendIPC({
        action:     'set_resolution',
        width:      parseInt(width, 10),
        height:     parseInt(height, 10),
        refresh:    parseInt(refresh || 0, 10),
        refresh_hz: parseFloat(refresh_hz || 0),
        output,
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

// POST /rotation  — set display rotation (0, 90, 180, 270 degrees)
// Body: { "degrees": 90, "output": 0 }
router.post('/rotation', async (req, res) => {
    const { degrees = 0, output = 0 } = req.body || {};
    const allowed = [0, 90, 180, 270];
    const deg = parseInt(degrees, 10);
    if (!allowed.includes(deg))
        return res.status(400).json({ ok: false, error: 'degrees must be 0, 90, 180, or 270' });
    await sendIPC({ action: 'set_rotation', rotation: deg, output: parseInt(output, 10) });
    res.json({ ok: true, rotation: deg });
});

// GET /setresolution via query string (legacy)
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
