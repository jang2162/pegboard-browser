// Spike 02 (A2 A3 A8 A9): a minimal session-scoped CDP proxy in front of the browser, driven by agent-browser.
import { createServer } from 'node:http';
import { execSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import WebSocket, { WebSocketServer } from 'ws';
import { launchChrome, sleep, writeResult, OUT } from './lib/launch.mjs';
import { connect } from './lib/cdp.mjs';
import { startFixture } from './lib/fixture.mjs';
import { ab } from './lib/ab.mjs';

const GATED = new Set(['Target.attachToTarget', 'Target.closeTarget', 'Target.activateTarget', 'Target.getTargetInfo']);
const TARGET_EVENTS = new Set(['Target.targetCreated', 'Target.targetInfoChanged', 'Target.targetDestroyed', 'Target.attachedToTarget']);

function startProxy({ upstreamWsUrl, token, allowedTargetIds }) {
  const log = [];
  const httpRequests = [];
  const rejected = [];
  const server = createServer((req, res) => {
    httpRequests.push(`${req.method} ${req.url}`);
    res.writeHead(404);
    res.end();
  });
  const wss = new WebSocketServer({ noServer: true, perMessageDeflate: false, maxPayload: 512 * 1024 * 1024 });
  server.on('upgrade', (req, socket, head) => {
    if (req.headers.authorization !== `Bearer ${token}` || !req.url.startsWith('/s/test')) {
      rejected.push(`${req.url} auth=${req.headers.authorization ? 'wrong' : 'none'}`);
      socket.write('HTTP/1.1 401 Unauthorized\r\n\r\n');
      socket.destroy();
      return;
    }
    wss.handleUpgrade(req, socket, head, (client) => {
      const upstream = new WebSocket(upstreamWsUrl, { perMessageDeflate: false, maxPayload: 512 * 1024 * 1024 });
      const methodById = new Map();
      const queue = [];
      upstream.on('open', () => queue.splice(0).forEach((m) => upstream.send(m)));
      const toUpstream = (m) => (upstream.readyState === WebSocket.OPEN ? upstream.send(m) : queue.push(m));

      client.on('message', (data) => {
        const msg = JSON.parse(data.toString());
        const entry = { dir: 'c2b', method: msg.method, sessionId: msg.sessionId, blocked: false };
        log.push(entry);
        if (msg.method === 'Browser.close') {
          entry.blocked = true;
          client.send(JSON.stringify({ id: msg.id, result: {} }));
          return;
        }
        if (GATED.has(msg.method) && msg.params?.targetId && !allowedTargetIds.has(msg.params.targetId)) {
          entry.blocked = true;
          client.send(JSON.stringify({ id: msg.id, error: { code: -32000, message: 'No target with given id found' } }));
          return;
        }
        methodById.set(msg.id, msg.method);
        toUpstream(data.toString());
      });

      upstream.on('message', (data) => {
        const msg = JSON.parse(data.toString());
        if (msg.id !== undefined) {
          const method = methodById.get(msg.id);
          methodById.delete(msg.id);
          if (method === 'Target.createTarget' && msg.result?.targetId) allowedTargetIds.add(msg.result.targetId);
          if (method === 'Target.getTargets' && msg.result?.targetInfos) {
            msg.result.targetInfos = msg.result.targetInfos.filter((t) => allowedTargetIds.has(t.targetId));
            client.send(JSON.stringify(msg));
            return;
          }
          client.send(data.toString());
          return;
        }
        if (TARGET_EVENTS.has(msg.method)) {
          const info = msg.params?.targetInfo;
          const id = info?.targetId || msg.params?.targetId;
          if (info?.openerId && allowedTargetIds.has(info.openerId)) allowedTargetIds.add(id);
          if (!allowedTargetIds.has(id)) {
            log.push({ dir: 'b2c', method: msg.method, blocked: true });
            return;
          }
        }
        client.send(data.toString());
      });
      const closeBoth = () => {
        try { client.close(); } catch {}
        try { upstream.close(); } catch {}
      };
      client.on('close', closeBoth);
      upstream.on('close', closeBoth);
      upstream.on('error', closeBoth);
    });
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      const port = server.address().port;
      resolve({ url: `ws://127.0.0.1:${port}/s/test`, port, log, httpRequests, rejected, close: () => server.close() });
    });
  });
}

