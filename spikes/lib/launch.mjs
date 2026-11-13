// Throwaway spike helper: launch headed Chrome for Testing on a scratch user data dir.
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, existsSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const SPIKES = resolve(dirname(fileURLToPath(import.meta.url)), '..');
export const OUT = join(SPIKES, 'out');
// CHROME wins; otherwise the newest chrome-* under ~/.agent-browser/browsers (as the README says).
function newestChrome() {
  const base = join(homedir(), '.agent-browser/browsers');
  const dirs = existsSync(base) ? readdirSync(base).filter((d) => d.startsWith('chrome-')).sort() : [];
  const dir = dirs.length ? dirs[dirs.length - 1] : 'chrome-157.0.8103.50';
  return join(base, dir, 'Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing');
}
export const CHROME = process.env.CHROME || newestChrome();

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export function writeResult(name, object) {
  mkdirSync(OUT, { recursive: true });
  const file = join(OUT, `${name}.json`);
  writeFileSync(file, JSON.stringify(object, null, 2));
  return file;
}

export async function launchChrome({ name, args = [], profileDirectory, fresh = true, startUrl = 'about:blank', win = { x: 80, y: 80, w: 1000, h: 700 }, viaOpen = false }) {
  const userDataDir = join(OUT, name, 'udd');
  if (fresh) rmSync(userDataDir, { recursive: true, force: true });
  mkdirSync(userDataDir, { recursive: true });
  const portFile = join(userDataDir, 'DevToolsActivePort');
  rmSync(portFile, { force: true });
  const argv = [
    `--user-data-dir=${userDataDir}`,
    '--remote-debugging-port=0',
    '--no-first-run',
    '--no-default-browser-check',
    `--window-size=${win.w},${win.h}`,
    `--window-position=${win.x},${win.y}`,
    ...(profileDirectory ? [`--profile-directory=${profileDirectory}`] : []),
    ...args,
    startUrl,
  ];
  // viaOpen: start through LaunchServices so the new window is activated on top of others.
  const proc = viaOpen
    ? spawn('open', ['-n', '-a', CHROME.replace(/\/Contents\/MacOS\/.*$/, ''), '--args', ...argv], { stdio: 'ignore' })
    : spawn(CHROME, argv, { stdio: 'ignore' });
  for (let i = 0; i < 150 && !existsSync(portFile); i++) await sleep(100);
  if (!existsSync(portFile)) throw new Error('DevToolsActivePort not written; Chrome did not start');
  await sleep(200);
  const [port, path] = readFileSync(portFile, 'utf8').trim().split('\n');
  return {
    proc,
    port: Number(port),
    browserWsUrl: `ws://127.0.0.1:${port}${path}`,
    userDataDir,
    async kill() {
      if (viaOpen) {
        spawnSync('pkill', ['-f', `user-data-dir=${userDataDir}`]);
        await sleep(800);
        return;
      }
      proc.kill('SIGTERM');
      for (let i = 0; i < 50 && proc.exitCode === null; i++) await sleep(100);
      if (proc.exitCode === null) proc.kill('SIGKILL');
    },
  };
}

// Read one screen pixel (points, top-left origin). Returns [r,g,b] or null when capture is unavailable.
export function screenPixel(x, y) {
  const file = join(OUT, 'pixel.png');
  const cap = spawnSync('screencapture', ['-x', '-R', `${x},${y},1,1`, '-t', 'png', file]);
  if (cap.status !== 0) return null;
  const py = `
import zlib,struct,sys
d=open(sys.argv[1],'rb').read(); i=8; idat=b''; ct=None
while i < len(d):
    n,t=struct.unpack('>I4s', d[i:i+8]); body=d[i+8:i+8+n]
    if t==b'IHDR': w,h,bd,ct=struct.unpack('>IIBB', body[:10])
    if t==b'IDAT': idat+=body
    i+=12+n
raw=zlib.decompress(idat); bpp={2:3,6:4}.get(ct,4)*(bd//8)
px=raw[1:1+bpp]; step=bd//8
print(px[0], px[step], px[2*step])
`;
  const r = spawnSync('python3', ['-I', '-c', py, file]);
  if (r.status !== 0) return null;
  return r.stdout.toString().trim().split(' ').map(Number);
}
