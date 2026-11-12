# Task 9 report: core API server, daemon, `pegboard` CLI

## Implemented
- `crates/pegboard-core/src/service.rs` (new): the `Daemon` type (`start`, `handle`, `shutdown`, plus `request_stop`/`wait_stopped`). `daemon.rs` re-exports it (`pub use crate::service::Daemon`), and `run_command` is untouched and called with its real signature. Chosen module split: `Daemon` in a new `service.rs` because `daemon.rs` already holds the Task 8 command path.
- `crates/pegboard-core/src/api.rs` (new): `serve` (unix socket, mode 0600 in the 0700 home, one JSON line per message, 4 MiB line cap, peer uid checked with `peer_cred`, i.e. getpeereid on macOS, stale socket replaced, refuses when a live daemon answers) and blocking `Client` (`connect` returns `not_running` and deletes a refused socket file; `call`).
- `crates/pegboard-cli` (new crate, binary `pegboard`): clap CLI per the brief; hidden `daemon`; `browser start` spawns `pegboard daemon` in its own process group with stdout/stderr to `<home>/daemon.log` and waits up to 20 s for the socket.
- Small changes to existing code: `Browser` got `has_exited()` and `close(&self)` (child now behind a std Mutex; `shutdown(self)` kept and now calls `close`); `Runner::cleanup_leftovers()`; protocol constant `codes::RECORDING_NOT_FOUND`.
- New dependencies: `clap 4` (cli), `libc` (cli, already used by core), `serde` (cli, workspace). Core got none.

## Rulings applied
- Every session id / recording id from a request is validated (`SessionId::parse`, `valid_recording_id`) before use; invalid -> `invalid_params` (there is no `bad_request` code).
- `session.create` runs in a spawned task and finishes even if the connection drops; on a failed attach or log open the half-made session is closed again.
- One `ActionLog` per session in a `tokio::sync::Mutex`; holding it serializes that session's commands, other sessions run concurrently.
- Runner target provider set once in `Daemon::start`.
- Command timeout -> `runner.kill_daemon(id)`.
- `evaluate_in_targets` is not exposed. `profile.*` use `list_profiles`/`create_profile`/`delete_profile`.
- Session close: runner detach (stops running commands), tabs closed (or ungrouped with `--keep-tabs`), recording finished, `<home>/sessions/<id>/files` deleted always (recording dir kept). Nothing in the codebase needs those files after close.
- `BrowserStatus` is used as it is (running, pid, chromeVersion, sessions); no port or ws URL is exposed. `browser.start` is not an API method.
- Daemon ends when the Chrome process ends (500 ms poll of `try_wait`), on SIGTERM/SIGINT, or after `browser.stop`; the socket file is removed before the sessions are closed; `daemon.pid` is written to the home (used by tests).

## Deviations / notes for the reviewer
- Leftover cleanup is scoped: `/tmp/pegboard-<uid>/ab` is shared by every pegboard home of the user, so killing all `peg-*` daemons could kill another home's live ones (e.g. a test home vs. the user's real one). `cleanup_leftovers` only handles sessions that have a directory under this home's `sessions/` (created by the runner for every session that ran a command, removed on close), kills only a process that really is the agent-browser binary, then removes the daemon files and the directory. A `peg-*` daemon with no such directory in this home is left alone.
- `Client::connect` removes a socket that refuses connections, as the brief says; a daemon that is exactly between `bind` and `listen` could lose its socket to a concurrent client. `browser start` polls with a plain connect (no deletion) for that reason. The daemon-start race of two simultaneous `pegboard daemon` is only guarded by the connect probe before launching and before binding.
- `browser stop` with no daemon exits 3. Human output of agent-browser results: its `data` text when it is a single string, else JSON; `--json` prints the agent-browser `output` as one line. `--json` must come before an agent-browser command (everything after the command name belongs to agent-browser).
- A built-in subcommand name (`session`, `profile`, `browser`, `recording`) shadows an agent-browser command of the same name.
- Peer uid check is done (peer_cred); the socket mode 0600 in the 0700 home is the first line of defence.

