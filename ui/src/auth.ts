import { ApiFailure, apiClient } from './api';
import { expose } from './legacy-globals';
import { acceptNativeRevision, devicePatch, nativeCommand, nativeRevision, nativeState, resultText } from './native-page';

// The text a refused session request shows: the error kind when it has no fields
// (as the login form always did), the device's message otherwise.
const failureText = (error: ApiFailure, fallback: string) =>
  typeof error.envelope.error === 'string' ? error.envelope.error : error.envelope.message || fallback;

// One session token for the typed client and for the page-wide fetch wrapper that
// also covers the network controller and plugin fragments.
export let adoptSession = function(csrf: string | null) {
  apiClient.setCsrf(csrf);
  globalThis.ndimonCsrf = csrf || '';
};

// ══ Auth ════════════════════════════════════════════════════════
export let _booted = false;
export let startApp = async function() {
  const netNote = await consumeNetConfirm();
  let signedIn = true, defaultPassword = false;
  try {
    const session = await apiClient.session();
    if (session.csrf) adoptSession(session.csrf);
    signedIn = session.logged_in;
    defaultPassword = !!session.default_password;
  } catch {
    // An unreachable device boots the shell as before; its requests will say if sign-in is needed.
  }
  if (!signedIn) {
    showLogin();
    if (netNote) setText('login-err', netNote);
    return;
  }
  if (netNote) setTimeout(() => toast(netNote, netNote.startsWith('Network settings confirmed') ? 'success' : 'warning', 8000), 400);
  hideLogin();
  if (defaultPassword) (document.getElementById('pw-warning') as HTMLElement).style.display = '';
  if (_booted) {
    resumeNetWait(true);
    return;
  }
  _booted = true;
  initSSE();
  loadInitialData();
  resumeNetWait(true);
  // The hash page may need its loaders now that we're authenticated.
  const cur = document.querySelector('.page.active')?.id.replace('page-', '');
  if (cur && cur !== 'dashboard') loadPageData(cur);
};

export let showLogin = function() {
  (document.getElementById('login-overlay') as HTMLElement).style.display = 'flex';
  setTimeout(() => (document.getElementById('login-pw') as HTMLInputElement)?.focus(), 50);
};
export let hideLogin = function() { (document.getElementById('login-overlay') as HTMLElement).style.display = 'none'; }

export let doLogin = async function() {
  const pw = (document.getElementById('login-pw') as HTMLInputElement).value;
  const err = (document.getElementById('login-err') as HTMLElement);
  err.textContent = '';
  try {
    await apiClient.login(pw);
    adoptSession(apiClient.csrfToken);
    (document.getElementById('login-pw') as HTMLInputElement).value = '';
    await startApp();
  } catch (error) {
    err.textContent = error instanceof ApiFailure ? failureText(error, 'login failed') : 'network error';
  }
};

export let doLogout = async function() {
  try { await apiClient.logout(); } catch {}
  location.reload();
};

export let changePassword = async function() {
  const cur = (document.getElementById('pw-current') as HTMLInputElement).value;
  const nw  = (document.getElementById('pw-new') as HTMLInputElement).value;
  const cf  = (document.getElementById('pw-confirm') as HTMLInputElement).value;
  const msg = (document.getElementById('pw-msg') as HTMLElement);
  if (nw !== cf) { msg.textContent = 'passwords do not match'; return; }
  try {
    await apiClient.changePassword(cur, nw);
  } catch (error) {
    msg.textContent = error instanceof ApiFailure ? failureText(error, 'failed') : 'failed';
    return;
  }
  adoptSession(apiClient.csrfToken);
  msg.textContent = '';
  ['pw-current','pw-new','pw-confirm'].forEach(id => (document.getElementById(id) as HTMLInputElement).value = '');
  (document.getElementById('pw-warning') as HTMLElement).style.display = 'none';
  toast('Password changed');
};

// Extra DNS names the front door accepts besides its addresses, hostname,
// hostname.local and resolver-domain names.
export let loadWebHosts = async function() {
  try {
    const state = await nativeState();
    const input = document.getElementById('web-hosts') as HTMLInputElement | null;
    if (input) input.value = state.device.web_allowed_hosts.join(', ');
    acceptNativeRevision('security', state.revision);
  } catch {}
};

export let saveWebHosts = async function() {
  const input = document.getElementById('web-hosts') as HTMLInputElement;
  const msg = document.getElementById('web-hosts-msg') as HTMLElement;
  const hosts = input.value.split(/[\s,]+/).map(h => h.trim().toLowerCase()).filter(Boolean);
  const r = await nativeCommand({set_device: devicePatch({web_allowed_hosts: hosts})}, await nativeRevision('security'));
  if (r && r.ok) {
    acceptNativeRevision('security', r.revision);
    msg.textContent = '';
    input.value = hosts.join(', ');
    toast('Host names saved');
  } else if (!r?.cancelled) {
    msg.textContent = resultText(r, 'Save failed');
  }
};

// Retain the classic-script bindings used by handlers, plugins and rollback.
expose('_booted', () => _booted, value => { _booted = value; });
expose('adoptSession', () => adoptSession, value => { adoptSession = value; });
expose('startApp', () => startApp, value => { startApp = value; });
expose('showLogin', () => showLogin, value => { showLogin = value; });
expose('hideLogin', () => hideLogin, value => { hideLogin = value; });
expose('doLogin', () => doLogin, value => { doLogin = value; });
expose('doLogout', () => doLogout, value => { doLogout = value; });
expose('changePassword', () => changePassword, value => { changePassword = value; });
expose('loadWebHosts', () => loadWebHosts, value => { loadWebHosts = value; });
expose('saveWebHosts', () => saveWebHosts, value => { saveWebHosts = value; });
