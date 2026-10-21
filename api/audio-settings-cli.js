#!/usr/bin/env node
'use strict';
const {sendIPC}=require('./ipc-oneshot');
const audio=require('./audio-settings').createStore({sendIPC});
const [command,...ids]=process.argv.slice(2);
if(command==='select-v1'&&!ids.length){
 const fs=require('fs');const file='/etc/ndimon-device-settings.json';const cfg=JSON.parse(fs.readFileSync(file,'utf8')),settings=audio.getSettings();
 if(cfg.audio_engine==='v2'&&(settings.sources['net-in']?.enabled||settings.sinks['net-out']?.enabled||Object.values(audio.getCanonicalSettings().sources||{}).some(s=>s.enabled&&s.resource?.startsWith('tessera:'))||Object.values(audio.getCanonicalSettings().sinks||{}).some(s=>s.enabled&&s.resource?.startsWith('tessera:')))){
  cfg.audio_engine='v1';require('./persistence').writeConfig(file,JSON.stringify(cfg));
  require('child_process').execFileSync('/usr/local/sbin/ndimon-priv',['restart-service','ndimon-r'],{timeout:30000});
 }
}
else if(command==='register-tessera'&&!ids.length){
 const doc=audio.getSettings();
 if(audio.getCanonicalSettings().version===2){console.log(JSON.stringify({ok:true,registered:true}));}
 else {
 doc.sources['net-in'] ||= {type:'alsa',enabled:false,channels:2,delay_ms:0,gain_db:0,alsa_pcm:'ndimon_net_in'};
 doc.sinks['net-out'] ||= {type:'alsa',enabled:false,channels:2,delay_ms:0,alsa_pcm:'ndimon_net_out',mix:{ndi:0,'net-in':-60}};
 audio.putSettings(doc).then(result=>{console.log(JSON.stringify(result));if(!result.ok)process.exitCode=1;}).catch(e=>{console.error(e.message);process.exitCode=1;});
}
}
else if(command!=='remove'||!ids.length){console.error('usage: audio-settings-cli.js remove <id>... | register-tessera');process.exitCode=2;}
else audio.removeIds(...ids).then(result=>{console.log(JSON.stringify(result));if(!result.ok)process.exitCode=1;}).catch(e=>{console.error(e.message);process.exitCode=1;});
