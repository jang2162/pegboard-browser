// Spike 03 (A4 A5 A7): tab group control without a fork, through an extension driven over CDP.
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { launchChrome, sleep, writeResult, OUT, SPIKES } from './lib/launch.mjs';
import { connect } from './lib/cdp.mjs';
import { startFixture } from './lib/fixture.mjs';

const results = {};
const record = (key, value) => {
  results[key] = value;
  console.log(key, JSON.stringify(value).slice(0, 500));
};
const fixture = await startFixture();
const chrome = await launchChrome({
  name: '03-tabgroups',
  args: [`--load-extension=${join(SPIKES, 'ext-tabgroups')}`, '--disable-features=DisableLoadExtensionCommandLineSwitch'],
});
const cdp = await connect(chrome.browserWsUrl);
const version = await cdp.send('Browser.getVersion');

// 1 (A4): does the live protocol expose anything about tab groups?
const protocol = await (await fetch(`http://127.0.0.1:${chrome.port}/json/protocol`)).json();
const hits = [];
for (const domain of protocol.domains) {
  for (const kind of ['commands', 'events', 'types']) {
    for (const item of domain[kind] || []) {
      const name = item.name || item.id;
      const text = `${name} ${item.description || ''}`;
      if (/tab.?group|tabgroup/i.test(text)) hits.push(`${domain.domain}.${name} (${kind})`);
    }
  }
}
writeFileSync(join(OUT, '03-protocol-groups.txt'), `${version.product}\n${hits.join('\n')}\n`);
record('1_protocolTabGroupHits', { product: version.product, protocolVersion: version.protocolVersion, domains: protocol.domains.length, hits });

// 2: is the extension loaded, and can we reach its service worker?
async function worker() {
  for (let i = 0; i < 40; i++) {
    const { targetInfos } = await cdp.send('Target.getTargets');
    const sw = targetInfos.find((t) => t.type === 'service_worker' && /\/sw\.js$/.test(t.url));
    if (sw) return sw;
    await sleep(250);
  }
  return null;
}
const sw = await worker();
record('2_extensionLoaded', { loaded: !!sw, url: sw?.url });
if (!sw) {
  console.log('saved', writeResult('03-tabgroups', results));
  await chrome.kill();
  fixture.close();
  process.exit(2);
}
let swSid = await cdp.attach(sw.targetId);
const ext = (expr) => cdp.evaluate(swSid, `(async () => JSON.stringify(await (${expr})))()`).then((s) => (s === undefined ? undefined : JSON.parse(s)));

// 3 (A5 core): map CDP targetId to chrome.tabs tabId.
const t1 = await cdp.newPage(`${fixture.origin}/counter`);
const t2 = await cdp.newPage(`${fixture.origin}/opener`);
const tabIdOf = async (targetId) => (await ext('pegTargets()')).find((t) => t.targetId === targetId)?.tabId;
const tab1 = await tabIdOf(t1.targetId);
const tab2 = await tabIdOf(t2.targetId);
record('3_targetToTab', { tab1, tab2, mapped: Number.isInteger(tab1) && Number.isInteger(tab2) });

// 4: create a group and read members back.
const groupId = await ext(`pegGroup([${tab1}, ${tab2}], "조사", "blue")`);
record('4_group', { groupId, members: await ext(`pegMembers(${groupId})`) });

// 5: does a window.open popup tab from a grouped tab join the group by itself?
const before = (await cdp.send('Target.getTargets')).targetInfos.map((t) => t.targetId);
await cdp.send('Runtime.evaluate', { expression: `document.getElementById('open').click()`, userGesture: true }, t2.sessionId);
await sleep(1500);
const popup = (await cdp.send('Target.getTargets')).targetInfos.find((t) => t.type === 'page' && !before.includes(t.targetId));
const popupTab = popup ? await tabIdOf(popup.targetId) : null;
const popupInfo = popupTab ? await ext(`pegTab(${popupTab})`) : null;
let popupAdded = null;
if (popupInfo && popupInfo.groupId !== groupId) {
  await ext(`pegAdd(${groupId}, [${popupTab}])`);
  popupAdded = (await ext(`pegTab(${popupTab})`)).groupId === groupId;
}
record('5_windowOpen', { opened: !!popup, openerId: popup?.openerId, joinedAutomatically: popupInfo?.groupId === groupId, popupInfo, addedByExtension: popupAdded });

// 6: where does a CDP-created tab land, and how long until it is in the group?
const s6 = Date.now();
const t3 = await cdp.send('Target.createTarget', { url: 'about:blank' });
const tab3 = await tabIdOf(t3.targetId);
const info3 = await ext(`pegTab(${tab3})`);
await ext(`pegAdd(${groupId}, [${tab3}])`);
record('6_cdpCreatedTab', { landedInGroup: info3.groupId === groupId, info3, msToGrouped: Date.now() - s6, members: await ext(`pegMembers(${groupId})`) });

// 7: can the extension observe a tab leaving the group? (ungroup stands in for a manual drag)
await ext(`pegUngroup([${tab3}])`);
await sleep(500);
record('7_membershipEvents', { events: await cdp.evaluate(swSid, `JSON.stringify(self.pegEvents)`).then(JSON.parse) });

// 8 (A7): a CDP browser context. Same window? Groupable?
const { browserContextId } = await cdp.send('Target.createBrowserContext');
const t4 = await cdp.send('Target.createTarget', { url: `${fixture.origin}/cookie/get`, browserContextId });
await sleep(1500);
const tab4 = await tabIdOf(t4.targetId);
let info4 = null;
let groupErr = null;
if (Number.isInteger(tab4)) {
  info4 = await ext(`pegTab(${tab4})`).catch((e) => ({ error: e.message }));
  await ext(`pegAdd(${groupId}, [${tab4}])`).catch((e) => (groupErr = e.message));
}
const win1 = await cdp.send('Browser.getWindowForTarget', { targetId: t1.targetId });
const win4 = await cdp.send('Browser.getWindowForTarget', { targetId: t4.targetId });
record('8_browserContext', {
  visibleToExtension: Number.isInteger(tab4),
  info4,
  sameWindowAsGroup: win1.windowId === win4.windowId,
  groupError: groupErr,
  groupedAfter: Number.isInteger(tab4) && !groupErr ? (await ext(`pegTab(${tab4})`)).groupId === groupId : false,
});

// 9: after the service worker goes idle, can we still call it?
await cdp.send('Target.detachFromTarget', { sessionId: swSid }).catch(() => {});
await sleep(45000);
const { targetInfos } = await cdp.send('Target.getTargets');
const still = targetInfos.find((t) => t.type === 'service_worker' && /\/sw\.js$/.test(t.url));
let callable = false;
let err = null;
if (still) {
  swSid = await cdp.attach(still.targetId);
  callable = await ext(`pegMembers(${groupId})`).then(() => true, (e) => ((err = e.message), false));
}
record('9_workerAfterIdle', { targetStillListed: !!still, callable, error: err, eventsKept: still ? await cdp.evaluate(swSid, `self.pegEvents ? self.pegEvents.length : -1`).catch(() => null) : null });

console.log('saved', writeResult('03-tabgroups', { when: new Date().toISOString(), ...results }));
cdp.close();
await chrome.kill();
fixture.close();
process.exit(0);
