//! Per-session CDP endpoint: a WebSocket server that puts `filter` on the wire.
//!
//! An unmodified CDP client connects to `ws://127.0.0.1:<port>/s/<session id>` with
//! `Authorization: Bearer <token>` and believes it talks to the whole browser. Every
//! client connection gets its own upstream browser connection; every message in either
//! direction is parsed, judged by `filter` against the session's scope, and re-serialized.
//! The scope (targets and CDP sessions) is maintained here from what passes the filter.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use pegboard_protocol::SessionId;
use rand::RngCore;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::bridge::{Bridge, GroupId};
use crate::proxy::filter::{self, BrowserDecision, ClientDecision, Scope};

/// CDP screenshots and snapshots can be large.
const MAX_MESSAGE: usize = 256 << 20;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REQUEST_HEAD: usize = 16 * 1024;
/// Dropped target events kept while a tab creation is in flight (see `ClientConn::stash`).
const STASH_LIMIT: usize = 1024;

fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE))
}

/// Source of browser connections; one logical connection per client connection.
#[async_trait]
pub trait Upstream: Send + Sync {
    async fn open(&self) -> Result<Box<dyn UpstreamConn>>;
}

#[async_trait]
pub trait UpstreamConn: Send {
    async fn send(&mut self, text: String) -> Result<()>;
    /// The next text message; `None` once the connection is gone.
    async fn recv(&mut self) -> Option<String>;
}

/// Phase 1: a fresh WebSocket to the browser endpoint per client.
pub struct WsUpstream {
    pub browser_ws_url: String,
}

struct WsUpstreamConn {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

#[async_trait]
impl Upstream for WsUpstream {
    async fn open(&self) -> Result<Box<dyn UpstreamConn>> {
        let (ws, _) = tokio_tungstenite::connect_async_with_config(
            self.browser_ws_url.as_str(),
            Some(ws_config()),
            false,
        )
        .await
        .context("connecting to the browser")?;
        Ok(Box::new(WsUpstreamConn { ws }))
    }
}

#[async_trait]
impl UpstreamConn for WsUpstreamConn {
    async fn send(&mut self, text: String) -> Result<()> {
        self.ws.send(Message::text(text)).await?;
        Ok(())
    }

