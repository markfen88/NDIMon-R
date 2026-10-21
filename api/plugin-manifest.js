'use strict';
const Ajv = require('ajv');
const namePattern = '^[a-z][a-z0-9-]{1,31}$';
const names = { type: 'array', uniqueItems: true, items: { type: 'string', pattern: namePattern }, default: [] };
const paths = { type: 'array', uniqueItems: true, items: { type: 'string', pattern: '^/[A-Za-z0-9_./-]+$', not: { pattern: '(^|/)\\.\\.(/|$)' } }, default: [] };
const file = value => ({ type: 'string', pattern: '^[a-zA-Z0-9_-]+\\.[a-zA-Z0-9]+$', default: value });
const version = { type: 'string', pattern: '^(?:[0-9]+:)?[0-9][A-Za-z0-9.+:~\\-]*$' };
const schema = {
  type: 'object', additionalProperties: false,
  required: ['name', 'version', 'title', 'min_ndimon'],
  properties: {
    name: { type: 'string', pattern: namePattern }, version, min_ndimon: version,
    title: { type: 'string', minLength: 1, maxLength: 40 },
    icon: { type: 'string', pattern: '^bi-[a-z0-9-]+$', default: 'bi-puzzle' },
    routes: file('routes.js'), ui: file('ui.html'),
    backup: { ...file('backup.js'), type: ['string', 'null'] },
    install: file('install.sh'), uninstall: file('uninstall.sh'),
    priv_commands: names, supplementary_groups: names,
    services: { type: 'array', uniqueItems: true, items: { type: 'string', pattern: '^[A-Za-z0-9_@.-]+\\.service$' }, default: [] },
    readwrite_paths: paths, config_files: paths,
  },
};
const validate = new Ajv({ useDefaults: true, allErrors: true }).compile(schema);
const deniedGroups = new Set(['root','sudo','wheel','adm','shadow','disk','docker','lxd','kmem','systemd-journal','ndimon']);
const deniedServices = new Set(['ssh.service','sshd.service','ndimon-r.service','ndimon-api.service','ndimon-finder.service','ndimon-watchdog.service']);
function validateManifest(obj) {
  let manifest;
  try { manifest = JSON.parse(JSON.stringify(obj)); } catch { return { ok: false, errors: ['invalid manifest'] }; }
  if (!validate(manifest)) return { ok: false, errors: validate.errors.map(e => `${e.instancePath || '/'} ${e.message}`) };
  const errors = [];
  for (const group of manifest.supplementary_groups) if (deniedGroups.has(group) || manifest.name !== 'tessera' && ['tessera','tessera-admin'].includes(group)) errors.push('privileged group: '+group);
  for (const target of manifest.readwrite_paths) {
    if (!['/run/','/var/lib/','/etc/'].some(base=>target === base+manifest.name || target.startsWith(base+manifest.name+'/') || manifest.name === 'tessera' && target.startsWith(base+'tessera'))) errors.push('unscoped writable path: '+target);
  }
  for (const unit of manifest.services) {
    if (deniedServices.has(unit) || unit !== manifest.name+'.service' && ![manifest.name+'-',manifest.name+'@','ndimon-'+manifest.name+'.','ndimon-'+manifest.name+'-'].some(prefix=>unit.startsWith(prefix)) && !(manifest.name === 'tessera' && unit === 'tesserad.service' || manifest.name === 'bluetooth' && unit === 'bluealsa.service')) errors.push('unscoped service: '+unit);
  }
  return errors.length ? {ok:false,errors} : { ok: true, manifest };
}
module.exports = { validateManifest };
