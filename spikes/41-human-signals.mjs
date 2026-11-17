// Spike 41 (phase 2b, design section 5 item 3): which signals tell a person's input (H1-H5) apart from the
// agent's commands and page scripts (H6, H7). Same tab throughout, steps alternate.
//
// Signals collected per step:
//   cdp   R's own page session: Page.frameRequestedNavigation (reason), Page.frameStartedNavigating
//         (navigationType), Page.frameNavigated (main frame), Page.navigatedWithinDocument.
//   ext   the test extension spikes/ext-activity (permission `tabs` only): chrome.tabs.onActivated,
//         chrome.windows.onFocusChanged, handed over through a Runtime.addBinding function.
//   page  listeners R installs in the page (focus, blur, visibilitychange, pointerdown, keydown with isTrusted).
//
// Usage: node 41-human-signals.mjs [--auto] [--only=H1,H2,...]
//   --auto  H1-H5 by OS-level input (System Events keystrokes, CGEvent clicks, app activation), which Chrome
//           receives as real user input. Without it a person does H1-H5 when asked.
// Each row says where its input came from: person, synthetic-os, agent-browser, page-script.
import { join } from 'node:path';
import { launchChrome, sleep, writeResult, SPIKES, onCleanup } from './lib/launch.mjs';
import { connect } from './lib/cdp.mjs';
import { startProbe, html } from './lib/probe.mjs';
import { ab } from './lib/ab.mjs';
import {
  flags as readFlags, chromePids, activatePid, ensureFront, activateFinder, keystroke, keyCode, clickAt, axRect, keepAwake, waitEnter, closePrompt,
} from './lib/desktop.mjs';

const FL = readFlags();
const want = (id) => !FL.only || FL.only.some((o) => id.startsWith(o));
const out = { when: new Date().toISOString(), flags: { auto: FL.auto, only: FL.only }, steps: [] };
const WIN = { x: 80, y: 80, w: 1200, h: 820 };

const probe = await startProbe();
probe.routes['/h'] = (_req, res, url) => {
  const step = url.searchParams.get('step') || 'start';
  res.writeHead(200, { 'content-type': 'text/html', 'cache-control': 'no-store' });
  res.end(
    html(
      `<style>body{margin:0;font:22px sans-serif}#go{display:block;margin:120px 80px;padding:60px;background:#cde;font-size:40px}</style>
<p style="margin:20px 80px">page ${step}</p>
<a id="go" href="/h?step=${encodeURIComponent(step)}-link">big link</a>
<script>
window.pegGo = (kind) => {
  if (kind === 'location') location = '/h?step=script-location';
  if (kind === 'push') history.pushState({}, '', '/h?step=pushed');
  if (kind === 'back') history.back();
  if (kind === 'go') history.go(-1);
  if (kind === 'aclick') document.getElementById('go').click();
  if (kind === 'openclose') { const w = window.open('/h?step=popup'); setTimeout(() => w && w.close(), 1500); }
};
</script>`,
      `h ${step}`,
    ),
  );
};
probe.routes['/meta'] = (_req, res) => {
  res.writeHead(200, { 'content-type': 'text/html', 'cache-control': 'no-store' });
  res.end(`<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="refresh" content="2;url=/h?step=meta-refresh"><title>meta</title></head><body><p>refreshing in 2 s</p></body></html>`);
};
const U = (step) => `${probe.origin}/h?step=${step}`;

const keep = keepAwake();
const ext = join(SPIKES, 'ext-activity');
const chrome = await launchChrome({
  name: '41-human-signals',
  profileDirectory: 'Agent',
  win: WIN,
  viaOpen: true,
  args: [`--load-extension=${ext}`, '--disable-features=DisableLoadExtensionCommandLineSwitch'],
});
onCleanup(() => keep.stop());
const F = await connect(chrome.browserWsUrl);
out.browser = (await F.send('Browser.getVersion')).product;
console.log(`BROWSER: ${out.browser}`);
const mainPid = () => chromePids(chrome.userDataDir).main;

// ---- signal log ----
const signals = [];
const push = (src, name, detail) => signals.push({ t: Date.now(), src, name, detail });