    async fn recv(&mut self) -> Option<String> {
        loop {
            match self.ws.next().await? {
                Ok(Message::Text(t)) => return Some(t.as_str().to_owned()),
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Endpoint {
    /// `ws://127.0.0.1:<port>/s/<session id>`
    pub url: String,
    pub token: String,
}

struct SessionEntry {
    token: String,
    group: GroupId,
    scope: Mutex<Scope>,
    /// Set to true when the session is unregistered; client connections then close.
    closed: watch::Sender<bool>,
}

struct Shared {
    upstream: Arc<dyn Upstream>,
    bridge: Arc<Bridge>,
    sessions: Mutex<HashMap<String, Arc<SessionEntry>>>,
}

/// One loopback listener serving every session's CDP endpoint.
pub struct ProxyServer {
    port: u16,
    shared: Arc<Shared>,
    accept: tokio::task::JoinHandle<()>,
}

impl ProxyServer {
    pub async fn start(upstream: Arc<dyn Upstream>, bridge: Arc<Bridge>) -> Result<ProxyServer> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("binding the session endpoint listener")?;
        let port = listener.local_addr()?.port();
        let shared = Arc::new(Shared {
            upstream,
            bridge,
            sessions: Mutex::new(HashMap::new()),
        });
        let accepting = shared.clone();
        let accept = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((sock, _)) => {
                        let shared = accepting.clone();
                        tokio::spawn(async move {
                            let _ = handle_socket(shared, sock).await;
                        });
                    }
                    // E.g. out of file descriptors; do not spin.
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
        });
        Ok(ProxyServer {
            port,
            shared,
            accept,
        })
    }

    /// Registers a session scope. Targets and CDP sessions are tracked inside the server from then on.
    /// Registering an id again replaces its scope and token and drops its old clients.
    pub fn register(
        &self,
        id: &SessionId,
        context: &str,
        group: GroupId,
        initial_targets: Vec<String>,
    ) -> Endpoint {
        let token = new_token();
        let entry = Arc::new(SessionEntry {
            token: token.clone(),
            group,
            scope: Mutex::new(Scope {
                context: context.to_string(),
                targets: initial_targets.into_iter().collect(),
                cdp_sessions: HashSet::new(),
            }),
            closed: watch::channel(false).0,
        });
        let old = self
            .shared
            .sessions
            .lock()
            .unwrap()
            .insert(id.0.clone(), entry);
        if let Some(old) = old {
            old.closed.send_replace(true);
        }
        Endpoint {
            url: format!("ws://127.0.0.1:{}/s/{}", self.port, id.0),
            token,
        }
    }

    /// Forgets the session and drops its client connections.
    pub fn unregister(&self, id: &SessionId) {
        let entry = self.shared.sessions.lock().unwrap().remove(&id.0);
        if let Some(entry) = entry {
            entry.closed.send_replace(true);
        }
    }

    /// The session's targets, sorted.
    pub fn targets(&self, id: &SessionId) -> Vec<String> {
        let entry = self.shared.sessions.lock().unwrap().get(&id.0).cloned();
        let mut targets: Vec<String> = entry
            .map(|e| e.scope.lock().unwrap().targets.iter().cloned().collect())
            .unwrap_or_default();
        targets.sort();
        targets
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for ProxyServer {
    fn drop(&mut self) {
        self.accept.abort();
        let sessions: Vec<_> = self.shared.sessions.lock().unwrap().drain().collect();
        for (_, entry) in sessions {
            entry.closed.send_replace(true);
        }
    }
}

/// 32 bytes from the OS RNG, hex encoded.
fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// Handshake

struct RequestHead {
    method: String,
    path: String,
    /// Lower-cased names.
    headers: Vec<(String, String)>,
}

impl RequestHead {
    /// The header's value when it occurs exactly once.
    fn single(&self, name: &str) -> Option<&str> {
        let mut found = self.headers.iter().filter(|(n, _)| n == name);
        let first = found.next()?;
        found.next().is_none().then_some(first.1.as_str())
    }

    fn has_token(&self, name: &str, token: &str) -> bool {
        self.headers
            .iter()
            .filter(|(n, _)| n == name)
            .flat_map(|(_, v)| v.split(','))
            .any(|t| t.trim().eq_ignore_ascii_case(token))
    }
}

/// Reads the request head; returns it and any bytes already read past it.
async fn read_head(sock: &mut TcpStream) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            bail!("connection closed before the request head ended");
        }
        let from = buf.len().saturating_sub(3);
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf[from..].windows(4).position(|w| w == b"\r\n\r\n") {
            let end = from + pos + 4;
            let rest = buf.split_off(end);
            return Ok((buf, rest));
        }
        if buf.len() > MAX_REQUEST_HEAD {
            bail!("request head too large");
        }
    }
}

fn parse_head(bytes: &[u8]) -> Option<RequestHead> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);
    match req.parse(bytes) {
        Ok(httparse::Status::Complete(_)) => {}
        _ => return None,
    }
    let headers = req
        .headers
        .iter()
        .map(|h| {
            Some((
                h.name.to_ascii_lowercase(),
                std::str::from_utf8(h.value).ok()?.to_string(),
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(RequestHead {
        method: req.method?.to_string(),
        path: req.path?.to_string(),
        headers,
    })
}

async fn respond_status(sock: &mut TcpStream, status: &str) {
    let resp = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let _ = sock.write_all(resp.as_bytes()).await;
    let _ = sock.shutdown().await;
}

/// The session the request may connect to, or the HTTP status to refuse it with.
fn authorize(shared: &Shared, head: &RequestHead) -> Result<Arc<SessionEntry>, &'static str> {
    if !head.has_token("upgrade", "websocket") {
        return Err("404 Not Found");
    }
    const UNAUTHORIZED: &str = "401 Unauthorized";
    let id = head.path.strip_prefix("/s/").ok_or(UNAUTHORIZED)?;
    let entry = shared
        .sessions
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or(UNAUTHORIZED)?;
    let presented = head
        .single("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(UNAUTHORIZED)?;
    if !bool::from(presented.as_bytes().ct_eq(entry.token.as_bytes())) {
        return Err(UNAUTHORIZED);
    }
    if head.method != "GET"
        || !head.has_token("connection", "upgrade")
        || head.single("sec-websocket-version") != Some("13")
        || head.single("sec-websocket-key").is_none()
    {
        return Err("400 Bad Request");
    }
    Ok(entry)
}

async fn handle_socket(shared: Arc<Shared>, mut sock: TcpStream) -> Result<()> {
    let (head, rest) = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_head(&mut sock))
        .await
        .context("handshake timed out")??;
    let Some(head) = parse_head(&head) else {
        respond_status(&mut sock, "400 Bad Request").await;
        return Ok(());
    };
    let entry = match authorize(&shared, &head) {
        Ok(entry) => entry,
        Err(status) => {
            respond_status(&mut sock, status).await;
            return Ok(());
        }
    };
    let mut closed = entry.closed.subscribe();
    if *closed.borrow_and_update() {
        respond_status(&mut sock, "401 Unauthorized").await;
        return Ok(());
    }
    // Open the browser side before accepting, so a dead browser is a failed handshake.
    let up = match shared.upstream.open().await {
        Ok(up) => up,
        Err(_) => {
            respond_status(&mut sock, "502 Bad Gateway").await;
            return Ok(());
        }
    };
    let key = head.single("sec-websocket-key").unwrap_or_default();
    let accept = derive_accept_key(key.as_bytes());
    let resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    sock.write_all(resp.as_bytes()).await?;
    let client =
        WebSocketStream::from_partially_read(sock, rest, Role::Server, Some(ws_config())).await;
    let (tab_tx, tab_rx) = mpsc::unbounded_channel();
    let conn = ClientConn {
        bridge: shared.bridge.clone(),
        entry,
        client,
        up,
        pending: HashMap::new(),
        next_upstream_id: 1,
        tabs_in_flight: 0,
        stash: VecDeque::new(),
        tab_tx,
        tab_rx,
    };
    conn.run(closed).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// One client connection

/// What the client needs to recognize a reply: its own request id and session.
struct ClientRef {
    id: Value,
    session_id: Option<Value>,
}

enum Pending {
    /// A client request, forwarded under a proxy-assigned id.
    Client { client: ClientRef, method: String },
    /// Context check of a tab the bridge created for a client `Target.createTarget`.
    VerifyTab {
        client: ClientRef,
        target_id: String,
    },
    /// The proxy's own request whose reply nobody needs.
    Internal,
}

struct TabCreated {
    client: ClientRef,
    result: Result<String>,
}

struct ClientConn {
    bridge: Arc<Bridge>,
    entry: Arc<SessionEntry>,
    client: WebSocketStream<TcpStream>,
    up: Box<dyn UpstreamConn>,
    /// Upstream request id -> who asked. Every request upstream carries a proxy-assigned
    /// id, so client ids can neither collide with nor answer the proxy's own requests.
    pending: HashMap<u64, Pending>,
    next_upstream_id: u64,
    tabs_in_flight: usize,
    /// Top-level target events dropped while a tab creation is in flight. Chrome can
    /// report the new tab before the bridge returns its id; once the tab is in scope these
    /// are judged again so the client still sees its own tab appear.
    stash: VecDeque<Value>,
    tab_tx: mpsc::UnboundedSender<TabCreated>,
    tab_rx: mpsc::UnboundedReceiver<TabCreated>,
}

const TARGET_EVENTS: &[&str] = &[
    "Target.targetCreated",
    "Target.targetInfoChanged",
    "Target.targetDestroyed",
    "Target.attachedToTarget",
    "Target.detachedFromTarget",
    "Target.targetCrashed",
];

fn reply(client: &ClientRef, body: Result<Value, (i64, String)>) -> Value {
    let mut v = match body {
        Ok(result) => json!({ "id": client.id, "result": result }),
        Err((code, message)) => {
            json!({ "id": client.id, "error": { "code": code, "message": message } })
        }
    };
    if let Some(s) = &client.session_id {
        v["sessionId"] = s.clone();
    }
    v
}

impl ClientConn {
    async fn run(mut self, mut closed: watch::Receiver<bool>) {
        loop {
            let step = tokio::select! {
                _ = async { let _ = closed.wait_for(|c| *c).await; } => break,
                m = self.client.next() => match m {
                    Some(Ok(Message::Text(t))) => self.on_client(t.as_str()).await,
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => Ok(()),
                },
                m = self.up.recv() => match m {
                    Some(t) => self.on_browser(&t).await,
                    None => break,
                },
                Some(done) = self.tab_rx.recv() => self.on_tab_created(done).await,
            };
            if step.is_err() {
                break;
            }
        }
        let _ = self.client.close(None).await;
    }

    async fn send_client(&mut self, v: &Value) -> Result<()> {
        self.client.send(Message::text(v.to_string())).await?;
        Ok(())
    }

    async fn send_upstream(&mut self, mut v: Value, pending: Pending) -> Result<()> {
        let id = self.next_upstream_id;
        self.next_upstream_id += 1;
        v["id"] = json!(id);
        self.pending.insert(id, pending);
        self.up.send(v.to_string()).await
    }

    async fn on_client(&mut self, text: &str) -> Result<()> {
        let Ok(msg) = serde_json::from_str::<Value>(text) else {
            let err =
                json!({ "error": { "code": -32700, "message": "Message must be valid JSON" } });
            return self.send_client(&err).await;
        };
        let client = ClientRef {
            id: msg.get("id").cloned().unwrap_or(Value::Null),
            session_id: msg.get("sessionId").filter(|s| s.is_string()).cloned(),
        };
        if !(client.id.is_u64() || client.id.is_i64()) {
            let err = (
                -32600,
                "Message must have integer 'id' property".to_string(),
            );
            return self.send_client(&reply(&client, Err(err))).await;
        }
        let decision = filter::client_message(&msg, &self.entry.scope.lock().unwrap());
        // The browser receives the value the decision was made on, re-serialized; never the
        // client's original bytes.
        let forward = match decision {
            ClientDecision::Forward => msg,
            ClientDecision::ForwardRewritten(v) => v,
            ClientDecision::Reply(result) => {
                return self.send_client(&reply(&client, Ok(result))).await
            }
            ClientDecision::Error { code, message } => {
                return self
                    .send_client(&reply(&client, Err((code, message))))
                    .await;
            }
            ClientDecision::CreateTab { url } => {
                self.tabs_in_flight += 1;
                let bridge = self.bridge.clone();
                let group = self.entry.group.clone();
                let tx = self.tab_tx.clone();
                tokio::spawn(async move {
                    let result = bridge.create_tab(&group, &url).await;
                    let _ = tx.send(TabCreated { client, result });
                });
                return Ok(());
            }
        };
        let method = forward["method"].as_str().unwrap_or_default().to_string();
        self.send_upstream(forward, Pending::Client { client, method })
            .await
    }

    async fn on_browser(&mut self, text: &str) -> Result<()> {
        // Default-deny: what does not parse is not forwarded.
        let Ok(msg) = serde_json::from_str::<Value>(text) else {
            return Ok(());
        };
        let Some(id) = msg.get("id") else {
            return self.on_browser_event(msg).await;
        };
        // A reply to nobody's request is dropped.
        let Some(pending) = id.as_u64().and_then(|id| self.pending.remove(&id)) else {
            return Ok(());
        };
        match pending {
            Pending::Client { client, method } => {
                self.on_client_response(msg, client, &method).await
            }
            Pending::VerifyTab { client, target_id } => {
                self.finish_tab(&msg, client, target_id).await
            }
            Pending::Internal => Ok(()),
        }
    }

    async fn on_client_response(
        &mut self,
        mut msg: Value,
        client: ClientRef,
        method: &str,
    ) -> Result<()> {
        msg["id"] = client.id.clone();
        let decision = {
            let mut scope = self.entry.scope.lock().unwrap();
            adopt_listed_popups(&msg, &mut scope);
            let decision = filter::browser_message(msg, Some(method), &scope);
            if let BrowserDecision::Forward(v) = &decision {
                if method == "Target.attachToTarget" {
                    if let Some(s) = v.pointer("/result/sessionId").and_then(Value::as_str) {
                        scope.cdp_sessions.insert(s.to_string());
                    }
                }
            }
            decision
        };
        match decision {
            BrowserDecision::Forward(v) | BrowserDecision::Adopt { forward: v, .. } => {
                self.send_client(&v).await
            }
            // The session left the scope while the request was in flight; still answer.
            BrowserDecision::Drop => {
                let err = (-32001, "Session with given id not found.".to_string());
                self.send_client(&reply(&client, Err(err))).await
            }
        }
    }

    async fn on_browser_event(&mut self, msg: Value) -> Result<()> {
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let stashable = self.tabs_in_flight > 0
            && msg.get("sessionId").is_none()
            && TARGET_EVENTS.contains(&method);
        let copy = stashable.then(|| msg.clone());
        let auto_attached = auto_attached_session(&msg);
        let decision = {
            let mut scope = self.entry.scope.lock().unwrap();
            let decision = filter::browser_message(msg, None, &scope);
            match &decision {
                BrowserDecision::Forward(v) => track_event(v, &mut scope),
                BrowserDecision::Adopt { target_id, forward } => {
                    scope.targets.insert(target_id.clone());
                    track_event(forward, &mut scope);
                }
                BrowserDecision::Drop => {}
            }
            decision
        };
        match decision {
            BrowserDecision::Forward(v) | BrowserDecision::Adopt { forward: v, .. } => {
                self.send_client(&v).await
            }
            BrowserDecision::Drop => {
                if let Some(copy) = copy {
                    if self.stash.len() == STASH_LIMIT {
                        if let Some(oldest) = self.stash.pop_front() {
                            self.release(&oldest).await?;
                        }
                    }
                    self.stash.push_back(copy);
                    Ok(())
                } else if let Some(session_id) = auto_attached {
                    self.release_session(session_id).await
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Browser-level auto-attach attaches this connection to every new target, including
    /// the human's. Such a session is never shown to the client, so the proxy releases it
    /// itself: resume, then detach. Chrome 157 holds a human page that calls `window.open`
    /// until the popup's auto-attached session is resumed, even with
    /// `waitForDebuggerOnStart: false` and even after a detach without a resume.
    async fn release_session(&mut self, session_id: String) -> Result<()> {
        let resume =
            json!({ "method": "Runtime.runIfWaitingForDebugger", "sessionId": session_id });
        self.send_upstream(resume, Pending::Internal).await?;
        let detach =
            json!({ "method": "Target.detachFromTarget", "params": { "sessionId": session_id } });
        self.send_upstream(detach, Pending::Internal).await
    }

    /// A stashed event that will not be shown after all.
    async fn release(&mut self, event: &Value) -> Result<()> {
        match auto_attached_session(event) {
            Some(session_id) => self.release_session(session_id).await,
            None => Ok(()),
        }
    }

    async fn forget_stash_if_idle(&mut self) -> Result<()> {
        if self.tabs_in_flight == 0 {
            let stashed: Vec<Value> = self.stash.drain(..).collect();
            for event in &stashed {
                self.release(event).await?;
            }
        }
        Ok(())
    }

    async fn on_tab_created(&mut self, done: TabCreated) -> Result<()> {
        match done.result {
            Ok(target_id) => {
                let check = json!({ "method": "Target.getTargetInfo", "params": { "targetId": target_id } });
                let pending = Pending::VerifyTab {
                    client: done.client,
                    target_id,
                };
                self.send_upstream(check, pending).await
            }
            Err(e) => {
                self.tabs_in_flight -= 1;
                self.forget_stash_if_idle().await?;
                let err = (-32000, format!("creating the tab failed: {e:#}"));
                self.send_client(&reply(&done.client, Err(err))).await
            }
        }
    }

    /// The bridge created `target_id`; it joins the scope only if it lives in the session's context.
    async fn finish_tab(
        &mut self,
        info: &Value,
        client: ClientRef,
        target_id: String,
    ) -> Result<()> {
        let context = info
            .pointer("/result/targetInfo/browserContextId")
            .and_then(Value::as_str);
        let in_context = {
            let mut scope = self.entry.scope.lock().unwrap();
            let ok = context == Some(scope.context.as_str());
            if ok {
                scope.targets.insert(target_id.clone());
            }
            ok
        };
        self.tabs_in_flight -= 1;
        if !in_context {
            self.forget_stash_if_idle().await?;
            let close =
                json!({ "method": "Target.closeTarget", "params": { "targetId": target_id } });
            self.send_upstream(close, Pending::Internal).await?;
            let err = (
                -32000,
                "the new tab was not created in the session's profile".to_string(),
            );
            return self.send_client(&reply(&client, Err(err))).await;
        }
        // Judge the stashed events again now that the tab is in scope, in arrival order,
        // before the reply (the order Chrome itself uses). Events still dropped are stashed
        // again only while another tab creation is in flight.
        let stashed: Vec<Value> = self.stash.drain(..).collect();
        for event in stashed {
            self.on_browser_event(event).await?;
        }
        self.send_client(&reply(&client, Ok(json!({ "targetId": target_id }))))
            .await
    }
}

/// The session of a top-level `Target.attachedToTarget` (browser-level auto-attach).
fn auto_attached_session(msg: &Value) -> Option<String> {
    if msg.get("sessionId").is_some() || msg["method"] != "Target.attachedToTarget" {
        return None;
    }
    msg.pointer("/params/sessionId")
        .and_then(Value::as_str)
        .map(String::from)
}

/// Scope bookkeeping for an event that passed the filter.
fn track_event(msg: &Value, scope: &mut Scope) {
    let params = msg.get("params").unwrap_or(&Value::Null);
    let str_param = |k: &str| params.get(k).and_then(Value::as_str).map(String::from);
    match msg.get("method").and_then(Value::as_str) {
        Some("Target.attachedToTarget") => {
            if let Some(s) = str_param("sessionId") {
                scope.cdp_sessions.insert(s);
            }
        }
        Some("Target.detachedFromTarget") => {
            if let Some(s) = str_param("sessionId") {
                scope.cdp_sessions.remove(&s);
            }
        }
        Some("Target.targetDestroyed") => {
            if let Some(t) = str_param("targetId") {
                scope.targets.remove(&t);
            }
        }
        _ => {}
    }
}

/// A `targetInfos` listing can show a popup of an own tab before (or without) a
/// `Target.targetCreated` event on this connection, e.g. when the client never enabled
/// target discovery. Such popups are adopted by the same rule as the event: the opener is
/// in scope and the popup lives in the session's context.
fn adopt_listed_popups(msg: &Value, scope: &mut Scope) {
    let Some(infos) = msg.pointer("/result/targetInfos").and_then(Value::as_array) else {
        return;
    };
    loop {
        let mut added = false;
        for info in infos {
            let s = |k: &str| info.get(k).and_then(Value::as_str);
            if let (Some(id), Some(opener), Some(context)) =
                (s("targetId"), s("openerId"), s("browserContextId"))
            {
                if context == scope.context
                    && scope.targets.contains(opener)
                    && !scope.targets.contains(id)
                {
                    scope.targets.insert(id.to_string());
                    added = true;
                }
            }
        }
        // A popup of a popup is adopted once its opener is.
        if !added {
            break;
        }
    }
}
