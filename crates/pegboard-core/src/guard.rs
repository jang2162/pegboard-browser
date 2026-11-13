//! The request guard: the daemon's own browser connection that judges every request of the
//! agent profiles by the address rules (`address`) before it leaves the browser.
//!
//! How (measured in the phase 2a spikes, G1-G5, D1-D4):
//! - One long-lived connection asks for browser-level auto-attach with
//!   `waitForDebuggerOnStart`, so every new tab and popup waits until the guard has turned
//!   request interception (`Fetch.enable`) on for it and resumes it. On every attached page
//!   and frame the guard asks for the same auto-attach again, so cross-site frames (OOPIF)
//!   and workers are caught too, before their first request.
//! - Targets of the human profile are resumed and let go at once: their requests are never
//!   paused, failed or changed. The extension's own targets are let go the same way.
//! - Each paused request is judged by `judge_request`: the URL first, then every address the
//!   daemon resolves its host name to (2 s limit; no answer fails the request).
//! - WebSocket handshakes never reach `Fetch` (spike B1). Documents that can run script are
//!   served again with a `Content-Security-Policy: connect-src` header derived from the
//!   address policy (`connect_src`; spike B2, round 3). The browser grants local-network
//!   access in agent profiles (`Browser::grant_local_network`), without which Chrome holds the
//!   local requests of such a document.
//! - A target the guard cannot set up is closed (a tab) or left waiting (a frame or worker):
//!   it never runs unguarded. If the connection ends, Chrome lifts the interception (G5):
//!   `ready()` is false until the guard has reconnected and the daemon refuses commands
//!   meanwhile.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use pegboard_protocol::SessionPolicy;
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use url::Url;

use crate::address::{self, judge_resolved, judge_url, Target, Verdict};
use crate::cdp::{CdpConn, CdpEvent};

/// The address policy that applies in a browser context (an agent profile).
pub type PolicyOf = Arc<dyn Fn(&str) -> SessionPolicy + Send + Sync>;
/// pegboard's own ports, read whenever a request is judged.
pub type OwnPorts = Arc<dyn Fn() -> Vec<u16> + Send + Sync>;

const RESOLVE_TIMEOUT: Duration = Duration::from_secs(2);
const RECONNECT_PAUSE: Duration = Duration::from_millis(500);

/// Kinds of target whose requests the guard intercepts on their own session.
const INTERCEPTED: &[&str] = &["page", "iframe", "shared_worker", "service_worker"];

/// Resolves host names for the guard. `PEGBOARD_TEST_HOST_RULES` (tests only) holds the
/// `--host-resolver-rules` the browser was started with; its `MAP <name> <ip>` entries are
/// answered from the rules, so the daemon and the browser agree on test names.
#[derive(Debug, Clone, Default)]
pub struct Resolver {
    map: HashMap<String, IpAddr>,
}

impl Resolver {
    pub fn from_env() -> Resolver {
        let rules = std::env::var("PEGBOARD_TEST_HOST_RULES").unwrap_or_default();
        Resolver::from_rules(&rules)
    }

    /// Reads the `MAP <name> <ip>` entries of a host resolver rule list; other entries are
    /// ignored.
    pub fn from_rules(rules: &str) -> Resolver {
        let map = rules
            .split(',')
            .filter_map(|rule| {
                let words: Vec<&str> = rule.split_whitespace().collect();
                match words.as_slice() {
                    ["MAP", name, ip] => {
                        let ip = ip.trim_start_matches('[').trim_end_matches(']');
                        Some((name.to_ascii_lowercase(), ip.parse().ok()?))
                    }
                    _ => None,
                }
            })
            .collect();
        Resolver { map }
    }

    pub async fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>> {
        if let Some(ip) = self.map.get(&host.to_ascii_lowercase()) {
            return Ok(vec![*ip]);
        }
        let found = tokio::time::timeout(RESOLVE_TIMEOUT, tokio::net::lookup_host((host, port)))
            .await
            .map_err(|_| anyhow!("resolving timed out"))??;
        Ok(found.map(|a| a.ip()).collect())
    }
}

