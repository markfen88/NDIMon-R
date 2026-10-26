import { expose } from './legacy-globals';
import { store } from './store';
import { confirmedCommand, nativeCommand, nativeConfirm, nativePending, nativeState, resultText } from './native-page';
import type { LegacyResult, LegacyPending, RestoreResult, RestoreReply, RestorePlan, LegacyReads, InspectReply, BackupSection } from './legacy-types';

// ══ Backup, restore, network ════════════════════════════════════
export const NET_PENDING_KEY = 'ndimon-net-pending';
export const SECTION_LABELS: Record<string, string> = {
  outputs: 'Outputs and sources', presets: 'Source presets',
  discovery: 'Discovery, groups, extra IPs', transport: 'Transport',
  display: 'Splash and OSD', device: 'Decoder, watchdog, NTP, reboot schedule',
  identity: 'Device name', auth: 'Admin password', network: 'Network',
  services: 'Decoder restart',
};
export let restorePlan: RestorePlan | null = null;
export let undoName = '';
export let netTimer: ReturnType<typeof setInterval> | null = null;
export let netPoll: ReturnType<typeof setInterval> | null = null;

export let modalOf = function(id: string) { return bootstrap.Modal.getOrCreateInstance((document.getElementById(id) as HTMLElement)); }
// The one way the page closes a dialog. Bootstrap ignores hide() while a dialog is
// still animating in (a quick close, or a slow or busy device), which leaves it open:
// hide it once it has finished showing instead.
export let hideModal = function(id: string) {
  const dialog = document.getElementById(id) as HTMLElement;
  const modal = bootstrap.Modal.getInstance(dialog);
  if (!modal) return;
  if (dialog.classList.contains('show')) {
    const hideWhenShown = () => modal.hide();
    dialog.addEventListener('shown.bs.modal', hideWhenShown, { once: true });
    dialog.addEventListener('hidden.bs.modal', () => dialog.removeEventListener('shown.bs.modal', hideWhenShown), { once: true });
  }
  modal.hide();
}

// Promise-based replacement for window.confirm(), styled like the other modals.
export let confirmDialog = async function(title: string, body: string, opts: { danger?: boolean; label?: string } = {}) {
  // Bootstrap ignores show() while a dialog is still fading out, so a question asked
  // right after another (a changed impact) waits for the first to finish closing.
  const shown = (document.getElementById('confirm-modal') as HTMLElement);
  if (shown.style.display && shown.style.display !== 'none' && !shown.classList.contains('show')) {
    await new Promise<void>(resolve => shown.addEventListener('hidden.bs.modal', () => resolve(), { once: true }));
  }
  return new Promise<boolean>(resolve => {
    const m = (document.getElementById('confirm-modal') as HTMLElement);
    setText('confirm-title', title);
    (document.getElementById('confirm-body') as HTMLElement).textContent = body;
    const ok = (document.getElementById('confirm-ok') as HTMLButtonElement);
    ok.className = 'btn btn-sm ' + (opts.danger ? 'btn-danger' : 'btn-primary');
    ok.textContent = opts.label || 'Confirm';
    let settled = false;
    const finish = (v: boolean) => { if (!settled) { settled = true; resolve(v); } };
    ok.onclick = () => { finish(true); hideModal('confirm-modal'); };
    m.addEventListener('hidden.bs.modal', () => finish(false), { once: true });
    modalOf('confirm-modal').show();
    setTimeout(() => ok.focus(), 200);
  });
};

// Report the outcome of a post(). Returns true when it succeeded.
export let done = function(r: LegacyResult | null | undefined, okMsg: string) {
  if (r && r.ok === false) { toast(resultText(r, 'Request failed'), 'danger'); return false; }
  if (okMsg) toast(okMsg);
  return true;
};

export let setBusy = function(btn: HTMLButtonElement | null, on: boolean, label?: string) {
  if (!btn) return;
  if (on) {
    if (!btn.dataset.label) btn.dataset.label = btn.innerHTML;
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner-border spinner-border-sm me-1"></span>${label || 'Working…'}`;
  } else {
    btn.disabled = false;
    if (btn.dataset.label) btn.innerHTML = btn.dataset.label;
  }
};

export let persistNetPending = function(info: LegacyPending | null) {
  if (!info || !info.token) { sessionStorage.removeItem(NET_PENDING_KEY); return; }
  sessionStorage.setItem(NET_PENDING_KEY, JSON.stringify({
    token: info.token,
    address: info.address || '',
    deadline: info.deadline || 0,
    mode: info.mode || '',
  }));
};

