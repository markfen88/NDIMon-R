'use strict';
const express    = require('express');
const bodyParser = require('body-parser');
const path       = require('path');

const app = express();
const PORT = process.env.PORT || 80;

// --- Middleware ---
app.use(bodyParser.json());
app.use(bodyParser.urlencoded({ extended: true }));
app.use(bodyParser.text());
app.use(express.static(path.join(__dirname, 'public')));

// CORS
app.use((req, res, next) => {
    res.header('Access-Control-Allow-Origin', '*');
    res.header('Access-Control-Allow-Methods', 'GET,HEAD,OPTIONS,POST,PUT');
    res.header('Access-Control-Allow-Headers', 'Origin, X-Requested-With, Content-Type, Accept, Authorization');
    next();
});

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

// --- New unified API routes ---
app.use('/api',             require('./routes/Status').router);

// Legacy routes (BirdDog compatibility without /v1 prefix)
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
});
