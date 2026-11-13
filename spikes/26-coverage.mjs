// Spike 26 (round 2, D1-D5): interception coverage holes (OOPIF, service worker, browser-level Fetch, how tabs are made, session decision order).
import { randomBytes } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';
import { writeResult, sleep, launchChrome, CHROME, SPIKES } from './lib/launch.mjs';
import { startProbe, html, openTwoProfiles, reporter, parseArgs } from './lib/probe.mjs';
import { connect } from './lib/cdp.mjs';

const out = {};
const rec = reporter('26', out);
const runId = randomBytes(4).toString('hex');
const only = parseArgs().only;
const want = (k) => !only || only.includes(k);
const probe = await startProbe();
const R = probe.routes;
const HDR = { 'content-type': 'text/html' };
R['/blank'] = (_q, res) => (res.writeHead(200, HDR), res.end(html('<p>b</p>')));
R['/ok'] = (_q, res) => (res.writeHead(200, { 'access-control-allow-origin': '*' }), res.end('ok'));
R['/img.png'] = (_q, res) => (res.writeHead(200, { 'content-type': 'image/gif' }), res.end(Buffer.from('R0lGODlhAQABAAAAACw=', 'base64')));
// D1: top page with a cross-site iframe whose document loads an image and runs a fetch
R['/oop-inner'] = (_q, res) => (res.writeHead(200, HDR), res.end(html(`<img src="/img.png?d1=inner-img"><script>fetch('/ok?d1=inner-fetch',{mode:'no-cors'})</script>`)));
const show = (id, v) => rec(id, JSON.stringify(v));
const mkev = (conn) => (sid, expr, gesture = false) => conn.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true, userGesture: gesture }, sid).then((r) => (r.exceptionDetails ? `EXC ${r.exceptionDetails.exception?.description?.split('\n')[0] || r.exceptionDetails.text}` : r.result.value), (e) => `CDPERR ${e.message}`);
const hitsWith = (s) => probe.hits.filter((h) => h.search.includes(s)).length;

