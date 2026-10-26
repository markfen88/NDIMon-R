import { ApiFailure, apiClient } from './api';
import { store } from './store';
import { expose } from './legacy-globals';
import type { Command } from './generated/Command';
import type { DesiredState } from './generated/DesiredState';
import type { DevicePatch } from './generated/DevicePatch';
import type { Domain } from './generated/Domain';
import type { Transport } from './generated/Transport';
import type { AudioReply, LegacyResult, StatusEvent, LegacyEvent } from './legacy-types';

export let nativeState = async function(): Promise<Readonly<DesiredState>> {
  if (!store.state) await store.reload();
  return store.state!;
};

// The pending network change as observed (it never carries the confirmation token).
export const nativePending = async () => {
  // Wait for the initial read that is already under way instead of starting another.
  if (!store.observed) await store.start();
  return store.observed?.network_pending ?? null;
};

export const devicePatch =(patch: Partial<DevicePatch>): DevicePatch => ({
  hostname: null, ndi_alias: null, mode: null, decode_mode: null,
  watchdog_mode: null, ntp_server: null, reboot_schedule: null, about: null,
  analog_audio_in_gain: null, analog_audio_out_gain: null, plugin_feeds: null,
  allow_insecure_feeds: null, web_allowed_hosts: null, ...patch,
});

// Let the existing Bootstrap dialog finish hiding before a changed impact
// opens it again. Its promise resolves on the button click, before the fade.
export const nativeConfirm = async (title: string, text: string, options: {label?: string; danger?: boolean}): Promise<boolean> => {
  const accepted = await confirmDialog(title,text,options);
  const modal = document.getElementById('confirm-modal');
  if (modal?.style.display && modal.style.display !== 'none') {
    await new Promise<void>(resolve=>modal.addEventListener('hidden.bs.modal',()=>resolve(),{once:true}));
  }
  return accepted;
};

// Keep the existing presentation/error boundary while requests use typed v1.
export let nativeCommand = async function(command: Command, revision?: number,
  options: { confirm?: boolean; expect_impact?: string } = {}): Promise<LegacyResult> {
  try {
    const expect_revision = revision ?? (await nativeState()).revision;
    apiClient.setCsrf(globalThis.ndimonCsrf || null);
    let cancelled = false;
    const reply = await apiClient.command({ command, expect_revision, ...(
      options.expect_impact ? { expect_impact: options.expect_impact } : {}) },
    options.confirm === false ? undefined : async impact => {
      const text = impact.lines.join(' ') || 'This change briefly interrupts the device.';
      const accepted = await nativeConfirm('Apply change', text, { label: 'Apply' });
      cancelled = !accepted;
      return accepted;
    }).catch(error => {
      if (cancelled) return null;
      throw error;
    });
    if (!reply) return { ok: false, error: 'Not applied', cancelled: true };
    // This write is the only change since any editor that shares its base revision
    // (the device refused it otherwise), so those editors move on with it.
    for (const [key, value] of editRevisions) if (value === expect_revision) editRevisions.set(key, reply.revision);
    // Sequential saves must compose against the accepted revision. A failed
    // refresh does not turn a completed command into a reported write failure.
    try { await store.reload(); } catch {}
    return { ok: true, revision: reply.revision, result: (reply as { result?: unknown }).result };
  } catch (error) {
    if (!(error instanceof ApiFailure)) return { ok: false, error: 'Could not reach the device' };
    const refusal = error.envelope.error;
    if (error.status === 401) { showLogin(); return { ok: false, error: 'Signed out' }; }
    if (typeof refusal === 'string') return { ok: false, error: refusal };
    if ('confirmation_required' in refusal) return { ok: false, error: 'confirmation_required', impact: refusal.confirmation_required.impact };
    if ('revision_conflict' in refusal) return { ok: false, error: 'revision_conflict', revision: refusal.revision_conflict.current_revision };
    if ('locked' in refusal) return { ok: false, error: refusal.locked.field === 'matrix_confirmation_required' ? 'matrix_confirmation_required' : 'locked', errors: [refusal.locked.field] };
    if ('invalid' in refusal) return { ok: false, error: 'invalid', errors: refusal.invalid.errors.map(e => e.field + ': ' + e.reason) };
    // The legacy routes reported the bare reason; keep that text for these kinds.
    if ('unsupported' in refusal) return { ok: false, error: refusal.unsupported.what };
    if ('apply_failed' in refusal) return { ok: false, error: refusal.apply_failed.reason };
    return { ok: false, error: error.envelope.message };
  }
};

