'use strict';
const net = require('net');
const HTTP = {invalid:400,locked:409,not_found:404,permission_denied:403,rate_limited:429,unsupported:501,internal:500,confirmation_required:409,response_too_large:502,offline:503,timeout:504,protocol:502};
class TesseraError extends Error {
  constructor(code,detail={}) { super(detail.reason || detail.message || detail.what || code); this.code=code;this.detail=detail;this.http=HTTP[code]||502;this.impact=detail.impact; }
}
const frame = value => { const data=Buffer.from(JSON.stringify(value));const header=Buffer.alloc(4);header.writeUInt32BE(data.length);return Buffer.concat([header,data]); };
class TesseraClient {
  constructor({socketPath='/run/tessera/control.sock',timeoutMs=3000,maxFrame=1048576}={}) {Object.assign(this,{socketPath,timeoutMs,maxFrame});this.id=0;this.tail=Promise.resolve();this.connected=false;this.closed=false;}
  online(){return this.connected;}
  request(body) {const job=this.tail.then(()=>this.exchange(body));this.tail=job.catch(()=>{});return job;}
  exchange(body) {
    if(this.closed)return Promise.reject(new TesseraError('offline'));
    return new Promise((resolve,reject)=>{
      let finished=false,buffer=Buffer.alloc(0);const id=++this.id;
      let socket=this.socket;
      if(!socket||socket.destroyed){socket=this.socket=net.createConnection(this.socketPath);socket.on('error',()=>{});socket.on('close',()=>{this.connected=false;});}
      const finish=(error,value)=>{if(finished)return;finished=true;clearTimeout(timer);socket.removeListener('data',data);socket.removeListener('error',errorHandler);socket.removeListener('close',close);if(error){const transport=error.transport||['offline','timeout','protocol'].includes(error.code);this.connected=!transport;if(transport)socket.destroy();else socket.unref();reject(error);}else{this.connected=true;socket.unref();resolve(value);}};
      const errorHandler=e=>{const error=new TesseraError(e.code==='EACCES'?'permission_denied':'offline',{message:e.message});error.transport=true;finish(error);};
      const close=()=>finish(new TesseraError('offline'));
      const data=chunk=>{try{buffer=Buffer.concat([buffer,chunk]);if(buffer.length>=4){const n=buffer.readUInt32BE();if(!n||n>this.maxFrame)throw new TesseraError('protocol',{message:'invalid response frame length'});if(buffer.length>=n+4){if(buffer.length!==n+4)throw new TesseraError('protocol');const response=JSON.parse(buffer.subarray(4).toString('utf8'));if(response.v!==1||response.id!==id&&(response.id!==0||!Object.hasOwn(response.result||{},'err'))||!response.result)throw new TesseraError('protocol');if(Object.hasOwn(response.result,'err')){const err=response.result.err;const code=typeof err==='string'?err:Object.keys(err)[0];const detail=typeof err==='string'?{}:err[code];finish(new TesseraError(code,detail));}else if(Object.hasOwn(response.result,'ok'))finish(null,response.result.ok);else throw new TesseraError('protocol');}}}catch(e){finish(e instanceof TesseraError?e:new TesseraError('protocol',{message:e.message}));}};
      const timer=setTimeout(()=>finish(new TesseraError('timeout')),this.timeoutMs);
      socket.ref();socket.on('data',data);socket.once('error',errorHandler);socket.once('close',close);
      socket.write(frame({v:1,id,body}));
    });
  }
  async query(name,args){const reply=await this.request({query:args?{[name]:args}:name});if(!reply||typeof reply!=='object'||!Object.hasOwn(reply,name)){this.connected=false;this.socket?.destroy();throw new TesseraError('protocol',{message:'unexpected query response'});}return reply[name];}
  command(command){return this.request({command});}
  confirmedCommand(command,expect){if(typeof expect!=='string'||!expect)throw new TesseraError('invalid',{reason:'confirmation token required'});return this.request({confirmed_command:{command,expect}});}
  close(){this.closed=true;clearTimeout(this.retry);this.socket?.destroy();this.eventSocket?.destroy();}
  subscribeEvents(onEvent,onClose=()=>{}) {
    let backoff=1000;
    const connect=()=>{if(this.closed)return;let buffer=Buffer.alloc(0);const socket=this.eventSocket=net.createConnection(this.socketPath);socket.on('connect',()=>{backoff=1000;socket.write(frame({v:1,id:1,body:'subscribe_events'}));});socket.on('error',()=>{});socket.on('data',chunk=>{try{buffer=Buffer.concat([buffer,chunk]);while(buffer.length>=4){const n=buffer.readUInt32BE();if(!n||n>this.maxFrame)throw Error('frame limit');if(buffer.length<n+4)break;const r=JSON.parse(buffer.subarray(4,n+4));buffer=buffer.subarray(n+4);const ok=r.result?.ok;if(ok?.event)onEvent(ok.event);if(ok?.lagged)onEvent({lagged:ok.lagged});}}catch{socket.destroy();}});socket.on('close',()=>{onClose();if(!this.closed){this.retry=setTimeout(connect,backoff);this.retry.unref();backoff=Math.min(backoff*2,30000);}});socket.unref();};connect();return ()=>{this.closed=true;clearTimeout(this.retry);this.eventSocket?.destroy();};
  }
}
module.exports={TesseraClient,TesseraError,frame};