/// Judges a request the way the guard does: the URL by the address rules, then (for a host
/// name that is not `localhost`) every address the name resolves to. A name that does not
/// resolve is refused.
pub async fn judge_request(
    url: &str,
    policy: &SessionPolicy,
    own_ports: &[u16],
    resolver: &Resolver,
) -> Verdict {
    let verdict = judge_url(url, policy, own_ports);
    if verdict != Verdict::Allow {
        return verdict;
    }
    let Ok(parsed) = Url::parse(url) else {
        return Verdict::Deny(address::SCHEME);
    };
    if !matches!(parsed.scheme(), "http" | "https" | "ws" | "wss") {
        return verdict;
    }
    let (Some(Target::Name(name)), Some(port)) =
        (address::target_of(&parsed), parsed.port_or_known_default())
    else {
        return verdict;
    };
    if address::is_loopback_name(&name) {
        return verdict;
    }
    let addresses = match resolver.resolve(&name, port).await {
        Ok(a) if !a.is_empty() => a,
        _ => return Verdict::Deny(address::UNRESOLVED),
    };
    for ip in addresses {
        let v = judge_resolved(ip, port, &name, policy, own_ports);
        if v != Verdict::Allow {
            return v;
        }
    }
    Verdict::Allow
}

/// The `Content-Security-Policy` an agent document is served with: `connect-src` derived
/// from the address policy. It is the only defence for WebSocket and EventSource, which
/// `Fetch` never sees (spike B1); `fetch` and XHR are judged by the guard as well.
/// - With an allow-list: the listed hosts and their subdomains, any port, over http(s) and
///   ws(s).
/// - Without one a CSP cannot say "everything but private addresses", so plain `ws:` is cut
///   down instead: http(s) and `wss:` anywhere, `ws:` only to the document's own origin
///   (`'self'`) and to loopback names (unless `no_localhost`), or anywhere with
///   `allow_private_network`.
///
/// `data:` and `blob:` stay open in every case (nothing leaves the browser).
pub fn connect_src(policy: &SessionPolicy) -> String {
    let mut sources: Vec<String> = Vec::new();
    if policy.allow_domains.is_empty() {
        sources.extend(["'self'", "http:", "https:", "wss:"].map(String::from));
        if policy.allow_private_network {
            sources.push("ws:".into());
        } else if !policy.no_localhost {
            for host in ["localhost", "*.localhost", "127.0.0.1", "[::1]"] {
                sources.push(format!("ws://{host}:*"));
            }
        }
    }
    for entry in &policy.allow_domains {
        let hosts = match entry.parse::<IpAddr>() {
            Ok(IpAddr::V6(v6)) => vec![format!("[{v6}]")],
            Ok(IpAddr::V4(v4)) => vec![v4.to_string()],
            Err(_) => {
                let name = entry.to_ascii_lowercase();
                vec![format!("*.{name}"), name]
            }
        };
        for host in hosts {
            for scheme in ["http", "https", "ws", "wss"] {
                sources.push(format!("{scheme}://{host}:*"));
            }
        }
    }
    sources.extend(["data:", "blob:"].map(String::from));
    format!("connect-src {}", sources.join(" "))
}

/// Whether a document response can run script that opens connections (it gets the CSP).
fn runs_script(headers: &[(String, String)]) -> bool {
    let Some((_, value)) = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
    else {
        return true;
    };
    let mime = value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    mime.is_empty()
        || mime.ends_with("+xml")
        || matches!(
            mime.as_str(),
            "text/html" | "application/xhtml+xml" | "text/xml" | "application/xml"
        )
}

fn is_attachment(headers: &[(String, String)]) -> bool {
    headers.iter().any(|(n, v)| {
        n.eq_ignore_ascii_case("content-disposition")
            && v.trim_start()
                .to_ascii_lowercase()
                .starts_with("attachment")
    })
}

