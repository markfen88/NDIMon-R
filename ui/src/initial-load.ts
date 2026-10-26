import { apiClient } from './api';
import { store } from './store';
import { nativeAudio, nativeCommand, nativeConfirm, nativeState, nativeRevision, acceptNativeRevision } from './native-page';
import { refreshTelemetry, startTelemetryPolling } from './sse';
import { liveView } from './live';
import type { OutputAudio } from './generated/OutputAudio';
import type { OutputVideo } from './generated/OutputVideo';
import type { ScaleMode } from './generated/ScaleMode';
import { expose } from './legacy-globals';
import type { VideoMode, AudioInput, AudioDestination, OutputStatus, LegacyResult, AudioReply, AudioDraft, AudioCatalog, LegacyReads, OutputModesReply } from './legacy-types';

// ══ Initial load ════════════════════════════════════════════════
export let loadInitialData = async function() {
  try { await store.start(); } catch {}
  await refreshTelemetry();
  startTelemetryPolling();
  loadSourceIPs();
  loadSettings();
  loadWebHosts();
  loadVersionTag();
  startSvcPolling();
};

export let loadVersionTag = async function() {
  try {
    const d = await api<LegacyReads['/v1/AboutMe']>('/v1/AboutMe');
    setText('sb-version', d.FirmwareVersion ? 'v' + String(d.FirmwareVersion).replace(/^v/, '') : '—');
    if (d.Model) setText('sb-platform', d.Model);
  } catch {}
};

export let loadSourceIPs = async function() {
  try { await store.refreshSources(); } catch {}
};

export let fmtHz = function(hz: number) {
  // Show fractional rates nicely: 59.94, 29.97, 23.98 etc.
  if (!hz) return '—';
  return (Math.round(hz * 100) / 100) + ' Hz';
};

// Per-output modes cache: key = output_index
export const outputModes: Record<number, VideoMode[]> = {};
// The source an output shows, and who chose it when a Discovery Server did.
const sourceLabel = (out: OutputStatus): string =>
  (out.source_name || 'None') + (out.source_name && out.source_origin === 'discovery_server' ? ' · set by Discovery Server' : '');

export let loadOutputModes = async function(cardIdx: number, outputIndex: number) {
  try {
    const catalog = await apiClient.get<OutputModesReply>(`/api/v1/catalog/display_modes?output=${outputIndex+1}`);
    const video = (await nativeState()).outputs[outputIndex+1]?.video;
    const d = {...catalog, ...(video ? {auto_resolution:video.resolution==='auto',rotation:video.rotation,
      ...(video.resolution !== 'auto' ? {current:video.resolution.mode} : {})} : {})};
    outputModes[outputIndex] = d.modes || [];
    const cur = d.current || {};
    const resMap = new Map();
    outputModes[outputIndex].forEach(m => {
      const key = m.width+'x'+m.height;
      if (!resMap.has(key)) resMap.set(key, { width: m.width, height: m.height, preferred: m.preferred });
      else if (m.preferred) resMap.get(key).preferred = true;
    });
    const whSel = (document.getElementById(`res-wh-${cardIdx}`) as HTMLSelectElement);
    if (!whSel) return;
    whSel.innerHTML = '';
    // Auto option — matches source resolution dynamically
    const autoOpt = document.createElement('option');
    autoOpt.value = 'auto'; autoOpt.dataset.auto = '1';
    autoOpt.textContent = 'Auto (match source)';
    if (d.auto_resolution) autoOpt.selected = true;
    whSel.appendChild(autoOpt);
    resMap.forEach((r) => {
      const o = document.createElement('option');
      o.value = r.width+'x'+r.height;
      o.dataset.w = r.width as unknown as string; o.dataset.h = r.height as unknown as string;
      o.textContent = `${r.width} × ${r.height}${r.preferred?' ★':''}`;
      if (!d.auto_resolution && r.width === cur.width && r.height === cur.height) o.selected = true;
      whSel.appendChild(o);
    });
    populateOutputRefreshRates(cardIdx, outputIndex, cur);
    // Hide refresh rate column when auto mode is active
    const fpsCol = (document.getElementById(`res-fps-${cardIdx}`) as HTMLSelectElement)?.parentElement;
    if (fpsCol && d.auto_resolution) fpsCol.style.display = 'none';
    if (Array.isArray(d.supported_rotations)) {
      const select = (document.getElementById(`rotation-sel-${cardIdx}`) as HTMLSelectElement);
      if (select) for (const option of select.options) option.disabled = !d.supported_rotations.includes(Number(option.value));
    }
    // Sync rotation dropdown from modes response
    if (d.rotation !== undefined) setVal(`rotation-sel-${cardIdx}`, String(d.rotation));
  } catch {}
};

export let populateOutputRefreshRates = function(cardIdx: number, outputIndex: number, cur: Partial<VideoMode>) {
  const whSel = (document.getElementById(`res-wh-${cardIdx}`) as HTMLSelectElement);
  const fpsSel = (document.getElementById(`res-fps-${cardIdx}`) as HTMLSelectElement);
  if (!whSel || !fpsSel) return;
  const opt = whSel.options[whSel.selectedIndex];
  if (!opt) return;
  const w = parseInt(opt.dataset.w!), h = parseInt(opt.dataset.h!);
  const rates = (outputModes[outputIndex] || []).filter(m => m.width === w && m.height === h);
  fpsSel.innerHTML = '';
  rates.forEach(m => {
    const o = document.createElement('option');
    o.value = (m.refresh_hz || m.refresh) as unknown as string;
    o.dataset.hz = (m.refresh_hz || m.refresh) as unknown as string;
    o.textContent = fmtHz(m.refresh_hz || m.refresh) + (m.preferred ? ' ★' : '');
    const curHz = cur ? (cur.refresh_hz || cur.refresh) : 0;
    if (curHz && Math.abs((m.refresh_hz || m.refresh) - curHz) < 0.02) o.selected = true;
    fpsSel.appendChild(o);
  });
};

