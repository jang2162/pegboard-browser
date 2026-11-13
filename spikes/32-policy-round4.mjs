// Spike 32 (round 4, item 2; builds on 28): does a download / clipboard-permission policy set on the agent context while it is the DEFAULT
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
const rec = reporter('32', out);
const runId = randomBytes(4).toString('hex');
const guard = clipboardGuard();
const { sys } = guard;
const userDownloads = join(homedir(), 'Downloads');
const landed = () => (existsSync(userDownloads) ? readdirSync(userDownloads).filter((f) => f.includes(runId)) : []);
onCleanup(() => { for (const f of landed()) rmSync(join(userDownloads, f), { force: true }); });
const probe = await startProbe();
probe.routes['/dl'] = (_q, res, url) => { const n = Number(url.searchParams.get('n') || 50000); res.writeHead(200, { 'content-type': 'application/octet-stream', 'content-disposition': `attachment; filename="${url.searchParams.get('name') || 'f'}-${runId}.bin"`, 'content-length': n }); res.end(Buffer.alloc(n, 0x61)); };
probe.routes['/blank'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html('<p>b</p>')));
const env = await openTwoProfiles({ name: '32-policy', probe, seedPath: '/blank', args: [] });
const P = env.cdp; // the "product" long-lived policy connection
const ev = (sid, expr, gesture = false) => P.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true, userGesture: gesture }, sid).then((r) => (r.exceptionDetails ? `EXC ${r.exceptionDetails.exception?.description?.split('\n')[0]}` : r.result.value), (e) => `CDPERR ${e.message}`);
const nameOf = (id, ctxs) => (id === env.agentCtx ? 'Agent' : id === env.humanCtx ? 'Human' : '?');
const defaultNow = async () => nameOf((await P.send('Target.getBrowserContexts')).defaultBrowserContextId);
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