/// The headers a document is served again with: the original ones without those that
/// described the bytes as they came over the wire, and the CSP.
fn reserved_headers(headers: &[(String, String)], csp: &str) -> Vec<Value> {
    let mut out: Vec<Value> = headers
        .iter()
        .filter(|(n, _)| {
            !["content-encoding", "content-length", "transfer-encoding"]
                .iter()
                .any(|drop| n.eq_ignore_ascii_case(drop))
        })
        .map(|(n, v)| json!({ "name": n, "value": v }))
        .collect();
    out.push(json!({ "name": "Content-Security-Policy", "value": csp }));
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Guarded,
    Failed,
}

#[derive(Debug, Clone)]
struct Attached {
    target: String,
    context: String,
}

#[derive(Default)]
struct State {
    /// The guard's CDP sessions on agent targets.
    sessions: HashMap<String, Attached>,
    /// Agent page targets and whether interception is on for them.
    pages: HashMap<String, Status>,
    /// Refused requests per reason, since the daemon started (diagnostics, no URLs).
    denied: BTreeMap<&'static str, u64>,
}

struct Inner {
    ws_url: String,
    human_context: String,
    policy_of: PolicyOf,
    own_ports: OwnPorts,
    resolver: Resolver,
    state: Mutex<State>,
    /// The connection in use, closed when the guard is dropped: Chrome would otherwise keep
    /// pausing requests that nobody answers.
    conn: Mutex<Option<CdpConn>>,
    /// Bumped whenever a page's status changes.
    changed: watch::Sender<u64>,
    /// True while the guard's connection is up and auto-attach is on.
    ready: watch::Sender<bool>,
}

pub struct Guard {
    inner: Arc<Inner>,
    supervisor: JoinHandle<()>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.supervisor.abort();
        if let Some(conn) = self.inner.conn.lock().unwrap().take() {
            conn.close();
        }
    }
}

impl Guard {
    /// Starts the guard on its own connection to the browser at `ws_url`, and keeps it
    /// connected (reconnecting after a loss). Must be called inside a tokio runtime.
    pub fn start(
        ws_url: &str,
        human_context: &str,
        policy_of: PolicyOf,
        own_ports: OwnPorts,
        resolver: Resolver,
    ) -> Guard {
        let inner = Arc::new(Inner {
            ws_url: ws_url.to_string(),
            human_context: human_context.to_string(),
            policy_of,
            own_ports,
            resolver,
            state: Mutex::new(State::default()),
            conn: Mutex::new(None),
            changed: watch::channel(0).0,
            ready: watch::channel(false).0,
        });
        let supervisor = tokio::spawn(supervise(Arc::downgrade(&inner)));
        Guard { inner, supervisor }
    }

    pub fn is_ready(&self) -> bool {
        *self.inner.ready.borrow()
    }

    /// Waits up to `limit` for the guard to be up.
    pub async fn wait_ready(&self, limit: Duration) -> Result<()> {
        let mut ready = self.inner.ready.subscribe();
        tokio::time::timeout(limit, ready.wait_for(|r| *r))
            .await
            .map_err(|_| anyhow!("pegboard's request guard is not running"))?
            .map_err(|_| anyhow!("pegboard's request guard stopped"))?;
        Ok(())
    }

    /// Waits up to `limit` until request interception is on for the agent tab `target`.
    /// Fails when the guard could not set it up (it then closed the tab) or did not in time.
    pub async fn wait_guarded(&self, target: &str, limit: Duration) -> Result<()> {
        let mut changed = self.inner.changed.subscribe();
        let wait = async {
            loop {
                let status = self.inner.state.lock().unwrap().pages.get(target).copied();
                match status {
                    Some(Status::Guarded) if self.is_ready() => return Ok(()),
                    Some(Status::Failed) => {
                        bail!("pegboard could not check the requests of the new tab")
                    }
                    _ => {}
                }
                if changed.changed().await.is_err() {
                    bail!("pegboard's request guard stopped");
                }
            }
        };
        tokio::time::timeout(limit, wait)
            .await
            .map_err(|_| anyhow!("pegboard could not check the requests of the new tab in time"))?
    }

    /// Requests refused per reason since the daemon started (diagnostics).
    #[doc(hidden)]
    pub fn denied(&self) -> BTreeMap<&'static str, u64> {
        self.inner.state.lock().unwrap().denied.clone()
    }
}

