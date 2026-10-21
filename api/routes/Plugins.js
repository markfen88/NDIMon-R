'use strict';
const express = require('express');
const fs = require('fs');
const path = require('path');
const {Transform} = require('stream');
const {pipeline} = require('stream/promises');
const lib = require('./lib');
const feedClient = require('../plugin-feed');
const {compareVersions} = require('../plugin-version');
const {broadcastEvent} = require('../plugin-events');
const valid = name => typeof name === 'string' && /^[a-z][a-z0-9-]{1,31}$/.test(name);
module.exports = function pluginsRouter(registry,options = {}) {
  const router = express.Router();
  const {downloads = '/var/lib/ndimon/plugins/downloads',runPrivAsync = lib.runPrivAsync,readJson = lib.readJson,fetchFeed = feedClient.fetchFeed,downloadBundle = feedClient.downloadBundle,restartAfterInstall = true} = options;
  const getHostVersion = options.getHostVersion || (()=>{
    try { return fs.readFileSync('/etc/ndimon-firmware-version','utf8').trim(); }
    catch(e) { if (e.code !== 'ENOENT') throw e; return require('../package.json').version; }
  });
  const inspectBundle = options.inspectBundle || (async (name,file,version)=>JSON.parse((await runPrivAsync(['plugin-inspect',name,file,...(version ? ['--expect-version',version] : [])],300000)).stdout));
  const arch = process.arch === 'x64' ? 'amd64' : process.arch;
  let blockedJob = null, recovery = null, generation = 0;
  let audioRelease = null;
  let busy = false, lastFeedResult = {available:false,plugins:[],error:'no feeds configured'}, fetchedAt = 0, fetching;
  async function refresh(force = false) {
    if (fetching) return fetching;
    if (!force && fetchedAt && Date.now()-fetchedAt < 15*60*1000) return lastFeedResult;
    fetching = (async () => {
      const settings = readJson('/etc/ndimon-device-settings.json');
      const urls = settings.plugin_feeds || [];
      const results = await Promise.all(urls.map(url=>fetchFeed(url,{arch,allowInsecure:settings.allow_insecure_feeds === true})));
      lastFeedResult = {available:results.some(r=>r.available),plugins:results.flatMap(r=>r.plugins || []),error:results.filter(r=>!r.available).map(r=>r.error).join('; ') || (urls.length ? undefined : 'no feeds configured')};
      lastFeedResult.plugins.sort((a,b)=>compareVersions(b.version,a.version));
      fetchedAt = Date.now();
      return lastFeedResult;
    })();
    try { return await fetching; } finally { fetching = null; }
  }
  // No network operation is performed by the status loop.
  const timer = setInterval(()=>refresh().catch(e=>console.warn('[Plugins]',e.message)),15*60*1000);
  timer.unref();
  router.use((req,res,next)=>{lib.corsHeaders(res);next();});
  const wrap = fn => async (req,res,next) => {
    const mutation = !['GET','HEAD'].includes(req.method);
    const observedGeneration = generation;
    if (mutation && busy && blockedJob) {
      if (!recovery) {
        const unit = blockedJob;
        recovery = runPrivAsync(['plugin-job-status',unit],15000)
          .then(status=>status.stdout.trim() === 'inactive',()=>false)
          .finally(()=>{recovery = null;});
      }
      const inactive = await recovery;
      // Checking and acquiring are synchronous after the await. Another waiter
      // may already own a new generation even if its operation finished quickly.
      if (generation !== observedGeneration) return res.status(409).json({ok:false,error:'busy'});
      if (inactive) { blockedJob = null; busy = false; audioRelease?.(); audioRelease=null; }
    }
    if (mutation && busy) return res.status(409).json({ok:false,error:'busy'});
    if (mutation) { try { audioRelease=require('../audio-write-gate').acquirePlugin(); } catch(e) { return res.status(409).json({ok:false,error:e.code||'audio_busy'}); } busy = true; generation++; }
    try { await fn(req,res); }
    catch(e) { res.status(e.status || 500).json({ok:false,error:e.message}); }
    finally { if (mutation) { busy = !!blockedJob; if(!busy){audioRelease?.();audioRelease=null;} } }
  };
  const bad = (message,status=400) => {throw Object.assign(new Error(message),{status});};
  router.get('/',wrap(async(req,res)=>res.json({plugins:registry.list(),plugins_state_error:registry.stateError || null,feed:lastFeedResult,arch})));
  router.post('/state/reset',wrap(async(req,res)=>{
    registry.resetState();
    res.json({ok:true});
  }));
  router.get('/feed',wrap(async(req,res)=>res.json(await refresh())));
  router.post('/feed/refresh',wrap(async(req,res)=>res.json(await refresh(true))));
  router.post('/upload',wrap(async(req,res)=>{
    const name = req.query.name;
    if (!valid(name)) return bad('invalid plugin name');
    if (!/^application\/(octet-stream|gzip)(?:;|$)/i.test(req.headers['content-type'] || '')) return bad('raw archive required');
    fs.mkdirSync(downloads,{recursive:true,mode:0o700});
    const dest = path.join(downloads,name+'.tar.gz');
    const tempDir = fs.mkdtempSync(path.join(downloads,'.upload-'+name+'-'));
    const temp = path.join(tempDir,'archive');
    let size = 0;
    try {
      const limit = new Transform({transform(chunk,encoding,callback){size+=chunk.length;callback(size>200*1024*1024 ? Object.assign(new Error('archive exceeds 200mb'),{status:413}) : null,chunk);}});
      await pipeline(req,limit,fs.createWriteStream(temp,{flags:'wx',mode:0o600}));
      fs.renameSync(temp,dest);
      res.json({ok:true,name});
    } finally {fs.rmSync(tempDir,{recursive:true,force:true});}
  }));
  async function changeEnabled(name,enabled) {
    registry.assertStateWritable?.();
    const plugin = registry.get(name);
    if (['broken','error'].includes(plugin.state)) return bad(plugin.error || plugin.state,409);
    const action = enabled ? 'enable' : 'disable';
    let result = {stdout:'',stderr:''};
    if (plugin.manifest.priv_commands.includes(action)) result = await runPrivAsync(['plugin-exec',name,action],300000);
    else for (const unit of plugin.manifest.services) await runPrivAsync(['service-ctl',enabled?'start':'stop',unit],300000);
    registry.setEnabled(name,enabled);
    return result;
  }
  async function runPluginJob(args) {
    let result, error;
    try { result = await runPrivAsync(args,300000); } catch(e) { error = e; }
    const marker = /^NDIMON_PLUGIN_JOB=(ndimon-plugin-[a-z][a-z0-9-]{1,31}-[0-9]+\.service)$/m.exec(result?.stdout || error?.stdout || '');
    const unit = marker?.[1];
    if (unit) {
      try {
        const timedOut = error && (error.killed || error.code === 'ETIMEDOUT');
        if (timedOut) await runPrivAsync(['plugin-job-cancel',unit],15000);
        let inactive = false;
        for (let attempt = 0; attempt < 20; attempt++) {
          const status = await runPrivAsync(['plugin-job-status',unit],15000);
          if (status.stdout.trim() === 'inactive') { inactive = true; break; }
          if (status.stdout.trim() !== 'active') throw new Error('unknown plugin job state');
          await new Promise(resolve=>setTimeout(resolve,250));
        }
        if (!inactive) throw new Error('plugin job has not terminated');
      } catch(e) {
        blockedJob = unit;
        throw error || e;
      }
    } else if (error && (error.killed || error.code === 'ETIMEDOUT')) {
      // A timeout without a job marker cannot establish whether root work ran.
      // Keep mutations blocked; the operator must investigate rather than overlap.
      blockedJob = 'unknown';
    }
    if (error) throw error;
    return result;
  }
  async function install(req,res,name,updating = false) {
    registry.assertStateWritable?.();
    if (!valid(name)) return bad('invalid plugin name');
    const body = req.body || {};
    const wasEnabled = registry.get(name)?.state === 'enabled';
    let file, expectedVersion;
    if (updating || body.source==='feed') {
      const feed = await refresh();
      const entry = feed.plugins.find(p=>p.name===name && (!body.version || p.version===body.version));
      if (!entry) return bad('plugin not in feed',updating ? 409 : 404);
      const installed = registry.get(name);
      if (installed && compareVersions(entry.version,installed.version)<0 && body.force !== true) return bad('downgrade requires force',409);
      if (updating && (!installed || compareVersions(entry.version,installed.version)===0)) return bad('no newer version',409);
      expectedVersion = entry.version;
      broadcastEvent('plugin_progress',{name,step:'download'});
      file = await downloadBundle(entry,downloads,{allowInsecure:readJson('/etc/ndimon-device-settings.json').allow_insecure_feeds === true});
    } else if (body.source==='upload') {
      if (body.path) return bad('caller paths are not accepted');
      file = path.join(downloads,name+'.tar.gz');
      try {
        if (!fs.lstatSync(file).isFile() || fs.realpathSync(path.dirname(file)) !== fs.realpathSync(downloads)) return bad('invalid upload');
      } catch {return bad('upload missing');}
    } else return bad('invalid source');
    try {
      broadcastEvent('plugin_progress',{name,step:'install'});
      const manifest = await inspectBundle(name,file,expectedVersion);
      if (compareVersions(getHostVersion(),manifest.min_ndimon)<0) return bad('host_too_old',409);
      if (expectedVersion && manifest.version !== expectedVersion) return bad('bundle version mismatch',409);
      const installed = registry.get(name);
      if (installed && compareVersions(manifest.version,installed.version)<0 && body.force !== true) return bad('downgrade requires force',409);
      const result = await runPluginJob(['plugin-install',name,file,'--expect-version',manifest.version,...(body.force === true ? ['--force'] : [])]);
      // Root install changed the unit drop-in. Arrange restart before any
      // registry/state operation that could fail in this older API process.
      if (restartAfterInstall) res.once('finish',()=>{const restart=setTimeout(()=>runPrivAsync(['restart-api']).catch(e=>console.warn('[Plugins] restart',e.message)),500);restart.unref();});
      registry.recordInstall(name,{pendingEnable:body.enable === true,enabled:wasEnabled});
      let loadError = null;
      try {
        registry.reload();
        const plugin = registry.get(name);
        if (!plugin || ['error','broken'].includes(plugin.state)) loadError = plugin?.error || 'plugin could not be loaded';
      } catch(e) { loadError = e.message || String(e); }
      broadcastEvent('plugin_progress',{name,step:'done'});
      res.json({ok:true,state:'installed',restart_pending:true,load_error:loadError,
        pending_enable:body.enable === true,log:result.stdout+ (result.stderr || '')});
    } catch(e) {
      registry.reload();
      broadcastEvent('plugin_progress',{name,step:'failed',error:e.message});
      res.status(e.status || 500).json({ok:false,state:registry.get(name)?.state || 'broken',error:e.message,log:e.stderr || e.stdout || e.message});
    } finally {fs.rmSync(file,{force:true});}
  }
  router.post('/install',wrap((req,res)=>install(req,res,req.body?.name)));
  router.post('/:name/update',wrap((req,res)=>install(req,res,req.params.name,true)));
  for (const action of ['enable','disable','uninstall']) router.post('/:name/'+action,wrap(async(req,res)=>{
    const name = req.params.name;
    if (!valid(name)) return bad('invalid plugin name');
    const plugin = registry.get(name);
    if (!plugin) return bad('plugin not installed',404);
    try {
      let result = {stdout:'',stderr:''};
      if (action==='uninstall') {
        registry.assertStateWritable?.();
        const option = req.body?.force ? ['force'] : req.body?.purge ? ['purge'] : [];
        broadcastEvent('plugin_progress',{name,step:'uninstall'});
        result = await runPluginJob(['plugin-uninstall',name,...option]);
        registry.removeState(name);
        registry.reload();
        if (restartAfterInstall) res.once('finish',()=>{const restart=setTimeout(()=>runPrivAsync(['restart-api']).catch(e=>console.warn('[Plugins] restart',e.message)),500);restart.unref();});
      } else {
        result = await changeEnabled(name,action==='enable');
      }
      res.json({ok:true,state:action==='uninstall'?'removed':registry.get(name).state,log:result.stdout+(result.stderr || '')});
    } catch(e) {
      registry.reload();
      res.status(e.status || 500).json({ok:false,state:registry.get(name)?.state || 'broken',error:e.message,log:e.stderr || e.message});
    }
  }));
  return router;
};
