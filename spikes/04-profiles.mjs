// Spike 04 (A11 A12 A13): two profiles in one browser. Can CDP tell them apart, what leaks, and can a live profile be copied?
import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync, rmSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { launchChrome, sleep, writeResult, CHROME, SPIKES } from './lib/launch.mjs';
import { connect } from './lib/cdp.mjs';
import { startFixture } from './lib/fixture.mjs';

const ONLY = (process.argv.find((a) => a.startsWith('--only=')) || '--only=a11,a12,a13').split('=')[1].split(',');
const out = { a11: {}, a12: {}, a13: {} };
const rec = (part, key, value) => {
  out[part][key] = value;
  console.log(part, key, JSON.stringify(value).slice(0, 600));
};
const fixture = await startFixture();
const chrome = await launchChrome({
  name: '04-profiles',
  profileDirectory: 'Human',
  args: [`--load-extension=${join(SPIKES, 'ext-tabgroups')}`, '--disable-features=DisableLoadExtensionCommandLineSwitch'],
});
const udd = chrome.userDataDir;
const cdp = await connect(chrome.browserWsUrl);
const pages = async () => (await cdp.send('Target.getTargets')).targetInfos.filter((t) => t.type === 'page');
const text = (sid) => cdp.evaluate(sid, `document.body.innerText`);

// Ask the running browser to open a window for another profile of the same user data dir.
async function openProfileWindow(profileDirectory, url) {
  const before = (await pages()).map((t) => t.targetId);
  const t0 = Date.now();
  const r = spawnSync(CHROME, [`--user-data-dir=${udd}`, `--profile-directory=${profileDirectory}`, '--no-first-run', '--new-window', 'about:blank'], { timeout: 20000 });
  let target = null;
  for (let i = 0; i < 60 && !target; i++) {
    await sleep(250);
    target = (await pages()).find((t) => !before.includes(t.targetId));
  }
  if (!target) return { ok: false, exit: r.status, ms: Date.now() - t0 };
  const sessionId = await cdp.attach(target.targetId);
  if (url) await cdp.navigate(sessionId, url);
  return { ok: true, exit: r.status, ms: Date.now() - t0, targetId: target.targetId, sessionId, browserContextId: target.browserContextId };
}
// Which profile directory does a browser context belong to? Read it from chrome://version inside that context.
async function profilePathOf(ctx) {
  const p = await newIn(ctx, 'chrome://version');
  const path = await cdp.evaluate(p.sessionId, `document.getElementById('profile_path')?.textContent?.trim() || null`);
  await cdp.send('Target.closeTarget', { targetId: p.targetId });
  return path;
}

// Open a tab in a given profile. Target.createTarget rejects a real profile's browserContextId
// ("Failed to find browser context"), so fall back to window.open from a tab already in that profile.
let humanCtx, agentCtx;
const createMethod = {};
async function newIn(ctx, url) {
  const label = ctx === humanCtx ? 'Human' : ctx === agentCtx ? 'Agent' : ctx;
  try {
    const p = await cdp.newPage(url, { browserContextId: ctx });
    createMethod[label] = 'Target.createTarget(browserContextId)';
    return p;
  } catch (e) {
    createMethod[`${label}_createTargetError`] = e.message;
  }
  const list = await pages();
  const seed = list.find((t) => t.browserContextId === ctx && /^http/.test(t.url));
  if (!seed) throw new Error(`no seed tab in context ${label}`);
  const seedSid = await cdp.attach(seed.targetId);
  const before = list.map((t) => t.targetId);
  await cdp.send('Runtime.evaluate', { expression: `window.open('about:blank') && 1`, userGesture: true }, seedSid);
  let target = null;
  for (let i = 0; i < 40 && !target; i++) {
    await sleep(150);
    target = (await pages()).find((t) => !before.includes(t.targetId));
  }
  if (!target) throw new Error(`window.open produced no tab in ${label}`);
  const sessionId = await cdp.attach(target.targetId);
  if (url) await cdp.navigate(sessionId, url);
  createMethod[label] = 'window.open from a tab of that profile';
  return { targetId: target.targetId, sessionId, browserContextId: target.browserContextId };
}

const human1 = await cdp.firstPage();
await cdp.navigate(human1.sessionId, `${fixture.origin}/counter`);
humanCtx = (await pages()).find((t) => t.targetId === human1.targetId).browserContextId;
const agentWin = await openProfileWindow('Agent', `${fixture.origin}/counter`);
if (!agentWin.ok) {
  rec('a11', '6_openSecondProfile', agentWin);
  writeResult('04-profiles', out);
  await chrome.kill();
  process.exit(2);
}
agentCtx = agentWin.browserContextId;

