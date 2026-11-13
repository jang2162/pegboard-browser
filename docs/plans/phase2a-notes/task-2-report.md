# Task 2 report: what Chrome for Testing 157 allows (phase 2a measurements)

Browser: Google Chrome for Testing 157.0.8103.78 (the newest under the agent-browser browsers directory), macOS, headed, one temporary user-data-dir per spike with a `Human` and an `Agent` profile opened as `04-profiles.mjs` does. Findings and the method for Tasks 4, 5 and 7 are in `docs/spike-findings.md`, section "2a단계 측정". This file is the run record.

## What I ran

- `spikes/20-request-guard.mjs`, `21-downloads.mjs`, `22-clipboard.mjs`, plus a small helper `spikes/lib/probe.mjs` (probe web server that logs what reaches it, two-profile launcher, extra CDP connections). `spikes/lib/launch.mjs` now picks the newest `chrome-*` browser directory when `CHROME` is not set (the old default pointed at a version that is not installed here).
- Each spike was run at least twice after its last edit: 20 (two runs after the last edit, plus two earlier), 21 (two runs after the last edit, plus earlier ones while I fixed the spike), 22 (two runs after the last edit). Differences between the final two runs of each: millisecond figures (G3, G6, L3 progress timing), and one G4 line where two events carried the same millisecond timestamp (combo 11: `first=P` in one run, `first=A` in the other; the page result was identical). Nothing else differed.
- The only things the spikes did not do themselves: nothing was run against the pegboard proxy; statements about `filter.rs` are from reading the source.
- Final state checked after the last runs: `pgrep -fl "Chrome for Testing"` shows no process I started; no file with a probe run id is left in `~/Downloads`; the system clipboard was empty before and is empty after (no marker).

## Final output of each spike (second run of the final pair)

The `saved` lines are shortened (the original shows an absolute path). Nothing else is edited. `LAN-IP` in 20 stands for an RFC 1918 address of this machine that the script replaces in its own output.

### 20-request-guard.mjs

```
G1.A: {"notCaught":["websocket","sharedWorkerFetch","serviceWorkerScript","serviceWorkerFetch"],"caught":["nav:Document","iframe:Document","img:Image","pageFetch:XHR","evalFetch:XHR","xhr:XHR","beacon:Ping","dedicatedWorkerScript:Other","dedicatedWorkerFetch:XHR","sharedWorkerScript:Other"]}
G1.B: {"notCaught":["websocket","sharedWorkerFetch"],"caught":["nav:page","iframe:page","img:page","pageFetch:page","evalFetch:page","xhr:page","beacon:page","dedicatedWorkerScript:page","dedicatedWorkerFetch:page","sharedWorkerScript:page","serviceWorkerScript:service_worker","serviceWorkerFetch:service_worker"],"attached":["worker/undefined/undefined/FETCH-ERR Fetch.enable: 'Fetch.enable' wasn't found","worker/Agent/page/wait","worker/undefined/undefined/FETCH-ERR Fetch.enable: 'Fetch.enable' wasn't found","service_worker/Agent/page/wait"]}
G1.C: {"notCaught":["websocket","serviceWorkerScript"],"caught":["nav:page","iframe:page","img:page","pageFetch:page","evalFetch:page","xhr:page","beacon:page","dedicatedWorkerScript:page","dedicatedWorkerFetch:page","sharedWorkerScript:page","sharedWorkerFetch:shared_worker","serviceWorkerFetch:service_worker"],"attached":["shared_worker/Agent/browser/wait","service_worker/Agent/browser/wait"]}
G1.websocket: handshake never reaches Fetch.requestPaused (phases A, B, C); Network.webSocketCreated at P with Network.enable: ["ws://127.0.0.1:PORT/t/ws?p=A"] (observation only; blocking it was not measured)
G1: evalFetch and pageFetch caught with page-session Fetch: PASS; not caught in phase A (page session only): websocket,sharedWorkerFetch,serviceWorkerScript,serviceWorkerFetch
G2.request: url+redirectedRequestId only; request-stage keys={"top":["requestId","request","frameId","resourceType","networkId"],"request":["url","method","headers","initialPriority","referrerPolicy"]}; remote-address field present at request stage: false
G2.response: {"keys":{"top":["requestId","request","frameId","resourceType","responseStatusCode","responseStatusText","responseHeaders","networkId"],"request":["url","method","headers","initialPriority","referrerPolicy"]},"remoteIpAtPause":null,"remoteIpAfter500ms":null,"responseReceivedBeforePauseEvent":false}
G2.redirect: {"requestStageOnly":["/t/redirect","/t/body<-redirect"],"bothStages":["Request::/t/redirect","Response:302:/t/redirect","Request::/t/body:redirected","Response:200:/t/body:redirected"]}
G2.cut: PASS body not delivered (ERR:Failed to fetch); server was contacted: true
G3: {"note":"names mapped by --host-resolver-rules in one launch: loop.test->127.0.0.1, lan.test->a private LAN address of this machine, ten.test->10.255.255.1 (unroutable)","loop":{"requestStageSeesIP":false,"ripAtResponsePause":null,"ripWithin600ms":null,"getResponseBodyAtPause":"ok:16chars","ripAfterGetResponseBody":null,"ripAfterContinue":{"value":"127.0.0.1","receivedByP_msAfterContinue":2,"pageGotHeadersAt_msAfterContinue":1},"bodyReachedPage":true,"serverContactedBeforeRequestStageContinue":false,"serverHitsTotal":1,"failed":[]},"lan":{"requestStageSeesIP":false,"ripAtResponsePause":null,"ripWithin600ms":null,"getResponseBodyAtPause":"ok:16chars","ripAfterGetResponseBody":null,"ripAfterContinue":{"value":"LAN-IP","receivedByP_msAfterContinue":2,"pageGotHeadersAt_msAfterContinue":1},"bodyReachedPage":true,"serverContactedBeforeRequestStageContinue":false,"serverHitsTotal":1,"failed":[]},"ten":{"requestStageSeesIP":false,"bodyReachedPage":false,"serverContactedBeforeRequestStageContinue":false,"serverHitsTotal":0,"failed":[]}}
G4.switchOff: A's Fetch.disable=ok; P still caught after: Fetch.disable=true, Network.setBypassServiceWorker=true, Network.disable=true, Target.setAutoAttach(off)=true => PASS
G4.steal: {"aContinue":"Fetch.continueRequest: Invalid InterceptionId.","aFail":"Fetch.failRequest: Invalid InterceptionId.","aFulfill":"Fetch.fulfillRequest: Invalid InterceptionId."}
G4.combo: enable P then A | P:continue A:fulfill:FROM-A | P saw=true A saw=true first=P | page saw OK:FROM-A | server reached=false
G4.combo: enable P then A | P:fail A:continue | P saw=true A saw=false first=P | page saw ERR:Failed to fetch | server reached=false
G4.combo: enable P then A | P:fail A:fulfill:FROM-A | P saw=true A saw=false first=P | page saw ERR:Failed to fetch | server reached=false
G4.combo: enable P then A | P:fulfill:FROM-P A:continue | P saw=true A saw=false first=P | page saw OK:FROM-P | server reached=false
G4.combo: enable P then A | P:continue A:fail | P saw=true A saw=true first=P | page saw ERR:Failed to fetch | server reached=false
G4.combo: enable A then P | P:fail A:fulfill:FROM-A | P saw=true A saw=false first=P | page saw ERR:Failed to fetch | server reached=false
G4.combo: enable P then A | P:fail+400ms A:fulfill:FROM-A | P saw=true A saw=false first=P | page saw ERR:Failed to fetch | server reached=false
G4.combo: enable A then P | P:continue A:fulfill:FROM-A | P saw=true A saw=true first=P | page saw OK:FROM-A | server reached=false
G4.combo: enable P then A (A uses a NEW session made after P) | P:fail A:fulfill:FROM-A2 | P saw=false A saw=true first=A | page saw OK:FROM-A2 | server reached=false
G4.combo: enable P then A (A uses a NEW session made after P) | P:continue A:fulfill:FROM-A2 | P saw=false A saw=true first=A | page saw OK:FROM-A2 | server reached=false
G4.combo: enable P then A (A uses a NEW session made after P) | P:fail A:continue | P saw=true A saw=true first=A | page saw ERR:Failed to fetch | server reached=false
G4.serviceWorker: {"pageControlledBySW":true,"pageFetch_caughtByP_pageSessionOnly":[],"pageFetch_caughtByP_whenAbypassesSW":["page:XHR"],"caughtByP_whenPAlsoAttachedToSwTarget":["service_worker:XHR:sw=1"],"serverHitsInOrder_plain_bypass_withSw":["SW","page","SW"]}
G4.workerRace: {"workerFetchCaughtWhenOnlyPAutoAttaches":true,"workerFetchCaughtWhenAlsoAAutoAttachesAndResumesAtOnce":true,"workerFetchArrivedAtServer":true,"aSawAttach":["service_worker","worker","worker","worker"]}
G4: A cannot switch off P: yes; A cannot override a P block (fail) with continue/fulfill in any tried order: NO; worker race hole: no
G5.closeReleases: {"heldWhileOpen":{"pausedAtP2":1,"serverReached":false},"heldRequestAfterClose":"OK:not found","laterRequestAfterClose":"OK:not found"}
G5.perTab: {"popupWithoutAutoAttach":{"firstRequestCaught":false,"arrived":true},"popupWithPageAutoAttach":{"firstRequestCaught":false,"attached":["service_worker/nowait","worker/nowait","worker/nowait","worker/nowait","worker/nowait","worker/nowait","worker/nowait"],"arrived":true}}
G5.popupBrowserAutoAttach: {"firstRequestCaught":true,"subresourceCaught":true,"arrived":true,"attached":["page/Agent/wait"]}
G5.newWindow: {"discoverThenAttach":{"firstRequestCaught":false,"arrived":true,"attachedAfterCreate":true},"browserAutoAttachWait":{"firstRequestCaught":false,"arrived":true,"attached":["other/Agent/wait","browser_ui/Agent/wait","browser_ui/Agent/wait","page/Agent/wait"]},"humanTabsSeenByBrowserAutoAttach":{"humanTabOpened":"opened","attached":["page/Human/wait"]}}
G5: closing the P connection RESUMED the held request; later requests pass unguarded; popup first request caught: plain=false, pageAutoAttach=false; new browser-made window first request caught: discoverThenAttach=false, browserAutoAttach=false
G6: mean per request (100 sequential fetches): baseline 0.40ms (runs 0.51/0.28); request-stage 0.54ms (+0.15); request+response stage 0.55ms (+0.16)
saved spikes/out/20-request-guard.json
```

### 21-downloads.mjs

```
L1.perProfile: PASS: this launch's default context=Agent; D1 set Agent dir by browserContextId (ok), D2 set Human dir without a context id (skipped (default context is the Agent profile; a policy without a context id would land on the Agent profile); with the Human context id: Browser.setDownloadBehavior: Failed to find browser context for id 24D5E4421D067E15ADBE78964A15C980; default context is Agent); agent download landed: {"agentDir":true,"humanDir":false,"extraDir":false,"userDownloadsByName":false}; human download landed: {"agentDir":false,"humanDir":false,"extraDir":false,"userDownloadsByName":true}; begin events seen by ["D1(agent policy)"] for the agent download and ["D1(agent policy)"] for the human one
L2.linkClick: linkClick: begin seen, frameId present, byTargetId={"type":"page","ctx":"Agent","alive":true,"opener":null,"openerCtx":null}, byFrameTree={"ownerTarget":"main-frame-of-that-tab","ctx":"Agent","isMainFrame":true}
L2.directNavigate: directNavigate: begin seen, frameId present, byTargetId={"type":"page","ctx":"Agent","alive":true,"opener":null,"openerCtx":null}, byFrameTree={"ownerTarget":"main-frame-of-that-tab","ctx":"Agent","isMainFrame":true}
L2.blobDownload: blobDownload: begin seen, frameId present, byTargetId={"type":"page","ctx":"Agent","alive":true,"opener":null,"openerCtx":null}, byFrameTree={"ownerTarget":"main-frame-of-that-tab","ctx":"Agent","isMainFrame":true}
L2.windowOpenTab: windowOpenTab: begin seen, frameId present, byTargetId={"type":"page","ctx":"Agent","alive":false,"opener":"known-target","openerCtx":"Agent"}, byFrameTree=none
L2.iframeSameSite: iframeSameSite: begin seen, frameId present, byTargetId=none, byFrameTree={"ownerTarget":"child-frame-of-a-tab","ctx":"Agent","isMainFrame":false}
L2.iframeCrossSite: iframeCrossSite: begin seen, frameId present, byTargetId={"type":"iframe","ctx":"Agent","alive":true,"opener":null,"openerCtx":null}, byFrameTree=none
L2.swNavigatesClient: swNavigatesClient: begin seen, frameId present, byTargetId={"type":"page","ctx":"Agent","alive":true,"opener":null,"openerCtx":null}, byFrameTree={"ownerTarget":"main-frame-of-that-tab","ctx":"Agent","isMainFrame":true}
L2.swServedAttachment: swServedAttachment: begin seen, frameId present, byTargetId={"type":"page","ctx":"Agent","alive":true,"opener":null,"openerCtx":null}, byFrameTree={"ownerTarget":"main-frame-of-that-tab","ctx":"Agent","isMainFrame":true}
L2.humanTabClick: humanTabClick: NO EVENT, frameId undefined, byTargetId=none, byFrameTree=none
L2.humanTabJsClick: humanTabJsClick: begin seen, frameId present, byTargetId={"type":"page","ctx":"Human","alive":true,"opener":null,"openerCtx":null}, byFrameTree={"ownerTarget":"main-frame-of-that-tab","ctx":"Human","isMainFrame":true}
L2.iframeTargets: [{"ctx":"Agent","alive":true},{"ctx":"Agent","alive":true}]
L2: link click resolves to an Agent tab: PASS; all agent scenarios resolved: linkClick=true directNavigate=true blobDownload=true windowOpenTab=true iframeSameSite=true iframeCrossSite=true swNavigatesClient=true swServedAttachment=true; human tab, synthetic mouse click (hidden window): NO downloadWillBegin; human tab, JS click: resolves to Human: true (a real person's click: not measured)
L3.withContentLength: cancel ok in 50ms, final state canceled, totalBytes first reported 30000000, received at first event >= limit 6291456, final received 0, progress events 4 (received series [0,0,6291456,0], ms between events [0,507,7]), server sent 6553600, server saw early close true, leftover files []
L3.withoutContentLength: cancel ok in 3ms, final state canceled, totalBytes first reported 0, received at first event >= limit 6553600, final received 0, progress events 4 (received series [0,0,6553600,0], ms between events [2,502,3]), server sent 6553600, server saw early close true, leftover files []
L4.plainAscii: suggestedFilename="pbspike-<run>-l4-ascii.txt"; file on disk is named by the download guid: true (guid shaped like a uuid: true)
L4.utf8Star: suggestedFilename="한글-pbspike-<run>-l4-utf8.txt"; file on disk is named by the download guid: true (guid shaped like a uuid: true)
L4.traversal: suggestedFilename="_.._pbspike-<run>-l4-trav.txt"; file on disk is named by the download guid: true (guid shaped like a uuid: true)
L4.noHeader: suggestedFilename="plain.bin"; file on disk is named by the download guid: true (guid shaped like a uuid: true)
L5: policy connection closed after {"state":"inProgress","received":851968}; the download FINISHED (server sent all); agent dir during=["d7a6113b…:1114112"] after 500ms=["d7a6113b…:1900544"] after 6s=["d7a6113b…:4000000"]; the finished file landed: {"agentDir":true,"userDownloads":false} (files in ~/Downloads with this run id: 2); events after close seen on: []
L5.afterLoss: new agent download after the policy connection is gone landed: null; events seen on []
L1b: D4 holds Agent->agent dir (ok); D3 then sets Human (skipped (default context is Agent)) and Agent->extra dir (ok); agent download landed in: the NEWER connection's dir; D3's human policy still worked: {"landed":{"agentDir":false,"humanDir":false,"extraDir":false,"userDownloadsByName":true},"events":["D3(both)"]}; after the newer connection (D3) closed, an agent download landed in: neither (5 file(s) went to ~/Downloads)
Downloads: files with this run's id found in ~/Downloads and removed: 5 ["pbspike-<run>-l1-human","pbspike-<run>-l1b-d3closed","pbspike-<run>-l1b-human","pbspike-<run>-l2-human-js","pbspike-<run>-l5-after"]
saved spikes/out/21-downloads.json
```

### 22-clipboard.mjs