export let onResChange = function(cardIdx: number) {
  const out = liveView().outputs[cardIdx];
  const whSel = (document.getElementById(`res-wh-${cardIdx}`) as HTMLSelectElement);
  const fpsCol = (document.getElementById(`res-fps-${cardIdx}`) as HTMLSelectElement)?.parentElement;
  const isAuto = whSel?.options[whSel.selectedIndex]?.dataset.auto === '1';
  if (fpsCol) fpsCol.style.display = isAuto ? 'none' : '';
  if (!isAuto) populateOutputRefreshRates(cardIdx, out?.output_index ?? cardIdx, {});
};

export let updateNdiPreview = function(cardIdx?: number): void {
  const view = liveView();
  if (cardIdx === undefined) { (view.outputs || []).forEach((_, i) => updateNdiPreview(i)); updateSplashPreview(); return; }
  const preview = (document.getElementById(`ndi-preview-${cardIdx}`) as HTMLElement);
  if (!preview) return;
  const devName = ((document.getElementById('ndi-alias') as HTMLInputElement)?.value.trim()) ||
                  view.device_name || 'NDIMON-XXXX';
  const out = view.outputs[cardIdx];
  const outAlias = ((document.getElementById(`output-alias-${cardIdx}`) as HTMLInputElement)?.value.trim()) ||
                   out?.output_alias ||
                   out?.connector?.replace('HDMI-A-','HDMI-') || `HDMI-${cardIdx+1}`;
  preview.textContent = `NDI name: "${devName} (${outAlias})"`;
};

// Store expansion by hardware output identity, independently of editable aliases.
export const outputExpansionState: Map<string, boolean> = new Map();
export let outputExpansionKey = function(out: OutputStatus, idx?: number) {
  return 'output:' + (out.output_index !== undefined ? out.output_index : out.connector || idx);
};
export let outputExpansionOpen = function(key: string, fallback = true): boolean {
  if (outputExpansionState.has(key)) return outputExpansionState.get(key)!;
  if (key.endsWith(':advanced')) return fallback;
  try {
    const saved = localStorage.getItem('ndimon.outputs.expansion.' + key);
    if (saved === 'open' || saved === 'closed') return saved === 'open';
  } catch (_) { /* Storage may be unavailable in private browsing. */ }
  return fallback;
};
export let rememberOutputExpansion = function(details: HTMLDetailsElement) {
  const key = details.dataset.expansionKey;
  if (!key) return;
  outputExpansionState.set(key, details.open);
  if (key.endsWith(':advanced')) return;
  try { localStorage.setItem('ndimon.outputs.expansion.' + key, details.open ? 'open' : 'closed'); } catch (_) {}
};
document.addEventListener('DOMContentLoaded', () => {
  for (const [key, fallback] of [['connections', true], ['presets', false]] as const) {
    const section = document.querySelector<HTMLDetailsElement>('[data-expansion-key="' + key + '"]');
    if (section) section.open = outputExpansionOpen(key, fallback);
  }
});

