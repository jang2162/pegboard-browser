// Spike 30 (round 3, item 4): a download clicked INSIDE a loaded cross-site iframe (the iframe page has a link to an attachment).
// Can the top-level tab be found from the download's frameId?
import { randomBytes } from 'node:crypto';
import { mkdirSync, readdirSync, rmSync, existsSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { writeResult, sleep, OUT, onCleanup } from './lib/launch.mjs';
import { startProbe, html, openTwoProfiles, reporter } from './lib/probe.mjs';

const out = {};
const rec = reporter('30', out);
const runId = randomBytes(4).toString('hex');
const userDownloads = join(homedir(), 'Downloads');
const landed = () => (existsSync(userDownloads) ? readdirSync(userDownloads).filter((f) => f.includes(runId)) : []);
onCleanup(() => { for (const f of landed()) rmSync(join(userDownloads, f), { force: true }); });
const probe = await startProbe();
const R = probe.routes;
R['/dl'] = (_q, res, url) => { res.writeHead(200, { 'content-type': 'application/octet-stream', 'content-disposition': `attachment; filename="${url.searchParams.get('name') || 'f'}-${runId}.bin"`, 'content-length': 20000 }); res.end(Buffer.alloc(20000, 0x61)); };
R['/blank'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html('<p>b</p>')));
R['/linkpage'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html(`<body style="margin:0"><a id="l" href="/dl?name=iframelink" style="position:absolute;left:10px;top:10px;width:200px;height:40px;display:block">download</a></body>`)));
const env = await openTwoProfiles({ name: '30-ifd', probe, seedPath: '/blank', args: ['--host-resolver-rules=MAP c.test 127.0.0.1'] });
R['/top'] = (_q, res) => (res.writeHead(200, { 'content-type': 'text/html' }), res.end(html(`<iframe id="x" src="http://c.test:${probe.port}/linkpage" style="position:absolute;left:100px;top:100px;width:300px;height:100px;border:0"></iframe>`)));
const P = env.cdp, ps = env.agent.sessionId;
const dir = join(OUT, `30-${runId}`); mkdirSync(dir, { recursive: true });
const log = [];
P.on('Browser.downloadWillBegin', (p) => log.push({ ev: 'begin', guid: p.guid, frameId: p.frameId, name: p.suggestedFilename }));
P.on('Browser.downloadProgress', (p) => log.push({ ev: 'progress', guid: p.guid, state: p.state }));
const created = [];
P.on('Target.targetCreated', (p) => created.push(p.targetInfo));
try {
  await P.send('Browser.setDownloadBehavior', { behavior: 'allowAndName', downloadPath: dir, eventsEnabled: true, browserContextId: env.agentCtx });
  await P.send('Target.setDiscoverTargets', { discover: true });
  await P.send('Page.enable', {}, ps);
  for (let run = 1; run <= 3; run++) {
    log.length = 0; created.length = 0;
    await P.navigate(ps, `${probe.origin}/top`);
    await sleep(1200);
    const r = { run };
    const targets = (await P.send('Target.getTargets')).targetInfos;
    const iframeT = targets.find((t) => t.type === 'iframe');
    r.iframeTargetExists = !!iframeT;
    let isess = null;
    if (iframeT) {
      isess = await P.attach(iframeT.targetId);
      await P.send('Page.enable', {}, isess).catch((e) => (r.pageEnableOnIframe = e.message));
      r.iframeTargetInfoKeys = Object.keys(iframeT).filter((k) => iframeT[k] !== '' && iframeT[k] !== undefined).sort();
      r.iframeTargetInfo = { openerId: iframeT.openerId ?? null, parentFrameId: iframeT.parentFrameId ?? null, browserContext: iframeT.browserContextId === env.agentCtx ? 'Agent' : '?', openerFrameId: iframeT.openerFrameId ?? null };
      r.getTargetInfoOfIframe = await P.send('Target.getTargetInfo', { targetId: iframeT.targetId }).then((x) => ({ keys: Object.keys(x.targetInfo).filter((k) => x.targetInfo[k] !== '' && x.targetInfo[k] !== undefined).sort(), parentFrameId: x.targetInfo.parentFrameId ?? null, openerId: x.targetInfo.openerId ?? null }), (e) => e.message);
    }
    // real mouse click on the link INSIDE the iframe (iframe at 100,100; link at 10,10 inside, 200x40)
    for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) await P.send('Input.dispatchMouseEvent', { type, x: 100 + 10 + 100, y: 100 + 10 + 20, button: 'left', clickCount: type === 'mouseMoved' ? 0 : 1 }, ps);
    let begin = null;
    for (let i = 0; i < 50 && !begin; i++) { await sleep(100); begin = log.find((l) => l.ev === 'begin'); }
    r.downloadBegan = !!begin;
    if (begin) {
      r.frameIdIsATargetId = (await P.send('Target.getTargets')).targetInfos.some((t) => t.targetId === begin.frameId);
      r.frameIdIsTheIframeTarget = iframeT?.targetId === begin.frameId;
      // lookups
      const tree = {};
      for (const t of (await P.send('Target.getTargets')).targetInfos.filter((x) => x.type === 'page')) {
        const s = t.targetId === env.agent.targetId ? ps : await P.attach(t.targetId);
        await P.send('Page.enable', {}, s).catch(() => {});
        const ft = await P.send('Page.getFrameTree', {}, s).then((x) => x.frameTree, () => null);
        const ids = []; const walk = (n) => { ids.push(n.frame.id); (n.childFrames || []).forEach(walk); }; if (ft) walk(ft);
        tree[t.targetId === env.agent.targetId ? 'AgentTab' : t.targetId === env.human.targetId ? 'HumanTab' : 'other'] = { frames: ids.length, containsDownloadFrameId: ids.includes(begin.frameId), childFramesOfTop: ft ? (ft.childFrames || []).length : null };
      }
      r.getFrameTreePerPage = tree;
      if (isess) {
        const ft = await P.send('Page.getFrameTree', {}, isess).then((x) => x.frameTree, (e) => e.message);
        r.getFrameTreeFromIframeSession = typeof ft === 'string' ? ft : { frameIdIsDownloadFrame: ft.frame.id === begin.frameId, hasParentId: !!ft.frame.parentId, parentIdIsTopFrame: ft.frame.parentId === env.agent.targetId, childFrames: (ft.childFrames || []).length };
      }
      // the top-level tab through the parent chain
      r.topTabFoundBy = [];
      if (Object.values(tree).some((v) => v.containsDownloadFrameId)) r.topTabFoundBy.push('Page.getFrameTree on each page (containsDownloadFrameId)');
      if (r.getFrameTreeFromIframeSession?.parentIdIsTopFrame) r.topTabFoundBy.push('frame.parentId from the iframe session equals the tab targetId');
      r.parentFrameIdEqualsTabTargetId = r.iframeTargetInfo?.parentFrameId === env.agent.targetId; // round 4: compared, not just present
      r.parentFrameIdIsPresent = !!r.iframeTargetInfo?.parentFrameId;
      if (r.parentFrameIdEqualsTabTargetId) r.topTabFoundBy.push('TargetInfo.parentFrameId equals the tab targetId');
      if (r.iframeTargetInfo?.openerId) r.topTabFoundBy.push('TargetInfo.openerId');
      await sleep(600);
      r.fileInPolicyDir = readdirSync(dir).length;
    }
    rec(`F1.run${run}`, r);
    if (isess) await P.send('Target.detachFromTarget', { sessionId: isess }).catch(() => {});
    for (const f of readdirSync(dir)) rmSync(join(dir, f), { force: true });
  }
} finally {
  for (const f of landed()) rmSync(join(userDownloads, f), { force: true });
  rec('Cleanup', `files with this run's id left in ~/Downloads: ${landed().length}`);
  console.log('saved', writeResult('30-iframe-download', out));
  await env.close(); probe.close();
}
process.exit(0);
