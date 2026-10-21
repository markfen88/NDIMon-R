'use strict';
const router=require('express').Router();
const audio=require('../audio-settings');
const {sendIPC}=require('./lib');
function reply(res,result){return res.status(result.ok?200:(result.status||400)).json(result);}
function handle(fn){return async(req,res)=>{try{return await fn(req,res);}catch(e){return res.status(e.status||500).json({ok:false,error:e.code||e.message,errors:[e.message]});}};}
router.get('/endpoints',handle(async(req,res)=>res.json(await audio.getEndpoints())));
router.get('/outputs/:ch',handle(async(req,res)=>res.json(await audio.getOutput(Number(req.params.ch)))));
router.put('/outputs/:ch',handle(async(req,res)=>reply(res,await audio.putOutput(Number(req.params.ch),req.body))));
router.get('/engine',handle(async(req,res)=>res.json(audio.getEngine())));
router.post('/engine',handle(async(req,res)=>reply(res,await audio.setEngine(req.body))));
router.put('/engine',handle(async(req,res)=>reply(res,await audio.setEngine(req.body))));
router.get('/settings',handle(async(req,res)=>res.json(audio.getSettings())));
router.put('/settings',handle(async(req,res)=>reply(res,await audio.putSettings(req.body))));
router.patch('/sources/:id',handle(async(req,res)=>reply(res,await audio.patchSource(req.params.id,req.body))));
router.patch('/sinks/:id',handle(async(req,res)=>reply(res,await audio.patchSink(req.params.id,req.body))));
router.get('/status',handle(async(req,res)=>{const status=await sendIPC({action:'status'});return res.status(status.audio?200:503).json(status.audio||{ok:false,error:status.error||'audio unavailable'});}));
module.exports=router;
