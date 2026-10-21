'use strict';
const net=require('net');
function validateFlows(flows){
 if(!Array.isArray(flows)||Buffer.byteLength(JSON.stringify(flows))>60000)throw Error('flows must be an array fitting the 60KB plugin write limit');
 const ids=new Set();let rx=0;
 return flows.map(f=>{
  if(!f||typeof f!=='object'||Array.isArray(f)||Object.keys(f).some(k=>!['id','direction','peer','port','frames_per_packet','any_source'].includes(k)))throw Error('unknown flow field');
  if(!/^[A-Za-z0-9_-]{1,32}$/.test(f.id)||ids.has(f.id))throw Error('unique flow id required');ids.add(f.id);
  if(!['rx','tx'].includes(f.direction))throw Error('direction must be rx or tx');
  if(!Number.isInteger(f.port)||f.port<1024||f.port>65535)throw Error('port must be 1024..65535');
  if(f.any_source!==undefined&&typeof f.any_source!=='boolean')throw Error('any_source must be boolean');
  if(f.direction==='rx'&&++rx>1)throw Error('only one receive flow: each flow carries all device channels');
  if(f.any_source&&(f.direction!=='rx'||f.peer))throw Error('any_source is RX only and excludes peer');
  if(!f.any_source){if(!net.isIPv4(f.peer)||f.peer.split('.').some(v=>String(Number(v))!==v))throw Error('peer must be a unicast IPv4 address');const b=f.peer.split('.').map(Number);if(b[0]===0||b[0]===127||b[0]>=224||f.peer==='255.255.255.255')throw Error('peer must be a unicast IPv4 address');}
  if(f.frames_per_packet!==undefined&&(f.direction!=='tx'||!Number.isInteger(f.frames_per_packet)||f.frames_per_packet<1||f.frames_per_packet>1024))throw Error('frames_per_packet must be 1..1024 on TX');
  return {...f};
 });
}
function renderFlows(flows){return '# NDIMon-R Tessera plugin; static flows, all device channels\n'+validateFlows(flows).map(f=>['[[flow]]',`id = ${JSON.stringify(f.id)}`,`direction = ${JSON.stringify(f.direction)}`,'device = "main"','channels = []',f.any_source?'any_source = true':`peer = ${JSON.stringify(f.peer)}`,`port = ${f.port}`,f.frames_per_packet===undefined?'':`frames_per_packet = ${f.frames_per_packet}`].filter(Boolean).join('\n')).join('\n\n')+'\n';}
function parseFlows(text){if(!text.trim())return [];const flows=[];let f;for(const raw of text.split('\n')){const line=raw.trim();if(!line||line.startsWith('#'))continue;if(line==='[[flow]]'){f={};flows.push(f);continue;}const m=/^(id|direction|device|channels|peer|port|frames_per_packet|any_source)\s*=\s*(.+)$/.exec(line);if(!f||!m)throw Error('flow file edited by hand; unsupported TOML');let value;try{value=JSON.parse(m[2]);}catch{throw Error('flow file edited by hand; unsupported TOML');}if(m[1]==='device'){if(value!=='main')throw Error('unsupported device');}else if(m[1]==='channels'){if(!Array.isArray(value)||value.length)throw Error('flow file must use all device channels');}else {if(Object.hasOwn(f,m[1]))throw Error('duplicate flow field');f[m[1]]=value;}}return validateFlows(flows);}
module.exports={validateFlows,renderFlows,parseFlows};
