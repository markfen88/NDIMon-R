import { expose } from './legacy-globals';
import { store } from './store';
import { confirmedCommand, nativeCommand, resultText } from './native-page';
import { liveView } from './live';
import type { DomainObserved } from './generated/DomainObserved';
import type { StatusEvent } from './legacy-types';

// ══ Plugins ═════════════════════════════════════════════════════
// The plugin host: the plugins page and the API its UI fragments run against.
// Fragment cleanup and mount invalidation are unchanged. Enable, disable, remove
// and reset are typed commands whose lifecycle job is followed through the store's
// observations. Install, update and upload keep their routes because the device
// prepares the archive (feed download or uploaded file) there.
interface PluginEntry {
  name: string; title?: string; version?: string; state: string; error?: string; migration_warning?: string; icon?: string;
}
interface PluginListing {
  plugins: PluginEntry[];
  feed: { available?: boolean; error?: string; plugins?: PluginEntry[] };
  plugins_state_error?: string | null;
  error?: string;
}
interface Fragment {
  pane: HTMLElement; tab: HTMLElement; version?: string; cleanup?: (() => void) | undefined;
}
type PluginStatusListener = (status: StatusEvent) => void;

export const pluginStatusListeners = new Set<PluginStatusListener>();
export const pluginFragments = new Map<string, Fragment>();
export let pluginListing: PluginListing = { plugins: [], feed: { plugins: [] } };
export let pluginUIBusy = false;
const initializers = () => window as unknown as Record<string, unknown>;

export let apiGet = function<T = LegacyResultLike>(url: string): Promise<T> { return api<T>(url); };
type LegacyResultLike = Awaited<ReturnType<typeof api>>;

export let reconcilePluginFragments = function(plugins: PluginEntry[]) {
  const enabled = plugins.filter(plugin=>plugin.state === 'enabled');
  for (const [name,fragment] of pluginFragments) {
    if (enabled.some(plugin=>plugin.name === name && plugin.version === fragment.version)) continue;
    // Invalidate the mount token before invoking plugin cleanup or removing DOM.
    pluginFragments.delete(name);
    delete initializers()['NDIMON_PLUGIN_INIT_' + name];
    try { fragment.cleanup?.(); } catch (e) { console.warn('[Plugin] cleanup',e); }
    fragment.cleanup = undefined;
    fragment.pane.remove(); fragment.tab.remove();
  }
};

export let renderPluginStateError = function() {
  const banner = document.getElementById('plugin-state-warning');
  if (banner) banner.style.display = pluginListing.plugins_state_error ? '' : 'none';
  const button = document.getElementById('plugin-state-reset') as HTMLButtonElement | null;
  if (button) button.disabled = pluginUIBusy;
};

// Wait until the plugin lifecycle job the last command started has finished, as the
// store observes it. The device keeps observing after this page gives up waiting.
const settledPluginJob = (): DomainObserved | undefined => {
  const observed = store.observed?.domains.plugins;
  return observed?.status === 'applying' ? undefined : observed ?? { status: 'in_sync', applied_revision: null, detail: null };
};
export let waitForPluginJob = async function(timeoutMs = 310000): Promise<DomainObserved | undefined> {
  try { await store.refreshObserved(); } catch {}
  const now = settledPluginJob();
  if (now) return now;
  return new Promise(resolve => {
    const finish = (value: DomainObserved | undefined) => { unsubscribe(); clearTimeout(timer); resolve(value); };
    const unsubscribe = store.subscribe(() => { const settled = settledPluginJob(); if (settled) finish(settled); });
    const timer = setTimeout(() => finish(undefined), timeoutMs);
  });
};