// Per-output audio drafts stay mounted and are never replaced by status polling.
export const outputAudioDrafts: Map<number, AudioDraft> = new Map();
export let outputAudioCatalog: AudioCatalog = {inputs:[],destinations:[]};
export let outputAudioEngine = 'unknown';
export let outputAudioCatalogPending: Promise<void> | undefined;
export let outputAudioChannel = function(out: Pick<OutputStatus, "output_index">) { return Number(out.output_index) + 1; }
export let outputAudioError = function(result: LegacyResult) {
  if(result.error==='legacy_migration_required') return 'Existing audio cannot be migrated safely into this editor. No settings were changed. '+(result.errors||[]).join(' · ');
  return [result.error, ...(Array.isArray(result.errors) ? result.errors : [])].filter(Boolean).join(' · ') || 'Audio request failed';
};
export let loadOutputAudioCatalog = async function(force=false) {
  if (!force && outputAudioCatalogPending) return outputAudioCatalogPending;
  outputAudioCatalogPending = Promise.all([apiClient.get<AudioCatalog>('/api/v1/catalog/audio_endpoints'), nativeState()]).then(([catalog,state])=>{
    if (catalog.ok === false || !Array.isArray(catalog.inputs) || !Array.isArray(catalog.destinations)) throw Error(outputAudioError(catalog));
    outputAudioCatalog=catalog; outputAudioEngine=state.audio.engine;
  }).catch(error=>{outputAudioCatalogPending=undefined;throw error;});
  return outputAudioCatalogPending;
};
export let outputAudioDraft = function(data: AudioReply) {
  return {native:data.native,revision:data.revision,legacy_matrix:data.legacy_matrix===true,warning:data.warning||'',enabled:data.enabled===true,inputs:(data.inputs||[]).map(row=>({...row})),destinations:(data.destinations||[]).map(row=>({...row})),loaded:true,dirty:false,busy:false,error:''};
};
export let validateOutputAudio = function(draft: AudioDraft) {
  if (draft.enabled && (!draft.inputs.length || !draft.destinations.length)) throw Error('Select at least one input and one destination before enabling this output.');
  for (const kind of ['inputs','destinations'] as const) {
    const ids=new Set();
    for (const row of draft[kind]) {
      if (!row.endpoint) throw Error('Choose an endpoint for every '+(kind==='inputs'?'input':'destination')+'.');
      if (ids.has(row.endpoint)) throw Error('Each endpoint may be selected only once in this output.');
      ids.add(row.endpoint);
      if (!Number.isInteger(row.delay_ms) || row.delay_ms<0 || row.delay_ms>5000) throw Error('Every audio delay must be a whole number from 0 to 5000 ms.');
      if (kind==='inputs' && (!Number.isFinite((row as AudioInput).gain_db) || (row as AudioInput).gain_db < -60 || (row as AudioInput).gain_db > 12)) throw Error('Input gain must be between −60 and +12 dB.');
    }
  }
  if(draft.enabled)for(const input of draft.inputs)for(const destination of draft.destinations)if(input.delay_ms+destination.delay_ms>5000)throw Error('Input delay plus destination delay must not exceed 5000 ms for any route.');
};
export let outputAudioPayload = function(draft: AudioDraft) {
  validateOutputAudio(draft);
  return {revision:draft.revision,enabled:draft.enabled,inputs:draft.inputs.map(({endpoint,gain_db,delay_ms})=>({endpoint,gain_db,delay_ms})),destinations:draft.destinations.map(({endpoint,delay_ms})=>({endpoint,delay_ms}))};
};
export let mountOutputAudio = async function(out: Pick<OutputStatus, "output_index">) {
  const ch=outputAudioChannel(out), pane=(document.getElementById('output-audio-'+ch) as HTMLElement);
  if (!pane || !Number.isInteger(ch) || ch<1) return;
  let draft=outputAudioDrafts.get(ch);
  if (draft?.loaded) {renderOutputAudio(ch);return;}
  if (draft?.loading) return;
  draft={loading:true,error:''} as AudioDraft;outputAudioDrafts.set(ch,draft);
  pane.textContent='Loading audio settings…';
  try {
    const [,data]=await Promise.all([loadOutputAudioCatalog(),nativeAudio(ch)]);
    if (data.ok===false || !Array.isArray(data.inputs) || !Array.isArray(data.destinations)) throw Error(outputAudioError(data));
    outputAudioDrafts.set(ch,outputAudioDraft(data));
  } catch(error) {outputAudioDrafts.set(ch,{loaded:false,error:(error as Error).message} as AudioDraft);}
  renderOutputAudio(ch);
};
export let renderOutputAudio = function(ch: number) {
  const pane=(document.getElementById('output-audio-'+ch) as HTMLElement), draft=outputAudioDrafts.get(ch);
  if (!pane || !draft) return;
  pane.replaceChildren();
  const node=<K extends keyof HTMLElementTagNameMap>(tag: K, text?: string, className?: string)=>{const el=document.createElement(tag);if(text!==undefined)el.textContent=text;if(className)el.className=className;return el;};
  const button=(text: string, handler: () => void, className='btn btn-outline-secondary btn-sm')=>{const el=node('button',text,className);el.type='button';el.disabled=!!draft.busy;el.onclick=handler;return el;};
  const message=node('div',draft.error||'','alert alert-danger');message.hidden=!draft.error;message.setAttribute('role','alert');pane.append(message);
  if(draft.stale) pane.append(node('div','Audio changed in another session or on the device. Reload saved audio to see the current settings; saving these edits would be refused.','alert alert-warning'));
  // With the control plane, routes and engine are one change (field incident #5).
  const v1Note=globalThis.ndimonCsrf?' · Saving enabled routes switches to audio engine v2; the device asks before restarting the decoder.':' · Routing can be saved now; it becomes active only after explicitly switching to engine v2.';
  pane.append(node('p','Audio engine: '+outputAudioEngine+(outputAudioEngine==='v1'?v1Note:''),'form-text'));
  const targetEngine=outputAudioEngine==='v2'?'v1':'v2';
  pane.append(button(targetEngine==='v2'?'Use experimental audio engine v2…':'Return to audio engine v1…',async()=>{
    const title=targetEngine==='v2'?'Enable audio engine v2':'Return to audio engine v1';
    const message=targetEngine==='v2'?'Switch to experimental audio engine v2 and restart the decoder? Video may pause briefly. Saved routes for enabled outputs will become active.':'Return to audio engine v1 and restart the decoder? Video may pause briefly. Per-output network audio routes will become inactive; saved selections are kept.';
    if (!await nativeConfirm(title,message,{label:'Switch and restart'})) return;
    draft.busy=true;renderOutputAudio(ch);
    try {const result=await nativeCommand({set_audio_engine:{engine:targetEngine}});if(result.ok===false || result.error)throw Error(outputAudioError(result));outputAudioEngine=targetEngine;draft.error='';toast('Audio engine '+targetEngine+' selected; decoder restarting','success');}
    catch(error){draft.error=(error as Error).message;}finally{draft.busy=false;for(const [output] of outputAudioDrafts)renderOutputAudio(output);}
  }));
  if (!draft.loaded) {
    pane.append(button('Retry audio settings',()=>{outputAudioDrafts.delete(ch);mountOutputAudio(liveView().outputs.find(out=>outputAudioChannel(out)===ch)||{output_index:ch-1});}));return;
  }
  if(draft.legacy_matrix) pane.append(node('p',draft.warning||'Existing per-destination gains and mutes are preserved. Edits that would replace this mix are blocked until explicitly reviewed.','alert alert-warning'));
  const edit=()=>{draft.dirty=true;};
  const state=node('p',draft.enabled?'This output’s audio routes are enabled.':'Disabled: this output contributes no audio. Saved selections are retained.','form-text');
  const enableLabel=node('label',undefined,'form-check-label mb-2');const enable=node('input');enable.type='checkbox';enable.className='form-check-input me-2';enable.checked=draft.enabled;enable.disabled=!!draft.busy;
  enable.onchange=()=>{draft.enabled=enable.checked;edit();state.textContent=draft.enabled?'This output’s audio routes will be enabled when saved.':'Disabled: this output contributes no audio. Saved selections are retained.';};
  enableLabel.append(enable,document.createTextNode('Enable audio for this output'));pane.append(enableLabel,state);
  pane.append(node('p','Each input feeds every selected destination. Input delay and destination delay add together. Adjust HDMI and Tessera timing manually; automatic synchronization is not provided.','form-text'));
  for (const kind of ['inputs','destinations'] as const) {
    const input=kind==='inputs';pane.append(node('h6',input?'Audio inputs':'Audio destinations','mt-3'));
    const list=node('div');pane.append(list);
    draft[kind].forEach((row,index)=>{
      const wrapper=node('div',undefined,'border rounded p-2 mb-2');const grid=node('div',undefined,'row g-2 align-items-end');wrapper.append(grid);list.append(wrapper);
      const selectCol=node('div',undefined,'col-md-6');const selectLabel=node('label',(input?'Input':'Destination')+' '+(index+1),'form-label w-100');const select=node('select');select.className='form-select form-select-sm';select.disabled=!!draft.busy;select.required=true;
      const choices=outputAudioCatalog[kind]||[];const blank=node('option','Choose '+(input?'input':'destination')+'…');blank.value='';select.append(blank);
      const options=choices.some(endpoint=>endpoint.id===row.endpoint)||!row.endpoint?choices:[...choices,{id:row.endpoint,label:row.endpoint,ready:false,reason:'Endpoint is absent from the current catalog'}];
      for (const endpoint of options) {const option=node('option',endpoint.label+(endpoint.ready?'':' · unavailable'));option.value=endpoint.id;select.append(option);}
      select.value=row.endpoint;
      const info=node('div',undefined,'form-text');
      const describe=()=>{const endpoint=options.find(endpoint=>endpoint.id===select.value);info.textContent=endpoint?[endpoint.mapping,endpoint.ready?'Ready':('Unavailable: '+(endpoint.reason||'not ready')),endpoint.channels?endpoint.channels+' channel(s)':''].filter(Boolean).join(' · '):'';};describe();
      select.onchange=()=>{row.endpoint=select.value;edit();describe();};selectLabel.append(select);selectCol.append(selectLabel,info);grid.append(selectCol);
      const numeric=(field: 'gain_db' | 'delay_ms', label: string, min: number, max: number, step: number)=>{const col=node('div',undefined,'col-md');const l=node('label',label,'form-label w-100');const el=node('input');el.type='number';el.className='form-control form-control-sm';el.min=String(min);el.max=String(max);el.step=String(step);el.required=true;el.value=(row as AudioInput)[field] as unknown as string;el.disabled=!!draft.busy;el.oninput=()=>{(row as AudioInput)[field]=el.value.trim()===''?NaN:Number(el.value);edit();};l.append(el);col.append(l);grid.append(col);};
      if(input)numeric('gain_db','Gain (dB)',-60,12,0.1);numeric('delay_ms',input?'Input delay (ms)':'Destination delay (ms)',0,5000,1);
      const remove=node('div',undefined,'col-md-auto');const removeButton=button('Remove',()=>{draft[kind].splice(index,1);edit();renderOutputAudio(ch);});removeButton.setAttribute('aria-label','Remove '+(input?'input ':'destination ')+(index+1)+' from output '+ch);remove.append(removeButton);grid.append(remove);
    });
    pane.append(button(input?'Add audio input':'Add audio destination',()=>{(draft[kind] as (AudioInput | AudioDestination)[]).push(input?{endpoint:'',gain_db:0,delay_ms:0}:{endpoint:'',delay_ms:0});edit();renderOutputAudio(ch);}));
  }
  const controls=node('div',undefined,'d-flex gap-2 flex-wrap mt-3');
  controls.append(button('Refresh endpoint names and availability',async()=>{
    draft.busy=true;draft.error='';renderOutputAudio(ch);
    try{await loadOutputAudioCatalog(true);}catch(error){draft.error=(error as Error).message;}finally{draft.busy=false;renderOutputAudio(ch);}
  }));
  controls.append(button('Save output audio',()=>saveOutputAudio(ch),'btn btn-primary btn-sm'),button('Reload saved audio',async()=>{
    if(draft.dirty && !await confirmDialog('Reload saved audio','Discard unsaved audio edits for this output?',{label:'Reload'}))return;
    outputAudioDrafts.delete(ch);outputAudioCatalogPending=undefined;
    try {await store.reload();await mountOutputAudio({output_index:ch-1});}
    catch(error){outputAudioDrafts.set(ch,{loaded:false,error:(error as Error).message} as AudioDraft);renderOutputAudio(ch);}
  }));pane.append(controls);
};
// Audio changed on the device or in another session: clean drafts reload,
// drafts with unsaved edits keep them and say that saving would be refused.
export let refreshAudioFromServer = async function() {
  if (!outputAudioDrafts.size) return;
  try { await loadOutputAudioCatalog(true); } catch {}
  for (const [ch, draft] of outputAudioDrafts) {
    if (!draft.loaded || draft.busy) continue;
    let data; try { data = await nativeAudio(ch); } catch { continue; }
    if (data.ok === false || data.revision === undefined || data.revision === draft.revision) continue;
    if (draft.dirty) draft.stale = true;
    else outputAudioDrafts.set(ch, outputAudioDraft(data));
    renderOutputAudio(ch);
  }
};
export let saveOutputAudio = async function(ch: number) {
  const draft=outputAudioDrafts.get(ch);if(!draft?.loaded || draft.busy)return;
  let body;try{body=outputAudioPayload(draft);}catch(error){draft.error=(error as Error).message;renderOutputAudio(ch);return;}
  draft.busy=true;draft.error='';renderOutputAudio(ch);
  try {
    const audio: OutputAudio = {...(draft.native ?? {primary:false,legacy_matrix:false,definitions:{sources:{},sinks:{}}}),enabled:body.enabled,inputs:body.inputs,destinations:body.destinations};
    const command = {put_output_audio:{output:ch,audio,normalize_matrix:false}};
    let result=await nativeCommand(command,body.revision,{confirm:false}),confirmed=false;
    // The device asks before a change that restarts something (for example the
    // switch to audio engine v2). Show its impact text, then resend with the
    // impact token, so it applies only the change that was shown.
    for(let round=0;round<2 && result.error==='confirmation_required' && result.impact?.token;round++){
      const text=(result.impact.lines||[]).join(' ')||'This change restarts the decoder. Video may pause briefly.';
      if(!await nativeConfirm('Apply audio change',text,{label:'Apply'}))return;
      result=await nativeCommand(command,body.revision,{confirm:false,expect_impact:result.impact.token});confirmed=true;
    }
    if(result.error==='matrix_confirmation_required') throw Error('Save blocked: this edit would replace existing per-destination gains or mutes and could make a muted destination audible. The existing mix is unchanged. '+(result.errors||[]).join(' · '));
    if(result.error==='revision_conflict') draft.stale=true;
    if(result.ok===false || result.error) throw Error((result.error==='revision_conflict'?'Audio changed in another session. Reload saved audio for this output before saving again. ':'')+outputAudioError(result));
    if(result.revision!==undefined) {
      // A successful CAS proves no other writer changed drafts at this revision.
      for(const value of outputAudioDrafts.values())if(value.revision===body.revision)value.revision=result.revision;
    }
    draft.dirty=false;draft.stale=false;
    if(confirmed){try{await loadOutputAudioCatalog(true);}catch{}}
    toast('Output '+ch+' audio saved'+(outputAudioEngine==='v1'?' (engine v1 remains active)':''),'success');
  } catch(error) {draft.error=(error as Error).message;}
  finally {draft.busy=false;renderOutputAudio(ch);}
};

