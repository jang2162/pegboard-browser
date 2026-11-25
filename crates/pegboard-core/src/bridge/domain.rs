//! The bridge on the Pegboard fork: the browser's own CDP domain `Pegboard.*`, on the daemon's
//! control connection (the browser session that called `Pegboard.claim`; no other session
//! has the domain). The domain checks every tab and URL itself (only an agent profile's
//! tabs; `about:blank` or an absolute `http:`, `https:` or `data:` URL) and opens a data: URL
//! itself. Its requests and answers are the contract the fork's C++ side implements.

use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use pegboard_protocol::{ConfirmBy, ConfirmRequired, ConfirmRespondParams};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use super::{lock, ActivityFn, GroupId, TabBridge, CLOSE_QUIET};
use crate::cdp::{CdpConn, CdpEvent};
use crate::confirm::{BrowserResponder, ConfirmBroker, ConfirmPlace};
use crate::contexts::{is_agent_profile_dir, AgentContexts};
use crate::link::BrowserLink;

const TAB_ACTIVATED: &str = "Pegboard.tabActivated";
const PROFILE_LOADED: &str = "Pegboard.profileLoaded";
const PROFILE_UNLOADED: &str = "Pegboard.profileUnloaded";
const CONFIRMATION_ANSWERED: &str = "Pegboard.confirmationAnswered";

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

/// The browser as a responder of the confirmation broker (design 4.5): the domain shows a
/// confirmation as a dialog in the window of the session's tab (`requestConfirmation`) and
/// closes it again (`cancelConfirmation`); a person's answer comes back as the event
/// `Pegboard.confirmationAnswered` (`serve_confirmations`). The domain sends the event only
/// to the session that claimed it: the control connection.
pub struct DomainResponder {
    conn: CdpConn,
}

impl DomainResponder {
    /// The responder on `control`, the connection that claimed the domain.
    pub fn new(control: CdpConn) -> DomainResponder {
        DomainResponder { conn: control }
    }
}

/// The params of `Pegboard.requestConfirmation` for `req`, shown at `place`.
fn request_params(req: &ConfirmRequired, place: &ConfirmPlace) -> Value {
    json!({
        "confirmId": req.confirm_id,
        "browserContextId": place.browser_context_id,
        "targetIds": place.target_ids,
        "sessionId": req.session_id.0,
        "sessionName": req.session_name,
        "profileDirectory": place.profile_directory,
        "actionClass": req.class.as_str(),
        "details": req.details,
        "offerAllowForSession": req.offer_allow_for_session,
    })
}

#[async_trait]
impl BrowserResponder for DomainResponder {
    /// `Pegboard.requestConfirmation`; the domain answers `{}` once the dialog is up, and an
    /// error when it cannot show one (no window of the profile, a limit).
    async fn show(&self, req: &ConfirmRequired, place: &ConfirmPlace) -> Result<()> {
        self.conn
            .send(
                "Pegboard.requestConfirmation",
                request_params(req, place),
                None,
            )
            .await?;
        Ok(())
    }

    /// `Pegboard.cancelConfirmation`, which succeeds for a dialog that is gone. A failure
    /// (the link is gone) leaves nothing to do: the domain closes its dialogs when its owner
    /// goes, and each after its own safety time.
    async fn cancel(&self, confirm_id: &str) {
        let _ = self
            .conn
            .send(
                "Pegboard.cancelConfirmation",
                json!({ "confirmId": confirm_id }),
                None,
            )
            .await;
    }
}

/// The answer a `Pegboard.confirmationAnswered` of the control connection's own session
/// carries: exactly `{confirmId, answer}`. `None` for any other event and a malformed one.
pub fn confirmation_answer(e: &CdpEvent) -> Option<ConfirmRespondParams> {
    if e.method != CONFIRMATION_ANSWERED || e.session_id.is_some() {
        return None;
    }
    serde_json::from_value(e.params.clone()).ok()
}

/// Makes the browser a responder of `broker` (`set_browser`) while `link` lives, and hands
/// the domain's answers on `conn` to it as `ConfirmBy::Browser` (the broker takes them only
/// for confirmations the browser was asked and still shows). When the link ends the browser
/// is no responder any more: what only it could answer ends as `NoResponder`. An answer
/// that does not count (late, for an id the browser was not shown, malformed) is dropped:
/// a late one is ordinary (the dialog raced the time limit or an API answer).
pub fn serve_confirmations(
    broker: &Arc<ConfirmBroker>,
    conn: CdpConn,
    link: BrowserLink,
) -> JoinHandle<()> {
    // Before the browser is a responder: no answer can come before the watch.
    let mut events = conn.events();
    broker.set_browser(Some(Arc::new(DomainResponder::new(conn))));
    let broker = Arc::downgrade(broker);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = link.closed() => break,
                e = events.recv() => match e {
                    Ok(e) => {
                        let Some(answer) = confirmation_answer(&e) else {
                            continue;
                        };
                        let Some(b) = broker.upgrade() else {
                            return;
                        };
                        let _ = b.respond(&answer, ConfirmBy::Browser);
                    }
                    // An answer lost here leaves its command waiting until its time is up
                    // (denied): fail closed.
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            }
        }
        if let Some(b) = broker.upgrade() {
            b.set_browser(None);
        }
    })
}

