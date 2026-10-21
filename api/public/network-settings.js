/* Network profiles are drafted here; no edit changes the OS before Apply. */
(() => {
  'use strict';
  let current = null, draft = null, dirty = false, editing = null, pending = null, poll = null, generation=0;
  const $ = id => document.getElementById(id);
  const labels = {management:'Management / Control',ndi:'NDI (all outputs)',tessera:'Tessera'};
  const copy = value => JSON.parse(JSON.stringify(value));
  const storageKey = 'ndimon.network.pending';
  async function request(path, body) {
    const response = await fetch(path, body === undefined ? {} : {
      method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body),
    });
    const result = await response.json();
    if (!response.ok || result.ok === false) throw Object.assign(Error(result.error || 'Network request failed'),{status:response.status});
    return result;
  }
  function options(settings, selected) {
    return settings.networks.map(row => `<option value="${esc(row.id)}"${row.id===selected?' selected':''}>${esc(row.name)} — ${row.vlan_id===null?'Untagged':'VLAN '+row.vlan_id}</option>`).join('');
  }
  function usedBy(settings,id) {
    return Object.keys(labels).filter(role => settings[role]===id);
  }
  function render() {
    if (!draft || !$('network-profile-rows')) return;
    $('network-profile-rows').innerHTML = draft.networks.map(row => {
      const live = current.networks.find(n=>n.id===row.id);
      const usage = usedBy(draft,row.id).map(role=>`<a href="#${role==='management'?'settings':role==='ndi'?'ndi':'plugins'}">${esc(labels[role])}</a>`).join(', ') || 'Unassigned';
      return `<tr><td>${esc(row.name)}</td><td>${row.vlan_id===null?'Untagged':row.vlan_id}</td><td>${row.mode==='dhcp'?'DHCP':esc(row.address+'/'+row.prefix)}<div class="form-text">${live?.ready?'Live: '+esc(live.live_address):'Network unavailable'}</div></td><td>${usage}</td><td><button class="btn btn-outline-primary btn-sm" data-net-edit="${esc(row.id)}">Edit</button> <button class="btn btn-outline-danger btn-sm" data-net-remove="${esc(row.id)}">Remove</button></td></tr>`;
    }).join('');
    $('network-management').innerHTML = options(draft,draft.management);
    $('network-port').textContent = `${draft.interface} · ${draft.backend}`;
    $('network-draft-state').textContent = dirty?'Unapplied network changes':'All network changes applied';
    $('network-profile-apply').disabled = !dirty || !!pending;
    $('network-profile-discard').disabled = !dirty || !!pending;
    $('network-profile-editor').querySelectorAll('input,select,button').forEach(el=>el.disabled=!!pending);
    $('network-management').disabled = !!pending;
    $('network-profile-rows').querySelectorAll('button').forEach(el=>el.disabled=!!pending);
    if (!pending) fields();
  }
  async function load() {
    try {
      current = await request('/v1/Network/profiles');
      if (!dirty) draft = copy(current.settings);
      pending = current.pending;
      $('network-profile-error').textContent = '';
      render();
      if (pending) watch(pending);
    } catch(e) { if ($('network-profile-error')) $('network-profile-error').textContent=e.message; }
  }
  function edit(id) {
    const row = draft.networks.find(n=>n.id===id);
    if (!row) return;
    editing = id;
    $('network-profile-name').value=row.name;
    $('network-profile-tagged').value=row.vlan_id===null?'untagged':'tagged';
    $('network-profile-tag').value=row.vlan_id??'';
    $('network-profile-mode').value=row.mode;
    $('network-profile-address').value=row.address;
    $('network-profile-prefix').value=row.prefix||24;
    $('network-profile-gateway').value=row.gateway;
    $('network-profile-dns').value=row.dns.join(', ');
    $('network-profile-save').textContent='Update draft';
    fields();
  }
  function fields() {
    $('network-profile-tag').disabled=$('network-profile-tagged').value==='untagged';
    const isStatic=$('network-profile-mode').value==='static';
    $('network-profile-static').hidden=!isStatic;
    $('network-profile-management-fields').hidden=editing!==draft?.management;
  }
  function clearEditor() {
    editing=null;
    $('network-profile-editor').reset();
    $('network-profile-save').textContent='Add to draft';
    fields();
  }
  async function save(event) {
    event.preventDefault();
    if (!draft || pending) return;
    const tag=$('network-profile-tagged').value==='untagged'?null:Number($('network-profile-tag').value);
    if (tag!==null && (!Number.isInteger(tag)||tag<1||tag>4094)) return toast('Enter a VLAN ID from 1 to 4094','warning');
    const mode=$('network-profile-mode').value;
    const row={id:editing||(tag===null?'untagged':'vlan-'+tag),name:$('network-profile-name').value.trim(),vlan_id:tag,mode,
      address:mode==='static'?$('network-profile-address').value.trim():'',prefix:mode==='static'?Number($('network-profile-prefix').value):0,
      gateway:mode==='static'&&editing===draft.management?$('network-profile-gateway').value.trim():'',
      dns:mode==='static'&&editing===draft.management?$('network-profile-dns').value.split(',').map(v=>v.trim()).filter(Boolean):[]};
    if (draft.networks.some(n=>n.id!==editing&&(n.vlan_id===tag||n.id===row.id))) return toast('That network already exists','warning');
    const used=usedBy(draft,row.id);
    if (editing && used.length && !await confirmDialog('Change an assigned network?',`This affects ${used.map(r=>labels[r]).join(', ')}. Changes remain in the draft until you apply them.`,{label:'Update draft'})) return;
    if (editing) draft.networks=draft.networks.map(n=>n.id===editing?row:n); else draft.networks.push(row);
    dirty=true; clearEditor(); render();
  }
  async function remove(id) {
    if (!draft || pending) return;
    const row=draft.networks.find(n=>n.id===id),used=usedBy(draft,id);
    if (used.length) {
      await confirmDialog('Network is assigned',`Used by ${used.map(r=>labels[r]).join(', ')}. Select replacement networks in those control panels before removing it.`,{label:'Understood'});
      return;
    }
    if (!await confirmDialog('Remove network?',`${row.name} will be removed when you apply network settings.`,{danger:true,label:'Remove from draft'})) return;
    draft.networks=draft.networks.filter(n=>n.id!==id);dirty=true;render();
  }
  async function apply(settings=draft) {
    try {
      const plan=await request('/v1/Network/profiles/preview',settings);
      if (plan.unchanged) return toast('Network settings already match');
      const affected=plan.affected.map(r=>labels[r]).join(', ')||'Network configuration';
      if (!await confirmDialog('Apply network settings?',`${affected} may be interrupted.${plan.removed.length?' Removing: '+plan.removed.join(', ')+'.':''} ${plan.message}`,{label:'Apply network settings'})) return;
      const result=await request('/v1/Network/profiles/apply',settings);
      if (result.unchanged) return;
      dirty=false;watch(result);render();
    } catch(e) { toast(e.message,'danger'); }
  }
  function pendingDialog() {
    if ($('network-confirm-modal')) return;
    const el=document.createElement('div');el.className='modal fade';el.id='network-confirm-modal';el.style.zIndex='2100';
    el.dataset.bsBackdrop='static';el.dataset.bsKeyboard='false';el.tabIndex=-1;
    el.innerHTML='<div class="modal-dialog"><div class="modal-content"><div class="modal-header"><h5 class="modal-title">Keep network changes?</h5></div><div class="modal-body"><p id="network-confirm-message" role="status"></p><a id="network-confirm-address" target="_self"></a><p class="form-text mt-3">If you lose access or close this page, the appliance restores the previous network configuration automatically.</p></div><div class="modal-footer"><button class="btn btn-outline-danger" id="network-profile-revert">Revert</button><button class="btn btn-primary" id="network-profile-keep" disabled>Keep changes</button></div></div></div>';
    document.body.append(el);
    $('network-profile-keep').onclick=keep;
    $('network-profile-revert').onclick=revert;
  }
  function watch(info) {
    pending=info;pendingDialog();
    const observedGeneration=++generation;
    let updating=false;
    try { localStorage.setItem(storageKey,JSON.stringify(info)); } catch {}
    modalOf('network-confirm-modal').show();
    clearInterval(poll);
    const update=async()=>{
      if (updating || observedGeneration!==generation) return;
      updating=true;
      try {
        const result=await request('/api/net-pending',{token:pending.token});
        if (observedGeneration!==generation) return;
        pending=result.pending;
        const applied=pending.phase==='applied',seconds=pending.remaining_seconds;
        $('network-confirm-message').textContent=applied?`Reverts in ${seconds} seconds unless you keep these changes through the new management connection.`:'Applying network settings. The 60-second confirmation window starts when the new management configuration is active.';
        const link=$('network-confirm-address');
        if (pending.address) {
          link.href=`${location.protocol}//${pending.address}${location.port?':'+location.port:''}/#netprofile=${pending.token}`;
          link.textContent='Open management at '+pending.address;
        }
        $('network-profile-keep').disabled=!applied||location.hostname!==pending.address||seconds<=0;
      } catch(e) {
        if (observedGeneration!==generation) return;
        if (e.status===410) {finish();toast('The network change has ended. Reload Network Settings to check the active configuration.','warning');return;}
        $('network-profile-keep').disabled=true;
        $('network-confirm-message').textContent='Waiting for the device. If the new management address cannot be reached, the appliance will restore its previous configuration.';
      } finally { updating=false; }
    };
    update();poll=setInterval(update,1000);
  }
  async function keep() {
    if (!pending) return;
    try {
      await request('/api/net-confirm',{token:pending.token,profiles:true});
      finish();toast('Network settings kept','success');await load();
    } catch(e) { $('network-confirm-message').textContent=e.message; }
  }
  async function revert() {
    try {
      await request('/api/net-revert',{token:pending.token});
      finish();await load();toast('Previous network settings restored','warning');
    } catch(e) { $('network-confirm-message').textContent=e.message+' The device-side timeout will still revert unconfirmed changes.'; }
  }
  function finish() {
    generation++;clearInterval(poll);pending=null;
    try { localStorage.removeItem(storageKey); } catch {}
    modalOf('network-confirm-modal').hide();
  }
  async function resumeToken(token) {
    if (!/^[0-9a-f]{64}$/.test(token)) return;
    watch({token});
  }
  function resumeStored() {
    try { const info=JSON.parse(localStorage.getItem(storageKey));if (info?.token) {watch(info);return true;} } catch {}
    return false;
  }
  async function changeRole(role,network) {
    if (!['ndi','tessera'].includes(role)) throw Error('invalid network role');
    if (dirty) return toast('Apply or discard your draft in Network Settings before changing service assignments','warning');
    const status=await request('/v1/Network/profiles');
    const settings=copy(status.settings);settings[role]=network;
    await apply(settings);
  }
  async function loadRole(role,element) {
    const status=await request('/v1/Network/profiles');
    if (Number(element.dataset.networkRevision ?? -1)>status.settings.revision) return;
    const previous=element.value;
    const unsaved=element.dataset.savedRole!==undefined&&previous!==element.dataset.savedRole;
    const selected=unsaved?previous:status.settings[role];
    element.innerHTML=options(status.settings,selected);
    if (unsaved&&!status.settings.networks.some(n=>n.id===selected)) {
      const missing=document.createElement('option');missing.value=selected;missing.textContent='Unavailable network: '+selected;
      missing.selected=true;missing.disabled=true;element.append(missing);
    }
    element.dataset.savedRole=status.settings[role];
    element.dataset.networkRevision=String(status.settings.revision);
    element.disabled=!!status.pending;
  }
  window.NDIMON_NETWORK={changeRole,loadRole,resumeToken,resumeStored,watch};
  window.loadNetwork=load;
  document.addEventListener('DOMContentLoaded',()=>{
    if (!location.hash.startsWith('#netprofile=')) resumeStored();
    $('network-profile-editor').addEventListener('submit',save);
    $('network-profile-mode').onchange=fields;$('network-profile-tagged').onchange=fields;
    $('network-profile-clear').onclick=clearEditor;
    $('network-profile-apply').onclick=()=>apply();
    $('network-profile-discard').onclick=()=>{dirty=false;draft=copy(current.settings);clearEditor();render();};
    $('network-profile-rows').onclick=event=>{
      const button=event.target.closest('button');if (!button) return;
      if (button.dataset.netEdit) edit(button.dataset.netEdit);
      if (button.dataset.netRemove) remove(button.dataset.netRemove);
    };
    $('network-management').onchange=async event=>{
      const next=event.target.value;
      if (!await confirmDialog('Move management?', 'The web interface will move when you apply. Gateway and DNS on the previous management network will be cleared. Configure the replacement network before applying.',{label:'Update draft'})) return render();
      const old=draft.networks.find(n=>n.id===draft.management);old.gateway='';old.dns=[];
      draft.management=next;dirty=true;render();
    };
    const ndi=$('ndi-network-profile');
    if (ndi) {
      const refresh=()=>{ $('ndi-network-error').textContent='';return loadRole('ndi',ndi).catch(e=>{$('ndi-network-error').textContent=e.message;}); };
      refresh();
      $('ndi-network-apply').onclick=()=>changeRole('ndi',ndi.value).catch(e=>toast(e.message,'danger'));
      document.querySelector('[data-page="ndi"]')?.addEventListener('click',refresh);
    }
  });
})();
