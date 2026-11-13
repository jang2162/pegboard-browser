// Spike 21 (L1-L5): per-profile download policy, finding which tab a download came from, cancelling, naming, policy-connection loss.
// Only temporary directories are used as download targets. A uniquely named probe file that lands in ~/Downloads is recorded and removed.
import { mkdirSync, readdirSync, rmSync, statSync, existsSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { randomBytes } from 'node:crypto';
import { writeResult, sleep, OUT } from './lib/launch.mjs';
import { startProbe, html, openTwoProfiles, reporter } from './lib/probe.mjs';

const out = {};
const rec = reporter('21', out);
const runId = randomBytes(4).toString('hex');
const tag = (k) => `pbspike-${runId}-${k}`;
const t0 = Date.now();
const DL = join(OUT, '21-downloads', 'dl');
const dirAgent = join(DL, 'agent');
const dirHuman = join(DL, 'human');
const dirExtra = join(DL, 'extra');
rmSync(DL, { recursive: true, force: true });
for (const d of [dirAgent, dirHuman, dirExtra]) mkdirSync(d, { recursive: true });
const userDownloads = join(homedir(), 'Downloads');
import { onCleanup } from './lib/launch.mjs';
const landedInUserDownloads = () => (existsSync(userDownloads) ? readdirSync(userDownloads).filter((f) => f.includes(runId)) : []);
const ls = (d) => readdirSync(d).map((f) => `${f.length > 24 ? f.slice(0, 8) + '…' : f}:${statSync(join(d, f)).size}`);

// ---------- probe server ----------
// also on SIGINT/SIGTERM/early failure: remove only files carrying this run's random id
onCleanup(() => { for (const f of landedInUserDownloads()) rmSync(join(userDownloads, f), { force: true }); });
const probe = await startProbe();
const R = probe.routes;
const slow = {};
const attach = (name, extra = '') => `attachment; filename="${name}"${extra}`;
R['/blank'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html('<p>blank</p>')));
R['/dl/file'] = (_q, res, url) => {
  const size = Number(url.searchParams.get('size') || 1024);
  res.writeHead(200, { 'content-type': 'application/octet-stream', 'content-disposition': attach(url.searchParams.get('name')), 'content-length': size });
  res.end(Buffer.alloc(size, 120));
};
R['/dl/named'] = (_q, res, url) => {
  res.writeHead(200, { 'content-type': 'application/octet-stream', ...(url.searchParams.get('h') ? { 'content-disposition': url.searchParams.get('h') } : {}), 'content-length': 10 });
  res.end('0123456789');
};
R['/dl/plain.bin'] = (_q, res) => (res.writeHead(200, { 'content-type': 'application/octet-stream', 'content-length': 10 }), res.end('0123456789'));
R['/dl/slow'] = (req, res, url) => {
  const name = url.searchParams.get('name');
  const size = Number(url.searchParams.get('size') || 30_000_000);
  const chunk = Number(url.searchParams.get('chunk') || 262144);
  const ms = Number(url.searchParams.get('ms') || 20);
  const nolen = url.searchParams.get('nolen') === '1';
  const st = (slow[name] = { sent: 0, closedEarly: false, finished: false });
  res.writeHead(200, { 'content-type': 'application/octet-stream', 'content-disposition': attach(name), ...(nolen ? {} : { 'content-length': size }) });
  const timer = setInterval(() => {
    if (st.sent >= size) { clearInterval(timer); st.finished = true; return res.end(); }
    const n = Math.min(chunk, size - st.sent);
    res.write(Buffer.alloc(n, 121));
    st.sent += n;
  }, ms);
  res.on('close', () => { clearInterval(timer); if (!st.finished) st.closedEarly = true; });
};
const page = (body) => (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html(body)));
R['/dl/link'] = (_q, res, url) => page(`<a id="a" href="/dl/file?name=${url.searchParams.get('name')}" style="display:block;width:200px;height:60px">download</a>`)(_q, res);
R['/dl/page'] = page(`<p>sw page</p><script>navigator.serviceWorker.register('/dl/sw.js',{scope:'/dl/'})</script>`);
R['/dl/sw.js'] = (_q, res) =>
  (res.writeHead(200, { 'content-type': 'text/javascript', 'service-worker-allowed': '/dl/' }),
  res.end(`self.addEventListener('install',()=>self.skipWaiting());
self.addEventListener('activate',e=>e.waitUntil(clients.claim()));
self.addEventListener('message',(e)=>{ const n=e.data; clients.matchAll().then(cs=>cs.forEach(c=>c.navigate('/dl/file?name='+n))); });
self.addEventListener('fetch',e=>{ const u=new URL(e.request.url); if(u.pathname==='/dl/sw-made'){ e.respondWith(new Response('made by the service worker',{headers:{'content-type':'application/octet-stream','content-disposition':'attachment; filename="'+u.searchParams.get('name')+'"'}})); } });`));
R['/dl/iframes'] = (_q, res, url) =>
  page(`<iframe src="/dl/link?name=${url.searchParams.get('n1')}" style="position:absolute;left:10px;top:10px;width:220px;height:70px;border:0"></iframe>
<iframe src="http://c.test:${probe.port}/dl/link?name=${url.searchParams.get('n2')}" style="position:absolute;left:10px;top:100px;width:220px;height:70px;border:0"></iframe>`)(_q, res);
R['/dl/opener'] = (_q, res, url) => page(`<button id="b" onclick="window.open('/dl/file?name=${url.searchParams.get('name')}')" style="width:200px;height:60px">open</button>`)(_q, res);