export let readNetPending = function(): LegacyPending | null {
  try { return JSON.parse(sessionStorage.getItem(NET_PENDING_KEY) || 'null'); }
  catch { return null; }
};

export let clearNetPending = function() {
  sessionStorage.removeItem(NET_PENDING_KEY);
  clearInterval(netTimer!);
  clearInterval(netPoll!);
  netTimer = netPoll = null;
};

export let restoreModalFooter = function(mode: string) {
  const foot = (document.getElementById('bk-modal-footer') as HTMLElement);
  if (mode === 'result') {
    foot.innerHTML = '<button class="btn btn-secondary btn-sm" onclick="hideModal(\'bk-modal\')">Close</button>'
      + '<button class="btn btn-primary btn-sm" id="bk-result-net" style="display:none" onclick="resumeNetWait()">Confirm new address</button>';
    return;
  }
  foot.innerHTML = '<button class="btn btn-secondary btn-sm" id="bk-cancel-btn" onclick="hideModal(\'bk-modal\')">Cancel</button>'
    + '<button class="btn btn-primary btn-sm" id="bk-restore-btn" onclick="confirmRestore()">Restore</button>';
};

export let syncRestoreButton = function() {
  const btn = (document.getElementById('bk-restore-btn') as HTMLButtonElement);
  if (!btn) return;
  btn.disabled = !document.querySelector('#bk-summary input[type=checkbox]:checked');
};

export let consumeNetConfirm = async function() {
  if (location.hash.startsWith('#netprofile=')) {
    const token=location.hash.slice('#netprofile='.length);
    history.replaceState(null,'',location.pathname+location.search);
    await window.NDIMON_NETWORK.resumeToken(token);
    return '';
  }
  if (!location.hash.startsWith('#netconfirm=')) return '';
  const token = decodeURIComponent(location.hash.slice('#netconfirm='.length));
  history.replaceState(null, '', location.pathname + location.search);
  if (!await confirmDialog('Keep network changes?', 'Keep the new management network settings?', {label:'Keep changes'})) return 'Network settings remain unconfirmed and will revert.';
  try {
    const r = await fetch('/api/net-confirm', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ token }),
    });
    const j: LegacyResult = await r.json().catch(() => ({}));
    if (j.ok || r.status === 410) clearNetPending();
    return j.ok
      ? 'Network settings confirmed.'
      : (j.error || 'This confirmation has expired. The device has returned to its previous settings.');
  } catch {
    return 'Could not confirm the network change.';
  }
};

export let downloadBackup = async function() {
  const btn = (document.getElementById('bk-dl-btn') as HTMLButtonElement);
  setBusy(btn, true, 'Preparing…');
  try {
    const net = (document.getElementById('bk-network') as HTMLInputElement).checked ? '1' : '0';
    const pw = (document.getElementById('bk-password') as HTMLInputElement).checked ? '1' : '0';
    const r = await fetch(`/v1/Backup/export?network=${net}&password=${pw}`);
    if (r.status === 401) { showLogin(); return; }
    if (!r.ok) { toast('Could not build the backup', 'danger'); return; }
    const blob = await r.blob();
    const cd = r.headers.get('Content-Disposition') || '';
    const match = cd.match(/filename="([^"]+)"/);
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = match ? match[1] : 'ndimon-backup.json';
    a.click();
    setTimeout(() => URL.revokeObjectURL(a.href), 1000);
  } catch {
    toast('Could not download the backup', 'danger');
  } finally {
    setBusy(btn, false);
  }
};

