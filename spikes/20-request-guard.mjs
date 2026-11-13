// Spike 20 (G1-G6): what request interception (Fetch domain) from a second CDP connection can and cannot guard.
// P = the "pegboardd" connection, A = the "agent" connection (the browser connection that owns the agent tab's page session).
import { writeResult, sleep } from './lib/launch.mjs';
import { startProbe, html, openTwoProfiles, privateLanIp, stat, reporter } from './lib/probe.mjs';
import { writeFileSync } from 'node:fs';

const out = {};
const rec = reporter('20', out);
const b64 = (s) => Buffer.from(s).toString('base64');
const t0 = Date.now();

// ---------- probe server ----------
const probe = await startProbe({ lan: true });
const CORS = { 'access-control-allow-origin': '*', 'content-type': 'text/plain' };
const text = (body, extra = {}) => (_req, res) => (res.writeHead(200, { ...CORS, ...extra }), res.end(body));
const R = probe.routes;
R['/blank'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html('<p>blank</p>')));
for (const k of ['page-fetch', 'xhr', 'img', 'beacon', 'worker-fetch', 'shared-fetch', 'sw-fetch', 'eval-fetch', 'ping', 'body', 'viasw']) R[`/t/${k}`] = text('SECRET-BODY');
R['/t/iframe'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html('<p>iframe</p>')));
R['/t/redirect'] = (_q, res, url) => (res.writeHead(302, { location: url.searchParams.get('to'), ...CORS }), res.end());
R['/t/worker.js'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/javascript' }), res.end(`fetch('/t/worker-fetch'+self.location.search);`));
R['/t/shared.js'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/javascript' }), res.end(`onconnect=()=>{};fetch('/t/shared-fetch'+self.location.search);`));
R['/t/sw.js'] = (_q, res) =>
  (res.writeHead(200, { 'content-type': 'text/javascript', 'service-worker-allowed': '/' }),
  res.end(`self.addEventListener('install',()=>self.skipWaiting());
self.addEventListener('activate',e=>e.waitUntil(clients.claim()));
self.addEventListener('message',()=>{fetch('/t/sw-fetch'+self.location.search);});
self.addEventListener('fetch',e=>{ if(new URL(e.request.url).pathname==='/t/viasw') e.respondWith(fetch(e.request.url+'&sw=1')); });`));
R['/g1'] = (_q, res) =>
  (res.writeHead(200, { 'content-type': 'text/html' }),
  res.end(html(`<p>g1</p><script>
const q = location.search; window.__q = q;
fetch('/t/page-fetch' + q);
const x = new XMLHttpRequest(); x.open('GET', '/t/xhr' + q); x.send();
new Image().src = '/t/img' + q;
const f = document.createElement('iframe'); f.src = '/t/iframe' + q; document.body.appendChild(f);
navigator.sendBeacon('/t/beacon' + q, 'x');
try { new WebSocket('ws://' + location.host + '/t/ws' + q); } catch (e) {}
new Worker('/t/worker.js' + q);
try { new SharedWorker('/t/shared.js' + q); } catch (e) {}
navigator.serviceWorker.register('/t/sw.js' + q, { scope: '/' });
</script>`)));
R['/t/g5page'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html('<p>g5page</p><script>fetch("/t/g5-sub"+location.search)</script>')));
R['/t/g5-sub'] = text('x');
R['/g4'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html('<p>g4</p>')));

const lanIp = probe.lanListening ? probe.lanIp : null;
const hostRules = [`MAP c.test 127.0.0.1`, `MAP loop.test 127.0.0.1`, `MAP ten.test 10.255.255.1`, ...(lanIp ? [`MAP lan.test ${lanIp}`] : [])].join(', ');
const insecureOk = `http://c.test:${probe.port}`;
const env = await openTwoProfiles({
  name: '20-request-guard',
  probe,
  args: [`--host-resolver-rules=${hostRules}`, `--unsafely-treat-insecure-origin-as-secure=${insecureOk}`],
});
const { cdp: A, agent } = env;
const P = await env.connection();
const pSid = await P.attach(agent.targetId);

// ---------- P's guard (replaceable handler) ----------
const guard = { events: [], handler: null, keys: null, attached: [], autoFetch: false, patterns: [{ urlPattern: '*', requestStage: 'Request' }], net: new Map(), netSeq: 0 };
const isResp = (p) => p.responseStatusCode !== undefined || p.responseErrorReason !== undefined;
const cont = (conn, p, sid) => (isResp(p) ? conn.send('Fetch.continueResponse', { requestId: p.requestId }, sid) : conn.send('Fetch.continueRequest', { requestId: p.requestId }, sid)).catch((e) => ({ error: e.message }));
const sessionKind = new Map([[pSid, 'page']]);
P.on('Fetch.requestPaused', (p, sid) => {
  const e = { ts: Date.now(), url: p.request.url, type: p.resourceType, stage: isResp(p) ? 'Response' : 'Request', sid, via: sessionKind.get(sid) || 'other', redirectedFrom: p.redirectedRequestId || null, networkId: p.networkId, status: p.responseStatusCode };
  if (!guard.keys) guard.keys = { top: Object.keys(p), request: Object.keys(p.request) };
  guard.events.push(e);
  if (guard.handler) guard.handler(p, sid, e);
  else cont(P, p, sid);
});
P.on('Network.responseReceived', (p) => guard.net.set(p.requestId, { rip: p.response.remoteIPAddress, port: p.response.remotePort, seq: ++guard.netSeq, ts: Date.now(), url: p.response.url }));
const loadFailed = [];
const wsCreated = [];
P.on('Network.webSocketCreated', (p) => wsCreated.push(p.url.replace(/:\d+/, ':PORT')));
P.on('Network.loadingFailed', (p) => loadFailed.push({ requestId: p.requestId, error: p.errorText, blocked: p.blockedReason, ts: Date.now() }));
P.on('Target.attachedToTarget', async (p, parentSid) => {
  const info = p.targetInfo;
  sessionKind.set(p.sessionId, info.type);
  guard.attached.push({ type: info.type, ctx: info.browserContextId === env.agentCtx ? 'Agent' : info.browserContextId === env.humanCtx ? 'Human' : 'other', url: info.url.replace(/:\d+/, ':PORT'), waiting: p.waitingForDebugger, parent: parentSid ? 'page' : 'browser' });
  if (guard.autoFetch && info.browserContextId === env.agentCtx) {
    const err = await P.send('Fetch.enable', { patterns: guard.patterns }, p.sessionId).then(() => null, (e) => e.message);
    if (err) guard.attached.push({ type: info.type, fetchEnableError: err });
  }
  if (p.waitingForDebugger) await sleep(guard.resumeDelay || 0), P.send('Runtime.runIfWaitingForDebugger', {}, p.sessionId).catch(() => {});
});

