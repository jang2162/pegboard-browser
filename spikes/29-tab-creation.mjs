// Spike 29 (round 3, items 2, 3 and the blob-worker part of 7): the product's tab-creation path, service-worker main script
// with page-level vs browser-level auto-attach, blob worker fetch.
import { randomBytes } from 'node:crypto';
import { join } from 'node:path';
import { writeResult, sleep, launchChrome, SPIKES } from './lib/launch.mjs';
import { startProbe, html, openTwoProfiles, reporter, parseArgs } from './lib/probe.mjs';
import { connect } from './lib/cdp.mjs';

const out = {};
const rec = reporter('29', out);
const runId = randomBytes(4).toString('hex');
const only = parseArgs().only;
const want = (k) => !only || only.includes(k);
const probe = await startProbe();
const R = probe.routes;
R['/blank'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html('<p>b</p>')));
R['/ok'] = (_q, res) => (res.writeHead(200, { 'access-control-allow-origin': '*' }), res.end('ok'));
const hitsWith = (s) => probe.hits.filter((h) => h.search.includes(s)).length;
const mkev = (conn) => (sid, expr, gesture = false) => conn.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true, userGesture: gesture }, sid).then((r) => (r.exceptionDetails ? `EXC ${r.exceptionDetails.exception?.description?.split('\n')[0] || r.exceptionDetails.text}` : r.result.value), (e) => `CDPERR ${e.message}`);
const show = (id, v) => rec(id, JSON.stringify(v));

