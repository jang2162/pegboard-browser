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
//! - WebSocket handshakes never reach `Fetch` (spike B1). Documents from public addresses
//!   (every response but a download) are served again with a
//!   `Content-Security-Policy: connect-src` header derived from the address policy
//!   (`connect_src`; spike B2, round 3), their bodies read in pieces up to `MAX_BODY`, all
//!   bodies together within `BODY_BUDGET` and one profile's within `PROFILE_SHARE`.
//!   Documents from allowed loopback and private addresses pass unchanged (owner's ruling,
//!   `serves_again`).
//! - A target the guard cannot set up is closed (a tab) or left waiting (a frame or worker):
//!   it never runs unguarded. If the connection ends, Chrome lifts the interception (G5):
//!   `ready()` is false until the guard has reconnected and the daemon refuses commands
//!   meanwhile. After a reconnect the targets that are already there are set up before
//!   `ready()` is true again.
//! - The clipboard (phase 2a spikes C6-C12): unless the profile's sessions allow it, every
//!   page and frame gets `CLIPBOARD_SHIELD` before its first script, through this same
//!   held-from-the-start attachment, so the guard's session on it is older than any agent
//!   session (an older session's scripts run first and could disarm the shield, C12a). A
//!   frame attached already running (a sandboxed srcdoc frame, C12c) gets it into the running
//!   document too and is made to reload itself, so its next document has it first. After a
//!   reconnect the agents' connections of clipboard-denied sessions are dropped first
//!   (`BeforeReady`: no agent session stays older than the guard's), and the tabs that were
//!   there are shielded and reloaded (`clipboard_step`). The `clipboard-read`/
//!   `clipboard-write` denial that stops `navigator.clipboard` is the download policy
//!   connection's job (`downloads`).

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
/// Whether the sessions of a browser context (an agent profile) allow the clipboard:
/// `None` when no session holds the profile.
pub type ClipboardOf = Arc<dyn Fn(&str) -> Option<bool> + Send + Sync>;

/// The capture listeners that stop copy, cut and paste in a document: `L_hard2` of spike 23
/// (round 4, C12), which blocked every measured path together with the permission denial and
/// the recursive held attachment. The references it calls are saved before any page script
/// runs and called through `Reflect.apply`, so a page that replaces `Function.prototype.call`,
/// `preventDefault` or `addEventListener` later cannot disarm it (C12a, i1-i3). It leaves no
/// global behind.
///
/// `document.open()` (also the one `document.write` does on a document that is no longer
/// being parsed) removes every listener of the document and its window without making a new
/// global, so no new-document script runs again (review C2). The shield therefore wraps
/// `Document.prototype.open`, `write` and `writeln`: after the real `open` it adds its
/// listeners again, and a `write`/`writeln` on a document that is not loading opens it
/// first itself, so the listeners are back before anything written runs. The originals stay
/// inside the shield; the wrappers cannot be redefined (`configurable: false`), and a page may
/// still assign its own `document.write` on the instance (ad scripts do). Every value the
/// wrappers use later (getters, loop bounds) was saved at the start, and they loop by index,
/// so a page's later changes to `Array` or `Document` do not reach them.
pub const CLIPBOARD_SHIELD: &str = r#"(()=>{const ap=Reflect.apply,gd=Object.getOwnPropertyDescriptor,dp=Object.defineProperty,pd=Event.prototype.preventDefault,sip=Event.prototype.stopImmediatePropagation,ael=EventTarget.prototype.addEventListener,D=Document.prototype,op=D.open,wr=D.write,wl=D.writeln,rs=gd(D,"readyState").get,dv=gd(D,"defaultView").get,N=["copy","cut","paste","beforecopy","beforecut","beforepaste"],L=N.length;const f=(e)=>{ap(pd,e,[]);ap(sip,e,[])};const on=(d)=>{const T=[ap(dv,d,[]),d];for(let i=0;i<2;i++){const t=T[i];if(t)for(let j=0;j<L;j++)ap(ael,t,[N[j],f,true])}};on(document);const lock=(k,v)=>dp(D,k,{value:v,writable:true,configurable:false,enumerable:true});const opened=(d)=>{if(ap(rs,d,[])!=="loading"){ap(op,d,[]);on(d)}};lock("open",function open(){const r=ap(op,this,arguments);on(this);return r});lock("write",function write(){opened(this);return ap(wr,this,arguments)});lock("writeln",function writeln(){opened(this);return ap(wl,this,arguments)})})()"#;

/// What the guard does about the clipboard with a target it attached to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardStep {
    /// Nothing: the sessions allow the clipboard, or the target has no document.
    Leave,
    /// Adds `CLIPBOARD_SHIELD` for the target's next documents and, when it runs already,
    /// into its current one.
    Shield { run_immediately: bool },
    /// A tab that was there before the guard (re)connected: shielded, then reloaded (once
    /// interception is on), so that the shield is the first script of its next document.
    /// The agents' connections of its profile were dropped before (`BeforeReady`), so no
    /// agent session on it is older than the guard's (C12a).
    ShieldAndReload,
    /// A frame attached already running (a sandboxed srcdoc frame, spike C12c): its own first
    /// script ran before the shield, so the frame reloads itself (`location.reload()` in it)
    /// and its next document has the shield first.
    ShieldAndReloadFrame,
}

/// The clipboard step for a target of `kind`, attached `waiting` for the debugger or not,
/// in a profile whose sessions allow the clipboard (`Some(true)`), deny it (`Some(false)`) or
/// that no session holds (`None`: shielded, as the default denies). `early`: attached when
/// the guard (re)connected, set up before it counts as ready.
pub fn clipboard_step(
    kind: &str,
    waiting: bool,
    allowed: Option<bool>,
    early: bool,
) -> ClipboardStep {
    if !matches!(kind, "page" | "iframe") || allowed == Some(true) {
        return ClipboardStep::Leave;
    }
    if kind == "iframe" && !waiting {
        return ClipboardStep::ShieldAndReloadFrame;
    }
    if kind == "page" && !waiting && early && allowed == Some(false) {
        return ClipboardStep::ShieldAndReload;
    }
    ClipboardStep::Shield {
        run_immediately: !waiting,
    }
}

/// How long the shield's replies may take after the resume before a tab is closed.
const SHIELD_LIMIT: Duration = Duration::from_secs(15);

/// How often one frame target attached already running is reloaded. A frame that keeps
/// coming back running (a reload or navigation that attaches it again) beyond this has its
/// tab closed (fail closed) rather than being reloaded forever or left unshielded.
const FRAME_RELOADS: u32 = 3;
/// Frame targets whose reloads are counted at once; the counts start again past it.
const MAX_FRAME_RELOADS_KEPT: usize = 4096;

/// Called once a reconnected guard has turned auto-attach on and before it sets up the
/// targets that are there: drops the agents' connections of profiles whose sessions deny the
/// clipboard (`ProxyServer::drop_clients`), so no agent session is older than the guard's.
pub type BeforeReady = Arc<dyn Fn() -> futures_util::future::BoxFuture<'static, ()> + Send + Sync>;

