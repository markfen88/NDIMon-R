'use strict';
const net = require('net');
// One IPv4 endpoint. Discovery lists split/trim their items before this check.
function isNdiEndpoint(value) {
    if (typeof value !== 'string') return false;
    const match = /^(\d{1,3}(?:\.\d{1,3}){3})(?::([1-9]\d{0,4}))?$/.exec(value);
    return !!match && match[0] === value && net.isIP(match[1]) === 4 && (!match[2] || Number(match[2]) <= 65535);
}
// Resolve explicit user/preset selections. Automatic recovery deliberately
// keeps a separate name-only path so a stale discovery entry can be bypassed.
function resolveNdiEndpoint(source, explicit, discovery) {
    if (typeof source !== 'string' || !source || source === 'None') return '';
    if (isNdiEndpoint(explicit)) return explicit;
    const discovered = discovery?.list?.[source];
    return isNdiEndpoint(discovered) ? discovered : '';
}
module.exports = { isNdiEndpoint, resolveNdiEndpoint };