export let inspectBackup = async function(input: HTMLInputElement) {
  const file = input.files && input.files[0];
  input.value = '';
  if (!file) return;
  const btn = (document.getElementById('bk-up-btn') as HTMLButtonElement);
  setBusy(btn, true, 'Reading backup…');
  try {
    let doc: unknown;
    try { doc = JSON.parse(await file.text()); }
    catch { toast('Not a valid backup file', 'danger'); return; }
    const r = await fetch('/v1/Backup/inspect', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(doc),
    });
    if (r.status === 401) { showLogin(); return; }
    const plan: InspectReply = await r.json().catch(() => ({}));
    if (!r.ok || !plan.ok) { toast(plan.error || 'Backup was rejected', 'danger'); return; }
    // The file is compared with the document as it is now; applying it checks that.
    restorePlan = plan;
    restorePlan.revision = (await nativeState()).revision;
    const src = plan.source_device || {};
    const app = plan.app || {};
    const rows = (plan.sections || []).map((s: BackupSection) => `
      <div class="restore-row">
        <div class="form-check">
          <input class="form-check-input" type="checkbox" value="${esc(s.key)}" id="bk-${esc(s.key)}" ${s.selected ? 'checked' : ''} onchange="syncRestoreButton()">
          <label class="form-check-label" for="bk-${esc(s.key)}"><strong>${esc(s.label)}</strong>
            <span class="badge ${s.changed ? 'bg-warning text-dark' : 'bg-secondary'} ms-1">${s.changed ? 'Changed' : 'Unchanged'}</span></label>
        </div>
        <div class="form-text" style="white-space:pre-wrap">${esc(s.summary || '')}</div>
      </div>`).join('');
    const net = plan.network || {};
    const netLine = !net.present ? ''
      : net.needs_prompt ? '<div class="alert alert-warning py-2 mb-3">Network settings differ. You will be asked before they are overwritten.</div>'
      : net.applicable === false ? `<div class="alert alert-secondary py-2 mb-3">${esc(net.message || '')}</div>`
      : '<div class="form-text mb-3">Network settings match. They will be left as they are.</div>';
    const extra = (plan.extra_outputs || []).length
      ? `<div class="form-text">Output ${esc(plan.extra_outputs!.join(', '))} is in the file and will be stored. This device may not have that output.</div>` : '';
    const when = plan.created_at ? new Date(plan.created_at).toLocaleString() : '';
    restoreModalFooter('choose');
    (document.getElementById('bk-modal-title') as HTMLElement).textContent = 'Restore backup';
    (document.getElementById('bk-summary') as HTMLElement).innerHTML =
      `<div class="mb-3">From <strong>${esc(src.ndi_alias || src.hostname || 'unknown device')}</strong>`
      + (when ? ` · ${esc(when)}` : '')
      + (app.firmware ? ` · firmware ${esc(app.firmware)}` : '')
      + `</div>${netLine}${rows}${extra}`
      + `<div class="form-text mt-3">NDI groups keep the case they were saved with. Leave Device name off when setting up a second receiver from this file. Restoring selected settings restarts the decoder for a few seconds.</div>`;
    modalOf('bk-modal').show();
    syncRestoreButton();
  } catch {
    toast('Could not read the backup', 'danger');
  } finally {
    setBusy(btn, false);
  }
};

export let confirmRestore = function() {
  if (!restorePlan) return;
  restorePlan.chosen = [...document.querySelectorAll<HTMLInputElement>('#bk-summary input[type=checkbox]:checked')].map(el => el.value);
  if (!restorePlan.chosen.length && !(restorePlan.network && restorePlan.network.needs_prompt)) {
    toast('Choose at least one section to restore', 'warning');
    return;
  }
  const net = restorePlan.network || {};
  if (net.needs_prompt) {
    const warns = (net.warnings || []).map(w => `<div class="alert alert-warning py-2">${esc(w)}</div>`).join('');
    const ack = net.duplicate
      ? `<div class="form-check mt-2"><input class="form-check-input" type="checkbox" id="net-ack" onchange="document.getElementById('net-use').disabled=!this.checked"><label class="form-check-label" for="net-ack">I understand another device is using this address</label></div>`
      : '';
    (document.getElementById('net-modal-body') as HTMLElement).innerHTML = `<p>${esc(net.message || '')}</p>${warns}${ack}`;
    const use = (document.getElementById('net-use') as HTMLButtonElement);
    use.disabled = !!net.duplicate;
    hideModal('bk-modal');
    modalOf('net-modal').show();
    setTimeout(() => (document.getElementById('net-keep') as HTMLButtonElement)?.focus(), 200);
    return;
  }
  finishRestore(false);
};

