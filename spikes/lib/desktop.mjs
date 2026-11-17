// Phase 2b spike helpers: window states and OS-level input on macOS, for the steps a person would do.
// Everything here acts only on the Chrome this spike launched (found by its user-data-dir) or on the
// whole screen (lock, display sleep, Space switch). Input goes through System Events and CGEvent, which
// Chrome receives as real user input. osascript needs the Accessibility permission for that.
import { spawn, spawnSync } from 'node:child_process';
import { createInterface } from 'node:readline';

export function flags(argv = process.argv) {
  const has = (f) => argv.includes(f);
  const val = (name, dflt) => {
    const hit = argv.find((a) => a.startsWith(`--${name}=`)) || null;
    const i = argv.indexOf(`--${name}`);
    if (hit) return hit.slice(name.length + 3);
    if (i >= 0 && argv[i + 1] && !argv[i + 1].startsWith('--')) return argv[i + 1];
    return dflt;
  };
  const only = val('only', null);
  return { auto: has('--auto'), withLock: has('--with-lock'), withSleep: has('--with-sleep'), only: only ? only.split(',') : null, val };
}

export function osa(script, { js = false } = {}) {
  const r = spawnSync('osascript', [...(js ? ['-l', 'JavaScript'] : []), '-e', script], { encoding: 'utf8', timeout: 20000 });
  return { ok: r.status === 0, out: (r.stdout || '').trim(), err: (r.stderr || '').trim() };
}

// Every process of the Chrome started with this user-data-dir; `main` is the browser process.
export function chromePids(udd) {
  const r = spawnSync('pgrep', ['-f', '--', `--user-data-dir=${udd}`], { encoding: 'utf8' });
  const pids = (r.stdout || '').split('\n').map(Number).filter(Boolean);
  let main = null;
  for (const pid of pids) {
    const cmd = spawnSync('ps', ['-o', 'command=', '-p', String(pid)], { encoding: 'utf8' }).stdout || '';
    if (!cmd.includes('--type=') && /Chrome/.test(cmd)) main = pid;
  }
  return { pids, main };
}

// Whether some normal (layer 0) window of these processes is on the current screen. Windows on another
// Space, minimized ones and those of a locked session are not. null when the window list cannot be read.
export function onScreen(pids) {
  const r = osa(
    `ObjC.import('CoreGraphics');
const l = ObjC.castRefToObject($.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly | $.kCGWindowListExcludeDesktopElements, 0));
const out = [];
for (let i = 0; i < l.count; i++) { const d = l.objectAtIndex(i); if (ObjC.unwrap(d.objectForKey('kCGWindowLayer')) === 0) out.push(ObjC.unwrap(d.objectForKey('kCGWindowOwnerPID'))); }
JSON.stringify(out);`,
    { js: true },
  );
  if (!r.ok) return null;
  try {
    const owners = JSON.parse(r.out);
    return owners.some((p) => pids.includes(p));
  } catch {
    return null;
  }
}

export function screenLocked() {
  const r = osa(
    `ObjC.import('CoreGraphics'); const d = ObjC.deepUnwrap(ObjC.castRefToObject($.CGSessionCopyCurrentDictionary())); JSON.stringify(!!(d && d.CGSSessionScreenIsLocked));`,
    { js: true },
  );
  return r.ok ? r.out === 'true' : null;
}

export function activatePid(pid) {
  return osa(`tell application "System Events" to set frontmost of (first process whose unix id is ${pid}) to true`);
}

// Unix id of the frontmost application process, or null.
export function frontmostPid() {
  const r = osa('tell application "System Events" to unix id of first process whose frontmost is true');
  return r.ok ? Number(r.out) : null;
}

// Brings the process to the front and checks that it is; tries twice. Keystrokes go to the frontmost app,
// so a spike calls this before sending any.
export async function ensureFront(pid) {
  for (let i = 0; i < 2; i++) {
    activatePid(pid);
    await new Promise((r) => setTimeout(r, 600));
    if (frontmostPid() === pid) return true;
  }
  throw new Error(`the spike's Chrome (pid ${pid}) is not the frontmost app (frontmost: ${frontmostPid()}); no keystroke sent`);
}

// A plain app every Mac has, used as "another app" (only activated, never given input).
export function activateFinder() {
  return osa('tell application "Finder" to activate');
}

