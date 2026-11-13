// Spike 22 (C1-C5): can an agent-profile page read or write the system clipboard, and what can stop it?
// The system clipboard (text) is saved at the start and restored at the end, also on failure and on signals.
import { spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { writeResult, sleep } from './lib/launch.mjs';
import { startProbe, html, openTwoProfiles, reporter } from './lib/probe.mjs';

const out = {};
const rec = reporter('22', out);
const runId = randomBytes(4).toString('hex');
const t0 = Date.now();
const ENV = { ...process.env, LC_ALL: 'en_US.UTF-8' };

// ---------- save / restore the real clipboard ----------
const saved = spawnSync('pbpaste', { env: ENV });
const typesBefore = spawnSync('osascript', ['-e', 'clipboard info'], { encoding: 'utf8' }).stdout.trim();
const nonText = [...typesBefore.matchAll(/«class (\w+)»/g)].some((m) => !['utf8', 'ut16'].includes(m[1])) || /picture|PNG|TIFF|alias|furl|file/i.test(typesBefore);
let restored = false;
const restore = () => {
  if (restored) return;
  restored = true;
  spawnSync('pbcopy', { input: saved.stdout, env: ENV });
};
process.on('exit', restore);
for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(sig, () => (restore(), process.exit(130)));
process.on('uncaughtException', (e) => (console.error(e), restore(), process.exit(1)));
if (nonText) {
  console.log('The clipboard holds non-text data; this spike can only restore text. Not run (copy something as plain text first).');
  restored = true;
  process.exit(3);
}
const sys = {
  set: (v) => spawnSync('pbcopy', { input: v, env: ENV }),
  get: () => spawnSync('pbpaste', { env: ENV }).stdout.toString(),
};

// ---------- probe + two profiles ----------
const probe = await startProbe();
const R = probe.routes;
const mk = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html(`
<textarea id="t" rows="2" cols="30">SELECTME-${runId}</textarea>
<button id="cw" style="width:120px;height:40px" onclick="window.__cw=null;navigator.clipboard.writeText('CW-${runId}').then(()=>window.__cw='ok',e=>window.__cw='ERR '+e.name+': '+e.message)">writeText</button>
<button id="ce" style="width:120px;height:40px" onclick="(()=>{const t=document.getElementById('t');t.focus();t.select();window.__ce=String(document.execCommand('copy'))})()">execCopy</button>
<button id="cr" style="width:120px;height:40px" onclick="window.__cr=null;navigator.clipboard.readText().then(v=>window.__cr='OK:'+v,e=>window.__cr='ERR '+e.name+': '+e.message)">readText</button>
<button id="cwi" style="width:120px;height:40px" onclick="window.__cwi=null;navigator.clipboard.write([new ClipboardItem({'text/plain':new Blob(['CWI-${runId}'],{type:'text/plain'})})]).then(()=>window.__cwi='ok',e=>window.__cwi='ERR '+e.name+': '+e.message)">write()</button>
<script>
window.__paste = null; window.__hook = null;
document.addEventListener('paste', (e) => { window.__paste = e.clipboardData.getData('text/plain'); });
document.addEventListener('copy', (e) => { if (window.__hook) { e.clipboardData.setData('text/plain', window.__hook); e.preventDefault(); } });
document.addEventListener('cut', (e) => { if (window.__hook) { e.clipboardData.setData('text/plain', window.__hook); e.preventDefault(); } });
</script>`)));
R['/clip'] = mk;
R['/blank'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html('<p>blank</p>')));
const env = await openTwoProfiles({ name: '22-clipboard', probe, seedPath: '/clip', args: ['--host-resolver-rules=MAP c.test 127.0.0.1', `--unsafely-treat-insecure-origin-as-secure=http://c.test:${probe.port}`] });
const { cdp: A, agent, human } = env;
const agentCtx = env.agentCtx;
const O = probe.origin; // 127.0.0.1
const O2 = `http://localhost:${probe.port}`;
const O3 = `http://c.test:${probe.port}`; // never prompted: used for the scope tests so a prompt embargo on 127.0.0.1 cannot confuse them
const defaultCtx = await A.send('Target.getBrowserContexts').then((r) => (r.defaultBrowserContextId === agentCtx ? 'Agent' : r.defaultBrowserContextId === env.humanCtx ? 'Human' : 'other'));
const ev = (sid, expr, gesture = false) => A.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true, userGesture: gesture }, sid).then((r) => (r.exceptionDetails ? `EXC ${r.exceptionDetails.exception?.description?.split('\n')[0] || r.exceptionDetails.text}` : r.result.value), (e) => `CDPERR ${e.message}`);
const race = (expr) => `Promise.race([${expr}, new Promise((r) => setTimeout(() => r('PENDING: no answer in 5s'), 5000))])`;
const rect = async (sid, id) => JSON.parse(await ev(sid, `JSON.stringify((()=>{const r=document.getElementById('${id}').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}})())`));
const mouseClick = async (sid, id) => {
  const { x, y } = await rect(sid, id);
  for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) await A.send('Input.dispatchMouseEvent', { type, x, y, button: 'left', clickCount: type === 'mouseMoved' ? 0 : 1 }, sid);
};
// Chrome on macOS: Meta = 4. commands are the editing commands to run for the key event.
const key = async (sid, k, code, vk, { commands, modifiers = 4 } = {}) => {
  const base = { key: k, code, windowsVirtualKeyCode: vk, modifiers, ...(commands ? { commands } : {}) };
  await A.send('Input.dispatchKeyEvent', { type: 'rawKeyDown', ...base }, sid);
  await A.send('Input.dispatchKeyEvent', { type: 'keyUp', ...base }, sid);
};
const prepare = async (sid, url) => {
  await A.navigate(sid, url);
  await ev(sid, `window.__hook=null;window.__paste=null;window.__cw=window.__cr=window.__ce=window.__cwi=undefined;1`);
};
const selectText = (sid) => ev(sid, `(()=>{const t=document.getElementById('t');t.value='SELECTME-${runId}';t.focus();t.select();return 1})()`);
const settle = () => sleep(350);
const classify = (before, after, expect) => (after === before ? 'unchanged' : expect && after === expect ? `CHANGED to expected marker` : `CHANGED to "${after.slice(0, 30)}"`);

