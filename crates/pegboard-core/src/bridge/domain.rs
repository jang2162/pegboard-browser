//! The bridge on the Pegboard fork: the browser's own CDP domain `Pegboard.*`, on the daemon's
//! control connection (the browser session that called `Pegboard.claim`; no other session
//! has the domain). The domain checks every tab and URL itself (only an agent profile's
//! tabs; `about:blank` or an absolute `http:`, `https:` or `data:` URL) and opens a data: URL
//! itself. Its requests and answers are the contract the fork's C++ side implements.

use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use super::{lock, ActivityFn, GroupId, TabBridge, CLOSE_QUIET};
use crate::cdp::{CdpConn, CdpEvent};
use crate::contexts::is_agent_profile_dir;

const TAB_ACTIVATED: &str = "Pegboard.tabActivated";
const PROFILE_LOADED: &str = "Pegboard.profileLoaded";
const PROFILE_UNLOADED: &str = "Pegboard.profileUnloaded";

pub struct DomainBridge {
    conn: CdpConn,
    agent_context: String,
    /// Set by `follow_activity`.
    activity: std::sync::Mutex<Option<ActivityFn>>,
    /// Tab switches before this are not reported (`CLOSE_QUIET`).
    quiet_until: std::sync::Mutex<Option<SystemTime>>,
}

impl DomainBridge {
    /// The bridge of the agent profile whose context is `agent_context`, on `control`, the
    /// connection that claimed the domain.
    pub fn new(control: CdpConn, agent_context: &str) -> DomainBridge {
        DomainBridge {
            conn: control,
            agent_context: agent_context.to_string(),
            activity: std::sync::Mutex::new(None),
            quiet_until: std::sync::Mutex::new(None),
        }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.conn.send(method, params, None).await
    }

    fn quiet(&self) {
        *lock(&self.quiet_until) = Some(SystemTime::now() + CLOSE_QUIET);
    }

    /// One event of the control connection, for `follow_activity`.
    fn on_event(&self, e: &CdpEvent) {
        // The domain's events come on the control connection's own browser session.
        if e.method != TAB_ACTIVATED || e.session_id.is_some() {
            return;
        }
        let Some((context, target, at)) = activity_of(&e.params) else {
            return;
        };
        if context != self.agent_context {
            return;
        }
        if lock(&self.quiet_until).is_some_and(|until| at <= until) {
            return;
        }
        let on = lock(&self.activity).clone();
        if let Some(on) = on {
            on(&self.agent_context, &target, at);
        }
    }
}

#[async_trait]
impl TabBridge for DomainBridge {
    async fn create_group(&self, title: &str, color: &str) -> Result<(GroupId, String)> {
        let r = self
            .call(
                "Pegboard.createGroup",
                json!({ "browserContextId": self.agent_context, "title": title, "color": color }),
            )
            .await?;
        let group = r["groupId"]
            .as_i64()
            .ok_or_else(|| anyhow!("Pegboard.createGroup reply has no groupId"))?;
        Ok((GroupId(group), target_id(&r, "Pegboard.createGroup")?))
    }

    async fn create_tab(
        &self,
        group: &GroupId,
        url: &str,
        title: &str,
        color: &str,
    ) -> Result<(String, Option<GroupId>)> {
        let r = self
            .call(
                "Pegboard.createTab",
                json!({ "browserContextId": self.agent_context, "groupId": group.0,
                        "url": url, "title": title, "color": color }),
            )
            .await?;
        let moved = r["newGroupId"].as_i64().map(GroupId);
        Ok((target_id(&r, "Pegboard.createTab")?, moved))
    }

    async fn add_to_group(&self, group: &GroupId, target_id: &str) -> Result<()> {
        self.call(
            "Pegboard.addToGroup",
            json!({ "groupId": group.0, "targetId": target_id }),
        )
        .await?;
        Ok(())
    }

    async fn members(&self, group: &GroupId) -> Result<Vec<String>> {
        let r = self
            .call("Pegboard.getGroupMembers", json!({ "groupId": group.0 }))
            .await?;
        Ok(r["targetIds"]
            .as_array()
            .ok_or_else(|| anyhow!("Pegboard.getGroupMembers reply has no targetIds"))?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect())
    }

