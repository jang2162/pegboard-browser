# Task 7 report: argv filter and agent-browser runner

Status: DONE_WITH_CONCERNS (all gates pass; the concerns are design notes and deviations from the brief, listed at the end)

Commit: `1f8fa9c feat: agent-browser runner with argv filter` (base 35ccfcd)

## What I implemented

### `crates/pegboard-core/src/argv.rs`
- `Checked { Run(Vec<String>), TabNew { url } }`, `check`, `REJECTED_FLAGS` (exactly the brief's list), `REJECTED_COMMANDS` (the brief's list **plus `batch`**).
- Rejected flags are found anywhere before `--`, including the `--flag=value` form. Words after `--` are not inspected. I checked that agent-browser 0.42.0 really stops reading flags at `--`: `fill #name -- --json` filled the literal text `--json`, and `session -- --session peg-b` kept the earlier session name.
- **Deviation: the command must be the first word.** The brief says "the first non-flag word is the command". agent-browser does not work that way: it skips known flags *and their values*. So `--user-agent close session` runs `session`, and a check following the brief's rule would read `close`, or the other way round (`--user-agent x close` gives "x" to the rule but runs `close`). I verified this with `agent-browser --session peg-z --json --user-agent close session`, which printed the session. A leading flag is therefore rejected with "the command must come first; put options after it". agent-browser reads global flags after the command just the same (verified: `session --session peg-b` takes the later value), so nothing is lost. Another fact supports this: later occurrences of a flag override earlier ones (`--session peg-a ... session --session peg-b` gives `peg-b`), which is why the rejected flags must be refused anywhere. Prefix forms (`--sess`) and upper-case forms (`--SESSION`) are not treated as flags by agent-browser.
- `batch` added to the rejected commands: it runs further command lines inside one invocation and would bypass the check.
- `tab new [--label <name>] [url]` gives `TabNew { url }` (`about:blank` when no url is given). The value of `--label` is skipped and **dropped**. Task 8 should decide whether to support labels.

### `crates/pegboard-core/src/runner.rs`
- `find_agent_browser`: `$PEGBOARD_AGENT_BROWSER`, else `vendor/agent-browser/agent-browser` next to the executable, else in the repository this crate was built from (`CARGO_MANIFEST_DIR/../..`). It does not search parent directories, because a directory such as `/tmp` could hold a planted binary.
- `Runner::new`: returns `agent_browser_missing` (the message says `run scripts/fetch-agent-browser.sh`) when the binary is missing. It runs `--version` with a 10 s limit and returns `agent_browser_version` naming both the found and the expected version. It also writes `<PEGBOARD_HOME>/agent-browser.json` = `{}` (mode 0600) and points `AGENT_BROWSER_CONFIG` at it, so neither the user's `~/.agent-browser/config.json` nor a `./agent-browser.json` in the current directory changes session behaviour.
- **Invocation** (argument vector, no shell): `agent-browser --session peg-<id> [--cdp <url>] --json <argv>`. The environment has every inherited `AGENT_BROWSER_*` variable removed first. Then it sets `AGENT_BROWSER_SOCKET_DIR`, `_NO_VERSION_CHECK=1`, `_CONTENT_BOUNDARIES=1`, `_CONFIG` and **`AGENT_BROWSER_CDP_HEADERS={"Authorization":"Bearer <token>"}`**. The `--cdp-headers` flag is never used. stdin is null. Output pipes are read concurrently and drained for at most 5 s after exit. The last stdout line is parsed as JSON, otherwise kept as a string.
- `exec`: one deadline for the whole call. On timeout or detach the CLI child is killed and the call returns `timeout` (new code, see below) or `session_not_found`. A command that runs and fails is `Ok` with `success:false` in `output`. A closed tab maps to `tab_gone`: agent-browser's `No active page`, or the proxy's `Session with given id not found` / `No target with given id found` relayed as CDP errors (the first command after a manual close returns the second form, later ones the first), or a `tab_gone` code/type.
- **Attach and daemon start (deviation in order, same intent).** agent-browser's stream server opens as soon as the daemon starts, before any browser connection. `AGENT_BROWSER_STREAM_PORT=off|-1|99999` is ignored and a random port opens anyway. So `start_daemon` (run by every `exec` when no live daemon exists, serialized per session) starts the daemon with `stream disable` **without `--cdp`**. The daemon connects to the session only with the next command. `attach` = `exec get url` + `exec stream status`, and it fails if the stream is on. This also covers a daemon that died: it is restarted with the stream off, never on. I verified that an already open stream WebSocket is closed by `stream disable` (code 1005), and that an input event sent in that window does not launch a browser (`connected:false` throughout).
- `detach`: marks the session closed. Running commands are killed and new ones are refused, including one waiting to start a daemon: the closed flag is checked right before every spawn. Then it runs `close` (5 s limit, 1 s when commands were running, because the daemon finishes its current command before it reads `close`). If that fails, it kills the daemon with SIGKILL. The kill happens only if the pid in `peg-<id>.pid` really runs this binary (`proc_pidpath`), so a stale pid file can never kill an unrelated process. It removes the session's files (`.pid .sock .version .target .config .engine`) from the socket directory.
- `#[doc(hidden)] daemon_pid` and `kill_daemon` exist for the test harness's cleanup on panic.

