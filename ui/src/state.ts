import { expose } from './legacy-globals';
import type { VideoMode, ToastInstance } from './legacy-types';

// Desired state, observations and telemetry live in store.ts; live.ts combines
// them for drawing. Only presentation handles remain here.
// ══ State ══════════════════════════════════════════════════════
export let modes: VideoMode[] = [];
export let toastInst: ToastInstance | undefined;

document.addEventListener('DOMContentLoaded', async () => {
  toastInst = new bootstrap.Toast((document.getElementById('toast') as HTMLElement), { delay:4000 });
  document.querySelectorAll<HTMLElement>('.nav-item[data-page]').forEach(a => {
    a.addEventListener('click', e => { e.preventDefault(); showPage(a.dataset.page!); });
  });
  window.addEventListener('hashchange', () => {
    const name = location.hash.replace('#', '');
    if (PAGES.includes(name)) showPage(name, { fromHash: true });
  });
  const first = location.hash.replace('#', '');
  if (PAGES.includes(first) && first !== 'dashboard') showPage(first, { fromHash: true });
  await startApp();
});


// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('modes', () => modes, value => { modes = value; });
expose('toastInst', () => toastInst, value => { toastInst = value; });