async fn supervise(inner: Weak<Inner>) {
    loop {
        let Some(guard) = inner.upgrade() else {
            return;
        };
        if let Ok((conn, events)) = guard.connect().await {
            *guard.conn.lock().unwrap() = Some(conn.clone());
            guard.clone().run(conn, events).await;
            guard.conn.lock().unwrap().take();
        }
        guard.ready.send_replace(false);
        {
            let mut state = guard.state.lock().unwrap();
            state.sessions.clear();
            state.pages.clear();
        }
        guard.changed.send_modify(|n| *n += 1);
        drop(guard);
        tokio::time::sleep(RECONNECT_PAUSE).await;
    }
}

impl Inner {
    async fn connect(&self) -> Result<(CdpConn, mpsc::UnboundedReceiver<CdpEvent>)> {
        let (conn, events) = CdpConn::connect_queued(&self.ws_url).await?;
        // Existing targets are attached as well (without waiting); new ones wait for us.
        let on = conn
            .send(
                "Target.setAutoAttach",
                json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
                None,
            )
            .await;
        if let Err(e) = on {
            conn.close();
            return Err(e);
        }
        Ok((conn, events))
    }

    /// Handles the connection's events until it ends.
    async fn run(self: Arc<Self>, conn: CdpConn, mut events: mpsc::UnboundedReceiver<CdpEvent>) {
        self.ready.send_replace(true);
        self.changed.send_modify(|n| *n += 1);
        while let Some(event) = events.recv().await {
            match event.method.as_str() {
                "Target.attachedToTarget" => self.on_attached(&conn, event),
                "Target.detachedFromTarget" => self.on_detached(&event),
                "Fetch.requestPaused" => {
                    let Some(session) = event.session_id.clone() else {
                        continue;
                    };
                    let context = {
                        let state = self.state.lock().unwrap();
                        state.sessions.get(&session).map(|a| a.context.clone())
                    };
                    let me = self.clone();
                    let conn = conn.clone();
                    tokio::spawn(async move {
                        me.on_paused(&conn, &session, context, &event.params).await;
                    });
                }
                _ => {}
            }
        }
    }

    fn on_attached(self: &Arc<Self>, conn: &CdpConn, event: CdpEvent) {
        let p = &event.params;
        let s = |ptr: &str| p.pointer(ptr).and_then(Value::as_str).unwrap_or_default();
        let session = s("/sessionId").to_string();
        let target = s("/targetInfo/targetId").to_string();
        let kind = s("/targetInfo/type").to_string();
        let url = s("/targetInfo/url").to_string();
        let context = s("/targetInfo/browserContextId").to_string();
        let waiting = p["waitingForDebugger"].as_bool().unwrap_or(false);
        // The session the event came on owns the new one (`None`: the browser's).
        let parent = event.session_id.clone();
        let foreign = context == self.human_context || url.starts_with("chrome-extension://");
        if !foreign && !session.is_empty() {
            let mut state = self.state.lock().unwrap();
            state.sessions.insert(
                session.clone(),
                Attached {
                    target: target.clone(),
                    context: context.clone(),
                },
            );
        }
        let me = self.clone();
        let conn = conn.clone();
        tokio::spawn(async move {
            if foreign {
                release(&conn, &session, parent.as_deref()).await;
            } else {
                me.set_up(&conn, &session, &target, &kind, waiting).await;
            }
        });
    }