### Other changes
- `paths.rs`: `ensure` now calls `ensure_socket_dir(ab_socket_dir)`. For `/tmp/pegboard-<uid>` and its `ab`, it creates each when missing (mode 0700, not recursive). For each, `lstat` must show a real directory (not a symlink), owned by the current uid, mode exactly 0700. Otherwise it refuses with "refusing to use <dir>: it is a symbolic link / not a directory / owned by uid N / its mode is 0755, not 0700" and leaves the directory untouched. `Runner::run` re-checks before every spawn.
- `proxy/server.rs`: a diagnostic counter of refused client requests per method (`#[doc(hidden)] ProxyServer::refused_methods()`, bounded at 1024 distinct names). It is only reachable inside the daemon, never over CDP.
- `proxy/filter.rs`: five methods added to `ALLOWED_METHODS` (see below).
- `pegboard-protocol`: `codes::TIMEOUT = "timeout"` (no code fitted a stopped command).
- `tests/common/mod.rs`: a `/frame` fixture route (same-origin iframe).
- `docs/spike-findings.md`: the A2 method additions with reasons, the commands not supported in phase 1, and a note in A9 on the stream window.

## Allowlist additions (one line each; also appended to docs/spike-findings.md A2)
- `Network.setCookies` (`cookies set`): on a page session it writes only to that tab's profile cookies. It is the plural of the already allowed `Network.setCookie`. The browser-level call is still refused by rule 5. The e2e test `cookies_set_stays_in_the_agent_profile` shows the human profile does not see the cookie.
- `Page.handleJavaScriptDialog` (`dialog accept`/`dismiss`): answers a dialog of that page only. Without it, an open `confirm` blocked the tab (the next `get title` hit the 20 s timeout and the following `open` took 13 s).
- `Page.printToPDF` (`pdf`): renders that page. It returns base64 and uses no `IO.read` stream (checked: the pdf command passed with no other refusal).
- `DOM.getFrameOwner` (`fill` etc. after `frame #fr`): looks up a frame owner element inside that page.
- `Page.getLayoutMetrics` (`screenshot --full`): reads that page's layout size only.

Not added (by rule): `Page.bringToFront` (sent by `tab t1`; refused, but the command passes), `Browser.getWindowForTarget` (sent by `set viewport`; refused, but the command passes), `DOM.setFileInputFiles` (see "not supported").

## Not supported in phase 1
- `upload`: needs `DOM.setFileInputFiles`, through which the browser reads arbitrary local files into the page. A holder of the session endpoint alone could then exfiltrate any file the user can read, which reaches outside the session. It needs a pegboard-mediated path check.
- `download`: agent-browser 0.42.0 itself refuses it over `--cdp` ("download is not supported over --cdp ... only a browser agent-browser launched can save"). `Browser.setDownloadBehavior` is browser-level and refused anyway.
- `tab <number>`: not a proxy issue. 0.42.0 accepts only tab ids or labels (`tab t1`).
README does not exist yet (Task 10 creates it). The list sits in spike-findings A2 so Task 10 can move it to "아직 안 되는 것".

## Compatibility table (real session through the proxy; `compatibility_matrix`, final run after the allowlist additions)

"Refused" = CDP methods the proxy refused while the command ran (from `refused_methods`). Before the additions, these failed: `cookies set` (Network.setCookies), `pdf` (Page.printToPDF), `dialog accept` (Page.handleJavaScriptDialog, which then hung the tab), `fill` inside a frame (DOM.getFrameOwner), `screenshot --full` (Page.getLayoutMetrics).

