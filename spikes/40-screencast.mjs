// Spike 40 (phase 2b, design section 5 items 1 and 4, plus the event order the screen-off rule needs):
// screencast frames and direct captures by window state (V1, V2), frame size (V3), ack modes (V4),
// another connection's start/stop (V5), and the order of navigation events and new-document frames (S6).
//
// One tab of an `Agent` profile on a scratch user-data-dir. Connection F holds the focus-keeping session
// (`Emulation.setFocusEmulationEnabled`, `Page.setWebLifecycleState active`); connection R plays the
// recorder. Configuration A streams on F's session, configuration B on R's own session.
//
// Usage: node 40-screencast.mjs [--auto] [--with-lock] [--with-sleep] [--only=<ids>]
//   ids: visible,bg-tab,minimized,occluded,other-space,locked,display-sleep,v3,v4,v5,s6,s6b
//   (s6b runs only when named: --only=s6b [--navs <n per kind>] [--kinds=...])
//   --auto        window states without a person: other Space by Control+Right (System Events), lock by
//                 Control+Cmd+Q. Without it the script asks a person for those steps.
//   --with-lock   run the screen-lock step (a person must unlock; the script waits for that).
//   --with-sleep  run the display-sleep step (pmset displaysleepnow, woken by caffeinate -u).
// Every state row says how the state was made (`made`) and whether it was checked on screen (`verified`).
import zlib from 'node:zlib';
import { spawnSync } from 'node:child_process';
import { launchChrome, sleep, writeResult, screenPixel, onCleanup } from './lib/launch.mjs';
import { connect } from './lib/cdp.mjs';
import { startProbe, html } from './lib/probe.mjs';
import {
  flags as readFlags, chromePids, onScreen, screenLocked, activatePid, spaceRight, spaceLeft, lockScreen,
  displaySleepNow, wakeDisplay, chime, keepAwake, waitEnter, closePrompt, waitFor, imageSize, median, osa,
} from './lib/desktop.mjs';

const F_ = readFlags();
const want = (id) => !F_.only || F_.only.includes(id);
const out = { when: new Date().toISOString(), flags: { auto: F_.auto, withLock: F_.withLock, withSleep: F_.withSleep, only: F_.only }, rows: [] };
const line = (id, text, value) => {
  if (value !== undefined) out[id] = value;
  console.log(`${id}: ${text}`);
};
const WIN = { x: 80, y: 80, w: 1280, h: 860 };
const COLORS = ['rgb(255,0,0)', 'rgb(0,200,0)', 'rgb(0,0,255)'];
const MAGENTA = 'data:text/html,<body style="margin:0;background:rgb(255,0,255)"></body>';

// ---- pages ----
const probe = await startProbe();
const OTHER = `http://localhost:${probe.port}`; // another host (and site) than probe.origin
const q = (url, k, d) => url.searchParams.get(k) ?? d;
probe.routes['/anim'] = (_req, res, url) => {
  const doc = q(url, 'doc', 'x');
  const scroll = Number(q(url, 'scroll', 0));
  const color = COLORS[Number(q(url, 'color', 0)) % 3];
  const stopAfter = Number(q(url, 'stopAfter', 0));
  const final = Number(q(url, 'final', 0));
  // `cache=1` leaves out `no-store`, so the page may go into the back/forward cache.
  res.writeHead(200, { 'content-type': 'text/html', ...(q(url, 'cache', '0') === '1' ? {} : { 'cache-control': 'no-store' }) });
  res.end(
    html(
      `<style>body{margin:0;height:6000px;font:20px sans-serif}#id{position:fixed;left:0;top:0;width:80px;height:80px;background:${color}}</style>
<div id="id"></div><p style="margin:100px">doc ${doc}</p>
<script>
history.scrollRestoration = 'manual';
window.scrollTo(0, ${scroll});
let n = 0; const t0 = performance.now();
const iv = setInterval(() => {
  n++; document.body.style.background = 'hsl(' + (n * 7 % 360) + ',70%,85%)';
  if (${stopAfter} && performance.now() - t0 > ${stopAfter}) {
    clearInterval(iv); document.body.style.background = 'rgb(250,250,250)'; window.scrollTo(0, ${final}); document.title = 'final';
  }
}, 100);
</script>`,
      `doc ${doc}`,
    ),
  );
};
// The same document as /anim, but its content (colour box, scroll position, changing background) is
// shown only after `busyMs` of busy loop that starts `busyAfter` ms after the script runs; with `poll=n`
// it keeps n requests to /hang open all the time.
probe.routes['/late'] = (_req, res, url) => {
  const scroll = Number(q(url, 'scroll', 0));
  const color = COLORS[Number(q(url, 'color', 0)) % 3];
  const busyAfter = Number(q(url, 'busyAfter', 0));
  const busyMs = Number(q(url, 'busyMs', 0));
  const poll = Number(q(url, 'poll', 0));
  res.writeHead(200, { 'content-type': 'text/html', 'cache-control': 'no-store' });
  res.end(
    html(
      `<style>body{margin:0;height:6000px;font:20px sans-serif}#id{position:fixed;left:0;top:0;width:80px;height:80px;background:${color};display:none}#p{margin:100px;display:none}</style>
<div id="id"></div><p id="p">doc ${q(url, 'doc', 'x')}</p>
<script>
history.scrollRestoration = 'manual';
function show() {
  document.getElementById('id').style.display = 'block'; document.getElementById('p').style.display = 'block';
  window.scrollTo(0, ${scroll});
  let n = 0; setInterval(() => { n++; document.body.style.background = 'hsl(' + (n * 7 % 360) + ',70%,85%)'; }, 100);
  for (let k = 0; k < ${poll}; k++) (async () => { for (;;) { try { await fetch('/hang?r=' + Math.random(), { cache: 'no-store' }); } catch (e) { await new Promise((r) => setTimeout(r, 200)); } } })();
}
if (${busyMs}) setTimeout(() => { const t = performance.now(); while (performance.now() - t < ${busyMs}); show(); }, ${busyAfter});
else show();
</script>`,
      'late',
    ),
  );
};
probe.routes['/hang'] = (_req, res) => {
  const t = setTimeout(() => (res.writeHead(200, { 'content-type': 'text/plain', 'cache-control': 'no-store' }), res.end('ok')), 4000);
  res.on('close', () => clearTimeout(t));
};
probe.routes['/r302'] = (_req, res, url) => {
  res.writeHead(302, { location: url.searchParams.get('to'), 'cache-control': 'no-store' });
  res.end();
};
const anim = (host, params) => `${host}/anim?${new URLSearchParams(params)}`;

