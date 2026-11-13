// Phase 2a spike helpers: a probe web server that logs what reaches it, and a two-profile browser
// (Human + Agent, opened the way 04-profiles.mjs does it) with a way to open further CDP connections.
import { createServer } from 'node:http';
import { networkInterfaces } from 'node:os';
import { spawnSync } from 'node:child_process';
import { WebSocketServer } from 'ws';
import { launchChrome, sleep, CHROME, onCleanup } from './launch.mjs';
import { connect } from './cdp.mjs';

// First RFC 1918 address of this machine, or null. Only used to listen on it for the private-address probe.
export function privateLanIp() {
  for (const list of Object.values(networkInterfaces()))
    for (const a of list || []) if (a.family === 'IPv4' && !a.internal && /^(10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.)/.test(a.address)) return a.address;
  return null;
}

export function parseArgs(argv = process.argv) {
  const only = (argv.find((a) => a.startsWith('--only=')) || '').split('=')[1];
  return { only: only ? only.split(',') : null };
}

// Routes are added by the spike through `routes[path] = (req, res, url) => ...`.
export async function startProbe({ lan = false } = {}) {
  const hits = [];
  const routes = {};
  const sockets = new Set();
  const handler = (req, res) => {
    const url = new URL(req.url, 'http://x');
    hits.push({ path: url.pathname, search: url.search, host: req.headers.host, ts: Date.now() });
    const fn = routes[url.pathname];
    if (fn) return fn(req, res, url);
    res.writeHead(404);
    res.end('not found');
  };
  const wss = new WebSocketServer({ noServer: true });
  const onUpgrade = (req, socket, head) => {
    const url = new URL(req.url, 'http://x');
    hits.push({ path: url.pathname, search: url.search, host: req.headers.host, ts: Date.now(), upgrade: true });
    wss.handleUpgrade(req, socket, head, (ws) => ws.on('error', () => {}));
  };
  const servers = [];
  const listen = (host, port) =>
    new Promise((resolve, reject) => {
      const s = createServer(handler);
      s.on('upgrade', onUpgrade);
      s.on('connection', (c) => (sockets.add(c), c.on('close', () => sockets.delete(c))));
      s.on('error', reject);
      s.listen(port, host, () => resolve(s));
    });
  const s1 = await listen('127.0.0.1', 0);
  servers.push(s1);
  const port = s1.address().port;
  const lanIp = lan ? privateLanIp() : null;
  if (lanIp) servers.push(await listen(lanIp, port).catch(() => null));
  return {
    port,
    origin: `http://127.0.0.1:${port}`,
    lanListening: !!(lanIp && servers[1]),
    lanIp, // internal use only: never print or commit
    hits,
    routes,
    hitsOf: (pathPrefix) => hits.filter((h) => h.path.startsWith(pathPrefix)),
    close() {
      for (const c of sockets) c.destroy();
      for (const s of servers) s?.close();
    },
  };
}

export const html = (body, title = 'probe') => `<!doctype html><html><head><meta charset="utf-8"><title>${title}</title></head><body>${body}</body></html>`;

// Launch one Chrome with a Human and an Agent profile. `cdp` is the first (main) browser-level connection.
export async function openTwoProfiles({ name, args = [], seedPath = '/blank', probe }) {
  const chrome = await launchChrome({ name, profileDirectory: 'Human', args });
  const udd = chrome.userDataDir;
  const cdp = await connect(chrome.browserWsUrl);
  const version = (await cdp.send('Browser.getVersion')).product;
  console.log(`BROWSER: ${version}`);
  const extraConns = [];
  const pages = async () => (await cdp.send('Target.getTargets')).targetInfos.filter((t) => t.type === 'page');
  const human1 = await cdp.firstPage();
  await cdp.navigate(human1.sessionId, `${probe.origin}${seedPath}`);
  const humanCtx = (await pages()).find((t) => t.targetId === human1.targetId).browserContextId;
  const before = (await pages()).map((t) => t.targetId);
  spawnSync(CHROME, [`--user-data-dir=${udd}`, '--profile-directory=Agent', '--no-first-run', '--new-window', 'about:blank'], { timeout: 20000 });
  let agentTarget = null;
  for (let i = 0; i < 60 && !agentTarget; i++) {
    await sleep(250);
    agentTarget = (await pages()).find((t) => !before.includes(t.targetId));
  }
  if (!agentTarget) throw new Error('Agent profile window did not open');
  const agentCtx = agentTarget.browserContextId;
  const ctxs = await cdp.send('Target.getBrowserContexts');
  const defaultCtxIs = ctxs.defaultBrowserContextId === agentCtx ? 'Agent' : ctxs.defaultBrowserContextId === humanCtx ? 'Human' : 'other';
  console.log(`DEFAULT_CONTEXT: ${defaultCtxIs}; contexts listed by Target.getBrowserContexts: ${ctxs.browserContextIds.length}`);
  const agentSession = await cdp.attach(agentTarget.targetId);
  await cdp.navigate(agentSession, `${probe.origin}${seedPath}`);
  const api = {
    chrome,
    cdp,
    udd,
    humanCtx,
    agentCtx,
    version,
    defaultCtxIs,
    human: { targetId: human1.targetId, sessionId: human1.sessionId },
    agent: { targetId: agentTarget.targetId, sessionId: agentSession },
    pages,
    // Another browser-level CDP connection (the "pegboardd" side or the "agent" side).
    async connection() {
      const c = await connect(chrome.browserWsUrl);
      extraConns.push(c);
      return c;
    },
    // New tab in a profile: window.open from a tab already in that profile (createTarget rejects real profiles).
    async newIn(ctx, url) {
      const list = await pages();
      const seed = list.find((t) => t.browserContextId === ctx && /^http/.test(t.url));
      const seedSid = await cdp.attach(seed.targetId);
      const known = list.map((t) => t.targetId);
      await cdp.send('Runtime.evaluate', { expression: `window.open('about:blank') && 1`, userGesture: true }, seedSid);
      let target = null;
      for (let i = 0; i < 40 && !target; i++) {
        await sleep(150);
        target = (await pages()).find((t) => !known.includes(t.targetId));
      }
      if (!target) throw new Error('window.open produced no tab');
      const sessionId = await cdp.attach(target.targetId);
      if (url) await cdp.navigate(sessionId, url);
      return { targetId: target.targetId, sessionId };
    },
    async close() {
      for (const c of extraConns) try { c.close(); } catch {}
      try { cdp.close(); } catch {}
      await chrome.kill();
    },
  };
  return api;
}

export const mean = (a) => a.reduce((x, y) => x + y, 0) / (a.length || 1);
export const stat = (a) => ({ n: a.length, meanMs: +mean(a).toFixed(2), maxMs: +Math.max(...a).toFixed(1) });

// Print one result line and remember it.
export function reporter(prefix, out) {
  return (id, value) => {
    out[id] = value;
    const text = typeof value === 'string' ? value : JSON.stringify(value);
    console.log(`${id}: ${text}`);
  };
}