export interface ConfirmWording { title: string; label: string; danger?: boolean }

// For an action the operator has already confirmed in its own dialog (restart,
// reboot, update): ask the device for the impact it computes, then apply exactly
// that impact. If the impact changed before the resend, the operator is asked again
// in the same dialog style with the impact the device now reports.
export let confirmedCommand = async function(command: Command, wording: ConfirmWording): Promise<LegacyResult> {
  let token: string | undefined;
  for (let attempt = 0; ; attempt++) {
    const result = await nativeCommand(command, undefined, { confirm: false, ...(token ? { expect_impact: token } : {}) });
    const impact = result.impact;
    if (result.error !== 'confirmation_required' || !impact?.token || attempt === 2) return result;
    // The first impact is the one the operator's own dialog already covered.
    if (attempt > 0) {
      const text = impact.lines?.join(' ') || 'This change briefly interrupts the device.';
      if (!await nativeConfirm(wording.title, text, { label: wording.label, ...(wording.danger ? { danger: true } : {}) })) {
        return { ok: false, error: 'Not applied', cancelled: true };
      }
    }
    token = impact.token;
  }
};

export let nativeAudio = async function(ch: number): Promise<AudioReply> {
  const state = await nativeState();
  const bus = state.audio.outputs[ch] ?? { enabled: false, primary: false, inputs: [], destinations: [], legacy_matrix: false, definitions: { sources: {}, sinks: {} } };
  return { ...bus, revision: state.revision, native: bus,
    ...(bus.legacy_matrix ? { warning: 'Legacy per-destination gains and mutes are preserved. Changing this mix requires explicit normalization.' } : {}) };
};

export const transportValues: Record<Transport, string> = { rudp: 'RUDP', tcp: 'TCP', multi_tcp: 'M-TCP', udp: 'UDP', multicast: 'Multicast' };
export const transportFromLegacy = (value: string): Transport => (Object.keys(transportValues) as Transport[]).find(key => transportValues[key] === value) ?? 'rudp';

export let loadNativeSettings = async function() {
  const state = await nativeState();
  setVal('tp-sel', transportValues[state.ndi.receive]);
  setVal('decode-mode-sel', state.device.decode_mode);
  setVal('disc-ip', state.ndi.discovery_servers.join(','));
  setNdiGroupsFromString(state.ndi.groups.join(','));
  setVal('off-ips', state.ndi.off_subnet_sources.join(','));
  const advertise = document.getElementById('ds-advertise') as HTMLInputElement | null;
  const control = document.getElementById('ds-control') as HTMLInputElement | null;
  if (advertise) advertise.checked = state.ndi.receiver_advertising;
  if (control) control.checked = state.ndi.remote_control;
  setVal('ndi-alias', state.device.ndi_alias);
  acceptNativeRevision('decoder',state.revision);
  acceptNativeRevision('discovery',state.revision);
};

