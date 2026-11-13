# Task 2, round 2: re-measure what the review found overstated or unmeasured

A reviewer checked your findings against the scripts. Most observations stand. Several conclusions are written more strongly than the evidence allows, and a decision by the project owner rests on some of them. Measure the items below, then correct docs/spike-findings.md ("2a단계 측정") so every sentence says exactly what was observed; mark anything still unmeasured as 미측정. Same rules as before (spikes and docs only; no crates/ or extension/ changes; temporary user-data-dir; save/restore the clipboard; no file left in ~/Downloads; no personal paths; no subagents). Add signal/early-failure cleanup while you are in the scripts (see H below).

Each new measurement prints one line with its id. Run each at least twice.

## A. Clipboard (stop condition 4 is the owner's decision: get this right)
Established so far: with all clipboard permissions denied, a gesture-backed document.execCommand('copy') and key events carrying commands:["copy"|"cut"|"paste"] still reach the system clipboard; an injected override of Document.prototype.execCommand in the main world blocked the click-handler path in one arrangement. NOT established: that nothing blocks these paths. Measure:
- A1. Injected capture-phase listeners (Page.addScriptToEvaluateOnNewDocument, added on window and document for 'copy', 'cut', 'paste', 'beforecopy', 'beforecut', 'beforepaste', calling preventDefault + stopImmediatePropagation): does this block (a) execCommand('copy'|'cut') from a real click handler, (b) key events with commands:["copy"|"cut"], (c) commands:["paste"] reading the system clipboard into the page, (d) a page's own copy handler that sets clipboardData? Does the system clipboard change in each case?
- A2. Bypasses of the injected defences from the agent's side, using only what the session proxy allows an agent (see crates/pegboard-core/src/proxy/filter.rs ALLOWED_METHODS; read it, do not run the proxy): Runtime.evaluate in the main world trying to undo it (delete/redefine Document.prototype.execCommand, fetch a pristine execCommand from a fresh same-origin iframe, from an about:blank iframe, from a srcdoc iframe, from a sandboxed iframe, from a new window opened with window.open, removeEventListener is impossible without the reference but can a later-registered capture listener on a child node run first? try dispatching on an element inside a shadow root / an iframe document), Page.createIsolatedWorld + Runtime.evaluate in that world calling document.execCommand, Page.addScriptToEvaluateOnNewDocument by the agent connection registering its own script (which runs first: the one registered first or last? can the agent's script restore the original before pegboard's runs?), runImmediately. For each: does the system clipboard change?
- A3. Cross-site iframe (OOPIF), popup, and a tab opened later: is the injection present there when it is applied through browser-level Target.setAutoAttach with waitForDebuggerOnStart before the document runs? Is there any window where page script runs before the injection?
- A4. The key path: confirm exactly which Input.dispatchKeyEvent shapes touch the system clipboard: with `commands`, without `commands` for Meta+C/X/V AND Control+C/X/V AND Control+Insert / Shift+Insert / Shift+Delete, with `key`/`code`/`nativeVirtualKeyCode` variations, and Input.insertText. Also Input.dispatchMouseEvent right-click (context menu): can a context-menu Copy be triggered through CDP at all?
- A5. Other candidate blocks, each tried once and reported as works / does not / could not test: a Permissions-Policy or CSP response header injected at the Fetch response stage; Chrome enterprise policies DefaultClipboardSetting / ClipboardBlockedForUrls (via a managed-policy file for Chrome for Testing if one can be supplied without touching system-wide settings; if it would require writing outside the temporary dir, do NOT do it: report "not tested, needs system-level policy"); Emulation or Browser domain methods that mention clipboard.
- A6. Detection instead of prevention: can a process notice a clipboard change cheaply (macOS NSPasteboard changeCount through `osascript`/a tiny helper, polling)? Measure only whether the change count moves on each write path and how fast a poll sees it. Do not build a restorer.
State at the end, for the owner: with the best combination that held up under A2-A3, which write paths remain open and which read paths remain open, precisely.

## B. WebSockets (the address rules must cover them)
Established: Fetch.requestPaused never fires for a WebSocket handshake. NOT established: that a WebSocket cannot be blocked. Measure, for ws:// to a loopback port and to a name mapped to a private address:
- B1. Network.setBlockedURLs with patterns for ws:// and wss:// and with host patterns: does the handshake reach the server?
- B2. Content-Security-Policy `connect-src` injected into the document response at the Fetch response stage (continueResponse / fulfillRequest with added header): is new WebSocket(...) to a non-listed host refused? Does it also bind fetch/XHR/EventSource/sendBeacon? What about documents that are not fetched (about:blank, data:, blob:, srcdoc iframes) and workers?
- B3. Network.webSocketCreated / webSocketWillSendHandshakeRequest on a session pegboardd holds: timing relative to the server receiving the handshake; is there any CDP call that closes or fails that socket (Network.emulateNetworkConditions offline for that target, closing the target is too blunt but record it)?
- B4. Injected script replacing window.WebSocket (and the bypasses analogous to A2). Record as a weak measure.
- B5. WebTransport and WebRTC data channels: does any of B1-B2 see or block them? (One observation each is enough; "not measured" is acceptable if it needs infrastructure you do not have: say so.)

