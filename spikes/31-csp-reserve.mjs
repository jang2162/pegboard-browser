// Spike 31 (round 3, item 6): why do connections that a re-served (fulfillRequest) CSP allows hang? Isolate fulfill vs CSP.
import { randomBytes } from 'node:crypto';
import { writeResult, sleep } from './lib/launch.mjs';
import { startProbe, html, openTwoProfiles, reporter } from './lib/probe.mjs';

const out = {};
const rec = reporter('31', out);
const runId = randomBytes(4).toString('hex');
const probe = await startProbe({ lan: true });
const R = probe.routes;
const docHeaders = { 'content-type': 'text/html; charset=utf-8', 'x-orig': '1' };
R['/doc'] = (_q, res) => (res.writeHead(200, docHeaders), res.end(html('<p>doc</p><script>window.__loaded=true</script>')));
R['/ok'] = (_q, res) => (res.writeHead(200, { 'access-control-allow-origin': '*', 'content-type': 'text/plain' }), res.end('ok'));
const env = await openTwoProfiles({ name: '31-csp', probe, seedPath: '/doc', args: ['--host-resolver-rules=MAP lan.test ' + (probe.lanIp || '127.0.0.1')] });
const P = env.cdp, ps = env.agent.sessionId;
const ev = (expr, to = 15000) => P.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true }, ps, to).then((r) => (r.exceptionDetails ? `EXC ${r.exceptionDetails.exception?.description?.split('\n')[0]}` : r.result.value), (e) => `CDPERR ${e.message}`);
const CSP_ALL = `connect-src *`;
const variants = {
  'V0 pass-through (continueResponse, nothing changed)': { mode: 'pass' },
  'V1 fulfill, original headers + original body, NO CSP': { mode: 'fulfill' },
  'V2 fulfill + original headers + CSP connect-src *': { mode: 'fulfill', csp: CSP_ALL },
  'V3 continueResponse + CSP connect-src * (CSP not applied)': { mode: 'continue', csp: CSP_ALL },
  'V4 fulfill + CSP connect-src * + response headers rebuilt with explicit content-length': { mode: 'fulfill', csp: CSP_ALL, contentLength: true },
  'V5 fulfill + CSP default-src * (instead of connect-src)': { mode: 'fulfill', csp: `default-src * 'unsafe-inline'` },
  'V6 fulfill with responseCode 200 and header set from scratch (content-type + CSP only)': { mode: 'fulfill', csp: CSP_ALL, scratch: true },
};
let cur = null;
const info = {};
await P.send('Fetch.enable', { patterns: [{ urlPattern: '*/doc*', requestStage: 'Response' }] }, ps);
P.on('Fetch.requestPaused', async (p, sid) => {
  if (sid !== ps) return;
  if (!p.responseStatusCode) return P.send('Fetch.continueRequest', { requestId: p.requestId }, sid).catch(() => {});
  const v = variants[cur]; const hdrs = (p.responseHeaders || []);
  info.origHeaders = hdrs.map((h) => h.name.toLowerCase()).join(',');
  const withCsp = (h) => (v.csp ? [...h.filter((x) => x.name.toLowerCase() !== 'content-security-policy'), { name: 'Content-Security-Policy', value: v.csp }] : h);
  if (v.mode === 'pass') return P.send('Fetch.continueResponse', { requestId: p.requestId }, sid);
  if (v.mode === 'continue') return P.send('Fetch.continueResponse', { requestId: p.requestId, responseCode: p.responseStatusCode, responseHeaders: withCsp(hdrs) }, sid);
  const b = await P.send('Fetch.getResponseBody', { requestId: p.requestId }, sid);
  const body64 = b.base64Encoded ? b.body : Buffer.from(b.body).toString('base64');
  info.bodyBytes = Buffer.from(body64, 'base64').length;
  let h = v.scratch ? withCsp([{ name: 'content-type', value: 'text/html; charset=utf-8' }]) : withCsp(hdrs);
  if (v.contentLength) h = [...h.filter((x) => x.name.toLowerCase() !== 'content-length'), { name: 'content-length', value: String(info.bodyBytes) }];
  await P.send('Fetch.fulfillRequest', { requestId: p.requestId, responseCode: p.responseStatusCode, responseHeaders: h, body: body64 }, sid);
});
const probeAll = async () => {
  const r = {};
  r.readyState = await ev('document.readyState');
  r.loadedFlag = await ev('window.__loaded===true');
  r.bodyText = await ev('document.body&&document.body.innerText');
  const h0 = probe.hits.length;
  r.ws_allowed_loopback = await ev(`new Promise(r=>{const w=new WebSocket('ws://127.0.0.1:${probe.port}/ws');const t0=performance.now();w.onopen=()=>{r('open '+Math.round(performance.now()-t0)+'ms');w.close()};w.onerror=()=>r('error');setTimeout(()=>r('TIMEOUT 4s'),4000)})`);
  r.serverSawUpgrade = probe.hits.slice(h0).some((h) => h.upgrade);
  r.fetch_allowed_loopback = await ev(`Promise.race([fetch('http://127.0.0.1:${probe.port}/ok?v=${Math.random()}').then(r=>r.text()),new Promise(r=>setTimeout(()=>r('TIMEOUT 4s'),4000))])`);
  r.fetch_allowed_privateName = await ev(`Promise.race([fetch('http://lan.test:${probe.port}/ok?v=${Math.random()}').then(r=>r.text(),e=>'failed '+e.message),new Promise(r=>setTimeout(()=>r('TIMEOUT 4s'),4000))])`);
  return r;
};
try {
  for (const name of Object.keys(variants)) {
    const runs = [];
    for (let i = 0; i < 3; i++) {
      cur = name; info.bodyBytes = null;
      await P.navigate(ps, `${probe.origin}/doc?r=${i}`).catch(() => {});
      await sleep(400);
      const r = await probeAll();
      r.bodyBytesReserved = info.bodyBytes; r.originalHeaderNames = info.origHeaders;
      runs.push(r);
    }
    rec(name, runs.map((r) => ({ ws: r.ws_allowed_loopback, serverSawUpgrade: r.serverSawUpgrade, fetch: r.fetch_allowed_loopback, fetchPrivateName: r.fetch_allowed_privateName, readyState: r.readyState, scriptRan: r.loadedFlag, bodyBytes: r.bodyBytesReserved })));
    out[name] = runs;
  }
  // hypothesis from the pattern (only fulfilled documents hang; V0 and V3 do not): a fulfilled response has no real remote address,
  // so Chrome treats the document as non-local and its loopback/private requests wait for the local-network permission.
  // Fix measured: grant that permission to the origin from the pegboard side, then re-run the CSP variants with an allow list.
  const S = await env.connection();
  const grants = {};
  for (const n of ['local-network-access', 'local-network', 'loopback-network']) grants[n] = await S.send('Browser.setPermission', { permission: { name: n }, setting: 'granted', origin: probe.origin, browserContextId: env.agentCtx }).then(() => 'accepted', (e) => e.message.slice(0, 60));
  rec('FIX.setPermission', grants);
  variants['FIX-V1 fulfill, NO CSP, permission granted'] = { mode: 'fulfill' };
  variants['FIX-V2 fulfill + connect-src * , permission granted'] = { mode: 'fulfill', csp: CSP_ALL };
  variants['FIX-V7 fulfill + CSP allow list (loopback host only, http+ws), permission granted'] = { mode: 'fulfill', csp: `connect-src http://127.0.0.1:${probe.port} ws://127.0.0.1:${probe.port}` };
  for (const name of Object.keys(variants).filter((n) => n.startsWith('FIX-'))) {
    const runs = [];
    for (let i = 0; i < 3; i++) {
      cur = name; info.bodyBytes = null;
      await P.navigate(ps, `${probe.origin}/doc?r=${i}`).catch(() => {});
      await sleep(400);
      const r = await probeAll(); r.bodyBytesReserved = info.bodyBytes; runs.push(r);
    }
    rec(name, runs.map((r) => ({ ws_allowed_loopback: r.ws_allowed_loopback, serverSawUpgrade: r.serverSawUpgrade, fetch_allowed_loopback: r.fetch_allowed_loopback, fetch_privateName: r.fetch_allowed_privateName })));
    out[name] = runs;
  }
  S.close();
  out.originalHeaderNames = info.origHeaders;
  rec('headers', info.origHeaders);
} finally {
  console.log('saved', writeResult('31-csp-reserve', out));
  await env.close(); probe.close();
}
process.exit(0);