if (ONLY.includes('a11')) {
  rec('a11', '6_openSecondProfile', { ok: agentWin.ok, ms: agentWin.ms, exit: agentWin.exit });
  const human2 = await newIn(humanCtx, `${fixture.origin}/form`);
  // 3: create a tab in the Agent profile by context id.
  const agent2 = await newIn(agentCtx, `${fixture.origin}/opener`).catch((e) => ({ error: e.message }));
  rec('a11', '3a_createMethod', createMethod);
  const list = await pages();
  const ctxOf = (id) => list.find((t) => t.targetId === id)?.browserContextId;
  rec('a11', '1_contextIds', {
    humanCtx,
    agentCtx,
    distinct: !!humanCtx && !!agentCtx && humanCtx !== agentCtx,
    human2SameAsHuman: ctxOf(human2.targetId) === humanCtx,
    perTarget: list.map((t) => [t.url.replace(fixture.origin, ''), t.browserContextId]),
  });
  rec('a11', '2_getBrowserContexts', await cdp.send('Target.getBrowserContexts'));
  const sameWindow = agent2.targetId ? (await cdp.send('Browser.getWindowForTarget', { targetId: agent2.targetId })).windowId === (await cdp.send('Browser.getWindowForTarget', { targetId: agentWin.targetId })).windowId : null;
  rec('a11', '3_createInAgent', { ok: !!agent2.targetId, error: agent2.error, ctxIsAgent: agent2.targetId ? ctxOf(agent2.targetId) === agentCtx : null, inAgentWindow: sameWindow });
  // 4: popup opened from an Agent tab stays in the Agent context.
  if (agent2.targetId) {
    const before = list.map((t) => t.targetId);
    await cdp.send('Runtime.evaluate', { expression: `document.getElementById('open').click()`, userGesture: true }, agent2.sessionId);
    await sleep(1500);
    const popup = (await pages()).find((t) => !before.includes(t.targetId));
    rec('a11', '4_popupContext', { opened: !!popup, ctxIsAgent: popup?.browserContextId === agentCtx });
  }
  // 5: tell which context is which profile, using CDP only.
  const noCtxTab = await cdp.send('Target.createTarget', { url: 'about:blank' });
  const noCtx = (await pages()).find((t) => t.targetId === noCtxTab.targetId)?.browserContextId;
  rec('a11', '5_identify', {
    humanPath: await profilePathOf(humanCtx).catch((e) => `error: ${e.message}`),
    agentPath: await profilePathOf(agentCtx).catch((e) => `error: ${e.message}`),
    createTargetWithoutContextLandsIn: noCtx === humanCtx ? 'Human' : noCtx === agentCtx ? 'Agent' : noCtx,
  });
}

if (ONLY.includes('a12')) {
  const h = await newIn(humanCtx, `${fixture.origin}/cookie/set`);
  await sleep(500);
  const a = await newIn(agentCtx, `${fixture.origin}/cookie/get`);
  const hGet = await newIn(humanCtx, `${fixture.origin}/cookie/get`);
  rec('a12', '7_cookieAndStorage', { humanSees: await text(hGet.sessionId), agentSees: await text(a.sessionId) });
  const all = (await cdp.send('Target.getTargets')).targetInfos;
  rec('a12', '8_extensionPerProfile', {
    workers: all.filter((t) => t.type === 'service_worker' && /\/sw\.js$/.test(t.url)).map((t) => (t.browserContextId === humanCtx ? 'Human' : t.browserContextId === agentCtx ? 'Agent' : t.browserContextId)),
  });
  await cdp.send('Browser.grantPermissions', { origin: fixture.origin, permissions: ['geolocation'], browserContextId: humanCtx });
  const perm = `navigator.permissions.query({ name: 'geolocation' }).then((p) => p.state)`;
  rec('a12', '9_permissionGrantedInHumanOnly', { human: await cdp.evaluate(hGet.sessionId, perm), agent: await cdp.evaluate(a.sessionId, perm) });
  const names = (r) => (r.error ? `error: ${r.error}` : (r.cookies || []).filter((c) => c.name === 'spike').map((c) => `${c.name}=${c.value}@${c.domain}`));
  const safe = (pr) => pr.catch((e) => ({ error: e.message }));
  const defaultCtx = (await cdp.send('Target.getBrowserContexts')).defaultBrowserContextId;
  rec('a12', '12_browserLevelCookies', {
    defaultContextIs: defaultCtx === humanCtx ? 'Human' : defaultCtx === agentCtx ? 'Agent' : defaultCtx,
    noContext: names(await safe(cdp.send('Storage.getCookies'))),
    humanContext: names(await safe(cdp.send('Storage.getCookies', { browserContextId: humanCtx }))),
    agentContext: names(await safe(cdp.send('Storage.getCookies', { browserContextId: agentCtx }))),
    viaAgentPageSession: names(await safe(cdp.send('Network.getAllCookies', {}, a.sessionId))),
    viaHumanPageSession: names(await safe(cdp.send('Network.getAllCookies', {}, hGet.sessionId))),
  });
  rec('a12', '10_11_notRun', 'settings and download path checks need manual steps; not run');
}

