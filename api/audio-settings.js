'use strict';
const Ajv = require('ajv');
const id = '^[a-z][a-z0-9-]{0,31}$';
const integer = (minimum, maximum) => ({type:'integer', minimum, maximum});
const gain = {type:'number',minimum:-60,maximum:12};
const common = {type:{enum:['ndi','alsa']},enabled:{type:'boolean'},channels:integer(1,16),delay_ms:integer(0,5000),alsa_pcm:{type:'string',pattern:'^[A-Za-z0-9:_=,.+-]{1,512}$',not:{pattern:'[^A-Za-z0-9:_=,.+-]'}}};
const source = {type:'object',additionalProperties:false,required:['type','enabled','channels','delay_ms','gain_db'],properties:{...common,gain_db:gain},allOf:[{if:{properties:{type:{const:'alsa'}}},then:{required:['alsa_pcm']},else:{not:{required:['alsa_pcm']}}}]};
const sink = {type:'object',additionalProperties:false,required:['type','enabled','channels','delay_ms','alsa_pcm','mix'],properties:{...common,type:{const:'alsa'},mix:{type:'object',propertyNames:{pattern:id},additionalProperties:gain}}};
const schema = {type:'object',additionalProperties:false,required:['version','engine','sources','sinks','video_delay_ms'],properties:{
 version:{const:1},revision:integer(0,Number.MAX_SAFE_INTEGER),engine:{type:'object',additionalProperties:false,required:['enabled','rate','period_frames','master'],properties:{enabled:{type:'boolean'},rate:{const:48000},period_frames:{enum:[128,256,512,1024,2048]},master:{type:'string',pattern:id},memory_budget_mib:integer(1,512)}},
 sources:{type:'object',required:['ndi'],maxProperties:32,propertyNames:{pattern:id},additionalProperties:source},sinks:{type:'object',required:['hdmi'],maxProperties:32,propertyNames:{pattern:id},additionalProperties:sink},video_delay_ms:integer(0,5000)}};
const check = new Ajv({allErrors:true}).compile(schema);
function defaults(){return {version:1,revision:0,engine:{enabled:true,rate:48000,period_frames:512,master:'hdmi'},sources:{ndi:{type:'ndi',enabled:true,channels:2,delay_ms:0,gain_db:0}},sinks:{hdmi:{type:'alsa',enabled:true,channels:2,alsa_pcm:'auto',delay_ms:0,mix:{ndi:0}}},video_delay_ms:0};}
function validateV1(doc){
 if(!check(doc))return {ok:false,errors:check.errors.map(e=>`${e.instancePath} ${e.message}`)};
 const errors=[];
 if(doc.sources.ndi.type!=='ndi')errors.push('sources.ndi.type must be ndi');
 for(const [key,s]of Object.entries(doc.sources))if(key!=='ndi' && s.type!=='alsa')errors.push(`sources.${key}.type must be alsa`);
 const master=doc.sinks[doc.engine.master];if(!master || !master.enabled || master.type!=='alsa')errors.push('engine.master must name an enabled ALSA sink');
 return {ok:!errors.length,errors};
}
const graph=require('./audio-graph');
const legacy=require('./audio-legacy');
function validate(doc){if(doc?.version!==2)return validateV1(doc);const result=graph.validate(doc);if(!result.ok)return result;for(const out of Object.values(doc.outputs))if(out.legacy?.settings){const check=validateV1(out.legacy.settings);if(!check.ok)return {ok:false,errors:['invalid legacy projection metadata',...check.errors]};}return result;}
module.exports={schema:{anyOf:[schema,graph.schema]},legacySchema:schema,defaults,validate,validateV1,KEYS:['version','revision','engine','sources','sinks','outputs','routes','video_delay_ms']};

