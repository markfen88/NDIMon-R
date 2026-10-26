import { acceptNativeRevision, devicePatch, loadNativeSettings, nativeCommand, nativeRevision, nativeState } from './native-page';
import type { Splash } from './generated/Splash';
import { expose } from './legacy-globals';
import { liveView } from './live';
import type { LegacyReads } from './legacy-types';

// ══ Settings ════════════════════════════════════════════════════
export let loadSettings = async function() {
  try { await loadNativeSettings(); } catch {}
  loadSplashSettings();
  loadOsd();
  loadNetwork();
};

// The configured server is saved intent; whether the clock is synchronized is read from the OS.
export let loadNtp = async function() {
  try {
    const state = await nativeState();
    setVal('ntp-server', state.device.ntp_server || '');
    acceptNativeRevision('ntp', state.revision);
    const n = await api<LegacyReads['/v1/DeviceSettings/ntp']>('/v1/DeviceSettings/ntp');
    const sync = n.synchronized ? 'synchronized' : 'not synchronized';
    const srv  = n.active_server ? ` with ${n.active_server}` : '';
    setText('ntp-status', `OS clock ${sync}${srv}. Persists across reboots.`);
  } catch {}
};

export let saveNtp = async function() {
  const ntp = (document.getElementById('ntp-server') as HTMLInputElement).value.trim();
  const r = await nativeCommand({set_device: devicePatch({ntp_server: ntp})}, await nativeRevision('ntp'));
  if (r.ok) acceptNativeRevision('ntp', r.revision);
  done(r, ntp ? `NTP server set to ${ntp}` : 'NTP server reset to OS default');
  setTimeout(loadNtp, 1500);
};

// The decoder reads the logo from a fixed path per media type; the form shows it.
export const logoPath = (logo: Splash['logo']) =>
  logo?.media_type === 'image/png' ? '/etc/ndi-splash-logo.png' : logo?.media_type === 'image/jpeg' ? '/etc/ndi-splash-logo.jpg' : '';

export let loadSplashSettings = async function() {
  try {
    const state = await nativeState();
    const d = state.display.splash;
    setColorField('sp-bg-idle',   'sp-bg-idle-hex',  d.bg_idle);
    setColorField('sp-bg-live',   'sp-bg-live-hex',  d.bg_live);
    setColorField('sp-ac-idle',   'sp-ac-idle-hex',  d.accent_idle);
    setColorField('sp-ac-live',   'sp-ac-live-hex',  d.accent_live);
    setVal('sp-logo-path', logoPath(d.logo));
    setSlider('sp-logo-x', 'sp-logo-x-val', d.logo_x_pct, '%');
    setSlider('sp-logo-y', 'sp-logo-y-val', d.logo_y_pct, '%');
    setSlider('sp-logo-w', 'sp-logo-w-val', d.logo_w_pct, '%');
    setVal('sp-text-idle', d.text_idle||'');
    setVal('sp-text-live', d.text_live||'');
    setSlider('sp-text-height', 'sp-text-h-val', d.text_height_pct || 10, '%');
    setCheck('sp-show-signal', d.show_signal_text !== false);
    setCheck('sp-show-devname', d.show_device_name !== false);
    setCheck('sp-show-devurl', d.show_device_url !== false);
    setCheck('sp-show-sources', d.show_sources_available !== false);
    setBackdrop(d.backdrop || 'gradient', true);
    acceptNativeRevision('splash', state.revision);
  } catch(e) { console.warn('splash config load failed', e); }
  updateSplashPreview();
};

export let _spMode = 'idle';
export let setSplashPreviewMode = function(mode: string) {
  _spMode = mode;
  document.querySelectorAll<HTMLElement>('#sp-pv-mode button').forEach(b => b.classList.toggle('active', b.dataset.mode === mode));
  updateSplashPreview();
};