| Command | Result | Time | Refused | Error |
|---|---|---|---|---|
| `open /form` | pass | 12 ms | | |
| `snapshot -i` | pass | 7 ms | | |
| `click #name` | pass | 15 ms | | |
| `click @e1` (after snapshot) | pass | 11 ms | | |
| `fill #name hello` | pass | 5 ms | | |
| `type #name " world"` | pass | 7 ms | | |
| `press Enter` | pass | 4 ms | | |
| `hover #name` | pass | 6 ms | | |
| `get url` | pass | 3 ms | | |
| `get title` | pass | 3 ms | | |
| `get text label` | pass | 4 ms | | |
| `eval ...` | pass | 4 ms | | |
| `screenshot <path>` | pass | 23 ms | | |
| `scroll down` | pass | 4 ms | | |
| `wait 200` | pass | 206 ms | | |
| `back` | pass | 508 ms | | |
| `forward` | pass | 512 ms | | |
| `reload` | pass | 18 ms | | |
| `tab list` | pass | 4 ms | | |
| `tab new <url>` (run directly) | pass | 35 ms | | goes through the proxy's `Target.createTarget` -> bridge path |
| `tab t1` (switch) | pass | 11 ms | Page.bringToFront x1 | agent-browser ignores the refusal |
| `tab 1` | fail | 4 ms | | CLI: "positional integers are not accepted" |
| `cookies` | pass | 4 ms | | |
| `cookies set pegtest 1` | pass | 4 ms | | (needed Network.setCookies) |
| `storage local` | pass | 4 ms | | |
| `network requests` | pass | 4 ms | | |
| `pdf <path>` | pass | 39 ms | | (needed Page.printToPDF) |
| `upload #f <file>` | **fail** | 5 ms | DOM.setFileInputFiles x1 | not supported in phase 1 |
| `download #d <path>` | **fail** | 6 ms | | refused by agent-browser over --cdp |
| `click #c` (opens confirm) | pass | 13 ms | | |
| `dialog status` | pass | 5 ms | | |
| `dialog accept` | pass | 4 ms | | (needed Page.handleJavaScriptDialog) |
| `frame #fr` | pass | 7 ms | | |
| `fill #name` inside the frame | pass | 8 ms | | (needed DOM.getFrameOwner) |
| `frame main` | pass | 5 ms | | |
| extra: `dblclick`, `focus`, `keyboard type`, `get html`, `is visible`, `find role button click --name Add`, `scrollintoview`, `mouse move`, `console`, `errors`, `diff snapshot`, `tab close` | pass | 4-16 ms | | |
| extra: `set viewport 800 600` | pass | 4 ms | Browser.getWindowForTarget x1 | ignored by agent-browser |
| extra: `screenshot --full` | pass | 27 ms | | (needed Page.getLayoutMetrics) |