// Extension service worker and its binding.
let swTarget = null;
for (let i = 0; i < 40 && !swTarget; i++) {
  swTarget = (await F.send('Target.getTargets')).targetInfos.find((t) => t.type === 'service_worker' && t.url.endsWith('/sw.js'));
  if (!swTarget) await sleep(250);
}
if (!swTarget) throw new Error('the ext-activity service worker did not start');
const swEvents = [];
await F.send('Target.setDiscoverTargets', { discover: true });
F.on('Target.targetCreated', (p) => p.targetInfo.type === 'service_worker' && swEvents.push({ t: Date.now(), ev: 'created', id: p.targetInfo.targetId }));
F.on('Target.targetDestroyed', (p) => swEvents.push({ t: Date.now(), ev: 'destroyed', id: p.targetId }));
let swSid = await F.attach(swTarget.targetId);
const bindSw = async (sid) => {
  await F.send('Runtime.enable', {}, sid);
  await F.send('Runtime.addBinding', { name: 'pegActivity' }, sid);
};
await bindSw(swSid);
F.on('Runtime.bindingCalled', (p, sid) => {
  if (p.name !== 'pegActivity') return;
  const o = JSON.parse(p.payload);
  push('ext', o.ev, { tabId: o.tabId, windowId: o.windowId, url: o.url, none: o.none, replayed: !!o.replayed, sid: sid === swSid ? 'current' : 'old' });
});

// The agent-browser tab (as 01-occluded does it), then R's session on it.
const session = `hs${Date.now() % 1000000}`;
const opened = await ab(['open', U('start')], { session, cdp: String(chrome.port), flags: ['--pin-tab'] });
if (!opened.ok) throw new Error(`agent-browser open failed: ${opened.stdout} ${opened.stderr}`);
onCleanup(() => ab(['close'], { session }));
const tabs = async () => (await ab(['tab', 'list'], { session })).json?.data?.tabs || [];
const mine = (await tabs()).find((t) => /\/h\?step=start/.test(t.url));
if (!mine) throw new Error('agent-browser tab not found');
const R = await connect(chrome.browserWsUrl);
const sR = await R.attach(mine.targetId);
await R.send('Page.enable', {}, sR);
await R.send('Runtime.enable', {}, sR);
await R.send('Runtime.addBinding', { name: 'pegPage' }, sR);
const pageHook = `(() => { if (window.__peg) return; window.__peg = 1;
  const send = (name, e) => { try { pegPage(JSON.stringify({ name, trusted: e && e.isTrusted, vis: document.visibilityState })); } catch (x) {} };
  for (const n of ['focus', 'blur']) window.addEventListener(n, (e) => send(n, e), true);
  document.addEventListener('visibilitychange', (e) => send('visibilitychange', e), true);
  for (const n of ['pointerdown', 'keydown']) window.addEventListener(n, (e) => send(n, e), true);
})()`;
await R.send('Page.addScriptToEvaluateOnNewDocument', { source: pageHook }, sR);
await R.send('Runtime.evaluate', { expression: pageHook }, sR);
R.on('Runtime.bindingCalled', (p, sid) => {
  if (sid !== sR || p.name !== 'pegPage') return;
  const o = JSON.parse(p.payload);
  push('page', o.name, { isTrusted: o.trusted, visibility: o.vis });
});
const main = mine.targetId;
R.on('Page.frameRequestedNavigation', (p, sid) => sid === sR && p.frameId === main && push('cdp', 'frameRequestedNavigation', { reason: p.reason, disposition: p.disposition, url: p.url }));
R.on('Page.frameStartedNavigating', (p, sid) => sid === sR && p.frameId === main && push('cdp', 'frameStartedNavigating', { navigationType: p.navigationType, url: p.url }));
R.on('Page.frameNavigated', (p, sid) => sid === sR && !p.frame.parentId && push('cdp', 'frameNavigated', { type: p.type, url: p.frame.url }));
R.on('Page.navigatedWithinDocument', (p, sid) => sid === sR && p.frameId === main && push('cdp', 'navigatedWithinDocument', { navigationType: p.navigationType, url: p.url }));

// A second tab in the same window, in the background, for the tab switches.
const other = await R.send('Target.createTarget', { url: U('other'), background: true });
await sleep(1500);
const tabIdOf = async (targetId) => (await tabs()).find((t) => t.targetId === targetId)?.tabId;
await F.send('Target.activateTarget', { targetId: main });
activatePid(mainPid());
await sleep(1000);

