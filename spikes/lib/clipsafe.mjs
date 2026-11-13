// Save/restore the real (text) system clipboard, also on failure and signals. Shared by the clipboard spikes.
import { spawnSync, spawn } from 'node:child_process';
const ENV = { ...process.env, LC_ALL: 'en_US.UTF-8' };
export function clipboardGuard() {
  const saved = spawnSync('pbpaste', { env: ENV });
  const types = spawnSync('osascript', ['-e', 'clipboard info'], { encoding: 'utf8' }).stdout.trim();
  const nonText = [...types.matchAll(/«class (\w+)»/g)].some((m) => !['utf8', 'ut16'].includes(m[1])) || /picture|PNG|TIFF|alias|furl|file/i.test(types);
  let restored = false;
  const restore = () => { if (restored) return; restored = true; spawnSync('pbcopy', { input: saved.stdout, env: ENV }); };
  process.on('exit', restore);
  for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(sig, () => (restore(), process.exit(130)));
  process.on('uncaughtException', (e) => (console.error(e), restore(), process.exit(1)));
  if (nonText) { console.log('The clipboard holds non-text data; spike not run (copy plain text first).'); restored = true; process.exit(3); }
  const sys = { set: (v) => spawnSync('pbcopy', { input: v, env: ENV }), get: () => spawnSync('pbpaste', { env: ENV }).stdout.toString() };
  return { saved, sys, restore, savedBytes: saved.stdout.length, equalsSaved: () => sys.get() === saved.stdout.toString() };
}
// NSPasteboard change counter through one long-running JXA process that prints "<count> <epochMs>" on every change.
export function startChangeWatcher(intervalSec = 0.01) {
  const src = `ObjC.import('AppKit');ObjC.import('Foundation');const pb=$.NSPasteboard.generalPasteboard;let last=-1;while(true){const c=pb.changeCount;if(c!==last){last=c;console.log(c+' '+Date.now());}$.NSThread.sleepForTimeInterval(${intervalSec});}`;
  const p = spawn('osascript', ['-l', 'JavaScript', '-e', src], { stdio: ['ignore', 'pipe', 'pipe'] });
  const events = [];
  let buf = '';
  const parse = (d) => { buf += d; let i; while ((i = buf.indexOf('\n')) >= 0) { const [c, t] = buf.slice(0, i).trim().split(' ').map(Number); buf = buf.slice(i + 1); if (c) events.push({ count: c, t }); } };
  p.stdout.on('data', parse); p.stderr.on('data', parse); // osascript prints console.log to stderr
  process.on('exit', () => { try { p.kill('SIGKILL'); } catch {} });
  return { events, stop: () => { try { p.kill('SIGKILL'); } catch {} }, last: () => events[events.length - 1] };
}