// Build output cards once; update status fields on each tick.
export let builtOutputCount = 0;
export let builtOutputKeys = '';

export let buildOutputCards = function() {
  const container = (document.getElementById('outputs-container') as HTMLElement);
  const outputs = liveView().outputs;
  if (!container || !outputs?.length) return;
  builtOutputCount = outputs.length;
  builtOutputKeys = outputs.map(outputExpansionKey).join('|');

  container.innerHTML = outputs.map((out, idx) => {
    const alias = out.output_alias || out.connector?.replace('HDMI-A-','HDMI-') || `Output ${idx+1}`;
    return `
    <details class="card mb-3" data-card-idx="${idx}" data-expansion-key="${esc(outputExpansionKey(out,idx))}" ${outputExpansionOpen(outputExpansionKey(out,idx))?'open':''} ontoggle="rememberOutputExpansion(this)">
      <summary class="card-header">
        <i class="bi bi-display"></i><span>Output ${idx+1} · <span id="out-title-${idx}">${esc(alias)}</span></span>
        <span class="spacer"></span>
        <span id="out-chips-${idx}" class="d-flex gap-1"></span>
        <span id="out-badge-${idx}" class="chip ${out.connected?'chip-live':'chip-idle'}">${out.connected?'LIVE':'IDLE'}</span>
      </summary>
      <div class="card-body">
        <div class="sub-head"><i class="bi bi-broadcast-pin"></i>Source</div>
        <div class="row g-2 mb-2">
          <div class="col-12">
            <select class="form-select" id="src-sel-${idx}" onchange="onSrcChange(${idx})" aria-label="NDI source for output ${idx+1}">
              <option value="">— Select source —</option>
            </select>
          </div>
        </div>
        <div class="row g-2 mb-2" id="manual-ip-row-${idx}" style="display:none">
          <div class="col-7">
            <label class="form-label" for="manual-name-${idx}">Source Name</label>
            <input type="text" class="form-control form-control-sm" id="manual-name-${idx}" placeholder="MACHINE (Source Name)">
          </div>
          <div class="col-5">
            <label class="form-label" for="manual-ip-${idx}">IP Address <small>(optional)</small></label>
            <input type="text" class="form-control form-control-sm" id="manual-ip-${idx}" placeholder="192.168.1.100">
          </div>
        </div>
        <div class="save-row mb-3">
          <button class="btn btn-primary btn-sm" onclick="applySource(${idx})"><i class="bi bi-plug me-1"></i>Connect</button>
          <button class="btn btn-outline-secondary btn-sm" onclick="doDisconnect(${idx})" id="out-disc-${idx}"><i class="bi bi-x-lg me-1"></i>Disconnect</button>
        </div>

        <div class="row g-0 mb-3" style="background:var(--bg-card2);border:1px solid var(--border);border-radius:var(--radius-sm);padding:4px 12px">
          <div class="col-12"><div class="stat-row"><span class="stat-label">Now showing</span><span class="stat-val" id="out-src-${idx}">${esc(sourceLabel(out))}</span></div></div>
          <div class="col-12"><div class="stat-row"><span class="stat-label">Input → Output</span><span class="stat-val" id="out-res-${idx}">—</span></div></div>
          <div class="col-12"><div class="stat-row"><span class="stat-label">Frame rate · Codec</span><span class="stat-val" id="out-fps-${idx}">—</span></div></div>
        </div>

        <div class="sub-head"><i class="bi bi-tag"></i>Name</div>
        <div class="row g-2 mb-3 align-items-end">
          <div class="col-8">
            <input type="text" class="form-control form-control-sm" id="output-alias-${idx}"
                   value="${esc(out.output_alias||'')}" aria-label="Output name"
                   placeholder="${esc(out.connector?.replace('HDMI-A-','HDMI-')||'HDMI-'+(idx+1))}"
                   oninput="updateNdiPreview(${idx})">
            <div class="form-text" id="ndi-preview-${idx}"></div>
          </div>
          <div class="col-4 d-grid">
            <button class="btn btn-outline-primary btn-sm" onclick="saveOutputName(${idx})"><i class="bi bi-check-lg me-1"></i>Save Name</button>
          </div>
        </div>

        <details class="output-advanced" data-expansion-key="${esc(outputExpansionKey(out,idx))}:advanced" ${outputExpansionOpen(outputExpansionKey(out,idx)+':advanced',false)?'open':''} ontoggle="rememberOutputExpansion(this)">
          <summary>Advanced</summary>
          <div class="advanced-body">
        <div class="sub-head"><i class="bi bi-aspect-ratio"></i>Video</div>
        <div class="row g-2 mb-2">
          <div class="col-7">
            <label class="form-label" for="res-wh-${idx}">Resolution</label>
            <select class="form-select form-select-sm" id="res-wh-${idx}" onchange="onResChange(${idx})"><option>Loading…</option></select>
          </div>
          <div class="col-5">
            <label class="form-label" for="res-fps-${idx}">Refresh Rate</label>
            <select class="form-select form-select-sm" id="res-fps-${idx}"><option>—</option></select>
          </div>
        </div>
        <div class="row g-2 mb-3">
          <div class="col-8">
            <label class="form-label" for="scale-sel-${idx}">Scale Mode</label>
            <select class="form-select form-select-sm" id="scale-sel-${idx}">
              <option value="letterbox">Letterbox (keep aspect ratio)</option>
              <option value="stretch">Stretch (fill screen)</option>
              <option value="crop">Crop (fill, no bars)</option>
            </select>
          </div>
          <div class="col-4">
            <label class="form-label" for="rotation-sel-${idx}">Rotation</label>
            <select class="form-select form-select-sm" id="rotation-sel-${idx}">
              <option value="0">0&deg;</option>
              <option value="90">90&deg;</option>
              <option value="180">180&deg;</option>
              <option value="270">270&deg;</option>
            </select>
          </div>
        </div>
        <div class="save-row">
          <button class="btn btn-primary btn-sm" onclick="saveVideoSettings(${idx})"><i class="bi bi-check-lg me-1"></i>Save Video Settings</button>
          <span id="out-msg-${idx}" class="save-msg"></span>
        </div>
        <hr class="section-divider">
        <div class="sub-head"><i class="bi bi-volume-up"></i>Audio</div>
        <div id="output-audio-${outputAudioChannel(out)}">Loading audio settings…</div>
          </div>
        </details>
      </div>
    </details>`;
  }).join('');

  // Init each card
  outputs.forEach((out, idx) => {
    acceptNativeRevision('alias:'+out.output_index,store.state?.revision);
    acceptNativeRevision('video:'+out.output_index,store.state?.revision);
    setVal(`scale-sel-${idx}`, out.scale_mode || 'letterbox');
    setVal(`rotation-sel-${idx}`, String(out.rotation || 0));
    populateOutputSrcSelector(idx);
    loadOutputModes(idx, out.output_index);
    updateNdiPreview(idx);
    mountOutputAudio(out);
  });
};