const fetchEnable = (patterns) => P.send('Fetch.enable', { patterns }, pSid);
const aFetch = (url, opts = '{}', ms = 10000) =>
  A.evaluate(agent.sessionId, `(async()=>{try{const r=await Promise.race([fetch(${JSON.stringify(url)},${opts}),new Promise((_,j)=>setTimeout(()=>j(new Error('timeout')),${ms}))]);return 'OK:'+(await r.text()).slice(0,40)}catch(e){return 'ERR:'+e.message}})()`, ms + 3000);
const navA = (url) => A.navigate(agent.sessionId, url);
const reset = () => ((guard.events = []), (guard.handler = null));
const finish = async () => {
  for (const c of [P]) try { c.close(); } catch {}
  await env.close();
  probe.close();
};

try {
  // ================= G1 =================
  const KINDS = {
    nav: '/g1', iframe: '/t/iframe', img: '/t/img', pageFetch: '/t/page-fetch', evalFetch: '/t/eval-fetch', xhr: '/t/xhr', websocket: '/t/ws',
    beacon: '/t/beacon', dedicatedWorkerScript: '/t/worker.js', dedicatedWorkerFetch: '/t/worker-fetch', sharedWorkerScript: '/t/shared.js',
    sharedWorkerFetch: '/t/shared-fetch', serviceWorkerScript: '/t/sw.js', serviceWorkerFetch: '/t/sw-fetch',
  };
  async function runKinds(origin, p) {
    reset();
    guard.attached = [];
    await navA(`${origin}/g1?p=${p}`);
    await sleep(2500);
    await aFetch(`/t/eval-fetch?p=${p}`);
    await A.evaluate(agent.sessionId, `navigator.serviceWorker.ready.then(r=>r.active.postMessage('go'))&&1`).catch(() => {});
    await sleep(2500);
    const res = {};
    for (const [k, path] of Object.entries(KINDS)) {
      const arrived = probe.hits.some((h) => h.path === path && h.search === `?p=${p}`) || (k === 'nav' && probe.hits.some((h) => h.path === '/g1' && h.search === `?p=${p}`));
      const ev = guard.events.filter((e) => e.stage === 'Request' && new URL(e.url).pathname === path && (new URL(e.url).search === `?p=${p}` || k === 'nav'));
      const wsEv = k === 'websocket' ? guard.events.filter((e) => e.url.includes('/t/ws')) : ev;
      res[k] = { arrived, caught: wsEv.length > 0, via: [...new Set(wsEv.map((e) => e.via))].join('+') || '-', type: [...new Set(wsEv.map((e) => e.type))].join('+') || '-' };
    }
    return res;
  }
  const g1 = {};
  // Phase A: Fetch on the page session only.
  guard.autoFetch = false;
  await fetchEnable(guard.patterns);
  await P.send('Network.enable', {}, pSid);
  g1.A_pageSessionOnly = await runKinds(env.probe?.origin || probe.origin, 'A');
  g1.A_attachedTargets = guard.attached.slice();
  g1.webSocketCreatedEventSeenByP = wsCreated.slice(0, 3);
  // Phase B: + page-level auto-attach (waitForDebuggerOnStart) so children get Fetch before they run.
  guard.autoFetch = true;
  await P.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }, pSid);
  g1.B_pageAutoAttach = await runKinds(`http://localhost:${probe.port}`, 'B');
  g1.B_attachedTargets = guard.attached.slice();
  // Phase C: browser-level auto-attach.
  await P.send('Fetch.disable', {}, pSid);
  await P.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }, pSid);
  await P.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true });
  await sleep(1000);
  g1.C_pageSessionDisabledThenBrowserAutoAttach = await runKinds(`http://c.test:${probe.port}`, 'C');
  g1.C_attachedTargets = guard.attached.slice(); // only targets attached while the page loaded (runKinds resets the list first)
  await P.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true });
  guard.autoFetch = false;
  const missing = (ph) => Object.entries(g1[ph]).filter(([, v]) => !v.caught).map(([k, v]) => `${k}${v.arrived ? '' : '(never arrived)'}`);
  rec('G1.A', { notCaught: missing('A_pageSessionOnly'), caught: Object.entries(g1.A_pageSessionOnly).filter(([, v]) => v.caught).map(([k, v]) => `${k}:${v.type}`) });
  rec('G1.B', { notCaught: missing('B_pageAutoAttach'), caught: Object.entries(g1.B_pageAutoAttach).filter(([, v]) => v.caught).map(([k, v]) => `${k}:${v.via}`), attached: g1.B_attachedTargets.map((a) => `${a.type}/${a.ctx}/${a.parent}${a.waiting ? '/wait' : ''}${a.fetchEnableError ? '/FETCH-ERR ' + a.fetchEnableError : ''}`) });
  rec('G1.C', { notCaught: missing('C_pageSessionDisabledThenBrowserAutoAttach'), caught: Object.entries(g1.C_pageSessionDisabledThenBrowserAutoAttach).filter(([, v]) => v.caught).map(([k, v]) => `${k}:${v.via}`), attached: g1.C_attachedTargets.map((a) => `${a.type}/${a.ctx}/${a.parent}${a.waiting ? '/wait' : ''}${a.fetchEnableError ? '/FETCH-ERR ' + a.fetchEnableError : ''}`) });
  out.G1_detail = g1;
  rec('G1.websocket', `handshake never reaches Fetch.requestPaused (phases A, B, C); Network.webSocketCreated at P with Network.enable: ${JSON.stringify(g1.webSocketCreatedEventSeenByP)} (observation only; blocking it was not measured)`);
  const evalCaught = ['A_pageSessionOnly', 'B_pageAutoAttach'].every((ph) => g1[ph].evalFetch.caught && g1[ph].pageFetch.caught);
  rec('G1', `evalFetch and pageFetch caught with page-session Fetch: ${evalCaught ? 'PASS' : 'FAIL'}; not caught in phase A (page session only): ${missing('A_pageSessionOnly').join(',') || 'none'}`);

  // ================= G2 =================
  await P.send('Fetch.disable', {}, pSid).catch(() => {});
  guard.autoFetch = false;
  await P.send('Network.enable', {}, pSid);
  await navA(`${probe.origin}/blank`);
  // request stage, with a redirect
  reset();
  await fetchEnable([{ urlPattern: '*', requestStage: 'Request' }]);
  const redirRes = await aFetch(`/t/redirect?to=${encodeURIComponent('/t/body?g2=redir')}`);
  const reqStage = guard.events.filter((e) => /redirect|g2=redir/.test(e.url));
  const g2 = { requestKeys: guard.keys, redirect_requestStageOnly: reqStage.map((e) => ({ stage: e.stage, url: e.url.replace(/:\d+/, ':PORT'), redirectedFrom: !!e.redirectedFrom })), pageSaw: redirRes };
  // both stages
  reset();
  guard.net.clear();
  await P.send('Fetch.disable', {}, pSid);
  await fetchEnable([{ urlPattern: '*', requestStage: 'Request' }, { urlPattern: '*', requestStage: 'Response' }]);
  let respKeys = null;
  const respObs = [];
  guard.handler = async (p, sid, e) => {
    if (e.stage === 'Request') return void cont(P, p, sid);
    if (!respKeys) respKeys = { top: Object.keys(p), request: Object.keys(p.request) };
    const immediate = guard.net.get(p.networkId);
    await sleep(500);
    const later = guard.net.get(p.networkId);
    respObs.push({ url: e.url.replace(/:\d+/, ':PORT'), status: p.responseStatusCode, headerNames: (p.responseHeaders || []).map((h) => h.name).slice(0, 8), remoteIpAtPause: immediate?.rip ?? null, remoteIpAfter500ms: later?.rip ?? null, responseReceivedBeforePauseEvent: !!immediate });
    cont(P, p, sid);
  };
  await aFetch(`/t/body?g2=resp`);
  g2.responseStageKeys = respKeys;
  g2.responseStage = respObs;
  g2.redirect_bothStages = guard.events.filter((e) => /g2=redir|redirect/.test(e.url) || /g2=resp/.test(e.url)).map((e) => `${e.stage}:${e.status ?? ''}:${new URL(e.url).pathname}${e.redirectedFrom ? ':redirected' : ''}`);
  const redir2 = await (async () => {
    reset();
    await aFetch(`/t/redirect?to=${encodeURIComponent('/t/body?g2=redir2')}`);
    return guard.events.map((e) => `${e.stage}:${e.status ?? ''}:${new URL(e.url).pathname}${e.redirectedFrom ? ':redirected' : ''}`);
  })();
  g2.redirect_bothStages = redir2;
  // cut the body at response stage
  reset();
  const hitsBefore = probe.hitsOf('/t/body').length;
  guard.handler = async (p, sid, e) => {
    if (e.stage === 'Request') return void cont(P, p, sid);
    await P.send('Fetch.failRequest', { requestId: p.requestId, errorReason: 'BlockedByClient' }, sid).catch((er) => g2.failError = er.message);
  };
  const cut = await aFetch(`/t/body?g2=cut`);
  g2.cutAtResponseStage = { pageSaw: cut, bodyReachedPage: /SECRET-BODY/.test(cut), serverWasContacted: probe.hitsOf('/t/body').length > hitsBefore };
  out.G2_detail = g2;
  rec('G2.request', `url+redirectedRequestId only; request-stage keys=${JSON.stringify(guard.keys)}; remote-address field present at request stage: ${JSON.stringify(Object.keys(guard.keys.top).length && [...guard.keys.top, ...guard.keys.request].some((k) => /ip|address|remote/i.test(k)))}`);
  rec('G2.response', { keys: respKeys, remoteIpAtPause: respObs[0]?.remoteIpAtPause, remoteIpAfter500ms: respObs[0]?.remoteIpAfter500ms, responseReceivedBeforePauseEvent: respObs[0]?.responseReceivedBeforePauseEvent });
  rec('G2.redirect', { requestStageOnly: g2.redirect_requestStageOnly.map((r) => `${r.url.split('?')[0].split(':PORT')[1]}${r.redirectedFrom ? '<-redirect' : ''}`), bothStages: redir2 });
  rec('G2.cut', g2.cutAtResponseStage.bodyReachedPage ? `FAIL body reached page: ${cut}` : `PASS body not delivered (${cut}); server was contacted: ${g2.cutAtResponseStage.serverWasContacted}`);

  // ================= G3 =================
  reset();
  await P.send('Fetch.disable', {}, pSid);
  await fetchEnable([{ urlPattern: '*', requestStage: 'Request' }, { urlPattern: '*', requestStage: 'Response' }]);
  const privateIp = (ip) => !!ip && /^(10\.|127\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.|169\.254\.|::1|f[cd])/.test(ip);
  const g3 = {};
  for (const [h, expectIp] of [['loop', '127.0.0.1'], ['lan', lanIp ? 'LAN' : null], ['ten', '10.255.255.1']]) {
    if (!expectIp) { g3[h] = 'not measured: no private LAN address on this machine'; continue; }
    reset();
    loadFailed.length = 0;
    const obs = { reqStage: null, respStage: null };
    guard.handler = async (p, sid, e) => {
      if (!/g3=/.test(e.url)) return void cont(P, p, sid);
      if (e.stage === 'Request') {
        const before = probe.hits.filter((x) => x.search.includes(`g3=${h}`)).length;
        await sleep(400);
        obs.reqStage = { urlHost: new URL(e.url).hostname.replace(lanIp || 'x', 'LAN'), ipFieldKeys: [...Object.keys(p), ...Object.keys(p.request)].filter((k) => /ip|addr|remote/i.test(k)), serverHitsWhilePaused: probe.hits.filter((x) => x.search.includes(`g3=${h}`)).length - before };
        return void cont(P, p, sid);
      }
      const now = guard.net.get(p.networkId);
      await sleep(600);
      const later = guard.net.get(p.networkId);
      const bodyTry = await P.send('Fetch.getResponseBody', { requestId: p.requestId }, sid).then((r) => `ok:${r.body.length}chars`, (er) => er.message);
      const afterBody = guard.net.get(p.networkId);
      const rip = now?.rip || later?.rip || afterBody?.rip;
      const nm = (x) => (x?.rip ? (x.rip === lanIp ? 'LAN-IP' : x.rip) : null);
      obs.respStage = { ripAtPause: nm(now), ripWithin600ms: nm(later), getResponseBody: bodyTry, ripAfterGetResponseBody: nm(afterBody), private: privateIp(rip) };
      if (privateIp(rip)) await P.send('Fetch.failRequest', { requestId: p.requestId, errorReason: 'BlockedByClient' }, sid).catch(() => {});
      else {
        obs.continuedAt = Date.now();
        obs.networkId = p.networkId;
        cont(P, p, sid);
      }
    };
    const host = `${h}.test`;
    const started = Date.now();
    const pageSaw = await A.evaluate(agent.sessionId, `(async()=>{const t0=Date.now();try{const r=await Promise.race([fetch(${JSON.stringify(`http://${host}:${probe.port}/t/body?g3=${h}`)},{cache:'no-store'}),new Promise((_,j)=>setTimeout(()=>j(new Error('timeout')),9000))]);const headersAt=Date.now();const b=(await r.text()).slice(0,40);return 'OK:'+b+'|headersAt='+headersAt+'|bodyAt='+Date.now()}catch(e){return 'ERR:'+e.message}})()`, 12000);
    await sleep(300);
    const lateRip = obs.networkId ? guard.net.get(obs.networkId) : null;
    if (lateRip) obs.ripAfterContinue = { value: lateRip.rip === lanIp ? 'LAN-IP' : lateRip.rip, receivedByP_msAfterContinue: lateRip.ts - obs.continuedAt, pageGotHeadersAt_msAfterContinue: Number((/headersAt=(\d+)/.exec(pageSaw) || [])[1]) - obs.continuedAt };
    obs.pageSaw = pageSaw.replace(/:\d+/, ':PORT').replace(/\|headersAt.*/, '');
    obs.bodyReachedPage = /SECRET-BODY/.test(pageSaw);
    obs.elapsedMs = Date.now() - started;
    obs.networkLoadingFailed = loadFailed.map((f) => f.error || f.blocked).slice(0, 3);
    obs.serverHits = probe.hits.filter((x) => x.search.includes(`g3=${h}`)).length;
    g3[h] = obs;
  }
  out.G3_detail = g3;
  rec('G3', { note: 'names mapped by --host-resolver-rules in one launch: loop.test->127.0.0.1, lan.test->a private LAN address of this machine, ten.test->10.255.255.1 (unroutable)', ...Object.fromEntries(Object.entries(g3).map(([k, v]) => [k, typeof v === 'string' ? v : { requestStageSeesIP: v.reqStage?.ipFieldKeys?.length > 0, ripAtResponsePause: v.respStage?.ripAtPause, ripWithin600ms: v.respStage?.ripWithin600ms, getResponseBodyAtPause: v.respStage?.getResponseBody, ripAfterGetResponseBody: v.respStage?.ripAfterGetResponseBody, ripAfterContinue: v.ripAfterContinue, bodyReachedPage: v.bodyReachedPage, serverContactedBeforeRequestStageContinue: v.reqStage?.serverHitsWhilePaused > 0, serverHitsTotal: v.serverHits, failed: v.networkLoadingFailed }])) });

  // ================= G4 =================
  reset();
  await P.send('Fetch.disable', {}, pSid);
  await navA(`${probe.origin}/g4`);
  await fetchEnable([{ urlPattern: '*', requestStage: 'Request' }]);
  const g4 = {};
  // 4.1 baseline and A's attempts to switch P off
  guard.handler = null;
  reset();
  await aFetch('/t/g4a?step=base');
  g4.baselineCaught = guard.events.some((e) => e.url.includes('step=base'));
  g4.aFetchDisable = await A.send('Fetch.disable', {}, agent.sessionId).then(() => 'ok', (e) => e.message);
  await aFetch('/t/g4a?step=afterA-disable');
  g4.caughtAfterAFetchDisable = guard.events.some((e) => e.url.includes('step=afterA-disable'));
  g4.aNetworkBypassSW = await A.send('Network.setBypassServiceWorker', { bypass: true }, agent.sessionId).then(() => 'ok', (e) => e.message);
  await aFetch('/t/g4a?step=afterBypassSW');
  g4.caughtAfterSetBypassServiceWorker = guard.events.some((e) => e.url.includes('step=afterBypassSW'));
  await A.send('Network.setBypassServiceWorker', { bypass: false }, agent.sessionId).catch(() => {});
  g4.aNetworkEnableDisable = await A.send('Network.disable', {}, agent.sessionId).then(() => 'ok', (e) => e.message);
  await aFetch('/t/g4a?step=afterNetworkDisable');
  g4.caughtAfterNetworkDisable = guard.events.some((e) => e.url.includes('step=afterNetworkDisable'));
  g4.aSetAutoAttachOff = await A.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }, agent.sessionId).then(() => 'ok', (e) => e.message);
  await aFetch('/t/g4a?step=afterAutoAttachOff');
  g4.caughtAfterASetAutoAttachOff = guard.events.some((e) => e.url.includes('step=afterAutoAttachOff'));
  g4.aEmulateOffline = await A.send('Network.emulateNetworkConditions', { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 }, agent.sessionId).then(() => 'ok', (e) => e.message);
  // 4.2 A cannot act on P's paused request
  reset();
  let pHeld = null;
  await A.send('Fetch.enable', { patterns: [{ urlPattern: '*/nomatch-never*', requestStage: 'Request' }] }, agent.sessionId);
  guard.handler = (p) => { pHeld = p; };
  const heldFetch = aFetch('/t/g4a?step=held', '{}', 4000);
  await sleep(600);
  g4.aContinuesPsRequest = pHeld ? await A.send('Fetch.continueRequest', { requestId: pHeld.requestId }, agent.sessionId).then(() => 'ACCEPTED', (e) => e.message) : 'P held nothing';
  g4.aFailsPsRequest = pHeld ? await A.send('Fetch.failRequest', { requestId: pHeld.requestId, errorReason: 'Failed' }, agent.sessionId).then(() => 'ACCEPTED', (e) => e.message) : 'P held nothing';
  g4.aFulfillsPsRequest = pHeld ? await A.send('Fetch.fulfillRequest', { requestId: pHeld.requestId, responseCode: 200, body: b64('FROM-A') }, agent.sessionId).then(() => 'ACCEPTED', (e) => e.message) : 'P held nothing';
  if (pHeld) await P.send('Fetch.failRequest', { requestId: pHeld.requestId, errorReason: 'BlockedByClient' }, pSid).catch(() => {});
  g4.heldPageResult = await heldFetch;
  await A.send('Fetch.disable', {}, agent.sessionId).catch(() => {});
  // 4.3 both enabled: who decides?
  const A_EVENTS = [];
  let aAct = null;
  const aSids = new Set([agent.sessionId]);
  A.on('Fetch.requestPaused', (p, sid) => {
    if (!aSids.has(sid)) return;
    A_EVENTS.push({ ts: Date.now(), url: p.request.url });
    if (aAct) aAct(p, sid);
  });
  const act = (conn, spec, delay) => async (p, sid) => {
    if (!p.request.url.includes('/t/g4c')) return void cont(conn, p, sid);
    if (delay) await sleep(delay);
    const r = spec === 'continue' ? conn.send('Fetch.continueRequest', { requestId: p.requestId }, sid)
      : spec === 'fail' ? conn.send('Fetch.failRequest', { requestId: p.requestId, errorReason: 'BlockedByClient' }, sid)
      : conn.send('Fetch.fulfillRequest', { requestId: p.requestId, responseCode: 200, responseHeaders: [{ name: 'Access-Control-Allow-Origin', value: '*' }], body: b64(spec.split(':')[1]) }, sid);
    await r.catch((e) => (act.errors.push(`${spec}: ${e.message}`)));
  };
  act.errors = [];
  async function combo(label, pSpec, aSpec, { aFirst = false, pDelay = 0, aDelay = 0, aSid = agent.sessionId, aLabel = '' } = {}) {
    reset();
    A_EVENTS.length = 0;
    act.errors = [];
    await P.send('Fetch.disable', {}, pSid).catch(() => {});
    for (const sd of aSids) await A.send('Fetch.disable', {}, sd).catch(() => {});
    const enP = () => fetchEnable([{ urlPattern: '*/t/g4c*', requestStage: 'Request' }]);
    const enA = () => A.send('Fetch.enable', { patterns: [{ urlPattern: '*/t/g4c*', requestStage: 'Request' }] }, aSid);
    if (aFirst) { await enA(); await enP(); } else { await enP(); await enA(); }
    guard.handler = act(P, pSpec, pDelay);
    aAct = act(A, aSpec, aDelay);
    const aAct0 = aAct;
    aAct = (p, sd) => aAct0(p, sd);
    const before = probe.hitsOf('/t/g4c').length;
    const saw = await aFetch(`/t/g4c?c=${label}`, '{}', 5000);
    const r = { enableOrder: (aFirst ? 'A then P' : 'P then A') + aLabel, P: pSpec + (pDelay ? `+${pDelay}ms` : ''), A: aSpec + (aDelay ? `+${aDelay}ms` : ''), pSawIt: guard.events.some((e) => e.url.includes('g4c')), aSawIt: A_EVENTS.length > 0, firstToSee: !guard.events.length ? (A_EVENTS.length ? 'A' : '-') : !A_EVENTS.length ? 'P' : guard.events[0].ts <= A_EVENTS[0].ts ? 'P' : 'A', pageSaw: saw, serverReached: probe.hitsOf('/t/g4c').length > before, errors: act.errors.slice() };
    for (const sd of aSids) await A.send('Fetch.disable', {}, sd).catch(() => {});
    aAct = null;
    return r;
  }
  g4.combos = [];
  g4.combos.push(await combo('1', 'continue', 'fulfill:FROM-A'));
  g4.combos.push(await combo('2', 'fail', 'continue'));
  g4.combos.push(await combo('3', 'fail', 'fulfill:FROM-A'));
  g4.combos.push(await combo('4', 'fulfill:FROM-P', 'continue'));
  g4.combos.push(await combo('5', 'continue', 'fail'));
  g4.combos.push(await combo('6', 'fail', 'fulfill:FROM-A', { aFirst: true }));
  g4.combos.push(await combo('7', 'fail', 'fulfill:FROM-A', { pDelay: 400 }));
  g4.combos.push(await combo('8', 'continue', 'fulfill:FROM-A', { aFirst: true }));
  // A second session of the same connection, created after P's: does the newest session decide first?
  const a2 = await A.attach(agent.targetId);
  aSids.add(a2);
  g4.combos.push(await combo('9', 'fail', 'fulfill:FROM-A2', { aSid: a2, aLabel: ' (A uses a NEW session made after P)' }));
  g4.combos.push(await combo('10', 'continue', 'fulfill:FROM-A2', { aSid: a2, aLabel: ' (A uses a NEW session made after P)' }));
  g4.combos.push(await combo('11', 'fail', 'continue', { aSid: a2, aLabel: ' (A uses a NEW session made after P)' }));
  await A.send('Target.detachFromTarget', { sessionId: a2 }).catch(() => {});
  await P.send('Fetch.disable', {}, pSid).catch(() => {});
  reset();
  // 4.4 service worker route: a page request that goes through a SW; A toggles bypass
  await fetchEnable([{ urlPattern: '*', requestStage: 'Request' }]);
  guard.handler = null;
  await navA(`${probe.origin}/g1?p=SWT`); // registers the SW (scope /)
  await sleep(2000);
  await navA(`${probe.origin}/g4`); // now controlled by the SW
  const controlled = await A.evaluate(agent.sessionId, `!!navigator.serviceWorker.controller`);
  reset();
  await aFetch('/t/viasw?m=plain');
  const swHits = () => probe.hitsOf('/t/viasw').map((h) => (h.search.includes('sw=1') ? 'SW' : 'page'));
  const seen = (m) => guard.events.filter((e) => e.url.includes(`m=${m}`)).map((e) => `${e.via}:${e.type}${e.url.includes('sw=1') ? ':sw=1' : ''}`);
  const plain = seen('plain');
  const plainHits = swHits();
  await A.send('Network.enable', {}, agent.sessionId).catch(() => {});
  await A.send('Network.setBypassServiceWorker', { bypass: true }, agent.sessionId);
  await aFetch('/t/viasw?m=bypass');
  const bypass = seen('bypass');
  await A.send('Network.setBypassServiceWorker', { bypass: false }, agent.sessionId);
  await A.send('Network.disable', {}, agent.sessionId).catch(() => {});
  // same again with P also attached to the service worker target (page-level auto-attach)
  guard.autoFetch = true;
  await P.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }, pSid);
  await sleep(800);
  await aFetch('/t/viasw?m=withSwSession');
  const withSw = seen('withSwSession');
  await P.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }, pSid).catch(() => {});
  guard.autoFetch = false;
  g4.viaServiceWorker = { pageControlledBySW: controlled, pageFetch_caughtByP_pageSessionOnly: plain, pageFetch_caughtByP_whenAbypassesSW: bypass, caughtByP_whenPAlsoAttachedToSwTarget: withSw, serverHitsInOrder_plain_bypass_withSw: swHits() };
  // 4.5 A autoattaches to new workers and resumes them before P's Fetch is on them
  await P.send('Fetch.disable', {}, pSid).catch(() => {});
  guard.autoFetch = true;
  guard.resumeDelay = 400; // P is slow to enable Fetch on a new worker
  await fetchEnable([{ urlPattern: '*', requestStage: 'Request' }]);
  await P.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }, pSid);
  reset();
  await A.evaluate(agent.sessionId, `new Worker('/t/worker.js?p=G4NOA')&&1`);
  await sleep(2500);
  const noA = guard.events.some((e) => e.url.includes('/t/worker-fetch?p=G4NOA'));
  const aAttach = [];
  A.on('Target.attachedToTarget', (p, sid) => {
    if (sid !== agent.sessionId) return;
    aAttach.push(p.targetInfo.type);
    A.send('Runtime.runIfWaitingForDebugger', {}, p.sessionId).catch(() => {});
  });
  await A.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }, agent.sessionId);
  reset();
  await A.evaluate(agent.sessionId, `new Worker('/t/worker.js?p=G4WITHA')&&1`);
  await sleep(2500);
  const withA = guard.events.some((e) => e.url.includes('/t/worker-fetch?p=G4WITHA'));
  const arrivedWithA = probe.hits.some((h) => h.path === '/t/worker-fetch' && h.search === '?p=G4WITHA');
  g4.workerRace = { workerFetchCaughtWhenOnlyPAutoAttaches: noA, workerFetchCaughtWhenAlsoAAutoAttachesAndResumesAtOnce: withA, workerFetchArrivedAtServer: arrivedWithA, aSawAttach: aAttach };
  await A.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }, agent.sessionId).catch(() => {});
  await P.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }, pSid).catch(() => {});
  guard.autoFetch = false;
  guard.resumeDelay = 0;
  out.G4_detail = g4;
  const noOneSwitchedOff = g4.baselineCaught && g4.caughtAfterAFetchDisable && g4.caughtAfterSetBypassServiceWorker && g4.caughtAfterNetworkDisable && g4.caughtAfterASetAutoAttachOff;
  rec('G4.switchOff', `A's Fetch.disable=${g4.aFetchDisable}; P still caught after: Fetch.disable=${g4.caughtAfterAFetchDisable}, Network.setBypassServiceWorker=${g4.caughtAfterSetBypassServiceWorker}, Network.disable=${g4.caughtAfterNetworkDisable}, Target.setAutoAttach(off)=${g4.caughtAfterASetAutoAttachOff} => ${noOneSwitchedOff ? 'PASS' : 'FAIL'}`);
  rec('G4.steal', { aContinue: g4.aContinuesPsRequest, aFail: g4.aFailsPsRequest, aFulfill: g4.aFulfillsPsRequest });
  for (const c of g4.combos) rec(`G4.combo`, `enable ${c.enableOrder} | P:${c.P} A:${c.A} | P saw=${c.pSawIt} A saw=${c.aSawIt} first=${c.firstToSee} | page saw ${c.pageSaw} | server reached=${c.serverReached}${c.errors.length ? ' | errors ' + c.errors.join('; ') : ''}`);
  rec('G4.serviceWorker', g4.viaServiceWorker);
  rec('G4.workerRace', g4.workerRace);
  const blockSurvives = g4.combos.filter((c) => c.P.startsWith('fail')).every((c) => !/FROM-A|OK:/.test(c.pageSaw));
  rec('G4', `A cannot switch off P: ${noOneSwitchedOff ? 'yes' : 'NO'}; A cannot override a P block (fail) with continue/fulfill in any tried order: ${blockSurvives ? 'yes' : 'NO'}; worker race hole: ${g4.workerRace.workerFetchCaughtWhenOnlyPAutoAttaches && !g4.workerRace.workerFetchCaughtWhenAlsoAAutoAttachesAndResumesAtOnce ? 'YES (A can start a worker before P enables Fetch)' : 'no'}`);

  // ================= G5 =================
  reset();
  await P.send('Fetch.disable', {}, pSid).catch(() => {});
  const g5 = {};
  // 5.1 connection close
  const P2 = await env.connection();
  const p2Sid = await P2.attach(agent.targetId);
  let p2Held = 0;
  P2.on('Fetch.requestPaused', (p, sid) => { if (sid === p2Sid) p2Held++; });
  await P2.send('Fetch.enable', { patterns: [{ urlPattern: '*', requestStage: 'Request' }] }, p2Sid);
  const hitsB = probe.hitsOf('/t/g5').length;
  const heldFetch5 = aFetch('/t/g5?step=held', '{}', 6000);
  await sleep(800);
  g5.heldWhileConnectionOpen = { pausedAtP2: p2Held, serverReached: probe.hitsOf('/t/g5').length > hitsB };
  P2.close();
  await sleep(300);
  g5.heldRequestAfterClose = await heldFetch5;
  g5.heldServerReachedAfterClose = probe.hitsOf('/t/g5').length > hitsB;
  g5.laterRequestAfterClose = await aFetch('/t/g5?step=after', '{}', 5000);
  // 5.2 per tab: popup from the agent tab with only the first tab guarded
  await fetchEnable([{ urlPattern: '*', requestStage: 'Request' }]);
  reset();
  const known = (await env.pages()).map((t) => t.targetId);
  await A.send('Runtime.evaluate', { expression: `window.open('/t/g5?step=popup-plain')&&1`, userGesture: true }, agent.sessionId);
  await sleep(1500);
  g5.popupWithoutAutoAttach_firstRequestCaught = guard.events.some((e) => e.url.includes('popup-plain'));
  g5.popupWithoutAutoAttach_arrived = probe.hits.some((h) => h.search === '?step=popup-plain');
  // 5.3 page-level auto-attach with waitForDebuggerOnStart
  guard.autoFetch = true;
  guard.attached = [];
  await P.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }, pSid);
  reset();
  await A.send('Runtime.evaluate', { expression: `window.open('/t/g5?step=popup-pageauto')&&1`, userGesture: true }, agent.sessionId);
  await sleep(1500);
  g5.popupWithPageAutoAttach = { firstRequestCaught: guard.events.some((e) => e.url.includes('popup-pageauto')), attached: guard.attached.map((a) => `${a.type}/${a.waiting ? 'wait' : 'nowait'}`), arrived: probe.hits.some((h) => h.search === '?step=popup-pageauto') };
  await P.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }, pSid);
  // 5.4 new tab opened by the browser itself (as the bridge would) in the Agent profile, URL set at creation
  const { spawnSync } = await import('node:child_process');
  const { CHROME } = await import('./lib/launch.mjs');
  const openAgentWindow = async (step) => {
    spawnSync(CHROME, [`--user-data-dir=${env.udd}`, '--profile-directory=Agent', '--no-first-run', '--new-window', `${probe.origin}/t/g5?step=${step}`], { timeout: 20000 });
    await sleep(1500);
  };
  // 5.4a only setDiscoverTargets + attach afterwards
  reset();
  guard.autoFetch = false;
  const discovered = [];
  P.on('Target.targetCreated', async (p) => {
    if (p.targetInfo.type !== 'page' || p.targetInfo.browserContextId !== env.agentCtx || !p.targetInfo.url.includes('step=newwin-discover')) return;
    const sid = await P.attach(p.targetInfo.targetId).catch(() => null);
    if (!sid) return;
    sessionKind.set(sid, 'page');
    await P.send('Fetch.enable', { patterns: [{ urlPattern: '*', requestStage: 'Request' }] }, sid).catch(() => {});
    discovered.push(Date.now());
  });
  await P.send('Target.setDiscoverTargets', { discover: true });
  await openAgentWindow('newwin-discover');
  g5.newWindowDiscoverThenAttach = { firstRequestCaught: guard.events.some((e) => e.url.includes('newwin-discover')), arrived: probe.hits.some((h) => h.search === '?step=newwin-discover'), attachedAfterCreate: discovered.length > 0 };
  await P.send('Target.setDiscoverTargets', { discover: false });
  // 5.4b browser-level auto-attach with waitForDebuggerOnStart
  reset();
  guard.autoFetch = true;
  guard.attached = [];
  await P.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true });
  await sleep(500);
  guard.attached = [];
  reset();
  await openAgentWindow('newwin-browserauto');
  g5.newWindowBrowserAutoAttach = { firstRequestCaught: guard.events.some((e) => e.url.includes('newwin-browserauto')), arrived: probe.hits.some((h) => h.search === '?step=newwin-browserauto'), attached: guard.attached.map((a) => `${a.type}/${a.ctx}/${a.waiting ? 'wait' : 'nowait'}`) };
  reset();
  guard.attached = [];
  await A.send('Runtime.evaluate', { expression: `window.open('/t/g5page?step=popup-browserauto')&&1`, userGesture: true }, agent.sessionId);
  await sleep(1500);
  g5.popupWithBrowserAutoAttach = { firstRequestCaught: guard.events.some((e) => e.url.includes('g5page?step=popup-browserauto')), subresourceCaught: guard.events.some((e) => e.url.includes('/t/g5-sub')), arrived: probe.hits.some((h) => h.search === '?step=popup-browserauto'), attached: guard.attached.map((a) => `${a.type}/${a.ctx}/${a.waiting ? 'wait' : 'nowait'}`) };
  // a Human-profile tab at the same moment: does browser-level auto-attach also pause the human's new tabs?
  reset();
  guard.attached = [];
  const hk = (await env.pages()).map((t) => t.targetId);
  const human = await env.newIn(env.humanCtx, `${probe.origin}/blank`).then(() => 'opened', (e) => e.message);
  await sleep(500);
  g5.browserAutoAttachSeesHumanTabs = { humanTabOpened: human, attached: guard.attached.map((a) => `${a.type}/${a.ctx}/${a.waiting ? 'wait' : 'nowait'}`) };
  await P.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true });
  guard.autoFetch = false;
  out.G5_detail = g5;
  rec('G5.closeReleases', { heldWhileOpen: g5.heldWhileConnectionOpen, heldRequestAfterClose: g5.heldRequestAfterClose, laterRequestAfterClose: g5.laterRequestAfterClose });
  rec('G5.perTab', { popupWithoutAutoAttach: { firstRequestCaught: g5.popupWithoutAutoAttach_firstRequestCaught, arrived: g5.popupWithoutAutoAttach_arrived }, popupWithPageAutoAttach: g5.popupWithPageAutoAttach });
  rec('G5.popupBrowserAutoAttach', g5.popupWithBrowserAutoAttach);
  rec('G5.newWindow', { discoverThenAttach: g5.newWindowDiscoverThenAttach, browserAutoAttachWait: g5.newWindowBrowserAutoAttach, humanTabsSeenByBrowserAutoAttach: g5.browserAutoAttachSeesHumanTabs });
  rec('G5', `closing the P connection ${/ERR/.test(g5.heldRequestAfterClose) ? 'FAILED the held request' : 'RESUMED the held request'}; later requests ${/OK/.test(g5.laterRequestAfterClose) ? 'pass unguarded' : 'are blocked'}; popup first request caught: plain=${g5.popupWithoutAutoAttach_firstRequestCaught}, pageAutoAttach=${g5.popupWithPageAutoAttach.firstRequestCaught}; new browser-made window first request caught: discoverThenAttach=${g5.newWindowDiscoverThenAttach.firstRequestCaught}, browserAutoAttach=${g5.newWindowBrowserAutoAttach.firstRequestCaught}`);

  // ================= G6 =================
  await P.send('Fetch.disable', {}, pSid).catch(() => {});
  guard.handler = null;
  reset();
  await navA(`${probe.origin}/blank`);
  const bench = async (label) => {
    const times = await A.evaluate(agent.sessionId, `(async()=>{const t=[];for(let i=0;i<100;i++){const s=performance.now();const r=await fetch('/t/ping?b=${label}'+i,{cache:'no-store'});await r.text();t.push(performance.now()-s)}return t})()`, 60000);
    return stat(times);
  };
  const g6 = {};
  g6.baseline1 = await bench('b1');
  await fetchEnable([{ urlPattern: '*', requestStage: 'Request' }]);
  g6.requestStage = await bench('rq');
  await P.send('Fetch.disable', {}, pSid);
  await fetchEnable([{ urlPattern: '*', requestStage: 'Request' }, { urlPattern: '*', requestStage: 'Response' }]);
  g6.requestAndResponseStage = await bench('rr');
  await P.send('Fetch.disable', {}, pSid);
  g6.baseline2 = await bench('b2');
  out.G6_detail = g6;
  const base = (g6.baseline1.meanMs + g6.baseline2.meanMs) / 2;
  rec('G6', `mean per request (100 sequential fetches): baseline ${base.toFixed(2)}ms (runs ${g6.baseline1.meanMs}/${g6.baseline2.meanMs}); request-stage ${g6.requestStage.meanMs}ms (+${(g6.requestStage.meanMs - base).toFixed(2)}); request+response stage ${g6.requestAndResponseStage.meanMs}ms (+${(g6.requestAndResponseStage.meanMs - base).toFixed(2)})`);
} finally {
  out._meta = { when: new Date().toISOString(), elapsedS: Math.round((Date.now() - t0) / 1000) };
  console.log('saved', writeResult('20-request-guard', out));
  await finish();
}
process.exit(0);
