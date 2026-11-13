// Spike 23 (round 2, A1-A6 + G cut): which injected defences stop the clipboard write/read paths, and which bypasses exist.
// Agent side = a connection that may only use methods in the proxy's ALLOWED_METHODS (filter.rs, read not run).
// pegboardd side (P) = env.cdp, installs the defences. The system clipboard (text) is saved and restored.
import { randomBytes } from 'node:crypto';
import { writeResult, sleep } from './lib/launch.mjs';
import { startProbe, html, openTwoProfiles, reporter, parseArgs } from './lib/probe.mjs';
import { clipboardGuard, startChangeWatcher } from './lib/clipsafe.mjs';

const out = {};
const rec = reporter('23', out);
const runId = randomBytes(4).toString('hex');
const only = parseArgs().only;
const want = (k) => !only || only.includes(k);
const guard = clipboardGuard();
const { sys } = guard;

const probe = await startProbe();
const R = probe.routes;
const NAMES = ['copy', 'cut', 'paste', 'beforecopy', 'beforecut', 'beforepaste'];
const page = (extraHead = '') => (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(`<!doctype html><html><head><meta charset="utf-8"><title>c</title>
<script>window.__first={defL:!!window.__defL,defO:String(Document.prototype.execCommand).includes('return false'),t:Date.now()};window.__msgs=[];if(parent!==window)parent.postMessage({first:window.__first},'*');window.addEventListener('message',e=>window.__msgs.push(e.data));</script>${extraHead}
<style>body{margin:0}#t{position:absolute;left:0;top:0;width:250px;height:40px}button{position:absolute;left:0;width:120px;height:40px}iframe{position:absolute;left:300px;top:0;width:400px;height:300px;border:0}</style></head><body>
<textarea id="t">SELECTME-${runId}</textarea>
<button id="ce" style="top:60px" onclick="const t=document.getElementById('t');t.focus();t.select();const r=document.execCommand('copy');window.__ce=String(r);parent.postMessage({ce:String(r)},'*')">copy</button>
<button id="cx" style="top:110px" onclick="const t=document.getElementById('t');t.value='SELECTME-${runId}';t.focus();t.select();const r=document.execCommand('cut');window.__cx=String(r);parent.postMessage({cx:String(r)},'*')">cut</button>
<button id="cw" style="top:160px" onclick="window.__cw=null;navigator.clipboard.writeText('CW-${runId}').then(()=>window.__cw='ok',e=>window.__cw='ERR '+e.name)">writeText</button>
<script>window.__paste=null;window.__hook=null;
document.addEventListener('paste',e=>{window.__paste=e.clipboardData.getData('text/plain')});
document.addEventListener('copy',e=>{if(window.__hook){e.clipboardData.setData('text/plain',window.__hook);e.preventDefault()}});
document.addEventListener('cut',e=>{if(window.__hook){e.clipboardData.setData('text/plain',window.__hook);e.preventDefault()}});</script>
${probe.xframe ? `<iframe id="xf" src="${probe.xframe}/clip"></iframe>` : ''}</body></html>`));
R['/clip'] = page();
R['/blank'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html('<p>b</p>')));
const env = await openTwoProfiles({ name: '23-clip', probe, seedPath: '/clip', args: ['--host-resolver-rules=MAP c.test 127.0.0.1'] });
probe.xframe = `http://c.test:${probe.port}`;
R['/clipx'] = page();
const P = env.cdp, ps = env.agent.sessionId, agentCtx = env.agentCtx;
const AG = await env.connection();
const ags = await AG.attach(env.agent.targetId);
await AG.send('Page.enable', {}, ags);
const O = probe.origin;
const FRAME = `${O}/clipx`;
const ev = (sid, expr, gesture = false, conn = AG, extra = {}) => conn.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true, userGesture: gesture, ...extra }, sid).then((r) => (r.exceptionDetails ? `EXC ${r.exceptionDetails.exception?.description?.split('\n')[0] || r.exceptionDetails.text}` : r.result.value), (e) => `CDPERR ${e.message}`);
await P.send('Emulation.setFocusEmulationEnabled', { enabled: true }, ps); // setup by the pegboard side
const settle = () => sleep(350);
const rectOf = async (sid, id, conn = AG) => JSON.parse(await ev(sid, `JSON.stringify((()=>{const r=document.getElementById('${id}').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}})())`, false, conn));
const clickAt = async (sid, x, y, conn = AG, button = 'left') => { for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) await conn.send('Input.dispatchMouseEvent', { type, x, y, button, clickCount: type === 'mouseMoved' ? 0 : 1 }, sid); };
const click = async (sid, id, conn = AG) => { const { x, y } = await rectOf(sid, id, conn); await clickAt(sid, x, y, conn); };
const keyEv = async (sid, k, code, vk, { commands, modifiers = 4, type = 'rawKeyDown', extra = {}, conn = AG } = {}) => {
  const base = { key: k, code, windowsVirtualKeyCode: vk, modifiers, ...(commands ? { commands } : {}), ...extra };
  await conn.send('Input.dispatchKeyEvent', { type, ...base }, sid);
  await conn.send('Input.dispatchKeyEvent', { type: 'keyUp', ...base }, sid);
};
const selectT = (sid, conn = AG) => ev(sid, `(()=>{const t=document.getElementById('t');t.value='SELECTME-${runId}';t.focus();t.select();return 1})()`, false, conn);
const SEL = `SELECTME-${runId}`;
const classify = (before, after, exp) => (after === before ? 'unchanged' : exp && after === exp ? 'CHANGED(expected text)' : `CHANGED("${after.slice(0, 24)}")`);

