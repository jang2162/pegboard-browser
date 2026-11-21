//! Minimal Chrome DevTools Protocol client over a single connection: a virtual connection of
//! the daemon's `BrowserLink`, or a websocket of its own.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::link::{BrowserLink, VirtualReceiver, VirtualSender};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const NAVIGATE_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest message and frame a queued connection takes (Chrome sends a CDP message as one
/// frame). The request guard reads response bodies in pieces well below this; the limit is
/// there so that no other message (a long URL in an event) can end the guard's connection.
pub(crate) const QUEUED_MAX_MESSAGE: usize = 256 << 20;

#[derive(Debug, Clone)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
    pub session_id: Option<String>,
}

/// A reply, and how many events the connection had received before it.
type Reply = (Result<Value>, u64);

/// Outstanding requests, and whether the reader has finished (no reply can arrive any more).
#[derive(Default)]
struct PendingState {
    waiters: HashMap<u64, oneshot::Sender<Reply>>,
    closed: bool,
}

type Pending = Mutex<PendingState>;

/// What the writer task is given: a message to send, or the end of the connection.
enum Out {
    Text(String),
    Close,
}

/// The two ends a connection runs on: text messages out and in.
struct Channel {
    sink: ChannelSink,
    stream: ChannelStream,
}

enum ChannelSink {
    Websocket(SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>),
    Virtual(Option<VirtualSender>),
}

enum ChannelStream {
    Websocket(SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>),
    Virtual(VirtualReceiver),
}

impl Channel {
    async fn websocket(
        url: &str,
        headers: &[(&str, &str)],
        config: Option<WebSocketConfig>,
    ) -> Result<Channel> {
        let mut request = url
            .into_client_request()
            .with_context(|| format!("invalid websocket url {url}"))?;
        for (name, value) in headers {
            request.headers_mut().insert(
                HeaderName::from_bytes(name.as_bytes()).context("invalid header name")?,
                HeaderValue::from_str(value).context("invalid header value")?,
            );
        }
        let (ws, _) = tokio_tungstenite::connect_async_with_config(request, config, false)
            .await
            .with_context(|| format!("connecting to {url}"))?;
        let (sink, stream) = ws.split();
        Ok(Channel {
            sink: ChannelSink::Websocket(sink),
            stream: ChannelStream::Websocket(stream),
        })
    }

    async fn link(link: &BrowserLink) -> Result<Channel> {
        let (sender, receiver) = link.open().await?.split();
        Ok(Channel {
            sink: ChannelSink::Virtual(Some(sender)),
            stream: ChannelStream::Virtual(receiver),
        })
    }
}

impl ChannelSink {
    /// False once the connection cannot take messages any more.
    async fn send(&mut self, text: String) -> bool {
        match self {
            ChannelSink::Websocket(sink) => sink.send(Message::text(text)).await.is_ok(),
            ChannelSink::Virtual(Some(sender)) => sender.send(text).is_ok(),
            ChannelSink::Virtual(None) => false,
        }
    }

    /// Ends the connection: a close frame, or the virtual connection's sender dropped (which
    /// detaches its sessions).
    async fn close(&mut self) {
        match self {
            ChannelSink::Websocket(sink) => {
                let _ = sink.send(Message::Close(None)).await;
            }
            ChannelSink::Virtual(sender) => drop(sender.take()),
        }
    }
}

impl ChannelStream {
    /// The next text message; `None` once the connection has ended or failed.
    async fn next(&mut self) -> Option<String> {
        match self {
            ChannelStream::Websocket(stream) => loop {
                match stream.next().await? {
                    Ok(Message::Text(text)) => return Some(text.as_str().to_owned()),
                    Ok(_) => continue,
                    Err(_) => return None,
                }
            },
            ChannelStream::Virtual(receiver) => receiver.recv().await,
        }
    }
}

struct Inner {
    out: mpsc::UnboundedSender<Out>,
    pending: Pending,
    next_id: AtomicU64,
    events: broadcast::Sender<CdpEvent>,
    /// Every event, unbounded and in order, for a reader that must not miss one (see
    /// `connect_queued`). Dropped when the connection ends, which ends the queue.
    queue: Mutex<Option<mpsc::UnboundedSender<CdpEvent>>>,
    /// Events received so far (the reader counts them in socket order).
    events_received: AtomicU64,
}

