'use strict';
const express=require('express');
const fs=require('fs');
const {TesseraClient,TesseraError}=require('./tessera-client');
const {parseFlows,renderFlows,validateFlows}=require('./flows');
const FLOW='/etc/tessera/config.d/61-ndimon-flows.toml';
const DEVICE='/etc/ndimon-device-settings.json';
class InputError extends Error {constructor(message){super(message);this.http=400;}}
function object(body,keys){if(!body||typeof body!=='object'||Array.isArray(body)||Object.keys(body).some(k=>!keys.includes(k)))throw new TesseraError('invalid',{reason:'unknown or invalid fields'});return body;}
function integer(value,min,max,name){if(!Number.isInteger(value)||value<min||value>max)throw new TesseraError('invalid',{reason:`${name} must be ${min}..${max}`});return value;}
module.exports=function makeRouter(host){
 const router=express.Router(),client=host.tesseraClient||new TesseraClient(),io=host.tesseraFs||fs;
 let cached=null,cachedAt=0,pending=null,generation=0,mutation=Promise.resolve();
 const priv=(command,args=[],input)=>host.runPrivAsync(['plugin-exec','tessera',command,...args],300000,input);
 const readFlows=()=>{try{return parseFlows(io.readFileSync(FLOW,'utf8'));}catch(e){if(e.code==='ENOENT')return [];throw e;}};
 const command=(cmd,expect)=>expect===undefined?client.command(cmd):client.confirmedCommand(cmd,expect);
 const audioResult=result=>{if(!result.ok)throw Object.assign(new Error(result.errors?.join('; ')||'audio apply failed'),{http:result.status||400});return result;};
 const invalidate=()=>{cachedAt=0;generation++;};
 async function status(){if(cached&&Date.now()-cachedAt<2000)return cached;if(pending)return pending;const current=generation;pending=loadStatus().then(value=>{if(generation===current){cached=value;cachedAt=Date.now();}return value;}).finally(()=>{pending=null;});return pending;}
 async function loadStatus(){
  let daemon='online',state=null,metrics=null,error=null;
  try{state=await client.query('status');metrics=await client.query('metrics');}catch(e){daemon=client.online()?'online':'offline';error=e.message;}
  let flows=[],flowError=null;try{flows=readFlows();}catch(e){flowError=e.message;}
  const core=await host.sendIPC({action:'status'}),settings=host.audio.getSettings(),device=host.readJson(DEVICE);
  let embedded=null;try{embedded=JSON.parse(io.readFileSync(require('path').join(host.pluginDir||__dirname,'bundle-info.json'),'utf8'));}catch{}
  return {daemon,status:state,metrics,engine:core.audio||null,flows,audio:settings,audio_engine:device.audio_engine||'v1',audio_enabled:device.audio_engine==='v2'&&!!settings.sources['net-in']?.enabled,bundle:{installed_version:null,embedded_version:embedded?.version||null,daemon_version:state?.version||null,experimental:true},capabilities:{subscriptions:false},flow_toml:flowError?null:renderFlows(flows),error:error||flowError};
 }
 function endpoint(method,path,fn,{mutates=true}={}){router[method](path,async(req,res)=>{
  const run=async()=>{try{const result=await fn(req.body||{},req);if(mutates)invalidate();res.json(result??{ok:true});}catch(e){res.status(e.http||e.status||500).json({ok:false,error:e.code||e.message,detail:e.detail||e.message,...(e.impact?{impact:e.impact}:{})});}};
  if(mutates){const task=mutation.then(run);mutation=task.catch(()=>{});await task;}else await run();
 });}
 endpoint('get','/health',async()=>({installed:true,daemon:(await status()).daemon}),{mutates:false});
 endpoint('get','/status',status,{mutates:false});
 endpoint('get','/metrics',()=>client.query('metrics'),{mutates:false});
 endpoint('get','/diagnostics',async()=>({status:await client.query('status'),metrics:await client.query('metrics'),flows:readFlows(),experimental:true}),{mutates:false});
 endpoint('get','/flows',()=>({flows:readFlows(),toml:renderFlows(readFlows())}),{mutates:false});
 endpoint('post','/device',async body=>{
  object(body,['name','sample_rate','encoding','latency_us','label','expect']);const keys=Object.keys(body).filter(k=>k!=='expect');if(keys.length!==1)throw new InputError('change one device field at a time');const k=keys[0],v=body[k];let cmd;
  if(k==='name'){if(typeof v!=='string'||!v||Buffer.byteLength(v)>31||/[\x00-\x1f\x7f]/.test(v))throw new InputError('name must be 1..31 UTF-8 bytes without control characters');cmd={set_device_name:{device:'main',name:v}};}
  if(k==='sample_rate'){const audio=host.audio.getSettings();if(v!==48000&&(audio.sources['net-in']?.enabled||audio.sinks['net-out']?.enabled))throw new InputError('disable network audio before changing Tessera away from48000Hz');cmd={set_sample_rate:{device:'main',rate:integer(v,8000,384000,'sample_rate')}};}
  if(k==='encoding'){if(![16,24,32].includes(v))throw new InputError('encoding must be 16,24,32');cmd={set_encoding:{device:'main',encoding:v}};}
  if(k==='latency_us')cmd={set_latency:{device:'main',latency:integer(v,1,1000000,'latency_us')}};
  if(k==='label'){object(v,['direction','channel','label']);if(!['rx','tx'].includes(v.direction)||v.label!==null&&(typeof v.label!=='string'||Buffer.byteLength(v.label)>31||/[\x00-\x1f\x7f]/.test(v.label)))throw new InputError('invalid channel label');cmd={set_channel_label:{device:'main',direction:v.direction,channel:integer(v.channel,1,65535,'channel'),label:v.label}};}
  return {ok:true,result:await command(cmd,body.expect)};
 });
 endpoint('post','/clock',async body=>{
  object(body,['role','priority','expect']);if(Object.keys(body).filter(k=>k!=='expect').length!==1)throw new InputError('change one clock field at a time');let cmd;
  if(body.role!==undefined){if(!['follower','leader','auto'].includes(body.role))throw new InputError('invalid clock role');cmd={set_clock_role:{role:body.role}};}else cmd={set_clock_priority:{priority:integer(body.priority,0,255,'priority')}};
  return {ok:true,result:await command(cmd,body.expect)};
 });
 endpoint('post','/channels',async body=>{
  object(body,['rx','tx','expect']);integer(body.rx,1,16,'rx');integer(body.tx,1,16,'tx');
  const cmd={set_device_channels:{device:'main',rx:body.rx,tx:body.tx,dry_run:false}};
  if(body.expect===undefined){const dry=await client.command({set_device_channels:{...cmd.set_device_channels,dry_run:true}});throw new TesseraError('confirmation_required',{impact:dry.impact});}
  const before=await client.query('status'),device=before.devices.find(d=>d.key==='main'),previousAudio=host.audio.getSettings();
  const result=await client.confirmedCommand(cmd,body.expect);
  try{await priv('write-alsa',[String(body.rx),String(body.tx)]);const doc=host.audio.getSettings();if(doc.sources['net-in'])doc.sources['net-in'].channels=body.rx;if(doc.sinks['net-out'])doc.sinks['net-out'].channels=body.tx;audioResult(await host.audio.putSettings(doc));}
  catch(e){
   const rx=device?.rx_channels?.length,tx=device?.tx_channels?.length,rollback={set_device_channels:{device:'main',rx,tx,dry_run:false}};
   try{try{await client.command(rollback);}catch(impact){if(impact.code!=='confirmation_required'||!impact.impact?.token)throw impact;await client.confirmedCommand(rollback,impact.impact.token);}await priv('write-alsa',[String(rx),String(tx)]);audioResult(await host.audio.putSettings(previousAudio));}
   catch(failure){e.message+='; channel rollback failed: '+failure.message;e.http=503;}
   throw e;
  }
  return {ok:true,result,previous_channels:{rx:device?.rx_channels?.length,tx:device?.tx_channels?.length}};
 });
 const saveFlows=async body=>{
  object(body,['flows','expect']);let flows;try{flows=validateFlows(body.flows);}catch(e){throw new InputError(e.message);}const previous=readFlows();
  try{await priv('write-flows',[],JSON.stringify(flows));}catch(e){throw new InputError(e.stderr||e.message);}
  try{return {ok:true,result:await command('reload_config',body.expect),flows};}
  catch(error){try{await priv('write-flows',[],JSON.stringify(previous));if(error.code!=='confirmation_required'){try{await client.command('reload_config');}catch(impact){if(impact.code!=='confirmation_required'||!impact.impact?.token)throw impact;await client.confirmedCommand('reload_config',impact.impact.token);}}}catch(rollback){error.message+='; rollback/reconciliation failed: '+rollback.message;error.http=503;}throw error;}
 };
 endpoint('put','/flows',saveFlows);endpoint('post','/flows',saveFlows);
 endpoint('post','/service',async body=>{object(body,['action','confirm','expect']);if(!['start','stop','restart','reload'].includes(body.action))throw new InputError('invalid service action');if(body.action==='reload')return {ok:true,result:await command('reload_config',body.expect)};if(['stop','restart'].includes(body.action)&&body.confirm!==true)throw new TesseraError('confirmation_required',{impact:{restart:{scope:'daemon',reason:'Tessera service '+body.action,what_drops:['network audio only']}}});await priv('service',[body.action]);return {ok:true};});
 async function switchEngine(value){const cfg=host.readJson(DEVICE);if((cfg.audio_engine||'v1')===value)return;cfg.audio_engine=value;if(!host.writeJson(DEVICE,cfg))throw new InputError('could not save engine selection');await host.runPrivAsync(['restart-service','ndimon-r'],30000);}
 endpoint('post','/audio/enable',async body=>{
  object(body,['confirm']);if(body.confirm!==true)throw new TesseraError('confirmation_required',{impact:{restart:{scope:'decoder',reason:'Enable experimental audio engine v2; NDI video/audio restart briefly'},gate:'Experimental audio; external hardware and sustained clock behavior are not certified.'}});
  const state=await client.query('status'),d=state.devices.find(x=>x.key==='main');if(!d)throw new InputError('main Tessera device missing');
  if(d.sample_rate!==48000)throw new InputError('NDIMon audio engine operates at 48000Hz; set Tessera to48000Hz before enabling');
  const rx=integer(d.rx_channels.length,1,16,'RX PCM channels'),tx=integer(d.tx_channels.length,1,16,'TX PCM channels');await priv('write-alsa',[String(rx),String(tx)]);
  const before=host.audio.getSettings(),cfg=host.readJson(DEVICE),doc=JSON.parse(JSON.stringify(before));
  doc.sources['net-in']={type:'alsa',enabled:true,channels:rx,delay_ms:doc.sources['net-in']?.delay_ms||0,gain_db:doc.sources['net-in']?.gain_db||0,alsa_pcm:'ndimon_net_in'};
  doc.sinks['net-out']={type:'alsa',enabled:true,channels:tx,delay_ms:0,alsa_pcm:'ndimon_net_out',mix:doc.sinks['net-out']?.mix||{ndi:0,'net-in':-60}};doc.sinks.hdmi.mix['net-in']??=0;
  audioResult(await host.audio.putSettings(doc));
  try{await switchEngine('v2');return {ok:true,audio_engine:'v2',restart_required:false};}
  catch(error){try{await host.audio.putSettings(before);if(!host.writeJson(DEVICE,cfg))throw new InputError('engine rollback write failed');await host.runPrivAsync(['restart-service','ndimon-r'],30000);}catch(rollback){error.message+='; rollback failed: '+rollback.message;}throw error;}
 });
 endpoint('post','/audio/disable',async body=>{object(body,[]);await switchEngine('v1');const doc=host.audio.getSettings();if(doc.sources['net-in'])doc.sources['net-in'].enabled=false;if(doc.sinks['net-out'])doc.sinks['net-out'].enabled=false;audioResult(await host.audio.putSettings(doc));return {ok:true,audio_engine:'v1'};});
 endpoint('post','/audio/delay',async body=>{object(body,['ms']);integer(body.ms,0,2000,'ms');const current=host.audio.getSettings().sources['net-in'];if(!current)throw new InputError('network input is not registered');audioResult(await host.audio.patchSource('net-in',{delay_ms:body.ms}));return {ok:true};});
 endpoint('post','/audio/routing',async body=>{object(body,['net_out','hdmi']);const doc=host.audio.getSettings();for(const [key,id] of [['net_out','net-out'],['hdmi','hdmi']]){if(body[key]===undefined)continue;object(body[key],['ndi','net-in']);if(!doc.sinks[id])throw new InputError('network audio is not registered');for(const [source,db]of Object.entries(body[key])){if(typeof db!=='number'||!Number.isFinite(db)||db < -60||db>12)throw new InputError('gain must be -60..12dB');doc.sinks[id].mix[source]=db;}}audioResult(await host.audio.putSettings(doc));return {ok:true};});
 for(const method of ['get','put'])endpoint(method,'/subscriptions',()=>{throw new TesseraError('unsupported',{what:'Vendor subscription control is unavailable; configure static flows.'});},{mutates:false});
 client.subscribeEvents?.(event=>{invalidate();require('../../api/plugin-events').broadcastEvent('tessera_event',{event});},invalidate);
 router.status=()=>({daemon:client.online()?'online':'offline',clock_state:cached?.status?.clocks?.[0]?.state,offset_ns:cached?.status?.clocks?.[0]?.offset_ns});
 router.close=()=>client.close();
 return router;
};
