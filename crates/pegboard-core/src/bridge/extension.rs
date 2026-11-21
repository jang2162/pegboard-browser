//! The bridge on stock Chrome: a client for the bridge extension's service worker (tab groups,
//! tabs in the agent profile).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex};

use super::{lock, ActivityFn, GroupId, TabBridge, CLOSE_QUIET};
use crate::cdp::{CdpConn, CdpEvent};

const WORKER_WAIT: Duration = Duration::from_secs(10);
const PING_TIMEOUT: Duration = Duration::from_secs(2);
/// Longest wait for the load of a data: URL the bridge opens through CDP.
const DATA_LOAD_TIMEOUT: Duration = Duration::from_secs(15);

/// The worker's function for tab switches (see sw.js).
const ACTIVITY_BINDING: &str = "pegActivity";

pub struct ExtensionBridge {
    conn: CdpConn,
    agent_context: String,
    /// Flat-mode session attached to the worker; kept open for the bridge's lifetime.
    session: Mutex<String>,
    /// Set by `follow_activity`: every session attached to the worker gets the binding.
    activity: std::sync::Mutex<Option<ActivityFn>>,
    /// The worker session the binding was last added on; calls from others are not read.
    bound: std::sync::Mutex<String>,
    /// Tab switches before this are not reported (`CLOSE_QUIET`).
    quiet_until: std::sync::Mutex<Option<SystemTime>>,
    /// The script URL of the worker first found answering as the bridge
    /// (`chrome-extension://<id>/sw.js`): only a worker of exactly this URL is taken later
    /// (another extension of the profile may have a `/sw.js` too).
    script: String,
}