// ---------- the write / read suite on one tab ----------
async function suite(sid, label, { url = `${O}/clip`, only = null } = {}) {
  const r = {};
  const SYS = `SYS-${runId}-${label}`;
  const run = async (name, fn, expectFromPage) => {
    if (only && !only.includes(name)) return;
    await prepare(sid, url);
    sys.set(SYS);
    const detail = await fn();
    await settle();
    const after = sys.get();
    r[name] = { systemClipboard: classify(SYS, after, expectFromPage), ...(detail !== undefined ? { page: detail } : {}) };
  };
  // WRITES
  await run('W1_writeText_evaluate_gesture', async () => ev(sid, `navigator.clipboard.writeText('W1-${runId}').then(()=>'ok',e=>'ERR '+e.name+': '+e.message)`, true), `W1-${runId}`);
  await run('W1b_writeText_evaluate_noGesture', async () => ev(sid, race(`navigator.clipboard.writeText('W1b-${runId}').then(()=>'ok',e=>'ERR '+e.name+': '+e.message)`), false), `W1b-${runId}`);
  await run('W2_writeText_realClickHandler', async () => { await mouseClick(sid, 'cw'); await settle(); return ev(sid, 'window.__cw'); }, `CW-${runId}`);
  await run('W3_execCommandCopy_evaluate_gesture', async () => { await selectText(sid); return ev(sid, `String(document.execCommand('copy'))`, true); }, `SELECTME-${runId}`);
  await run('W3b_execCommandCopy_evaluate_noGesture', async () => { await selectText(sid); return ev(sid, `String(document.execCommand('copy'))`, false); }, `SELECTME-${runId}`);
  await run('W4_execCommandCopy_realClickHandler', async () => { await mouseClick(sid, 'ce'); await settle(); return ev(sid, 'window.__ce'); }, `SELECTME-${runId}`);
  await run('W5_clipboardWrite_realClickHandler', async () => { await mouseClick(sid, 'cwi'); await settle(); return ev(sid, 'window.__cwi'); }, `CWI-${runId}`);
  await run('W6_key_Meta+C_with_commands_copy', async () => { await selectText(sid); await key(sid, 'c', 'KeyC', 67, { commands: ['copy'] }); }, `SELECTME-${runId}`);
  await run('W7_key_Meta+C_no_commands', async () => { await selectText(sid); await key(sid, 'c', 'KeyC', 67); }, `SELECTME-${runId}`);
  await run('W8_key_Meta+X_with_commands_cut', async () => { await selectText(sid); await key(sid, 'x', 'KeyX', 88, { commands: ['cut'] }); }, `SELECTME-${runId}`);
  await run('W9_key_Meta+X_no_commands', async () => { await selectText(sid); await key(sid, 'x', 'KeyX', 88); }, `SELECTME-${runId}`);
  // C5: a copy handler in the page replaces what is copied
  await run('C5_copyHandler_viaExecCommand', async () => { await ev(sid, `window.__hook='HOOK-${runId}';1`); await selectText(sid); return ev(sid, `String(document.execCommand('copy'))`, true); }, `HOOK-${runId}`);
  await run('C5b_copyHandler_viaKeyCommands', async () => { await ev(sid, `window.__hook='HOOK-${runId}';1`); await selectText(sid); await key(sid, 'c', 'KeyC', 67, { commands: ['copy'] }); }, `HOOK-${runId}`);
  // READS (system clipboard is SYS; did the page see it?)
  const seen = (v) => (v === undefined || v === null ? 'nothing' : String(v).includes(SYS) ? 'PAGE SAW THE SYSTEM CLIPBOARD' : `saw "${String(v).slice(0, 40)}"`);
  const runRead = async (name, fn) => {
    if (only && !only.includes(name)) return;
    await prepare(sid, url);
    sys.set(SYS);
    const v = await fn();
    await settle();
    r[name] = { page: seen(v), systemClipboardAfter: classify(SYS, sys.get()) };
  };
  await runRead('R1_readText_evaluate_gesture', () => ev(sid, race(`navigator.clipboard.readText().then(v=>'OK:'+v,e=>'ERR '+e.name+': '+e.message)`), true));
  await runRead('R1b_readText_evaluate_noGesture', () => ev(sid, race(`navigator.clipboard.readText().then(v=>'OK:'+v,e=>'ERR '+e.name+': '+e.message)`), false));
  await runRead('R2_readText_realClickHandler', async () => { await mouseClick(sid, 'cr'); await settle(); return ev(sid, 'window.__cr'); });
  await runRead('R3_execCommandPaste_evaluate_gesture', async () => { await ev(sid, `document.getElementById('t').value='';document.getElementById('t').focus();1`); await ev(sid, `String(document.execCommand('paste'))`, true); return ev(sid, `document.getElementById('t').value`); });
  await runRead('R4_key_Meta+V_with_commands_paste', async () => { await ev(sid, `document.getElementById('t').value='';document.getElementById('t').focus();1`); await key(sid, 'v', 'KeyV', 86, { commands: ['paste'] }); await settle(); return `${await ev(sid, `document.getElementById('t').value`)} | paste event: ${await ev(sid, 'window.__paste')}`; });
  await runRead('R5_key_Meta+V_no_commands', async () => { await ev(sid, `document.getElementById('t').value='';document.getElementById('t').focus();1`); await key(sid, 'v', 'KeyV', 86); await settle(); return `${await ev(sid, `document.getElementById('t').value`)} | paste event: ${await ev(sid, 'window.__paste')}`; });
  return r;
}
const summarize = (r) => Object.entries(r).map(([k, v]) => `${k}=${v.systemClipboard || v.page}${v.systemClipboard && v.page !== undefined ? ` (page: ${String(v.page).slice(0, 70)})` : ''}`);

