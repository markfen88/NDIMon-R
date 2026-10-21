'use strict';
const fs=require('fs');
const {parseFlows,validateFlows}=require('./flows');
const {execFile}=require('child_process');
const audio=require('../../api/audio-settings');
const {TesseraClient}=require('./tessera-client');
const FLOW='/etc/tessera/config.d/61-ndimon-flows.toml';
function readFlows(){try{return parseFlows(fs.readFileSync(FLOW,'utf8'));}catch(e){if(e.code==='ENOENT')return [];throw e;}}
function validate(doc){try{if(!doc||typeof doc!=='object'||Object.keys(doc).some(k=>!['flows','audio'].includes(k)))throw Error('invalid Tessera backup');validateFlows(doc.flows);if(doc.audio){const defaults=audio.defaults();for(const k of ['sources','sinks'])if(doc.audio[k])Object.assign(defaults[k],doc.audio[k]);const result=audio.validate(defaults);if(!result.ok)throw Error(result.errors.join('; '));if(Object.keys(doc.audio).some(k=>!['sources','sinks'].includes(k))||Object.keys(doc.audio.sources||{}).some(k=>k!=='net-in')||Object.keys(doc.audio.sinks||{}).some(k=>k!=='net-out'))throw Error('invalid plugin audio ownership');if(doc.audio.sources?.['net-in']?.alsa_pcm!=='ndimon_net_in'||doc.audio.sinks?.['net-out']?.alsa_pcm!=='ndimon_net_out')throw Error('invalid plugin PCM names');}return null;}catch(e){return e.message;}}
function invoke(args,input){return new Promise((resolve,reject)=>{const root=process.getuid?.()===0;const child=execFile(root?'/usr/local/sbin/ndimon-priv':'sudo',root?args:['-n','/usr/local/sbin/ndimon-priv',...args],{timeout:300000,maxBuffer:65536},(e,out,err)=>e?reject(Error(err||e.message)):resolve(out));child.stdin.on('error',()=>{});child.stdin.end(input);});}
async function confirmedReload(client){try{return await client.command('reload_config');}catch(e){if(e.code!=='confirmation_required'||!e.impact?.token)throw e;return client.confirmedCommand('reload_config',e.impact.token);}}
module.exports={
 export(){const settings=audio.getSettings();return {flows:readFlows(),...(settings.sources['net-in']&&settings.sinks['net-out']?{audio:{sources:{'net-in':settings.sources['net-in']},sinks:{'net-out':settings.sinks['net-out']}}}:{})};},
 validate,
 async import(doc){
  const error=validate(doc);if(error)throw Error(error);
  const previous=readFlows(),previousAudio=audio.getSettings(),client=new TesseraClient();let applied=false,written=false;
  try{
   const status=await client.query('status'),device=status.devices.find(d=>d.key==='main');
   if(!device)throw Error('main Tessera device is missing');
   if(doc.audio&&(doc.audio.sources['net-in'].channels!==device.rx_channels.length||doc.audio.sinks['net-out'].channels!==device.tx_channels.length))throw Error('backup channels differ from the live Tessera device; configure device channels first');
   await invoke(['plugin-exec','tessera','write-flows'],JSON.stringify(doc.flows));written=true;
   await confirmedReload(client);applied=true;
   if(doc.audio){
    await invoke(['plugin-exec','tessera','write-alsa',String(device.rx_channels.length),String(device.tx_channels.length)]);
    const settings=audio.getSettings();Object.assign(settings.sources,doc.audio.sources);Object.assign(settings.sinks,doc.audio.sinks);
    const result=await audio.putSettings(settings);if(!result.ok)throw Error(result.errors?.join('; ')||'audio restore failed');
   }
  }catch(e){
   try{if(written)await invoke(['plugin-exec','tessera','write-flows'],JSON.stringify(previous));if(applied){await confirmedReload(client);const restored=await audio.putSettings(previousAudio);if(!restored.ok)throw Error('audio rollback failed');}}
   catch(rollback){e.message+='; rollback failed: '+rollback.message;}throw e;
  }finally{client.close();}
 }
};
