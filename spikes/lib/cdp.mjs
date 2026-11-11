// Throwaway spike helper: minimal CDP client over WebSocket (flat sessions).
import WebSocket from 'ws';

export function connect(wsUrl, { headers } = {}) {
  return new Promise((resolveConn, rejectConn) => {
    const ws = new WebSocket(wsUrl, { headers, perMessageDeflate: false, maxPayload: 512 * 1024 * 1024 });
    let nextId = 1;
    const pending = new Map();
    const listeners = new Map();
    ws.on('error', rejectConn);
    ws.on('unexpected-response', (_req, res) => rejectConn(new Error(`HTTP ${res.statusCode}`)));
    ws.on('message', (data) => {
      const msg = JSON.parse(data.toString());
      if (msg.id && pending.has(msg.id)) {
        const { resolve, reject, method } = pending.get(msg.id);
        pending.delete(msg.id);
        if (msg.error) reject(new Error(`${method}: ${msg.error.message}`));
        else resolve(msg.result);
      } else if (msg.method) {
        for (const cb of listeners.get(msg.method) || []) cb(msg.params, msg.sessionId);
      }
    });
    ws.on('close', () => {
      for (const { reject, method } of pending.values()) reject(new Error(`${method}: connection closed`));
      pending.clear();
    });
    const api = {
      send(method, params = {}, sessionId, timeoutMs = 30000) {
        const id = nextId++;
        return new Promise((resolve, reject) => {
          const t = setTimeout(() => {
            if (pending.delete(id)) reject(new Error(`${method}: timeout ${timeoutMs}ms`));
          }, timeoutMs);
          pending.set(id, {
            method,
            resolve: (v) => (clearTimeout(t), resolve(v)),
            reject: (e) => (clearTimeout(t), reject(e)),
          });
          ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
        });
      },
      on(eventName, cb) {
        if (!listeners.has(eventName)) listeners.set(eventName, []);
        listeners.get(eventName).push(cb);
      },
      async attach(targetId) {
        const { sessionId } = await api.send('Target.attachToTarget', { targetId, flatten: true });
        return sessionId;
      },
      async evaluate(sessionId, expression, timeoutMs = 10000) {
        const r = await api.send(
          'Runtime.evaluate',
          { expression, returnByValue: true, awaitPromise: true },
          sessionId,
          timeoutMs,
        );
        if (r.exceptionDetails) throw new Error(`evaluate: ${r.exceptionDetails.exception?.description || r.exceptionDetails.text}`);
        return r.result.value;
      },
      // Navigate an attached page and wait for its load event.
      async navigate(sessionId, url, timeoutMs = 15000) {
        await api.send('Page.enable', {}, sessionId);
        const loaded = new Promise((resolve, reject) => {
          const t = setTimeout(() => reject(new Error(`navigate timeout: ${url}`)), timeoutMs);
          api.on('Page.loadEventFired', (_p, sid) => {
            if (sid === sessionId) (clearTimeout(t), resolve());
          });
        });
        await api.send('Page.navigate', { url }, sessionId);
        await loaded;
      },
      // First page target of the browser, attached.
      async firstPage() {
        const { targetInfos } = await api.send('Target.getTargets');
        const page = targetInfos.find((t) => t.type === 'page');
        return { targetId: page.targetId, sessionId: await api.attach(page.targetId), info: page };
      },
      // New tab navigated to url, attached and loaded.
      async newPage(url, extra = {}) {
        const { targetId } = await api.send('Target.createTarget', { url: 'about:blank', ...extra });
        const sessionId = await api.attach(targetId);
        if (url) await api.navigate(sessionId, url);
        return { targetId, sessionId };
      },
      close() {
        ws.close();
      },
    };
    ws.on('open', () => resolveConn(api));
  });
}
