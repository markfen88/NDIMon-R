'use strict';
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { Readable, Transform } = require('stream');
const { pipeline } = require('stream/promises');
const {compareVersions} = require('./plugin-version');
function httpURL(value,allowInsecure = false) {
  const url = new URL(value);
  if ((url.protocol !== 'https:' && !(allowInsecure && url.protocol === 'http:')) || url.username || url.password) throw new Error('HTTPS required for feed URLs');
  return url.href;
}
async function fetchURL(target,signal,allowInsecure) {
  let url = httpURL(target,allowInsecure);
  for (let redirects = 0; redirects < 6; redirects++) {
    const response = await fetch(url,{signal,redirect:'manual'});
    if (![301,302,303,307,308].includes(response.status)) return response;
    const next = response.headers.get('location');
    if (!next) throw new Error('redirect has no location');
    await response.body?.cancel();
    url = httpURL(new URL(next,url).href,allowInsecure);
  }
  throw new Error('too many redirects');
}
async function fetchFeed(url, {arch,timeoutMs = 10000,allowInsecure = false} = {}) {
  const controller = new AbortController();
  const timer = setTimeout(()=>controller.abort(new Error('feed timeout')),timeoutMs);
  try {
    const read = async target => {
      const res = await fetchURL(target,controller.signal,allowInsecure);
      if (!res.ok) throw new Error(`feed HTTP ${res.status}`);
      return res.text();
    };
    let doc;
    try { doc = JSON.parse(await read(url)); } catch(e) {
      if (e instanceof SyntaxError) throw new Error('feed is not JSON');
      throw e;
    }
    const plugins = [];
    if (Array.isArray(doc)) {
      for (const release of doc) {
        const assets = release.assets || [];
        const sums = assets.find(a=>a.name==='SHA256SUMS');
        if (!sums) continue;
        const checksums = new Map((await read(sums.browser_download_url)).split('\n').map(line=> {
          const m = /^([a-fA-F0-9]{64})\s+\*?(.+)$/.exec(line.trim());
          return m ? [m[2],m[1].toLowerCase()] : ['', ''];
        }));
        for (const asset of assets) {
          const m = /^([a-z][a-z0-9-]{1,31})-plugin_(.+)_(arm64|amd64)\.tar\.gz$/.exec(asset?.name);
          if (m && /^(?:[0-9]+:)?[0-9][A-Za-z0-9.+:~\-]*$/.test(m[2]) && m[3] === arch && checksums.has(asset.name)) plugins.push({name:m[1],version:m[2],asset_url:asset.browser_download_url,sha256:checksums.get(asset.name),size:asset.size || 0});
        }
      }
    } else if (doc && Array.isArray(doc.plugins)) {
      plugins.push(...doc.plugins.filter(p=>!p.arch || p.arch===arch).map(({arch:ignored,...entry})=>entry));
    } else throw new Error('invalid feed shape');
    for (const p of plugins) {
      if (!/^[a-z][a-z0-9-]{1,31}$/.test(p.name) || !/^(?:[0-9]+:)?[0-9][A-Za-z0-9.+:~\-]*$/.test(p.version) || !/^[a-fA-F0-9]{64}$/.test(p.sha256)) throw new Error('invalid feed entry');
      httpURL(p.asset_url,allowInsecure);
    }
    plugins.sort((a,b)=>compareVersions(b.version,a.version));
    return {available:true,plugins};
  } catch(e) { return {available:false,error:controller.signal.aborted ? 'feed timeout' : e.message,plugins:[]}; }
  finally { clearTimeout(timer); }
}
async function downloadBundle(entry,destDir,{allowInsecure = false} = {}) {
  if (!/^[a-z][a-z0-9-]{1,31}$/.test(entry.name) || !/^[a-fA-F0-9]{64}$/.test(entry.sha256)) throw new Error('invalid bundle entry');
  fs.mkdirSync(destDir,{recursive:true,mode:0o700});
  const file = path.join(destDir,`${entry.name}-${crypto.randomBytes(12).toString('hex')}.tar.gz`);
  const controller = new AbortController();
  const timer = setTimeout(()=>controller.abort(),300000);
  try {
    const res = await fetchURL(entry.asset_url,controller.signal,allowInsecure);
    if (!res.ok || !res.body) throw new Error(`download HTTP ${res.status}`);
    const hash = crypto.createHash('sha256');
    let size = 0;
    const check = new Transform({transform(chunk,encoding,callback) {
      size += chunk.length;
      if (size > 200*1024*1024) return callback(new Error('bundle exceeds 200mb'));
      hash.update(chunk); callback(null,chunk);
    }});
    await pipeline(Readable.fromWeb(res.body),check,fs.createWriteStream(file,{flags:'wx',mode:0o600}),{signal:controller.signal});
    if (hash.digest('hex') !== entry.sha256.toLowerCase()) throw new Error('SHA256 mismatch');
    return file;
  } catch(e) { fs.rmSync(file,{force:true}); throw e; }
  finally { clearTimeout(timer); }
}
module.exports = {fetchFeed,downloadBundle};
