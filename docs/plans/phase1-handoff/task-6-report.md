# Task 6 report: agent profile registry and session manager

Status: DONE_WITH_CONCERNS (all tests pass; the concerns are about API additions beyond the brief, see the end)

Commit: `35c4f0a feat: agent profiles and session manager with focus emulation` (BASE b2af113)

## What was implemented

### `crates/pegboard-core/src/profiles.rs` (new)
- `DEFAULT_PROFILE = "default"`, `profile_dir` ("default" -> "PegboardAgent", "work" -> "PegboardAgent-work"), `valid_profile_name` (1..=32 bytes of `[a-z0-9-]`, not starting with '-').
- `Profiles::load(paths)`: reads `<home>/state.json` (`{"profiles":[...]}`, named profiles only, "default" implied). A missing file means no named profiles. Names in the file that are invalid, duplicated or "default" are dropped on load, so they never reach the filesystem or a process argument.
- `list()` ("default" first, then creation order), `exists()`, `create()` (invalid_params for a bad or duplicate name, also "default"), `delete(name, in_use)` (invalid_params for "default", then profile_not_found, then profile_in_use). On a failed save the in-memory change is rolled back and `internal` is returned.
- Save: directory created with 0700 if missing, then `state.json.tmp` opened with `create_new` + mode 0600 (permissions set again explicitly), written, fsynced, renamed over `state.json`. The temp file is removed on failure.
- `pub(crate) rpc_error` helper shared with session.rs.

### `crates/pegboard-core/src/session.rs` (new)
- `group_title` (blank or missing name -> session id; over 60 chars -> 59 chars + '…', counted in chars), `group_color` (9 colors, cycling).
- `ProfileRuntime { context, bridge }`.
- `SessionManager` = `Arc<Inner>` + the watcher task handle (aborted on Drop). `new` must run inside a tokio runtime.
- `create`: checks profile (valid name AND registered, else profile_not_found), checks the URL with `filter::normalize_url` (invalid_params), turns on target discovery on the daemon connection once, gets the profile runtime, reserves a never-issued id, `bridge.create_group(title, color)`, `proxy.register_with_bridge(...)`, inserts a not-yet-ready record, attaches focus emulation to the first tab, navigates it if a URL was given (on a temporary CDP session of its own; waits for the load event up to 15 s, a slow load is not an error, a navigation `errorText` is invalid_params), then marks the record ready. Any failure after the group exists runs `discard` (unregister, remove record, close the first tab plus everything in `bridge.members(group)` and the proxy scope). Recording id `r_` + the 8 id characters.
- `list` (ready sessions, creation order), `info`, `endpoint`, `touch`: targets always come from `proxy.targets(id)`, which the watcher keeps current. Sessions still being created are hidden (session_not_found).
- `close`: removes the record first (concurrent calls get session_not_found), collects `proxy.targets(id)`, unregisters the proxy (clients drop before tabs close), adds `bridge.members(group)`, then either closes all those tabs (only the ones still alive, retried once since the bridge's `closeTargets` fails as a whole on one unknown id) or `bridge.ungroup`. Focus sessions on tabs that stay open are detached. Returns the info with `state: Closed` and the targets the session had. Closed sessions are forgotten (info/endpoint/close then give session_not_found, matching Task 9's `full_flow`).
- `new_tab(id, url)`: checks the URL, `bridge.create_tab`; if that fails and the group has no members, Chrome has deleted the group together with its last tab (found by the e2e test: `No group with id`), so it creates a new group with the same title and color, moves the record and the proxy (`set_group`) to it and navigates its blank tab. Then verifies the tab's `browserContextId` is the session's, attaches focus emulation, `proxy.add_target`. On any failure the new tab is closed.
- `open_profile(name)`: profile check, then the runtime (window + bridge).
- `with_profiles(f)`: access to the registry (added; see concerns).

