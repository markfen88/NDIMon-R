import { expose } from './legacy-globals';
import { liveView } from './live';
import type { OutputStatus } from './legacy-types';

// ══ UI update ════════════════════════════════════════════════════
export let updateUI = function() {
  updateTopbar();
  updateDash();
  updateNDIState();
  renderTemps('dash-temps');
  renderTemps('sys-temps');
  const view = liveView();
  setText('sys-up', view.uptime||'—');
  setText('sys-plat', view.platform||'—');
  updateOutputCards();
};

export let liveSummary = function() {
  const outs = liveView().outputs || [];
  const live = outs.filter(o => o.connected);
  return { outs, live, sat: outs.some(o => o.decode_saturated) };
};

export let updateTopbar = function() {
  const { outs, live, sat } = liveSummary();
  const view = liveView();
  const anyLive = live.length > 0 || view.connected;
  const pill = (document.getElementById('topbar-pill') as HTMLElement);
  pill.className = 'status-pill ' + (sat ? 'warn' : anyLive ? 'live' : 'idle');
  const pillText = outs.length > 1
    ? (anyLive ? `${live.length}/${outs.length} live` : 'Idle')
    : (anyLive ? 'Live' : 'Idle');
  pill.innerHTML = `<div class="dot"></div><span>${esc(pillText)}</span>`;
  if (outs.length > 1) {
    setText('topbar-src-label', 'Sources');
    setText('topbar-src', live.length ? live.map(o => o.source_name).join(' · ') : '—');
  } else {
    setText('topbar-src-label', 'Source');
    setText('topbar-src', view.source_name || '—');
  }
  const res = view.display_width ? `${view.display_width}×${view.display_height} @ ${fmtHz(view.display_refresh_hz||view.display_refresh)}` : '—';
  const resEl = document.querySelector('#topbar-res span');
  if (resEl) resEl.textContent = res;
  setText('topbar-device', view.device_name || '—');
  const nc = (document.getElementById('nav-live-count') as HTMLElement);
  if (nc) { nc.style.display = live.length ? '' : 'none'; nc.textContent = live.length as unknown as string; }
};

export let decodeChips = function(out: OutputStatus) {
  if (!out.connected) return '';
  const chips = [];
  if (out.decode_backend && out.decode_backend !== 'none') {
    chips.push(`<span class="chip ${out.hw_decode ? 'chip-blue' : 'chip-muted'}" title="${out.hw_decode ? 'Hardware' : 'Software'} decode (${esc(out.decode_backend)})">${out.hw_decode ? 'HW' : 'SW'} ${esc(out.decode_backend)}</span>`);
  }
  if (out.decode_saturated) {
    chips.push(`<span class="chip chip-warn" title="Decoder can't keep up with the source">SATURATED${out.decode_fps ? ' ' + Math.round(out.decode_fps) + ' fps' : ''}</span>`);
  }
  return chips.join('');
};

export let outputLabel = function(out: OutputStatus, idx: number) {
  return out.output_alias || out.connector?.replace('HDMI-A-', 'HDMI-') || `Output ${idx+1}`;
};