## TDD evidence
RED (stub `fn main() {}` binary, tests written first): `cargo test -p pegboard-cli -- --test-threads=1`
```
test commands_without_daemon_exit_3 ... FAILED   (left: 0, right: 3)
test missing_session_is_a_usage_error_without_daemon ... FAILED  (left: 0, right: 2)
test stale_socket_is_cleaned_up ... FAILED       (left: 0, right: 3)
test result: FAILED. 0 passed; 3 failed; 5 ignored
```
Expected: no implementation yet, so the binary exits 0 for everything. (The brief's "compilation error" did not apply since I created the crate with an empty main first; the core code was written before the tests were run, the CLI behaviour was RED.)

GREEN: `cargo test -p pegboard-cli -- --test-threads=1` -> `3 passed; 5 ignored` (before adding the 6th ignored test: later `5 passed` ignored run).
`cargo test -p pegboard-cli -- --ignored --test-threads=1`:
```
daemon_exits_when_browser_is_closed ok / full_flow ok / missing_session_is_a_usage_error ok /
rejected_flags_exit_2_and_are_logged ok / socket_is_owner_only ok   -> 5 passed
```
After adding an invalid-id check and `leftovers_of_a_dead_pegboard_are_cleaned_on_start`: those two reran ok (2 passed).
`cargo test --workspace` (no browser): all `ok`, 99 core unit tests, 3 cli, 0 failed.
`cargo test -p pegboard-core --test e2e_actionlog -- --ignored --test-threads=1`: `17 passed; 0 failed`.
Clippy: no new warnings (two pre-existing in tests/e2e_bridge.rs).

## Processes
After all runs, `pgrep -fl "pegboard daemon|peg-s_|Chrome for Testing"` printed nothing. Tests used temp homes only; `Home::drop` stops the daemon, kills its pid and any Chrome using that home's `--user-data-dir`.

## Files
crates/pegboard-core/src/{api.rs,service.rs,daemon.rs,lib.rs,browser.rs,runner.rs}, crates/pegboard-protocol/src/lib.rs, crates/pegboard-cli/{Cargo.toml,src/main.rs,tests/e2e_cli.rs}, Cargo.lock.

## Self-review
- Tests listed in the brief are all present (plus one extra non-ignored usage test, one invalid-id check, one leftover test). Not covered by a test: command timeout killing the daemon, concurrent commands of different sessions, SIGTERM path, peer-uid rejection.
- Daemon has no per-request log; startup line goes to daemon.log.

# Fix round (review of 7cf18e9)

## RED
Tests were written first (`crates/pegboard-core/tests/api_server.rs`, new tests in `crates/pegboard-cli/tests/e2e_cli.rs`). Against 7cf18e9 they did not compile (the new API does not exist), which is the expected failure:
```
cargo test -p pegboard-core --test api_server
error[E0432]: unresolved imports `pegboard_core::api::serve_with`, `Handler`, `ServeOptions`
cargo test -p pegboard-cli
error[E0432]: unresolved import `pegboard_core::lock`   (x2)
error[E0063]: missing field `token` in initializer of `CommandExecParams`
```
Before the implementation of `api::serve_with`, the first version of the accept test also failed once on my own test bug (`set_read_timeout` on a closed socket gives EINVAL on macOS); fixed in the test.

## GREEN
- `cargo test -p pegboard-core --test api_server`: `accept_survives_many_idle_connections ... ok`
- `cargo test -p pegboard-cli -- --test-threads=1`: 8 passed, 9 ignored (stale_socket_is_removed_only_when_lock_is_free, second_daemon_refuses_when_lock_is_held, invalid_ids_exit_2, long_home_is_a_clear_usage_error, recording_id_traversal_is_rejected, plus the earlier non-ignored ones).
- Real browser, one at a time: `session_handle_is_required`, `concurrent_browser_start_yields_one_daemon`, `sigterm_closes_sessions_then_removes_socket`: ok.
- Final: `cargo test --workspace`: all ok (99 core unit, 4 protocol, 1 api_server, 8 cli). `cargo test -p pegboard-cli -- --ignored --test-threads=1`: first run 8 ok + `rejected_flags_exit_2_and_are_logged` FAILED because my own extra check there still expected exit 1 for an invalid id (now 2 by T8); test updated, rerun ok (1 passed). The other 8 ignored tests passed in that run.
- pgrep after everything: no `pegboard daemon`, no `peg-s_` process, no Chrome with a pegboard user-data-dir. (The only Chrome for Testing processes are the user's own Playwright browser, `playwright_chromiumdev_profile`, not started by tests.)

## Changes per finding
- I1/T1: `api::serve_with(paths, Arc<dyn Handler>, ServeOptions)`; `serve` wraps it with `ServeOptions::default()` (64 connections, 10 s idle). An accept error logs one line, sleeps 100 ms and continues. Over the cap: the connection is accepted and closed at once (my choice). The idle timeout wraps the read of each request line only; the handler runs outside it. `Handler` trait (`handle`, `request_stop`) is implemented by `Daemon`.
- I2/T2: new `lock.rs` (`DaemonLock::try_acquire`, flock EX|NB on `<home>/daemon.lock`, 0600, CLOEXEC so children do not inherit it). `pegboard daemon` takes it before `Daemon::start` (so before cleanup_leftovers and Browser::launch) and holds it until exit; if held it exits "already running" after a 1 s grace (a client that probes the lock holds it for an instant, so the daemon retries 20 x 50 ms rather than failing falsely). `Client::connect` deletes a refused socket only when it can take the lock; otherwise it returns not_running and leaves the file. `browser start`: if the lock is held it does not spawn and waits for the socket; if its own daemon exits and the lock is now held by another daemon, it keeps waiting (this is what makes two concurrent starts both exit 0). The unconditional stale-socket delete and the connect probe in `serve` are gone. `paths::ensure_home` (home only) added so the refusing daemon creates nothing else.
- I3/T3, M8: `pkill -f -- <pattern>`; `Home::drop` kills the pid in `daemon.pid` only when `ps -o command= -p` equals `<test pegboard path> daemon`.
- I4/T4: session tokens as ruled. `SessionInfo.token` (optional, skipped when None) set only in the `session.create` result; `CommandExecParams.token` optional. For `session.info`/`session.close` the wire params are local structs in `service.rs` (with `token`), so `SessionCloseParams` and the core tests that build it are unchanged. `Daemon::authorize` compares with `subtle::ConstantTimeEq` against the stored token (or an empty string for an unknown id) and returns the same `session_not_found` ("no session <id>") for unknown id, missing token and wrong token. Token = 16 OS-RNG bytes, 26 lowercase base32 chars, kept only in the in-memory `Slot`. The CLI prints `<id>.<token>` as line 1 of `session create` (`handle` in --json, `token` removed from that JSON); `--session`, `PEGBOARD_SESSION`, `session info|close` take a handle (token optional on the client, the daemon refuses a missing one). session.list/recording.* /browser.stop unchanged.
- M1/T5: the daemon main no longer drops `serve` before shutting down: the server keeps answering (`not_running` for new work) while `shutdown()` closes sessions then the browser, and only then is `serve` dropped (socket removed) and the process exits (lock released). Same on browser.stop, SIGTERM/SIGINT and browser exit.
- M2/T5: `session.create` checks `stopping` under the same lock `shutdown` drains; if stopping, the session is discarded (closed) and `not_running` is returned.
- M3/T6: chosen: the slot stays in the daemon's table while a close runs (guarded by a `closing` flag, a concurrent second close gets `session_not_found`). After a failed close, if the session manager no longer has the session (its record is removed at the start of `SessionManager::close`, so this is the usual case) the slot is removed and the recording is already finalized; if the manager still has it, the slot stays and the close can be retried. No test injects a close failure.
- M4/T7: `Client::call` read timeout: timeoutMs (default 60 s) + 15 s for command.exec, 60 s otherwise; expiry gives a `timeout` RpcError.
- M5/T8: `invalid_params` exits 2; the CLI validates session ids and recording ids itself before connecting; `PEGBOARD_HOME` too long (socket path over 103 bytes) is checked before anything else: "PEGBOARD_HOME is too long for a unix socket path", exit 2. Note `pegboard session close` of a malformed id now exits 2.
- M7/T9: at the 20 s deadline `browser start` sends SIGTERM, waits up to 10 s, then SIGKILL.
- M6: covered by T8.

## Existing tests changed
`session_id()` helper became `session_handle()` (line 1 must match `^s_[a-z2-7]{8}\.[a-z2-7]{26}$`); the sessions in full_flow, rejected_flags and others now use the handle for `--session` and `session close`; full_flow's files-dir check uses the id part. `rejected_flags...`: the `../escape` id check now expects exit 2 and "not a session id". `Home::drop` rewritten (T3).

## Files touched beyond the CLI/service/api
`pegboard-protocol/src/lib.rs` (token fields, one unit test), `pegboard-core/src/lock.rs` (new), `paths.rs` (`ensure_home`, `socket_path_fits`), `session.rs` (one line: `token: None` in the SessionInfo literal), `lib.rs`. Not touched: `daemon::run_command`, argv.rs, actionlog.rs, proxy/.

## Not covered by a test
Accept errors such as EMFILE (only the idle/cap path is tested), a failing `sessions.close` (T6), read-timeout expiry of `Client::call`, SIGKILL fallback of `browser start`.

# Fix round 2 (re-review of efb5e6f)

## RED
Tests first. `cargo test -p pegboard-core --test api_server`: `error[E0407]: method delivery_failed is not a member of trait Handler` (compile failure, expected). Behavioural RED on the old binary: `cargo test -p pegboard-cli -- session_handle_without waiting_start second_daemon`:
```
test session_handle_without_token_is_a_usage_error ... FAILED   (left: 3, right: 2)
test waiting_start_spawns_when_lock_is_released ... FAILED      (ran to the 20 s deadline: "did not answer")
test second_daemon_refuses_when_lock_is_held ... ok (already passing; now with PEGBOARD_CHROME=/nonexistent and a 10 s hard limit)
```

## GREEN
- api_server: `over_cap_connection_gets_busy_error`, `undelivered_session_create_is_discarded`, `accept_survives_many_idle_connections` ok.
- CLI non-ignored: 10 passed (incl. session_handle_without_token_is_a_usage_error, waiting_start_spawns_when_lock_is_released, second_daemon_refuses_when_lock_is_held).
- Ignored, one at a time first (undelivered_session_create_is_cleaned_up, browser_start_waits_out_a_stopping_daemon, session_handle_is_required: ok), then the full ignored suite once: 11 passed. `cargo test --workspace`: all ok.
- pgrep: no `pegboard daemon`, no `peg-s_`, no non-Playwright Chrome for Testing left. The user's Playwright Chrome was not touched.

## Changes
- P1/U1: `Handler::delivery_failed(&Request, &Response)` hook, called by the connection loop when writing an answer fails or the caller's end of stream is already closed (checked with a non-blocking `fill_buf`). `Daemon` implements it for `session.create` only: it takes the session's slot out of the table and runs the normal close path (detach, close tabs, finalize recording, delete files). Note: a client that is already gone before the daemon accepts the connection is dropped by the peer-credential check (`peer_cred` fails on macOS), so the request never runs; the test therefore waits 100 ms before disconnecting. The ignored real-browser test kills `pegboard session create` 300 ms after it starts (it passed; if the kill lands after the answer was already delivered the test would fail, so it is timing-based, and it did pass each time it ran).
- U2: client read timeouts: session.create 180 s, browser.stop 300 s, command.exec timeout + 15 s, others 60 s.
- P2/U3: `BrowserStatus.stopping` (serde default false). `browser start` is one loop: a socket that answers with a daemon that is not stopping means "already running"; a stopping daemon is waited out (same 20 s deadline), then a new one is started. `browser status` prints "stopping" in the human form.
- P3/U4: if the lock is free and the socket does not answer, the loop spawns the daemon (once), also when it had been waiting for another daemon that went away.
- P4: covered by U2 (browser.stop 300 s).
- P5/U5: a connection over the cap gets one line `{"error":{"code":"internal","message":"pegboard is busy; try again"}}` and is closed (written in a short task with a 1 s limit).
- P6/U6: `--session`/PEGBOARD_SESSION without a `.` or with an empty token is exit 2 with "session handle must be <id>.<token> (the first line printed by `pegboard session create`)". `session info|close` still accept an id alone (session_handle_is_required relies on that).
- P7/U7: the client timeout message ends "; the command may still be running".
- P8/U8: the second-daemon test sets PEGBOARD_CHROME to a nonexistent path and runs with a 10 s kill limit (`Home::run_limited`).

## Other fixes found while testing
- The daemon's lock grace was cut from 1 s to 100 ms (5 x 20 ms). With 1 s, a losing daemon of two concurrent `browser start` could take the lock when the winner was stopped within that second, and become an orphan daemon with its own Chrome (seen once: a leaked daemon after the concurrent test failed). I killed that leaked process by hand (it was from my own test home). 100 ms still covers a client's lock probe.
- Tests made robust against timing: `concurrent_browser_start_yields_one_daemon` waits up to 5 s for the losing daemon to exit before counting; `stale_socket_is_removed_only_when_lock_is_free` retries the final connect for up to 2 s because another test thread that is mid-fork holds a copy of the lock descriptor for an instant (flake seen only in the parallel workspace run).
- `session_handle_is_required`: the id-only `--session` case now expects exit 2 and the message (client-side); the wrong-token and mixed-token cases stay `session_not_found` with exit 1.
- api_server tests that use blocking socket calls are `#[tokio::test(flavor = "multi_thread")]`.
