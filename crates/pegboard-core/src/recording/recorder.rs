//! The recorder: the daemon's own browser connection that streams the screen of every
//! session tab (spike F1, configuration B) and hands the frames to one writer per session.
//!
//! - Every frame is acknowledged as soon as it arrives (F2) and goes through the tab's
//!   `ScreenGate` (F5) and `Throttle` (at most 2 a second, the settled screen last).
//! - A tab is attached only after the browser confirms, on this connection, that it is in
//!   the session's own context (`may_record`): never a tab of the human's profile.
//! - Disk writes happen on the session's writer thread, which owns each target's
//!   `ScreenWriter` for the whole session (also across reconnects); event handling never
//!   waits for them.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use pegboard_protocol::{RecordExtra, SessionId};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Notify};

use crate::actionlog::{url_for_log, ManifestHandle, ScreenStatus, SessionLog};
use crate::cdp::{CdpConn, CdpEvent};
use crate::recording::config::RecordingConfig;
use crate::recording::format::FrameKind;
use crate::recording::screen::{
    may_record, ScreenBudget, ScreenGate, ScreenWriter, Throttle, MIN_FRAME_GAP,
};

/// Longest wait between two connection attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(5);
const FIRST_BACKOFF: Duration = Duration::from_millis(250);
/// How long `begin_session` waits for the connection (the session goes on without it).
const BEGIN_WAIT: Duration = Duration::from_secs(2);
/// How long the test hook waits for it.
const CONNECT_WAIT: Duration = Duration::from_secs(5);
/// Stopping a tab's stream when the recording ends, at most.
const STOP_LIMIT: Duration = Duration::from_secs(1);

/// The stream's settings (design 4.2; F7 kept them).
fn screencast_params() -> Value {
    json!({ "format": "jpeg", "quality": 60, "maxWidth": 1280, "everyNthFrame": 1 })
}

fn now_ts() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

pub struct RecorderOptions {
    pub ws_url: String,
    pub human_context: String,
    pub config: RecordingConfig,
}

/// A stream frame held by the throttle: when it came, the page it showed, its base64 JPEG.
struct Shot {
    ts: String,
    url: Option<String>,
    data: String,
}

/// What the session's writer thread is asked to do.
enum Job {
    Frame {
        target: String,
        shot: Shot,
        kind: FrameKind,
    },
    Mark {
        target: String,
        ts: String,
        kind: FrameKind,
        url: Option<String>,
    },
    /// Everything before it is written; the thread ends.
    End(oneshot::Sender<()>),
}

/// The recording of a session whose screen is being recorded.
struct Rec {
    jobs: std_mpsc::Sender<Job>,
    budget: Arc<ScreenBudget>,
    /// Kept for the session's other recorders (navigation, people, extras: later tasks).
    _log: SessionLog,
    _extras: Vec<RecordExtra>,
}

/// A session as the recorder knows it: its context, the targets in its scope, and its
/// recording once `begin_session` started it.
struct Sess {
    context: String,
    targets: BTreeSet<String>,
    rec: Option<Rec>,
}

/// A tab the recorder is attached to.
struct Tab {
    session: SessionId,
    /// The recorder's CDP session on it.
    cdp: String,
    /// The connection generation it was attached on.
    epoch: u64,
    gate: ScreenGate,
    throttle: Throttle<Shot>,
    /// An Off marker was written since the last frame that was kept.
    off_marked: bool,
    streaming: bool,
    /// A main-frame document was committed after the attach (the frame tree read at attach
    /// time is older then).
    nav_seen: bool,
}

#[derive(Default)]
struct State {
    conn: Option<CdpConn>,
    epoch: u64,
    sessions: HashMap<SessionId, Sess>,
    tabs: HashMap<String, Tab>,
    /// The recorder's CDP session -> its target.
    by_cdp: HashMap<String, String>,
    /// Targets being attached right now.
    attaching: HashSet<String>,
}

impl State {
    /// The session whose recording is running and has `target` in its scope.
    fn recording_session_of(&self, target: &str) -> Option<(&SessionId, &Sess)> {
        self.sessions
            .iter()
            .find(|(_, s)| s.rec.is_some() && s.targets.contains(target))
    }

    fn wanted(&self) -> bool {
        self.sessions.values().any(|s| s.rec.is_some())
    }
}

pub struct Recorder {
    opts: RecorderOptions,
    state: Mutex<State>,
    /// Wakes the connection task when a recording begins.
    wake: Arc<Notify>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
        if let Some(conn) = self.state.lock().unwrap().conn.take() {
            conn.close();
        }
    }
}