try {
  rec('RUN', { id: 'run-' + runId });
  // ================= item 2 =================
  if (want('t2')) {
    const ext = join(SPIKES, '..', 'extension', 'bridge');
    const chrome = await launchChrome({ name: '29-tabs', profileDirectory: 'Human', args: [`--load-extension=${ext}`, '--disable-features=DisableLoadExtensionCommandLineSwitch'] });
    const B = await connect(chrome.browserWsUrl); // holds the interception
    const U = await connect(chrome.browserWsUrl); // drives the bridge extension
    console.log('BROWSER:', (await B.send('Browser.getVersion')).product);
    const first = await U.firstPage();
    await U.navigate(first.sessionId, `${probe.origin}/blank`);
    let extTarget = null; for (let i = 0; i < 40 && !extTarget; i++) { await sleep(250); extTarget = (await U.send('Target.getTargets')).targetInfos.find((t) => t.type === 'service_worker' && t.url.endsWith('/sw.js')); }
    const esw = await U.attach(extTarget.targetId);
    const extEval = (expr) => U.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true }, esw).then((r) => (r.exceptionDetails ? 'EXC ' + (r.exceptionDetails.exception?.description || '').split('\n')[0] : r.result.value));
    const caught = [];
    B.on('Fetch.requestPaused', (p, sid) => { caught.push({ url: p.request.url, type: p.resourceType, sid }); B.send('Fetch.continueRequest', { requestId: p.requestId }, sid).catch(() => {}); });
    let perTarget = false;
    B.on('Target.attachedToTarget', async (p) => {
      if (p.targetInfo.type === 'page' && perTarget) await B.send('Fetch.enable', { patterns: [{ urlPattern: '*' }] }, p.sessionId).catch(() => {});
      await B.send('Runtime.runIfWaitingForDebugger', {}, p.sessionId).catch(() => {});
    });
    // the sequence the product runs, using the real extension code: createGroup -> tabs.create(about:blank) -> tabs.group -> tabs.update(url)
    const productPath = async (tag) => {
      const g = await extEval(`self.peg.createGroup('spike','blue')`);
      if (typeof g === 'string') return { error: g };
      const t0 = Date.now();
      const r = await extEval(`self.peg.createTab(${g.groupId},'${probe.origin}/blank?${tag}','spike','blue')`);
      return { reply: r, ms: Date.now() - t0 };
    };
    const arrangements = {
      'X: browser-level auto-attach (waitForDebuggerOnStart) + Fetch.enable on each new page target before resume': async () => { perTarget = true; await B.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }); },
      'Y: browser-level Fetch.enable (no sessionId) + auto-attach that only resumes': async () => { perTarget = false; await B.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }); await B.send('Fetch.enable', { patterns: [{ urlPattern: '*' }] }); },
      'Z: browser-level Fetch.enable only (no auto-attach)': async () => { perTarget = false; await B.send('Fetch.enable', { patterns: [{ urlPattern: '*' }] }); },
    };
    const results = {};
    for (const [name, setup] of Object.entries(arrangements)) {
      await B.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }).catch(() => {});
      await B.send('Fetch.disable', {}).catch(() => {});
      await sleep(300);
      await setup();
      const runs = [];
      for (let i = 0; i < 10; i++) {
        const tag = `t2=${name[0]}${i}`; caught.length = 0;
        const r = await productPath(tag);
        await sleep(1000);
        runs.push({ caught: caught.some((c) => c.url.includes(tag)), serverSaw: hitsWith(tag) > 0, types: [...new Set(caught.filter((c) => c.url.includes(tag)).map((c) => c.type))], error: r.error });
        // close the tabs the product path made
        for (const t of (await U.send('Target.getTargets')).targetInfos.filter((x) => x.type === 'page' && x.targetId !== first.targetId)) await U.send('Target.closeTarget', { targetId: t.targetId }).catch(() => {});
        await sleep(200);
      }
      results[name] = { firstRequestCaught: `${runs.filter((r) => r.caught).length}/10`, serverSawAll: runs.filter((r) => r.serverSaw).length, resourceTypesSeen: [...new Set(runs.flatMap((r) => r.types))], errors: runs.filter((r) => r.error).length };
      show(`T2.${name}`, results[name]);
    }
    out.T2 = results;
    B.close(); U.close(); await chrome.kill();
  }
  // ================= item 3 + blob worker =================
  if (want('t3')) {
    const env = await openTwoProfiles({ name: '29-sw', probe, seedPath: '/blank', args: [] });
    const P = env.cdp, ps = env.agent.sessionId; const ev = mkev(P);
    const swRoute = (n) => (_q, res) => (res.writeHead(200, { 'content-type': 'text/javascript', 'service-worker-allowed': '/' }), res.end(`importScripts('/imp.js?t3=imp-${n}');self.addEventListener('install',()=>{fetch('/ok?t3=swfetch-${n}',{mode:'no-cors'}).catch(()=>{})});self.addEventListener('fetch',()=>{});`));
    R['/imp.js'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/javascript' }), res.end('//imp'));
    const label = (sid) => (sid === ps ? 'page-session' : 'sw-session');
    const results = {};
    for (const arr of ['B: page-level auto-attach (Target.setAutoAttach on the page session) + page-session Fetch.enable', 'C: browser-level auto-attach + page-session Fetch.enable']) {
      const rows = [];
      for (let i = 0; i < 10; i++) {
        const n = `${arr[0]}${i}`;
        R[`/sw-${n}.js`] = swRoute(n);
        const paused = [];
        const ph = (p, sid) => { if (p.request.url.includes(`-${n}`)) paused.push({ via: label(sid), kind: p.request.url.includes(`/sw-${n}.js`) ? 'main' : p.request.url.includes('imp-') ? 'imp' : 'fetch' }); P.send('Fetch.continueRequest', { requestId: p.requestId }, sid).catch(() => {}); };
        const ah = async (p) => {
          if (p.targetInfo.type === 'service_worker' && p.targetInfo.url.includes(n)) await P.send('Fetch.enable', { patterns: [{ urlPattern: '*' }] }, p.sessionId).catch(() => {});
          await P.send('Runtime.runIfWaitingForDebugger', {}, p.sessionId).catch(() => {});
        };
        P.on('Fetch.requestPaused', ph); P.on('Target.attachedToTarget', ah);
        await P.send('Fetch.enable', { patterns: [{ urlPattern: '*' }] }, ps);
        if (arr[0] === 'B') await P.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }, ps);
        else await P.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true });
        await P.navigate(ps, `${probe.origin}/blank?t3=${n}`).catch(() => {});
        const reg = await ev(ps, `navigator.serviceWorker.register('/sw-${n}.js').then(r=>'registered',e=>'ERR '+e.message)`);
        await sleep(1500);
        const k = (kind) => paused.filter((x) => x.kind === kind).map((x) => x.via).join('+') || 'NOT caught';
        rows.push({ reg, main: k('main'), importScripts: k('imp'), fetchInSW: k('fetch'), mainScriptServerHits: probe.hits.filter((h) => h.path === `/sw-${n}.js`).length });
        await ev(ps, `navigator.serviceWorker.getRegistrations().then(rs=>Promise.all(rs.map(r=>r.unregister()))).then(()=>1)`).catch(() => {});
        await P.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }, ps).catch(() => {});
        await P.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }).catch(() => {});
        await P.send('Fetch.disable', {}, ps).catch(() => {});
        await sleep(300);
      }
      results[arr] = { registered: rows.filter((r) => r.reg === 'registered').length, mainScriptCaught: `${rows.filter((r) => r.main !== 'NOT caught').length}/10`, mainViaWhich: [...new Set(rows.map((r) => r.main))], importScriptsCaught: `${rows.filter((r) => r.importScripts !== 'NOT caught').length}/10`, fetchInSWCaught: `${rows.filter((r) => r.fetchInSW !== 'NOT caught').length}/10`, serverReceivedMainScript: rows.filter((r) => r.mainScriptServerHits > 0).length };
      show(`T3.${arr}`, results[arr]);
    }
    out.T3 = results;
    // blob worker fetch
    const bw = {};
    const label2 = (sid) => (sid === ps ? 'page-session' : sid ? 'other-session' : 'browser-session');
    for (const arr of ['a: page-session Fetch only', 'b: page-session Fetch + page-level auto-attach', 'c: browser-level Fetch.enable (no sessionId)']) {
      const rows = [];
      for (let i = 0; i < 3; i++) {
        const tag = `t3b=${arr[0]}${i}`; const caught = [];
        const ph = (p, sid) => { if (p.request.url.includes(tag)) caught.push(label2(sid)); P.send('Fetch.continueRequest', { requestId: p.requestId }, sid).catch(() => {}); };
        P.on('Fetch.requestPaused', ph);
        const ah = async (p) => { await P.send('Runtime.runIfWaitingForDebugger', {}, p.sessionId).catch(() => {}); };
        P.on('Target.attachedToTarget', ah);
        if (arr[0] === 'c') await P.send('Fetch.enable', { patterns: [{ urlPattern: '*' }] }); else await P.send('Fetch.enable', { patterns: [{ urlPattern: '*' }] }, ps);
        if (arr[0] === 'b') await P.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }, ps);
        await P.navigate(ps, `${probe.origin}/blank`).catch(() => {});
        await ev(ps, `new Promise(r=>{const w=new Worker(URL.createObjectURL(new Blob(["fetch('${probe.origin}/ok?${tag}',{mode:'no-cors'}).then(()=>postMessage(1))"])));w.onmessage=()=>r(1);setTimeout(()=>r(0),3000)})`);
        await sleep(400);
        rows.push({ caught: caught.length > 0 ? [...new Set(caught)].join('+') : 'NOT caught', serverSaw: hitsWith(tag) > 0 });
        await P.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }, ps).catch(() => {});
        await P.send('Fetch.disable', {}).catch(() => {});
        await P.send('Fetch.disable', {}, ps).catch(() => {});
      }
      bw[arr] = rows; show(`T3.blobWorkerFetch.${arr}`, rows);
    }
    out.blobWorker = bw;
    await env.close();
  }
} finally {
  console.log('saved', writeResult('29-tab-creation', out));
  probe.close();
}
process.exit(0);