```
ENV: {"defaultBrowserContext":"Agent","systemClipboardBytesAtStart":0,"clipboardTypesAtStart":"text-only"}
C1.states: {"read":"prompt","write":"granted"}
C1.noFocusEmulation: W1_writeText_evaluate_gesture=unchanged (page: ERR NotAllowedError: Failed to execute 'writeText' on 'Clipboard': Doc); W1b_writeText_evaluate_noGesture=unchanged (page: ERR NotAllowedError: Failed to execute 'writeText' on 'Clipboard': Doc); R1_readText_evaluate_gesture=saw "ERR NotAllowedError: Failed to execute '"; R1b_readText_evaluate_noGesture=saw "ERR NotAllowedError: Failed to execute '"
C1.noFocusEmulation_more: W2_writeText_realClickHandler=CHANGED to expected marker (page: ok); W3_execCommandCopy_evaluate_gesture=unchanged (page: false); W4_execCommandCopy_realClickHandler=CHANGED to expected marker (page: true); W5_clipboardWrite_realClickHandler=CHANGED to expected marker (page: ok); W6_key_Meta+C_with_commands_copy=CHANGED to expected marker; W8_key_Meta+X_with_commands_cut=CHANGED to expected marker; C5_copyHandler_viaExecCommand=unchanged (page: false); R2_readText_realClickHandler=nothing; R4_key_Meta+V_with_commands_paste=PAGE SAW THE SYSTEM CLIPBOARD
C1.withFocusEmulation: W1_writeText_evaluate_gesture=CHANGED to expected marker (page: ok); W1b_writeText_evaluate_noGesture=unchanged (page: PENDING: no answer in 5s); W2_writeText_realClickHandler=CHANGED to expected marker (page: ok); W3_execCommandCopy_evaluate_gesture=CHANGED to expected marker (page: true); W3b_execCommandCopy_evaluate_noGesture=unchanged (page: false); W4_execCommandCopy_realClickHandler=CHANGED to expected marker (page: true); W5_clipboardWrite_realClickHandler=CHANGED to expected marker (page: ok); W6_key_Meta+C_with_commands_copy=CHANGED to expected marker; W7_key_Meta+C_no_commands=unchanged; W8_key_Meta+X_with_commands_cut=CHANGED to expected marker; W9_key_Meta+X_no_commands=unchanged; C5_copyHandler_viaExecCommand=CHANGED to expected marker (page: true); C5b_copyHandler_viaKeyCommands=CHANGED to expected marker; R1_readText_evaluate_gesture=saw "PENDING: no answer in 5s"; R1b_readText_evaluate_noGesture=saw "PENDING: no answer in 5s"; R2_readText_realClickHandler=saw "ERR NotAllowedError: Failed to execute '"; R3_execCommandPaste_evaluate_gesture=saw ""; R4_key_Meta+V_with_commands_paste=PAGE SAW THE SYSTEM CLIPBOARD; R5_key_Meta+V_no_commands=saw " | paste event: null"
C1.humanTabControl: W1_writeText_evaluate_gesture=CHANGED to expected marker (page: ok); W3_execCommandCopy_evaluate_gesture=CHANGED to expected marker (page: true); R1_readText_evaluate_gesture=saw "PENDING: no answer in 5s"
C2.setPermission: {"clipboard-read":"ok","clipboard-write":"ok","clipboard-sanitized-write":"Browser.setPermission: Invalid PermissionDescriptor name: clipboard-sanitized-write"} | with the Human context id: Browser.setPermission: Failed to find browser context for id 06FF3C377A9A0A306C3BCDA6E1646300 | grantPermissions: ok (grant only; there is no deny here) | resetPermissions: ok
C2.statesAfterDeny: {"read":"denied","write":"denied"}
C2.deniedSuite: W1_writeText_evaluate_gesture=unchanged (page: ERR NotAllowedError: Failed to execute 'writeText' on 'Clipboard': Wri); W1b_writeText_evaluate_noGesture=unchanged (page: ERR NotAllowedError: Failed to execute 'writeText' on 'Clipboard': Wri); W2_writeText_realClickHandler=unchanged (page: ERR NotAllowedError: Failed to execute 'writeText' on 'Clipboard': Wri); W3_execCommandCopy_evaluate_gesture=CHANGED to expected marker (page: true); W3b_execCommandCopy_evaluate_noGesture=unchanged (page: false); W4_execCommandCopy_realClickHandler=CHANGED to expected marker (page: true); W5_clipboardWrite_realClickHandler=unchanged (page: ERR NotAllowedError: Failed to execute 'write' on 'Clipboard': Write p); W6_key_Meta+C_with_commands_copy=CHANGED to expected marker; W7_key_Meta+C_no_commands=unchanged; W8_key_Meta+X_with_commands_cut=CHANGED to expected marker; W9_key_Meta+X_no_commands=unchanged; C5_copyHandler_viaExecCommand=CHANGED to expected marker (page: true); C5b_copyHandler_viaKeyCommands=CHANGED to expected marker; R1_readText_evaluate_gesture=saw "ERR NotAllowedError: Failed to execute '"; R1b_readText_evaluate_noGesture=saw "ERR NotAllowedError: Failed to execute '"; R2_readText_realClickHandler=saw "ERR NotAllowedError: Failed to execute '"; R3_execCommandPaste_evaluate_gesture=saw ""; R4_key_Meta+V_with_commands_paste=PAGE SAW THE SYSTEM CLIPBOARD; R5_key_Meta+V_no_commands=saw " | paste event: null"
C2.scope: {"sameOriginNewTabSameProfile":{"states":{"read":"denied","write":"denied"},"writeText":"ERR NotAllowedError: on 'Clipboard': Write permission denied."},"otherOriginSameProfile":{"states":{"read":"prompt","write":"granted"},"writeText":"ok"},"humanTabSameOrigin":{"states":{"read":"prompt","write":"granted"},"writeText":"ok"},"agentTabBeforeSetterCloses":{"states":{"read":"denied","write":"denied"},"writeText":"ERR NotAllowedError: on 'Clipboard': Write permission denied."},"agentTabAfterSetterConnectionClosed":{"states":{"read":"prompt","write":"granted"},"writeText":"ok"},"noOriginGiven_otherOrigin":{"states":{"read":"denied","write":"denied"},"writeText":"ERR NotAllowedError: on 'Clipboard': Write permission denied."},"afterResetByS2_otherOrigin":{"states":{"read":"prompt","write":"granted"},"writeText":"ok"},"afterResetByS2_originSetByClosedS_O3":{"states":{"read":"prompt","write":"granted"},"writeText":"ok"}} | state after resetPermissions: {"read":"prompt","write":"granted"}
C3: execCommand('copy') default [W3_execCommandCopy_evaluate_gesture=CHANGED to expected marker, W3b_execCommandCopy_evaluate_noGesture=unchanged, W4_execCommandCopy_realClickHandler=CHANGED to expected marker] and with all clipboard permissions denied [W3_execCommandCopy_evaluate_gesture=CHANGED to expected marker, W3b_execCommandCopy_evaluate_noGesture=unchanged, W4_execCommandCopy_realClickHandler=CHANGED to expected marker]; execCommand('paste') default [R3_execCommandPaste_evaluate_gesture=saw ""] denied [R3_execCommandPaste_evaluate_gesture=saw ""]
C4: default [W6_key_Meta+C_with_commands_copy=CHANGED to expected marker, W7_key_Meta+C_no_commands=unchanged, W8_key_Meta+X_with_commands_cut=CHANGED to expected marker, W9_key_Meta+X_no_commands=unchanged, R4_key_Meta+V_with_commands_paste=PAGE SAW THE SYSTEM CLIPBOARD, R5_key_Meta+V_no_commands=saw " | paste event: null"]; denied [W6_key_Meta+C_with_commands_copy=CHANGED to expected marker, W7_key_Meta+C_no_commands=unchanged, W8_key_Meta+X_with_commands_cut=CHANGED to expected marker, W9_key_Meta+X_no_commands=unchanged, R4_key_Meta+V_with_commands_paste=PAGE SAW THE SYSTEM CLIPBOARD, R5_key_Meta+V_no_commands=saw " | paste event: null"]
C5: default [C5_copyHandler_viaExecCommand=CHANGED to expected marker, C5b_copyHandler_viaKeyCommands=CHANGED to expected marker]; denied [C5_copyHandler_viaExecCommand=CHANGED to expected marker, C5b_copyHandler_viaKeyCommands=CHANGED to expected marker]
C3.override: {"overrideActiveInPage":true,"realClickWithOverride":{"pageSaw":"false","system":"unchanged"},"keyCommandsWithOverride":"CHANGED to expected marker","iframeInheritsOverride":"false","srcdocIframeInheritsOverride":"false","overrideActiveAfterInjectingConnectionClosed":false,"afterInjectingConnectionClosed_realClick":{"pageSaw":"true","system":"CHANGED to expected marker"}}
Clipboard: restored (0 bytes of text); current system clipboard equals the saved one: true; contains a pbclip-run marker: false
saved spikes/out/22-clipboard.json
```

## Stop conditions (Task 2 step 5)

1. G1, `Runtime.evaluate` fetch or page-script fetch not caught: **not met**. Both were caught in all three arrangements. Gaps to design for: WebSocket handshakes are never seen by `Fetch`; requests a service worker answers with `respondWith` are invisible on the page session; service-worker and shared-worker requests need their own attached session.
2. G4, agent side can switch off or hide interception and the proxy filter cannot stop it: **not met**. The agent connection cannot switch the guard off or steal its paused requests. A session created after the guard's that also enables `Fetch` decides first and defeats a block; `Fetch.enable` is not in the proxy allow list (read in `filter.rs`, not exercised). The service-worker route is not a CDP method and needs the guard to attach to the service worker target.
3. L2, link-click download cannot be attributed to a tab: **not met**. All agent scenarios resolved to an Agent tab. Exception recorded: the parent tab of a cross-site iframe (OOPIF) is not derivable from `frameId` alone.
4. C1-C4, a write path with no way to block it: **met**. `document.execCommand('copy')` with a user gesture (page click handler, or `userGesture:true`) writes the system clipboard even with every clipboard permission denied. Key events with `commands:["copy"]` also write regardless of permissions (blockable only by rejecting `commands` in the proxy). The only blocker I measured is a per-document script override, which disappears when the injecting connection closes and does not cover the key-command path. Reads can be blocked (permission for `readText`, proxy for `commands:["paste"]`).

## What I could not measure, and why

- A real person's click or keystroke (downloads in the Human window, Cmd+C/V): no person at the machine. Synthetic mouse events into the hidden Human window did not start a download within 12 s; a JS click did.
- Anything through the real pegboard proxy: the spikes talk to Chrome directly. The proxy statements come from reading `filter.rs`.
- Whether a WebSocket can be blocked at all (only the `Network.webSocketCreated` observation was recorded).
- Resolving a name on the daemon side to catch "public name, private address" (the response stage cannot do it; the alternative was not tried). HTTPS interception, IPv6 literals, DNS rebinding.
- The Human profile's context id is rejected by `Browser.setDownloadBehavior` and `Browser.setPermission` ("Failed to find browser context"); the default context was the Agent profile in every launch of spikes 21 and 22. So per-profile policy for the Human profile (and one connection holding two profiles) could not be measured; spike 20 did not print which context was default.
- Service-worker and shared-worker "resume race" (an agent resuming a worker before the guard enables Fetch) was tested only for dedicated workers, which turned out to ride the page session.
- Clipboard: text only. The clipboard was empty at the start. The restore writes the saved bytes back with `pbcopy`; after the first run the clipboard reported an (empty) text item where it had reported no types before. Non-text content would have made the spike refuse to run.
- Bypassing the `execCommand` override from inside the page (only same-page iframes and `srcdoc` iframes were tried; both inherit it).
- The G3 hosts were mapped in one launch rather than in separate runs.

## Surprising or worth knowing

- **Files did land in `~/Downloads`.** This is what L1/L1b/L5 measure (Human-profile downloads have no policy, and once the policy connection is gone or replaced, Chrome falls back to the default folder). Every `21-downloads.mjs` run left a few probe files there (the Human-profile ones and the "after policy loss" ones; 3 to 14 per run, 14 in the first run, where I had mistakenly kept a policy-less state for longer). Each file had a name containing a random per-run id, the script's `finally` block removed exactly the files carrying that id, and a listing afterwards showed none left. Nothing else in that folder was touched.
- The fail-open pattern is the same everywhere: guard (`Fetch`), clipboard permission, script injection and download policy all vanish when the connection that set them closes. A second connection setting the same download policy wins, and closing it does not restore the first.
- Closing the download policy connection does not stop an in-progress download, but its progress events stop, so completion is never reported.
- Download progress events arrive only about every 500 ms, so a size limit overshoots by about 500 ms of throughput.
- The newest session decides first when two sessions both use `Fetch`, regardless of who enabled it first.
- A service worker that answers a page request hides it from a page-session guard, and `Network.setBypassServiceWorker` on the agent side does the opposite of hiding (it makes the request visible).
- Browser-level auto-attach with `waitForDebuggerOnStart` also freezes new tabs of the Human profile until they are resumed; the proxy's rewrite of that flag for agents matters.
- A permission prompt left unanswered (default `clipboard-read` is `prompt`) makes `readText` hang, and repeated prompts get the origin blocked by Chrome's own embargo, which can look like a denial you did not set.


## Round 2

Reviewer follow-up (`task-2-round2.md`). Scripts 20-22 were changed only for cleanup and printing (version, default context, signal/early-failure cleanup; `lib/launch.mjs` picks the Chrome folder by numeric version among `chrome-<digits...>` folders). New scripts: `23-clipboard-defences.mjs` (A, G cut), `24-websocket.mjs` (B), `25-private-names.mjs` (C), `26-coverage.mjs` (D), `27-contexts-downloads.mjs` (E, F). Every script was run twice on Chrome for Testing 157.0.8103.78; the output below is the second run's full final output with absolute paths scrubbed (`<repo>`, `<home>`).

Corrections to the Round 1 notes above (the findings document carries the full text):
- "The newest session decides first when two sessions both use `Fetch`" is not a rule that held: D5 (7 orderings x 5 repeats, each stable) is explained by neither session age nor connection age.
- Default context was the Agent profile in Round 1 because the Agent window was opened last; Chrome only accepts the current default context's id (E).
- Clipboard stop condition 4 was too strong: a capture-phase listener injection blocked `execCommand` and `commands` key paths, with a measured hole (`sandbox="allow-scripts"` iframe).
- One scripting error was found and fixed during this round: script 23 left an injected defence on the agent tab after section A3, so section A4-A6 of the first two full runs (not reported anywhere) ran with a defence active; the reported runs are the two after the fix.
- Script 24's `--only=b2` debugging and a throwaway scratch script (CSP via `fulfillRequest` vs `continueResponse`, allowed-connection hang; deleted, not committed) were used to find that `continueResponse` does not apply an added CSP header and that connections the CSP allows hung.
- Clipboard restored after every run (empty before and after); no Chrome for Testing process left; no file with a run id left in `~/Downloads`.

Differences between the two runs of each script: millisecond figures, target-attach order in the printed `A3.attached` list, the order of profile pages in the E table, G4 combo 11 (same-millisecond tie, as in Round 1), and L3/L5 byte counts at cancel time.

### 20-request-guard.mjs (second run, full output)

