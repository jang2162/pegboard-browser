//! Phase 3a task 1: do several browser-target sessions (`Target.attachToBrowserTarget`) over
//! ONE websocket behave like separate CDP connections? The answer decides how the connection
//! multiplexer routes messages. Each test prints one `OBSERVED ...` line; run with
//! `-- --ignored --nocapture --test-threads=1` and copy those lines into the findings.
//!
//! These tests talk raw JSON over a single websocket (`RawLink`), not through `CdpConn`, so
//! nothing here hides a routing difference.
mod common;

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const LIMIT: Duration = Duration::from_secs(3);
/// How long to listen when asserting that something does NOT arrive.
const QUIET: Duration = Duration::from_millis(700);

/// One raw websocket to the browser. Every message received is kept in `backlog` until a
/// caller takes it, so events that arrive before their trigger's response are not lost.
struct RawLink {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    backlog: Vec<Value>,
}

fn session_of(msg: &Value) -> Option<&str> {
    msg.get("sessionId").and_then(Value::as_str)
}

impl RawLink {
    async fn connect(ws_url: &str) -> RawLink {
        let (ws, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();
        RawLink {
            ws,
            next_id: 0,
            backlog: Vec::new(),
        }
    }

    /// Sends `msg` after giving it a fresh `id`; returns the id.
    async fn send(&mut self, mut msg: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        msg["id"] = json!(id);
        self.ws
            .send(Message::Text(msg.to_string().into()))
            .await
            .unwrap();
        id
    }

    /// Sends a command, on `session` or on the root (`None`); returns its id.
    async fn cmd(&mut self, session: Option<&str>, method: &str, params: Value) -> u64 {
        let mut msg = json!({ "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.send(msg).await
    }

    /// Reads one more message into the backlog; false if `deadline` passed first.
    async fn pump(&mut self, deadline: Instant) -> bool {
        loop {
            match tokio::time::timeout_at(deadline, self.ws.next()).await {
                Err(_) => return false,
                Ok(None) => panic!("websocket closed"),
                Ok(Some(Err(e))) => panic!("websocket error: {e}"),
                Ok(Some(Ok(Message::Text(t)))) => {
                    self.backlog.push(serde_json::from_str(t.as_str()).unwrap());
                    return true;
                }
                Ok(Some(Ok(_))) => continue,
            }
        }
    }

    /// The first message for `session` (None = root, no `sessionId`) that `pred` accepts,
    /// taken out of the backlog. None after `limit`.
    async fn try_next_for(
        &mut self,
        session: Option<&str>,
        limit: Duration,
        pred: impl Fn(&Value) -> bool,
    ) -> Option<Value> {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(i) = self
                .backlog
                .iter()
                .position(|m| session_of(m) == session && pred(m))
            {
                return Some(self.backlog.remove(i));
            }
            if !self.pump(deadline).await {
                return None;
            }
        }
    }

    /// Like `try_next_for` with the 3 second limit; panics on timeout.
    async fn next_for(&mut self, session: Option<&str>, pred: impl Fn(&Value) -> bool) -> Value {
        self.try_next_for(session, LIMIT, pred)
            .await
            .unwrap_or_else(|| panic!("no matching message for session {session:?} in {LIMIT:?}"))
    }

    /// The response to command `id` (whatever session it came on); None after 3 seconds.
    async fn try_response(&mut self, id: u64) -> Option<Value> {
        let deadline = Instant::now() + LIMIT;
        loop {
            if let Some(i) = self.backlog.iter().position(|m| m["id"] == json!(id)) {
                return Some(self.backlog.remove(i));
            }
            if !self.pump(deadline).await {
                return None;
            }
        }
    }

    /// Like `try_response`; panics on timeout.
    async fn response(&mut self, id: u64) -> Value {
        self.try_response(id)
            .await
            .unwrap_or_else(|| panic!("no response to command {id} in {LIMIT:?}"))
    }

    /// Sends a command and returns its full response (result or error).
    async fn call_raw(&mut self, session: Option<&str>, method: &str, params: Value) -> Value {
        let id = self.cmd(session, method, params).await;
        self.response(id).await
    }

    /// Sends a command and returns its `result`; panics on a CDP error.
    async fn call(&mut self, session: Option<&str>, method: &str, params: Value) -> Value {
        let r = self.call_raw(session, method, params).await;
        match r.get("result") {
            Some(v) => v.clone(),
            None => panic!("{method} on {session:?} failed: {r}"),
        }
    }

    /// Listens for `d`, keeping everything that arrives in the backlog.
    async fn settle(&mut self, d: Duration) {
        let deadline = Instant::now() + d;
        while self.pump(deadline).await {}
    }

    /// Whether the backlog holds a message for `session` that `pred` accepts.
    fn seen(&self, session: Option<&str>, pred: impl Fn(&Value) -> bool) -> bool {
        self.backlog
            .iter()
            .any(|m| session_of(m) == session && pred(m))
    }

    /// Whether the backlog holds a message that `pred` accepts, on any session or the root.
    fn seen_anywhere(&self, pred: impl Fn(&Value) -> bool) -> bool {
        self.backlog.iter().any(pred)
    }

    async fn browser_session(&mut self) -> String {
        let r = self
            .call(None, "Target.attachToBrowserTarget", json!({}))
            .await;
        r["sessionId"].as_str().unwrap().to_string()
    }

    /// Opens a tab through the root and returns its target id.
    async fn new_tab(&mut self, url: &str) -> String {
        let r = self
            .call(None, "Target.createTarget", json!({ "url": url }))
            .await;
        r["targetId"].as_str().unwrap().to_string()
    }

    /// Attaches `session` to a target with a flat page session; returns the session id.
    async fn attach(&mut self, session: &str, target: &str) -> String {
        let r = self
            .call(
                Some(session),
                "Target.attachToTarget",
                json!({ "targetId": target, "flatten": true }),
            )
            .await;
        r["sessionId"].as_str().unwrap().to_string()
    }

    /// Waits until the page of `session` has loaded `path`.
    async fn ready(&mut self, session: &str, path: &str) {
        let want = format!("complete|{path}");
        let deadline = Instant::now() + LIMIT;
        loop {
            let r = self
                .call(
                    Some(session),
                    "Runtime.evaluate",
                    json!({ "expression": "document.readyState + '|' + location.pathname" }),
                )
                .await;
            if r["result"]["value"] == json!(want) {
                return;
            }
            assert!(Instant::now() < deadline, "page never reached {want}: {r}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn event(method: &'static str) -> impl Fn(&Value) -> bool {
    move |m| m["method"] == json!(method)
}

/// `Target.targetCreated` / `attachedToTarget` for the target `tid`.
fn about_target(method: &'static str, tid: String) -> impl Fn(&Value) -> bool {
    move |m| m["method"] == json!(method) && m["params"]["targetInfo"]["targetId"] == json!(tid)
}

/// `Runtime.consoleAPICalled` whose first argument is `marker`.
fn console(marker: &'static str) -> impl Fn(&Value) -> bool {
    move |m| {
        m["method"] == json!("Runtime.consoleAPICalled")
            && m["params"]["args"][0]["value"] == json!(marker)
    }
}

fn log_expr(marker: &str) -> Value {
    json!({ "expression": format!("console.log('{marker}')") })
}

/// The structure of a message, without its values (arrays count only as "array", since their
/// elements can differ in optional fields).
fn shape(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(_) => "bool".into(),
        Value::Number(_) => "number".into(),
        Value::String(_) => "string".into(),
        Value::Array(_) => "array".into(),
        Value::Object(o) => {
            let mut keys: Vec<_> = o.iter().collect();
            keys.sort_by_key(|(k, _)| k.as_str());
            let inner: Vec<String> = keys
                .iter()
                .map(|(k, v)| format!("{k}:{}", shape(v)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
    }
}

/// A response reduced to what must not differ between root and browser session: its shape
/// (results) or its code and message (errors).
fn response_kind(r: &Value) -> String {
    match r.get("error") {
        Some(e) => format!("error {} {}", e["code"], e["message"]),
        None => format!("result {}", shape(&r["result"])),
    }
}

struct Rig {
    // Kept alive for the test; the browser dies with `_browser`.
    _env: common::Env,
    fixture: common::Fixture,
    _browser: pegboard_core::browser::Browser,
    link: RawLink,
}

async fn rig() -> Rig {
    let (env, fixture, browser) = common::launched().await;
    let link = RawLink::connect(&browser.ws_url).await;
    Rig {
        _env: env,
        fixture,
        _browser: browser,
        link,
    }
}

#[tokio::test]
#[ignore]
async fn discover_targets_on_one_session_stays_there() {
    let Rig {
        fixture,
        mut link,
        _env,
        _browser,
    } = rig().await;
    let s1 = link.browser_session().await;
    let s2 = link.browser_session().await;
    link.call(
        Some(&s1),
        "Target.setDiscoverTargets",
        json!({ "discover": true }),
    )
    .await;
    let tid = link.new_tab(&format!("{}/counter", fixture.origin)).await;
    link.next_for(Some(&s1), about_target("Target.targetCreated", tid.clone()))
        .await;
    link.settle(QUIET).await;
    let on_s2 = link.seen(Some(&s2), event("Target.targetCreated"));
    let on_root = link.seen(None, event("Target.targetCreated"));
    println!("OBSERVED discover_s1_only: s1_got=true s2_got={on_s2} root_got={on_root}");
    assert!(!on_s2, "S2 got a targetCreated it never asked for");
    assert!(!on_root, "the root got a targetCreated it never asked for");
}

#[tokio::test]
#[ignore]
async fn auto_attach_on_one_session_stays_there() {
    let Rig {
        fixture,
        mut link,
        _env,
        _browser,
    } = rig().await;
    let s1 = link.browser_session().await;
    let s2 = link.browser_session().await;
    link.call(
        Some(&s1),
        "Target.setAutoAttach",
        json!({ "autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true }),
    )
    .await;
    let tid = link.new_tab(&format!("{}/counter", fixture.origin)).await;
    let attached = link
        .next_for(
            Some(&s1),
            about_target("Target.attachedToTarget", tid.clone()),
        )
        .await;
    let child = attached["params"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    link.ready(&child, "/counter").await;
    link.call(Some(&child), "Runtime.enable", json!({})).await;
    link.call(Some(&child), "Runtime.evaluate", log_expr("auto"))
        .await;
    link.next_for(Some(&child), console("auto")).await;
    link.settle(QUIET).await;
    let attach_elsewhere = link.seen(
        Some(&s2),
        about_target("Target.attachedToTarget", tid.clone()),
    ) || link
        .seen(None, about_target("Target.attachedToTarget", tid.clone()));
    let console_elsewhere = link.seen_anywhere(|m| {
        m["method"] == json!("Runtime.consoleAPICalled") && session_of(m) != Some(child.as_str())
    });
    println!(
        "OBSERVED auto_attach_s1_only: attached_on_s1=true attached_on_s2_or_root={attach_elsewhere} child_event_only_on_child={}",
        !console_elsewhere
    );
    assert!(!attach_elsewhere, "attachedToTarget reached S2 or the root");
    assert!(
        !console_elsewhere,
        "the child's console event arrived on another session"
    );
}

#[tokio::test]
#[ignore]
async fn page_session_events_go_only_to_that_session() {
    let Rig {
        fixture,
        mut link,
        _env,
        _browser,
    } = rig().await;
    let s1 = link.browser_session().await;
    let s2 = link.browser_session().await;
    let tid = link.new_tab(&format!("{}/counter", fixture.origin)).await;
    let p1 = link.attach(&s1, &tid).await;
    let p2 = link.attach(&s2, &tid).await;
    assert_ne!(p1, p2);
    link.ready(&p1, "/counter").await;

    // Only P1 enables Runtime; the log is triggered through P2.
    link.call(Some(&p1), "Runtime.enable", json!({})).await;
    link.call(Some(&p2), "Runtime.evaluate", log_expr("m1"))
        .await;
    link.next_for(Some(&p1), console("m1")).await;
    link.settle(QUIET).await;
    let p2_got_m1 = link.seen(Some(&p2), event("Runtime.consoleAPICalled"));
    let p1_stray = link.seen(Some(&p1), event("Runtime.consoleAPICalled"));

    // Now P2 enables too: a fresh log reaches both, each on its own session id.
    link.call(Some(&p2), "Runtime.enable", json!({})).await;
    link.call(Some(&p1), "Runtime.evaluate", log_expr("m2"))
        .await;
    link.next_for(Some(&p1), console("m2")).await;
    link.next_for(Some(&p2), console("m2")).await;
    println!(
        "OBSERVED page_sessions_one_tab: p2_got_event_before_its_own_enable={p2_got_m1} both_got_m2=true"
    );
    assert!(
        !p2_got_m1,
        "P2 received consoleAPICalled although only P1 enabled Runtime"
    );
    assert!(!p1_stray, "P1 received a duplicate consoleAPICalled");
}

#[tokio::test]
#[ignore]
async fn download_behavior_fetch_and_screencast_are_per_session() {
    let Rig {
        fixture,
        mut link,
        _env,
        _browser,
    } = rig().await;
    let s1 = link.browser_session().await;
    let s2 = link.browser_session().await;

    // 1. Browser.setDownloadBehavior: one value per context, set from different sessions.
    let ctx_a = link
        .call(None, "Target.createBrowserContext", json!({}))
        .await["browserContextId"]
        .as_str()
        .unwrap()
        .to_string();
    let ctx_b = link
        .call(None, "Target.createBrowserContext", json!({}))
        .await["browserContextId"]
        .as_str()
        .unwrap()
        .to_string();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    for (session, ctx, dir) in [(&s1, &ctx_a, &dir_a), (&s2, &ctx_b, &dir_b)] {
        link.call(
            Some(session),
            "Browser.setDownloadBehavior",
            json!({
                "behavior": "allow",
                "browserContextId": ctx,
                "downloadPath": dir.path().to_str().unwrap(),
            }),
        )
        .await;
    }
    for (ctx, name) in [(&ctx_a, "pb-link-a.bin"), (&ctx_b, "pb-link-b.bin")] {
        link.call(
            None,
            "Target.createTarget",
            json!({ "url": format!("{}/dl/{name}", fixture.origin), "browserContextId": ctx }),
        )
        .await;
    }
    let landed = |dir: &std::path::Path, name: &str| dir.join(name).is_file();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !(landed(dir_a.path(), "pb-link-a.bin") && landed(dir_b.path(), "pb-link-b.bin"))
        && Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let a_in_a = landed(dir_a.path(), "pb-link-a.bin");
    let b_in_b = landed(dir_b.path(), "pb-link-b.bin");
    let crossed = landed(dir_a.path(), "pb-link-b.bin") || landed(dir_b.path(), "pb-link-a.bin");

    // 2. Fetch.enable on P1 must pause for P1 only; P2 stays usable meanwhile.
    let tid = link.new_tab(&format!("{}/counter", fixture.origin)).await;
    let p1 = link.attach(&s1, &tid).await;
    let p2 = link.attach(&s2, &tid).await;
    link.ready(&p1, "/counter").await;
    link.call(
        Some(&p1),
        "Fetch.enable",
        json!({ "patterns": [{ "urlPattern": "*/pb-link-probe*" }] }),
    )
    .await;
    let fetch_id = link
        .cmd(
            Some(&p2),
            "Runtime.evaluate",
            json!({
                "expression": "fetch('/pb-link-probe').then(r => r.status)",
                "awaitPromise": true,
            }),
        )
        .await;
    let paused = link.next_for(Some(&p1), event("Fetch.requestPaused")).await;
    let p2_cmd = link
        .call(
            Some(&p2),
            "Runtime.evaluate",
            json!({ "expression": "1 + 1" }),
        )
        .await;
    link.settle(QUIET).await;
    let p2_paused = link.seen(Some(&p2), event("Fetch.requestPaused"));
    link.call(
        Some(&p1),
        "Fetch.continueRequest",
        json!({ "requestId": paused["params"]["requestId"] }),
    )
    .await;
    let fetched = link.response(fetch_id).await;
    let p2_free = p2_cmd["result"]["value"] == json!(2);
    let fetch_done = fetched.get("result").is_some();

    // 3. Page.startScreencast: a stream per session; stopping P1's leaves P2's running.
    let shot = json!({ "format": "jpeg", "quality": 20, "everyNthFrame": 1 });
    link.call(Some(&p1), "Page.startScreencast", shot.clone())
        .await;
    link.call(Some(&p2), "Page.startScreencast", shot).await;
    for p in [&p1, &p2] {
        let f = link.next_for(Some(p), event("Page.screencastFrame")).await;
        link.call(
            Some(p),
            "Page.screencastFrameAck",
            json!({ "sessionId": f["params"]["sessionId"] }),
        )
        .await;
    }
    link.call(Some(&p1), "Page.stopScreencast", json!({})).await;
    link.backlog
        .retain(|m| !(session_of(m) == Some(p1.as_str()) && event("Page.screencastFrame")(m)));
    let mut p2_after = 0;
    for _ in 0..3 {
        if let Some(f) = link
            .try_next_for(Some(&p2), LIMIT, event("Page.screencastFrame"))
            .await
        {
            p2_after += 1;
            link.call(
                Some(&p2),
                "Page.screencastFrameAck",
                json!({ "sessionId": f["params"]["sessionId"] }),
            )
            .await;
        }
    }
    let p1_after = link.seen(Some(&p1), event("Page.screencastFrame"));
    println!(
        "OBSERVED per_session_download_fetch_screencast: download_a_in_a={a_in_a} download_b_in_b={b_in_b} download_crossed={crossed} fetch_paused_on_p2={p2_paused} p2_commands_free_while_paused={p2_free} paused_fetch_completes={fetch_done} frames_p2_after_p1_stop={p2_after} frames_p1_after_stop={p1_after}"
    );
    assert!(
        a_in_a && b_in_b,
        "a download missed its context's directory"
    );
    assert!(
        !crossed,
        "one context's setDownloadBehavior overwrote the other's"
    );
    assert!(!p2_paused, "Fetch.enable on P1 paused a request on P2");
    assert!(p2_free, "P2 was stalled by P1's Fetch.enable");
    assert!(
        fetch_done,
        "the page request never finished after P1 continued it"
    );
    assert!(
        p2_after >= 1,
        "P2's screencast stopped when P1's was stopped"
    );
    assert!(!p1_after, "P1 still got frames after stopScreencast");
}

#[tokio::test]
#[ignore]
async fn detaching_a_browser_session_detaches_its_children() {
    // A measurement: passes either way, the outcome is the printed line.
    let Rig {
        fixture,
        mut link,
        _env,
        _browser,
    } = rig().await;
    let s1 = link.browser_session().await;
    let tid = link.new_tab(&format!("{}/counter", fixture.origin)).await;
    let p1 = link.attach(&s1, &tid).await;
    link.ready(&p1, "/counter").await;
    let detach = link
        .call_raw(None, "Target.detachFromTarget", json!({ "sessionId": s1 }))
        .await;
    let probe_id = link
        .cmd(
            Some(&p1),
            "Runtime.evaluate",
            json!({ "expression": "1 + 1" }),
        )
        .await;
    let outcome = match link.try_response(probe_id).await {
        Some(r) if r.get("result").is_some() => "alive".to_string(),
        Some(r) => format!("gone error={}", r["error"]),
        None => "unknown no_answer".to_string(),
    };
    println!(
        "OBSERVED child_after_parent_detach={} root_detach_response={}",
        outcome.split(' ').next().unwrap(),
        detach
    );
    println!("OBSERVED child_after_parent_detach_detail={outcome}");
}

#[tokio::test]
#[ignore]
async fn proxy_allowlist_behaves_the_same_on_a_browser_session() {
    let Rig {
        fixture,
        mut link,
        _env,
        _browser,
    } = rig().await;
    let s1 = link.browser_session().await;
    let tid = link.new_tab(&format!("{}/counter", fixture.origin)).await;

    // Browser-level methods of `proxy::filter::ALLOWED_METHODS`, sent first on the root and
    // then on S1 with the same parameters.
    let fixed: Vec<(&str, Value)> = vec![
        ("Browser.getVersion", json!({})),
        ("Target.getTargets", json!({})),
        ("Target.setDiscoverTargets", json!({ "discover": false })),
        (
            "Target.setAutoAttach",
            json!({ "autoAttach": false, "waitForDebuggerOnStart": false, "flatten": true }),
        ),
        ("Target.getTargetInfo", json!({ "targetId": tid })),
        (
            "Target.getTargetInfo",
            json!({ "targetId": "no-such-target" }),
        ),
        (
            "Target.closeTarget",
            json!({ "targetId": "no-such-target" }),
        ),
    ];
    let mut report = Vec::new();
    for (method, params) in &fixed {
        let on_root = link.call_raw(None, method, params.clone()).await;
        let on_s1 = link.call_raw(Some(&s1), method, params.clone()).await;
        assert_eq!(
            session_of(&on_s1),
            Some(s1.as_str()),
            "{method}: reply left S1"
        );
        assert_eq!(
            response_kind(&on_root),
            response_kind(&on_s1),
            "{method} answers differently on a browser session"
        );
        report.push(format!(
            "{method}={}",
            response_kind(&on_s1).split(' ').next().unwrap()
        ));
    }

    // Methods that create or consume state, one fresh round on each side.
    let mut kinds = Vec::new();
    for session in [None, Some(s1.as_str())] {
        let created = link
            .call_raw(
                session,
                "Target.createTarget",
                json!({ "url": "about:blank" }),
            )
            .await;
        let new_tid = created["result"]["targetId"].as_str().unwrap().to_string();
        let attached = link
            .call_raw(
                session,
                "Target.attachToTarget",
                json!({ "targetId": new_tid, "flatten": true }),
            )
            .await;
        let child = attached["result"]["sessionId"]
            .as_str()
            .unwrap()
            .to_string();
        let detached = link
            .call_raw(
                session,
                "Target.detachFromTarget",
                json!({ "sessionId": child }),
            )
            .await;
        let closed = link
            .call_raw(
                session,
                "Target.closeTarget",
                json!({ "targetId": new_tid }),
            )
            .await;
        kinds.push(
            [&created, &attached, &detached, &closed]
                .map(response_kind)
                .join(" | "),
        );
    }
    println!(
        "OBSERVED allowlist_root_vs_s1: same_shape={} {}",
        kinds[0] == kinds[1],
        report.join(" ")
    );
    assert_eq!(
        kinds[0], kinds[1],
        "create/attach/detach/close answer differently on a browser session"
    );
}
