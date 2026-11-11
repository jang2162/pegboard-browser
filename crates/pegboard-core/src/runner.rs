//! Runs the pinned, unmodified agent-browser CLI against a session's proxy endpoint.
//!
//! agent-browser keeps one daemon per `--session` name; pegboard uses `peg-<session id>`
//! and a socket directory of its own, so the user's own agent-browser daemons are never
//! touched. The session token reaches agent-browser through the environment
//! (`AGENT_BROWSER_CDP_HEADERS`), never through process arguments, which every local user
//! can read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pegboard_protocol::{codes, CommandExecResult, RpcError, SessionId};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::watch;

use crate::paths::{ensure, ensure_socket_dir, Paths};
use crate::proxy::server::Endpoint;

pub const AGENT_BROWSER_VERSION: &str = "0.42.0";
/// Time limit of a command when the caller gives none.
pub const DEFAULT_TIMEOUT_MS: u64 = 60_000;
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
/// The daemon finishes a running command before it reads `close`; with commands running,
/// it is given less time before it is killed.
const BUSY_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
/// Files agent-browser keeps per session in the socket directory.
const DAEMON_FILES: &[&str] = &["pid", "sock", "version", "target", "config", "engine"];
/// Errors meaning the tab agent-browser works on is gone: agent-browser's own once the
/// session has no tab left, and the proxy's (relayed as CDP errors) for a CDP session or
/// target that left the session because its tab closed.
const TAB_GONE_ERRORS: &[&str] = &[
    "No active page",
    "Session with given id not found",
    "No target with given id found",
];
/// How long the output pipes are read after the CLI exited (a stray holder of the pipes
/// must not hang the caller).
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const BIN_IN_REPO: &str = "vendor/agent-browser/agent-browser";
const ENV_PREFIX: &str = "AGENT_BROWSER_";

fn rpc_error(code: &str, message: impl Into<String>) -> RpcError {
    RpcError {
        code: code.to_string(),
        message: message.into(),
        data: None,
    }
}

fn missing(looked_at: &[PathBuf]) -> RpcError {
    let places: Vec<String> = looked_at.iter().map(|p| p.display().to_string()).collect();
    rpc_error(
        codes::AGENT_BROWSER_MISSING,
        format!(
            "agent-browser not found (looked at {}); run scripts/fetch-agent-browser.sh",
            places.join(", ")
        ),
    )
}

/// `$PEGBOARD_AGENT_BROWSER`, else `vendor/agent-browser/agent-browser` next to the
/// executable or in the repository this crate was built from. No other directory is
/// searched: a parent directory such as `/tmp` could hold a planted binary.
pub fn find_agent_browser() -> Result<PathBuf, RpcError> {
    if let Some(p) = std::env::var_os("PEGBOARD_AGENT_BROWSER") {
        let p = PathBuf::from(p);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(missing(&[p]))
        };
    }
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::parent)
    {
        candidates.push(dir.join(BIN_IN_REPO));
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(BIN_IN_REPO),
    );
    match candidates.iter().find(|p| p.is_file()) {
        Some(p) => Ok(p.clone()),
        None => Err(missing(&candidates)),
    }
}

/// The agent-browser session (and daemon) name of a pegboard session.
pub fn daemon_session_name(id: &SessionId) -> String {
    format!("peg-{}", id.0)
}

/// Drops every inherited `AGENT_BROWSER_*` variable; pegboard sets its own.
fn clear_agent_browser_env(cmd: &mut std::process::Command) {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with(ENV_PREFIX) {
            cmd.env_remove(&key);
        }
    }
}

/// Runs `<bin> --version` with a time limit and returns its last word.
fn installed_version(bin: &Path) -> Result<String, RpcError> {
    let mut cmd = std::process::Command::new(bin);
    clear_agent_browser_env(&mut cmd);
    let mut child = cmd
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            rpc_error(
                codes::AGENT_BROWSER_MISSING,
                format!(
                    "cannot run {}: {e}; run scripts/fetch-agent-browser.sh",
                    bin.display()
                ),
            )
        })?;
    let deadline = std::time::Instant::now() + VERSION_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(rpc_error(
                    codes::AGENT_BROWSER_VERSION,
                    format!("{} --version did not answer", bin.display()),
                ));
            }
        }
    }
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        use std::io::Read;
        let _ = stdout.read_to_string(&mut out);
    }
    Ok(out.split_whitespace().last().unwrap_or("").to_string())
}