```text
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
G1.A: {"notCaught":["websocket","sharedWorkerFetch","serviceWorkerScript","serviceWorkerFetch"],"caught":["nav:Document","iframe:Document","img:Image","pageFetch:XHR","evalFetch:XHR","xhr:XHR","beacon:Ping","dedicatedWorkerScript:Other","dedicatedWorkerFetch:XHR","sharedWorkerScript:Other"]}
G1.B: {"notCaught":["websocket","sharedWorkerFetch"],"caught":["nav:page","iframe:page","img:page","pageFetch:page","evalFetch:page","xhr:page","beacon:page","dedicatedWorkerScript:page","dedicatedWorkerFetch:page","sharedWorkerScript:page","serviceWorkerScript:service_worker","serviceWorkerFetch:service_worker"],"attached":["worker/undefined/undefined/FETCH-ERR Fetch.enable: 'Fetch.enable' wasn't found","worker/Agent/page/wait","worker/undefined/undefined/FETCH-ERR Fetch.enable: 'Fetch.enable' wasn't found","service_worker/Agent/page/wait"]}
G1.C: {"notCaught":["websocket","serviceWorkerScript"],"caught":["nav:page","iframe:page","img:page","pageFetch:page","evalFetch:page","xhr:page","beacon:page","dedicatedWorkerScript:page","dedicatedWorkerFetch:page","sharedWorkerScript:page","sharedWorkerFetch:shared_worker","serviceWorkerFetch:service_worker"],"attached":["shared_worker/Agent/browser/wait","service_worker/Agent/browser/wait"]}
G1.websocket: handshake never reaches Fetch.requestPaused (phases A, B, C); Network.webSocketCreated at P with Network.enable: ["ws://127.0.0.1:PORT/t/ws?p=A"] (observation only; blocking it was not measured)
G1: evalFetch and pageFetch caught with page-session Fetch: PASS; not caught in phase A (page session only): websocket,sharedWorkerFetch,serviceWorkerScript,serviceWorkerFetch
G2.request: url+redirectedRequestId only; request-stage keys={"top":["requestId","request","frameId","resourceType","networkId"],"request":["url","method","headers","initialPriority","referrerPolicy"]}; remote-address field present at request stage: false
G2.response: {"keys":{"top":["requestId","request","frameId","resourceType","responseStatusCode","responseStatusText","responseHeaders","networkId"],"request":["url","method","headers","initialPriority","referrerPolicy"]},"remoteIpAtPause":null,"remoteIpAfter500ms":null,"responseReceivedBeforePauseEvent":false}
G2.redirect: {"requestStageOnly":["/t/redirect","/t/body<-redirect"],"bothStages":["Request::/t/redirect","Response:302:/t/redirect","Request::/t/body:redirected","Response:200:/t/body:redirected"]}
G2.cut: PASS body not delivered (ERR:Failed to fetch); server was contacted: true
G3: {"note":"names mapped by --host-resolver-rules in one launch: loop.test->127.0.0.1, lan.test->a private LAN address of this machine, ten.test->10.255.255.1 (unroutable)","loop":{"requestStageSeesIP":false,"ripAtResponsePause":null,"ripWithin600ms":null,"getResponseBodyAtPause":"ok:16chars","ripAfterGetResponseBody":null,"ripAfterContinue":{"value":"127.0.0.1","receivedByP_msAfterContinue":1,"pageGotHeadersAt_msAfterContinue":1},"bodyReachedPage":true,"serverContactedBeforeRequestStageContinue":false,"serverHitsTotal":1,"failed":[]},"lan":{"requestStageSeesIP":false,"ripAtResponsePause":null,"ripWithin600ms":null,"getResponseBodyAtPause":"ok:16chars","ripAfterGetResponseBody":null,"ripAfterContinue":{"value":"LAN-IP","receivedByP_msAfterContinue":2,"pageGotHeadersAt_msAfterContinue":2},"bodyReachedPage":true,"serverContactedBeforeRequestStageContinue":false,"serverHitsTotal":1,"failed":[]},"ten":{"requestStageSeesIP":false,"bodyReachedPage":false,"serverContactedBeforeRequestStageContinue":false,"serverHitsTotal":0,"failed":[]}}
G4.switchOff: A's Fetch.disable=ok; P still caught after: Fetch.disable=true, Network.setBypassServiceWorker=true, Network.disable=true, Target.setAutoAttach(off)=true => PASS
G4.steal: {"aContinue":"Fetch.continueRequest: Invalid InterceptionId.","aFail":"Fetch.failRequest: Invalid InterceptionId.","aFulfill":"Fetch.fulfillRequest: Invalid InterceptionId."}
G4.combo: enable P then A | P:continue A:fulfill:FROM-A | P saw=true A saw=true first=P | page saw OK:FROM-A | server reached=false
G4.combo: enable P then A | P:fail A:continue | P saw=true A saw=false first=P | page saw ERR:Failed to fetch | server reached=false
G4.combo: enable P then A | P:fail A:fulfill:FROM-A | P saw=true A saw=false first=P | page saw ERR:Failed to fetch | server reached=false
G4.combo: enable P then A | P:fulfill:FROM-P A:continue | P saw=true A saw=false first=P | page saw OK:FROM-P | server reached=false
G4.combo: enable P then A | P:continue A:fail | P saw=true A saw=true first=P | page saw ERR:Failed to fetch | server reached=false
G4.combo: enable A then P | P:fail A:fulfill:FROM-A | P saw=true A saw=false first=P | page saw ERR:Failed to fetch | server reached=false
G4.combo: enable P then A | P:fail+400ms A:fulfill:FROM-A | P saw=true A saw=false first=P | page saw ERR:Failed to fetch | server reached=false
G4.combo: enable A then P | P:continue A:fulfill:FROM-A | P saw=true A saw=true first=P | page saw OK:FROM-A | server reached=false
G4.combo: enable P then A (A uses a NEW session made after P) | P:fail A:fulfill:FROM-A2 | P saw=false A saw=true first=A | page saw OK:FROM-A2 | server reached=false
G4.combo: enable P then A (A uses a NEW session made after P) | P:continue A:fulfill:FROM-A2 | P saw=false A saw=true first=A | page saw OK:FROM-A2 | server reached=false
G4.combo: enable P then A (A uses a NEW session made after P) | P:fail A:continue | P saw=true A saw=true first=P | page saw ERR:Failed to fetch | server reached=false
G4.serviceWorker: {"pageControlledBySW":true,"pageFetch_caughtByP_pageSessionOnly":[],"pageFetch_caughtByP_whenAbypassesSW":["page:XHR"],"caughtByP_whenPAlsoAttachedToSwTarget":["service_worker:XHR:sw=1"],"serverHitsInOrder_plain_bypass_withSw":["SW","page","SW"]}
G4.workerRace: {"workerFetchCaughtWhenOnlyPAutoAttaches":true,"workerFetchCaughtWhenAlsoAAutoAttachesAndResumesAtOnce":true,"workerFetchArrivedAtServer":true,"aSawAttach":["service_worker","worker","worker","worker"]}
G4: A cannot switch off P: yes; A cannot override a P block (fail) with continue/fulfill in any tried order: NO; worker race hole: no
G5.closeReleases: {"heldWhileOpen":{"pausedAtP2":1,"serverReached":false},"heldRequestAfterClose":"OK:not found","laterRequestAfterClose":"OK:not found"}
G5.perTab: {"popupWithoutAutoAttach":{"firstRequestCaught":false,"arrived":true},"popupWithPageAutoAttach":{"firstRequestCaught":false,"attached":["service_worker/nowait","worker/nowait","worker/nowait","worker/nowait","worker/nowait","worker/nowait","worker/nowait"],"arrived":true}}
G5.popupBrowserAutoAttach: {"firstRequestCaught":true,"subresourceCaught":true,"arrived":true,"attached":["page/Agent/wait"]}
G5.newWindow: {"discoverThenAttach":{"firstRequestCaught":false,"arrived":true,"attachedAfterCreate":true},"browserAutoAttachWait":{"firstRequestCaught":false,"arrived":true,"attached":["other/Agent/wait","browser_ui/Agent/wait","browser_ui/Agent/wait","page/Agent/wait"]},"humanTabsSeenByBrowserAutoAttach":{"humanTabOpened":"opened","attached":["page/Human/wait"]}}
G5: closing the P connection RESUMED the held request; later requests pass unguarded; popup first request caught: plain=false, pageAutoAttach=false; new browser-made window first request caught: discoverThenAttach=false, browserAutoAttach=false
G6: mean per request (100 sequential fetches): baseline 0.38ms (runs 0.48/0.28); request-stage 0.54ms (+0.16); request+response stage 0.57ms (+0.19)
saved <repo>/spikes/out/20-request-guard.json
```

### 21-downloads.mjs (second run, full output)

```text
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
L1.perProfile: PASS: this launch's default context=Agent; D1 set Agent dir by browserContextId (ok), D2 set Human dir without a context id (skipped (default context is the Agent profile; a policy without a context id would land on the Agent profile); with the Human context id: Browser.setDownloadBehavior: Failed to find browser context for id 882EC724D28DDB1AA1AEE2241BA739C6; default context is Agent); agent download landed: {"agentDir":true,"humanDir":false,"extraDir":false,"userDownloadsByName":false}; human download landed: {"agentDir":false,"humanDir":false,"extraDir":false,"userDownloadsByName":true}; begin events seen by ["D1(agent policy)"] for the agent download and ["D1(agent policy)"] for the human one
L2.linkClick: linkClick: begin seen, frameId present, byTargetId={"type":"page","ctx":"Agent","alive":true,"opener":null,"openerCtx":null}, byFrameTree={"ownerTarget":"main-frame-of-that-tab","ctx":"Agent","isMainFrame":true}
L2.directNavigate: directNavigate: begin seen, frameId present, byTargetId={"type":"page","ctx":"Agent","alive":true,"opener":null,"openerCtx":null}, byFrameTree={"ownerTarget":"main-frame-of-that-tab","ctx":"Agent","isMainFrame":true}
L2.blobDownload: blobDownload: begin seen, frameId present, byTargetId={"type":"page","ctx":"Agent","alive":true,"opener":null,"openerCtx":null}, byFrameTree={"ownerTarget":"main-frame-of-that-tab","ctx":"Agent","isMainFrame":true}
L2.windowOpenTab: windowOpenTab: begin seen, frameId present, byTargetId={"type":"page","ctx":"Agent","alive":false,"opener":"known-target","openerCtx":"Agent"}, byFrameTree=none
L2.iframeSameSite: iframeSameSite: begin seen, frameId present, byTargetId=none, byFrameTree={"ownerTarget":"child-frame-of-a-tab","ctx":"Agent","isMainFrame":false}
L2.iframeCrossSite: iframeCrossSite: begin seen, frameId present, byTargetId={"type":"iframe","ctx":"Agent","alive":true,"opener":null,"openerCtx":null}, byFrameTree=none
L2.swNavigatesClient: swNavigatesClient: begin seen, frameId present, byTargetId={"type":"page","ctx":"Agent","alive":true,"opener":null,"openerCtx":null}, byFrameTree={"ownerTarget":"main-frame-of-that-tab","ctx":"Agent","isMainFrame":true}
L2.swServedAttachment: swServedAttachment: begin seen, frameId present, byTargetId={"type":"page","ctx":"Agent","alive":true,"opener":null,"openerCtx":null}, byFrameTree={"ownerTarget":"main-frame-of-that-tab","ctx":"Agent","isMainFrame":true}
L2.humanTabClick: humanTabClick: NO EVENT, frameId undefined, byTargetId=none, byFrameTree=none
L2.humanTabJsClick: humanTabJsClick: begin seen, frameId present, byTargetId={"type":"page","ctx":"Human","alive":true,"opener":null,"openerCtx":null}, byFrameTree={"ownerTarget":"main-frame-of-that-tab","ctx":"Human","isMainFrame":true}
L2.iframeTargets: [{"ctx":"Agent","alive":true},{"ctx":"Agent","alive":true}]
L2: link click resolves to an Agent tab: PASS; all agent scenarios resolved: linkClick=true directNavigate=true blobDownload=true windowOpenTab=true iframeSameSite=true iframeCrossSite=true swNavigatesClient=true swServedAttachment=true; human tab, synthetic mouse click (hidden window): NO downloadWillBegin; human tab, JS click: resolves to Human: true (a real person's click: not measured)
L3.withContentLength: cancel ok in 35ms, final state canceled, totalBytes first reported 30000000, received at first event >= limit 6291456, final received 0, progress events 4 (received series [0,0,6291456,0], ms between events [2,503,3]), server sent 6553600, server saw early close true, leftover files []
L3.withoutContentLength: cancel ok in 6ms, final state canceled, totalBytes first reported 0, received at first event >= limit 6291456, final received 0, progress events 4 (received series [0,0,6291456,0], ms between events [2,497,3]), server sent 6291456, server saw early close true, leftover files []
L4.plainAscii: suggestedFilename="pbspike-<run>-l4-ascii.txt"; file on disk is named by the download guid: true (guid shaped like a uuid: true)
L4.utf8Star: suggestedFilename="한글-pbspike-<run>-l4-utf8.txt"; file on disk is named by the download guid: true (guid shaped like a uuid: true)
L4.traversal: suggestedFilename="_.._pbspike-<run>-l4-trav.txt"; file on disk is named by the download guid: true (guid shaped like a uuid: true)
L4.noHeader: suggestedFilename="plain.bin"; file on disk is named by the download guid: true (guid shaped like a uuid: true)
L5: policy connection closed after {"state":"inProgress","received":851968}; the download FINISHED (server sent all); agent dir during=["2486eac0…:1114112"] after 500ms=["2486eac0…:1900544"] after 6s=["2486eac0…:4000000"]; the finished file landed: {"agentDir":true,"userDownloads":false} (files in ~/Downloads with this run id: 2); events after close seen on: []
L5.afterLoss: new agent download after the policy connection is gone landed: null; events seen on []
L1b: D4 holds Agent->agent dir (ok); D3 then sets Human (skipped (default context is Agent)) and Agent->extra dir (ok); agent download landed in: the NEWER connection's dir; D3's human policy still worked: {"landed":{"agentDir":false,"humanDir":false,"extraDir":false,"userDownloadsByName":true},"events":["D3(both)"]}; after the newer connection (D3) closed, an agent download landed in: neither (5 file(s) went to ~/Downloads)
Downloads: files with this run's id found in ~/Downloads and removed: 5 ["pbspike-<run>-l1-human","pbspike-<run>-l1b-d3closed","pbspike-<run>-l1b-human","pbspike-<run>-l2-human-js","pbspike-<run>-l5-after"]
saved <repo>/spikes/out/21-downloads.json
```

### 22-clipboard.mjs (second run, full output)

```text
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
ENV: {"defaultBrowserContext":"Agent","systemClipboardBytesAtStart":0,"clipboardTypesAtStart":"text-only"}
C1.states: {"read":"prompt","write":"granted"}
C1.noFocusEmulation: W1_writeText_evaluate_gesture=unchanged (page: ERR NotAllowedError: Failed to execute 'writeText' on 'Clipboard': Doc); W1b_writeText_evaluate_noGesture=unchanged (page: ERR NotAllowedError: Failed to execute 'writeText' on 'Clipboard': Doc); R1_readText_evaluate_gesture=saw "ERR NotAllowedError: Failed to execute '"; R1b_readText_evaluate_noGesture=saw "ERR NotAllowedError: Failed to execute '"
C1.noFocusEmulation_more: W2_writeText_realClickHandler=CHANGED to expected marker (page: ok); W3_execCommandCopy_evaluate_gesture=unchanged (page: false); W4_execCommandCopy_realClickHandler=CHANGED to expected marker (page: true); W5_clipboardWrite_realClickHandler=CHANGED to expected marker (page: ok); W6_key_Meta+C_with_commands_copy=CHANGED to expected marker; W8_key_Meta+X_with_commands_cut=CHANGED to expected marker; C5_copyHandler_viaExecCommand=unchanged (page: false); R2_readText_realClickHandler=nothing; R4_key_Meta+V_with_commands_paste=PAGE SAW THE SYSTEM CLIPBOARD
C1.withFocusEmulation: W1_writeText_evaluate_gesture=CHANGED to expected marker (page: ok); W1b_writeText_evaluate_noGesture=unchanged (page: PENDING: no answer in 5s); W2_writeText_realClickHandler=CHANGED to expected marker (page: ok); W3_execCommandCopy_evaluate_gesture=CHANGED to expected marker (page: true); W3b_execCommandCopy_evaluate_noGesture=unchanged (page: false); W4_execCommandCopy_realClickHandler=CHANGED to expected marker (page: true); W5_clipboardWrite_realClickHandler=CHANGED to expected marker (page: ok); W6_key_Meta+C_with_commands_copy=CHANGED to expected marker; W7_key_Meta+C_no_commands=unchanged; W8_key_Meta+X_with_commands_cut=CHANGED to expected marker; W9_key_Meta+X_no_commands=unchanged; C5_copyHandler_viaExecCommand=CHANGED to expected marker (page: true); C5b_copyHandler_viaKeyCommands=CHANGED to expected marker; R1_readText_evaluate_gesture=saw "PENDING: no answer in 5s"; R1b_readText_evaluate_noGesture=saw "PENDING: no answer in 5s"; R2_readText_realClickHandler=saw "ERR NotAllowedError: Failed to execute '"; R3_execCommandPaste_evaluate_gesture=saw ""; R4_key_Meta+V_with_commands_paste=PAGE SAW THE SYSTEM CLIPBOARD; R5_key_Meta+V_no_commands=saw " | paste event: null"
C1.humanTabControl: W1_writeText_evaluate_gesture=CHANGED to expected marker (page: ok); W3_execCommandCopy_evaluate_gesture=CHANGED to expected marker (page: true); R1_readText_evaluate_gesture=saw "PENDING: no answer in 5s"
C2.setPermission: {"clipboard-read":"ok","clipboard-write":"ok","clipboard-sanitized-write":"Browser.setPermission: Invalid PermissionDescriptor name: clipboard-sanitized-write"} | with the Human context id: Browser.setPermission: Failed to find browser context for id ACE950FE6BA0ED5F9141A608090DE237 | grantPermissions: ok (grant only; there is no deny here) | resetPermissions: ok
C2.statesAfterDeny: {"read":"denied","write":"denied"}
C2.deniedSuite: W1_writeText_evaluate_gesture=unchanged (page: ERR NotAllowedError: Failed to execute 'writeText' on 'Clipboard': Wri); W1b_writeText_evaluate_noGesture=unchanged (page: ERR NotAllowedError: Failed to execute 'writeText' on 'Clipboard': Wri); W2_writeText_realClickHandler=unchanged (page: ERR NotAllowedError: Failed to execute 'writeText' on 'Clipboard': Wri); W3_execCommandCopy_evaluate_gesture=CHANGED to expected marker (page: true); W3b_execCommandCopy_evaluate_noGesture=unchanged (page: false); W4_execCommandCopy_realClickHandler=CHANGED to expected marker (page: true); W5_clipboardWrite_realClickHandler=unchanged (page: ERR NotAllowedError: Failed to execute 'write' on 'Clipboard': Write p); W6_key_Meta+C_with_commands_copy=CHANGED to expected marker; W7_key_Meta+C_no_commands=unchanged; W8_key_Meta+X_with_commands_cut=CHANGED to expected marker; W9_key_Meta+X_no_commands=unchanged; C5_copyHandler_viaExecCommand=CHANGED to expected marker (page: true); C5b_copyHandler_viaKeyCommands=CHANGED to expected marker; R1_readText_evaluate_gesture=saw "ERR NotAllowedError: Failed to execute '"; R1b_readText_evaluate_noGesture=saw "ERR NotAllowedError: Failed to execute '"; R2_readText_realClickHandler=saw "ERR NotAllowedError: Failed to execute '"; R3_execCommandPaste_evaluate_gesture=saw ""; R4_key_Meta+V_with_commands_paste=PAGE SAW THE SYSTEM CLIPBOARD; R5_key_Meta+V_no_commands=saw " | paste event: null"
C2.scope: {"sameOriginNewTabSameProfile":{"states":{"read":"denied","write":"denied"},"writeText":"ERR NotAllowedError: on 'Clipboard': Write permission denied."},"otherOriginSameProfile":{"states":{"read":"prompt","write":"granted"},"writeText":"ok"},"humanTabSameOrigin":{"states":{"read":"prompt","write":"granted"},"writeText":"ok"},"agentTabBeforeSetterCloses":{"states":{"read":"denied","write":"denied"},"writeText":"ERR NotAllowedError: on 'Clipboard': Write permission denied."},"agentTabAfterSetterConnectionClosed":{"states":{"read":"prompt","write":"granted"},"writeText":"ok"},"noOriginGiven_otherOrigin":{"states":{"read":"denied","write":"denied"},"writeText":"ERR NotAllowedError: on 'Clipboard': Write permission denied."},"afterResetByS2_otherOrigin":{"states":{"read":"prompt","write":"granted"},"writeText":"ok"},"afterResetByS2_originSetByClosedS_O3":{"states":{"read":"prompt","write":"granted"},"writeText":"ok"}} | state after resetPermissions: {"read":"prompt","write":"granted"}
C3: execCommand('copy') default [W3_execCommandCopy_evaluate_gesture=CHANGED to expected marker, W3b_execCommandCopy_evaluate_noGesture=unchanged, W4_execCommandCopy_realClickHandler=CHANGED to expected marker] and with all clipboard permissions denied [W3_execCommandCopy_evaluate_gesture=CHANGED to expected marker, W3b_execCommandCopy_evaluate_noGesture=unchanged, W4_execCommandCopy_realClickHandler=CHANGED to expected marker]; execCommand('paste') default [R3_execCommandPaste_evaluate_gesture=saw ""] denied [R3_execCommandPaste_evaluate_gesture=saw ""]
C4: default [W6_key_Meta+C_with_commands_copy=CHANGED to expected marker, W7_key_Meta+C_no_commands=unchanged, W8_key_Meta+X_with_commands_cut=CHANGED to expected marker, W9_key_Meta+X_no_commands=unchanged, R4_key_Meta+V_with_commands_paste=PAGE SAW THE SYSTEM CLIPBOARD, R5_key_Meta+V_no_commands=saw " | paste event: null"]; denied [W6_key_Meta+C_with_commands_copy=CHANGED to expected marker, W7_key_Meta+C_no_commands=unchanged, W8_key_Meta+X_with_commands_cut=CHANGED to expected marker, W9_key_Meta+X_no_commands=unchanged, R4_key_Meta+V_with_commands_paste=PAGE SAW THE SYSTEM CLIPBOARD, R5_key_Meta+V_no_commands=saw " | paste event: null"]
C5: default [C5_copyHandler_viaExecCommand=CHANGED to expected marker, C5b_copyHandler_viaKeyCommands=CHANGED to expected marker]; denied [C5_copyHandler_viaExecCommand=CHANGED to expected marker, C5b_copyHandler_viaKeyCommands=CHANGED to expected marker]
C3.override: {"overrideActiveInPage":true,"realClickWithOverride":{"pageSaw":"false","system":"unchanged"},"keyCommandsWithOverride":"CHANGED to expected marker","iframeInheritsOverride":"false","srcdocIframeInheritsOverride":"false","overrideActiveAfterInjectingConnectionClosed":false,"afterInjectingConnectionClosed_realClick":{"pageSaw":"true","system":"CHANGED to expected marker"}}
Clipboard: restored (0 bytes of text); current system clipboard equals the saved one: true; contains a pbclip-run marker: false
saved <repo>/spikes/out/22-clipboard.json
```

