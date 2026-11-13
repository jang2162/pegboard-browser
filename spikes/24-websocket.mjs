// Spike 24 (round 2, B1-B5): can a WebSocket be blocked, and by what? Loopback and a name mapped to a private address.
import { randomBytes } from 'node:crypto';
import { writeResult, sleep } from './lib/launch.mjs';
import { startProbe, html, openTwoProfiles, reporter, parseArgs } from './lib/probe.mjs';

const out = {};
const rec = reporter('24', out);
const runId = randomBytes(4).toString('hex');
const only = parseArgs().only;
const want = (k) => !only || only.includes(k);
const probe = await startProbe({ lan: true });
const R = probe.routes;
const SIDE = {}; // headers the server adds per path
R['/blank'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html', ...(SIDE.blank || {}) }), res.end(html('<p>b</p>')));
R['/w.js'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/javascript', ...(SIDE.worker || {}) }), res.end(`onmessage=(e)=>{const u=e.data;const w=new WebSocket(u);w.onopen=()=>postMessage('open');w.onerror=()=>postMessage('error');setTimeout(()=>postMessage('timeout'),4000)}`));
R['/ev'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/event-stream' }), res.end('data: x\n\n'));
R['/ok'] = (_q, res) => (res.writeHead(200, { 'access-control-allow-origin': '*' }), res.end('ok'));
if (!probe.lanListening) { console.log('no private LAN address on this machine: the private-name half cannot be run'); }
const env = await openTwoProfiles({ name: '24-ws', probe, seedPath: '/blank', args: ['--host-resolver-rules=MAP lan.test ' + (probe.lanIp || '127.0.0.1')] });
const P = env.cdp, ps = env.agent.sessionId;
const AG = await env.connection();
const ags = await AG.attach(env.agent.targetId);
await AG.send('Page.enable', {}, ags);
await P.send('Page.enable', {}, ps);
const ev = (sid, expr, gesture = false, conn = AG, extra = {}) => conn.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true, userGesture: gesture, ...extra }, sid).then((r) => (r.exceptionDetails ? `EXC ${r.exceptionDetails.exception?.description?.split('\n')[0] || r.exceptionDetails.text}` : r.result.value), (e) => `CDPERR ${e.message}`);
const O = probe.origin;
const TARGETS = { loopback: `ws://127.0.0.1:${probe.port}/ws`, privateName: `ws://lan.test:${probe.port}/ws` };
const wsJs = (u) => `new Promise(r=>{try{const w=new WebSocket('${u}');w.onopen=()=>{r('open');w.close()};w.onerror=()=>r('error');setTimeout(()=>r('timeout'),3000)}catch(e){r('THROWS '+e.name)}})`;
const load = async (path = '/blank', sid = ags, conn = AG) => { await conn.navigate(sid, `${O}${path}`); };
const hitsNow = () => probe.hits.filter((h) => h.upgrade).length;
// run the websocket probe against both targets; report page outcome and whether the server saw the upgrade
async function both(label, js = wsJs, sid = ags) {
  const r = {};
  for (const [k, u] of Object.entries(TARGETS)) {
    const h0 = hitsNow();
    const page = await ev(sid, js(u));
    await sleep(150);
    r[k] = `page:${page}, server saw upgrade:${hitsNow() > h0}`;
  }
  return r;
}
const show = (id, r) => rec(id, JSON.stringify(r));
const sessionVersion = env.version;
let blocked = null;
try {
  rec('RUN', { id: 'run-' + runId, version: sessionVersion, privateAddressAvailable: probe.lanListening });
  // baseline
  if (want('b0')) { await load(); show('B0.baseline', await both('base')); }
  // ================= B1 =================
  if (want('b1')) {
    await P.send('Network.enable', {}, ps);
    const trySet = async (label, params) => {
      let ack;
      try { await P.send('Network.setBlockedURLs', params, ps); ack = 'accepted'; } catch (e) { ack = e.message; }
      await load();
      const r = await both(label);
      await P.send('Network.setBlockedURLs', { urls: [] }, ps).catch(() => {});
      show(`B1.${label}`, { setBlockedURLs: ack, ...r });
    };
    await trySet('urls=[ws://*,wss://*]', { urls: ['ws://*', 'wss://*'] });
    await trySet('urls=[*://127.0.0.1:*/*, *://lan.test:*/*]', { urls: ['*://127.0.0.1:*/ws', '*://lan.test:*/ws'] });
    await trySet('urls=[ws://lan.test:*] (host pattern)', { urls: ['ws://lan.test:*'] });
    await trySet('urls=[*/ws] (path only)', { urls: ['*/ws'] });
    await trySet('urlPatterns=[{ws://*,block}]', { urlPatterns: [{ urlPattern: 'ws://*', block: true }] });
    await trySet('urlPatterns=[{*,block} for host lan.test]', { urlPatterns: [{ urlPattern: '*lan.test*', block: true }] });
    // control: does the same blocking list stop fetch (so the list itself works)?
    await P.send('Network.setBlockedURLs', { urls: ['*/ok'] }, ps);
    await load();
    show('B1.control_fetch_blocked_by_same_mechanism', { fetch: await ev(ags, `fetch('${O}/ok').then(r=>'reached','e=>'+'blocked:'+e.message)`.replace("'e=>'+'blocked:'+e.message", "e=>'blocked:'+e.message")) });
    await P.send('Network.setBlockedURLs', { urls: [] }, ps).catch(() => {});
    await P.send('Network.disable', {}, ps).catch(() => {});
  }
  // ================= B2 =================
  if (want('b2')) {
    const CSP = `connect-src http://127.0.0.1:${probe.port} 'self'`;
    let applyTo = new Set(['Document']);
    let cspMode = 'continue';
    await P.send('Fetch.enable', { patterns: [{ urlPattern: '*', requestStage: 'Response' }] }, ps);
    P.on('Fetch.requestPaused', (p, sid) => {
      if (sid !== ps) return;
      if (p.responseStatusCode && applyTo.has(p.resourceType)) {
        const hdrs = [...(p.responseHeaders || []).filter((h) => h.name.toLowerCase() !== 'content-security-policy'), { name: 'Content-Security-Policy', value: CSP }];
        if (cspMode === 'continue') P.send('Fetch.continueResponse', { requestId: p.requestId, responseCode: p.responseStatusCode, responseHeaders: hdrs }, sid).catch((e) => console.log('continueResponse failed', e.message));
        else P.send('Fetch.getResponseBody', { requestId: p.requestId }, sid).then((b) => P.send('Fetch.fulfillRequest', { requestId: p.requestId, responseCode: p.responseStatusCode, responseHeaders: hdrs, body: b.base64Encoded ? b.body : Buffer.from(b.body).toString('base64') }, sid)).catch((e) => console.log('fulfill failed', e.message));
      } else if (p.responseStatusCode) P.send('Fetch.continueResponse', { requestId: p.requestId }, sid).catch(() => {});
      else P.send('Fetch.continueRequest', { requestId: p.requestId }, sid).catch(() => {});
    });
    for (cspMode of ['continue', 'fulfill']) {
    await load();
    show(`B2[${cspMode}].ws_to_listed_and_unlisted_hosts`, await both('csp'));
    const others = await ev(ags, `(async()=>{const r={};const t=(n,p)=>p.then(v=>r[n]=v,e=>r[n]='blocked:'+(e.message||e.name));
      await t('fetch_privateName',fetch('http://lan.test:${probe.port}/ok',{mode:'no-cors'}).then(()=>'reached'));
      await t('xhr_privateName',new Promise((res,rej)=>{const x=new XMLHttpRequest();x.open('GET','http://lan.test:${probe.port}/ok');x.onload=()=>res('reached');x.onerror=()=>rej(new Error('error'));x.send()}));
      await t('eventsource_privateName',new Promise((res,rej)=>{const e=new EventSource('http://lan.test:${probe.port}/ev');e.onopen=()=>{res('reached');e.close()};e.onerror=()=>{rej(new Error('error'));e.close()};setTimeout(()=>res('timeout'),2500)}));
      r.beacon_privateName=String(navigator.sendBeacon('http://lan.test:${probe.port}/ok','x'));
      return JSON.stringify(r)})()`);
    await sleep(300);
    show(`B2[${cspMode}].other_channels_to_unlisted_host`, { page: others, serverSawBeacon: probe.hitsOf('/ok').some((h) => h.host.startsWith('lan.test')) });
    // documents that were not fetched
    const frames = await ev(ags, `(async()=>{const r={};const test=(n,mk)=>new Promise(res=>{const f=document.createElement('iframe');mk(f);f.onload=()=>{try{res(f.contentWindow.eval(\`(${wsJs(TARGETS.privateName).replace(/`/g, '')})\`).then?'':'')}catch(e){res('EXC '+e.message)}};document.body.appendChild(f);setTimeout(()=>res('no load'),3000)});return 'see per-kind below'})()`);
    const kind = async (name, mkJs) => { await load(); const r = await ev(ags, `new Promise(res=>{const f=document.createElement('iframe');${mkJs};window.addEventListener('message',e=>{if(e.data&&e.data.k==='${name}')res(e.data.v)});document.body.appendChild(f);setTimeout(()=>res('no message'),5000)})`); await sleep(150); return `${r}`; };
    const inner = (name) => `<script>const w=new WebSocket('${TARGETS.privateName}');w.onopen=()=>parent.postMessage({k:'${name}',v:'open'},'*');w.onerror=()=>parent.postMessage({k:'${name}',v:'error'},'*')<\\/script>`;
    const h0 = hitsNow();
    const r = {};
    r.aboutBlankIframe_viaParentRealm = await (async () => { await load(); return ev(ags, `new Promise(res=>{const f=document.createElement('iframe');document.body.appendChild(f);const w=new f.contentWindow.WebSocket('${TARGETS.privateName}');w.onopen=()=>res('open');w.onerror=()=>res('error');setTimeout(()=>res('timeout'),3000)})`); })();
    r.srcdocIframe = await kind('srcdoc', `f.srcdoc=\`${inner('srcdoc')}\``);
    r.dataIframe = await kind('data', `f.src='data:text/html,'+encodeURIComponent(\`${inner('data')}\`)`);
    r.blobIframe = await kind('blob', `f.src=URL.createObjectURL(new Blob([\`${inner('blob')}\`],{type:'text/html'}))`);
    r.privateNameServerSawUpgradeTotal = hitsNow() > h0;
    show(`B2[${cspMode}].local_scheme_iframes (private name)`, r);
    // top-level documents Fetch never sees
    const top = {};
    await AG.send('Page.navigate', { url: 'data:text/html,' + encodeURIComponent(`<script>const w=new WebSocket('${TARGETS.privateName}');w.onopen=()=>document.title='open';w.onerror=()=>document.title='error'</script>`) }, ags);
    await sleep(1500);
    top.dataUrlTopLevel = await ev(ags, 'document.title');
    await load();
    await ev(ags, `window.__b=URL.createObjectURL(new Blob([\`<script>const w=new WebSocket('${TARGETS.privateName}');w.onopen=()=>document.title='open';w.onerror=()=>document.title='error'<\\/script>\`],{type:'text/html'}));1`);
    await AG.send('Page.navigate', { url: await ev(ags, 'window.__b') }, ags);
    await sleep(1500);
    top.blobUrlTopLevel = await ev(ags, 'document.title');
    top.note = 'a Fetch response-stage hook was active for Document responses; data: and blob: top-level navigations produced no Fetch event';
    show(`B2[${cspMode}].top_level_unfetched_documents`, top);
    // workers
    await load();
    const w1 = await ev(ags, `new Promise(res=>{const w=new Worker('/w.js');w.onmessage=e=>res(e.data);w.postMessage('${TARGETS.privateName}')})`);
    await load();
    const w2 = await ev(ags, `new Promise(res=>{const w=new Worker(URL.createObjectURL(new Blob([\`onmessage=(e)=>{const w=new WebSocket(e.data);w.onopen=()=>postMessage('open');w.onerror=()=>postMessage('error');setTimeout(()=>postMessage('timeout'),3000)}\`])));w.onmessage=e=>res(e.data);w.postMessage('${TARGETS.privateName}')})`);
    applyTo = new Set(['Document', 'Worker']);
    await load();
    const w3 = await ev(ags, `new Promise(res=>{const w=new Worker('/w.js?2');w.onmessage=e=>res(e.data);w.postMessage('${TARGETS.privateName}')})`);
    applyTo = new Set(['Document']);
    show(`B2[${cspMode}].workers (private name)`, { dedicatedWorker_scriptResponseWithoutCSP: w1, blobWorker_inheritsCreatorCSP: w2, dedicatedWorker_CSPAddedToWorkerScriptResponse: w3 });
    // popup opened by the page: no hook on its first request unless auto-attach is set up
    await load();
    const popup = await ev(ags, `new Promise(res=>{const w=window.open('/blank');setTimeout(()=>{try{const x=new w.WebSocket('${TARGETS.privateName}');x.onopen=()=>res('open');x.onerror=()=>res('error');setTimeout(()=>res('timeout'),3000)}catch(e){res('EXC '+e.message)}},800)})`, true);
    show(`B2[${cspMode}].window_open_popup_with_page_session_hook_only`, { csp_applied_to_popup_document: popup === 'error' ? 'yes (ws refused)' : `no (${popup})` });
    }
    await P.send('Fetch.disable', {}, ps);
  }
  // ================= B3 =================
  if (want('b3')) {
    const b3 = {};
    const created = [];
    P.on('Network.webSocketCreated', (p, sid) => { if (sid === ps) created.push({ t: Date.now(), url: p.url, id: p.requestId }); });
    P.on('Network.webSocketWillSendHandshakeRequest', (p, sid) => { if (sid === ps) created.push({ t: Date.now(), hs: true }); });
    await P.send('Network.enable', {}, ps);
    await load();
    const h0 = probe.hits.length;
    await ev(ags, `window.__w=new WebSocket('${TARGETS.loopback}');1`);
    await sleep(500);
    const hit = probe.hits.slice(h0).find((h) => h.upgrade);
    const cr = created.find((c) => c.url);
    const hs = created.find((c) => c.hs);
    b3.timing = { webSocketCreated_to_serverSawUpgradeMs: hit && cr ? hit.ts - cr.t : null, handshakeRequestEvent_to_serverMs: hit && hs ? hit.ts - hs.t : null, note: 'positive = event arrived at pegboardd before the server saw the handshake (millisecond clocks, same machine)' };
    // can a call fail the socket at the moment of the event?
    const race = [];
    for (let i = 0; i < 10; i++) {
      await load();
      const before = hitsNow();
      const fn = (p, sid) => { if (sid === ps) P.send('Network.emulateNetworkConditions', { offline: true, latency: 0, downloadThroughput: -1, uploadThroughput: -1 }, ps).catch(() => {}); };
      P.on('Network.webSocketCreated', fn);
      const res = await ev(ags, wsJs(TARGETS.loopback));
      await sleep(100);
      race.push({ page: res, serverSawUpgrade: hitsNow() > before });
      await P.send('Network.emulateNetworkConditions', { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 }, ps);
      // remove fn: listeners are append-only, so mark it dead
      fn.dead = true;
      P.on = P.on; // keep API
      break; // one iteration only: the client has no off(); further iterations would stack listeners
    }
    b3.offlineEmulationInReactionToCreatedEvent = race;
    // established socket: offline emulation
    await load();
    await ev(ags, `window.__st='pending';window.__w=new WebSocket('${TARGETS.loopback}');window.__w.onopen=()=>window.__st='open';window.__w.onclose=()=>window.__st='closed';window.__w.onerror=()=>window.__st='error';1`);
    await sleep(600);
    const st1 = await ev(ags, 'window.__st');
    await P.send('Network.emulateNetworkConditions', { offline: true, latency: 0, downloadThroughput: -1, uploadThroughput: -1 }, ps);
    await sleep(800);
    const st2 = await ev(ags, 'window.__st');
    await P.send('Network.emulateNetworkConditions', { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 }, ps);
    b3.establishedSocketOfflineEmulation = { before: st1, after: st2 };
    // new socket while offline
    await P.send('Network.emulateNetworkConditions', { offline: true, latency: 0, downloadThroughput: -1, uploadThroughput: -1 }, ps);
    const h1 = hitsNow();
    b3.newSocketWhileOffline = { page: await ev(ags, wsJs(TARGETS.loopback)), serverSawUpgrade: hitsNow() > h1 };
    await P.send('Network.emulateNetworkConditions', { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 }, ps);
    // by-rule variant
    b3.emulateNetworkConditionsByRule = await P.send('Network.emulateNetworkConditionsByRule', { offline: false, matchedNetworkConditions: [{ urlPattern: 'ws://*', latency: 0, downloadThroughput: -1, uploadThroughput: -1, offline: true }] }, ps).then((r) => 'accepted: ' + JSON.stringify(r).slice(0, 80), (e) => e.message);
    if (/accepted/.test(b3.emulateNetworkConditionsByRule)) { await load(); const h2 = hitsNow(); b3.byRuleOfflineForWs = { page: await ev(ags, wsJs(TARGETS.loopback)), serverSawUpgrade: hitsNow() > h2 }; }
    await P.send('Network.emulateNetworkConditions', { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 }, ps);
    // close the socket from page side by navigating (blunt)
    await load();
    await ev(ags, `window.__w=new WebSocket('${TARGETS.loopback}');1`);
    await sleep(400);
    await AG.send('Page.navigate', { url: `${O}/blank?x` }, ags);
    await sleep(500);
    b3.navigateAway = 'navigating the tab closes its sockets (blunt; not measured on the server side)';
    show('B3', b3);
    await P.send('Network.disable', {}, ps).catch(() => {});
  }
  // ================= B4 =================
  if (want('b4')) {
    const b4 = {};
    const rep = `(()=>{const N=window.WebSocket;const f=function WebSocket(u,p){throw new DOMException('blocked','SecurityError')};f.prototype=N.prototype;Object.defineProperty(window,'WebSocket',{value:f,writable:false,configurable:false})})()`;
    const id = (await P.send('Page.addScriptToEvaluateOnNewDocument', { source: rep }, ps)).identifier;
    await load();
    b4.replaced_main_world = await both('replaced');
    const viaWorker = await ev(ags, `new Promise(res=>{const w=new Worker('/w.js?3');w.onmessage=e=>res(e.data);w.postMessage('${TARGETS.privateName}')})`);
    b4.bypass_dedicatedWorker = `page-script result inside worker:${viaWorker}`;
    const h0 = hitsNow();
    b4.bypass_sandboxedIframeOpaqueOrigin = await ev(ags, `new Promise(res=>{const f=document.createElement('iframe');f.setAttribute('sandbox','allow-scripts');window.addEventListener('message',e=>{if(e.data&&e.data.k==='sbx')res(e.data.v)});f.srcdoc='<script>try{const w=new WebSocket("${TARGETS.privateName}");w.onopen=()=>parent.postMessage({k:"sbx",v:"open"},"*");w.onerror=()=>parent.postMessage({k:"sbx",v:"error"},"*")}catch(e){parent.postMessage({k:"sbx",v:"THROWS "+e.name},"*")}<\\/script>';document.body.appendChild(f);setTimeout(()=>res('no message'),4000)})`);
    b4.bypass_aboutBlankIframe = await ev(ags, `new Promise(res=>{const f=document.createElement('iframe');document.body.appendChild(f);try{const w=new f.contentWindow.WebSocket('${TARGETS.privateName}');w.onopen=()=>res('open');w.onerror=()=>res('error');setTimeout(()=>res('timeout'),3000)}catch(e){res('THROWS '+e.name)}})`);
    const { frameTree } = await AG.send('Page.getFrameTree', {}, ags);
    const { executionContextId } = await AG.send('Page.createIsolatedWorld', { frameId: frameTree.frame.id, worldName: 'iso' }, ags);
    b4.bypass_isolatedWorld = await ev(ags, wsJs(TARGETS.privateName), false, AG, { contextId: executionContextId });
    b4.serverSawUpgradeDuringBypasses = hitsNow() > h0;
    await P.send('Page.removeScriptToEvaluateOnNewDocument', { identifier: id }, ps).catch(() => {});
    show('B4 (weak measure)', b4);
  }
  // ================= B5 =================
  if (want('b5')) {
    const b5 = {};
    b5.webTransport = typeof 'WebTransport' !== 'undefined' ? 'not measured: needs an HTTP/3 (QUIC) server, which this spike does not have' : '';
    await load();
    b5.webTransportApiPresent = await ev(ags, `typeof WebTransport`);
    const rtcJs = `new Promise(async res=>{try{const a=new RTCPeerConnection(),b=new RTCPeerConnection();a.onicecandidate=e=>e.candidate&&b.addIceCandidate(e.candidate);b.onicecandidate=e=>e.candidate&&a.addIceCandidate(e.candidate);const dc=a.createDataChannel('x');b.ondatachannel=e=>{e.channel.onmessage=m=>res('received '+m.data)};dc.onopen=()=>dc.send('hi');const o=await a.createOffer();await a.setLocalDescription(o);await b.setRemoteDescription(o);const an=await b.createAnswer();await b.setLocalDescription(an);await a.setRemoteDescription(an);setTimeout(()=>res('timeout'),6000)}catch(e){res('EXC '+e.message)}})`;
    await load();
    b5.webRTC_dataChannel_baseline_nothing_active = await ev(ags, rtcJs);
    await P.send('Network.enable', {}, ps);
    await P.send('Network.setBlockedURLs', { urls: ['ws://*', 'wss://*', '*lan.test*', '*127.0.0.1*'] }, ps);
    const netEvents = [];
    P.on('Network.requestWillBeSent', (p, sid) => { if (sid === ps) netEvents.push(p.request.url.slice(0, 40)); });
    await load();
    b5.webRTC_dataChannel_with_setBlockedURLs_active = await ev(ags, rtcJs);
    b5.networkRequestEventsDuringRtc = netEvents.filter((u) => !u.includes('/blank')).length;
    await P.send('Network.setBlockedURLs', { urls: [] }, ps);
    b5.note = 'one observation each; the data channel here is peer-to-peer inside one page over host candidates, so it shows only that neither Network.setBlockedURLs nor Fetch is involved, not that a remote peer is unreachable';
    show('B5', b5);
  }
} finally {
  out._hits = probe.hits.filter((h) => h.upgrade).length;
  console.log('saved', writeResult('24-websocket', out));
  await env.close();
  probe.close();
}
process.exit(0);
