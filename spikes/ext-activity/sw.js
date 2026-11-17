// Throwaway spike (phase 2b, 41-human-signals.mjs): hands tab activation and window focus changes to
// `self.pegActivity`, a function the spike creates with CDP `Runtime.addBinding` on this service worker.
// Events that arrive while the binding is missing (e.g. after the worker restarted) are kept in
// `self.pegMissed` so the spike can count them.
self.pegMissed = [];
function send(o) {
  o.at = Date.now();
  if (typeof self.pegActivity === 'function') {
    try {
      self.pegActivity(JSON.stringify(o));
      return;
    } catch (e) {
      o.bindingError = String(e);
    }
  }
  self.pegMissed.push(o);
}
chrome.tabs.onActivated.addListener(async ({ tabId, windowId }) => {
  let url = null;
  try {
    url = (await chrome.tabs.get(tabId)).url;
  } catch {}
  send({ ev: 'tabs.onActivated', tabId, windowId, url });
});
chrome.windows.onFocusChanged.addListener((windowId) => {
  send({ ev: 'windows.onFocusChanged', windowId, none: windowId === chrome.windows.WINDOW_ID_NONE });
});
// Hands the kept events to the binding (after the spike set it again on a restarted worker); returns how many.
self.pegReplay = () => {
  const kept = self.pegMissed.splice(0);
  for (const o of kept) send({ ...o, replayed: true });
  return kept.length;
};