### 23-clipboard-defences.mjs (second run, full output)

```text
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
RUN: {"id":"run-ecef6d58","version":"Chrome/157.0.8103.78"}
A1.none: a_click_execCopy=CHANGED(expected text)(true); b_click_execCut=CHANGED(expected text)(true); c_evalGesture_execCopy=CHANGED(expected text)(true); d_evalGesture_execCut=CHANGED(expected text)(true); e_key_Meta+C_commands=CHANGED(expected text); f_key_Meta+X_commands=CHANGED(expected text); g_key_paste_commands=unchanged(textarea="SYS-ecef6d58" pasteEvent="SYS-ecef6d58"); h_pageCopyHandler_click=CHANGED(expected text); i_pageCopyHandler_keyCommands=CHANGED(expected text); j_click_writeText_API=CHANGED(expected text)(ok) | paste: PAGE READ SYSTEM CLIPBOARD
A1.L_naive: a_click_execCopy=unchanged(true); b_click_execCut=unchanged(true); c_evalGesture_execCopy=unchanged(true); d_evalGesture_execCut=unchanged(true); e_key_Meta+C_commands=unchanged; f_key_Meta+X_commands=unchanged; g_key_paste_commands=unchanged(textarea="" pasteEvent="null"); h_pageCopyHandler_click=unchanged; i_pageCopyHandler_keyCommands=unchanged; j_click_writeText_API=CHANGED(expected text)(ok) | paste: page did not read it
A1.L_hard: a_click_execCopy=unchanged(true); b_click_execCut=unchanged(true); c_evalGesture_execCopy=unchanged(true); d_evalGesture_execCut=unchanged(true); e_key_Meta+C_commands=unchanged; f_key_Meta+X_commands=unchanged; g_key_paste_commands=unchanged(textarea="" pasteEvent="null"); h_pageCopyHandler_click=unchanged; i_pageCopyHandler_keyCommands=unchanged; j_click_writeText_API=CHANGED(expected text)(ok) | paste: page did not read it
A1.O_hard: a_click_execCopy=unchanged(false); b_click_execCut=unchanged(false); c_evalGesture_execCopy=unchanged(false); d_evalGesture_execCut=unchanged(false); e_key_Meta+C_commands=CHANGED(expected text); f_key_Meta+X_commands=CHANGED(expected text); g_key_paste_commands=unchanged(textarea="SYS-ecef6d58" pasteEvent="SYS-ecef6d58"); h_pageCopyHandler_click=unchanged; i_pageCopyHandler_keyCommands=CHANGED(expected text); j_click_writeText_API=CHANGED(expected text)(ok) | paste: PAGE READ SYSTEM CLIPBOARD
A1.L_hard_O_hard: a_click_execCopy=unchanged(false); b_click_execCut=unchanged(false); c_evalGesture_execCopy=unchanged(false); d_evalGesture_execCut=unchanged(false); e_key_Meta+C_commands=unchanged; f_key_Meta+X_commands=unchanged; g_key_paste_commands=unchanged(textarea="" pasteEvent="null"); h_pageCopyHandler_click=unchanged; i_pageCopyHandler_keyCommands=unchanged; j_click_writeText_API=CHANGED(expected text)(ok) | paste: page did not read it
A2.O_hard:b1_delete_or_redefine_execCommand: unchanged | {"deleted":false,"assigned":"assigned","copy":"false"}
A2.L_hard_O_hard:b2_iframe_about: unchanged | {"execIsOverride":true,"defL":true,"copy":"false"}
A2.L_hard_O_hard:b2_iframe_same: unchanged | {"execIsOverride":true,"defL":true,"copy":"false"}
A2.L_hard_O_hard:b2_iframe_srcdoc: unchanged | {"execIsOverride":true,"defL":true,"copy":"false"}
A2.L_hard_O_hard:b2_iframe_sandbox: unchanged | {"execIsOverride":true,"defL":true,"copy":"false"}
A2.O_hard:b2o_iframe_about: unchanged | {"execIsOverride":true,"defL":false,"copy":"false"}
A2.O_hard:b2o_iframe_same: unchanged | {"execIsOverride":true,"defL":false,"copy":"false"}
A2.L_hard:b2l_iframe_about: unchanged | {"execIsOverride":false,"defL":true,"copy":"false"}
A2.L_hard:b2l_iframe_same: unchanged | {"execIsOverride":false,"defL":true,"copy":"false"}
A2.L_hard_O_hard:b2_iframe_sandbox_opaque_click: CHANGED(expected text) | [{"first":{"defL":false,"defO":false,"t":1794571780110}},{"sbx":"true","defL":false}]
A2.L_hard_O_hard:b6_window_open_then_copy_from_opener: unchanged | {"execIsOverride":false,"copy":"false"}
A2.L_hard:b7_shadowroot_textarea: unchanged | true
A2.O_hard:b8_isolatedWorld_execCopy: CHANGED(expected text) | isolated world execCommand -> true
A2.L_hard:b8l_isolatedWorld_execCopy: unchanged | isolated world execCommand -> true
A2.L_hard_O_hard:b8lo_isolatedWorld_execCopy: unchanged | isolated world execCommand -> true
A2.b9_agent_script_registered_AFTER_pegboard (O_naive): unchanged | {"pristineIsOverride":true,"currentIsOverride":true,"viaPristine":"false"}
A2.b9_agent_script_registered_BEFORE_pegboard (O_naive): unchanged | {"pristineIsOverride":true,"currentIsOverride":true,"viaPristine":"false"}
A2.b9d_agent_session_older_registered_first_pegboard_via_younger_session (O_naive): CHANGED(expected text) | {"pristineIsOverride":false,"viaPristine":"true"}
A2.b9c_runImmediately_after_override: unchanged | ok; sees override: true
A2.L_naive:b10_redefine_Event.preventDefault_then_click_copy: CHANGED(expected text) | true
A2.L_hard:b10h_same_attack_on_hardened_listeners: unchanged | true
A2.L_hard:b11_later_listeners_run_after (observed order): unchanged | listeners run: []
A3.popup_window.open (page first script saw injection): unchanged | first-script state {"defL":true,"defO":true,"t":1794571787548}; click execCommand result false
A3.new_tab_later (window.open from agent tab, then evaluate execCommand w/ gesture): unchanged | execCommand false
A3.cross_site_iframe_click_execCopy: unchanged | iframe messages: [{"first":{"defL":true,"defO":true,"t":1794571790622}},{"ce":"false"}] (before click [{"first":{"defL":true,"defO":true,"t":1794571790622}}]); top frame injected: true
A3.cross_site_iframe_key_Meta+C_commands: unchanged | typed into iframe textarea at (400,20), selection by key: none -> only text selected by the click
A3.attached: ["browser_ui(running)","page(running)","browser_ui(running)","browser_ui(running)","page(running)","browser_ui(running)","page(running)","browser_ui(running)","browser_ui(running)","iframe(running)","iframe(running)","iframe(paused)","iframe(paused)","page(paused)","iframe(paused)","page(paused)","iframe(paused)","iframe(paused)","iframe(paused)"]
A4.Meta+C no commands: {"sys":"unchanged","textareaAfter":"SELECTME-ecef6d5"}
A4.Meta+C commands:[copy]: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-ecef6d5"}
A4.Meta+X no commands: {"sys":"unchanged","textareaAfter":"SELECTME-ecef6d5"}
A4.Meta+X commands:[cut]: {"sys":"CHANGED(expected text)","textareaAfter":""}
A4.Meta+V no commands: {"sys":"no read"}
A4.Meta+V commands:[paste]: {"sys":"PAGE READ SYSTEM CLIPBOARD"}
A4.Control+C no commands: {"sys":"unchanged","textareaAfter":"SELECTME-ecef6d5"}
A4.Control+C commands:[copy]: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-ecef6d5"}
A4.Control+X no commands: {"sys":"unchanged","textareaAfter":"SELECTME-ecef6d5"}
A4.Control+X commands:[cut]: {"sys":"CHANGED(expected text)","textareaAfter":""}
A4.Control+V no commands: {"sys":"no read"}
A4.Control+V commands:[paste]: {"sys":"PAGE READ SYSTEM CLIPBOARD"}
A4.Control+Insert no commands: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-ecef6d5"}
A4.Control+Insert commands:[copy]: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-ecef6d5"}
A4.Shift+Insert no commands: {"sys":"PAGE READ SYSTEM CLIPBOARD"}
A4.Shift+Insert commands:[paste]: {"sys":"PAGE READ SYSTEM CLIPBOARD"}
A4.Shift+Delete no commands: {"sys":"CHANGED(expected text)","textareaAfter":""}
A4.Shift+Delete commands:[cut]: {"sys":"CHANGED(expected text)","textareaAfter":""}
A4.Meta+C commands, type=keyDown: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-ecef6d5"}
A4.Meta+C commands, no vk, key only: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-ecef6d5"}
A4.Meta+C commands, nativeVirtualKeyCode only: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-ecef6d5"}
A4.Meta+C commands, key "C" modifiers 0: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-ecef6d5"}
A4.commands:[copy] on a plain "a" key, no modifiers: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-ecef6d5"}
A4.commands:[selectAll,copy] no modifiers: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-ecef6d5"}
A4.Input.insertText: {"sys":"unchanged","textareaAfter":"INSERTED"}
A4.right-click contextmenu: {"sys":"unchanged","contextmenuEvents":1,"note":"a native context menu is not observable over CDP; no menu item can be sent through Input.* (not tested beyond the event)"}
A5.permissionsPolicy_writeText: {"sys":"unchanged","policy":"{\"write\":false,\"read\":false}","page":"ERR NotAllowedError"}
A5.permissionsPolicy_execCommandCopy_click: {"sys":"CHANGED(expected text)","page":"true"}
A5.permissionsPolicy_key_commands_copy: {"sys":"CHANGED(expected text)"}
A5.CSP: not tested: CSP has no clipboard directive
A5.enterprisePolicy_DefaultClipboardSetting: not tested, needs system-level policy (macOS reads managed preferences from a system location; no flag points Chrome for Testing at a policy file inside the temporary dir)
A5.browserSetPermission_denied: {"shapes":{"clipboard-write+allowWithoutSanitization=false":"ok","clipboard-read":"ok","clipboard-sanitized-write":"Browser.setPermission: Invalid PermissionDescriptor name: clipboard-sanitized-write"},"writeTextAPI":{"writeText":"ERR NotAllowedError","sys":"unchanged"},"execCommandCopyClick":{"sys":"CHANGED(expected text)","page":"true"}}
A5.emulationDomain: Emulation.setFocusEmulationEnabled: not a block (round 1: a real click focuses the document); no other Emulation/Browser method mentioning clipboard exists in the protocol beyond Browser.setPermission/grantPermissions (clipboardReadWrite, clipboardSanitizedWrite)
A6.execCommand click: {"counterMoved":true,"increments":1,"firstSeenMsAfterActionStart":18,"actionCallMs":8}
A6.key Meta+C commands: {"counterMoved":true,"increments":1,"firstSeenMsAfterActionStart":20,"actionCallMs":6}
A6.evaluate execCommand(gesture): {"counterMoved":true,"increments":1,"firstSeenMsAfterActionStart":11,"actionCallMs":1}
A6.writeText click: {"counterMoved":true,"increments":1,"firstSeenMsAfterActionStart":22,"actionCallMs":6}
A6.paste (read only): {"counterMoved":false,"increments":0,"firstSeenMsAfterActionStart":null,"actionCallMs":9}
A6.no clipboard action (control): {"counterMoved":false,"increments":0,"firstSeenMsAfterActionStart":null,"actionCallMs":1}
A6.oneShotOsascriptMs: 79
A6.oneShotOutput: "prints count"
A6.watcherPollIntervalMs: 10
Clipboard: restored (0 bytes text); equals the saved one: true; contains this run's marker: false
saved <repo>/spikes/out/23-clipboard-defences.json
```

### 24-websocket.mjs (second run, full output)

