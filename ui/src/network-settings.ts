import type { NetworkProfile } from './generated/NetworkProfile';
import type { AddressMode } from './generated/AddressMode';
import type { NetworkProfiles } from './generated/NetworkProfiles';

// /v1/Network/profiles flattens assignments and adds live observations (addresses,
// handoff state) that have no native observation yet; the draft keeps that shape.
// Writes are typed commands: stage_network applies the impact the preview showed.
type Role = 'management' | 'ndi' | 'tessera';
type ServiceRole = Exclude<Role, 'management'>;
interface Settings extends Record<Role, string> {
  version: number;
  revision: number;
  interface: string;
  backend: string;
  networks: NetworkProfile[];
}
// Who may keep a pending change: anyone holding the token, or only the new management address.
type ConfirmPolicy = 'token_only' | 'require_management_address';
interface Pending {
  token: string;
  confirm_policy?: ConfirmPolicy;
  phase?: string;
  address?: string;
  remaining_seconds?: number;
  profiles?: boolean;
  // The previous settings could not be restored (NI-17): the device keeps trying.
  recovery_pending?: boolean;
  recovery_error?: string;
}
interface Status {
  settings: Settings;
  networks: (NetworkProfile & { ready: boolean; live_address: string })[];
  pending: Pending | null;
  confirm_policy?: ConfirmPolicy;
  // Network ownership handoff (NE-9.5–9.9): prepared, or completed to Tessera.
  quiesced?: boolean;
  externally_managed?: boolean;
  // Adoption was refused (NE-3): nothing can be edited, only the live addressing is known.
  refusal?: { reason: string; detail: string };
  observed?: { interface: string; live_address: string; live_prefix: number }[];
}
interface Replies {
  '/v1/Network/profiles': Status;
  '/v1/Network/profiles/preview': { unchanged: boolean; affected: Role[]; removed: string[]; message: string; confirm_policy?: ConfirmPolicy; impact?: { token: string } };
  '/api/net-pending': { pending: Pending };
  '/api/net-confirm': { ok: boolean };
  '/api/net-revert': { ok: boolean };
}
interface NetworkElements {
  'ndi-network-apply': HTMLButtonElement;
  'ndi-network-error': HTMLElement;
  'ndi-network-profile': HTMLSelectElement;
  'network-confirm-address': HTMLAnchorElement;
  'network-confirm-message': HTMLElement;
  'network-confirm-modal': HTMLElement;
  'network-draft-state': HTMLElement;
  'network-handoff-state': HTMLElement;
  'network-refusal': HTMLElement;
  'network-recovery-state': HTMLElement;
  'network-recovery-message': HTMLElement;
  'network-recovery-accept': HTMLButtonElement;
  'network-profile-controls': HTMLElement;
  'network-keep-help': HTMLElement;
  'network-management': HTMLSelectElement;
  'network-port': HTMLElement;
  'network-profile-address': HTMLInputElement;
  'network-profile-apply': HTMLButtonElement;
  'network-profile-clear': HTMLButtonElement;
  'network-profile-discard': HTMLButtonElement;
  'network-profile-dns': HTMLInputElement;
  'network-profile-editor': HTMLFormElement;
  'network-profile-error': HTMLElement;
  'network-profile-gateway': HTMLInputElement;
  'network-profile-keep': HTMLButtonElement;
  'network-profile-management-fields': HTMLElement;
  'network-profile-mode': HTMLSelectElement;
  'network-profile-name': HTMLInputElement;
  'network-profile-prefix': HTMLInputElement;
  'network-profile-revert': HTMLButtonElement;
  'network-profile-rows': HTMLElement;
  'network-profile-save': HTMLButtonElement;
  'network-profile-search': HTMLInputElement;
  'network-profile-search-field': HTMLElement;
  'network-profile-static': HTMLElement;
  'network-profile-tag': HTMLInputElement;
  'network-profile-tagged': HTMLSelectElement;
}
declare global {
  interface Window {
    NDIMON_NETWORK: {
      changeRole(role: ServiceRole, network: string): Promise<void>;
      loadRole(role: ServiceRole, element: HTMLSelectElement): Promise<void>;
      resumeToken(token: string): Promise<void>;
      resumeStored(): boolean;
      watch(info: Pending): void;
    };
    loadNetwork(): Promise<void>;
  }
}

