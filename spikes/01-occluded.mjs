// Spike 01 (A1): does automation and capture keep working when the agent window is hidden?
// Scenarios: fg, bg-tab, partial, occluded, minimized. Manual ones (other-space, locked) are not automated.
import { launchChrome, sleep, writeResult, screenPixel } from './lib/launch.mjs';
import { connect } from './lib/cdp.mjs';
import { startFixture } from './lib/fixture.mjs';
import { ab } from './lib/ab.mjs';

const arg = (name, dflt) => {
  const hit = process.argv.find((a) => a.startsWith(`--${name}=`));
  return hit ? hit.split('=')[1].split(',') : dflt;
};
const SCENARIOS = arg('only', ['fg', 'bg-tab', 'partial', 'occluded', 'minimized']);
const CONFIGS = arg('config', ['default', 'flags', 'cdp', 'flags+cdp']);
const OUT_NAME = arg('out', ['01-occluded'])[0];
const FLAGS = [
  '--disable-backgrounding-occluded-windows',
  '--disable-renderer-backgrounding',
  '--disable-background-timer-throttling',
];
const WIN = { x: 80, y: 80, w: 1000, h: 700 };
const MAGENTA = 'data:text/html,<body style="margin:0;background:rgb(255,0,255)"></body>';
const isMagenta = (px) => !!px && px[0] > 200 && px[1] < 80 && px[2] > 200;
// The fixture pages are light (pastel counter, white form). A dark pixel means another app's window is on top.
const isLight = (px) => !!px && Math.min(...px) > 140 && !isMagenta(px);
const LEFT = [WIN.x + 150, WIN.y + 400];
const RIGHT = [WIN.x + WIN.w - 150, WIN.y + 400];
// Review Focus 2: check on screen that the window stacking is what the scenario claims.
function stacking(scenario) {
  const left = screenPixel(...LEFT);
  const right = screenPixel(...RIGHT);
  let valid;
  if (scenario === 'fg' || scenario === 'bg-tab') valid = isLight(left) && isLight(right);
  else if (scenario === 'partial') valid = isLight(left) && isMagenta(right);
  else if (scenario === 'occluded') valid = isMagenta(left) && isMagenta(right);
  else valid = true;
  return { left, right, valid };
}

async function measure(cdp, page, session, port) {
  const m = {};
  const state = JSON.parse(await cdp.evaluate(page.sessionId, `JSON.stringify([document.visibilityState, document.hasFocus()])`));
  m.visibilityState = state[0];
  m.hasFocus = state[1];

  const read = async () => JSON.parse(await cdp.evaluate(page.sessionId, `JSON.stringify([window.__ticks, window.__frames, performance.now()])`));
  const a = await read();
  await sleep(5000);
  const b = await read();
  const secs = (b[2] - a[2]) / 1000;
  m.ticksPerSec = +((b[0] - a[0]) / secs).toFixed(1);
  m.framesPerSec = +((b[1] - a[1]) / secs).toFixed(1);

  const url = await ab(['get', 'url'], { session });
  m.boundToCounter = /\/counter/.test(url.json?.data?.url || '');

  const snap = await ab(['snapshot', '-i'], { session, timeoutMs: 30000 });
  m.snapshotOk = snap.ok && /button/.test(snap.json?.data?.snapshot || '');
  m.snapshotMs = snap.ms;

  const before = Number(await cdp.evaluate(page.sessionId, `document.getElementById('clicks').textContent`));
  const click = await ab(['click', '#btn'], { session, timeoutMs: 30000 });
  await sleep(300);
  const after = Number(await cdp.evaluate(page.sessionId, `document.getElementById('clicks').textContent`));
  m.clickOk = click.ok && after === before + 1;
  m.clickMs = click.ms;
  if (!click.ok) m.clickError = String(click.json?.error || click.stderr).slice(0, 200);

  const t0 = Date.now();
  try {
    const shot = await cdp.send('Page.captureScreenshot', { format: 'jpeg', quality: 60 }, page.sessionId, 10000);
    m.screenshotOk = !!shot.data && shot.data.length > 1000;
  } catch (e) {
    m.screenshotOk = false;
    m.screenshotError = e.message;
  }
  m.screenshotMs = Date.now() - t0;

  let frames = 0;
  const counter = { on: true };
  cdp.on('Page.screencastFrame', (p, sid) => {
    if (sid !== page.sessionId || !counter.on) return;
    frames++;
    cdp.send('Page.screencastFrameAck', { sessionId: p.sessionId }, page.sessionId).catch(() => {});
  });
  await cdp.send('Page.startScreencast', { format: 'jpeg', quality: 50, everyNthFrame: 1 }, page.sessionId);
  await sleep(10000);
  await cdp.send('Page.stopScreencast', {}, page.sessionId).catch(() => {});
  counter.on = false;
  m.screencastFrames = frames;

  m.pass = m.snapshotOk && m.clickOk && m.screenshotOk && m.screenshotMs <= 2000 && m.screencastFrames >= 10;
  return m;
}