// ---- browser ----
const keep = keepAwake();
const chrome = await launchChrome({ name: '40-screencast', profileDirectory: 'Agent', win: WIN, viaOpen: true });
onCleanup(() => keep.stop());
const F = await connect(chrome.browserWsUrl);
const R = await connect(chrome.browserWsUrl);
out.browser = (await F.send('Browser.getVersion')).product;
console.log(`BROWSER: ${out.browser}`);
const page = await F.firstPage();
const sF = page.sessionId;
await F.navigate(sF, anim(probe.origin, { doc: 'main' }));
await F.send('Emulation.setFocusEmulationEnabled', { enabled: true }, sF);
await F.send('Page.setWebLifecycleState', { state: 'active' }, sF);
const sR = await R.attach(page.targetId);
await R.send('Page.enable', {}, sR);
const { windowId } = await F.send('Browser.getWindowForTarget', { targetId: page.targetId });
const pids = () => chromePids(chrome.userDataDir);
const mainPid = () => pids().main;
out.dpr = await F.evaluate(sF, 'window.devicePixelRatio');
line('ENV', `devicePixelRatio ${out.dpr}, window ${WIN.w}x${WIN.h}`);
const CONFIGS = { A: { id: 'A', conn: F, sid: sF, what: "F's focus-keeping session" }, B: { id: 'B', conn: R, sid: sR, what: "R's own session" } };

// ---- stream and capture helpers ----
function streamOn(conn, sid, { params = {}, ackDelayMs = 0 } = {}) {
  const frames = [];
  let on = true;
  const handler = (p, s) => {
    if (s !== sid || !on) return;
    const size = imageSize(p.data);
    frames.push({ t: performance.now(), bytes: Math.floor((p.data.length * 3) / 4), w: size?.w, h: size?.h, y: p.metadata?.scrollOffsetY });
    const ack = () => conn.send('Page.screencastFrameAck', { sessionId: p.sessionId }, sid).catch(() => {});
    if (ackDelayMs) setTimeout(ack, ackDelayMs);
    else ack();
  };
  conn.on('Page.screencastFrame', handler);
  const started = conn.send('Page.startScreencast', { format: 'jpeg', quality: 60, maxWidth: 1280, ...params }, sid);
  return {
    frames,
    started,
    between: (a, b) => frames.filter((f) => f.t >= a && f.t < b),
    async stop() {
      on = false;
      conn.off('Page.screencastFrame', handler);
      await conn.send('Page.stopScreencast', {}, sid).catch(() => {});
    },
  };
}

async function captures(conn, sid, n = 10, params = {}) {
  const ms = [];
  const errors = [];
  let bytes = null;
  let size = null;
  for (let i = 0; i < n; i++) {
    const t0 = performance.now();
    try {
      const r = await conn.send('Page.captureScreenshot', { format: 'jpeg', quality: 60, ...params }, sid, 5000);
      ms.push(performance.now() - t0);
      bytes = Math.floor((r.data.length * 3) / 4);
      size = imageSize(r.data);
    } catch (e) {
      errors.push(`${e.message} (${Math.round(performance.now() - t0)} ms)`);
    }
  }
  return { ok: ms.length, of: n, medianMs: ms.length ? +median(ms).toFixed(1) : null, maxMs: ms.length ? +Math.max(...ms).toFixed(1) : null, bytes, w: size?.w, h: size?.h, errors };
}

async function pageState() {
  try {
    return JSON.parse(await F.evaluate(sF, 'JSON.stringify([document.visibilityState, document.hasFocus()])', 3000));
  } catch (e) {
    return ['unreadable', e.message];
  }
}

async function cpuSample() {
  const { processInfo } = await F.send('SystemInfo.getProcessInfo');
  const by = {};
  for (const p of processInfo) by[p.type] = (by[p.type] || 0) + p.cpuTime;
  const ps = {};
  for (const p of processInfo) {
    const r = spawnSync('ps', ['-o', '%cpu=', '-p', String(p.id)], { encoding: 'utf8' });
    ps[p.type] = +((ps[p.type] || 0) + (Number((r.stdout || '').trim()) || 0)).toFixed(1);
  }
  return { cpuTime: by, psCpu: ps, t: performance.now() };
}
const cpuDelta = (a, b) => {
  const secs = (b.t - a.t) / 1000;
  const pct = {};
  for (const k of Object.keys(b.cpuTime)) pct[k] = +((100 * (b.cpuTime[k] - (a.cpuTime[k] || 0))) / secs).toFixed(1);
  return { cpuPercentFromCpuTime: pct, psCpuAtEnd: b.psCpu };
};

