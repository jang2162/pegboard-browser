// Throwaway spike helper: local pages used by the experiments.
import { createServer } from 'node:http';

const page = (title, body) =>
  `<!doctype html><html><head><meta charset="utf-8"><title>${title}</title></head><body>${body}</body></html>`;

const PAGES = {
  '/counter': page(
    'counter',
    `<h1>counter</h1><button id="btn">Add</button> <span id="clicks">0</span>
<script>
window.__ticks = 0; window.__frames = 0;
document.getElementById('btn').addEventListener('click', () => {
  const el = document.getElementById('clicks'); el.textContent = String(Number(el.textContent) + 1);
});
setInterval(() => { window.__ticks++; document.body.style.background = 'hsl(' + (window.__ticks * 7 % 360) + ',70%,85%)'; }, 100);
(function loop() { window.__frames++; requestAnimationFrame(loop); })();
</script>`,
  ),
  '/cookie/get': page(
    'cookie-get',
    `<pre id="out"></pre><script>
document.getElementById('out').textContent = JSON.stringify({ cookie: document.cookie, ls: localStorage.getItem('spike') });
</script>`,
  ),
  '/opener': page('opener', `<button id="open" onclick="window.open('/counter')">Open</button>`),
  '/form': page(
    'form',
    `<label>Name <input id="name"></label> <label>Password <input id="pw" type="password"></label>`,
  ),
  '/writer': page(
    'writer',
    `<p>writer</p><script>setInterval(() => localStorage.setItem('busy', String(Date.now())), 100);</script>`,
  ),
};

export function startFixture() {
  return new Promise((resolve) => {
    const server = createServer((req, res) => {
      const path = new URL(req.url, 'http://x').pathname;
      if (path === '/cookie/set') {
        res.writeHead(200, { 'content-type': 'text/html', 'set-cookie': 'spike=1; Max-Age=86400; Path=/' });
        res.end(page('cookie-set', `<p>set</p><script>localStorage.setItem('spike', '1');</script>`));
        return;
      }
      if (PAGES[path]) {
        res.writeHead(200, { 'content-type': 'text/html' });
        res.end(PAGES[path]);
        return;
      }
      res.writeHead(404);
      res.end('not found');
    });
    server.listen(0, '127.0.0.1', () => {
      resolve({ origin: `http://127.0.0.1:${server.address().port}`, close: () => server.close() });
    });
  });
}
