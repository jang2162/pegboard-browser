//! Client for the bridge extension's service worker (tab groups, tabs in the agent profile).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex};

use crate::cdp::{CdpConn, CdpEvent};

const WORKER_WAIT: Duration = Duration::from_secs(10);
const PING_TIMEOUT: Duration = Duration::from_secs(2);
/// Longest wait for the load of a data: URL the bridge opens through CDP.
const DATA_LOAD_TIMEOUT: Duration = Duration::from_secs(15);

/// The worker's function for tab switches (see sw.js).
const ACTIVITY_BINDING: &str = "pegActivity";
/// Tab switches until this long after the bridge closed tabs are Chrome's doing (it
/// activates a neighbour of a closed tab), not a person's.
const CLOSE_QUIET: Duration = Duration::from_secs(2);

/// Told of every tab switch in the profile: (the profile's browser context, the target that
/// became active, when).
pub type ActivityFn = Arc<dyn Fn(&str, &str, SystemTime) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupId(pub i64);

pub struct Bridge {
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
}

/// Finds the bridge worker of `context`, waiting up to `wait` for it to start.
async fn find_worker(conn: &CdpConn, context: &str, wait: Duration) -> Result<Option<String>> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let r = conn.send("Target.getTargets", json!({}), None).await?;
        let found = r["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| is_worker_of(t, context));
        if let Some(t) = found {
            return Ok(t["targetId"].as_str().map(String::from));
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

impl Bridge {
    /// Attaches to the bridge service worker that lives in `agent_context` and keeps that CDP session open.
    pub async fn attach(conn: &CdpConn, agent_context: &str) -> Result<Bridge> {
        let deadline = tokio::time::Instant::now() + WORKER_WAIT;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let target = find_worker(conn, agent_context, left)
                .await?
                .ok_or_else(|| {
                    anyhow!("bridge service worker for the agent profile did not appear within 10s")
                })?;
            // A worker that is still starting (or being restarted) can accept a session
            // yet never answer, so require one answered ping before trusting it.
            if let Ok(session) = conn.attach(&target).await {
                let ping = conn.evaluate(&session, "typeof self.peg");
                if let Ok(Ok(v)) = tokio::time::timeout(PING_TIMEOUT, ping).await {
                    if v == "object" {
                        return Ok(Bridge {
                            conn: conn.clone(),
                            agent_context: agent_context.to_string(),
                            session: Mutex::new(session),
                            activity: std::sync::Mutex::new(None),
                            bound: std::sync::Mutex::new(String::new()),
                            quiet_until: std::sync::Mutex::new(None),
                        });
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("bridge service worker did not answer within 10s");
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    /// Calls `self.peg.<func>(...args)` in the worker; retries once after re-attaching when the session is gone.
    async fn call(&self, func: &str, args: &[Value]) -> Result<Value> {
        // Arguments are serialized as JSON, never spliced in as raw text.
        let args = args
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let expr = format!("(async () => JSON.stringify(await self.peg.{func}({args})))()");
        let mut session = self.session.lock().await;
        let first = self.conn.evaluate(&session, &expr).await;
        let out = match first {
            Err(e) if is_session_gone(&e) => {
                let target = find_worker(&self.conn, &self.agent_context, Duration::ZERO)
                    .await?
                    .ok_or_else(|| {
                        anyhow!("bridge_asleep: the bridge service worker is not running")
                    })?;
                *session = self.conn.attach(&target).await?;
                self.bind_if_following(&session).await;
                self.conn.evaluate(&session, &expr).await?
            }
            other => other?,
        };
        let text = out
            .as_str()
            .ok_or_else(|| anyhow!("bridge call {func} returned no value"))?;
        serde_json::from_str(text).with_context(|| format!("parsing bridge reply of {func}"))
    }

    pub async fn create_group(&self, title: &str, color: &str) -> Result<(GroupId, String)> {
        let r = self
            .call("createGroup", &[json!(title), json!(color)])
            .await?;
        Ok((GroupId(group_id(&r)?), target_id(&r)?))
    }

    /// Makes a tab in `group`. When Chrome already deleted the group (it goes with its last
    /// tab) the worker starts a new one with `title` and `color` and the group id is returned
    /// with the target.
    pub async fn create_tab(
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

    pub async fn add_to_group(&self, group: &GroupId, target_id: &str) -> Result<()> {
        self.call("addToGroup", &[json!(group.0), json!(target_id)])
            .await?;
        Ok(())
    }

    pub async fn members(&self, group: &GroupId) -> Result<Vec<String>> {
        let r = self.call("members", &[json!(group.0)]).await?;
        Ok(r["targetIds"]
            .as_array()
            .ok_or_else(|| anyhow!("members reply has no targetIds"))?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect())
    }

    pub async fn close_targets(&self, target_ids: &[String]) -> Result<()> {
        self.quiet();
        let closed = self.call("closeTargets", &[json!(target_ids)]).await;
        self.quiet();
        closed?;
        Ok(())
    }

    /// Reports every tab switch in the profile to `on` from now on, also across restarts
    /// of the worker (spike H8: a restarted worker has lost the binding; the switches it
    /// kept meanwhile are asked for when it is added again).
    pub async fn follow_activity(self: &Arc<Self>, on: ActivityFn) {
        *lock(&self.activity) = Some(on);
        // Subscribed before the binding exists, so no call of it is missed.
        let events = self.conn.events();
        {
            let session = self.session.lock().await;
            self.bind_if_following(&session).await;
        }
        tokio::spawn(follow(Arc::downgrade(self), events));
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
            "Target.targetCreated"
                if is_worker_of(&e.params["targetInfo"], &self.agent_context) =>
            {
                let Some(target) = e.params["targetInfo"]["targetId"].as_str() else {
                    return;
                };
                let mut session = self.session.lock().await;
                if let Ok(new) = self.conn.attach(target).await {
                    let old = std::mem::replace(&mut *session, new);
                    self.bind_if_following(&session).await;
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

    /// Takes the tabs out of their groups; no other tab is touched.
    pub async fn ungroup_targets(&self, target_ids: &[String]) -> Result<()> {
        self.call("ungroupTargets", &[json!(target_ids)]).await?;
        Ok(())
    }

    pub async fn ungroup(&self, group: &GroupId) -> Result<()> {
        self.call("ungroup", &[json!(group.0)]).await?;
        Ok(())
    }
}

/// Follows the daemon's connection for the bridge's tab switches while the bridge lives.
async fn follow(bridge: std::sync::Weak<Bridge>, mut events: broadcast::Receiver<CdpEvent>) {
    loop {
        match events.recv().await {
            Ok(e) => {
                let Some(b) = bridge.upgrade() else {
                    return;
                };
                if e.method == "Target.targetCreated" {
                    // Attaching waits for calls in flight: not on this loop.
                    if is_worker_of(&e.params["targetInfo"], &b.agent_context) {
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

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
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
