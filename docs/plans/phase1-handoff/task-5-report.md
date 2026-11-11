# Task 5 report: per-session CDP endpoint (proxy server)

Status: DONE_WITH_CONCERNS (all tests pass. The concerns are a Chrome behaviour found during the work and some follow-ups for Task 6 and Task 7.)

Commits (branch phase1-local-mvp, BASE 4cf7650):
- `45c0ec0` fix: bridge refuses relative and non-web urls for new tabs
- `c0890e6` feat: per-session CDP endpoint with tab creation through the bridge

## Most important finding (read first)

**The filter's rewrite of browser-level `Target.setAutoAttach` to `waitForDebuggerOnStart: false` is not enough on Chrome 157.**
When a client turns on browser-level auto-attach through the proxy, a human page that calls `window.open` freezes for good: `Runtime.evaluate` on it never returns, and the popup stays at url "". I measured each variant on a raw browser connection, with no proxy involved:

| browser-level setAutoAttach on a raw connection | human page after `window.open` |
|---|---|
| none | loads |
| `autoAttach:true, waitForDebuggerOnStart:false, flatten:true` | frozen (also after 25 s more) |
| same + `filter: [{type:"tab",exclude:true},{}]` | frozen |
| same + `filter: [{type:"page",exclude:true},{}]` | frozen |
| same + `filter: [{exclude:true}]` (attach to nothing) | frozen |
| `waitForDebuggerOnStart:false` + client sends `Runtime.runIfWaitingForDebugger` to every auto-attached session | loads |

Chrome even reports the popup's attach event with `"waitingForDebugger": false`.

Inside the proxy, three ways of handling these sessions gave these results: detach only, still frozen; resume only, loads; resume then detach, loads.

