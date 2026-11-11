# Task 2 report: CDP client and browser launcher

## Implemented
- crates/pegboard-core/src/cdp.rs: CdpConn (tokio-tungstenite, one reader task routing responses by id and events to a broadcast channel of 1024, 30 s request timeout, pending requests failed on close), attach (flatten), evaluate (awaitPromise, returnByValue, exceptions become errors), navigate (subscribes before Page.navigate, waits for Page.loadEventFired for that session, 15 s), TargetInfo, page_targets.
- crates/pegboard-core/src/browser.rs: HUMAN_PROFILE_DIR, BrowserConfig, find_chrome (PEGBOARD_CHROME, else newest chrome-* by numeric version), Browser::launch (brief's arguments, no start URL, DevToolsActivePort polling, early exit detection, human_context = context of first page target), open_profile_window (re-invokes the binary; tracks profile_dir -> context and returns the known context without reopening if a page target of it still exists; "Default" returns human_context), shutdown (Browser.close, SIGKILL after 5 s). Child is spawned with kill_on_drop, so a failing test still kills the browser.
- paths.rs: added `paths_in(home)`; `paths()` now delegates to it.
- Cargo: workspace deps tokio-tungstenite 0.26 and futures-util 0.3.
- extension/bridge/manifest.json and sw.js: placeholder for Task 3.
- tests/common/mod.rs (env(), fixture() with a hand-rolled tokio HTTP server serving /counter, /form, /opener, /cookie/set, /cookie/get, /writer) and tests/e2e_browser.rs (the 3 tests from the brief verbatim).

## Tests
- `cargo test -p pegboard-core --test e2e_browser -- --ignored --test-threads=1`: 3 passed (about 4 s). No leftover Chrome processes afterwards.
- `cargo test --workspace`: all pass (e2e ignored by default); `cargo clippy --workspace --all-targets`: no warnings.
- RED step: first compile was done together with implementation, so no separate failing run was recorded (the brief's expected RED was a compile error because modules did not exist).

## Files
Cargo.toml, Cargo.lock, crates/pegboard-core/{Cargo.toml, src/lib.rs, src/paths.rs, src/cdp.rs, src/browser.rs, tests/e2e_browser.rs, tests/common/mod.rs}, extension/bridge/{manifest.json, sw.js}

## Notes
- Commit: 3c87e74. Commit trailer uses the line from global-constraints.md (Claude Fable 5.2).
- Browser has extra private fields (child, profiles), beyond the brief's public ones.
- If a profile's window was closed by the user, the next open_profile_window reopens it and may yield the same context id.

## Fix round 1
- browser.rs open_profile_window: a new page target is accepted only if its browserContextId is Some, non-empty, differs from human_context and differs from the context of every other recorded profile. Poll timeout still returns an error (no guess).
- Added e2e test human_tab_opened_meanwhile_is_not_mistaken_for_the_agent_profile (human tab via Target.createTarget concurrent with open_profile_window; asserts it lands in human_context; PegboardAgent and PegboardAgent-work contexts differ from human and each other).
- Command: cargo test -p pegboard-core --test e2e_browser -- --ignored --test-threads=1
- Output: 4 passed; 0 failed (about 4.2 s). Clippy clean, no leftover Chrome.
- Commit: 698936f fix: never take a human tab's context for an agent profile