const rows = [];
async function probeTab(label, sid) {
  const tag = `${label.replace(/\W+/g, '').slice(0, 12)}${Math.random().toString(36).slice(2, 5)}`;
  await P.send('Emulation.setFocusEmulationEnabled', { enabled: true }, sid).catch(() => {});
  const bUser = landed().length;
  const clickReturn = await ev(sid, `(()=>{const a=document.createElement('a');a.href='/dl?n=50000&name=${tag}';a.download='';document.body.appendChild(a);const r=a.click();return String(r)})()`, true);
  const href = await ev(sid, 'location.href');
  await sleep(1800);
  const dl = landed().length > bUser ? '~/Downloads (removed at the end)' : 'not in ~/Downloads (no policy folder is checked in this script)';
  for (const f of landed()) rmSync(join(userDownloads, f), { force: true });
  return { download: dl, clickReturn, href: String(href).replace(/:\d+/, ':PORT') };
}
async function clipProbe(sid) {
  await P.send('Emulation.setFocusEmulationEnabled', { enabled: true }, sid).catch(() => {});
  sys.set(`SYS-${runId}`);
  const cw = await ev(sid, `navigator.clipboard.writeText('CW-${runId}').then(()=>'ok',e=>'ERR '+e.name)`, true);
  await sleep(600);
  const v = sys.get();
  return { writeText: cw, hasFocus: await ev(sid, 'document.hasFocus()'), visibility: await ev(sid, 'document.visibilityState'), systemClipboard: v === `CW-${runId}` ? 'CHANGED (page wrote it)' : v === `SYS-${runId}` ? 'unchanged' : 'other' };
}
const makeDefault = async (name) => {
  if ((await defaultNow()) === name) return;
  const t = name === 'Human' ? env.human : env.agent;
  await activate(t.targetId, t.sessionId);
  if ((await defaultNow()) !== name) await reinvoke(name);
};
try {
  rec('RUN', { id: 'run-' + runId, version: env.version });
  await P.navigate(env.agent.sessionId, `${probe.origin}/blank`);
  // ---- clipboard control, NO policy of any kind, same agent tab, same gesture writeText ----
  for (let rep = 1; rep <= 3; rep++) {
    await makeDefault('Agent'); const a = { default: await defaultNow(), ...(await clipProbe(env.agent.sessionId)) };
    await makeDefault('Human'); const h = { default: await defaultNow(), ...(await clipProbe(env.agent.sessionId)) };
    rec(`CLIPBOARD_CONTROL_no_policy.rep${rep}`, { agentDefault: a, humanDefault: h });
    (out.clipControl = out.clipControl || []).push({ agentDefault: a, humanDefault: h });
  }
  // ---- download control with no policy (shows the test sees a landing file) ----
  await makeDefault('Agent');
  rec('DOWNLOAD_CONTROL_no_policy', { default: await defaultNow(), ...(await probeTab('ctl', env.agent.sessionId)) });
  // ---- deny set the way the product does: ONE dedicated connection that sends only Browser.setDownloadBehavior{deny} ----
  for (let cycle = 1; cycle <= 3; cycle++) {
    const R = { cycle, steps: {} };
    await makeDefault('Agent');
    R.defaultWhenSet = await defaultNow();
    const D = await env.connection();
    R.setDownloadBehavior = await D.send('Browser.setDownloadBehavior', { behavior: 'deny', browserContextId: env.agentCtx }).then(() => 'ok', (e) => e.message.slice(0, 80));
    await P.navigate(env.agent.sessionId, `${probe.origin}/blank`);
    R.steps['1 agent default, deny just set'] = { default: await defaultNow(), ...(await probeTab('s1', env.agent.sessionId)) };
    await makeDefault('Human');
    R.steps['2 human default, existing agent tab'] = { default: await defaultNow(), ...(await probeTab('s2', env.agent.sessionId)) };
    // new agent TAB created while human is default: window.open from the agent tab (stays in the agent profile)
    const known = (await pages()).map((t) => t.targetId);
    await ev(env.agent.sessionId, `window.open('${probe.origin}/blank','_blank')&&1`, true);
    let nt = null; for (let i = 0; i < 40 && !nt; i++) { await sleep(150); nt = (await pages()).find((x) => !known.includes(x.targetId)); }
    R.newTabCreatedByWindowOpen = { created: !!nt, defaultAfterCreate: await defaultNow() };
    if (nt) {
      const ns = await P.attach(nt.targetId); await sleep(500);
      await makeDefault('Human');
      R.steps['3 NEW agent tab (window.open) created while human default'] = { defaultAtTest: await defaultNow(), ...(await probeTab('s3', ns)) };
      await P.send('Target.closeTarget', { targetId: nt.targetId }).catch(() => {});
    }
    // new agent WINDOW created while human is default: window.open with window features
    await makeDefault('Human');
    const known2 = (await pages()).map((t) => t.targetId);
    await ev(env.agent.sessionId, `window.open('${probe.origin}/blank','w${runId}','popup,width=420,height=320')&&1`, true);
    let nw = null; for (let i = 0; i < 40 && !nw; i++) { await sleep(150); nw = (await pages()).find((x) => !known2.includes(x.targetId)); }
    R.newWindowCreatedByWindowOpenPopup = { created: !!nw, defaultAfterCreate: await defaultNow() };
    if (nw) {
      const ws = await P.attach(nw.targetId); await sleep(500);
      await makeDefault('Human');
      R.steps['4 NEW agent window (window.open popup) created while human default'] = { defaultAtTest: await defaultNow(), ...(await probeTab('s4', ws)) };
      await P.send('Target.closeTarget', { targetId: nw.targetId }).catch(() => {});
    }
    // new agent window created by re-invoking Chrome for the Agent profile (the way the product opens one), then the human is made default again before the test
    await makeDefault('Human');
    const ra = await reinvoke('Agent');
    R.newWindowByReinvoke = { created: !!ra, defaultAfterCreate: await defaultNow() };
    if (ra) {
      const rs = await P.attach(ra.targetId); await P.navigate(rs, `${probe.origin}/blank`).catch(() => {});
      await makeDefault('Human');
      R.steps['5 NEW agent window (re-invoke) created, human made default again'] = { defaultAtTest: await defaultNow(), ...(await probeTab('s5', rs)) };
      await P.send('Target.closeTarget', { targetId: ra.targetId }).catch(() => {});
    }
    R.deniedConnectionAliveAtEnd = await D.send('Browser.getVersion').then(() => true, () => false);
    D.close(); await sleep(400);
    await makeDefault('Agent');
    R.afterDedicatedConnectionClosed = { default: await defaultNow(), ...(await probeTab('s6', env.agent.sessionId)) }; // policy should be gone (lifetime check)
    // positive controls with the deny connection CLOSED: new window.open tab and a re-invoked Agent window must DROP a file in ~/Downloads
    {
      await makeDefault('Human');
      const k1 = (await pages()).map((t) => t.targetId);
      await ev(env.agent.sessionId, `window.open('${probe.origin}/blank','_blank')&&1`, true);
      let pt = null; for (let i = 0; i < 40 && !pt; i++) { await sleep(150); pt = (await pages()).find((x) => !k1.includes(x.targetId)); }
      if (pt) { const ps2 = await P.attach(pt.targetId); await sleep(500); await makeDefault('Human'); R.positiveControl_newTab_denyConnectionClosed = { defaultAtTest: await defaultNow(), ...(await probeTab('p1', ps2)) }; await P.send('Target.closeTarget', { targetId: pt.targetId }).catch(() => {}); }
      await makeDefault('Human');
      const pr = await reinvoke('Agent');
      if (pr) { const prs = await P.attach(pr.targetId); await P.navigate(prs, `${probe.origin}/blank`).catch(() => {}); await makeDefault('Human'); R.positiveControl_reinvokedAgentWindow_denyConnectionClosed = { defaultAtTest: await defaultNow(), ...(await probeTab('p2', prs)) }; await P.send('Target.closeTarget', { targetId: pr.targetId }).catch(() => {}); }
    }
    rows.push(R); rec(`DENY_DEDICATED.cycle${cycle}`, R);
  }
  out.rows = rows;
} finally {
  guard.restore();
  for (const f of landed()) rmSync(join(userDownloads, f), { force: true });
  rec('Cleanup', `clipboard restored: ${guard.equalsSaved()}; files with this run's id left in ~/Downloads: ${landed().length}`);
  console.log('saved', writeResult('32-policy-round4', out));
  await env.close(); probe.close();
}
process.exit(0);
