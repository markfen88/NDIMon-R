import { nativeState, nativeCommand } from './native-page';
import { expose } from './legacy-globals';
import { liveView } from './live';
import type { LegacyPreset } from './legacy-types';

// ══ Source Presets ══════════════════════════════════════════════
export let _presets: LegacyPreset[] = [];

export let loadPresets = async function() {
  try {
    const state = await nativeState();
    _presets = Object.entries(state.presets).filter((entry): entry is [string, NonNullable<typeof entry[1]>]=>!!entry[1])
      .sort((a,b)=>(a[1].position??Number.MAX_SAFE_INTEGER)-(b[1].position??Number.MAX_SAFE_INTEGER))
      .map(([name,preset])=>({name,source:preset.source.name,ip:preset.source.endpoint}));
  } catch { _presets = []; }
  renderPresets();
  populatePresetSourceSelect();
};

export let populatePresetSourceSelect = function() {
  const sel = (document.getElementById('preset-src') as HTMLSelectElement);
  if (!sel) return;
  const prev = sel.value;
  sel.innerHTML = '';
  liveView().sources.forEach(s => {
    const o = document.createElement('option');
    o.value = s; o.textContent = s;
    sel.appendChild(o);
  });
  if (prev) sel.value = prev;
};

export let outputOptionsHtml = function() {
  const outs = liveView().outputs || [];
  if (!outs.length) return '<option value="1">Output 1</option>';
  return outs.map((o, i) => {
    const label = o.output_alias || o.connector || `Output ${i+1}`;
    return `<option value="${(o.output_index ?? i) + 1}">${esc(label)}</option>`;
  }).join('');
};

export let renderPresets = function() {
  const el = (document.getElementById('preset-list') as HTMLElement);
  if (!el) return;
  if (!_presets.length) { el.innerHTML = '<div class="empty"><i class="bi bi-bookmark"></i>No presets saved.</div>'; return; }
  const multi = (liveView().outputs || []).length > 1;
  el.innerHTML = _presets.map((p, i) => `
    <div class="source-item" style="gap:.5rem">
      <span class="src-name" style="line-height:1.25">
        <strong>${esc(p.name)}</strong>
        <span class="form-text" style="display:block;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;margin:0">${esc(p.source)}</span>
      </span>
      <select class="form-select form-select-sm" id="preset-out-${i}" style="width:auto;${multi ? '' : 'display:none'}" aria-label="Output">${outputOptionsHtml()}</select>
      <button class="btn btn-primary btn-sm" onclick="recallPreset(${i})" title="Recall"><i class="bi bi-play-fill"></i></button>
      <button class="btn btn-outline-danger btn-sm" onclick="deletePreset(${i})" title="Delete"><i class="bi bi-trash"></i></button>
    </div>`).join('');
};

export let savePreset = async function() {
  const name = ((document.getElementById('preset-name') as HTMLInputElement)?.value || '').trim();
  const source = (document.getElementById('preset-src') as HTMLSelectElement)?.value || '';
  if (!name)   { toast('Enter a preset name', 'warning'); return; }
  if (!source) { toast('No source selected', 'warning'); return; }
  const r = await nativeCommand({save_preset:{name,preset:{source:{name:source,endpoint:liveView().sourceIPs[source]||''}}}});
  if (r.ok) { (document.getElementById('preset-name') as HTMLInputElement).value = ''; await loadPresets(); toast('Preset saved'); }
  else toast('Save failed: ' + (r.error || 'unknown'), 'danger');
};

export let recallPreset = async function(idx: number) {
  const p = _presets[idx];
  if (!p) return;
  const outSel = (document.getElementById(`preset-out-${idx}`) as HTMLSelectElement);
  const output = parseInt(outSel?.value || '1', 10);
  const r = await nativeCommand({recall_preset:{name:p.name,output}});
  if (r.ok) toast(`Recalled "${p.name}" → output ${output}`);
  else toast('Recall failed: ' + (r.error || 'unknown'), 'danger');
};

export let deletePreset = async function(idx: number) {
  const p = _presets[idx];
  if (!p) return;
  if (!(await confirmDialog('Delete preset', `Delete the preset "${p.name}"?`, { danger: true, label: 'Delete' }))) return;
  const r = await nativeCommand({delete_preset:{name:p.name}});
  if (r.ok) { await loadPresets(); toast('Preset deleted'); }
};


// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('_presets', () => _presets, value => { _presets = value; });
expose('loadPresets', () => loadPresets, value => { loadPresets = value; });
expose('populatePresetSourceSelect', () => populatePresetSourceSelect, value => { populatePresetSourceSelect = value; });
expose('outputOptionsHtml', () => outputOptionsHtml, value => { outputOptionsHtml = value; });
expose('renderPresets', () => renderPresets, value => { renderPresets = value; });
expose('savePreset', () => savePreset, value => { savePreset = value; });
expose('recallPreset', () => recallPreset, value => { recallPreset = value; });
expose('deletePreset', () => deletePreset, value => { deletePreset = value; });
