// Spike 27 (round 2, E1-E4 + F1-F4): which browser-context ids Chrome accepts; cross-site iframe downloads; completion; split connections; blank-tab downloads.
import { randomBytes } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { mkdirSync, readdirSync, rmSync, statSync, existsSync, renameSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { writeResult, sleep, launchChrome, CHROME, SPIKES, OUT, onCleanup } from './lib/launch.mjs';
import { startProbe, html, openTwoProfiles, reporter, parseArgs } from './lib/probe.mjs';
import { connect } from './lib/cdp.mjs';

const out = {};
const rec = reporter('27', out);
const runId = randomBytes(4).toString('hex');
const only = parseArgs().only;
const want = (k) => !only || only.includes(k);
const userDownloads = join(homedir(), 'Downloads');
const landed = () => (existsSync(userDownloads) ? readdirSync(userDownloads).filter((f) => f.includes(runId)) : []);
onCleanup(() => { for (const f of landed()) rmSync(join(userDownloads, f), { force: true }); }); // only files carrying this run's id
const probe = await startProbe();
const R = probe.routes;
const body = (n) => Buffer.alloc(n, 0x61);
// /dl?n=<bytes>&name=<file>&len=0|1 : an attachment download
R['/dl'] = (_q, res, url) => {
  const n = Number(url.searchParams.get('n') || 100000); const name = url.searchParams.get('name') || 'f'; const len = url.searchParams.get('len') !== '0';
  res.writeHead(200, { 'content-type': 'application/octet-stream', 'content-disposition': `attachment; filename="${name}-${runId}.bin"`, ...(len ? { 'content-length': n } : {}) });
  res.end(body(n));
};
R['/blank'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html('<p>b</p>')));
const tmp = (n) => { const d = join(OUT, `27-${runId}-${n}`); mkdirSync(d, { recursive: true }); return d; };
const ls = (d) => (existsSync(d) ? readdirSync(d) : []);
const show = (id, v) => rec(id, JSON.stringify(v));
const mkev = (conn) => (sid, expr, gesture = false) => conn.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true, userGesture: gesture }, sid).then((r) => (r.exceptionDetails ? `EXC ${r.exceptionDetails.exception?.description?.split('\n')[0] || r.exceptionDetails.text}` : r.result.value), (e) => `CDPERR ${e.message}`);
const acc = (p) => p.then(() => 'accepted', (e) => e.message.replace(/ for id .*/, ' for id <id>').slice(0, 90));