```text
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
RUN: {"id":"run-a2f0efc3","version":"Chrome/157.0.8103.78","privateAddressAvailable":true}
B0.baseline: {"loopback":"page:open, server saw upgrade:true","privateName":"page:open, server saw upgrade:true"}
B1.urls=[ws://*,wss://*]: {"setBlockedURLs":"accepted","loopback":"page:open, server saw upgrade:true","privateName":"page:open, server saw upgrade:true"}
B1.urls=[*://127.0.0.1:*/*, *://lan.test:*/*]: {"setBlockedURLs":"accepted","loopback":"page:open, server saw upgrade:true","privateName":"page:open, server saw upgrade:true"}
B1.urls=[ws://lan.test:*] (host pattern): {"setBlockedURLs":"accepted","loopback":"page:open, server saw upgrade:true","privateName":"page:open, server saw upgrade:true"}
B1.urls=[*/ws] (path only): {"setBlockedURLs":"accepted","loopback":"page:open, server saw upgrade:true","privateName":"page:open, server saw upgrade:true"}
B1.urlPatterns=[{ws://*,block}]: {"setBlockedURLs":"accepted","loopback":"page:open, server saw upgrade:true","privateName":"page:open, server saw upgrade:true"}
B1.urlPatterns=[{*,block} for host lan.test]: {"setBlockedURLs":"Network.setBlockedURLs: Pattern \"*lan.test*\" failed to parse as a URLPattern.","loopback":"page:open, server saw upgrade:true","privateName":"page:open, server saw upgrade:true"}
B1.control_fetch_blocked_by_same_mechanism: {"fetch":"blocked:Failed to fetch"}
B2[continue].ws_to_listed_and_unlisted_hosts: {"loopback":"page:open, server saw upgrade:true","privateName":"page:open, server saw upgrade:true"}
B2[continue].other_channels_to_unlisted_host: {"page":"{\"fetch_privateName\":\"reached\",\"xhr_privateName\":\"reached\",\"eventsource_privateName\":\"blocked:error\",\"beacon_privateName\":\"true\"}","serverSawBeacon":true}
B2[continue].local_scheme_iframes (private name): {"aboutBlankIframe_viaParentRealm":"open","srcdocIframe":"open","dataIframe":"open","blobIframe":"open","privateNameServerSawUpgradeTotal":true}
B2[continue].top_level_unfetched_documents: {"dataUrlTopLevel":"error","blobUrlTopLevel":"error","note":"a Fetch response-stage hook was active for Document responses; data: and blob: top-level navigations produced no Fetch event"}
B2[continue].workers (private name): {"dedicatedWorker_scriptResponseWithoutCSP":"open","blobWorker_inheritsCreatorCSP":"open","dedicatedWorker_CSPAddedToWorkerScriptResponse":"open"}
B2[continue].window_open_popup_with_page_session_hook_only: {"csp_applied_to_popup_document":"no (open)"}
B2[fulfill].ws_to_listed_and_unlisted_hosts: {"loopback":"page:timeout, server saw upgrade:false","privateName":"page:error, server saw upgrade:false"}
B2[fulfill].other_channels_to_unlisted_host: {"page":"{\"fetch_privateName\":\"blocked:Failed to fetch\",\"xhr_privateName\":\"blocked:error\",\"eventsource_privateName\":\"blocked:error\",\"beacon_privateName\":\"true\"}","serverSawBeacon":true}
B2[fulfill].local_scheme_iframes (private name): {"aboutBlankIframe_viaParentRealm":"error","srcdocIframe":"error","dataIframe":"error","blobIframe":"error","privateNameServerSawUpgradeTotal":false}
B2[fulfill].top_level_unfetched_documents: {"dataUrlTopLevel":"error","blobUrlTopLevel":"error","note":"a Fetch response-stage hook was active for Document responses; data: and blob: top-level navigations produced no Fetch event"}
B2[fulfill].workers (private name): {"dedicatedWorker_scriptResponseWithoutCSP":"open","blobWorker_inheritsCreatorCSP":"error","dedicatedWorker_CSPAddedToWorkerScriptResponse":"open"}
B2[fulfill].window_open_popup_with_page_session_hook_only: {"csp_applied_to_popup_document":"no (open)"}
B3: {"timing":{"webSocketCreated_to_serverSawUpgradeMs":0,"handshakeRequestEvent_to_serverMs":0,"note":"positive = event arrived at pegboardd before the server saw the handshake (millisecond clocks, same machine)"},"offlineEmulationInReactionToCreatedEvent":[{"page":"open","serverSawUpgrade":true}],"establishedSocketOfflineEmulation":{"before":"open","after":"open"},"newSocketWhileOffline":{"page":"error","serverSawUpgrade":false},"emulateNetworkConditionsByRule":"accepted: {\"ruleIds\":[\"880766EB8BD802D3B7FD1F7A77F01A27\"]}","byRuleOfflineForWs":{"page":"open","serverSawUpgrade":true},"navigateAway":"navigating the tab closes its sockets (blunt; not measured on the server side)"}
B4 (weak measure): {"replaced_main_world":{"loopback":"page:THROWS SecurityError, server saw upgrade:false","privateName":"page:THROWS SecurityError, server saw upgrade:false"},"bypass_dedicatedWorker":"page-script result inside worker:open","bypass_sandboxedIframeOpaqueOrigin":"open","bypass_aboutBlankIframe":"THROWS SecurityError","bypass_isolatedWorld":"open","serverSawUpgradeDuringBypasses":true}
B5: {"webTransport":"not measured: needs an HTTP/3 (QUIC) server, which this spike does not have","webTransportApiPresent":"function","webRTC_dataChannel_baseline_nothing_active":"timeout","webRTC_dataChannel_with_setBlockedURLs_active":"timeout","networkRequestEventsDuringRtc":0,"note":"one observation each; the data channel here is peer-to-peer inside one page over host candidates, so it shows only that neither Network.setBlockedURLs nor Fetch is involved, not that a remote peer is unreachable"}
saved <repo>/spikes/out/24-websocket.json
```

### 25-private-names.mjs (second run, full output)

```text
RUN: {"id":"run-244a6c75","privateAddressAvailable":true}
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
C1.latency_100_requests_each: {"noHook":{"n":100,"meanMs":0.25,"maxMs":2.4},"hookContinueOnly":{"n":100,"meanMs":0.6,"maxMs":2.2},"hookPlusLookupCachedName_page":{"n":100,"meanMs":0.71,"maxMs":3.2},"lookupCachedName_nodeMs":{"n":100,"meanMs":0.19,"maxMs":2.5},"lookupCachedName_results":["2 addr"],"hookPlusLookupUnique_page":{"n":100,"meanMs":0.85,"maxMs":1.9},"lookupUnique_nodeMs":{"n":100,"meanMs":0.37,"maxMs":1.4},"lookupUnique_results":["2 addr"]}
C1.mechanism_check_failRequest_on_loopback_name: {"note":"lookup of \"localhost\" returns loopback; request failed by hook when marked","pageSaw":"failed Failed to fetch","withoutLookup":"reached"}
C1.gap_system_resolver_vs_chrome: {"systemResolverFor_names_Chrome_maps_via_host_resolver_rules":{"loop.test":"ENOTFOUND","lan.test":"ENOTFOUND","pub.test":"ENOTFOUND"},"chromeFetchToLoop_test":"reached","note":"Chrome connected where the system resolver has no answer, so a daemon-side lookup would see NXDOMAIN for a name Chrome resolves to loopback here (flag-based mapping is test-only; DoH, a second lookup returning another address and rebinding were not measured)"}
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
C2.[default flags + address-space override only]: {"origin":"public (by switch)","fetch_loopback_simple":"no answer in 5s (a prompt may be pending)","fetch_loopback_customHeader_preflight":"no answer in 5s (a prompt may be pending)","fetch_loopback_noCors":"no answer in 5s (a prompt may be pending)","fetch_privateName_simple":"no answer in 5s (a prompt may be pending)","img_loopback":"timeout","ws_loopback":"timeout","serverSaw":[],"control_loopbackPage_fetch_loopback":"reached ok","setPermissionNames":{"local-network-access":"accepted","local-network":"accepted","loopback-network":"accepted","local-network-access-loopback":"Browser.setPermission: Invalid PermissionDescriptor name: local-network-access-l","private-network-access":"Browser.setPermission: Invalid PermissionDescriptor name: private-network-access"},"afterGrantAttempt_fetch_loopback":"reached ok"}
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
C2.[+ --enable-features=LocalNetworkAccessChecks]: {"origin":"public (by switch)","fetch_loopback_simple":"no answer in 5s (a prompt may be pending)","fetch_loopback_customHeader_preflight":"no answer in 5s (a prompt may be pending)","fetch_loopback_noCors":"no answer in 5s (a prompt may be pending)","fetch_privateName_simple":"no answer in 5s (a prompt may be pending)","img_loopback":"timeout","ws_loopback":"timeout","serverSaw":[],"control_loopbackPage_fetch_loopback":"reached ok"}
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
C2.[+ --enable-features=LocalNetworkAccessChecks,LocalNetworkAccessWebSocketsTargetAddressSpace]: {"origin":"public (by switch)","fetch_loopback_simple":"no answer in 5s (a prompt may be pending)","fetch_loopback_customHeader_preflight":"no answer in 5s (a prompt may be pending)","fetch_loopback_noCors":"no answer in 5s (a prompt may be pending)","fetch_privateName_simple":"no answer in 5s (a prompt may be pending)","img_loopback":"timeout","ws_loopback":"timeout","serverSaw":[],"control_loopbackPage_fetch_loopback":"reached ok"}
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
C2.[+ --disable-features=LocalNetworkAccessChecks]: {"origin":"public (by switch)","fetch_loopback_simple":"reached ok","fetch_loopback_customHeader_preflight":"reached ok","fetch_loopback_noCors":"reached ","fetch_privateName_simple":"reached ok","img_loopback":"error","ws_loopback":"open","serverSaw":["c2=1","c2=2","c2=2","c2=3","c2=4","c2=5","c2=6(ws)"],"control_loopbackPage_fetch_loopback":"reached ok"}
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
C3: {"ipv4_loopback":"reached","ipv6_loopback_literal":"reached","ipv4_mapped_ipv6_literal_dotted":"reached","ipv4_mapped_ipv6_literal_hex":"reached","decimal_ipv4_literal":"reached","octal_hex_ipv4_literal":"reached","short_ipv4_literal":"reached","zero_address":"reached","https_loopback_self_signed":"reached","trailing_dot_name":"failed Failed to fetch","pausedAtRequestStage":[{"tag":"?c3=a","scheme":"http:","hostname":"127.0.0.1","privateByRules":true},{"tag":"?c3=b","scheme":"http:","hostname":"[::1]","privateByRules":true},{"tag":"?c3=c","scheme":"http:","hostname":"[::ffff:7f00:1]","privateByRules":false},{"tag":"?c3=d","scheme":"http:","hostname":"[::ffff:7f00:1]","privateByRules":false},{"tag":"?c3=e","scheme":"http:","hostname":"127.0.0.1","privateByRules":true},{"tag":"?c3=f","scheme":"http:","hostname":"127.0.0.1","privateByRules":true},{"tag":"?c3=g","scheme":"http:","hostname":"127.0.0.1","privateByRules":true},{"tag":"?c3=h","scheme":"http:","hostname":"0.0.0.0","privateByRules":true},{"tag":"?c3=i","scheme":"https:","hostname":"127.0.0.1","privateByRules":true},{"tag":"?c3=j","scheme":"http:","hostname":"loop.test.","privateByRules":false}],"note":"privateByRules = a literal-address check applied to URL.hostname as the daemon would see it"}
saved <repo>/spikes/out/25-private-names.json
```

### 26-coverage.mjs (second run, full output)

```text
RUN: {"id":"run-f9bf4583"}
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
D1.a_page_session_Fetch_only: {"iframeTargetsListed":1,"innerImg":{"caught":[],"serverSaw":true},"innerPageFetch":{"caught":[],"serverSaw":true},"evalFetchInsideIframe":{"page":"reached","caught":[]},"iframeDocument":["page-session"]}
D1.b_page_session_Fetch_plus_page_session_autoAttach_recursive: {"iframeTargetsListed":1,"innerImg":{"caught":["other-session"],"serverSaw":true},"innerPageFetch":{"caught":["other-session"],"serverSaw":true},"evalFetchInsideIframe":{"page":"reached","caught":["other-session","other-session"]},"iframeDocument":["page-session"]}
D1.c_browser_level_autoAttach: {"iframeTargetsListed":1,"innerImg":{"caught":["other-session"],"serverSaw":true},"innerPageFetch":{"caught":["other-session"],"serverSaw":true},"evalFetchInsideIframe":{"page":"reached","caught":["other-session","other-session"]},"iframeDocument":["other-session"]}
D3: {"browserLevel_Fetch_enable_ack":"accepted","browserLevel_seen":{"agentTab":true,"agentTab2":true,"humanTab":true,"fetchInAgentTab":true},"browserLevel_extra":{"crossSiteIframeImg":true,"crossSiteIframeFetch":true,"dedicatedWorkerFetch":false,"popupFirstRequest":true},"attachToBrowserTarget_Fetch_enable_ack":"accepted","attachToBrowserTarget_seen":{"agentTab":true,"agentTab2":true,"humanTab":true,"fetchInAgentTab":true},"pageSessionOnly_seen":{"agentTab":true,"agentTab2":true,"humanTab":false,"fetchInAgentTab":true},"Network_setRequestInterception":"Network.setRequestInterception: 'Network.setRequestInterception' wasn't found"}
D5.sessions created P then A; Fetch.enable P then A: ["page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)"]
D5.sessions created P then A; Fetch.enable A then P: ["page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)"]
D5.sessions created A then P; Fetch.enable A then P: ["page got: A-FULFILLED (both saw it)","page got: A-FULFILLED (both saw it)","page got: A-FULFILLED (both saw it)","page got: A-FULFILLED (both saw it)","page got: A-FULFILLED (both saw it)"]
D5.sessions created A then P; Fetch.enable P then A: ["page got: A-FULFILLED (both saw it)","page got: A-FULFILLED (both saw it)","page got: A-FULFILLED (both saw it)","page got: A-FULFILLED (both saw it)","page got: A-FULFILLED (both saw it)"]
D5.PEGBOARD connection younger than agent connection; sessions A then P; Fetch.enable A then P: ["page: failed (only P saw it)","page: failed (only P saw it)","page: failed (only P saw it)","page: failed (only P saw it)","page: failed (only P saw it)"]
D5.PEGBOARD connection younger than agent connection; sessions A then P; Fetch.enable P then A: ["page: failed (only P saw it)","page: failed (only P saw it)","page: failed (only P saw it)","page: failed (only P saw it)","page: failed (only P saw it)"]
D5.PEGBOARD connection younger than agent connection; sessions P then A; Fetch.enable P then A: ["page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)"]
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
D2.before_resume: [{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}},{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}},{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}},{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}},{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}}]
D2.after_resume: [{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}},{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}},{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}},{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}},{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}}]
D4.chrome.tabs.create({url}): ["NOT caught","NOT caught","NOT caught"]
D4.chrome.tabs.create(about:blank) then Page.navigate by CDP: ["caught","caught","caught"]
D4.window.open: ["caught","caught","caught"]
D4.target=_blank link click: ["caught","caught","caught"]
D4.--new-window URL (browser binary): ["NOT caught","NOT caught","NOT caught"]
D4.chrome.tabs.create({url}), auto-attach WITHOUT waitForDebuggerOnStart: ["NOT caught","NOT caught","NOT caught"]
saved <repo>/spikes/out/26-coverage.json
```

### 27-contexts-downloads.mjs (second run, full output)

