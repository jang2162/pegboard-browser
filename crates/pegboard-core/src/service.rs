//! The long-running daemon: owns the browser, the proxy, the session manager and the
//! runner, and answers the core API methods (see `api` for the socket).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use pegboard_protocol::{
    codes, BrowserStatus, CommandExecParams, CommandExecResult, Request, Response, RpcError,
    SessionCloseParams, SessionCreateParams, SessionId, SessionInfo, JSONRPC,
};
use rand::RngCore;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::sync::{watch, Mutex as AsyncMutex};

use crate::actionlog::{self, valid_recording_id, ActionLog, Manifest};
use crate::browser::{find_chrome, Browser, BrowserConfig};
use crate::daemon::run_command;
use crate::paths::{ensure, Paths};
use crate::profiles::{rpc_error, Profiles};
use crate::proxy::server::{ProxyServer, WsUpstream};
use crate::runner::{Runner, AGENT_BROWSER_VERSION};
use crate::session::SessionManager;

/// Time one session may take to close while the daemon shuts down.
const SESSION_CLOSE_LIMIT: Duration = Duration::from_secs(15);

/// A session's action log. Holding the lock is what serializes the session's commands.
type SessionLog = Arc<AsyncMutex<ActionLog>>;

/// What the daemon keeps per open session.
struct Slot {
    log: SessionLog,
    /// Authorizes commands in the session. In memory only: never on disk or in a log.
    token: String,
    /// A close is running.
    closing: AtomicBool,
    /// Commands accepted and not finished yet (running, or waiting for the log).
    busy: AtomicUsize,
    /// When the session was created or its last command ended.
    last_end: Mutex<Instant>,
}

/// How long a session may sit without a command before it is closed.
const DEFAULT_IDLE: Duration = Duration::from_secs(3600);

/// The idle period: `PEGBOARD_TEST_IDLE_SECS` (a test hook, read once when the daemon
/// starts, so only whoever starts the daemon can set it) or an hour.
fn idle_period(var: Option<&str>) -> Duration {
    match var.and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(secs) if secs > 0 => Duration::from_secs(secs),
        _ => DEFAULT_IDLE,
    }
}

impl Slot {
    fn new(log: SessionLog, token: String) -> Slot {
        Slot {
            log,
            token,
            closing: AtomicBool::new(false),
            busy: AtomicUsize::new(0),
            last_end: Mutex::new(Instant::now()),
        }
    }

    /// No command is running or waiting, and the last one ended at least `period` ago.
    fn idle_for(&self, period: Duration) -> bool {
        self.busy.load(Ordering::SeqCst) == 0 && self.last_end.lock().unwrap().elapsed() >= period
    }
}

/// A command in flight; the session's idle time counts from when it is dropped.
struct Active<'a>(&'a Slot);