const env = await openTwoProfiles({ name: '21-downloads', probe, args: ['--host-resolver-rules=MAP c.test 127.0.0.1'] });
const { cdp: A, agent, human } = env;
const agentCtx = env.agentCtx;
const humanCtx = env.humanCtx;

// ---------- download event log on several connections ----------
const log = [];
const watch = (conn, label) => {
  conn.on('Browser.downloadWillBegin', (p) => log.push({ conn: label, ev: 'begin', ts: Date.now(), guid: p.guid, frameId: p.frameId, filename: p.suggestedFilename, url: p.url.replace(/:\d+/, ':PORT') }));
  conn.on('Browser.downloadProgress', (p) => log.push({ conn: label, ev: 'progress', ts: Date.now(), guid: p.guid, total: p.totalBytes, received: p.receivedBytes, state: p.state }));
};
const D1 = await env.connection(); // policy connection for the Agent profile
const D2 = await env.connection(); // policy connection for the Human profile (only usable when the Human profile is the default context)
const D3 = await env.connection(); // one connection asked to hold both
const M = await env.connection(); // discovery / frame lookups
for (const [c, l] of [[A, 'A(main)'], [D1, 'D1(agent policy)'], [D2, 'D2(human policy)'], [D3, 'D3(both)'], [M, 'M(discovery)']]) watch(c, l);

// target table, filled from discovery events so even a tab that is already gone can be looked up
const targets = new Map();
M.on('Target.targetCreated', (p) => targets.set(p.targetInfo.targetId, { ...p.targetInfo, createdAt: Date.now(), alive: true }));
M.on('Target.targetInfoChanged', (p) => targets.set(p.targetInfo.targetId, { ...(targets.get(p.targetInfo.targetId) || {}), ...p.targetInfo, alive: true }));
M.on('Target.targetDestroyed', (p) => { const t = targets.get(p.targetId); if (t) { t.alive = false; t.destroyedAt = Date.now(); } });
await M.send('Target.setDiscoverTargets', { discover: true });
await sleep(300);
const ctxName = (c) => (c === agentCtx ? 'Agent' : c === humanCtx ? 'Human' : c || '?');
const flatten = (ft) => [ft.frame.id, ...(ft.childFrames || []).flatMap(flatten)];
async function resolveFrame(frameId) {
  const direct = targets.get(frameId);
  const r = { frameId: frameId === undefined ? 'MISSING' : 'present' };
  if (direct) r.byTargetId = { type: direct.type, ctx: ctxName(direct.browserContextId), alive: direct.alive, opener: direct.openerId ? (targets.get(direct.openerId) ? 'known-target' : 'unknown-target') : null, openerCtx: direct.openerId ? ctxName(targets.get(direct.openerId)?.browserContextId) : null };
  const pageTargets = [...targets.values()].filter((t) => t.type === 'page' && t.alive);
  for (const t of pageTargets) {
    const sid = await M.attach(t.targetId).catch(() => null);
    if (!sid) continue;
    const tree = await M.send('Page.getFrameTree', {}, sid).catch(() => null);
    await M.send('Target.detachFromTarget', { sessionId: sid }).catch(() => {});
    if (tree && flatten(tree.frameTree).includes(frameId)) { r.byFrameTree = { ownerTarget: t.targetId === frameId ? 'main-frame-of-that-tab' : 'child-frame-of-a-tab', ctx: ctxName(t.browserContextId), isMainFrame: tree.frameTree.frame.id === frameId }; break; }
  }
  return r;
}