```text
RUN: {"id":"run-fc1fc143"}
BROWSER: Chrome/157.0.8103.78
E.a_human_only_human_window_is_last_active: {"getBrowserContexts":{"nonDefaultIds":[],"defaultId":"Human"},"pageTargetsByProfile":{"Human":1},"setDownloadBehavior":{"Human":"accepted","(no id given)":"accepted"},"setPermission":{"Human":"accepted","(no id given)":"accepted"}}
E.b_agent_window_opened_last: {"getBrowserContexts":{"nonDefaultIds":[],"defaultId":"Agent"},"pageTargetsByProfile":{"Agent":1,"Human":1},"setDownloadBehavior":{"Human":"Browser.setDownloadBehavior: Failed to find browser context for id <id>","Agent":"accepted","(no id given)":"accepted"},"setPermission":{"Human":"Browser.setPermission: Failed to find browser context for id <id>","Agent":"accepted","(no id given)":"accepted"}}
E.c_after_activating_the_human_window: {"getBrowserContexts":{"nonDefaultIds":[],"defaultId":"Human"},"pageTargetsByProfile":{"Agent":1,"Human":1},"setDownloadBehavior":{"Human":"accepted","Agent":"Browser.setDownloadBehavior: Failed to find browser context for id <id>","(no id given)":"accepted"},"setPermission":{"Human":"accepted","Agent":"Browser.setPermission: Failed to find browser context for id <id>","(no id given)":"accepted"}}
E.d_three_profiles: {"getBrowserContexts":{"nonDefaultIds":[],"defaultId":"Agent2"},"pageTargetsByProfile":{"Agent":1,"Agent2":1,"Human":1},"setDownloadBehavior":{"Human":"Browser.setDownloadBehavior: Failed to find browser context for id <id>","Agent":"Browser.setDownloadBehavior: Failed to find browser context for id <id>","Agent2":"accepted","(no id given)":"accepted"},"setPermission":{"Human":"Browser.setPermission: Failed to find browser context for id <id>","Agent":"Browser.setPermission: Failed to find browser context for id <id>","Agent2":"accepted","(no id given)":"accepted"}}
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
F1: {"downloadBegan":true,"files":1,"frameIdOfDownload":"present","targetsAfter":[{"type":"page","hasOpenerId":false,"hasParentFrameId":false,"parentFrameIdEqualsDownloadFrame":false,"targetIdEqualsFrameId":false,"browserContext":"Human"},{"type":"page","hasOpenerId":false,"hasParentFrameId":false,"parentFrameIdEqualsDownloadFrame":false,"targetIdEqualsFrameId":false,"browserContext":"Agent"}],"iframeTargetIsTheFrameOfTheDownload":false,"getFrameTreePerPage":{"HumanTab":{"containsDownloadFrameId":false,"frames":1},"AgentTab":{"containsDownloadFrameId":true,"frames":2}},"ownerTabFound":["AgentTab"],"resolvedMeansTabFound":"TAB found: AgentTab"}
F2: [{"bytes":1048576,"contentLength":true,"completedEventReceivedBytes":1048576,"totalBytesInEvent":1048576,"sizeOnDiskAtCompletedEvent":1048576,"equalsExpected":true,"renameOnCompleted":"ok","reappearedInPolicyDir":false,"finalSizeInMovedDir":1048576,"laterEventsForGuid":0,"otherFilesInPolicyDir":0},{"bytes":3000000,"contentLength":true,"completedEventReceivedBytes":3000000,"totalBytesInEvent":3000000,"sizeOnDiskAtCompletedEvent":3000000,"equalsExpected":true,"renameOnCompleted":"ok","reappearedInPolicyDir":false,"finalSizeInMovedDir":3000000,"laterEventsForGuid":0,"otherFilesInPolicyDir":0},{"bytes":1500000,"contentLength":false,"completedEventReceivedBytes":1500000,"totalBytesInEvent":1500000,"sizeOnDiskAtCompletedEvent":1500000,"equalsExpected":true,"renameOnCompleted":"ok","reappearedInPolicyDir":false,"finalSizeInMovedDir":1500000,"laterEventsForGuid":0,"otherFilesInPolicyDir":0}]
F3.events connection first, policy connection newer: {"beforeClose":{"landedIn":"policy-connection dir","eventsOnE":{"begin":1,"completed":1}},"afterPolicyConnectionClosed":{"landedIn":"~/Downloads (removed at the end)","eventsOnE":{"begin":1,"completed":1}},"afterBothClosed":{"landedIn":"~/Downloads (removed at the end)","eventsOnE":{"begin":0,"completed":0}}}
F3.policy connection first, events connection newer: {"beforeClose":{"landedIn":"events-connection dir","eventsOnE":{"begin":1,"completed":1}},"afterPolicyConnectionClosed":{"landedIn":"events-connection dir","eventsOnE":{"begin":1,"completed":1}},"afterBothClosed":{"landedIn":"~/Downloads (removed at the end)","eventsOnE":{"begin":0,"completed":0}}}
F4.deny | chrome.tabs.create({url: <download url>}): {"policy":"deny","pagesBefore":1,"pagesAfter":1,"downloadEvents":{"begin":1,"completed":0,"canceled":1},"filesInPolicyDir":0,"filesInUserDownloads":0,"targetDestroyedEvents":1,"detachedEvents":1,"autoAttachedPagePausedBeforeNavigation":[true],"sessionAfter":"n/a (no session held by this script before the download)"}
F4.deny | chrome.tabs.create(about:blank) then Page.navigate(<download url>): {"policy":"deny","pagesBefore":1,"pagesAfter":2,"downloadEvents":{"begin":1,"completed":0,"canceled":1},"filesInPolicyDir":0,"filesInUserDownloads":0,"targetDestroyedEvents":0,"detachedEvents":0,"autoAttachedPagePausedBeforeNavigation":[true,false],"sessionAfter":"session answers"}
F4.deny | Target.createTarget({url: <download url>}): {"policy":"deny","pagesBefore":1,"pagesAfter":1,"downloadEvents":{"begin":1,"completed":0,"canceled":1},"filesInPolicyDir":0,"filesInUserDownloads":0,"targetDestroyedEvents":1,"detachedEvents":1,"autoAttachedPagePausedBeforeNavigation":[true],"sessionAfter":"n/a (no session held by this script before the download)"}
F4.deny | Target.createTarget(about:blank) then Page.navigate(<download url>): {"policy":"deny","pagesBefore":1,"pagesAfter":2,"downloadEvents":{"begin":1,"completed":0,"canceled":1},"filesInPolicyDir":0,"filesInUserDownloads":0,"targetDestroyedEvents":0,"detachedEvents":0,"autoAttachedPagePausedBeforeNavigation":[true,false],"sessionAfter":"session answers"}
F4.allowAndName | chrome.tabs.create({url: <download url>}): {"policy":"allowAndName","pagesBefore":1,"pagesAfter":1,"downloadEvents":{"begin":1,"completed":1,"canceled":0},"filesInPolicyDir":1,"filesInUserDownloads":0,"targetDestroyedEvents":1,"detachedEvents":1,"autoAttachedPagePausedBeforeNavigation":[true],"sessionAfter":"n/a (no session held by this script before the download)"}
F4.allowAndName | chrome.tabs.create(about:blank) then Page.navigate(<download url>): {"policy":"allowAndName","pagesBefore":1,"pagesAfter":2,"downloadEvents":{"begin":1,"completed":1,"canceled":0},"filesInPolicyDir":1,"filesInUserDownloads":0,"targetDestroyedEvents":0,"detachedEvents":0,"autoAttachedPagePausedBeforeNavigation":[true,false],"sessionAfter":"session answers"}
F4.allowAndName | Target.createTarget({url: <download url>}): {"policy":"allowAndName","pagesBefore":1,"pagesAfter":1,"downloadEvents":{"begin":1,"completed":1,"canceled":0},"filesInPolicyDir":1,"filesInUserDownloads":0,"targetDestroyedEvents":1,"detachedEvents":1,"autoAttachedPagePausedBeforeNavigation":[true],"sessionAfter":"n/a (no session held by this script before the download)"}
F4.allowAndName | Target.createTarget(about:blank) then Page.navigate(<download url>): {"policy":"allowAndName","pagesBefore":1,"pagesAfter":2,"downloadEvents":{"begin":1,"completed":1,"canceled":0},"filesInPolicyDir":1,"filesInUserDownloads":0,"targetDestroyedEvents":0,"detachedEvents":0,"autoAttachedPagePausedBeforeNavigation":[true,false],"sessionAfter":"session answers"}
Downloads: files with this run's id left in ~/Downloads after cleanup: 0
saved <repo>/spikes/out/27-contexts-downloads.json
```

## Round 3