export let resultRows = function(results?: Record<string, RestoreResult>) {
  return Object.entries(results || {}).map(([key, v]) => {
    const label = SECTION_LABELS[key] || key;
    const vlanFailures = ((v && v.vlans) || []).filter(vlan => vlan.ok === false).map(vlan =>
      `<div class="restore-row"><span class="result-fail">Failed</span> · VLAN ${esc(vlan.id)} — ${esc(vlan.error || 'error')}</div>`).join('');
    if (v && v.ok === false)
      return `<div class="restore-row"><span class="result-fail">Failed</span> · ${esc(label)} — ${esc(v.error || 'error')}</div>` + vlanFailures;
    if (v && v.skipped)
      return `<div class="restore-row"><span class="result-skip">Skipped</span> · ${esc(label)} — ${esc(v.skipped)}</div>` + vlanFailures;
    const extra = v && v.warning ? ` — ${esc(v.warning)}` : (v && v.pending ? ' — waiting for address confirmation' : '');
    return `<div class="restore-row"><span class="result-ok">Restored</span> · ${esc(label)}${extra}</div>` + vlanFailures;
  }).join('');
};

export let showRestoreResult = function(out: RestoreReply, title?: string) {
  restoreModalFooter('result');
  (document.getElementById('bk-modal-title') as HTMLElement).textContent = title || 'Restore complete';
  const failed = Object.entries(out.results || {}).some(([, v]) => v && v.ok === false);
  (document.getElementById('bk-summary') as HTMLElement).innerHTML =
    (failed
      ? '<div class="alert alert-warning py-2">Restored, with a problem in one or more sections.</div>'
      : '<div class="alert alert-success py-2">Settings restored.</div>')
    + resultRows(out.results)
    + (out.noop ? '<div class="form-text">Nothing was selected, so the decoder was left running.</div>' : '');
  const netBtn = (document.getElementById('bk-result-net') as HTMLButtonElement);
  if (netBtn && out.network && out.network.token) netBtn.style.display = '';
  modalOf('bk-modal').show();
};

export let refreshAfterRestore = function() {
  loadSnapshots();
  loadSysInfo();
  try { loadSettings(); } catch {}
};

// POST a restore request. The Restore button's own dialog already covers the first
// impact the device reports, so it is resent with that impact's token; an impact
// that changed meanwhile is shown, in the generic dialog, before the next resend.
const restoreCall = async (url: string, body: object, owner: string): Promise<{ ok: boolean; out: RestoreReply; cancelled?: boolean } | null> => {
  let token: string | undefined;
  for (let attempt = 0; ; attempt++) {
    const r = await fetch(url, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ ...body, ...(token ? { expect_impact: token } : {}) }),
    });
    if (r.status === 401) { showLogin(); return null; }
    const out: RestoreReply = await r.json().catch(() => ({}));
    const impact = out.impact;
    if (out.error !== 'confirmation_required' || !impact?.token || attempt === 2) return { ok: r.ok, out };
    if (attempt > 0) {
      hideModal(owner);
      if (!await nativeConfirm('Apply change', impact.lines?.join(' ') || 'This change briefly interrupts the device.', { label: 'Apply' })) {
        return { ok: false, out, cancelled: true };
      }
      modalOf(owner).show();
    }
    token = impact.token;
  }
};

export let finishRestore = async function(applyNetwork: boolean) {
  if (!restorePlan) return;
  if (!(restorePlan.chosen || []).length && !applyNetwork) {
    hideModal('net-modal');
    toast('Nothing to restore', 'warning');
    return;
  }
  hideModal('net-modal');
  const btn = (document.getElementById('bk-restore-btn') as HTMLButtonElement);
  setBusy(btn, true, 'Restoring…');
  (document.getElementById('bk-modal-title') as HTMLElement).textContent = 'Restoring…';
  (document.getElementById('bk-summary') as HTMLElement).innerHTML =
    '<p>Writing settings. The picture goes to the splash screen for a few seconds if the decoder needs to restart.</p>';
  modalOf('bk-modal').show();
  try {
    const reply = await restoreCall('/v1/Backup/apply', {
      import_id: restorePlan.import_id,
      sections: restorePlan.chosen || [],
      apply_network: !!applyNetwork,
      expect_revision: restorePlan.revision,
    }, 'bk-modal');
    if (!reply) return;
    if (reply.cancelled) { hideModal('bk-modal'); return; }
    const out = reply.out;
    if (!reply.ok || !out.ok) { toast(out.error || 'Restore failed', 'danger'); hideModal('bk-modal'); return; }
    showRestoreResult(out);
    if (out.network && out.network.token) persistNetPending(out.network);
    refreshAfterRestore();
    if (out.relogin) showLogin();
  } catch {
    toast('Restore failed', 'danger');
    hideModal('bk-modal');
  } finally {
    setBusy(btn, false);
  }
};