const PERM_NAMES = ['clipboard-read', 'clipboard-write', 'clipboard-sanitized-write'];
const setPerm = (conn, name, setting, extra = {}) => conn.send('Browser.setPermission', { permission: { name }, setting, ...extra }).then(() => 'ok', (e) => e.message);
const permState = (sid, name) => ev(sid, `navigator.permissions.query({name:'${name}'}).then(p=>p.state,e=>'ERR '+e.message)`);

try {
  await A.send('Page.enable', {}, agent.sessionId);
  rec('ENV', { defaultBrowserContext: defaultCtx, systemClipboardBytesAtStart: saved.stdout.length, clipboardTypesAtStart: typesBefore ? 'text-only' : 'empty' });

  // ===== C1: no permission set, agent profile tab =====
  const c1 = {};
  c1.permissionStates = { read: await permState(agent.sessionId, 'clipboard-read'), write: await permState(agent.sessionId, 'clipboard-write') };
  c1.noFocusEmulation = await suite(agent.sessionId, 'c1nf', { only: ['W1_writeText_evaluate_gesture', 'W1b_writeText_evaluate_noGesture', 'R1_readText_evaluate_gesture', 'R1b_readText_evaluate_noGesture'] });
  const NOFOCUS_KEYS = ['W2_writeText_realClickHandler', 'W3_execCommandCopy_evaluate_gesture', 'W4_execCommandCopy_realClickHandler', 'W5_clipboardWrite_realClickHandler', 'W6_key_Meta+C_with_commands_copy', 'W8_key_Meta+X_with_commands_cut', 'C5_copyHandler_viaExecCommand', 'R2_readText_realClickHandler', 'R4_key_Meta+V_with_commands_paste'];
  c1.noFocusEmulationMore = await suite(agent.sessionId, 'c1nf2', { only: NOFOCUS_KEYS });
  await A.send('Emulation.setFocusEmulationEnabled', { enabled: true }, agent.sessionId);
  c1.withFocusEmulation = await suite(agent.sessionId, 'c1');
  await A.send('Emulation.setFocusEmulationEnabled', { enabled: true }, human.sessionId);
  c1.humanTabControl = await suite(human.sessionId, 'c1h', { only: ['W1_writeText_evaluate_gesture', 'R1_readText_evaluate_gesture', 'W3_execCommandCopy_evaluate_gesture'] });
  out.C1_detail = c1;
  rec('C1.states', JSON.stringify(c1.permissionStates));
  rec('C1.noFocusEmulation', summarize(c1.noFocusEmulation).join('; '));
  rec('C1.noFocusEmulation_more', summarize(c1.noFocusEmulationMore).join('; '));
  rec('C1.withFocusEmulation', summarize(c1.withFocusEmulation).join('; '));
  rec('C1.humanTabControl', summarize(c1.humanTabControl).join('; '));

  // ===== C2: Browser.setPermission / grantPermissions / resetPermissions =====
  const c2 = {};
  const S = await env.connection(); // the "policy" connection
  c2.setPermissionDenied = {};
  for (const n of PERM_NAMES) c2.setPermissionDenied[n] = await setPerm(S, n, 'denied', { origin: O, browserContextId: agentCtx });
  c2.setPermissionDeniedHumanCtx = await setPerm(S, 'clipboard-read', 'denied', { origin: O, browserContextId: env.humanCtx });
  for (const n of ['clipboard-read', 'clipboard-write']) await setPerm(S, n, 'denied', { origin: O3, browserContextId: agentCtx });
  c2.grantPermissionsEnumValues = await S.send('Browser.grantPermissions', { permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite'], origin: O, browserContextId: agentCtx }).then(() => 'ok (grant only; there is no deny here)', (e) => e.message);
  // grant just put them to granted; set denied again for the test
  for (const n of PERM_NAMES) await setPerm(S, n, 'denied', { origin: O, browserContextId: agentCtx });
  c2.statesAfterDeny = { read: await permState(agent.sessionId, 'clipboard-read'), write: await permState(agent.sessionId, 'clipboard-write') };
  c2.deniedSuite = await suite(agent.sessionId, 'c2');
  // scope: states are read with permissions.query (not affected by focus); writes are attempted where focus is emulated
  const st = async (sid) => ({ read: await permState(sid, 'clipboard-read'), write: await permState(sid, 'clipboard-write') });
  const tryWrite = (sid) => ev(sid, race(`navigator.clipboard.writeText('x').then(()=>'ok',e=>'ERR '+e.name+': '+e.message.slice(-40))`), true);
  await A.send('Emulation.setFocusEmulationEnabled', { enabled: true }, human.sessionId);
  const scope = {};
  scope.sameOriginNewTabSameProfile = await (async () => { const t = await env.newIn(agentCtx, `${O3}/clip`); await A.send('Emulation.setFocusEmulationEnabled', { enabled: true }, t.sessionId); const v = { states: await st(t.sessionId), writeText: await tryWrite(t.sessionId) }; await A.send('Target.closeTarget', { targetId: t.targetId }); return v; })();
  await A.navigate(agent.sessionId, `${O2}/clip`);
  scope.otherOriginSameProfile = { states: await st(agent.sessionId), writeText: await tryWrite(agent.sessionId) };
  await A.navigate(human.sessionId, `${O3}/clip`);
  scope.humanTabSameOrigin = { states: await st(human.sessionId), writeText: await tryWrite(human.sessionId) };
  await A.navigate(agent.sessionId, `${O3}/clip`);
  scope.agentTabBeforeSetterCloses = { states: await st(agent.sessionId), writeText: await tryWrite(agent.sessionId) };
  S.close();
  await sleep(700);
  await A.navigate(agent.sessionId, `${O3}/clip`);
  scope.agentTabAfterSetterConnectionClosed = { states: await st(agent.sessionId), writeText: await tryWrite(agent.sessionId) };
  // without an origin: all origins of the context?
  const S2 = await env.connection();
  for (const n of ['clipboard-read', 'clipboard-write']) await setPerm(S2, n, 'denied', { browserContextId: agentCtx }); // no origin given
  await A.navigate(agent.sessionId, `${O2}/clip`);
  scope.noOriginGiven_otherOrigin = { states: await st(agent.sessionId), writeText: await tryWrite(agent.sessionId) };
  c2.resetPermissions = await S2.send('Browser.resetPermissions', { browserContextId: agentCtx }).then(() => 'ok', (e) => e.message);
  await A.navigate(agent.sessionId, `${O2}/clip`);
  scope.afterResetByS2_otherOrigin = { states: await st(agent.sessionId), writeText: await tryWrite(agent.sessionId) };
  await A.navigate(agent.sessionId, `${O3}/clip`);
  scope.afterResetByS2_originSetByClosedS_O3 = { states: await st(agent.sessionId), writeText: await tryWrite(agent.sessionId) };
  c2.stateAfterReset = scope.afterResetByS2_originSetByClosedS_O3.states;
  S2.close();
  c2.scope = scope;
  out.C2_detail = c2;
  rec('C2.setPermission', JSON.stringify(c2.setPermissionDenied) + ` | with the Human context id: ${c2.setPermissionDeniedHumanCtx} | grantPermissions: ${c2.grantPermissionsEnumValues} | resetPermissions: ${c2.resetPermissions}`);
  rec('C2.statesAfterDeny', JSON.stringify(c2.statesAfterDeny));
  rec('C2.deniedSuite', summarize(c2.deniedSuite).join('; '));
  rec('C2.scope', JSON.stringify(scope) + ` | state after resetPermissions: ${JSON.stringify(c2.stateAfterReset)}`);

  // ===== C3 / C4 / C5 summary lines from the suites =====
  const pick = (s, keys) => keys.map((k) => `${k}=${s[k]?.systemClipboard || s[k]?.page}`).join(', ');
  rec('C3', `execCommand('copy') default [${pick(c1.withFocusEmulation, ['W3_execCommandCopy_evaluate_gesture', 'W3b_execCommandCopy_evaluate_noGesture', 'W4_execCommandCopy_realClickHandler'])}] and with all clipboard permissions denied [${pick(c2.deniedSuite, ['W3_execCommandCopy_evaluate_gesture', 'W3b_execCommandCopy_evaluate_noGesture', 'W4_execCommandCopy_realClickHandler'])}]; execCommand('paste') default [${pick(c1.withFocusEmulation, ['R3_execCommandPaste_evaluate_gesture'])}] denied [${pick(c2.deniedSuite, ['R3_execCommandPaste_evaluate_gesture'])}]`);
  rec('C4', `default [${pick(c1.withFocusEmulation, ['W6_key_Meta+C_with_commands_copy', 'W7_key_Meta+C_no_commands', 'W8_key_Meta+X_with_commands_cut', 'W9_key_Meta+X_no_commands', 'R4_key_Meta+V_with_commands_paste', 'R5_key_Meta+V_no_commands'])}]; denied [${pick(c2.deniedSuite, ['W6_key_Meta+C_with_commands_copy', 'W7_key_Meta+C_no_commands', 'W8_key_Meta+X_with_commands_cut', 'W9_key_Meta+X_no_commands', 'R4_key_Meta+V_with_commands_paste', 'R5_key_Meta+V_no_commands'])}]`);
  rec('C5', `default [${pick(c1.withFocusEmulation, ['C5_copyHandler_viaExecCommand', 'C5b_copyHandler_viaKeyCommands'])}]; denied [${pick(c2.deniedSuite, ['C5_copyHandler_viaExecCommand', 'C5b_copyHandler_viaKeyCommands'])}]`);

  // ===== extra: can an injected override stop execCommand('copy')? =====
  const extra = {};
  const X = await env.connection(); // a separate "pegboardd" connection injects the override into the agent tab
  const xs = await X.attach(agent.targetId);
  await X.send('Page.enable', {}, xs);
  const injected = await X.send('Page.addScriptToEvaluateOnNewDocument', { source: `Document.prototype.execCommand = function(){ return false; };` }, xs);
  await prepare(agent.sessionId, `${O}/clip`);
  extra.overrideActiveInPage = await ev(agent.sessionId, `String(Document.prototype.execCommand).includes('return false')`);
  sys.set(`SYS-${runId}-x`);
  await mouseClick(agent.sessionId, 'ce');
  await settle();
  extra.realClickWithOverride = { pageSaw: await ev(agent.sessionId, 'window.__ce'), system: classify(`SYS-${runId}-x`, sys.get(), `SELECTME-${runId}`) };
  await selectText(agent.sessionId);
  sys.set(`SYS-${runId}-x`);
  extra.keyCommandsWithOverride = null;
  await key(agent.sessionId, 'c', 'KeyC', 67, { commands: ['copy'] });
  await settle();
  extra.keyCommandsWithOverride = classify(`SYS-${runId}-x`, sys.get(), `SELECTME-${runId}`);
  extra.iframeInheritsOverride = await ev(agent.sessionId, `(()=>{const f=document.createElement('iframe');document.body.appendChild(f);return String(f.contentDocument.execCommand('copy'))})()`, true);
  extra.srcdocIframeInheritsOverride = await ev(agent.sessionId, `new Promise(r=>{const f=document.createElement('iframe');f.srcdoc='<textarea>a</textarea>';f.onload=()=>r(String(f.contentDocument.execCommand('copy')));document.body.appendChild(f)})`, true);
  X.close();
  await sleep(600);
  await prepare(agent.sessionId, `${O}/clip`);
  extra.overrideActiveAfterInjectingConnectionClosed = await ev(agent.sessionId, `String(Document.prototype.execCommand).includes('return false')`);
  sys.set(`SYS-${runId}-x`);
  await mouseClick(agent.sessionId, 'ce');
  await settle();
  extra.afterInjectingConnectionClosed_realClick = { pageSaw: await ev(agent.sessionId, 'window.__ce'), system: classify(`SYS-${runId}-x`, sys.get(), `SELECTME-${runId}`) };
  out.extra_detail = extra;
  rec('C3.override', JSON.stringify(extra));
} finally {
  restore();
  out._meta = { when: new Date().toISOString(), elapsedS: Math.round((Date.now() - t0) / 1000), clipboardRestored: true };
  rec('Clipboard', `restored (${saved.stdout.length} bytes of text); current system clipboard equals the saved one: ${sys.get() === saved.stdout.toString()}; contains a pbclip-run marker: ${sys.get().includes(runId)}`);
  console.log('saved', writeResult('22-clipboard', out));
  await env.close();
  probe.close();
}
process.exit(0);
