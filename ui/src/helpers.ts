import { expose } from './legacy-globals';
import type { LegacyResult } from './legacy-types';

// ══ Helpers ══════════════════════════════════════════════════════
export let handle401 = function(r: Response) { if (r.status === 401) { showLogin(); throw new Error('unauthorized'); } return r; }
export let api = async function<T = LegacyResult>(url: string): Promise<T> { return fetch(url).then(handle401).then(r=>r.json()); }
export let del = async function(url: string) { return post(url, undefined, 'DELETE'); }
// The device asks before a change that restarts or interrupts something:
// 409 confirmation_required with the impact. Show its text, then resend with
// the impact token so only the change that was shown is applied; if the impact
// changed meanwhile it is shown again. Callers that run their own confirmation
// flow pass {confirm:false}.
export let post = async function(url: string, body?: object, method='POST', opts: { confirm?: boolean } = {}): Promise<LegacyResult> {
  let j = await postOnce(url,body,method);
  for (let round=0; opts.confirm!==false && round<2 && j && j.error==='confirmation_required' && j.impact?.token; round++) {
    const text=(j.impact.lines||[]).join(' ')||'This change briefly interrupts the device.';
    if (!await confirmDialog('Apply change',text,{label:'Apply'})) return { ok: false, error: 'Not applied', cancelled: true };
    j = await postOnce(url,{...(body||{}),expect_impact:j.impact.token},method);
  }
  return j;
};
export let postOnce = async function(url: string, body?: object, method='POST'): Promise<LegacyResult> {
  try {
    const r = await fetch(url,{method,headers:{'Content-Type':'application/json'},body:JSON.stringify(body)}).then(handle401);
    const j: LegacyResult = await r.json().catch(() => ({}));
    if (!r.ok && j.ok !== false) return { ok: false, error: j.error || `HTTP ${r.status}` };
    return j;
  } catch (e) {
    if (e && (e as Error).message === 'unauthorized') return { ok: false, error: 'Signed out' };
    return { ok: false, error: 'Could not reach the device' };
  }
};

export let setText = function(id: string, v: string | number) { const el=(document.getElementById(id) as HTMLElement); if(el) el.textContent=v as string; }
export let setVal = function(id: string, v: string | number | undefined)  { const el=(document.getElementById(id) as HTMLInputElement); if(el&&v!=null) el.value=v as string; }
export let setCheck = function(id: string, v: boolean){ const el=(document.getElementById(id) as HTMLInputElement); if(el) el.checked=v; }

export let setBar = function(id: string, pct: number, isGreen?: boolean) {
  const el=(document.getElementById(id) as HTMLElement); if(!el) return;
  el.style.width = Math.min(100,Math.max(0,pct))+'%';
  el.className = 'bar-fill ' + (pct>85?'bar-danger':pct>65?'bar-warn':isGreen?'bar-green':'bar-blue');
};

export let fmtCodec = function(c?: string) {
  if(!c||c==='none') return '—';
  if(c==='uncompressed') return 'NDI (SpeedHQ)';
  return String(c).toUpperCase().replace('H264','H.264').replace('H265','H.265').replace('HEVC','H.265');
};

export let esc = function(s: unknown, forAttr?: boolean | number) {
  const html = String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
  if(forAttr) return html.replace(/\\/g,'\\\\').replace(/'/g,'\\\'');
  return html;
};

// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('handle401', () => handle401, value => { handle401 = value; });
expose('api', () => api, value => { api = value; });
expose('del', () => del, value => { del = value; });
expose('post', () => post, value => { post = value; });
expose('postOnce', () => postOnce, value => { postOnce = value; });
expose('setText', () => setText, value => { setText = value; });
expose('setVal', () => setVal, value => { setVal = value; });
expose('setCheck', () => setCheck, value => { setCheck = value; });
expose('setBar', () => setBar, value => { setBar = value; });
expose('fmtCodec', () => fmtCodec, value => { fmtCodec = value; });
expose('esc', () => esc, value => { esc = value; });