## Answers to 3a-3f
- **3a** agent-browser adopts the session's existing first tab and opens none of its own. After `attach`, `tab list` shows exactly one tab whose `targetId` is the session's first target, and the session's targets are unchanged (`attach_adopts_the_first_tab`).
- **3b** Yes. `tab new <url>` from agent-browser (browser-level auto-attach forced to `waitForDebuggerOnStart:false`, tab created through the bridge) gives a working tab: `get url` and `fill` work on it at once, and the session has 2 targets (`new_tab_and_tab_switch_work_through_the_proxy`).
- **3c** Yes. `tab t1` switches back and `click` lands on the first tab. agent-browser sends `Page.bringToFront`, which is refused, and goes on. It never sends `Target.activateTarget` (the test asserts it is absent from the refused list). The `tab <n>` syntax with a number is not accepted by 0.42.0 itself.
- **3d** Yes (`human_tab_loads_while_agent_browser_is_attached`). While agent-browser runs commands, a new human tab (opened by `window.open(..., 'noopener')` from the human page; CDP cannot create a target in the default profile by id) and a human popup both reach `readyState complete` on the right path, and agent-browser's `tab list` still shows one tab.
- **3e** Yes (`disallowed_urls_fail_and_leave_the_tab_unchanged`). `open file:///etc/hosts` and `open chrome://settings` return `success:false` with `CDP error (Page.navigate): url scheme is not allowed in a pegboard session`, and the tab stays on `/counter` (checked over the test's own connection and with `get url`).
- **3f** After `attach`, `stream status` gives `enabled:false, connected:true` and `lsof` shows the daemon listening on no TCP port, also after further commands (`stream_server_is_off_after_attach`). Window measured by polling the daemon's LISTEN sockets with `lsof` during `attach` (`stream_exposure_window_has_no_browser`, 4 runs): first seen 51-66 ms after attach started, last seen 65-101 ms (lsof sampling is about 40 ms, so the window is roughly 0-50 ms, at most about 100 ms). The whole `attach` takes 130-180 ms. **The proxy had 0 connections every time the port was seen listening** (asserted), because the stream is turned off before the daemon is given the endpoint. The brief's order (`get url` first) would have left it open while connected; I did not measure that order.

## Security of the invocation (requirement 4)
- Argument vector only (`std::process::Command`), never a shell.
- The token goes through `AGENT_BROWSER_CDP_HEADERS`, never `--cdp-headers`. Verified: the unit test `invocation_keeps_the_token_out_of_argv_and_drops_inherited_settings` checks the exact argv and env. The e2e test `token_is_not_in_process_arguments` runs `ps -axww -o pid=,command=` while a CLI is running: the CLI line shows the endpoint URL but no process anywhere shows the token. The daemon's argv is just the binary path. The daemon's files (`.config`, `.target`, `.pid`, `.version`) contain no token.
- Residual: the token is in the CLI's environment, and in the daemon's environment or memory. Same-user processes can read that (`ps -E`); other users cannot. This matches the phase 1 threat model in global-constraints.

## Daemon lifecycle (requirement 6) and timeouts (requirement 7)
- `detach_stops_the_daemon_while_a_command_runs`: with `wait 20000` running, `detach` returns in about 1.0 s. The running exec returns `session_not_found`, the daemon pid is gone, a later exec is refused, and the pid file is removed. Every test ends in `Harness::finish`, which detaches all sessions and checks that the recorded daemon pids are dead and that `pgrep -f <test chrome user-data-dir>` is empty. A drop guard kills recorded daemons if a test panics. After all runs: `pgrep -fl 'vendor/agent-browser/agent-browser$'` is empty and `/tmp/pegboard-501/ab` is empty. The user's daemon was never touched: separate socket dir, `peg-` names, and kills only by pid file plus binary-path check.
- `command_over_its_time_limit_is_stopped`: `wait 10000` with a 1000 ms limit returns `timeout` in under 3 s and the CLI process is gone. **The daemon is not killed.** It finishes the wait, so the next command took about 9 s. I kept the daemon because killing it would drop agent-browser's state (refs, active tab). See concerns.

## Tests and results
Commands and final results (all after the commit's content):
- `cargo test -p pegboard-core --lib`: 72 passed (new: argv 9, paths 3, runner 5)
- `cargo test --workspace`: all passed (protocol 3, core lib 72, e2e_runner non-ignored 2)
- `cargo test -p pegboard-core --test e2e_runner -- --ignored --test-threads=1`: 15 passed. These are the brief's 5 (`drives_the_session_tab`, `stream_server_is_off_after_attach`, `hidden_session_tab_click_is_fast`, `tab_list_shows_only_session_tabs`, `command_after_tab_was_closed_by_hand_reports_tab_gone`) plus `attach_adopts_the_first_tab`, `new_tab_and_tab_switch_work_through_the_proxy`, `human_tab_loads_while_agent_browser_is_attached`, `disallowed_urls_fail_and_leave_the_tab_unchanged`, `stream_exposure_window_has_no_browser`, `token_is_not_in_process_arguments`, `detach_stops_the_daemon_while_a_command_runs`, `command_over_its_time_limit_is_stopped`, `cookies_set_stays_in_the_agent_profile` and `compatibility_matrix` (not a gate; prints the table).
- Non-ignored in e2e_runner: `missing_binary_is_a_clear_error`, `wrong_version_is_a_clear_error` (serialized by a mutex because they set `PEGBOARD_AGENT_BROWSER`).
- Proxy code changed, so I re-ran: e2e_proxy 18 passed, e2e_session 11 passed, e2e_browser 6 passed, e2e_bridge 4 passed (all `--ignored --test-threads=1`).
- clippy: no warnings in the new or changed files (2 old warnings remain in e2e_bridge.rs). The new files and the touched filter.rs/server.rs are rustfmt-clean; paths.rs was not rustfmt-clean before and I did not reformat it.
- Output: the browser tests print measurement lines with `--nocapture` only. No warnings.

## TDD evidence
- RED (argv): `cargo test -p pegboard-core --lib argv` with only the tests written gave `6 error[E0425]: cannot find function check`, `2 error[E0433]: cannot find type Checked`, as expected.
- RED (paths): the three new tests alone gave `6 error[E0425]: cannot find function ensure_socket_dir`.
- RED (e2e, behaviour): I wrote the runner skeleton before the e2e file, so there was no compile-RED for e2e_runner. The first full e2e run failed 13/13 (`stream disable failed while attaching: Streaming is not enabled for this session`): after starting the daemon with the stream off, connecting does not turn it on again. That led to the `stream status` check. Later REDs: `command_after_tab_was_closed_by_hand_reports_tab_gone` failed with `CDP error (DOM.enable): Session with given id not found` until that proxy error was mapped. The human-tab test failed because CDP cannot create a target in the default profile by context id (test fixed). Every compatibility gap above failed before its method was added.
- GREEN: the results listed under "Tests and results".

## Files changed
- new: `crates/pegboard-core/src/argv.rs`, `crates/pegboard-core/src/runner.rs`, `crates/pegboard-core/tests/e2e_runner.rs`
- changed: `crates/pegboard-core/src/lib.rs`, `src/paths.rs`, `src/proxy/filter.rs`, `src/proxy/server.rs`, `tests/common/mod.rs`, `crates/pegboard-protocol/src/lib.rs`, `docs/spike-findings.md`

## Self-review findings (fixed before the commit)
- A single deadline now covers a daemon start plus the command. Before, the time limit could double.
- The closed flag is re-checked right before every spawn, so a command waiting to start a daemon cannot start one after `detach`.
- Daemon files are cleaned up after `close` and after a kill. The `.target` file used to stay behind.
- The argv rejection messages no longer claim that pegboard sets flags it does not set.

## Concerns and notes for the controller
1. **Deviation in `check`**: the command must be the first word (evidence above). If the brief's rule must be kept literally, it needs a full table of agent-browser's value-taking global flags, and any flag missing from that table becomes a bypass.
2. **`batch` added to `REJECTED_COMMANDS`** (it runs nested command lines that skip the check).
3. **Attach order differs from the brief**: stream off first, then connect. `exec` also restarts a dead daemon the same way. Same goal, smaller exposure, and no browser is reachable during the exposure.
4. **New protocol code `timeout`**.
5. **A timed-out command keeps the daemon busy** until agent-browser's own operation ends (the action timeout defaults to 25 s; `wait` runs to its full length). The caller never hangs, but the session's next command can wait. The alternative is to kill the daemon on timeout and let the next `exec` restart it (state lost). This is a decision for Task 8.
6. **`tab_gone` also matches the proxy's "Session with given id not found" and "No target with given id found"**. With several tabs open, such an error can be transient (agent-browser falls back to another tab on its next command), so the agent may be told `tab_gone` once while another tab remains.
7. **The CLI's own file access stays open**: `screenshot <path>`, `pdf <path>`, `--screenshot-dir`, `--download-path`, `--init-script <path>`, `cookies set --curl <file>` and `read <url>` (the CLI fetches by itself) read or write paths as the user, without going through the proxy. These are not in the brief's lists and I did not restrict them. Whether pegboard confines output and input paths is a policy question for Task 8 or later.
8. `TabNew` drops `tab new --label`.
9. The diagnostic hook `refused_methods` is compiled into production, like the existing test hooks. It is never exposed to clients.
10. The daemon check (`proc_pidpath`) is macOS-only. On other platforms the runner never kills a daemon (fails safe). macOS is the only target.
11. A process from `spikes/node_modules/agent-browser` (pid 76996, ppid 1) was already running before this task. It is not pegboard's and was left alone.

---

# Fix round 1 (review findings C1, C2, I3-I6; rulings R1-R5)

Commit: `606ea35 fix: allow-list grammar for agent-browser argv; minimal child environment` (on top of 1f8fa9c)

## What changed

- **R1/R2, which fix C1, C2, I3 and I4. `argv.rs` is rewritten as an allow-list grammar.** `REJECTED_FLAGS` and `REJECTED_COMMANDS` are gone, together with the tests that asserted the old behaviour. `GRAMMAR` is a table of `Form`s: the command and subcommand words; ordered positional `Arg`s, each a `Kind` that is required or optional; an optional `rest` kind; the `Flag`s with their arity (`Switch`, `Value(Kind)`, `OptionalBool`); `literal_rest`, which says whether `--` is allowed before the rest; and `default_file`. `check` picks the form whose words are the longest prefix of argv, then reads every remaining word against that form. The rules:
  - A word starting with `-` must be a declared flag of the form. The one exception is a numeric positional: a negative number is not read as a flag.
  - A flag value never starts with `-`, not even a negative number. A positional never starts with `-` unless it comes after an allowed `--`.
  - `--` is allowed only in `fill` and `type`, immediately before the text, once every required positional is filled and before any text word. It must be followed by at least one word.
  - A word containing NUL is refused.
  - Every refusal names the offending word.
  - Commands outside the set get "'<cmd>' is not supported in pegboard phase 1", or "'<cmd> <sub>' ..." when only the subcommand is unknown.
  - `tab new [Url]` still returns `Checked::TabNew { url }`, with the url normalized; `about:blank` when none is given. `--label` is not accepted.
  - `open`, `a11y`, `vitals`, `tab new` and `cookies set --url` take a `Url`, which is judged by `normalize_url`. The normalized string is passed on.
- **R3. File arguments.** The signature is now `check(argv, files_dir)`. `File` accepts only `^[A-Za-z0-9][A-Za-z0-9._-]{0,99}$` with an extension declared per command (`pdf`: pdf; `screenshot`: png, jpg, jpeg) and rewrites it to `<files_dir>/<name>`. A `screenshot` without a file name gets `shot-<unix millis>.png` chosen by pegboard. No form that reads a caller-chosen file remains.
  - Added `Paths::session_files(&SessionId)` (`<home>/sessions/<id>/files`) and `paths::ensure_session_files`. Every component is created 0700 and must pass the same lstat, owner and mode checks as the socket dir.
  - `ensure_owned_private_dir` is now public.
  - `Paths` gained `ab_home` (`<home>/agent-browser-home`).
- **R4, which fixes I5. Child environment.**
  - `env_clear()`, then only: `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, `HOME=<home>/agent-browser-home` (0700), `TMPDIR=<that>/tmp` (0700), `LANG` and `LC_ALL` = `en_US.UTF-8`, and pegboard's own `AGENT_BROWSER_*` (socket dir, config, no version check, content boundaries, CDP headers).
  - `current_dir` is the session's files dir. `--version` also runs with a cleared environment.
  - `--debug` and `--content-boundaries` cannot be passed (no global flag except `screenshot --annotate`).
  - stdout and stderr returned to the caller have the exact token replaced by `[redacted]`. When a command runs without an endpoint (`close`), the session's last token is used.
- **R5, which fixes I6. `tab_gone`.**
  - `exec` maps to `tab_gone` only when agent-browser's JSON says `code` or `type` `tab_gone`, or when the session has no targets left, or when the target agent-browser is bound to (its `peg-<id>.target` file) is no longer among the session's targets.
  - The session's targets come from a new `Runner::set_session_targets(f)`; the e2e harness passes `proxy.targets`. **Task 8/9 must wire this**: without it, only the `tab_gone` code is mapped.
  - A `tab_gone` error now carries the CLI output and stderr in `RpcError.data`. Any other failure is a plain `CommandExecResult` with output and stderr kept.
  - The proxy-error string matching is gone.
- **Allowlist (+2, one line each, also in spike-findings A2):**
  - `Page.createIsolatedWorld` (`a11y`): creates an isolated JS world in a frame of that page.
  - `Page.addScriptToEvaluateOnNewDocument` (`vitals`): a script for that page's next documents, no wider than `eval`.
- **Dropped from the R2 set after checking:**
  - `cookies clear` needs `Network.clearBrowserCookies`. I did not add it: its scope is the whole profile at best, and I did not verify that.
  - `set media`, `set geo` and `set offline` need `Emulation.setEmulatedMedia`, `Emulation.setGeolocationOverride` and `Network.emulateNetworkConditions`.
  - `keyboard down|up` do not exist in 0.42.0 ("Unknown subcommand").
  - `get cdp-url` is left out; it reveals the endpoint URL and is not needed.
- `DAEMON_FILES` now also cleans `.log` and `.stream`.
- docs/spike-findings.md A2 lists the phase 1 command set, the not-supported table and the two added methods.

## Final grammar

Kinds:
- Sel: non-empty selector or `@ref`.
- ShotSel: a selector that does not end in .png/.jpg/.jpeg/.webp.
- Text: one word of free text.
- Url: `normalize_url`, normalized string passed on.
- Key: `[A-Za-z0-9+]{1,64}`.
- Num: finite decimal, may be negative as a positional.
- TabRef: `t<N>`, a label, or a 32-hex target id.
- File(exts): bare name, rewritten into the files dir.
- Choice[...]: one of the listed words.

No value or positional may start with `-`, except where noted.

| Command words | Positionals (`?` = optional, `...` = rest) | Flags (arity) |
|---|---|---|
| `open` | Url | |
| `back`, `forward`, `reload` | | |
| `snapshot` | | `-i/--interactive`, `-u/--urls`, `-c/--compact`, `--delta`, `--full` (switch); `-d/--depth` Num; `-s/--selector` Sel |
| `get url`, `get title` | | |
| `get text\|html\|value\|count\|box\|styles` | Sel | |
| `get attr` | Sel, Text | |
| `is visible\|enabled\|checked` | Sel | |
| `click` | Sel | `--new-tab`, `--human` (switch) |
| `dblclick`, `hover`, `focus`, `check`, `uncheck`, `scrollintoview`, `highlight`, `frame` | Sel | |
| `fill` | Sel, Text... (required; `--` allowed right before it, then may start with `-`) | |
| `type` | Sel, Text... (as `fill`) | `--clear` (switch); `--delay` Num |
| `press` | Key | |
| `keyboard type`, `keyboard inserttext` | Text... (required; no `--`) | |
| `select` | Sel, Text... (required) | |
| `scroll` | Choice[up,down,left,right]?, Num? | `-s/--selector` Sel |
| `drag` | Sel, Sel | `--human` (switch) |
| `mouse move` | Num, Num | `--duration`, `--steps`, `--seed` Num; `--human` (switch) |
| `mouse down`, `mouse up` | Choice[left,right,middle]? | |
| `mouse wheel` | Num, Num? | |
| `find role\|text\|label\|placeholder\|alt\|title\|testid` | Text, Choice[click,fill,check,hover,text]?, Text... (optional) | `--name` Text; `--exact` (switch) |
| `find first\|last` | Sel, action?, Text... | as above |
| `find nth` | Num, Sel, action?, Text... | as above |
| `wait` | Sel? (a number waits that long) | `--state` Choice[visible,hidden,attached,detached]; `--url`, `--text`, `--fn` Text; `--load` Choice[load,domcontentloaded,networkidle]; `--timeout` Num |
| `eval` | Text... (required; no `--`) | |
| `screenshot` | ShotSel?, File(png,jpg,jpeg)?; pegboard appends `shot-<millis>.png` when no file is given | `--full/-f` (switch); `--annotate` (OptionalBool: takes a following `true`/`false`) |
| `pdf` | File(pdf) | |
| `tab` | TabRef? | |
| `tab list` | | |
| `tab new` | Url? -> `Checked::TabNew` | |
| `tab close` | TabRef? | |
| `dialog accept` | Text? | |
| `dialog dismiss`, `dialog status` | | |
| `cookies`, `cookies get` | | |
| `cookies set` | Text, Text | `--url` Url; `--domain`, `--path` Text; `--http-only`, `--secure` (switch); `--same-site` Choice[Strict,Lax,None]; `--expires` Num |
| `storage local\|session` | Text? (key) | |
| `storage local\|session get` | Text? | |
| `storage local\|session set` | Text, Text | |
| `storage local\|session clear` | | |
| `console`, `errors` | | `--clear` (switch) |
| `network requests` | | `--clear` (switch); `--filter`, `--type`, `--method`, `--status` Text |
| `a11y` | Url? | `--tags` Text; `-s/--selector` Sel |
| `vitals` | Url? | |
| `set viewport` | Num, Num, Num? | |

## How each arity was verified
1. **Help text.** `agent-browser <cmd> --help` for every command in the set, saved and read.
2. **Fake-daemon probe (main evidence).** A small Node unix-socket server posed as the daemon of session `peg-fake`: it wrote `peg-fake.pid` (its own pid), `.version` (0.42.0), `.engine` and `.config` like a real daemon, and logged each JSON request the real CLI sent. So for any command line I could see exactly how the unmodified CLI parsed it: the `action`, every field, and the global `settings`.
   - The probe ran in an isolated environment: `env -i`, a scratch HOME, our socket dir, an empty config, and `AGENT_BROWSER_EXECUTABLE_PATH=/usr/bin/false`, so that no Chrome could ever be launched. Two stray probe daemons were closed afterwards; nothing is left.
   - All 93 accepted sample lines, in their checked form (absolute file paths, normalized URLs; at least one per form, every flag with its value), produced a request on `peg-fake`. That shows `--session` was never overridden. In every case `settings` held only the probe's own `executable-path`, so no global flag consumed any word. The parsed fields matched the declared arity: for example `-d 3` became `maxDepth:3`, `--delay 10` became `delay:10`, `--same-site Strict` became `sameSite`, and `--annotate false` set `annotate:false` with the path kept.
3. **Parser behaviours this showed, and which the grammar relies on:**
   - C1 reproduced: `get url --user-agent -- --headers ...` made `user-agent` = `"--"`. `snapshot --max-output -- --session x` also took `--`.
   - `--` is honoured only by `fill` and `type` (`fill #q -- --json` gives `value:"--json"`). `keyboard type`, `eval`, `dialog accept`, `select` and `screenshot` keep `--` as a literal word. That is why `--` is allowed only for `fill` and `type`.
   - `fill` joins all remaining words, including flag-looking words after the text (`fill #a hello --x` gives `"hello --x"`). The global parser still sees those words, which is why the "no `-` word unless declared" rule matters.
   - `--state` after `wait` belongs to wait (`state:"hidden"`). Before the command it is the global storage-state option.
   - Combined short flags (`-ic`) and a value flag without its value (`snapshot -d`) are silently ignored by the CLI. The grammar refuses both.
   - `screenshot`: a single positional is a path only if it ends in lower-case .png/.jpg/.jpeg; otherwise it is a selector. With two positionals, the first is the selector and the second the path. pegboard always emits `[selector] <absolute path>`.
   - Negative numbers are not flags (`mouse wheel -50 0`, `set geo 37.7 -122.4`).
   - `dialog accept a b` keeps only `a`, so its text is a single word. `cookies set n v w` and `storage local set k v w` are CLI errors, so the grammar does not allow extra words there.
4. **End to end.** Every form actually exercised against the session went through `check` and then `exec` in `compatibility_matrix` (table below).

## Compatibility after the fix (through `check`, as Task 8 will run it)
- Everything in the earlier table still passes, plus:
  - `get value|attr|count|box|styles`, `wait --state|--text|--fn|--load`;
  - `screenshot` (named, unnamed, element), `pdf page.pdf`;
  - `storage local set/get`, `storage session clear`, `network requests --filter`, `dialog dismiss`;
  - `highlight`, `keyboard inserttext`, `is enabled`, `find nth`, `mouse down/up/wheel -50`, `scroll up 100`;
  - `a11y` (with the new method) and `vitals`.
- Refused by `check` as designed: `tab 1`, `upload`, `download`, `diff`, `read`, `set media|geo|offline`.
- Fails at the CLI: `tab close` with "Cannot close the last tab"; see concern 1.

## Tests (all real assertions on `check` or on the runner)
- argv (11): `earlier_passing_cases_still_pass` (includes `wait "#x" --state hidden`), `c1_double_dash_after_value_flag_is_rejected`, `double_dash_only_where_text_is_expected`, `unknown_flags_are_rejected_for_every_allowed_command` (every grammar form crossed with 17 bad flags, and the message must name the word), `not_supported_commands_are_rejected`, `file_names_are_confined`, `file_reading_forms_are_rejected`, `urls_are_validated_for_open_and_tab_new`, `flag_values_starting_with_dash_are_rejected_unless_text`, `global_boolean_takes_an_optional_value`, `missing_or_extra_arguments_are_rejected`.
- paths: `session_files_dir_is_private_and_not_a_symlink` (new).
- runner:
  - `child_environment_is_minimal`: a fake CLI script dumps `env`, `pwd` and argv. The variable set is exactly the 10 listed under R4; inherited `AGENT_BROWSER_PROVIDER` and `HTTPS_PROXY` are absent; HOME is pegboard's; the cwd is the files dir; no token appears in argv.
  - `token_is_scrubbed_from_returned_text`.
  - `tab_gone_only_when_the_bound_tab_left_the_session`.
- e2e (new): `screenshot_lands_in_the_session_files_dir` (named, unnamed and pdf files are in the files dir; the dir and its parent are 0700; agent-browser's tmp is empty; nothing in the test's cwd), and `wrong_tab_id_is_a_plain_failure_not_tab_gone` (`tab t9` and `tab <human target id>` both give exit 1 with output kept, and the tab still works).

Commands and results:
- `cargo test --workspace`: core lib 76 passed (argv 11, paths 5, runner 6, ...); protocol 3; e2e_runner non-ignored 2. No warnings in changed files.
- `cargo test -p pegboard-core --test e2e_runner -- --ignored --test-threads=1`: **17 passed**, the whole file. The CLI works with the new minimal environment and its own HOME; it needed nothing from the real HOME.
- Proxy filter changed, so I re-ran `e2e_proxy` (18 passed) and `e2e_session` (11 passed).
- Afterwards no agent-browser process from this worktree is running and `/tmp/pegboard-501/ab` is empty.
- clippy: no warnings in changed files.

TDD note: argv.rs was replaced wholesale, so the new unit tests were written together with the grammar, not before it. The RED for C1 is the CLI probe above, where the old `check` accepted the C1 lines because it stopped inspecting at the first `--`. Every new test passed on the grammar's first compile, except the paths test, which failed because the test itself created a directory with mode 0755. The e2e changes passed on the first full run.

## Concerns
1. **`Checked::TabNew` handled by `SessionManager::new_tab` gives a tab agent-browser never sees.** After pegboard created the second tab, `tab list` still showed 1 tab and `tab close` answered "Cannot close the last tab". This matches the phase 0 note that agent-browser does not follow tabs created by others after connecting. Running `tab new <url>` through agent-browser instead does work (3b), with the proxy creating the tab via the bridge from the normalized URL. Task 8 should either run `tab new <normalized url>` as a `Run` (the proxy still owns tab creation), or follow `new_tab` with a command that makes agent-browser pick the tab up (`tab <targetId>` accepts CDP target ids; untested). I kept `TabNew` as R2 asks.
2. **`set_session_targets` must be wired in Task 8/9**, or `tab_gone` reduces to agent-browser's own code.
3. Files the CLI writes get its default mode (0644) inside the 0700 directory. Session files directories are not deleted when a session closes; deciding that is left to Task 8/9.
4. `check` now needs the session's files dir (`paths.session_files(id)`). The runner creates it on every run; a caller that only calls `check` does not need it to exist.