impl<'a> Active<'a> {
    fn begin(slot: &'a Slot) -> Active<'a> {
        slot.busy.fetch_add(1, Ordering::SeqCst);
        Active(slot)
    }
}

impl Drop for Active<'_> {
    fn drop(&mut self) {
        *self.0.last_end.lock().unwrap() = Instant::now();
        self.0.busy.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct Daemon {
    paths: Paths,
    browser: Arc<Browser>,
    sessions: SessionManager,
    runner: Arc<Runner>,
    /// Kept alive for the daemon's lifetime (it serves the session endpoints).
    _proxy: Arc<ProxyServer>,
    chrome_version: String,
    slots: Mutex<HashMap<SessionId, Arc<Slot>>>,
    /// Set once shutdown begins; new sessions and commands are refused from then on.
    stopping: AtomicBool,
    /// Sessions without a command for this long are closed (unless their policy says
    /// `no_expire`).
    idle: Duration,
    shutdown_done: AsyncMutex<bool>,
    stop: watch::Sender<bool>,
    me: Weak<Daemon>,
}

fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn internal(message: impl Into<String>) -> RpcError {
    rpc_error(codes::INTERNAL, message)
}

/// Longest a command may be given.
const MAX_TIMEOUT_MS: u64 = 600_000;

/// `timeout_ms` of `command.exec`: when given, 1 to `MAX_TIMEOUT_MS`.
fn checked_timeout(timeout_ms: Option<u64>) -> Result<(), RpcError> {
    match timeout_ms {
        Some(ms) if ms == 0 || ms > MAX_TIMEOUT_MS => Err(invalid(format!(
            "timeoutMs must be between 1 and {MAX_TIMEOUT_MS}"
        ))),
        _ => Ok(()),
    }
}

/// Longest argv word, whole argv and word count `command.exec` accepts.
pub const MAX_ARGV_WORD_BYTES: usize = 64 * 1024;
pub const MAX_ARGV_TOTAL_BYTES: usize = 256 * 1024;
pub const MAX_ARGV_WORDS: usize = 256;

fn limit_exceeded(message: impl Into<String>) -> RpcError {
    rpc_error(codes::LIMIT_EXCEEDED, message)
}

/// The size of the command line of `command.exec`. The message never quotes a word.
fn checked_argv_size(argv: &[String]) -> Result<(), RpcError> {
    if argv.len() > MAX_ARGV_WORDS {
        return Err(limit_exceeded(format!(
            "a command line has at most {MAX_ARGV_WORDS} words"
        )));
    }
    if argv.iter().any(|w| w.len() > MAX_ARGV_WORD_BYTES) {
        return Err(limit_exceeded(format!(
            "a word of a command line has at most {MAX_ARGV_WORD_BYTES} bytes"
        )));
    }
    if argv.iter().map(String::len).sum::<usize>() > MAX_ARGV_TOTAL_BYTES {
        return Err(limit_exceeded(format!(
            "a command line has at most {MAX_ARGV_TOTAL_BYTES} bytes"
        )));
    }
    Ok(())
}

/// The session's log, locked for one command. Once the session's close has begun nothing
/// is logged any more, so nothing runs either.
async fn begin_command<'a>(
    slot: &'a Slot,
    id: &SessionId,
) -> Result<tokio::sync::MutexGuard<'a, ActionLog>, RpcError> {
    let log = slot.log.lock().await;
    // Checked after the lock: `finish_session` takes the same lock to finalize the recording
    // once `closing` is set, so a command that gets here first is logged before that.
    if slot.closing.load(Ordering::SeqCst) {
        return Err(no_session(id));
    }
    Ok(log)
}

fn invalid(message: impl Into<String>) -> RpcError {
    rpc_error(codes::INVALID_PARAMS, message)
}

/// `$PEGBOARD_EXTENSION`, else `extension/bridge` next to the executable or in the
/// repository this crate was built from.
pub fn find_extension() -> anyhow::Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(p) = std::env::var_os("PEGBOARD_EXTENSION") {
        candidates.push(PathBuf::from(p));
    } else {
        if let Some(dir) = std::env::current_exe()
            .ok()
            .as_deref()
            .and_then(|p| p.parent())
        {
            candidates.push(dir.join("extension/bridge"));
        }
        candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../extension/bridge"));
    }
    candidates
        .iter()
        .find(|p| p.join("manifest.json").is_file())
        .and_then(|p| p.canonicalize().ok())
        .ok_or_else(|| anyhow!("bridge extension not found (looked at {candidates:?})"))
}

/// Parses the params of a request; a missing `params` is treated as an empty object.
fn params<T: DeserializeOwned>(value: &Value) -> Result<T, RpcError> {
    let value = if value.is_null() {
        json!({})
    } else {
        value.clone()
    };
    serde_json::from_value(value).map_err(|e| invalid(format!("bad params: {e}")))
}

/// Session ids arrive through `Deserialize`, which does not check them, and become file and
/// socket names.
fn checked_session(id: &SessionId) -> Result<SessionId, RpcError> {
    SessionId::parse(&id.0).ok_or_else(|| invalid(format!("{:?} is not a session id", id.0)))
}

#[derive(Deserialize)]
struct NameParams {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionIdParams {
    session_id: SessionId,
    #[serde(default)]
    token: Option<String>,
}

/// Unknown fields are refused: a misspelt `keepTabs` would close the tabs.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CloseParams {
    session_id: SessionId,
    #[serde(default)]
    keep_tabs: bool,
    #[serde(default)]
    token: Option<String>,
}