    async fn close_targets(&self, target_ids: &[String]) -> Result<()> {
        self.quiet();
        let closed = self
            .call("Pegboard.closeTargets", json!({ "targetIds": target_ids }))
            .await;
        self.quiet();
        closed?;
        Ok(())
    }

    async fn ungroup_targets(&self, target_ids: &[String]) -> Result<()> {
        self.call(
            "Pegboard.ungroupTargets",
            json!({ "targetIds": target_ids }),
        )
        .await?;
        Ok(())
    }

    async fn ungroup(&self, group: &GroupId) -> Result<()> {
        self.call("Pegboard.ungroup", json!({ "groupId": group.0 }))
            .await?;
        Ok(())
    }

    /// Reports the domain's `Pegboard.tabActivated` of this bridge's context. The domain
    /// keeps no switches for later: there is no worker that stops.
    async fn follow_activity(self: Arc<Self>, on: ActivityFn) {
        *lock(&self.activity) = Some(on);
        let events = self.conn.events();
        tokio::spawn(follow(Arc::downgrade(&self), events));
    }
}

/// Follows the control connection for the bridge's tab switches while the bridge lives.
async fn follow(bridge: Weak<DomainBridge>, mut events: broadcast::Receiver<CdpEvent>) {
    loop {
        match events.recv().await {
            Ok(e) => {
                let Some(b) = bridge.upgrade() else {
                    return;
                };
                b.on_event(&e);
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// (context, target, when) of a `Pegboard.tabActivated`; `timestamp` is in ms since the
/// epoch.
fn activity_of(params: &Value) -> Option<(String, String, SystemTime)> {
    let context = params["browserContextId"].as_str()?.to_string();
    let target = params["targetId"].as_str()?.to_string();
    let ms = &params["timestamp"];
    let at = match ms.as_u64() {
        Some(ms) => UNIX_EPOCH + Duration::from_millis(ms),
        None => {
            let ms = ms.as_f64().filter(|ms| ms.is_finite() && *ms >= 0.0)?;
            UNIX_EPOCH + Duration::from_secs_f64(ms / 1000.0)
        }
    };
    Some((context, target, at))
}

fn target_id(r: &Value, method: &str) -> Result<String> {
    match r["targetId"].as_str() {
        Some(s) => Ok(s.to_string()),
        None => bail!("{method} reply has no targetId"),
    }
}

/// A profile the browser has loaded (`Pegboard.getProfiles`). `is_agent` is the browser's
/// word; whether a context is an agent profile's is decided by its directory name
/// (`AgentContexts::set`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainProfile {
    pub profile_directory: String,
    pub browser_context_id: String,
    pub is_agent: bool,
}

/// A profile the browser loaded or unloaded (`Pegboard.profileLoaded`/`profileUnloaded`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileEvent {
    Loaded {
        profile_directory: String,
        browser_context_id: String,
    },
    Unloaded {
        profile_directory: String,
        browser_context_id: String,
    },
}

/// Takes the domain for `conn`'s browser session (`Pegboard.claim`): from then on no other
/// session has it.
pub async fn claim(conn: &CdpConn) -> Result<()> {
    conn.send("Pegboard.claim", json!({}), None).await?;
    Ok(())
}

/// The profiles the browser has loaded now (`Pegboard.getProfiles`). An entry without a
/// directory or a context is left out: a context nobody names is a person's.
pub async fn get_profiles(conn: &CdpConn) -> Result<Vec<DomainProfile>> {
    let r = conn.send("Pegboard.getProfiles", json!({}), None).await?;
    Ok(r["profiles"]
        .as_array()
        .ok_or_else(|| anyhow!("Pegboard.getProfiles reply has no profiles"))?
        .iter()
        .filter_map(|p| {
            let (profile_directory, browser_context_id) = names(p)?;
            Some(DomainProfile {
                profile_directory,
                browser_context_id,
                is_agent: p["isAgent"].as_bool().unwrap_or(false),
            })
        })
        .collect())
}

/// A `Pegboard.profileLoaded`/`profileUnloaded` of the control connection's own session.
pub fn profile_event(e: &CdpEvent) -> Option<ProfileEvent> {
    if e.session_id.is_some() {
        return None;
    }
    let loaded = match e.method.as_str() {
        PROFILE_LOADED => true,
        PROFILE_UNLOADED => false,
        _ => return None,
    };
    let (profile_directory, browser_context_id) = names(&e.params)?;
    Some(if loaded {
        ProfileEvent::Loaded {
            profile_directory,
            browser_context_id,
        }
    } else {
        ProfileEvent::Unloaded {
            profile_directory,
            browser_context_id,
        }
    })
}

/// Loads the agent profile `profile_dir` (making it if needed) and opens a window of it, or
/// finds it open (`Pegboard.openProfileWindow`): its browser context. A person's profile is
/// refused here before the domain (which refuses it too) is asked.
pub async fn open_profile_window(conn: &CdpConn, profile_dir: &str) -> Result<String> {
    if !is_agent_profile_dir(profile_dir) {
        bail!("{profile_dir} is not an agent profile");
    }
    let r = conn
        .send(
            "Pegboard.openProfileWindow",
            json!({ "profileDirectory": profile_dir }),
            None,
        )
        .await?;
    match r["browserContextId"].as_str() {
        Some(c) if !c.is_empty() => Ok(c.to_string()),
        _ => bail!("Pegboard.openProfileWindow reply has no browserContextId"),
    }
}

/// (profileDirectory, browserContextId), both non-empty.
fn names(v: &Value) -> Option<(String, String)> {
    let dir = v["profileDirectory"].as_str().filter(|s| !s.is_empty())?;
    let context = v["browserContextId"].as_str().filter(|s| !s.is_empty())?;
    Some((dir.to_string(), context.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde_json::{json, Value};

    use crate::bridge::CLOSE_QUIET;
    use crate::link::fake::FakeBrowser;

    /// A domain bridge of the agent context "A" on a fake browser.
    async fn bridge() -> (Arc<DomainBridge>, FakeBrowser, CdpConn) {
        let (link, fake) = FakeBrowser::start();
        let conn = CdpConn::over(&link).await.unwrap();
        (Arc::new(DomainBridge::new(conn.clone(), "A")), fake, conn)
    }

    /// The next request the browser got, which must be `method` on the control connection's
    /// own browser session, with exactly `params`.
    async fn expect(fake: &mut FakeBrowser, method: &str, params: Value) -> Value {
        let m = fake.next().await;
        assert_eq!(m["method"], method, "{m}");
        assert_eq!(m["sessionId"], "B1", "{m}");
        assert_eq!(m["params"], params, "{m}");
        m
    }

    fn activated(context: &str, target: &str, ms: u64) -> Value {
        json!({ "method": "Pegboard.tabActivated", "sessionId": "B1", "params": {
            "browserContextId": context, "targetId": target, "timestamp": ms } })
    }

    fn ms_now(plus: u64) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + plus
    }

    type Switches = tokio::sync::mpsc::UnboundedReceiver<(String, String, SystemTime)>;

    async fn follow(b: &Arc<DomainBridge>) -> Switches {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        b.clone()
            .follow_activity(Arc::new(move |ctx: &str, target: &str, at| {
                let _ = tx.send((ctx.to_string(), target.to_string(), at));
            }))
            .await;
        rx
    }

    async fn next_switch(rx: &mut Switches) -> Option<(String, String, SystemTime)> {
        tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .ok()
            .flatten()
    }

    #[tokio::test]
    async fn create_tab_sends_context_and_group() {
        let (b, mut fake, _conn) = bridge().await;
        let call = {
            let b = b.clone();
            tokio::spawn(async move {
                b.create_tab(&GroupId(7), "https://a.test/x", "work", "blue")
                    .await
            })
        };
        let m = expect(
            &mut fake,
            "Pegboard.createTab",
            json!({ "browserContextId": "A", "groupId": 7, "url": "https://a.test/x",
                    "title": "work", "color": "blue" }),
        )
        .await;
        fake.answer(&m, json!({ "targetId": "T1" }));
        assert_eq!(call.await.unwrap().unwrap(), ("T1".to_string(), None));

        // A data: URL goes to the domain as it is (it opens it itself), in one request.
        let call = {
            let b = b.clone();
            tokio::spawn(async move {
                b.create_tab(&GroupId(7), "data:text/html,hi", "work", "blue")
                    .await
            })
        };
        let m = expect(
            &mut fake,
            "Pegboard.createTab",
            json!({ "browserContextId": "A", "groupId": 7, "url": "data:text/html,hi",
                    "title": "work", "color": "blue" }),
        )
        .await;
        fake.answer(&m, json!({ "targetId": "T2" }));
        assert_eq!(call.await.unwrap().unwrap().0, "T2");
        fake.quiet().await;

        // The group comes with its first tab.
        let call = {
            let b = b.clone();
            tokio::spawn(async move { b.create_group("work", "blue").await })
        };
        let m = expect(
            &mut fake,
            "Pegboard.createGroup",
            json!({ "browserContextId": "A", "title": "work", "color": "blue" }),
        )
        .await;
        fake.answer(&m, json!({ "groupId": 3, "targetId": "T3" }));
        assert_eq!(call.await.unwrap().unwrap(), (GroupId(3), "T3".to_string()));
    }

    #[tokio::test]
    async fn a_vanished_group_comes_back_as_new_group_id() {
        let (b, mut fake, _conn) = bridge().await;
        let call = {
            let b = b.clone();
            tokio::spawn(async move {
                b.create_tab(&GroupId(7), "about:blank", "again", "red")
                    .await
            })
        };
        let m = expect(
            &mut fake,
            "Pegboard.createTab",
            json!({ "browserContextId": "A", "groupId": 7, "url": "about:blank",
                    "title": "again", "color": "red" }),
        )
        .await;
        fake.answer(&m, json!({ "targetId": "T2", "newGroupId": 9 }));
        assert_eq!(
            call.await.unwrap().unwrap(),
            ("T2".to_string(), Some(GroupId(9)))
        );
    }

    #[tokio::test]
    async fn a_reply_without_its_fields_or_an_error_is_an_error() {
        let (b, mut fake, _conn) = bridge().await;
        let call = {
            let b = b.clone();
            tokio::spawn(async move { b.create_tab(&GroupId(7), "about:blank", "t", "red").await })
        };
        let m = fake.next().await;
        fake.answer(&m, json!({}));
        assert!(call.await.unwrap().is_err());
        let call = {
            let b = b.clone();
            tokio::spawn(async move { b.create_group("t", "red").await })
        };
        let m = fake.next().await;
        fake.answer(&m, json!({ "targetId": "T1" }));
        assert!(call.await.unwrap().is_err());
        let call = {
            let b = b.clone();
            tokio::spawn(async move { b.members(&GroupId(1)).await })
        };
        let m = fake.next().await;
        fake.send(json!({ "id": m["id"], "sessionId": "B1",
            "error": { "code": -32000, "message": "not an agent tab" } }));
        let e = call.await.unwrap().unwrap_err();
        assert!(format!("{e:#}").contains("not an agent tab"), "{e:#}");
    }

    #[tokio::test]
    async fn the_other_calls_send_the_table_requests() {
        let (b, mut fake, _conn) = bridge().await;
        let ids = vec!["T1".to_string(), "T2".to_string()];

        let call = {
            let b = b.clone();
            tokio::spawn(async move { b.add_to_group(&GroupId(4), "T1").await })
        };
        let m = expect(
            &mut fake,
            "Pegboard.addToGroup",
            json!({ "groupId": 4, "targetId": "T1" }),
        )
        .await;
        fake.answer(&m, json!({}));
        call.await.unwrap().unwrap();

        let call = {
            let b = b.clone();
            tokio::spawn(async move { b.members(&GroupId(4)).await })
        };
        let m = expect(
            &mut fake,
            "Pegboard.getGroupMembers",
            json!({ "groupId": 4 }),
        )
        .await;
        fake.answer(&m, json!({ "targetIds": ["T1", "T2"] }));
        assert_eq!(call.await.unwrap().unwrap(), ids);

        let call = {
            let (b, ids) = (b.clone(), ids.clone());
            tokio::spawn(async move { b.close_targets(&ids).await })
        };
        let m = expect(
            &mut fake,
            "Pegboard.closeTargets",
            json!({ "targetIds": ["T1", "T2"] }),
        )
        .await;
        fake.answer(&m, json!({}));
        call.await.unwrap().unwrap();

        let call = {
            let (b, ids) = (b.clone(), ids.clone());
            tokio::spawn(async move { b.ungroup_targets(&ids).await })
        };
        let m = expect(
            &mut fake,
            "Pegboard.ungroupTargets",
            json!({ "targetIds": ["T1", "T2"] }),
        )
        .await;
        fake.answer(&m, json!({}));
        call.await.unwrap().unwrap();

        let call = {
            let b = b.clone();
            tokio::spawn(async move { b.ungroup(&GroupId(4)).await })
        };
        let m = expect(&mut fake, "Pegboard.ungroup", json!({ "groupId": 4 })).await;
        fake.answer(&m, json!({}));
        call.await.unwrap().unwrap();
        fake.quiet().await;
    }

    #[tokio::test]
    async fn activity_from_other_contexts_is_ignored() {
        let (b, fake, _conn) = bridge().await;
        let mut rx = follow(&b).await;
        let at = ms_now(0);
        fake.send(activated("H", "T-HUMAN", at));
        fake.send(activated("A", "T1", at));
        assert_eq!(
            next_switch(&mut rx).await,
            Some((
                "A".to_string(),
                "T1".to_string(),
                UNIX_EPOCH + Duration::from_millis(at)
            ))
        );
        assert_eq!(next_switch(&mut rx).await, None);
        // An event of a child session (a tab's) is not the domain's.
        fake.send(
            json!({ "method": "Pegboard.tabActivated", "sessionId": "S-TAB",
            "params": { "browserContextId": "A", "targetId": "T2", "timestamp": at } }),
        );
        // Neither is one without a context, a target or a time.
        fake.send(
            json!({ "method": "Pegboard.tabActivated", "sessionId": "B1",
            "params": { "targetId": "T3", "timestamp": at } }),
        );
        fake.send(
            json!({ "method": "Pegboard.tabActivated", "sessionId": "B1",
            "params": { "browserContextId": "A", "timestamp": at } }),
        );
        fake.send(
            json!({ "method": "Pegboard.tabActivated", "sessionId": "B1",
            "params": { "browserContextId": "A", "targetId": "T4" } }),
        );
        assert_eq!(next_switch(&mut rx).await, None);
    }

    #[tokio::test]
    async fn activity_during_close_quiet_is_ignored() {
        let (b, mut fake, _conn) = bridge().await;
        let mut rx = follow(&b).await;
        let call = {
            let b = b.clone();
            tokio::spawn(async move { b.close_targets(&["T1".to_string()]).await })
        };
        let m = fake.next().await;
        assert_eq!(m["method"], "Pegboard.closeTargets");
        fake.answer(&m, json!({}));
        call.await.unwrap().unwrap();
        // Chrome activates a neighbour of the closed tab: not reported.
        fake.send(activated("A", "T2", ms_now(500)));
        // Later than the quiet time: reported.
        let later = CLOSE_QUIET.as_millis() as u64 + 1000;
        fake.send(activated("A", "T3", ms_now(later)));
        assert_eq!(
            next_switch(&mut rx).await.map(|s| s.1).as_deref(),
            Some("T3")
        );
        assert_eq!(next_switch(&mut rx).await, None);
    }

    #[tokio::test]
    async fn activity_stops_with_the_bridge() {
        let (b, fake, _conn) = bridge().await;
        let mut rx = follow(&b).await;
        drop(b);
        fake.send(activated("A", "T1", ms_now(0)));
        assert_eq!(next_switch(&mut rx).await, None);
    }

    #[tokio::test]
    async fn profile_calls_send_the_table_requests() {
        let (_b, mut fake, conn) = bridge().await;

        let call = {
            let conn = conn.clone();
            tokio::spawn(async move { claim(&conn).await })
        };
        let m = expect(&mut fake, "Pegboard.claim", json!({})).await;
        fake.answer(&m, json!({}));
        call.await.unwrap().unwrap();

        let call = {
            let conn = conn.clone();
            tokio::spawn(async move { get_profiles(&conn).await })
        };
        let m = expect(&mut fake, "Pegboard.getProfiles", json!({})).await;
        fake.answer(
            &m,
            json!({ "profiles": [
                { "profileDirectory": "Default", "browserContextId": "C0", "isAgent": false },
                { "profileDirectory": "PegboardAgent", "browserContextId": "C1", "isAgent": true },
                // Incomplete: left out (a context nobody names is a person's).
                { "profileDirectory": "PegboardAgent-x", "isAgent": true },
            ] }),
        );
        assert_eq!(
            call.await.unwrap().unwrap(),
            vec![
                DomainProfile {
                    profile_directory: "Default".into(),
                    browser_context_id: "C0".into(),
                    is_agent: false
                },
                DomainProfile {
                    profile_directory: "PegboardAgent".into(),
                    browser_context_id: "C1".into(),
                    is_agent: true
                },
            ]
        );

        let call = {
            let conn = conn.clone();
            tokio::spawn(async move { open_profile_window(&conn, "PegboardAgent-2").await })
        };
        let m = expect(
            &mut fake,
            "Pegboard.openProfileWindow",
            json!({ "profileDirectory": "PegboardAgent-2" }),
        )
        .await;
        fake.answer(&m, json!({ "browserContextId": "C2" }));
        assert_eq!(call.await.unwrap().unwrap(), "C2");

        // A person's profile is not asked for at all; an answer without a context fails.
        assert!(open_profile_window(&conn, "Default").await.is_err());
        let call = {
            let conn = conn.clone();
            tokio::spawn(async move { open_profile_window(&conn, "PegboardAgent").await })
        };
        let m = expect(
            &mut fake,
            "Pegboard.openProfileWindow",
            json!({ "profileDirectory": "PegboardAgent" }),
        )
        .await;
        fake.answer(&m, json!({ "browserContextId": "" }));
        assert!(call.await.unwrap().is_err());
        fake.quiet().await;
    }

    #[test]
    fn profile_events_name_directory_and_context() {
        let ev = |method: &str, session: Option<&str>, params: Value| crate::cdp::CdpEvent {
            method: method.into(),
            params,
            session_id: session.map(String::from),
        };
        let p = json!({ "profileDirectory": "PegboardAgent", "browserContextId": "C1" });
        assert_eq!(
            profile_event(&ev("Pegboard.profileLoaded", None, p.clone())),
            Some(ProfileEvent::Loaded {
                profile_directory: "PegboardAgent".into(),
                browser_context_id: "C1".into()
            })
        );
        assert_eq!(
            profile_event(&ev("Pegboard.profileUnloaded", None, p.clone())),
            Some(ProfileEvent::Unloaded {
                profile_directory: "PegboardAgent".into(),
                browser_context_id: "C1".into()
            })
        );
        assert_eq!(
            profile_event(&ev("Pegboard.profileLoaded", Some("S1"), p.clone())),
            None
        );
        assert_eq!(profile_event(&ev("Target.targetCreated", None, p)), None);
        for bad in [
            json!({ "profileDirectory": "PegboardAgent" }),
            json!({ "browserContextId": "C1" }),
            json!({ "profileDirectory": "PegboardAgent", "browserContextId": "" }),
        ] {
            assert_eq!(
                profile_event(&ev("Pegboard.profileLoaded", None, bad.clone())),
                None,
                "{bad}"
            );
        }
    }
}
