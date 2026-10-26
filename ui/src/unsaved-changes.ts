import { expose } from './legacy-globals';
import { liveView } from './live';

// ══ Unsaved-changes marker ═══════════════════════════════════════════
// Any edit inside a card that has a primary (save/apply) button marks the card
// dirty; pressing that button clears it. Controls that save themselves on
// change (their onchange calls a save function) are left out.
export let markDirty = function(el: Element | null) { el?.closest?.('.content .card')?.classList.add('dirty'); }
document.addEventListener('DOMContentLoaded', () => {
  const selfSaving = (t: HTMLElement) => /save|apply/i.test(t.getAttribute?.('onchange') || '');
  const onEdit = (e: Event) => {
    const card = (e.target as HTMLElement).closest?.('.content .card');
    if (card && card.querySelector('.btn-primary') && !selfSaving(e.target as HTMLElement)) card.classList.add('dirty');
  };
  document.addEventListener('input', onEdit);
  document.addEventListener('change', onEdit);
  document.addEventListener('click', e => {
    if ((e.target as HTMLElement).closest?.('.btn-primary, .btn-outline-primary')) (e.target as HTMLElement).closest('.content .card')?.classList.remove('dirty');
  });
});

export let updateNDIState = function() {
  // Build per-output connection status
  const el = (document.getElementById('ndi-connections') as HTMLElement);
  const view = liveView();
  if (el && view.outputs?.length) {
    el.innerHTML = view.outputs.map((out, idx) => {
      const badge = out.connected
        ? '<span class="chip chip-live">LIVE</span>'
        : '<span class="chip chip-idle">IDLE</span>';
      const label = out.output_alias || `Output ${idx+1}`;
      const src = out.source_name || 'None';
      const res = out.input_width ? `${out.input_width}×${out.input_height}` : '—';
      const fps = out.fps > 0 ? out.fps.toFixed(1)+' fps' : '—';
      const codec = fmtCodec(out.codec);
      const dec = decodeChips(out);
      return `<div class="col-12 col-md-6 connection-summary">
        <div class="stat-row"><span class="sub-head mb-0"><i class="bi bi-display"></i>${esc(label)}</span><span class="d-flex gap-1 align-items-center">${dec}${badge}</span></div>
        <div class="stat-row"><span class="stat-label">Source</span><span class="stat-val" title="${esc(src)}">${esc(src)}</span></div>
        <div class="stat-row"><span class="stat-label">Input</span><span class="stat-val">${res}</span></div>
        <div class="stat-row"><span class="stat-label">Frame rate · Codec</span><span class="stat-val">${fps} · ${codec}</span></div>
      </div>`;
    }).join('');
  } else if (el) {
    el.innerHTML = '<div class="empty"><i class="bi bi-hourglass-split"></i>Waiting for the decoder…</div>';
  }
  if (view.decoder) {
    const tally = view.decoder.tally === 'TallyOn' ? 'TallyOn' : 'TallyOff';
    setVal('tally-sel', tally);
    setVal('ndi-audio-sel', view.decoder.audio);
  }
  // Active HX decode backend hint (Decoder settings card)
  if (view.outputs?.length) {
    const backends = [...new Set(view.outputs.filter(o => o.connected && o.decode_backend && o.decode_backend !== 'none')
                                          .map(o => (o.hw_decode ? 'HW ' : 'SW ') + o.decode_backend))];
    setText('decode-backend-hint', 'Active backend: ' + (backends.length ? backends.join(', ') : '—'));
  }
};

export let renderTemps = function(id: string) {
  const el = (document.getElementById(id) as HTMLElement);
  if (!el) return;
  const t = liveView().temps||{}, keys=Object.keys(t);
  if (!keys.length) { el.innerHTML='<div class="form-text" style="grid-column:1/-1">No temperature sensors</div>'; return; }
  el.innerHTML = keys.map(k => {
    const v=t[k], cls=v>75?'temp-hot':v>60?'temp-warm':'temp-ok';
    const lbl=k.replace('thermal','').replace(/_/g,' ').trim()||k;
    return `<div class="temp-chip"><div class="temp-val ${cls}">${v.toFixed(0)}°C</div><div class="temp-lbl">${esc(lbl)}</div></div>`;
  }).join('');
};


// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('markDirty', () => markDirty, value => { markDirty = value; });
expose('updateNDIState', () => updateNDIState, value => { updateNDIState = value; });
expose('renderTemps', () => renderTemps, value => { renderTemps = value; });
