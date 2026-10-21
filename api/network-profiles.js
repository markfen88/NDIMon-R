'use strict';
const Ajv = require('ajv');
const identifier = {type:'string',pattern:'^[a-z][a-z0-9-]{0,31}$'};
const schema = {type:'object',additionalProperties:false,
  required:['version','revision','interface','backend','networks','management','ndi','tessera'],
  properties:{version:{const:1},revision:{type:'integer',minimum:0,maximum:Number.MAX_SAFE_INTEGER},
    interface:{type:'string',pattern:'^[A-Za-z0-9_-]{1,15}$'},backend:{enum:['networkmanager','networkd','netplan','ifupdown']},
    management:identifier,ndi:identifier,tessera:identifier,
    networks:{type:'array',minItems:1,maxItems:9,items:{type:'object',additionalProperties:false,
      required:['id','name','vlan_id','mode','address','prefix','gateway','dns'],properties:{
        id:identifier,name:{type:'string',minLength:1,maxLength:64},vlan_id:{anyOf:[{type:'null'},{type:'integer',minimum:1,maximum:4094}]},
        mode:{enum:['dhcp','static']},address:{type:'string',maxLength:15},prefix:{type:'integer',minimum:0,maximum:30},
        gateway:{type:'string',maxLength:15},dns:{type:'array',maxItems:3,items:{type:'string',maxLength:15}},
      }}}}};
const check = new Ajv({allErrors:true}).compile(schema);
function validateProfiles(settings) {
  if (!check(settings)) throw Object.assign(Error('Invalid network profiles: '+check.errors.map(e=>e.message).join('; ')),{status:400});
  const ids=new Set(),tags=new Set();
  for (const row of settings.networks) {
    if (ids.has(row.id)||tags.has(row.vlan_id)) throw Error('Duplicate network identifier or VLAN');
    ids.add(row.id);tags.add(row.vlan_id);
  }
  for (const role of ['management','ndi','tessera']) if (!ids.has(settings[role])) throw Error(role+' requires a replacement network');
  return settings;
}

// The privileged helper validates the complete document again. The API never
// writes profile state or edits individual VLANs outside the transaction.
function createProfiles(runPrivAsync) {
  async function invoke(command, input, args = []) {
    const timeout = ['rollback','revert','confirm'].includes(command) ? 180000 : 20000;
    const { stdout } = await runPrivAsync(['net-profiles-' + command, ...args], timeout,
      input === undefined ? undefined : JSON.stringify(input));
    return JSON.parse(stdout);
  }
  return {
    status: () => invoke('status'),
    preview: settings => invoke('preview', validateProfiles(settings)),
    apply: settings => invoke('stage', validateProfiles(settings)),
    rollback: () => invoke('rollback'),
    confirm: (token, localAddress) => invoke('confirm', undefined, [token, localAddress]),
    revert: token => invoke('revert', undefined, [token]),
  };
}
module.exports = { createProfiles, validateProfiles, schema };