/// Where the browser shows a question of a session in the agent profile `profile_dir`: the
/// profile's context now, and the session's tabs (`targets`), the one agent-browser acts on
/// (`acted_on`) first. `None` when the profile has no known context (the browser is then no
/// responder for the question).
pub fn confirm_place(
    agents: &AgentContexts,
    profile_dir: &str,
    acted_on: Option<&str>,
    targets: &[String],
) -> Option<ConfirmPlace> {
    let browser_context_id = agents.context_of(profile_dir)?;
    let mut target_ids: Vec<String> = Vec::with_capacity(targets.len());
    if let Some(first) = acted_on.filter(|t| targets.iter().any(|s| s == t)) {
        target_ids.push(first.to_string());
    }
    for t in targets {
        if !target_ids.contains(t) {
            target_ids.push(t.clone());
        }
    }
    Some(ConfirmPlace {
        browser_context_id,
        profile_directory: profile_dir.to_string(),
        target_ids,
    })
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

#[cfg(test)]
mod confirm_tests {
    use super::*;

    use std::time::Duration;

    use pegboard_protocol::{ActionClass, ConfirmAnswer, ConfirmDetail, ConfirmOutcome, SessionId};
    use serde_json::{json, Value};

    use crate::confirm::{ConfirmAsk, ConfirmDecision};
    use crate::link::fake::FakeBrowser;

    const LONG: Duration = Duration::from_secs(20);

    fn place() -> ConfirmPlace {
        ConfirmPlace {
            browser_context_id: "C1".into(),
            profile_directory: "PegboardAgent-work".into(),
            target_ids: vec!["T2".into(), "T1".into()],
        }
    }

    fn question(place: Option<ConfirmPlace>) -> ConfirmAsk {
        ConfirmAsk {
            session_id: SessionId("s_aaaaaaaa".into()),
            session_name: "build".into(),
            profile: "work".into(),
            class: ActionClass::Upload,
            details: vec![
                ConfirmDetail {
                    label: "host".into(),
                    value: "https://a.test".into(),
                },
                ConfirmDetail {
                    label: "file".into(),
                    value: "/tmp/report.pdf (12 bytes)".into(),
                },
            ],
            offer_allow_for_session: false,
            place,
        }
    }

    /// A broker whose browser responder is the domain on a fake browser.
    async fn served() -> (Arc<ConfirmBroker>, FakeBrowser, JoinHandle<()>) {
        let (link, fake) = FakeBrowser::start();
        let conn = CdpConn::over(&link).await.unwrap();
        let broker = ConfirmBroker::new(LONG);
        let serving = serve_confirmations(&broker, conn, link);
        (broker, fake, serving)
    }

    fn ask(
        broker: &Arc<ConfirmBroker>,
        place: Option<ConfirmPlace>,
    ) -> JoinHandle<ConfirmDecision> {
        let broker = broker.clone();
        tokio::spawn(async move { broker.ask(question(place)).await })
    }

    /// The next request, which must be `method` on the control connection's own session.
    async fn expect(fake: &mut FakeBrowser, method: &str) -> Value {
        let m = fake.next().await;
        assert_eq!(m["method"], method, "{m}");
        assert_eq!(m["sessionId"], "B1", "{m}");
        m
    }

    fn answered(id: &Value, answer: &str) -> Value {
        json!({ "method": "Pegboard.confirmationAnswered", "sessionId": "B1",
                "params": { "confirmId": id, "answer": answer } })
    }

    async fn decided(h: JoinHandle<ConfirmDecision>) -> ConfirmDecision {
        tokio::time::timeout(Duration::from_secs(3), h)
            .await
            .expect("the confirmation did not end within 3 s")
            .unwrap()
    }

    #[tokio::test]
    async fn request_sends_the_shape_the_domain_expects() {
        let (broker, mut fake, _serving) = served().await;
        let waiting = ask(&broker, Some(place()));
        let m = expect(&mut fake, "Pegboard.requestConfirmation").await;
        let id = broker.pending()[0].confirm_id.clone();
        assert_eq!(
            m["params"],
            json!({
                "confirmId": id,
                "browserContextId": "C1",
                "targetIds": ["T2", "T1"],
                "sessionId": "s_aaaaaaaa",
                "sessionName": "build",
                "profileDirectory": "PegboardAgent-work",
                "actionClass": "upload",
                "details": [
                    { "label": "host", "value": "https://a.test" },
                    { "label": "file", "value": "/tmp/report.pdf (12 bytes)" },
                ],
                "offerAllowForSession": false,
            })
        );
        fake.answer(&m, json!({}));
        // Ending it another way (here: the daemon stops) closes the dialog.
        broker.cancel_all();
        let m = expect(&mut fake, "Pegboard.cancelConfirmation").await;
        assert_eq!(m["params"], json!({ "confirmId": id }));
        fake.answer(&m, json!({}));
        assert_eq!(decided(waiting).await.outcome, ConfirmOutcome::Cancelled);
        fake.quiet().await;
    }

    #[tokio::test]
    async fn an_answer_event_reaches_the_broker_as_by_browser() {
        let (broker, mut fake, _serving) = served().await;
        for (answer, outcome) in [
            ("approve", ConfirmOutcome::Approved),
            ("deny", ConfirmOutcome::Denied),
        ] {
            let waiting = ask(&broker, Some(place()));
            let m = expect(&mut fake, "Pegboard.requestConfirmation").await;
            fake.answer(&m, json!({}));
            fake.send(answered(&m["params"]["confirmId"], answer));
            // The broker closes the window it answered from (the domain: no such window
            // any more, which is a success).
            let c = expect(&mut fake, "Pegboard.cancelConfirmation").await;
            assert_eq!(c["params"]["confirmId"], m["params"]["confirmId"]);
            fake.answer(&c, json!({}));
            let d = decided(waiting).await;
            assert_eq!(d.outcome, outcome);
            assert_eq!(d.by, Some(ConfirmBy::Browser));
            assert_eq!(json!(d.confirm_id), m["params"]["confirmId"]);
        }
        fake.quiet().await;
    }

    #[tokio::test]
    async fn a_refused_request_drops_the_browser_out() {
        let (broker, mut fake, _serving) = served().await;
        // Nobody else can answer: it ends at once.
        let waiting = ask(&broker, Some(place()));
        let m = expect(&mut fake, "Pegboard.requestConfirmation").await;
        fake.send(json!({ "id": m["id"], "sessionId": "B1",
            "error": { "code": -32000, "message": "no window" } }));
        assert_eq!(decided(waiting).await.outcome, ConfirmOutcome::NoResponder);
        // It may have shown a window all the same: it is closed.
        let c = expect(&mut fake, "Pegboard.cancelConfirmation").await;
        fake.answer(&c, json!({}));

        // With a subscriber it waits on, and the browser's answer no longer counts.
        let _sub = broker.subscribe().unwrap();
        let waiting = ask(&broker, Some(place()));
        let m = expect(&mut fake, "Pegboard.requestConfirmation").await;
        fake.send(json!({ "id": m["id"], "sessionId": "B1",
            "error": { "code": -32000, "message": "no window" } }));
        let c = expect(&mut fake, "Pegboard.cancelConfirmation").await;
        fake.answer(&c, json!({}));
        let id = m["params"]["confirmId"].as_str().unwrap().to_string();
        fake.send(answered(&json!(id), "approve"));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(broker.pending().len(), 1, "the browser's answer counted");
        broker
            .respond(
                &ConfirmRespondParams {
                    confirm_id: id,
                    answer: ConfirmAnswer::Deny,
                },
                ConfirmBy::Api,
            )
            .unwrap();
        let d = decided(waiting).await;
        assert_eq!(
            (d.outcome, d.by),
            (ConfirmOutcome::Denied, Some(ConfirmBy::Api))
        );
        fake.quiet().await;
    }

    #[tokio::test]
    async fn losing_the_control_link_unregisters_the_browser() {
        let (broker, mut fake, serving) = served().await;
        assert!(broker.has_responder(true));
        let waiting = ask(&broker, Some(place()));
        let m = expect(&mut fake, "Pegboard.requestConfirmation").await;
        fake.answer(&m, json!({}));
        fake.disconnect();
        assert_eq!(decided(waiting).await.outcome, ConfirmOutcome::NoResponder);
        tokio::time::timeout(Duration::from_secs(3), serving)
            .await
            .expect("the watch did not end with the link")
            .unwrap();
        assert!(!broker.has_responder(true));
        // A later question has nobody to ask.
        assert_eq!(
            broker.ask(question(Some(place()))).await.outcome,
            ConfirmOutcome::NoResponder
        );
    }

    #[tokio::test]
    async fn answers_for_unknown_ids_are_ignored() {
        let (broker, mut fake, _serving) = served().await;
        let _sub = broker.subscribe().unwrap();
        // Asked of the API subscriber only (no place): the browser was not shown it.
        let api_only = ask(&broker, None);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while broker.pending().is_empty() {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let api_id = broker.pending()[0].confirm_id.clone();
        let shown = ask(&broker, Some(place()));
        let m = expect(&mut fake, "Pegboard.requestConfirmation").await;
        fake.answer(&m, json!({}));
        let shown_id = m["params"]["confirmId"].clone();

        fake.send(answered(&json!(api_id), "approve"));
        fake.send(answered(&json!("0".repeat(32)), "approve"));
        // Malformed, or not the control connection's own session's.
        for params in [
            json!({ "confirmId": shown_id }),
            json!({ "answer": "approve" }),
            json!({ "confirmId": shown_id, "answer": "yes" }),
            json!({ "confirmId": shown_id, "answer": "approve", "sessionId": "s_bbbbbbbb" }),
        ] {
            fake.send(
                json!({ "method": "Pegboard.confirmationAnswered", "sessionId": "B1",
                              "params": params }),
            );
        }
        fake.send(
            json!({ "method": "Pegboard.confirmationAnswered", "sessionId": "S-TAB",
                          "params": { "confirmId": shown_id, "answer": "approve" } }),
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            broker.pending().len(),
            2,
            "an answer that does not count counted"
        );

        // The watch is still there for a real one.
        fake.send(answered(&shown_id, "deny"));
        let c = expect(&mut fake, "Pegboard.cancelConfirmation").await;
        assert_eq!(c["params"]["confirmId"], shown_id);
        fake.answer(&c, json!({}));
        let d = decided(shown).await;
        assert_eq!(
            (d.outcome, d.by),
            (ConfirmOutcome::Denied, Some(ConfirmBy::Browser))
        );
        broker.cancel_all();
        assert_eq!(decided(api_only).await.outcome, ConfirmOutcome::Cancelled);
        // The browser never had it: no window to close.
        fake.quiet().await;
    }

    #[test]
    fn confirmation_answer_reads_only_the_domain_event() {
        let ev = |method: &str, session: Option<&str>, params: Value| CdpEvent {
            method: method.into(),
            params,
            session_id: session.map(String::from),
        };
        let p = json!({ "confirmId": "ab", "answer": "allowForSession" });
        assert_eq!(
            confirmation_answer(&ev(CONFIRMATION_ANSWERED, None, p.clone())),
            Some(ConfirmRespondParams {
                confirm_id: "ab".into(),
                answer: ConfirmAnswer::AllowForSession
            })
        );
        assert_eq!(
            confirmation_answer(&ev(CONFIRMATION_ANSWERED, Some("S1"), p.clone())),
            None
        );
        assert_eq!(
            confirmation_answer(&ev("Pegboard.tabActivated", None, p)),
            None
        );
    }

    #[test]
    fn an_unknown_context_gives_no_place() {
        let agents = AgentContexts::default();
        let tabs = vec!["T1".to_string(), "T2".to_string()];
        assert_eq!(confirm_place(&agents, "PegboardAgent", None, &tabs), None);
        agents.set("PegboardAgent", "C1");
        // Another profile's context is not this one's.
        assert_eq!(
            confirm_place(&agents, "PegboardAgent-work", None, &tabs),
            None
        );
        // A person's profile never has a place.
        assert_eq!(confirm_place(&agents, "Default", None, &tabs), None);
        assert_eq!(
            confirm_place(&agents, "PegboardAgent", Some("T2"), &tabs),
            Some(ConfirmPlace {
                browser_context_id: "C1".into(),
                profile_directory: "PegboardAgent".into(),
                target_ids: vec!["T2".into(), "T1".into()],
            })
        );
        // A tab agent-browser acts on that is not the session's is not shown first (or at
        // all); no tab at all still has the profile's window.
        assert_eq!(
            confirm_place(&agents, "PegboardAgent", Some("T9"), &tabs).map(|p| p.target_ids),
            Some(tabs.clone())
        );
        assert_eq!(
            confirm_place(&agents, "PegboardAgent", None, &[]).map(|p| p.target_ids),
            Some(vec![])
        );
        // The profile unloaded: nowhere.
        agents.forget_context("C1");
        assert_eq!(confirm_place(&agents, "PegboardAgent", None, &tabs), None);
    }
}
