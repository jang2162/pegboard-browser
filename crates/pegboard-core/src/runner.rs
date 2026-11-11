//! Runs the pinned, unmodified agent-browser CLI against a session's proxy endpoint.
//!
//! agent-browser keeps one daemon per `--session` name; pegboard uses `peg-<session id>`
//! and a socket directory of its own, so the user's own agent-browser daemons are never
//! touched. The session token reaches agent-browser through the environment
//! (`AGENT_BROWSER_CDP_HEADERS`), never through process arguments, which every local user
//! can read. The CLI runs with a minimal environment of pegboard's choosing (its own HOME,
//! no proxy variables, nothing inherited) in the session's files directory.

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

use crate::paths::{
    ensure, ensure_owned_private_dir, ensure_session_files, ensure_socket_dir, Paths,
};
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
const DAEMON_FILES: &[&str] = &[
    "pid", "sock", "version", "target", "config", "engine", "log", "stream",
];
/// PATH of the agent-browser processes.
const CHILD_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const CHILD_LANG: &str = "en_US.UTF-8";
/// Put in place of the session token in any text returned to the caller.
const REDACTED: &str = "[redacted]";
/// How long the output pipes are read after the CLI exited (a stray holder of the pipes
/// must not hang the caller).
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const BIN_IN_REPO: &str = "vendor/agent-browser/agent-browser";

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

/// The session's targets as the proxy sees them (`ProxyServer::targets`).
pub type SessionTargets = Arc<dyn Fn(&SessionId) -> Vec<String> + Send + Sync>;

/// Runs `<bin> --version` with a time limit and returns its last word.
fn installed_version(bin: &Path) -> Result<String, RpcError> {
    let mut cmd = std::process::Command::new(bin);
    cmd.env_clear().env("PATH", CHILD_PATH);
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
    /// Tells a closed tab from an ordinary failure (see `set_session_targets`).
    targets: Mutex<Option<SessionTargets>>,
}

#[derive(Clone)]
struct Slot {
    /// Set to true by `detach`; running commands are then stopped and new ones refused.
    closed: watch::Sender<bool>,
    /// Held while the session's daemon is being started, so it is started once.
    starting: Arc<tokio::sync::Mutex<()>>,
    /// Commands of the session running now.
    running: Arc<AtomicUsize>,
    /// The session's token, scrubbed from any text returned to the caller.
    token: Arc<Mutex<Option<String>>>,
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
        ensure_owned_private_dir(&paths.ab_home).map_err(internal)?;
        ensure_owned_private_dir(&paths.ab_home.join("tmp")).map_err(internal)?;
        let config = paths.home.join("agent-browser.json");
        write_private(&config, b"{}\n").map_err(internal)?;
        Ok(Runner {
            bin,
            paths,
            config,
            sessions: Mutex::new(HashMap::new()),
            targets: Mutex::new(None),
        })
    }

    /// Gives the runner the session's current targets, so a failed command is reported as
    /// `tab_gone` exactly when the tab agent-browser works on has left the session. Without
    /// it, only agent-browser's own `tab_gone` code is.
    pub fn set_session_targets(
        &self,
        f: impl Fn(&SessionId) -> Vec<String> + Send + Sync + 'static,
    ) {
        *self.targets.lock().unwrap() = Some(Arc::new(f));
    }

    /// `set_session_targets` with an already shared provider (`SessionManager::targets_fn`).
    pub fn set_session_targets_provider(&self, f: SessionTargets) {
        *self.targets.lock().unwrap() = Some(f);
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
        *slot.token.lock().unwrap() = Some(endpoint.token.clone());
        // One time limit for the whole call, including a daemon start.
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        self.start_daemon(id, &slot, &closed, deadline).await?;
        let r = self
            .run(id, Some(endpoint), argv, deadline, Some(closed))
            .await?;
        if self.tab_gone(id, &r) {
            return Err(RpcError {
                code: codes::TAB_GONE.to_string(),
                message: failure_text(&r),
                data: Some(json!({ "output": r.output, "stderr": r.stderr })),
            });
        }
        Ok(r)
    }

    /// A failed command hit a closed tab: agent-browser says `tab_gone`, or the tab it is
    /// bound to (recorded in its `.target` file) is no longer one of the session's targets.
    /// Any other failure (a wrong tab id, an element not found) is an ordinary result.
    fn tab_gone(&self, id: &SessionId, r: &CommandExecResult) -> bool {
        if succeeded(r) {
            return false;
        }
        let field = |k: &str| r.output.get(k).and_then(Value::as_str);
        if field("code") == Some("tab_gone") || field("type") == Some("tab_gone") {
            return true;
        }
        let Some(targets) = self.targets.lock().unwrap().clone() else {
            return false;
        };
        let live = targets(id);
        let bound = self.bound_target(id);
        live.is_empty() || bound.is_some_and(|b| !live.contains(&b))
    }

    /// The tab agent-browser's daemon is bound to (from its `.target` file), if it has one.
    pub fn bound_target(&self, id: &SessionId) -> Option<String> {
        std::fs::read_to_string(self.daemon_file(id, "target"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.get("targetId").and_then(Value::as_str).map(String::from))
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
                token: Arc::new(Mutex::new(None)),
            })
            .clone()
    }

    fn command(
        &self,
        id: &SessionId,
        endpoint: Option<&Endpoint>,
        argv: &[String],
        cwd: &Path,
    ) -> Command {
        let mut std_cmd = std::process::Command::new(&self.bin);
        std_cmd
            .env_clear()
            .current_dir(cwd)
            .env("PATH", CHILD_PATH)
            .env("HOME", &self.paths.ab_home)
            .env("TMPDIR", self.paths.ab_home.join("tmp"))
            .env("LANG", CHILD_LANG)
            .env("LC_ALL", CHILD_LANG)
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
        let cwd = ensure_session_files(&self.paths, id)
            .map_err(|e| internal("session files directory", e))?;
        let token = endpoint
            .map(|e| e.token.clone())
            .or_else(|| self.slot(id).token.lock().unwrap().clone());
        let mut child = self
            .command(id, endpoint, argv, &cwd)
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
        let stdout = scrub(&drained(stdout).await, token.as_deref());
        let stderr = scrub(&drained(stderr).await, token.as_deref());
        Ok(CommandExecResult {
            exit_code: status.code().unwrap_or(-1),
            output: last_json_line(&stdout),
            stderr,
        })
    }
}

