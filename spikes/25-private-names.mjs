// Spike 25 (round 2, C1-C3): a name that resolves to a private address; Chrome's own protections; IPv6 / mapped / HTTPS forms.
import { randomBytes } from 'node:crypto';
import dns from 'node:dns/promises';
import { createServer } from 'node:http';
import { createServer as createHttps } from 'node:https';
import { spawnSync } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { writeResult, sleep, OUT, onCleanup } from './lib/launch.mjs';
import { startProbe, html, openTwoProfiles, reporter, parseArgs, stat } from './lib/probe.mjs';
import { readFileSync } from 'node:fs';

const out = {};
const rec = reporter('25', out);
const runId = randomBytes(4).toString('hex');
const only = parseArgs().only;
const want = (k) => !only || only.includes(k);
const pub = await startProbe();            // the "public" origin server (address space overridden by a Chrome switch in C2)
const tgt = await startProbe({ lan: true }); // the target: loopback and private-LAN listeners
const seen = [];
const okRoute = (_q, res, url) => (res.writeHead(200, { 'access-control-allow-origin': '*', 'access-control-allow-headers': '*', 'access-control-allow-private-network': 'true', 'access-control-allow-local-network': 'true' }), res.end('ok'));
for (const pr of [pub, tgt]) { pr.routes['/ok'] = okRoute; pr.routes['/blank'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html('<p>b</p>'))); }
// record preflight headers on the target
const origHandler = tgt.hits;
const dnsPriv = (a) => /^(127\.|10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.|169\.254\.|0\.0\.0\.0)/.test(a) || a === '::1' || /^f[cd]/i.test(a) || /^fe[89ab]/i.test(a) || /^::ffff:(127|10|192\.168|172\.(1[6-9]|2\d|3[01])|169\.254)\./i.test(a);
const privateHost = (h) => /^\[(.*)\]$/.test(h) ? dnsPriv(h.slice(1, -1)) : dnsPriv(h);
const launchEnv = async (name, extraArgs = []) => openTwoProfiles({ name, probe: tgt, seedPath: '/blank', args: [`--host-resolver-rules=MAP loop.test 127.0.0.1, MAP lan.test ${tgt.lanIp || '127.0.0.1'}, MAP pub.test 127.0.0.1`, ...extraArgs] });
const mkev = (conn) => (sid, expr, gesture = false) => conn.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true, userGesture: gesture }, sid).then((r) => (r.exceptionDetails ? `EXC ${r.exceptionDetails.exception?.description?.split('\n')[0] || r.exceptionDetails.text}` : r.result.value), (e) => `CDPERR ${e.message}`);

