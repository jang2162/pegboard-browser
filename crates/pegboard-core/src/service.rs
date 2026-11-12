//! The long-running daemon: owns the browser, the proxy, the session manager and the
//! runner, and answers the core API methods (see `api` for the socket).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::{anyhow, Context};
use pegboard_protocol::{
    codes, BrowserStatus, CommandExecParams, CommandExecResult, Request, Response, RpcError,
    SessionCloseParams, SessionCreateParams, SessionId, SessionInfo, JSONRPC,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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

pub struct Daemon {
    paths: Paths,
    browser: Arc<Browser>,
    sessions: SessionManager,
    runner: Arc<Runner>,
    /// Kept alive for the daemon's lifetime (it serves the session endpoints).
    _proxy: Arc<ProxyServer>,
    chrome_version: String,
    logs: Mutex<HashMap<SessionId, SessionLog>>,
    /// Set once shutdown begins; new sessions and commands are refused from then on.
    stopping: AtomicBool,
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
        if let Some(dir) = std::env::current_exe().ok().as_deref().and_then(|p| p.parent()) {
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
    let value = if value.is_null() { json!({}) } else { value.clone() };
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
        let runner = Arc::new(
            Runner::new(paths.clone()).map_err(|e| anyhow!("{}: {}", e.code, e.message))?,
        );
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
        let sessions = SessionManager::new(browser.clone(), proxy.clone(), profiles, paths.clone(), cfg);
        // Once, here: it is what lets the runner tell `tab_gone` from other failures.
        runner.set_session_targets_provider(sessions.targets_fn());
        let (stop, _) = watch::channel(false);
        let daemon = Arc::new_cyclic(|me| Daemon {
            paths,
            browser,
            sessions,
            runner,
            _proxy: proxy,
            chrome_version,
            logs: Mutex::new(HashMap::new()),
            stopping: AtomicBool::new(false),
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
        Ok(daemon)
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
                to_value(self.sessions.info(&checked_session(&p.session_id)?)?)
            }
            "session.close" => {
                let p: SessionCloseParams = params(p)?;
                to_value(self.close_session(p).await?)
            }
            "command.exec" => to_value(self.exec(params(p)?).await?),
            "recording.list" => to_value(actionlog::list(&self.paths).map_err(|e| internal(e.to_string()))?),
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
        self.logs.lock().unwrap().insert(id, log);
        Ok(info)
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
    fn remove_files(&self, id: &SessionId) {
        let files = self.paths.session_files(id);
        let _ = std::fs::remove_dir_all(&files);
        if let Some(dir) = files.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }

    async fn close_session(&self, p: SessionCloseParams) -> Result<SessionInfo, RpcError> {
        let id = checked_session(&p.session_id)?;
        let p = SessionCloseParams {
            session_id: id.clone(),
            ..p
        };
        let me = self.me()?;
        // The session leaves the table first, so a second close finds nothing.
        let log = self.logs.lock().unwrap().remove(&id);
        let Some(log) = log else {
            return Err(rpc_error(
                codes::SESSION_NOT_FOUND,
                format!("no session {}", id.0),
            ));
        };
        tokio::spawn(async move { me.finish_session(p, log, true).await })
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
        let log = self.logs.lock().unwrap().get(&id).cloned();
        let Some(log) = log else {
            return Err(rpc_error(
                codes::SESSION_NOT_FOUND,
                format!("no session {}", id.0),
            ));
        };
        // One command at a time per session; other sessions are not held up.
        let mut log = log.lock().await;
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
        let logs: Vec<(SessionId, SessionLog)> = self.logs.lock().unwrap().drain().collect();
        let browser_alive = !self.browser.has_exited();
        for (id, log) in logs {
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