const RESOLVE_TIMEOUT: Duration = Duration::from_secs(2);
const RECONNECT_PAUSE: Duration = Duration::from_millis(500);
/// Largest document body the guard serves again; a larger one (as sent or once decoded) is
/// failed. Read in `BODY_PIECE` pieces, so no single CDP message gets big.
pub const MAX_BODY: usize = 16 << 20;
const BODY_PIECE: usize = 1 << 20;
/// All document bodies held at once, across every tab: past this, a document is failed
/// rather than read (re-review N2: many large documents at once must not exhaust the
/// daemon's memory and take the guard down). Counted in bytes, in `BODY_UNIT` steps.
pub const BODY_BUDGET: usize = 128 << 20;
/// What one profile's documents may hold of `BODY_BUDGET` at once, so that one agent's
/// pages cannot take the whole budget from the other profiles' sessions.
pub const PROFILE_SHARE: usize = 32 << 20;
/// The step the body budget is counted in.
const BODY_UNIT: usize = 64 << 10;
/// Longest time reading one document body may take; past it the document is failed.
const BODY_DEADLINE: Duration = Duration::from_secs(20);
/// Request-stage resolutions of documents kept for their response stage; past this many
/// the table is emptied (those documents are then served again: fail closed).
const MAX_PENDING_DOCUMENTS: usize = 4096;
/// Attachments kept in the diagnostics log.
const ATTACH_LOG: usize = 256;
/// How long a failed tab's status is kept for a waiter.
const FAILED_KEPT: Duration = Duration::from_secs(60);

/// Kinds of target whose requests the guard intercepts on their own session.
const INTERCEPTED: &[&str] = &["page", "iframe", "shared_worker", "service_worker"];
/// How long the guard waits, after a (re)connect, for the targets that were already there to
/// be set up before it lets commands through.
const EARLY_SETUP_LIMIT: Duration = Duration::from_secs(10);

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
    judge_and_resolve(url, policy, own_ports, resolver).await.0
}

/// Where a request was judged to go: its host and port and the addresses it was judged on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub host: String,
    pub port: u16,
    pub addresses: Vec<IpAddr>,
}

