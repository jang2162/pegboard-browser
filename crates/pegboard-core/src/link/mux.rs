//! Routing between virtual connections and the physical one.
//!
//! Who owns a session: a browser session belongs to the virtual connection that opened it; a
//! session announced by `Target.attachedToTarget` belongs to the owner of the session that
//! carried the event; the session in a `Target.attachToTarget` answer belongs to the
//! requester. `Target.detachedFromTarget` removes it. A message whose owner is not known is
//! given to no one.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};
use tokio::sync::{mpsc, oneshot, watch};

use super::transport::{Incoming, Transport};

pub(crate) type VconnId = u64;

/// How long opening a browser session may take.
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);

type Opened = Result<(Handle, mpsc::UnboundedReceiver<String>)>;

/// A virtual connection, closed when this is dropped. Made as soon as the browser session
/// exists, so a session nobody takes (its `open` was cancelled) is closed too.
pub(crate) struct Handle {
    inner: Arc<Inner>,
    id: VconnId,
}

impl Handle {
    pub(crate) fn send(&self, text: String) -> Result<()> {
        self.inner.send(self.id, text)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.inner.close(self.id);
    }
}

/// Methods that name a session in their params, which must then be the sender's own.
const PARAMS_SESSION_METHODS: [&str; 2] = ["Target.detachFromTarget", "Target.sendMessageToTarget"];

/// A request on the physical connection, by the id the browser saw.
enum Pending {
    /// A virtual connection's request, and the id it gave. `claims` when the session in the
    /// answer becomes the requester's (`Target.attachToTarget`).
    Virtual {
        vconn: VconnId,
        id: Value,
        claims: bool,
    },
    /// `open` waits for its browser session.
    Open(oneshot::Sender<Opened>),
    /// The link's own request, whose answer nobody needs.
    Ignore,
}

struct Vconn {
    tx: mpsc::UnboundedSender<String>,
    browser_session: String,
}

struct State {
    /// `None` once the physical connection has ended.
    out: Option<mpsc::UnboundedSender<String>>,
    vconns: HashMap<VconnId, Vconn>,
    owners: HashMap<String, VconnId>,
    pending: HashMap<u64, Pending>,
    next_vconn: VconnId,
}

pub(crate) struct Inner {
    state: Mutex<State>,
    next_id: AtomicU64,
    closed: watch::Sender<bool>,
}