// ---- window states ----
let cover = null;
let bgTab = null;
const STATES = [
  {
    id: 'visible',
    made: 'none',
    apply: async () => (activatePid(mainPid()), true),
    restore: async () => {},
  },
  {
    id: 'bg-tab',
    made: 'cdp (Target.activateTarget on another tab of the same window)',
    apply: async () => {
      bgTab = await F.send('Target.createTarget', { url: anim(probe.origin, { doc: 'bg' }) });
      await sleep(800);
      await F.send('Target.activateTarget', { targetId: bgTab.targetId });
      // Only the CDP reply says the other tab is in front; the screen is not checked.
      return 'unverified (cdp reply only)';
    },
    restore: async () => {
      await F.send('Target.activateTarget', { targetId: page.targetId });
    },
    cleanup: async () => {
      if (bgTab) await F.send('Target.closeTarget', { targetId: bgTab.targetId }).catch(() => {});
      bgTab = null;
    },
  },
  {
    id: 'minimized',
    made: 'cdp (Browser.setWindowBounds minimized)',
    apply: async () => {
      await F.send('Browser.setWindowBounds', { windowId, bounds: { windowState: 'minimized' } });
      await sleep(1000);
      return (await F.send('Browser.getWindowBounds', { windowId })).bounds.windowState === 'minimized';
    },
    restore: async () => {
      await F.send('Browser.setWindowBounds', { windowId, bounds: { windowState: 'normal' } });
    },
  },
  {
    id: 'occluded',
    made: 'own cover window (a second Chrome on its own user-data-dir, magenta page) over the whole window',
    apply: async () => {
      activatePid(mainPid());
      cover = await launchChrome({ name: '40-cover', win: { x: WIN.x - 20, y: WIN.y - 20, w: WIN.w + 40, h: WIN.h + 40 }, viaOpen: true });
      const c = await connect(cover.browserWsUrl);
      const cp = await c.firstPage();
      await c.send('Page.navigate', { url: MAGENTA }, cp.sessionId);
      c.close();
      await sleep(2500);
      const px = [screenPixel(WIN.x + 150, WIN.y + 400), screenPixel(WIN.x + WIN.w - 150, WIN.y + 400)];
      if (px.some((p) => p === null)) return 'unknown (no screen capture permission)';
      return px.every((p) => p[0] > 200 && p[1] < 80 && p[2] > 200);
    },
    restore: async () => {
      if (cover) await cover.kill();
      cover = null;
      activatePid(mainPid());
    },
  },
  {
    id: 'other-space',
    made: F_.auto ? 'synthetic-os (System Events Control+Right)' : 'person',
    apply: async () => {
      activatePid(mainPid());
      await sleep(500);
      if (F_.auto) {
        spaceRight();
      } else {
        await waitEnter('After Enter you have 5 s: switch to ANOTHER Space (Control+Right or a swipe), leaving this Chrome window behind. Come back only after the chime.');
      }
      await sleep(F_.auto ? 2500 : 5000);
      return onScreen(pids().pids) === false ? true : onScreen(pids().pids) === null ? 'unknown (window list unreadable)' : false;
    },
    restore: async () => {
      if (F_.auto) {
        spaceLeft();
        await sleep(1500);
      } else {
        chime();
        console.log('>>> PERSON: come back to the Space with the Chrome window now (the script waits).');
        await waitFor(() => onScreen(pids().pids) === true, 10 * 60000, 500);
      }
      activatePid(mainPid());
    },
  },
  {
    id: 'locked',
    needs: () => F_.withLock,
    made: F_.auto ? 'synthetic-os (System Events Control+Cmd+Q), person unlocks' : 'person locks and unlocks',
    apply: async () => {
      if (F_.auto) lockScreen();
      else await waitEnter('After Enter you have 5 s: lock the screen (Control+Cmd+Q). Stay locked until the chime, then unlock.');
      return !!(await waitFor(() => screenLocked() === true, 15000, 500));
    },
    restore: async () => {
      chime();
      console.log('>>> PERSON: unlock the screen now (the script waits up to 10 min).');
      await waitFor(() => screenLocked() === false, 10 * 60000, 500);
      await sleep(1000);
      activatePid(mainPid());
    },
  },
  {
    id: 'display-sleep',
    needs: () => F_.withSleep,
    made: 'pmset displaysleepnow (woken by caffeinate -u)',
    apply: async () => {
      keep.stop();
      displaySleepNow();
      await sleep(3000);
      const r = osa(`ObjC.import('CoreGraphics'); String($.CGDisplayIsAsleep($.CGMainDisplayID()))`, { js: true });
      return r.ok ? r.out === 'true' : 'unknown';
    },
    restore: async () => {
      wakeDisplay();
      keep.start();
      await sleep(2000);
      if (screenLocked()) {
        chime();
        console.log('>>> PERSON: the display sleep locked the screen; unlock it now (the script waits up to 10 min).');
        await waitFor(() => screenLocked() === false, 10 * 60000, 500);
      }
      activatePid(mainPid());
    },
  },
];

// One configuration in the current state: 10 s of stream, then 10 captures. The stream stays on when
// `keepOn` (for V2).
async function measure(state, cfg, keepOn) {
  const s = streamOn(cfg.conn, cfg.sid);
  await s.started.catch((e) => (s.startError = e.message));
  const t0 = performance.now();
  await sleep(10000);
  const n = s.between(t0, t0 + 10000).length;
  const cap = await captures(cfg.conn, cfg.sid, 10);
  if (!keepOn) await s.stop();
  return { s, row: { config: cfg.id, streamFrames10s: n, startError: s.startError, capture: cap } };
}

function v1Line(state, made, verified, r) {
  const c = r.capture;
  const stream = r.streamFrames10s >= 10 ? 'PASS' : r.streamFrames10s > 0 ? 'FAIL (some frames)' : 'FAIL (no frames)';
  const capt = c.ok >= 9 && (c.maxMs ?? 1e9) <= 1000 ? 'PASS' : 'FAIL';
  const m1 = r.streamFrames10s === 0 && (c.of - c.ok >= 2 || (c.maxMs ?? 1e9) > 1000);
  console.log(
    `V1 ${state} ${r.config}: stream ${r.streamFrames10s} frames/10s ${stream}; capture ${c.ok}/${c.of} median ${c.medianMs} ms max ${c.maxMs} ms ${capt}; M1-condition ${m1 ? 'MET' : 'not met'} [made: ${made}; verified: ${verified}]`,
  );
}

for (const st of STATES) {
  if (!want(st.id)) continue;
  if (st.needs && !st.needs()) {
    console.log(`V1 ${st.id}: SKIPPED (needs ${st.id === 'locked' ? '--with-lock' : '--with-sleep'})`);
    out.rows.push({ state: st.id, skipped: true });
    continue;
  }
  const person = /person/.test(st.made);
  // A person state is entered once (both configurations measured inside it, V2 for B only);
  // a state made without a person is entered twice so that each configuration gets a V2 reading.
  const passes = person ? [['A', 'B']] : [['A', 'B'], ['B', 'A']];
  for (const order of passes) {
    let verified;
    try {
      verified = await st.apply();
    } catch (e) {
      verified = `apply failed: ${e.message}`;
    }
    if (verified === false) {
      console.log(`V1 ${st.id}: SKIPPED (the state could not be made: ${st.made})`);
      out.rows.push({ state: st.id, skipped: true, made: st.made, verified });
      await st.restore().catch(() => {});
      if (st.cleanup) await st.cleanup();
      break;
    }
    await sleep(3000);
    const pageBefore = await pageState();
    const results = [];
    let last = null;
    for (const [i, id] of order.entries()) {
      const keepOn = i === order.length - 1;
      const m = await measure(st.id, CONFIGS[id], keepOn);
      results.push(m.row);
      if (keepOn) last = m;
    }
    const pageDuring = await pageState();
    await st.restore().catch((e) => console.log(`restore failed: ${e.message}`));
    // V2: the stream of the last configuration was never restarted.
    await sleep(1000);
    const t0 = performance.now();
    await sleep(5000);
    const resumed = last.s.between(t0, t0 + 5000).length;
    await last.s.stop();
    if (st.cleanup) await st.cleanup();
    const row = { state: st.id, made: st.made, verified, order, pageBefore, pageDuring, results, v2: { config: last.row.config, framesIn5sAfterRestore: resumed } };
    out.rows.push(row);
    for (const r of results) v1Line(st.id, st.made, verified, r);
    console.log(`V2 ${st.id} ${last.row.config}: ${resumed} frames in 5 s after restoring, stream not restarted ${resumed > 0 ? 'PASS' : 'FAIL'}`);
    console.log(`    page (visibilityState, hasFocus) before ${JSON.stringify(pageBefore)} during ${JSON.stringify(pageDuring)}`);
    await sleep(1500);
  }
}