export let updateOutputCards = function() {
  const view = liveView();
  if (!view.outputs?.length) return;
  // Rebuild if output count changed
  if (builtOutputCount !== view.outputs.length || builtOutputKeys !== view.outputs.map(outputExpansionKey).join('|')) { buildOutputCards(); return; }
  view.outputs.forEach((out, idx) => {
    // Badge + chips
    const badge = (document.getElementById(`out-badge-${idx}`) as HTMLElement);
    if (badge) { badge.className = `chip ${out.connected?'chip-live':'chip-idle'}`; badge.textContent = out.connected?'LIVE':'IDLE'; }
    const chips = (document.getElementById(`out-chips-${idx}`) as HTMLElement);
    if (chips) chips.innerHTML = decodeChips(out);
    const disc = (document.getElementById(`out-disc-${idx}`) as HTMLButtonElement);
    if (disc) disc.disabled = !out.connected && !out.source_name;
    // Status rows (read-only labels — never touch form controls like dropdowns)
    setText(`out-src-${idx}`, sourceLabel(out));
    const inStr = out.input_width ? `${out.input_width}×${out.input_height}` : '—';
    const outStr = out.width ? `${out.width}×${out.height} @ ${fmtHz(out.refresh_hz||out.refresh)}` : '—';
    setText(`out-res-${idx}`, `${inStr} → ${outStr}`);
    setText(`out-fps-${idx}`, (out.fps > 0 ? out.fps.toFixed(1)+' fps' : '—') + ' · ' + fmtCodec(out.codec));
    // Do NOT call populateOutputSrcSelector here — it overwrites the user's
    // dropdown selection every 1.5s, making it impossible to change source.
    // Source list is only rebuilt when sources actually change (see below).
  });
  // Only rebuild source dropdowns when the source list changes
  const srcKey = JSON.stringify(view.sources);
  if (srcKey !== _lastSrcKey) {
    _lastSrcKey = srcKey;
    view.outputs.forEach((out, idx) => populateOutputSrcSelector(idx));
    populatePresetSourceSelect();
  }
};
export let _lastSrcKey = '';