### `crates/pegboard-core/src/proxy/server.rs` (changed)
- `SessionEntry` now carries its `SessionId`, an optional per-session bridge (`None` = the server's own bridge, as before) and its group behind a `Mutex`.
- `register` is unchanged in signature and behaviour; new `register_with_bridge(id, context, group, bridge, targets)`.
- New `add_target(&self, id, target_id) -> bool` (false if the session is unknown; calls the callback only when the target is new to the scope), `remove_target(&self, id, target_id)`, `set_group(&self, id, group)`, `on_target_added(&self, f)` with `pub type TargetAdded`.
- The callback is invoked, after the scope lock is released and before the client is sent anything about the target, in: `finish_tab` (bridge-created tab via client `Target.createTarget`), `on_browser_event` `Adopt` (only when newly inserted), `adopt_listed_popups` (now returns the adopted ids), `add_target`.
- `ClientConn` holds `shared` instead of a bridge clone; `CreateTab` uses the session's bridge, falling back to the server's.
- The file was rustfmt-clean before and is again.

## Where each controller point is implemented

1. `add_target` / `remove_target` / `on_target_added`: `proxy/server.rs`, `impl ProxyServer` (plus `Shared::entry`, `Shared::target_added`). Call sites listed above; each inserts into the scope under the scope lock, releases it, calls the callback, and only then sends to the client. Tested by `e2e_proxy::targets_entering_the_scope_are_reported_before_the_client_hears_of_them` (the callback has run by the time the client holds the `createTarget` reply; popup adoption; `add_target` twice reports once; `remove_target` hides it from the client's listing; unknown session returns false).
2. Stale `proxy.targets()`: `SessionManager::new` subscribes to `browser.conn.events()`; `Inner::ensure_discovery` sends `Target.setDiscoverTargets {discover:true}` once (tokio `OnceCell`, awaited at the start of `create`). `watch` -> `Inner::on_event`: `targetDestroyed` -> `proxy.remove_target` for every session + forget focus; `targetCreated`/`targetInfoChanged` of a page with `openerId` in a session's scope and the session's context -> `Inner::adopt` -> `proxy.add_target`. On broadcast lag, `Inner::reconcile` re-reads `Target.getTargets` (removes dead targets, adopts listed popups transitively, restores missing focus emulation). `info`/`list` read `proxy.targets`. `close` (and `discard`) add `bridge.members(group)`. Tests: `manually_closed_tab_is_reported_and_recoverable`, `popup_of_a_session_tab_joins_the_session_without_a_client`, `close_removes_tabs_and_keep_tabs_leaves_them` (a tab created directly through the bridge in the group, unknown to the proxy, is closed too).
3. Focus emulation: `Inner::ensure_focus` attaches on `browser.conn` (no proxy), sends `Emulation.setFocusEmulationEnabled {enabled:true}` and `Page.setWebLifecycleState {state:"active"}`, and keeps the session in `Inner::focus` (target -> session). Called for the first tab in `create`, in `new_tab`, and through the proxy callback (spawned) for every other target entering a scope. A focus session that Chrome detaches while its target is still in a scope is re-attached (`on_event`, `Target.detachedFromTarget`). Detached on close with keep_tabs (`release_focus`). Test: `session_tab_stays_responsive_when_hidden`. Control run: with `enabled:false` the same test fails with `visibilityState == "hidden"`, so the test discriminates. Emulation set on about:blank survives the cross-process navigation to the fixture.
4. One runtime per profile: `Inner::runtimes` (tokio Mutex, also serializes window opening), `Inner::runtime`: `open_profile_window(profile_dir(name))` each time (cheap when a page of the known context is alive), reuses the cached runtime when the context is unchanged, otherwise attaches a new `Bridge` (covers the profile being unloaded after all its windows were closed). The default profile's directory is "PegboardAgent" (`profile_dir`, unit test `profile_dirs_never_collide_with_the_human_profile`).
5. Untrusted names: `Inner::check_profile` requires `valid_profile_name` and registry membership before `profile_dir` or `open_profile_window` is reached; `Profiles::create` validates; `load` drops invalid names. Session names are only passed to `group_title` -> `bridge.create_group` (JSON-serialized by the bridge) and stored in `SessionInfo.name`. Tests: `unknown_profile_is_an_error` ("nope", "../x", "Default"), `profile_names_are_restricted`, `named_profiles_persist_in_a_private_state_file`.
6. `state.json`: `Profiles::save` (temp + rename, 0600). Test `named_profiles_persist_in_a_private_state_file` checks the mode, reload, deletion, no leftover temp file.
7. No partial state: `create` validates profile and URL before anything is made; after `create_group`, any failure runs `Inner::discard`. `new_tab` closes its tab on failure. `Profiles::create/delete` roll back on a failed save.
8. Ids: `create` loops on `SessionId::generate()` until the id is not in `Table::issued` (every id ever issued by this manager, so a recording id is never reused within a daemon run).
9. Tests shut browsers down: `Harness::shutdown` drops manager, proxy and bridge, then `Arc::try_unwrap`s the browser (retries up to 2.5 s) and calls `Browser::shutdown`; on panic, dropping the harness/runtime drops the last `Arc<Browser>` and `kill_on_drop` kills Chrome. After all runs, `ps` showed no Chrome with a temporary user data dir.
10. No `Target.setAutoAttach` anywhere on the daemon connection; only `setDiscoverTargets` + explicit `attachToTarget`. `Target.createTarget` is never used; tabs come from the bridge.

## Tests and results

Commands and results (final run, after the last code change, before the commit):

```
cargo test -p pegboard-core --lib
test result: ok. 55 passed; 0 failed

cargo test -p pegboard-core --test e2e_session -- --ignored --test-threads=1
test close_removes_tabs_and_keep_tabs_leaves_them ... ok
test create_opens_group_in_agent_window ... ok
test manually_closed_tab_is_reported_and_recoverable ... ok
test named_profile_is_isolated_from_default ... ok
test popup_of_a_session_tab_joins_the_session_without_a_client ... ok
test session_tab_stays_responsive_when_hidden ... ok
test two_sessions_with_the_same_name_get_separate_groups ... ok
test unknown_profile_is_an_error ... ok
test result: ok. 8 passed; 0 failed; finished in 16.89s

cargo test -p pegboard-core --test e2e_proxy -- --ignored --test-threads=1
test result: ok. 18 passed; 0 failed; finished in 46.13s   (17 existing + 1 new)
```

No compiler warnings in the touched crates' targets; `cargo clippy -p pegboard-core --all-targets` reports only two pre-existing warnings in `tests/e2e_bridge.rs`.

Unit tests added: 4 in the brief (`profile_names_are_restricted`, `profile_dirs_never_collide_with_the_human_profile`, `default_profile_cannot_be_deleted_and_duplicates_are_refused`, `group_title_handles_odd_names`) plus `named_profiles_persist_in_a_private_state_file` and `group_colors_cycle`. E2e: the 7 of the brief plus `popup_of_a_session_tab_joins_the_session_without_a_client` (controller point 2), and the new e2e_proxy test (point 1). Several brief tests check more than the brief lists (location of the first tab, list/info/proxy agreement, URL refusal, unknown-session `new_tab`, client `createTarget` after regrouping, double close).

## TDD evidence

RED (tests written, implementation absent):
```
cargo test -p pegboard-core --lib
error[E0425]: cannot find function `group_title` in this scope   (x4)
error[E0425]: cannot find function `profile_dir` in this scope   (x3)
error[E0425]: cannot find function `valid_profile_name` in this scope (x2)
error[E0433]: cannot find type `Profiles` in this scope
cargo test -p pegboard-core --test e2e_session --no-run
error[E0432]: unresolved import `pegboard_core::profiles::Profiles`
error[E0432]: unresolved import `pegboard_core::session::SessionManager`
```
Expected: the brief says compile errors. (The brief's command `cargo test -p pegboard-core profiles session` is rejected by cargo, which takes one filter: `unexpected argument 'session'`; I used `--lib`.)

A second, behavioural RED during implementation: `manually_closed_tab_is_reported_and_recoverable` failed with `browser_failed: ... No group with id: 1612396017` -- Chrome deletes a tab group with its last tab. Fixed by regrouping in `new_tab` (+ `ProxyServer::set_group`).

GREEN: the runs shown above.

## Files changed
- `crates/pegboard-core/src/profiles.rs` (new)
- `crates/pegboard-core/src/session.rs` (new, ~810 lines including tests)
- `crates/pegboard-core/src/proxy/server.rs`
- `crates/pegboard-core/src/lib.rs`
- `crates/pegboard-core/tests/e2e_session.rs` (new)
- `crates/pegboard-core/tests/e2e_proxy.rs` (one test added)

## Self-review findings (fixed before commit)
- `close` with keep_tabs=false only forgot focus entries; if closing failed, the surviving tabs kept an attached focus session. Now every target is released (detached) after close, whatever happened.
- `reconcile` did not restore focus emulation for scope targets whose attach had failed; it now does.

## Concerns / things the controller should rule on
1. **Per-session bridge (`register_with_bridge`)**: beyond the two additions the controller named. `ProxyServer` was started with one bridge, but each profile has its own bridge (the extension instance of the default profile cannot create tabs in a "work" group), so a client `Target.createTarget` in a named-profile session would have failed. `register` is kept as is; the plan's Task 9 assembly (`ProxyServer::start(WsUpstream)` before any profile is open) still conflicts with `start` requiring a bridge -- Task 9 must either open the default agent profile first or `start` should take no bridge (now easy: every session can bring its own).
2. **`set_group`** (also beyond the two named additions), needed because Chrome deletes an empty group (see RED above). An agent whose last tab was closed by hand and who then calls `Target.createTarget` directly through the proxy (not via `new_tab`) still gets an error until `new_tab` regrouped the session; `new_tab` is the recovery path the brief names.
3. **`with_profiles`** accessor added because `Profiles` moves into the manager and Task 9 needs `profile.create/delete/list`. Deleting a profile while a `create` for it is in flight is not atomic (in_use is computed by the caller).
4. Two concurrent `new_tab` calls on a session whose group is gone could create two groups; Task 9 serializes commands per session, which avoids it.
5. `create` with a URL whose navigation fails (`errorText`, e.g. connection refused) fails the whole create with invalid_params and rolls back; a slow load (over 15 s) does not fail it. This is my choice; the brief does not say.
6. When a target is destroyed, Chrome reports the focus session's detach before `targetDestroyed`, so the re-attach logic makes one harmless failed `attachToTarget` per closed tab.
7. Commit trailer names Opus 5.5 (the model that wrote it), per the Task 5 ruling, not the "Fable 5.2" line in global-constraints.md.

---

# Fix round 1

Commit: `35ccfcd fix: verify the first tab's profile; per-session bridges; profile deletion guarded by session creation` (on top of 35c4f0a)

## Changes

**F1 (first tab's context unchecked)** -- `session.rs`: new free function `require_context(conn, target, context)` (`Target.getTargetInfo` on the daemon's `browser.conn`; mismatch or failure -> `browser_failed`). `create` calls it right after `bridge.create_group`, before `proxy.register` and before any focus session. On mismatch the reported id is not touched at all (it is the untrusted value); only `bridge.members(group)` are closed, through the bridge (`close_tabs`), whose `closeTargets` refuses tabs outside its profile. `new_tab` uses the same helper (its error code moves from `internal` to `browser_failed`). Related hardening found while doing this: `new_tab`'s failure cleanup used to close the reported id with `Target.closeTarget` on the browser connection, which could close a human tab if the id were foreign; it now detaches its focus session and closes through the bridge only.

**R1** -- `proxy/server.rs`: `ProxyServer::start(upstream)` (no bridge; `Shared.bridge` removed). One registration function `register(id, context, group, bridge, initial_targets) -> Endpoint`; `register_with_bridge` and the optional entry bridge are gone, `SessionEntry.bridge` is a required `Arc<Bridge>`. To keep the handshake unit tests free of a real bridge, `authorize` is generic over a private one-method trait `Authorizable { fn token(&self) -> &str }` (implemented by `SessionEntry`; the tests use a `TestEntry`). Callers updated: `tests/common/mod.rs` (`session_fixture`), `tests/e2e_proxy.rs` (`two_sessions_do_not_see_each_other`), `tests/e2e_session.rs` (`Harness`), `session.rs`.

**R2** -- `session.rs`: `with_profiles` removed. `Inner.profiles` is now a `tokio::sync::Mutex<Profiles>`. `create` starts with `SessionManager::reserve`, which under that lock checks the profile (`check_profile`: valid name + registered) and, under the table lock, picks the unused id and records it in the new `Table.creating` (id -> profile). The returned `Reservation` guard removes the `creating` entry on drop (end of `create`, any error, or cancellation); by then the session has its record or never will. `delete_profile` takes the same profiles lock, computes `in_use` from records (ready or not) plus `creating`, and calls `Profiles::delete` (the fsync runs under the tokio lock only; no std mutex is held across it). New `list_profiles() -> Vec<ProfileInfo>` (ids of ready sessions per profile, oldest first), `create_profile(name) -> Result<ProfileInfo, RpcError>`, `delete_profile(name) -> Result<(), RpcError>`. `open_profile` checks under the same lock.

**R3, R4** -- no change.

**T1** -- `focused_targets()` (`#[doc(hidden)]`, sorted keys of the focus map, including attaches in flight) added as the test-visible view.

## Tests added (written before the implementation; RED was a compile failure)
RED, `cargo test -p pegboard-core --test e2e_session --no-run` / `--test e2e_proxy --no-run`:
```
error[E0061]: this function takes 2 arguments but 1 argument was supplied      (ProxyServer::start)
error[E0061]: this method takes 4 arguments but 5 arguments were supplied      (register)
error[E0599]: no method named `create_profile` / `delete_profile` (x4) / `list_profiles` / `focused_targets` found for struct `SessionManager`
```
- `popup_of_session_a_is_not_adopted_by_session_b`: A on /opener, B on /counter, popup opened from A's tab via `browser.conn` with a user gesture; polled every 50 ms for up to 3 s: B's targets never change, the popup joins A; 500 ms later B and B's proxy scope still lack it.
- `popup_in_the_human_profile_is_never_adopted`: one session; the human page navigates to /opener and opens a popup (asserted to be in the human context); after 2 s no session lists it and `focused_targets()` contains neither the popup nor the human page, but does contain the session's own tab (the view is not trivially empty).
- `profile_cannot_be_deleted_while_a_session_uses_it`: `create_profile("work")` -> Named, no sessions; session in "work"; `list_profiles` = [default (Default, no sessions), work ([id])]; `delete_profile("work")` -> profile_in_use; `delete_profile("default")` -> invalid_params; close; delete succeeds; create in "work" -> profile_not_found; second delete -> profile_not_found; `Profiles::load` from disk shows only "default".

No test for F1's mismatch branch: it needs a bridge that lies about a target id, which the real extension cannot be made to do from a test without a test hook in production code. The check itself runs (and passes) on every `create` in the e2e suite.

The "deleted while being created" race itself is not tested deterministically (it would need a hook to pause `create` after reservation); it holds by construction: reservation and the in-use check share the profiles lock, and the `creating` entry lives until the record exists.

## Covering test runs (after the last change, before the commit)
```
cargo test -p pegboard-core --lib
test result: ok. 55 passed; 0 failed

cargo test -p pegboard-core --test e2e_session -- --ignored --test-threads=1
close_removes_tabs_and_keep_tabs_leaves_them ok; create_opens_group_in_agent_window ok;
manually_closed_tab_is_reported_and_recoverable ok; named_profile_is_isolated_from_default ok;
popup_in_the_human_profile_is_never_adopted ok; popup_of_a_session_tab_joins_the_session_without_a_client ok;
popup_of_session_a_is_not_adopted_by_session_b ok; profile_cannot_be_deleted_while_a_session_uses_it ok;
session_tab_stays_responsive_when_hidden ok; two_sessions_with_the_same_name_get_separate_groups ok;
unknown_profile_is_an_error ok
test result: ok. 11 passed; 0 failed; finished in 30.66s

cargo test -p pegboard-core --test e2e_proxy -- --ignored --test-threads=1
test result: ok. 18 passed; 0 failed; finished in 43.27s
```
No compiler warnings; clippy reports only the two pre-existing warnings in `tests/e2e_bridge.rs`. No Chrome with a temporary user data dir left running afterwards.

## Remaining notes
- Concern 2 of the first report (an agent's direct `Target.createTarget` after the group was deleted fails until `new_tab` regroups) is unchanged.
- Concurrent `new_tab` on a session whose group is gone can still make two groups (Task 9 serializes per session).