// ---- OS-level actions (auto mode) ----
// Every OS step starts with our Chrome checked to be the frontmost app (keystrokes go there).
const front = () => ensureFront(mainPid());
async function linkPoint() {
  const r = await R.evaluate(
    sR,
    `JSON.stringify((() => { const b = document.getElementById('go').getBoundingClientRect();
      return [window.screenX + (window.outerWidth - window.innerWidth) / 2 + b.x + b.width / 2, window.screenY + (window.outerHeight - window.innerHeight) + b.y + b.height / 2]; })())`,
  );
  return JSON.parse(r);
}
const os = {
  H1: async () => {
    await front();
    keystroke('l', ['cmd']);
    await sleep(300);
    await front();
    keystroke(U('H1-typed'));
    await sleep(200);
    await front();
    keyCode(36);
    return 'synthetic-os (System Events: Cmd+L, typing, Return)';
  },
  H2: async () => {
    await front();
    const pid = mainPid();
    const r = axRect(pid, 'Back');
    if (r) {
      clickAt(r.x + r.w / 2, r.y + r.h / 2);
      return 'synthetic-os (CGEvent click on the Back button found through accessibility)';
    }
    await front();
    keystroke('[', ['cmd']);
    return 'synthetic-os (Cmd+[ keystroke: the Back button was not found)';
  },
  H3: async () => {
    await front();
    const [x, y] = await linkPoint();
    clickAt(x, y);
    return 'synthetic-os (CGEvent click on the link)';
  },
  H4: async () => {
    await front();
    keyCode(48, ['ctrl']); // Control+Tab: next tab
    await sleep(1500);
    await front();
    keyCode(48, ['ctrl', 'shift']); // back
    return 'synthetic-os (Control+Tab, then Control+Shift+Tab)';
  },
  H5: async () => {
    await front();
    activateFinder();
    await sleep(2000);
    activatePid(mainPid());
    return 'synthetic-os (Finder activated, then this Chrome again)';
  },
};
const asks = {
  H1: `Click this Chrome window's address bar, type ${U('H1-typed')} and press Return.`,
  H2: "Click the Chrome window's Back button once.",
  H3: 'Click the big link on the page.',
  H4: 'Click the other tab ("h other") of this window, wait a second, then click back on the first tab.',
  H5: 'Click another app (for example Finder), wait two seconds, then click back on this Chrome window.',
};

// ---- steps ----
async function step(id, label, act, settleMs = 3000) {
  if (!want(id)) return;
  const t0 = Date.now();
  let source;
  let error = null;
  try {
    source = await act();
  } catch (e) {
    error = e.message;
  }
  await sleep(settleMs);
  const got = signals.filter((s) => s.t >= t0);
  const row = { step: id, label, source, error, signals: got.map((s) => ({ ms: s.t - t0, src: s.src, name: s.name, detail: s.detail })) };
  out.steps.push(row);
  const brief = got.map((s) => `${s.src}:${s.name}${s.detail?.reason ? `(${s.detail.reason})` : ''}${s.detail?.navigationType ? `(${s.detail.navigationType})` : ''}${s.detail?.type ? `(${s.detail.type})` : ''}${s.src === 'page' ? `(trusted ${s.detail.isTrusted})` : ''}`);
  console.log(`${id} ${label} [${source}]${error ? ` ERROR ${error}` : ''}: ${brief.join(', ') || '(no signal)'}`);
}
const human = (id) => async () => {
  if (FL.auto) return os[id]();
  await waitEnter(asks[id]);
  return 'person';
};
const agent = (args) => async () => {
  const r = await ab(args, { session });
  if (!r.ok) throw new Error(`agent-browser ${args.join(' ')}: ${String(r.json?.error || r.stderr).slice(0, 200)}`);
  return `agent-browser ${args[0]}`;
};
const script = (expr, what) => async () => {
  await R.evaluate(sR, expr);
  return `page-script (${what})`;
};

await step('H1', 'person types an address', human('H1'));
await step('H6-open', 'agent open', agent(['open', U('H6-open')]));
await step('H2', 'person presses Back', human('H2'));
await step('H6-back', 'agent back', agent(['back']));
await step('H3', 'person clicks a link', human('H3'));
await ab(['open', U('before-H6-click')], { session });
await step('H6-click', 'agent clicks a link', agent(['click', '#go']));
await step('H4', 'person switches tabs and back', human('H4'));
await step('H6-tab', 'agent switches tabs and back', async () => {
  const there = await tabIdOf(other.targetId);
  const here = await tabIdOf(main);
  if (!there || !here) throw new Error(`tab ids not found (${there}, ${here})`);
  await agent(['tab', there])();
  await sleep(1500);
  await agent(['tab', here])();
  return `agent-browser tab ${there}, tab ${here}`;
});
await step('H5', 'person goes to another app and back', human('H5'));
await step('H7-location', 'page script location=', script(`setTimeout(() => pegGo('location'), 300); 1`, 'location='));
if (want('H7-meta')) {
  await ab(['open', `${probe.origin}/meta`], { session });
  await step('H7-meta', 'meta refresh (page loaded by the agent just before)', async () => 'page (meta refresh after 2 s)', 4000);
}
await step('H7-push', 'page script history.pushState', script(`setTimeout(() => pegGo('push'), 300); 1`, 'pushState'));
// More page-script steps that could look like a person: history moves, a scripted link click, and a popup
// that opens and closes (tab activations without anyone switching tabs).
await ab(['open', U('before-H7-back')], { session });
await ab(['open', U('H7-back-from')], { session });
await step('H7-history-back', 'page script history.back()', script(`setTimeout(() => pegGo('back'), 300); 1`, 'history.back()'));
await ab(['open', U('H7-go-from')], { session });
await step('H7-history-go', 'page script history.go(-1)', script(`setTimeout(() => pegGo('go'), 300); 1`, 'history.go(-1)'));
await step('H7-a-click', 'page script a.click()', script(`setTimeout(() => pegGo('aclick'), 300); 1`, 'a.click()'));
await ab(['open', U('H7-openclose')], { session });
await step(
  'H7-open-close',
  'page script window.open, then window.close after 1.5 s',
  async () => {
    // window.open needs user activation; the gesture comes from CDP, not from a person.
    await R.send('Runtime.evaluate', { expression: `pegGo('openclose'); 1`, userGesture: true }, sR);
    return 'page-script (window.open + close, CDP user gesture)';
  },
  4000,
);