// From here on the window is visible and frontmost.
activatePid(mainPid());
await F.navigate(sF, anim(probe.origin, { doc: 'main' }));
await sleep(1000);

// ---- V3: frame and capture sizes ----
if (want('v3')) {
  const s = streamOn(R, sR);
  await s.started;
  await sleep(3000);
  await s.stop();
  const fr = s.frames;
  const stream = { frames: fr.length, medianBytes: median(fr.map((f) => f.bytes)), w: fr.at(-1)?.w, h: fr.at(-1)?.h };
  const plain = await captures(R, sR, 10);
  const m = await R.send('Page.getLayoutMetrics', {}, sR);
  const vv = m.cssVisualViewport;
  const scale = Math.min(1, 1280 / (vv.clientWidth * out.dpr));
  const clip = { x: vv.pageX, y: vv.pageY, width: vv.clientWidth, height: vv.clientHeight, scale };
  const clipped = await captures(R, sR, 10, { clip });
  out.v3 = { stream, plain, clip, clipped, cssViewport: [vv.clientWidth, vv.clientHeight] };
  line('V3 stream', `maxWidth 1280: ${stream.w}x${stream.h}, median ${stream.medianBytes} bytes over ${stream.frames} frames`);
  line('V3 capture plain', `${plain.w}x${plain.h}, ${plain.bytes} bytes, median ${plain.medianMs} ms max ${plain.maxMs} ms (${plain.ok}/${plain.of})`);
  line('V3 capture clip', `scale ${scale.toFixed(3)}: ${clipped.w}x${clipped.h}, ${clipped.bytes} bytes, median ${clipped.medianMs} ms max ${clipped.maxMs} ms (${clipped.ok}/${clipped.of})`);
  line('V3 capture time saved by clip', `${plain.medianMs != null && clipped.medianMs != null ? (plain.medianMs - clipped.medianMs).toFixed(1) : '?'} ms median (devicePixelRatio ${out.dpr})`);
}

// ---- V4: ack at once vs 500 ms late ----
if (want('v4')) {
  out.v4 = {};
  const c0 = await cpuSample();
  await sleep(10000);
  out.v4.noStream = cpuDelta(c0, await cpuSample());
  line('V4 no stream', JSON.stringify(out.v4.noStream));
  for (const [mode, ackDelayMs] of [['ack-now', 0], ['ack-500ms', 500]]) {
    await F.navigate(sF, anim(probe.origin, { doc: 'v4' }));
    const s = streamOn(R, sR, { ackDelayMs });
    await s.started;
    const a = await cpuSample();
    const t0 = performance.now();
    await sleep(10000);
    const cpu = cpuDelta(a, await cpuSample());
    const fps = +(s.between(t0, t0 + 10000).length / 10).toFixed(1);
    // The page changes for 1.5 s, then scrolls to 777 and stops changing.
    await F.navigate(sF, anim(probe.origin, { doc: 'v4stop', stopAfter: 1500, final: 777 }));
    const tNav = performance.now();
    await sleep(5000);
    const after = s.between(tNav, tNav + 5000);
    const lastY = after.at(-1)?.y;
    await s.stop();
    out.v4[mode] = { fps, cpu, framesAfterStopPage: after.length, lastFrameScrollY: lastY };
    line(`V4 ${mode}`, `${fps} frames/s; cpu ${JSON.stringify(cpu.cpuPercentFromCpuTime)}; last frame after the page stopped shows the final state (scrollY 777): ${lastY === 777 ? 'PASS' : `FAIL (last scrollY ${lastY})`}`);
  }
}

// ---- V5: the agent's own screencast on the same tab ----
if (want('v5')) {
  await F.navigate(sF, anim(probe.origin, { doc: 'v5' }));
  const G = await connect(chrome.browserWsUrl);
  const sG = await G.attach(page.targetId);
  await G.send('Page.enable', {}, sG);
  const s = streamOn(R, sR);
  await s.started;
  const phase = async (name, act) => {
    let agentFrames = 0;
    const gh = (p, sid) => {
      if (sid !== sG) return;
      agentFrames++;
      G.send('Page.screencastFrameAck', { sessionId: p.sessionId }, sG).catch(() => {});
    };
    G.on('Page.screencastFrame', gh);
    let error = null;
    if (act) await act().catch((e) => (error = e.message));
    const t0 = performance.now();
    await sleep(3000);
    G.off('Page.screencastFrame', gh);
    const fr = s.between(t0, t0 + 3000);
    return { name, recorderFrames: fr.length, recorderWidth: fr.at(-1)?.w, agentFrames, error };
  };
  const phases = [
    await phase('baseline'),
    await phase('agent startScreencast (maxWidth 320)', () => G.send('Page.startScreencast', { format: 'jpeg', quality: 30, maxWidth: 320 }, sG)),
    await phase('agent stopScreencast', () => G.send('Page.stopScreencast', {}, sG)),
    await phase('agent stopScreencast without a start', () => G.send('Page.stopScreencast', {}, sG)),
  ];
  await s.stop();
  G.close();
  out.v5 = phases;
  const base = phases[0].recorderFrames;
  for (const p of phases) line(`V5 ${p.name}`, `recorder ${p.recorderFrames} frames/3s (width ${p.recorderWidth}), agent ${p.agentFrames} frames${p.error ? `, error ${p.error}` : ''}`);
  const stopped = phases.slice(1).some((p) => p.recorderFrames === 0 && base > 0);
  const changed = phases.slice(1).some((p) => p.recorderWidth && phases[0].recorderWidth && p.recorderWidth !== phases[0].recorderWidth);
  line('V5 verdict', `the agent's session ${stopped ? 'CAN stop' : 'does not stop'} the recorder's stream, ${changed ? 'CAN change' : 'does not change'} its frame size`);
}