## C. A public name that resolves to a private address
Established: neither the request-stage nor the response-stage Fetch event carries the remote address; Network.responseReceived.remoteIPAddress arrives only after the response is released. Measure:
- C1. pegboardd resolving the name itself at the request stage: from the spike (Node dns.lookup with the same resolver rules you gave Chrome is not possible, so test the mechanism instead): at requestPaused, resolve the hostname with the system resolver, fail the request if any returned address is private. Measure the added latency (100 requests, cached and uncached) and describe the residual gap precisely (Chrome may resolve differently from the system resolver: --host-resolver-rules, DNS-over-HTTPS, a second lookup returning a different address).
- C2. Chrome's own private-network protections on 157: with default flags, does a page on a public (non-loopback, non-private) origin get blocked from fetching a private address or loopback (Private Network Access / Local Network Access)? Try the features/flags that exist in this build (list what you tried, exact names) and record what a page on http://<public-looking name mapped to a public-range address you serve locally> can reach. If you cannot construct a "public" origin locally, say so and stop there.
- C3. IPv6 literals and IPv4-mapped IPv6, HTTPS requests (self-signed is fine with --ignore-certificate-errors in the spike only): are they seen at the request stage the same way?

## D. Interception coverage holes in G1/G5
- D1. Cross-site iframe (OOPIF): are its subrequests and a Runtime.evaluate fetch inside it caught, with page-session Fetch only and with browser-level auto-attach?
- D2. Service worker script request: caught in arrangement B but not C. Find out whether that is a mechanism or a race (vary the order: attach+enable before resume vs after; repeat 5 times each) and say which.
- D3. Fetch.enable on the BROWSER-level session (no sessionId) and on a tab target vs its page session: does anything give context-wide interception without per-target attach?
- D4. How the product really opens tabs: the bridge extension calls chrome.tabs.create (see extension/bridge/sw.js). With browser-level auto-attach + waitForDebuggerOnStart held by another connection, is the FIRST request of a tab created by chrome.tabs.create({url}) caught? And for a tab created as about:blank then navigated by CDP? And window.open, target=_blank link click, `--new-window URL`? Give the table. (Load the bridge extension in the spike the way spikes/04-profiles.mjs loads an extension, or use chrome.tabs.create from any test extension.)
- D5. Decision order when two sessions both have Fetch enabled: repeat each ordering 5 times, including "new agent session created after P, agent enables first, then P enables", and state the rule only if it held every time.

## E. Which context ids Chrome accepts (L1, C2 and the per-profile table rest on this)
In every run so far the default browser context was the Agent profile and Chrome rejected the Human context id. The product starts the HUMAN profile first. Measure Browser.setDownloadBehavior and Browser.setPermission with each profile's context id when (a) the human profile window was opened first and is the last active, (b) the agent window was opened last, (c) after activating the human window again, (d) with three profiles. Print Target.getBrowserContexts, which id is "default", and which ids each call accepts. Say plainly whether "per profile" means per browser context or "only non-default contexts can be addressed".

## F. Downloads
- F1. A download started inside a cross-site iframe: can the top-level tab be found (Target.getTargetInfo of the iframe target: openerId / parentFrameId / browserContextId; Page.getFrameTree on each page session; Target.getTargets relationships)? Fix the summary line so "resolved" means the TAB was found.
- F2. Moving the finished guid file out of the policy folder on downloadProgress state "completed": is the file complete and closed at that moment (size equals totalBytes, rename succeeds, no later write)?
- F3. Policy connection and event connection separated: do events still arrive on the event connection when the policy connection closes, and where does a download started in that gap go?
- F4. First navigation of a brand-new blank tab is a download (both a tab made by chrome.tabs.create and one made by Target.createTarget): does the tab close, does the CDP session die, is a file written under deny / under allowAndName?

## G. Small corrections
- execCommand('cut') through the API (not only the key path).
- Print the browser version (Browser.getVersion) at the start of every spike.
- spikes/lib/launch.mjs: pick the Chrome folder by numeric version comparison among folders named chrome-<digits...> only; remove the duplicated import.
- 20: print which context is the default one.
- Fix the sentence in the downloads section that says every file went to a temporary folder (some runs deliberately let files land in ~/Downloads and then removed them).
- Wherever a conclusion about the proxy filter comes from reading filter.rs, say so in that sentence.

## H. Cleanup
In all three (and any new) scripts: close the browser on SIGINT/SIGTERM and when setup fails before the main try; 21 removes its uniquely named files from ~/Downloads on those paths too.

## Output
- New or extended spike scripts (new ids may go in spikes/23-*.mjs etc.).
- docs/spike-findings.md corrected in place (keep the structure; change verdicts and wording to match evidence; add the new measurements; restate the four stop-condition verdicts with exactly what is established; restate the "method for Tasks 4, 5, 7" lines using only measured facts; add a short list "미측정" at the end).
- docs/plans/phase2a-notes/task-2-report.md: append "Round 2" with the raw final output of each script.
- Commit, then push with: git -c credential.helper='!gh auth git-credential' push origin HEAD:refs/heads/phase2a-task2-spikes
- Final message under 25 lines: status, commit, then one line each: clipboard (what remains open with the best measured defence), WebSocket (blockable or not, by what), name-resolves-to-private (blockable or not, by what), context ids (what "per profile" turned out to mean), tab-creation first request (caught or not for chrome.tabs.create).