const chromeCount = () => Number(execSync(`pgrep -f "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing" | wc -l`).toString().trim());
const strip = (r) => {
  const d = { ...(r.json?.data || {}) };
  delete d.lifecycle;
  return d;
};

const fixture = await startFixture();
const chrome = await launchChrome({ name: '02-proxy' });
const direct = await connect(chrome.browserWsUrl);
// Two "human" tabs that the agent session must never see.
const human1 = await direct.firstPage();
await direct.navigate(human1.sessionId, `${fixture.origin}/counter`);
const human2 = await direct.newPage(`${fixture.origin}/cookie/get`);
const humanIds = [human1.targetId, human2.targetId];

const token = 'spike-token-' + Math.random().toString(36).slice(2);
const allowed = new Set();
const proxy = await startProxy({ upstreamWsUrl: chrome.browserWsUrl, token, allowedTargetIds: allowed });
const session = `px${Date.now() % 1000000}`;
const via = { session, cdp: proxy.url, cdpHeaders: { Authorization: `Bearer ${token}` } };
const steps = [];

// Review Focus 1: every command must leave traffic in the proxy log, otherwise it did not go through the proxy.
async function step(n, label, args, opts = {}, check = () => true) {
  const before = proxy.log.length;
  const r = await ab(args, { ...via, ...opts });
  const data = strip(r);
  const row = { n, label, args: args.join(' '), ok: r.ok, ms: r.ms, viaProxy: proxy.log.length > before, data: JSON.stringify(data).slice(0, 400) };
  if (!r.ok) row.error = String(r.json?.error || r.stderr || r.stdout).slice(0, 300);
  try {
    row.check = await check(r, data);
  } catch (e) {
    row.check = `check threw: ${e.message}`;
  }
  steps.push(row);
  console.log(JSON.stringify(row));
  return r;
}
const humanTabsAlive = async () => {
  const { targetInfos } = await direct.send('Target.getTargets');
  return humanIds.every((id) => targetInfos.some((t) => t.targetId === id));
};
const pages = async () => (await direct.send('Target.getTargets')).targetInfos.filter((t) => t.type === 'page');

const pagesBefore = (await pages()).length;
await step(1, 'open creates own tab', ['open', `${fixture.origin}/counter`], {}, async () => ({
  pagesBefore,
  pagesAfter: (await pages()).length,
  humanUrlsUnchanged: (await pages()).filter((t) => humanIds.includes(t.targetId)).map((t) => t.url),
  allowed: [...allowed],
}));
await step(2, 'snapshot', ['snapshot', '-i'], {}, (r, d) => /button/.test(d.snapshot || ''));
const agentTarget = [...allowed][0];
const agentSid = agentTarget ? await direct.attach(agentTarget) : null;
await step(3, 'click', ['click', '#btn'], {}, async () => (agentSid ? await direct.evaluate(agentSid, `document.getElementById('clicks').textContent`) : 'no agent target'));
await step(4, 'open form', ['open', `${fixture.origin}/form`]);
await step(4.1, 'fill', ['fill', '#name', 'hello'], {}, async () => (agentSid ? await direct.evaluate(agentSid, `document.getElementById('name').value`) : null));
await step(5, 'get url', ['get', 'url'], {}, (r, d) => d.url);
await step(5.1, 'screenshot', ['screenshot', join(OUT, '02.png')]);
await step(6, 'tab list hides human tabs', ['tab', 'list'], {}, (r, d) => ({
  urls: (d.tabs || []).map((t) => t.url),
  humanVisible: (d.tabs || []).some((t) => humanIds.includes(t.targetId)),
}));
await step(7, 'tab new opener', ['tab', 'new', `${fixture.origin}/opener`]);
await step(7.1, 'click window.open', ['click', '#open']);
await sleep(1500);
await step(7.2, 'tab list shows popup', ['tab', 'list'], {}, (r, d) => ({ urls: (d.tabs || []).map((t) => t.url), allowedCount: allowed.size }));
await step(8, 'eval', ['eval', 'document.title'], {}, (r, d) => d.result);