export let netConfirmHref = function(info: LegacyPending | null) {
  if (!info || !info.token) return '';
  const port = location.port ? ':' + location.port : '';
  if (info.address)
    return `${location.protocol}//${info.address}${port}/#netconfirm=${info.token}`;
  return `${location.origin}/#netconfirm=${info.token}`;
};

export let renderNetWait = function(info: LegacyPending) {
  const text = (document.getElementById('net-wait-text') as HTMLElement);
  const link = (document.getElementById('net-wait-link') as HTMLAnchorElement);
  const href = netConfirmHref(info);
  if (info.address) {
    text.textContent = 'Open this address from a computer that can reach it. The change reverts on its own if you do not.';
    link.href = href;
    link.textContent = href;
  } else {
    text.textContent = 'The new address will appear on the connected display. When it does, this link updates. You can also copy it and replace the host with the address on the screen.';
    link.removeAttribute('href');
    link.textContent = href || (`/#netconfirm=${info.token || ''}`);
  }
};

export let showNetWait = function(info: LegacyPending) {
  if (info?.profiles) return window.NDIMON_NETWORK.watch(info);
  persistNetPending(info);
  renderNetWait(info);
  const deadline = (info.deadline || 0) * 1000;
  clearInterval(netTimer!);
  clearInterval(netPoll!);
  const tick = () => {
    const left = Math.max(0, Math.round((deadline - Date.now()) / 1000));
    const el = (document.getElementById('net-wait-count') as HTMLElement);
    if (el) el.textContent = left ? `Reverts in ${left}s if not confirmed.` : 'The change is reverting.';
    if (!left) {
      clearInterval(netTimer!);
      clearInterval(netPoll!);
    }
  };
  tick();
  netTimer = setInterval(tick, 1000);
  netPoll = setInterval(async () => {
    try {
      await store.refreshObserved();
      const pending = store.observed?.network_pending ?? null;
      if (!pending) {
        clearNetPending();
        hideModal('net-wait');
        toast('The pending network change has ended. Check the address on this page.', 'warning', 8000);
        loadNetwork();
        return;
      }
      const live = pending.management_address || '';
      if (live && live !== info.address) {
        info.address = live;
        persistNetPending(info);
        renderNetWait(info);
      }
    } catch {}
  }, 2000);
  hideModal('bk-modal');
  modalOf('net-wait').show();
};

export let resumeNetWait = async function(quiet?: boolean) {
  if (window.NDIMON_NETWORK?.resumeStored()) return;
  let info = readNetPending();
  try {
    const pending = await nativePending();
    if (!pending) {
      clearNetPending();
      if (!quiet) toast('No network change is waiting', 'warning');
      loadNetwork();
      return;
    }
    // The device never reports the token; this browser keeps the one it was given.
    info = {
      token: ((info && info.token) || '') as string,
      address: pending.management_address || (info && info.address) || '',
      deadline: Math.round(Date.now() / 1000) + pending.remaining_seconds || (info && info.deadline) || 0,
      mode: (info && info.mode) || '',
    };
  } catch {
    if (!info || !info.token) {
      if (!quiet) toast('Could not load the pending network change', 'danger');
      return;
    }
  }
  if (!info.token) {
    if (!quiet) toast('The confirmation link is no longer available. Cancel from the Network card if you need to revert.', 'warning', 8000);
    return;
  }
  showNetWait(info);
};

export let copyNetWaitLink = async function() {
  const link = (document.getElementById('net-wait-link') as HTMLAnchorElement);
  const text = (link && (link.href && link.href !== location.href + '#' ? link.href : link.textContent)) || '';
  if (!text) return;
  if (navigator.clipboard && navigator.clipboard.writeText) {
    try { await navigator.clipboard.writeText(text); toast('Link copied'); return; }
    catch { /* HTTP and denied clipboard access use the selection fallback. */ }
  }
  const ta = document.createElement('textarea');
  ta.value = text; document.body.appendChild(ta); ta.select();
  try {
    if (!document.execCommand('copy')) throw new Error('copy denied');
    toast('Link copied');
  } catch { toast('Could not copy the link', 'danger'); }
  finally { ta.remove(); }
};

export let cancelNetworkChange = async function() {
  const r = await nativeCommand('rollback_network');
  if (!done(r, '')) return;
  clearInterval(netTimer!);
  clearInterval(netPoll!);
  clearNetPending();
  hideModal('net-wait');
  toast('Network change reverted', 'warning');
  loadNetwork();
};