const MODS = { cmd: 'command down', ctrl: 'control down', shift: 'shift down', opt: 'option down' };
const using = (mods) => (mods.length ? ` using {${mods.map((m) => MODS[m]).join(', ')}}` : '');
export function keystroke(text, mods = []) {
  return osa(`tell application "System Events" to keystroke ${JSON.stringify(text)}${using(mods)}`);
}
export function keyCode(code, mods = []) {
  return osa(`tell application "System Events" to key code ${code}${using(mods)}`);
}

// A left click at a screen point (points, top-left origin), posted as HID events.
export function clickAt(x, y) {
  return osa(
    `ObjC.import('CoreGraphics');
const p = $.CGPointMake(${x}, ${y});
for (const t of [$.kCGEventMouseMoved, $.kCGEventLeftMouseDown, $.kCGEventLeftMouseUp]) {
  const e = $.CGEventCreateMouseEvent(null, t, p, $.kCGMouseButtonLeft);
  $.CGEventPost($.kCGHIDEventTap, e);
  delay(0.06);
}
'ok';`,
    { js: true },
  );
}

// Screen rectangle of the first UI element of the process's front window whose description is `desc`
// (e.g. Chrome's "Back" button). Searching the whole tree can take a few seconds.
export function axRect(pid, desc) {
  const r = osa(
    `const se = Application('System Events');
const p = se.processes.whose({ unixId: ${pid} })[0];
const all = p.windows[0].entireContents();
let hit = null;
for (const e of all) { try { if (e.description() === ${JSON.stringify(desc)}) { hit = e; break; } } catch (x) {} }
hit ? JSON.stringify({ pos: hit.position(), size: hit.size() }) : 'null';`,
    { js: true },
  );
  if (!r.ok || r.out === 'null') return null;
  try {
    const { pos, size } = JSON.parse(r.out);
    return { x: pos[0], y: pos[1], w: size[0], h: size[1] };
  } catch {
    return null;
  }
}

export const lockScreen = () => keystroke('q', ['ctrl', 'cmd']);
export const displaySleepNow = () => spawnSync('pmset', ['displaysleepnow']);
export const wakeDisplay = () => spawnSync('caffeinate', ['-u', '-t', '2']);
// Next / previous Space (Mission Control's Control+Right / Control+Left shortcuts).
export const spaceRight = () => keyCode(124, ['ctrl']);
export const spaceLeft = () => keyCode(123, ['ctrl']);
export const chime = () => spawn('afplay', ['/System/Library/Sounds/Glass.aiff'], { stdio: 'ignore' }).on('error', () => {});

// Keeps the display awake while the spike runs (paused around the display-sleep step).
export function keepAwake() {
  let p = null;
  const start = () => {
    if (!p) p = spawn('caffeinate', ['-d', '-i', '-w', String(process.pid)], { stdio: 'ignore' });
  };
  const stop = () => {
    if (p) {
      try { p.kill(); } catch {}
      p = null;
    }
  };
  process.on('exit', stop);
  start();
  return { start, stop };
}

let rl = null;
export function waitEnter(prompt) {
  if (!rl) rl = createInterface({ input: process.stdin, output: process.stdout });
  return new Promise((resolve) => rl.question(`\n>>> PERSON: ${prompt}\n>>> press Enter when done: `, () => resolve()));
}
export function closePrompt() {
  if (rl) rl.close();
  rl = null;
}

export async function waitFor(check, ms, every = 250) {
  const end = Date.now() + ms;
  for (;;) {
    const v = await check();
    if (v) return v;
    if (Date.now() > end) return null;
    await new Promise((r) => setTimeout(r, every));
  }
}

// Width and height of a JPEG or PNG given as base64 (from the SOF marker or the IHDR chunk).
export function imageSize(b64) {
  const b = Buffer.from(b64, 'base64');
  if (b[0] === 0x89 && b[1] === 0x50) return { w: b.readUInt32BE(16), h: b.readUInt32BE(20) };
  let i = 2;
  while (i + 9 < b.length) {
    if (b[i] !== 0xff) { i++; continue; }
    const m = b[i + 1];
    if (m >= 0xc0 && m <= 0xc3) return { w: b.readUInt16BE(i + 7), h: b.readUInt16BE(i + 5) };
    if (m === 0xd8 || m === 0x01 || (m >= 0xd0 && m <= 0xd7)) { i += 2; continue; }
    i += 2 + b.readUInt16BE(i + 2);
  }
  return null;
}

export const median = (a) => {
  if (!a.length) return null;
  const s = [...a].sort((x, y) => x - y);
  const m = Math.floor(s.length / 2);
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
};