// 9: wrong token must fail and must not make agent-browser launch its own browser.
{
  const before = chromeCount();
  const s2 = `bad${Date.now() % 1000000}`;
  const r = await ab(['snapshot', '-i'], { session: s2, cdp: proxy.url, cdpHeaders: { Authorization: 'Bearer wrong' }, timeoutMs: 30000 });
  await sleep(1500);
  const row = { n: 9, label: 'wrong token', ok: r.ok, chromeBefore: before, chromeAfter: chromeCount(), rejected: [...proxy.rejected], error: String(r.json?.error || r.stderr).slice(0, 300) };
  steps.push(row);
  console.log(JSON.stringify(row));
  await ab(['close'], { session: s2 });
}

// 12: the per-session stream server (A9).
const status1 = await step(12, 'stream status', ['stream', 'status']);
const streamPort = status1.json?.data?.port;
if (streamPort) {
  const listen = execSync(`lsof -nP -iTCP:${streamPort} -sTCP:LISTEN || true`).toString().trim().split('\n').slice(-1)[0];
  const unauth = await new Promise((resolve) => {
    const ws = new WebSocket(`ws://127.0.0.1:${streamPort}`);
    const out = { opened: false, messages: 0 };
    const t = setTimeout(() => (ws.terminate(), resolve(out)), 3000);
    ws.on('open', () => (out.opened = true));
    ws.on('message', () => out.messages++);
    ws.on('error', (e) => (clearTimeout(t), (out.error = e.message), resolve(out)));
  });
  steps.push({ n: 12.1, label: 'stream port reachable without auth', streamPort, listen, unauth });
  console.log(JSON.stringify(steps.at(-1)));
}
await step(12.2, 'stream disable', ['stream', 'disable']);
await step(12.3, 'stream status after disable', ['stream', 'status'], {}, (r, d) => d);

// 10, 11: action policy and confirmation over --cdp (A8).
const policy = join(OUT, '02-policy.json');
writeFileSync(policy, JSON.stringify({ default: 'allow', deny: ['eval'] }));
await step(10, 'policy denies eval', ['eval', 'document.title'], { flags: ['--action-policy', policy] });
await step(10.1, 'policy still allows snapshot', ['snapshot', '-i'], { flags: ['--action-policy', policy] });
writeFileSync(policy, JSON.stringify({ default: 'allow' }));
const conf = await step(11, 'confirm-actions gates eval', ['eval', 'document.title'], { flags: ['--action-policy', policy, '--confirm-actions', 'eval'] }, (r) => r.stdout.slice(0, 400));
const cid = (conf.stdout.match(/c_[0-9a-f]+/) || [])[0];
if (cid) await step(11.1, 'deny pending confirmation', ['deny', cid]);

// 13: close must not take the browser or the human tabs down.
const closeFrom = proxy.log.length;
await step(13, 'close', ['close'], {}, async () => {
  await sleep(1500);
  const sent = proxy.log.slice(closeFrom).filter((e) => e.dir === 'c2b').map((e) => e.method + (e.blocked ? ' (blocked)' : ''));
  return { browserAlive: chrome.proc.exitCode === null, humanTabsAlive: await humanTabsAlive(), pagesLeft: (await pages()).map((t) => t.url), sent };
});

const methods = {};
for (const e of proxy.log) if (e.dir === 'c2b') methods[e.method] = (methods[e.method] || 0) + 1;
const sorted = Object.fromEntries(Object.entries(methods).sort(([a], [b]) => a.localeCompare(b)));
writeResult('02-methods', sorted);
console.log('saved', writeResult('02-proxy', { when: new Date().toISOString(), httpRequests: proxy.httpRequests, rejected: proxy.rejected, blocked: proxy.log.filter((e) => e.blocked), steps }));
console.log('distinct CDP methods:', Object.keys(sorted).length);
direct.close();
proxy.close();
await chrome.kill();
fixture.close();
process.exit(0);