// Each editor keeps the revision its form was filled from; a save is refused
// with `revision_conflict` if the document moved on in the meantime.
const editRevisions = new Map<string, number>();
export const nativeRevision = async (key: string): Promise<number> => editRevisions.get(key) ?? (await nativeState()).revision;
export const acceptNativeRevision = (key: string, revision?: number): void => {
  if (revision !== undefined) editRevisions.set(key, revision);
};
document.addEventListener('DOMContentLoaded', () => {
  const remember = (event: Event) => {
    const target = event.target as HTMLElement;
    const card = target.closest?.('[data-card-idx]') as HTMLElement | null;
    const id = target.id;
    const key = card && /^(output-alias-|res-|scale-sel-|rotation-sel-)/.test(id) ? (id.startsWith('output-alias-') ? 'alias:' : 'video:') + (liveView().outputs[Number(card.dataset.cardIdx)]?.output_index ?? card.dataset.cardIdx) :
      ['tp-sel', 'decode-mode-sel'].includes(id) ? 'decoder' :
      ['disc-ip', 'off-ips', 'ndi-alias', 'ndi-grp-input', 'ds-advertise', 'ds-control'].includes(id) || target.closest?.('#ndi-grp-tags') ? 'discovery' : '';
    if (key && store.state && !editRevisions.has(key)) editRevisions.set(key, store.state.revision);
  };
  document.addEventListener('input', remember);
  document.addEventListener('change', remember);
  document.addEventListener('click', remember);
});

/** The device message for a failed native result, as `post()` used to show `error`. */
export const resultText = (result: LegacyResult, fallback: string): string =>
  result.error === 'invalid' && result.errors?.length ? result.errors.join(' · ') : result.error || fallback;

// Readings without a native observation arrive as telemetry samples (see Telemetry).
export let applyTelemetry = function(data: StatusEvent) {
  store.setTelemetry({ system: data.system, plugins: data.plugins });
};
// A sample redraws only what shows telemetry; the rest follows the store's own changes.
export let renderTelemetry = function() {
  const view = liveView();
  updateDash();
  renderTemps('dash-temps');
  renderTemps('sys-temps');
  setText('sys-up', view.uptime||'—');
  setText('sys-plat', view.platform||'—');
};

// A card shows the saved settings again only when the operator has no unsaved edit in it.
const cardClean = (...ids: string[]) => ids.every(id => !document.getElementById(id)?.closest('.card')?.classList.contains('dirty'));

let lastPluginProgress: unknown;
export let renderFromStore = function(domains: readonly Domain[]) {
  const state = store.state;
  if (!state) return;
  const progress = store.status.services as LegacyEvent | undefined;
  if (progress?.type === 'plugin_progress' && progress !== lastPluginProgress) {
    lastPluginProgress = progress;
    toast(`${progress.name}: ${progress.step}${progress.error ? ' · ' + progress.error : ''}`, progress.error ? 'danger' : 'info');
  }
  updateUI();
  if (domains.length) void refreshAudioFromServer();
  if (domains.includes('presets')) void loadPresets();
  if (domains.includes('ndi') || domains.includes('device')) {
    const ids = ['tp-sel', 'decode-mode-sel', 'disc-ip', 'off-ips', 'ndi-alias', 'ds-advertise', 'ds-control'];
    if (cardClean(...ids)) { editRevisions.delete('decoder'); editRevisions.delete('discovery'); void loadNativeSettings(); }
  }
  if (domains.includes('display')) {
    if (cardClean('sp-bg-idle')) void loadSplashSettings();
    if (cardClean('osd-enabled')) void loadOsd();
  }
  if (domains.includes('device')) {
    if (cardClean('web-hosts')) void loadWebHosts();
    if (cardClean('ntp-server')) void loadNtp();
    if (cardClean('rb-enabled')) void loadRebootSchedule();
    if (cardClean('wd-mode')) void loadWatchdog();
  }
  if (domains.includes('network')) void loadNetwork();
  if (domains.includes('plugins')) void loadPlugins();
};

expose('nativeState', () => nativeState, value => { nativeState = value; });
expose('nativeCommand', () => nativeCommand, value => { nativeCommand = value; });
expose('nativeAudio', () => nativeAudio, value => { nativeAudio = value; });