export let resetPluginState = async function() {
  if (pluginUIBusy) return;
  if (!await confirmDialog('Reset plugin state','Clear saved plugin enable flags? Installed plugins and their settings are kept. All plugins will be disabled.',{danger:true,label:'Reset state'})) return;
  pluginUIBusy = true; renderPluginStateError();
  try {
    const result = await confirmedCommand('reset_plugin_state',{title:'Reset plugin state',label:'Reset state',danger:true});
    if (!result.ok) throw new Error(resultText(result, 'Could not reset plugin state'));
    pluginListing.plugins_state_error = null;
    toast('Plugin state reset','success');
  } catch (e) { toast((e as Error).message,'danger'); }
  finally { pluginUIBusy = false; renderPluginStateError(); await loadPlugins(); }
};

export let notifyPluginStatus = function(status: StatusEvent) {
  if (Object.prototype.hasOwnProperty.call(status,'plugins_state_error')) {
    pluginListing.plugins_state_error = status.plugins_state_error;
    renderPluginStateError();
  }
  if (Array.isArray(status.plugins)) {
    const previous = new Map(pluginListing.plugins.map(plugin=>[plugin.name,plugin]));
    pluginListing.plugins = (status.plugins as PluginEntry[]).map(plugin=>({...previous.get(plugin.name),...plugin}));
    reconcilePluginFragments(pluginListing.plugins);
    renderPluginList();
  }
  for (const callback of pluginStatusListeners) {
    try { callback(status); } catch (e) { console.warn('[Plugin] status', e); }
  }
};

// The documented API of plugin fragments. `state` is the host's current view of the
// device in the flat shape fragments have always seen, computed from the store.
if (typeof window !== 'undefined') (window as unknown as { NDIMON: unknown }).NDIMON = {
  get: (url: string) => apiGet(url), post, toast,
  onStatus(callback: PluginStatusListener) { pluginStatusListeners.add(callback); return () => pluginStatusListeners.delete(callback); },
  get state() { return { ...(store.status.decoder as object | undefined), ...liveView() }; },
};

export let loadPlugins = async function() {
  try {
    pluginListing = await apiGet<PluginListing>('/v1/plugins');
    if (!pluginListing.plugins) throw new Error(pluginListing.error || 'Could not load plugins');
    renderPluginList();
    const feed = await apiGet<PluginListing['feed']>('/v1/plugins/feed');
    pluginListing.feed = feed;
    renderPluginList();
  } catch (e) { toast((e as Error).message, 'danger'); }
};

export let renderPluginList = function() {
  renderPluginStateError();
  const container = document.getElementById('plugin-list');
  if (!container) return;
  container.replaceChildren();
  const feed = pluginListing.feed || {};
  setText('plugin-feed-status', feed.available ? 'Feed available' : (feed.error || 'No feed configured; upload an archive to install.'));
  const entries = new Map((pluginListing.plugins || []).map(plugin => [plugin.name, plugin]));
  for (const entry of feed.plugins || []) if (!entries.has(entry.name)) entries.set(entry.name, { ...entry, title: entry.name, state: 'available' });
  if (!entries.size) container.textContent = 'No plugins installed.';
  for (const plugin of entries.values()) {
    const row = document.createElement('div');
    row.className = 'd-flex align-items-center gap-2 flex-wrap border-bottom py-2';
    row.dataset.plugin = plugin.name;
    const label = document.createElement('span');
    label.className = 'me-auto';
    label.textContent = `${plugin.title || plugin.name} ${plugin.version || ''} · ${plugin.state}${plugin.error ? ' · ' + plugin.error : plugin.migration_warning ? ' · ' + plugin.migration_warning : ''}`;
    row.appendChild(label);
    const actions = plugin.state === 'available' ? [['install','Install']] :
      plugin.state === 'broken' ? [['uninstall','Retry remove'],['force','Force remove']] :
      plugin.state === 'error' ? [['uninstall','Remove'],['force','Force remove']] :
      [[plugin.state === 'enabled' ? 'disable' : 'enable',plugin.state === 'enabled' ? 'Disable' : 'Enable'],['update','Update'],['uninstall','Remove']];
    for (const [action,text] of actions) {
      const button = document.createElement('button');
      button.className = 'btn btn-sm btn-outline-secondary';
      button.textContent = text; button.disabled = pluginUIBusy;
      button.onclick = () => runPluginAction(plugin.name, action);
      row.appendChild(button);
    }
    container.appendChild(row);
  }
  const enabled = pluginListing.plugins.filter(p => p.state === 'enabled');
  reconcilePluginFragments(pluginListing.plugins);
  for (const plugin of enabled) {
    if (pluginFragments.has(plugin.name)) continue;
    const pane = document.createElement('div');
    pane.id = 'plugin-pane-' + plugin.name; pane.className = 'card card-body'; pane.setAttribute('role','tabpanel');
    const tab = document.createElement('button');
    tab.className = 'nav-link'; tab.type = 'button'; tab.textContent = plugin.title ?? '';
    tab.setAttribute('role','tab'); tab.setAttribute('aria-controls',pane.id);
    tab.onclick = () => selectPluginTab(plugin.name);
    document.getElementById('plugin-tabs')!.appendChild(tab);
    document.getElementById('plugin-panes')!.appendChild(pane);
    pluginFragments.set(plugin.name,{pane,tab,version:plugin.version});
    mountPluginFragment(plugin.name).catch(e => { pane.textContent = e.message; });
  }
  const selected = [...pluginFragments].find(([,f]) => f.tab.classList.contains('active'))?.[0];
  if (enabled.length) selectPluginTab(selected || enabled[0].name);
};

