import { expose } from './legacy-globals';
import { confirmedCommand } from './native-page';
import type { LegacyReads } from './legacy-types';

const signedReleaseRequired = 'updates need a signed release; no release key is configured';

// ══ Software update ═════════════════════════════════════════════
export let loadUpdateStatus = async function() {
  try {
    const v = await api<LegacyReads['/api/v1/system/version']>('/api/v1/system/version');
    const el = (document.getElementById('upd-status') as HTMLElement);
    const btn = (document.getElementById('upd-apply-btn') as HTMLButtonElement);
    let txt = v.commit ? `commit ${String(v.commit).slice(0, 9)}` : (v.firmware || '—');
    if (v.update_running) txt = 'Updating…';
    else if (!v.update_supported) txt = v.update_message || `${txt} · manual updates only`;
    setText('upd-status', txt);
    if (btn) btn.disabled = !v.update_supported;
  } catch {}
};

export let checkUpdate = async function() {
  setText('upd-status', 'Checking…');
  try {
    const v = await api<LegacyReads['/api/v1/system/version']>('/api/v1/system/version');
    const btn = (document.getElementById('upd-apply-btn') as HTMLButtonElement);
    if (!v.update_supported) {
      setText('upd-status', v.update_message || signedReleaseRequired);
      if (btn) btn.disabled = true;
    } else if (v.updates_available > 0) {
      setText('upd-status', `${v.updates_available} update(s) available`);
      if (btn) btn.disabled = false;
    } else if (v.updates_available === 0) {
      setText('upd-status', 'Up to date');
      if (btn) btn.disabled = true;
    } else {
      setText('upd-status', v.update_message || 'A signed release version is required');
    }
  } catch { setText('upd-status', 'Check failed'); }
};

export let applyUpdate = async function() {
  try {
    // Without a configured release key the device refuses updates; say why
    // before asking. A device that installs signed releases refuses an
    // unusable request with its own message.
    const version = await api<LegacyReads['/api/v1/system/version']>('/api/v1/system/version');
    if (!version.update_supported) {
      setText('upd-status', version.update_message || signedReleaseRequired);
      toast(version.update_message || signedReleaseRequired, 'warning');
      return;
    }
    // Signed updates install a named release; the device reports the newest
    // one it can verify once releases and a key exist.
    const target = version.latest_version;
    if (!target) {
      setText('upd-status', 'No signed release is available to install');
      return;
    }
    // This dialog is the operator's one prompt; confirmedCommand applies the impact it covers.
    if (!(await confirmDialog('Update now?', 'Install a signed release and restart services. The web UI will be unavailable during the update.', { label: 'Update now' }))) return;
    toast('Starting update…', 'warning');
    const r = await confirmedCommand({ install_update: { package: null, version: target, force: false } }, { title: 'Update now?', label: 'Update now' });
    if (r.ok) setText('upd-status', 'Updating… services will restart');
    else toast('Update failed: ' + (r.error || 'unknown'), 'danger');
  } catch { toast('Update request failed', 'danger'); }
};


// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('loadUpdateStatus', () => loadUpdateStatus, value => { loadUpdateStatus = value; });
expose('checkUpdate', () => checkUpdate, value => { checkUpdate = value; });
expose('applyUpdate', () => applyUpdate, value => { applyUpdate = value; });