/* Network profiles are drafted here; no edit changes the OS before Apply. */
(() => {
  'use strict';
  let current: Status | null = null, draft: Settings | null = null, dirty = false, editing: string | null = null, pending: Pending | null = null, poll: ReturnType<typeof setInterval> | null = null, generation=0;
  const $ = <K extends keyof NetworkElements>(id: K) => document.getElementById(id) as NetworkElements[K];
  const labels: Record<Role, string> = {management:'Management / Control',ndi:'NDI (all outputs)',tessera:'Tessera'};
  const copy = <T>(value: T): T => JSON.parse(JSON.stringify(value));
  const storageKey = 'ndimon.network.pending';
  // Service pages (NDI, plugin panes) show their network choice from the same profiles.
  const roleSelects = new Map<HTMLSelectElement, ServiceRole>();
  function refreshRoles() {
    for (const [element, role] of roleSelects) {
      if (element.isConnected === false) roleSelects.delete(element);
      else loadRole(role, element).catch(() => {});
    }
  }
  async function request<P extends keyof Replies>(path: P, body?: unknown): Promise<Replies[P]> {
    const response = await fetch(path, body === undefined ? {} : {
      method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body),
    });
    const result = await response.json();
    if (!response.ok || result.ok === false) throw Object.assign(Error(result.error || 'Network request failed'),{status:response.status,code:result.error});
    return result;
  }
  function options(settings: Settings, selected: string) {
    return settings.networks.map(row => `<option value="${esc(row.id)}"${row.id===selected?' selected':''}>${esc(row.name)} — ${row.vlan_id===null?'Untagged':'VLAN '+row.vlan_id}</option>`).join('');
  }
  function usedBy(settings: Settings,id: string) {
    return (Object.keys(labels) as Role[]).filter(role => settings[role]===id);
  }
  // A device whose configuration cannot be adopted still shows what is live, read-only.
  function observedRows(rows: NonNullable<Status['observed']>) {
    return rows.map(row => `<div class="net-row"><div class="net-main"><div class="net-name">${esc(row.interface)}</div><div class="net-meta"><span>Current configuration</span>`
      + (row.live_address ? `<span class="net-live"><span class="src-dot on"></span><span class="net-addr">${esc(row.live_address+'/'+row.live_prefix)}</span></span>` : '<span class="net-live off"><span class="src-dot"></span>No IPv4 address</span>')
      + '</div></div><div class="net-uses"><span class="chip chip-muted">Read-only</span></div></div>').join('');
  }
  function render() {
    if (!draft || !$('network-profile-rows')) return;
    const refused = current!.refusal;
    $('network-profile-rows').innerHTML = refused ? observedRows(current!.observed ?? []) : draft!.networks.map(row => {
      const live = current!.networks.find(n=>n.id===row.id);
      const usage = usedBy(draft!,row.id).map(role=>`<a class="chip chip-blue" href="#${role==='management'?'settings':role==='ndi'?'ndi':'plugins'}">${esc(labels[role])}</a>`).join('') || '<span class="chip chip-muted">Unassigned</span>';
      const state = !live ? '<span class="net-live"><span class="src-dot"></span>Not applied yet</span>'
        : live.ready ? `<span class="net-live"><span class="src-dot on"></span><span class="net-addr">${esc(live.live_address)}</span></span>`
        : '<span class="net-live off"><span class="src-dot"></span>Network unavailable</span>';
      return `<div class="net-row"><div class="net-main"><div class="net-name">${esc(row.name)}</div><div class="net-meta"><span>${row.vlan_id===null?'Untagged':'VLAN '+row.vlan_id}</span><span>${row.mode==='dhcp'?'DHCP':'<span class="net-addr">'+esc(row.address+'/'+row.prefix)+'</span>'}</span>${state}</div></div>`
        + `<div class="net-uses" title="Assigned in each service's control panel">${usage}</div>`
        + `<div class="net-actions"><button class="btn btn-outline-secondary btn-sm" data-net-edit="${esc(row.id)}"><i class="bi bi-pencil me-1" aria-hidden="true"></i>Edit</button><button class="btn btn-outline-danger btn-sm" data-net-remove="${esc(row.id)}"><i class="bi bi-trash3 me-1" aria-hidden="true"></i>Remove</button></div></div>`;
    }).join('');
    // The card's Unsaved marker follows the draft, not the last button pressed.
    $('network-profile-rows').closest('.card')?.classList.toggle('dirty', dirty);
    $('network-management').innerHTML = options(draft!,draft!.management);
    $('network-port').textContent = refused ? `${draft!.interface} · read-only` : `${draft!.interface} · ${draft!.backend}`;
    $('network-draft-state').textContent = refused ? 'Read-only' : dirty?'Unapplied network changes':'All network changes applied';
    // Nothing here can change a configuration the engine cannot adopt: explain it
    // like the handoff notice and show only the live rows.
    if (refused) $('network-refusal').innerHTML = "NDIMon-R cannot manage this device's network configuration, so network settings are read-only. The current address is shown below."
      + `<div class="net-addr mt-1">${esc(refused.reason+(refused.detail?': '+refused.detail:''))}</div>`;
    $('network-refusal').hidden = !refused;
    // A recovery that failed leaves the media running and the network as it is;
    // say why, and offer the way out once the operator has seen what is live.
    const stuck = current!.pending?.recovery_pending ? current!.pending : null;
    $('network-recovery-message').textContent = stuck
      ? 'The previous network settings could not be restored' + (stuck.recovery_error ? ': ' + stuck.recovery_error : '') + '. The device keeps trying, and network changes are paused.' : '';
    $('network-recovery-state').hidden = !stuck;
    $('network-profile-controls').hidden = !!refused;
    $('network-keep-help').textContent = current!.confirm_policy==='require_management_address'
      ? 'Changes can interrupt network access. Keep changes through the new management address within 60 seconds after apply, or the previous configuration is restored.'
      : 'Changes can interrupt network access. Keep them within 60 seconds after they are applied, or the previous configuration is restored.';
    // While a handoff is prepared or Tessera owns the network, the device refuses
    // network changes; say why and keep the card read-only.
    const held = current!.externally_managed ? 'Tessera manages the network on this device. These settings are read-only here.'
      : current!.quiesced ? 'A network handoff to Tessera is being prepared. Network changes are paused until it completes or is cancelled.' : '';
    $('network-handoff-state').textContent = held;
    $('network-handoff-state').hidden = !held;
    const locked = !!pending || !!held || !!refused;
    $('network-profile-apply').disabled = !dirty || locked;
    $('network-profile-discard').disabled = !dirty || locked;
    $('network-profile-editor').querySelectorAll<HTMLInputElement | HTMLSelectElement | HTMLButtonElement>('input,select,button').forEach(el=>el.disabled=locked);
    $('network-management').disabled = locked;
    $('network-profile-rows').querySelectorAll('button').forEach(el=>el.disabled=locked);
    if (!locked) fields();
  }
  async function load() {
    try {
      current = await request('/v1/Network/profiles');
      if (!dirty) draft = copy(current!.settings);
      pending = current!.pending;
      $('network-profile-error').textContent = '';
      render();
      // A failed recovery has nothing to keep; the banner above carries it.
      if (pending && !pending.recovery_pending) watch(pending);
      refreshRoles();
    } catch(e) { if ($('network-profile-error')) $('network-profile-error').textContent=(e as Error).message; }
  }
  function edit(id: string) {
    const row = draft!.networks.find(n=>n.id===id);
    if (!row) return;
    editing = id;
    $('network-profile-name').value=row.name;
    $('network-profile-tagged').value=row.vlan_id===null?'untagged':'tagged';
    $('network-profile-tag').value=(row.vlan_id??'') as string;
    $('network-profile-mode').value=row.mode;
    $('network-profile-address').value=row.address;
    $('network-profile-prefix').value=(row.prefix||24) as unknown as string;
    $('network-profile-gateway').value=row.gateway;
    $('network-profile-dns').value=row.dns.join(', ');
    $('network-profile-search').value=(row.dns_search??[]).join(', ');
    $('network-profile-save').textContent='Update draft';
    fields();
  }
  function fields() {
    $('network-profile-tag').disabled=$('network-profile-tagged').value==='untagged';
    const isStatic=$('network-profile-mode').value==='static';
    $('network-profile-static').hidden=!isStatic;
    $('network-profile-management-fields').hidden=editing!==draft?.management;
    // Search domains apply to DHCP and static management alike, so this sits outside the static block.
    $('network-profile-search-field').hidden=editing!==draft?.management;
  }
  function clearEditor() {
    editing=null;
    $('network-profile-editor').reset();
    $('network-profile-save').textContent='Add to draft';
    fields();
  }
  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (!draft || pending) return;
    const tag=$('network-profile-tagged').value==='untagged'?null:Number($('network-profile-tag').value);
    if (tag!==null && (!Number.isInteger(tag)||tag<1||tag>4094)) return toast('Enter a VLAN ID from 1 to 4094','warning');
    const mode=$('network-profile-mode').value as AddressMode;
    const row={id:editing||(tag===null?'untagged':'vlan-'+tag),name:$('network-profile-name').value.trim(),vlan_id:tag,mode,
      address:mode==='static'?$('network-profile-address').value.trim():'',prefix:mode==='static'?Number($('network-profile-prefix').value):0,
      gateway:mode==='static'&&editing===draft!.management?$('network-profile-gateway').value.trim():'',
      dns:mode==='static'&&editing===draft!.management?$('network-profile-dns').value.split(',').map(v=>v.trim()).filter(Boolean):[],
      // A host name has no spaces, so commas and whitespace both separate domains.
      dns_search:editing===draft!.management?$('network-profile-search').value.split(/[\s,]+/).filter(Boolean):[]};
    if (draft!.networks.some(n=>n.id!==editing&&(n.vlan_id===tag||n.id===row.id))) return toast('That network already exists','warning');
    const used=usedBy(draft!,row.id);
    if (editing && used.length && !await confirmDialog('Change an assigned network?',`This affects ${used.map(r=>labels[r]).join(', ')}. Changes remain in the draft until you apply them.`,{label:'Update draft'})) return;
    if (editing) draft!.networks=draft!.networks.map(n=>n.id===editing?row:n); else draft!.networks.push(row);
    dirty=true; clearEditor(); render();
  }
  async function remove(id: string) {
    if (!draft || pending) return;
    const row=draft!.networks.find(n=>n.id===id),used=usedBy(draft!,id);
    if (used.length) {
      await confirmDialog('Network is assigned',`Used by ${used.map(r=>labels[r]).join(', ')}. Select replacement networks in those control panels before removing it.`,{label:'Understood'});
      return;
    }
    if (!await confirmDialog('Remove network?',`${row!.name} will be removed when you apply network settings.`,{danger:true,label:'Remove from draft'})) return;
    draft!.networks=draft!.networks.filter(n=>n.id!==id);dirty=true;render();
  }
  // The draft's flat roles are the native assignments; the tessera role is the audio one.
  function profilesOf(settings: Settings): NetworkProfiles {
    return {interface:settings.interface,backend:settings.backend as NetworkProfiles['backend'],networks:settings.networks,
      roles:{management:settings.management,ndi:settings.ndi,audio:settings.tessera},confirm_policy:current?.confirm_policy??'token_only'};
  }
  // What staging returned, in the shape the Keep dialog watches.
  function pendingOf(applied: { unchanged?: boolean; token?: string; pending?: { phase?: string; management_address?: string | null; remaining_seconds?: number; confirm_policy?: ConfirmPolicy } }): Pending & { unchanged?: boolean } {
    if (applied.unchanged) return {token:'',unchanged:true};
    const p=applied.pending??{};
    return {token:applied.token??'',phase:p.phase,address:p.management_address??'',remaining_seconds:p.remaining_seconds,confirm_policy:p.confirm_policy,profiles:true};
  }
  async function apply(settings=draft, again=false): Promise<void> {
    try {
      const plan=await request('/v1/Network/profiles/preview',settings);
      if (plan.unchanged) return toast('Network settings already match');
      const affected=plan.affected.map(r=>labels[r]).join(', ')||'Network configuration';
      if (!await confirmDialog('Apply network settings?',`${affected} may be interrupted.${plan.removed.length?' Removing: '+plan.removed.join(', ')+'.':''} ${plan.message}`,{label:'Apply network settings'})) return render();
      const staged=await nativeCommand({stage_network:{profiles:profilesOf(settings!)}},undefined,{confirm:false,expect_impact:plan.impact?.token});
      if (!staged.ok) throw Object.assign(Error(staged.errors?.length?staged.errors.join(' · '):staged.error||'Network request failed'),{code:staged.error});
      const result=pendingOf((staged.result??{}) as Parameters<typeof pendingOf>[0]);
      if (result.unchanged) return;
      dirty=false;watch(result);render();
    } catch(e) {
      // The device seals each preview; if it expired or no longer matches the
      // settings, show the device's current impact again instead of an error code.
      if ((e as Error & { code?: string }).code==='confirmation_required') {
        if (!again) return apply(settings,true);
        toast('The network impact changed while you were confirming. Review it and apply again.','warning');return;
      }
      toast((e as Error).message,'danger');
    }
  }
  function pendingDialog() {
    if ($('network-confirm-modal')) return;
    const el=document.createElement('div');el.className='modal fade';el.id='network-confirm-modal';el.style.zIndex='2100';
    el.dataset.bsBackdrop='static';el.dataset.bsKeyboard='false';el.tabIndex=-1;
    el.innerHTML='<div class="modal-dialog"><div class="modal-content"><div class="modal-header"><h5 class="modal-title">Keep network changes?</h5></div><div class="modal-body"><p id="network-confirm-message" role="status"></p><a id="network-confirm-address" target="_self"></a><p class="form-text mt-3">If you lose access or close this page, the appliance restores the previous network configuration automatically.</p></div><div class="modal-footer"><button class="btn btn-outline-danger" id="network-profile-revert">Revert</button><button class="btn btn-primary" id="network-profile-keep">Keep changes</button></div></div></div>';
    document.body.append(el);
    $('network-profile-keep').onclick=keep;
    $('network-profile-revert').onclick=revert;
  }
  function watch(info: Pending) {
    pending=info;pendingDialog();
    const observedGeneration=++generation;
    let updating=false;
    try { localStorage.setItem(storageKey,JSON.stringify(info)); } catch {}
    modalOf('network-confirm-modal').show();
    clearInterval(poll!);
    const update=async()=>{
      if (updating || observedGeneration!==generation) return;
      updating=true;
      try {
        const result=await request('/api/net-pending',{token:pending!.token});
        if (observedGeneration!==generation) return;
        pending=result.pending;
        if (pending!.recovery_pending) {
          $('network-confirm-message').textContent='The previous network settings could not be restored'+(pending!.recovery_error?': '+pending!.recovery_error:'')+'. The device keeps trying; Keep is unavailable.';
          $('network-profile-keep').disabled=true;
          return;
        }
        const applied=pending!.phase==='applied',seconds=pending!.remaining_seconds;
        $('network-confirm-message').textContent=applied?(pending!.confirm_policy==='require_management_address'?`Reverts in ${seconds} seconds unless you keep these changes through the new management connection.`:`Reverts in ${seconds} seconds unless you keep these changes.`):'Applying network settings. The 60-second confirmation window starts when the new management configuration is active.';
        const link=$('network-confirm-address');
        if (pending!.address) {
          link.href=`${location.protocol}//${pending!.address}${location.port?':'+location.port:''}/#netprofile=${pending!.token}`;
          link.textContent='Open management at '+pending!.address;
        }
        $('network-profile-keep').disabled=false;
      } catch(e) {
        if (observedGeneration!==generation) return;
        if ((e as Error & { status?: number }).status===410) {finish();toast('The network change has ended. Reload Network Settings to check the active configuration.','warning');return;}
        $('network-profile-keep').disabled=false;
        $('network-confirm-message').textContent='Waiting for the device. If the new management address cannot be reached, the appliance will restore its previous configuration.';
      } finally { updating=false; }
    };
    update();poll=setInterval(update,1000);
  }
  async function keep() {
    if (!pending) return;
    try {
      await request('/api/net-confirm',{token:pending!.token,profiles:true});
      finish();toast('Network settings kept','success');await load();
    } catch(e) { $('network-confirm-message').textContent=(e as Error).message; }
  }
  async function revert() {
    try {
      await request('/api/net-revert',{token:pending!.token});
      finish();await load();toast('Previous network settings restored','warning');
    } catch(e) { $('network-confirm-message').textContent=(e as Error).message+' The device-side timeout will still revert unconfirmed changes.'; }
  }
  // The operator's way out of a recovery that cannot finish (NI-17). The device
  // computes what it found (the failure, the live addresses, foreign
  // configuration); the dialog shows exactly that before anything is given up.
  async function acceptCurrent() {
    try {
      const first=await nativeCommand('accept_network_recovery',undefined,{confirm:false});
      const impact=first.impact;
      if (first.error!=='confirmation_required' || !impact?.token) throw Error(first.error||'Network request failed');
      if (!await confirmDialog('Accept the current network?',impact.lines?.join(' ')||'The network stays as it is now.',{label:'Accept current network'})) return;
      const done=await nativeCommand('accept_network_recovery',undefined,{confirm:false,expect_impact:impact.token});
      if (!done.ok) throw Error(done.error||'Network request failed');
      // A file the device could not put back still holds what the failed change wrote.
      const left=((done.result as {unrestored?:{path:string;reason:string}[]}|undefined)?.unrestored||[]).map(file=>`${file.path} could not be put back (${file.reason})`);
      finish();toast(['Network accepted as it is',...left].join('. '),'warning');await load();
    } catch(e) { toast((e as Error).message,'danger'); }
  }
  function finish() {
    generation++;clearInterval(poll!);pending=null;
    try { localStorage.removeItem(storageKey); } catch {}
    hideModal('network-confirm-modal');
  }
  async function resumeToken(token: string) {
    if (!/^[0-9a-f]{64}$/.test(token)) return;
    watch({token});
  }
  function resumeStored() {
    try { const info=JSON.parse(localStorage.getItem(storageKey)!);if (info?.token) {watch(info);return true;} } catch {}
    return false;
  }
  async function changeRole(role: ServiceRole,network: string) {
    if (!['ndi','tessera'].includes(role)) throw Error('invalid network role');
    if (dirty) return toast('Apply or discard your draft in Network Settings before changing service assignments','warning');
    const status=await request('/v1/Network/profiles');
    const settings=copy(status.settings);settings[role]=network;
    await apply(settings);
  }
  async function loadRole(role: ServiceRole,element: HTMLSelectElement) {
    roleSelects.set(element,role);
    const status=await request('/v1/Network/profiles');
    if (Number(element.dataset.networkRevision ?? -1)>status.settings.revision) return;
    // Keep a choice the operator made since the last apply until it is applied,
    // even when it equals the value saved before a change elsewhere (a poll after
    // Keep must not undo a selection made just before it).
    if (!element.dataset.choiceWatch) {
      element.dataset.choiceWatch='1';
      element.addEventListener('change',()=>{element.dataset.userChoice=element.value;});
    }
    const choice=element.dataset.userChoice;
    const unsaved=choice!==undefined&&choice!==status.settings[role];
    if (!unsaved) delete element.dataset.userChoice;
    const selected=unsaved?choice!:status.settings[role];
    element.innerHTML=options(status.settings,selected);
    if (unsaved&&!status.settings.networks.some(n=>n.id===selected)) {
      const missing=document.createElement('option');missing.value=selected;missing.textContent='Unavailable network: '+selected;
      missing.selected=true;missing.disabled=true;element.append(missing);
    }
    element.dataset.savedRole=status.settings[role];
    element.dataset.networkRevision=String(status.settings.revision);
    element.disabled=!!status.pending||!!status.quiesced||!!status.externally_managed||!!status.refusal;
  }
  window.NDIMON_NETWORK={changeRole,loadRole,resumeToken,resumeStored,watch};
  window.loadNetwork=load;
  document.addEventListener('DOMContentLoaded',()=>{
    if (!location.hash.startsWith('#netprofile=')) resumeStored();
    $('network-profile-editor').addEventListener('submit',save);
    $('network-recovery-accept').onclick=acceptCurrent;
    $('network-profile-mode').onchange=fields;$('network-profile-tagged').onchange=fields;
    $('network-profile-clear').onclick=()=>{clearEditor();render();};
    $('network-profile-apply').onclick=()=>apply();
    $('network-profile-discard').onclick=()=>{dirty=false;draft=copy(current!.settings);clearEditor();render();};
    $('network-profile-rows').onclick=event=>{
      const button=(event.target as HTMLElement).closest('button');if (!button) return;
      if (button.dataset.netEdit) edit(button.dataset.netEdit);
      if (button.dataset.netRemove) remove(button.dataset.netRemove);
    };
    $('network-management').onchange=async event=>{
      const next=(event.target as HTMLSelectElement).value;
      if (!await confirmDialog('Move management?', 'The web interface will move when you apply. Gateway and DNS on the previous management network will be cleared. Configure the replacement network before applying.',{label:'Update draft'})) return render();
      const old=draft!.networks.find(n=>n.id===draft!.management);old!.gateway='';old!.dns=[];old!.dns_search=[];
      draft!.management=next;dirty=true;render();
    };
    const ndi=$('ndi-network-profile');
    if (ndi) {
      const refresh=()=>{ $('ndi-network-error').textContent='';return loadRole('ndi',ndi).catch(e=>{$('ndi-network-error').textContent=(e as Error).message;}); };
      refresh();
      $('ndi-network-apply').onclick=()=>changeRole('ndi',ndi.value).catch(e=>toast((e as Error).message,'danger'));
      document.querySelector('[data-page="ndi"]')?.addEventListener('click',refresh);
    }
  });
})();