/// 16 random bytes from the OS generator as 26 lowercase base32 characters.
fn new_token() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let mut out = String::with_capacity(26);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in bytes {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn no_session(id: &SessionId) -> RpcError {
    rpc_error(codes::SESSION_NOT_FOUND, format!("no session {}", id.0))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecordingParams {
    recording_id: String,
}

fn to_value<T: Serialize>(v: T) -> Result<Value, RpcError> {
    serde_json::to_value(v).map_err(|e| internal(e.to_string()))
}

impl Daemon {
    /// Starts the browser, the proxy and everything that depends on them.
    pub async fn start(paths: Paths) -> anyhow::Result<Arc<Daemon>> {
        ensure(&paths)?;
        // First, so a missing or wrong agent-browser fails before a window opens.
        let runner =
            Arc::new(Runner::new(paths.clone()).map_err(|e| anyhow!("{}: {}", e.code, e.message))?);
        runner.cleanup_leftovers();
        let cfg = BrowserConfig {
            chrome: find_chrome()?,
            extension_dir: find_extension()?,
        };
        let browser = Arc::new(Browser::launch(&paths, &cfg).await?);
        match Self::assemble(paths, runner, browser.clone(), cfg).await {
            Ok(daemon) => Ok(daemon),
            Err(e) => {
                browser.close().await;
                Err(e)
            }
        }
    }

    async fn assemble(
        paths: Paths,
        runner: Arc<Runner>,
        browser: Arc<Browser>,
        cfg: BrowserConfig,
    ) -> anyhow::Result<Arc<Daemon>> {
        let version = browser
            .conn
            .send("Browser.getVersion", json!({}), None)
            .await
            .context("asking the browser for its version")?;
        let chrome_version = version["product"].as_str().unwrap_or("unknown").to_string();
        let proxy = Arc::new(
            ProxyServer::start(Arc::new(WsUpstream {
                browser_ws_url: browser.ws_url.clone(),
            }))
            .await?,
        );
        let profiles = Profiles::load(&paths)?;
        let sessions =
            SessionManager::new(browser.clone(), proxy.clone(), profiles, paths.clone(), cfg);
        // Once, here: it is what lets the runner tell `tab_gone` from other failures.
        runner.set_session_targets_provider(sessions.targets_fn());
        let (stop, _) = watch::channel(false);
        let idle = idle_period(std::env::var("PEGBOARD_TEST_IDLE_SECS").ok().as_deref());
        let daemon = Arc::new_cyclic(|me| Daemon {
            paths,
            browser,
            sessions,
            runner,
            _proxy: proxy,
            chrome_version,
            slots: Mutex::new(HashMap::new()),
            stopping: AtomicBool::new(false),
            idle,
            shutdown_done: AsyncMutex::new(false),
            stop,
            me: me.clone(),
        });
        // The daemon ends with its browser.
        let weak = Arc::downgrade(&daemon);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let Some(d) = weak.upgrade() else { return };
                if d.browser.has_exited() {
                    d.request_stop();
                    return;
                }
            }
        });
        // Sessions nobody has used for the idle period are closed like `session close` does.
        let weak = Arc::downgrade(&daemon);
        let every = (idle / 4).clamp(Duration::from_millis(100), Duration::from_secs(30));
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let Some(d) = weak.upgrade() else { return };
                d.expire_idle().await;
            }
        });
        Ok(daemon)
    }

    /// Closes the sessions that have been idle for the idle period. A session with a
    /// command in flight is never closed (its idle time starts when the command ends).
    async fn expire_idle(&self) {
        if self.stopping.load(Ordering::SeqCst) {
            return;
        }
        let due: Vec<(SessionId, Arc<Slot>)> = self
            .slots
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, slot)| slot.idle_for(self.idle))
            .map(|(id, slot)| (id.clone(), slot.clone()))
            .collect();
        for (id, slot) in due {
            // Shutdown may have begun while an earlier session was closing.
            if self.stopping.load(Ordering::SeqCst) {
                return;
            }
            if matches!(self.sessions.policy(&id), Ok(p) if p.no_expire) {
                continue;
            }
            // Same path as `session close`. The log is what a command holds while it runs
            // and takes before it starts: with it in hand nothing is running, and `busy`
            // shows whether one is on its way to it. Only then is the session marked
            // closing, so a command is either seen here or refused as coming too late.
            let Ok(guard) = slot.log.try_lock() else {
                continue;
            };
            if !slot.idle_for(self.idle) || slot.closing.swap(true, Ordering::SeqCst) {
                continue;
            }
            drop(guard);
            if let Err(e) = self.close_slot(id.clone(), slot, false).await {
                eprintln!(
                    "pegboard: expiring session {}: {}: {}",
                    id.0, e.code, e.message
                );
            }
        }
    }

    /// Asks whoever runs the daemon to end it (`wait_stopped` then returns).
    pub fn request_stop(&self) {
        self.stop.send_replace(true);
    }

    /// Returns when the daemon should end: `browser.stop` was answered or the browser ended.
    pub async fn wait_stopped(&self) {
        let mut rx = self.stop.subscribe();
        let _ = rx.wait_for(|stopped| *stopped).await;
    }

    pub async fn handle(&self, req: Request) -> Response {
        let outcome = self.dispatch(&req.method, &req.params).await;
        let (result, error) = match outcome {
            Ok(v) => (Some(v), None),
            Err(e) => (None, Some(e)),
        };
        Response {
            jsonrpc: JSONRPC.to_string(),
            id: req.id,
            result,
            error,
        }
    }

    async fn dispatch(&self, method: &str, p: &Value) -> Result<Value, RpcError> {
        match method {
            "browser.status" => to_value(self.status()),
            "browser.stop" => {
                self.shutdown().await;
                Ok(json!({}))
            }
            "profile.list" => to_value(self.sessions.list_profiles().await),
            "profile.create" => {
                let p: NameParams = params(p)?;
                to_value(self.sessions.create_profile(&p.name).await?)
            }
            "profile.open" => {
                let p: NameParams = params(p)?;
                self.running()?;
                self.sessions.open_profile(&p.name).await?;
                Ok(json!({}))
            }
            "profile.delete" => {
                let p: NameParams = params(p)?;
                self.sessions.delete_profile(&p.name).await?;
                Ok(json!({}))
            }
            "session.create" => to_value(self.create_session(params(p)?).await?),
            "session.list" => to_value(self.sessions.list()),
            "session.info" => {
                let p: SessionIdParams = params(p)?;
                let id = checked_session(&p.session_id)?;
                self.authorize(&id, p.token.as_deref())?;
                to_value(self.sessions.info(&id)?)
            }
            "session.close" => {
                let p: CloseParams = params(p)?;
                to_value(self.close_session(p).await?)
            }
            "command.exec" => to_value(self.exec(params(p)?).await?),
            "recording.list" => {
                to_value(actionlog::list(&self.paths).map_err(|e| internal(e.to_string()))?)
            }
            "recording.get" => {
                let p: RecordingParams = params(p)?;
                self.recording(&p.recording_id)
            }
            other => Err(rpc_error(
                codes::UNKNOWN_METHOD,
                format!("unknown method {other:?}"),
            )),
        }
    }

    fn status(&self) -> BrowserStatus {
        BrowserStatus {
            running: !self.browser.has_exited(),
            pid: Some(self.browser.pid),
            chrome_version: Some(self.chrome_version.clone()),
            sessions: self.sessions.list().len(),
            stopping: self.stopping.load(Ordering::SeqCst),
        }
    }

    fn running(&self) -> Result<(), RpcError> {
        if self.stopping.load(Ordering::SeqCst) || self.browser.has_exited() {
            return Err(rpc_error(codes::NOT_RUNNING, "pegboard is shutting down"));
        }
        Ok(())
    }

    fn me(&self) -> Result<Arc<Daemon>, RpcError> {
        self.me.upgrade().ok_or_else(|| internal("daemon is gone"))
    }

    /// Runs in a task of its own: `SessionManager::create` cannot clean up when its future
    /// is dropped, so it must finish even if the requesting connection goes away.
    async fn create_session(&self, p: SessionCreateParams) -> Result<SessionInfo, RpcError> {
        self.running()?;
        let me = self.me()?;
        tokio::spawn(async move { me.create_session_task(p).await })
            .await
            .map_err(|e| internal(format!("session creation failed: {e}")))?
    }

    async fn create_session_task(&self, p: SessionCreateParams) -> Result<SessionInfo, RpcError> {
        let info = self.sessions.create(p).await?;
        let id = info.id.clone();
        let manifest = Manifest::new(
            &info.recording_id,
            &id.0,
            &info.name,
            &info.profile,
            &info.created_at,
            &self.chrome_version,
            AGENT_BROWSER_VERSION,
        );
        let log = match ActionLog::open(&self.paths, manifest) {
            Ok(log) => Arc::new(AsyncMutex::new(log)),
            Err(e) => {
                self.discard(&id, None, true).await;
                return Err(internal(format!("could not open the action log: {e}")));
            }
        };
        let attached = match self.sessions.endpoint(&id) {
            Ok(endpoint) => self.runner.attach(&id, &endpoint).await,
            Err(e) => Err(e),
        };
        if let Err(e) = attached {
            self.discard(&id, Some(log), true).await;
            return Err(e);
        }
        let token = new_token();
        let slot = Arc::new(Slot::new(log.clone(), token.clone()));
        // Checked under the lock `shutdown` drains with: a session is either drained by
        // the shutdown or refused here, never left behind.
        let refused = {
            let mut slots = self.slots.lock().unwrap();
            if self.stopping.load(Ordering::SeqCst) {
                true
            } else {
                slots.insert(id.clone(), slot);
                false
            }
        };
        if refused {
            self.discard(&id, Some(log), true).await;
            return Err(rpc_error(codes::NOT_RUNNING, "pegboard is shutting down"));
        }
        Ok(SessionInfo {
            token: Some(token),
            ..info
        })
    }

    /// Takes back a session whose creation failed after the session existed.
    async fn discard(&self, id: &SessionId, log: Option<SessionLog>, close_tabs: bool) {
        let _ = self.runner.detach(id).await;
        if close_tabs {
            let p = SessionCloseParams {
                session_id: id.clone(),
                keep_tabs: false,
            };
            let _ = self.sessions.close(p).await;
        }
        if let Some(log) = log {
            let _ = log.lock().await.finish(&now());
        }
        self.remove_files(id);
    }

    /// The session's files directory goes with the session; the recording stays.
    /// A session nobody was told about (nobody has its token) is closed again.
    async fn discard_undelivered(&self, id: &SessionId) {
        let slot = self.slots.lock().unwrap().remove(id);
        let (Some(slot), Ok(me)) = (slot, self.me()) else {
            return;
        };
        let p = SessionCloseParams {
            session_id: id.clone(),
            keep_tabs: false,
        };
        let _ =
            tokio::spawn(async move { me.finish_session(p, slot.log.clone(), true).await }).await;
    }

    fn remove_files(&self, id: &SessionId) {
        let files = self.paths.session_files(id);
        let _ = std::fs::remove_dir_all(&files);
        if let Some(dir) = files.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }

    /// The session's slot, if `token` is its token. An unknown id, a missing token and a
    /// wrong token are the same error, and the comparison takes the same time.
    fn authorize(&self, id: &SessionId, token: Option<&str>) -> Result<Arc<Slot>, RpcError> {
        let slot = self.slots.lock().unwrap().get(id).cloned();
        let expected = slot.as_ref().map_or("", |s| s.token.as_str());
        let same: bool = token
            .unwrap_or("")
            .as_bytes()
            .ct_eq(expected.as_bytes())
            .into();
        match slot {
            Some(slot) if same => Ok(slot),
            _ => Err(no_session(id)),
        }
    }

    async fn close_session(&self, p: CloseParams) -> Result<SessionInfo, RpcError> {
        let id = checked_session(&p.session_id)?;
        let slot = self.authorize(&id, p.token.as_deref())?;
        // One close at a time.
        if slot.closing.swap(true, Ordering::SeqCst) {
            return Err(no_session(&id));
        }
        self.close_slot(id, slot, p.keep_tabs).await
    }

    /// Closes a session whose slot has `closing` set by the caller.
    async fn close_slot(
        &self,
        id: SessionId,
        slot: Arc<Slot>,
        keep_tabs: bool,
    ) -> Result<SessionInfo, RpcError> {
        let p = SessionCloseParams {
            session_id: id.clone(),
            keep_tabs,
        };
        let me = self.me()?;
        let closed = tokio::spawn(async move {
            let result = me.finish_session(p, slot.log.clone(), true).await;
            // The session leaves the daemon's table once it is closed. If closing failed
            // but the session manager still has it, it stays so that it can be closed
            // again; otherwise it is gone (its recording is finalized) and so is the slot.
            if result.is_ok() || me.sessions.info(&id).is_err() {
                me.slots.lock().unwrap().remove(&id);
            } else {
                slot.closing.store(false, Ordering::SeqCst);
            }
            result
        });
        closed
            .await
            .map_err(|e| internal(format!("closing the session failed: {e}")))?
    }

    /// Stops the session's commands and agent-browser daemon, closes its tabs (unless the
    /// browser is gone), ends its recording and deletes its files.
    async fn finish_session(
        &self,
        p: SessionCloseParams,
        log: SessionLog,
        close_tabs: bool,
    ) -> Result<SessionInfo, RpcError> {
        let id = p.session_id.clone();
        if let Err(e) = self.runner.detach(&id).await {
            eprintln!("pegboard: {}: {}", e.code, e.message);
        }
        let result = if close_tabs {
            self.sessions.close(p).await
        } else {
            self.sessions.info(&id)
        };
        // Taken after `detach` stopped the running command, which releases the lock.
        let _ = log.lock().await.finish(&now());
        self.remove_files(&id);
        result
    }

    async fn exec(&self, p: CommandExecParams) -> Result<CommandExecResult, RpcError> {
        self.running()?;
        let id = checked_session(&p.session_id)?;
        let slot = self.authorize(&id, p.token.as_deref())?;
        checked_timeout(p.timeout_ms)?;
        checked_argv_size(&p.argv)?;
        // Counted from here, so the session is not closed for idleness under the command.
        let _active = Active::begin(&slot);
        // One command at a time per session; other sessions are not held up.
        let mut log = begin_command(&slot, &id).await?;
        let result = run_command(
            &self.sessions,
            &self.runner,
            &self.paths,
            &mut log,
            &id,
            &p.argv,
            p.timeout_ms,
        )
        .await;
        if matches!(&result, Err(e) if e.code == codes::TIMEOUT) {
            // The agent-browser daemon may still be busy with the command; the next one
            // starts a fresh daemon.
            self.runner.kill_daemon(&id);
        }
        result
    }

    fn recording(&self, recording_id: &str) -> Result<Value, RpcError> {
        if !valid_recording_id(recording_id) {
            return Err(invalid(format!("{recording_id:?} is not a recording id")));
        }
        match actionlog::read(&self.paths, recording_id) {
            Ok((manifest, actions)) => Ok(json!({ "manifest": manifest, "actions": actions })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(rpc_error(
                codes::RECORDING_NOT_FOUND,
                format!("no recording {recording_id}"),
            )),
            Err(e) => Err(internal(e.to_string())),
        }
    }

    /// Closes every session and the browser. Safe to call more than once.
    pub async fn shutdown(&self) {
        let mut done = self.shutdown_done.lock().await;
        if *done {
            return;
        }
        *done = true;
        self.stopping.store(true, Ordering::SeqCst);
        let slots: Vec<(SessionId, Arc<Slot>)> = self.slots.lock().unwrap().drain().collect();
        let browser_alive = !self.browser.has_exited();
        for (id, slot) in slots {
            let log = slot.log.clone();
            let p = SessionCloseParams {
                session_id: id,
                keep_tabs: false,
            };
            let finish = self.finish_session(p, log, browser_alive);
            let _ = tokio::time::timeout(SESSION_CLOSE_LIMIT, finish).await;
        }
        self.browser.close().await;
    }
}