// ---- S6: order of navigation events and the new document's first frame ----
// PNG first pixel (viewport top-left: the fixed colour box that names the document).
function pngFirstPixel(b64) {
  const b = Buffer.from(b64, 'base64');
  let i = 8;
  let idat = [];
  let ct = 6;
  let bd = 8;
  while (i < b.length) {
    const n = b.readUInt32BE(i);
    const t = b.toString('ascii', i + 4, i + 8);
    const body = b.subarray(i + 8, i + 8 + n);
    if (t === 'IHDR') { bd = body[8]; ct = body[9]; }
    if (t === 'IDAT') idat.push(body);
    i += 12 + n;
  }
  const raw = zlib.inflateSync(Buffer.concat(idat));
  const step = bd / 8;
  return [raw[1], raw[1 + step], raw[1 + 2 * step]];
}
const colorOf = (px) => (px[0] > 150 && px[1] < 100 && px[2] < 100 ? 0 : px[1] > 120 && px[0] < 100 && px[2] < 100 ? 1 : px[2] > 150 && px[0] < 100 && px[1] < 100 ? 2 : -1);

if (want('s6')) {
  const KINDS = ['direct', 'redirect-302-other-host', 'script-location', 'popup-window-open'];
  const N = Number(F_.val('navs', 50));
  let seq = 0;
  const log = [];
  R.tap = (msg) => {
    seq++;
    if (msg.method === 'Page.frameRequestedNavigation' || msg.method === 'Page.frameStartedNavigating') log.push({ seq, name: msg.method.slice(5), sid: msg.sessionId, frameId: msg.params.frameId, p: msg.params });
    else if (msg.method === 'Page.frameNavigated') log.push({ seq, name: 'frameNavigated', sid: msg.sessionId, frameId: msg.params.frame.id, main: !msg.params.frame.parentId, p: { url: msg.params.frame.url, type: msg.params.type } });
    else if (msg.method === 'Page.screencastFrame') log.push({ seq, name: 'frame', sid: msg.sessionId, y: msg.params.metadata?.scrollOffsetY });
    else if (msg.id && msg.result && typeof msg.result.data === 'string') {
      // Only captures are answered with image data on R during S6.
      const px = pngFirstPixel(msg.result.data);
      log.push({ seq, name: 'capture', sid: null, color: colorOf(px), px });
    }
  };
  // Acks for every stream on R (main tab and popups).
  R.on('Page.screencastFrame', (p, sid) => R.send('Page.screencastFrameAck', { sessionId: p.sessionId }, sid).catch(() => {}));
  await F.navigate(sF, anim(probe.origin, { doc: 'start', scroll: 150, color: 2 }));
  await R.send('Page.startScreencast', { format: 'jpeg', quality: 60, maxWidth: 1280 }, sR);
  await sleep(1000);
  let host = probe.origin;
  let prevScroll = 150;
  let prevColor = 2;
  const trials = [];
  let popupSid = null;
  let popupTarget = null;
  R.on('Target.attachedToTarget', async (p) => {
    const ti = p.targetInfo;
    if (ti.type === 'page' && ti.openerId === page.targetId && p.waitingForDebugger) {
      popupSid = p.sessionId;
      popupTarget = ti.targetId;
      await R.send('Page.enable', {}, p.sessionId).catch(() => {});
      await R.send('Page.startScreencast', { format: 'jpeg', quality: 60, maxWidth: 1280 }, p.sessionId).catch(() => {});
      await R.send('Runtime.runIfWaitingForDebugger', {}, p.sessionId).catch(() => {});
    } else {
      if (p.waitingForDebugger) await R.send('Runtime.runIfWaitingForDebugger', {}, p.sessionId).catch(() => {});
      await R.send('Target.detachFromTarget', { sessionId: p.sessionId }).catch(() => {});
    }
  });
  const near = (y, s) => typeof y === 'number' && (Math.abs(y - s) <= 2 || Math.abs(y - s * out.dpr) <= 2);
  for (let i = 0; i < N; i++) {
    const kind = KINDS[i % KINDS.length];
    const scroll = 200 + i * 7;
    const color = i % 3 === prevColor ? (i + 1) % 3 : i % 3;
    const params = { doc: i, scroll, color };
    const startSeq = seq;
    let target = page.targetId;
    let rel = sR;
    const caps = [];
    if (kind === 'popup-window-open') {
      popupSid = null;
      await R.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true });
      await F.send('Runtime.evaluate', { expression: `window.open(${JSON.stringify(anim(host, params))}) && 1`, userGesture: true }, sF);
    } else {
      let nav;
      if (kind === 'direct') nav = F.send('Page.navigate', { url: anim(host, params) }, sF);
      else if (kind === 'redirect-302-other-host') {
        const to = host === probe.origin ? OTHER : probe.origin;
        nav = F.send('Page.navigate', { url: `${host}/r302?to=${encodeURIComponent(anim(to, params))}` }, sF);
        host = to;
      } else nav = F.evaluate(sF, `setTimeout(() => { location.href = ${JSON.stringify(anim(host, params))}; }, 0); 1`);
      nav.catch(() => {});
      // Direct captures sent while the navigation runs.
      for (const at of [0, 30, 100, 250]) {
        caps.push(sleep(at).then(() => R.send('Page.captureScreenshot', { format: 'png' }, sR, 10000).catch((e) => ({ error: e.message }))));
      }
    }
    // Until the new document's first frame (or 6 s).
    await waitFor(() => {
      if (kind === 'popup-window-open') {
        if (!popupSid) return false;
        rel = popupSid;
        target = popupTarget;
      }
      return log.some((e) => e.seq > startSeq && e.name === 'frame' && e.sid === rel && near(e.y, scroll));
    }, 6000, 20);
    await Promise.all(caps);
    await sleep(300);
    const evs = log.filter((e) => e.seq > startSeq && (e.sid === rel || e.name === 'capture'));
    const first = (pred) => evs.find(pred)?.seq ?? null;
    const reqSeq = first((e) => e.name === 'frameRequestedNavigation' && e.frameId === target);
    const startNavSeq = first((e) => e.name === 'frameStartedNavigating' && e.frameId === target);
    const navigatedSeq = first((e) => e.name === 'frameNavigated' && e.main && e.frameId === target);
    const newSeq = first((e) => e.name === 'frame' && near(e.y, scroll));
    const cls = (e) => (near(e.y, scroll) ? 'new' : kind !== 'popup-window-open' && near(e.y, prevScroll) ? 'old' : 'other');
    const framesStartedToNavigated = evs.filter((e) => e.name === 'frame' && startNavSeq != null && e.seq > startNavSeq && (navigatedSeq == null || e.seq < navigatedSeq)).map(cls);
    const captureRows = evs
      .filter((e) => e.name === 'capture')
      .map((e) => ({
        doc: e.color === color ? 'new' : e.color === prevColor ? 'old' : 'other',
        px: e.px,
        afterStarted: startNavSeq != null && startNavSeq < e.seq,
        afterNavigated: navigatedSeq != null && navigatedSeq < e.seq,
      }));
    const order = [['requested', reqSeq], ['started', startNavSeq], ['navigated', navigatedSeq], ['newFrame', newSeq]]
      .filter(([, s]) => s != null)
      .sort((a, b) => a[1] - b[1])
      .map(([n]) => n)
      .join('<');
    const startedEv = evs.find((e) => e.seq === startNavSeq);
    const requestedEv = evs.find((e) => e.seq === reqSeq);
    trials.push({
      i, kind, order, reqSeq, startNavSeq, navigatedSeq, newSeq,
      requestedReason: requestedEv?.p?.reason, startedNavigationType: startedEv?.p?.navigationType,
      framesStartedToNavigated, captures: captureRows,
      framesNavigatedToNew: evs.filter((e) => e.name === 'frame' && navigatedSeq != null && e.seq > navigatedSeq && (newSeq == null || e.seq < newSeq)).map(cls),
      otherFrames: evs.filter((e) => e.name === 'frame' && cls(e) === 'other').map((e) => e.y),
    });
    if (kind === 'popup-window-open') {
      await R.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }).catch(() => {});
      if (popupTarget) await F.send('Target.closeTarget', { targetId: popupTarget }).catch(() => {});
      popupTarget = null;
      await sleep(300);
    } else {
      prevScroll = scroll;
      prevColor = color;
    }
  }
  R.tap = null;
  await R.send('Page.stopScreencast', {}, sR).catch(() => {});
  out.s6 = { trials };
  const byKind = {};
  for (const t of trials) {
    const k = (byKind[t.kind] ||= { n: 0, orders: {}, reasons: {}, navigationTypes: {}, newFrameSeen: 0 });
    k.n++;
    k.orders[t.order] = (k.orders[t.order] || 0) + 1;
    k.reasons[t.requestedReason ?? '(none)'] = (k.reasons[t.requestedReason ?? '(none)'] || 0) + 1;
    k.navigationTypes[t.startedNavigationType ?? '(none)'] = (k.navigationTypes[t.startedNavigationType ?? '(none)'] || 0) + 1;
    if (t.newSeq != null) k.newFrameSeen++;
  }
  out.s6.byKind = byKind;
  for (const [kind, k] of Object.entries(byKind)) line(`S6 ${kind}`, `n ${k.n}, new frame seen ${k.newFrameSeen}; orders ${JSON.stringify(k.orders)}; requested.reason ${JSON.stringify(k.reasons)}; started.navigationType ${JSON.stringify(k.navigationTypes)}`);
  const withNew = trials.filter((t) => t.newSeq != null);
  for (const [name, key] of [['frameRequestedNavigation', 'reqSeq'], ['frameStartedNavigating', 'startNavSeq'], ['frameNavigated', 'navigatedSeq']]) {
    const before = withNew.filter((t) => t[key] != null && t[key] < t.newSeq).length;
    const missing = withNew.filter((t) => t[key] == null).length;
    line(`S6 ${name} before the first new-document frame`, `${before}/${withNew.length} (missing ${missing}) ${before === withNew.length ? 'PASS' : 'FAIL'}`);
  }
  const between = trials.flatMap((t) => t.framesStartedToNavigated);
  line('S6 frames between frameStartedNavigating and frameNavigated', JSON.stringify({ total: between.length, old: between.filter((x) => x === 'old').length, new: between.filter((x) => x === 'new').length, other: between.filter((x) => x === 'other').length }));
  const late = trials.flatMap((t) => t.framesNavigatedToNew || []);
  line('S6 frames between frameNavigated and the first new-document frame', JSON.stringify({ total: late.length, old: late.filter((x) => x === 'old').length, other: late.filter((x) => x === 'other').length }));
  const caps = trials.flatMap((t) => t.captures);
  const newBeforeNavigated = caps.filter((c) => c.doc === 'new' && !c.afterNavigated).length;
  const newBeforeStarted = caps.filter((c) => c.doc === 'new' && !c.afterStarted).length;
  line('S6 mid-navigation captures', `${caps.length}: new doc ${caps.filter((c) => c.doc === 'new').length}, old ${caps.filter((c) => c.doc === 'old').length}, other ${caps.filter((c) => c.doc === 'other').length}`);
  line('S6 capture showed the new document before frameNavigated arrived', `${newBeforeNavigated} ${newBeforeNavigated === 0 ? 'PASS' : 'FAIL'}`);
  const oldAfter = caps.filter((c) => c.doc === 'old' && c.afterNavigated).length;
  line('S6 capture showed the old document after frameNavigated arrived', `${oldAfter}`);
  line('S6 capture showed the new document before frameStartedNavigating arrived', `${newBeforeStarted} ${newBeforeStarted === 0 ? 'PASS' : 'FAIL'}`);
  const other = trials.flatMap((t) => t.otherFrames);
  line('S6 frames matching neither document', `${other.length} (scrollY values ${JSON.stringify([...new Set(other)].slice(0, 20))})`);
}

