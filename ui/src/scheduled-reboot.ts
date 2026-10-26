import { expose } from './legacy-globals';
import { acceptNativeRevision, devicePatch, nativeCommand, nativeRevision, nativeState, resultText } from './native-page';
import type { Weekday } from './generated/Weekday';
import type { LegacyReads } from './legacy-types';

// ══ Scheduled reboot ════════════════════════════════════════════
export let selectedRebootDays = function() {
  return [...document.querySelectorAll<HTMLElement>('.rb-day.active')].map(b => b.dataset.day!);
};

export let setRebootDays = function(days: string[]) {
  const set = new Set(days || []);
  document.querySelectorAll<HTMLElement>('.rb-day').forEach(b => {
    b.classList.toggle('active', set.has(b.dataset.day!));
  });
};

export let toggleRebootDay = function(btn: HTMLElement) {
  btn.classList.toggle('active');
};

export let toggleRebootSchedule = function() {
  const on = !!(document.getElementById('rb-enabled') as HTMLInputElement)?.checked;
  const fields = (document.getElementById('rb-fields') as HTMLElement);
  if (fields) { fields.style.opacity = on ? '1' : '.5'; fields.style.pointerEvents = on ? '' : 'none'; }
};

export let formatRebootNext = function(iso?: string | null) {
  if (!iso) return '';
  try {
    return new Date(iso).toLocaleString(undefined, { weekday: 'short', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
  } catch { return ''; }
};

// When the box will next reboot is computed by the device (its timer); the saved
// schedule itself comes from the store.
const loadRebootNext = async () => {
  const d = await api<LegacyReads['/v1/DeviceSettings/reboot-schedule']>('/v1/DeviceSettings/reboot-schedule');
  return d.next;
};
const showRebootNext = (enabled: boolean, next?: string | null) => {
  const nextEl = (document.getElementById('rb-next') as HTMLElement);
  if (!nextEl) return;
  const when = formatRebootNext(next);
  nextEl.textContent = enabled && when
    ? `Next reboot: ${when}.`
    : 'If the box is off at that time, it will not reboot when it next starts.';
};

export let loadRebootSchedule = async function() {
  try {
    const state = await nativeState();
    const d = state.device.reboot_schedule;
    const en = (document.getElementById('rb-enabled') as HTMLInputElement);
    if (en) en.checked = !!d.enabled;
    setVal('rb-time', d.time || '04:00');
    setRebootDays(d.days && d.days.length ? d.days : ['sun','mon','tue','wed','thu','fri','sat']);
    toggleRebootSchedule();
    acceptNativeRevision('reboot', state.revision);
    showRebootNext(!!d.enabled, await loadRebootNext());
  } catch {}
};

export let saveRebootSchedule = async function() {
  const enabled = !!(document.getElementById('rb-enabled') as HTMLInputElement)?.checked;
  let time = ((document.getElementById('rb-time') as HTMLInputElement)?.value || '').trim();
  if (/^\d{2}:\d{2}:\d{2}$/.test(time)) time = time.slice(0, 5);
  const days = selectedRebootDays() as Weekday[];
  if (enabled && !days.length) { toast('Select at least one day', 'warning'); return; }
  const daysOut: Weekday[] = days.length ? days : ['sun','mon','tue','wed','thu','fri','sat'];
  try {
    const r = await nativeCommand({set_device: devicePatch({reboot_schedule: { enabled, time, days: daysOut }})}, await nativeRevision('reboot'));
    if (!r.ok) { toast(resultText(r, 'Save failed'), 'danger'); return; }
    acceptNativeRevision('reboot', r.revision);
    let next: string | null | undefined;
    try { next = await loadRebootNext(); } catch {}
    const when = formatRebootNext(next);
    toast(enabled ? (when ? `Scheduled — next ${when}` : 'Scheduled reboot saved') : 'Scheduled reboot off');
    showRebootNext(enabled, next);
  } catch { toast('Save failed', 'danger'); }
};


// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('selectedRebootDays', () => selectedRebootDays, value => { selectedRebootDays = value; });
expose('setRebootDays', () => setRebootDays, value => { setRebootDays = value; });
expose('toggleRebootDay', () => toggleRebootDay, value => { toggleRebootDay = value; });
expose('toggleRebootSchedule', () => toggleRebootSchedule, value => { toggleRebootSchedule = value; });
expose('formatRebootNext', () => formatRebootNext, value => { formatRebootNext = value; });
expose('loadRebootSchedule', () => loadRebootSchedule, value => { loadRebootSchedule = value; });
expose('saveRebootSchedule', () => saveRebootSchedule, value => { saveRebootSchedule = value; });
