import { expose } from './legacy-globals';
import { acceptNativeRevision, devicePatch, nativeCommand, nativeRevision, nativeState } from './native-page';
import type { WatchdogMode } from './generated/WatchdogMode';
import type { LegacyReads } from './legacy-types';

// ══ Watchdog ═════════════════════════════════════════════════════
export let loadWatchdog = async function() {
  try {
    const state = await nativeState();
    setVal('wd-mode', state.device.watchdog_mode || 'active');
    acceptNativeRevision('watchdog', state.revision);
  } catch {}
  refreshWatchdogStats();
};

export let saveWatchdogMode = async function() {
  const mode = (document.getElementById('wd-mode') as HTMLSelectElement).value as WatchdogMode;
  const r = await nativeCommand({set_device: devicePatch({watchdog_mode: mode})}, await nativeRevision('watchdog'));
  if (r.ok) acceptNativeRevision('watchdog', r.revision);
  done(r, 'Watchdog mode: ' + mode);
  setTimeout(refreshWatchdogStats, 500);
};

export let refreshWatchdogStats = async function() {
  try {
    // Live health state from ndimon-r
    const h = await api<LegacyReads['/api/health']>('/api/health');
    let health = 'ok', stall = 0;
    if (h.outputs) for (const o of h.outputs) {
      if (o.health !== 'ok' && o.health !== 'idle') health = o.health;
      stall = Math.max(stall, o.stall_count || 0);
    }
    const hColor = health === 'ok' ? 'var(--live)' : health === 'degraded' ? 'var(--warn)' : 'var(--danger)';
    setText('wd-health', health);
    const hEl = (document.getElementById('wd-health') as HTMLElement); if (hEl) hEl.style.color = hColor;
    setText('wd-stall', stall);

    // Action stats from watchdog service
    const w = await api<LegacyReads['/api/watchdog-stats']>('/api/watchdog-stats');
    const badge = (document.getElementById('wd-badge') as HTMLElement);
    const mode = w.watchdog_mode || 'unknown';
    if (badge) {
      const colors: Record<string, string> = { disabled: 'chip-muted', passive: 'chip-blue', active: 'chip-live', unknown: 'chip-warn' };
      badge.className = 'chip ' + (colors[mode] || 'chip-muted');
      badge.textContent = mode.charAt(0).toUpperCase() + mode.slice(1);
    }
    setText('wd-recv', w.recv_stalls || 0);
    setText('wd-video', w.video_stalls || 0);
    setText('wd-dec-stall', w.decoder_stalls || 0);
    setText('wd-disp-freeze', w.display_freezes || 0);
    setText('wd-reconnect', w.reconnects || 0);
    setText('wd-svc-restart', w.service_restarts || 0);
  } catch {}
};


// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('loadWatchdog', () => loadWatchdog, value => { loadWatchdog = value; });
expose('saveWatchdogMode', () => saveWatchdogMode, value => { saveWatchdogMode = value; });
expose('refreshWatchdogStats', () => refreshWatchdogStats, value => { refreshWatchdogStats = value; });
