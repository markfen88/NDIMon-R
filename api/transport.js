'use strict';
// Receive-transport default and one-time migration.
//
// Before the transport selection was wired to the SDK, the shipped "TCP" value
// was inert and the SDK ran its own default (RUDP). A saved "TCP" without the
// Rxpm_user_set marker is therefore the old template value, not a choice, and
// is moved to the default once. Choosing a transport in the UI sets the marker.
const { readJson, writeJson } = require('./routes/lib');

const RX_SETTINGS  = '/etc/ndimon-rx-settings.json';
const DEFAULT_RXPM = 'RUDP';
const KNOWN_RXPM   = ['RUDP', 'Multicast', 'M-TCP', 'UDP', 'TCP'];

// Returns true when the file was changed.
function migrateDefaultTransport(file = RX_SETTINGS) {
    const cfg = readJson(file);
    if (!cfg || typeof cfg !== 'object' || Object.keys(cfg).length === 0) return false;
    if (cfg.Rxpm === 'TCP' && cfg.Rxpm_user_set !== true) {
        cfg.Rxpm = DEFAULT_RXPM;
        writeJson(file, cfg);
        console.log(`[transport] migrated untouched default TCP -> ${DEFAULT_RXPM}`);
        return true;
    }
    return false;
}

module.exports = { RX_SETTINGS, DEFAULT_RXPM, KNOWN_RXPM, migrateDefaultTransport };
