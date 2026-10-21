'use strict';
const graph=require('./audio-graph');
const clone=graph.clone;
function endpoint(value,alias,direction,primary,catalog){
 const list=direction==='source'?catalog.inputs:catalog.destinations;
 if(alias==='ndi'&&direction==='source')return {...(list.find(e=>e.provider==='ndi'&&e.output===primary)||{}),id:'ndi-'+primary,node_id:'ndi-'+primary,provider:'ndi',channels:value.channels,output:primary};
 if(alias==='hdmi'&&value.alsa_pcm==='auto')return {...(list.find(e=>e.provider==='hdmi'&&e.output===primary)||{}),id:'hdmi-'+primary,node_id:'hdmi-'+primary,provider:'hdmi',channels:value.channels,alsa_pcm:'auto',resource:'hdmi:'+primary,channel_start:1};
 if(value.alsa_pcm==='ndimon_net_in'||value.alsa_pcm==='ndimon_net_out'){
  const d=direction==='source'?'rx':'tx',id=`tessera-${d}-1-${value.channels}`;
  return {...list.find(e=>e.id===id),id,node_id:id,provider:'tessera',channels:value.channels,alsa_pcm:`ndimon_tessera_${d}_1_${value.channels}`,resource:'tessera:main:'+d,channel_start:1};
 }
 return list.find(e=>e.alsa_pcm===value.alsa_pcm&&e.channels===value.channels)||{id:alias,node_id:alias,provider:'unknown',channels:value.channels,alsa_pcm:value.alsa_pcm,resource:'alsa:'+value.alsa_pcm,channel_start:1};
}
function migrate(settings,primary,catalog,{disablePrimary=false}={}){
 const base=clone(settings);delete base.revision;
 const doc={version:2,revision:settings.revision||0,engine:clone(settings.engine),sources:{},sinks:{},outputs:{},routes:{},video_delay_ms:settings.video_delay_ms};
 const legacy={settings:base,sources:{},sinks:{}},choice={enabled:!disablePrimary,primary:true,inputs:[],destinations:[],legacy,legacy_matrix:false};
 for(const [alias,value]of Object.entries(settings.sources)){
  const entry=endpoint(value,alias,'source',primary,catalog),id=entry.node_id||entry.id;legacy.sources[alias]=id;doc.sources[id]={...graph.concrete(entry),enabled:value.enabled};
  if(value.enabled)choice.inputs.push({endpoint:entry.id,gain_db:value.gain_db,delay_ms:value.delay_ms});
 }
 for(const [alias,value]of Object.entries(settings.sinks)){
  const entry=endpoint(value,alias,'sink',primary,catalog),id=entry.node_id||entry.id;legacy.sinks[alias]=id;doc.sinks[id]={...graph.concrete(entry),enabled:value.enabled};
  if(value.enabled)choice.destinations.push({endpoint:entry.id,delay_ms:value.delay_ms});
  for(const [source,mix]of Object.entries(value.mix)){
   const s=settings.sources[source];if(!s)continue;
   const active=value.enabled&&s.enabled&&!disablePrimary;
   const gain=s.gain_db<=-60||mix<=-60?-60:s.gain_db+mix,delay=s.delay_ms+value.delay_ms;
   if(delay>5000||gain>12||gain < -60){if(active)throw Object.assign(Error(`legacy audio ${source}→${alias} cannot migrate: effective delay ${delay}ms/gain ${gain}dB exceeds graph limits`),{status:409,code:'legacy_migration_required'});continue;}
   const sid=legacy.sources[source];doc.routes[graph.routeId(primary,sid,id)]={output:primary,source:sid,sink:id,enabled:value.enabled&&s.enabled,gain_db:gain,delay_ms:delay};
  }
 }
 doc.engine.master=legacy.sinks[settings.engine.master];
 choice.enabled=choice.enabled&&choice.inputs.length>0&&choice.destinations.length>0;
 for(const input of choice.inputs){const id=Object.entries(doc.sources).find(([,s])=>s.endpoint===input.endpoint)?.[0];const gains=[...new Set(Object.values(doc.routes).filter(r=>r.source===id&&r.enabled).map(r=>r.gain_db))];if(gains.length===1)input.gain_db=gains[0];else if(gains.length>1)choice.legacy_matrix=true;}
 choice.definitions={sources:clone(doc.sources),sinks:clone(doc.sinks)};
 doc.outputs[String(primary)]=choice;return graph.refreshEnabled(doc);
}
function project(doc,primary,defaults){
 const output=doc.outputs[String(primary)],saved=output?.legacy;
 if(saved?.settings){const settings=clone(saved.settings);settings.revision=doc.revision||0;return settings;}
 const settings=defaults();settings.revision=doc.revision||0;settings.engine={enabled:doc.engine.enabled,rate:doc.engine.rate,period_frames:doc.engine.period_frames,master:'hdmi'};settings.video_delay_ms=doc.video_delay_ms;settings.sources.ndi.enabled=false;settings.sinks.hdmi.mix={ndi:-60};
 const sources={},sinks={};
 function sourceAlias(id,s){if(s.type==='ndi'&&s.output===primary)return 'ndi';return id;}
 function sinkAlias(id,s){if(s.resource==='hdmi:'+primary)return 'hdmi';if(s.resource==='tessera:main:tx'&&(s.channel_start||1)===1)return 'net-out';return id;}
 const primaryRoutes=Object.values(doc.routes).filter(r=>r.output===primary);
 const publicInputs=output?.inputs||[],publicDestinations=output?.destinations||[];
 for(const [id,s]of Object.entries(doc.sources)){
  if(s.type==='ndi'&&s.output!==primary)continue;
  const routes=primaryRoutes.filter(r=>r.source===id);if(!routes.length&&!(s.type==='ndi'&&s.output===primary))continue;
  const alias=sourceAlias(id,s),choice=publicInputs.find(i=>i.endpoint===s.endpoint);
  const value={type:s.type,enabled:!!output?.enabled&&routes.some(r=>r.enabled),channels:s.channels,delay_ms:choice?.delay_ms||0,gain_db:choice?.gain_db||0};if(s.type==='alsa')value.alsa_pcm=alias==='net-in'?'ndimon_net_in':s.alsa_pcm;
  sources[alias]=value;
 }
 for(const [id,s]of Object.entries(doc.sinks)){
  const routes=primaryRoutes.filter(r=>r.sink===id);if(!routes.length&&s.resource!=='hdmi:'+primary)continue;
  const alias=sinkAlias(id,s),choice=publicDestinations.find(i=>i.endpoint===s.endpoint),mix={};
  for(const r of routes){const source=doc.sources[r.source];if(source.type==='ndi'&&source.output!==primary)continue;const sid=sourceAlias(r.source,source);mix[sid]=r.gain_db<=-60?-60:r.gain_db-(sources[sid]?.gain_db||0);}
  sinks[alias]={type:'alsa',enabled:!!output?.enabled&&routes.some(r=>r.enabled),channels:s.channels,alsa_pcm:alias==='net-out'?'ndimon_net_out':s.alsa_pcm,delay_ms:choice?.delay_ms||0,mix};
 }
 settings.sources={...settings.sources,...sources};settings.sinks={...settings.sinks,...sinks};
 if(!settings.sinks.hdmi.enabled){settings.sinks.hdmi.enabled=true;for(const key of Object.keys(settings.sinks.hdmi.mix))settings.sinks.hdmi.mix[key]=-60;}
 return settings;
}
function overlay(original,settings,primary,catalog){
 assertEditable(original,primary,catalog);
 const incoming=migrate(settings,primary,catalog),doc=clone(original);
 const before=project(original,primary,()=>clone(settings));
 const explicitEnable=Object.entries(settings.sources).some(([id,s])=>s.enabled&&!before.sources[id]?.enabled)||Object.entries(settings.sinks).some(([id,s])=>s.enabled&&!before.sinks[id]?.enabled);
 if(original.outputs[String(primary)]?.enabled===false&&!explicitEnable)incoming.outputs[String(primary)].enabled=false;
 for(const [id,r]of Object.entries(doc.routes))if(r.output===primary)delete doc.routes[id];
 for(const kind of ['sources','sinks'])for(const [id,value]of Object.entries(incoming[kind])){
  const existing=doc[kind][id];if(existing&&Object.values(doc.routes).some(r=>r[kind==='sources'?'source':'sink']===id)){
   if(existing.channels!==value.channels||existing.alsa_pcm!==value.alsa_pcm||existing.output!==value.output)throw Object.assign(Error('legacy edit would reconfigure a shared endpoint: '+id),{status:409});
   value.enabled=existing.enabled||value.enabled;
  }
  doc[kind][id]=value;
 }
 Object.assign(doc.routes,incoming.routes);doc.outputs[String(primary)]=incoming.outputs[String(primary)];
 doc.engine={...original.engine,...incoming.engine,master:doc.sinks[original.engine.master]?.enabled?original.engine.master:incoming.engine.master,...(original.engine.memory_budget_mib!==undefined?{memory_budget_mib:original.engine.memory_budget_mib}:{})};doc.video_delay_ms=settings.video_delay_ms;
 return graph.refreshEnabled(doc);
}
function assertEditable(doc,primary,catalog){
 const output=doc.outputs[String(primary)],routes=Object.values(doc.routes).filter(r=>r.output===primary),unsupported=()=>{throw Object.assign(Error('Use Outputs audio controls for this primary graph; legacy edits cannot represent its selected sources/destinations safely'),{status:409,code:'legacy_edit_unsupported'});};
 if(!output)return;
 if(output.legacy){for(const route of routes)if(!Object.values(output.legacy.sources||{}).includes(route.source)||!Object.values(output.legacy.sinks||{}).includes(route.sink))unsupported();return;}
 if(!Array.isArray(output.inputs)||!Array.isArray(output.destinations))unsupported();
 const relevant=[...routes.map(route=>[doc.sources[route.source],doc.sinks[route.sink]]),...Object.values(output.definitions?.sources||{}).map(source=>[source,null]),...Object.values(output.definitions?.sinks||{}).map(sink=>[null,sink])];
 for(const [source,sink]of relevant){if(source?.type==='ndi'&&source.output!==primary)unsupported();for(const [node,list]of [[source,catalog.inputs],[sink,catalog.destinations]])if(node?.resource?.startsWith('tessera:')){const full=Math.max(0,...list.filter(e=>e.provider==='tessera'&&e.resource===node.resource&&e.channel_start===1&&e.ready).map(e=>e.channels));if((node.channel_start||1)!==1||node.channels!==full)unsupported();}}
}
module.exports={migrate,project,overlay,endpoint,assertEditable};
