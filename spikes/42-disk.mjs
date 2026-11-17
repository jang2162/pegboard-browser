// Spike 42 (phase 2b, design section 5 item 5): disk use of one recorded session.
// A test server hands out a text page, a page with many pictures and a CSS animation page in turn. Every
// 20 s the tab navigates, every 2 s it scrolls, every 5 s a simulated command runs with a direct capture
// before and after it. The stream (R's own session, JPEG 60, maxWidth 1280, acked at once) is thinned to
// 2 frames a second (a frame within 500 ms of the last kept one waits; the last one waiting is kept) and
// written to files, as the recorder would.
//
// Usage: node 42-disk.mjs [--minutes <n>] [--keep-frames]   (default 30 minutes; frames deleted at the end)
import { mkdirSync, writeFileSync, appendFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { launchChrome, sleep, writeResult, OUT, onCleanup } from './lib/launch.mjs';
import { connect } from './lib/cdp.mjs';
import { startProbe, html } from './lib/probe.mjs';
import { flags as readFlags, keepAwake, chromePids, activatePid } from './lib/desktop.mjs';

const FL = readFlags();
const MINUTES = Number(FL.val('minutes', 30));
const KEEP = process.argv.includes('--keep-frames');
if (!(MINUTES > 0)) throw new Error('--minutes must be a positive number');

const probe = await startProbe();
const words = 'recording black box session agent browser page frame screen command capture navigation'.split(' ');
const text = (n) => Array.from({ length: n }, (_, i) => words[(i * 7 + (i >> 3)) % words.length]).join(' ');
const send = (res, body) => (res.writeHead(200, { 'content-type': 'text/html', 'cache-control': 'no-store' }), res.end(body));
probe.routes['/text'] = (_q, res) =>
  send(res, html(`<style>body{font:18px/1.6 Georgia,serif;max-width:900px;margin:40px auto}</style>${Array.from({ length: 80 }, (_, i) => `<h2>Section ${i}</h2><p>${text(160)}</p>`).join('')}`, 'text'));
probe.routes['/images'] = (_q, res) =>
  send(
    res,
    html(
      `<style>body{margin:0;display:grid;grid-template-columns:repeat(4,1fr);gap:8px;padding:8px}canvas{width:100%}</style>
${Array.from({ length: 48 }, (_, i) => `<canvas id="c${i}" width="400" height="300"></canvas>`).join('')}
<script>
// Photo-like content: gradients, shapes and some grain (JPEG-unfriendly, as photos are).
for (let i = 0; i < 48; i++) {
  const c = document.getElementById('c' + i).getContext('2d');
  const g = c.createLinearGradient(0, 0, 400, 300);
  g.addColorStop(0, 'hsl(' + (i * 37 % 360) + ',60%,50%)'); g.addColorStop(1, 'hsl(' + (i * 91 % 360) + ',70%,30%)');
  c.fillStyle = g; c.fillRect(0, 0, 400, 300);
  for (let k = 0; k < 40; k++) { c.fillStyle = 'hsla(' + Math.random() * 360 + ',70%,60%,0.5)'; c.beginPath(); c.arc(Math.random() * 400, Math.random() * 300, Math.random() * 60, 0, 7); c.fill(); }
  const d = c.getImageData(0, 0, 400, 300);
  for (let p = 0; p < d.data.length; p += 4) { const n = (Math.random() - 0.5) * 40; d.data[p] += n; d.data[p + 1] += n; d.data[p + 2] += n; }
  c.putImageData(d, 0, 0);
}
</script>`,
      'images',
    ),
  );
probe.routes['/anim'] = (_q, res) =>
  send(
    res,
    html(
      `<style>body{margin:0;height:3000px;background:#f4f4f8}
.b{position:absolute;width:80px;height:80px;border-radius:12px;animation:spin 3s linear infinite, move 5s ease-in-out infinite alternate}
@keyframes spin{to{transform:rotate(360deg)}} @keyframes move{to{left:80%}}
#bar{position:fixed;bottom:0;left:0;height:12px;background:#36c;animation:grow 4s linear infinite} @keyframes grow{from{width:0}to{width:100%}}</style>
${Array.from({ length: 20 }, (_, i) => `<div class="b" style="top:${40 + i * 120}px;left:${(i * 13) % 60}%;background:hsl(${i * 18},70%,55%)"></div>`).join('')}<div id="bar"></div>`,
      'anim',
    ),
  );
const PAGES = ['/text', '/images', '/anim'];

const dir = join(OUT, '42-disk', 'screen');
rmSync(join(OUT, '42-disk', 'screen'), { recursive: true, force: true });
mkdirSync(dir, { recursive: true });
const index = join(dir, 'index.jsonl');
writeFileSync(index, '');
let fileNo = 0;
let fileBytes = 0;
const counts = { streamReceived: 0, streamKept: 0, streamDropped: 0, captures: 0, captureFailures: 0 };
function writeFrame(b64, kind, meta = {}) {
  const buf = Buffer.from(b64, 'base64');
  const file = `${String(++fileNo).padStart(6, '0')}.jpg`;
  writeFileSync(join(dir, file), buf);
  const lineText = JSON.stringify({ ts: new Date().toISOString(), file, url: currentUrl, kind, ...meta }) + '\n';
  appendFileSync(index, lineText);
  fileBytes += buf.length + Buffer.byteLength(lineText);
}

const keep = keepAwake();
const chrome = await launchChrome({ name: '42-disk', profileDirectory: 'Agent', viaOpen: true, win: { x: 80, y: 80, w: 1280, h: 860 } });
onCleanup(() => keep.stop());
const F = await connect(chrome.browserWsUrl);
const R = await connect(chrome.browserWsUrl);
console.log(`BROWSER: ${(await F.send('Browser.getVersion')).product}`);
activatePid(chromePids(chrome.userDataDir).main);
const page = await F.firstPage();
const sF = page.sessionId;
let currentUrl = `${probe.origin}${PAGES[0]}`;
await F.navigate(sF, currentUrl);
await F.send('Emulation.setFocusEmulationEnabled', { enabled: true }, sF);
await F.send('Page.setWebLifecycleState', { state: 'active' }, sF);
const sR = await R.attach(page.targetId);
await R.send('Page.enable', {}, sR);

// 2 frames a second: a frame within 500 ms of the last kept one waits; a later one replaces it; the one
// waiting is written when its 500 ms are up (so the last frame before the screen stops is kept).
let lastWrite = 0;
let pending = null;
let timer = null;
function offer(frame) {
  const now = Date.now();
  if (!timer && now - lastWrite >= 500) {
    writeFrame(frame.data, 'change', frame.meta);
    counts.streamKept++;
    lastWrite = now;
    return;
  }
  if (pending) counts.streamDropped++;
  pending = frame;
  if (!timer)
    timer = setTimeout(() => {
      timer = null;
      if (pending) {
        writeFrame(pending.data, 'change', pending.meta);
        counts.streamKept++;
        pending = null;
        lastWrite = Date.now();
      }
    }, Math.max(0, 500 - (now - lastWrite)));
}
R.on('Page.screencastFrame', (p, sid) => {
  if (sid !== sR) return;
  R.send('Page.screencastFrameAck', { sessionId: p.sessionId }, sR).catch(() => {});
  counts.streamReceived++;
  offer({ data: p.data, meta: { width: p.metadata.deviceWidth, height: p.metadata.deviceHeight } });
});
await R.send('Page.startScreencast', { format: 'jpeg', quality: 60, maxWidth: 1280 }, sR);

const capture = async (kind) => {
  try {
    const r = await R.send('Page.captureScreenshot', { format: 'jpeg', quality: 60 }, sR, 5000);
    writeFrame(r.data, kind);
    counts.captures++;
  } catch {
    counts.captureFailures++;
  }
};

const start = Date.now();
const end = start + MINUTES * 60000;
let nextNav = start + 20000;
let nextScroll = start + 2000;
let nextCommand = start + 5000;
let nextReport = start + 60000;
let pageNo = 0;
let dir_ = 1;
while (Date.now() < end) {
  const now = Date.now();
  if (now >= nextNav) {
    nextNav += 20000;
    currentUrl = `${probe.origin}${PAGES[++pageNo % PAGES.length]}`;
    await F.navigate(sF, currentUrl).catch((e) => console.log(`navigate: ${e.message}`));
  }
  if (now >= nextScroll) {
    nextScroll += 2000;
    const at = await F.evaluate(sF, `(() => { window.scrollBy(0, ${dir_} * 400); return [scrollY, document.documentElement.scrollHeight - innerHeight]; })()`).catch(() => [0, 0]);
    if (at[0] >= at[1] - 1) dir_ = -1;
    if (at[0] <= 0) dir_ = 1;
  }
  if (now >= nextCommand) {
    nextCommand += 5000;
    await capture('before');
    await F.evaluate(sF, `document.body.dataset.cmd = String(Date.now()); 1`).catch(() => {});
    await capture('after');
  }
  if (now >= nextReport) {
    nextReport += 60000;
    const min = (now - start) / 60000;
    console.log(`progress ${min.toFixed(0)} min: ${(fileBytes / 1e6).toFixed(1)} MB, ${fileNo} files, ${JSON.stringify(counts)}`);
  }
  await sleep(100);
}
await R.send('Page.stopScreencast', {}, sR).catch(() => {});
await sleep(700);

const minutes = (Date.now() - start) / 60000;
const perMin = fileBytes / minutes;
const perHour = perMin * 60;
const result = {
  minutes: +minutes.toFixed(2),
  totalBytes: fileBytes,
  files: fileNo,
  ...counts,
  bytesPerMinute: Math.round(perMin),
  bytesPerHour: Math.round(perHour),
  // 14 days of continuous recording, and of 8 recorded hours a day.
  fourteenDays24h: Math.round(perHour * 24 * 14),
  fourteenDays8h: Math.round(perHour * 8 * 14),
  daysIn5GB24h: +(5e9 / (perHour * 24)).toFixed(2),
  daysIn5GB8h: +(5e9 / (perHour * 8)).toFixed(2),
  framesKept: !!KEEP,
};
const MB = (b) => `${(b / 1e6).toFixed(1)} MB`;
console.log(`D total ${minutes.toFixed(1)} min: ${MB(fileBytes)} in ${fileNo} files (stream kept ${counts.streamKept} of ${counts.streamReceived}, captures ${counts.captures}, capture failures ${counts.captureFailures})`);
console.log(`D per minute ${MB(perMin)}; per hour ${MB(perHour)}`);
console.log(`D 14 days: ${MB(result.fourteenDays24h)} at 24 h/day, ${MB(result.fourteenDays8h)} at 8 h/day`);
console.log(`D 5 GB lasts ${result.daysIn5GB24h} days at 24 h/day, ${result.daysIn5GB8h} days at 8 h/day`);
const per30 = perMin * 30;
console.log(`D 30-minute use ${MB(per30)}: ${per30 > 1e9 ? 'FAIL (M5: over 1 GB)' : per30 > 250e6 ? 'REPORT (over 250 MB)' : 'PASS (under 250 MB)'}`);
console.log('saved', writeResult('42-disk', result));
if (!KEEP) rmSync(dir, { recursive: true, force: true });
F.close();
R.close();
await chrome.kill();
probe.close();
process.exit(0);