export let updateSplashPreview = function() {
  const pv = (document.getElementById('sp-preview') as HTMLElement);
  if (!pv) return;
  const v = (id: string) => (document.getElementById(id) as HTMLInputElement)?.value;
  const on = (id: string) => !!(document.getElementById(id) as HTMLInputElement)?.checked;
  const live = _spMode === 'live';
  pv.style.setProperty('--sp-bg', v(live ? 'sp-bg-live' : 'sp-bg-idle') || '#000');
  pv.style.setProperty('--sp-ac', v(live ? 'sp-ac-live' : 'sp-ac-idle') || '#fff');
  pv.classList.toggle('flat', v('sp-backdrop') === 'flat');
  const h = pv.clientHeight || 200;
  const textPct = +(v('sp-text-height') || 4);
  const statusPx = Math.max(8, h * textPct / 100);
  const subPx = Math.max(6, statusPx * 0.6);
  const pill = (document.getElementById('sp-pv-pill') as HTMLElement);
  setText('sp-pv-text', live ? v('sp-text-live') || 'Signal Available' : v('sp-text-idle') || 'No Signal');
  pill.style.fontSize = statusPx + 'px';
  pill.style.display = on('sp-show-signal') ? '' : 'none';
  const src = (document.getElementById('sp-pv-src') as HTMLElement);
  src.style.fontSize = subPx + 'px';
  src.style.display = on('sp-show-sources') ? '' : 'none';
  const n = liveView().sources.length;
  setText('sp-pv-src', n === 0 ? 'No sources found' : `${n} source${n === 1 ? '' : 's'} available`);
  const dev = (document.getElementById('sp-pv-dev') as HTMLElement);
  setText('sp-pv-dev', ((document.getElementById('ndi-alias') as HTMLInputElement)?.value.trim()) || liveView().device_name || 'NDIMON-XXXX');
  dev.style.fontSize = statusPx * 0.8 + 'px';
  dev.style.display = on('sp-show-devname') ? '' : 'none';
  const url = (document.getElementById('sp-pv-url') as HTMLElement);
  setText('sp-pv-url', location.origin);
  url.style.fontSize = subPx + 'px';
  url.style.display = on('sp-show-devurl') ? '' : 'none';
  const logo = (document.getElementById('sp-pv-logo') as HTMLElement);
  logo.style.left = v('sp-logo-x') + '%';
  logo.style.top = v('sp-logo-y') + '%';
  logo.style.width = v('sp-logo-w') + '%';
  logo.style.display = v('sp-logo-path') ? '' : 'none';
};

// Backdrop is a segmented control backed by a hidden input so saveSplash() reads one value.
export let setBackdrop = function(value: string, silent?: boolean) {
  const v = value === 'flat' ? 'flat' : 'gradient';
  const input = (document.getElementById('sp-backdrop') as HTMLInputElement);
  if (!input) return;
  input.value = v;
  document.querySelectorAll<HTMLElement>('#sp-backdrop-seg button').forEach(b => b.classList.toggle('active', b.dataset.v === v));
  updateSplashPreview();
  if (!silent) markDirty(input);
};

// One-click colour sets; each fills the four colour fields, nothing is saved until Save.
export const SPLASH_PALETTES = [
  { name: 'Midnight', idle: ['#0D1B2A', '#4488CC'], live: ['#0D2B1A', '#22FF88'] },
  { name: 'Graphite', idle: ['#14171C', '#9AA4B2'], live: ['#121A16', '#3DDC84'] },
  { name: 'Slate',    idle: ['#0F172A', '#60A5FA'], live: ['#052E16', '#4ADE80'] },
  { name: 'Studio',   idle: ['#050505', '#F59E0B'], live: ['#050505', '#22C55E'] },
];
export let renderSplashPalettes = function() {
  const el = (document.getElementById('sp-palettes') as HTMLElement);
  if (!el) return;
  el.innerHTML = SPLASH_PALETTES.map((p, i) =>
    `<button type="button" class="sp-swatch" onclick="applySplashPalette(${i})" title="${p.name}: fills the four colours">` +
    [...p.idle, ...p.live].map(c => `<span style="background:${c}"></span>`).join('') + `<b>${p.name}</b></button>`).join('');
};
export let applySplashPalette = function(i: number) {
  const p = SPLASH_PALETTES[i];
  if (!p) return;
  setColorField('sp-bg-idle', 'sp-bg-idle-hex', p.idle[0]);
  setColorField('sp-ac-idle', 'sp-ac-idle-hex', p.idle[1]);
  setColorField('sp-bg-live', 'sp-bg-live-hex', p.live[0]);
  setColorField('sp-ac-live', 'sp-ac-live-hex', p.live[1]);
  updateSplashPreview();
  markDirty((document.getElementById('sp-palettes') as HTMLElement));
};
document.addEventListener('DOMContentLoaded', renderSplashPalettes);

document.addEventListener('DOMContentLoaded', () => {
  const card = (document.getElementById('sp-preview') as HTMLElement)?.closest('.card');
  if (card) card.addEventListener('input', updateSplashPreview);
  window.addEventListener('resize', updateSplashPreview);
});