export let _lastTilesKey = '';
export let updateDash = function() {
  // Per-output tiles
  const tiles = (document.getElementById('dash-outputs') as HTMLElement);
  const view = liveView();
  const outs = view.outputs || [];
  if (tiles) {
    const key = JSON.stringify(outs.map(o => [o.connected, o.source_name, o.input_width, o.input_height, o.width, o.height,
      o.refresh_hz || o.refresh, o.codec, o.decode_backend, o.hw_decode, o.decode_saturated, o.output_alias]));
    // Frame rate changes every tick; update it in place so the tiles don't re-render constantly.
    outs.forEach((o, idx) => setText(`tile-fps-${idx}`, o.fps > 0 ? o.fps.toFixed(1) + ' fps' : '—'));
    if (key !== _lastTilesKey) {
      _lastTilesKey = key;
      tiles.innerHTML = outs.length ? outs.map((o, idx) => {
        const state = o.decode_saturated ? 'sat' : o.connected ? 'live' : 'idle';
        const inRes = o.input_width ? `${o.input_width}×${o.input_height}` : '—';
        const outRes = o.width ? `${o.width}×${o.height}` : '—';
        const hz = o.width ? fmtHz(o.refresh_hz || o.refresh) : '';
        return `<div class="tile ${state}">
          <div class="tile-head">
            <span class="tile-name"><i class="bi bi-display"></i>${esc(outputLabel(o, idx))}</span>
            <span class="chip ${o.connected ? 'chip-live' : 'chip-idle'}">${o.connected ? 'LIVE' : 'IDLE'}</span>
          </div>
          <div class="tile-src" title="${esc(o.source_name || '')}">${o.connected || o.source_name ? esc(o.source_name) : 'No source'}</div>
          <div class="tile-grid">
            <div><div class="k">Input</div><div class="v">${inRes}</div></div>
            <div><div class="k">Output</div><div class="v" title="${esc(hz)}">${outRes}</div></div>
            <div><div class="k">Frame rate</div><div class="v" id="tile-fps-${idx}">${o.fps > 0 ? o.fps.toFixed(1) + ' fps' : '—'}</div></div>
          </div>
          <div class="tile-chips">
            ${o.connected ? `<span class="chip chip-muted">${esc(fmtCodec(o.codec))}</span>` : ''}
            ${hz ? `<span class="chip chip-muted">${esc(hz)}</span>` : ''}
            ${decodeChips(o)}
          </div>
        </div>`;
      }).join('') : '<div class="tile idle"><div class="tile-head"><span class="tile-name"><i class="bi bi-display"></i>Output</span><span class="chip chip-idle">—</span></div><div class="tile-src">Waiting for the decoder…</div></div>';
    }
  }

  setText('dash-plat', view.platform||'—');
  const dp = view.total_frames>0 ? (view.dropped_frames/view.total_frames*100).toFixed(1) : '0.0';
  setText('dash-frames', view.total_frames ? `${view.total_frames.toLocaleString()} / ${view.dropped_frames.toLocaleString()} (${dp}%)` : '—');

  const cpu=view.cpu_percent||0, mem=(view.memory||{}).percent||0;
  setText('dash-cpu-pct', cpu+'%');
  setBar('dash-cpu-bar', cpu);
  setText('dash-mem-pct', mem+'% · '+(view.memory.used_mb||0)+' / '+(view.memory.total_mb||0)+' MB');
  setBar('dash-mem-bar', mem, true);
  setText('dash-uptime', view.uptime||'—');
  setText('dash-audio', view.audio_enabled?'Enabled':'Disabled');
  // Decode saturation banner (any output behind its source)
  const satWarn = (document.getElementById('sat-warning') as HTMLElement);
  if (satWarn) satWarn.style.display = outs.some(o => o.decode_saturated) ? '' : 'none';
  // System health bars
  setText('sys-cpu-pct', cpu+'%'); setBar('sys-cpu-bar', cpu);
  setText('sys-mem-pct', mem+'%'); setBar('sys-mem-bar', mem, true);

  // Output picker for the dashboard source list
  const wrap = (document.getElementById('dash-target-wrap') as HTMLElement);
  const sel = (document.getElementById('dash-target-out') as HTMLSelectElement);
  if (wrap && sel) {
    wrap.style.display = outs.length > 1 ? '' : 'none';
    const html = outputOptionsHtml();
    if (sel.dataset.html !== html) { const prev = sel.value; sel.innerHTML = html; sel.dataset.html = html; if (prev) sel.value = prev; }
  }

  // Source list
  setText('dash-src-count', view.sources.length);
  const sl = (document.getElementById('dash-src-list') as HTMLElement);
  if (sl) {
    const liveBy: Record<string, string[]> = {};
    outs.forEach((o, idx) => { if (o.connected && o.source_name) (liveBy[o.source_name] ||= []).push(outputLabel(o, idx)); });
    const srcKey = JSON.stringify([view.sources, liveBy, view.sourceIPs]);
    if (sl.dataset.key !== srcKey) {
      sl.dataset.key = srcKey;
      sl.className = '';
      if (!view.sources.length) {
        sl.innerHTML = '<div class="empty"><i class="bi bi-wifi-off"></i>No NDI sources found. Check the network, groups, or discovery server.</div>';
      } else {
        sl.innerHTML = view.sources.map(s => {
          const on = liveBy[s];
          return `<div class="source-item ${on ? 'active-src' : ''}">
            <span class="src-dot ${on ? 'on' : ''}"></span>
            <span class="src-name" title="${esc(s)}">${esc(s)}${view.sourceIPs[s] ? ` <span class="src-ip">${esc(view.sourceIPs[s])}</span>` : ''}</span>
            ${on ? `<span class="src-tag">ON ${esc(on.join(', ').toUpperCase())}</span>` : ''}
            <button class="btn btn-outline-primary btn-sm" style="padding:2px 10px;font-size:.74rem" onclick="connectFromDash('${esc(s,1)}')"><i class="bi bi-plug me-1"></i>Connect</button>
          </div>`;
        }).join('');
      }
      const filter = (document.getElementById('dash-src-filter') as HTMLInputElement);
      if (filter) filter.style.display = view.sources.length > 8 ? '' : 'none';
      applySourceFilter();
    }
  }
};

// Narrow the dashboard source list by name or address; the filter box only appears past eight sources.
export let applySourceFilter = function() {
  const q = ((document.getElementById('dash-src-filter') as HTMLInputElement)?.value || '').trim().toLowerCase();
  document.querySelectorAll<HTMLElement>('#dash-src-list .source-item').forEach(item => {
    const name = item.querySelector('.src-name')?.textContent.toLowerCase() || '';
    item.style.display = !q || name.includes(q) ? '' : 'none';
  });
};


// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('updateUI', () => updateUI, value => { updateUI = value; });
expose('liveSummary', () => liveSummary, value => { liveSummary = value; });
expose('updateTopbar', () => updateTopbar, value => { updateTopbar = value; });
expose('decodeChips', () => decodeChips, value => { decodeChips = value; });
expose('outputLabel', () => outputLabel, value => { outputLabel = value; });
expose('_lastTilesKey', () => _lastTilesKey, value => { _lastTilesKey = value; });
expose('updateDash', () => updateDash, value => { updateDash = value; });
expose('applySourceFilter', () => applySourceFilter, value => { applySourceFilter = value; });