/// Removes a request's waiter when its reply future ends or is dropped (a no-op once the
/// reader took it out to answer).
struct Waiter {
    inner: Arc<Inner>,
    id: u64,
}

impl Drop for Waiter {
    fn drop(&mut self) {
        self.inner.pending.lock().unwrap().waiters.remove(&self.id);
    }
}

/// Cheap to clone; all clones share one connection.
#[derive(Clone)]
pub struct CdpConn {
    inner: Arc<Inner>,
}

impl CdpConn {
    /// A connection of its own to `url` (a websocket). The daemon's connections ride its
    /// `BrowserLink` (`over`); this is for tests and other endpoints (the session proxy).
    pub async fn connect(url: &str) -> Result<CdpConn> {
        Self::connect_with_headers(url, &[]).await
    }

    /// Connects sending extra HTTP headers with the upgrade request (e.g. `Authorization`).
    pub async fn connect_with_headers(url: &str, headers: &[(&str, &str)]) -> Result<CdpConn> {
        Ok(Self::start(
            Channel::websocket(url, headers, None).await?,
            None,
        ))
    }

    /// Connects and also hands every event to the returned queue, none dropped (the
    /// broadcast of `events` drops events for a reader that lags). The queue ends when the
    /// connection does. Messages up to 256 MiB are accepted (tungstenite's default is 64 MiB
    /// a message and 16 MiB a frame).
    pub async fn connect_queued(url: &str) -> Result<(CdpConn, mpsc::UnboundedReceiver<CdpEvent>)> {
        let (tx, rx) = mpsc::unbounded_channel();
        let config = WebSocketConfig::default()
            .max_message_size(Some(QUEUED_MAX_MESSAGE))
            .max_frame_size(Some(QUEUED_MAX_MESSAGE));
        let channel = Channel::websocket(url, &[], Some(config)).await?;
        Ok((Self::start(channel, Some(tx)), rx))
    }

    /// A connection on a new virtual connection of `link`: to its user, a connection of its
    /// own to the browser. `close` closes the virtual connection (its sessions are detached).
    pub async fn over(link: &BrowserLink) -> Result<CdpConn> {
        Ok(Self::start(Channel::link(link).await?, None))
    }

    /// `over`, also handing every event to the returned queue, none dropped (as
    /// `connect_queued`; the link takes messages up to 256 MiB).
    pub async fn over_queued(
        link: &BrowserLink,
    ) -> Result<(CdpConn, mpsc::UnboundedReceiver<CdpEvent>)> {
        let (tx, rx) = mpsc::unbounded_channel();
        Ok((Self::start(Channel::link(link).await?, Some(tx)), rx))
    }