// ---- defences (installed by P on the agent tab's session) ----
const DEF = {
  none: [],
  L_naive: [`(()=>{const f=(e)=>{e.preventDefault();e.stopImmediatePropagation()};for(const t of [window,document])for(const n of ${JSON.stringify(NAMES)})t.addEventListener(n,f,true);window.__defL=1})()`],
  L_hard: [`(()=>{const c=Function.prototype.call,pd=Event.prototype.preventDefault,sip=Event.prototype.stopImmediatePropagation;const f=(e)=>{c.call(pd,e);c.call(sip,e)};for(const t of [window,document])for(const n of ${JSON.stringify(NAMES)})t.addEventListener(n,f,true);window.__defL=1})()`],
  O_naive: [`Document.prototype.execCommand=function(){return false}`],
  O_hard: [`(()=>{const o=function execCommand(){return false};Object.defineProperty(Document.prototype,'execCommand',{value:o,writable:false,configurable:false})})()`],
};
DEF.L_hard2 = [`(()=>{const ap=Reflect.apply,pd=Event.prototype.preventDefault,sip=Event.prototype.stopImmediatePropagation,ael=EventTarget.prototype.addEventListener;const f=(e)=>{ap(pd,e,[]);ap(sip,e,[])};for(const t of [window,document])for(const n of ${JSON.stringify(NAMES)})ap(ael,t,[n,f,true]);window.__defL=1})()`];
DEF.L_hard_O_hard = [...DEF.L_hard, ...DEF.O_hard];
let installed = []; // {sessionId, identifier}
async function install(name, conn = P, sid = ps) {
  for (const source of DEF[name]) {
    const r = await conn.send('Page.addScriptToEvaluateOnNewDocument', { source }, sid);
    installed.push({ conn, sid, id: r.identifier });
  }
}
async function uninstall() { for (const i of installed) await i.conn.send('Page.removeScriptToEvaluateOnNewDocument', { identifier: i.id }, i.sid).catch(() => {}); installed = []; }
await P.send('Page.enable', {}, ps);

async function load(url = `${O}/clip`, sid = ags, conn = AG) {
  await conn.navigate(sid, url);
  await ev(sid, `window.__hook=null;window.__paste=null;1`, false, conn);
}
async function trial(def, name, act, expect = SEL, url) {
  await uninstall(); await install(def);
  await load(url);
  sys.set(`SYS-${runId}`);
  const detail = await act();
  await settle();
  return { sys: classify(`SYS-${runId}`, sys.get(), expect), ...(detail !== undefined ? { page: detail } : {}) };
}
const sum = (r) => Object.entries(r).map(([k, v]) => `${k}=${v.sys}${v.page !== undefined ? `(${String(v.page).slice(0, 60)})` : ''}`).join('; ');

// ---------- the action table used by A1 ----------
const ACT = {
  a_click_execCopy: async () => { await click(ags, 'ce'); await settle(); return ev(ags, 'window.__ce'); },
  b_click_execCut: async () => { await click(ags, 'cx'); await settle(); return ev(ags, 'window.__cx'); },
  c_evalGesture_execCopy: async () => { await selectT(ags); return ev(ags, `String(document.execCommand('copy'))`, true); },
  d_evalGesture_execCut: async () => { await selectT(ags); return ev(ags, `String(document.execCommand('cut'))`, true); },
  'e_key_Meta+C_commands': async () => { await selectT(ags); await keyEv(ags, 'c', 'KeyC', 67, { commands: ['copy'] }); },
  'f_key_Meta+X_commands': async () => { await selectT(ags); await keyEv(ags, 'x', 'KeyX', 88, { commands: ['cut'] }); },
  g_key_paste_commands: async () => { await ev(ags, `document.getElementById('t').value='';document.getElementById('t').focus();1`); await keyEv(ags, 'v', 'KeyV', 86, { commands: ['paste'] }); await settle(); return `textarea="${(await ev(ags, `document.getElementById('t').value`)).slice(0, 30)}" pasteEvent="${String(await ev(ags, 'window.__paste')).slice(0, 30)}"`; },
  h_pageCopyHandler_click: async () => { await ev(ags, `window.__hook='HOOK-${runId}';1`); await click(ags, 'ce'); await settle(); },
  i_pageCopyHandler_keyCommands: async () => { await ev(ags, `window.__hook='HOOK-${runId}';1`); await selectT(ags); await keyEv(ags, 'c', 'KeyC', 67, { commands: ['copy'] }); },
  j_click_writeText_API: async () => { await click(ags, 'cw'); await settle(); return ev(ags, 'window.__cw'); },
};
const EXPECT = { h_pageCopyHandler_click: `HOOK-${runId}`, i_pageCopyHandler_keyCommands: `HOOK-${runId}`, j_click_writeText_API: `CW-${runId}`, g_key_paste_commands: `SYS-${runId}` };
const SYSVAL = `SYS-${runId}`;
const seenPaste = (r) => (/SYS-/.test(r.page || '') ? 'PAGE READ SYSTEM CLIPBOARD' : 'page did not read it');