**Fix (in `server.rs`, `ClientConn::release_session`):** the client never sees a top-level `Target.attachedToTarget` that the filter drops (a human target, another session's target, extension workers). For each one, the proxy itself sends `Runtime.runIfWaitingForDebugger` on that session and then `Target.detachFromTarget`. The session's connection therefore keeps no attachment to human targets either, which closes the deferred Task 4 minor "browser-level setAutoAttach still creates browser-side sessions for human targets".
- Test: `human_tab_opened_during_session_loads_normally`. It failed (30 s evaluate timeout) with only the filter rewrite and passes with the release.
- A tab is held only while its event waits in the stash, i.e. while one of this connection's tab creations is in flight (about 1 s at most). It is released when that creation settles.
- **For Task 7:** auto-attached targets of the session's own (forwarded) tabs are the client's to resume. A client that relies on `waitForDebuggerOnStart:false` and never resumes could freeze its own pages that open popups. agent-browser asks for `true`, so it presumably resumes; measure it.

**Extension privileges (controller requirement 6): no hole.** Tabs the bridge opens report `typeof chrome.tabs/debugger/tabGroups` = `undefined,undefined,undefined`. The about:blank tabs (the group's first tab and a `Target.createTarget {}` tab) report `location.origin == "null"`, and the /counter tab reports the fixture origin. No change to how the bridge creates the first document was needed. Output of `session_tabs_have_no_extension_privileges`:
```
"about:blank": apis="undefined,undefined,undefined" origin="null"
"about:blank": apis="undefined,undefined,undefined" origin="null"
"http://127.0.0.1:54592/counter": apis="undefined,undefined,undefined" origin="http://127.0.0.1:54592"
```

## What was implemented

`crates/pegboard-core/src/proxy/server.rs` (new):
- `Upstream` / `UpstreamConn` traits (async-trait), `WsUpstream` (a fresh browser WebSocket per client, 256 MiB message/frame limit), `Endpoint`, `ProxyServer::{start, register, unregister, targets, port}`. These match the brief, except that `ProxyServer` has fields.
- Handshake (hand-written, so that each refusal gets the status the brief asks for). The server reads the request head (max 16 KiB, 10 s) and parses it with httparse:
  - not a websocket upgrade → `404`
  - path is not `/s/<registered id>`, or the request lacks exactly one `Authorization: Bearer <token>` header with a constant-time match → `401`
  - malformed websocket headers → `400`
  - browser unreachable → `502`
  - otherwise the server writes `101` itself and wraps the socket with `WebSocketStream::from_partially_read`.
- Per client connection, one task (`ClientConn::run`) selects over four inputs: the client, its own upstream, bridge tab-creation results, and the session's `closed` watch.
  - **Id rewriting:** every request sent upstream carries a proxy-assigned id. A pending map (upstream id → client id + method, or internal) restores the client's id on the response. Client ids cannot collide with the proxy's own requests (`getTargetInfo`, `closeTarget`, resume/detach), and an upstream reply to an unknown id is dropped. A client message without an integer id gets an error locally.
  - **CreateTab:** `bridge.create_tab` runs in a spawned task, so the connection keeps flowing. The result triggers an internal `Target.getTargetInfo` on the connection's own upstream. If `browserContextId` equals the scope's context, the target joins `scope.targets` and the client gets `{"targetId"}`. Otherwise the proxy closes the tab with `Target.closeTarget` and returns a CDP error.
  - **Stash:** while a tab creation is in flight, top-level target events that the filter drops are stashed (at most 1024). Once the new tab is in scope they are judged again, in arrival order and before the reply. Chrome reports the new tab before the bridge returns, so the client sees its own `targetCreated`, `targetInfoChanged` and `attachedToTarget`. Events still dropped are released (auto-attach sessions resumed and detached) once no creation is in flight.
  - **Popups:** the filter adopts them from `Target.targetCreated` and, new, from top-level `Target.attachedToTarget`. In addition, `adopt_listed_popups` adopts them from any `targetInfos` response by the same rule (opener in scope and same context, iterated to a fixpoint). A client that never enabled discovery still sees its popups.
  - When a response's session left the scope while the request was in flight, the filter drops the response. The client then gets a `-32001` error for its id instead of waiting forever.
- The scope is per session (`Mutex<Scope>`) and shared by all client connections of that session. The lock is never held across an await.
- `unregister` (also re-`register` of the same id, and `Drop` of `ProxyServer`) sets the session's `closed` watch. Every client connection of that session closes its websocket and drops its upstream. `Drop` also aborts the accept loop, so no listener is left behind.

`crates/pegboard-core/src/proxy/filter.rs`, two rules added, each with a unit test:
- rule 3 also adopts a popup reported by a top-level `Target.attachedToTarget` (browser-level auto-attach can report it before or without `targetCreated`);
- new rule 5: `Target.receivedMessageFromTarget` (non-flat sessions) is dropped unless its `params.sessionId` is in scope.

`crates/pegboard-core/src/cdp.rs`:
- `CdpConn::connect_with_headers(ws_url, &[(name, value)])`; `connect` delegates to it.
- Fixed the deferred Task 2 minor. The reader now marks the connection closed under the pending lock, and `send` refuses at once afterwards instead of waiting 30 s. `unregister_drops_clients` needs this.

`extension/bridge/sw.js`: `allowedUrl()`. `createTab` throws before creating any tab unless the url is exactly `about:blank` or `new URL(url)` (no base) parses with protocol `http:`, `https:` or `data:`. The browser receives `parsed.href`, never the raw string.

Tests:
- `tests/common/mod.rs`: `SessionFixture` / `session_fixture()` / `client(&endpoint)`.
- `tests/e2e_proxy.rs`: 13 tests.
- `tests/e2e_bridge.rs`: `bridge_refuses_relative_and_extension_urls`.

Dependencies added to pegboard-core: `async-trait 0.1`, `subtle 2.6`, `httparse 1` (already in the lock through tungstenite), and `rand` (workspace, 0.8). All were already in the local cargo cache.

## Controller requirements: where each is implemented

1. **Forward the re-serialized Value.** In `ClientConn::on_client`, `msg` / the `ForwardRewritten` value goes through `send_upstream` (`v.to_string()` with the id replaced). The client's text is never forwarded. Browser messages are re-serialized too.
2. **Scope bookkeeping in the server.**
   - attachToTarget responses → `on_client_response`, where `Pending::Client.method == "Target.attachToTarget"` adds `result.sessionId`.
   - Forwarded or adopted `attachedToTarget` adds `params.sessionId`, `detachedFromTarget` removes it, and `targetDestroyed` removes the target. All three are handled in `track_event`, called from `on_browser_event`.
   - The request id → method map per connection is `ClientConn::pending`.
3. **One upstream per client connection.** `handle_socket` calls `shared.upstream.open()` once per accepted client. The upstream is owned by that `ClientConn` and is never shared.
4. **`CdpConn::connect_with_headers`.** In `cdp.rs`.
5. **Bridge hardening.** `allowedUrl` in `extension/bridge/sw.js`. Test `bridge_refuses_relative_and_extension_urls` (all five urls return Err; group members and page count unchanged).
6. **No extension privileges.** Test `session_tabs_have_no_extension_privileges` checks the first tab, a `createTarget {}` tab and a /counter tab. No hole was found (see above).
7. **Extra tests.**
   - `navigate_to_file_url_is_refused`: Err mentions "url scheme", and the page stays at about:blank.
   - `human_tab_opened_during_session_loads_normally`: CDP cannot create a tab in the human profile's context ("Failed to find browser context"), so the human page opens it with `window.open` (user gesture) on the browser connection. The title is "counter" within 5 s, and the session's listing does not show the tab.
8. **Constant-time token comparison.** `subtle::ConstantTimeEq` in `authorize()`. A length mismatch leaks only the length, which is public (64 hex characters).
9. **Token.** `new_token()`: 32 bytes from `rand::rngs::OsRng`, hex encoded.
10. **Tests always shut browsers down and leave no listeners.**
    - `SessionFixture` field order drops the proxy, then the bridge, then the browser (`kill_on_drop`), then the fixture server, then the temp dir. A panicking test therefore still kills its Chrome and aborts both listeners.
    - Passing tests call `shutdown()` (Browser.close).
    - After all runs, `ps` shows no Chrome with a temporary user-data-dir.

## Tests and results

Full run before committing (output in the scratchpad file full-run.txt):
- `cargo test --workspace`: pegboard-core 40 unit tests and pegboard-protocol 3 tests pass. The e2e files are all ignored in this mode.
- `cargo test -p pegboard-core --test e2e_browser -- --ignored --test-threads=1`: 4 passed.
- `cargo test -p pegboard-core --test e2e_bridge -- --ignored --test-threads=1 --skip two_minutes`: 5 passed, 1 filtered out.
- `cargo test -p pegboard-core --test e2e_proxy -- --ignored --test-threads=1`: **13 passed** (18.7 s).
  - The brief's 8: `rejects_missing_or_wrong_token` (also checks a one-character-short token, the 101 on a correct token, and the 404 for plain HTTP), `lists_only_session_targets`, `cannot_attach_to_human_target`, `created_tab_lands_in_agent_profile_even_when_human_window_is_active`, `browser_level_cookie_read_is_refused_and_page_level_is_scoped` (also proves the cookie exists in the human profile), `popup_from_session_tab_becomes_visible` (with discovery; asserts the targetCreated event), `two_sessions_do_not_see_each_other` (also cross-endpoint session use and cross-token), `unregister_drops_clients` (also a 401 on reconnect).
  - Added: `popup_is_visible_without_target_discovery`, `created_tab_is_announced_and_auto_attached`, `session_tabs_have_no_extension_privileges`, `navigate_to_file_url_is_refused`, `human_tab_opened_during_session_loads_normally`.
- `cargo clippy -p pegboard-core --all-targets`: no warnings in the new or changed code. The 2 remaining warnings are pre-existing in e2e_bridge.rs (`&[t.clone()]`).

### TDD evidence

The server and the brief's 8 tests were written in the same pass, so the brief's step 2 RED (a compile error) was not captured separately. RED was captured for the behaviour that needed it:
- **Bridge urls.** RED: with the HEAD version of sw.js temporarily restored, `cargo test -p pegboard-core --test e2e_bridge bridge_refuses_relative -- --ignored --test-threads=1` gave `create_tab("sw.js"): Ok("014A38EA…")` → `panicked: "sw.js" was accepted`. Expected: the old createTab passed any string to chrome.tabs.update. GREEN: the same command passes, and each url fails with `url not allowed`.
- **Human tab under auto-attach.** RED: `human_tab_opened_during_session_loads_normally` with only the filter's `waitForDebuggerOnStart:false` rewrite failed with `Runtime.evaluate timed out after 30s` (the human page was frozen). With detach only (no resume) it still failed. GREEN: it passes in 1.4 s with resume + detach.
- **Own-tab events via the stash.** RED: with stashing disabled (`let stashable = false && …`, temporary), `created_tab_is_announced_and_auto_attached` failed with `no Target.targetCreated for the new tab`. Expected: Chrome reports the tab before the bridge returns its id. GREEN: it passes. Instrumentation showed that targetCreated, targetInfoChanged and attachedToTarget for the new tab were all replayed from the stash.

## Files changed

- `crates/pegboard-core/src/proxy/server.rs` (new, about 800 lines after rustfmt)
- `crates/pegboard-core/src/proxy/mod.rs`
- `crates/pegboard-core/src/proxy/filter.rs` (2 rules and 2 unit tests)
- `crates/pegboard-core/src/cdp.rs` (`connect_with_headers`, closed flag)
- `crates/pegboard-core/Cargo.toml`, `Cargo.lock`
- `crates/pegboard-core/tests/common/mod.rs` (`SessionFixture`, `session_fixture`, `client`)
- `crates/pegboard-core/tests/e2e_proxy.rs` (new)
- `crates/pegboard-core/tests/e2e_bridge.rs` (1 test)
- `extension/bridge/sw.js`

## Deviations from the brief

- `session_fixture()` returns a struct, `SessionFixture`, not the 7-tuple. The tests also need the session id, the bridge, the group and the agent context, and the struct's field order guarantees kill-on-drop order.
- `ProxyServer` has fields (the brief wrote `pub struct ProxyServer;` as a placeholder).
- **Commit trailer:** I used `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` instead of global-constraints' `Claude Fable 5.2`. My harness's attribution instruction names the model that actually wrote these commits and says it replaces earlier guidance. Amend the trailer if the plan's line must be kept.

## Self-review notes and open concerns

1. **The Chrome auto-attach freeze (above)** changes a Task 4 assumption. The filter rewrite stays (harmless), but the real protection is the proxy's resume + detach. A client that cannot see its own popup's opener resume is the Task 7 question.
2. **`targets()` can go stale.** The server only learns of `Target.targetDestroyed` when some client connection of the session has discovery on (Chrome sends target events only then). If the human closes a session tab while no discovering client is connected, `targets()` still lists it. Task 6's close path, `bridge.close_targets(proxy.targets(id))`, would then fail on the unknown id, because sw.js `closeTargets` throws on the first unknown target. Task 6 should reconcile against `bridge.members(group)` or tolerate unknown ids.
3. **Orphan tab.** If a client disconnects while its `Target.createTarget` is in flight, the bridge still creates the tab in the group. It never enters the scope (verification runs on the client's upstream, which is gone).
4. **Popup adoption from `targetInfos` responses** (`adopt_listed_popups`) is a server-side addition to the filter's rule, with the same criterion. It exists so that popups work without discovery and so the daemon's scope learns them.
5. **Shared scope.** Several client connections of the same session share one scope. CDP session ids from one connection are useless on another (Chrome scopes them per connection), so sharing `cdp_sessions` is harmless.
6. **`server.rs` is about 800 lines.** It holds one responsibility (handshake, per-connection loop, bookkeeping) and I did not split it. Flagging it per the rules in case the plan expects a smaller file.
7. **Unaddressed deferred minors.** Several deferred Task 4 minors remain (`detachFromTarget` does not check `targetId` when `sessionId` is in scope; the `getTargetInfo` singular response is not filtered but is gated client-side). The "sessions on human targets" minor is now resolved by the release.

---

# Fix round 1 (review: I1, I2, W1, plus cleanups)

Commit: `b2af113` fix: proxy connections cannot be stalled by their client and always release hidden sessions.

Files changed: `crates/pegboard-core/src/proxy/server.rs`, `crates/pegboard-core/src/proxy/filter.rs` (visibility only), `crates/pegboard-core/tests/e2e_proxy.rs`. Nothing outside these files was needed.

## New finding: W1's "turn auto-attach off first" is harmful as written

The suggested order was: turn browser-level auto-attach off, then release hidden sessions. I implemented that order first, and `disconnect_right_after_human_popup_does_not_freeze_it` failed in round 0. Instrumentation showed why:
- When Chrome processes `Target.setAutoAttach {autoAttach:false}`, it **detaches every auto-attached session of that connection** (`Target.detachedFromTarget` arrives for each).
- It does this without a resume, so it is another detach-without-resume. The popup stayed frozen.
- Resumes sent after the "off" fail with "Session with given id not found".

I also measured that a frozen target **can be recovered**: a fresh attach to it, then `Runtime.runIfWaitingForDebugger`, made the popup load (test helper experiment; 5 of 5 rounds loaded).

Consequences in the implementation:
1. **Shutdown order:** release every held session (resume + detach) first. Then send auto-attach off. Then read the upstream until the "off" is confirmed and every release is answered, bounded by 500 ms. Then close.
2. **General recovery (normal operation too):**
   - A resume sent by the proxy (`Pending::Resume { target_id }`) that comes back with an error means Chrome detached the session before the resume arrived. That happens on the shutdown off, and also when the **client itself** turns auto-attach off while a human popup's attach event is still unread.
   - The proxy then re-attaches that target (`Pending::Recover`). Chrome's `attachedToTarget` for the new session is handled like any hidden session (resume, then detach).
   - Each target is re-attached at most once (`ClientConn::recovered`), so this cannot loop.
   - In the W1 test, instrumentation showed that every one of the 5 rounds went through this recovery path (the popup's attach event arrived after the off).
   - I added a test for the client-off case: `client_turning_auto_attach_off_does_not_freeze_human_popup`.

## Changes

- **I1, writer task.**
  - Client writes go through `ClientSender`: an unbounded channel feeding a writer task, bounded by queued bytes (`MAX_QUEUED_TO_CLIENT` = 64 MiB). A single message larger than that still goes out when the queue is empty.
  - The connection loop only enqueues (`send_client` is synchronous). A client that lets the queue overflow is disconnected.
  - I bounded the queue by bytes rather than by message count, so that a slow but legitimate client receiving many small events is not cut off while big screenshots are still capped.
  - Shutdown closes the upstream first, then waits for the writer for 2 s and aborts it after that, which drops the socket.
  - `ProxyServer::open_connections()` counts client connections whose upstream is open (`OpenConnection` guard).
- **I2, stash.**
  - The stash keeps a dropped top-level target event only while a tab creation is in flight **and** only if its `targetInfo.browserContextId` equals the session's context.
  - Every other hidden auto-attached session is released at once.
  - In addition, stashed events expire after `STASH_MAX_AGE` (10 s), counted from their original arrival, which survives re-stashing. Back-to-back creations therefore cannot hold another session's agent-context tab forever either.
  - Test hook: `#[doc(hidden)] ProxyServer::set_tab_creation_delay(Duration)` delays each bridge tab creation of later connections.
- **W1, shutdown.**
  - `ClientConn::shutdown` runs on every exit path: unregister, client close or error, upstream gone, queue overflow, server drop.
  - It releases everything in `auto_sessions`: every top-level attached session this upstream holds, forwarded or stashed, together with its target. Then it turns auto-attach off and drains as described above. Every step has its own 500 ms timeout.
- **Cleanups.**
  - `filter::TARGET_EVENTS` is now `pub(crate)` and reused; the duplicate in server.rs is gone.
  - `authorize` takes the session map (`&Sessions`) instead of `&Shared`, so it is unit-testable. Construction moved into `SessionEntry::new`.
  - 9 unit tests for `parse_head`/`authorize`: correct token; missing Authorization; two Authorization headers (both orders of good/bad); wrong scheme (Basic, lowercase bearer, none, double space); wrong token of the same length, shorter, longer, empty; unknown session; trailing junk (`/`, `/x`, `?a=1`, `#f`, `/x/`, `//`, `/`); plain HTTP → 404; malformed upgrade → 400 only after auth.

## Tests added (e2e_proxy.rs)

- `client_that_stops_reading_cannot_stall_unregister_or_freeze_human_tabs`: raw tokio-tungstenite client; attach plus browser-level auto-attach (wait:true); 200 × `Runtime.evaluate` returning 1 MB, never read. Asserts that a human popup loads within 5 s, and that after `unregister` `open_connections()` reaches 0 within 3 s.
- `human_popup_is_released_while_tab_creation_is_in_flight`: tab creation delay 8 s; 3 `Target.createTarget` calls spawned without awaiting; a human `window.open` popup must load within 5 s. All 3 creations then succeed and the client lists 4 tabs.
- `disconnect_right_after_human_popup_does_not_freeze_it`: 5 rounds of: raw client enables auto-attach, the human page schedules `window.open`, the client socket is dropped at once (no close handshake), and the popup must load within 5 s. Ends with `open_connections()` reaching 0.
- `client_turning_auto_attach_off_does_not_freeze_human_popup` (added for the new finding): 5 rounds of: auto-attach on, human popup scheduled, client sends auto-attach off at once, and the popup must load within 5 s.

## Evidence

RED: the four new e2e tests, run against the previous server (`c0890e6`) with only the two test hooks patched in temporarily (a connection counter and the tab delay), with filter.rs visibility reverted, then restored. Command: `cargo test -p pegboard-core --test e2e_proxy -- --ignored --test-threads=1 client_that_stops human_popup_is_released disconnect_right_after client_turning`
```
test client_that_stops_reading_cannot_stall_unregister_or_freeze_human_tabs ... FAILED   (upstream still open 3 s after unregister)
test client_turning_auto_attach_off_does_not_freeze_human_popup ... FAILED             (round 0: human popup froze after the client turned auto-attach off)
test disconnect_right_after_human_popup_does_not_freeze_it ... FAILED                  (round 1: human popup froze after the client disconnected)
test human_popup_is_released_while_tab_creation_is_in_flight ... FAILED                (human popup was held while tab creations were in flight)
test result: FAILED. 0 passed; 4 failed
```
The W1-as-suggested order (off first) also failed the disconnect test (round 0), as described above.

GREEN, covering commands as given:
```
$ cargo test -p pegboard-core --lib proxy
test result: ok. 48 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out

$ cargo test -p pegboard-core --test e2e_proxy -- --ignored --test-threads=1
test browser_level_cookie_read_is_refused_and_page_level_is_scoped ... ok
test cannot_attach_to_human_target ... ok
test client_that_stops_reading_cannot_stall_unregister_or_freeze_human_tabs ... ok
test client_turning_auto_attach_off_does_not_freeze_human_popup ... ok
test created_tab_is_announced_and_auto_attached ... ok
test created_tab_lands_in_agent_profile_even_when_human_window_is_active ... ok
test disconnect_right_after_human_popup_does_not_freeze_it ... ok
test human_popup_is_released_while_tab_creation_is_in_flight ... ok
test human_tab_opened_during_session_loads_normally ... ok
test lists_only_session_targets ... ok
test navigate_to_file_url_is_refused ... ok
test popup_from_session_tab_becomes_visible ... ok
test popup_is_visible_without_target_discovery ... ok
test rejects_missing_or_wrong_token ... ok
test session_tabs_have_no_extension_privileges ... ok
test two_sessions_do_not_see_each_other ... ok
test unregister_drops_clients ... ok
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 35.69s
```
Further checks:
- The four auto-attach tests (`client_that_stops`, `human_popup_is_released`, `disconnect_right_after`, `human_tab_opened`) were also run twice in a row, all green.
- `cargo clippy -p pegboard-core --all-targets`: only the 2 pre-existing e2e_bridge warnings.
- No Chrome with a temporary user-data-dir was left running.

## Remaining concerns

- **Shutdown drain behind a flood.** The drain is bounded at 500 ms. If the upstream still holds many large replies for a flooding client, the "off" confirmation may not be read in time. A human popup attached in that window would then stay held. This is unlikely, and bounded by design (the review asked for ≤ 500 ms).
- **File size.** `server.rs` is now about 1190 lines; roughly 180 of them are the new unit tests. The connection logic could be split (handshake / connection / release) if the plan wants smaller files.
- **Proxy attaches to human targets.** The recovery re-attaches to a human target from the proxy's own upstream connection. The session is never visible to the client (its events are dropped by the filter's session rule) and is detached right after the resume.
