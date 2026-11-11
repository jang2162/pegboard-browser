# Task 3 report
Implemented extension/bridge/sw.js (self.peg API), crates/pegboard-core/src/bridge.rs (Bridge, GroupId; args serialized via serde_json; re-attach once on session loss; bridge_asleep error if the worker is absent), test helpers launched/activate_any_target_in/context_of, tests/e2e_bridge.rs (4 tests verbatim plus an ungroup assertion).
Result: cargo test -p pegboard-core --test e2e_bridge -- --ignored --test-threads=1 -> 4 passed in 125s. The 120s idle test passed with the CDP session attached, so the fallback WebSocket was not needed. Recorded in docs/spike-findings.md (A5, [확인]).
Concerns: tests rely on Browser drop (kill_on_drop) for cleanup on failure; tests call b.shutdown() on success. No separate RED run was captured (compile error expected before implementation).

## Fix round 1
Pre-change observation (new test, old sw.js): the old code did NOT act on the human tab. Chrome's per-profile scoping already refuses: `chrome.debugger.getTargets()` in the agent worker does not list human-profile targets, so `tabIdOf` threw `Error: unknown target <id>` for both close_targets and add_to_group; the human target stayed intact. (Finding about Chrome: the agent worker cannot see human targets.)
Changes:
- tests/e2e_bridge.rs: added `bridge_refuses_targets_of_the_human_profile`.
- sw.js `tabIdOf` (shared by addToGroup/closeTargets): after the target->tab mapping, `chrome.tabs.get` and a `chrome.windows.getAll()` membership check, rejecting with `target is not in this profile`. closeTargets resolves all ids before removing anything, so one bad id refuses the whole call.
- bridge.rs `attach`: found while re-running tests. Intermittently (about half of runs) the freshly attached worker session never answered (every evaluate timed out at 30s), also with the old sw.js. attach now requires one answered `typeof self.peg` ping (2s) and otherwise re-finds the worker and re-attaches, within the 10s budget.
Tests: `cargo test -p pegboard-core --test e2e_bridge -- --ignored --test-threads=1 --skip two_minutes` x3 -> 4 passed each (about 5s); idle test rerun -> passed (121s).