export let setColorField = function(pickerId: string, hexId: string, val: string) {
  if (!val) return;
  const picker = (document.getElementById(pickerId) as HTMLInputElement);
  const hex    = (document.getElementById(hexId) as HTMLInputElement);
  if (picker) picker.value = val;
  if (hex)    hex.value    = val;
};

export let setSlider = function(sliderId: string, labelId: string, val: number, suffix?: string) {
  const sl = (document.getElementById(sliderId) as HTMLInputElement);
  const lb = (document.getElementById(labelId) as HTMLElement);
  if (sl) sl.value = Math.round(val) as unknown as string;
  if (lb) lb.textContent = Math.round(val) + (suffix||'');
};

export let syncColorHex = function(pickerId: string, hexId: string) {
  const hex = (document.getElementById(hexId) as HTMLInputElement);
  const picker = (document.getElementById(pickerId) as HTMLInputElement);
  if (!hex || !picker) return;
  const v = hex.value.trim();
  if (/^#[0-9a-fA-F]{6}$/.test(v)) { picker.value = v; updateSplashPreview(); }
};

// Keep hex field in sync when color picker changes
document.addEventListener('DOMContentLoaded', () => {
  const pairs = [
    ['sp-bg-idle','sp-bg-idle-hex'],['sp-bg-live','sp-bg-live-hex'],
    ['sp-ac-idle','sp-ac-idle-hex'],['sp-ac-live','sp-ac-live-hex'],
  ];
  for (const [pid, hid] of pairs) {
    const p = (document.getElementById(pid) as HTMLInputElement);
    if (p) p.addEventListener('input', () => {
      const h = (document.getElementById(hid) as HTMLInputElement);
      if (h) h.value = p.value;
      updateSplashPreview();
    });
  }
});

export let loadSysInfo = async function() {
  try {
    const d = await api<LegacyReads['/v1/AboutMe']>('/v1/AboutMe');
    setText('sys-model', d.Model||'—');
    setText('sys-host', d.HostName||d.DeviceName||'—');
    setText('sys-ip',   Array.isArray(d.IPAddresses)?d.IPAddresses[0]:d.IPAddress||'—');
    setText('sys-ndi',  d.NDIVersion||'—');
    setText('sys-fw',   d.FirmwareVersion||'—');
    setText('sys-build', d.BuildDate||'—');
  } catch {}
  loadServices();
  loadUpdateStatus();
  loadVaapiInfo();
};

export let loadVaapiInfo = async function() {
  try {
    const v = await api<LegacyReads['/api/v1/system/vaapi-info']>('/api/v1/system/vaapi-info');
    const row = (document.getElementById('sys-vaapi-row') as HTMLElement);
    if (!v.available) { if (row) row.style.display = 'none'; return; }
    if (row) row.style.display = '';
    const profs = (v.decode_profiles||[]).join(', ') || 'none';
    setText('sys-vaapi', `${v.driver||'VAAPI'} — ${profs}`);
  } catch {}
};


// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('loadSettings', () => loadSettings, value => { loadSettings = value; });
expose('loadNtp', () => loadNtp, value => { loadNtp = value; });
expose('saveNtp', () => saveNtp, value => { saveNtp = value; });
expose('loadSplashSettings', () => loadSplashSettings, value => { loadSplashSettings = value; });
expose('_spMode', () => _spMode, value => { _spMode = value; });
expose('setSplashPreviewMode', () => setSplashPreviewMode, value => { setSplashPreviewMode = value; });
expose('updateSplashPreview', () => updateSplashPreview, value => { updateSplashPreview = value; });
expose('setBackdrop', () => setBackdrop, value => { setBackdrop = value; });
expose('SPLASH_PALETTES', () => SPLASH_PALETTES);
expose('renderSplashPalettes', () => renderSplashPalettes, value => { renderSplashPalettes = value; });
expose('applySplashPalette', () => applySplashPalette, value => { applySplashPalette = value; });
expose('setColorField', () => setColorField, value => { setColorField = value; });
expose('setSlider', () => setSlider, value => { setSlider = value; });
expose('syncColorHex', () => syncColorHex, value => { syncColorHex = value; });
expose('loadSysInfo', () => loadSysInfo, value => { loadSysInfo = value; });
expose('loadVaapiInfo', () => loadVaapiInfo, value => { loadVaapiInfo = value; });