impl Recorder {
    /// Connects on first use and reconnects (backoff up to 5 s) when the connection drops.
    /// Must be called inside a tokio runtime.
    pub fn start(opts: RecorderOptions) -> Arc<Recorder> {
        let recorder = Arc::new(Recorder {
            opts,
            state: Mutex::new(State::default()),
            wake: Arc::new(Notify::new()),
            task: Mutex::new(None),
        });
        let task = tokio::spawn(supervise(
            Arc::downgrade(&recorder),
            recorder.wake.clone(),
            recorder.opts.ws_url.clone(),
        ));
        *recorder.task.lock().unwrap() = Some(task);
        recorder
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// From the session manager: a target joined the session's scope (any time, also before begin_session).
    pub fn add_target(self: &Arc<Self>, id: &SessionId, target_id: &str, context: &str) {
        let recording = {
            let mut st = self.state();
            let sess = st.sessions.entry(id.clone()).or_insert_with(|| Sess {
                context: context.to_string(),
                targets: BTreeSet::new(),
                rec: None,
            });
            sess.targets.insert(target_id.to_string());
            sess.rec.is_some()
        };
        if recording {
            let me = self.clone();
            let target = target_id.to_string();
            tokio::spawn(async move {
                me.attach(&target).await;
            });
        }
    }

    pub fn remove_target(&self, id: &SessionId, target_id: &str) {
        let detach = {
            let mut st = self.state();
            let Some(sess) = st.sessions.get_mut(id) else {
                return;
            };
            sess.targets.remove(target_id);
            if sess.targets.is_empty() && sess.rec.is_none() {
                st.sessions.remove(id);
            }
            if !st.tabs.get(target_id).is_some_and(|t| &t.session == id) {
                return;
            }
            let Some(mut tab) = st.tabs.remove(target_id) else {
                return;
            };
            st.by_cdp.remove(&tab.cdp);
            self.flush_tab(&st, target_id, &mut tab);
            st.conn
                .clone()
                .filter(|_| tab.epoch == st.epoch)
                .map(|c| (c, tab.cdp))
        };
        if let Some((conn, cdp)) = detach {
            tokio::spawn(async move {
                let _ = conn
                    .send("Target.detachFromTarget", json!({ "sessionId": cdp }), None)
                    .await;
            });
        }
    }

    /// From the daemon once the action log exists: starts the screen of the session's targets.
    pub async fn begin_session(
        self: &Arc<Self>,
        id: &SessionId,
        context: &str,
        log: SessionLog,
        manifest: ManifestHandle,
        extras: Vec<RecordExtra>,
    ) {
        let targets: Vec<String> = {
            let mut st = self.state();
            let sess = st.sessions.entry(id.clone()).or_insert_with(|| Sess {
                context: context.to_string(),
                targets: BTreeSet::new(),
                rec: None,
            });
            if sess.rec.is_some() {
                return;
            }
            sess.context = context.to_string();
            let budget = ScreenBudget::new(self.opts.config.max_per_recording_bytes());
            let jobs = spawn_writer(manifest, budget.clone());
            sess.rec = Some(Rec {
                jobs,
                budget,
                _log: log,
                _extras: extras,
            });
            sess.targets.iter().cloned().collect()
        };
        self.wake.notify_one();
        // The tabs are attached once the connection is up (the connection task attaches
        // every recorded tab when it connects, too).
        if self.connected(BEGIN_WAIT).await.is_some() {
            let attaches = targets.iter().map(|t| self.attach(t));
            futures_util::future::join_all(attaches).await;
        }
    }

    /// Stops the streams, writes the pending (settled) frames and the screen status; idempotent.
    pub async fn end_session(&self, id: &SessionId) {
        let (done, stops) = {
            let mut st = self.state();
            let Some(sess) = st.sessions.remove(id) else {
                return;
            };
            let mut stops = Vec::new();
            for target in &sess.targets {
                let Some(mut tab) = st.tabs.remove(target) else {
                    continue;
                };
                st.by_cdp.remove(&tab.cdp);
                if let Some(rec) = &sess.rec {
                    self.flush_into(rec, target, &mut tab);
                }
                if let Some(conn) = st.conn.clone().filter(|_| tab.epoch == st.epoch) {
                    stops.push((conn, tab.cdp));
                }
            }
            let done = sess.rec.map(|rec| {
                let (tx, rx) = oneshot::channel();
                let _ = rec.jobs.send(Job::End(tx));
                rx
            });
            (done, stops)
        };
        let stopping = stops.into_iter().map(|(conn, cdp)| async move {
            let stop = async {
                let _ = conn
                    .send("Page.stopScreencast", json!({}), Some(&cdp))
                    .await;
                let _ = conn
                    .send("Target.detachFromTarget", json!({ "sessionId": cdp }), None)
                    .await;
            };
            let _ = tokio::time::timeout(STOP_LIMIT, stop).await;
        });
        futures_util::future::join_all(stopping).await;
        if let Some(done) = done {
            let _ = done.await;
        }
    }

    /// Test hook (`test.recordTarget`): puts the target in the recording session's scope as
    /// the session manager would and says whether the recorder attached to it. A target it
    /// refused is taken out again.
    pub async fn record_target_for_test(self: &Arc<Self>, id: &SessionId, target_id: &str) -> bool {
        let added = {
            let mut st = self.state();
            match st.sessions.get_mut(id) {
                Some(sess) if sess.rec.is_some() => sess.targets.insert(target_id.to_string()),
                _ => return false,
            }
        };
        if self.connected(CONNECT_WAIT).await.is_none() {
            return false;
        }
        let attached = self.attach(target_id).await;
        if !attached && added {
            if let Some(sess) = self.state().sessions.get_mut(id) {
                sess.targets.remove(target_id);
            }
        }
        attached
    }

    /// For a direct capture of `target_id` (later task): the recorder's connection and CDP
    /// session on the tab and a ticket, or `None` when the tab may not be captured now
    /// (not attached, the recording capped, or the gate closed).
    pub fn capture_ticket(&self, id: &SessionId, target_id: &str) -> Option<CaptureTicket> {
        let st = self.state();
        let tab = st.tabs.get(target_id).filter(|t| &t.session == id)?;
        let sess = st.sessions.get(id)?;
        if sess.rec.as_ref()?.budget.capped() || tab.epoch != st.epoch {
            return None;
        }
        let navigation = tab.gate.capture_ticket(&self.opts.config)?;
        Some(CaptureTicket {
            conn: st.conn.clone()?,
            cdp_session: tab.cdp.clone(),
            target: target_id.to_string(),
            epoch: tab.epoch,
            navigation,
        })
    }

    /// Whether a capture sent with `ticket` may be written now that its result came: the
    /// tab is still on the document it was sent under, and that is not screen-off (F5).
    pub fn capture_valid(&self, ticket: &CaptureTicket) -> bool {
        let st = self.state();
        st.tabs.get(&ticket.target).is_some_and(|t| {
            t.epoch == ticket.epoch
                && t.cdp == ticket.cdp_session
                && t.gate.capture_valid(&self.opts.config, ticket.navigation)
        })
    }

    /// The connection, waiting up to `limit` for it.
    async fn connected(&self, limit: Duration) -> Option<CdpConn> {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(conn) = self.state().conn.clone() {
                return Some(conn);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Attaches to the target when it is in a recording session's scope and, by the
    /// browser's own answer on this connection, in that session's context; starts its
    /// stream. True when the tab is (or already was) attached on the current connection.
    async fn attach(self: &Arc<Self>, target: &str) -> bool {
        let (conn, epoch, session_context) = {
            let mut st = self.state();
            let Some(conn) = st.conn.clone() else {
                return false;
            };
            if st.tabs.get(target).is_some_and(|t| t.epoch == st.epoch) {
                return true;
            }
            let Some((_, sess)) = st.recording_session_of(target) else {
                return false;
            };
            let context = sess.context.clone();
            if !st.attaching.insert(target.to_string()) {
                return false;
            }
            (conn, st.epoch, context)
        };
        let attached = self.attach_on(&conn, epoch, target, &session_context).await;
        self.state().attaching.remove(target);
        attached
    }

    async fn attach_on(
        self: &Arc<Self>,
        conn: &CdpConn,
        epoch: u64,
        target: &str,
        session_context: &str,
    ) -> bool {
        let Ok(r) = conn
            .send("Target.getTargetInfo", json!({ "targetId": target }), None)
            .await
        else {
            return false;
        };
        let info = &r["targetInfo"];
        let context = info["browserContextId"].as_str().unwrap_or("");
        if info["type"] != "page" || !may_record(context, session_context, &self.opts.human_context)
        {
            if crate::testenv::hooks() {
                eprintln!(
                    "pegboard: recorder: not attaching {target}: not a page of the session's context"
                );
            }
            return false;
        }
        let url = info["url"].as_str().map(String::from);
        let Ok(cdp) = conn.attach(target).await else {
            return false;
        };
        // Known before any of its page events can come (they start with Page.enable).
        let registered = {
            let mut st = self.state();
            let session = st
                .recording_session_of(target)
                .filter(|_| st.epoch == epoch)
                .map(|(id, _)| id.clone());
            match session {
                Some(session) => {
                    st.by_cdp.insert(cdp.clone(), target.to_string());
                    st.tabs.insert(
                        target.to_string(),
                        Tab {
                            session,
                            cdp: cdp.clone(),
                            epoch,
                            gate: ScreenGate::new(url.as_deref()),
                            throttle: Throttle::new(MIN_FRAME_GAP),
                            off_marked: false,
                            streaming: false,
                            nav_seen: false,
                        },
                    );
                    true
                }
                None => false,
            }
        };
        if registered && self.start_stream(conn, target, &cdp).await {
            return true;
        }
        // Left the scope meanwhile, or the tab did not take the stream.
        {
            let mut st = self.state();
            if st.tabs.get(target).is_some_and(|t| t.cdp == cdp) {
                st.tabs.remove(target);
            }
            st.by_cdp.remove(&cdp);
        }
        let _ = conn
            .send("Target.detachFromTarget", json!({ "sessionId": cdp }), None)
            .await;
        false
    }

    async fn start_stream(&self, conn: &CdpConn, target: &str, cdp: &str) -> bool {
        let setup = async {
            conn.send("Page.enable", json!({}), Some(cdp)).await?;
            conn.send(
                "Page.setLifecycleEventsEnabled",
                json!({ "enabled": true }),
                Some(cdp),
            )
            .await?;
            conn.send("Page.getFrameTree", json!({}), Some(cdp)).await
        };
        let Ok(tree) = setup.await else {
            return false;
        };
        let capped = {
            let mut st = self.state();
            let capped = st
                .recording_session_of(target)
                .and_then(|(_, s)| s.rec.as_ref())
                .is_none_or(|r| r.budget.capped());
            let Some(tab) = st.tabs.get_mut(target).filter(|t| t.cdp == cdp) else {
                return false;
            };
            // The main frame's URL as of now, unless an event already told a newer one.
            if let Some(url) = tree.pointer("/frameTree/frame/url").and_then(Value::as_str) {
                if !tab.nav_seen {
                    tab.gate.committed(url);
                }
            }
            capped
        };
        if capped {
            return true;
        }
        if conn
            .send("Page.startScreencast", screencast_params(), Some(cdp))
            .await
            .is_err()
        {
            return false;
        }
        if let Some(tab) = self.state().tabs.get_mut(target).filter(|t| t.cdp == cdp) {
            tab.streaming = true;
        }
        true
    }

    /// Hands the tab's held frame to the writer when it may be written.
    fn flush_tab(&self, st: &State, target: &str, tab: &mut Tab) {
        if let Some(rec) = st.sessions.get(&tab.session).and_then(|s| s.rec.as_ref()) {
            self.flush_into(rec, target, tab);
        }
    }

    fn flush_into(&self, rec: &Rec, target: &str, tab: &mut Tab) {
        let open = tab.gate.stream_open(&self.opts.config, Instant::now());
        match tab.throttle.flush() {
            Some(shot) if open && !rec.budget.capped() => {
                let _ = rec.jobs.send(Job::Frame {
                    target: target.to_string(),
                    shot,
                    kind: FrameKind::Change,
                });
            }
            _ => {}
        }
    }

    /// One event of the recorder's connection, in the order the browser sent them.
    fn on_event(self: &Arc<Self>, conn: &CdpConn, e: CdpEvent) {
        let Some(cdp) = e.session_id.as_deref() else {
            if e.method == "Target.detachedFromTarget" {
                if let Some(cdp) = e.params["sessionId"].as_str() {
                    self.detached(cdp);
                }
            }
            return;
        };
        let frame = e.method == "Page.screencastFrame";
        if frame {
            // At once, whatever becomes of the frame (F2); the reply is not awaited.
            drop(conn.send_now(
                "Page.screencastFrameAck",
                json!({ "sessionId": e.params["sessionId"] }),
                Some(cdp),
            ));
        }
        let now = Instant::now();
        let stops = {
            let mut st = self.state();
            let Some(target) = st.by_cdp.get(cdp).cloned() else {
                return;
            };
            let Some(session) = st.tabs.get(&target).map(|t| t.session.clone()) else {
                return;
            };
            let Some(rec) = st.sessions.get(&session).and_then(|s| s.rec.as_ref()) else {
                return;
            };
            if frame && rec.budget.capped() {
                // Over the recording's limit: every stream of the session stops.
                st.tabs
                    .values_mut()
                    .filter(|t| t.session == session && t.streaming)
                    .map(|t| {
                        t.streaming = false;
                        t.cdp.clone()
                    })
                    .collect()
            } else {
                self.handle(&mut st, &target, &session, &e, now);
                Vec::new()
            }
        };
        for cdp in stops {
            let conn = conn.clone();
            tokio::spawn(async move {
                let _ = conn
                    .send("Page.stopScreencast", json!({}), Some(&cdp))
                    .await;
            });
        }
    }

    fn handle(
        &self,
        st: &mut State,
        target: &str,
        session: &SessionId,
        e: &CdpEvent,
        now: Instant,
    ) {
        let State { tabs, sessions, .. } = st;
        let (Some(tab), Some(rec)) = (
            tabs.get_mut(target),
            sessions.get(session).and_then(|s| s.rec.as_ref()),
        ) else {
            return;
        };
        let cfg = &self.opts.config;
        let p = &e.params;
        // A page target's main frame has the target's id.
        let main = |key: &str| p[key].as_str() == Some(target);
        match e.method.as_str() {
            "Page.screencastFrame" => {
                if !tab.gate.stream_open(cfg, now) {
                    self.closed(rec, target, tab);
                    return;
                }
                tab.off_marked = false;
                let shot = Shot {
                    ts: now_ts(),
                    url: tab.gate.current_url().map(url_for_log),
                    data: p["data"].as_str().unwrap_or_default().to_string(),
                };
                if let Some(shot) = tab.throttle.offer(now, shot) {
                    let _ = rec.jobs.send(Job::Frame {
                        target: target.to_string(),
                        shot,
                        kind: FrameKind::Change,
                    });
                }
                return;
            }
            "Page.frameStartedNavigating" if main("frameId") => {
                if let Some(url) = p["url"].as_str() {
                    tab.gate.navigation_started(url);
                }
            }
            "Page.frameNavigated" if p.pointer("/frame/parentId").is_none() => {
                let Some(url) = p.pointer("/frame/url").and_then(Value::as_str) else {
                    return;
                };
                let loader = p.pointer("/frame/loaderId").and_then(Value::as_str);
                let restored = p["type"] == "BackForwardCacheRestore";
                tab.gate
                    .navigated(cfg, url, loader.unwrap_or_default(), restored, now);
                tab.nav_seen = true;
            }
            "Page.navigatedWithinDocument" if main("frameId") => {
                if let Some(url) = p["url"].as_str() {
                    tab.gate.committed(url);
                    tab.nav_seen = true;
                }
            }
            "Page.lifecycleEvent" if main("frameId") => {
                if let (Some(loader), Some(name)) = (p["loaderId"].as_str(), p["name"].as_str()) {
                    tab.gate.lifecycle(loader, name);
                }
                return;
            }
            _ => return,
        }
        // A main-frame navigation event.
        if !tab.gate.stream_open(cfg, now) {
            self.closed(rec, target, tab);
        }
    }

    /// The gate is closed: the held frame goes, and an Off marker is written once.
    fn closed(&self, rec: &Rec, target: &str, tab: &mut Tab) {
        tab.throttle.discard();
        if !tab.off_marked {
            tab.off_marked = true;
            let _ = rec.jobs.send(Job::Mark {
                target: target.to_string(),
                ts: now_ts(),
                kind: FrameKind::Off,
                url: tab.gate.url().map(url_for_log),
            });
        }
    }

    /// The browser ended the recorder's session on a tab (the tab may live on): its held
    /// frame is written, and a tab still in a recording session's scope is attached again.
    fn detached(self: &Arc<Self>, cdp: &str) {
        let target = {
            let mut st = self.state();
            let Some(target) = st.by_cdp.remove(cdp) else {
                return;
            };
            let Some(mut tab) = st.tabs.remove(&target) else {
                return;
            };
            self.flush_tab(&st, &target, &mut tab);
            target
        };
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            me.attach(&target).await;
        });
    }

    /// When the next held frame is due.
    fn next_due(&self) -> Option<Instant> {
        self.state()
            .tabs
            .values()
            .filter_map(|t| t.throttle.due())
            .min()
    }

    /// Hands out the held frames whose screen has been still for the gap.
    fn on_tick(&self, now: Instant) {
        let mut st = self.state();
        let State { tabs, sessions, .. } = &mut *st;
        for (target, tab) in tabs.iter_mut() {
            let Some(rec) = sessions.get(&tab.session).and_then(|s| s.rec.as_ref()) else {
                tab.throttle.discard();
                continue;
            };
            if !tab.throttle.due().is_some_and(|due| due <= now) {
                continue;
            }
            if !tab.gate.stream_open(&self.opts.config, now) || rec.budget.capped() {
                tab.throttle.discard();
                continue;
            }
            if let Some(shot) = tab.throttle.tick(now) {
                let _ = rec.jobs.send(Job::Frame {
                    target: target.clone(),
                    shot,
                    kind: FrameKind::Change,
                });
            }
        }
    }

    /// A new connection: every tab of a recording session is attached on it.
    fn connected_now(self: &Arc<Self>, conn: CdpConn) {
        let targets: Vec<String> = {
            let mut st = self.state();
            st.conn = Some(conn);
            st.epoch += 1;
            st.sessions
                .values()
                .filter(|s| s.rec.is_some())
                .flat_map(|s| s.targets.iter().cloned())
                .collect()
        };
        for target in targets {
            let me = self.clone();
            tokio::spawn(async move {
                me.attach(&target).await;
            });
        }
    }

    /// The connection ended: the tabs' held frames are written, the tabs are no longer
    /// attached. Frames that would have come meanwhile are missing from the index.
    fn disconnected(&self) {
        let mut st = self.state();
        st.conn = None;
        st.by_cdp.clear();
        let tabs: Vec<(String, Tab)> = st.tabs.drain().collect();
        for (target, mut tab) in tabs {
            self.flush_tab(&st, &target, &mut tab);
        }
    }
}

/// What a later task needs to take a direct capture of a recorded tab and to check, when
/// its result comes, that it may be written (`Recorder::capture_valid`).
pub struct CaptureTicket {
    pub conn: CdpConn,
    pub cdp_session: String,
    pub target: String,
    epoch: u64,
    navigation: u64,
}

/// The recorder's connection: made once a recording begins, made again (with backoff) when
/// it ends while recordings are running.
async fn supervise(recorder: Weak<Recorder>, wake: Arc<Notify>, ws_url: String) {
    let mut backoff = FIRST_BACKOFF;
    loop {
        let wanted = match recorder.upgrade() {
            Some(r) => r.state().wanted(),
            None => return,
        };
        if !wanted {
            wake.notified().await;
            continue;
        }
        let (conn, queue) = match CdpConn::connect_queued(&ws_url).await {
            Ok(c) => c,
            Err(e) => {
                if crate::testenv::hooks() {
                    eprintln!("pegboard: recorder: connecting failed: {e:#}");
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            }
        };
        backoff = FIRST_BACKOFF;
        match recorder.upgrade() {
            Some(r) => r.connected_now(conn.clone()),
            None => return,
        }
        run_events(&recorder, &conn, queue).await;
        conn.close();
        match recorder.upgrade() {
            Some(r) => r.disconnected(),
            None => return,
        }
        tokio::time::sleep(backoff).await;
    }
}

/// Handles the connection's events and the throttles' timers until the connection ends.
async fn run_events(
    recorder: &Weak<Recorder>,
    conn: &CdpConn,
    mut queue: mpsc::UnboundedReceiver<CdpEvent>,
) {
    loop {
        let Some(due) = recorder.upgrade().map(|r| r.next_due()) else {
            return;
        };
        let timer = async {
            match due {
                Some(due) => tokio::time::sleep_until(tokio::time::Instant::from_std(due)).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            event = queue.recv() => {
                let Some(event) = event else { return };
                let Some(r) = recorder.upgrade() else { return };
                r.on_event(conn, event);
            }
            () = timer => {
                let Some(r) = recorder.upgrade() else { return };
                r.on_tick(Instant::now());
            }
        }
    }
}

/// The session's writer thread: owns its targets' `ScreenWriter`s for the whole recording
/// and keeps the manifest's `screen` up to date. A write error drops that frame and is told
/// in daemon.log once.
fn spawn_writer(manifest: ManifestHandle, budget: Arc<ScreenBudget>) -> std_mpsc::Sender<Job> {
    let (tx, rx) = std_mpsc::channel::<Job>();
    std::thread::spawn(move || {
        let mut w = Writer {
            screen_dir: manifest.dir().join("screen"),
            recording: manifest.get().recording_id,
            manifest,
            budget,
            writers: HashMap::new(),
            told: HashSet::new(),
        };
        for job in rx {
            match job {
                Job::Frame { target, shot, kind } => w.frame(&target, shot, kind),
                Job::Mark {
                    target,
                    ts,
                    kind,
                    url,
                } => {
                    if let Some(writer) = w.writer(&target) {
                        let result = writer.mark(&ts, kind, url.as_deref());
                        w.result(result);
                    }
                }
                Job::End(done) => {
                    let _ = done.send(());
                    return;
                }
            }
        }
    });
    tx
}

struct Writer {
    screen_dir: std::path::PathBuf,
    recording: String,
    manifest: ManifestHandle,
    budget: Arc<ScreenBudget>,
    writers: HashMap<String, ScreenWriter>,
    /// Errors already in daemon.log.
    told: HashSet<String>,
}

impl Writer {
    /// The target's writer, made on its first frame or marker.
    fn writer(&mut self, target: &str) -> Option<&mut ScreenWriter> {
        if !self.writers.contains_key(target) {
            match ScreenWriter::create(&self.screen_dir, target, self.budget.clone()) {
                Ok(w) => {
                    self.writers.insert(target.to_string(), w);
                }
                Err(e) => {
                    self.tell(&format!("screen of {target}: {e}"));
                    return None;
                }
            }
        }
        self.writers.get_mut(target)
    }

    fn frame(&mut self, target: &str, shot: Shot, kind: FrameKind) {
        let jpeg = match data_encoding::BASE64.decode(shot.data.as_bytes()) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.tell(&format!("a frame that is not base64: {e}"));
                return;
            }
        };
        let Some(writer) = self.writer(target) else {
            return;
        };
        match writer.write_frame(&shot.ts, shot.url.as_deref(), &jpeg, kind) {
            Ok(Some(_)) => self.set_screen(ScreenStatus::Recorded),
            Ok(None) => {
                // Over the limit: every tab of the recording says so, once.
                let ts = now_ts();
                let results: Vec<_> = self
                    .writers
                    .values_mut()
                    .map(|w| w.mark(&ts, FrameKind::Capped, None))
                    .collect();
                for r in results {
                    self.result(r);
                }
                self.set_screen(ScreenStatus::Capped);
            }
            Err(e) => self.tell(&format!("a frame of {target} was not written: {e}")),
        }
    }

    /// `Recorded` only over `None`; `Capped` over anything but itself.
    fn set_screen(&mut self, status: ScreenStatus) {
        let current = self.manifest.get().screen;
        let change = match status {
            ScreenStatus::Recorded => current == ScreenStatus::None,
            other => current != other,
        };
        if change {
            let result = self.manifest.update(|m| m.screen = status);
            self.result(result);
        }
    }

    fn result(&mut self, r: std::io::Result<()>) {
        if let Err(e) = r {
            self.tell(&e.to_string());
        }
    }

    fn tell(&mut self, message: &str) {
        if self.told.insert(message.to_string()) {
            eprintln!("pegboard: recording {}: {message}", self.recording);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actionlog::{ActionLog, Manifest};
    use crate::recording::format::{read_jsonl, IndexLine};

    const T1: &str = "0123456789ABCDEF0123456789ABCDEF";
    const T2: &str = "FEDCBA9876543210FEDCBA9876543210";

    /// A JPEG header (baseline, 4x3) and a few bytes of `fill`, base64.
    fn frame(fill: u8) -> String {
        let mut b = vec![
            0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0x03, 0x00, 0x04,
        ];
        b.extend([0x03, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        b.extend([0xFF, 0xDA, 0x00, 0x02, fill, fill, 0xFF, 0xD9]);
        data_encoding::BASE64.encode(&b)
    }

    fn open_log(dir: &std::path::Path) -> (SessionLog, ManifestHandle) {
        let paths = crate::paths::paths_in(dir);
        let m = Manifest::new(
            "r_abcdefgh",
            "s_abcdefgh",
            "",
            "PegboardAgent",
            "2026-01-01T00:00:00Z",
            "chrome",
            "ab",
        );
        let log = ActionLog::open(&paths, m).unwrap();
        let handle = log.manifest_handle();
        (Arc::new(tokio::sync::Mutex::new(log)), handle)
    }

    async fn end(jobs: &std_mpsc::Sender<Job>) {
        let (tx, rx) = oneshot::channel();
        jobs.send(Job::End(tx)).unwrap();
        rx.await.unwrap();
    }

    fn index(manifest: &ManifestHandle, target: &str) -> Vec<IndexLine> {
        read_jsonl(
            &manifest
                .dir()
                .join("screen")
                .join(target)
                .join("index.jsonl"),
        )
        .unwrap()
    }

    fn shot(fill: u8) -> Shot {
        Shot {
            ts: now_ts(),
            url: Some("http://a.test/".into()),
            data: frame(fill),
        }
    }

    #[tokio::test]
    async fn the_writer_records_frames_and_the_screen_status() {
        let tmp = tempfile::tempdir().unwrap();
        let (_log, manifest) = open_log(tmp.path());
        assert_eq!(manifest.get().screen, ScreenStatus::None);
        let budget = ScreenBudget::new(1_000_000);
        let jobs = spawn_writer(manifest.clone(), budget);
        jobs.send(Job::Mark {
            target: T1.into(),
            ts: now_ts(),
            kind: FrameKind::Off,
            url: Some("http://off.test/?q".into()),
        })
        .unwrap();
        // A marker alone is no screen.
        jobs.send(Job::Frame {
            target: T1.into(),
            shot: Shot {
                data: "not base64!".into(),
                ..shot(1)
            },
            kind: FrameKind::Change,
        })
        .unwrap();
        // Not a target id: nothing is made outside screen/.
        jobs.send(Job::Frame {
            target: "../x".into(),
            shot: shot(2),
            kind: FrameKind::Change,
        })
        .unwrap();
        end(&jobs).await;
        assert_eq!(manifest.get().screen, ScreenStatus::None);
        assert!(!manifest.dir().join("x").exists());
        let jobs = spawn_writer(manifest.clone(), ScreenBudget::new(1_000_000));
        jobs.send(Job::Frame {
            target: T2.into(),
            shot: shot(3),
            kind: FrameKind::Change,
        })
        .unwrap();
        end(&jobs).await;
        assert_eq!(manifest.get().screen, ScreenStatus::Recorded);
        let lines = index(&manifest, T2);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].width, Some(4));
        assert_eq!(lines[0].height, Some(3));
        let lines = index(&manifest, T1);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].kind, FrameKind::Off);
        assert_eq!(lines[0].url.as_deref(), Some("http://off.test/"));
    }

    #[tokio::test]
    async fn over_the_limit_every_tab_is_marked_capped_once() {
        let tmp = tempfile::tempdir().unwrap();
        let (_log, manifest) = open_log(tmp.path());
        let one = data_encoding::BASE64
            .decode(frame(0).as_bytes())
            .unwrap()
            .len() as u64;
        let budget = ScreenBudget::new(2 * one);
        let jobs = spawn_writer(manifest.clone(), budget.clone());
        for (i, target) in [T1, T2, T1, T2, T1].into_iter().enumerate() {
            jobs.send(Job::Frame {
                target: target.into(),
                shot: shot(i as u8),
                kind: FrameKind::Change,
            })
            .unwrap();
        }
        end(&jobs).await;
        assert!(budget.capped());
        assert_eq!(manifest.get().screen, ScreenStatus::Capped);
        for target in [T1, T2] {
            let kinds: Vec<FrameKind> = index(&manifest, target).iter().map(|l| l.kind).collect();
            assert_eq!(
                kinds,
                vec![FrameKind::Change, FrameKind::Capped],
                "{target}"
            );
        }
    }

    fn off_config() -> RecordingConfig {
        RecordingConfig {
            screen_off_domains: vec!["screen-off.test".into()],
            ..RecordingConfig::default()
        }
    }

    /// A recorder with one recording session and one attached tab on `url`, and the
    /// session's manifest. No browser: events are handed to `handle` directly.
    fn rig(dir: &std::path::Path, url: &str) -> (Arc<Recorder>, SessionId, ManifestHandle) {
        let recorder = Recorder::start(RecorderOptions {
            ws_url: "ws://127.0.0.1:9/none".into(),
            human_context: "HUMAN".into(),
            config: off_config(),
        });
        let (log, manifest) = open_log(dir);
        let id = SessionId("s_abcdefgh".into());
        let budget = ScreenBudget::new(1_000_000);
        let jobs = spawn_writer(manifest.clone(), budget.clone());
        {
            let mut st = recorder.state();
            st.sessions.insert(
                id.clone(),
                Sess {
                    context: "AGENT".into(),
                    targets: [T1.to_string()].into(),
                    rec: Some(Rec {
                        jobs,
                        budget,
                        _log: log,
                        _extras: Vec::new(),
                    }),
                },
            );
            st.by_cdp.insert("C1".into(), T1.into());
            st.tabs.insert(
                T1.into(),
                Tab {
                    session: id.clone(),
                    cdp: "C1".into(),
                    epoch: 0,
                    gate: ScreenGate::new(Some(url)),
                    throttle: Throttle::new(MIN_FRAME_GAP),
                    off_marked: false,
                    streaming: true,
                    nav_seen: false,
                },
            );
        }
        (recorder, id, manifest)
    }

    fn event(method: &str, params: Value) -> CdpEvent {
        CdpEvent {
            method: method.into(),
            params,
            session_id: Some("C1".into()),
        }
    }

    fn feed(r: &Recorder, id: &SessionId, e: CdpEvent, now: Instant) {
        let mut st = r.state();
        r.handle(&mut st, T1, id, &e, now);
    }

    fn shot_event(fill: u8) -> CdpEvent {
        event(
            "Page.screencastFrame",
            json!({ "data": frame(fill), "sessionId": 1 }),
        )
    }

    fn navigated(url: &str, loader: &str, kind: &str) -> CdpEvent {
        event(
            "Page.frameNavigated",
            json!({ "frame": { "id": T1, "url": url, "loaderId": loader }, "type": kind }),
        )
    }

    #[tokio::test]
    async fn frames_of_a_screen_off_page_never_reach_the_writer() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, manifest) = rig(tmp.path(), "http://a.test/");
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        feed(&r, &id, shot_event(1), ms(0)); // written
        feed(&r, &id, shot_event(2), ms(100)); // held
                                               // A subframe's navigation changes nothing.
        feed(
            &r,
            &id,
            event(
                "Page.frameNavigated",
                json!({ "frame": { "id": "F2", "parentId": T1, "url": "http://screen-off.test/" } }),
            ),
            ms(150),
        );
        feed(
            &r,
            &id,
            navigated("http://screen-off.test/x?s", "L1", "Navigation"),
            ms(200),
        );
        // The held frame went with the gate; frames on the off page are dropped.
        for i in 0..5 {
            feed(&r, &id, shot_event(10 + i), ms(300 + u64::from(i) * 100));
        }
        r.on_tick(ms(5000));
        // Leaving: old-page frames still come until the new document paints.
        feed(
            &r,
            &id,
            navigated("http://b.test/", "L2", "Navigation"),
            ms(6000),
        );
        feed(&r, &id, shot_event(20), ms(6100));
        let fcp = |loader: &str| {
            event(
                "Page.lifecycleEvent",
                json!({ "frameId": T1, "loaderId": loader, "name": "firstContentfulPaint" }),
            )
        };
        feed(&r, &id, fcp("L1"), ms(6150));
        feed(&r, &id, shot_event(21), ms(6200));
        feed(&r, &id, fcp("L2"), ms(6250));
        feed(&r, &id, shot_event(22), ms(6300)); // written
                                                 // A back/forward-cache restore into the off page and out again: 250 ms.
        feed(
            &r,
            &id,
            navigated("http://screen-off.test/", "L3", "BackForwardCacheRestore"),
            ms(7000),
        );
        feed(&r, &id, shot_event(30), ms(7100));
        feed(
            &r,
            &id,
            navigated("http://b.test/", "L4", "BackForwardCacheRestore"),
            ms(8000),
        );
        feed(&r, &id, shot_event(31), ms(8100));
        feed(&r, &id, shot_event(32), ms(8300)); // written
        r.end_session(&id).await;

