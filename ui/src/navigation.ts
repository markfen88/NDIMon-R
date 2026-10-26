import { expose } from './legacy-globals';

// ══ Navigation ══════════════════════════════════════════════════
export const PAGES = ['dashboard', 'outputs', 'ndi', 'plugins', 'settings', 'system'];

export let showPage = function(name: string, opts: { fromHash?: boolean } = {}) {
  if (!PAGES.includes(name)) name = 'dashboard';
  document.querySelectorAll<HTMLElement>('.page').forEach(p => p.classList.remove('active'));
  document.querySelectorAll<HTMLElement>('.nav-item').forEach(n => n.classList.toggle('active', n.dataset.page === name));
  (document.getElementById('page-'+name) as HTMLElement).classList.add('active');
  if (!opts.fromHash && location.hash !== '#'+name) history.replaceState(null, '', '#'+name);
  if (_booted) loadPageData(name);
  window.scrollTo({ top: 0 });
};

export let loadPageData = function(name: string) {
  if (name === 'plugins') loadPlugins();
  if (name === 'settings') { loadSettings(); loadWebHosts(); }
  if (name === 'system')   { loadSysInfo(); loadWatchdog(); loadRebootSchedule(); loadSnapshots(); loadNtp(); }
  if (name === 'outputs')  { loadSourceIPs(); loadPresets(); }
  if (name === 'ndi')      loadSourceIPs();
};


// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('PAGES', () => PAGES);
expose('showPage', () => showPage, value => { showPage = value; });
expose('loadPageData', () => loadPageData, value => { loadPageData = value; });
