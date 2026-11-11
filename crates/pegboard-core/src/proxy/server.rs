//! Per-session CDP endpoint: a WebSocket server that puts `filter` on the wire.
//!
//! An unmodified CDP client connects to `ws://127.0.0.1:<port>/s/<session id>` with
//! `Authorization: Bearer <token>` and believes it talks to the whole browser. Every
//! client connection gets its own upstream browser connection; every message in either
//! direction is parsed, judged by `filter` against the session's scope, and re-serialized.
//! The scope (targets and CDP sessions) is maintained here from what passes the filter.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use pegboard_protocol::SessionId;
use rand::RngCore;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
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
const STASH_MAX_AGE: Duration = Duration::from_secs(10);
/// Bytes queued for a client that is not reading before it is disconnected.
const MAX_QUEUED_TO_CLIENT: usize = 64 << 20;
/// Bounds of the connection shutdown (see `ClientConn::shutdown`).
const SHUTDOWN_STEP: Duration = Duration::from_millis(500);
const SHUTDOWN_DRAIN: Duration = Duration::from_millis(500);
const CLIENT_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

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
    id: SessionId,
    token: String,
    /// Chrome deletes a group with its last tab; the session can then move to a new one.
    group: Mutex<GroupId>,
    /// Bridge of the profile the session lives in; it creates the session's tabs.
    bridge: Arc<Bridge>,
    scope: Mutex<Scope>,
    /// Set to true when the session is unregistered; client connections then close.
    closed: watch::Sender<bool>,
}

impl SessionEntry {
    fn new(
        id: SessionId,
        token: String,
        context: &str,
        group: GroupId,
        bridge: Arc<Bridge>,
        targets: Vec<String>,
    ) -> SessionEntry {
        SessionEntry {
            id,
            token,
            group: Mutex::new(group),
            bridge,
            scope: Mutex::new(Scope {
                context: context.to_string(),
                targets: targets.into_iter().collect(),
                cdp_sessions: HashSet::new(),
            }),
            closed: watch::channel(false).0,
        }
    }
}

type Sessions = Mutex<HashMap<String, Arc<SessionEntry>>>;

/// Called with the session and the target whenever a target enters a session scope.
pub type TargetAdded = Arc<dyn Fn(&SessionId, &str) + Send + Sync>;

struct Shared {
    upstream: Arc<dyn Upstream>,
    sessions: Sessions,
    on_target_added: Mutex<Option<TargetAdded>>,
    /// Client connections whose upstream is open.
    connections: Arc<AtomicUsize>,
    /// Test hook: delay before each bridge tab creation.
    tab_delay_ms: AtomicU64,
}

/// One loopback listener serving every session's CDP endpoint.
pub struct ProxyServer {
    port: u16,
    shared: Arc<Shared>,
    accept: tokio::task::JoinHandle<()>,
}