/// The service workers of `context` whose script is some extension's `/sw.js`: (target id,
/// script URL).
async fn workers(conn: &CdpConn, context: &str) -> Result<Vec<(String, String)>> {
    let r = conn.send("Target.getTargets", json!({}), None).await?;
    Ok(r["targetInfos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| is_worker_of(t, context))
        .filter_map(|t| {
            Some((
                t["targetId"].as_str()?.to_string(),
                t["url"].as_str()?.to_string(),
            ))
        })
        .collect())
}

impl ExtensionBridge {
    /// Attaches to the bridge service worker that lives in `agent_context` and keeps that CDP session open.
    pub async fn attach(conn: &CdpConn, agent_context: &str) -> Result<ExtensionBridge> {
        let deadline = tokio::time::Instant::now() + WORKER_WAIT;
        let mut seen = false;
        loop {
            // A worker that is still starting (or being restarted) can accept a session
            // yet never answer, and another extension's worker does not know `self.peg`: the
            // bridge is the one that answers the ping.
            for (target, script) in workers(conn, agent_context).await? {
                seen = true;
                let Ok(session) = conn.attach(&target).await else {
                    continue;
                };
                if answers(conn, &session).await {
                    return Ok(ExtensionBridge {
                        conn: conn.clone(),
                        agent_context: agent_context.to_string(),
                        session: Mutex::new(session),
                        activity: std::sync::Mutex::new(None),
                        bound: std::sync::Mutex::new(String::new()),
                        quiet_until: std::sync::Mutex::new(None),
                        script,
                    });
                }
                let _ = conn
                    .send(
                        "Target.detachFromTarget",
                        json!({ "sessionId": session }),
                        None,
                    )
                    .await;
            }
            if tokio::time::Instant::now() >= deadline {
                if seen {
                    bail!("bridge service worker did not answer within 10s");
                }
                bail!("bridge service worker for the agent profile did not appear within 10s");
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    /// Calls `self.peg.<func>(...args)` in the worker. A worker that does not answer a ping
    /// first (stopped, restarted, or a session that waits for a worker that will not come
    /// back) is found or started again (`revive`), so a call never waits on a dead session
    /// until the CDP limit; also retried once after re-attaching when the session is gone.
    async fn call(&self, func: &str, args: &[Value]) -> Result<Value> {
        // Arguments are serialized as JSON, never spliced in as raw text.
        let args = args
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let expr = format!("(async () => JSON.stringify(await self.peg.{func}({args})))()");
        let mut session = self.session.lock().await;
        if !answers(&self.conn, &session).await {
            self.revive(&mut session).await?;
        }
        let first = self.conn.evaluate(&session, &expr).await;
        let out = match first {
            Err(e) if is_session_gone(&e) => {
                self.revive(&mut session).await?;
                self.conn.evaluate(&session, &expr).await?
            }
            other => other?,
        };
        let text = out
            .as_str()
            .ok_or_else(|| anyhow!("bridge call {func} returned no value"))?;
        serde_json::from_str(text).with_context(|| format!("parsing bridge reply of {func}"))
    }

    /// A new session on the worker, if one runs and answers (a worker that restarted is a
    /// new target). Otherwise `bridge_asleep` at once: a worker stopped by force does not come
    /// back by any means measured (spike-findings H9), and the call must not hang on it.
    /// Binds the new session when following.
    async fn revive(&self, session: &mut String) -> Result<()> {
        let ours = workers(&self.conn, &self.agent_context)
            .await?
            .into_iter()
            .find(|(_, script)| *script == self.script);
        if let Some((target, _)) = ours {
            if let Ok(new) = self.conn.attach(&target).await {
                if answers(&self.conn, &new).await {
                    let old = std::mem::replace(session, new);
                    let _ = tokio::time::timeout(
                        PING_TIMEOUT,
                        self.conn.send(
                            "Target.detachFromTarget",
                            json!({ "sessionId": old }),
                            None,
                        ),
                    )
                    .await;
                    self.bind_if_following(session).await;
                    if crate::testenv::hooks() {
                        eprintln!("pegboard: bridge: attached again to its worker");
                    }
                    return Ok(());
                }
                let _ = tokio::time::timeout(
                    PING_TIMEOUT,
                    self.conn
                        .send("Target.detachFromTarget", json!({ "sessionId": new }), None),
                )
                .await;
            }
        }
        bail!("bridge_asleep: the bridge service worker is not running")
    }

    /// Navigates the tab on a CDP session of its own and waits for the load (a page that
    /// loads slowly is not an error; a navigation that fails is).
    async fn open_in_tab(&self, target: &str, url: &str) -> Result<()> {
        let session = self.conn.attach(target).await?;
        let navigate = async {
            let mut events = self.conn.events();
            self.conn
                .send("Page.enable", json!({}), Some(&session))
                .await?;
            let r = self
                .conn
                .send("Page.navigate", json!({ "url": url }), Some(&session))
                .await?;
            if let Some(err) = r.get("errorText").and_then(Value::as_str) {
                bail!("opening the URL failed: {err}");
            }
            let loaded = async {
                while let Ok(e) = events.recv().await {
                    if e.method == "Page.loadEventFired"
                        && e.session_id.as_deref() == Some(session.as_str())
                    {
                        return;
                    }
                }
            };
            let _ = tokio::time::timeout(DATA_LOAD_TIMEOUT, loaded).await;
            Ok(())
        };
        let result = navigate.await;
        let _ = self
            .conn
            .send(
                "Target.detachFromTarget",
                json!({ "sessionId": session }),
                None,
            )
            .await;
        result
    }

    async fn bind_if_following(&self, session: &str) {
        if lock(&self.activity).is_none() {
            return;
        }
        *lock(&self.bound) = session.to_string();
        let bind = async {
            self.conn
                .send("Runtime.enable", json!({}), Some(session))
                .await?;
            self.conn
                .send(
                    "Runtime.addBinding",
                    json!({ "name": ACTIVITY_BINDING }),
                    Some(session),
                )
                .await?;
            self.conn
                .evaluate(session, "self.peg.replayActivity()")
                .await
        };
        if let Err(e) = bind.await {
            eprintln!("pegboard: bridge: tab switches are not followed: {e:#}");
        }
    }

    /// A target that is this bridge's worker (its context, its exact script URL).
    fn is_ours(&self, info: &Value) -> bool {
        is_worker_of(info, &self.agent_context)
            && info["url"].as_str() == Some(self.script.as_str())
    }

    fn quiet(&self) {
        *lock(&self.quiet_until) = Some(SystemTime::now() + CLOSE_QUIET);
    }

    /// One event of the daemon's connection, for `follow_activity`.
    async fn on_event(&self, e: CdpEvent) {
        match e.method.as_str() {
            "Runtime.bindingCalled" => {
                if e.params["name"] != ACTIVITY_BINDING
                    || e.session_id.as_deref() != Some(lock(&self.bound).as_str())
                {
                    return;
                }
                let Some((target, at)) = activity_of(&e.params["payload"]) else {
                    return;
                };
                if lock(&self.quiet_until).is_some_and(|until| at <= until) {
                    return;
                }
                let on = lock(&self.activity).clone();
                if let Some(on) = on {
                    on(&self.agent_context, &target, at);
                }
            }
            // The worker restarted (it stops when idle): attach to the new one, add the
            // binding again and take the switches it kept.
            "Target.targetCreated" if self.is_ours(&e.params["targetInfo"]) => {
                let Some(target) = e.params["targetInfo"]["targetId"].as_str() else {
                    return;
                };
                let mut session = self.session.lock().await;
                if let Ok(new) = self.conn.attach(target).await {
                    if !answers(&self.conn, &new).await {
                        let _ = self
                            .conn
                            .send("Target.detachFromTarget", json!({ "sessionId": new }), None)
                            .await;
                        return;
                    }
                    let old = std::mem::replace(&mut *session, new);
                    self.bind_if_following(&session).await;
                    if crate::testenv::hooks() {
                        eprintln!("pegboard: bridge: attached again to its worker (it restarted)");
                    }
                    // Gone with the old worker, or (a call attached first) a second session.
                    let _ = self
                        .conn
                        .send("Target.detachFromTarget", json!({ "sessionId": old }), None)
                        .await;
                }
            }
            _ => {}
        }
    }
}

#[async_trait]
impl TabBridge for ExtensionBridge {
    async fn create_group(&self, title: &str, color: &str) -> Result<(GroupId, String)> {
        let r = self
            .call("createGroup", &[json!(title), json!(color)])
            .await?;
        Ok((GroupId(group_id(&r)?), target_id(&r)?))
    }

    /// Makes a tab in `group`. When Chrome already deleted the group (it goes with its last
    /// tab) the worker starts a new one with `title` and `color` and the group id is returned
    /// with the target.
    async fn create_tab(
        &self,
        group: &GroupId,
        url: &str,
        title: &str,
        color: &str,
    ) -> Result<(String, Option<GroupId>)> {
        // `chrome.tabs.update` navigates as if the extension's page asked for it, and Chrome
        // does not let that kind of navigation take a tab to a data: URL: the call succeeds
        // and the tab stays at about:blank. A data: URL is opened through CDP instead, the
        // way `open` does it.
        let via_cdp = url.starts_with("data:");
        let for_worker = if via_cdp { "about:blank" } else { url };
        let r = self
            .call(
                "createTab",
                &[
                    json!(group.0),
                    json!(for_worker),
                    json!(title),
                    json!(color),
                ],
            )
            .await?;
        let moved = r["newGroupId"].as_i64().map(GroupId);
        let target = target_id(&r)?;
        if via_cdp {
            if let Err(e) = self.open_in_tab(&target, url).await {
                // The tab was made for this URL: it does not stay behind without it.
                let _ = self.close_targets(std::slice::from_ref(&target)).await;
                return Err(e);
            }
        }
        Ok((target, moved))
    }

    async fn add_to_group(&self, group: &GroupId, target_id: &str) -> Result<()> {
        self.call("addToGroup", &[json!(group.0), json!(target_id)])
            .await?;
        Ok(())
    }

    async fn members(&self, group: &GroupId) -> Result<Vec<String>> {
        let r = self.call("members", &[json!(group.0)]).await?;
        Ok(r["targetIds"]
            .as_array()
            .ok_or_else(|| anyhow!("members reply has no targetIds"))?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect())
    }

    async fn close_targets(&self, target_ids: &[String]) -> Result<()> {
        self.quiet();
        let closed = self.call("closeTargets", &[json!(target_ids)]).await;
        self.quiet();
        closed?;
        Ok(())
    }

    /// Reports every tab switch in the profile to `on` from now on, also across restarts
    /// of the worker (spike H8: a restarted worker has lost the binding; the switches it
    /// kept meanwhile are asked for when it is added again).
    async fn follow_activity(self: Arc<Self>, on: ActivityFn) {
        *lock(&self.activity) = Some(on);
        // Subscribed before the binding exists, so no call of it is missed.
        let events = self.conn.events();
        {
            let session = self.session.lock().await;
            self.bind_if_following(&session).await;
        }
        tokio::spawn(follow(Arc::downgrade(&self), events));
    }

    /// Takes the tabs out of their groups; no other tab is touched.
    async fn ungroup_targets(&self, target_ids: &[String]) -> Result<()> {
        self.call("ungroupTargets", &[json!(target_ids)]).await?;
        Ok(())
    }

    async fn ungroup(&self, group: &GroupId) -> Result<()> {
        self.call("ungroup", &[json!(group.0)]).await?;
        Ok(())
    }

    /// Test hook (`test.detachBridge`): lets go of the worker (detaches the session), so
    /// nothing of pegboard keeps it running. The next call attaches again (`revive`).
    async fn detach_for_test(&self) -> bool {
        let mut session = self.session.lock().await;
        let old = std::mem::take(&mut *session);
        *lock(&self.bound) = String::new();
        self.conn
            .send("Target.detachFromTarget", json!({ "sessionId": old }), None)
            .await
            .is_ok()
    }
}

/// Follows the daemon's connection for the bridge's tab switches while the bridge lives.
async fn follow(
    bridge: std::sync::Weak<ExtensionBridge>,
    mut events: broadcast::Receiver<CdpEvent>,
) {
    loop {
        match events.recv().await {
            Ok(e) => {
                let Some(b) = bridge.upgrade() else {
                    return;
                };
                if e.method == "Target.targetCreated" {
                    // Attaching waits for calls in flight: not on this loop.
                    if b.is_ours(&e.params["targetInfo"]) {
                        tokio::spawn(async move { b.on_event(e).await });
                    }
                } else {
                    b.on_event(e).await;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// (target, when) of a tab switch the worker reported.
fn activity_of(payload: &Value) -> Option<(String, SystemTime)> {
    let v: Value = serde_json::from_str(payload.as_str()?).ok()?;
    if v["event"] != "activated" {
        return None;
    }
    let target = v["targetId"].as_str()?.to_string();
    let at = UNIX_EPOCH + Duration::from_millis(v["at"].as_u64()?);
    Some((target, at))
}

/// The bridge's worker in `context` (a target of type service_worker of sw.js).
fn is_worker_of(info: &Value, context: &str) -> bool {
    info["type"] == "service_worker"
        && info["browserContextId"].as_str() == Some(context)
        && info["url"]
            .as_str()
            .is_some_and(|u| u.starts_with("chrome-extension://") && u.ends_with("/sw.js"))
}

/// Whether the worker behind `session` answers (within `PING_TIMEOUT`).
async fn answers(conn: &CdpConn, session: &str) -> bool {
    let ping = conn.evaluate(session, "typeof self.peg");
    matches!(tokio::time::timeout(PING_TIMEOUT, ping).await, Ok(Ok(v)) if v == "object")
}

fn is_session_gone(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}");
    s.contains("Session with given id not found") || s.contains("No session with given id")
}

fn group_id(r: &Value) -> Result<i64> {
    r["groupId"]
        .as_i64()
        .ok_or_else(|| anyhow!("reply has no groupId"))
}

fn target_id(r: &Value) -> Result<String> {
    match r["targetId"].as_str() {
        Some(s) => Ok(s.to_string()),
        None => bail!("reply has no targetId"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// A browser stand-in with one agent profile ("A", a page P1) and its bridge worker
    /// `W<n>`. `running` false: the worker is stopped (not listed; a session on it takes
    /// requests and never answers, as on the second machine). `restart` makes a new worker
    /// `W<n+1>` and says so (`Target.targetCreated`); only a session on the current worker
    /// answers. The worker's `replayActivity` hands over one kept tab switch (of `T9`) through
    /// the binding of the session it is called on. Every method sent is in `seen`.
    /// There is also always another extension's worker `X1` (`chrome-extension://y/sw.js`)
    /// in the profile, which does not know `self.peg`. Events can be sent with `inject`.
    struct Fake {
        running: AtomicBool,
        worker: AtomicU32,
        seen: std::sync::Mutex<Vec<String>>,
        outbox: std::sync::Mutex<Vec<Value>>,
        poke: tokio::sync::Notify,
    }

    impl Fake {
        fn new() -> Arc<Fake> {
            Arc::new(Fake {
                running: AtomicBool::new(true),
                worker: AtomicU32::new(1),
                seen: Default::default(),
                outbox: Default::default(),
                poke: tokio::sync::Notify::new(),
            })
        }

        fn inject(&self, event: Value) {
            self.outbox.lock().unwrap().push(event);
            self.poke.notify_one();
        }

        fn worker_created(&self, id: &str, url: &str) -> Value {
            json!({ "method": "Target.targetCreated", "params": { "targetInfo": {
                "targetId": id, "type": "service_worker", "browserContextId": "A", "url": url } } })
        }

        /// Chrome starts a new bridge worker.
        fn restart(&self) {
            let n = self.worker.fetch_add(1, Ordering::SeqCst) + 1;
            self.running.store(true, Ordering::SeqCst);
            self.inject(self.worker_created(&format!("W{n}"), "chrome-extension://x/sw.js"));
        }

        fn switch(session: &str, target: &str, at: u64) -> Value {
            let payload = format!(r#"{{"event":"activated","targetId":"{target}","at":{at}}}"#);
            json!({ "method": "Runtime.bindingCalled", "sessionId": session,
                    "params": { "name": ACTIVITY_BINDING, "payload": payload } })
        }

        fn count(&self, what: &str) -> usize {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .filter(|m| *m == what)
                .count()
        }

        fn current(&self) -> String {
            format!("S-W{}", self.worker.load(Ordering::SeqCst))
        }

        fn reply(&self, v: &Value) -> Option<Vec<Value>> {
            let method = v["method"].as_str().unwrap_or("").to_string();
            let what = match v["params"]["targetId"].as_str() {
                Some(t) => format!("{method} {t}"),
                None => method.clone(),
            };
            self.seen.lock().unwrap().push(what);
            let on = v["sessionId"].as_str().unwrap_or("");
            let running = self.running.load(Ordering::SeqCst);
            let worker_session = on.starts_with("S-W");
            if worker_session && (!running || on != self.current()) {
                return None;
            }
            if on.starts_with("S-X") && method == "Runtime.evaluate" {
                return Some(vec![
                    json!({ "id": v["id"], "result": { "result": { "type": "undefined" } } }),
                ]);
            }
            let mut out = Vec::new();
            let result = match method.as_str() {
                "Target.getTargets" => {
                    let mut infos = vec![json!({ "targetId": "P1", "type": "page",
                        "url": "about:blank", "browserContextId": "A" })];
                    // Listed first: the bridge must not take it for its own.
                    infos.push(json!({ "targetId": "X1", "type": "service_worker",
                        "browserContextId": "A", "url": "chrome-extension://y/sw.js" }));
                    if running {
                        infos.push(
                            json!({ "targetId": format!("W{}", self.worker.load(Ordering::SeqCst)),
                            "type": "service_worker", "browserContextId": "A",
                            "url": "chrome-extension://x/sw.js" }),
                        );
                    }
                    json!({ "targetInfos": infos })
                }
                "Target.attachToTarget" => {
                    json!({ "sessionId": format!("S-{}", v["params"]["targetId"].as_str().unwrap_or("")) })
                }
                "Runtime.evaluate" => {
                    let expr = v["params"]["expression"].as_str().unwrap_or("");
                    let value = match expr {
                        "typeof self.peg" => json!("object"),
                        "self.peg.replayActivity()" => {
                            let payload =
                                r#"{"event":"activated","targetId":"T9","at":1700000000000}"#;
                            out.push(json!({ "method": "Runtime.bindingCalled", "sessionId": on,
                                "params": { "name": ACTIVITY_BINDING, "payload": payload } }));
                            json!(1)
                        }
                        _ => json!(r#"{"targetIds":["T1"]}"#),
                    };
                    json!({ "result": { "value": value } })
                }
                _ => json!({}),
            };
            out.insert(0, json!({ "id": v["id"], "result": result }));
            Some(out)
        }
    }

    async fn fake_browser(fake: Arc<Fake>) -> String {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            loop {
                tokio::select! {
                    m = ws.next() => {
                        let Some(Ok(Message::Text(t))) = m else { return };
                        let v: Value = serde_json::from_str(&t).unwrap();
                        for out in fake.reply(&v).unwrap_or_default() {
                            ws.send(Message::text(out.to_string())).await.unwrap();
                        }
                    }
                    () = fake.poke.notified() => {
                        let out: Vec<Value> = fake.outbox.lock().unwrap().drain(..).collect();
                        for e in out {
                            ws.send(Message::text(e.to_string())).await.unwrap();
                        }
                    }
                }
            }
        });
        format!("ws://{addr}")
    }

    #[tokio::test]
    async fn a_call_on_a_stopped_worker_fails_at_once_instead_of_waiting_out_the_limit() {
        let fake = Fake::new();
        let conn = CdpConn::connect(&fake_browser(fake.clone()).await)
            .await
            .unwrap();
        let bridge = ExtensionBridge::attach(&conn, "A").await.unwrap();
        assert_eq!(
            bridge.members(&GroupId(1)).await.unwrap(),
            vec!["T1".to_string()]
        );
        // Stopped by force: its session stays and never answers again; nothing restarts it.
        fake.running.store(false, Ordering::SeqCst);
        let t = std::time::Instant::now();
        let e = bridge.members(&GroupId(1)).await.unwrap_err();
        let took = t.elapsed();
        assert!(format!("{e:#}").contains("bridge_asleep"), "{e:#}");
        assert!(took < PING_TIMEOUT + Duration::from_secs(1), "{took:?}");
    }

    #[tokio::test]
    async fn a_call_after_the_worker_restarted_goes_to_the_new_one() {
        let fake = Fake::new();
        let conn = CdpConn::connect(&fake_browser(fake.clone()).await)
            .await
            .unwrap();
        let bridge = ExtensionBridge::attach(&conn, "A").await.unwrap();
        // A new worker; the old session is dead. (Not followed: no rebind by event.)
        fake.worker.fetch_add(1, Ordering::SeqCst);
        let t = std::time::Instant::now();
        assert_eq!(
            bridge.members(&GroupId(1)).await.unwrap(),
            vec!["T1".to_string()]
        );
        assert!(t.elapsed() < PING_TIMEOUT + Duration::from_secs(1));
        let t = std::time::Instant::now();
        bridge.members(&GroupId(1)).await.unwrap();
        assert!(t.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn a_restarted_worker_is_bound_again_and_hands_over_the_switches_it_kept() {
        let fake = Fake::new();
        let conn = CdpConn::connect(&fake_browser(fake.clone()).await)
            .await
            .unwrap();
        let bridge = Arc::new(ExtensionBridge::attach(&conn, "A").await.unwrap());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        bridge
            .clone()
            .follow_activity(Arc::new(move |ctx: &str, target: &str, at| {
                let _ = tx.send((ctx.to_string(), target.to_string(), at));
            }))
            .await;
        // The first worker's replay (its binding is new too).
        let first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!((first.0.as_str(), first.1.as_str()), ("A", "T9"));
        // Chrome starts a new worker: the bridge attaches to it, binds it and takes what it
        // kept, without any call.
        fake.restart();
        let got = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("no switch from the restarted worker")
            .unwrap();
        assert_eq!(
            (got.0.as_str(), got.1.as_str(), got.2),
            (
                "A",
                "T9",
                UNIX_EPOCH + Duration::from_millis(1_700_000_000_000)
            )
        );
        assert_eq!(fake.count("Runtime.addBinding"), 2);
    }

    type Switches = tokio::sync::mpsc::UnboundedReceiver<(String, String, SystemTime)>;

    /// A bridge following tab switches on the stand-in, and the switches it reports (the
    /// first worker's replay already taken).
    async fn followed(fake: &Arc<Fake>) -> (Arc<ExtensionBridge>, Switches) {
        let conn = CdpConn::connect(&fake_browser(fake.clone()).await)
            .await
            .unwrap();
        let bridge = Arc::new(ExtensionBridge::attach(&conn, "A").await.unwrap());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        bridge
            .clone()
            .follow_activity(Arc::new(move |ctx: &str, target: &str, at| {
                let _ = tx.send((ctx.to_string(), target.to_string(), at));
            }))
            .await;
        let first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.1, "T9");
        (bridge, rx)
    }

    async fn next_switch(rx: &mut Switches) -> Option<String> {
        tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .ok()
            .flatten()
            .map(|(_, target, _)| target)
    }

    fn ms_now(plus: u64) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + plus
    }

    #[tokio::test]
    async fn another_extensions_worker_is_never_taken_for_the_bridge() {
        let fake = Fake::new();
        let (_bridge, mut rx) = followed(&fake).await;
        // Its worker is listed first and was refused (no `self.peg`): the bridge is on W1.
        assert_eq!(fake.count("Runtime.addBinding"), 1);
        fake.inject(fake.worker_created("X2", "chrome-extension://y/sw.js"));
        // A worker of the bridge's own URL that does not answer (W7 is not the running one)
        // is not taken either: the bridge stays bound to W1.
        fake.inject(fake.worker_created("W7", "chrome-extension://x/sw.js"));
        assert_eq!(next_switch(&mut rx).await, None);
        fake.inject(Fake::switch("S-W1", "T1", ms_now(0)));
        assert_eq!(next_switch(&mut rx).await.as_deref(), Some("T1"));
        let seen = fake.seen.lock().unwrap().clone();
        assert_eq!(fake.count("Target.attachToTarget X2"), 0, "{seen:?}");
        assert_eq!(fake.count("Runtime.addBinding"), 1, "{seen:?}");
    }

    #[tokio::test]
    async fn only_the_bound_session_reports_switches() {
        let fake = Fake::new();
        let (_bridge, mut rx) = followed(&fake).await;
        // A call of the binding from another session (an old worker, another extension).
        fake.inject(Fake::switch("S-X1", "T-FOREIGN", ms_now(0)));
        fake.inject(Fake::switch("S-W1", "T1", ms_now(0)));
        assert_eq!(next_switch(&mut rx).await.as_deref(), Some("T1"));
        assert_eq!(next_switch(&mut rx).await, None);
    }

    #[tokio::test]
    async fn switches_right_after_the_bridge_closed_tabs_are_chromes() {
        let fake = Fake::new();
        let (bridge, mut rx) = followed(&fake).await;
        bridge.close_targets(&["T1".to_string()]).await.unwrap();
        // Chrome activates a neighbour of the closed tab: not reported.
        fake.inject(Fake::switch("S-W1", "T2", ms_now(500)));
        // Later than the quiet time: reported.
        let later = CLOSE_QUIET.as_millis() as u64 + 1000;
        fake.inject(Fake::switch("S-W1", "T3", ms_now(later)));
        assert_eq!(next_switch(&mut rx).await.as_deref(), Some("T3"));
        assert_eq!(next_switch(&mut rx).await, None);
    }

    #[test]
    fn a_tab_switch_report_names_the_target_and_its_time() {
        let payload = json!(r#"{"event":"activated","targetId":"T1","at":1700000000123}"#);
        assert_eq!(
            activity_of(&payload),
            Some((
                "T1".to_string(),
                UNIX_EPOCH + Duration::from_millis(1_700_000_000_123)
            ))
        );
        for bad in [
            json!(r#"{"event":"other","targetId":"T1","at":1}"#),
            json!(r#"{"event":"activated","at":1}"#),
            json!(r#"{"event":"activated","targetId":"T1"}"#),
            json!("not json"),
            json!({ "event": "activated" }),
        ] {
            assert_eq!(activity_of(&bad), None, "{bad}");
        }
    }

    #[test]
    fn only_the_bridge_worker_of_the_profile_is_followed() {
        let w = |kind: &str, ctx: &str, url: &str| json!({ "type": kind, "browserContextId": ctx, "url": url });
        assert!(is_worker_of(
            &w("service_worker", "A", "chrome-extension://x/sw.js"),
            "A"
        ));
        assert!(!is_worker_of(
            &w("service_worker", "H", "chrome-extension://x/sw.js"),
            "A"
        ));
        assert!(!is_worker_of(
            &w("page", "A", "chrome-extension://x/sw.js"),
            "A"
        ));
        assert!(!is_worker_of(
            &w("service_worker", "A", "https://a.test/sw.js"),
            "A"
        ));
    }
}