Seven focused re-measurements (`task-2-round3.md`), on top of the round-2 commit cherry-picked into `phase2a-mcp-policy` (product files untouched). New scripts: `28-policy-persistence.mjs` (item 1), `29-tab-creation.mjs` (items 2, 3, blob worker of 7), `30-iframe-download.mjs` (item 4), `31-csp-reserve.mjs` (item 6); changed: `23-clipboard-defences.mjs` (A3 split into L_hard alone and L_hard+O_hard; A7 for item 5; `L_hard2`), `26-coverage.mjs` (D3 blob worker used a relative URL inside a `blob:` worker, so the request never left: round 2's "blob worker fetch not caught" was a test error).

Run status: the batch of two runs per script finished for 26, 29, 30, 31 and the first run of 23 (exit 0). The second run of 23 and both batch runs of 28 stopped at the start with exit 3: the user's clipboard held image data (copied during the batch), and the clipboard guard refuses to run rather than overwrite non-text data. Nothing was written to the clipboard in those runs. Script 28 had run twice successfully earlier in the session (the second run added a no-policy control); the saved result of that second run is below as JSON. Script 23's A7 had also been run on its own several times while it was being written (the final code ran once in the batch).

Cleanup after the batch: no Chrome for Testing process left, no file with a run id in `~/Downloads`, clipboard untouched by the failed runs.

### 23-clipboard-defences (batch run 1), full output

```text
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
RUN: {"id":"run-a830c763","version":"Chrome/157.0.8103.78"}
A1.none: a_click_execCopy=CHANGED(expected text)(true); b_click_execCut=CHANGED(expected text)(true); c_evalGesture_execCopy=CHANGED(expected text)(true); d_evalGesture_execCut=CHANGED(expected text)(true); e_key_Meta+C_commands=CHANGED(expected text); f_key_Meta+X_commands=CHANGED(expected text); g_key_paste_commands=unchanged(textarea="SYS-a830c763" pasteEvent="SYS-a830c763"); h_pageCopyHandler_click=CHANGED(expected text); i_pageCopyHandler_keyCommands=CHANGED(expected text); j_click_writeText_API=CHANGED(expected text)(ok) | paste: PAGE READ SYSTEM CLIPBOARD
A1.L_naive: a_click_execCopy=unchanged(true); b_click_execCut=unchanged(true); c_evalGesture_execCopy=unchanged(true); d_evalGesture_execCut=unchanged(true); e_key_Meta+C_commands=unchanged; f_key_Meta+X_commands=unchanged; g_key_paste_commands=unchanged(textarea="" pasteEvent="null"); h_pageCopyHandler_click=unchanged; i_pageCopyHandler_keyCommands=unchanged; j_click_writeText_API=CHANGED(expected text)(ok) | paste: page did not read it
A1.L_hard: a_click_execCopy=unchanged(true); b_click_execCut=unchanged(true); c_evalGesture_execCopy=unchanged(true); d_evalGesture_execCut=unchanged(true); e_key_Meta+C_commands=unchanged; f_key_Meta+X_commands=unchanged; g_key_paste_commands=unchanged(textarea="" pasteEvent="null"); h_pageCopyHandler_click=unchanged; i_pageCopyHandler_keyCommands=unchanged; j_click_writeText_API=CHANGED(expected text)(ok) | paste: page did not read it
A1.O_hard: a_click_execCopy=unchanged(false); b_click_execCut=unchanged(false); c_evalGesture_execCopy=unchanged(false); d_evalGesture_execCut=unchanged(false); e_key_Meta+C_commands=CHANGED(expected text); f_key_Meta+X_commands=CHANGED(expected text); g_key_paste_commands=unchanged(textarea="SYS-a830c763" pasteEvent="SYS-a830c763"); h_pageCopyHandler_click=unchanged; i_pageCopyHandler_keyCommands=CHANGED(expected text); j_click_writeText_API=CHANGED(expected text)(ok) | paste: PAGE READ SYSTEM CLIPBOARD
A1.L_hard_O_hard: a_click_execCopy=unchanged(false); b_click_execCut=unchanged(false); c_evalGesture_execCopy=unchanged(false); d_evalGesture_execCut=unchanged(false); e_key_Meta+C_commands=unchanged; f_key_Meta+X_commands=unchanged; g_key_paste_commands=unchanged(textarea="" pasteEvent="null"); h_pageCopyHandler_click=unchanged; i_pageCopyHandler_keyCommands=unchanged; j_click_writeText_API=CHANGED(expected text)(ok) | paste: page did not read it
A2.O_hard:b1_delete_or_redefine_execCommand: unchanged | {"deleted":false,"assigned":"assigned","copy":"false"}
A2.L_hard_O_hard:b2_iframe_about: unchanged | {"execIsOverride":true,"defL":true,"copy":"false"}
A2.L_hard_O_hard:b2_iframe_same: unchanged | {"execIsOverride":true,"defL":true,"copy":"false"}
A2.L_hard_O_hard:b2_iframe_srcdoc: unchanged | {"execIsOverride":true,"defL":true,"copy":"false"}
A2.L_hard_O_hard:b2_iframe_sandbox: unchanged | {"execIsOverride":true,"defL":true,"copy":"false"}
A2.O_hard:b2o_iframe_about: unchanged | {"execIsOverride":true,"defL":false,"copy":"false"}
A2.O_hard:b2o_iframe_same: unchanged | {"execIsOverride":true,"defL":false,"copy":"false"}
A2.L_hard:b2l_iframe_about: unchanged | {"execIsOverride":false,"defL":true,"copy":"false"}
A2.L_hard:b2l_iframe_same: unchanged | {"execIsOverride":false,"defL":true,"copy":"false"}
A2.L_hard_O_hard:b2_iframe_sandbox_opaque_click: CHANGED(expected text) | [{"first":{"defL":false,"defO":false,"t":1794573459109}},{"sbx":"true","defL":false}]
A2.L_hard_O_hard:b6_window_open_then_copy_from_opener: unchanged | {"execIsOverride":false,"copy":"false"}
A2.L_hard:b7_shadowroot_textarea: unchanged | true
A2.O_hard:b8_isolatedWorld_execCopy: CHANGED(expected text) | isolated world execCommand -> true
A2.L_hard:b8l_isolatedWorld_execCopy: unchanged | isolated world execCommand -> true
A2.L_hard_O_hard:b8lo_isolatedWorld_execCopy: unchanged | isolated world execCommand -> true
A2.b9_agent_script_registered_AFTER_pegboard (O_naive): unchanged | {"pristineIsOverride":true,"currentIsOverride":true,"viaPristine":"false"}
A2.b9_agent_script_registered_BEFORE_pegboard (O_naive): unchanged | {"pristineIsOverride":true,"currentIsOverride":true,"viaPristine":"false"}
A2.b9d_agent_session_older_registered_first_pegboard_via_younger_session (O_naive): CHANGED(expected text) | {"pristineIsOverride":false,"viaPristine":"true"}
A2.b9c_runImmediately_after_override: unchanged | ok; sees override: true
A2.L_naive:b10_redefine_Event.preventDefault_then_click_copy: CHANGED(expected text) | true
A2.L_hard:b10h_same_attack_on_hardened_listeners: unchanged | true
A2.L_hard:b11_later_listeners_run_after (observed order): unchanged | listeners run: []
A3[L_hard].popup_window.open (page first script saw injection): unchanged | first-script state {"defL":true,"defO":false,"t":1794573466756}; click execCommand result true
A3[L_hard].new_tab_later (window.open from agent tab, then evaluate execCommand w/ gesture): unchanged | execCommand true
A3[L_hard].cross_site_iframe_click_execCopy: unchanged | iframe messages: [{"first":{"defL":true,"defO":false,"t":1794573469833}},{"ce":"true"}] (before click [{"first":{"defL":true,"defO":false,"t":1794573469833}}]); top frame injected: true
A3[L_hard].cross_site_iframe_key_Meta+C_commands: unchanged | clicked the iframe textarea, commands selectAll then copy (key path inside the cross-site iframe)
A3[L_hard].attached: ["page(running)","page(running)","browser_ui(running)","browser_ui(running)","browser_ui(running)","browser_ui(running)","page(running)","browser_ui(running)","browser_ui(running)","iframe(running)","iframe(running)","iframe(paused)","iframe(paused)","page(paused)","iframe(paused)","page(paused)","iframe(paused)","iframe(paused)","iframe(paused)"]
A3[L_hard_O_hard].popup_window.open (page first script saw injection): unchanged | first-script state {"defL":true,"defO":true,"t":1794573472980}; click execCommand result false
A3[L_hard_O_hard].new_tab_later (window.open from agent tab, then evaluate execCommand w/ gesture): unchanged | execCommand false
A3[L_hard_O_hard].cross_site_iframe_click_execCopy: unchanged | iframe messages: [{"first":{"defL":true,"defO":true,"t":1794573476104}},{"ce":"false"}] (before click [{"first":{"defL":true,"defO":true,"t":1794573476104}}]); top frame injected: true
A3[L_hard_O_hard].cross_site_iframe_key_Meta+C_commands: unchanged | clicked the iframe textarea, commands selectAll then copy (key path inside the cross-site iframe)
A3[L_hard_O_hard].attached: ["page(running)","page(running)","browser_ui(running)","browser_ui(running)","browser_ui(running)","browser_ui(running)","page(running)","browser_ui(running)","browser_ui(running)","iframe(running)","iframe(running)","iframe(paused)","iframe(paused)","page(paused)","iframe(paused)","page(paused)","iframe(paused)","iframe(paused)","iframe(paused)"]
A7a.L_hard: agent session OLDER than pegboard injecting session | i1 neutralise Event.prototype.preventDefault + stopImmediatePropagation before L_hard captures them: {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"CHANGED(expected text)","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard: agent session OLDER than pegboard injecting session | i2 EventTarget.prototype.addEventListener replaced by a no-op before L_hard registers: {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"CHANGED(expected text)","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard: agent session OLDER than pegboard injecting session | i3 Function.prototype.call replaced (L_hard calls the saved originals through it): {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"CHANGED(expected text)","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard: agent session OLDER than pegboard injecting session | ii agent registers its own window capture copy/cut listener FIRST that only calls stopImmediatePropagation: {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"CHANGED(expected text)","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard: agent session OLDER than pegboard injecting session | i4 agent saves pristine references on window, no tampering (control: capture only): {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"unchanged","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard: pegboard injecting session OLDER than agent session (control) | i1 neutralise Event.prototype.preventDefault + stopImmediatePropagation before L_hard captures them: {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"unchanged","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard: pegboard injecting session OLDER than agent session (control) | i2 EventTarget.prototype.addEventListener replaced by a no-op before L_hard registers: {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"unchanged","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard: pegboard injecting session OLDER than agent session (control) | i3 Function.prototype.call replaced (L_hard calls the saved originals through it): {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"CHANGED(expected text)","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard: pegboard injecting session OLDER than agent session (control) | ii agent registers its own window capture copy/cut listener FIRST that only calls stopImmediatePropagation: {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"unchanged","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard: pegboard injecting session OLDER than agent session (control) | i4 agent saves pristine references on window, no tampering (control: capture only): {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"unchanged","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard2: agent session OLDER than pegboard injecting session | i1 neutralise Event.prototype.preventDefault + stopImmediatePropagation before L_hard captures them: {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"CHANGED(expected text)","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard2: agent session OLDER than pegboard injecting session | i2 EventTarget.prototype.addEventListener replaced by a no-op before L_hard registers: {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"CHANGED(expected text)","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard2: agent session OLDER than pegboard injecting session | i3 Function.prototype.call replaced (L_hard calls the saved originals through it): {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"unchanged","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard2: agent session OLDER than pegboard injecting session | ii agent registers its own window capture copy/cut listener FIRST that only calls stopImmediatePropagation: {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"CHANGED(expected text)","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard2: agent session OLDER than pegboard injecting session | i4 agent saves pristine references on window, no tampering (control: capture only): {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"unchanged","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard2: pegboard injecting session OLDER than agent session (control) | i1 neutralise Event.prototype.preventDefault + stopImmediatePropagation before L_hard captures them: {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"unchanged","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard2: pegboard injecting session OLDER than agent session (control) | i2 EventTarget.prototype.addEventListener replaced by a no-op before L_hard registers: {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"unchanged","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard2: pegboard injecting session OLDER than agent session (control) | i3 Function.prototype.call replaced (L_hard calls the saved originals through it): {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"unchanged","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard2: pegboard injecting session OLDER than agent session (control) | ii agent registers its own window capture copy/cut listener FIRST that only calls stopImmediatePropagation: {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"unchanged","evalGestureExecCommandCopy":"unchanged (page false)"}
A7a.L_hard2: pegboard injecting session OLDER than agent session (control) | i4 agent saves pristine references on window, no tampering (control: capture only): {"clickExecCommandCopy":"unchanged (page false)","keyCommandsCopy":"unchanged","evalGestureExecCommandCopy":"unchanged (page false)"}
A7b.a_click_execCopy: unchanged (false)
A7b.b_click_execCut: unchanged (false)
A7b.c_evalGesture_execCopy: unchanged (false)
A7b.d_evalGesture_execCut: unchanged (false)
A7b.e_key_Meta+C_commands: unchanged
A7b.f_key_Meta+X_commands: unchanged
A7b.g_key_paste_commands: unchanged (textarea="" pasteEvent="null")
A7b.h_pageCopyHandler_click: unchanged
A7b.i_pageCopyHandler_keyCommands: unchanged
A7b.j_click_writeText_API: unchanged (ERR NotAllowedError)
A7b.Control+Insert (no commands): unchanged
A7b.Shift+Delete (no commands): unchanged
A7b.Shift+Insert (no commands): no read
A7b.Control+C commands:[copy]: unchanged
A7b.Control+X commands:[cut]: unchanged
A7b.Control+V commands:[paste]: no read
A7c.targetsAttachedWhileCreatingSandboxedSrcdocIframe: ["iframe:about:srcdoc","iframe:about:srcdoc"]
A7c.control_targetsAttachedForPlainSrcdocIframe: []
A7c.control_targetsAttachedByLoadingThePageAlone: []
A7c.srcdocSandboxOpaque_gestureCopy: {"messages":"[{\"first\":{\"defL\":true,\"defO\":true,\"t\":1794573513760}},{\"sbx\":\"true\",\"defL\":true}]","system":"unchanged"}
A7c.targetsAttachedWhileCreatingSandboxedCrossSiteIframe: ["iframe:(paused)","iframe:(paused)"]
A7c.crossSiteSandboxed_gestureCopy: {"messages":"[{\"first\":{\"defL\":true,\"defO\":true,\"t\":1794573514864}},{\"first\":{\"defL\":true,\"defO\":true,\"t\":1794573514891}},{\"ce\":\"false\"}]","system":"unchanged"}
A4.Meta+C no commands: {"sys":"unchanged","textareaAfter":"SELECTME-a830c76"}
A4.Meta+C commands:[copy]: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-a830c76"}
A4.Meta+X no commands: {"sys":"unchanged","textareaAfter":"SELECTME-a830c76"}
A4.Meta+X commands:[cut]: {"sys":"CHANGED(expected text)","textareaAfter":""}
A4.Meta+V no commands: {"sys":"no read"}
A4.Meta+V commands:[paste]: {"sys":"PAGE READ SYSTEM CLIPBOARD"}
A4.Control+C no commands: {"sys":"unchanged","textareaAfter":"SELECTME-a830c76"}
A4.Control+C commands:[copy]: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-a830c76"}
A4.Control+X no commands: {"sys":"unchanged","textareaAfter":"SELECTME-a830c76"}
A4.Control+X commands:[cut]: {"sys":"CHANGED(expected text)","textareaAfter":""}
A4.Control+V no commands: {"sys":"no read"}
A4.Control+V commands:[paste]: {"sys":"PAGE READ SYSTEM CLIPBOARD"}
A4.Control+Insert no commands: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-a830c76"}
A4.Control+Insert commands:[copy]: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-a830c76"}
A4.Shift+Insert no commands: {"sys":"PAGE READ SYSTEM CLIPBOARD"}
A4.Shift+Insert commands:[paste]: {"sys":"PAGE READ SYSTEM CLIPBOARD"}
A4.Shift+Delete no commands: {"sys":"CHANGED(expected text)","textareaAfter":""}
A4.Shift+Delete commands:[cut]: {"sys":"CHANGED(expected text)","textareaAfter":""}
A4.Meta+C commands, type=keyDown: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-a830c76"}
A4.Meta+C commands, no vk, key only: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-a830c76"}
A4.Meta+C commands, nativeVirtualKeyCode only: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-a830c76"}
A4.Meta+C commands, key "C" modifiers 0: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-a830c76"}
A4.commands:[copy] on a plain "a" key, no modifiers: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-a830c76"}
A4.commands:[selectAll,copy] no modifiers: {"sys":"CHANGED(expected text)","textareaAfter":"SELECTME-a830c76"}
A4.Input.insertText: {"sys":"unchanged","textareaAfter":"INSERTED"}
A4.right-click contextmenu: {"sys":"unchanged","contextmenuEvents":1,"note":"a native context menu is not observable over CDP; no menu item can be sent through Input.* (not tested beyond the event)"}
A5.permissionsPolicy_writeText: {"sys":"unchanged","policy":"{\"write\":false,\"read\":false}","page":"ERR NotAllowedError"}
A5.permissionsPolicy_execCommandCopy_click: {"sys":"CHANGED(expected text)","page":"true"}
A5.permissionsPolicy_key_commands_copy: {"sys":"CHANGED(expected text)"}
A5.CSP: not tested: CSP has no clipboard directive
A5.enterprisePolicy_DefaultClipboardSetting: not tested, needs system-level policy (macOS reads managed preferences from a system location; no flag points Chrome for Testing at a policy file inside the temporary dir)
A5.browserSetPermission_denied: {"shapes":{"clipboard-write+allowWithoutSanitization=false":"ok","clipboard-read":"ok","clipboard-sanitized-write":"Browser.setPermission: Invalid PermissionDescriptor name: clipboard-sanitized-write"},"writeTextAPI":{"writeText":"ERR NotAllowedError","sys":"unchanged"},"execCommandCopyClick":{"sys":"CHANGED(expected text)","page":"true"}}
A5.emulationDomain: Emulation.setFocusEmulationEnabled: not a block (round 1: a real click focuses the document); no other Emulation/Browser method mentioning clipboard exists in the protocol beyond Browser.setPermission/grantPermissions (clipboardReadWrite, clipboardSanitizedWrite)
A6.execCommand click: {"counterMoved":true,"increments":1,"firstSeenMsAfterActionStart":12,"actionCallMs":7}
A6.key Meta+C commands: {"counterMoved":true,"increments":1,"firstSeenMsAfterActionStart":17,"actionCallMs":6}
A6.evaluate execCommand(gesture): {"counterMoved":true,"increments":1,"firstSeenMsAfterActionStart":12,"actionCallMs":4}
A6.writeText click: {"counterMoved":true,"increments":1,"firstSeenMsAfterActionStart":21,"actionCallMs":6}
A6.paste (read only): {"counterMoved":false,"increments":0,"firstSeenMsAfterActionStart":null,"actionCallMs":10}
A6.no clipboard action (control): {"counterMoved":false,"increments":0,"firstSeenMsAfterActionStart":null,"actionCallMs":2}
A6.oneShotOsascriptMs: 76
A6.oneShotOutput: "prints count"
A6.watcherPollIntervalMs: 10
Clipboard: restored (0 bytes text); equals the saved one: true; contains this run's marker: false
saved <repo>/spikes/out/23-clipboard-defences.json
```

### 26-coverage (batch run 2), full output

```text
RUN: {"id":"run-08561514"}
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
D1.a_page_session_Fetch_only: {"iframeTargetsListed":1,"innerImg":{"caught":[],"serverSaw":true},"innerPageFetch":{"caught":[],"serverSaw":true},"evalFetchInsideIframe":{"page":"reached","caught":[]},"iframeDocument":["page-session"]}
D1.b_page_session_Fetch_plus_page_session_autoAttach_recursive: {"iframeTargetsListed":1,"innerImg":{"caught":["other-session"],"serverSaw":true},"innerPageFetch":{"caught":["other-session"],"serverSaw":true},"evalFetchInsideIframe":{"page":"reached","caught":["other-session","other-session"]},"iframeDocument":["page-session"]}
D1.c_browser_level_autoAttach: {"iframeTargetsListed":1,"innerImg":{"caught":["other-session"],"serverSaw":true},"innerPageFetch":{"caught":["other-session"],"serverSaw":true},"evalFetchInsideIframe":{"page":"reached","caught":["other-session","other-session"]},"iframeDocument":["other-session"]}
D3: {"browserLevel_Fetch_enable_ack":"accepted","browserLevel_seen":{"agentTab":true,"agentTab2":true,"humanTab":true,"fetchInAgentTab":true},"browserLevel_extra":{"crossSiteIframeImg":true,"crossSiteIframeFetch":true,"dedicatedWorkerFetch":true,"popupFirstRequest":true},"attachToBrowserTarget_Fetch_enable_ack":"accepted","attachToBrowserTarget_seen":{"agentTab":true,"agentTab2":true,"humanTab":true,"fetchInAgentTab":true},"pageSessionOnly_seen":{"agentTab":true,"agentTab2":true,"humanTab":false,"fetchInAgentTab":true},"Network_setRequestInterception":"Network.setRequestInterception: 'Network.setRequestInterception' wasn't found"}
D5.sessions created P then A; Fetch.enable P then A: ["page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)"]
D5.sessions created P then A; Fetch.enable A then P: ["page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)"]
D5.sessions created A then P; Fetch.enable A then P: ["page got: A-FULFILLED (both saw it)","page got: A-FULFILLED (both saw it)","page got: A-FULFILLED (both saw it)","page got: A-FULFILLED (both saw it)","page got: A-FULFILLED (both saw it)"]
D5.sessions created A then P; Fetch.enable P then A: ["page got: A-FULFILLED (both saw it)","page got: A-FULFILLED (both saw it)","page got: A-FULFILLED (both saw it)","page got: A-FULFILLED (both saw it)","page got: A-FULFILLED (both saw it)"]
D5.PEGBOARD connection younger than agent connection; sessions A then P; Fetch.enable A then P: ["page: failed (only P saw it)","page: failed (only P saw it)","page: failed (only P saw it)","page: failed (only P saw it)","page: failed (only P saw it)"]
D5.PEGBOARD connection younger than agent connection; sessions A then P; Fetch.enable P then A: ["page: failed (only P saw it)","page: failed (only P saw it)","page: failed (only P saw it)","page: failed (only P saw it)","page: failed (only P saw it)"]
D5.PEGBOARD connection younger than agent connection; sessions P then A; Fetch.enable P then A: ["page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)","page got: A-FULFILLED (only A saw it)"]
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
D2.before_resume: [{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}},{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}},{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}},{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}},{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}}]
D2.after_resume: [{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}},{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}},{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}},{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}},{"scriptRequest":"NOT caught","importScripts":"sw-session","fetchInsideSW":"sw-session","serverSaw":{"script":2,"fetchInsideSW":1}}]
D4.chrome.tabs.create({url}): ["NOT caught","NOT caught","NOT caught"]
D4.chrome.tabs.create(about:blank) then Page.navigate by CDP: ["caught","caught","caught"]
D4.window.open: ["caught","caught","caught"]
D4.target=_blank link click: ["caught","caught","caught"]
D4.--new-window URL (browser binary): ["NOT caught","NOT caught","NOT caught"]
D4.chrome.tabs.create({url}), auto-attach WITHOUT waitForDebuggerOnStart: ["NOT caught","NOT caught","NOT caught"]
saved <repo>/spikes/out/26-coverage.json
```

### 29-tab-creation (batch run 2), full output

```text
RUN: {"id":"run-a57972a4"}
BROWSER: Chrome/157.0.8103.78
T2.X: browser-level auto-attach (waitForDebuggerOnStart) + Fetch.enable on each new page target before resume: {"firstRequestCaught":"10/10","serverSawAll":10,"resourceTypesSeen":["Document"],"errors":0}
T2.Y: browser-level Fetch.enable (no sessionId) + auto-attach that only resumes: {"firstRequestCaught":"10/10","serverSawAll":10,"resourceTypesSeen":["Document"],"errors":0}
T2.Z: browser-level Fetch.enable only (no auto-attach): {"firstRequestCaught":"10/10","serverSawAll":10,"resourceTypesSeen":["Document"],"errors":0}
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
T3.B: page-level auto-attach (Target.setAutoAttach on the page session) + page-session Fetch.enable: {"registered":10,"mainScriptCaught":"1/10","mainViaWhich":["sw-session","NOT caught"],"importScriptsCaught":"10/10","fetchInSWCaught":"10/10","serverReceivedMainScript":10}
T3.C: browser-level auto-attach + page-session Fetch.enable: {"registered":10,"mainScriptCaught":"0/10","mainViaWhich":["NOT caught"],"importScriptsCaught":"10/10","fetchInSWCaught":"10/10","serverReceivedMainScript":10}
T3.blobWorkerFetch.a: page-session Fetch only: [{"caught":"page-session","serverSaw":true},{"caught":"page-session","serverSaw":true},{"caught":"page-session","serverSaw":true}]
T3.blobWorkerFetch.b: page-session Fetch + page-level auto-attach: [{"caught":"page-session","serverSaw":true},{"caught":"page-session","serverSaw":true},{"caught":"page-session","serverSaw":true}]
T3.blobWorkerFetch.c: browser-level Fetch.enable (no sessionId): [{"caught":"browser-session","serverSaw":true},{"caught":"browser-session","serverSaw":true},{"caught":"browser-session","serverSaw":true}]
saved <repo>/spikes/out/29-tab-creation.json
```

### 30-iframe-download (batch run 2), full output

```text
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
F1.run1: {"run":1,"iframeTargetExists":true,"iframeTargetInfoKeys":["attached","browserContextId","canAccessOpener","parentFrameId","parentId","targetId","title","type","url"],"iframeTargetInfo":{"openerId":null,"parentFrameId":"C486C8DB610A6E54224D92B5D2405003","browserContext":"Agent","openerFrameId":null},"getTargetInfoOfIframe":{"keys":["attached","browserContextId","canAccessOpener","parentFrameId","parentId","targetId","title","type","url"],"parentFrameId":"C486C8DB610A6E54224D92B5D2405003","openerId":null},"downloadBegan":true,"frameIdIsATargetId":true,"frameIdIsTheIframeTarget":true,"getFrameTreePerPage":{"HumanTab":{"frames":1,"containsDownloadFrameId":false,"childFramesOfTop":0},"AgentTab":{"frames":1,"containsDownloadFrameId":false,"childFramesOfTop":0}},"getFrameTreeFromIframeSession":{"frameIdIsDownloadFrame":true,"hasParentId":true,"parentIdIsTopFrame":true,"childFrames":0},"topTabFoundBy":["frame.parentId from the iframe session equals the tab targetId","TargetInfo.parentFrameId"],"fileInPolicyDir":1}
F1.run2: {"run":2,"iframeTargetExists":true,"iframeTargetInfoKeys":["attached","browserContextId","canAccessOpener","parentFrameId","parentId","targetId","title","type","url"],"iframeTargetInfo":{"openerId":null,"parentFrameId":"C486C8DB610A6E54224D92B5D2405003","browserContext":"Agent","openerFrameId":null},"getTargetInfoOfIframe":{"keys":["attached","browserContextId","canAccessOpener","parentFrameId","parentId","targetId","title","type","url"],"parentFrameId":"C486C8DB610A6E54224D92B5D2405003","openerId":null},"downloadBegan":true,"frameIdIsATargetId":true,"frameIdIsTheIframeTarget":true,"getFrameTreePerPage":{"AgentTab":{"frames":1,"containsDownloadFrameId":false,"childFramesOfTop":0},"HumanTab":{"frames":1,"containsDownloadFrameId":false,"childFramesOfTop":0}},"getFrameTreeFromIframeSession":{"frameIdIsDownloadFrame":true,"hasParentId":true,"parentIdIsTopFrame":true,"childFrames":0},"topTabFoundBy":["frame.parentId from the iframe session equals the tab targetId","TargetInfo.parentFrameId"],"fileInPolicyDir":1}
F1.run3: {"run":3,"iframeTargetExists":true,"iframeTargetInfoKeys":["attached","browserContextId","canAccessOpener","parentFrameId","parentId","targetId","title","type","url"],"iframeTargetInfo":{"openerId":null,"parentFrameId":"C486C8DB610A6E54224D92B5D2405003","browserContext":"Agent","openerFrameId":null},"getTargetInfoOfIframe":{"keys":["attached","browserContextId","canAccessOpener","parentFrameId","parentId","targetId","title","type","url"],"parentFrameId":"C486C8DB610A6E54224D92B5D2405003","openerId":null},"downloadBegan":true,"frameIdIsATargetId":true,"frameIdIsTheIframeTarget":true,"getFrameTreePerPage":{"AgentTab":{"frames":1,"containsDownloadFrameId":false,"childFramesOfTop":0},"HumanTab":{"frames":1,"containsDownloadFrameId":false,"childFramesOfTop":0}},"getFrameTreeFromIframeSession":{"frameIdIsDownloadFrame":true,"hasParentId":true,"parentIdIsTopFrame":true,"childFrames":0},"topTabFoundBy":["frame.parentId from the iframe session equals the tab targetId","TargetInfo.parentFrameId"],"fileInPolicyDir":1}
Cleanup: files with this run's id left in ~/Downloads: 0
saved <repo>/spikes/out/30-iframe-download.json
```

### 31-csp-reserve (batch run 2), full output

```text
BROWSER: Chrome/157.0.8103.78
DEFAULT_CONTEXT: Agent; contexts listed by Target.getBrowserContexts: 0
V0 pass-through (continueResponse, nothing changed): [{"ws":"open 4ms","serverSawUpgrade":true,"fetch":"ok","fetchPrivateName":"ok","readyState":"complete","scriptRan":true,"bodyBytes":null},{"ws":"open 2ms","serverSawUpgrade":true,"fetch":"ok","fetchPrivateName":"ok","readyState":"complete","scriptRan":true,"bodyBytes":null},{"ws":"open 2ms","serverSawUpgrade":true,"fetch":"ok","fetchPrivateName":"ok","readyState":"complete","scriptRan":true,"bodyBytes":null}]
V1 fulfill, original headers + original body, NO CSP: [{"ws":"TIMEOUT 4s","serverSawUpgrade":false,"fetch":"ok","fetchPrivateName":"TIMEOUT 4s","readyState":"complete","scriptRan":true,"bodyBytes":143},{"ws":"TIMEOUT 4s","serverSawUpgrade":false,"fetch":"ok","fetchPrivateName":"TIMEOUT 4s","readyState":"complete","scriptRan":true,"bodyBytes":143},{"ws":"TIMEOUT 4s","serverSawUpgrade":false,"fetch":"ok","fetchPrivateName":"TIMEOUT 4s","readyState":"complete","scriptRan":true,"bodyBytes":143}]
V2 fulfill + original headers + CSP connect-src *: [{"ws":"TIMEOUT 4s","serverSawUpgrade":false,"fetch":"ok","fetchPrivateName":"TIMEOUT 4s","readyState":"complete","scriptRan":true,"bodyBytes":143},{"ws":"TIMEOUT 4s","serverSawUpgrade":false,"fetch":"ok","fetchPrivateName":"failed Failed to fetch","readyState":"complete","scriptRan":true,"bodyBytes":143},{"ws":"TIMEOUT 4s","serverSawUpgrade":false,"fetch":"ok","fetchPrivateName":"failed Failed to fetch","readyState":"complete","scriptRan":true,"bodyBytes":143}]
V3 continueResponse + CSP connect-src * (CSP not applied): [{"ws":"open 2ms","serverSawUpgrade":true,"fetch":"ok","fetchPrivateName":"ok","readyState":"complete","scriptRan":true,"bodyBytes":null},{"ws":"open 2ms","serverSawUpgrade":true,"fetch":"ok","fetchPrivateName":"ok","readyState":"complete","scriptRan":true,"bodyBytes":null},{"ws":"open 3ms","serverSawUpgrade":true,"fetch":"ok","fetchPrivateName":"ok","readyState":"complete","scriptRan":true,"bodyBytes":null}]
V4 fulfill + CSP connect-src * + response headers rebuilt with explicit content-length: [{"ws":"TIMEOUT 4s","serverSawUpgrade":false,"fetch":"ok","fetchPrivateName":"failed Failed to fetch","readyState":"complete","scriptRan":true,"bodyBytes":143},{"ws":"TIMEOUT 4s","serverSawUpgrade":false,"fetch":"ok","fetchPrivateName":"failed Failed to fetch","readyState":"complete","scriptRan":true,"bodyBytes":143},{"ws":"error","serverSawUpgrade":false,"fetch":"ok","fetchPrivateName":"failed Failed to fetch","readyState":"complete","scriptRan":true,"bodyBytes":143}]
V5 fulfill + CSP default-src * (instead of connect-src): [{"ws":"error","serverSawUpgrade":false,"fetch":"ok","fetchPrivateName":"failed Failed to fetch","readyState":"complete","scriptRan":true,"bodyBytes":143},{"ws":"error","serverSawUpgrade":false,"fetch":"ok","fetchPrivateName":"failed Failed to fetch","readyState":"complete","scriptRan":true,"bodyBytes":143},{"ws":"error","serverSawUpgrade":false,"fetch":"ok","fetchPrivateName":"failed Failed to fetch","readyState":"complete","scriptRan":true,"bodyBytes":143}]
V6 fulfill with responseCode 200 and header set from scratch (content-type + CSP only): [{"ws":"error","serverSawUpgrade":false,"fetch":"ok","fetchPrivateName":"failed Failed to fetch","readyState":"complete","scriptRan":true,"bodyBytes":143},{"ws":"error","serverSawUpgrade":false,"fetch":"ok","fetchPrivateName":"failed Failed to fetch","readyState":"complete","scriptRan":true,"bodyBytes":143},{"ws":"error","serverSawUpgrade":false,"fetch":"ok","fetchPrivateName":"failed Failed to fetch","readyState":"complete","scriptRan":true,"bodyBytes":143}]
FIX.setPermission: {"local-network-access":"accepted","local-network":"accepted","loopback-network":"accepted"}
FIX-V1 fulfill, NO CSP, permission granted: [{"ws_allowed_loopback":"open 3ms","serverSawUpgrade":true,"fetch_allowed_loopback":"ok","fetch_privateName":"ok"},{"ws_allowed_loopback":"open 3ms","serverSawUpgrade":true,"fetch_allowed_loopback":"ok","fetch_privateName":"ok"},{"ws_allowed_loopback":"open 3ms","serverSawUpgrade":true,"fetch_allowed_loopback":"ok","fetch_privateName":"ok"}]
FIX-V2 fulfill + connect-src * , permission granted: [{"ws_allowed_loopback":"open 2ms","serverSawUpgrade":true,"fetch_allowed_loopback":"ok","fetch_privateName":"ok"},{"ws_allowed_loopback":"open 2ms","serverSawUpgrade":true,"fetch_allowed_loopback":"ok","fetch_privateName":"ok"},{"ws_allowed_loopback":"open 2ms","serverSawUpgrade":true,"fetch_allowed_loopback":"ok","fetch_privateName":"ok"}]
FIX-V7 fulfill + CSP allow list (loopback host only, http+ws), permission granted: [{"ws_allowed_loopback":"open 3ms","serverSawUpgrade":true,"fetch_allowed_loopback":"ok","fetch_privateName":"failed Failed to fetch"},{"ws_allowed_loopback":"open 1ms","serverSawUpgrade":true,"fetch_allowed_loopback":"ok","fetch_privateName":"failed Failed to fetch"},{"ws_allowed_loopback":"open 2ms","serverSawUpgrade":true,"fetch_allowed_loopback":"ok","fetch_privateName":"failed Failed to fetch"}]
headers: content-type,x-orig,date,connection,keep-alive,transfer-encoding
saved <repo>/spikes/out/31-csp-reserve.json
```

### 28-policy-persistence (saved JSON of the last successful run: control + 2 variants x 3 cycles x 6 steps)

```json
{
 "RUN": {
  "id": "run-eeddf5e7",
  "version": "Chrome/157.0.8103.78"
 },
 "CONTROL.no_policy": {
  "default": "Agent",
  "download": "~/Downloads (removed at the end)",
  "downloadEvents": "none",
  "clipboardWriteText": "ok",
  "systemClipboard": "CHANGED (page wrote it)"
 },
 "POLICY.deny.cycle1": {
  "variant": "deny",
  "cycle": 1,
  "steps": {
   "1 agent default, policy just set": {
    "default": "Agent",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "2 human default via re-invoke (new human window)": {
    "humanWindowOpened": true,
    "default": "Human",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "3 agent default again via activate": {
    "default": "Agent",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "4 human default via activateTarget+bringToFront": {
    "default": "Human",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "5 agent default again via re-invoke (second agent window)": {
    "default": "Agent",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "6 SECOND agent-profile window, policy not re-set": {
    "default": "Agent",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   }
  },
  "defaultWhenPolicySet": "Agent",
  "policySet": {
   "download": "ok",
   "read": "ok",
   "write": "ok"
  }
 },
 "POLICY.deny.cycle2": {
  "variant": "deny",
  "cycle": 2,
  "steps": {
   "1 agent default, policy just set": {
    "default": "Agent",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "2 human default via re-invoke (new human window)": {
    "humanWindowOpened": true,
    "default": "Human",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "3 agent default again via activate": {
    "default": "Agent",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "4 human default via activateTarget+bringToFront": {
    "default": "Human",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "5 agent default again via re-invoke (second agent window)": {
    "default": "Agent",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "6 SECOND agent-profile window, policy not re-set": {
    "default": "Agent",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   }
  },
  "defaultWhenPolicySet": "Agent",
  "policySet": {
   "download": "ok",
   "read": "ok",
   "write": "ok"
  }
 },
 "POLICY.deny.cycle3": {
  "variant": "deny",
  "cycle": 3,
  "steps": {
   "1 agent default, policy just set": {
    "default": "Agent",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "2 human default via re-invoke (new human window)": {
    "humanWindowOpened": true,
    "default": "Human",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "3 agent default again via activate": {
    "default": "Agent",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "4 human default via activateTarget+bringToFront": {
    "default": "Human",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "5 agent default again via re-invoke (second agent window)": {
    "default": "Agent",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "6 SECOND agent-profile window, policy not re-set": {
    "default": "Agent",
    "download": "nowhere",
    "downloadEvents": "inProgress/canceled",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   }
  },
  "defaultWhenPolicySet": "Agent",
  "policySet": {
   "download": "ok",
   "read": "ok",
   "write": "ok"
  }
 },
 "POLICY.allowAndName.cycle1": {
  "variant": "allowAndName",
  "cycle": 1,
  "steps": {
   "1 agent default, policy just set": {
    "default": "Agent",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "2 human default via re-invoke (new human window)": {
    "humanWindowOpened": true,
    "default": "Human",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "3 agent default again via activate": {
    "default": "Agent",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "4 human default via activateTarget+bringToFront": {
    "default": "Human",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "5 agent default again via re-invoke (second agent window)": {
    "default": "Agent",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "6 SECOND agent-profile window, policy not re-set": {
    "default": "Agent",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   }
  },
  "defaultWhenPolicySet": "Agent",
  "policySet": {
   "download": "ok",
   "read": "ok",
   "write": "ok"
  }
 },
 "POLICY.allowAndName.cycle2": {
  "variant": "allowAndName",
  "cycle": 2,
  "steps": {
   "1 agent default, policy just set": {
    "default": "Agent",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "2 human default via re-invoke (new human window)": {
    "humanWindowOpened": true,
    "default": "Human",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "3 agent default again via activate": {
    "default": "Agent",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "4 human default via activateTarget+bringToFront": {
    "default": "Human",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "5 agent default again via re-invoke (second agent window)": {
    "default": "Agent",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "6 SECOND agent-profile window, policy not re-set": {
    "default": "Agent",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   }
  },
  "defaultWhenPolicySet": "Agent",
  "policySet": {
   "download": "ok",
   "read": "ok",
   "write": "ok"
  }
 },
 "POLICY.allowAndName.cycle3": {
  "variant": "allowAndName",
  "cycle": 3,
  "steps": {
   "1 agent default, policy just set": {
    "default": "Agent",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "2 human default via re-invoke (new human window)": {
    "humanWindowOpened": true,
    "default": "Human",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "3 agent default again via activate": {
    "default": "Agent",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "4 human default via activateTarget+bringToFront": {
    "default": "Human",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "5 agent default again via re-invoke (second agent window)": {
    "default": "Agent",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   },
   "6 SECOND agent-profile window, policy not re-set": {
    "default": "Agent",
    "download": "policy temp folder",
    "downloadEvents": "inProgress/completed",
    "clipboardWriteText": "ERR NotAllowedError",
    "systemClipboard": "unchanged"
   }
  },
  "defaultWhenPolicySet": "Agent",
  "policySet": {
   "download": "ok",
   "read": "ok",
   "write": "ok"
  }
 },
 "rows": [
  {
   "variant": "deny",
   "cycle": 1,
   "steps": {
    "1 agent default, policy just set": {
     "default": "Agent",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "2 human default via re-invoke (new human window)": {
     "humanWindowOpened": true,
     "default": "Human",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "3 agent default again via activate": {
     "default": "Agent",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "4 human default via activateTarget+bringToFront": {
     "default": "Human",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "5 agent default again via re-invoke (second agent window)": {
     "default": "Agent",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "6 SECOND agent-profile window, policy not re-set": {
     "default": "Agent",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    }
   },
   "defaultWhenPolicySet": "Agent",
   "policySet": {
    "download": "ok",
    "read": "ok",
    "write": "ok"
   }
  },
  {
   "variant": "deny",
   "cycle": 2,
   "steps": {
    "1 agent default, policy just set": {
     "default": "Agent",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "2 human default via re-invoke (new human window)": {
     "humanWindowOpened": true,
     "default": "Human",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "3 agent default again via activate": {
     "default": "Agent",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "4 human default via activateTarget+bringToFront": {
     "default": "Human",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "5 agent default again via re-invoke (second agent window)": {
     "default": "Agent",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "6 SECOND agent-profile window, policy not re-set": {
     "default": "Agent",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    }
   },
   "defaultWhenPolicySet": "Agent",
   "policySet": {
    "download": "ok",
    "read": "ok",
    "write": "ok"
   }
  },
  {
   "variant": "deny",
   "cycle": 3,
   "steps": {
    "1 agent default, policy just set": {
     "default": "Agent",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "2 human default via re-invoke (new human window)": {
     "humanWindowOpened": true,
     "default": "Human",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "3 agent default again via activate": {
     "default": "Agent",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "4 human default via activateTarget+bringToFront": {
     "default": "Human",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "5 agent default again via re-invoke (second agent window)": {
     "default": "Agent",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "6 SECOND agent-profile window, policy not re-set": {
     "default": "Agent",
     "download": "nowhere",
     "downloadEvents": "inProgress/canceled",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    }
   },
   "defaultWhenPolicySet": "Agent",
   "policySet": {
    "download": "ok",
    "read": "ok",
    "write": "ok"
   }
  },
  {
   "variant": "allowAndName",
   "cycle": 1,
   "steps": {
    "1 agent default, policy just set": {
     "default": "Agent",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "2 human default via re-invoke (new human window)": {
     "humanWindowOpened": true,
     "default": "Human",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "3 agent default again via activate": {
     "default": "Agent",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "4 human default via activateTarget+bringToFront": {
     "default": "Human",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "5 agent default again via re-invoke (second agent window)": {
     "default": "Agent",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "6 SECOND agent-profile window, policy not re-set": {
     "default": "Agent",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    }
   },
   "defaultWhenPolicySet": "Agent",
   "policySet": {
    "download": "ok",
    "read": "ok",
    "write": "ok"
   }
  },
  {
   "variant": "allowAndName",
   "cycle": 2,
   "steps": {
    "1 agent default, policy just set": {
     "default": "Agent",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "2 human default via re-invoke (new human window)": {
     "humanWindowOpened": true,
     "default": "Human",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "3 agent default again via activate": {
     "default": "Agent",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "4 human default via activateTarget+bringToFront": {
     "default": "Human",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "5 agent default again via re-invoke (second agent window)": {
     "default": "Agent",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "6 SECOND agent-profile window, policy not re-set": {
     "default": "Agent",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    }
   },
   "defaultWhenPolicySet": "Agent",
   "policySet": {
    "download": "ok",
    "read": "ok",
    "write": "ok"
   }
  },
  {
   "variant": "allowAndName",
   "cycle": 3,
   "steps": {
    "1 agent default, policy just set": {
     "default": "Agent",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "2 human default via re-invoke (new human window)": {
     "humanWindowOpened": true,
     "default": "Human",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "3 agent default again via activate": {
     "default": "Agent",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "4 human default via activateTarget+bringToFront": {
     "default": "Human",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "5 agent default again via re-invoke (second agent window)": {
     "default": "Agent",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    },
    "6 SECOND agent-profile window, policy not re-set": {
     "default": "Agent",
     "download": "policy temp folder",
     "downloadEvents": "inProgress/completed",
     "clipboardWriteText": "ERR NotAllowedError",
     "systemClipboard": "unchanged"
    }
   },
   "defaultWhenPolicySet": "Agent",
   "policySet": {
    "download": "ok",
    "read": "ok",
    "write": "ok"
   }
  }
 ],
 "Cleanup": "clipboard restored: true; files with this run's id left in ~/Downloads: 0"
}
```