if (ONLY.includes('a13')) {
  const cookieFile = ['Cookies', join('Network', 'Cookies')].map((p) => join(udd, 'Agent', p));
  const mtime = () => Math.max(...cookieFile.filter(existsSync).map((f) => statSync(f).mtimeMs), 0);
  const m0 = mtime();
  const setter = await newIn(agentCtx, `${fixture.origin}/cookie/set`);
  let flushedMs = null;
  const t0 = Date.now();
  for (let i = 0; i < 80 && flushedMs === null; i++) {
    await sleep(500);
    if (mtime() > m0) flushedMs = Date.now() - t0;
  }
  rec('a13', '13_cookieOnDisk', { files: cookieFile.filter(existsSync).map((f) => f.replace(udd, '')), flushedMs });

  const copyProfile = (name) => {
    const t = Date.now();
    const r = spawnSync('rsync', ['-a', '--exclude=Cache', '--exclude=Code Cache', '--exclude=GPUCache', '--exclude=LOCK', '--exclude=lockfile', `${join(udd, 'Agent')}/`, `${join(udd, name)}/`]);
    const size = spawnSync('du', ['-sk', join(udd, name)]).stdout.toString().split('\t')[0];
    return { exit: r.status, ms: Date.now() - t, kb: Number(size), stderr: r.stderr.toString().slice(0, 200) };
  };
  const infoCache = () => Object.keys(JSON.parse(readFileSync(join(udd, 'Local State'), 'utf8')).profile?.info_cache || {});

  rec('a13', '14_copy', copyProfile('AgentCopy'));
  const copyWin = await openProfileWindow('AgentCopy', `${fixture.origin}/cookie/get`);
  rec('a13', '15_openCopy', { ok: copyWin.ok, ms: copyWin.ms, newContext: copyWin.ok && ![humanCtx, agentCtx].includes(copyWin.browserContextId), sees: copyWin.ok ? await text(copyWin.sessionId) : null });
  await sleep(1500);
  rec('a13', '16_localState', { profiles: infoCache() });
  if (copyWin.ok) {
    await cdp.evaluate(copyWin.sessionId, `document.cookie = 'spike=2; Max-Age=86400; Path=/'; localStorage.setItem('spike', '2'); 1`);
    const check = await newIn(agentCtx, `${fixture.origin}/cookie/get`);
    rec('a13', '17_originalUntouched', { originalSees: await text(check.sessionId) });
    // 18: close the copy's tabs, delete its directory, browser must stay healthy.
    for (const t of (await pages()).filter((t) => t.browserContextId === copyWin.browserContextId)) await cdp.send('Target.closeTarget', { targetId: t.targetId });
    await sleep(2500);
    let rmError = null;
    try {
      rmSync(join(udd, 'AgentCopy'), { recursive: true, force: true });
    } catch (e) {
      rmError = e.message;
    }
    await sleep(1000);
    const alive = await cdp.send('Browser.getVersion').then(() => true, () => false);
    rec('a13', '18_deleteCopy', { rmError, dirGone: !existsSync(join(udd, 'AgentCopy')), browserAlive: alive, localStateStillLists: infoCache().includes('AgentCopy') });
  }
  // 19: copy five times while the original profile is being written to.
  const writer = await newIn(agentCtx, `${fixture.origin}/writer`);
  const attempts = [];
  for (let i = 1; i <= 5; i++) {
    const name = `AgentCopy${i}`;
    const copy = copyProfile(name);
    const win = await openProfileWindow(name, `${fixture.origin}/cookie/get`);
    const sees = win.ok ? await text(win.sessionId).catch((e) => `error: ${e.message}`) : null;
    attempts.push({ name, copyExit: copy.exit, copyMs: copy.ms, opened: win.ok, sees, ok: win.ok && /spike=1/.test(sees || '') && /"ls":"1"/.test(sees || '') });
    if (win.ok) for (const t of (await pages()).filter((t) => t.browserContextId === win.browserContextId)) await cdp.send('Target.closeTarget', { targetId: t.targetId });
    await sleep(800);
  }
  rec('a13', '19_copyUnderWrite', { attempts: attempts.length, ok: attempts.filter((a) => a.ok).length, detail: attempts });
  await cdp.send('Target.closeTarget', { targetId: writer.targetId });
  await cdp.send('Target.closeTarget', { targetId: setter.targetId });
}

console.log('saved', writeResult('04-profiles', { when: new Date().toISOString(), ...out }));
cdp.close();
await chrome.kill();
fixture.close();
process.exit(0);