// H8: does the binding survive the worker going idle and waking up?
if (want('H8')) {
  const swSeen = (tid) => F.send('Target.getTargets').then((r) => r.targetInfos.find((t) => t.type === 'service_worker' && t.targetId === tid));
  console.log('H8a: 30 s idle with the CDP session to the worker kept, then H4 again');
  await sleep(30000);
  await step('H8a-H4', 'person switches tabs after 30 s idle (worker session kept)', human('H4'));
  console.log('H8b: worker session detached, 35 s idle, H4 while detached, then re-attach');
  await F.send('Target.detachFromTarget', { sessionId: swSid }).catch(() => {});
  const oldTarget = swTarget.targetId;
  await sleep(35000);
  const stillThere = !!(await swSeen(oldTarget));
  await step('H8b-H4', 'person switches tabs while no CDP session holds the worker', human('H4'));
  let now = (await F.send('Target.getTargets')).targetInfos.find((t) => t.type === 'service_worker' && t.url.endsWith('/sw.js'));
  let after = null;
  if (now) {
    swSid = await F.attach(now.targetId);
    after = JSON.parse(await F.evaluate(swSid, `JSON.stringify({ binding: typeof self.pegActivity, missed: self.pegMissed.length, missedEvents: self.pegMissed.map((o) => o.ev) })`));
    // Set the binding again on the new worker and have it hand over what it kept.
    const t0 = Date.now();
    await bindSw(swSid);
    after.replayCalled = await F.evaluate(swSid, 'self.pegReplay()');
    await sleep(500);
    after.replayedReceived = signals.filter((s) => s.t >= t0 && s.src === 'ext' && s.detail?.replayed).length;
  }
  out.h8 = { workerTargetBefore: oldTarget, workerListedAfterIdle: stillThere, workerTargetNow: now?.targetId ?? null, sameTarget: now?.targetId === oldTarget, after, workerTargetEvents: swEvents };
  console.log(`H8 verdict: worker restarted ${now?.targetId !== oldTarget ? 'yes' : 'no'}; binding after re-attach ${after?.binding}; events missed while detached ${after?.missed} ${JSON.stringify(after?.missedEvents)}`);
  console.log(`H8 replay: binding set again, pegReplay() handed over ${after?.replayCalled}, received through the binding ${after?.replayedReceived} ${after && after.missed > 0 && after.replayedReceived === after.missed ? 'PASS' : 'FAIL'}`);
}

// ---- summary ----
const kinds = (rows) => new Set(rows.flatMap((r) => r.signals.map((s) => `${s.src}:${s.name}${s.detail?.reason ? `(${s.detail.reason})` : ''}${s.detail?.navigationType ? `(${s.detail.navigationType})` : ''}`)));
const humanRows = out.steps.filter((r) => /^H[1-5]$/.test(r.step));
const otherRows = out.steps.filter((r) => /^H[67]/.test(r.step));
const hs = kinds(humanRows);
const os_ = kinds(otherRows);
const onlyHuman = [...hs].filter((k) => !os_.has(k));
const onlyOther = [...os_].filter((k) => !hs.has(k));
out.summary = { onlyInHumanSteps: onlyHuman, onlyInAgentOrScriptSteps: onlyOther, inputMode: FL.auto ? 'synthetic-os' : 'person' };
console.log(`SUMMARY input for H1-H5: ${FL.auto ? 'synthetic OS input (not a person)' : 'a person'}`);
console.log(`SUMMARY signals seen only in H1-H5: ${JSON.stringify(onlyHuman)}`);
console.log(`SUMMARY signals seen only in H6-H7: ${JSON.stringify(onlyOther)}`);
console.log('TABLE | step | input | signals |');
for (const r of out.steps) console.log(`TABLE | ${r.step} | ${r.source ?? r.error} | ${r.signals.map((s) => `${s.src}:${s.name}${s.detail?.reason ? `(${s.detail.reason})` : ''}${s.detail?.navigationType ? `(${s.detail.navigationType})` : ''}`).join(', ')} |`);

closePrompt();
console.log('saved', writeResult('41-human-signals', out));
await ab(['close'], { session });
F.close();
R.close();
await chrome.kill();
probe.close();
process.exit(0);
