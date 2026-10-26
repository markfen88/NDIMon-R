import { store } from './store';
import { nativeCommand, nativeState, nativeRevision, acceptNativeRevision, devicePatch, transportFromLegacy } from './native-page';
import type { Backdrop } from './generated/Backdrop';
import type { Splash } from './generated/Splash';
import { expose } from './legacy-globals';
import { liveView } from './live';

// ══ Actions ══════════════════════════════════════════════════════
// Connect a discovered source to an output (1-based channel). Used by the
// dashboard source list; the NDI page goes through applySource().
export let doConnect = async function(name: string, ip?: string, outputCh?: number) {
  const r = await nativeCommand(!name || name==='None'?{forget_source:{output:outputCh||1}}:{select_source:{output:outputCh||1,source:{name,endpoint:ip||''}}});
  done(r, 'Connecting to '+name+'…');
};

export let connectFromDash = function(name: string) {
  const sel = (document.getElementById('dash-target-out') as HTMLSelectElement);
  const view = liveView();
  const ch = (view.outputs || []).length > 1 && sel?.value ? parseInt(sel.value, 10) : 1;
  doConnect(name, view.sourceIPs[name] || '', ch);
};

export let doDisconnect = async function(cardIdx: number) {
  const out = liveView().outputs[cardIdx];
  const output = (out?.output_index ?? cardIdx ?? 0) + 1;
  const r = await nativeCommand({forget_source:{output}});
  const sel = (document.getElementById(`src-sel-${cardIdx || 0}`) as HTMLSelectElement);
  if (sel) { sel.value = 'None'; onSrcChange(cardIdx || 0); }
  done(r, `Output ${(cardIdx || 0) + 1}: disconnected`);
};

export let refreshSources = async function() {
  const r = await nativeCommand('refresh_ndi');
  if (r.cancelled || (r.ok === false && r.error === 'Signed out')) return;
  toast('Rescanning NDI sources…');
  setTimeout(loadSourceIPs, 3000);
};

export let saveTally = async function() {
  const state = await nativeState();
  const value = (document.getElementById('tally-sel') as HTMLSelectElement).value;
  const r = await nativeCommand({set_ndi:{...state.ndi,tally:value==='TallyOn'?'on':value==='TallyVideo'?'video':'off'}},state.revision);
  done(r, 'Tally saved');
};

export let saveAudioMode = async function() {
  const state = await nativeState();
  const enabled = (document.getElementById('ndi-audio-sel') as HTMLSelectElement).value === 'NDIAudioEn';
  const r = await nativeCommand({put_output_settings:{outputs:{},ndi:null,audio_enabled:{1:enabled}}},state.revision);
  done(r, 'Audio saved');
};

export let saveDecoder = async function() {
  const state = await nativeState();
  const receive = transportFromLegacy((document.getElementById('tp-sel') as HTMLSelectElement).value);
  const decode_mode = (document.getElementById('decode-mode-sel') as HTMLSelectElement).value as typeof state.device.decode_mode;
  const a = await nativeCommand({set_ndi:{...state.ndi,receive}},await nativeRevision('decoder'));
  const b = await nativeCommand({set_device:devicePatch({decode_mode})},a.ok?a.revision:await nativeRevision('decoder'));
  if(a.ok && b.ok) acceptNativeRevision('decoder',b.revision);
  done(a.ok === false ? a : b, 'Decoder settings saved');
};

export let saveSplash = async function() {
  const state = await nativeState();
  const field = (id: string) => document.getElementById(id) as HTMLInputElement;
  const current = state.display.splash;
  // The read-only path field is the logo: emptied means none, otherwise the stored asset stays.
  const splash: Splash = {
    ...current,
    bg_idle:     field('sp-bg-idle').value,
    bg_live:     field('sp-bg-live').value,
    accent_idle: field('sp-ac-idle').value,
    accent_live: field('sp-ac-live').value,
    logo:        field('sp-logo-path').value.trim() ? current.logo : null,
    logo_x_pct:  +field('sp-logo-x').value,
    logo_y_pct:  +field('sp-logo-y').value,
    logo_w_pct:  +field('sp-logo-w').value,
    text_idle:   field('sp-text-idle').value,
    text_live:   field('sp-text-live').value,
    text_height_pct: +field('sp-text-height').value,
    show_signal_text: field('sp-show-signal').checked,
    show_device_name: field('sp-show-devname').checked,
    show_device_url:  field('sp-show-devurl').checked,
    show_sources_available: field('sp-show-sources').checked,
    backdrop:    field('sp-backdrop').value as Backdrop,
  };
  const r = await nativeCommand({set_display:{splash,osd:state.display.osd}}, await nativeRevision('splash'));
  if (r.ok) acceptNativeRevision('splash', r.revision);
  return done(r, 'Splash settings saved');
};

