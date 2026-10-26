import { expose } from './legacy-globals';
import { confirmedCommand } from './native-page';
import type { Service } from './generated/Service';
import type { LegacyReads } from './legacy-types';

// ══ Services ═══════════════════════════════════════════════════
export const SVC_LABELS: Record<string, string> = { 'ndimon-r':'Decoder', 'ndimon-finder':'Finder', 'ndimon-control':'Web UI and control' };

// The unit names the service list reports, and the typed service each one controls.
const SERVICE_IDS: Record<string, Service> = { 'ndimon-r': 'decoder', 'ndimon-finder': 'finder', 'ndimon-control': 'control' };

export let loadServices = async function() {
  try {
    const d = await api<LegacyReads['/api/services']>('/api/services');
    const el = (document.getElementById('svc-list') as HTMLElement);
    if (!el || !d.services) return;
    el.innerHTML = d.services.map(s => {
      const label = SVC_LABELS[s.name] || s.name;
      const running = s.active === 'active';
      const stateText = running ? s.sub : s.active;
      return `<div class="svc-row">
        <span class="src-dot ${running ? 'on' : ''}" style="${running ? '' : 'background:var(--danger)'}"></span>
        <span class="svc-name">${esc(label)}</span>
        <span class="svc-state" style="color:${running ? 'var(--live)' : 'var(--danger)'}">${esc(stateText)}</span>
        <button class="icon-btn" style="width:26px;height:26px;font-size:.75rem" onclick="restartService('${esc(s.name,1)}')" title="Restart ${esc(label)}"><i class="bi bi-arrow-clockwise"></i></button>
      </div>`;
    }).join('');
  } catch {}
};

// What the operator is told before each unit restarts.
export const RESTART_WARNINGS: Record<string, string> = {
  'ndimon-r': 'Every output shows the splash screen while the decoder restarts.',
  'ndimon-finder': 'Source discovery pauses for a few seconds. Outputs keep playing.',
  'ndimon-control': 'The web UI is unavailable for a few seconds. Reload the page afterwards.',
};

export let restartService = async function(name: string) {
  const label = SVC_LABELS[name] || name;
  if (!(await confirmDialog(`Restart ${label}?`, RESTART_WARNINGS[name] || 'The service is unavailable for a few seconds.', { label: 'Restart' }))) return;
  toast(`Restarting ${label}…`, 'warning');
  const service = SERVICE_IDS[name];
  const r = service ? await confirmedCommand({ service_control: { service, action: 'restart' } }, { title: `Restart ${label}?`, label: 'Restart' })
    : { ok: false, error: `Unknown service: ${name}` };
  // Restarting the web layer itself closes this connection before it can answer.
  if (name === 'ndimon-control' && r.ok === false && r.error === 'Could not reach the device')
    toast('The API connection closed. Restart status is unconfirmed; reload the page.', 'warning');
  else done(r, `${label} restarted`);
  setTimeout(loadServices, 2000);
};

export let restartAllServices = async function() {
  if (!(await confirmDialog('Restart decoder and finder?', 'Every output drops to the splash screen for a few seconds. The web UI stays up.', { label: 'Restart' }))) return;
  toast('Restarting decoder and finder…', 'warning');
  const results = [];
  for (const name of ['ndimon-finder', 'ndimon-r'])
    results.push(await confirmedCommand({ service_control: { service: SERVICE_IDS[name], action: 'restart' } }, { title: 'Restart decoder and finder?', label: 'Restart' }));
  const bad = results.find(r => r && r.ok === false);
  done(bad || {}, 'Decoder and finder restarted');
  setTimeout(loadServices, 3000);
};

export let sysReboot = async function() {
  if (!(await confirmDialog('Reboot the device?', 'Outputs go dark until the decoder is back, usually under a minute. Saved sources reconnect automatically.', { danger: true, label: 'Reboot' }))) return;
  const r = await confirmedCommand('reboot', { title: 'Reboot the device?', label: 'Reboot', danger: true });
  if (done(r, '')) toast('Rebooting…','warning');
};


// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('SVC_LABELS', () => SVC_LABELS);
expose('loadServices', () => loadServices, value => { loadServices = value; });
expose('restartService', () => restartService, value => { restartService = value; });
expose('restartAllServices', () => restartAllServices, value => { restartAllServices = value; });
expose('sysReboot', () => sysReboot, value => { sysReboot = value; });
