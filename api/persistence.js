'use strict';
// /etc remains root-owned. Only the fixed appliance config allowlist is writable
// through the existing root helper, which validates payloads and replaces atomically.
const path = require('path');
const { execFileSync } = require('child_process');
const PRIV = '/usr/local/sbin/ndimon-priv';

function invoke(action, file, data) {
    if (path.dirname(file) !== '/etc') throw new Error('config must be directly under /etc');
    const args = [action, path.basename(file)];
    const root = process.getuid && process.getuid() === 0;
    const result = execFileSync(root ? PRIV : 'sudo', root ? args : ['-n', PRIV, ...args], {
        input: data, timeout: 10000, maxBuffer: 65536, stdio: ['pipe', 'pipe', 'pipe'],
    });
    return result;
}
function writeConfig(file, data) { invoke('write-config', file, data); }
function removeLogo(file) { invoke('remove-logo', file); }
module.exports = { writeConfig, removeLogo };