impl ProxyServer {
    pub async fn start(upstream: Arc<dyn Upstream>) -> Result<ProxyServer> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("binding the session endpoint listener")?;
        let port = listener.local_addr()?.port();
        let shared = Arc::new(Shared {
            upstream,
            sessions: Mutex::new(HashMap::new()),
            on_target_added: Mutex::new(None),
            connections: Arc::new(AtomicUsize::new(0)),
            tab_delay_ms: AtomicU64::new(0),
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

    /// Registers a session scope; `bridge` (the bridge of the profile `context` belongs
    /// to) creates its tabs. Targets and CDP sessions are tracked inside the server from
    /// then on. Registering an id again replaces its scope and token and drops its old
    /// clients.
    pub fn register(
        &self,
        id: &SessionId,
        context: &str,
        group: GroupId,
        bridge: Arc<Bridge>,
        initial_targets: Vec<String>,
    ) -> Endpoint {
        let token = new_token();
        let entry = Arc::new(SessionEntry::new(
            id.clone(),
            token.clone(),
            context,
            group,
            bridge,
            initial_targets,
        ));
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

    /// Puts a target into the session's scope (the caller checked that it belongs there).
    /// Returns false when the session is unknown. Calls the `on_target_added` callback
    /// when the target is new to the scope.
    pub fn add_target(&self, id: &SessionId, target_id: &str) -> bool {
        let Some(entry) = self.shared.entry(id) else {
            return false;
        };
        let added = entry
            .scope
            .lock()
            .unwrap()
            .targets
            .insert(target_id.to_string());
        if added {
            self.shared.target_added(&entry, target_id);
        }
        true
    }

    /// Takes a target out of the session's scope (e.g. it was destroyed).
    pub fn remove_target(&self, id: &SessionId, target_id: &str) {
        if let Some(entry) = self.shared.entry(id) {
            entry.scope.lock().unwrap().targets.remove(target_id);
        }
    }

    /// Moves the session to another tab group (its old group was deleted with its last tab).
    pub fn set_group(&self, id: &SessionId, group: GroupId) {
        if let Some(entry) = self.shared.entry(id) {
            *entry.group.lock().unwrap() = group;
        }
    }

    /// Sets the callback run whenever a target enters a session scope: a tab created
    /// through a client's `Target.createTarget`, an adopted popup, or `add_target`. It
    /// runs after the scope was updated and before the client hears of the target, on the
    /// connection's task, so it must not block.
    pub fn on_target_added(&self, f: impl Fn(&SessionId, &str) + Send + Sync + 'static) {
        *self.shared.on_target_added.lock().unwrap() = Some(Arc::new(f));
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Client connections (of all sessions) whose browser connection is still open.
    pub fn open_connections(&self) -> usize {
        self.shared.connections.load(Ordering::SeqCst)
    }

    /// Test hook: wait this long before each bridge tab creation of connections opened
    /// from now on, to make the "tab creation in flight" window deterministic.
    #[doc(hidden)]
    pub fn set_tab_creation_delay(&self, delay: Duration) {
        let ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
        self.shared.tab_delay_ms.store(ms, Ordering::Relaxed);
    }
}

impl Shared {
    fn entry(&self, id: &SessionId) -> Option<Arc<SessionEntry>> {
        self.sessions.lock().unwrap().get(&id.0).cloned()
    }

    /// Must be called without the scope lock held.
    fn target_added(&self, entry: &SessionEntry, target_id: &str) {
        let callback = self.on_target_added.lock().unwrap().clone();
        if let Some(f) = callback {
            f(&entry.id, target_id);
        }
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

/// What the handshake needs to know about a registered session.
trait Authorizable {
    fn token(&self) -> &str;
}

impl Authorizable for SessionEntry {
    fn token(&self) -> &str {
        &self.token
    }
}

/// The session the request may connect to, or the HTTP status to refuse it with.
fn authorize<E: Authorizable>(
    sessions: &Mutex<HashMap<String, Arc<E>>>,
    head: &RequestHead,
) -> Result<Arc<E>, &'static str> {
    if !head.has_token("upgrade", "websocket") {
        return Err("404 Not Found");
    }
    const UNAUTHORIZED: &str = "401 Unauthorized";
    let id = head.path.strip_prefix("/s/").ok_or(UNAUTHORIZED)?;
    let entry = sessions
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or(UNAUTHORIZED)?;
    let presented = head
        .single("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(UNAUTHORIZED)?;
    if !bool::from(presented.as_bytes().ct_eq(entry.token().as_bytes())) {
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
    let entry = match authorize(&shared.sessions, &head) {
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
    let open = OpenConnection::new(shared.connections.clone());
    let key = head.single("sec-websocket-key").unwrap_or_default();
    let accept = derive_accept_key(key.as_bytes());
    let resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    sock.write_all(resp.as_bytes()).await?;
    let client =
        WebSocketStream::from_partially_read(sock, rest, Role::Server, Some(ws_config())).await;
    let (sink, client_rx) = client.split();
    let (client_tx, writer) = ClientSender::spawn(sink);
    let (tab_tx, tab_rx) = mpsc::unbounded_channel();
    let conn = ClientConn {
        tab_delay: Duration::from_millis(shared.tab_delay_ms.load(Ordering::Relaxed)),
        shared: shared.clone(),
        entry,
        client_rx,
        client_tx,
        up,
        open,
        pending: HashMap::new(),
        next_upstream_id: 1,
        tabs_in_flight: 0,
        stash: VecDeque::new(),
        auto_sessions: HashMap::new(),
        recovered: HashSet::new(),
        tab_tx,
        tab_rx,
    };
    conn.run(closed, writer).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// One client connection

/// Counts a client connection from the moment its upstream is open until it is dropped.
struct OpenConnection(Arc<AtomicUsize>);

impl OpenConnection {
    fn new(count: Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        OpenConnection(count)
    }
}

impl Drop for OpenConnection {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Messages to the client go through a writer task, so a client that stops reading can
/// never stall the connection loop. The queue is bounded in bytes; a client that lets it
/// overflow is disconnected.
struct ClientSender {
    tx: mpsc::UnboundedSender<Message>,
    queued: Arc<AtomicUsize>,
}

impl ClientSender {
    fn spawn(
        mut sink: SplitSink<WebSocketStream<TcpStream>, Message>,
    ) -> (ClientSender, JoinHandle<()>) {
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let queued = Arc::new(AtomicUsize::new(0));
        let sent = queued.clone();
        let writer = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let len = msg.len();
                let ok = sink.send(msg).await.is_ok();
                sent.fetch_sub(len, Ordering::SeqCst);
                if !ok {
                    return;
                }
            }
            let _ = sink.close().await;
        });
        (ClientSender { tx, queued }, writer)
    }

    fn send(&self, v: &Value) -> Result<()> {
        let text = v.to_string();
        let len = text.len();
        let before = self.queued.fetch_add(len, Ordering::SeqCst);
        // A single message larger than the budget still goes out when the queue is empty.
        if before > 0 && before + len > MAX_QUEUED_TO_CLIENT {
            bail!("the client does not read its messages");
        }
        self.tx
            .send(Message::text(text))
            .map_err(|_| anyhow!("the client connection is closed"))
    }
}

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
    /// Resume of a hidden auto-attached session (see `ClientConn::release_session`).
    Resume { target_id: Option<String> },
    /// Re-attach of a target whose hidden session was detached before it was resumed.
    Recover,
}

struct TabCreated {
    client: ClientRef,
    result: Result<String>,
}

struct ClientConn {
    tab_delay: Duration,
    shared: Arc<Shared>,
    entry: Arc<SessionEntry>,
    client_rx: SplitStream<WebSocketStream<TcpStream>>,
    client_tx: ClientSender,
    up: Box<dyn UpstreamConn>,
    open: OpenConnection,
    /// Upstream request id -> who asked. Every request upstream carries a proxy-assigned
    /// id, so client ids can neither collide with nor answer the proxy's own requests.
    pending: HashMap<u64, Pending>,
    next_upstream_id: u64,
    tabs_in_flight: usize,
    /// Top-level target events about the session's own browser context, dropped while a
    /// tab creation is in flight. Chrome can report the new tab before the bridge returns
    /// its id; once the tab is in scope these are judged again so the client still sees its
    /// own tab appear. Events about other contexts are never kept.
    stash: VecDeque<(Instant, Value)>,
    /// Sessions this upstream got from browser-level auto-attach and still holds (shown to
    /// the client or stashed), with their target. They are released before the upstream
    /// closes.
    auto_sessions: HashMap<String, Option<String>>,
    /// Targets already re-attached once to resume them; never retried, so a target whose
    /// resume keeps failing cannot loop.
    recovered: HashSet<String>,
    tab_tx: mpsc::UnboundedSender<TabCreated>,
    tab_rx: mpsc::UnboundedReceiver<TabCreated>,
}

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
    async fn run(mut self, mut closed: watch::Receiver<bool>, writer: JoinHandle<()>) {
        loop {
            let stash_expiry = self.stash.front().map(|(at, _)| *at + STASH_MAX_AGE);
            let step = tokio::select! {
                _ = async { let _ = closed.wait_for(|c| *c).await; } => break,
                m = self.client_rx.next() => match m {
                    Some(Ok(Message::Text(t))) => self.on_client(t.as_str()).await,
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => Ok(()),
                },
                m = self.up.recv() => match m {
                    Some(t) => self.on_browser(&t).await,
                    None => break,
                },
                Some(done) = self.tab_rx.recv() => self.on_tab_created(done).await,
                _ = tokio::time::sleep_until(stash_expiry.unwrap_or_else(Instant::now)),
                    if stash_expiry.is_some() => self.expire_stash().await,
            };
            if step.is_err() {
                break;
            }
        }
        self.shutdown(writer).await;
    }

    /// Every exit path ends here. Closing the upstream acts as a detach without a resume,
    /// which leaves a held page frozen, and so does turning auto-attach off (Chrome then
    /// detaches every auto-attached session). So, in this order, each step bounded in time:
    /// release every auto-attached session this upstream holds; turn auto-attach off; read
    /// the upstream until the browser confirmed that and every release (including the
    /// re-attach of targets detached before their resume arrived) was answered.
    async fn shutdown(mut self, writer: JoinHandle<()>) {
        // Stashed auto-attach sessions are in `auto_sessions` too.
        self.stash.clear();
        let held: Vec<_> = self.auto_sessions.drain().collect();
        for (session_id, target_id) in held {
            let release = self.release_session(session_id, target_id);
            let _ = tokio::time::timeout(SHUTDOWN_STEP, release).await;
        }
        let off = json!({ "method": "Target.setAutoAttach",
            "params": { "autoAttach": false, "waitForDebuggerOnStart": false, "flatten": true } });
        let off_id =
            tokio::time::timeout(SHUTDOWN_STEP, self.send_upstream(off, Pending::Internal))
                .await
                .ok()
                .and_then(Result::ok);
        if let Some(off_id) = off_id {
            let deadline = Instant::now() + SHUTDOWN_DRAIN;
            let mut off_confirmed = false;
            while !off_confirmed || self.release_in_flight() {
                let Ok(Some(text)) = tokio::time::timeout_at(deadline, self.up.recv()).await else {
                    break;
                };
                let Ok(msg) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                off_confirmed |= msg.get("id").and_then(Value::as_u64) == Some(off_id);
                let _ =
                    tokio::time::timeout(SHUTDOWN_STEP, self.on_message_while_closing(msg)).await;
            }
        }
        let ClientConn {
            up,
            open,
            client_tx,
            client_rx,
            ..
        } = self;
        drop(up);
        drop(open);
        drop(client_tx);
        let abort = writer.abort_handle();
        if tokio::time::timeout(CLIENT_CLOSE_TIMEOUT, writer)
            .await
            .is_err()
        {
            // The writer is stuck on a client that does not read; aborting it drops the socket.
            abort.abort();
        }
        drop(client_rx);
    }

    fn send_client(&self, v: &Value) -> Result<()> {
        self.client_tx.send(v)
    }

    fn release_in_flight(&self) -> bool {
        self.pending
            .values()
            .any(|p| matches!(p, Pending::Resume { .. } | Pending::Recover))
    }

    /// While closing nothing reaches the client; only releases still matter.
    async fn on_message_while_closing(&mut self, msg: Value) -> Result<()> {
        if let Some(id) = msg.get("id").and_then(Value::as_u64) {
            if let Some(Pending::Resume { target_id }) = self.pending.remove(&id) {
                return self.on_resume_reply(&msg, target_id).await;
            }
            return Ok(());
        }
        match auto_attached_session(&msg) {
            Some((session_id, target_id)) => self.release_session(session_id, target_id).await,
            None => Ok(()),
        }
    }

    async fn send_upstream(&mut self, mut v: Value, pending: Pending) -> Result<u64> {
        let id = self.next_upstream_id;
        self.next_upstream_id += 1;
        v["id"] = json!(id);
        self.pending.insert(id, pending);
        self.up.send(v.to_string()).await?;
        Ok(id)
    }

    async fn on_client(&mut self, text: &str) -> Result<()> {
        let Ok(msg) = serde_json::from_str::<Value>(text) else {
            let err =
                json!({ "error": { "code": -32700, "message": "Message must be valid JSON" } });
            return self.send_client(&err);
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
            return self.send_client(&reply(&client, Err(err)));
        }
        let decision = filter::client_message(&msg, &self.entry.scope.lock().unwrap());
        // The browser receives the value the decision was made on, re-serialized; never the
        // client's original bytes.
        let forward = match decision {
            ClientDecision::Forward => msg,
            ClientDecision::ForwardRewritten(v) => v,
            ClientDecision::Reply(result) => return self.send_client(&reply(&client, Ok(result))),
            ClientDecision::Error { code, message } => {
                return self.send_client(&reply(&client, Err((code, message))));
            }
            ClientDecision::CreateTab { url } => {
                self.tabs_in_flight += 1;
                let bridge = self.entry.bridge.clone();
                let group = self.entry.group.lock().unwrap().clone();
                let tx = self.tab_tx.clone();
                let delay = self.tab_delay;
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    let result = bridge.create_tab(&group, &url).await;
                    let _ = tx.send(TabCreated { client, result });
                });
                return Ok(());
            }
        };
        let method = forward["method"].as_str().unwrap_or_default().to_string();
        self.send_upstream(forward, Pending::Client { client, method })
            .await
            .map(drop)
    }

    async fn on_browser(&mut self, text: &str) -> Result<()> {
        // Default-deny: what does not parse is not forwarded.
        let Ok(msg) = serde_json::from_str::<Value>(text) else {
            return Ok(());
        };
        let Some(id) = msg.get("id") else {
            return self.on_browser_event(msg, None).await;
        };
        // A reply to nobody's request is dropped.
        let Some(pending) = id.as_u64().and_then(|id| self.pending.remove(&id)) else {
            return Ok(());
        };
        match pending {
            Pending::Client { client, method } => self.on_client_response(msg, client, &method),
            Pending::VerifyTab { client, target_id } => {
                self.finish_tab(&msg, client, target_id).await
            }
            Pending::Resume { target_id } => self.on_resume_reply(&msg, target_id).await,
            Pending::Internal | Pending::Recover => Ok(()),
        }
    }

    fn on_client_response(
        &mut self,
        mut msg: Value,
        client: ClientRef,
        method: &str,
    ) -> Result<()> {
        msg["id"] = client.id.clone();
        let (decision, adopted) = {
            let mut scope = self.entry.scope.lock().unwrap();
            let adopted = adopt_listed_popups(&msg, &mut scope);
            let decision = filter::browser_message(msg, Some(method), &scope);
            if let BrowserDecision::Forward(v) = &decision {
                if method == "Target.attachToTarget" {
                    if let Some(s) = v.pointer("/result/sessionId").and_then(Value::as_str) {
                        scope.cdp_sessions.insert(s.to_string());
                    }
                }
            }
            (decision, adopted)
        };
        for target_id in &adopted {
            self.shared.target_added(&self.entry, target_id);
        }
        match decision {
            BrowserDecision::Forward(v) | BrowserDecision::Adopt { forward: v, .. } => {
                self.send_client(&v)
            }
            // The session left the scope while the request was in flight; still answer.
            BrowserDecision::Drop => {
                let err = (-32001, "Session with given id not found.".to_string());
                self.send_client(&reply(&client, Err(err)))
            }
        }
    }

    /// `stashed_at` is set when a stashed event is judged again; it keeps its original age.
    async fn on_browser_event(&mut self, msg: Value, stashed_at: Option<Instant>) -> Result<()> {
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let top_level = msg.get("sessionId").is_none();
        if top_level && method == "Target.detachedFromTarget" {
            if let Some(s) = msg.pointer("/params/sessionId").and_then(Value::as_str) {
                self.auto_sessions.remove(s);
            }
        }
        let is_target_event = top_level && filter::TARGET_EVENTS.contains(&method);
        let event_context = msg
            .pointer("/params/targetInfo/browserContextId")
            .and_then(Value::as_str)
            .map(String::from);
        let auto_attached = auto_attached_session(&msg);
        let copy = (self.tabs_in_flight > 0 && is_target_event && event_context.is_some())
            .then(|| msg.clone());
        let (decision, own_context, adopted) = {
            let mut scope = self.entry.scope.lock().unwrap();
            let own_context = event_context.as_deref() == Some(scope.context.as_str());
            let decision = filter::browser_message(msg, None, &scope);
            let mut adopted = None;
            match &decision {
                BrowserDecision::Forward(v) => track_event(v, &mut scope),
                BrowserDecision::Adopt { target_id, forward } => {
                    if scope.targets.insert(target_id.clone()) {
                        adopted = Some(target_id.clone());
                    }
                    track_event(forward, &mut scope);
                }
                BrowserDecision::Drop => {}
            }
            (decision, own_context, adopted)
        };
        if let Some(target_id) = adopted {
            self.shared.target_added(&self.entry, &target_id);
        }
        match decision {
            BrowserDecision::Forward(v) | BrowserDecision::Adopt { forward: v, .. } => {
                if let Some((session_id, target_id)) = auto_attached {
                    self.auto_sessions.insert(session_id, target_id);
                }
                self.send_client(&v)
            }
            BrowserDecision::Drop => match copy {
                // Only an event about the session's own context can be adopted later.
                Some(copy) if own_context => {
                    if let Some((session_id, target_id)) = auto_attached {
                        self.auto_sessions.insert(session_id, target_id);
                    }
                    if self.stash.len() == STASH_LIMIT {
                        if let Some((_, oldest)) = self.stash.pop_front() {
                            self.release(&oldest).await?;
                        }
                    }
                    self.stash
                        .push_back((stashed_at.unwrap_or_else(Instant::now), copy));
                    Ok(())
                }
                _ => match auto_attached {
                    Some((session_id, target_id)) => {
                        self.release_session(session_id, target_id).await
                    }
                    None => Ok(()),
                },
            },
        }
    }

    /// Browser-level auto-attach attaches this connection to every new target, including
    /// the human's. Such a session is never shown to the client, so the proxy releases it
    /// itself: resume, then detach. Chrome 157 holds a human page that calls `window.open`
    /// until the popup's auto-attached session is resumed, even with
    /// `waitForDebuggerOnStart: false`; a detach without a resume (also the implicit one when
    /// auto-attach is turned off or the connection closes) leaves it held.
    async fn release_session(
        &mut self,
        session_id: String,
        target_id: Option<String>,
    ) -> Result<()> {
        self.auto_sessions.remove(&session_id);
        let resume =
            json!({ "method": "Runtime.runIfWaitingForDebugger", "sessionId": session_id });
        self.send_upstream(resume, Pending::Resume { target_id })
            .await?;
        let detach =
            json!({ "method": "Target.detachFromTarget", "params": { "sessionId": session_id } });
        self.send_upstream(detach, Pending::Internal)
            .await
            .map(drop)
    }

    /// A resume that failed means Chrome detached the session before the resume arrived
    /// (e.g. the client turned auto-attach off meanwhile), and the target may be held. A
    /// fresh attach is resumed and detached through its `Target.attachedToTarget` event
    /// like any hidden session; each target is re-attached at most once.
    async fn on_resume_reply(&mut self, reply: &Value, target_id: Option<String>) -> Result<()> {
        let Some(target_id) = target_id else {
            return Ok(());
        };
        if reply.get("error").is_none() || !self.recovered.insert(target_id.clone()) {
            return Ok(());
        }
        let attach = json!({ "method": "Target.attachToTarget", "params": { "targetId": target_id, "flatten": true } });
        self.send_upstream(attach, Pending::Recover).await.map(drop)
    }

    /// A stashed event that will not be shown after all.
    async fn release(&mut self, event: &Value) -> Result<()> {
        match auto_attached_session(event) {
            Some((session_id, target_id)) => self.release_session(session_id, target_id).await,
            None => Ok(()),
        }
    }

    async fn forget_stash_if_idle(&mut self) -> Result<()> {
        if self.tabs_in_flight == 0 {
            let stashed: Vec<_> = self.stash.drain(..).collect();
            for (_, event) in &stashed {
                self.release(event).await?;
            }
        }
        Ok(())
    }

    /// Stashed events wait at most `STASH_MAX_AGE`, so back-to-back tab creations cannot
    /// hold another session's auto-attached tab forever.
    async fn expire_stash(&mut self) -> Result<()> {
        let now = Instant::now();
        while let Some((at, _)) = self.stash.front() {
            if *at + STASH_MAX_AGE > now {
                break;
            }
            let (_, event) = self.stash.pop_front().unwrap();
            self.release(&event).await?;
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
                self.send_upstream(check, pending).await.map(drop)
            }
            Err(e) => {
                self.tabs_in_flight -= 1;
                self.forget_stash_if_idle().await?;
                let err = (-32000, format!("creating the tab failed: {e:#}"));
                self.send_client(&reply(&done.client, Err(err)))
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
        let (in_context, added) = {
            let mut scope = self.entry.scope.lock().unwrap();
            let ok = context == Some(scope.context.as_str());
            let added = ok && scope.targets.insert(target_id.clone());
            (ok, added)
        };
        if added {
            self.shared.target_added(&self.entry, &target_id);
        }
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
            return self.send_client(&reply(&client, Err(err)));
        }
        // Judge the stashed events again now that the tab is in scope, in arrival order,
        // before the reply (the order Chrome itself uses). Events still dropped are stashed
        // again only while another tab creation is in flight (bounded by `STASH_MAX_AGE`).
        let stashed: Vec<_> = self.stash.drain(..).collect();
        for (at, event) in stashed {
            self.on_browser_event(event, Some(at)).await?;
        }
        self.send_client(&reply(&client, Ok(json!({ "targetId": target_id }))))
    }
}

/// Session and target of a top-level `Target.attachedToTarget` (browser-level auto-attach,
/// or an attach of this connection).
fn auto_attached_session(msg: &Value) -> Option<(String, Option<String>)> {
    if msg.get("sessionId").is_some() || msg["method"] != "Target.attachedToTarget" {
        return None;
    }
    let s = |p: &str| msg.pointer(p).and_then(Value::as_str).map(String::from);
    Some((s("/params/sessionId")?, s("/params/targetInfo/targetId")))
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
/// in scope and the popup lives in the session's context. Returns the adopted targets.
fn adopt_listed_popups(msg: &Value, scope: &mut Scope) -> Vec<String> {
    let mut adopted = Vec::new();
    let Some(infos) = msg.pointer("/result/targetInfos").and_then(Value::as_array) else {
        return adopted;
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
                    adopted.push(id.to_string());
                    added = true;
                }
            }
        }
        // A popup of a popup is adopted once its opener is.
        if !added {
            return adopted;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const ID: &str = "s_abcdefgh";

    struct TestEntry(String);

    impl Authorizable for TestEntry {
        fn token(&self) -> &str {
            &self.0
        }
    }

    fn sessions() -> Mutex<HashMap<String, Arc<TestEntry>>> {
        let entry = TestEntry(TOKEN.into());
        Mutex::new(HashMap::from([(ID.to_string(), Arc::new(entry))]))
    }

    fn head(path: &str, extra: &[&str]) -> RequestHead {
        let mut text = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
        );
        for h in extra {
            text.push_str(h);
            text.push_str("\r\n");
        }
        text.push_str("\r\n");
        parse_head(text.as_bytes()).expect("head parses")
    }

    fn status(path: &str, extra: &[&str]) -> Result<(), &'static str> {
        authorize(&sessions(), &head(path, extra)).map(drop)
    }

    fn bearer(token: &str) -> String {
        format!("Authorization: Bearer {token}")
    }

    #[test]
    fn parse_head_lowercases_names_and_rejects_garbage() {
        let h = head("/s/x", &["X-Custom: Value"]);
        assert_eq!(h.method, "GET");
        assert_eq!(h.path, "/s/x");
        assert_eq!(h.single("x-custom"), Some("Value"));
        assert!(parse_head(b"not http at all\r\n\r\n").is_none());
        assert!(
            parse_head(b"GET / HTTP/1.1\r\nHost: x\r\n").is_none(),
            "incomplete head"
        );
    }

    #[test]
    fn correct_token_is_accepted() {
        let path = format!("/s/{ID}");
        assert_eq!(status(&path, &[&bearer(TOKEN)]), Ok(()));
    }

    #[test]
    fn missing_authorization_is_401() {
        assert_eq!(status(&format!("/s/{ID}"), &[]), Err("401 Unauthorized"));
    }

    #[test]
    fn two_authorization_headers_are_401() {
        let path = format!("/s/{ID}");
        let good = bearer(TOKEN);
        assert_eq!(status(&path, &[&good, &good]), Err("401 Unauthorized"));
        assert_eq!(
            status(&path, &[&good, "Authorization: Bearer wrong"]),
            Err("401 Unauthorized")
        );
    }

    #[test]
    fn wrong_scheme_is_401() {
        let path = format!("/s/{ID}");
        for h in [
            format!("Authorization: Basic {TOKEN}"),
            format!("Authorization: bearer {TOKEN}"),
            format!("Authorization: {TOKEN}"),
            format!("Authorization: Bearer  {TOKEN}"),
        ] {
            assert_eq!(status(&path, &[&h]), Err("401 Unauthorized"), "{h}");
        }
    }

    #[test]
    fn wrong_token_of_same_or_other_length_is_401() {
        let path = format!("/s/{ID}");
        let mut same_len = TOKEN.to_string();
        same_len.replace_range(63.., "0");
        assert_eq!(same_len.len(), TOKEN.len());
        for t in [same_len.as_str(), &TOKEN[..63], &format!("{TOKEN}0"), ""] {
            assert_eq!(status(&path, &[&bearer(t)]), Err("401 Unauthorized"), "{t}");
        }
    }

    #[test]
    fn unknown_or_malformed_session_path_is_401() {
        let good = bearer(TOKEN);
        for path in [
            "/s/s_unknown00".to_string(),
            format!("/s/{ID}/"),
            format!("/s/{ID}/x"),
            format!("/s/{ID}?a=1"),
            format!("/s/{ID}#f"),
            format!("/x/{ID}"),
            format!("/s//{ID}"),
            "/".to_string(),
        ] {
            assert_eq!(status(&path, &[&good]), Err("401 Unauthorized"), "{path}");
        }
    }

    #[test]
    fn plain_http_is_404_whatever_the_token() {
        let text = format!(
            "GET /s/{ID} HTTP/1.1\r\nHost: x\r\n{}\r\n\r\n",
            bearer(TOKEN)
        );
        let h = parse_head(text.as_bytes()).unwrap();
        assert_eq!(authorize(&sessions(), &h).map(drop), Err("404 Not Found"));
    }

    #[test]
    fn malformed_upgrade_is_400_only_after_auth() {
        let text = format!(
            "GET /s/{ID} HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n{}\r\n\r\n",
            bearer(TOKEN)
        );
        let h = parse_head(text.as_bytes()).unwrap();
        assert_eq!(authorize(&sessions(), &h).map(drop), Err("400 Bad Request"));
        let text = format!("GET /s/{ID} HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\r\n");
        let h = parse_head(text.as_bytes()).unwrap();
        assert_eq!(
            authorize(&sessions(), &h).map(drop),
            Err("401 Unauthorized")
        );
    }
}