export let selectPluginTab = function(name: string) {
  for (const [key,{pane,tab}] of pluginFragments) {
    pane.hidden = key !== name;
    tab.classList.toggle('active',key === name);
    tab.setAttribute('aria-selected',String(key === name));
  }
};

export let mountPluginFragment = async function(name: string) {
  const fragment = pluginFragments.get(name);
  if (!fragment) return;
  const current = () => pluginFragments.get(name) === fragment;
  const response = await fetch(`/plugins/${encodeURIComponent(name)}/ui.html`).then(handle401);
  if (!current()) return;
  if (!response.ok) throw new Error('Could not load plugin page');
  const html = await response.text();
  if (!current()) return;
  fragment.pane.innerHTML = html;
  for (const script of [...fragment.pane.querySelectorAll('script')]) {
    const executable = document.createElement('script');
    for (const attr of script.attributes) executable.setAttribute(attr.name,attr.value);
    executable.textContent = script.textContent;
    const loaded = script.src ? new Promise((resolve,reject) => { executable.onload = resolve; executable.onerror = reject; }) : null;
    script.replaceWith(executable);
    if (loaded) {
      await loaded;
      if (!current()) return;
    }
  }
  const init = initializers()['NDIMON_PLUGIN_INIT_' + name];
  if (typeof init === 'function') {
    const cleanup = await init(fragment.pane);
    if (!current()) {
      if (typeof cleanup === 'function') cleanup();
      return;
    }
    if (typeof cleanup === 'function') fragment.cleanup = cleanup;
  }
};

export let refreshPluginFeed = async function() {
  const result = await post('/v1/plugins/feed/refresh',{});
  if (result.ok === false) return toast(result.error!,'danger');
  await loadPlugins();
};