        let lines = index(&manifest, T1);
        let summary: Vec<(FrameKind, Option<String>)> =
            lines.iter().map(|l| (l.kind, l.url.clone())).collect();
        let a = Some("http://a.test/".to_string());
        let off = Some("http://screen-off.test/x".to_string());
        let b = Some("http://b.test/".to_string());
        assert_eq!(
            summary,
            vec![
                (FrameKind::Change, a),
                (FrameKind::Off, off),
                (FrameKind::Change, b.clone()),
                (FrameKind::Off, Some("http://screen-off.test/".into())),
                (FrameKind::Change, b),
            ]
        );
        let files = lines.iter().filter(|l| l.file.is_some()).count();
        let jpgs = std::fs::read_dir(manifest.dir().join("screen").join(T1))
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|x| x == "jpg")
            })
            .count();
        assert_eq!(files, jpgs);
        assert_eq!(manifest.get().screen, ScreenStatus::Recorded);
    }

    #[tokio::test]
    async fn the_settled_frame_is_written_by_the_timer_and_at_the_end() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, manifest) = rig(tmp.path(), "http://a.test/");
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        for i in 0..10u8 {
            feed(&r, &id, shot_event(i), ms(u64::from(i) * 100));
        }
        // 0 and 500 went out; 900 is held until the screen was still for the gap.
        assert_eq!(r.next_due(), Some(ms(1400)));
        r.on_tick(ms(1399));
        r.on_tick(ms(1400));
        assert_eq!(r.next_due(), None);
        feed(&r, &id, shot_event(50), ms(5000));
        feed(&r, &id, shot_event(51), ms(5100));
        // Held at the end: written by end_session.
        r.end_session(&id).await;
        // Idempotent.
        r.end_session(&id).await;
        let lines = index(&manifest, T1);
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert!(lines.iter().all(|l| l.kind == FrameKind::Change));
        let last = std::fs::read(
            manifest
                .dir()
                .join("screen")
                .join(T1)
                .join(lines[4].file.as_deref().unwrap()),
        )
        .unwrap();
        assert_eq!(
            last,
            data_encoding::BASE64.decode(frame(51).as_bytes()).unwrap()
        );
        let held = std::fs::read(
            manifest
                .dir()
                .join("screen")
                .join(T1)
                .join(lines[2].file.as_deref().unwrap()),
        )
        .unwrap();
        assert_eq!(
            held,
            data_encoding::BASE64.decode(frame(9).as_bytes()).unwrap()
        );
    }

    #[tokio::test]
    async fn a_tab_leaving_the_scope_keeps_its_settled_frame() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, manifest) = rig(tmp.path(), "http://a.test/");
        let t0 = Instant::now();
        feed(&r, &id, shot_event(1), t0);
        feed(&r, &id, shot_event(2), t0 + Duration::from_millis(100));
        r.remove_target(&id, T1);
        assert!(r.state().tabs.is_empty());
        r.end_session(&id).await;
        assert_eq!(index(&manifest, T1).len(), 2);
    }
}
