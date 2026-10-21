'use strict';
const crypto=require('crypto');
const Ajv=require('ajv');
const id='^[a-z][a-z0-9-]{0,31}$';
const integer=(minimum,maximum)=>({type:'integer',minimum,maximum});
const gain={type:'number',minimum:-60,maximum:12};
const inputChoice={type:'object',additionalProperties:false,required:['endpoint','gain_db','delay_ms'],properties:{endpoint:{type:'string',minLength:1,maxLength:160},gain_db:gain,delay_ms:integer(0,5000)}};
const destinationChoice={type:'object',additionalProperties:false,required:['endpoint','delay_ms'],properties:{endpoint:{type:'string',minLength:1,maxLength:160},delay_ms:integer(0,5000)}};
const choices={type:'object',additionalProperties:false,required:['enabled','inputs','destinations'],properties:{revision:integer(0,Number.MAX_SAFE_INTEGER),normalize_matrix:{type:'boolean'},enabled:{type:'boolean'},inputs:{type:'array',maxItems:32,items:inputChoice},destinations:{type:'array',maxItems:32,items:destinationChoice}}};
const endpointProperties={type:{enum:['ndi','alsa']},enabled:{type:'boolean'},channels:integer(1,16),output:integer(1,8),alsa_pcm:{type:'string',pattern:'^[A-Za-z0-9:_=,.+-]{1,512}$',not:{pattern:'[^A-Za-z0-9:_=,.+-]'}},endpoint:{type:'string',maxLength:160},resource:{type:'string',maxLength:160},channel_start:integer(1,65535),channel_count:integer(1,16)};
const source={type:'object',additionalProperties:false,required:['type','enabled','channels'],properties:endpointProperties,allOf:[{if:{properties:{type:{const:'ndi'}}},then:{required:['output'],not:{required:['alsa_pcm']}},else:{required:['alsa_pcm'],not:{required:['output']}}}]};
const sink={type:'object',additionalProperties:false,required:['type','enabled','channels','alsa_pcm'],properties:{...endpointProperties,type:{const:'alsa'}},not:{required:['output']}};
const route={type:'object',additionalProperties:false,required:['output','source','sink','enabled','gain_db','delay_ms'],properties:{output:integer(1,8),source:{type:'string',pattern:id},sink:{type:'string',pattern:id},enabled:{type:'boolean'},gain_db:gain,delay_ms:integer(0,5000),mapping:{const:'default'}}};
const schema={type:'object',additionalProperties:false,required:['version','engine','sources','sinks','outputs','routes','video_delay_ms'],properties:{version:{const:2},revision:integer(0,Number.MAX_SAFE_INTEGER),engine:{type:'object',additionalProperties:false,required:['enabled','rate','period_frames','master'],properties:{enabled:{type:'boolean'},rate:{const:48000},period_frames:{enum:[128,256,512,1024,2048]},master:{type:'string',anyOf:[{pattern:id},{const:''}]},memory_budget_mib:integer(1,512)}},sources:{type:'object',maxProperties:32,propertyNames:{pattern:id},additionalProperties:source},sinks:{type:'object',maxProperties:32,propertyNames:{pattern:id},additionalProperties:sink},outputs:{type:'object',maxProperties:8,propertyNames:{pattern:'^[1-8]$'},additionalProperties:{type:'object',required:['enabled'],properties:{enabled:{type:'boolean'},inputs:choices.properties.inputs,destinations:choices.properties.destinations,legacy:{type:'object'},legacy_matrix:{type:'boolean'}}}},routes:{type:'object',propertyNames:{pattern:id},additionalProperties:route},video_delay_ms:integer(0,5000)}};
const ajv=new Ajv({allErrors:true});
const check=ajv.compile(schema),checkChoices=ajv.compile(choices);
const clone=value=>JSON.parse(JSON.stringify(value));
function overlap(a,b){return !!a.resource&&a.resource===b.resource&&(a.channel_start||1)<(b.channel_start||1)+(b.channel_count||b.channels)&&(b.channel_start||1)<(a.channel_start||1)+(a.channel_count||a.channels);}
function identity(value){return value.type==='ndi'?'ndi:'+value.output:value.resource?value.resource+':'+(value.channel_start||1)+':'+(value.channel_count||value.channels):'alsa:'+value.alsa_pcm;}
function validate(doc){
 if(!check(doc))return {ok:false,errors:check.errors.map(e=>`${e.instancePath} ${e.message}`)};
 const errors=[],master=doc.sinks[doc.engine.master];if((Object.keys(doc.sources).length||Object.keys(doc.sinks).length||Object.keys(doc.routes).length)?!master?.enabled:doc.engine.master!=='')errors.push('engine.master must name an enabled ALSA sink');
 for(const [kind,values]of [['source',doc.sources],['sink',doc.sinks]])for(const [name,value]of Object.entries(values)){if(value.channel_count!==undefined&&value.channel_count!==value.channels)errors.push(`${kind} ${name}: channel_count must equal channels`);if((value.channel_start||1)+value.channels>65536)errors.push(`${kind} ${name}: channel range overflows`);}
 const sinks=Object.entries(doc.sinks);for(let i=0;i<sinks.length;i++)for(let j=i+1;j<sinks.length;j++){const [a,x]=sinks[i],[b,y]=sinks[j];if((x.alsa_pcm===y.alsa_pcm&&x.alsa_pcm!=='auto')||(x.alsa_pcm==='auto'&&y.alsa_pcm==='auto'&&x.resource===y.resource)||overlap(x,y))errors.push(`sinks ${a}/${b} would create overlapping physical writers`);}
 const contributions=new Map(),inputRanges=new Map();
 for(const [name,r]of Object.entries(doc.routes)){
  const s=doc.sources[r.source],t=doc.sinks[r.sink];if(!doc.outputs[String(r.output)]||!s||!t){errors.push(`route ${name} references missing output/source/sink`);continue;}
  if(!r.enabled||!doc.outputs[String(r.output)].enabled||!s.enabled||!t.enabled)continue;
  let selected=inputRanges.get(r.output);if(!selected){selected=new Map();inputRanges.set(r.output,selected);}for(const [otherId,other]of selected)if(otherId!==r.source&&(identity(other)===identity(s)||overlap(other,s)))errors.push(`output ${r.output} has overlapping source selections ${otherId}/${r.source}`);selected.set(r.source,s);
  const key=identity(s)+'->'+identity(t),prior=contributions.get(key);if(prior!==undefined)errors.push(`route ${name} duplicates source/sink contribution from output ${prior}`);else contributions.set(key,r.output);
 }
 return {ok:!errors.length,errors};
}
function assertChoices(value){if(!checkChoices(value))throw Object.assign(Error(checkChoices.errors.map(e=>`${e.instancePath} ${e.message}`).join('; ')),{status:400});return clone(value);}
function resolve(catalog,id,direction){const entry=catalog[direction].find(e=>e.id===id);if(!entry)throw Object.assign(Error('unknown audio endpoint: '+id),{status:400});return entry;}
function concrete(endpoint,direction){const value={type:endpoint.provider==='ndi'?'ndi':'alsa',enabled:true,channels:endpoint.channels,endpoint:endpoint.id};if(value.type==='ndi')value.output=endpoint.output;else Object.assign(value,{alsa_pcm:endpoint.alsa_pcm,resource:endpoint.resource,channel_start:endpoint.channel_start||1,channel_count:endpoint.channels});return value;}
function routeId(output,source,sink){return 'o'+output+'-'+crypto.createHash('sha256').update(source+'\0'+sink).digest('hex').slice(0,24);}
function compileOutput(original,ch,requested,catalog){
 const doc=clone(original),choice=assertChoices(requested);delete choice.revision;delete choice.normalize_matrix;
 choice.enabled=choice.enabled&&choice.inputs.length>0&&choice.destinations.length>0;
 const inputs=choice.inputs.map(v=>({...v,entry:resolve(catalog,v.endpoint,'inputs')})),destinations=choice.destinations.map(v=>({...v,entry:resolve(catalog,v.endpoint,'destinations')}));
 for(const values of [inputs,destinations]){const seen=new Set();for(let i=0;i<values.length;i++){const a=values[i].entry,key=identity(concrete(a));if(seen.has(key))throw Object.assign(Error('duplicate canonical endpoint selection: '+a.label),{status:409});seen.add(key);for(let j=0;j<i;j++)if(overlap(concrete(a),concrete(values[j].entry)))throw Object.assign(Error('overlapping channel selections: '+a.label+' / '+values[j].entry.label),{status:409});}}
 const previousRoutes=Object.values(original.routes).filter(r=>r.output===ch),previous=original.outputs[String(ch)];
 const signature=value=>JSON.stringify({inputs:(value.inputs||[]).map(i=>[resolve(catalog,i.endpoint,'inputs').node_id||i.endpoint,i.gain_db]).sort(),destinations:(value.destinations||[]).map(i=>resolve(catalog,i.endpoint,'destinations').node_id||i.endpoint).sort()});
 const sameMix=previous&&signature(previous)===signature(choice);
 for(const [id,route]of Object.entries(doc.routes))if(route.output===ch)delete doc.routes[id];
 const definitions={sources:{},sinks:{}};
 for(const {entry}of inputs)definitions.sources[entry.node_id||entry.id]=concrete(entry,'input');
 for(const {entry}of destinations)definitions.sinks[entry.node_id||entry.id]=concrete(entry,'destination');
 for(const kind of ['sources','sinks'])for(const [id,node]of Object.entries(definitions[kind])){const existing=doc[kind][id];if(existing&&identity(existing)===identity(node)){if(existing.channels!==node.channels)throw Object.assign(Error('shared endpoint geometry differs: '+id),{status:409});definitions[kind][id]={...clone(existing),enabled:true};}}
 if(choice.enabled){for(const {entry}of inputs)if(['ndi','hdmi','tessera'].includes(entry.provider))doc.sources[entry.node_id||entry.id]=definitions.sources[entry.node_id||entry.id];for(const {entry}of destinations)if(['ndi','hdmi','tessera'].includes(entry.provider))doc.sinks[entry.node_id||entry.id]=definitions.sinks[entry.node_id||entry.id];}
 if(choice.enabled)for(const input of inputs)for(const destination of destinations){const source=input.entry.node_id||input.entry.id,sink=destination.entry.node_id||destination.entry.id,delay=input.delay_ms+destination.delay_ms;if(delay>5000)throw Object.assign(Error(`effective delay for ${input.entry.label} → ${destination.entry.label} exceeds5000ms`),{status:400});if(input.entry.provider==='unknown'||destination.entry.provider==='unknown')continue;doc.routes[routeId(ch,source,sink)]={output:ch,source,sink,enabled:true,gain_db:input.gain_db,delay_ms:delay};}
 const after=Object.values(doc.routes).filter(r=>r.output===ch);
 if(previous?.legacy_matrix&&!sameMix&&requested.normalize_matrix!==true)throw Object.assign(Error('Legacy per-destination gains/mutes would change; explicit normalization required'),{status:409,code:'matrix_confirmation_required',impact:{warning:'Muted destinations may become audible',before:previousRoutes.map(({source,sink,gain_db})=>({source,sink,gain_db})),after:after.map(({source,sink,gain_db})=>({source,sink,gain_db}))}});
 if(previous?.legacy_matrix&&sameMix)for(const route of after){const prior=previousRoutes.find(r=>r.source===route.source&&r.sink===route.sink);if(prior)route.gain_db=prior.gain_db;else if(previous.legacy){const l=previous.legacy,sa=Object.keys(l.sources||{}).find(k=>l.sources[k]===route.source),ta=Object.keys(l.sinks||{}).find(k=>l.sinks[k]===route.sink),sg=l.settings.sources[sa]?.gain_db,mix=l.settings.sinks[ta]?.mix?.[sa];if(Number.isFinite(sg)&&Number.isFinite(mix))route.gain_db=sg<=-60||mix<=-60?-60:sg+mix;}}
 doc.outputs[String(ch)]={...doc.outputs[String(ch)],...choice,definitions};
 if(previous?.legacy&&sameMix){const saved=clone(previous.legacy);for(const [alias,id]of Object.entries(saved.sources||{})){const selected=inputs.find(i=>(i.entry.node_id||i.entry.id)===id);if(selected)saved.settings.sources[alias].delay_ms=selected.delay_ms;}for(const [alias,id]of Object.entries(saved.sinks||{})){const selected=destinations.find(i=>(i.entry.node_id||i.entry.id)===id);if(selected)saved.settings.sinks[alias].delay_ms=selected.delay_ms;}doc.outputs[String(ch)].legacy=saved;}
 else{delete doc.outputs[String(ch)].legacy;delete doc.outputs[String(ch)].legacy_matrix;}
 refreshEnabled(doc);
 const result=validate(doc);if(!result.ok)throw Object.assign(Error(result.errors.join('; ')),{status:409});return doc;
}
function refreshEnabled(doc){
 const active=Object.values(doc.routes).filter(r=>r.enabled&&doc.outputs[String(r.output)]?.enabled);
 const sourceIds=new Set(active.map(r=>r.source)),sinkIds=new Set(active.map(r=>r.sink));
 for(const [id,r]of Object.entries(doc.routes))if(!active.includes(r))delete doc.routes[id];
 for(const id of Object.keys(doc.sources))if(!sourceIds.has(id))delete doc.sources[id];else doc.sources[id].enabled=true;
 if(active.length&&!sinkIds.has(doc.engine.master))doc.engine.master=active.find(r=>doc.sinks[r.sink].resource?.startsWith('hdmi:'))?.sink||active[0].sink;
 if(!active.length){doc.sources={};doc.sinks={};doc.routes={};doc.engine.master='';return doc;}
 for(const id of Object.keys(doc.sinks))if(!sinkIds.has(id)&&id!==doc.engine.master)delete doc.sinks[id];else doc.sinks[id].enabled=true;
 return doc;
}
module.exports={schema,choices,validate,assertChoices,identity,overlap,concrete,compileOutput,routeId,clone,refreshEnabled};