    /// Runs a connection on `channel`: a writer task and a reader task.
    fn start(channel: Channel, queue: Option<mpsc::UnboundedSender<CdpEvent>>) -> CdpConn {
        let Channel {
            mut sink,
            mut stream,
        } = channel;
        let (out, mut out_rx) = mpsc::unbounded_channel::<Out>();
        let (events, _) = broadcast::channel(1024);
        let inner = Arc::new(Inner {
            out,
            pending: Mutex::new(PendingState::default()),
            next_id: AtomicU64::new(1),
            events,
            queue: Mutex::new(queue),
            events_received: AtomicU64::new(0),
        });

        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                let sent = match msg {
                    Out::Text(text) => sink.send(text).await,
                    Out::Close => false,
                };
                if !sent {
                    break;
                }
            }
            sink.close().await;
        });

        let reader = inner.clone();
        tokio::spawn(async move {
            while let Some(text) = stream.next().await {
                let Ok(v) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if let Some(id) = v.get("id").and_then(Value::as_u64) {
                    let waiter = reader.pending.lock().unwrap().waiters.remove(&id);
                    if let Some(tx) = waiter {
                        let result = match v.get("error") {
                            Some(e) => Err(anyhow!("CDP error: {e}")),
                            None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        let before = reader.events_received.load(Ordering::SeqCst);
                        let _ = tx.send((result, before));
                    }
                } else if let Some(method) = v.get("method").and_then(Value::as_str) {
                    let event = CdpEvent {
                        method: method.to_string(),
                        params: v.get("params").cloned().unwrap_or(Value::Null),
                        session_id: v.get("sessionId").and_then(Value::as_str).map(String::from),
                    };
                    reader.events_received.fetch_add(1, Ordering::SeqCst);
                    if let Some(queue) = reader.queue.lock().unwrap().as_ref() {
                        let _ = queue.send(event.clone());
                    }
                    let _ = reader.events.send(event);
                }
            }
            reader.queue.lock().unwrap().take();
            // The stream ended or failed (e.g. a message over the limit): close the sending
            // side too, so the other side sees the end and nothing half-open is left.
            let _ = reader.out.send(Out::Close);
            // Connection closed: fail every outstanding request, and every later one.
            let waiters: Vec<_> = {
                let mut pending = reader.pending.lock().unwrap();
                pending.closed = true;
                pending.waiters.drain().collect()
            };
            let before = reader.events_received.load(Ordering::SeqCst);
            for (_, tx) in waiters {
                let _ = tx.send((Err(anyhow!("CDP connection closed")), before));
            }
        });

        CdpConn { inner }
    }

    pub async fn send(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value> {
        self.send_now(method, params, session_id).await
    }

    /// Writes the request at once (before this returns, so requests sent one after the other
    /// reach the browser in that order) and returns a future of its reply, which may be
    /// awaited later or not at all.
    pub fn send_now(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> impl std::future::Future<Output = Result<Value>> + Send + 'static {
        let reply = self.send_counted(method, params, session_id);
        async move { reply.await.map(|(value, _)| value) }
    }

    /// `send_now`, also giving how many events the connection had received before the reply
    /// (in socket order): a queued reader that has handled that many events has seen every
    /// event the browser sent before answering.
    pub fn send_counted(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> impl std::future::Future<Output = Result<(Value, u64)>> + Send + 'static {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session_id {
            msg["sessionId"] = json!(s);
        }
        let (tx, rx) = oneshot::channel();
        let sent = {
            let mut pending = self.inner.pending.lock().unwrap();
            if pending.closed {
                false
            } else {
                pending.waiters.insert(id, tx);
                true
            }
        };
        let sent = sent && {
            let ok = self.inner.out.send(Out::Text(msg.to_string())).is_ok();
            if !ok {
                self.inner.pending.lock().unwrap().waiters.remove(&id);
            }
            ok
        };
        // A caller that stops waiting (a timeout of its own, a dropped future) takes its
        // waiter out, so requests that are never answered do not pile up.
        let waiter = Waiter {
            inner: self.inner.clone(),
            id,
        };
        let inner = self.inner.clone();
        let method = method.to_string();
        async move {
            let _waiter = waiter;
            if !sent {
                bail!("CDP connection closed");
            }
            match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
                Ok(Ok((result, before))) => result
                    .map(|value| (value, before))
                    .with_context(|| format!("{method} failed")),
                Ok(Err(_)) => bail!("CDP connection closed during {method}"),
                Err(_) => {
                    inner.pending.lock().unwrap().waiters.remove(&id);
                    bail!("{method} timed out after {}s", REQUEST_TIMEOUT.as_secs())
                }
            }
        }
    }

    /// Closes the connection (the websocket, or the virtual connection and its sessions);
    /// outstanding and later requests fail. Chrome drops what the connection had set
    /// (permissions, download behavior, interception).
    pub fn close(&self) {
        let _ = self.inner.out.send(Out::Close);
    }

    pub fn events(&self) -> broadcast::Receiver<CdpEvent> {
        self.inner.events.subscribe()
    }

    /// Attach to a target in flat mode and return the session id.
    pub async fn attach(&self, target_id: &str) -> Result<String> {
        let r = self
            .send(
                "Target.attachToTarget",
                json!({ "targetId": target_id, "flatten": true }),
                None,
            )
            .await?;
        r["sessionId"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| anyhow!("attachToTarget returned no sessionId"))
    }

    /// Evaluate an expression (awaiting promises) and return its value.
    pub async fn evaluate(&self, session_id: &str, expression: &str) -> Result<Value> {
        let r = self
            .send(
                "Runtime.evaluate",
                json!({ "expression": expression, "awaitPromise": true, "returnByValue": true }),
                Some(session_id),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            bail!("evaluate threw: {ex}");
        }
        Ok(r["result"].get("value").cloned().unwrap_or(Value::Null))
    }

    /// Navigate and wait for `Page.loadEventFired` (15 s limit).
    pub async fn navigate(&self, session_id: &str, url: &str) -> Result<()> {
        // Subscribe before navigating so the load event cannot be missed.
        let mut events = self.events();
        self.send("Page.enable", json!({}), Some(session_id))
            .await?;
        let r = self
            .send("Page.navigate", json!({ "url": url }), Some(session_id))
            .await?;
        if let Some(err) = r.get("errorText").and_then(Value::as_str) {
            bail!("navigation to {url} failed: {err}");
        }
        let wait = async {
            loop {
                match events.recv().await {
                    Ok(e)
                        if e.method == "Page.loadEventFired"
                            && e.session_id.as_deref() == Some(session_id) =>
                    {
                        return Ok(());
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => bail!("CDP connection closed"),
                }
            }
        };
        tokio::time::timeout(NAVIGATE_TIMEOUT, wait)
            .await
            .map_err(|_| anyhow!("navigation to {url} timed out after 15s"))?
    }
}