// All graph and compatibility mutations share one writer. Snapshot PUTs use CAS;
// patch helpers read their projection only after acquiring this queue.
function createStore(deps={}){
 const fs=require('fs'),persistence=require('./persistence');
 const file='/etc/ndimon-audio-settings.json';
 const read=deps.read||(()=>{try{return JSON.parse(fs.readFileSync(file,'utf8'));}catch(e){if(e.code==='ENOENT')return defaults();throw e;}});
 const write=deps.write||(doc=>{persistence.writeConfig(file,JSON.stringify(doc));return true;});
 const send=deps.sendIPC||require('./ipc-oneshot').sendIPC;
 let tail=Promise.resolve(),pendingSnapshot=null,catalogPending=null,catalogCache=null,catalogAt=0,primary=deps.primaryOutput||1;
 const serial=fn=>{const result=tail.then(async()=>{let release;try{release=require('./audio-write-gate').acquireAudio();return await fn();}catch(e){return failure(e);}finally{release?.();}});tail=result.catch(()=>{});return result;};
 function raw(){if(pendingSnapshot)return graph.clone(pendingSnapshot);try{const doc=read();const checked=validate(doc);if(!checked.ok&&doc?.version===2)throw Object.assign(Error('Saved audio graph is invalid: '+checked.errors.join('; ')),{status:503,code:'invalid_saved_graph'});return checked.ok?{...doc,revision:doc.revision||0}:defaults();}catch(e){if(e instanceof SyntaxError)return defaults();throw e;}}
 function primaryOf(doc){return Number(Object.entries(doc.outputs||{}).find(([,v])=>v.primary)?.[0])||primary;}
 function getSettings(){const doc=raw();return doc.version===2?legacy.project(doc,primaryOf(doc),defaults):doc;}
 function failure(error){return {ok:false,error:error.code||error.message,errors:[error.message],status:error.status||500,...(error.impact?{impact:error.impact}:{})};}
 function revisionConflict(previous){return {ok:false,status:409,error:'revision_conflict',revision:previous.revision||0,errors:['Audio settings changed. Reload the latest settings before saving.']};}
 async function context(doc){
  if(deps.getCatalog){const value=await deps.getCatalog(doc);return value;}
  if(catalogCache&&Date.now()-catalogAt<1500)return require('./audio-endpoints').buildCatalog({...catalogCache,savedGraph:doc});
  if(!catalogPending)catalogPending=(async()=>{
   const outputs=await send({action:'get_status_all'}),status=await send({action:'status'});primary=status.audio?.primary_output||primary;
   let tesseraStatus=null,tesseraEnabled=false;
   try{const registry=require('./plugins').getRegistry();tesseraEnabled=registry?.get('tessera')?.state==='enabled';if(tesseraEnabled){const {TesseraClient}=require('/opt/ndimon-r/plugins/tessera/tessera-client');const client=new TesseraClient();try{tesseraStatus=await client.query('status');}finally{client.close();}}}catch{}
   catalogCache={outputs:Array.isArray(outputs)?outputs:[],tesseraStatus,tesseraEnabled};catalogAt=Date.now();return catalogCache;
  })().finally(()=>{catalogPending=null;});
  const gathered=await catalogPending;return require('./audio-endpoints').buildCatalog({...gathered,savedGraph:doc});
 }
 async function ensurePcms(doc,catalog,previous=null){
  if(deps.ensurePcms)return deps.ensurePcms(doc,catalog,previous);
  const selections=[],seen=new Set();
  for(const candidate of [previous,doc])if(candidate)for(const kind of ['sources','sinks'])for(const node of Object.values(candidate[kind]||{})){
   if(!node.enabled||!/^tessera:main:(rx|tx)$/.test(node.resource||''))continue;
   const value={direction:node.resource.endsWith(':rx')?'rx':'tx',first_channel:node.channel_start||1,channels:node.channels},key=JSON.stringify(value);
   if(!seen.has(key)){seen.add(key);selections.push(value);}
  }
  if(selections.length)await require('./routes/lib').runPrivAsync(['plugin-exec','tessera','write-pcms'],300000,JSON.stringify(selections));
 }
 async function rollback(previous){const errors=[];try{if(!write(previous))errors.push('rollback write failed');}catch(e){errors.push('rollback write failed: '+e.message);}try{const reload=await send({action:'reload_audio'});if(!reload.ok)errors.push('rollback reload failed: '+(reload.error||'audio apply failed'));}catch(e){errors.push('rollback reload failed: '+e.message);}return errors;}
 async function commit(requested,previous,{catalog=null,preparePcms=true}={}){
  const checked=validate(requested);if(!checked.ok)return checked;
  const doc=graph.clone(requested);doc.revision=(previous.revision||0)+1;
  if(doc.video_delay_ms>0){const cap=await send({action:'check_audio'});if(!cap.ok||!Number.isInteger(cap.video_delay_cap_ms))return {ok:false,status:503,errors:[cap.error||'decoder did not report video delay cap']};if(doc.video_delay_ms>cap.video_delay_cap_ms)return {ok:false,errors:[`video delay capped at ${cap.video_delay_cap_ms} ms`]};}
  if(doc.version===2&&preparePcms)await ensurePcms(doc,catalog||await context(doc),previous);
  pendingSnapshot=previous;
  try{
   if(!write(doc)){const errors=await rollback(previous);return {ok:false,status:500,errors:['could not write audio settings',...errors]};}
   const apply=await send({action:'reload_audio'});
   if(!apply.ok&&doc.version===1&&apply.restart_required&&Array.isArray(apply.refused)&&apply.refused.length){
    const status=await send({action:'status'}),effective=status.audio?.settings&&graph.clone(status.audio.settings);
    if(!validate(effective).ok){const errors=await rollback(previous);return {...apply,apply,errors:['could not read effective audio settings',...errors],status:503};}
    effective.revision=doc.revision;if(!write(effective))return {...apply,apply,status:500,errors:['could not persist effective audio settings']};
    return {...apply,apply,revision:effective.revision,status:409,errors:[apply.error||'structural change refused']};
   }
   if(!apply.ok){const errors=await rollback(previous);return {...apply,ok:false,apply,status:['decoder_offline','timeout'].includes(apply.error)?503:apply.restart_required?409:400,errors:[apply.error||'audio apply failed',...errors]};}
   catalogAt=0;return {ok:true,revision:doc.revision,apply};
  }catch(e){const errors=await rollback(previous);return {...failure(e),errors:[e.message,...errors]};}finally{pendingSnapshot=null;}
 }
 async function putSnapshot(doc,previous){
  if(!doc||typeof doc!=='object'||Array.isArray(doc))return {ok:false,errors:['invalid audio settings']};
  if(doc.revision!==(previous.revision||0))return revisionConflict(previous);
  if(previous.version===2&&doc.version===1){const checked=validateV1(doc);if(!checked.ok)return checked;const catalog=await context(previous);return commit(legacy.overlay(previous,doc,primaryOf(previous),catalog),previous,{catalog});}
  return commit(doc,previous);
 }
 function patch(kind,id,changes){return serial(async()=>{try{if(!new RegExp('^[a-z][a-z0-9-]{0,31}$').test(id)||!changes||typeof changes!=='object'||Array.isArray(changes))return {ok:false,errors:['invalid endpoint patch']};const previous=raw(),doc=getSettings();doc[kind][id]={...doc[kind][id],...changes};return await putSnapshot(doc,previous);}catch(e){return failure(e);}});}
 return {
  getSettings,getCanonicalSettings:raw,
  getEngine(){const lib=deps.device||require('./routes/lib');return {audio_engine:lib.readJson('/etc/ndimon-device-settings.json').audio_engine||'v1',experimental:true};},
  setEngine:value=>serial(async()=>{try{if(!value||Object.keys(value).some(k=>!['audio_engine','confirm'].includes(k))||!['v1','v2'].includes(value.audio_engine)||value.confirm!==true)return {ok:false,status:400,error:'explicit_confirmation_required',errors:['Explicit engine confirmation is required']};const lib=deps.device||require('./routes/lib'),file='/etc/ndimon-device-settings.json',previous=lib.readJson(file);if((previous.audio_engine||'v1')===value.audio_engine)return {ok:true,audio_engine:value.audio_engine};if(value.audio_engine==='v2'){const doc=raw(),checked=validate(doc);if(!checked.ok)return checked;await ensurePcms(doc,await context(doc));}if(!lib.writeJson(file,{...previous,audio_engine:value.audio_engine}))throw Error('could not save engine selection');try{await lib.runPrivAsync(['restart-service','ndimon-r'],30000);}catch(e){if(!lib.writeJson(file,previous))e.message+='; engine selection rollback write failed';try{await lib.runPrivAsync(['restart-service','ndimon-r'],30000);}catch(rollback){e.message+='; engine rollback restart failed: '+rollback.message;}throw e;}return {ok:true,audio_engine:value.audio_engine,restarted:true};}catch(e){return failure(e);}}),getPrimaryEnabled:()=>{const doc=raw();return doc.version!==2||!!doc.outputs[String(primaryOf(doc))]?.enabled;},
  putSettings:doc=>serial(async()=>{try{return await putSnapshot(graph.clone(doc),raw());}catch(e){return failure(e);}}),
  patchSource:(id,value)=>patch('sources',id,value),patchSink:(id,value)=>patch('sinks',id,value),
  async getEndpoints(){const doc=raw();return require('./audio-endpoints').publicCatalog(await context(doc),doc.revision||0);},
  async getOutput(ch){const doc=raw(),catalog=await context(doc),p=primaryOf(doc);if(!Number.isInteger(ch)||ch<1||ch>8)throw Object.assign(Error('output must be1..8'),{status:400});let value;if(doc.version===1){try{value=legacy.migrate(doc,p,catalog,{disablePrimary:true}).outputs[String(ch)];if(value)value.enabled=doc.engine.enabled&&Object.values(doc.sources).some(s=>s.enabled)&&Object.values(doc.sinks).some(s=>s.enabled);}catch(e){throw e;}}else value=doc.outputs[String(ch)];const result=value?{enabled:value.enabled,inputs:value.inputs||[],destinations:value.destinations||[],...(value.legacy_matrix?{legacy_matrix:true,warning:'Legacy per-destination gains and mutes are preserved. Changing this mix requires explicit normalization.'}:{})}:{enabled:false,inputs:[],destinations:[]};return {...graph.clone(result),revision:doc.revision||0};},
  putOutput:(ch,choice)=>serial(async()=>{try{
   const previous=raw();if(!Number.isInteger(ch)||ch<1||ch>8)return {ok:false,errors:['output must be1..8']};if(choice?.revision!==(previous.revision||0))return revisionConflict(previous);
   const catalog=await context(previous),p=primaryOf(previous);let doc=previous;
   if(doc.version===1){doc=legacy.migrate(doc,p,catalog,{disablePrimary:ch===p&&choice.enabled===false});doc.outputs[String(p)].primary=true;}
   doc=graph.compileOutput(doc,ch,choice,catalog);
   return await commit(doc,previous,{catalog});
  }catch(e){return failure(e);}}),
  removeIds:(...ids)=>serial(async()=>{try{ids=ids.flat();if(ids.some(x=>x==='ndi'||x==='hdmi'))return {ok:false,errors:['reserved audio ids cannot be removed']};const previous=raw();if(previous.version===2&&ids.some(id=>id==='net-in'||id==='net-out')){const doc=graph.clone(previous),removed=new Set();for(const [kind,direction]of [['sources','rx'],['sinks','tx']])if(ids.includes(direction==='rx'?'net-in':'net-out'))for(const [id,node]of Object.entries(doc[kind]))if(node.resource==='tessera:main:'+direction){removed.add(id);delete doc[kind][id];}for(const [id,route]of Object.entries(doc.routes))if(removed.has(route.source)||removed.has(route.sink))delete doc.routes[id];graph.refreshEnabled(doc);return await commit(doc,previous,{preparePcms:false});}const doc=getSettings();for(const id of ids){delete doc.sources[id];delete doc.sinks[id];for(const sink of Object.values(doc.sinks))delete sink.mix[id];}if(!doc.sinks[doc.engine.master]){doc.engine.master='hdmi';doc.sinks.hdmi.enabled=true;}return await putSnapshot(doc,previous);}catch(e){return failure(e);}})
 };
}
Object.assign(module.exports,{createStore},createStore());