impl Inner {
    /// Starts routing the transport's messages. The routing task ends with the physical
    /// connection, or when the last handle of the link is dropped (which closes it).
    pub(crate) fn start(transport: Transport) -> Arc<Inner> {
        let Transport {
            mut incoming,
            outgoing,
        } = transport;
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                out: Some(outgoing),
                vconns: HashMap::new(),
                owners: HashMap::new(),
                pending: HashMap::new(),
                next_vconn: 1,
            }),
            next_id: AtomicU64::new(1),
            closed: watch::channel(false).0,
        });
        let weak: Weak<Inner> = Arc::downgrade(&inner);
        tokio::spawn(async move {
            while let Some(message) = incoming.recv().await {
                let Some(inner) = weak.upgrade() else { return };
                match message {
                    Incoming::Text(text) => Inner::dispatch(&inner, &text),
                    Incoming::Oversized { head, tail } => inner.oversized(&head, &tail),
                }
                drop(inner);
                // While a backlog is queued `recv` is always ready, and this task would go on
                // routing until tokio's budget runs out (128 messages; seconds for large
                // ones). A task it woke meanwhile (the user of a virtual connection, a
                // timeout) waits for it on this worker, so it lets them run after every
                // message while more is queued.
                if !incoming.is_empty() {
                    tokio::task::yield_now().await;
                }
            }
            if let Some(inner) = weak.upgrade() {
                inner.end();
            }
        });
        inner
    }

    pub(crate) async fn open(&self) -> Opened {
        let (tx, rx) = oneshot::channel();
        {
            let mut st = self.state.lock().unwrap();
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let msg = json!({"id": id, "method": "Target.attachToBrowserTarget", "params": {}});
            let Some(out) = &st.out else {
                bail!("the browser connection has ended");
            };
            out.send(msg.to_string())
                .map_err(|_| anyhow!("the browser connection has ended"))?;
            st.pending.insert(id, Pending::Open(tx));
        }
        // If this is dropped or times out, the session is detached once its answer came: the
        // handle made for it is dropped unused.
        match tokio::time::timeout(OPEN_TIMEOUT, rx).await {
            Ok(Ok(opened)) => opened,
            Ok(Err(_)) => bail!("the browser connection ended while opening a session"),
            Err(_) => bail!("Target.attachToBrowserTarget timed out"),
        }
    }

    pub(crate) fn send(&self, vconn: VconnId, text: String) -> Result<()> {
        let mut msg: Value = serde_json::from_str(&text).context("a CDP message is not JSON")?;
        let obj = msg
            .as_object_mut()
            .context("a CDP message is not a JSON object")?;
        let id = obj.get("id").cloned().context("a CDP message has no id")?;
        let method = obj.get("method").and_then(Value::as_str).unwrap_or("");
        let claims = method == "Target.attachToTarget";

        let mut st = self.state.lock().unwrap();
        let st = &mut *st;
        let Some(out) = &st.out else {
            bail!("the browser connection has ended");
        };
        let Some(vc) = st.vconns.get(&vconn) else {
            bail!("the virtual connection has ended");
        };
        let owns = |s: &Value| matches!(s, Value::String(s) if st.owners.get(s) == Some(&vconn));
        // A session it names, on the message or in the params of a method that acts on
        // another session, must be its own: the browser never sees one that is not.
        let foreign = obj.get("sessionId").is_some_and(|s| !owns(s))
            || (PARAMS_SESSION_METHODS.contains(&method)
                && params_session_value(obj).is_some_and(|s| !owns(s)));
        if foreign {
            let error = json!({"id": id, "error": {"code": -32001,
                "message": "Session with given id not found."}});
            let _ = vc.tx.send(error.to_string());
            return Ok(());
        }
        if !obj.contains_key("sessionId") {
            obj.insert("sessionId".into(), vc.browser_session.clone().into());
        }
        let link_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        obj.insert("id".into(), link_id.into());
        out.send(msg.to_string())
            .map_err(|_| anyhow!("the browser connection has ended"))?;
        st.pending
            .insert(link_id, Pending::Virtual { vconn, id, claims });
        Ok(())
    }

    /// Closes a virtual connection: detaches its sessions on the root and forgets them.
    pub(crate) fn close(&self, vconn: VconnId) {
        let mut st = self.state.lock().unwrap();
        self.close_locked(&mut st, vconn);
    }

    pub(crate) async fn closed(&self) {
        let mut rx = self.closed.subscribe();
        let _ = rx.wait_for(|closed| *closed).await;
    }

    pub(crate) fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    pub(crate) fn virtual_connections(&self) -> usize {
        self.state.lock().unwrap().vconns.len()
    }

    fn close_locked(&self, st: &mut State, vconn: VconnId) {
        let Some(vc) = forget(st, vconn) else { return };
        let Some(out) = &st.out else { return };
        // Children first, then the browser session (which takes any left with it).
        let mut sessions = vc.1;
        sessions.push(vc.0.browser_session);
        for session in sessions {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let msg = json!({"id": id, "method": "Target.detachFromTarget",
                "params": {"sessionId": session}});
            if out.send(msg.to_string()).is_ok() {
                st.pending.insert(id, Pending::Ignore);
            }
        }
    }

    /// The physical connection has ended: every virtual connection ends, every request
    /// fails, and nothing more is sent.
    fn end(&self) {
        {
            let mut st = self.state.lock().unwrap();
            st.out = None;
            st.vconns.clear();
            st.owners.clear();
            st.pending.clear();
        }
        self.closed.send_replace(true);
    }

    /// Routes one message from the browser.
    fn dispatch(self: &Arc<Self>, text: &str) {
        let Ok(Value::Object(mut msg)) = serde_json::from_str::<Value>(text) else {
            dropped("not a JSON object", None);
            return;
        };
        // A new virtual connection nobody took; dropped (which closes it) after the lock is
        // released, as it is declared before the lock.
        let mut _untaken: Option<Opened> = None;
        let mut st = self.state.lock().unwrap();
        let st = &mut *st;
        let session = match msg.get("sessionId") {
            None => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => {
                dropped("a session id that is not a string", Some(&msg));
                return;
            }
        };

        if let Some(link_id) = msg.get("id").and_then(Value::as_u64) {
            match st.pending.remove(&link_id) {
                Some(Pending::Virtual { vconn, id, claims }) => {
                    let Some(vc) = st.vconns.get(&vconn) else {
                        dropped("an answer for a closed connection", Some(&msg));
                        return;
                    };
                    if claims {
                        if let Some(s) = msg.get("result").and_then(|r| session_of(r)) {
                            claim(&mut st.owners, s, vconn);
                        }
                    }
                    msg.insert("id".into(), id);
                    if !deliver(vc, msg) {
                        self.close_locked(st, vconn);
                    }
                }
                Some(Pending::Open(tx)) => _untaken = self.opened(st, msg, tx),
                Some(Pending::Ignore) => {}
                None => dropped("an answer to no request", Some(&msg)),
            }
            return;
        }

        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let Some(session) = session else {
            // Root events belong to the link (it opens the browser sessions there), not to
            // a virtual connection. A browser session the browser detached ends its
            // connection, as a closed websocket would.
            if method == "Target.detachedFromTarget" {
                let gone = params_session(&msg).and_then(|s| {
                    st.vconns
                        .iter()
                        .find(|(_, vc)| vc.browser_session == s)
                        .map(|(id, _)| *id)
                });
                if let Some(vconn) = gone {
                    forget(st, vconn);
                }
            }
            dropped("an event on the root", Some(&msg));
            return;
        };
        let Some(&owner) = st.owners.get(&session) else {
            dropped("a session nobody owns", Some(&msg));
            return;
        };
        match method {
            "Target.attachedToTarget" => {
                if let Some(child) = params_session(&msg) {
                    claim(&mut st.owners, child, owner);
                }
            }
            "Target.detachedFromTarget" => {
                if let Some(child) = params_session(&msg) {
                    let browser_session = st.vconns.get(&owner).map(|vc| &vc.browser_session);
                    if st.owners.get(child) == Some(&owner)
                        && browser_session.map(String::as_str) != Some(child)
                    {
                        st.owners.remove(child);
                    }
                }
            }
            _ => {}
        }
        match st.vconns.get(&owner) {
            Some(vc) => {
                if !deliver(vc, msg) {
                    self.close_locked(st, owner);
                }
            }
            None => dropped("a session of a closed connection", Some(&msg)),
        }
    }

    /// A message over the limit, of which only its ends are known. It is delivered to no
    /// one; the one virtual connection it belongs to ends (a request it answers gets an error
    /// first). Found by its id (an answer: Chrome writes `{"id":N` first) or else by its
    /// session (Chrome writes `,"sessionId":"…"}` last). With no owner it is only dropped:
    /// the link and every other virtual connection go on.
    fn oversized(&self, head: &[u8], tail: &[u8]) {
        let mut st = self.state.lock().unwrap();
        let st = &mut *st;
        if let Some(link_id) = leading_id(head) {
            match st.pending.remove(&link_id) {
                Some(Pending::Virtual { vconn, id, .. }) => {
                    if let Some(vc) = st.vconns.get(&vconn) {
                        let error = json!({"id": id, "error": {"code": -32000,
                            "message": "The message is too large."}});
                        let _ = vc.tx.send(error.to_string());
                    }
                    self.close_locked(st, vconn);
                }
                Some(Pending::Open(tx)) => {
                    let _ = tx.send(Err(anyhow!(
                        "Target.attachToBrowserTarget answered too large a message"
                    )));
                }
                Some(Pending::Ignore) => {}
                None => dropped("an oversized answer to no request", None),
            }
            return;
        }
        match trailing_session(tail).and_then(|s| st.owners.get(s).copied()) {
            Some(owner) => self.close_locked(st, owner),
            None => dropped("an oversized message nobody owns", None),
        }
    }

    /// The answer to `Target.attachToBrowserTarget`: a new virtual connection. Returns it when
    /// nobody waits for it any more; the caller drops it once the state is unlocked.
    fn opened(
        self: &Arc<Self>,
        st: &mut State,
        msg: Map<String, Value>,
        tx: oneshot::Sender<Opened>,
    ) -> Option<Opened> {
        if let Some(error) = msg.get("error") {
            let _ = tx.send(Err(anyhow!("Target.attachToBrowserTarget failed: {error}")));
            return None;
        }
        let Some(session) = msg.get("result").and_then(session_of) else {
            let _ = tx.send(Err(anyhow!(
                "Target.attachToBrowserTarget gave no session id"
            )));
            return None;
        };
        let vconn = st.next_vconn;
        st.next_vconn += 1;
        let (vtx, vrx) = mpsc::unbounded_channel();
        st.vconns.insert(
            vconn,
            Vconn {
                tx: vtx,
                browser_session: session.to_owned(),
            },
        );
        st.owners.insert(session.to_owned(), vconn);
        // From here on the handle closes it, also when the `open` that waits for it is
        // cancelled after this (the answer then sits unread in the channel and is dropped).
        let handle = Handle {
            inner: self.clone(),
            id: vconn,
        };
        tx.send(Ok((handle, vrx))).err()
    }
}

