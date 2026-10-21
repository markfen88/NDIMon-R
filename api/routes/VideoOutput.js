'use strict';
const express = require('express');
const router  = express.Router();
const { sendIPC, corsHeaders, parseOutputIndex } = require('./lib');

router.use((req, res, next) => { corsHeaders(res); next(); });

function resolutionResponse(res, data) {
    const status = data.error === 'timeout' || data.error === 'expired' ? 504
        : data.error === 'decoder_offline' ? 503 : data.error === 'resolution_busy' ? 409
        : data.ok === false ? 400 : 200;
    return res.status(status).json(data);
}

// GET /modes?output=0  — list available display modes for an output (0-based index)
router.get('/modes', async (req, res) => {
    const output = parseOutputIndex(req.query.output, 0);
    const data = await sendIPC({ action: 'get_modes', output });
    res.json(data);
});

// GET /resolution?output=0  — get current resolution
router.get('/resolution', async (req, res) => {
    const output = parseOutputIndex(req.query.output, 0);
    const data = await sendIPC({ action: 'get_modes', output });
    res.json(data.current || {});
});

// POST /resolution  — set resolution (or auto mode)
// Body: { "auto": true, "output": 0 }  — enable auto mode
// Body: { "width": 1920, "height": 1080, "refresh_hz": 59.94, "output": 0 }  — manual
router.post('/resolution', async (req, res) => {
    const body = req.body || {};
    const output = parseOutputIndex(body.output, 0);
    // The IPC client times out at 2s. Expire queued resolution commands first,
    // leaving a margin for a modeset that is already being applied.
    const deadline_monotonic_ms = Number(process.hrtime.bigint() / 1000000n) + 1500;

    if (body.auto) {
        const data = await sendIPC({ action: 'auto_resolution', output, deadline_monotonic_ms });
        return resolutionResponse(res, data);
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
        deadline_monotonic_ms,
    });
    resolutionResponse(res, data);
});

// POST /scaleMode  — set display scale mode (letterbox/stretch/crop)
// Body: { "scale_mode": "letterbox", "output": 0 }
router.post('/scaleMode', async (req, res) => {
    const { scale_mode = 'letterbox', output = 0 } = req.body || {};
    const allowed = ['letterbox','stretch','crop'];
    if (!allowed.includes(scale_mode))
        return res.status(400).json({ ok: false, error: 'invalid scale_mode' });
    await sendIPC({ action: 'set_scale_mode', scale_mode, output: parseOutputIndex(output, 0) });
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
    const result = await sendIPC({ action: 'set_rotation', rotation: deg, output: parseOutputIndex(output, 0) });
    const status = result.error === 'timeout' ? 504 : result.error === 'decoder_offline' ? 503 : result.error === 'rotation_busy' ? 409 : result.ok === false ? 400 : 200;
    res.status(status).json(result);
});

// GET /setresolution via query string (legacy)
router.get('/setresolution', (req, res) => {
    res.status(405).json({ ok: false, error: 'use POST /resolution' });
});

module.exports = router;
