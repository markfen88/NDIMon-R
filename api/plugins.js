'use strict';
const fs = require('fs');
const path = require('path');
const Module = require('module');
const { validateManifest } = require('./plugin-manifest');
const lib = require('./routes/lib');
let activeRegistry, loadError = null;
function loadModule(file) {
  // Plugin entries share the host's existing dependencies, without installing
  // a second Express tree in every plugin directory.
  const mod = new Module(file, module);
  mod.filename = file;
  mod.paths = [...Module._nodeModulePaths(path.dirname(file)), path.join(__dirname,'node_modules')];
  require.cache[file] = mod;
  try { mod._compile(fs.readFileSync(file,'utf8'),file); } catch (e) { delete require.cache[file]; throw e; }
  return mod.exports;
}
function loadPlugins(app, {root = '/opt/ndimon-r/plugins', stateFile = '/etc/ndimon-plugins.json', stateRoot = '/var/lib/ndimon/plugins', runPrivAsync = lib.runPrivAsync} = {}) {
  let entries = new Map(), stateError = null;
  function readState() {
    if (stateError) return {};
    try {
      const state = JSON.parse(fs.readFileSync(stateFile,'utf8'));
      if (!state || typeof state !== 'object' || Array.isArray(state)) throw new Error('plugin state must be an object');
      stateError = null; return state;
    } catch(e) {
      if (e.code === 'ENOENT') { stateError = null; return {}; }
      const message = `plugin state: ${e.message}`;
      if (stateError !== message) console.warn('[Plugins]',message);
      stateError = message; return {};
    }
  }
  function assertStateWritable() {
    readState();
    if (stateError) throw Object.assign(new Error('plugins_state_corrupt'),{status:409});
  }
  function writeState(state, message) {
    // Read and write are synchronous, so no API mutation can interleave them.
    assertStateWritable();
    if (!lib.writeJson(stateFile,state)) throw new Error(message);
  }
  function readManifest(name) {
    if (!/^[a-z][a-z0-9-]{1,31}$/.test(name)) throw new Error('invalid plugin name');
    const result = validateManifest(JSON.parse(fs.readFileSync(path.join(root,name,'plugin.json'),'utf8')));
    if (!result.ok) throw new Error(result.errors.join('; '));
    if (result.manifest.name !== name) throw new Error('plugin name mismatch');
    return result.manifest;
  }
  function ensureRouter(p) {
    if (typeof p.router === 'function') return;
    const pluginDir = path.join(root,p.name), stateDir = path.join(stateRoot,p.name);
    let audio;
    if (fs.existsSync(path.join(__dirname,'audio-settings.js'))) audio = require('./audio-settings');
    try {
      p.router = loadModule(path.join(pluginDir,p.manifest.routes))({...lib,pluginDir,stateDir,log:console,audio});
      if (typeof p.router !== 'function') throw new Error('routes must return a router');
    } catch(e) { delete p.router; p.state = 'error'; p.error = e.message; throw e; }
  }
  const registry = {
    get stateError() { return stateError; },
    list: () => [...entries.values()].map(({router,hook, ...entry}) => entry),
    get: name => registry.list().find(p => p.name === name),
    assertStateWritable,
    resetState() {
      if (!lib.writeJson(stateFile,{})) throw new Error('could not reset plugin state');
      stateError = null;
      registry.reload();
    },
    setEnabled(name, enabled) {
      assertStateWritable();
      const p = entries.get(name);
      if (!p) throw new Error('plugin not installed');
      if (p.state === 'error' || p.state === 'broken') throw new Error(p.error || p.state);
      if (enabled) ensureRouter(p);
      const state = readState();
      state[name] = {...state[name],enabled:!!enabled,version:p.version};
      delete state[name].pending_enable; delete state[name].enable_error;
      writeState(state,'could not write plugin state');
      p.state = enabled ? 'enabled' : 'installed';
      if (!enabled && p.router) { p.router.close?.(); delete p.router; }
      delete p.pending_enable; delete p.enable_error;
    },
    recordInstall(name, {pendingEnable = false, enabled = false} = {}) {
      assertStateWritable();
      // Installation metadata is readable without executing a router that may
      // require permissions granted only to the next API process.
      const manifest = readManifest(name);
      const state = readState();
      state[name] = {installed:true,enabled:!!enabled && !pendingEnable,
        ...(pendingEnable ? {pending_enable:true} : {}),version:manifest.version};
      writeState(state,'could not record plugin installation');
      return manifest;
    },
    setPendingEnable(name) { return registry.recordInstall(name,{pendingEnable:true}); },
    removeState(name) {
      assertStateWritable();
      const entry = entries.get(name);
      if (entry?.router) { entry.router.close?.(); delete entry.router; }
      const state = readState();
      delete state[name];
      writeState(state,'could not remove plugin state');
    },
    reload() {
      const state = readState();
      const previous = entries;
      entries = new Map();
      if (!fs.existsSync(root)) { for (const entry of previous.values()) entry.router?.close?.(); return registry; }
      for (const dirent of fs.readdirSync(root,{withFileTypes:true})) {
        if (!dirent.isDirectory() || !/^[a-z][a-z0-9-]{1,31}$/.test(dirent.name)) continue;
        const name = dirent.name, pluginDir = path.join(root,name);
        let p = {name, state:'error'};
        try {
          const manifest = readManifest(name);
          p = {name, version:manifest.version, title:manifest.title, icon:manifest.icon, manifest, state:state[name]?.enabled && !state[name]?.pending_enable ? 'enabled' : 'installed', ...(state[name]?.pending_enable ? {pending_enable:true} : {}), ...(state[name]?.enable_error ? {enable_error:state[name].enable_error} : {})};
          if (fs.existsSync(path.join(pluginDir,'.broken'))) {
            previous.get(name)?.router?.close?.();
            p.state = 'broken'; p.error = 'plugin operation failed; retry or force remove';
            entries.set(name,p); continue;
          }
          if (previous.get(name)?.version === manifest.version) p.router = previous.get(name).router;
          else { previous.get(name)?.router?.close?.(); for (const key of Object.keys(require.cache)) if (key.startsWith(pluginDir+path.sep)) delete require.cache[key]; }
          if (p.state === 'enabled') ensureRouter(p);
          else if (p.router) { p.router.close?.(); delete p.router; }
          if (manifest.backup && fs.existsSync(path.join(pluginDir,manifest.backup))) {
            p.hook = loadModule(path.join(pluginDir,manifest.backup));
            if (!p.hook || typeof p.hook !== 'object' || Array.isArray(p.hook) ||
                !['export','validate','import'].every(key=>typeof p.hook[key] === 'function')) {
              throw new Error('backup hook requires export, validate and import functions');
            }
          }
          if (fs.existsSync(path.join(pluginDir,'.broken'))) { p.state = 'broken'; p.error = 'plugin operation failed; retry or force remove'; }
        } catch(e) { p.state = 'error'; p.error = e.message; }
        entries.set(name,p);
      }
      for (const [name, entry] of previous) if (!entries.has(name)) entry.router?.close?.();
      return registry;
    },
    backupHooks: () => Object.fromEntries([...entries].filter(([,p]) => ['installed','enabled'].includes(p.state) && p.hook).map(([n,p]) => [n,p.hook])),
    statusProviders: () => Object.fromEntries([...entries].filter(([,p]) => p.state === 'enabled' && typeof p.router?.status === 'function').map(([n,p]) => [n,() => p.router.status()])),
    summary() {
      const providers = registry.statusProviders();
      return registry.list().map(({name,state,version,title,icon}) => {
        let status = null;
        try { status = providers[name]?.() ?? null; } catch(e) { status = {error:e.message}; }
        return {name,state,version,title,icon,status};
      });
    },
  };
  app.use('/v1/plugins/:name',(req,res,next) => {
    const p = entries.get(req.params.name);
    if (p?.state !== 'enabled') return next();
    lib.corsHeaders(res);
    p.router(req,res,next);
  });
  app.get('/plugins/:name/:file',(req,res,next) => {
    const p = entries.get(req.params.name);
    if (p?.state !== 'enabled' || req.params.file !== 'ui.html') return next();
    lib.corsHeaders(res);
    res.type('html').send(fs.readFileSync(path.join(root,p.name,p.manifest.ui),'utf8'));
  });
  activeRegistry = registry; loadError = null;
  try { registry.reload(); } catch(e) { loadError = e.message; console.warn('[Plugins] load',e.message); }
  // Only startup consumes pending work; ordinary reloads after an install must
  // leave it queued until the new process has its systemd groups and paths.
  registry.ready = (async () => {
    const names = Object.keys(readState());
    for (const name of names) {
      const state = readState(), saved = state[name];
      if (!saved?.pending_enable) continue;
      const p = entries.get(name), requestedVersion = saved.version;
      try {
        // At most one attempt: consume atomically before any external effect.
        delete saved.pending_enable; delete saved.enable_error; saved.enabled = false;
        writeState(state,'could not consume pending enable');
        if (p) { delete p.pending_enable; delete p.enable_error; }
        if (!p) throw new Error('plugin not installed');
        if (p.version !== requestedVersion) throw new Error('installed version differs from pending enable');
        if (!['installed','enabled'].includes(p.state)) throw new Error(p.error || p.state);
        ensureRouter(p);
        if (p.manifest.priv_commands.includes('enable')) await runPrivAsync(['plugin-exec',name,'enable'],300000);
        else for (const unit of p.manifest.services) await runPrivAsync(['service-ctl','start',unit],300000);
        registry.setEnabled(name,true);
      } catch(e) {
        let error = `pending enable failed: ${e.message || String(e)}`;
        try {
          const latest = readState();
          if (latest[name]?.version === requestedVersion) {
            delete latest[name].pending_enable;
            latest[name].enabled = false; latest[name].enable_error = error;
            writeState(latest,'could not record enable error');
          }
        } catch (recordError) { error += `; ${recordError.message}`; }
        if (p) { p.state = 'error'; p.error = error; p.enable_error = error; }
      }
    }
    return registry;
  })();
  return registry;
}
module.exports = {loadPlugins, getRegistry: () => activeRegistry, getStateError: () => activeRegistry?.stateError || loadError, reportLoadError: error => {loadError = error.message || String(error);} };
