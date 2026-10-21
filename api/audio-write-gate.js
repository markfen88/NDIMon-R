'use strict';
// Plugin lifecycle helpers run a separate root CLI. Exclude API writers while
// those jobs own the file, and refuse lifecycle starts during an API apply.
let audio=0,plugin=false;
function acquire(kind){
 if(plugin||(kind==='plugin'&&audio))throw Object.assign(Error('Audio settings are busy with another operation'),{status:409,code:'audio_busy'});
 if(kind==='plugin')plugin=true;else audio++;
 let released=false;return ()=>{if(released)return;released=true;if(kind==='plugin')plugin=false;else audio--;};
}
module.exports={acquireAudio:()=>acquire('audio'),acquirePlugin:()=>acquire('plugin')};
