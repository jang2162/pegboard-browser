// Spike 28 (round 3, item 1): does a download / clipboard-permission policy set on the agent context while it is the DEFAULT
// context keep applying to agent tabs after the Human profile becomes the default context (and after the Agent becomes default again)?
import { randomBytes } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { mkdirSync, readdirSync, rmSync, existsSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { writeResult, sleep, CHROME, OUT, onCleanup } from './lib/launch.mjs';
import { startProbe, html, openTwoProfiles, reporter, parseArgs } from './lib/probe.mjs';
import { clipboardGuard } from './lib/clipsafe.mjs';

const out = {};
const rec = reporter('28', out);
const runId = randomBytes(4).toString('hex');
const guard = clipboardGuard();
const { sys } = guard;
const userDownloads = join(homedir(), 'Downloads');
const landed = () => (existsSync(userDownloads) ? readdirSync(userDownloads).filter((f) => f.includes(runId)) : []);
onCleanup(() => { for (const f of landed()) rmSync(join(userDownloads, f), { force: true }); });
const probe = await startProbe();
probe.routes['/dl'] = (_q, res, url) => { const n = Number(url.searchParams.get('n') || 50000); res.writeHead(200, { 'content-type': 'application/octet-stream', 'content-disposition': `attachment; filename="${url.searchParams.get('name') || 'f'}-${runId}.bin"`, 'content-length': n }); res.end(Buffer.alloc(n, 0x61)); };
probe.routes['/blank'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html('<p>b</p>')));
const env = await openTwoProfiles({ name: '28-policy', probe, seedPath: '/blank', args: [] });
const P = env.cdp; // the "product" long-lived policy connection
const ev = (sid, expr, gesture = false) => P.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true, userGesture: gesture }, sid).then((r) => (r.exceptionDetails ? `EXC ${r.exceptionDetails.exception?.description?.split('\n')[0]}` : r.result.value), (e) => `CDPERR ${e.message}`);
const nameOf = (id, ctxs) => (id === env.agentCtx ? 'Agent' : id === env.humanCtx ? 'Human' : '?');
const defaultNow = async () => nameOf((await P.send('Target.getBrowserContexts')).defaultBrowserContextId);
const log = [];
P.on('Browser.downloadWillBegin', (p) => log.push({ ev: 'begin', guid: p.guid, name: p.suggestedFilename }));
P.on('Browser.downloadProgress', (p) => log.push({ ev: 'progress', guid: p.guid, state: p.state }));
const pages = async () => (await P.send('Target.getTargets')).targetInfos.filter((t) => t.type === 'page');
const reinvoke = async (profile) => {
  const before = (await pages()).map((t) => t.targetId);
  spawnSync(CHROME, [`--user-data-dir=${env.udd}`, `--profile-directory=${profile}`, '--no-first-run', '--new-window', 'about:blank'], { timeout: 20000 });
  let t = null; for (let i = 0; i < 40 && !t; i++) { await sleep(250); t = (await pages()).find((x) => !before.includes(x.targetId)); }
  await sleep(500);
  return t;
};
const activate = async (targetId, sid) => { await P.send('Target.activateTarget', { targetId }).catch(() => {}); await P.send('Page.bringToFront', {}, sid).catch(() => {}); await sleep(800); };
let dlDir = null;
const ls = (d) => (d && existsSync(d) ? readdirSync(d) : []);
async function test(label, sid, variant) {
  const tag = `${label.replace(/\W+/g, '').slice(0, 14)}${Math.random().toString(36).slice(2, 5)}`;
  await P.send('Emulation.setFocusEmulationEnabled', { enabled: true }, sid).catch(() => {});
  sys.set(`SYS-${runId}`);
  log.length = 0;
  const bDir = ls(dlDir).length, bUser = landed().length;
  await ev(sid, `(()=>{const a=document.createElement('a');a.href='/dl?n=50000&name=${tag}';a.download='';document.body.appendChild(a);a.click();return 1})()`, true);
  const cw = await ev(sid, `navigator.clipboard.writeText('CW-${runId}').then(()=>'ok',e=>'ERR '+e.name)`, true);
  await sleep(1800);
  const clip = sys.get() === `CW-${runId}` ? 'CHANGED (page wrote it)' : sys.get() === `SYS-${runId}` ? 'unchanged' : 'other';
  const whereDl = ls(dlDir).length > bDir ? 'policy temp folder' : landed().length > bUser ? '~/Downloads (removed at the end)' : 'nowhere';
  const states = [...new Set(log.filter((l) => l.ev === 'progress').map((l) => l.state))];
  for (const f of landed()) rmSync(join(userDownloads, f), { force: true });
  const row = { default: await defaultNow(), download: whereDl, downloadEvents: states.join('/') || 'none', clipboardWriteText: cw, systemClipboard: clip };
  return row;
}
const rows = [];
try {
  rec('RUN', { id: 'run-' + runId, version: env.version });
  // control: no policy set yet (shows the test can see a download landing in ~/Downloads and a clipboard write)
  await P.navigate(env.agent.sessionId, `${probe.origin}/blank`);
  rec('CONTROL.no_policy', await test('ctl', env.agent.sessionId, 'none'));
  for (const variant of ['deny', 'allowAndName']) {
    for (let cycle = 1; cycle <= 3; cycle++) {
      dlDir = join(OUT, `28-${runId}-${variant}-${cycle}`); mkdirSync(dlDir, { recursive: true });
      const R = { variant, cycle, steps: {} };
      // make the agent the default, then set the policy the way the product does
      if ((await defaultNow()) !== 'Agent') await activate(env.agent.targetId, env.agent.sessionId);
      if ((await defaultNow()) !== 'Agent') { const t = await reinvoke('Agent'); R.agentDefaultViaReinvoke = !!t; }
      R.defaultWhenPolicySet = await defaultNow();
      const set = async (ctx) => {
        const r = {};
        r.download = await P.send('Browser.setDownloadBehavior', { behavior: variant, ...(variant === 'allowAndName' ? { downloadPath: dlDir } : {}), eventsEnabled: true, browserContextId: ctx }).then(() => 'ok', (e) => e.message.slice(0, 70));
        r.read = await P.send('Browser.setPermission', { permission: { name: 'clipboard-read' }, setting: 'denied', browserContextId: ctx }).then(() => 'ok', (e) => e.message.slice(0, 70));
        r.write = await P.send('Browser.setPermission', { permission: { name: 'clipboard-write' }, setting: 'denied', browserContextId: ctx }).then(() => 'ok', (e) => e.message.slice(0, 70));
        return r;
      };
      R.policySet = await set(env.agentCtx);
      await P.navigate(env.agent.sessionId, `${probe.origin}/blank`);
      R.steps['1 agent default, policy just set'] = await test('s1', env.agent.sessionId, variant);
      // human becomes default: by re-invoking Chrome for the Human profile
      const th = await reinvoke('Human');
      R.steps['2 human default via re-invoke (new human window)'] = { humanWindowOpened: !!th, ...(await test('s2', env.agent.sessionId, variant)) };
      // agent default again via activateTarget + bringToFront
      await activate(env.agent.targetId, env.agent.sessionId);
      R.steps['3 agent default again via activate'] = await test('s3', env.agent.sessionId, variant);
      // human default via activateTarget + bringToFront on a human tab
      await activate(env.human.targetId, env.human.sessionId);
      R.steps['4 human default via activateTarget+bringToFront'] = await test('s4', env.agent.sessionId, variant);
      // agent default again via re-invoking Chrome for the Agent profile (this opens a SECOND agent window)
      const t2 = await reinvoke('Agent');
      R.steps['5 agent default again via re-invoke (second agent window)'] = await test('s5', env.agent.sessionId, variant);
      if (t2) {
        const s2 = await P.attach(t2.targetId);
        await P.navigate(s2, `${probe.origin}/blank`).catch(() => {});
        R.steps['6 SECOND agent-profile window, policy not re-set'] = await test('s6', s2, variant);
        await P.send('Target.closeTarget', { targetId: t2.targetId }).catch(() => {});
      }
      if (th) await P.send('Target.closeTarget', { targetId: th.targetId }).catch(() => {});
      rows.push(R);
      rec(`POLICY.${variant}.cycle${cycle}`, R);
      // reset policies for the next variant/cycle (best effort)
      await P.send('Browser.setDownloadBehavior', { behavior: 'default', browserContextId: env.agentCtx }).catch(() => P.send('Browser.setDownloadBehavior', { behavior: 'default' }).catch(() => {}));
      await P.send('Browser.resetPermissions', {}).catch(() => {});
    }
  }
  out.rows = rows;
} finally {
  guard.restore();
  for (const f of landed()) rmSync(join(userDownloads, f), { force: true });
  rec('Cleanup', `clipboard restored: ${guard.equalsSaved()}; files with this run's id left in ~/Downloads: ${landed().length}`);
  console.log('saved', writeResult('28-policy-persistence', out));
  await env.close(); probe.close();
}
process.exit(0);
