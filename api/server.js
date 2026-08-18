'use strict';
const express    = require('express');
const bodyParser = require('body-parser');
const path       = require('path');
const auth       = require('./auth');

const app = express();
const PORT = process.env.PORT || 80;

app.use((req, res, next) => {
    res.setHeader('X-Content-Type-Options', 'nosniff');
    res.setHeader('X-Frame-Options', 'DENY');
    res.setHeader('Referrer-Policy', 'no-referrer');
    next();
});

// Browser POST/PUT/PATCH/DELETE must match Host. SameSite=Lax already blocks
// cross-site cookie POST; this rejects forged Origin when a browser sends one.
app.use((req, res, next) => {
    if (req.method === 'GET' || req.method === 'HEAD' || req.method === 'OPTIONS') return next();
    const origin = req.headers.origin;
    if (!origin) return next();
    try {
        if (new URL(origin).host !== req.headers.host) {
            return res.status(403).json({ ok: false, error: 'csrf' });
        }
    } catch {
        return res.status(403).json({ ok: false, error: 'csrf' });
    }
    next();
});

// --- Middleware ---
// 4mb covers splash logo uploads without a 20mb JSON DoS surface.
app.use(bodyParser.json({ limit: '4mb' }));
app.use(bodyParser.urlencoded({ extended: true }));
app.use(bodyParser.text());
app.use(express.static(path.join(__dirname, 'public'), {
    setHeaders: (res, filePath) => {
        // Prevent stale HTML/JS from being served after deploys
        if (filePath.endsWith('.html') || filePath.endsWith('.js')) {
            res.setHeader('Cache-Control', 'no-store');
        }
    }
}));

// Intentionally no CORS headers: the API is same-origin only. Cross-origin
// browser access (and the CSRF exposure it created) is not supported.

// --- Auth ---
// Login/logout/auth-status/password live outside the guard; everything under
// /v1/* and /api/* (and the legacy prefixes) requires a session cookie or
// "Authorization: Bearer <token>" from POST /api/login.
auth.installRoutes(app);
app.use(['/v1', '/api', '/NDIDecode', '/NDIEncoder', '/NDIFinder',
         '/VideoOutput', '/DeviceSettings', '/System', '/AboutMe'],
        auth.middleware);

// --- Route modules ---
app.use('/v1/NDIDecode',    require('./routes/NDIDecode'));
app.use('/v1/NDIEncoder',   require('./routes/NDIEncode'));
app.use('/v1/NDIFinder',    require('./routes/NDIFinder'));
app.use('/v1/VideoOutput',  require('./routes/VideoOutput'));
app.use('/v1/DeviceSettings', require('./routes/DeviceSettings'));
app.use('/v1/ApiSettings',  require('./routes/ApiSettings'));
app.use('/v1/AboutMe',      require('./routes/AboutMe'));
app.use('/v1/System',       require('./routes/System'));

app.use('/v1/Splash',       require('./routes/Splash'));
app.use('/v1/Presets',      require('./routes/Presets'));

// --- New unified API routes ---
app.use('/api',             require('./routes/Status').router);

// Legacy routes (without /v1 prefix)
app.use('/NDIDecode',    require('./routes/NDIDecode'));
app.use('/NDIEncoder',   require('./routes/NDIEncode'));
app.use('/NDIFinder',    require('./routes/NDIFinder'));
app.use('/VideoOutput',  require('./routes/VideoOutput'));
app.use('/DeviceSettings', require('./routes/DeviceSettings'));
app.use('/System',       require('./routes/System'));
app.use('/AboutMe',      require('./routes/AboutMe'));

// Root redirect to web UI
app.get('/', (req, res) => {
    res.sendFile(path.join(__dirname, 'public', 'index.html'));
});

app.listen(PORT, () => {
    console.log(`[NDIMon-R] API running on port ${PORT}`);
    // Re-apply NTP to the OS clock after reboot (config lives in /etc).
    try { require('./routes/DeviceSettings').applySavedNtp(); } catch (e) {
        console.warn('[ntp] startup apply failed:', e.message);
    }
});