// Save first, so the display shows what the form shows.
export let previewSplash = async function(sourceAvailable: boolean) {
  if (!(await saveSplash())) return;
  const r = await nativeCommand({preview_splash:{source_available:sourceAvailable}});
  done(r, 'Splash preview sent to the display');
};

export let uploadLogo = async function(input: HTMLInputElement) {
  const file = input.files![0];
  if (!file) return;
  const reader = new FileReader();
  reader.onload = async (e) => {
    const dataUrl = e.target!.result as string;
    const base64  = dataUrl.split(',')[1];
    try {
      // No native upload route exists: this stores the image and commits it as the logo.
      const r = await post('/v1/Splash/uploadLogo', { data: base64, filename: file.name });
      if (r.ok) {
        (document.getElementById('sp-logo-path') as HTMLInputElement).value = r.path!;
        updateSplashPreview();
        // The upload already moved the document; the splash form's own edits stay pending.
        try { await store.reload(); } catch {}
        acceptNativeRevision('splash', store.state?.revision);
        toast('Logo uploaded. Save to apply it.');
      } else {
        toast('Upload failed: ' + (r.error || 'unknown'), 'danger');
      }
    } catch (err) {
      toast('Upload error: ' + (err as Error).message, 'danger');
    }
  };
  reader.readAsDataURL(file);
  input.value = '';  // reset so same file can be re-uploaded
};

export let loadOsd = async function() {
  try {
    const state = await nativeState();
    (document.getElementById('osd-enabled') as HTMLInputElement).checked = !!state.display.osd.enabled;
    (document.getElementById('osd-text') as HTMLInputElement).value = state.display.osd.text || '';
    acceptNativeRevision('osd', state.revision);
  } catch {}
};

export let saveOsd = async function() {
  const state = await nativeState();
  const enabled = (document.getElementById('osd-enabled') as HTMLInputElement).checked;
  const text    = (document.getElementById('osd-text') as HTMLInputElement).value.trim();
  const r = await nativeCommand({set_display:{splash:state.display.splash,osd:{enabled,text}}}, await nativeRevision('osd'));
  if (r.ok) acceptNativeRevision('osd', r.revision);
  done(r, 'OSD settings saved');
};

export let saveNetSettings = async function() {
  const state = await nativeState();
  const ip = (document.getElementById('disc-ip') as HTMLInputElement).value.trim();
  const groups = getNdiGroupsString().split(',').map(value=>value.trim()).filter(Boolean);
  const ips = (document.getElementById('off-ips') as HTMLInputElement).value.trim();
  const alias = (document.getElementById('ndi-alias') as HTMLInputElement).value.trim();
  const split = (value: string) => value.split(/[\s,;]+/).filter(Boolean);
  const results: import('./legacy-types').LegacyResult[] = [];
  let ndi = {...state.ndi};
  let revision = await nativeRevision('discovery');
  const save = async (command: import('./generated/Command').Command) => {
    const result = await nativeCommand(command,revision);
    if(result.ok && result.revision!==undefined) {
      revision=result.revision;
      if(typeof command==='object' && 'set_ndi' in command)ndi=command.set_ndi;
    }
    results.push(result);
  };
  await save({set_ndi:{...ndi,discovery_servers:split(ip)}});
  await save({set_ndi:{...ndi,groups}});
  await save({set_ndi:{...ndi,off_subnet_sources:split(ips)}});
  const advertise = document.getElementById('ds-advertise') as HTMLInputElement | null;
  const control = document.getElementById('ds-control') as HTMLInputElement | null;
  if (advertise && control && (advertise.checked !== ndi.receiver_advertising || control.checked !== ndi.remote_control))
    await save({set_ndi:{...ndi,receiver_advertising:advertise.checked,remote_control:control.checked}});
  await save({set_device:devicePatch({ndi_alias:alias})});
  const bad = results.find(r => r && r.ok === false);
  if (!done(bad || {}, 'Discovery settings saved')) return;
  acceptNativeRevision('discovery',revision);
  updateSplashPreview();
  refreshSources();
};



// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('doConnect', () => doConnect, value => { doConnect = value; });
expose('connectFromDash', () => connectFromDash, value => { connectFromDash = value; });
expose('doDisconnect', () => doDisconnect, value => { doDisconnect = value; });
expose('refreshSources', () => refreshSources, value => { refreshSources = value; });
expose('saveTally', () => saveTally, value => { saveTally = value; });
expose('saveAudioMode', () => saveAudioMode, value => { saveAudioMode = value; });
expose('saveDecoder', () => saveDecoder, value => { saveDecoder = value; });
expose('saveSplash', () => saveSplash, value => { saveSplash = value; });
expose('previewSplash', () => previewSplash, value => { previewSplash = value; });
expose('uploadLogo', () => uploadLogo, value => { uploadLogo = value; });
expose('loadOsd', () => loadOsd, value => { loadOsd = value; });
expose('saveOsd', () => saveOsd, value => { saveOsd = value; });
expose('saveNetSettings', () => saveNetSettings, value => { saveNetSettings = value; });