const defaultCtxName = ctxName((await M.send('Target.getBrowserContexts')).defaultBrowserContextId);
const humanPolicyPossible = defaultCtxName === 'Human';
const policy = (conn, dir, ctx, behavior = 'allowAndName') => conn.send('Browser.setDownloadBehavior', { behavior, downloadPath: dir, eventsEnabled: true, browserContextId: ctx }).then(() => 'ok', (e) => e.message);

// ---------- scenario runner ----------
async function scenario(key, trigger, { waitMs = 5000, name = tag(key) } = {}) {
  const from = log.length;
  const listBefore = { agent: ls(dirAgent).length, human: ls(dirHuman).length };
  await trigger(name);
  const t = Date.now();
  let begin = null;
  while (Date.now() - t < waitMs) {
    await sleep(100);
    begin = log.slice(from).find((e) => e.ev === 'begin' && e.filename && e.filename.includes(name));
    if (begin && log.slice(from).some((e) => e.ev === 'progress' && e.guid === begin.guid && ['completed', 'canceled'].includes(e.state))) break;
  }
  const mine = begin ? log.slice(from).filter((e) => e.guid === begin.guid) : [];
  await sleep(400);
  const guid = begin?.guid;
  const landed = begin ? { agentDir: readdirSync(dirAgent).includes(guid), humanDir: readdirSync(dirHuman).includes(guid), extraDir: readdirSync(dirExtra).includes(guid), userDownloadsByName: landedInUserDownloads().some((f) => f.includes(name)) } : null;
  const res = { landed, name: begin ? 'seen' : 'NO downloadWillBegin', begunOnConnections: [...new Set(mine.filter((e) => e.ev === 'begin').map((e) => e.conn))], states: [...new Set(mine.filter((e) => e.ev === 'progress').map((e) => e.state))], suggestedFilename: begin?.filename?.replace(runId, '<run>'), filesNow: { agent: ls(dirAgent).length - listBefore.agent, human: ls(dirHuman).length - listBefore.human } };
  if (begin) res.resolution = await resolveFrame(begin.frameId);
  res.landedInUserDownloads = landedInUserDownloads().length;
  return res;
}
const click = (conn, sid, x, y) => async () => {
  for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) await conn.send('Input.dispatchMouseEvent', { type, x, y, button: 'left', clickCount: type === 'mouseMoved' ? 0 : 1 }, sid);
};

