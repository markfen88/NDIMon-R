import { expose } from './legacy-globals';

// ══ NDI Group Tags ══════════════════════════════════════════════
export let ndiGroups: string[] = [];

export let renderNdiGroups = function() {
  const container = (document.getElementById('ndi-grp-tags') as HTMLElement);
  if (!container) return;
  container.innerHTML = ndiGroups.map((g, i) =>
    `<span class="chip chip-blue" style="font-size:.76rem;padding:3px 6px 3px 9px">
      ${esc(g)}<button type="button" class="btn-close btn-close-white" style="font-size:.5rem;margin-left:4px" aria-label="Remove ${esc(g)}" onclick="removeNdiGroup(${i})"></button>
    </span>`
  ).join('');
};

export let addNdiGroup = function() {
  const input = (document.getElementById('ndi-grp-input') as HTMLInputElement);
  const val = (input.value || '').trim();
  if (!val || ndiGroups.includes(val)) { input.value = ''; return; }
  ndiGroups.push(val);
  input.value = '';
  renderNdiGroups();
};

export let removeNdiGroup = function(idx: number) {
  ndiGroups.splice(idx, 1);
  if (ndiGroups.length === 0) ndiGroups.push('public');
  renderNdiGroups();
};

export let getNdiGroupsString = function() {
  return ndiGroups.length ? ndiGroups.join(',') : 'public';
};

export let setNdiGroupsFromString = function(s: string) {
  ndiGroups = (s || 'public').split(',').map(g => g.trim()).filter(Boolean);
  if (ndiGroups.length === 0) ndiGroups.push('public');
  renderNdiGroups();
};

// Allow Enter key in group input to add
document.addEventListener('DOMContentLoaded', () => {
  const inp = (document.getElementById('ndi-grp-input') as HTMLInputElement);
  if (inp) inp.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); addNdiGroup(); } });
});


// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('ndiGroups', () => ndiGroups, value => { ndiGroups = value; });
expose('renderNdiGroups', () => renderNdiGroups, value => { renderNdiGroups = value; });
expose('addNdiGroup', () => addNdiGroup, value => { addNdiGroup = value; });
expose('removeNdiGroup', () => removeNdiGroup, value => { removeNdiGroup = value; });
expose('getNdiGroupsString', () => getNdiGroupsString, value => { getNdiGroupsString = value; });
expose('setNdiGroupsFromString', () => setNdiGroupsFromString, value => { setNdiGroupsFromString = value; });