try {
  rec('RUN', { id: 'run-' + runId, privateAddressAvailable: tgt.lanListening });
  // ================= C1 =================
  if (want('c1')) {
    const env = await launchEnv('25-c1');
    const P = env.cdp, ps = env.agent.sessionId; const ev = mkev(P);
    await P.send('Page.enable', {}, ps);
    await P.navigate(ps, `${tgt.origin}/blank`);
    const N = 100;
    const timeFetches = async (label) => {
      const r = await ev(ps, `(async()=>{const a=[];for(let i=0;i<${N};i++){const t=performance.now();await fetch('${tgt.origin}/ok?i='+i+'&r='+Math.random(),{cache:'no-store'}).then(r=>r.text());a.push(performance.now()-t)}return JSON.stringify(a)})()`);
      return JSON.parse(r);
    };
    const res = {};
    res.noHook = stat(await timeFetches('no hook'));
    const lookups = [];
    let lookupMode = 'none';
    await P.send('Fetch.enable', { patterns: [{ urlPattern: '*', requestStage: 'Request' }] }, ps);
    P.on('Fetch.requestPaused', async (p, sid) => {
      if (sid !== ps) return;
      let fail = false;
      if (lookupMode !== 'none' && p.request.url.includes('/ok?i=')) {
        const name = lookupMode === 'cached' ? 'localhost' : `u${lookups.length}-${runId}.localhost`;
        const t = process.hrtime.bigint();
        const a = await dns.lookup(name, { all: true }).then((r) => r.map((x) => x.address), (e) => e.code);
        lookups.push({ ms: Number(process.hrtime.bigint() - t) / 1e6, result: Array.isArray(a) ? a.length + ' addr' : a });
        fail = Array.isArray(a) && a.some(dnsPriv) && p.request.url.includes('BLOCKME');
      }
      (fail ? P.send('Fetch.failRequest', { requestId: p.requestId, errorReason: 'AccessDenied' }, sid) : P.send('Fetch.continueRequest', { requestId: p.requestId }, sid)).catch(() => {});
    });
    lookupMode = 'none'; res.hookContinueOnly = stat(await timeFetches('hook'));
    lookupMode = 'cached'; lookups.length = 0; res.hookPlusLookupCachedName_page = stat(await timeFetches('cached')); res.lookupCachedName_nodeMs = stat(lookups.map((l) => l.ms)); res.lookupCachedName_results = [...new Set(lookups.map((l) => l.result))];
    lookupMode = 'uncached'; lookups.length = 0; res.hookPlusLookupUnique_page = stat(await timeFetches('uncached')); res.lookupUnique_nodeMs = stat(lookups.map((l) => l.ms)); res.lookupUnique_results = [...new Set(lookups.map((l) => l.result))];
    show('C1.latency_100_requests_each', res);
    // does failRequest at the request stage stop a hostname that the daemon resolves to a private address?
    lookupMode = 'none';
    const blocked = await ev(ps, `fetch('${tgt.origin}/ok?i=BLOCKME',{cache:'no-store'}).then(()=>'reached',e=>'failed '+e.message)`);
    show('C1.mechanism_check_failRequest_on_loopback_name', { note: 'lookup of "localhost" returns loopback; request failed by hook when marked', pageSaw: await (async () => { lookupMode = 'cached'; const r = await ev(ps, `fetch('${tgt.origin}/ok?i=BLOCKME',{cache:'no-store'}).then(()=>'reached',e=>'failed '+e.message)`); lookupMode = 'none'; return r; })(), withoutLookup: blocked });
    // the residual gap, measured: the name Chrome was told to map vs what the system resolver says
    const sys = {};
    for (const n of ['loop.test', 'lan.test', 'pub.test']) sys[n] = await dns.lookup(n, { all: true }).then((r) => 'resolves to ' + r.length + ' address(es)', (e) => e.code);
    const chromeSees = await ev(ps, `fetch('http://loop.test:${tgt.port}/ok?i=gap',{cache:'no-store'}).then(()=>'reached',e=>'failed '+e.message)`);
    show('C1.gap_system_resolver_vs_chrome', { systemResolverFor_names_Chrome_maps_via_host_resolver_rules: sys, chromeFetchToLoop_test: chromeSees, note: 'Chrome connected where the system resolver has no answer, so a daemon-side lookup would see NXDOMAIN for a name Chrome resolves to loopback here (flag-based mapping is test-only; DoH, a second lookup returning another address and rebinding were not measured)' });
    env.cdp.close(); await env.close();
  }
  // ================= C2 =================
  if (want('c2')) {
    const SPACE = `--ip-address-space-overrides=127.0.0.1:${pub.port}=public`;
    const configs = [
      ['default flags + address-space override only', [SPACE]],
      ['+ --enable-features=LocalNetworkAccessChecks', [SPACE, '--enable-features=LocalNetworkAccessChecks']],
      ['+ --enable-features=LocalNetworkAccessChecks,LocalNetworkAccessWebSocketsTargetAddressSpace', [SPACE, '--enable-features=LocalNetworkAccessChecks,LocalNetworkAccessWebSocketsTargetAddressSpace']],
      ['+ --disable-features=LocalNetworkAccessChecks', [SPACE, '--disable-features=LocalNetworkAccessChecks']],
    ];
    const results = {};
    for (const [label, args] of configs) {
      const env = await launchEnv('25-c2', args);
      const P = env.cdp, ps = env.agent.sessionId; const ev = mkev(P);
      await P.send('Page.enable', {}, ps);
      // the "public" page: served by `pub` whose 127.0.0.1:port is classified as public by the override switch
      await P.navigate(ps, `http://127.0.0.1:${pub.port}/blank`);
      const t = { origin: 'public (by switch)' };
      const tryFetch = (u, opts = '') => ev(ps, `Promise.race([fetch('${u}',${opts || '{}'}).then(r=>r.text().then(x=>'reached '+x),e=>'failed '+e.message),new Promise(r=>setTimeout(()=>r('no answer in 5s (a prompt may be pending)'),5000))])`);
      const before = tgt.hits.length;
      const seenPre = [];
      t.fetch_loopback_simple = await tryFetch(`http://127.0.0.1:${tgt.port}/ok?c2=1`);
      t.fetch_loopback_customHeader_preflight = await tryFetch(`http://127.0.0.1:${tgt.port}/ok?c2=2`, `{headers:{'x-a':'1'}}`);
      t.fetch_loopback_noCors = await tryFetch(`http://127.0.0.1:${tgt.port}/ok?c2=3`, `{mode:'no-cors'}`);
      t.fetch_privateName_simple = await tryFetch(`http://lan.test:${tgt.port}/ok?c2=4`);
      t.img_loopback = await ev(ps, `new Promise(r=>{const i=new Image();i.onload=()=>r('loaded');i.onerror=()=>r('error');i.src='http://127.0.0.1:${tgt.port}/ok?c2=5';setTimeout(()=>r('timeout'),4000)})`);
      t.ws_loopback = await ev(ps, `new Promise(r=>{try{const w=new WebSocket('ws://127.0.0.1:${tgt.port}/ws?c2=6');w.onopen=()=>{r('open');w.close()};w.onerror=()=>r('error');setTimeout(()=>r('timeout'),4000)}catch(e){r('throws '+e.name)}})`);
      await sleep(300);
      t.serverSaw = tgt.hits.slice(before).filter((h) => h.search.includes('c2=')).map((h) => h.search.replace('?', '') + (h.upgrade ? '(ws)' : ''));
      // control: the same fetches from a loopback page (not overridden)
      if (label.startsWith('default')) await P.navigate(ps, `http://127.0.0.1:${pub.port}/blank`).catch(() => {});
      await P.navigate(ps, `${tgt.origin}/blank`);
      t.control_loopbackPage_fetch_loopback = await tryFetch(`http://127.0.0.1:${tgt.port}/ok?c2=7`);
      if (label.startsWith('default')) {
        const S = await env.connection();
        const names = ['local-network-access', 'local-network', 'loopback-network', 'local-network-access-loopback', 'private-network-access'];
        t.setPermissionNames = {};
        for (const n of names) t.setPermissionNames[n] = await S.send('Browser.setPermission', { permission: { name: n }, setting: 'granted', origin: `http://127.0.0.1:${pub.port}`, browserContextId: env.agentCtx }).then(() => 'accepted', (e) => e.message.slice(0, 80));
        await P.navigate(ps, `http://127.0.0.1:${pub.port}/blank`);
        t.afterGrantAttempt_fetch_loopback = await tryFetch(`http://127.0.0.1:${tgt.port}/ok?c2=8`);
        S.close();
      }
      results[label] = t;
      show(`C2.[${label}]`, t);
      await env.close();
    }
    out.C2 = results;
  }
  // ================= C3 =================
  if (want('c3')) {
    // listeners: IPv6 loopback and HTTPS (self-signed) on loopback
    const v6 = createServer((req, res) => { res.writeHead(200, { 'access-control-allow-origin': '*' }); res.end('v6-ok'); });
    const v6ok = await new Promise((r) => { v6.on('error', () => r(false)); v6.listen(0, '::1', () => r(true)); });
    const dir = join(OUT, '25-certs'); mkdirSync(dir, { recursive: true });
    const gen = spawnSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', join(dir, 'k.pem'), '-out', join(dir, 'c.pem'), '-days', '1', '-subj', '/CN=localhost'], { encoding: 'utf8' });
    let https = null;
    if (gen.status === 0) { https = createHttps({ key: readFileSync(join(dir, 'k.pem')), cert: readFileSync(join(dir, 'c.pem')) }, (req, res) => { res.writeHead(200, { 'access-control-allow-origin': '*' }); res.end('https-ok'); }); await new Promise((r) => https.listen(0, '127.0.0.1', r)); }
    const env = await launchEnv('25-c3', ['--ignore-certificate-errors']);
    const P = env.cdp, ps = env.agent.sessionId; const ev = mkev(P);
    await P.send('Page.enable', {}, ps);
    await P.navigate(ps, `${tgt.origin}/blank`);
    const pausedSeen = [];
    await P.send('Fetch.enable', { patterns: [{ urlPattern: '*', requestStage: 'Request' }] }, ps);
    P.on('Fetch.requestPaused', (p, sid) => { if (sid === ps) { const u = new URL(p.request.url); if (u.search.includes('c3=')) pausedSeen.push({ tag: u.search, scheme: u.protocol, hostname: u.hostname, privateByRules: privateHost(u.hostname) }); } P.send('Fetch.continueRequest', { requestId: p.requestId }, sid).catch(() => {}); });
    const c3 = {};
    const tryF = async (name, url) => { const r = await ev(ps, `fetch('${url}',{mode:'no-cors',cache:'no-store'}).then(()=>'reached',e=>'failed '+e.message)`); c3[name] = r; };
    await tryF('ipv4_loopback', `http://127.0.0.1:${tgt.port}/ok?c3=a`);
    if (v6ok) { await tryF('ipv6_loopback_literal', `http://[::1]:${v6.address().port}/ok?c3=b`); await tryF('ipv4_mapped_ipv6_literal_dotted', `http://[::ffff:127.0.0.1]:${tgt.port}/ok?c3=c`); await tryF('ipv4_mapped_ipv6_literal_hex', `http://[::ffff:7f00:1]:${tgt.port}/ok?c3=d`); }
    await tryF('decimal_ipv4_literal', `http://2130706433:${tgt.port}/ok?c3=e`);
    await tryF('octal_hex_ipv4_literal', `http://0x7f.0.0.1:${tgt.port}/ok?c3=f`);
    await tryF('short_ipv4_literal', `http://127.1:${tgt.port}/ok?c3=g`);
    await tryF('zero_address', `http://0.0.0.0:${tgt.port}/ok?c3=h`);
    if (https) await tryF('https_loopback_self_signed', `https://127.0.0.1:${https.address().port}/ok?c3=i`);
    await tryF('trailing_dot_name', `http://loop.test.:${tgt.port}/ok?c3=j`);
    await sleep(300);
    c3.pausedAtRequestStage = pausedSeen;
    c3.note = 'privateByRules = a literal-address check applied to URL.hostname as the daemon would see it';
    show('C3', c3);
    await env.close(); v6.close(); https?.close();
  }
} finally {
  console.log('saved', writeResult('25-private-names', out));
  pub.close(); tgt.close();
}
function show(id, v) { rec(id, JSON.stringify(v)); }
process.exit(0);