const fixture = await startFixture();
const results = [];
const setupFailures = [];
for (const config of CONFIGS) {
  // viaOpen so the target window starts frontmost; fg and bg-tab run first while it still is.
  const chrome = await launchChrome({ name: `01-${config.replace('+', '-')}`, args: config.includes('flags') ? FLAGS : [], win: WIN, viaOpen: true });
  const cdp = await connect(chrome.browserWsUrl);
  // agent-browser opens its own pinned tab; we attach to that same tab to measure it.
  const session = `occ${Date.now() % 1000000}`;
  let opened;
  for (let attempt = 1; attempt <= 3; attempt++) {
    opened = await ab(['open', `${fixture.origin}/counter`], { session, cdp: String(chrome.port), flags: ['--pin-tab'] });
    if (opened.ok) break;
    setupFailures.push({ config, attempt, error: String(opened.json?.error || opened.stderr).slice(0, 200) });
    await sleep(1500);
  }
  if (!opened.ok) throw new Error(`open failed: ${opened.stdout} ${opened.stderr}`);
  const tabs = await ab(['tab', 'list'], { session });
  const tab = (tabs.json?.data?.tabs || []).find((t) => /\/counter/.test(t.url));
  if (!tab) throw new Error(`counter tab not found: ${tabs.stdout}`);
  const page = { targetId: tab.targetId, sessionId: await cdp.attach(tab.targetId) };
  await cdp.send('Page.enable', {}, page.sessionId);
  await cdp.send('Target.activateTarget', { targetId: page.targetId });
  if (config.includes('cdp')) {
    await cdp.send('Emulation.setFocusEmulationEnabled', { enabled: true }, page.sessionId);
    await cdp.send('Page.setWebLifecycleState', { state: 'active' }, page.sessionId);
  }
  const { windowId } = await cdp.send('Browser.getWindowForTarget', { targetId: page.targetId });

  for (const scenario of SCENARIOS) {
    let cover = null;
    let other = null;
    const row = { config, scenario };
    try {
      if (scenario === 'bg-tab') {
        other = await cdp.newPage(`${fixture.origin}/form`);
        await cdp.send('Target.activateTarget', { targetId: other.targetId });
      } else if (scenario === 'partial' || scenario === 'occluded') {
        const win = scenario === 'occluded' ? { x: WIN.x - 20, y: WIN.y - 20, w: WIN.w + 40, h: WIN.h + 40 } : { x: WIN.x + WIN.w / 2, y: WIN.y - 20, w: WIN.w / 2 + 20, h: WIN.h + 40 };
        cover = await launchChrome({ name: 'cover', win, viaOpen: true });
        const c = await connect(cover.browserWsUrl);
        const cp = await c.firstPage();
        await c.send('Page.navigate', { url: MAGENTA }, cp.sessionId);
        c.close();
        await sleep(2500);
      } else if (scenario === 'minimized') {
        await cdp.send('Browser.setWindowBounds', { windowId, bounds: { windowState: 'minimized' } });
      }
      await sleep(3000);
      const before = stacking(scenario);
      Object.assign(row, await measure(cdp, page, session, chrome.port));
      const after = stacking(scenario);
      row.stackingBefore = before;
      row.stackingAfter = after;
      // A row only counts when the screen showed the intended stacking both before and after measuring.
      row.valid = before.valid && after.valid;
    } catch (e) {
      row.error = e.message;
    } finally {
      if (other) {
        await cdp.send('Target.closeTarget', { targetId: other.targetId }).catch(() => {});
        await cdp.send('Target.activateTarget', { targetId: page.targetId }).catch(() => {});
      }
      if (cover) await cover.kill();
      if (scenario === 'minimized') await cdp.send('Browser.setWindowBounds', { windowId, bounds: { windowState: 'normal' } }).catch(() => {});
      await sleep(1000);
    }
    results.push(row);
    console.log(JSON.stringify(row));
  }
  await ab(['close'], { session });
  cdp.close();
  await chrome.kill();
}
fixture.close();
console.log('saved', writeResult(OUT_NAME, { when: new Date().toISOString(), setupFailures, results }));
process.exit(0);
