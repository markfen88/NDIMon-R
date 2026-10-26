import { expose } from './legacy-globals';

// ══ Toast ═══════════════════════════════════════════════════════
export let toast = function(msg: string, type?: string, ms?: number) {
  const el = (document.getElementById('toast') as HTMLElement);
  el.className = `toast align-items-center text-bg-${type||'success'} border-0`;
  (document.getElementById('toast-msg') as HTMLElement).textContent = msg;
  const delay = ms || ((type === 'danger' || type === 'warning') ? 7000 : 4000);
  toastInst = bootstrap.Toast.getOrCreateInstance(el, { delay });
  toastInst._config.delay = delay;
  toastInst.show();
};


// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('toast', () => toast, value => { toast = value; });