/// Removes a virtual connection and every session it owns from the tables; returns it and
/// its sessions other than the browser session.
fn forget(st: &mut State, vconn: VconnId) -> Option<(Vconn, Vec<String>)> {
    let vc = st.vconns.remove(&vconn)?;
    st.pending
        .retain(|_, p| !matches!(p, Pending::Virtual { vconn: v, .. } if *v == vconn));
    let mut children = Vec::new();
    st.owners.retain(|session, owner| {
        if *owner != vconn {
            return true;
        }
        if *session != vc.browser_session {
            children.push(session.clone());
        }
        false
    });
    Some((vc, children))
}

/// Records `session` as `owner`'s, unless it already has an owner.
fn claim(owners: &mut HashMap<String, VconnId>, session: &str, owner: VconnId) {
    let current = *owners.entry(session.to_owned()).or_insert(owner);
    if current != owner {
        dropped("a session announced for a second owner", None);
    }
}

/// Hands a message to a virtual connection. Its browser session is its root: that session id
/// is taken off, as a connection of its own would see no session id there. False when nobody
/// reads the connection any more.
fn deliver(vc: &Vconn, mut msg: Map<String, Value>) -> bool {
    if msg.get("sessionId").and_then(Value::as_str) == Some(vc.browser_session.as_str()) {
        msg.remove("sessionId");
    }
    vc.tx.send(Value::Object(msg).to_string()).is_ok()
}

fn session_of(v: &Value) -> Option<&str> {
    v.get("sessionId").and_then(Value::as_str)
}

fn params_session(msg: &Map<String, Value>) -> Option<&str> {
    msg.get("params").and_then(session_of)
}

/// The id of a message that starts `{"id":N` (an answer, as Chrome writes it).
fn leading_id(head: &[u8]) -> Option<u64> {
    let rest = head.trim_ascii_start().strip_prefix(b"{\"id\":")?;
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    // A number cut off by the end of `head` is not known.
    if digits == 0 || digits == rest.len() {
        return None;
    }
    std::str::from_utf8(&rest[..digits]).ok()?.parse().ok()
}

/// The session of a message that ends `,"sessionId":"…"}` (as Chrome writes it). A message
/// whose last member is not that ends `}}` or so and has none.
fn trailing_session(tail: &[u8]) -> Option<&str> {
    const KEY: &[u8] = b",\"sessionId\":\"";
    let body = tail.trim_ascii_end().strip_suffix(b"\"}")?;
    let at = body.windows(KEY.len()).rposition(|w| w == KEY)?;
    let value = &body[at + KEY.len()..];
    if value.is_empty() || value.iter().any(|&b| b == b'"' || b == b'\\') {
        return None;
    }
    std::str::from_utf8(value).ok()
}

/// `params.sessionId`, whatever its type.
fn params_session_value(msg: &Map<String, Value>) -> Option<&Value> {
    msg.get("params").and_then(|p| p.get("sessionId"))
}