export let runPluginAction = async function(name: string, action: string) {
  if (pluginUIBusy) return;
  if (['uninstall','force'].includes(action) && !await confirmDialog('Remove plugin',`Remove ${name}? Its configuration is kept.`,{danger:true,label:'Remove'})) return;
  pluginUIBusy = true;
  try {
    let result;
    if (action === 'install') {
      // The device downloads and prepares the archive itself.
      result = await post('/v1/plugins/install',{name,source:'feed',enable:true});
    } else if (action === 'update') {
      result = await post(`/v1/plugins/${encodeURIComponent(name)}/update`,{});
    } else {
      const version = pluginListing.plugins.find(plugin => plugin.name === name)?.version ?? null;
      const command = action === 'enable' || action === 'disable' ? { set_plugin_enabled: { name, enabled: action === 'enable', version } }
        : { remove_plugin: { name, purge: false, force: action === 'force' } };
      result = action === 'uninstall' || action === 'force'
        ? await confirmedCommand(command,{title:'Remove plugin',label:'Remove',danger:true}) : await nativeCommand(command);
      if (result.ok) {
        // Accepted intent starts a lifecycle job; its outcome arrives as an observation.
        const outcome = await waitForPluginJob();
        if (outcome?.status === 'failed') result = { ok: false, error: outcome.detail || 'Plugin operation failed' };
        else {
          try { pluginListing = { ...pluginListing, ...(await apiGet<PluginListing>('/v1/plugins')) }; } catch {}
          // A removed plugin is no longer listed.
          const listed = pluginListing.plugins.find(plugin => plugin.name === name)?.state;
          result = { ...result, state: listed ?? ('remove_plugin' in command ? 'removed' : action === 'enable' ? 'enabled' : 'installed') };
        }
      }
    }
    if (!result.ok) throw new Error(resultText(result, result.log || 'Plugin operation failed'));
    toast(`${name}: ${result.state}`, 'success');
  } catch (e) { toast((e as Error).message,'danger'); }
  finally { pluginUIBusy = false; await loadPlugins(); }
};

export let uploadPlugin = async function() {
  const name = (document.getElementById('plugin-upload-name') as HTMLInputElement).value.trim();
  const file = (document.getElementById('plugin-upload') as HTMLInputElement).files![0];
  if (!/^[a-z][a-z0-9-]{1,31}$/.test(name) || !file) return toast('Choose a plugin name and archive','warning');
  if (pluginUIBusy) return;
  if (!await confirmDialog('Install plugin',`Install ${name}? Only install archives from publishers you trust.`,{label:'Install'})) return;
  pluginUIBusy = true;
  try {
    const response = await fetch('/v1/plugins/upload?name='+encodeURIComponent(name),{method:'POST',headers:{'Content-Type':'application/octet-stream'},body:file}).then(handle401);
    const uploaded = await response.json();
    if (!response.ok || !uploaded.ok) throw new Error(uploaded.error || 'Upload failed');
    const result = await post('/v1/plugins/install',{name,source:'upload',enable:true});
    if (!result.ok) throw new Error(result.error || 'Install failed');
    toast(`${name}: ${result.state}`, 'success');
  } catch (e) { toast((e as Error).message,'danger'); }
  finally { pluginUIBusy = false; await loadPlugins(); }
};

// Retain the classic-script bindings used by handlers, plugin fragments and tests.
expose('pluginStatusListeners', () => pluginStatusListeners);
expose('pluginFragments', () => pluginFragments);
expose('pluginListing', () => pluginListing, value => { pluginListing = value; });
expose('pluginUIBusy', () => pluginUIBusy, value => { pluginUIBusy = value; });
expose('apiGet', () => apiGet, value => { apiGet = value; });
expose('reconcilePluginFragments', () => reconcilePluginFragments, value => { reconcilePluginFragments = value; });
expose('renderPluginStateError', () => renderPluginStateError, value => { renderPluginStateError = value; });
expose('waitForPluginJob', () => waitForPluginJob, value => { waitForPluginJob = value; });
expose('resetPluginState', () => resetPluginState, value => { resetPluginState = value; });
expose('notifyPluginStatus', () => notifyPluginStatus, value => { notifyPluginStatus = value; });
expose('loadPlugins', () => loadPlugins, value => { loadPlugins = value; });
expose('renderPluginList', () => renderPluginList, value => { renderPluginList = value; });
expose('selectPluginTab', () => selectPluginTab, value => { selectPluginTab = value; });
expose('mountPluginFragment', () => mountPluginFragment, value => { mountPluginFragment = value; });
expose('refreshPluginFeed', () => refreshPluginFeed, value => { refreshPluginFeed = value; });
expose('runPluginAction', () => runPluginAction, value => { runPluginAction = value; });
expose('uploadPlugin', () => uploadPlugin, value => { uploadPlugin = value; });