try {
  // ================= L1 =================
  const l1 = {};
  l1.agentPolicyOnD1 = await policy(D1, dirAgent, agentCtx);
  l1.humanPolicyWithItsContextId = await policy(D2, dirHuman, humanCtx);
  l1.defaultBrowserContextIs = await M.send('Target.getBrowserContexts').then((r) => ctxName(r.defaultBrowserContextId), (e) => e.message);
  l1.humanPolicyOnD2 = humanPolicyPossible ? await policy(D2, dirHuman, undefined) : 'skipped (default context is the Agent profile; a policy without a context id would land on the Agent profile)';
  await agentNav('/dl/link?name=' + tag('l1-agent'));
  async function agentNav(path, which = agent) { await A.navigate(which.sessionId, probe.origin + path); }
  l1.agentDownload = await scenario('l1-agent', () => A.evaluate(agent.sessionId, `document.getElementById('a').click()`));
  await A.navigate(human.sessionId, `${probe.origin}/dl/link?name=${tag('l1-human')}`);
  l1.humanDownload = await scenario('l1-human', () => A.evaluate(human.sessionId, `document.getElementById('a').click()`));
  // the policy of one profile does not leak into the other: files went to the matching directory
  l1.filesInAgentDir = ls(dirAgent);
  l1.filesInHumanDir = ls(dirHuman);
  out.L1_detail = l1;
  const ok1 = l1.agentPolicyOnD1 === 'ok' && l1.agentDownload.landed?.agentDir && (humanPolicyPossible ? l1.humanDownload.landed?.humanDir : l1.humanDownload.landed?.userDownloadsByName);
  rec('L1.perProfile', `${ok1 ? 'PASS' : 'FAIL'}: this launch's default context=${defaultCtxName}; D1 set Agent dir by browserContextId (${l1.agentPolicyOnD1}), D2 set Human dir without a context id (${l1.humanPolicyOnD2}; with the Human context id: ${l1.humanPolicyWithItsContextId}; default context is ${l1.defaultBrowserContextIs}); agent download landed: ${JSON.stringify(l1.agentDownload.landed)}; human download landed: ${JSON.stringify(l1.humanDownload.landed)}; begin events seen by ${JSON.stringify(l1.agentDownload.begunOnConnections)} for the agent download and ${JSON.stringify(l1.humanDownload.begunOnConnections)} for the human one`);
  

  // ================= L2 =================
  const l2 = {};
  await agentNav('/dl/link?name=' + tag('l2-click'));
  l2.linkClick = await scenario('l2-click', () => A.evaluate(agent.sessionId, `document.getElementById('a').click()`));
  await agentNav('/blank');
  l2.directNavigate = await scenario('l2-nav', (n) => A.send('Page.navigate', { url: `${probe.origin}/dl/file?name=${n}` }, agent.sessionId));
  await agentNav('/blank');
  l2.blobDownload = await scenario('l2-blob', (n) => A.evaluate(agent.sessionId, `(()=>{const a=document.createElement('a');a.href=URL.createObjectURL(new Blob(['blob body']));a.download=${JSON.stringify(n)};document.body.appendChild(a);a.click();return 1})()`));
  await agentNav('/dl/opener?name=' + tag('l2-popup'));
  l2.windowOpenTab = await scenario('l2-popup', () => click(A, agent.sessionId, 60, 30)());
  await agentNav('/dl/iframes?n1=' + tag('l2-iframe-same') + '&n2=' + tag('l2-iframe-cross'));
  await sleep(1200);
  l2.iframeSameSite = await scenario('l2-iframe-same', () => click(A, agent.sessionId, 60, 30)());
  await agentNav('/dl/iframes?n1=' + tag('l2-iframe-same2') + '&n2=' + tag('l2-iframe-cross'));
  await sleep(1200);
  l2.iframeCrossSite = await scenario('l2-iframe-cross', () => click(A, agent.sessionId, 60, 120)());
  l2.iframeTargetsSeen = [...targets.values()].filter((t) => t.type === 'iframe').map((t) => ({ ctx: ctxName(t.browserContextId), alive: t.alive }));
  // service worker
  await agentNav('/dl/page');
  await A.evaluate(agent.sessionId, `navigator.serviceWorker.ready.then(()=>1)`);
  await sleep(800);
  await agentNav('/dl/page');
  const controlled = await A.evaluate(agent.sessionId, `!!navigator.serviceWorker.controller`);
  l2.swNavigatesClient = await scenario('l2-sw-navigate', (n) => A.evaluate(agent.sessionId, `navigator.serviceWorker.controller.postMessage(${JSON.stringify(n)})`));
  l2.swNavigatesClient.pageControlledBySW = controlled;
  l2.swServedAttachment = await scenario('l2-sw-made', (n) => A.send('Page.navigate', { url: `${probe.origin}/dl/sw-made?name=${n}` }, agent.sessionId));
  // a click in the Human tab (synthetic trusted input; no person was involved)
  await A.navigate(human.sessionId, `${probe.origin}/dl/link?name=${tag('l2-human')}`);
  l2.humanTabClick = await scenario('l2-human', () => click(A, human.sessionId, 60, 30)(), { waitMs: 12000 });
  await A.navigate(human.sessionId, `${probe.origin}/dl/link?name=${tag('l2-human-js')}`);
  l2.humanTabJsClick = await scenario('l2-human-js', () => A.evaluate(human.sessionId, `document.getElementById('a').click()`));
  out.L2_detail = l2;
  const sumL2 = (k, v) => `${k}: ${v.name === 'seen' ? 'begin seen' : 'NO EVENT'}, frameId ${v.resolution?.frameId}, ${v.resolution?.byTargetId ? `byTargetId=${JSON.stringify(v.resolution.byTargetId)}` : 'byTargetId=none'}, ${v.resolution?.byFrameTree ? `byFrameTree=${JSON.stringify(v.resolution.byFrameTree)}` : 'byFrameTree=none'}`;
  for (const [k, v] of Object.entries(l2)) if (v && typeof v === 'object' && v.name) rec(`L2.${k}`, sumL2(k, v));
  rec('L2.iframeTargets', JSON.stringify(l2.iframeTargetsSeen));
  const resolved = (v) => v.name === 'seen' && (v.resolution?.byTargetId?.ctx === 'Agent' || v.resolution?.byFrameTree?.ctx === 'Agent');
  rec('L2', `link click resolves to an Agent tab: ${resolved(l2.linkClick) ? 'PASS' : 'FAIL'}; all agent scenarios resolved: ${['linkClick', 'directNavigate', 'blobDownload', 'windowOpenTab', 'iframeSameSite', 'iframeCrossSite', 'swNavigatesClient', 'swServedAttachment'].map((k) => `${k}=${resolved(l2[k])}`).join(' ')}; human tab, synthetic mouse click (hidden window): ${l2.humanTabClick.name}; human tab, JS click: resolves to Human: ${l2.humanTabJsClick.resolution?.byTargetId?.ctx === 'Human' || l2.humanTabJsClick.resolution?.byFrameTree?.ctx === 'Human'} (a real person's click: not measured)`);

  // ================= L3 =================
  const l3 = {};
  async function cancelRun(key, extra) {
    const LIMIT = 2 * 1024 * 1024;
    const name = tag(key);
    await agentNav('/blank');
    const from = log.length;
    let canceledAt = null, cancelAck = null, cancelErr = null, firstOver = null, begin = null;
    const poll = setInterval(async () => {
      const ev = log.slice(from);
      begin = begin || ev.find((e) => e.ev === 'begin' && e.filename === name);
      const over = ev.find((e) => e.ev === 'progress' && begin && e.guid === begin.guid && e.received >= LIMIT);
      if (over && !canceledAt) {
        canceledAt = Date.now();
        firstOver = over;
        await D1.send('Browser.cancelDownload', { guid: begin.guid, browserContextId: agentCtx }).then(() => (cancelAck = Date.now() - canceledAt), (e) => (cancelErr = e.message));
      }
    }, 10);
    const before = ls(dirAgent);
    await A.send('Page.navigate', { url: `${probe.origin}/dl/slow?name=${name}&size=30000000${extra}` }, agent.sessionId);
    const t = Date.now();
    while (Date.now() - t < 8000) {
      await sleep(100);
      if (begin && log.slice(from).some((e) => e.guid === begin.guid && e.ev === 'progress' && ['completed', 'canceled'].includes(e.state))) break;
    }
    clearInterval(poll);
    await sleep(800);
    const mine = begin ? log.slice(from).filter((e) => e.guid === begin.guid && e.ev === 'progress') : [];
    const files = ls(dirAgent).filter((f) => !before.includes(f));
    return { totalBytesReportedFirst: mine[0]?.total, receivedAtFirstOverLimit: firstOver?.received, cancelAckMs: cancelAck, cancelError: cancelErr, finalState: mine[mine.length - 1]?.state, finalReceived: mine[mine.length - 1]?.received, progressEvents: mine.length, receivedSeries: mine.map((e) => e.received).slice(0, 12), msBetweenEvents: mine.slice(1).map((e, i) => e.ts - mine[i].ts).slice(0, 12), serverSent: slow[name]?.sent, serverSawEarlyClose: slow[name]?.closedEarly, partialFilesLeftInDir: files, landedInUserDownloads: landedInUserDownloads().length };
  }
  l3.withContentLength = await cancelRun('l3-len', '');
  l3.withoutContentLength = await cancelRun('l3-nolen', '&nolen=1');
  out.L3_detail = l3;
  const show = (v) => `cancel ${v.cancelError ? 'ERROR ' + v.cancelError : 'ok in ' + v.cancelAckMs + 'ms'}, final state ${v.finalState}, totalBytes first reported ${v.totalBytesReportedFirst}, received at first event >= limit ${v.receivedAtFirstOverLimit}, final received ${v.finalReceived}, progress events ${v.progressEvents} (received series ${JSON.stringify(v.receivedSeries)}, ms between events ${JSON.stringify(v.msBetweenEvents)}), server sent ${v.serverSent}, server saw early close ${v.serverSawEarlyClose}, leftover files ${JSON.stringify(v.partialFilesLeftInDir)}`;
  rec('L3.withContentLength', show(l3.withContentLength));
  rec('L3.withoutContentLength', show(l3.withoutContentLength));

  // ================= L4 =================
  const l4 = {};
  for (const [k, h] of [['plainAscii', `attachment; filename="${tag('l4-ascii')}.txt"`], ['utf8Star', `attachment; filename*=UTF-8''%ED%95%9C%EA%B8%80-${tag('l4-utf8')}.txt`], ['traversal', `attachment; filename="../../${tag('l4-trav')}.txt"`], ['noHeader', '']]) {
    await agentNav('/blank');
    const from = log.length;
    const before = ls(dirAgent);
    const target = h ? `/dl/named?h=${encodeURIComponent(h)}` : '/dl/plain.bin';
    await A.send('Page.navigate', { url: probe.origin + target }, agent.sessionId);
    await sleep(1500);
    const b = log.slice(from).find((e) => e.ev === 'begin');
    l4[k] = { suggestedFilename: b?.filename?.replace(runId, '<run>'), newFileNameOnDisk: ls(dirAgent).filter((f) => !before.includes(f)).map((f) => f.split(':')[0]).map((f) => (f.includes('…') ? 'guid-like:' + f : f)), guidIsFilename: b ? [dirAgent, dirHuman, dirExtra].some((d) => readdirSync(d).includes(b.guid)) : null, guidShape: b ? /^[0-9A-F-]{36}$/i.test(b.guid) : null };
  }
  out.L4_detail = l4;
  for (const [k, v] of Object.entries(l4)) rec(`L4.${k}`, `suggestedFilename=${JSON.stringify(v.suggestedFilename)}; file on disk is named by the download guid: ${v.guidIsFilename} (guid shaped like a uuid: ${v.guidShape})`);

  // ================= L5 =================
  const l5 = {};
  await agentNav('/blank');
  const name5 = tag('l5-loss');
  const from5 = log.length;
  const before5 = ls(dirAgent);
  await A.send('Page.navigate', { url: `${probe.origin}/dl/slow?name=${name5}&size=4000000&chunk=65536&ms=40` }, agent.sessionId);
  await sleep(700);
  const mid = log.slice(from5).filter((e) => e.ev === 'progress').pop();
  l5.progressBeforeClose = mid ? { state: mid.state, received: mid.received } : null;
  const dirBefore = ls(dirAgent).filter((f) => !before5.includes(f));
  D1.close(); // the policy connection dies mid-download
  const closedAt = Date.now();
  await sleep(500);
  const afterShort = { dir: ls(dirAgent).filter((f) => !before5.includes(f)), userDownloads: landedInUserDownloads().map((f) => f.replace(runId, '<run>')) };
  await sleep(4500);
  const mineAll = log.slice(from5).filter((e) => e.ev === 'progress');
  l5.eventsAfterClose = [...new Set(mineAll.filter((e) => e.ts > closedAt).map((e) => `${e.conn}:${e.state}`))];
  l5.serverSent = slow[name5]?.sent;
  l5.serverFinished = slow[name5]?.finished;
  l5.serverSawEarlyClose = slow[name5]?.closedEarly;
  l5.dirDuring = dirBefore;
  l5.dirAfter500ms = afterShort.dir;
  l5.lossFileLanded = { agentDir: readdirSync(dirAgent).some((f) => f.length === 36), userDownloads: landedInUserDownloads().some((f) => f.includes(name5)) };
  l5.dirAfter6s = ls(dirAgent).filter((f) => !before5.includes(f));
  l5.userDownloadsAfter6s = landedInUserDownloads().map((f) => f.replace(runId, '<run>'));
  out.L5_detail = l5;
  rec('L5', `policy connection closed after ${JSON.stringify(l5.progressBeforeClose)}; ${l5.serverFinished ? 'the download FINISHED (server sent all)' : l5.serverSawEarlyClose ? 'the download was ABORTED (server saw the connection drop after ' + l5.serverSent + ' bytes)' : 'server state unclear'}; agent dir during=${JSON.stringify(l5.dirDuring)} after 500ms=${JSON.stringify(l5.dirAfter500ms)} after 6s=${JSON.stringify(l5.dirAfter6s)}; the finished file landed: ${JSON.stringify(l5.lossFileLanded)} (files in ~/Downloads with this run id: ${l5.userDownloadsAfter6s.length}); events after close seen on: ${JSON.stringify(l5.eventsAfterClose)}`);

  // a download started after the policy connection is gone (the 5.4 "gap"): where does it go?
  await agentNav('/dl/link?name=' + tag('l5-after'));
  l5.startedAfterPolicyLoss = await scenario('l5-after', () => A.evaluate(agent.sessionId, `document.getElementById('a').click()`));
  l5.afterLossAgentDirNew = l5.startedAfterPolicyLoss.filesNow.agent;
  rec('L5.afterLoss', `new agent download after the policy connection is gone landed: ${JSON.stringify(l5.startedAfterPolicyLoss.landed)}; events seen on ${JSON.stringify(l5.startedAfterPolicyLoss.begunOnConnections)}`);

  // ================= L1b: one connection asked to hold two contexts / two connections on one context =================
  const l1b = {};
  const D4 = await env.connection();
  l1b.d4Agent = await policy(D4, dirAgent, agentCtx);
  l1b.d3Human = humanPolicyPossible ? await policy(D3, dirHuman, undefined) : 'skipped (default context is Agent)';
  l1b.d3AgentExtraDir = await policy(D3, dirExtra, agentCtx);
  await agentNav('/dl/link?name=' + tag('l1b-both'));
  const bf = { a: ls(dirAgent).length, x: ls(dirExtra).length };
  l1b.agentDownloadWhileTwoConnectionsHoldAgent = await scenario('l1b-both', () => A.evaluate(agent.sessionId, `document.getElementById('a').click()`));
  l1b.whereItLanded = { olderConnectionDir: ls(dirAgent).length - bf.a, newerConnectionDir: ls(dirExtra).length - bf.x };
  l1b.d3HumanStillWorks = await (async () => {
    await A.navigate(human.sessionId, `${probe.origin}/dl/link?name=${tag('l1b-human')}`);
    const r = await scenario('l1b-human', () => A.evaluate(human.sessionId, `document.getElementById('a').click()`));
    return { landed: r.landed, events: r.begunOnConnections };
  })();
  D3.close();
  await sleep(500);
  await agentNav('/dl/link?name=' + tag('l1b-d3closed'));
  const bf2 = { a: ls(dirAgent).length, x: ls(dirExtra).length };
  l1b.agentDownloadAfterNewerConnectionClosed = await scenario('l1b-d3closed', () => A.evaluate(agent.sessionId, `document.getElementById('a').click()`));
  l1b.afterNewerClosed = { olderConnectionDir: ls(dirAgent).length - bf2.a, newerConnectionDir: ls(dirExtra).length - bf2.x, userDownloads: l1b.agentDownloadAfterNewerConnectionClosed.landedInUserDownloads };
  out.L1b_detail = l1b;
  rec('L1b', `D4 holds Agent->agent dir (${l1b.d4Agent}); D3 then sets Human (${l1b.d3Human}) and Agent->extra dir (${l1b.d3AgentExtraDir}); agent download landed in: ${l1b.whereItLanded.newerConnectionDir ? 'the NEWER connection\'s dir' : l1b.whereItLanded.olderConnectionDir ? 'the OLDER connection\'s dir' : 'neither'}; D3's human policy still worked: ${JSON.stringify(l1b.d3HumanStillWorks)}; after the newer connection (D3) closed, an agent download landed in: ${l1b.afterNewerClosed.olderConnectionDir ? 'the older connection\'s dir (policy restored)' : l1b.afterNewerClosed.newerConnectionDir ? 'the closed connection\'s dir' : 'neither (' + l1b.afterNewerClosed.userDownloads + ' file(s) went to ~/Downloads)'}`);
  D4.close();
} finally {
  const strays = landedInUserDownloads();
  out.userDownloadsStrays = strays.map((f) => f.replace(runId, '<run>'));
  for (const f of strays) rmSync(join(userDownloads, f), { force: true }); // only files carrying this run's random id
  rec('Downloads', `files with this run's id found in ~/Downloads and removed: ${strays.length}${strays.length ? ' ' + JSON.stringify(out.userDownloadsStrays) : ''}`);
  out._meta = { when: new Date().toISOString(), elapsedS: Math.round((Date.now() - t0) / 1000) };
  console.log('saved', writeResult('21-downloads', out));
  for (const c of [D1, D2, D3, M]) try { c.close(); } catch {}
  await env.close();
  probe.close();
  rmSync(DL, { recursive: true, force: true });
}
process.exit(0);