/// Defence in depth: the token never reaches the caller, whatever agent-browser prints.
fn scrub(text: &str, token: Option<&str>) -> String {
    match token {
        Some(t) if !t.is_empty() => text.replace(t, REDACTED),
        _ => text.to_string(),
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

/// The command ran and reported success.
pub fn succeeded(r: &CommandExecResult) -> bool {
    r.exit_code == 0 && r.output.get("success") != Some(&Value::Bool(false))
}

/// What a failed command said: its `error`, else its stderr.
pub fn failure_text(r: &CommandExecResult) -> String {
    match r.output.get("error") {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None if !r.stderr.trim().is_empty() => r.stderr.trim().to_string(),
        None => r.output.to_string(),
    }
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
    fn success_needs_exit_zero_and_no_failure_flag() {
        assert!(succeeded(&result(0, json!({ "success": true }))));
        assert!(!succeeded(&result(1, json!({ "success": true }))));
        assert!(!succeeded(&result(0, json!({ "success": false }))));
    }

    /// A runner over a fake `agent-browser` script, with a fresh pegboard home.
    fn fake_runner(tmp: &Path, script: &str) -> Runner {
        use std::os::unix::fs::PermissionsExt;
        let bin = tmp.join("agent-browser");
        std::fs::write(&bin, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let paths = crate::paths::paths_in(&tmp.join("home"));
        ensure_owned_private_dir(&paths.home).unwrap();
        Runner {
            bin,
            config: paths.home.join("agent-browser.json"),
            paths,
            sessions: Mutex::new(HashMap::new()),
            targets: Mutex::new(None),
        }
    }

    fn endpoint() -> Endpoint {
        Endpoint {
            url: "ws://127.0.0.1:1/s/s_abcdefgh".into(),
            token: "0123456789abcdef0123456789abcdef".into(),
        }
    }

    fn deadline() -> tokio::time::Instant {
        tokio::time::Instant::now() + Duration::from_secs(10)
    }

    #[tokio::test]
    async fn child_environment_is_minimal() {
        std::env::set_var("AGENT_BROWSER_PROVIDER", "browserbase");
        std::env::set_var("HTTPS_PROXY", "http://proxy.invalid:1");
        let tmp = tempfile::tempdir().unwrap();
        let runner = fake_runner(
            tmp.path(),
            r#"env > env.txt; pwd -P > pwd.txt; printf '%s\n' "$@" > args.txt; echo '{"success":true}'"#,
        );
        let id = SessionId("s_abcdefgh".into());
        let ep = endpoint();
        let argv = vec!["get".to_string(), "url".to_string()];
        let r = runner
            .run(&id, Some(&ep), &argv, deadline(), None)
            .await
            .unwrap();
        std::env::remove_var("AGENT_BROWSER_PROVIDER");
        std::env::remove_var("HTTPS_PROXY");
        assert_eq!(r.output, json!({ "success": true }));
        let files = runner.paths.session_files(&id);
        let read = |f: &str| std::fs::read_to_string(files.join(f)).unwrap();
        let mut env: Vec<String> = read("env.txt")
            .lines()
            .map(|l| {
                l.split_once('=')
                    .map(|(k, _)| k.to_string())
                    .unwrap_or_default()
            })
            .collect();
        env.sort();
        env.retain(|k| k != "PWD" && k != "SHLVL" && k != "_" && k != "OLDPWD");
        assert_eq!(
            env,
            [
                "AGENT_BROWSER_CDP_HEADERS",
                "AGENT_BROWSER_CONFIG",
                "AGENT_BROWSER_CONTENT_BOUNDARIES",
                "AGENT_BROWSER_NO_VERSION_CHECK",
                "AGENT_BROWSER_SOCKET_DIR",
                "HOME",
                "LANG",
                "LC_ALL",
                "PATH",
                "TMPDIR",
            ]
        );
        let all = read("env.txt");
        assert!(
            all.contains(&format!("HOME={}", runner.paths.ab_home.display())),
            "{all}"
        );
        assert!(all.contains("AGENT_BROWSER_CONTENT_BOUNDARIES=1"));
        assert_eq!(
            PathBuf::from(read("pwd.txt").trim()),
            files.canonicalize().unwrap()
        );
        let args = read("args.txt");
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "--session",
                "peg-s_abcdefgh",
                "--cdp",
                &ep.url,
                "--json",
                "get",
                "url"
            ]
        );
        assert!(!args.contains(&ep.token));
    }

    #[tokio::test]
    async fn token_is_scrubbed_from_returned_text() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = fake_runner(
            tmp.path(),
            r#"T=$(echo "$AGENT_BROWSER_CDP_HEADERS" | sed 's/.*Bearer //; s/".*//'); echo "$AGENT_BROWSER_CDP_HEADERS" >&2; printf '{"success":false,"error":"bad %s"}\n' "$T""#,
        );
        let id = SessionId("s_abcdefgh".into());
        let ep = endpoint();
        let argv = vec!["get".to_string(), "url".to_string()];
        let r = runner
            .run(&id, Some(&ep), &argv, deadline(), None)
            .await
            .unwrap();
        assert_eq!(r.output["error"], json!("bad [redacted]"));
        assert!(r.stderr.contains("Bearer [redacted]"), "{}", r.stderr);
        assert!(!r.stderr.contains(&ep.token));
    }

    #[test]
    fn tab_gone_only_when_the_bound_tab_left_the_session() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = fake_runner(tmp.path(), "exit 0");
        let id = SessionId(format!("s_{}", "tabgonet"));
        crate::paths::ensure_socket_dir(&runner.paths.ab_socket_dir).unwrap();
        let target_file = runner.daemon_file(&id, "target");
        std::fs::write(
            &target_file,
            r#"{"targetId":"T1","url":"x","pinned":false}"#,
        )
        .unwrap();
        let failed = result(
            1,
            json!({ "success": false, "error": "No target with given id found" }),
        );
        let pinned = result(1, json!({ "success": false, "code": "tab_gone" }));
        // Without the session's targets only agent-browser's own code counts.
        assert!(!runner.tab_gone(&id, &failed));
        assert!(runner.tab_gone(&id, &pinned));
        let live = Arc::new(Mutex::new(vec!["T1".to_string(), "T2".to_string()]));
        let seen = live.clone();
        runner.set_session_targets(move |_| seen.lock().unwrap().clone());
        // The bound tab is still there: e.g. `tab t9` failed. An ordinary failure.
        assert!(!runner.tab_gone(&id, &failed));
        assert!(!runner.tab_gone(&id, &result(0, json!({ "success": true }))));
        *live.lock().unwrap() = vec!["T2".to_string()];
        assert!(runner.tab_gone(&id, &failed));
        live.lock().unwrap().clear();
        assert!(runner.tab_gone(&id, &failed));
        let _ = std::fs::remove_file(target_file);
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
