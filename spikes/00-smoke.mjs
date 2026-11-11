// Spike 00: environment check.
import { launchChrome } from './lib/launch.mjs';
import { connect } from './lib/cdp.mjs';
import { startFixture } from './lib/fixture.mjs';
import { ab } from './lib/ab.mjs';

const fixture = await startFixture();
const chrome = await launchChrome({ name: '00-smoke' });
let code = 1;
try {
  const cdp = await connect(chrome.browserWsUrl);
  const version = await cdp.send('Browser.getVersion');
  console.log('product:', version.product);
  const page = await cdp.firstPage();
  await cdp.navigate(page.sessionId, `${fixture.origin}/counter`);
  const session = `smoke${Date.now() % 100000}`;
  const snap = await ab(['snapshot', '-i'], { session, cdp: String(chrome.port) });
  console.log('snapshot ok:', snap.ok, `${snap.ms}ms`);
  console.log(JSON.stringify(snap.json?.data?.snapshot));
  if (snap.stderr) console.log('stderr:', snap.stderr.slice(0, 300));
  await ab(['close'], { session });
  code = snap.ok && /button/.test(snap.stdout) && /ref/.test(snap.stdout) ? 0 : 1;
  cdp.close();
} finally {
  await chrome.kill();
  fixture.close();
}
process.exit(code);
