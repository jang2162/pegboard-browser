// Spike 01b (A1 follow-up): under a full cover window, measure without agent-browser first,
// then see which agent-browser command (if any) raises the target window above the cover.
import { launchChrome, sleep, writeResult, screenPixel } from './lib/launch.mjs';
import { connect } from './lib/cdp.mjs';
import { startFixture } from './lib/fixture.mjs';
import { ab } from './lib/ab.mjs';

const WIN = { x: 80, y: 80, w: 1000, h: 700 };
const MAGENTA = 'data:text/html,<body style="margin:0;background:rgb(255,0,255)"></body>';
const isMagenta = (px) => !!px && px[0] > 200 && px[1] < 80 && px[2] > 200;
const covered = () => {
  const l = screenPixel(WIN.x + 150, WIN.y + 400);
  const r = screenPixel(WIN.x + WIN.w - 150, WIN.y + 400);
  return { covered: isMagenta(l) && isMagenta(r), l, r };
};
const withFlags = process.argv.includes('--flags');
const fixture = await startFixture();
const chrome = await launchChrome({
  name: '01b',
  win: WIN,
  viaOpen: true,
  args: withFlags ? ['--disable-backgrounding-occluded-windows', '--disable-renderer-backgrounding', '--disable-background-timer-throttling'] : [],
});
const cdp = await connect(chrome.browserWsUrl);
const session = `rz${Date.now() % 1000000}`;
await ab(['open', `${fixture.origin}/counter`], { session, cdp: String(chrome.port), flags: ['--pin-tab'] });
const tab = (await ab(['tab', 'list'], { session })).json.data.tabs.find((t) => /counter/.test(t.url));
const sid = await cdp.attach(tab.targetId);
await cdp.send('Target.activateTarget', { targetId: tab.targetId });

const cover = await launchChrome({ name: 'cover', win: { x: WIN.x - 20, y: WIN.y - 20, w: WIN.w + 40, h: WIN.h + 40 }, viaOpen: true });
const c = await connect(cover.browserWsUrl);
await c.send('Page.navigate', { url: MAGENTA }, (await c.firstPage()).sessionId);
c.close();
await sleep(4000);

const rows = [];
const note = async (label, extra = {}) => {
  const state = JSON.parse(await cdp.evaluate(sid, `JSON.stringify([document.visibilityState, document.hasFocus()])`));
  const row = { label, ...covered(), visibilityState: state[0], hasFocus: state[1], ...extra };
  rows.push(row);
  console.log(JSON.stringify(row));
};
await note('covered, before anything');

// CDP only, no agent-browser.
const read = async () => JSON.parse(await cdp.evaluate(sid, `JSON.stringify([window.__ticks, window.__frames, performance.now()])`));
const a = await read();
await sleep(5000);
const b = await read();
const secs = (b[2] - a[2]) / 1000;
let frames = 0;
let on = true;
cdp.on('Page.screencastFrame', (p, s) => {
  if (!on) return;
  frames++;
  cdp.send('Page.screencastFrameAck', { sessionId: p.sessionId }, s).catch(() => {});
});
await cdp.send('Page.startScreencast', { format: 'jpeg', quality: 50, everyNthFrame: 1 }, sid);
await sleep(10000);
await cdp.send('Page.stopScreencast', {}, sid);
on = false;
const t0 = Date.now();
const shot = await cdp.send('Page.captureScreenshot', { format: 'jpeg', quality: 60 }, sid, 10000).then(() => true, () => false);
await note('cdp only', { ticksPerSec: +((b[0] - a[0]) / secs).toFixed(1), framesPerSec: +((b[1] - a[1]) / secs).toFixed(1), screencastFrames: frames, screenshotOk: shot, screenshotMs: Date.now() - t0 });

for (const args of [['get', 'url'], ['snapshot', '-i'], ['eval', '1+1'], ['screenshot', '/tmp/pegspike-shot.png'], ['hover', '#btn'], ['click', '#btn'], ['fill', '#btn', 'x'], ['open', `${fixture.origin}/form`], ['tab', 'new', `${fixture.origin}/counter`]]) {
  const r = await ab(args, { session, timeoutMs: 30000 });
  await sleep(700);
  await note(`after ab ${args.slice(0, 2).join(' ')}`, { ok: r.ok, ms: r.ms });
}
console.log('saved', writeResult(withFlags ? '01b-raise-flags' : '01b-raise', { when: new Date().toISOString(), rows }));
await ab(['close'], { session });
cdp.close();
await cover.kill();
await chrome.kill();
fixture.close();
process.exit(0);