export let loadSnapshots = async function() {
  const box = (document.getElementById('bk-snaps') as HTMLElement);
  if (!box) return;
  try {
    const d = await api<LegacyReads['/v1/Backup/snapshots']>('/v1/Backup/snapshots');
    const snaps = d.snapshots || [];
    if (!snaps.length) { box.innerHTML = ''; return; }
    const newest = snaps[0];
    const when = newest.mtime ? new Date(newest.mtime).toLocaleString() : '';
    box.innerHTML = `<button class="btn btn-outline-warning btn-sm" onclick="askUndo('${esc(newest.name, true)}')">Undo last restore</button>`
      + `<div class="form-text mt-1">${esc(when)}</div>`;
  } catch {}
};

export let askUndo = function(name: string) {
  undoName = name;
  modalOf('bk-undo-modal').show();
};

export let commitUndo = async function() {
  const btn = (document.getElementById('bk-undo-go') as HTMLButtonElement);
  setBusy(btn, true, 'Restoring…');
  try {
    // The Undo button's dialog covers the impact the device first reports.
    const r = await confirmedCommand({ undo_restore: { snapshot: undoName } }, { title: 'Undo last restore', label: 'Undo restore' });
    hideModal('bk-undo-modal');
    if (r.cancelled) return;
    if (!r.ok) { toast(resultText(r, 'Undo failed'), 'danger'); return; }
    const out = (r.result ?? {}) as RestoreReply;
    showRestoreResult(out, 'Previous settings restored');
    if (out.network && out.network.token) persistNetPending(out.network);
    refreshAfterRestore();
    if (out.relogin) showLogin();
  } catch {
    toast('Undo failed', 'danger');
  } finally {
    setBusy(btn, false);
  }
};


// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('NET_PENDING_KEY', () => NET_PENDING_KEY);
expose('SECTION_LABELS', () => SECTION_LABELS);
expose('restorePlan', () => restorePlan, value => { restorePlan = value; });
expose('undoName', () => undoName, value => { undoName = value; });
expose('netTimer', () => netTimer, value => { netTimer = value; });
expose('netPoll', () => netPoll, value => { netPoll = value; });
expose('modalOf', () => modalOf, value => { modalOf = value; });
expose('hideModal', () => hideModal, value => { hideModal = value; });
expose('confirmDialog', () => confirmDialog, value => { confirmDialog = value; });
expose('done', () => done, value => { done = value; });
expose('setBusy', () => setBusy, value => { setBusy = value; });
expose('persistNetPending', () => persistNetPending, value => { persistNetPending = value; });
expose('readNetPending', () => readNetPending, value => { readNetPending = value; });
expose('clearNetPending', () => clearNetPending, value => { clearNetPending = value; });
expose('restoreModalFooter', () => restoreModalFooter, value => { restoreModalFooter = value; });
expose('syncRestoreButton', () => syncRestoreButton, value => { syncRestoreButton = value; });
expose('consumeNetConfirm', () => consumeNetConfirm, value => { consumeNetConfirm = value; });
expose('downloadBackup', () => downloadBackup, value => { downloadBackup = value; });
expose('inspectBackup', () => inspectBackup, value => { inspectBackup = value; });
expose('confirmRestore', () => confirmRestore, value => { confirmRestore = value; });
expose('resultRows', () => resultRows, value => { resultRows = value; });
expose('showRestoreResult', () => showRestoreResult, value => { showRestoreResult = value; });
expose('refreshAfterRestore', () => refreshAfterRestore, value => { refreshAfterRestore = value; });
expose('finishRestore', () => finishRestore, value => { finishRestore = value; });
expose('netConfirmHref', () => netConfirmHref, value => { netConfirmHref = value; });
expose('renderNetWait', () => renderNetWait, value => { renderNetWait = value; });
expose('showNetWait', () => showNetWait, value => { showNetWait = value; });
expose('resumeNetWait', () => resumeNetWait, value => { resumeNetWait = value; });
expose('copyNetWaitLink', () => copyNetWaitLink, value => { copyNetWaitLink = value; });
expose('cancelNetworkChange', () => cancelNetworkChange, value => { cancelNetworkChange = value; });
expose('loadSnapshots', () => loadSnapshots, value => { loadSnapshots = value; });
expose('askUndo', () => askUndo, value => { askUndo = value; });
expose('commitUndo', () => commitUndo, value => { commitUndo = value; });