try {
  rec('RUN', { id: 'run-' + runId });
  // ================= E =================
  if (want('e')) {
    const chrome = await launchChrome({ name: '27-ctx', profileDirectory: 'Human' });
    const B = await connect(chrome.browserWsUrl);
    console.log('BROWSER:', (await B.send('Browser.getVersion')).product);
    const pages = async () => (await B.send('Target.getTargets')).targetInfos.filter((t) => t.type === 'page');
    const first = await B.firstPage();
    await B.navigate(first.sessionId, `${probe.origin}/blank`);
    const ids = {}; // profile name -> context id
    ids.Human = (await pages()).find((t) => t.targetId === first.targetId).browserContextId;
    const openProfile = async (name) => {
      const before = (await pages()).map((t) => t.targetId);
      spawnSync(CHROME, [`--user-data-dir=${chrome.userDataDir}`, `--profile-directory=${name}`, '--no-first-run', '--new-window', 'about:blank'], { timeout: 20000 });
      let t = null; for (let i = 0; i < 60 && !t; i++) { await sleep(250); t = (await pages()).find((x) => !before.includes(x.targetId)); }
      if (!t) throw new Error('window did not open for ' + name);
      ids[name] = t.browserContextId; return t;
    };
    const dir = tmp('e');
    const stage = async (label) => {
      const ctxs = await B.send('Target.getBrowserContexts');
      const name = (id) => Object.entries(ids).find(([, v]) => v === id)?.[0] || '?';
      const res = { getBrowserContexts: { nonDefaultIds: ctxs.browserContextIds.map(name), defaultId: name(ctxs.defaultBrowserContextId) }, pageTargetsByProfile: {} };
      for (const t of await pages()) res.pageTargetsByProfile[name(t.browserContextId)] = (res.pageTargetsByProfile[name(t.browserContextId)] || 0) + 1;
      res.setDownloadBehavior = {}; res.setPermission = {};
      for (const [pn, id] of [...Object.entries(ids), ['(no id given)', undefined]]) {
        res.setDownloadBehavior[pn] = await acc(B.send('Browser.setDownloadBehavior', { behavior: 'deny', ...(id ? { browserContextId: id } : {}) }));
        res.setPermission[pn] = await acc(B.send('Browser.setPermission', { permission: { name: 'clipboard-read' }, setting: 'denied', origin: probe.origin, ...(id ? { browserContextId: id } : {}) }));
      }
      await B.send('Browser.setDownloadBehavior', { behavior: 'default' }).catch(() => {});
      await B.send('Browser.resetPermissions', {}).catch(() => {});
      show(`E.${label}`, res); out[`E_${label}`] = res;
    };
    await stage('a_human_only_human_window_is_last_active');
    await openProfile('Agent');
    await stage('b_agent_window_opened_last');
    // c: activate the human window again
    await B.send('Target.activateTarget', { targetId: first.targetId }).catch(() => {});
    await B.send('Page.bringToFront', {}, first.sessionId).catch(() => {});
    await sleep(800);
    await stage('c_after_activating_the_human_window');
    await openProfile('Agent2');
    await stage('d_three_profiles');
    B.close(); await chrome.kill();
  }
  // ================= F1-F3 (two profiles; Agent is the default context) =================
  let env = null;
  if (want('f1') || want('f2') || want('f3')) {
    R['/xdl'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html(`<iframe id="x" src="http://c.test:${probe.port}/dl?n=20000&name=xframe" style="width:300px;height:100px"></iframe>`)));
    env = await openTwoProfiles({ name: '27-dl', probe, seedPath: '/blank', args: ['--host-resolver-rules=MAP c.test 127.0.0.1'] });
  }
  const watch = (conn, log, tag) => {
    conn.on('Browser.downloadWillBegin', (p) => log.push({ tag, ev: 'begin', t: Date.now(), guid: p.guid, frameId: p.frameId, name: p.suggestedFilename, url: p.url }));
    conn.on('Browser.downloadProgress', (p) => log.push({ tag, ev: 'progress', t: Date.now(), guid: p.guid, total: p.totalBytes, received: p.receivedBytes, state: p.state }));
  };
  if (want('f1')) {
    const P = env.cdp, ps = env.agent.sessionId, dir = tmp('f1'), log = [];
    watch(P, log, 'P');
    await P.send('Browser.setDownloadBehavior', { behavior: 'allowAndName', downloadPath: dir, eventsEnabled: true, browserContextId: env.agentCtx });
    await P.send('Page.enable', {}, ps);
    // collect every target relationship that exists when the download starts
    const infos = [];
    P.on('Target.targetCreated', (p) => infos.push(p.targetInfo));
    await P.send('Target.setDiscoverTargets', { discover: true });
    await P.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: false, flatten: true }, ps).catch(() => {});
    await P.navigate(ps, `${probe.origin}/xdl`).catch(() => {});
    await sleep(2500);
    const begin = log.find((l) => l.ev === 'begin' && l.name.includes('xframe'));
    const f1 = { downloadBegan: !!begin, files: ls(dir).length };
    if (begin) {
      f1.frameIdOfDownload = 'present';
      const targets = (await P.send('Target.getTargets')).targetInfos;
      f1.targetsAfter = targets.filter((t) => t.type === 'iframe' || t.type === 'page').map((t) => ({ type: t.type, hasOpenerId: !!t.openerId, hasParentFrameId: !!t.parentFrameId, parentFrameIdEqualsDownloadFrame: t.parentFrameId === begin.frameId, targetIdEqualsFrameId: t.targetId === begin.frameId, browserContext: t.browserContextId === env.agentCtx ? 'Agent' : t.browserContextId === env.humanCtx ? 'Human' : '?' }));
      f1.iframeTargetIsTheFrameOfTheDownload = targets.some((t) => t.type === 'iframe' && t.targetId === begin.frameId);
      // Page.getFrameTree on every page session
      const tree = {};
      for (const t of targets.filter((x) => x.type === 'page')) {
        const s = await P.attach(t.targetId); await P.send('Page.enable', {}, s).catch(() => {});
        const ft = await P.send('Page.getFrameTree', {}, s).then((r) => r.frameTree, (e) => null);
        const flat = []; const walk = (n) => { flat.push(n.frame.id); (n.childFrames || []).forEach(walk); }; if (ft) walk(ft);
        tree[t.targetId === env.agent.targetId ? 'AgentTab' : t.targetId === env.human.targetId ? 'HumanTab' : 'otherTab'] = { containsDownloadFrameId: flat.includes(begin.frameId), frames: flat.length };
        await P.send('Target.detachFromTarget', { sessionId: s }).catch(() => {});
      }
      f1.getFrameTreePerPage = tree;
      f1.ownerTabFound = Object.entries(tree).filter(([, v]) => v.containsDownloadFrameId).map(([k]) => k);
      // the frame's owner via the iframe target's session
      const it = targets.find((t) => t.type === 'iframe' && t.targetId === begin.frameId);
      if (it) { const s = await P.attach(it.targetId); f1.iframeTargetInfoFromGetTargetInfo = await P.send('Target.getTargetInfo', { targetId: it.targetId }).then((r) => Object.keys(r.targetInfo).filter((k) => r.targetInfo[k] !== undefined && r.targetInfo[k] !== '').sort(), (e) => e.message); f1.frameOwnerViaDOM = await P.send('DOM.getFrameOwner', { frameId: begin.frameId }, ps).then((r) => 'backendNodeId ' + (r.backendNodeId ? 'present' : 'absent'), (e) => e.message); }
    }
    f1.resolvedMeansTabFound = f1.ownerTabFound?.length ? `TAB found: ${f1.ownerTabFound}` : 'TAB NOT found by any of the three lookups';
    show('F1', f1); out.F1 = f1;
  }
  if (want('f2')) {
    const P = env.cdp, dir = tmp('f2'), moved = tmp('f2moved'), log = [];
    watch(P, log, 'P');
    await P.send('Browser.setDownloadBehavior', { behavior: 'allowAndName', downloadPath: dir, eventsEnabled: true, browserContextId: env.agentCtx });
    const f2 = [];
    const A = env.agent.sessionId;
    for (const [n, len] of [[1048576, 1], [3000000, 1], [1500000, 0]]) {
      log.length = 0;
      await P.send('Runtime.evaluate', { expression: `(()=>{const a=document.createElement('a');a.href='/dl?n=${n}&len=${len}&name=f2';a.download='';document.body.appendChild(a);a.click();return 1})()`, userGesture: true }, A);
      let done = null, guid = null;
      for (let i = 0; i < 100 && !done; i++) { await sleep(100); guid = log.find((l) => l.ev === 'begin')?.guid; done = log.find((l) => l.ev === 'progress' && l.state === 'completed' && l.guid === guid); }
      if (!done) { f2.push({ n, completed: false }); continue; }
      const p = join(dir, guid);
      const before = existsSync(p) ? statSync(p).size : null;
      let rename = 'ok';
      try { renameSync(p, join(moved, guid)); } catch (e) { rename = e.message; }
      await sleep(800);
      f2.push({ bytes: n, contentLength: !!len, completedEventReceivedBytes: done.received, totalBytesInEvent: done.total, sizeOnDiskAtCompletedEvent: before, equalsExpected: before === n, renameOnCompleted: rename, reappearedInPolicyDir: existsSync(p), finalSizeInMovedDir: existsSync(join(moved, guid)) ? statSync(join(moved, guid)).size : null, laterEventsForGuid: log.filter((l) => l.guid === guid && l.t > done.t).length, otherFilesInPolicyDir: ls(dir).length });
    }
    show('F2', f2); out.F2 = f2;
  }
  if (want('f3')) {
    const dirD = tmp('f3-policy'), dirE = tmp('f3-events');
    const A = env.agent.sessionId; const P = env.cdp;
    const f3 = {};
    for (const order of ['events connection first, policy connection newer', 'policy connection first, events connection newer']) {
      const r = {};
      const D = await env.connection(), E = await env.connection(); // created here; 'newer' is decided by who calls setDownloadBehavior last
      const log = [];
      watch(E, log, 'E');
      const polD = () => D.send('Browser.setDownloadBehavior', { behavior: 'allowAndName', downloadPath: dirD, eventsEnabled: false, browserContextId: env.agentCtx });
      const polE = () => E.send('Browser.setDownloadBehavior', { behavior: 'allowAndName', downloadPath: dirE, eventsEnabled: true, browserContextId: env.agentCtx });
      if (order.startsWith('events')) { await polE(); await polD(); } else { await polD(); await polE(); }
      const dl = async (tag) => { const bD = ls(dirD).length, bE = ls(dirE).length, bU = landed().length; log.length = 0; await P.send('Runtime.evaluate', { expression: `(()=>{const a=document.createElement('a');a.href='/dl?n=300000&name=f3${tag}';a.download='';document.body.appendChild(a);a.click();return 1})()`, userGesture: true }, A); await sleep(1800); return { landedIn: ls(dirD).length > bD ? 'policy-connection dir' : ls(dirE).length > bE ? 'events-connection dir' : landed().length > bU ? '~/Downloads (removed at the end)' : 'nowhere seen', eventsOnE: { begin: log.filter((l) => l.ev === 'begin').length, completed: log.filter((l) => l.state === 'completed').length } }; };
      r.beforeClose = await dl('a');
      D.close(); await sleep(700);
      r.afterPolicyConnectionClosed = await dl('b');
      E.close(); await sleep(700);
      r.afterBothClosed = await dl('c');
      f3[order] = r; show(`F3.${order}`, r);
      for (const f of landed()) rmSync(join(userDownloads, f), { force: true });
    }
    out.F3 = f3;
  }
  if (env) await env.close();
  // ================= F4 =================
  if (want('f4')) {
    const ext = join(SPIKES, '..', 'extension', 'bridge');
    const chrome = await launchChrome({ name: '27-f4', profileDirectory: 'Human', args: [`--load-extension=${ext}`, '--disable-features=DisableLoadExtensionCommandLineSwitch'] });
    const B = await connect(chrome.browserWsUrl);
    const first = await B.firstPage();
    await B.navigate(first.sessionId, `${probe.origin}/blank`);
    let extTarget = null; for (let i = 0; i < 40 && !extTarget; i++) { await sleep(250); extTarget = (await B.send('Target.getTargets')).targetInfos.find((t) => t.type === 'service_worker' && t.url.endsWith('/sw.js')); }
    const esw = await B.attach(extTarget.targetId);
    const extEval = (expr) => B.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true }, esw).then((r) => r.exceptionDetails ? 'EXC' : r.result.value);
    const f4 = {};
    const log = [];
    watch(B, log, 'B');
    const events = [];
    B.on('Target.targetDestroyed', (p) => events.push({ ev: 'targetDestroyed', t: Date.now(), id: p.targetId }));
    B.on('Target.detachedFromTarget', (p) => events.push({ ev: 'detachedFromTarget', t: Date.now(), session: p.sessionId }));
    B.on('Target.attachedToTarget', async (p) => { if (p.targetInfo.type === 'page') events.push({ ev: 'attached', t: Date.now(), waiting: p.waitingForDebugger, session: p.sessionId }); await B.send('Runtime.runIfWaitingForDebugger', {}, p.sessionId).catch(() => {}); });
    await B.send('Target.setDiscoverTargets', { discover: true });
    await B.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true });
    const countPages = async () => (await B.send('Target.getTargets')).targetInfos.filter((t) => t.type === 'page').length;
    for (const behavior of ['deny', 'allowAndName']) {
      const dir = tmp(`f4-${behavior}`);
      await B.send('Browser.setDownloadBehavior', { behavior, ...(behavior === 'allowAndName' ? { downloadPath: dir } : {}), eventsEnabled: true });
      const mk = {
        'chrome.tabs.create({url: <download url>})': async () => { await extEval(`chrome.tabs.create({url:'${probe.origin}/dl?n=50000&name=f4a',active:false}).then(t=>t.id)`); },
        'chrome.tabs.create(about:blank) then Page.navigate(<download url>)': async () => { await extEval(`chrome.tabs.create({url:'about:blank',active:false}).then(t=>t.id)`); await sleep(400); const t = (await B.send('Target.getTargets')).targetInfos.filter((x) => x.type === 'page').pop(); const s = await B.attach(t.targetId); await B.send('Page.enable', {}, s); B.send('Page.navigate', { url: `${probe.origin}/dl?n=50000&name=f4b` }, s).catch(() => {}); globalThis.__s = s; },
        'Target.createTarget({url: <download url>})': async () => { await B.send('Target.createTarget', { url: `${probe.origin}/dl?n=50000&name=f4c` }); },
        'Target.createTarget(about:blank) then Page.navigate(<download url>)': async () => { const { targetId } = await B.send('Target.createTarget', { url: 'about:blank' }); await sleep(300); const s = await B.attach(targetId); await B.send('Page.enable', {}, s); B.send('Page.navigate', { url: `${probe.origin}/dl?n=50000&name=f4d` }, s).catch(() => {}); globalThis.__s = s; globalThis.__t = targetId; },
      };
      for (const [k, fn] of Object.entries(mk)) {
        const pagesBefore = await countPages(); log.length = 0; events.length = 0;
        const fBefore = ls(dir).length, uBefore = landed().length;
        await fn(); await sleep(2500);
        const sessionAlive = globalThis.__s ? await B.send('Runtime.evaluate', { expression: '1', returnByValue: true }, globalThis.__s, 3000).then(() => 'session answers', (e) => 'session dead: ' + e.message.slice(0, 60)) : 'n/a (no session held by this script before the download)';
        const row = { policy: behavior, pagesBefore, pagesAfter: await countPages(), downloadEvents: { begin: log.filter((l) => l.ev === 'begin').length, completed: log.filter((l) => l.state === 'completed').length, canceled: log.filter((l) => l.state === 'canceled').length }, filesInPolicyDir: ls(dir).length - fBefore, filesInUserDownloads: landed().length - uBefore, targetDestroyedEvents: events.filter((e) => e.ev === 'targetDestroyed').length, detachedEvents: events.filter((e) => e.ev === 'detachedFromTarget').length, autoAttachedPagePausedBeforeNavigation: events.filter((e) => e.ev === 'attached').map((e) => e.waiting), sessionAfter: sessionAlive };
        globalThis.__s = null;
        f4[`${behavior} | ${k}`] = row; show(`F4.${behavior} | ${k}`, row);
        for (const f of landed()) rmSync(join(userDownloads, f), { force: true });
        // close any leftover non-first pages
        for (const t of (await B.send('Target.getTargets')).targetInfos.filter((x) => x.type === 'page' && x.targetId !== first.targetId)) await B.send('Target.closeTarget', { targetId: t.targetId }).catch(() => {});
        await sleep(300);
      }
    }
    out.F4 = f4;
    B.close(); await chrome.kill();
  }
} finally {
  for (const f of landed()) rmSync(join(userDownloads, f), { force: true });
  out.userDownloadsAfterRun = landed().length;
  rec('Downloads', `files with this run's id left in ~/Downloads after cleanup: ${landed().length}`);
  console.log('saved', writeResult('27-contexts-downloads', out));
  probe.close();
}
process.exit(0);