pub struct Runner {
    bin: PathBuf,
    paths: Paths,
    /// An empty agent-browser config file of pegboard's own, so neither the user's
    /// `~/.agent-browser/config.json` nor an `agent-browser.json` in some working directory
    /// changes how sessions behave.
    config: PathBuf,
    /// Entries stay after `detach`, because session ids are never reused.
    sessions: Mutex<HashMap<SessionId, Slot>>,
}

#[derive(Clone)]
struct Slot {
    /// Set to true by `detach`; running commands are then stopped and new ones refused.
    closed: watch::Sender<bool>,
    /// Held while the session's daemon is being started, so it is started once.
    starting: Arc<tokio::sync::Mutex<()>>,
    /// Commands of the session running now.
    running: Arc<AtomicUsize>,
}

/// Counts a running command for as long as it lives.
struct Running(Arc<AtomicUsize>);

impl Running {
    fn new(count: &Arc<AtomicUsize>) -> Running {
        count.fetch_add(1, Ordering::SeqCst);
        Running(count.clone())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Runner {
    pub fn new(paths: Paths) -> Result<Runner, RpcError> {
        let bin = find_agent_browser()?;
        let found = installed_version(&bin)?;
        if found != AGENT_BROWSER_VERSION {
            let found = if found.is_empty() { "nothing" } else { &found };
            return Err(rpc_error(
                codes::AGENT_BROWSER_VERSION,
                format!(
                    "{} reports version {found}, expected {AGENT_BROWSER_VERSION}; run scripts/fetch-agent-browser.sh",
                    bin.display()
                ),
            ));
        }
        let internal = |e: std::io::Error| rpc_error(codes::INTERNAL, e.to_string());
        ensure(&paths).map_err(internal)?;
        let config = paths.home.join("agent-browser.json");
        write_private(&config, b"{}\n").map_err(internal)?;
        Ok(Runner {
            bin,
            paths,
            config,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    /// Called once right after session creation: attaches agent-browser and turns its stream
    /// server off (see `start_daemon`), then checks that it is still off now that the daemon
    /// is connected to the session.
    pub async fn attach(&self, id: &SessionId, endpoint: &Endpoint) -> Result<(), RpcError> {
        for argv in [["get", "url"], ["stream", "status"]] {
            let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
            let r = self.exec(id, endpoint, &argv, DEFAULT_TIMEOUT_MS).await?;
            if !succeeded(&r) {
                return Err(rpc_error(
                    codes::BROWSER_FAILED,
                    format!(
                        "agent-browser `{}` failed while attaching: {}",
                        argv.join(" "),
                        failure_text(&r)
                    ),
                ));
            }
            if r.output.pointer("/data/enabled") == Some(&Value::Bool(true)) {
                return Err(rpc_error(
                    codes::BROWSER_FAILED,
                    "agent-browser turned its stream server back on while attaching",
                ));
            }
        }
        Ok(())
    }

    /// Runs one agent-browser command (an argv that passed `argv::check`) in the session.
    /// A command that runs but fails is a result with `success: false` in `output`; only a
    /// command that could not run, timed out, or hit a closed tab is an error.
    pub async fn exec(
        &self,
        id: &SessionId,
        endpoint: &Endpoint,
        argv: &[String],
        timeout_ms: u64,
    ) -> Result<CommandExecResult, RpcError> {
        let slot = self.slot(id);
        let closed = slot.closed.subscribe();
        if *closed.borrow() {
            return Err(session_closed(id));
        }
        let _running = Running::new(&slot.running);
        // One time limit for the whole call, including a daemon start.
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        self.start_daemon(id, &slot, &closed, deadline).await?;
        let r = self
            .run(id, Some(endpoint), argv, deadline, Some(closed))
            .await?;
        if tab_gone(&r.output) {
            return Err(rpc_error(codes::TAB_GONE, failure_text(&r)));
        }
        Ok(r)
    }

    /// Makes sure the session's daemon runs with its stream server off.
    ///
    /// agent-browser's daemon opens its unauthenticated stream server as soon as it starts,
    /// before any browser connection, and no setting keeps it off. So a daemon is started by
    /// `stream disable` without the endpoint, and connects to the session only with the next
    /// command: while the stream server is up, it has no browser to show or drive. This also
    /// covers a daemon that died and would otherwise come back with its stream on.
    async fn start_daemon(
        &self,
        id: &SessionId,
        slot: &Slot,
        closed: &watch::Receiver<bool>,
        deadline: tokio::time::Instant,
    ) -> Result<(), RpcError> {
        let _starting = slot.starting.lock().await;
        if self.daemon_alive(id) {
            return Ok(());
        }
        let argv = vec!["stream".to_string(), "disable".to_string()];
        let r = self
            .run(id, None, &argv, deadline, Some(closed.clone()))
            .await?;
        if succeeded(&r) {
            Ok(())
        } else {
            Err(rpc_error(
                codes::BROWSER_FAILED,
                format!("agent-browser could not start: {}", failure_text(&r)),
            ))
        }
    }

    fn daemon_alive(&self, id: &SessionId) -> bool {
        self.daemon_pid(id)
            .is_some_and(|pid| pid > 1 && is_this_binary(pid, &self.bin))
    }

    /// `close` on the agent-browser session, which stops its daemon. Commands of the session
    /// still running are stopped first, and later ones are refused. A daemon that does not
    /// close in time is killed.
    pub async fn detach(&self, id: &SessionId) -> Result<(), RpcError> {
        let slot = self.slot(id);
        let busy = slot.running.load(Ordering::SeqCst) > 0;
        slot.closed.send_replace(true);
        // The session's daemon-start lock: no command starts a daemon after this point.
        let _starting = slot.starting.lock().await;
        let close = vec!["close".to_string()];
        let timeout = if busy {
            BUSY_CLOSE_TIMEOUT
        } else {
            CLOSE_TIMEOUT
        };
        let deadline = tokio::time::Instant::now() + timeout;
        let closed = match self.run(id, None, &close, deadline, None).await {
            Ok(r) if succeeded(&r) => Ok(()),
            Ok(r) => Err(failure_text(&r)),
            Err(e) => Err(e.message),
        };
        let result = match closed {
            Ok(()) => Ok(()),
            Err(_) if self.kill_daemon(id) => Ok(()),
            // No daemon was running (e.g. it never started): nothing to stop.
            Err(_) if self.daemon_pid(id).is_none() => Ok(()),
            Err(why) => Err(rpc_error(
                codes::BROWSER_FAILED,
                format!(
                    "agent-browser session {} did not stop: {why}",
                    daemon_session_name(id)
                ),
            )),
        };
        if result.is_ok() {
            self.remove_daemon_files(id);
        }
        result
    }

    /// The process id agent-browser recorded for the session's daemon, if any.
    #[doc(hidden)]
    pub fn daemon_pid(&self, id: &SessionId) -> Option<i32> {
        let file = self.daemon_file(id, "pid");
        std::fs::read_to_string(file).ok()?.trim().parse().ok()
    }

    /// Last resort: kills the session's daemon, but only if the recorded process really is
    /// this agent-browser binary (a stale pid file can name an unrelated process). Returns
    /// whether a daemon was killed.
    #[doc(hidden)]
    pub fn kill_daemon(&self, id: &SessionId) -> bool {
        let Some(pid) = self.daemon_pid(id).filter(|_| self.daemon_alive(id)) else {
            return false;
        };
        // SAFETY: plain syscall on a pid that runs our binary.
        let killed = unsafe { libc::kill(pid, libc::SIGKILL) == 0 };
        if killed {
            self.remove_daemon_files(id);
        }
        killed
    }

    fn daemon_file(&self, id: &SessionId, ext: &str) -> PathBuf {
        self.paths
            .ab_socket_dir
            .join(format!("{}.{ext}", daemon_session_name(id)))
    }

    fn remove_daemon_files(&self, id: &SessionId) {
        for ext in DAEMON_FILES {
            let _ = std::fs::remove_file(self.daemon_file(id, ext));
        }
    }

    fn slot(&self, id: &SessionId) -> Slot {
        self.sessions
            .lock()
            .unwrap()
            .entry(id.clone())
            .or_insert_with(|| Slot {
                closed: watch::channel(false).0,
                starting: Arc::new(tokio::sync::Mutex::new(())),
                running: Arc::new(AtomicUsize::new(0)),
            })
            .clone()
    }

    fn command(&self, id: &SessionId, endpoint: Option<&Endpoint>, argv: &[String]) -> Command {
        let mut std_cmd = std::process::Command::new(&self.bin);
        clear_agent_browser_env(&mut std_cmd);
        std_cmd
            .env("AGENT_BROWSER_SOCKET_DIR", &self.paths.ab_socket_dir)
            .env("AGENT_BROWSER_NO_VERSION_CHECK", "1")
            .env("AGENT_BROWSER_CONTENT_BOUNDARIES", "1")
            .env("AGENT_BROWSER_CONFIG", &self.config)
            .arg("--session")
            .arg(daemon_session_name(id));
        if let Some(ep) = endpoint {
            let headers = json!({ "Authorization": format!("Bearer {}", ep.token) });
            std_cmd
                .env("AGENT_BROWSER_CDP_HEADERS", headers.to_string())
                .arg("--cdp")
                .arg(&ep.url);
        }
        std_cmd
            .arg("--json")
            .args(argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut cmd = Command::from(std_cmd);
        cmd.kill_on_drop(true);
        cmd
    }

    async fn run(
        &self,
        id: &SessionId,
        endpoint: Option<&Endpoint>,
        argv: &[String],
        deadline: tokio::time::Instant,
        closed: Option<watch::Receiver<bool>>,
    ) -> Result<CommandExecResult, RpcError> {
        // Checked right before the spawn: a command must never start a daemon after `detach`.
        if closed.as_ref().is_some_and(|c| *c.borrow()) {
            return Err(session_closed(id));
        }
        let internal =
            |what: &str, e: std::io::Error| rpc_error(codes::INTERNAL, format!("{what}: {e}"));
        ensure_socket_dir(&self.paths.ab_socket_dir)
            .map_err(|e| internal("socket directory", e))?;
        let mut child = self
            .command(id, endpoint, argv)
            .spawn()
            .map_err(|e| internal("starting agent-browser", e))?;
        let stdout = tokio::spawn(read_all(child.stdout.take()));
        let stderr = tokio::spawn(read_all(child.stderr.take()));
        let stop = |child: &mut tokio::process::Child| {
            let _ = child.start_kill();
            stdout.abort();
            stderr.abort();
        };
        let status = tokio::select! {
            s = child.wait() => s.map_err(|e| internal("waiting for agent-browser", e))?,
            _ = tokio::time::sleep_until(deadline) => {
                stop(&mut child);
                let _ = child.wait().await;
                return Err(rpc_error(
                    codes::TIMEOUT,
                    format!("agent-browser `{}` did not finish in time and was stopped", argv.join(" ")),
                ));
            }
            _ = wait_closed(closed) => {
                stop(&mut child);
                let _ = child.wait().await;
                return Err(session_closed(id));
            }
        };
        let drained = |task: tokio::task::JoinHandle<Vec<u8>>| async move {
            let abort = task.abort_handle();
            match tokio::time::timeout(DRAIN_TIMEOUT, task).await {
                Ok(Ok(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
                _ => {
                    abort.abort();
                    String::new()
                }
            }
        };
        let stdout = drained(stdout).await;
        let stderr = drained(stderr).await;
        Ok(CommandExecResult {
            exit_code: status.code().unwrap_or(-1),
            output: last_json_line(&stdout),
            stderr,
        })
    }
}

fn session_closed(id: &SessionId) -> RpcError {
    rpc_error(
        codes::SESSION_NOT_FOUND,
        format!("session {} was closed", id.0),
    )
}

async fn wait_closed(closed: Option<watch::Receiver<bool>>) {
    match closed {
        Some(mut rx) => {
            // A dropped sender cannot happen while the runner lives; treat it as closed.
            let _ = rx.wait_for(|c| *c).await;
        }
        None => std::future::pending().await,
    }
}

async fn read_all<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut buf).await;
    }
    buf
}

/// agent-browser prints its JSON result as the last line; anything else is kept as text.
fn last_json_line(stdout: &str) -> Value {
    let last = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    serde_json::from_str(last).unwrap_or_else(|_| Value::String(stdout.trim().to_string()))
}

fn succeeded(r: &CommandExecResult) -> bool {
    r.exit_code == 0 && r.output.get("success") != Some(&Value::Bool(false))
}

fn failure_text(r: &CommandExecResult) -> String {
    match r.output.get("error") {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None if !r.stderr.trim().is_empty() => r.stderr.trim().to_string(),
        None => r.output.to_string(),
    }
}

/// agent-browser's answer when the tab it works on no longer exists: `tab_gone` (pinned
/// tabs) or one of `TAB_GONE_ERRORS`.
fn tab_gone(output: &Value) -> bool {
    if output.get("success") != Some(&Value::Bool(false)) {
        return false;
    }
    let field = |k: &str| output.get(k).and_then(Value::as_str);
    field("code") == Some("tab_gone")
        || field("type") == Some("tab_gone")
        || field("error").is_some_and(|e| TAB_GONE_ERRORS.iter().any(|m| e.contains(m)))
}

/// Whether process `pid` runs the executable at `bin`.
#[cfg(target_os = "macos")]
fn is_this_binary(pid: i32, bin: &Path) -> bool {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is valid for its full length.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return false;
    }
    buf.truncate(n as usize);
    let running = PathBuf::from(String::from_utf8_lossy(&buf).into_owned());
    match (running.canonicalize(), bin.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(not(target_os = "macos"))]
fn is_this_binary(_pid: i32, _bin: &Path) -> bool {
    false
}

/// Writes a file readable only by its owner.
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn result(exit_code: i32, output: Value) -> CommandExecResult {
        CommandExecResult {
            exit_code,
            output,
            stderr: String::new(),
        }
    }

    #[test]
    fn last_stdout_line_is_the_result() {
        assert_eq!(
            last_json_line("warning: x\n{\"success\":true}\n\n"),
            json!({ "success": true })
        );
        assert_eq!(last_json_line("not json\n"), json!("not json"));
        assert_eq!(last_json_line(""), json!(""));
    }

    #[test]
    fn closed_tab_errors_are_recognized() {
        for e in [
            "No active page",
            "CDP error (DOM.enable): Session with given id not found.",
            "CDP error (Target.attachToTarget): No target with given id found",
        ] {
            assert!(tab_gone(&json!({ "success": false, "error": e })), "{e}");
        }
        assert!(tab_gone(&json!({ "success": false, "code": "tab_gone" })));
        assert!(!tab_gone(
            &json!({ "success": false, "error": "Element not found: #x" })
        ));
        assert!(!tab_gone(
            &json!({ "success": true, "error": "No active page" })
        ));
    }

    #[test]
    fn success_needs_exit_zero_and_no_failure_flag() {
        assert!(succeeded(&result(0, json!({ "success": true }))));
        assert!(!succeeded(&result(1, json!({ "success": true }))));
        assert!(!succeeded(&result(0, json!({ "success": false }))));
    }

    #[test]
    fn invocation_keeps_the_token_out_of_argv_and_drops_inherited_settings() {
        std::env::set_var("AGENT_BROWSER_PROVIDER", "browserbase");
        let tmp = tempfile::tempdir().unwrap();
        let runner = Runner {
            bin: PathBuf::from("/bin/echo"),
            paths: crate::paths::paths_in(tmp.path()),
            config: tmp.path().join("agent-browser.json"),
            sessions: Mutex::new(HashMap::new()),
        };
        let id = SessionId("s_abcdefgh".into());
        let ep = Endpoint {
            url: "ws://127.0.0.1:1/s/s_abcdefgh".into(),
            token: "tok3n".into(),
        };
        let cmd = runner.command(&id, Some(&ep), &["get".into(), "url".into()]);
        std::env::remove_var("AGENT_BROWSER_PROVIDER");
        let std_cmd = cmd.as_std();
        let args: Vec<&OsStr> = std_cmd.get_args().collect();
        assert_eq!(
            args,
            [
                "--session",
                "peg-s_abcdefgh",
                "--cdp",
                &ep.url,
                "--json",
                "get",
                "url"
            ]
            .map(OsStr::new)
        );
        let envs: HashMap<&OsStr, Option<&OsStr>> = std_cmd.get_envs().collect();
        assert_eq!(
            envs[OsStr::new("AGENT_BROWSER_PROVIDER")],
            None,
            "inherited value kept"
        );
        assert_eq!(
            envs[OsStr::new("AGENT_BROWSER_CDP_HEADERS")],
            Some(OsStr::new(r#"{"Authorization":"Bearer tok3n"}"#))
        );
        assert_eq!(
            envs[OsStr::new("AGENT_BROWSER_CONFIG")],
            Some(runner.config.as_os_str())
        );
    }

    #[test]
    fn only_this_binary_is_recognized_as_a_daemon() {
        let me = std::process::id() as i32;
        let exe = std::env::current_exe().unwrap();
        assert!(is_this_binary(me, &exe));
        assert!(!is_this_binary(me, Path::new("/bin/sh")));
        assert!(!is_this_binary(i32::MAX, &exe));
    }
}