export let populateOutputSrcSelector = function(cardIdx: number) {
  const sel = (document.getElementById(`src-sel-${cardIdx}`) as HTMLSelectElement);
  if (!sel) return;
  const view = liveView();
  const cur = view.outputs[cardIdx]?.source_name || '';
  // Preserve user's current dropdown selection across rebuilds
  const prev = sel.value || cur;
  const isManual = prev === '__manual__';
  sel.innerHTML = '';
  // "None" disconnects the output
  const noneOpt = document.createElement('option');
  noneOpt.value = 'None'; noneOpt.textContent = 'None (Disconnect)';
  sel.appendChild(noneOpt);
  // Discovered sources
  let foundPrev = (prev === 'None');
  view.sources.forEach(s => {
    const o = document.createElement('option');
    o.value = s; o.textContent = s;
    sel.appendChild(o);
    if (s === prev) foundPrev = true;
  });
  // Manual entry option
  const manOpt = document.createElement('option');
  manOpt.value = '__manual__'; manOpt.textContent = 'Manual...';
  sel.appendChild(manOpt);
  if (isManual) foundPrev = true;
  // If the saved selection isn't in the discovered list, add it
  if (!foundPrev && prev && prev !== 'None') {
    const curOpt = document.createElement('option');
    curOpt.value = prev; curOpt.textContent = prev + ' (not discovered)';
    sel.insertBefore(curOpt, manOpt);
  }
  // Restore selection
  sel.value = prev;
  onSrcChange(cardIdx);
};