try {
  rec('RUN', { id: 'run-' + runId, version: env.version });
  // ================= A1 =================
  if (want('a1')) {
    const a1 = {};
    for (const def of ['none', 'L_naive', 'L_hard', 'O_hard', 'L_hard_O_hard']) {
      a1[def] = {};
      for (const [k, fn] of Object.entries(ACT)) a1[def][k] = await trial(def, k, fn, EXPECT[k] || SEL);
      for (const k of Object.keys(a1[def])) if (k === 'g_key_paste_commands') a1[def][k].read = seenPaste(a1[def][k]);
      rec(`A1.${def}`, sum(a1[def]) + ` | paste: ${a1[def].g_key_paste_commands.read}`);
    }
    out.A1 = a1;
  }
  // ================= A2 =================
  if (want('a2')) {
    const a2 = {};
    const exec = (sid, expr, g = true) => ev(sid, expr, g);
    const bt = async (def, name, fn, expect = SEL, url) => { a2[`${def}:${name}`] = await trial(def, name, fn, expect, url); };
    // (b1) undo it in the main world
    await bt('O_hard', 'b1_delete_or_redefine_execCommand', async () => { await selectT(ags); return exec(ags, `(()=>{let a,b,c;try{a=delete Document.prototype.execCommand}catch(e){a='ERR '+e.message};try{Document.prototype.execCommand=Object.getPrototypeOf(document).constructor.prototype.execCommand;b='assigned'}catch(e){b='ERR '+e.message};c=String(document.execCommand('copy'));return JSON.stringify({deleted:a,assigned:b,copy:c})})()`); });
    // (b2..b5) fresh frames: is the injection present there?
    const frameTest = (kind) => async () => { await selectT(ags); return exec(ags, `new Promise(r=>{const f=document.createElement('iframe');${kind === 'about' ? '' : kind === 'same' ? `f.src='/blank';` : kind === 'srcdoc' ? `f.srcdoc='<textarea id=q>Z</textarea>';` : `f.setAttribute('sandbox','allow-same-origin allow-scripts');f.srcdoc='<textarea id=q>Z</textarea>';`}const go=()=>{try{const d=f.contentDocument;const ta=d.createElement('textarea');ta.value='${SEL}';d.body.appendChild(ta);ta.focus();ta.select();r(JSON.stringify({execIsOverride:String(f.contentWindow.Document.prototype.execCommand).includes('return false'),defL:!!f.contentWindow.__defL,copy:String(d.execCommand('copy'))}))}catch(e){r('EXC '+e.message)}};if(${kind === 'about'})(document.body.appendChild(f),go());else{f.onload=go;document.body.appendChild(f)}})`); };
    for (const k of ['about', 'same', 'srcdoc', 'sandbox']) await bt('L_hard_O_hard', `b2_iframe_${k}`, frameTest(k));
    for (const k of ['about', 'same']) await bt('O_hard', `b2o_iframe_${k}`, frameTest(k));
    for (const k of ['about', 'same']) await bt('L_hard', `b2l_iframe_${k}`, frameTest(k));
    // sandboxed WITHOUT allow-same-origin: its own script does the copy from a real click
    await bt('L_hard_O_hard', 'b2_iframe_sandbox_opaque_click', async () => {
      await ev(ags, `(()=>{const f=document.createElement('iframe');f.id='sbx';f.setAttribute('sandbox','allow-scripts');f.style.cssText='position:absolute;left:300px;top:320px;width:300px;height:100px;border:0';f.srcdoc='<body style="margin:0"><textarea id=q style="width:100px;height:30px">${SEL}</textarea><button id=b style="position:absolute;left:0;top:50px;width:100px;height:40px" onclick="const q=document.getElementById(\\'q\\');q.focus();q.select();parent.postMessage({sbx:String(document.execCommand(\\'copy\\')),defL:!!window.__defL},\\'*\\')">c</button>';document.body.appendChild(f);return 1})()`);
      await sleep(500); await clickAt(ags, 350, 320 + 70); await settle();
      return ev(ags, `JSON.stringify(window.__msgs)`);
    });
    // (b6) a window opened with window.open: a different target; only the agent tab's session has the injection
    await bt('L_hard_O_hard', 'b6_window_open_then_copy_from_opener', async () => {
      const r = await exec(ags, `new Promise(res=>{const w=window.open('/blank');const t=setInterval(()=>{try{if(w.document.readyState==='complete'&&w.document.body){clearInterval(t);const ta=w.document.createElement('textarea');ta.value='${SEL}';w.document.body.appendChild(ta);ta.focus();ta.select();res(JSON.stringify({execIsOverride:String(w.Document.prototype.execCommand).includes('return false'),copy:String(w.document.execCommand('copy'))}))}}catch(e){clearInterval(t);res('EXC '+e.message)}},100)})`);
      return r;
    });
    // (b7) shadow root
    await bt('L_hard', 'b7_shadowroot_textarea', async () => exec(ags, `(()=>{const h=document.createElement('div');document.body.appendChild(h);const s=h.attachShadow({mode:'open'});const ta=document.createElement('textarea');ta.value='${SEL}';s.appendChild(ta);ta.focus();ta.select();return String(document.execCommand('copy'))})()`));
    // (b8) isolated world: its own JS realm, shared DOM listeners
    const isoTest = (def) => async () => {
      const { frameTree } = await AG.send('Page.getFrameTree', {}, ags);
      const { executionContextId } = await AG.send('Page.createIsolatedWorld', { frameId: frameTree.frame.id, worldName: 'agent-iso' }, ags);
      await selectT(ags);
      const r = await ev(ags, `String(document.execCommand('copy'))`, true, AG, { contextId: executionContextId });
      return `isolated world execCommand -> ${r}`;
    };
    await bt('O_hard', 'b8_isolatedWorld_execCopy', isoTest());
    await bt('L_hard', 'b8l_isolatedWorld_execCopy', isoTest());
    await bt('L_hard_O_hard', 'b8lo_isolatedWorld_execCopy', isoTest());
    // (b9) the agent registers its own new-document script; order vs pegboard's
    const orderTest = async (agentFirst) => {
      await uninstall();
      const key = `res-${agentFirst}`;
      const agentSrc = `window.__pristine=Document.prototype.execCommand;window.__agentRan=(window.__agentRan||0)+1;`;
      let aId, pId;
      if (agentFirst) aId = (await AG.send('Page.addScriptToEvaluateOnNewDocument', { source: agentSrc }, ags)).identifier;
      for (const source of DEF.O_naive) pId = (await P.send('Page.addScriptToEvaluateOnNewDocument', { source }, ps)).identifier;
      if (!agentFirst) aId = (await AG.send('Page.addScriptToEvaluateOnNewDocument', { source: agentSrc }, ags)).identifier;
      await load(); sys.set(SYSVAL);
      await selectT(ags);
      const r = await ev(ags, `JSON.stringify({pristineIsOverride:String(window.__pristine).includes('return false'),currentIsOverride:String(Document.prototype.execCommand).includes('return false'),viaPristine:String(window.__pristine.call(document,'copy'))})`, true);
      await settle();
      await AG.send('Page.removeScriptToEvaluateOnNewDocument', { identifier: aId }, ags).catch(() => {});
      await P.send('Page.removeScriptToEvaluateOnNewDocument', { identifier: pId }, ps).catch(() => {});
      return { sys: classify(SYSVAL, sys.get(), SEL), page: r };
    };
    a2['b9_agent_script_registered_AFTER_pegboard (O_naive)'] = await orderTest(false);
    a2['b9_agent_script_registered_BEFORE_pegboard (O_naive)'] = await orderTest(true);
    // (b9d) is the order by registration time or by session age? pegboard registers through a session created AFTER the agent's session
    {
      await uninstall();
      const agentSrc = `window.__pristine=Document.prototype.execCommand;`;
      const aId = (await AG.send('Page.addScriptToEvaluateOnNewDocument', { source: agentSrc }, ags)).identifier;
      const ps2 = await P.attach(env.agent.targetId); // younger session than ags
      const pId = (await P.send('Page.addScriptToEvaluateOnNewDocument', { source: DEF.O_naive[0] }, ps2)).identifier;
      await load(); sys.set(SYSVAL); await selectT(ags);
      const r = await ev(ags, `JSON.stringify({pristineIsOverride:String(window.__pristine).includes('return false'),viaPristine:String(window.__pristine.call(document,'copy'))})`, true);
      await settle();
      a2['b9d_agent_session_older_registered_first_pegboard_via_younger_session (O_naive)'] = { sys: classify(SYSVAL, sys.get(), SEL), page: r };
      await AG.send('Page.removeScriptToEvaluateOnNewDocument', { identifier: aId }, ags).catch(() => {});
      await P.send('Page.removeScriptToEvaluateOnNewDocument', { identifier: pId }, ps2).catch(() => {});
      await P.send('Target.detachFromTarget', { sessionId: ps2 }).catch(() => {});
    }
    // (b9c) runImmediately
    await uninstall(); await install('O_hard'); await load(); sys.set(SYSVAL); await selectT(ags);
    const ri = await AG.send('Page.addScriptToEvaluateOnNewDocument', { source: `window.__ri=String(Document.prototype.execCommand).includes('return false')`, runImmediately: true }, ags).then((r) => 'ok', (e) => e.message);
    a2['b9c_runImmediately_after_override'] = { sys: classify(SYSVAL, sys.get(), SEL), page: `${ri}; sees override: ${await ev(ags, 'window.__ri')}` };
    // (b10) the naive listener defence can be neutralised from the page by redefining Event methods
    await bt('L_naive', 'b10_redefine_Event.preventDefault_then_click_copy', async () => { await ev(ags, `Event.prototype.preventDefault=function(){};Event.prototype.stopImmediatePropagation=function(){};1`); await click(ags, 'ce'); await settle(); return ev(ags, 'window.__ce'); });
    await bt('L_hard', 'b10h_same_attack_on_hardened_listeners', async () => { await ev(ags, `Event.prototype.preventDefault=function(){};Event.prototype.stopImmediatePropagation=function(){};1`); await click(ags, 'ce'); await settle(); return ev(ags, 'window.__ce'); });
    // a later capture listener on a child node cannot run before the window capture listener (read from the event order)
    await bt('L_hard', 'b11_later_listeners_run_after (observed order)', async () => {
      await ev(ags, `window.__ord=[];const t=document.getElementById('t');for(const n of [window,document,t])n.addEventListener('copy',()=>window.__ord.push(n===window?'window':n===document?'document':'textarea'),true);1`);
      await click(ags, 'ce'); await settle(); return `listeners run: ${await ev(ags, 'JSON.stringify(window.__ord)')}`;
    });
    for (const [k, v] of Object.entries(a2)) rec(`A2.${k}`, `${v.sys}${v.page !== undefined ? ` | ${String(v.page).slice(0, 220)}` : ''}`);
    out.A2 = a2;
  }
  // ================= A3 =================
  for (const a3Name of (want('a3') ? ['L_hard', 'L_hard_O_hard'] : [])) {
    await uninstall();
    const a3 = {};
    const injected = [];
    const early = [];
    const defSrc = DEF[a3Name].join(';');
    const b = P; // browser-level session of pegboardd
    const onAttach = async (p, sid) => {
      injected.push({ type: p.targetInfo.type, url: p.targetInfo.url.replace(/:\d+/, ':PORT'), waiting: p.waitingForDebugger });
      try {
        await b.send('Page.enable', {}, sid).catch(() => {});
        await b.send('Page.addScriptToEvaluateOnNewDocument', { source: defSrc }, sid).catch(() => {});
        if (p.targetInfo.type === 'page' || p.targetInfo.type === 'iframe') await b.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }, sid).catch(() => {});
        await b.send('Runtime.runIfWaitingForDebugger', {}, sid).catch(() => {});
      } catch {}
    };
    b.on('Target.attachedToTarget', (p, from) => { if (!from) onAttach(p, p.sessionId); else onAttach(p, p.sessionId); });
    await b.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true });
    await b.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }, ps); // the existing agent tab: OOPIFs attach below its page session
    installed.push({ conn: b, sid: ps, id: (await b.send('Page.addScriptToEvaluateOnNewDocument', { source: defSrc }, ps)).identifier });
    await sleep(500);
    const trialA3 = async (name, fn, expect = SEL) => { sys.set(SYSVAL); const detail = await fn(); await settle(); a3[name] = { sys: classify(SYSVAL, sys.get(), expect), page: detail }; rec(`A3[${a3Name}].${name}`, `${a3[name].sys} | ${String(detail).slice(0, 260)}`); };
    // existing agent tab: the page session of P has nothing; navigate so the document is created after attach
    await AG.navigate(ags, `${O}/clip`).catch(() => {});
    // we cannot rely on the old tab (already running): open a NEW tab from the agent tab -> auto-attach path
    await trialA3('popup_window.open (page first script saw injection)', async () => {
      const known = (await P.send('Target.getTargets')).targetInfos.map((t) => t.targetId);
      await ev(ags, `window.open('${O}/clip','_blank')&&1`, true);
      let t; for (let i = 0; i < 40 && !t; i++) { await sleep(150); t = (await P.send('Target.getTargets')).targetInfos.find((x) => x.type === 'page' && !known.includes(x.targetId)); }
      const s = await AG.attach(t.targetId); await sleep(800);
      const first = await ev(s, 'JSON.stringify(window.__first)');
      await click(s, 'ce'); await settle();
      const res = await ev(s, 'window.__ce');
      await AG.send('Target.closeTarget', { targetId: t.targetId });
      return `first-script state ${first}; click execCommand result ${res}`;
    });
    await trialA3('new_tab_later (window.open from agent tab, then evaluate execCommand w/ gesture)', async () => {
      const known = (await P.send('Target.getTargets')).targetInfos.map((t) => t.targetId);
      await ev(ags, `window.open('${O}/clip','_blank')&&1`, true);
      let t; for (let i = 0; i < 40 && !t; i++) { await sleep(150); t = (await P.send('Target.getTargets')).targetInfos.find((x) => x.type === 'page' && !known.includes(x.targetId)); }
      const s = await AG.attach(t.targetId); await sleep(800);
      await selectT(s); const r = await ev(s, `String(document.execCommand('copy'))`, true);
      await AG.send('Target.closeTarget', { targetId: t.targetId });
      return `execCommand ${r}`;
    });
    // cross-site iframe (OOPIF): button inside the iframe at (300+60, 60+20)
    await trialA3('cross_site_iframe_click_execCopy', async () => {
      await AG.navigate(ags, FRAME).catch(() => {});
      await sleep(1200);
      const first = await ev(ags, `JSON.stringify(window.__msgs)`);
      await clickAt(ags, 300 + 60, 60 + 20); await settle();
      return `iframe messages: ${await ev(ags, 'JSON.stringify(window.__msgs)')} (before click ${first}); top frame injected: ${await ev(ags, 'window.__first.defL')}`;
    });
    // nested key path inside the iframe: click its textarea to focus then Meta+C with commands
    await trialA3('cross_site_iframe_key_Meta+C_commands', async () => {
      await clickAt(ags, 300 + 100, 20); await sleep(200);
      await keyEv(ags, 'a', 'KeyA', 65, { commands: ['selectAll'] });
      await keyEv(ags, 'c', 'KeyC', 67, { commands: ['copy'] });
      return 'clicked the iframe textarea, commands selectAll then copy (key path inside the cross-site iframe)';
    }, SEL);
    // the window before injection: how many targets were attached with waitingForDebugger
    rec(`A3[${a3Name}].attached`, JSON.stringify(injected.map((i) => `${i.type}${i.waiting ? '(paused)' : '(running)'}`)));
    a3.attached = injected;
    await b.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }).catch(() => {});
    await b.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }, ps).catch(() => {});
    await uninstall();
    (out.A3 = out.A3 || {})[a3Name] = a3;
  }
  // ================= A7 (round 3, item 5) =================
  if (want('a7')) {
    await uninstall();
    const a7 = {};
    const unsetAuto = async () => { await P.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }).catch(() => {}); await P.send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }, ps).catch(() => {}); };
    // ---- (a) agent session OLDER than pegboard's injecting session ----
    // ags was created before this younger pegboard session, so the agent's scripts run first (see b9d).
    const pyoung = await P.attach(env.agent.targetId);
    await P.send('Page.enable', {}, pyoung);
    const attacks = {
      'i1 neutralise Event.prototype.preventDefault + stopImmediatePropagation before L_hard captures them': `Event.prototype.preventDefault=function(){};Event.prototype.stopImmediatePropagation=function(){};`,
      'i2 EventTarget.prototype.addEventListener replaced by a no-op before L_hard registers': `EventTarget.prototype.addEventListener=function(){};`,
      'i3 Function.prototype.call replaced (L_hard calls the saved originals through it)': `Function.prototype.call=function(){};`,
      'ii agent registers its own window capture copy/cut listener FIRST that only calls stopImmediatePropagation': `(()=>{const f=(e)=>{e.stopImmediatePropagation()};for(const n of ['copy','cut'])window.addEventListener(n,f,true)})()`,
      'i4 agent saves pristine references on window, no tampering (control: capture only)': `window.__saved={pd:Event.prototype.preventDefault,sip:Event.prototype.stopImmediatePropagation,ael:EventTarget.prototype.addEventListener,call:Function.prototype.call}`,
    };
    for (const defName of ['L_hard', 'L_hard2']) for (const [order0, agentOlder] of [['agent session OLDER than pegboard injecting session', true], ['pegboard injecting session OLDER than agent session (control)', false]]) {
      const order = `${defName}: ${order0}`;
      for (const [name, src] of Object.entries(attacks)) {
        await uninstall();
        const ids = [];
        // pegboard's L_hard always through P's sessions: older = ps (created first), younger = pyoung
        const pegSid = agentOlder ? pyoung : ps;
        const agSid = ags;
        // agent's own script through the agent connection's session (ags is older than pyoung, younger than ps)
        ids.push([P, pegSid, (await P.send('Page.addScriptToEvaluateOnNewDocument', { source: DEF[defName][0] }, pegSid)).identifier]);
        ids.push([AG, agSid, (await AG.send('Page.addScriptToEvaluateOnNewDocument', { source: src }, agSid)).identifier]);
        await load();
        sys.set(SYSVAL);
        await click(ags, 'ce'); await settle();
        const clickRes = await ev(ags, 'window.__ce');
        const clickSys = classify(SYSVAL, sys.get(), SEL);
        sys.set(SYSVAL); await selectT(ags); await keyEv(ags, 'c', 'KeyC', 67, { commands: ['copy'] }); await settle();
        const keySys = classify(SYSVAL, sys.get(), SEL);
        sys.set(SYSVAL); await selectT(ags); const evalRes = await ev(ags, `String(document.execCommand('copy'))`, true); await settle();
        a7[`${order} | ${name}`] = { clickExecCommandCopy: `${clickSys} (page ${clickRes})`, keyCommandsCopy: keySys, evalGestureExecCommandCopy: `${classify(SYSVAL, sys.get(), SEL)} (page ${evalRes})` };
        rec(`A7a.${order} | ${name}`, JSON.stringify(a7[`${order} | ${name}`]));
        for (const [c, sd, id] of ids) await c.send('Page.removeScriptToEvaluateOnNewDocument', { identifier: id }, sd).catch(() => {});
      }
    }
    await P.send('Target.detachFromTarget', { sessionId: pyoung }).catch(() => {});
    // ---- (b) ONE configuration: L_hard + clipboard permission denied + recursive auto-attach; every write path + the extra keys ----
    {
      await uninstall();
      const S = await env.connection();
      for (const n of ['clipboard-read', 'clipboard-write']) await S.send('Browser.setPermission', { permission: { name: n }, setting: 'denied', browserContextId: agentCtx }).catch((e) => console.log('setPermission', e.message));
      const handler = async (p) => {
        await P.send('Page.enable', {}, p.sessionId).catch(() => {});
        // an attach that is NOT paused (observed for sandboxed srcdoc iframes) is also injected into the running document
        await P.send('Page.addScriptToEvaluateOnNewDocument', { source: DEF.L_hard2[0], ...(p.waitingForDebugger ? {} : { runImmediately: true }) }, p.sessionId).catch(() => {});
        if (p.targetInfo.type === 'page' || p.targetInfo.type === 'iframe') await P.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }, p.sessionId).catch(() => {});
        await P.send('Runtime.runIfWaitingForDebugger', {}, p.sessionId).catch(() => {});
      };
      let on = true; P.on('Target.attachedToTarget', (p) => { if (on) handler(p); });
      await P.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true });
      await P.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }, ps);
      installed.push({ conn: P, sid: ps, id: (await P.send('Page.addScriptToEvaluateOnNewDocument', { source: DEF.L_hard2[0] }, ps)).identifier });
      await sleep(400);
      const b = {};
      for (const [k, fn] of Object.entries(ACT)) { await load(); sys.set(SYSVAL); const detail = await fn(); await settle(); b[k] = { sys: classify(SYSVAL, sys.get(), EXPECT[k] || SEL), page: detail }; }
      b.g_key_paste_commands.read = seenPaste(b.g_key_paste_commands);
      const keyTry = async (name, params, kind) => {
        await load(); sys.set(SYSVAL);
        if (kind === 'paste') await ev(ags, `document.getElementById('t').value='';document.getElementById('t').focus();1`); else await selectT(ags);
        await keyEv(ags, ...params); await settle();
        b[name] = kind === 'paste' ? (String(await ev(ags, `document.getElementById('t').value`)).includes(SYSVAL) ? 'PAGE READ SYSTEM CLIPBOARD' : 'no read') : classify(SYSVAL, sys.get(), SEL);
      };
      await keyTry('Control+Insert (no commands)', ['Insert', 'Insert', 45, { modifiers: 2 }], 'w');
      await keyTry('Shift+Delete (no commands)', ['Delete', 'Delete', 46, { modifiers: 8 }], 'w');
      await keyTry('Shift+Insert (no commands)', ['Insert', 'Insert', 45, { modifiers: 8 }], 'paste');
      await keyTry('Control+C commands:[copy]', ['c', 'KeyC', 67, { modifiers: 2, commands: ['copy'] }], 'w');
      await keyTry('Control+X commands:[cut]', ['x', 'KeyX', 88, { modifiers: 2, commands: ['cut'] }], 'w');
      await keyTry('Control+V commands:[paste]', ['v', 'KeyV', 86, { modifiers: 2, commands: ['paste'] }], 'paste');
      for (const [k, v] of Object.entries(b)) rec(`A7b.${k}`, typeof v === 'string' ? v : `${v.sys}${v.page !== undefined ? ` (${String(v.page).slice(0, 50)})` : ''}`);
      a7.configuration_b = b;
      // ---- (c) sandbox="allow-scripts" iframe under the same configuration ----
      const seen = [];
      P.on('Target.attachedToTarget', (p) => seen.push(`${p.targetInfo.type}:${p.targetInfo.url.replace(/:\d+/, ':PORT').slice(0, 40)}${p.waitingForDebugger ? '(paused)' : ''}`));
      const c = {};
      await load(); seen.length = 0; sys.set(SYSVAL);
      await ev(ags, `(()=>{const f=document.createElement('iframe');f.id='sbx';f.setAttribute('sandbox','allow-scripts');f.style.cssText='position:absolute;left:300px;top:320px;width:300px;height:100px;border:0';f.srcdoc='<body style="margin:0"><textarea id=q style="width:100px;height:30px">${SEL}</textarea><button id=b style="position:absolute;left:0;top:50px;width:100px;height:40px" onclick="const q=document.getElementById(\\'q\\');q.focus();q.select();parent.postMessage({sbx:String(document.execCommand(\\'copy\\')),defL:!!window.__defL},\\'*\\')">c</button>';document.body.appendChild(f);return 1})()`);
      await sleep(700);
      c.targetsAttachedWhileCreatingSandboxedSrcdocIframe = seen.slice();
      // control: the same attach events for a plain (unsandboxed) srcdoc iframe, and the page's own cross-site iframe alone
      await load(); seen.length = 0;
      await ev(ags, `(()=>{const f=document.createElement('iframe');f.srcdoc='<p>x</p>';document.body.appendChild(f);return 1})()`);
      await sleep(700);
      c.control_targetsAttachedForPlainSrcdocIframe = seen.slice();
      await load(); seen.length = 0; await sleep(700);
      c.control_targetsAttachedByLoadingThePageAlone = seen.slice();
      await load(); seen.length = 0;
      await ev(ags, `(()=>{const f=document.createElement('iframe');f.id='sbx';f.setAttribute('sandbox','allow-scripts');f.style.cssText='position:absolute;left:300px;top:320px;width:300px;height:100px;border:0';f.srcdoc='<body style="margin:0"><textarea id=q style="width:100px;height:30px">${SEL}</textarea><button id=b style="position:absolute;left:0;top:50px;width:100px;height:40px" onclick="const q=document.getElementById(\\'q\\');q.focus();q.select();parent.postMessage({sbx:String(document.execCommand(\\'copy\\')),defL:!!window.__defL},\\'*\\')">c</button>';document.body.appendChild(f);return 1})()`);
      await sleep(700);
      await clickAt(ags, 350, 320 + 70); await settle();
      c.srcdocSandboxOpaque_gestureCopy = { messages: await ev(ags, 'JSON.stringify(window.__msgs)'), system: classify(SYSVAL, sys.get(), SEL) };
      // the same sandbox on a cross-site src (its own process)
      await load(`${O}/clip`); seen.length = 0; sys.set(SYSVAL);
      await ev(ags, `(()=>{const f=document.createElement('iframe');f.setAttribute('sandbox','allow-scripts');f.style.cssText='position:absolute;left:300px;top:0;width:400px;height:300px;border:0';f.src='http://c.test:${probe.port}/clip';document.body.appendChild(f);return 1})()`);
      await sleep(1500);
      c.targetsAttachedWhileCreatingSandboxedCrossSiteIframe = seen.slice();
      await clickAt(ags, 300 + 60, 60 + 20); await settle();
      c.crossSiteSandboxed_gestureCopy = { messages: await ev(ags, 'JSON.stringify(window.__msgs)'), system: classify(SYSVAL, sys.get(), SEL) };
      for (const [k, v] of Object.entries(c)) rec(`A7c.${k}`, JSON.stringify(v));
      a7.configuration_c = c;
      on = false; await unsetAuto(); await uninstall(); await S.close?.();
    }
    out.A7 = a7;
  }
  // ================= A4 =================
  if (want('a4')) {
    await uninstall();
    const a4 = {};
    const mods = { Meta: 4, Control: 2, Shift: 8, Alt: 1 };
    const keyTry = async (name, params, kind) => {
      await load(); sys.set(SYSVAL);
      let page;
      if (kind === 'paste') await ev(ags, `document.getElementById('t').value='';document.getElementById('t').focus();1`); else await selectT(ags);
      await keyEv(ags, ...params);
      await settle();
      if (kind === 'paste') page = await ev(ags, `document.getElementById('t').value`);
      a4[name] = { sys: kind === 'paste' ? (String(page).includes(SYSVAL) ? 'PAGE READ SYSTEM CLIPBOARD' : 'no read') : classify(SYSVAL, sys.get(), SEL) };
      if (kind !== 'paste') a4[name].textareaAfter = (await ev(ags, `document.getElementById('t').value`)).slice(0, 16);
      return a4[name];
    };
    const rows = [];
    for (const [mn, m] of [['Meta', 4], ['Control', 2]])
      for (const [kn, k, c, vk, cmd, kind] of [['C', 'c', 'KeyC', 67, 'copy', 'w'], ['X', 'x', 'KeyX', 88, 'cut', 'w'], ['V', 'v', 'KeyV', 86, 'paste', 'paste']]) {
        await keyTry(`${mn}+${kn} no commands`, [k, c, vk, { modifiers: m }], kind);
        await keyTry(`${mn}+${kn} commands:[${cmd}]`, [k, c, vk, { modifiers: m, commands: [cmd] }], kind);
      }
    await keyTry('Control+Insert no commands', ['Insert', 'Insert', 45, { modifiers: 2 }], 'w');
    await keyTry('Control+Insert commands:[copy]', ['Insert', 'Insert', 45, { modifiers: 2, commands: ['copy'] }], 'w');
    await keyTry('Shift+Insert no commands', ['Insert', 'Insert', 45, { modifiers: 8 }], 'paste');
    await keyTry('Shift+Insert commands:[paste]', ['Insert', 'Insert', 45, { modifiers: 8, commands: ['paste'] }], 'paste');
    await keyTry('Shift+Delete no commands', ['Delete', 'Delete', 46, { modifiers: 8 }], 'w');
    await keyTry('Shift+Delete commands:[cut]', ['Delete', 'Delete', 46, { modifiers: 8, commands: ['cut'] }], 'w');
    // shape variations with commands:[copy]
    await keyTry('Meta+C commands, type=keyDown', ['c', 'KeyC', 67, { commands: ['copy'], type: 'keyDown' }], 'w');
    await keyTry('Meta+C commands, no vk, key only', ['c', undefined, undefined, { commands: ['copy'], extra: {} }], 'w');
    await keyTry('Meta+C commands, nativeVirtualKeyCode only', ['c', 'KeyC', undefined, { commands: ['copy'], extra: { nativeVirtualKeyCode: 8 } }], 'w');
    await keyTry('Meta+C commands, key "C" modifiers 0', ['C', 'KeyC', 67, { commands: ['copy'], modifiers: 0 }], 'w');
    await keyTry('commands:[copy] on a plain "a" key, no modifiers', ['a', 'KeyA', 65, { commands: ['copy'], modifiers: 0 }], 'w');
    await keyTry('commands:[selectAll,copy] no modifiers', ['x', 'KeyX', 88, { commands: ['selectAll', 'copy'], modifiers: 0 }], 'w');
    await load(); sys.set(SYSVAL); await selectT(ags);
    await AG.send('Input.insertText', { text: 'INSERTED' }, ags);
    await settle();
    a4['Input.insertText'] = { sys: classify(SYSVAL, sys.get(), 'INSERTED'), textareaAfter: await ev(ags, `document.getElementById('t').value`) };
    // right-click
    await load(); sys.set(SYSVAL); await selectT(ags);
    await ev(ags, `window.__cm=0;document.addEventListener('contextmenu',()=>window.__cm++);1`);
    const { x, y } = await rectOf(ags, 't');
    await clickAt(ags, x, y, AG, 'right'); await sleep(800);
    a4['right-click contextmenu'] = { sys: classify(SYSVAL, sys.get(), SEL), contextmenuEvents: await ev(ags, 'window.__cm'), note: 'a native context menu is not observable over CDP; no menu item can be sent through Input.* (not tested beyond the event)' };
    await keyEv(ags, 'Escape', 'Escape', 27, { modifiers: 0 });
    for (const [k, v] of Object.entries(a4)) rec(`A4.${k}`, JSON.stringify(v));
    out.A4 = a4;
  }
  // ================= A5 =================
  if (want('a5')) {
    await uninstall();
    const a5 = {};
    // Permissions-Policy added to the document response by P at the Fetch response stage
    await P.send('Fetch.enable', { patterns: [{ urlPattern: '*/clip', requestStage: 'Response' }] }, ps);
    P.on('Fetch.requestPaused', async (p, sid) => {
      if (sid !== ps) return;
      const hdrs = [...(p.responseHeaders || []), { name: 'Permissions-Policy', value: 'clipboard-write=(), clipboard-read=()' }];
      P.send('Fetch.continueResponse', { requestId: p.requestId, responseCode: p.responseStatusCode, responseHeaders: hdrs }, sid).catch((e) => console.log('continueResponse failed', e.message));
    });
    await load();
    sys.set(SYSVAL);
    const pp = await ev(ags, `document.featurePolicy ? JSON.stringify({write:document.featurePolicy.allowsFeature('clipboard-write'),read:document.featurePolicy.allowsFeature('clipboard-read')}) : 'no featurePolicy'`);
    await click(ags, 'cw'); await settle();
    a5.permissionsPolicy_writeText = { sys: classify(SYSVAL, sys.get(), `CW-${runId}`), policy: pp, page: await ev(ags, 'window.__cw') };
    sys.set(SYSVAL); await click(ags, 'ce'); await settle();
    a5.permissionsPolicy_execCommandCopy_click = { sys: classify(SYSVAL, sys.get(), SEL), page: await ev(ags, 'window.__ce') };
    sys.set(SYSVAL); await selectT(ags); await keyEv(ags, 'c', 'KeyC', 67, { commands: ['copy'] }); await settle();
    a5.permissionsPolicy_key_commands_copy = { sys: classify(SYSVAL, sys.get(), SEL) };
    await P.send('Fetch.disable', {}, ps);
    a5.CSP = 'not tested: CSP has no clipboard directive';
    a5.enterprisePolicy_DefaultClipboardSetting = 'not tested, needs system-level policy (macOS reads managed preferences from a system location; no flag points Chrome for Testing at a policy file inside the temporary dir)';
    // Browser/Emulation methods that mention clipboard
    const S = await env.connection();
    const permShapes = {};
    for (const d of [{ name: 'clipboard-write', allowWithoutSanitization: false }, { name: 'clipboard-read' }, { name: 'clipboard-sanitized-write' }])
      permShapes[d.name + (d.allowWithoutSanitization === false ? '+allowWithoutSanitization=false' : '')] = await S.send('Browser.setPermission', { permission: d, setting: 'denied', origin: O, browserContextId: agentCtx }).then(() => 'ok', (e) => e.message);
    await load(); sys.set(SYSVAL);
    await click(ags, 'cw'); await settle();
    const c1 = { writeText: await ev(ags, 'window.__cw'), sys: classify(SYSVAL, sys.get(), `CW-${runId}`) };
    sys.set(SYSVAL); await click(ags, 'ce'); await settle();
    a5.browserSetPermission_denied = { shapes: permShapes, writeTextAPI: c1, execCommandCopyClick: { sys: classify(SYSVAL, sys.get(), SEL), page: await ev(ags, 'window.__ce') } };
    S.close();
    a5.emulationDomain = 'Emulation.setFocusEmulationEnabled: not a block (round 1: a real click focuses the document); no other Emulation/Browser method mentioning clipboard exists in the protocol beyond Browser.setPermission/grantPermissions (clipboardReadWrite, clipboardSanitizedWrite)';
    for (const [k, v] of Object.entries(a5)) rec(`A5.${k}`, typeof v === 'string' ? v : JSON.stringify(v));
    out.A5 = a5;
    await sleep(700);
  }
  // ================= A6 =================
  if (want('a6')) {
    await uninstall();
    const w = startChangeWatcher(0.01);
    await sleep(1500);
    const a6 = {};
    const probeWrite = async (name, fn) => {
      await load();
      sys.set(SYSVAL); await sleep(300);
      const c0 = w.events.length;
      const t0 = Date.now();
      await fn();
      const t1 = Date.now();
      await sleep(500);
      const fresh = w.events.slice(c0);
      a6[name] = { counterMoved: fresh.length > 0, increments: fresh.length ? fresh[fresh.length - 1].count - (w.events[c0 - 1]?.count ?? fresh[0].count - 1) : 0, firstSeenMsAfterActionStart: fresh.length ? fresh[0].t - t0 : null, actionCallMs: t1 - t0 };
    };
    await probeWrite('execCommand click', async () => { await click(ags, 'ce'); });
    await probeWrite('key Meta+C commands', async () => { await selectT(ags); await keyEv(ags, 'c', 'KeyC', 67, { commands: ['copy'] }); });
    await probeWrite('evaluate execCommand(gesture)', async () => { await selectT(ags); await ev(ags, `document.execCommand('copy')`, true); });
    await probeWrite('writeText click', async () => { await click(ags, 'cw'); });
    await probeWrite('paste (read only)', async () => { await ev(ags, `document.getElementById('t').focus();1`); await keyEv(ags, 'v', 'KeyV', 86, { commands: ['paste'] }); });
    await probeWrite('no clipboard action (control)', async () => { await ev(ags, '1'); });
    // one JXA poll: cost of a one-shot check
    const { spawnSync } = await import('node:child_process');
    const t0 = Date.now();
    const one = spawnSync('osascript', ['-l', 'JavaScript', '-e', `ObjC.import('AppKit');$.NSPasteboard.generalPasteboard.changeCount`], { encoding: 'utf8' });
    a6.oneShotOsascriptMs = Date.now() - t0; a6.oneShotOutput = one.stdout.trim().length > 0 ? 'prints count' : 'empty';
    a6.watcherPollIntervalMs = 10;
    w.stop();
    for (const [k, v] of Object.entries(a6)) rec(`A6.${k}`, JSON.stringify(v));
    out.A6 = a6;
  }
} finally {
  guard.restore();
  rec('Clipboard', `restored (${guard.savedBytes} bytes text); equals the saved one: ${guard.equalsSaved()}; contains this run's marker: ${sys.get().includes(runId)}`);
  console.log('saved', writeResult('23-clipboard-defences', out));
  await env.close();
  probe.close();
}
process.exit(0);
