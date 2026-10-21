'use strict';

// Pure catalog construction: gathering daemon/IPC status and installing PCMs belong to callers.
const validCount = value => Number.isInteger(value) && value >= 1 && value <= 16;
const validChannel = value => Number.isInteger(value) && value >= 1 && value <= 65535;
const ownEntries = value => value && typeof value === 'object' && !Array.isArray(value) ? Object.entries(value) : [];
const scalar = value => typeof value === 'string' ? value : '';
function mapping(channels, input, ndi=false) {
  if (channels === 1) return input ? 'Mono → stereo: the same sample feeds both channels' : 'Mono destination: default channel mapping';
  if (channels === 2) return ndi ? 'NDI stereo downmix' : 'Stereo channel pair';
  return input ? 'Multichannel: matching channels; 6+ channels downmix when feeding stereo' : 'Multichannel destination: matching channel indices';
}
function chNumber(output) {
  const ch = Number.isInteger(output?.ch_num) ? output.ch_num : Number.isInteger(output?.output_index) ? output.output_index + 1 : null;
  return ch >= 1 && ch <= 8 ? ch : null;
}
function buildCatalog({outputs=[],tesseraStatus=null,tesseraEnabled=false,savedGraph=null}={}) {
  const inputs=new Map(), destinations=new Map();
  const savedNodes=kind=>[...ownEntries(savedGraph?.[kind]),...ownEntries(savedGraph?.outputs).flatMap(([,output])=>ownEntries(output?.definitions?.[kind]))];
  const sourceNodes=savedNodes('sources'), sinkNodes=savedNodes('sinks');
  const main=Array.isArray(tesseraStatus?.devices) ? tesseraStatus.devices.find(device=>device.key==='main') : null;
  const channelsFor=direction=>Array.isArray(main?.[direction+'_channels']) ? main[direction+'_channels'] : [];
  const tesseraReason=()=>!tesseraEnabled ? 'Tessera plugin is disabled' : !tesseraStatus ? 'Tessera daemon is offline' : !main ? 'Tessera main device is unavailable' : main.sample_rate!==48000 ? 'Tessera main device must use 48 kHz' : '';
  const rangeReady=(direction,start,count)=>{
    const reason=tesseraReason();if(reason)return reason;
    if(!validChannel(start)||!validCount(count)||start+count-1>channelsFor(direction).length)return 'Saved channel range is unavailable on the Tessera main device';
    return '';
  };
  for(const output of Array.isArray(outputs)?outputs:[]) {
    const ch=chNumber(output);if(ch===null)continue;
    const label=scalar(output.output_alias)||scalar(output.connector)||'Output '+ch;
    const saved=sourceNodes.find(([,node])=>node?.type==='ndi'&&node.output===ch)?.[1];
    const channels=validCount(saved?.channels)?saved.channels:2;
    const ndiReady=output.connected===true&&output.audio_available===true;
    const ndiReason=!output.connected?'NDI source is not connected':!output.audio_available?'NDI audio capture is unavailable':'';
    inputs.set('ndi-'+ch,{id:'ndi-'+ch,node_id:'ndi-'+ch,label:label+' · NDI'+(output.source_name?' · '+scalar(output.source_name):''),provider:'ndi',channels,rate:48000,ready:ndiReady,...(!ndiReady?{reason:ndiReason}:{}),mapping:mapping(channels,true,true),output:ch});
    const hdmiReady=output.drm_ready===true;
    destinations.set('hdmi-'+ch,{id:'hdmi-'+ch,node_id:'hdmi-'+ch,label:label+' · HDMI/DisplayPort · Stereo',provider:'hdmi',channels:2,rate:48000,ready:hdmiReady,...(!hdmiReady?{reason:'Display connector is not ready'}:{}),mapping:mapping(2,false),alsa_pcm:'auto',resource:'hdmi:'+ch,channel_start:1,channel_count:2});
  }
  for(const direction of ['rx','tx']) {
    const target=direction==='rx'?inputs:destinations, channels=channelsFor(direction), count=channels.length;
    const addRange=(start,size)=>{
      const id='tessera-'+direction+'-'+start+'-'+size;if(target.has(id))return;
      const names=channels.slice(start-1,start-1+size).map((channel,index)=>scalar(channel.label)||direction.toUpperCase()+' '+(channel.channel??start+index));
      const range=size===1?'channel '+start:'channels '+start+'–'+(start+size-1);
      const shape=size===1?'Mono':size===2?'Stereo':'Multichannel';
      const reason=rangeReady(direction,start,size);
      target.set(id,{id,node_id:id,label:(scalar(main?.name)||'Tessera main')+' · '+(direction==='rx'?'Receive ':'Transmit ')+range+' · '+names.join(' / ')+' · '+shape,provider:'tessera',channels:size,rate:48000,ready:!reason,...(reason?{reason}:{}),mapping:mapping(size,direction==='rx'),alsa_pcm:'ndimon_tessera_'+direction+'_'+start+'_'+size,resource:'tessera:main:'+direction,channel_start:start,channel_count:size});
    };
    if(validCount(count))addRange(1,count);
    for(let index=0;index<count;index++) {
      const start=Number.isInteger(channels[index]?.channel)?channels[index].channel:index+1;
      if(validChannel(start))addRange(start,1);
    }
  }
  function retain(target,nodes,input) {
    for(const [nodeId,node] of nodes) {
      if(!node || typeof node!=='object')continue;
      const id=scalar(node.endpoint)||nodeId;if(target.has(id))continue;
      const channels=validCount(node.channels)?node.channels:validCount(node.channel_count)?node.channel_count:2;
      const start=validChannel(node.channel_start)?node.channel_start:1;
      const count=validCount(node.channel_count)?node.channel_count:channels;
      let entry={id,node_id:nodeId,label:'Saved endpoint · '+id,provider:'unknown',channels,rate:48000,ready:false,reason:'Saved endpoint provider is unavailable',mapping:mapping(channels,input)};
      for(const key of ['alsa_pcm','resource','channel_start','channel_count','output'])if(node[key]!==undefined)entry[key]=node[key];
      if(input&&node.type==='ndi'&&Number.isInteger(node.output)) {
        const live=inputs.get('ndi-'+node.output);entry={...entry,...(live||{}),id,node_id:'ndi-'+node.output,channels,output:node.output,mapping:mapping(channels,true,true),...(!live?{provider:'ndi',ready:false,reason:'Saved NDI output is unavailable'}:{})};
      } else if(/^hdmi:[1-8]$/.test(scalar(node.resource))) {
        const ch=Number(node.resource.slice(5)), live=destinations.get('hdmi-'+ch);
        entry={...entry,...(live||{}),id,node_id:'hdmi-'+ch,provider:'hdmi',channels,channel_start:start,channel_count:count,...(!live?{ready:false,reason:'Saved display output is unavailable'}:{})};
      } else if(node.resource==='tessera:main:rx'||node.resource==='tessera:main:tx') {
        const direction=node.resource.endsWith(':rx')?'rx':'tx';
        const reason=(input!== (direction==='rx'))?'Saved endpoint direction is invalid':rangeReady(direction,start,count);
        const canonical='tessera-'+direction+'-'+start+'-'+count, live=(input?inputs:destinations).get(canonical);
        entry={...entry,...(live||{}),id,node_id:canonical,provider:'tessera',channels,channel_start:start,channel_count:count,mapping:mapping(channels,input),ready:!reason,...(reason?{reason}:{reason:undefined})};
      }
      // Saved PCM/resource metadata is authoritative for orphan/alias choices.
      for(const key of ['alsa_pcm','resource'])if(node[key]!==undefined)entry[key]=node[key];
      if(entry.reason===undefined)delete entry.reason;
      target.set(id,entry);
    }
  }
  retain(inputs,sourceNodes,true);retain(destinations,sinkNodes,false);
  return {inputs:[...inputs.values()],destinations:[...destinations.values()]};
}
function publicCatalog(catalog,revision) {
  const strip=entry=>{
    const result={};for(const key of ['id','label','provider','channels','rate','ready','reason','mapping','output'])if(entry[key]!==undefined)result[key]=entry[key];return result;
  };
  return {revision,inputs:(catalog.inputs||[]).map(strip),destinations:(catalog.destinations||[]).map(strip)};
}
module.exports={buildCatalog,publicCatalog};
