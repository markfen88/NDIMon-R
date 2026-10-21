'use strict';
const fs = require('fs');

function readBackupAuth(file, files = fs) {
    let text;
    try { text = files.readFileSync(file, 'utf8'); }
    catch (error) { if (error.code === 'ENOENT') return null; throw error; }
    // A legacy zero-byte placeholder means the default password is still active.
    // Other corrupt data remains an error; do not relax general config reads.
    if (text === '') return null;
    try { return JSON.parse(text); }
    catch (error) { error.message = `Invalid backup auth file ${file}: ${error.message}`; throw error; }
}

function findConfiguredLogo(configured, files = fs) {
    if (typeof configured !== 'string' || !/^\/etc\/ndi-splash-logo\.(png|jpe?g)$/.test(configured)) return null;
    try {
        if (!files.lstatSync(configured).isFile()) return null;
        return { file: configured, ext: configured.endsWith('.png') ? 'png' : 'jpg' };
    } catch { return null; }
}

function restorableLogo(display) {
    // Buggy-era backups could include orphan image data while the logo was disabled.
    if (display.splash && Object.prototype.hasOwnProperty.call(display.splash, 'logo_path') && display.splash.logo_path === '')
        return { ...display, logo_base64: '', logo_ext: '' };
    return display;
}
module.exports = { readBackupAuth, findConfiguredLogo, restorableLogo };
