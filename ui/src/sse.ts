import { expose } from './legacy-globals';
import { store } from './store';
import { applyTelemetry, renderFromStore, renderTelemetry } from './native-page';
import type { StatusEvent } from './legacy-types';

// One native stream per tab feeds the store; every view draws from it.
let subscribed = false;
export let initSSE = function() {
  if (!subscribed) {
    subscribed = true;
    store.subscribe((_state, domains) => renderFromStore(domains));
    store.subscribeTelemetry(() => renderTelemetry());
    store.subscribeConnection(ok => setConnState(ok));
  }
  void store.start().catch(() => setConnState(false));
};

export let _connLost = false;
export let setConnState = function(ok: boolean) {
  if (_connLost === !ok) return;
  _connLost = !ok;
  const dot = (document.getElementById('conn-dot') as HTMLElement);
  if (dot) dot.classList.toggle('lost', !ok);
  setText('conn-text', ok ? 'Live' : 'Reconnecting');
  const wrap = (document.getElementById('conn-wrap') as HTMLElement);
  if (wrap) wrap.title = ok ? 'Live updates connected' : 'Lost contact with the device';
  const b = (document.getElementById('sse-warning') as HTMLElement);
  if (b) b.style.display = ok ? 'none' : '';
};

// CPU, memory, temperatures, uptime and plugin status have no native observation
// yet: /api/status is their only source, sampled into the store.
let telemetryPolling: ReturnType<typeof setInterval> | null = null;
export let refreshTelemetry = async function() {
  try {
    const d = await api<StatusEvent>('/api/status');
    applyTelemetry(d);
    notifyPluginStatus(d);
  } catch {}
};
export let startTelemetryPolling = function() {
  if (!telemetryPolling) telemetryPolling = setInterval(refreshTelemetry, 1500);
};

// Refresh service status every 10s when system page is visible
export let _svcInterval: ReturnType<typeof setInterval> | null = null;
export let startSvcPolling = function() {
  if (_svcInterval) return;
  _svcInterval = setInterval(() => {
    if ((document.getElementById('page-system') as HTMLElement)?.classList.contains('active')) { loadServices(); refreshWatchdogStats(); }
  }, 10000);
};


// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('initSSE', () => initSSE, value => { initSSE = value; });
expose('_connLost', () => _connLost, value => { _connLost = value; });
expose('setConnState', () => setConnState, value => { setConnState = value; });
expose('_svcInterval', () => _svcInterval, value => { _svcInterval = value; });
expose('startSvcPolling', () => startSvcPolling, value => { startSvcPolling = value; });