export let onSrcChange = function(cardIdx: number) {
  const sel = (document.getElementById(`src-sel-${cardIdx}`) as HTMLSelectElement);
  const manRow = (document.getElementById(`manual-ip-row-${cardIdx}`) as HTMLInputElement);
  if (manRow) manRow.style.display = (sel?.value === '__manual__') ? '' : 'none';
};



export let saveOutputName = async function(cardIdx: number) {
  const out = liveView().outputs[cardIdx];
  const outputIndex = out?.output_index ?? cardIdx;
  const outAlias = ((document.getElementById(`output-alias-${cardIdx}`) as HTMLInputElement)?.value || '').trim();
  const r = await nativeCommand({set_output_alias:{output:outputIndex+1,alias:outAlias}},await nativeRevision('alias:'+outputIndex));
  if (r.ok) acceptNativeRevision('alias:'+outputIndex,r.revision);
  done(r, `Output ${cardIdx+1}: name saved`);
};

export let applySource = async function(cardIdx: number) {
  const view = liveView();
  const out = view.outputs[cardIdx];
  const outputIndex = out?.output_index ?? cardIdx;
  const srcSel = (document.getElementById(`src-sel-${cardIdx}`) as HTMLSelectElement);
  const srcVal = srcSel?.value || '';
  if (srcVal === '__manual__') {
    const manName = ((document.getElementById(`manual-name-${cardIdx}`) as HTMLInputElement)?.value || '').trim();
    const manIP = ((document.getElementById(`manual-ip-${cardIdx}`) as HTMLInputElement)?.value || '').trim();
    if (!manName) { toast('Enter a source name for manual connect', 'warning'); return; }
    const r = await nativeCommand(manName==='None'?{forget_source:{output:outputIndex+1}}:{select_source:{output:outputIndex+1,source:{name:manName,endpoint:manIP}}});
    done(r, `Output ${cardIdx+1}: connecting to ${manName}…`);
  } else if (!srcVal) {
    toast('Pick a source first', 'warning');
  } else {
    const r = await nativeCommand(srcVal==='None'?{forget_source:{output:outputIndex+1}}:{select_source:{output:outputIndex+1,source:{name:srcVal,endpoint:view.sourceIPs[srcVal]||''}}});
    done(r, srcVal === 'None' ? `Output ${cardIdx+1}: disconnected` : `Output ${cardIdx+1}: connecting to ${srcVal}…`);
  }
};

