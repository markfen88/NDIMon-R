'use strict';
const net=require('net');
// No subscriber, reconnect timer, or module-load side effects.
function sendIPC(command){
 return new Promise(resolve=>{
  let done=false,data='';
  const socket=net.createConnection('/tmp/ndi-decoder.sock',()=>socket.write(JSON.stringify(command)+'\n'));
  function finish(result){if(done)return;done=true;socket.destroy();resolve(result);}
  socket.on('data',chunk=>{data+=chunk;if(data.length>4*1024*1024)finish({ok:false,error:'ipc_response_too_large'});});
  socket.on('end',()=>{try{finish(JSON.parse(data));}catch{finish({ok:false,error:'bad_ipc'});}});
  socket.on('error',error=>finish({ok:false,error:['ENOENT','ECONNREFUSED'].includes(error.code)?'decoder_offline':error.message}));
  socket.on('close',()=>{if(!done)finish({ok:false,error:'ipc_closed'});});
  socket.setTimeout(1500,()=>finish({ok:false,error:'timeout'}));
 });
}
module.exports={sendIPC};