#[async_trait::async_trait]
impl crate::api::Handler for Daemon {
    async fn handle(&self, req: Request) -> Response {
        Daemon::handle(self, req).await
    }

    fn request_stop(&self) {
        Daemon::request_stop(self)
    }

    async fn delivery_failed(&self, req: &Request, resp: &Response) {
        if req.method == "session.create" {
            if let Some(info) = resp
                .result
                .clone()
                .and_then(|r| serde_json::from_value::<SessionInfo>(r).ok())
            {
                self.discard_undelivered(&info.id).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn misspelt_fields_of_create_and_close_are_invalid_params() {
        let e = params::<SessionCreateParams>(&json!({"policy": {"denny": ["eval"]}}))
            .err()
            .unwrap();
        assert_eq!(e.code, codes::INVALID_PARAMS);
        let e = params::<SessionCreateParams>(&json!({"policy": {"allow_domains": ["a.test"]}}))
            .err()
            .unwrap();
        assert_eq!(e.code, codes::INVALID_PARAMS);
        let e = params::<CloseParams>(&json!({"sessionId": "s_abcdefgh", "keepTab": true}))
            .err()
            .unwrap();
        assert_eq!(e.code, codes::INVALID_PARAMS);
        assert!(
            params::<CloseParams>(&json!({"sessionId": "s_abcdefgh", "keepTabs": true})).is_ok()
        );
    }

    #[test]
    fn oversized_argv_is_refused() {
        let words = |n: usize, len: usize| vec!["a".repeat(len); n];
        assert!(checked_argv_size(&words(1, MAX_ARGV_WORD_BYTES)).is_ok());
        assert!(checked_argv_size(&words(MAX_ARGV_WORDS, 1)).is_ok());
        assert!(checked_argv_size(&words(4, MAX_ARGV_TOTAL_BYTES / 4)).is_ok());
        for bad in [
            words(1, MAX_ARGV_WORD_BYTES + 1),
            words(MAX_ARGV_WORDS + 1, 1),
            // Each word fits, the whole does not.
            words(5, MAX_ARGV_TOTAL_BYTES / 4),
            words(4, MAX_ARGV_TOTAL_BYTES / 4 + 1),
        ] {
            let e = checked_argv_size(&bad).unwrap_err();
            assert_eq!(e.code, codes::LIMIT_EXCEEDED);
            assert!(
                e.message.len() < 200,
                "the message does not quote the words"
            );
        }
    }

    #[test]
    fn idle_period_comes_from_the_test_variable_or_is_an_hour() {
        assert_eq!(idle_period(None), Duration::from_secs(3600));
        assert_eq!(idle_period(Some("2")), Duration::from_secs(2));
        for bad in ["", "0", "-1", "soon"] {
            assert_eq!(idle_period(Some(bad)), Duration::from_secs(3600), "{bad}");
        }
    }

    #[test]
    fn a_session_with_a_command_in_flight_is_not_idle() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = slot(tmp.path());
        let period = Duration::from_millis(50);
        assert!(!slot.idle_for(period), "just created");
        std::thread::sleep(Duration::from_millis(80));
        assert!(slot.idle_for(period));
        {
            let _active = Active::begin(&slot);
            std::thread::sleep(Duration::from_millis(80));
            assert!(!slot.idle_for(period), "a command is running");
        }
        // The idle time starts when the command ends.
        assert!(!slot.idle_for(period));
        std::thread::sleep(Duration::from_millis(80));
        assert!(slot.idle_for(period));
    }

    #[test]
    fn command_timeout_is_bounded() {
        assert!(checked_timeout(None).is_ok());
        assert!(checked_timeout(Some(1)).is_ok());
        assert!(checked_timeout(Some(600_000)).is_ok());
        for bad in [0, 600_001, u64::MAX] {
            let e = checked_timeout(Some(bad)).unwrap_err();
            assert_eq!(e.code, codes::INVALID_PARAMS, "{bad}");
        }
    }

    fn slot(dir: &std::path::Path) -> Slot {
        let paths = crate::paths::paths_in(dir);
        let manifest = Manifest::new(
            "r_abcdefgh",
            "s_abcdefgh",
            "demo",
            "default",
            "2026-01-01T00:00:00Z",
            "157",
            "0.42.0",
        );
        Slot::new(
            Arc::new(AsyncMutex::new(ActionLog::open(&paths, manifest).unwrap())),
            "t".into(),
        )
    }

    #[tokio::test]
    async fn no_command_starts_once_the_session_is_closing() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = slot(tmp.path());
        let id = SessionId("s_abcdefgh".into());
        assert!(begin_command(&slot, &id).await.is_ok());
        slot.closing.store(true, Ordering::SeqCst);
        let e = begin_command(&slot, &id).await.err().expect("refused");
        assert_eq!(e.code, codes::SESSION_NOT_FOUND);
        // A command that waited for the log while the close began is refused too.
        slot.closing.store(false, Ordering::SeqCst);
        let held = slot.log.lock().await;
        let waiting = begin_command(&slot, &id);
        tokio::pin!(waiting);
        assert!(futures_util::poll!(waiting.as_mut()).is_pending());
        slot.closing.store(true, Ordering::SeqCst);
        drop(held);
        assert_eq!(
            waiting.await.err().expect("refused").code,
            codes::SESSION_NOT_FOUND
        );
    }
}
