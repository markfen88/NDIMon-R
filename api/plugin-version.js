'use strict';
// Debian verrevcmp: tilde, end/digits, letters, then punctuation.
function order(c) {
  if (c === '~') return -1;
  if (!c || /[0-9]/.test(c)) return 0;
  return /[A-Za-z]/.test(c) ? c.charCodeAt(0) : c.charCodeAt(0) + 256;
}
function part(a, b) {
  let i = 0, j = 0;
  while (i < a.length || j < b.length) {
    while ((a[i] && !/[0-9]/.test(a[i])) || (b[j] && !/[0-9]/.test(b[j]))) {
      const d = order(a[i]) - order(b[j]);
      if (d) return Math.sign(d);
      if (i < a.length) i++;
      if (j < b.length) j++;
    }
    while (a[i] === '0') i++;
    while (b[j] === '0') j++;
    let x = '', y = '';
    while (a[i] && /[0-9]/.test(a[i])) x += a[i++];
    while (b[j] && /[0-9]/.test(b[j])) y += b[j++];
    if (x.length !== y.length) return Math.sign(x.length - y.length);
    if (x !== y) return x < y ? -1 : 1;
  }
  return 0;
}
function split(v) {
  const m = /^(?:(\d+):)?(.*)$/.exec(v);
  const dash = m[2].lastIndexOf('-');
  return [BigInt(m[1] || 0), dash < 0 ? m[2] : m[2].slice(0,dash), dash < 0 ? '0' : m[2].slice(dash+1)];
}
function compareVersions(a, b) {
  const x = split(a), y = split(b);
  return x[0] !== y[0] ? (x[0] < y[0] ? -1 : 1) : part(x[1], y[1]) || part(x[2], y[2]);
}
module.exports = { compareVersions };
