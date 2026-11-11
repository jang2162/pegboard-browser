// Throwaway spike helper: run the pinned agent-browser CLI in an isolated socket dir.
import { execFile } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { SPIKES } from './launch.mjs';

const BIN = join(SPIKES, 'node_modules', '.bin', 'agent-browser');
// Short path: unix socket paths have a ~104 byte limit.
export const SOCK_DIR = '/tmp/pegspike-ab';

export function ab(args, { session, cdp, cdpHeaders, env = {}, flags = [], timeoutMs = 60000 } = {}) {
  mkdirSync(SOCK_DIR, { recursive: true });
  const argv = [
    ...(session ? ['--session', session] : []),
    ...(cdp ? ['--cdp', cdp] : []),
    ...(cdpHeaders ? ['--cdp-headers', JSON.stringify(cdpHeaders)] : []),
    ...flags,
    '--json',
    ...args,
  ];
  const started = Date.now();
  return new Promise((resolve) => {
    execFile(
      BIN,
      argv,
      {
        timeout: timeoutMs,
        maxBuffer: 64 * 1024 * 1024,
        env: {
          ...process.env,
          AGENT_BROWSER_SOCKET_DIR: SOCK_DIR,
          AGENT_BROWSER_NO_VERSION_CHECK: '1',
          ...env,
        },
      },
      (err, stdout, stderr) => {
        let json = null;
        try {
          json = JSON.parse(stdout.trim().split('\n').pop());
        } catch {}
        resolve({
          code: err ? (typeof err.code === 'number' ? err.code : 1) : 0,
          stdout,
          stderr,
          json,
          ms: Date.now() - started,
          ok: !err && (json ? json.success !== false : true),
        });
      },
    );
  });
}