/// `judge_request`, also returning the addresses an allowed http(s)/ws(s) request was judged
/// on: the resolved ones for a name, the literal for an IP, 127.0.0.1 for `*.localhost`.
pub async fn judge_and_resolve(
    url: &str,
    policy: &SessionPolicy,
    own_ports: &[u16],
    resolver: &Resolver,
) -> (Verdict, Option<Resolved>) {
    let verdict = judge_url(url, policy, own_ports);
    if verdict != Verdict::Allow {
        return (verdict, None);
    }
    let Ok(parsed) = Url::parse(url) else {
        return (Verdict::Deny(address::SCHEME), None);
    };
    if !matches!(parsed.scheme(), "http" | "https" | "ws" | "wss") {
        return (verdict, None);
    }
    let (Some(target), Some(port)) = (address::target_of(&parsed), parsed.port_or_known_default())
    else {
        return (verdict, None);
    };
    let resolved = |host: String, addresses: Vec<IpAddr>| Resolved {
        host,
        port,
        addresses,
    };
    let name = match target {
        Target::Ip(ip) => return (verdict, Some(resolved(ip.to_string(), vec![ip]))),
        Target::Name(name) if address::is_loopback_name(&name) => {
            let loopback = vec![IpAddr::from([127, 0, 0, 1])];
            return (verdict, Some(resolved(name, loopback)));
        }
        Target::Name(name) => name,
    };
    let addresses = match resolver.resolve(&name, port).await {
        Ok(a) if !a.is_empty() => a,
        _ => return (Verdict::Deny(address::UNRESOLVED), None),
    };
    for &ip in &addresses {
        let v = judge_resolved(ip, port, &name, policy, own_ports);
        if v != Verdict::Allow {
            return (v, None);
        }
    }
    (Verdict::Allow, Some(resolved(name, addresses)))
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
///
/// `None` when the CSP would restrict nothing (no allow-list and `allow_private_network`):
/// such documents are not served again. A served-again document loses its address space,
/// and Chrome then refuses its loopback and private requests outright when it is not a
/// secure context (an `http:` page on a name other than `localhost`).
pub fn connect_src(policy: &SessionPolicy) -> Option<String> {
    let mut sources: Vec<String> = Vec::new();
    if policy.allow_domains.is_empty() {
        if policy.allow_private_network {
            return None;
        }
        sources.extend(["'self'", "http:", "https:", "wss:"].map(String::from));
        if !policy.no_localhost {
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
    Some(format!("connect-src {}", sources.join(" ")))
}

/// Whether a document is served again with the CSP, given the addresses its request was
/// judged on at the request stage (`stored`; never resolved again, so a second DNS answer
/// cannot pick the outcome). Not when every one of them is a loopback or private address the
/// policy allows (owner's ruling): served again, such a document would lose its address
/// space, and Chrome refuses the local requests of an insecure document without one
/// (second-machine round 2), which breaks `http:` intranet and dev sites. Those documents
/// pass unchanged; their requests are still judged, their WebSockets are not covered.
/// Anything else is served again: a public address, a mix, no stored resolution.
pub fn serves_again(stored: Option<&Resolved>, policy: &SessionPolicy, own_ports: &[u16]) -> bool {
    !stored
        .is_some_and(|r| local_and_allowed(Some(&r.addresses), &r.host, r.port, policy, own_ports))
}

/// True when the host's addresses are known, all loopback or private, and all allowed.
fn local_and_allowed(
    addresses: Option<&[IpAddr]>,
    host: &str,
    port: u16,
    policy: &SessionPolicy,
    own_ports: &[u16],
) -> bool {
    let Some(addresses) = addresses.filter(|a| !a.is_empty()) else {
        return false;
    };
    addresses.iter().all(|&ip| {
        let local = address::is_loopback(ip) || address::is_private(ip);
        local && judge_resolved(ip, port, host, policy, own_ports) == Verdict::Allow
    })
}

/// Types a navigation saves as a download instead of showing: no document is made from
/// them. Chrome does not sniff `application/octet-stream` into a document.
fn is_download_type(mime: &str) -> bool {
    matches!(
        mime,
        "application/octet-stream"
            | "application/zip"
            | "application/gzip"
            | "application/x-gzip"
            | "application/x-tar"
            | "application/x-7z-compressed"
    )
}

/// Whether a response is a download rather than a document: every `Content-Type` value (all
/// headers, each comma-separated item) is a download type. Every other response, whatever
/// its type, becomes a document and gets the CSP: an image, a text or a PDF document has no
/// script of its own, but a same-origin page can run script in it (`frames[0].eval`, `new
/// frames[0].WebSocket(...)`) under that document's policy (re-review N1). A missing,
/// unknown or sniffed type is a document too.
fn is_download(headers: &[(String, String)]) -> bool {
    let types: Vec<String> = headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("content-type"))
        .flat_map(|(_, v)| v.split(','))
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .collect();
    !types.is_empty() && types.iter().all(|t| is_download_type(t))
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
fn reserved_headers(headers: &[(String, String)], csp: Option<&str>) -> Vec<Value> {
    let mut out: Vec<Value> = headers
        .iter()
        .filter(|(n, _)| {
            !["content-encoding", "content-length", "transfer-encoding"]
                .iter()
                .any(|drop| n.eq_ignore_ascii_case(drop))
        })
        .map(|(n, v)| json!({ "name": n, "value": v }))
        .collect();
    if let Some(csp) = csp {
        out.push(json!({ "name": "Content-Security-Policy", "value": csp }));
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Interception is on; `paused` when the guard held the target from its start (it was
    /// attached waiting for the debugger), so not one of its requests went unjudged.
    Guarded {
        paused: bool,
    },
    Failed(std::time::Instant),
}

/// A target the guard was attached to, as `set_up` needs it.
struct Attachment<'a> {
    session: &'a str,
    target: &'a str,
    kind: &'a str,
    context: &'a str,
    waiting: bool,
    /// Attached when the guard (re)connected (see `clipboard_step`).
    early: bool,
}

#[derive(Debug, Clone)]
struct Attached {
    target: String,
    context: String,
    kind: String,
    /// The guard's session it was attached through (`None`: the browser level).
    parent: Option<String>,
}

#[derive(Default)]
struct State {
    /// The guard's CDP sessions on agent targets.
    sessions: HashMap<String, Attached>,
    /// Agent page targets and whether interception is on for them.
    pages: HashMap<String, Status>,
    /// Request id of an allowed document request -> where it was judged to go, for its
    /// response stage (`serves_again`).
    documents: HashMap<String, Resolved>,
    /// Connections made so far (diagnostics: a reconnect shows as a jump).
    connections: u64,
    /// Documents served again with the CSP so far (diagnostics).
    served: u64,
    /// The last `ATTACH_LOG` attachments and what the guard did with them (diagnostics).
    attach_log: std::collections::VecDeque<String>,
    /// Refused requests per reason, since the daemon started (diagnostics, no URLs).
    denied: BTreeMap<&'static str, u64>,
    /// Frame target -> how often it was attached running and reloaded (`FRAME_RELOADS`).
    frame_reloads: HashMap<String, u32>,
    /// Targets attached when the guard (re)connected whose set-up is not done, with their
    /// context: commands of the context's clipboard-denied sessions wait for them
    /// (`early_pending`).
    early_pending: HashMap<String, String>,
    /// The current connection's event loop runs (`ready` is set only while it does).
    live: bool,
}

struct Inner {
    ws_url: String,
    human_context: String,
    policy_of: PolicyOf,
    clipboard_of: ClipboardOf,
    before_ready: BeforeReady,
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
    /// Test hook (`drop_connection_for_test`): no reconnect before this instant.
    hold_until: Mutex<Option<tokio::time::Instant>>,
    /// `BODY_UNIT`s of document bodies that may be held at once, all profiles together.
    body_budget: Arc<tokio::sync::Semaphore>,
    /// Browser context -> the `BODY_UNIT`s its documents may hold at once (`PROFILE_SHARE`).
    profile_budgets: Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>,
    /// Tests only (`PEGBOARD_TEST_HOOKS`): what the guard did with each document response is
    /// written to stderr (the daemon's log), with the URL's path only.
    trace: bool,
    /// Test hook: false serves documents again without adding the CSP header, to tell what
    /// the header does from what serving again does.
    csp_header: std::sync::atomic::AtomicBool,
    /// Test hook (`PEGBOARD_TEST_NO_CLIPBOARD_SHIELD` with `PEGBOARD_TEST_HOOKS`): no shield
    /// and no clipboard reloads, to tell what they do from the rest of the guard.
    no_shield: bool,
    /// Test hook: the set-up of targets there at a (re)connect waits this long first (a busy
    /// renderer).
    early_delay: Mutex<Duration>,
    /// Test hook: the shield of frames counts as failed.
    fail_shield: std::sync::atomic::AtomicBool,
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
        clipboard_of: ClipboardOf,
        before_ready: BeforeReady,
        own_ports: OwnPorts,
        resolver: Resolver,
    ) -> Guard {
        let inner = Arc::new(Inner {
            ws_url: ws_url.to_string(),
            human_context: human_context.to_string(),
            policy_of,
            clipboard_of,
            before_ready,
            own_ports,
            resolver,
            state: Mutex::new(State::default()),
            conn: Mutex::new(None),
            changed: watch::channel(0).0,
            ready: watch::channel(false).0,
            hold_until: Mutex::new(None),
            body_budget: Arc::new(tokio::sync::Semaphore::new(BODY_BUDGET / BODY_UNIT)),
            profile_budgets: Mutex::new(HashMap::new()),
            trace: std::env::var_os("PEGBOARD_TEST_HOOKS").is_some(),
            csp_header: std::sync::atomic::AtomicBool::new(true),
            no_shield: std::env::var_os("PEGBOARD_TEST_HOOKS").is_some()
                && std::env::var_os("PEGBOARD_TEST_NO_CLIPBOARD_SHIELD").is_some(),
            early_delay: Mutex::new(Duration::ZERO),
            fail_shield: std::sync::atomic::AtomicBool::new(false),
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
                    Some(Status::Guarded { paused: true }) if self.is_ready() => return Ok(()),
                    // Attached only after it had started (the guard was reconnecting): its
                    // first requests may have gone unjudged.
                    Some(Status::Guarded { paused: false }) => {
                        bail!("pegboard could not check the first requests of the new tab")
                    }
                    Some(Status::Failed(_)) => {
                        self.inner.state.lock().unwrap().pages.remove(target);
                        bail!("pegboard could not check the requests of the new tab")
                    }
                    Some(Status::Guarded { paused: true }) | None => {}
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

    /// What the guard did with its last attachments, oldest first (diagnostics): target id,
    /// kind, whose profile, whether it was waiting for the debugger, and the outcome.
    #[doc(hidden)]
    pub fn attach_log(&self) -> Vec<String> {
        self.inner
            .state
            .lock()
            .unwrap()
            .attach_log
            .iter()
            .cloned()
            .collect()
    }

    /// Documents served again with the CSP since the guard started (diagnostics).
    #[doc(hidden)]
    pub fn served(&self) -> u64 {
        self.inner.state.lock().unwrap().served
    }

    /// Connections the guard has made since it started (diagnostics).
    #[doc(hidden)]
    pub fn connections(&self) -> u64 {
        self.inner.state.lock().unwrap().connections
    }

    /// Test hook: whether documents served again get the CSP header (`false` only to measure
    /// what serving again does without it). The caller gates it.
    #[doc(hidden)]
    pub fn set_csp_header_for_test(&self, on: bool) {
        self.inner
            .csp_header
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether targets of `context` that were there when the guard (re)connected are still
    /// being set up (shielded, reloaded). `ready` does not wait past `EARLY_SETUP_LIMIT`; the
    /// commands of the context's clipboard-denied sessions do (`SessionManager`).
    pub fn early_pending(&self, context: &str) -> bool {
        self.inner
            .state
            .lock()
            .unwrap()
            .early_pending
            .values()
            .any(|c| c == context)
    }

    /// Test hook: the set-up of the targets there at the next (re)connect waits `delay`
    /// first. The caller gates it.
    #[doc(hidden)]
    pub fn set_early_delay_for_test(&self, delay: Duration) {
        *self.inner.early_delay.lock().unwrap() = delay;
    }

    /// Test hook: the shield of frames counts as failed. The caller gates it.
    #[doc(hidden)]
    pub fn set_fail_frame_shield_for_test(&self, on: bool) {
        self.inner
            .fail_shield
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// Test hook: closes the guard's connection as a failure would, and keeps it from
    /// reconnecting for `hold`. The caller gates it (`SessionManager`).
    #[doc(hidden)]
    pub fn drop_connection_for_test(&self, hold: Duration) {
        *self.inner.hold_until.lock().unwrap() = Some(tokio::time::Instant::now() + hold);
        self.inner.ready.send_replace(false);
        if let Some(conn) = self.inner.conn.lock().unwrap().as_ref() {
            conn.close();
        }
    }
}

async fn supervise(inner: Weak<Inner>) {
    loop {
        let Some(guard) = inner.upgrade() else {
            return;
        };
        let hold = *guard.hold_until.lock().unwrap();
        if let Some(until) = hold {
            tokio::time::sleep_until(until).await;
            guard.hold_until.lock().unwrap().take();
        }
        if let Ok((conn, events)) = guard.connect().await {
            *guard.conn.lock().unwrap() = Some(conn.clone());
            guard.state.lock().unwrap().connections += 1;
            guard.clone().run(conn.clone(), events).await;
            // Closed in every case: a connection left open would keep auto-attach on with
            // `waitForDebuggerOnStart`, holding new targets (the human's too) that nobody
            // resumes.
            conn.close();
            guard.conn.lock().unwrap().take();
        }
        guard.ready.send_replace(false);
        {
            let mut state = guard.state.lock().unwrap();
            state.sessions.clear();
            state.pages.clear();
            state.documents.clear();
            state.frame_reloads.clear();
            state.early_pending.clear();
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
        if self.hold_until.lock().unwrap().is_some() {
            return;
        }
        // The targets that were there already were attached while auto-attach was turned on
        // (their events are queued). They are set up in a task of their own, after a
        // reconnect only once the agents' connections that may hold sessions older than the
        // guard's are dropped (they connect again, younger), and `ready` waits for it. New
        // targets meanwhile (held from their start: the human's, the bridge's worker) are
        // handled at once by the loop below, never held up by this.
        let reconnect = {
            let mut state = self.state.lock().unwrap();
            state.live = true;
            state.connections > 1
        };
        let mut early = Vec::new();
        while let Ok(event) = events.try_recv() {
            if event.method == "Target.attachedToTarget" {
                early.push(event);
            } else {
                self.on_event(&conn, event);
            }
        }
        let readiness = {
            let me = self.clone();
            let conn = conn.clone();
            tokio::spawn(async move {
                if reconnect {
                    (me.before_ready)().await;
                }
                let handles: Vec<_> = early
                    .into_iter()
                    .map(|event| me.on_attached(&conn, event, true))
                    .collect();
                let set_up = async {
                    for handle in handles {
                        let _ = handle.await;
                    }
                };
                let _ = tokio::time::timeout(EARLY_SETUP_LIMIT, set_up).await;
                // Not for a connection that has ended meanwhile.
                let state = me.state.lock().unwrap();
                if state.live {
                    me.ready.send_replace(true);
                    me.changed.send_modify(|n| *n += 1);
                }
            })
        };
        while let Some(event) = events.recv().await {
            self.on_event(&conn, event);
        }
        self.state.lock().unwrap().live = false;
        readiness.abort();
    }

    fn on_event(self: &Arc<Self>, conn: &CdpConn, event: CdpEvent) {
        match event.method.as_str() {
            "Target.attachedToTarget" => drop(self.on_attached(conn, event, false)),
            "Target.detachedFromTarget" => self.on_detached(&event),
            "Fetch.requestPaused" => {
                let Some(session) = event.session_id.clone() else {
                    return;
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

    /// Sets up (or lets go) the attached target in a task of its own; the handle tells when
    /// that is done.
    fn on_attached(
        self: &Arc<Self>,
        conn: &CdpConn,
        event: CdpEvent,
        early: bool,
    ) -> JoinHandle<()> {
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
        // The bridge extension's service worker runs in every profile; it is let go like the
        // human's targets. Any other extension target in an agent profile is judged.
        let bridge_worker = kind == "service_worker" && url.starts_with("chrome-extension://");
        let foreign = context == self.human_context || bridge_worker;
        if !foreign && !session.is_empty() {
            let mut state = self.state.lock().unwrap();
            state.sessions.insert(
                session.clone(),
                Attached {
                    target: target.clone(),
                    context: context.clone(),
                    kind: kind.clone(),
                    parent: parent.clone(),
                },
            );
        }
        if early && !foreign {
            self.state
                .lock()
                .unwrap()
                .early_pending
                .insert(target.clone(), context.clone());
        }
        let me = self.clone();
        let conn = conn.clone();
        let whose = if context == me.human_context {
            "human"
        } else if context.is_empty() {
            "no-context"
        } else {
            "agent"
        };
        tokio::spawn(async move {
            let outcome = if foreign {
                release(&conn, &session, parent.as_deref()).await
            } else {
                let attached = Attachment {
                    session: &session,
                    target: &target,
                    kind: &kind,
                    context: &context,
                    waiting,
                    early,
                };
                me.set_up(&conn, attached).await
            };
            if early {
                me.state.lock().unwrap().early_pending.remove(&target);
                me.changed.send_modify(|n| *n += 1);
            }
            me.log_attach(format!(
                "{target} {kind} {whose} waiting={waiting} parent={} -> {outcome}",
                parent.is_some()
            ));
        })
    }

    fn log_attach(&self, line: String) {
        let mut state = self.state.lock().unwrap();
        if state.attach_log.len() == ATTACH_LOG {
            state.attach_log.pop_front();
        }
        state.attach_log.push_back(line);
    }

    /// Shields an agent target's documents from the clipboard events (`clipboard_step`),
    /// turns interception on for it and resumes it. A tab that cannot be guarded is closed; a
    /// frame or worker is left waiting. Returns what happened (for the diagnostics log).
    async fn set_up(&self, conn: &CdpConn, a: Attachment<'_>) -> String {
        if a.early {
            let delay = *self.early_delay.lock().unwrap();
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
        let Attachment {
            session,
            target,
            kind,
            context,
            waiting,
            ..
        } = a;
        let s = Some(session);
        let step = if self.no_shield {
            ClipboardStep::Leave
        } else {
            clipboard_step(kind, waiting, (self.clipboard_of)(context), a.early)
        };
        let run_immediately = match step {
            ClipboardStep::Leave => None,
            ClipboardStep::Shield { run_immediately } => Some(run_immediately),
            ClipboardStep::ShieldAndReload | ClipboardStep::ShieldAndReloadFrame => Some(true),
        };
        // The shield is written now, before anything else on this session, and its replies
        // are awaited only after the resume. Both are renderer-side commands: a held target
        // with no live renderer yet (a popup without opener, one that navigates to another
        // site or to a download) answers them only once its navigation goes on, which the
        // resume allows, so awaiting them first held such a popup forever (second machine,
        // Task 7). The session keeps them in order and hands them to the renderer when it
        // comes, before its first document runs.
        let shield = run_immediately.map(|run_immediately| {
            let enable = conn.send_now("Page.enable", json!({}), s);
            let mut params = json!({ "source": CLIPBOARD_SHIELD });
            if run_immediately {
                params["runImmediately"] = json!(true);
            }
            let add = conn.send_now("Page.addScriptToEvaluateOnNewDocument", params, s);
            async move {
                enable.await?;
                add.await?;
                anyhow::Ok(())
            }
        });
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
                let resumed = if waiting {
                    outcome(
                        conn.send("Runtime.runIfWaitingForDebugger", json!({}), s)
                            .await,
                    )
                } else {
                    "not-waiting".to_string()
                };
                if let Some(shield) = shield {
                    let forced = self.fail_shield.load(std::sync::atomic::Ordering::SeqCst)
                        && kind == "iframe";
                    let shielded = if forced {
                        Err(anyhow!("failed by a test"))
                    } else {
                        tokio::time::timeout(SHIELD_LIMIT, shield)
                            .await
                            .map_err(|_| anyhow!("no answer in time"))
                            .and_then(|r| r)
                    };
                    if let Err(e) = shielded {
                        // Fail closed: it already runs (resumed above) without the shield, so
                        // the tab (the frame's tab for a frame) is closed.
                        self.set_page(kind, target, Status::Failed(std::time::Instant::now()));
                        let closed = self.close_tab_of(conn, session).await;
                        return format!("resume={resumed} shield failed ({e:#}), {closed}");
                    }
                }
                self.set_page(kind, target, Status::Guarded { paused: waiting });
                let reloaded = self.reload_for_clipboard(conn, step, &a).await;
                format!("guarded resume={resumed}{reloaded}")
            }
            Err(e) => {
                self.set_page(kind, target, Status::Failed(std::time::Instant::now()));
                if kind == "page" {
                    let _ = conn
                        .send("Target.closeTarget", json!({ "targetId": target }), None)
                        .await;
                }
                format!("failed ({e:#})")
            }
        }
    }

    /// The reload a clipboard step asks for, once the target is guarded (its requests are
    /// judged). Returns what happened, for the diagnostics log.
    async fn reload_for_clipboard(
        &self,
        conn: &CdpConn,
        step: ClipboardStep,
        a: &Attachment<'_>,
    ) -> String {
        let s = Some(a.session);
        match step {
            ClipboardStep::ShieldAndReload => match conn.send("Page.reload", json!({}), s).await {
                Ok(_) => " reload=ok".to_string(),
                // Its document may not have the shield first: fail closed.
                Err(e) => format!(
                    " reload failed ({e:#}), {}",
                    self.close_tab_of(conn, a.session).await
                ),
            },
            ClipboardStep::ShieldAndReloadFrame => {
                let allowed = {
                    let mut state = self.state.lock().unwrap();
                    if state.frame_reloads.len() >= MAX_FRAME_RELOADS_KEPT {
                        state.frame_reloads.clear();
                    }
                    let n = state.frame_reloads.entry(a.target.to_string()).or_insert(0);
                    *n += 1;
                    *n <= FRAME_RELOADS
                };
                if !allowed {
                    return format!(
                        " frame-reload=limit, {}",
                        self.close_tab_of(conn, a.session).await
                    );
                }
                // `location` cannot be replaced by the page ([LegacyUnforgeable]).
                let r = conn
                    .send(
                        "Runtime.evaluate",
                        json!({ "expression": "location.reload()" }),
                        s,
                    )
                    .await;
                match r {
                    Ok(r) if r.get("exceptionDetails").is_none() => " frame-reload=ok".to_string(),
                    other => format!(
                        " frame-reload failed ({}), {}",
                        outcome(other),
                        self.close_tab_of(conn, a.session).await
                    ),
                }
            }
            _ => String::new(),
        }
    }

    /// Closes the tab that holds the guard's `session` (the tab itself, or the tab a frame
    /// sits in, found through the sessions the guard attached them by). Returns what
    /// happened, for the diagnostics log.
    async fn close_tab_of(&self, conn: &CdpConn, session: &str) -> String {
        let tab = {
            let state = self.state.lock().unwrap();
            let mut at = state.sessions.get(session);
            let mut hops = 0;
            loop {
                match at {
                    Some(a) if a.kind == "page" => break Some(a.target.clone()),
                    Some(a) if hops < 32 => {
                        at = a.parent.as_deref().and_then(|p| state.sessions.get(p));
                        hops += 1;
                    }
                    _ => break None,
                }
            }
        };
        let Some(tab) = tab else {
            return "its tab is unknown".to_string();
        };
        self.set_page("page", &tab, Status::Failed(std::time::Instant::now()));
        let r = conn
            .send("Target.closeTarget", json!({ "targetId": tab }), None)
            .await;
        format!("tab {tab} closed={}", outcome(r))
    }

    fn set_page(&self, kind: &str, target: &str, status: Status) {
        if kind != "page" {
            return;
        }
        {
            let mut state = self.state.lock().unwrap();
            // Failures nobody waited for do not pile up.
            state
                .pages
                .retain(|_, s| !matches!(s, Status::Failed(at) if at.elapsed() > FAILED_KEPT));
            state.pages.insert(target.to_string(), status);
        }
        self.changed.send_modify(|n| *n += 1);
    }

    fn on_detached(&self, event: &CdpEvent) {
        let Some(session) = event.params["sessionId"].as_str() else {
            return;
        };
        let mut state = self.state.lock().unwrap();
        if let Some(gone) = state.sessions.remove(session) {
            // A tab's session ends with the tab; a failure stays visible to a waiter for a
            // while (`FAILED_KEPT`).
            if matches!(state.pages.get(&gone.target), Some(Status::Guarded { .. })) {
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
            return self
                .on_response(conn, session, &context, &policy, params)
                .await;
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
        let (verdict, resolved) = judge_and_resolve(url, &policy, &own_ports, &self.resolver).await;
        match verdict {
            Verdict::Allow => {
                if params["resourceType"] == "Document" {
                    if let Some(resolved) = resolved {
                        let mut state = self.state.lock().unwrap();
                        if state.documents.len() >= MAX_PENDING_DOCUMENTS {
                            state.documents.clear();
                        }
                        // Under the interception id and, in case Chrome gives the response
                        // stage another one, under the request's network id (a redirect keeps
                        // that id: each hop's response stage takes it before the next hop
                        // stores it again).
                        if let Some(network) = params["networkId"].as_str() {
                            state
                                .documents
                                .insert(format!("net:{network}"), resolved.clone());
                        }
                        state.documents.insert(request_id.to_string(), resolved);
                    }
                }
                pass().await
            }
            Verdict::Deny(reason) => {
                *self.state.lock().unwrap().denied.entry(reason).or_default() += 1;
                fail().await
            }
        }
    }

    /// A document's response: served again with the CSP unless it is a download, a redirect
    /// or empty, or comes from an allowed local address; passed on otherwise.
    async fn on_response(
        &self,
        conn: &CdpConn,
        session: &str,
        context: &str,
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
        let fail = || async {
            let _ = conn
                .send(
                    "Fetch.failRequest",
                    json!({ "requestId": request_id, "errorReason": "BlockedByClient" }),
                    s,
                )
                .await;
        };
        // Where the request stage judged this document to go (taken in any case, so the
        // table does not keep it).
        let stored = {
            let mut state = self.state.lock().unwrap();
            let by_id = state.documents.remove(request_id);
            let by_network = params["networkId"]
                .as_str()
                .and_then(|n| state.documents.remove(&format!("net:{n}")));
            by_id.or(by_network)
        };
        let Some(csp) = connect_src(policy) else {
            if self.trace {
                eprintln!(
                    "pegboard guard: document status {status} -> passed (no CSP for this policy)"
                );
            }
            return pass().await;
        };
        // No document is made from a redirect, an empty answer, an error or a download.
        let has_location = headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("location"));
        let no_document = params.get("responseErrorReason").is_some()
            || (matches!(status, 301 | 302 | 303 | 307 | 308) && has_location)
            || matches!(status, 204 | 205)
            || is_attachment(&headers)
            || is_download(&headers);
        let traced = |outcome: &str| {
            if self.trace {
                let path = params
                    .pointer("/request/url")
                    .and_then(Value::as_str)
                    .and_then(|u| Url::parse(u).ok())
                    .map(|u| u.path().to_string())
                    .unwrap_or_default();
                let types: Vec<&str> = headers
                    .iter()
                    .filter(|(n, _)| n.eq_ignore_ascii_case("content-type"))
                    .map(|(_, v)| v.as_str())
                    .collect();
                let error = params["responseErrorReason"].as_str().unwrap_or("-");
                let network = params["networkId"].as_str().unwrap_or("-");
                let kind = params["resourceType"].as_str().unwrap_or("-");
                eprintln!(
                    "pegboard guard: document {path} status {status} type {types:?} \
                     error {error} resource {kind} network {network} session {session} -> {outcome}"
                );
            }
        };
        if no_document {
            traced("passed (no document)");
            return pass().await;
        }
        let own_ports = (self.own_ports)();
        if !serves_again(stored.as_ref(), policy, &own_ports) {
            traced("passed (allowed local address)");
            return pass().await;
        }
        let budget = Budget {
            global: self.body_budget.clone(),
            profile: self
                .profile_budgets
                .lock()
                .unwrap()
                .entry(context.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(PROFILE_SHARE / BODY_UNIT)))
                .clone(),
            unit: BODY_UNIT,
        };
        let served = async {
            let read = read_body_capped(
                conn,
                session,
                request_id,
                &headers,
                MAX_BODY,
                &budget,
                BODY_DEADLINE,
            );
            let Some((body, _held)) = read.await? else {
                bail!("the document is larger than {MAX_BODY} bytes");
            };
            let mut fulfill = json!({
                "requestId": request_id,
                "responseCode": status,
                "responseHeaders": reserved_headers(
                    &headers,
                    self.csp_header
                        .load(std::sync::atomic::Ordering::SeqCst)
                        .then_some(csp.as_str()),
                ),
                "body": data_encoding::BASE64.encode(&body),
            });
            if let Some(phrase) = params["responseStatusText"]
                .as_str()
                .filter(|p| !p.is_empty())
            {
                fulfill["responsePhrase"] = json!(phrase);
            }
            conn.send("Fetch.fulfillRequest", fulfill, s).await
        };
        let served = served.await;
        match &served {
            Ok(_) => {
                self.state.lock().unwrap().served += 1;
                traced("served again with the CSP")
            }
            Err(e) => traced(&format!("failed: {e:#}")),
        }
        if served.is_err() {
            // The document cannot get its CSP (too large, or the body could not be read): it
            // does not load.
            *self
                .state
                .lock()
                .unwrap()
                .denied
                .entry(NOT_SERVED)
                .or_default() += 1;
            fail().await;
        }
    }
}

/// Diagnostics key for documents failed because they could not be served again.
pub const NOT_SERVED: &str = "document could not be served again";

/// The budgets a body is read under: all profiles together (`global`) and the document's
/// own profile (`profile`), each a count of `unit`-byte steps.
pub struct Budget {
    pub global: Arc<tokio::sync::Semaphore>,
    pub profile: Arc<tokio::sync::Semaphore>,
    pub unit: usize,
}

/// A body read under the budget, with the budget it holds until dropped.
type HeldBody = (Vec<u8>, Vec<tokio::sync::OwnedSemaphorePermit>);

/// The body of a paused response, at most `limit` bytes, or `None` when it is larger.
/// A declared `Content-Length` over the limit is refused without reading; otherwise the body
/// is taken as a stream and read in pieces of at most `BODY_PIECE` (as Chrome decodes it, so
/// a small compressed body that grows past the limit is caught too).
///
/// The bytes read so far are held against `budget`, in `unit` steps, from both the global
/// and the profile's semaphore, never waiting: when either has no room the read fails (fail
/// closed). Counted in bytes, so many small pieces cost what their bytes cost. The permits
/// come back with the body and are released when the caller drops them. Reading stops after
/// `deadline`; the stream is closed whatever happened.
async fn read_body_capped(
    conn: &CdpConn,
    session: &str,
    request_id: &str,
    headers: &[(String, String)],
    limit: usize,
    budget: &Budget,
    deadline: Duration,
) -> Result<Option<HeldBody>> {
    let declared = headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        .filter_map(|(_, v)| v.trim().parse::<u64>().ok())
        .max();
    if declared.is_some_and(|n| n > limit as u64) {
        return Ok(None);
    }
    let s = Some(session);
    let stream = conn
        .send(
            "Fetch.takeResponseBodyAsStream",
            json!({ "requestId": request_id }),
            s,
        )
        .await?;
    let handle = stream["stream"]
        .as_str()
        .ok_or_else(|| anyhow!("no stream handle"))?
        .to_string();
    let read = async {
        let mut body = Vec::new();
        let mut held = Vec::new();
        let mut held_units = 0usize;
        loop {
            let piece = conn
                .send(
                    "IO.read",
                    json!({ "handle": handle, "size": BODY_PIECE }),
                    s,
                )
                .await?;
            let data = piece["data"].as_str().unwrap_or_default();
            if piece["base64Encoded"].as_bool().unwrap_or(false) {
                let bytes = data_encoding::BASE64
                    .decode(data.as_bytes())
                    .map_err(|e| anyhow!("bad base64 from IO.read: {e}"))?;
                body.extend_from_slice(&bytes);
            } else {
                body.extend_from_slice(data.as_bytes());
            }
            if body.len() > limit {
                return Ok(None);
            }
            let needed = body.len().div_ceil(budget.unit);
            if needed > held_units {
                let more = u32::try_from(needed - held_units)?;
                let global = budget
                    .global
                    .clone()
                    .try_acquire_many_owned(more)
                    .map_err(|_| anyhow!("over the guard's budget for document bodies"))?;
                let profile = budget
                    .profile
                    .clone()
                    .try_acquire_many_owned(more)
                    .map_err(|_| anyhow!("over the profile's share of the body budget"))?;
                held.push(global);
                held.push(profile);
                held_units = needed;
            }
            if piece["eof"].as_bool().unwrap_or(false) {
                return Ok(Some((body, held)));
            }
        }
    };
    let result = tokio::time::timeout(deadline, read).await;
    // Outside the time limit, so a read that ran out of time still closes its stream.
    let _ = conn.send("IO.close", json!({ "handle": handle }), s).await;
    result.map_err(|_| anyhow!("reading the document took too long"))?
}

/// Lets go of a target the guard does not judge: resumes it (Chrome holds a target an
/// auto-attached session never resumed) and detaches.
async fn release(conn: &CdpConn, session: &str, parent: Option<&str>) -> String {
    let resumed = conn
        .send("Runtime.runIfWaitingForDebugger", json!({}), Some(session))
        .await;
    let detached = conn
        .send(
            "Target.detachFromTarget",
            json!({ "sessionId": session }),
            parent,
        )
        .await;
    format!(
        "released resume={} detach={}",
        outcome(resumed),
        outcome(detached)
    )
}

fn outcome(r: Result<Value>) -> String {
    match r {
        Ok(_) => "ok".into(),
        Err(e) => format!("err({e:#})"),
    }
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

    #[test]
    fn what_the_guard_does_for_the_clipboard() {
        use ClipboardStep::*;
        let new_doc = Shield {
            run_immediately: false,
        };
        let running = Shield {
            run_immediately: true,
        };
        // (kind, waiting, allowed, set up before the guard counts as ready, step)
        let table: &[(&str, bool, Option<bool>, bool, ClipboardStep)] = &[
            // New pages and frames, held from their start: the shield before the first script.
            ("page", true, Some(false), false, new_doc),
            ("iframe", true, Some(false), false, new_doc),
            // A context no session holds: shielded too (closed unless allowed).
            ("page", true, None, false, new_doc),
            // A page attached a second time, not waiting (a popup is attached through the
            // browser level and through its opener's session): into the running document.
            // Never closed or reloaded: its first attachment held it.
            ("page", false, Some(false), false, running),
            ("page", false, None, false, running),
            // A tab that was there before the guard (re)connected, in a profile whose
            // sessions deny the clipboard: shielded and reloaded once the agents' older
            // connections are gone, so the shield is the first script of its next document.
            ("page", false, Some(false), true, ShieldAndReload),
            // ... but a tab no session's policy is about is shielded where it stands.
            ("page", false, None, true, running),
            // A frame that runs already (a sandboxed srcdoc frame, spike C12c): its own
            // first script ran before the shield, so it is reloaded (shield first).
            ("iframe", false, Some(false), false, ShieldAndReloadFrame),
            ("iframe", false, None, false, ShieldAndReloadFrame),
            ("iframe", false, Some(false), true, ShieldAndReloadFrame),
            // Sessions that allow the clipboard: nothing.
            ("page", true, Some(true), false, Leave),
            ("page", false, Some(true), true, Leave),
            ("iframe", false, Some(true), false, Leave),
            // Workers have no document.
            ("service_worker", true, Some(false), false, Leave),
            ("shared_worker", false, None, true, Leave),
            ("worker", true, Some(false), false, Leave),
        ];
        for (kind, waiting, allowed, early, step) in table {
            assert_eq!(
                clipboard_step(kind, *waiting, *allowed, *early),
                *step,
                "{kind} waiting={waiting} allowed={allowed:?} early={early}"
            );
        }
    }

    #[test]
    fn the_shield_is_the_measured_script() {
        // L_hard2 of spike 23 (round 4, C12): saved references called through Reflect.apply,
        // capture listeners on the window and the document for every clipboard event.
        for part in [
            "const ap=Reflect.apply",
            "pd=Event.prototype.preventDefault",
            "sip=Event.prototype.stopImmediatePropagation",
            "ael=EventTarget.prototype.addEventListener",
            "ap(pd,e,[]);ap(sip,e,[])",
            "const T=[ap(dv,d,[]),d]",
            r#"["copy","cut","paste","beforecopy","beforecut","beforepaste"]"#,
            "ap(ael,t,[N[j],f,true])",
            "on(document)",
            // document.open clears listeners (review C2): put back after it, and before a
            // write that opens.
            r#"lock("open",function open(){const r=ap(op,this,arguments);on(this);return r})"#,
            r#"if(ap(rs,d,[])!=="loading"){ap(op,d,[]);on(d)}"#,
            r#"lock("write",function write(){opened(this);"#,
            r#"lock("writeln",function writeln(){opened(this);"#,
            "configurable:false",
        ] {
            assert!(CLIPBOARD_SHIELD.contains(part), "{part}");
        }
        // Nothing the page can change later is iterated (no for...of in the wrappers).
        assert_eq!(CLIPBOARD_SHIELD.matches(" of ").count(), 0);
    }

    fn words(csp: &str) -> Vec<&str> {
        assert!(csp.starts_with("connect-src "), "{csp}");
        csp.split(' ').skip(1).collect()
    }

    #[test]
    fn with_an_allow_list_only_the_listed_hosts_can_be_connected_to() {
        let csp = connect_src(&listed(&["example.com", "10.0.0.1", "::1"])).unwrap();
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
        let w_of = |p: SessionPolicy| connect_src(&p).unwrap();
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
        // Nothing to restrict: the document is passed on as it came.
        for no_localhost in [false, true] {
            let open = SessionPolicy {
                allow_private_network: true,
                no_localhost,
                ..SessionPolicy::default()
            };
            assert_eq!(connect_src(&open), None);
        }
        // With an allow-list there is always a CSP.
        let listed_open = SessionPolicy {
            allow_private_network: true,
            ..listed(&["a.test"])
        };
        assert!(connect_src(&listed_open).is_some());
    }

    #[tokio::test]
    async fn documents_on_allowed_local_addresses_are_not_served_again() {
        let r = Resolver::from_rules(
            "MAP pub.test 93.184.216.34, MAP loop.test 127.0.0.1, MAP lan.test 192.168.1.20",
        );
        let ports = [9222];
        let default = SessionPolicy::default();
        let private = SessionPolicy {
            allow_private_network: true,
            ..SessionPolicy::default()
        };
        // As the guard does it: the request stage judges and resolves, the response stage
        // decides on what the request stage stored.
        let again = |url: &'static str, p: SessionPolicy| {
            let r = r.clone();
            async move {
                let (_, stored) = judge_and_resolve(url, &p, &ports, &r).await;
                serves_again(stored.as_ref(), &p, &ports)
            }
        };
        // Public: served again.
        assert!(again("http://pub.test/", default.clone()).await);
        assert!(again("http://93.184.216.34/", default.clone()).await);
        // Loopback, allowed: passed unchanged (name, literal, localhost names).
        assert!(!again("http://loop.test:3000/", default.clone()).await);
        assert!(!again("http://127.0.0.1:3000/", default.clone()).await);
        assert!(!again("http://[::1]:3000/", default.clone()).await);
        assert!(!again("http://a.localhost/", default.clone()).await);
        // Loopback, not allowed by the policy: nothing stored (the request was refused), so
        // served again.
        let closed = SessionPolicy {
            no_localhost: true,
            ..SessionPolicy::default()
        };
        assert!(again("http://loop.test/", closed).await);
        // Private: passed when allowed, served again when not.
        assert!(!again("http://lan.test/", private.clone()).await);
        assert!(!again("http://10.0.0.1/", private.clone()).await);
        assert!(again("http://lan.test/", default.clone()).await);
        assert!(again("http://10.0.0.1/", default.clone()).await);
        // A name that does not resolve, or no host: served again (fail closed).
        assert!(again("http://nothing.invalid/", private.clone()).await);
        assert!(again("not a url", private.clone()).await);
    }

    #[test]
    fn the_response_stage_uses_the_request_stage_resolution_only() {
        let ports = [9222];
        let default = SessionPolicy::default();
        let stored = |addresses: &[&str]| Resolved {
            host: "rebind.test".into(),
            port: 80,
            addresses: addresses.iter().map(|a| a.parse().unwrap()).collect(),
        };
        // The request went to a public address: served again, whatever a later lookup of the
        // name would say (there is no later lookup).
        assert!(serves_again(
            Some(&stored(&["93.184.216.34"])),
            &default,
            &ports
        ));
        // It went to an allowed loopback address: passed unchanged.
        assert!(!serves_again(
            Some(&stored(&["127.0.0.1"])),
            &default,
            &ports
        ));
        // One public address among them: served again.
        assert!(serves_again(
            Some(&stored(&["127.0.0.1", "93.184.216.34"])),
            &default,
            &ports
        ));
        // Nothing stored (the request stage never saw it, or the table was emptied): served
        // again.
        assert!(serves_again(None, &default, &ports));
    }

    #[test]
    fn local_and_allowed_needs_every_address() {
        let ports = [9222];
        let default = SessionPolicy::default();
        let private = SessionPolicy {
            allow_private_network: true,
            ..SessionPolicy::default()
        };
        // Every address must be local and allowed; one public address serves again.
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(local_and_allowed(
            Some(&[ip("10.0.0.1"), ip("127.0.0.1")]),
            "m",
            80,
            &private,
            &ports
        ));
        assert!(!local_and_allowed(
            Some(&[ip("10.0.0.1"), ip("8.8.8.8")]),
            "m",
            80,
            &private,
            &ports
        ));
        assert!(!local_and_allowed(
            Some(&[ip("127.0.0.1")]),
            "m",
            9222,
            &default,
            &ports
        ));
        assert!(!local_and_allowed(None, "m", 80, &default, &ports));
        assert!(!local_and_allowed(Some(&[]), "m", 80, &default, &ports));
    }

    /// A CDP endpoint that answers `Fetch.takeResponseBodyAsStream` and `IO.read` with a body
    /// of `total` bytes in pieces of `piece`, counts the reads and records `IO.close`. With
    /// `stall`, it never answers `IO.read`.
    async fn body_server(
        total: usize,
        piece: usize,
        stall: bool,
    ) -> (
        String,
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<std::sync::atomic::AtomicBool>,
    ) {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (counted, seen_close) = (reads.clone(), closed.clone());
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
            let mut sent = 0usize;
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let v: Value = serde_json::from_str(&text).unwrap();
                let result = match v["method"].as_str().unwrap() {
                    "Fetch.takeResponseBodyAsStream" => json!({ "stream": "s1" }),
                    "IO.read" => {
                        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if stall {
                            continue;
                        }
                        let n = piece.min(total - sent);
                        sent += n;
                        let data = data_encoding::BASE64.encode(&vec![b'a'; n]);
                        json!({ "data": data, "base64Encoded": true, "eof": sent == total })
                    }
                    "IO.close" => {
                        seen_close.store(true, std::sync::atomic::Ordering::SeqCst);
                        json!({})
                    }
                    _ => json!({}),
                };
                let reply = json!({ "id": v["id"], "result": result }).to_string();
                if ws.send(Message::text(reply)).await.is_err() {
                    return;
                }
            }
        });
        (url, reads, closed)
    }

    /// A budget of `global` and `profile` steps of `unit` bytes.
    fn budget(global: usize, profile: usize, unit: usize) -> Budget {
        Budget {
            global: Arc::new(tokio::sync::Semaphore::new(global)),
            profile: Arc::new(tokio::sync::Semaphore::new(profile)),
            unit,
        }
    }

    const NO_DEADLINE: Duration = Duration::from_secs(30);

    async fn read(
        total: usize,
        piece: usize,
        limit: usize,
        headers: &[(String, String)],
        b: &Budget,
    ) -> (Result<Option<HeldBody>>, usize) {
        let (url, reads, _) = body_server(total, piece, false).await;
        let conn = CdpConn::connect(&url).await.unwrap();
        let r = read_body_capped(&conn, "S", "r", headers, limit, b, NO_DEADLINE).await;
        (r, reads.load(std::sync::atomic::Ordering::SeqCst))
    }

    #[tokio::test]
    async fn document_bodies_are_read_in_pieces_up_to_the_limit() {
        let (piece, limit) = (1000, 10_000);
        let b = budget(1000, 1000, 100);
        // Under the limit: the whole body.
        let (r, _) = read(limit - 5, piece, limit, &[], &b).await;
        assert_eq!(r.unwrap().map(|(b, _)| b.len()), Some(limit - 5));
        // Over the limit, with no length declared (a compressed body grows when decoded):
        // reading stops just past the limit.
        let (r, reads) = read(limit * 100, piece, limit, &[], &b).await;
        assert!(r.unwrap().is_none());
        assert_eq!(reads, limit / piece + 1);
        // A declared length over the limit: refused without reading.
        let too_long = vec![("Content-Length".to_string(), (limit + 1).to_string())];
        let (r, reads) = read(10, piece, limit, &too_long, &b).await;
        assert!(r.unwrap().is_none());
        assert_eq!(reads, 0);
    }

    #[tokio::test]
    async fn the_budget_counts_bytes_not_reads() {
        // 500 bytes in 1-byte pieces under a budget of 10 steps of 100 bytes: 5 steps, not
        // 500.
        let b = budget(10, 10, 100);
        let (r, reads) = read(500, 1, 1 << 20, &[], &b).await;
        let held = r.unwrap().unwrap();
        assert_eq!(held.0.len(), 500);
        assert_eq!(reads, 500);
        assert_eq!(b.global.available_permits(), 5);
        assert_eq!(b.profile.available_permits(), 5);
        drop(held);
        assert_eq!(b.global.available_permits(), 10);
    }

    #[tokio::test]
    async fn bodies_read_at_the_same_time_share_one_budget() {
        let unit = 1000;
        // A global budget of 5 steps. A first body of 3 steps is held.
        let b = budget(5, 100, unit);
        let (first, _) = read(3 * unit, unit, 1 << 20, &[], &b).await;
        let first = first.unwrap().unwrap();
        assert_eq!(b.global.available_permits(), 2);
        // A second body of 3 steps does not fit beside it: refused, and what it had taken
        // is given back.
        let (second, _) = read(3 * unit, unit, 1 << 20, &[], &b).await;
        let e = format!("{:#}", second.unwrap_err());
        assert!(e.contains("budget"), "{e}");
        assert_eq!(b.global.available_permits(), 2);
        // Once the first is released, it fits.
        drop(first);
        let (third, _) = read(3 * unit, unit, 1 << 20, &[], &b).await;
        assert_eq!(third.unwrap().unwrap().0.len(), 3 * unit);
    }

    #[tokio::test]
    async fn one_profile_at_its_share_does_not_block_another() {
        let unit = 1000;
        let global = Arc::new(tokio::sync::Semaphore::new(10));
        let profile = |share: usize| Budget {
            global: global.clone(),
            profile: Arc::new(tokio::sync::Semaphore::new(share)),
            unit,
        };
        let (a, b) = (profile(4), profile(4));
        // Profile a takes its whole share ...
        let (held, _) = read(4 * unit, unit, 1 << 20, &[], &a).await;
        let held = held.unwrap().unwrap();
        // ... and cannot take more, although the global budget has room.
        let (more, _) = read(unit, unit, 1 << 20, &[], &a).await;
        let e = format!("{:#}", more.unwrap_err());
        assert!(e.contains("share"), "{e}");
        assert_eq!(global.available_permits(), 6);
        // Profile b still reads.
        let (other, _) = read(4 * unit, unit, 1 << 20, &[], &b).await;
        assert_eq!(other.unwrap().unwrap().0.len(), 4 * unit);
        drop(held);
    }

    #[tokio::test]
    async fn a_read_past_its_deadline_fails_and_closes_the_stream() {
        let b = budget(10, 10, 1000);
        let (url, _, closed) = body_server(10, 10, true).await;
        let conn = CdpConn::connect(&url).await.unwrap();
        let started = std::time::Instant::now();
        let r = tokio::time::timeout(
            Duration::from_secs(5),
            read_body_capped(
                &conn,
                "S",
                "r",
                &[],
                1 << 20,
                &b,
                Duration::from_millis(200),
            ),
        )
        .await
        .expect("the deadline was not kept");
        let e = format!("{:#}", r.unwrap_err());
        assert!(e.contains("too long"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(
            closed.load(std::sync::atomic::Ordering::SeqCst),
            "IO.close was not sent"
        );
    }

    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn every_response_but_a_download_is_a_document() {
        // Documents, whatever their type: script can run in an image or text document
        // through a same-origin page.
        for t in [
            "text/html; charset=utf-8",
            "image/svg+xml",
            "image/png",
            "text/plain",
            "application/pdf",
            "application/json",
            "video/mp4",
            "unknown/unknown",
            "*/*",
            "",
            "nonsense",
        ] {
            assert!(!is_download(&h(&[("content-type", t)])), "{t}");
        }
        assert!(!is_download(&h(&[])));
        // Downloads: every Content-Type value a download type.
        for t in [
            "application/octet-stream",
            "application/zip",
            "application/x-gzip",
        ] {
            assert!(is_download(&h(&[("Content-Type", t)])), "{t}");
        }
        assert!(!is_download(&h(&[
            ("content-type", "application/octet-stream"),
            ("content-type", "text/html")
        ])));
        assert!(!is_download(&h(&[(
            "content-type",
            "application/zip, text/html"
        )])));
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
            Some("connect-src data:"),
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