#[derive(Debug, Clone)]
pub struct TargetInfo {
    pub target_id: String,
    pub kind: String,
    pub url: String,
    pub browser_context_id: Option<String>,
    pub opener_id: Option<String>,
}

/// All targets of type `page`.
pub async fn page_targets(conn: &CdpConn) -> Result<Vec<TargetInfo>> {
    let r = conn.send("Target.getTargets", json!({}), None).await?;
    let infos = r["targetInfos"].as_array().cloned().unwrap_or_default();
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(String::from);
    Ok(infos
        .iter()
        .filter(|t| t["type"] == "page")
        .map(|t| TargetInfo {
            target_id: s(t, "targetId").unwrap_or_default(),
            kind: "page".to_string(),
            url: s(t, "url").unwrap_or_default(),
            browser_context_id: s(t, "browserContextId"),
            opener_id: s(t, "openerId"),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reply carries how many events came before it on the socket.
    #[tokio::test]
    async fn a_reply_counts_the_events_before_it() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let mut ids = Vec::new();
            while ids.len() < 2 {
                if let Some(Ok(Message::Text(t))) = ws.next().await {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    ids.push(v["id"].as_u64().unwrap());
                }
            }
            let reply = |id: u64| Message::text(json!({ "id": id, "result": {} }).to_string());
            let event = || Message::text(json!({ "method": "Page.x", "params": {} }).to_string());
            ws.send(reply(ids[0])).await.unwrap();
            ws.send(event()).await.unwrap();
            ws.send(event()).await.unwrap();
            ws.send(reply(ids[1])).await.unwrap();
            let _ = ws.next().await;
        });
        let (conn, mut queue) = CdpConn::connect_queued(&format!("ws://{addr}"))
            .await
            .unwrap();
        let first = conn.send_counted("A", json!({}), None);
        let second = conn.send_counted("B", json!({}), None);
        assert_eq!(first.await.unwrap().1, 0);
        assert_eq!(second.await.unwrap().1, 2);
        assert_eq!(queue.recv().await.unwrap().method, "Page.x");
        conn.close();
    }

    /// A request whose caller stopped waiting leaves no waiter behind.
    #[tokio::test]
    async fn a_request_given_up_on_is_forgotten() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            // Never answers.
            while ws.next().await.is_some() {}
        });
        let conn = CdpConn::connect(&format!("ws://{addr}")).await.unwrap();
        for _ in 0..3 {
            let r = tokio::time::timeout(
                Duration::from_millis(20),
                conn.send("Runtime.evaluate", json!({}), Some("S")),
            )
            .await;
            assert!(r.is_err());
        }
        // Dropped before it was ever polled, too.
        drop(conn.send_now("Page.x", json!({}), None));
        assert!(conn.inner.pending.lock().unwrap().waiters.is_empty());
        conn.close();
    }
}
