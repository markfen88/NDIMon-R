'use strict';
// Observe the existing authenticated SSE endpoint without changing Status's
// connection lifecycle or owning another browser EventSource.
const clients = new Set();
function trackClient(req,res,next) {
  clients.add(res);
  const remove = () => clients.delete(res);
  req.once('close',remove); res.once('close',remove); res.once('finish',remove);
  next();
}
function broadcastEvent(type,payload) {
  const message = `data: ${JSON.stringify({...payload,type})}\n\n`;
  for (const client of clients) {
    try { client.write(message); } catch { clients.delete(client); }
  }
}
module.exports = {trackClient,broadcastEvent};