    /// Turns interception on for an agent target and resumes it. A tab that cannot be
    /// guarded is closed; a frame or worker is left waiting.
    async fn set_up(&self, conn: &CdpConn, session: &str, target: &str, kind: &str, waiting: bool) {
        let s = Some(session);
        let result = async {
            if matches!(kind, "page" | "iframe") {
                conn.send(
                    "Target.setAutoAttach",
                    json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
                    s,
                )
                .await?;
            }
            let patterns = if matches!(kind, "page" | "iframe") {
                json!([
                    { "urlPattern": "*", "requestStage": "Request" },
                    { "urlPattern": "*", "resourceType": "Document", "requestStage": "Response" },
                ])
            } else {
                json!([{ "urlPattern": "*", "requestStage": "Request" }])
            };
            let enabled = conn
                .send("Fetch.enable", json!({ "patterns": patterns }), s)
                .await;
            // A dedicated worker has no Fetch domain; its requests pass through its page's
            // session (spike G1). Other kinds outside `INTERCEPTED` make no requests of
            // their own that the guard knows of.
            if INTERCEPTED.contains(&kind) {
                enabled?;
            }
            anyhow::Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                if waiting {
                    let _ = conn
                        .send("Runtime.runIfWaitingForDebugger", json!({}), s)
                        .await;
                }
                self.set_page(kind, target, Status::Guarded);
            }
            Err(_) => {
                self.set_page(kind, target, Status::Failed);
                if kind == "page" {
                    let _ = conn
                        .send("Target.closeTarget", json!({ "targetId": target }), None)
                        .await;
                }
            }
        }
    }

    fn set_page(&self, kind: &str, target: &str, status: Status) {
        if kind != "page" {
            return;
        }
        self.state
            .lock()
            .unwrap()
            .pages
            .insert(target.to_string(), status);
        self.changed.send_modify(|n| *n += 1);
    }

    fn on_detached(&self, event: &CdpEvent) {
        let Some(session) = event.params["sessionId"].as_str() else {
            return;
        };
        let mut state = self.state.lock().unwrap();
        if let Some(gone) = state.sessions.remove(session) {
            // A tab's session ends with the tab; keep a failure visible to a waiter.
            if state.pages.get(&gone.target) == Some(&Status::Guarded) {
                state.pages.remove(&gone.target);
            }
        }
    }

    async fn on_paused(
        &self,
        conn: &CdpConn,
        session: &str,
        context: Option<String>,
        params: &Value,
    ) {
        let request_id = params["requestId"].as_str().unwrap_or_default();
        let s = Some(session);
        let pass = || async {
            let _ = conn
                .send(
                    "Fetch.continueRequest",
                    json!({ "requestId": request_id }),
                    s,
                )
                .await;
        };
        let fail = || async {
            let _ = conn
                .send(
                    "Fetch.failRequest",
                    json!({ "requestId": request_id, "errorReason": "BlockedByClient" }),
                    s,
                )
                .await;
        };
        // Only the guard's own agent sessions are intercepted; anything else is unknown.
        let Some(context) = context else {
            return fail().await;
        };
        let policy = (self.policy_of)(&context);
        let response_stage = params.get("responseStatusCode").is_some()
            || params.get("responseErrorReason").is_some();
        if response_stage {
            return self.on_response(conn, session, &policy, params).await;
        }
        let url = params
            .pointer("/request/url")
            .and_then(Value::as_str)
            .unwrap_or("");
        // A blob: URL is read from the page's own memory; nothing leaves the browser.
        if url.starts_with("blob:") {
            return pass().await;
        }
        let own_ports = (self.own_ports)();
        match judge_request(url, &policy, &own_ports, &self.resolver).await {
            Verdict::Allow => pass().await,
            Verdict::Deny(reason) => {
                *self.state.lock().unwrap().denied.entry(reason).or_default() += 1;
                fail().await
            }
        }
    }

    /// A document's response: served again with the CSP when the document can run script;
    /// passed on otherwise.
    async fn on_response(
        &self,
        conn: &CdpConn,
        session: &str,
        policy: &SessionPolicy,
        params: &Value,
    ) {
        let request_id = params["requestId"].as_str().unwrap_or_default();
        let s = Some(session);
        let pass = || async {
            let _ = conn
                .send(
                    "Fetch.continueRequest",
                    json!({ "requestId": request_id }),
                    s,
                )
                .await;
        };
        let status = params["responseStatusCode"].as_u64().unwrap_or(0);
        let headers: Vec<(String, String)> = params["responseHeaders"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|h| {
                        let f = |k: &str| h[k].as_str().unwrap_or_default().to_string();
                        (f("name"), f("value"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let csp = connect_src(policy);
        // No document is made from a redirect, an empty answer or a download.
        let no_document = params.get("responseErrorReason").is_some()
            || (300..400).contains(&status)
            || matches!(status, 204 | 205)
            || is_attachment(&headers)
            || !runs_script(&headers);
        if no_document {
            return pass().await;
        }
        let served = async {
            let body = conn
                .send(
                    "Fetch.getResponseBody",
                    json!({ "requestId": request_id }),
                    s,
                )
                .await?;
            let text = body["body"].as_str().unwrap_or_default();
            let encoded = if body["base64Encoded"].as_bool().unwrap_or(false) {
                text.to_string()
            } else {
                data_encoding::BASE64.encode(text.as_bytes())
            };
            let mut fulfill = json!({
                "requestId": request_id,
                "responseCode": status,
                "responseHeaders": reserved_headers(&headers, &csp),
                "body": encoded,
            });
            if let Some(phrase) = params["responseStatusText"]
                .as_str()
                .filter(|p| !p.is_empty())
            {
                fulfill["responsePhrase"] = json!(phrase);
            }
            conn.send("Fetch.fulfillRequest", fulfill, s).await
        };
        if served.await.is_err() {
            // The document cannot get its CSP: it does not load.
            let _ = conn
                .send(
                    "Fetch.failRequest",
                    json!({ "requestId": request_id, "errorReason": "BlockedByClient" }),
                    s,
                )
                .await;
        }
    }
}

/// Lets go of a target the guard does not judge: resumes it (Chrome holds a target an
/// auto-attached session never resumed) and detaches.
async fn release(conn: &CdpConn, session: &str, parent: Option<&str>) {
    let _ = conn
        .send("Runtime.runIfWaitingForDebugger", json!({}), Some(session))
        .await;
    let _ = conn
        .send(
            "Target.detachFromTarget",
            json!({ "sessionId": session }),
            parent,
        )
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(domains: &[&str]) -> SessionPolicy {
        SessionPolicy {
            allow_domains: domains.iter().map(|d| d.to_string()).collect(),
            ..SessionPolicy::default()
        }
    }

    fn words(csp: &str) -> Vec<&str> {
        assert!(csp.starts_with("connect-src "), "{csp}");
        csp.split(' ').skip(1).collect()
    }

    #[test]
    fn with_an_allow_list_only_the_listed_hosts_can_be_connected_to() {
        let csp = connect_src(&listed(&["example.com", "10.0.0.1", "::1"]));
        let w = words(&csp);
        for source in [
            "http://example.com:*",
            "wss://*.example.com:*",
            "ws://10.0.0.1:*",
            "https://[::1]:*",
            "data:",
            "blob:",
        ] {
            assert!(w.contains(&source), "{source} missing in {csp}");
        }
        for wide in ["*", "'self'", "http:", "https:", "ws:", "wss:"] {
            assert!(!w.contains(&wide), "{wide} in {csp}");
        }
        assert!(!csp.contains("*.10.0.0.1"), "{csp}");
    }

    #[test]
    fn without_an_allow_list_plain_websockets_go_to_loopback_and_the_own_origin_only() {
        let w_of = |p: SessionPolicy| connect_src(&p);
        let default = w_of(SessionPolicy::default());
        let w = words(&default);
        for source in [
            "'self'",
            "http:",
            "https:",
            "wss:",
            "ws://localhost:*",
            "ws://127.0.0.1:*",
            "ws://[::1]:*",
            "data:",
            "blob:",
        ] {
            assert!(w.contains(&source), "{source} missing in {default}");
        }
        assert!(!w.contains(&"ws:") && !w.contains(&"*"), "{default}");
        let closed = w_of(SessionPolicy {
            no_localhost: true,
            ..SessionPolicy::default()
        });
        assert!(!closed.contains("ws://"), "{closed}");
        assert!(!words(&closed).contains(&"ws:"), "{closed}");
        let open = w_of(SessionPolicy {
            allow_private_network: true,
            ..SessionPolicy::default()
        });
        assert!(words(&open).contains(&"ws:"), "{open}");
    }

    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn which_documents_get_the_csp() {
        assert!(runs_script(&h(&[(
            "Content-Type",
            "text/html; charset=utf-8"
        )])));
        assert!(runs_script(&h(&[("content-type", "image/svg+xml")])));
        assert!(runs_script(&h(&[(
            "content-type",
            "application/xhtml+xml"
        )])));
        assert!(runs_script(&h(&[])));
        assert!(!runs_script(&h(&[("content-type", "application/pdf")])));
        assert!(!runs_script(&h(&[("content-type", "text/plain")])));
        assert!(is_attachment(&h(&[(
            "Content-Disposition",
            "attachment; filename=a.html"
        )])));
        assert!(!is_attachment(&h(&[("content-disposition", "inline")])));
    }

    #[test]
    fn served_again_without_wire_encoding_and_with_the_csp() {
        let out = reserved_headers(
            &h(&[
                ("Content-Type", "text/html"),
                ("Content-Encoding", "gzip"),
                ("content-length", "10"),
                ("Transfer-Encoding", "chunked"),
                ("Content-Security-Policy", "script-src 'self'"),
                ("Set-Cookie", "a=1"),
            ]),
            "connect-src data:",
        );
        let names: Vec<&str> = out.iter().map(|h| h["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "Content-Type",
                "Content-Security-Policy",
                "Set-Cookie",
                "Content-Security-Policy"
            ]
        );
        assert_eq!(out[3]["value"], "connect-src data:");
        // The page's own CSP stays: both apply.
        assert_eq!(out[1]["value"], "script-src 'self'");
    }

    #[test]
    fn test_host_rules_map_names_only() {
        let r = Resolver::from_rules("MAP private.test 10.255.255.1, MAP v6.test [fd00::1],EXCLUDE x.test, MAP bad.test nowhere");
        assert_eq!(
            r.map.get("private.test"),
            Some(&"10.255.255.1".parse().unwrap())
        );
        assert_eq!(r.map.get("v6.test"), Some(&"fd00::1".parse().unwrap()));
        assert_eq!(r.map.len(), 2);
        assert!(Resolver::from_rules("").map.is_empty());
    }

    #[tokio::test]
    async fn judge_request_uses_the_resolved_addresses() {
        let r = Resolver::from_rules(
            "MAP private.test 10.255.255.1, MAP loop.test 127.0.0.1, MAP pub.test 93.184.216.34",
        );
        let default = SessionPolicy::default();
        let ports = [9222];
        let judge = |url: &'static str, policy: SessionPolicy| {
            let r = r.clone();
            async move { judge_request(url, &policy, &ports, &r).await }
        };
        assert_eq!(
            judge("http://private.test/", default.clone()).await,
            Verdict::Deny(address::PRIVATE)
        );
        assert_eq!(
            judge("http://loop.test:9222/", default.clone()).await,
            Verdict::Deny(address::OWN_PORT)
        );
        assert_eq!(
            judge("http://loop.test:3000/", default.clone()).await,
            Verdict::Allow
        );
        assert_eq!(
            judge("ws://pub.test/", default.clone()).await,
            Verdict::Allow
        );
        let open = SessionPolicy {
            allow_private_network: true,
            ..SessionPolicy::default()
        };
        assert_eq!(judge("http://private.test/", open).await, Verdict::Allow);
        // Literal addresses and localhost are judged without a lookup.
        assert_eq!(
            judge("http://10.0.0.1/", default.clone()).await,
            Verdict::Deny(address::PRIVATE)
        );
        assert_eq!(
            judge("http://localhost:3000/", default.clone()).await,
            Verdict::Allow
        );
        // The URL rules come first.
        assert_eq!(
            judge("file:///etc/hosts", default.clone()).await,
            Verdict::Deny(address::SCHEME)
        );
        // A name nobody can resolve is refused (`.invalid` never resolves, RFC 6761).
        assert_eq!(
            judge("http://nothing.invalid/", default).await,
            Verdict::Deny(address::UNRESOLVED)
        );
    }
}