/// Notes a message given to no one, with its method and session only (never its content),
/// when `PEGBOARD_LINK_DEBUG` is set (with test hooks on).
pub(crate) fn dropped(why: &str, msg: Option<&Map<String, Value>>) {
    static ON: OnceLock<bool> = OnceLock::new();
    if !*ON.get_or_init(|| crate::testenv::var("PEGBOARD_LINK_DEBUG").is_some()) {
        return;
    }
    let field = |k: &str| msg.and_then(|m| m.get(k)).map(Value::to_string);
    eprintln!(
        "pegboard: link: dropped {why}: id={} method={} session={}",
        field("id").unwrap_or_default(),
        field("method").unwrap_or_default(),
        field("sessionId").unwrap_or_default(),
    );
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{json, Value};
    use tokio::sync::mpsc;

    use super::super::fake::FakeBrowser;
    use super::super::transport::{Incoming, Transport};
    use super::super::{BrowserLink, VirtualReceiver, VirtualSender};

    const WAIT: Duration = Duration::from_secs(3);
    const QUIET: Duration = Duration::from_millis(200);

    async fn open(link: &BrowserLink) -> (VirtualSender, VirtualReceiver) {
        link.open().await.unwrap().split()
    }

    async fn recv(rx: &mut VirtualReceiver) -> Value {
        let text = tokio::time::timeout(WAIT, rx.recv())
            .await
            .expect("nothing within 3s")
            .expect("the virtual connection ended");
        serde_json::from_str(&text).unwrap()
    }

    async fn nothing(rx: &mut VirtualReceiver) {
        if let Ok(got) = tokio::time::timeout(QUIET, rx.recv()).await {
            panic!("expected nothing, got {got:?}");
        }
    }

    fn send(tx: &VirtualSender, v: Value) {
        tx.send(v.to_string()).unwrap();
    }

    fn not_found(id: Value) -> Value {
        json!({"id": id, "error": {"code": -32001, "message": "Session with given id not found."}})
    }

    fn attached(on: &str, child: &str) -> Value {
        json!({"method": "Target.attachedToTarget", "sessionId": on,
            "params": {"sessionId": child, "waitingForDebugger": false,
                "targetInfo": {"targetId": format!("T{child}"), "type": "page"}}})
    }

    fn event(on: &str, method: &str) -> Value {
        json!({"method": method, "sessionId": on, "params": {}})
    }

    #[tokio::test]
    async fn ids_are_rewritten_and_restored() {
        let (link, mut fake) = FakeBrowser::start();
        let (s1, mut r1) = open(&link).await;
        let (s2, mut r2) = open(&link).await;

        send(&s1, json!({"id": 1, "method": "Browser.getVersion"}));
        send(&s2, json!({"id": 1, "method": "Browser.getVersion"}));
        let a = fake.next().await;
        let b = fake.next().await;
        assert_ne!(a["id"], b["id"], "the browser saw one id twice");
        let (m1, m2) = if a["sessionId"] == "B1" {
            (a, b)
        } else {
            (b, a)
        };
        assert_eq!(m1["sessionId"], "B1");
        assert_eq!(m2["sessionId"], "B2");

        // Answered in the other order: each gets its own answer, under its own id.
        fake.answer(&m2, json!({"who": "B2"}));
        fake.answer(&m1, json!({"who": "B1"}));
        assert_eq!(
            recv(&mut r1).await,
            json!({"id": 1, "result": {"who": "B1"}})
        );
        assert_eq!(
            recv(&mut r2).await,
            json!({"id": 1, "result": {"who": "B2"}})
        );
        nothing(&mut r1).await;
        nothing(&mut r2).await;

        // An id need not be a number; it comes back as it was.
        send(&s1, json!({"id": "x", "method": "Browser.getVersion"}));
        let m = fake.next().await;
        assert!(m["id"].is_u64());
        fake.answer(&m, json!({}));
        assert_eq!(recv(&mut r1).await, json!({"id": "x", "result": {}}));

        // A message with no id cannot be answered; it is refused.
        assert!(s1
            .send(json!({"method": "Browser.getVersion"}).to_string())
            .is_err());
        assert!(s1.send("not json".into()).is_err());
        fake.quiet().await;
    }

    #[tokio::test]
    async fn a_message_without_session_goes_to_its_browser_session() {
        let (link, mut fake) = FakeBrowser::start();
        let (s1, mut r1) = open(&link).await;
        let (s2, _r2) = open(&link).await;

        let params = json!({"discover": true, "filter": [{"type": "page"}]});
        send(
            &s1,
            json!({"id": 3, "method": "Target.setDiscoverTargets", "params": params}),
        );
        let m = fake.next().await;
        assert_eq!(m["sessionId"], "B1");
        assert_eq!(m["method"], "Target.setDiscoverTargets");
        assert_eq!(m["params"], params);

        send(&s2, json!({"id": 3, "method": "Target.getTargets"}));
        assert_eq!(fake.next().await["sessionId"], "B2");

        // Its own browser session by name is the same session.
        send(
            &s1,
            json!({"id": 4, "method": "Target.getTargets", "sessionId": "B1"}),
        );
        assert_eq!(fake.next().await["sessionId"], "B1");

        // Events on its browser session reach it as a connection of its own sees them: with
        // no session id.
        fake.send(event("B1", "Target.targetCreated"));
        assert_eq!(
            recv(&mut r1).await,
            json!({"method": "Target.targetCreated", "params": {}})
        );
    }

    #[tokio::test]
    async fn a_virtual_connection_cannot_use_another_ones_session() {
        let (link, mut fake) = FakeBrowser::start();
        let (_s1, mut r1) = open(&link).await;
        let (s2, mut r2) = open(&link).await;

        send(
            &s2,
            json!({"id": 7, "method": "Runtime.evaluate", "sessionId": "B1",
            "params": {"expression": "1"}}),
        );
        fake.quiet().await;
        assert_eq!(recv(&mut r2).await, not_found(json!(7)));

        // A session nobody has, and a session id that is not a string, likewise.
        send(
            &s2,
            json!({"id": 8, "method": "Runtime.evaluate", "sessionId": "NOPE"}),
        );
        send(
            &s2,
            json!({"id": 9, "method": "Runtime.evaluate", "sessionId": 1}),
        );
        fake.quiet().await;
        assert_eq!(recv(&mut r2).await, not_found(json!(8)));
        assert_eq!(recv(&mut r2).await, not_found(json!(9)));
        nothing(&mut r1).await;
    }

    #[tokio::test]
    async fn auto_attached_children_belong_to_the_attaching_session() {
        let (link, mut fake) = FakeBrowser::start();
        let (s1, mut r1) = open(&link).await;
        let (s2, mut r2) = open(&link).await;

        fake.send(attached("B1", "C1"));
        let got = recv(&mut r1).await;
        assert_eq!(got["method"], "Target.attachedToTarget");
        assert_eq!(got["params"]["sessionId"], "C1");
        assert!(got.get("sessionId").is_none());

        fake.send(event("C1", "Page.loadEventFired"));
        assert_eq!(recv(&mut r1).await, event("C1", "Page.loadEventFired"));
        nothing(&mut r2).await;

        // V1 may use C1, V2 may not.
        send(
            &s1,
            json!({"id": 1, "method": "Page.enable", "sessionId": "C1"}),
        );
        assert_eq!(fake.next().await["sessionId"], "C1");
        send(
            &s2,
            json!({"id": 2, "method": "Page.enable", "sessionId": "C1"}),
        );
        fake.quiet().await;
        assert_eq!(recv(&mut r2).await, not_found(json!(2)));

        // Both attached to the same tab: each session goes to the one that attached it.
        fake.send(attached("B2", "C2"));
        assert_eq!(recv(&mut r2).await["params"]["sessionId"], "C2");
        fake.send(event("C2", "Page.loadEventFired"));
        fake.send(event("C1", "Page.domContentEventFired"));
        assert_eq!(recv(&mut r2).await, event("C2", "Page.loadEventFired"));
        assert_eq!(
            recv(&mut r1).await,
            event("C1", "Page.domContentEventFired")
        );
        nothing(&mut r1).await;
        nothing(&mut r2).await;

        // An explicit attach: the session in the answer is the requester's.
        send(
            &s2,
            json!({"id": 5, "method": "Target.attachToTarget",
            "params": {"targetId": "T1", "flatten": true}}),
        );
        let m = fake.next().await;
        fake.answer(&m, json!({"sessionId": "P2"}));
        assert_eq!(
            recv(&mut r2).await,
            json!({"id": 5, "result": {"sessionId": "P2"}})
        );
        fake.send(event("P2", "Runtime.executionContextCreated"));
        assert_eq!(recv(&mut r2).await["sessionId"], "P2");
        nothing(&mut r1).await;
        send(
            &s1,
            json!({"id": 6, "method": "Runtime.enable", "sessionId": "P2"}),
        );
        fake.quiet().await;
        assert_eq!(recv(&mut r1).await, not_found(json!(6)));

        // Detached: the session is gone from the table.
        fake.send(
            json!({"method": "Target.detachedFromTarget", "sessionId": "B1",
            "params": {"sessionId": "C1"}}),
        );
        assert_eq!(recv(&mut r1).await["method"], "Target.detachedFromTarget");
        fake.send(event("C1", "Page.loadEventFired"));
        nothing(&mut r1).await;
        send(
            &s1,
            json!({"id": 7, "method": "Page.enable", "sessionId": "C1"}),
        );
        fake.quiet().await;
        assert_eq!(recv(&mut r1).await, not_found(json!(7)));
    }

    #[tokio::test]
    async fn nested_children_follow_their_parent() {
        let (link, mut fake) = FakeBrowser::start();
        let (s1, mut r1) = open(&link).await;
        let (s2, mut r2) = open(&link).await;

        fake.send(attached("B1", "C1"));
        recv(&mut r1).await;
        fake.send(attached("C1", "G1"));
        assert_eq!(recv(&mut r1).await, attached("C1", "G1"));
        fake.send(event("G1", "Runtime.consoleAPICalled"));
        assert_eq!(recv(&mut r1).await, event("G1", "Runtime.consoleAPICalled"));
        nothing(&mut r2).await;

        send(
            &s1,
            json!({"id": 1, "method": "Runtime.enable", "sessionId": "G1"}),
        );
        assert_eq!(fake.next().await["sessionId"], "G1");
        send(
            &s2,
            json!({"id": 1, "method": "Runtime.enable", "sessionId": "G1"}),
        );
        fake.quiet().await;
        assert_eq!(recv(&mut r2).await, not_found(json!(1)));

        // A grandchild of V2's stays V2's.
        fake.send(attached("B2", "C2"));
        fake.send(attached("C2", "G2"));
        fake.send(event("G2", "Runtime.consoleAPICalled"));
        recv(&mut r2).await;
        recv(&mut r2).await;
        assert_eq!(recv(&mut r2).await, event("G2", "Runtime.consoleAPICalled"));
        nothing(&mut r1).await;
    }

    #[tokio::test]
    async fn unknown_session_messages_go_nowhere() {
        let (link, mut fake) = FakeBrowser::start();
        let (s1, mut r1) = open(&link).await;
        let (_s2, mut r2) = open(&link).await;

        // A session nobody owns; an event on the root; a child announced on an unknown
        // session; an answer to a request nobody made.
        fake.send(event("Z", "Page.loadEventFired"));
        fake.send(json!({"method": "Target.targetCreated", "params": {"targetInfo": {}}}));
        fake.send(attached("Z", "ZC"));
        fake.send(event("ZC", "Page.loadEventFired"));
        fake.send(json!({"id": 999, "result": {}}));
        fake.send(json!({"id": 999, "result": {}, "sessionId": "B1"}));
        fake.send(json!("not an object"));
        nothing(&mut r1).await;
        nothing(&mut r2).await;

        // A child announced on the root belongs to nobody either.
        fake.send(json!({"method": "Target.attachedToTarget",
            "params": {"sessionId": "RC", "targetInfo": {}}}));
        fake.send(event("RC", "Page.loadEventFired"));
        nothing(&mut r1).await;
        send(
            &s1,
            json!({"id": 1, "method": "Page.enable", "sessionId": "RC"}),
        );
        fake.quiet().await;
        assert_eq!(recv(&mut r1).await, not_found(json!(1)));

        // The link still works.
        send(&s1, json!({"id": 2, "method": "Browser.getVersion"}));
        let m = fake.next().await;
        fake.answer(&m, json!({}));
        assert_eq!(recv(&mut r1).await, json!({"id": 2, "result": {}}));
    }

    #[tokio::test]
    async fn closing_a_virtual_connection_detaches_its_sessions() {
        let (link, mut fake) = FakeBrowser::start();
        let (s1, mut r1) = open(&link).await;
        let (s2, mut r2) = open(&link).await;
        fake.send(attached("B1", "C1"));
        fake.send(attached("C1", "G1"));
        recv(&mut r1).await;
        recv(&mut r1).await;

        // A clone keeps it open.
        let s1b = s1.clone();
        drop(s1);
        fake.quiet().await;
        drop(s1b);

        let mut detached = Vec::new();
        for _ in 0..3 {
            let m = fake.next().await;
            assert_eq!(m["method"], "Target.detachFromTarget");
            assert!(m.get("sessionId").is_none(), "not sent on the root: {m}");
            detached.push(m["params"]["sessionId"].as_str().unwrap().to_string());
        }
        assert_eq!(detached.last().map(String::as_str), Some("B1"));
        detached.sort();
        assert_eq!(detached, ["B1", "C1", "G1"]);
        fake.quiet().await;
        assert!(tokio::time::timeout(WAIT, r1.recv())
            .await
            .unwrap()
            .is_none());

        // Its sessions belong to nobody now.
        fake.send(event("C1", "Page.loadEventFired"));
        fake.send(event("B1", "Target.targetCreated"));
        nothing(&mut r2).await;
        send(
            &s2,
            json!({"id": 1, "method": "Page.enable", "sessionId": "C1"}),
        );
        fake.quiet().await;
        assert_eq!(recv(&mut r2).await, not_found(json!(1)));

        // The other one is untouched.
        send(&s2, json!({"id": 2, "method": "Browser.getVersion"}));
        let m = fake.next().await;
        assert_eq!(m["sessionId"], "B2");
        fake.answer(&m, json!({}));
        assert_eq!(recv(&mut r2).await, json!({"id": 2, "result": {}}));
    }

    #[tokio::test]
    async fn a_browser_session_the_browser_detaches_ends_its_connection() {
        let (link, mut fake) = FakeBrowser::start();
        let (s1, mut r1) = open(&link).await;
        let (_s2, mut r2) = open(&link).await;
        fake.send(attached("B1", "C1"));
        recv(&mut r1).await;

        fake.send(json!({"method": "Target.detachedFromTarget",
            "params": {"sessionId": "B1"}}));
        assert!(tokio::time::timeout(WAIT, r1.recv())
            .await
            .unwrap()
            .is_none());
        assert!(s1
            .send(json!({"id": 1, "method": "Browser.getVersion"}).to_string())
            .is_err());
        fake.send(event("C1", "Page.loadEventFired"));
        nothing(&mut r2).await;
        drop(s1);
        fake.quiet().await;
    }

    #[tokio::test]
    async fn link_end_ends_every_virtual_connection() {
        let (link, mut fake) = FakeBrowser::start();
        let (s1, mut r1) = open(&link).await;
        let (s2, mut r2) = open(&link).await;

        // A request the browser never answers.
        send(&s1, json!({"id": 1, "method": "Runtime.evaluate"}));
        fake.next().await;
        let waiting = tokio::spawn(async move { r1.recv().await });

        fake.disconnect();
        let answer = tokio::time::timeout(WAIT, waiting)
            .await
            .expect("a request still waits 3s after the link ended")
            .unwrap();
        assert_eq!(answer, None);
        assert_eq!(tokio::time::timeout(WAIT, r2.recv()).await.unwrap(), None);
        tokio::time::timeout(WAIT, link.closed()).await.unwrap();
        assert!(link.is_closed());

        assert!(s1
            .send(json!({"id": 2, "method": "Browser.getVersion"}).to_string())
            .is_err());
        assert!(s2
            .send(json!({"id": 2, "method": "Browser.getVersion"}).to_string())
            .is_err());
        assert!(tokio::time::timeout(WAIT, link.open())
            .await
            .unwrap()
            .is_err());
    }

    #[tokio::test]
    async fn a_cancelled_open_does_not_leak_its_session() {
        let (link, mut fake) = FakeBrowser::start();
        let mut opening = Box::pin(link.open());
        // Polled once: the request is out. Then the answer is routed while nobody polls.
        let pending = std::future::poll_fn(|cx| {
            std::task::Poll::Ready(std::future::Future::poll(opening.as_mut(), cx).is_pending())
        })
        .await;
        assert!(pending);
        tokio::time::sleep(QUIET).await;
        drop(opening);
        let m = fake.next().await;
        assert_eq!(m["method"], "Target.detachFromTarget");
        assert_eq!(m["params"]["sessionId"], "B1");
        assert!(m.get("sessionId").is_none(), "not sent on the root: {m}");
        // Nothing of it is left: its session belongs to nobody, the link works on.
        let (s2, mut r2) = open(&link).await;
        fake.send(event("B1", "Page.loadEventFired"));
        nothing(&mut r2).await;
        send(&s2, json!({"id": 1, "method": "Browser.getVersion"}));
        assert_eq!(fake.next().await["sessionId"], "B2");
    }

    #[tokio::test]
    async fn a_dropped_receiver_closes_the_connection() {
        let (link, mut fake) = FakeBrowser::start();
        let (s1, mut r1) = open(&link).await;
        let (_s2, mut r2) = open(&link).await;
        fake.send(attached("B1", "C1"));
        recv(&mut r1).await;
        drop(r1);
        fake.quiet().await;
        // The next message for it finds nobody reading: it is closed, its sessions detached.
        fake.send(event("C1", "Page.loadEventFired"));
        let mut detached = Vec::new();
        for _ in 0..2 {
            let m = fake.next().await;
            assert_eq!(m["method"], "Target.detachFromTarget");
            detached.push(m["params"]["sessionId"].as_str().unwrap().to_string());
        }
        assert_eq!(detached, ["C1", "B1"]);
        assert!(s1
            .send(json!({"id": 1, "method": "Browser.getVersion"}).to_string())
            .is_err());
        // An answer meets the same end.
        let (s3, r3) = open(&link).await;
        send(&s3, json!({"id": 1, "method": "Browser.getVersion"}));
        let m = fake.next().await;
        drop(r3);
        fake.answer(&m, json!({}));
        let m = fake.next().await;
        assert_eq!(m["method"], "Target.detachFromTarget");
        assert_eq!(m["params"]["sessionId"], "B3");
        fake.quiet().await;
        nothing(&mut r2).await;
    }

    #[tokio::test]
    async fn params_session_of_another_connection_is_refused() {
        let (link, mut fake) = FakeBrowser::start();
        let (s1, mut r1) = open(&link).await;
        let (s2, mut r2) = open(&link).await;
        fake.send(attached("B1", "C1"));
        recv(&mut r1).await;

        for method in ["Target.detachFromTarget", "Target.sendMessageToTarget"] {
            for (id, session) in [
                (1, json!("C1")),
                (2, json!("B1")),
                (3, json!("NOPE")),
                (4, json!(5)),
            ] {
                send(
                    &s2,
                    json!({"id": id, "method": method,
                        "params": {"sessionId": session, "message": "{}"}}),
                );
                fake.quiet().await;
                assert_eq!(
                    recv(&mut r2).await,
                    not_found(json!(id)),
                    "{method} {session}"
                );
            }
        }
        nothing(&mut r1).await;

        // Its owner may.
        send(
            &s1,
            json!({"id": 5, "method": "Target.detachFromTarget", "params": {"sessionId": "C1"}}),
        );
        let m = fake.next().await;
        assert_eq!(m["sessionId"], "B1");
        assert_eq!(m["params"]["sessionId"], "C1");
        // Without a session in the params (by target), it goes on as before.
        send(
            &s2,
            json!({"id": 6, "method": "Target.detachFromTarget", "params": {"targetId": "T1"}}),
        );
        assert_eq!(fake.next().await["sessionId"], "B2");
    }

    /// `CdpConn` rides a virtual connection unchanged: requests on its browser session,
    /// events without it, and `close` detaches.
    #[tokio::test]
    async fn a_cdp_connection_rides_a_virtual_connection() {
        let (link, mut fake) = FakeBrowser::start();
        let (conn, mut queue) = crate::cdp::CdpConn::over_queued(&link).await.unwrap();
        let reply = conn.send_now("Browser.getVersion", json!({}), None);
        let m = fake.next().await;
        assert_eq!(m["sessionId"], "B1");
        fake.answer(&m, json!({"product": "x"}));
        assert_eq!(reply.await.unwrap(), json!({"product": "x"}));
        fake.send(event("B1", "Target.targetCreated"));
        let got = tokio::time::timeout(WAIT, queue.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.method, "Target.targetCreated");
        assert_eq!(got.session_id, None);
        assert_eq!(link.virtual_connections(), 1);

        conn.close();
        let m = fake.next().await;
        assert_eq!(m["method"], "Target.detachFromTarget");
        assert_eq!(m["params"]["sessionId"], "B1");
        assert!(tokio::time::timeout(WAIT, queue.recv())
            .await
            .unwrap()
            .is_none());
        assert!(conn
            .send("Browser.getVersion", json!({}), None)
            .await
            .is_err());
        assert_eq!(link.virtual_connections(), 0);
    }

    #[tokio::test]
    async fn an_oversized_answer_ends_only_its_connection() {
        const MAX: usize = 1024;
        let big = "x".repeat(MAX);
        let (link, mut fake) = FakeBrowser::start();
        let (s1, mut r1) = open(&link).await;
        let (s2, mut r2) = open(&link).await;

        // V1's request answered with too large a message, on its browser session (Chrome
        // writes the session last).
        send(&s1, json!({"id": 1, "method": "Runtime.evaluate"}));
        let m = fake.next().await;
        let mut answer = json!({"id": m["id"], "result": {"value": big}});
        answer["sessionId"] = m["sessionId"].clone();
        fake.send_limited(answer, MAX);
        let error = recv(&mut r1).await;
        assert_eq!(error["id"], 1);
        assert_eq!(error["error"]["code"], -32000);
        assert!(tokio::time::timeout(WAIT, r1.recv())
            .await
            .unwrap()
            .is_none());
        let detach = fake.next().await;
        assert_eq!(detach["method"], "Target.detachFromTarget");
        assert_eq!(detach["params"]["sessionId"], "B1");
        assert!(s1
            .send(json!({"id": 2, "method": "Browser.getVersion"}).to_string())
            .is_err());

        // An oversized event on a child of V2's ends V2 alone; one nobody owns, nobody.
        let (s3, mut r3) = open(&link).await;
        fake.send(attached("B2", "C2"));
        recv(&mut r2).await;
        fake.send_limited(
            json!({"method": "Runtime.consoleAPICalled", "params": {"v": big}, "sessionId": "C2"}),
            MAX,
        );
        let mut detached = Vec::new();
        for _ in 0..2 {
            let m = fake.next().await;
            assert_eq!(m["method"], "Target.detachFromTarget");
            detached.push(m["params"]["sessionId"].as_str().unwrap().to_string());
        }
        assert_eq!(detached, ["C2", "B2"]);
        assert!(tokio::time::timeout(WAIT, r2.recv())
            .await
            .unwrap()
            .is_none());
        fake.send_limited(
            json!({"method": "Page.x", "params": {"v": big}, "sessionId": "NOPE"}),
            MAX,
        );
        fake.send_limited(json!({"method": "Page.x", "params": {"v": big}}), MAX);
        fake.send_limited(json!({"id": 999_999, "result": {"v": big}}), MAX);
        fake.quiet().await;
        assert!(!s2
            .send(json!({"id": 3, "method": "Browser.getVersion"}).to_string())
            .is_ok());

        // The link and V3 go on.
        assert!(!link.is_closed());
        send(&s3, json!({"id": 1, "method": "Browser.getVersion"}));
        let m = fake.next().await;
        assert_eq!(m["sessionId"], "B3");
        fake.answer(&m, json!({}));
        assert_eq!(recv(&mut r3).await, json!({"id": 1, "result": {}}));
    }

    #[test]
    fn the_ends_of_a_message_name_its_owner() {
        use super::{leading_id, trailing_session};
        assert_eq!(leading_id(b"{\"id\":42,\"result\":{"), Some(42));
        assert_eq!(leading_id(b" {\"id\":7,"), Some(7));
        // Cut off, or not first: not known.
        assert_eq!(leading_id(b"{\"id\":42"), None);
        assert_eq!(leading_id(b"{\"method\":\"x\",\"params\":{\"id\":4,"), None);
        assert_eq!(
            trailing_session(b"xx\"},\"sessionId\":\"AB12\"}"),
            Some("AB12")
        );
        assert_eq!(trailing_session(b",\"sessionId\":\"AB12\"}}"), None);
        assert_eq!(trailing_session(b"\"v\":\"a\"}}"), None);
        assert_eq!(trailing_session(b",\"sessionId\":\"\"}"), None);
    }

    #[tokio::test]
    async fn opening_fails_when_the_browser_refuses() {
        let (in_tx, in_rx) = mpsc::unbounded_channel::<Incoming>();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let link = BrowserLink::start(Transport {
            incoming: in_rx,
            outgoing: out_tx,
        });
        let opening = tokio::spawn({
            let link = link.clone();
            async move { link.open().await.map(|_| ()) }
        });
        let m: Value = serde_json::from_str(&out_rx.recv().await.unwrap()).unwrap();
        assert_eq!(m["method"], "Target.attachToBrowserTarget");
        in_tx
            .send(Incoming::Text(
                json!({"id": m["id"], "error": {"code": -32000, "message": "no"}}).to_string(),
            ))
            .unwrap();
        assert!(opening.await.unwrap().is_err());
        assert!(!link.is_closed());
    }

    /// The link hands on a backlog of messages for one virtual connection (one whose user
    /// does not read, say) without holding up another: a task woken by a message it routed
    /// runs while it goes on, not only once the whole backlog is through.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_backlog_for_one_connection_does_not_hold_up_another() {
        let (link, fake) = FakeBrowser::start();
        let (_s1, _r1) = open(&link).await;
        let (_s2, mut r2) = open(&link).await;
        let got = tokio::spawn(async move {
            r2.recv().await.expect("the virtual connection ended");
            std::time::Instant::now()
        });
        // The receiver waits before anything comes.
        tokio::time::sleep(QUIET).await;

        let mut big = event("B1", "Runtime.consoleAPICalled");
        big["params"]["value"] = "x".repeat(1 << 20).into();
        let big = big.to_string();
        let sent = std::time::Instant::now();
        fake.send(event("B2", "Page.loadEventFired"));
        for _ in 0..200 {
            fake.send_text(big.clone());
        }
        let latency = got.await.unwrap() - sent;
        assert!(
            latency < Duration::from_millis(750),
            "V2 got its message after {latency:?}"
        );
    }
}
