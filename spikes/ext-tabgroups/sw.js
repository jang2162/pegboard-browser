// Throwaway spike: helpers called from the test script through CDP Runtime.evaluate on this service worker.
self.pegEvents = [];
chrome.tabs.onUpdated.addListener((tabId, change) => {
  if ('groupId' in change) self.pegEvents.push({ tabId, groupId: change.groupId, at: Date.now() });
});

self.pegTargets = async () =>
  (await chrome.debugger.getTargets()).filter((t) => t.type === 'page').map((t) => ({ targetId: t.id, tabId: t.tabId, url: t.url }));

self.pegGroup = async (tabIds, title, color) => {
  const groupId = await chrome.tabs.group({ tabIds });
  await chrome.tabGroups.update(groupId, { title, color });
  return groupId;
};

self.pegAdd = async (groupId, tabIds) => {
  await chrome.tabs.group({ groupId, tabIds });
};

self.pegUngroup = async (tabIds) => {
  await chrome.tabs.ungroup(tabIds);
};

self.pegMembers = async (groupId) => (await chrome.tabs.query({ groupId })).map((t) => t.id);

self.pegTab = async (tabId) => {
  const t = await chrome.tabs.get(tabId);
  return { id: t.id, groupId: t.groupId, windowId: t.windowId, incognito: t.incognito, index: t.index };
};
