#!/usr/bin/env node
'use strict';
const fs = require('fs');
const path = require('path');
const {fetchFeed,downloadBundle} = require('./plugin-feed');
const {compareVersions} = require('./plugin-version');
async function main() {
  const [name,arch,destination] = process.argv.slice(2);
  if (!/^[a-z][a-z0-9-]{1,31}$/.test(name || '') || !['arm64','amd64'].includes(arch)) throw new Error('usage: plugin-feed-cli <name> <arm64|amd64>');
  let settings = {};
  try { settings = JSON.parse(fs.readFileSync('/etc/ndimon-device-settings.json','utf8')); } catch(e) {if(e.code!=='ENOENT') throw e;}
  const feeds = process.env.NDIMON_PLUGIN_FEEDS ? process.env.NDIMON_PLUGIN_FEEDS.split('\n').filter(Boolean) : settings.plugin_feeds || [];
  const results = await Promise.all(feeds.map(url=>fetchFeed(url,{arch,allowInsecure:settings.allow_insecure_feeds === true})));
  const entries = results.flatMap(r=>r.plugins || []).filter(p=>p.name===name).sort((a,b)=>compareVersions(b.version,a.version));
  if (!entries.length) throw new Error('plugin not found in feeds: '+results.map(r=>r.error).filter(Boolean).join('; '));
  const dir = destination || '/var/lib/ndimon/import';
  const file = await downloadBundle(entries[0],dir,{allowInsecure:settings.allow_insecure_feeds === true});
  const dest = path.join(dir,name+'.tar.gz');
  fs.renameSync(file,dest);
  process.stdout.write(dest+'\n');
}
main().catch(e=>{console.error(e.message);process.exitCode=1;});
