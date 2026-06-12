'use strict';
// Authentication for the NDIMon-R web UI / REST API.
//
// - Password hash (scrypt) stored in /etc/ndimon-auth.json
// - Default password "ndimon" until the user sets one (UI shows a warning)
// - Session tokens are random 256-bit values kept in memory and delivered as
//   an HttpOnly cookie; API clients may instead send "Authorization: Bearer".
// - No external dependencies — Node core crypto only.

const crypto = require('crypto');
const fs     = require('fs');

const AUTH_FILE        = '/etc/ndimon-auth.json';
const DEFAULT_PASSWORD = 'ndimon';
const SESSION_TTL_MS   = 7 * 24 * 3600 * 1000;   // 7 days
const COOKIE_NAME      = 'ndimon_session';

// token → expiry (ms epoch). In-memory: sessions reset on API restart,
// which is acceptable for an appliance (users just log in again).
const sessions = new Map();

// Login rate limiting: ip → [failure timestamps]
const loginFailures = new Map();
const MAX_FAILURES  = 5;
const FAILURE_WINDOW_MS = 60 * 1000;

function readAuthFile() {
    try {
        const j = JSON.parse(fs.readFileSync(AUTH_FILE, 'utf8'));
        if (j && j.salt && j.hash) return j;
    } catch {}
    return null;
}

function hashPassword(password, saltHex) {
    const salt = Buffer.from(saltHex, 'hex');
    return crypto.scryptSync(String(password), salt, 32).toString('hex');
}

function isDefaultPassword() {
    return readAuthFile() === null;
}

function verifyPassword(password) {
    const stored = readAuthFile();
    if (!stored) {
        // No password set yet — compare against the default
        const a = crypto.createHash('sha256').update(String(password)).digest();
        const b = crypto.createHash('sha256').update(DEFAULT_PASSWORD).digest();
        return crypto.timingSafeEqual(a, b);
    }
    const candidate = Buffer.from(hashPassword(password, stored.salt), 'hex');
    const expected  = Buffer.from(stored.hash, 'hex');
    return candidate.length === expected.length &&
           crypto.timingSafeEqual(candidate, expected);
}

function setPassword(password) {
    const salt = crypto.randomBytes(16).toString('hex');
    const data = {
        salt,
        hash: hashPassword(password, salt),
        updated: new Date().toISOString(),
    };
    const tmp = AUTH_FILE + '.tmp';
    fs.writeFileSync(tmp, JSON.stringify(data), { mode: 0o600 });
    fs.renameSync(tmp, AUTH_FILE);
}

function createSession() {
    const token = crypto.randomBytes(32).toString('hex');
    sessions.set(token, Date.now() + SESSION_TTL_MS);
    // Opportunistic prune
    if (sessions.size > 100) {
        const now = Date.now();
        for (const [t, exp] of sessions) if (exp < now) sessions.delete(t);
    }
    return token;
}

function isValidToken(token) {
    if (!token) return false;
    const exp = sessions.get(token);
    if (!exp) return false;
    if (exp < Date.now()) { sessions.delete(token); return false; }
    return true;
}

function parseCookies(req) {
    const out = {};
    const raw = req.headers.cookie;
    if (!raw) return out;
    for (const part of raw.split(';')) {
        const eq = part.indexOf('=');
        if (eq > 0) out[part.slice(0, eq).trim()] = part.slice(eq + 1).trim();
    }
    return out;
}

function tokenFromRequest(req) {
    const cookies = parseCookies(req);
    if (cookies[COOKIE_NAME]) return cookies[COOKIE_NAME];
    const auth = req.headers.authorization || '';
    if (auth.startsWith('Bearer ')) return auth.slice(7).trim();
    return null;
}

function rateLimited(ip) {
    const now = Date.now();
    const fails = (loginFailures.get(ip) || []).filter(t => now - t < FAILURE_WINDOW_MS);
    loginFailures.set(ip, fails);
    return fails.length >= MAX_FAILURES;
}

function recordFailure(ip) {
    const fails = loginFailures.get(ip) || [];
    fails.push(Date.now());
    loginFailures.set(ip, fails);
}

// Paths that work without a session (login + auth probe).
const EXEMPT = new Set(['/api/login', '/api/auth-status']);

function isLoopback(req) {
    const a = req.socket.remoteAddress || '';
    return a === '127.0.0.1' || a === '::1' || a === '::ffff:127.0.0.1';
}

// Express middleware protecting /v1/* and /api/* routes.
// Note: when mounted with a path prefix, req.path is relative to the mount
// point — use baseUrl + path to get the full request path.
function middleware(req, res, next) {
    if (EXEMPT.has((req.baseUrl || '') + req.path)) return next();
    // On-device tooling (the watchdog polls /api/health) is trusted: a local
    // user already controls the box. Remote requests must authenticate.
    if (isLoopback(req)) return next();
    if (isValidToken(tokenFromRequest(req))) return next();
    res.status(401).json({ ok: false, error: 'unauthorized' });
}

// Mounts /api/login, /api/logout, /api/auth-status, /api/password on app.
function installRoutes(app) {
    app.post('/api/login', (req, res) => {
        const ip = req.socket.remoteAddress || 'unknown';
        if (rateLimited(ip)) {
            return res.status(429).json({ ok: false, error: 'too many attempts, wait a minute' });
        }
        const password = (req.body && req.body.password) || '';
        if (!verifyPassword(password)) {
            recordFailure(ip);
            return res.status(401).json({ ok: false, error: 'wrong password' });
        }
        const token = createSession();
        res.setHeader('Set-Cookie',
            `${COOKIE_NAME}=${token}; HttpOnly; SameSite=Lax; Path=/; Max-Age=${SESSION_TTL_MS / 1000}`);
        res.json({ ok: true, token, default_password: isDefaultPassword() });
    });

    app.post('/api/logout', (req, res) => {
        const token = tokenFromRequest(req);
        if (token) sessions.delete(token);
        res.setHeader('Set-Cookie', `${COOKIE_NAME}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0`);
        res.json({ ok: true });
    });

    app.get('/api/auth-status', (req, res) => {
        res.json({
            auth_required: true,
            logged_in: isValidToken(tokenFromRequest(req)),
            default_password: isDefaultPassword(),
        });
    });

    // Change password (requires a valid session — enforced here, not by the
    // global middleware, because this route is mounted before it).
    app.post('/api/password', (req, res) => {
        if (!isValidToken(tokenFromRequest(req))) {
            return res.status(401).json({ ok: false, error: 'unauthorized' });
        }
        const { current, password } = req.body || {};
        if (!verifyPassword(current || '')) {
            return res.status(403).json({ ok: false, error: 'current password is wrong' });
        }
        if (!password || String(password).length < 6) {
            return res.status(400).json({ ok: false, error: 'new password must be at least 6 characters' });
        }
        try {
            setPassword(String(password));
            res.json({ ok: true });
        } catch (e) {
            res.status(500).json({ ok: false, error: e.message });
        }
    });
}

module.exports = { middleware, installRoutes, isValidToken, tokenFromRequest };