export let saveVideoSettings = async function(cardIdx: number) {
  const out = liveView().outputs[cardIdx];
  const outputIndex = out?.output_index ?? cardIdx;
  const msg = (document.getElementById(`out-msg-${cardIdx}`) as HTMLElement);
  if (msg) msg.textContent = 'Applying…';

  // Resolution
  const whSel  = (document.getElementById(`res-wh-${cardIdx}`) as HTMLSelectElement);
  const fpsSel = (document.getElementById(`res-fps-${cardIdx}`) as HTMLSelectElement);
  const whOpt  = whSel?.options[whSel.selectedIndex];
  const fpsOpt = fpsSel?.options[fpsSel.selectedIndex];
  const results = [];
  let revision = await nativeRevision('video:'+outputIndex);
  let video: OutputVideo = {...((await nativeState()).outputs[outputIndex+1]?.video ?? {resolution:'auto',scale_mode:'letterbox',rotation:0})};
  const saveVideo = async (next: OutputVideo) => {
    const result = await nativeCommand({set_output_video:{output:outputIndex+1,video:next}},revision);
    if (result.ok && result.revision !== undefined) { video=next; revision=result.revision; acceptNativeRevision('video:'+outputIndex,revision); }
    return result;
  };
  if (whOpt?.dataset.auto === '1') {
    results.push(await saveVideo({...video,resolution:'auto'}));
  } else if (whOpt?.dataset.w && fpsOpt) {
    const hz = parseFloat(fpsOpt.dataset.hz || fpsOpt.value);
    results.push(await saveVideo({...video,resolution:{mode:{width:+whOpt.dataset.w,height:+whOpt.dataset.h!,refresh_hz:hz}}}));
  }

  // Scale mode
  const scale = (document.getElementById(`scale-sel-${cardIdx}`) as HTMLSelectElement)?.value || 'letterbox';
  results.push(await saveVideo({...video,scale_mode:scale as ScaleMode}));

  // Rotation
  const rotation = parseInt((document.getElementById(`rotation-sel-${cardIdx}`) as HTMLSelectElement)?.value || '0', 10);
  results.push(await saveVideo({...video,rotation}));

  const bad = results.find(r => r && r.ok === false);
  if (msg) msg.textContent = bad ? 'Failed' : 'Saved';
  done(bad || {}, `Output ${cardIdx+1}: video settings saved`);
};


// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('loadInitialData', () => loadInitialData, value => { loadInitialData = value; });
expose('loadVersionTag', () => loadVersionTag, value => { loadVersionTag = value; });
expose('loadSourceIPs', () => loadSourceIPs, value => { loadSourceIPs = value; });
expose('fmtHz', () => fmtHz, value => { fmtHz = value; });
expose('outputModes', () => outputModes);
expose('loadOutputModes', () => loadOutputModes, value => { loadOutputModes = value; });
expose('populateOutputRefreshRates', () => populateOutputRefreshRates, value => { populateOutputRefreshRates = value; });
expose('onResChange', () => onResChange, value => { onResChange = value; });
expose('updateNdiPreview', () => updateNdiPreview, value => { updateNdiPreview = value; });
expose('outputExpansionState', () => outputExpansionState);
expose('outputExpansionKey', () => outputExpansionKey, value => { outputExpansionKey = value; });
expose('outputExpansionOpen', () => outputExpansionOpen, value => { outputExpansionOpen = value; });
expose('rememberOutputExpansion', () => rememberOutputExpansion, value => { rememberOutputExpansion = value; });
expose('outputAudioDrafts', () => outputAudioDrafts);
expose('outputAudioCatalog', () => outputAudioCatalog, value => { outputAudioCatalog = value; });
expose('outputAudioEngine', () => outputAudioEngine, value => { outputAudioEngine = value; });
expose('outputAudioCatalogPending', () => outputAudioCatalogPending, value => { outputAudioCatalogPending = value; });
expose('outputAudioChannel', () => outputAudioChannel, value => { outputAudioChannel = value; });
expose('outputAudioError', () => outputAudioError, value => { outputAudioError = value; });
expose('loadOutputAudioCatalog', () => loadOutputAudioCatalog, value => { loadOutputAudioCatalog = value; });
expose('outputAudioDraft', () => outputAudioDraft, value => { outputAudioDraft = value; });
expose('validateOutputAudio', () => validateOutputAudio, value => { validateOutputAudio = value; });
expose('outputAudioPayload', () => outputAudioPayload, value => { outputAudioPayload = value; });
expose('mountOutputAudio', () => mountOutputAudio, value => { mountOutputAudio = value; });
expose('renderOutputAudio', () => renderOutputAudio, value => { renderOutputAudio = value; });
expose('refreshAudioFromServer', () => refreshAudioFromServer, value => { refreshAudioFromServer = value; });
expose('saveOutputAudio', () => saveOutputAudio, value => { saveOutputAudio = value; });
expose('builtOutputCount', () => builtOutputCount, value => { builtOutputCount = value; });
expose('builtOutputKeys', () => builtOutputKeys, value => { builtOutputKeys = value; });
expose('buildOutputCards', () => buildOutputCards, value => { buildOutputCards = value; });
expose('updateOutputCards', () => updateOutputCards, value => { updateOutputCards = value; });
expose('_lastSrcKey', () => _lastSrcKey, value => { _lastSrcKey = value; });
expose('populateOutputSrcSelector', () => populateOutputSrcSelector, value => { populateOutputSrcSelector = value; });
expose('onSrcChange', () => onSrcChange, value => { onSrcChange = value; });
expose('saveOutputName', () => saveOutputName, value => { saveOutputName = value; });
expose('applySource', () => applySource, value => { applySource = value; });
expose('saveVideoSettings', () => saveVideoSettings, value => { saveVideoSettings = value; });
