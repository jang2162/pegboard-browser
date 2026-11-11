// Pegboard bridge: tab group and tab control for the profile this worker runs in.
// Called from pegboard-core through CDP Runtime.evaluate on this service worker.
// The extension runs once per profile, so chrome.tabs.create here always lands in this profile.

async function targetIdOf(tabId) {
  // A freshly created tab can take a moment to show up in the debugger target list.
  for (let i = 0; i < 50; i++) {
    const t = (await chrome.debugger.getTargets()).find((x) => x.type === 'page' && x.tabId === tabId);
    if (t) return t.id;
    await new Promise((r) => setTimeout(r, 50));
  }
  throw new Error(`no target for tab ${tabId}`);
}

async function tabIdOf(targetId) {
  const t = (await chrome.debugger.getTargets()).find((x) => x.type === 'page' && x.id === targetId);
  if (!t || t.tabId === undefined) throw new Error(`unknown target ${targetId}`);
  // Defense in depth: the tab must be a real tab of a window owned by this profile.
  const notMine = new Error(`target is not in this profile: ${targetId}`);
  let tab;
  try {
    tab = await chrome.tabs.get(t.tabId);
  } catch (_) {
    throw notMine;
  }
  const windows = await chrome.windows.getAll();
  if (!windows.some((w) => w.id === tab.windowId)) throw notMine;
  return t.tabId;
}

// Only exactly about:blank and absolute http:, https: and data: URLs. chrome.tabs resolves a
// relative string against this extension's origin, so nothing else may reach it.
function allowedUrl(url) {
  if (url === 'about:blank') return url;
  const refused = new Error(`url not allowed: ${JSON.stringify(url)}`);
  if (typeof url !== 'string') throw refused;
  let parsed;
  try {
    parsed = new URL(url);
  } catch (_) {
    throw refused;
  }
  if (!['http:', 'https:', 'data:'].includes(parsed.protocol)) throw refused;
  return parsed.href;
}

self.peg = {
  async createGroup(title, color) {
    const tab = await chrome.tabs.create({ url: 'about:blank', active: false });
    const groupId = await chrome.tabs.group({ tabIds: [tab.id] });
    await chrome.tabGroups.update(groupId, { title, color });
    return { groupId, targetId: await targetIdOf(tab.id) };
  },

  async createTab(groupId, url) {
    const target = allowedUrl(url);
    const tab = await chrome.tabs.create({ url: 'about:blank', active: false });
    await chrome.tabs.group({ groupId, tabIds: [tab.id] });
    if (target !== 'about:blank') await chrome.tabs.update(tab.id, { url: target });
    return { targetId: await targetIdOf(tab.id) };
  },

  async addToGroup(groupId, targetId) {
    await chrome.tabs.group({ groupId, tabIds: [await tabIdOf(targetId)] });
    return {};
  },

  async members(groupId) {
    const tabs = await chrome.tabs.query({ groupId });
    const targets = await chrome.debugger.getTargets();
    const ids = [];
    for (const tab of tabs) {
      const t = targets.find((x) => x.type === 'page' && x.tabId === tab.id);
      if (t) ids.push(t.id);
    }
    return { targetIds: ids };
  },

  async closeTargets(targetIds) {
    const tabIds = [];
    for (const id of targetIds) tabIds.push(await tabIdOf(id));
    if (tabIds.length) await chrome.tabs.remove(tabIds);
    return {};
  },

  async ungroup(groupId) {
    const tabs = await chrome.tabs.query({ groupId });
    if (tabs.length) await chrome.tabs.ungroup(tabs.map((t) => t.id));
    return {};
  },
};