// ---- S6b: when do the old document's frames and captures stop after leaving it? ----
// For leaving a screen-off domain: frames and captures of the old document still arrive after
// frameNavigated (S6). Per kind of navigation, every mark of the new main-frame document (its loaderId:
// lifecycle events, loadEventFired, and the candidate "networkAlmostIdle and firstContentfulPaint both
// arrived") is set against the old document's frames and captures by arrival order on R's socket. Which
// document a frame or capture shows is told, as in S6, by scroll offset and the colour box.
// Kinds: direct, redirect-302-other-host, script-location (light pages), history-back (back to a page
// that may come from the back/forward cache), busy-first-paint (content shown after a 1.5 s busy loop
// that starts 0.7 s after load, so after the network is idle), long-poll (three requests always open).
// Usage: node 40-screencast.mjs --only=s6b [--navs <n per kind>] [--kinds=a,b]  (default 30 per kind, all)
if (F_.only?.includes('s6b')) {
  const ALL = ['direct', 'redirect-302-other-host', 'script-location', 'history-back', 'busy-first-paint', 'long-poll'];
  const KINDS = (F_.val('kinds', null) || ALL.join(',')).split(',');
  const PER = Number(F_.val('navs', 30));
  const CAPTURE_AT = [0, 30, 100, 250, 500, 1000, 1500, 2500, 4000];
  const WAIT_MS = 8000;
  let seq = 0;
  const log = [];
  R.tap = (msg) => {
    seq++;
    const t = performance.now();
    if (msg.sessionId !== sR) return;
    const p = msg.params;
    if (msg.method === 'Page.lifecycleEvent' && p.frameId === page.targetId) log.push({ seq, t, name: `lifecycle:${p.name}`, loaderId: p.loaderId });
    else if (msg.method === 'Page.loadEventFired') log.push({ seq, t, name: 'loadEventFired' });
    else if (msg.method === 'Page.frameNavigated' && !p.frame.parentId) log.push({ seq, t, name: 'frameNavigated', loaderId: p.frame.loaderId, type: p.type });
    else if (msg.method === 'Page.screencastFrame') log.push({ seq, t, name: 'frame', y: p.metadata?.scrollOffsetY });
  };
  R.on('Page.screencastFrame', (p, sid) => sid === sR && R.send('Page.screencastFrameAck', { sessionId: p.sessionId }, sR).catch(() => {}));
  await R.send('Page.setLifecycleEventsEnabled', { enabled: true }, sR);
  const near = (y, s) => typeof y === 'number' && (Math.abs(y - s) <= 2 || Math.abs(y - s * out.dpr) <= 2);
  let host = probe.origin;
  let prev = { scroll: 150, color: 2 };
  await F.navigate(sF, anim(host, { doc: 'start', scroll: 150, color: 2 }));
  await R.send('Page.startScreencast', { format: 'jpeg', quality: 60, maxWidth: 1280 }, sR);
  await sleep(1000);
  let docNo = 0;
  const nextDoc = (extra = {}) => {
    const i = docNo++;
    const scroll = 200 + (i % 500) * 7;
    let color = i % 3;
    if (color === prev.color) color = (color + 1) % 3;
    return { doc: i, scroll, color, ...extra };
  };
  const settle = async (scroll) => {
    const from = seq;
    await waitFor(() => log.some((e) => e.seq > from && e.name === 'frame' && near(e.y, scroll)), 6000, 20);
    await sleep(600);
  };
  // One measured navigation: `go()` starts it; `to` is the new document.
  async function trial(kind, to, go) {
    const startSeq = seq;
    const t0 = performance.now();
    const caps = [];
    go().catch(() => {});
    const sent = CAPTURE_AT.map((at) =>
      sleep(at).then(async () => {
        const c = { sentSeq: seq, sentMs: performance.now() - t0 };
        const r = await R.send('Page.captureScreenshot', { format: 'png' }, sR, 10000).catch(() => null);
        c.replySeq = seq;
        if (r) {
          const col = colorOf(pngFirstPixel(r.data));
          c.doc = col === to.color ? 'new' : col === prev.color ? 'old' : 'other';
        } else c.doc = 'failed';
        caps.push(c);
      }),
    );
    let loader = null;
    await waitFor(() => {
      const evs = log.filter((e) => e.seq > startSeq);
      loader = evs.find((e) => e.name === 'frameNavigated')?.loaderId ?? null;
      return loader && ['lifecycle:networkAlmostIdle', 'lifecycle:firstContentfulPaint'].every((n) => evs.some((e) => e.name === n && e.loaderId === loader));
    }, WAIT_MS, 20);
    await Promise.all(sent);
    await sleep(800);
    const evs = log.filter((e) => e.seq > startSeq);
    const navd = evs.find((e) => e.name === 'frameNavigated');
    const cls0 = (e) => (near(e.y, to.scroll) ? 'new' : near(e.y, prev.scroll) ? 'old' : 'other');
    const marks = {};
    if (navd) marks.frameNavigated = { seq: navd.seq, ms: navd.t - t0 };
    // How long after frameNavigated the old document's last stream frame arrived (the hold window a
    // back/forward-cache restore, which has no lifecycle events, would need).
    const lastOldAfterNav = navd ? evs.filter((e) => e.name === 'frame' && e.seq > navd.seq && cls0(e) === 'old').map((e) => e.t - navd.t) : [];
    for (const e of evs) {
      if (e.name.startsWith('lifecycle:') && navd && e.loaderId === navd.loaderId && !marks[e.name]) marks[e.name] = { seq: e.seq, ms: e.t - t0 };
      if (e.name === 'loadEventFired' && navd && e.seq > navd.seq && !marks.loadEventFired) marks.loadEventFired = { seq: e.seq, ms: e.t - t0 };
    }
    const a = marks['lifecycle:networkAlmostIdle'];
    const b = marks['lifecycle:firstContentfulPaint'];
    if (a && b) marks['networkAlmostIdle+firstContentfulPaint'] = a.seq > b.seq ? a : b;
    const frames = evs.filter((e) => e.name === 'frame').map((e) => ({ seq: e.seq, ms: e.t - t0, doc: cls0(e) }));
    const row = {
      kind,
      navigatedType: navd?.type ?? null,
      marks,
      frames,
      captures: caps,
      oldFrameLastMsAfterNavigated: lastOldAfterNav.length ? Math.max(...lastOldAfterNav) : null,
      failedCapturesSentBeforeNavigated: navd ? caps.filter((c) => c.doc === 'failed' && c.sentSeq <= navd.seq).length : null,
      failedCapturesSentAfterNavigated: navd ? caps.filter((c) => c.doc === 'failed' && c.sentSeq > navd.seq).length : null,
    };
    prev = { scroll: to.scroll, color: to.color };
    return row;
  }
  const RUN = {
    direct: async () => {
      const to = nextDoc();
      return trial('direct', to, () => F.send('Page.navigate', { url: anim(host, to) }, sF));
    },
    'redirect-302-other-host': async () => {
      const to = nextDoc();
      const dest = host === probe.origin ? OTHER : probe.origin;
      const from = host;
      host = dest;
      return trial('redirect-302-other-host', to, () => F.send('Page.navigate', { url: `${from}/r302?to=${encodeURIComponent(anim(dest, to))}` }, sF));
    },
    'script-location': async () => {
      const to = nextDoc();
      return trial('script-location', to, () => F.evaluate(sF, `setTimeout(() => { location.href = ${JSON.stringify(anim(host, to))}; }, 0); 1`));
    },
    'history-back': async () => {
      // Two pages that the back/forward cache may keep (no `no-store`), then back from the second.
      const x = nextDoc({ cache: 1 });
      await F.navigate(sF, anim(host, x));
      await settle(x.scroll);
      prev = { scroll: x.scroll, color: x.color };
      const y = nextDoc({ cache: 1 });
      await F.navigate(sF, anim(host, y));
      await settle(y.scroll);
      prev = { scroll: y.scroll, color: y.color };
      return trial('history-back', x, () => F.evaluate(sF, 'history.back(); 1'));
    },
    'busy-first-paint': async () => {
      const to = nextDoc({ busyAfter: 700, busyMs: 1500 });
      return trial('busy-first-paint', to, () => F.send('Page.navigate', { url: `${host}/late?${new URLSearchParams(to)}` }, sF));
    },
    'long-poll': async () => {
      const to = nextDoc({ poll: 3 });
      return trial('long-poll', to, () => F.send('Page.navigate', { url: `${host}/late?${new URLSearchParams(to)}` }, sF));
    },
  };
  const trials = [];
  for (let r = 0; r < PER; r++) {
    for (const kind of KINDS) {
      trials.push(await RUN[kind]());
      // Back on a light page so that every trial leaves the same kind of old document.
      const base = nextDoc();
      await F.navigate(sF, anim(host, base)).catch(() => {});
      await settle(base.scroll);
      prev = { scroll: base.scroll, color: base.color };
    }
  }
  R.tap = null;
  await R.send('Page.stopScreencast', {}, sR).catch(() => {});
  out.s6b = { trials, captureAtMs: CAPTURE_AT, waitMs: WAIT_MS };
  const MARKS = ['frameNavigated', 'loadEventFired', 'lifecycle:firstContentfulPaint', 'lifecycle:networkAlmostIdle', 'networkAlmostIdle+firstContentfulPaint', 'lifecycle:networkIdle'];
  for (const kind of KINDS) {
    const ts = trials.filter((t) => t.kind === kind);
    const types = {};
    for (const t of ts) types[t.navigatedType ?? '(none)'] = (types[t.navigatedType ?? '(none)'] || 0) + 1;
    const lastOld = ts.map((t) => t.oldFrameLastMsAfterNavigated).filter((x) => x != null);
    line(
      `S6b ${kind} old stream frames after frameNavigated`,
      `in ${lastOld.length}/${ts.length} navigations; last one arrived max ${lastOld.length ? Math.max(...lastOld).toFixed(0) : '-'} ms, median ${lastOld.length ? median(lastOld).toFixed(0) : '-'} ms after frameNavigated`,
    );
    line(
      `S6b ${kind} failed captures`,
      `sent before frameNavigated ${ts.reduce((n, t) => n + (t.failedCapturesSentBeforeNavigated || 0), 0)}, sent after ${ts.reduce((n, t) => n + (t.failedCapturesSentAfterNavigated || 0), 0)}`,
    );
    line(`S6b ${kind}`, `${ts.length} navigations; frameNavigated.type ${JSON.stringify(types)}; captures old ${ts.flatMap((t) => t.captures).filter((c) => c.doc === 'old').length}, failed ${ts.flatMap((t) => t.captures).filter((c) => c.doc === 'failed').length} of ${ts.flatMap((t) => t.captures).length}`);
    for (const name of MARKS) {
      const r = { present: 0, oldFramesAfter: 0, oldCapturesAfter: 0, capturesSentAfter: 0, oldOfCapturesSentAfter: 0, newFramesBefore: 0, ms: [] };
      for (const t of ts) {
        const m = t.marks[name];
        if (!m) continue;
        r.present++;
        r.ms.push(m.ms);
        r.oldFramesAfter += t.frames.filter((f) => f.doc === 'old' && f.seq > m.seq).length;
        r.oldCapturesAfter += t.captures.filter((c) => c.doc === 'old' && c.replySeq > m.seq).length;
        const after = t.captures.filter((c) => c.sentSeq > m.seq);
        r.capturesSentAfter += after.length;
        r.oldOfCapturesSentAfter += after.filter((c) => c.doc === 'old').length;
        r.newFramesBefore += t.frames.filter((f) => f.doc === 'new' && f.seq < m.seq).length;
      }
      const ok = r.present === ts.length && r.oldFramesAfter === 0 && r.oldCapturesAfter === 0;
      line(
        `S6b ${kind} ${name}`,
        `arrived in ${r.present}/${ts.length} (median ${r.ms.length ? median(r.ms).toFixed(0) : '-'} ms, max ${r.ms.length ? Math.max(...r.ms).toFixed(0) : '-'} ms after the navigation started); after it: old frames ${r.oldFramesAfter}, old captures ${r.oldCapturesAfter}; captures sent after it ${r.capturesSentAfter} (old ${r.oldOfCapturesSentAfter}); new frames before it (lost) ${r.newFramesBefore} ${ok ? 'HOLDS' : r.present < ts.length ? 'MISSING in some navigations' : 'LEAKS'}`,
      );
    }
  }
}

closePrompt();
console.log('saved', writeResult('40-screencast', out));
F.close();
R.close();
await chrome.kill();
probe.close();
process.exit(0);
