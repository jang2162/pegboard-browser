// Spike 02b (A8): action policy and confirmation on a session attached with --cdp.
// 0.42.0 matches policy entries against internal action names (evaluate, url), not the documented categories.
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { launchChrome, writeResult, OUT } from './lib/launch.mjs';
import { ab } from './lib/ab.mjs';

const chrome = await launchChrome({ name: '02b' });
const session = `pol${Date.now() % 1000000}`;
const policy = join(OUT, '02b-policy.json');
const rows = [];
const run = async (label, args, flags = []) => {
  const r = await ab(args, { session, cdp: String(chrome.port), flags });
  const row = { label, ok: r.ok, error: String(r.json?.error || '').slice(0, 160), out: r.json?.data?.result ?? null, raw: r.ok ? undefined : r.stdout.slice(0, 300) };
  rows.push(row);
  console.log(JSON.stringify(row));
  return r;
};
await run('attach', ['open', 'about:blank'], ['--pin-tab']);
writeFileSync(policy, JSON.stringify({ default: 'allow', deny: ['eval'] }));
await run('deny:[eval] (documented category) then eval', ['eval', '1+1'], ['--action-policy', policy]);
writeFileSync(policy, JSON.stringify({ default: 'allow', deny: ['evaluate'] }));
await run('deny:[evaluate] (internal action name) then eval', ['eval', '1+1'], ['--action-policy', policy]);
await run('deny:[evaluate] then snapshot', ['snapshot', '-i'], ['--action-policy', policy]);
writeFileSync(policy, JSON.stringify({ default: 'allow' }));
await run('policy reset then eval', ['eval', '1+1'], ['--action-policy', policy]);
for (const name of ['eval', 'evaluate']) {
  const r = await run(`confirm-actions ${name} then eval`, ['eval', '1+1'], ['--confirm-actions', name]);
  const id = (r.stdout.match(/c_[0-9a-f]+/) || [])[0];
  if (id) await run(`deny ${id}`, ['deny', id]);
}
console.log('saved', writeResult('02b-policy', { when: new Date().toISOString(), rows }));
await ab(['close'], { session });
await chrome.kill();
process.exit(0);