try {
  rec('RUN', { id: 'run-' + runId });
  const env = await openTwoProfiles({ name: '26-cov', probe, seedPath: '/blank', args: ['--host-resolver-rules=MAP c.test 127.0.0.1'] });
  const P = env.cdp, ps = env.agent.sessionId; const ev = mkev(P);
  R['/oop'] = (_q, res) => (res.writeHead(200, HDR), res.end(html(`<iframe id="x" src="http://c.test:${probe.port}/oop-inner" style="width:300px;height:100px"></iframe>`)));
  // a generic recorder of paused requests across all sessions we enable
  const paused = [];
  P.on('Fetch.requestPaused', (p, sid) => { paused.push({ sid, url: p.request.url, type: p.resourceType }); P.send('Fetch.continueRequest', { requestId: p.requestId }, sid).catch(() => {}); });
  const pausedFor = (s) => paused.filter((x) => x.url.includes(s));
  const label = (sid) => (sid === ps ? 'page-session' : sid ? 'other-session' : 'browser-session');
  // ================= D1 =================
  if (want('d1')) {
    const d1 = {};
    const run = async (name, setup) => {
      paused.length = 0;
      const h0 = hitsWith('d1=');
      const clean = await setup();
      await P.navigate(ps, `${probe.origin}/oop`).catch(() => {});
      await sleep(1500);
      // fetch evaluated INSIDE the iframe: needs the iframe target
      const targets = (await P.send('Target.getTargets')).targetInfos.filter((t) => t.type === 'iframe');
      let evalResult = 'no iframe target found';
      if (targets.length) {
        const s = await P.attach(targets[targets.length - 1].targetId);
        const before = paused.length;
        evalResult = await ev(s, `fetch('/ok?d1=eval-in-iframe-${name.slice(0, 2)}',{mode:'no-cors'}).then(()=>'reached',e=>'failed '+e.message)`);
        await sleep(300);
        await P.send('Target.detachFromTarget', { sessionId: s }).catch(() => {});
      }
      d1[name] = {
        iframeTargetsListed: targets.length,
        innerImg: { caught: pausedFor('inner-img').map((x) => label(x.sid)), serverSaw: hitsWith('inner-img') > 0 },
        innerPageFetch: { caught: pausedFor('inner-fetch').map((x) => label(x.sid)), serverSaw: hitsWith('inner-fetch') > 0 },
        evalFetchInsideIframe: { page: evalResult, caught: pausedFor('eval-in-iframe').map((x) => label(x.sid)) },
        iframeDocument: pausedFor('/oop-inner').map((x) => label(x.sid)),
      };
      if (clean) await clean();
      show(`D1.${name}`, d1[name]);
    };
    await run('a_page_session_Fetch_only', async () => { await P.send('Fetch.enable', { patterns: [{ urlPattern: '*' }] }, ps); return async () => P.send('Fetch.disable', {}, ps); });
    await run('b_page_session_Fetch_plus_page_session_autoAttach_recursive', async () => {
      await P.send('Fetch.enable', { patterns: [{ urlPattern: '*' }] }, ps);
      const h = async (p, from) => { if (p.targetInfo.type === 'iframe' || p.targetInfo.type === 'page') { await P.send('Fetch.enable', { patterns: [{ urlPattern: '*' }] }, p.sessionId).catch(() => {}); await P.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }, p.sessionId).catch(() => {}); } await P.send('Runtime.runIfWaitingForDebugger', {}, p.sessionId).catch(() => {}); };
      P.on('Target.attachedToTarget', h);
      await P.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }, ps);
      return async () => { await P.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }, ps).catch(() => {}); await P.send('Fetch.disable', {}, ps).catch(() => {}); };
    });
    await run('c_browser_level_autoAttach', async () => {
      await P.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true });
      return async () => { await P.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }).catch(() => {}); };
    });
    out.D1 = d1;
  }
  // ================= D3 =================
  if (want('d3')) {
    const d3 = {};
    const t2 = await env.newIn(env.agentCtx, null);
    const humanSid = env.human.sessionId;
    const probeAll = async (name, conn, sid) => {
      paused.length = 0;
      await P.navigate(ps, `${probe.origin}/blank?d3=${name}-agent`).catch(() => {});
      await P.navigate(t2.sessionId, `${probe.origin}/blank?d3=${name}-agent2`).catch(() => {});
      await P.navigate(humanSid, `${probe.origin}/blank?d3=${name}-human`).catch(() => {});
      await ev(ps, `fetch('/ok?d3=${name}-fetch',{mode:'no-cors'}).then(()=>1)`);
      await sleep(400);
      return { agentTab: pausedFor(`${name}-agent?`.slice(0, -1)).length > 0 && pausedFor(`d3=${name}-agent`).length > 0, agentTab2: pausedFor(`d3=${name}-agent2`).length > 0, humanTab: pausedFor(`d3=${name}-human`).length > 0, fetchInAgentTab: pausedFor(`d3=${name}-fetch`).length > 0 };
    };
    // (1) Fetch.enable with no sessionId (browser-level)
    d3.browserLevel_Fetch_enable_ack = await P.send('Fetch.enable', { patterns: [{ urlPattern: '*' }] }).then(() => 'accepted', (e) => e.message);
    d3.browserLevel_seen = await probeAll('bl', P);
    // does the browser-level Fetch also see cross-site iframe subrequests, a dedicated worker, a popup's first request?
    paused.length = 0;
    await P.navigate(ps, `${probe.origin}/oop`).catch(() => {});
    await ev(ps, `new Promise(r=>{const w=new Worker(URL.createObjectURL(new Blob(["fetch('${probe.origin}/ok?d3=blw-worker',{mode:'no-cors'}).then(()=>postMessage(1))"])));w.onmessage=()=>r(1);setTimeout(()=>r(0),3000)})`);
    await ev(ps, `window.open('/blank?d3=blw-popup')&&1`, true);
    await sleep(1500);
    d3.browserLevel_extra = { crossSiteIframeImg: pausedFor('inner-img').length > 0, crossSiteIframeFetch: pausedFor('inner-fetch').length > 0, dedicatedWorkerFetch: pausedFor('blw-worker').length > 0, popupFirstRequest: pausedFor('blw-popup').length > 0 };
    await P.send('Fetch.disable', {}).catch(() => {});
    // (2) a session from attachToBrowserTarget
    const bt = await P.send('Target.attachToBrowserTarget').then((r) => r.sessionId, (e) => 'ERR ' + e.message);
    d3.attachToBrowserTarget_Fetch_enable_ack = bt.startsWith('ERR') ? bt : await P.send('Fetch.enable', { patterns: [{ urlPattern: '*' }] }, bt).then(() => 'accepted', (e) => e.message);
    d3.attachToBrowserTarget_seen = await probeAll('bt', P);
    if (!bt.startsWith('ERR')) await P.send('Fetch.disable', {}, bt).catch(() => {});
    // (3) per-tab only: the agent tab's page session
    await P.send('Fetch.enable', { patterns: [{ urlPattern: '*' }] }, ps);
    d3.pageSessionOnly_seen = await probeAll('ps', P);
    await P.send('Fetch.disable', {}, ps).catch(() => {});
    // (4) Network.setRequestInterception
    d3.Network_setRequestInterception = await P.send('Network.setRequestInterception', { patterns: [{ urlPattern: '*' }] }, ps).then(() => 'accepted', (e) => e.message);
    await P.send('Target.closeTarget', { targetId: t2.targetId }).catch(() => {});
    show('D3', d3);
    out.D3 = d3;
  }
  // ================= D5 =================
  if (want('d5')) {
    const A = await env.connection(); // "agent" connection (own sessions)
    const PY = await env.connection(); // a pegboardd connection created AFTER the agent's
    const results = {};
    const orders = [
      ['sessions created P then A; Fetch.enable P then A', 'PA', 'PA'],
      ['sessions created P then A; Fetch.enable A then P', 'PA', 'AP'],
      ['sessions created A then P; Fetch.enable A then P', 'AP', 'AP'],
      ['sessions created A then P; Fetch.enable P then A', 'AP', 'PA'],
      ['PEGBOARD connection younger than agent connection; sessions A then P; Fetch.enable A then P', 'AP', 'AP', 'young'],
      ['PEGBOARD connection younger than agent connection; sessions A then P; Fetch.enable P then A', 'AP', 'PA', 'young'],
      ['PEGBOARD connection younger than agent connection; sessions P then A; Fetch.enable P then A', 'PA', 'PA', 'young'],
    ];
    P.on('Fetch.requestPaused', () => {});
    for (const [name, create, enable, age] of orders) {
      const Pc = age === 'young' ? PY : P;
      const wins = [];
      for (let rep = 0; rep < 5; rep++) {
        const sess = {}, conns = { P: Pc, A };
        for (const who of create) sess[who] = await conns[who].attach(env.agent.targetId);
        const seenBy = new Set();
        const tag = `d5-${rep}-${create}${enable}-${runId}`;
        // handlers: P blocks (failRequest), A fulfils; each continues if it is not the one with the tag
        const mine = (who) => (p, sid) => { if (sid !== sess[who] || !p.request.url.includes(tag)) return; seenBy.add(who); if (who === 'P') conns.P.send('Fetch.failRequest', { requestId: p.requestId, errorReason: 'AccessDenied' }, sid).catch(() => {}); else conns.A.send('Fetch.fulfillRequest', { requestId: p.requestId, responseCode: 200, body: Buffer.from('A-FULFILLED').toString('base64'), responseHeaders: [{ name: 'access-control-allow-origin', value: '*' }] }, sid).catch(() => {}); };
        const hP = mine('P'), hA = mine('A');
        Pc.on('Fetch.requestPaused', hP); A.on('Fetch.requestPaused', hA);
        for (const who of enable) await conns[who].send('Fetch.enable', { patterns: [{ urlPattern: `*${tag}*` }] }, sess[who]);
        await conns.P.send('Page.enable', {}, sess.P);
        await Pc.navigate(sess.P, `${probe.origin}/blank`).catch(() => {});
        const r = await mkev(Pc)(sess.P, `fetch('${probe.origin}/ok?${tag}').then(r=>r.text()).then(t=>'page got: '+t,e=>'page: failed')`);
        wins.push(`${r}${seenBy.size === 2 ? ' (both saw it)' : ' (only ' + [...seenBy] + ' saw it)'}`);
        for (const who of 'PA') { await conns[who].send('Fetch.disable', {}, sess[who]).catch(() => {}); await conns[who].send('Target.detachFromTarget', { sessionId: sess[who] }).catch(() => {}); }
      }
      results[name] = wins;
      show(`D5.${name}`, wins);
    }
    A.close(); PY.close();
    out.D5 = results;
  }
  await env.close();
  // ================= D2 =================
  if (want('d2')) {
    const env2 = await openTwoProfiles({ name: '26-sw', probe, seedPath: '/blank', args: [] });
    const P2 = env2.cdp, ps2 = env2.agent.sessionId; const ev2 = mkev(P2);
    const swRoute = (n) => (_q, res) => (res.writeHead(200, { 'content-type': 'text/javascript', 'service-worker-allowed': '/' }), res.end(`importScripts('/imp.js?d2=imp-${n}');self.addEventListener('install',()=>{fetch('/ok?d2=swfetch-${n}',{mode:'no-cors'}).catch(()=>{})});self.addEventListener('fetch',()=>{});`));
    R['/imp.js'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/javascript' }), res.end('//imp'));
    const results = { before_resume: [], after_resume: [] };
    const sessLog = [];
    for (const mode of ['before_resume', 'after_resume']) {
      for (let i = 0; i < 5; i++) {
        const n = `${mode}-${i}`;
        R[`/sw-${n}.js`] = swRoute(n);
        const paused = [];
        const ph = (p, sid) => { if (p.request.url.includes(`-${n}`) || p.request.url.includes(`/sw-${n}.js`)) paused.push({ via: sid === ps2 ? 'page-session' : 'sw-session', url: p.request.url.replace(/^.*\/\/[^/]+/, ''), type: p.resourceType }); P2.send('Fetch.continueRequest', { requestId: p.requestId }, sid).catch(() => {}); };
        P2.on('Fetch.requestPaused', ph);
        const ah = async (p) => {
          if (p.targetInfo.type !== 'service_worker') { await P2.send('Runtime.runIfWaitingForDebugger', {}, p.sessionId).catch(() => {}); return; }
          if (!p.targetInfo.url.includes(n)) { await P2.send('Runtime.runIfWaitingForDebugger', {}, p.sessionId).catch(() => {}); return; }
          if (mode === 'before_resume') { await P2.send('Fetch.enable', { patterns: [{ urlPattern: '*' }] }, p.sessionId); await P2.send('Runtime.runIfWaitingForDebugger', {}, p.sessionId); }
          else { await P2.send('Runtime.runIfWaitingForDebugger', {}, p.sessionId); await P2.send('Fetch.enable', { patterns: [{ urlPattern: '*' }] }, p.sessionId).catch(() => {}); }
          sessLog.push(`${n}: sw target attached, waiting=${p.waitingForDebugger}`);
        };
        P2.on('Target.attachedToTarget', ah);
        await P2.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true });
        await P2.send('Fetch.enable', { patterns: [{ urlPattern: '*' }] }, ps2);
        await P2.navigate(ps2, `${probe.origin}/blank?d2=${n}`).catch(() => {});
        await ev2(ps2, `navigator.serviceWorker.register('/sw-${n}.js').then(r=>r.update().then(()=>'registered'),e=>'ERR '+e.message)`);
        await sleep(1500);
        // a page fetch that the SW answers (it only has an empty fetch handler, so it falls through to the network)
        const caughtScript = paused.filter((x) => x.url.startsWith(`/sw-${n}.js`)).map((x) => x.via);
        const caughtImp = paused.filter((x) => x.url.includes(`imp-${n}`)).map((x) => x.via);
        const caughtFetch = paused.filter((x) => x.url.includes(`swfetch-${n}`)).map((x) => x.via);
        results[mode].push({ scriptRequest: caughtScript.join('+') || 'NOT caught', importScripts: caughtImp.join('+') || 'NOT caught', fetchInsideSW: caughtFetch.join('+') || 'NOT caught', serverSaw: { script: probe.hits.filter((h) => h.path === `/sw-${n}.js`).length, fetchInsideSW: hitsWith(`swfetch-${n}`) } });
        // unregister so the next trial starts clean
        await ev2(ps2, `navigator.serviceWorker.getRegistrations().then(rs=>Promise.all(rs.map(r=>r.unregister()))).then(()=>1)`).catch(() => {});
        await P2.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }).catch(() => {});
        await P2.send('Fetch.disable', {}, ps2).catch(() => {});
        await sleep(300);
      }
      show(`D2.${mode}`, results[mode]);
    }
    out.D2 = results;
    await env2.close();
  }
  // ================= D4 =================
  if (want('d4')) {
    // one Chrome, the real bridge extension loaded read-only from extension/bridge, one profile
    const ext = join(SPIKES, '..', 'extension', 'bridge');
    const chrome = await launchChrome({ name: '26-tabs', profileDirectory: 'Human', args: [`--load-extension=${ext}`, '--disable-features=DisableLoadExtensionCommandLineSwitch'] });
    const B = await connect(chrome.browserWsUrl); // holds the interception ("pegboardd")
    const U = await connect(chrome.browserWsUrl); // the "driver" that makes tabs
    const evB = mkev(B), evU = mkev(U);
    const first = await U.firstPage();
    await U.navigate(first.sessionId, `${probe.origin}/blank`);
    let extTarget = null;
    for (let i = 0; i < 40 && !extTarget; i++) { await sleep(250); extTarget = (await U.send('Target.getTargets')).targetInfos.find((t) => t.type === 'service_worker' && t.url.includes('chrome-extension://') && t.url.endsWith('/sw.js')); }
    const d4 = { bridgeServiceWorkerFound: !!extTarget };
    const table = {};
    const caught = [];
    B.on('Fetch.requestPaused', (p, sid) => { caught.push({ url: p.request.url, type: p.resourceType }); B.send('Fetch.continueRequest', { requestId: p.requestId }, sid).catch(() => {}); });
    const attachedInfo = [];
    const autoAttach = async (wait) => {
      B.on('Target.attachedToTarget', async (p) => { attachedInfo.push({ type: p.targetInfo.type, waiting: p.waitingForDebugger }); if (p.targetInfo.type === 'page') { await B.send('Fetch.enable', { patterns: [{ urlPattern: '*' }] }, p.sessionId).catch(() => {}); await B.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: wait, flatten: true }, p.sessionId).catch(() => {}); } await B.send('Runtime.runIfWaitingForDebugger', {}, p.sessionId).catch(() => {}); });
      await B.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: wait, flatten: true });
    };
    const firstSeen = (tag) => ({ firstRequestCaught: caught.some((c) => c.url.includes(tag)), serverSaw: hitsWith(tag) > 0 });
    await autoAttach(true);
    await B.send('Target.setDiscoverTargets', { discover: true });
    await sleep(500);
    const esw = extTarget ? await U.attach(extTarget.targetId) : null;
    const extEval = (expr) => U.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true }, esw).then((r) => r.exceptionDetails ? 'EXC ' + (r.exceptionDetails.exception?.description || '').split('\n')[0] : r.result.value);
    const reps = 3;
    // 1 chrome.tabs.create({url})
    table['chrome.tabs.create({url})'] = [];
    for (let i = 0; i < reps; i++) { const tag = `d4=create-${i}`; caught.length = 0; await extEval(`chrome.tabs.create({url:'${probe.origin}/blank?${tag}',active:false}).then(t=>t.id)`); await sleep(1200); table['chrome.tabs.create({url})'].push(firstSeen(tag)); }
    // 2 about:blank then navigated by CDP (the product's createTab does this)
    table['chrome.tabs.create(about:blank) then Page.navigate by CDP'] = [];
    for (let i = 0; i < reps; i++) {
      const tag = `d4=blank-nav-${i}`; caught.length = 0;
      const tid = await extEval(`chrome.tabs.create({url:'about:blank',active:false}).then(t=>t.id)`);
      let tgt = null; for (let k = 0; k < 30 && !tgt; k++) { await sleep(100); tgt = (await U.send('Target.getTargets')).targetInfos.filter((t) => t.type === 'page').pop(); }
      const s = await U.attach(tgt.targetId);
      await U.send('Page.enable', {}, s); await U.send('Page.navigate', { url: `${probe.origin}/blank?${tag}` }, s); await sleep(1000);
      table['chrome.tabs.create(about:blank) then Page.navigate by CDP'].push(firstSeen(tag));
    }
    // 3 window.open  4 target=_blank link click
    table['window.open'] = [];
    for (let i = 0; i < reps; i++) { const tag = `d4=open-${i}`; caught.length = 0; await U.send('Runtime.evaluate', { expression: `window.open('/blank?${tag}')&&1`, userGesture: true }, first.sessionId); await sleep(1200); table['window.open'].push(firstSeen(tag)); }
    table['target=_blank link click'] = [];
    for (let i = 0; i < reps; i++) {
      const tag = `d4=link-${i}`; caught.length = 0;
      await U.send('Runtime.evaluate', { expression: `(()=>{document.body.innerHTML='<a id=l target=_blank href="/blank?${tag}" style="position:absolute;left:10px;top:10px;width:200px;height:40px;display:block">go</a>';return 1})()` }, first.sessionId);
      for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) await U.send('Input.dispatchMouseEvent', { type, x: 60, y: 30, button: 'left', clickCount: type === 'mouseMoved' ? 0 : 1 }, first.sessionId);
      await sleep(1200); table['target=_blank link click'].push(firstSeen(tag));
    }
    // 5 --new-window URL (second invocation of the browser binary, same user data dir)
    table['--new-window URL (browser binary)'] = [];
    for (let i = 0; i < reps; i++) { const tag = `d4=newwin-${i}`; caught.length = 0; spawnSync(CHROME, [`--user-data-dir=${chrome.userDataDir}`, '--profile-directory=Human', '--no-first-run', '--new-window', `${probe.origin}/blank?${tag}`], { timeout: 20000 }); await sleep(1500); table['--new-window URL (browser binary)'].push(firstSeen(tag)); }
    // variant: browser-level auto-attach WITHOUT waitForDebuggerOnStart, for chrome.tabs.create({url})
    await B.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: false, flatten: true });
    table['chrome.tabs.create({url}), auto-attach WITHOUT waitForDebuggerOnStart'] = [];
    for (let i = 0; i < reps; i++) { const tag = `d4=nowait-${i}`; caught.length = 0; await extEval(`chrome.tabs.create({url:'${probe.origin}/blank?${tag}',active:false}).then(t=>t.id)`); await sleep(1200); table['chrome.tabs.create({url}), auto-attach WITHOUT waitForDebuggerOnStart'].push(firstSeen(tag)); }
    d4.table = table;
    d4.attachedTargetsWaiting = attachedInfo.filter((a) => a.waiting).length;
    for (const [k, v] of Object.entries(table)) show(`D4.${k}`, v.map((x) => (x.firstRequestCaught ? 'caught' : 'NOT caught') + (x.serverSaw ? '' : ' (server never saw it)')));
    out.D4 = d4;
    for (const c of [B, U]) c.close();
    await chrome.kill();
  }
} finally {
  console.log('saved', writeResult('26-coverage', out));
  probe.close();
}
process.exit(0);
