//! The recorder: the daemon's own browser connection that streams the screen of every
//! session tab (spike F1, configuration B) and hands the frames to one writer per session.
//!
//! - Every frame is acknowledged as soon as it arrives (F2) and goes through the target's
//!   `ScreenGate` (F5) and `Throttle` (at most 2 a second, the settled screen last).
//! - A tab is attached only after the browser confirms, on this connection, that it is in
//!   the session's own context (`may_record`): never a tab of the human's profile.
//! - What the recorder knows of a target's screen (gate, held frame, Off marker) belongs to
//!   the session's recording and outlives an attach: a reattach or reconnect continues it
//!   and assumes nothing about a document it did not see (`ScreenGate::attached`).
//! - Disk writes happen on the session's writer thread, which owns each target's
//!   `ScreenWriter` for the whole session (also across reconnects); event handling never
//!   waits for them.
//! - Around an agent command (`before_command`, `after_command`): the frame before is the
//!   last frame stored of the tab (no capture: owner decision after spike F7); the frame after
//!   is one direct capture, sent only on a document the gate lets through and kept only if
//!   the tab is still on it (F5), within `CAPTURE_LIMIT`.
//! - Besides the screen: the session's navigation log (tabs opened, main-frame commits,
//!   tabs closed), the marks of a possible human intervention (`events`: something in a
//!   session tab outside the session's command windows, spike F6), and with `--record` the
//!   network and console lines. All of it is written by the writer thread too.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime};

use pegboard_protocol::{RecordExtra, SessionId};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Notify};

use crate::actionlog::{url_for_log, ActionEntry, ManifestHandle, ScreenStatus, SessionLog};
use crate::cdp::{CdpConn, CdpEvent};
use crate::recording::config::RecordingConfig;
use crate::recording::events::{
    classify, console_line, nav_line, CommandWindows, NavFollow, Requests, Signal,
};
use crate::recording::format::{
    Appended, CappedLog, ConsoleLine, FrameKind, NavEvent, NavLine, NetworkLine, LINE_LOG_LIMIT,
};
use crate::recording::screen::{
    frame_name, may_record, ScreenBudget, ScreenGate, ScreenWriter, Throttle, MIN_FRAME_GAP,
};

/// Longest wait between two connection attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(5);
const FIRST_BACKOFF: Duration = Duration::from_millis(250);
/// How long `begin_session` (inside `session create`) waits for the tabs' attach. An attach
/// is four or five round trips on a local socket (tens of ms); the bound only keeps a slow
/// or unreachable browser from delaying the session, whose tabs the connection task
/// attaches later anyway. 300 ms is what one command may lose at most (design 4.6 allows
/// 1 s in the worst case).
const BEGIN_WAIT: Duration = Duration::from_millis(300);
/// How long `watch_first` (inside `session create`, before the first tab is navigated)
/// waits to attach. A first connection and an attach take tens of ms; past this the tab is
/// navigated anyway and its first attach waits for the page to paint (fail closed).
pub const WATCH_WAIT: Duration = Duration::from_millis(500);
/// How long the test hook waits for the connection.
const CONNECT_WAIT: Duration = Duration::from_secs(5);
/// Stopping a tab's stream when the recording ends, at most.
const STOP_LIMIT: Duration = Duration::from_secs(1);
/// Waiting for the writer to finish the recording's files, at most.
const WRITER_LIMIT: Duration = Duration::from_secs(5);

/// Longest wait for the capture after a command (spike F7: measured at most 183 ms). The
/// frame before is no capture, so a command waits at most this long for its pictures,
/// overlapping the look at the focused element.
pub const CAPTURE_LIMIT: Duration = Duration::from_millis(500);
/// After a capture of a target timed out, that target is not captured again until a stream
/// frame arrives from it or this long has passed (a frozen screen does not cost every
/// command the limit).
pub const FROZEN_BACKOFF: Duration = Duration::from_secs(30);
/// Widest capture, as the stream's `maxWidth` (F3).
const CAPTURE_MAX_WIDTH: f64 = 1280.0;
/// A capture that ran out of the limit counts as a frozen screen only if it had at least
/// this long itself (not when waiting for the tab used up the limit).
const FROZEN_MIN_CAPTURE: Duration = Duration::from_millis(250);
/// How much later than `CAPTURE_LIMIT` the hard stop of `after_command` comes (its steps'
/// own limits come first, so a frozen tab is still marked).
const LIMIT_GRACE: Duration = Duration::from_millis(20);
/// Between two looks at a tab that is being attached.
const TICKET_POLL: Duration = Duration::from_millis(5);
/// How long a tab switch pegboard announced (`expect_activation`) is waited for.
const EXPECT_ACTIVATION: Duration = Duration::from_secs(5);

/// `f`, or `None` when it takes longer than `limit`.
pub async fn bounded<T>(limit: Duration, f: impl Future<Output = Option<T>>) -> Option<T> {
    tokio::time::timeout(limit, f).await.ok().flatten()
}

/// Which targets' captures timed out lately (`FROZEN_BACKOFF`).
#[derive(Default)]
pub struct CaptureHealth {
    frozen_until: HashMap<String, Instant>,
}

impl CaptureHealth {
    pub fn may_capture(&self, target: &str, now: Instant) -> bool {
        self.frozen_until
            .get(target)
            .is_none_or(|until| now >= *until)
    }

    pub fn timed_out(&mut self, target: &str, now: Instant) {
        self.frozen_until
            .insert(target.to_string(), now + FROZEN_BACKOFF);
    }

    /// A stream frame came from the target: its screen is not frozen.
    pub fn frame_seen(&mut self, target: &str) {
        self.frozen_until.remove(target);
    }
}

/// The device pixel ratio from `Page.getLayoutMetrics` (device over CSS content width, as
/// the spike measured it); 1 when it cannot be read. Page zoom counts in it (a page zoomed to
/// 200 % reads as 2), which only changes the scale of the capture.
fn device_pixel_ratio(metrics: &Value) -> f64 {
    let device = metrics["contentSize"]["width"].as_f64();
    let css = metrics["cssContentSize"]["width"].as_f64();
    match (device, css) {
        (Some(d), Some(c)) if d > 0.0 && c > 0.0 => d / c,
        _ => 1.0,
    }
}

/// `Page.captureScreenshot` parameters (F3): JPEG 60. Without `metrics` (a tab known to
/// need no clip) the screen as it is. With them: when the picture would be wider than 1280
/// (a ratio above 1, or a window wider than 1280), the visual viewport scaled to 1280 wide,
/// like the stream's `maxWidth`; otherwise as it is. `None` when the viewport cannot be read.
fn capture_params(metrics: Option<&Value>) -> Option<Value> {
    let mut params = json!({ "format": "jpeg", "quality": 60 });
    let Some(metrics) = metrics else {
        return Some(params);
    };
    let dpr = device_pixel_ratio(metrics);
    let vv = &metrics["cssVisualViewport"];
    let width = vv["clientWidth"].as_f64().filter(|w| *w > 0.0)?;
    let height = vv["clientHeight"].as_f64().filter(|h| *h > 0.0)?;
    if width * dpr > CAPTURE_MAX_WIDTH {
        params["clip"] = json!({
            "x": vv["pageX"].as_f64().unwrap_or(0.0),
            "y": vv["pageY"].as_f64().unwrap_or(0.0),
            "width": width,
            "height": height,
            "scale": CAPTURE_MAX_WIDTH / (width * dpr),
        });
    }
    Some(params)
}

/// The stream's settings (design 4.2; F7 kept them).
fn screencast_params() -> Value {
    json!({ "format": "jpeg", "quality": 60, "maxWidth": 1280, "everyNthFrame": 1 })
}

/// Read in an isolated world (a page's own scripts cannot change what it sees).
/// Only an entry of type `paint`: `performance.mark('first-contentful-paint')` is a `mark`.
const PAINTED_JS: &str =
    "performance.getEntriesByName('first-contentful-paint', 'paint').length > 0";

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
        /// The number given out when the job was sent (`Rec::reserve`); `None`: the
        /// writer's next.
        number: Option<u32>,
    },
    Mark {
        target: String,
        ts: String,
        kind: FrameKind,
        url: Option<String>,
    },
    /// A direct capture after a command, written as an `After` frame under the number its
    /// name was given with; whether it was written goes back.
    Capture {
        target: String,
        shot: Shot,
        number: u32,
        reply: oneshot::Sender<bool>,
    },
    /// A line of navigation.jsonl, network.jsonl or console.jsonl.
    Line(Line),
    /// Everything before it is written; the thread ends.
    End(oneshot::Sender<()>),
}

/// A line of one of the recording's logs besides the screen.
enum Line {
    Nav(NavLine),
    Network(NetworkLine),
    Console(ConsoleLine),
}

impl Line {
    fn file(&self) -> &'static str {
        match self {
            Line::Nav(_) => "navigation.jsonl",
            Line::Network(_) => "network.jsonl",
            Line::Console(_) => "console.jsonl",
        }
    }
}

/// The recording of a session whose screen is being recorded.
struct Rec {
    jobs: std_mpsc::Sender<Job>,
    budget: Arc<ScreenBudget>,
    /// The recording id, for daemon.log.
    id: String,
    /// Set when the recording ended without waiting for the writer any longer: from then on
    /// the writer leaves the manifest alone.
    abandoned: Arc<AtomicBool>,
    /// Per target, the last frame written (stream or capture) since its last Off marker:
    /// the frame before a command. Kept by the writer.
    last: LastFrames,
    /// Per target, the number of its next frame. Given out when a frame is handed to the
    /// writer (always under the recorder's state lock, so in the order of the jobs): a
    /// capture's name is known before it is written.
    numbers: Mutex<HashMap<String, u32>>,
    /// The session's action log, for the marks of a possible human intervention.
    log: SessionLog,
    /// What `--record` asked for besides the screen.
    extras: Vec<RecordExtra>,
}

/// Target -> "<targetId>/<file>" of its last frame written.
type LastFrames = Arc<Mutex<HashMap<String, String>>>;

impl Rec {
    fn reserve(&self, target: &str) -> u32 {
        let mut numbers = self.numbers.lock().unwrap_or_else(|e| e.into_inner());
        let next = numbers.entry(target.to_string()).or_insert(1);
        let number = *next;
        *next = next.saturating_add(1);
        number
    }

    fn line(&self, line: Line) {
        let _ = self.jobs.send(Job::Line(line));
    }

    fn nav(&self, target: &str, event: NavEvent, url: Option<&str>, opener: Option<&str>) {
        self.line(Line::Nav(nav_line(&now_ts(), target, event, url, opener)));
    }

    /// Appends a mark of a possible human intervention to the session's log, in a task of
    /// its own (the log is locked while a command runs; events are not held up by it).
    /// Nothing is written once the log has ended.
    fn human(
        &self,
        session: &SessionId,
        ts: &str,
        target: &str,
        url: Option<&str>,
        what: &str,
        certain: bool,
    ) {
        let entry =
            ActionEntry::human(&session.0, ts, target, url.map(String::from), what, certain);
        let log = self.log.clone();
        let rid = self.id.clone();
        tokio::spawn(async move {
            let mut log = log.lock().await;
            if log.manifest().ended_at.is_some() {
                return;
            }
            if let Err(e) = log.append(entry) {
                eprintln!("pegboard: recording {rid}: a human intervention was not logged: {e}");
            }
        });
    }

    /// Hands a stream frame to the writer.
    fn send_frame(&self, target: &str, shot: Shot, kind: FrameKind) {
        let number = self.reserve(target);
        let _ = self.jobs.send(Job::Frame {
            target: target.to_string(),
            shot,
            kind,
            number: Some(number),
        });
    }
}

/// The frame after a command: its name, known as soon as the capture was handed to the
/// writer, and whether the writer wrote it (`unless_failed`).
pub struct AfterFrame {
    pub name: String,
    written: oneshot::Receiver<bool>,
}

impl AfterFrame {
    /// The name, unless the writer already said it could not write the frame (or is gone).
    /// Called right before the action log line is written: a write still under way keeps
    /// the name (one JPEG; it fails only when the disk does, or at the recording's limit).
    pub fn unless_failed(mut self) -> Option<String> {
        use tokio::sync::oneshot::error::TryRecvError;
        match self.written.try_recv() {
            Ok(true) | Err(TryRecvError::Empty) => Some(self.name),
            Ok(false) | Err(TryRecvError::Closed) => None,
        }
    }
}

/// What the recording knows of one target's screen, for as long as the target is in the
/// session's scope (across attaches and connections).
struct Screen {
    gate: ScreenGate,
    throttle: Throttle<Shot>,
    /// An Off marker was written since the last frame that was kept.
    off_marked: bool,
}

impl Screen {
    fn new(url: Option<&str>) -> Screen {
        Screen {
            gate: ScreenGate::new(url),
            throttle: Throttle::new(MIN_FRAME_GAP),
            off_marked: false,
        }
    }
}

/// A session as the recorder knows it: its context, the targets in its scope, and its
/// recording once `begin_session` started it.
struct Sess {
    context: String,
    targets: BTreeSet<String>,
    rec: Option<Rec>,
    screens: HashMap<String, Screen>,
    /// Its tabs are followed before the recording begins (`watch_first`).
    watched: bool,
    /// The screen is switched off for now (test hook `test.setScreen`): no stream, no
    /// capture, no frame names.
    paused: bool,
    /// When the session's commands ran (`command_window`).
    windows: CommandWindows,
    /// Per target, its navigations between request and commit.
    follow: HashMap<String, NavFollow>,
    /// Per target, its requests (`--record network`).
    requests: HashMap<String, Requests>,
    /// Targets whose `created` line is written.
    created: HashSet<String>,
}

impl Sess {
    fn new(context: &str) -> Sess {
        Sess {
            context: context.to_string(),
            targets: BTreeSet::new(),
            rec: None,
            screens: HashMap::new(),
            watched: false,
            paused: false,
            windows: CommandWindows::default(),
            follow: HashMap::new(),
            requests: HashMap::new(),
            created: HashSet::new(),
        }
    }

    fn records(&self, extra: RecordExtra) -> bool {
        self.rec.as_ref().is_some_and(|r| r.extras.contains(&extra))
    }

    /// The target's `created` line, once.
    fn created(&mut self, target: &str, url: Option<&str>, opener: Option<&str>) {
        if let Some(rec) = &self.rec {
            if self.created.insert(target.to_string()) {
                rec.nav(target, NavEvent::Created, url, opener);
                if let Some(url) = url {
                    // The page it is created on is named; an attach does not name it again.
                    self.follow
                        .entry(target.to_string())
                        .or_default()
                        .unlogged(url);
                }
            }
        }
    }
}

/// A tab the recorder is attached to on the current connection.
struct Tab {
    session: SessionId,
    /// The recorder's CDP session on it.
    cdp: String,
    /// The connection generation it was attached on.
    epoch: u64,
    streaming: bool,
    /// Whether its captures need a clip (wider than 1280), once a capture found out. A tab
    /// that needs one reads the viewport for every capture (its scroll position); one that
    /// does not is captured without that round trip (a later resize is seen on a reattach).
    clip: Option<bool>,
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
    /// Test hook: no connection before this.
    held_until: Option<Instant>,
    health: CaptureHealth,
    /// Events of the current connection handled so far, in the order the socket brought
    /// them (`Recorder::event_handled`).
    handled: u64,
    /// Tabs pegboard is switching to itself, until when (`expect_activation`).
    expected: HashMap<String, Instant>,
}

impl State {
    /// The session that has `target` in its scope and whose tabs are followed: its
    /// recording runs, or `watch_first` asked for it.
    fn recording_session_of(&self, target: &str) -> Option<(&SessionId, &Sess)> {
        self.sessions
            .iter()
            .find(|(_, s)| (s.rec.is_some() || s.watched) && s.targets.contains(target))
    }

    fn wanted(&self) -> bool {
        self.sessions.values().any(|s| s.rec.is_some() || s.watched)
    }
}

pub struct Recorder {
    opts: RecorderOptions,
    state: Mutex<State>,
    /// Told whenever an event has been handled (a capture waits for the events before it).
    progress: Notify,
    /// Wakes the connection task when a recording begins or a test hold ends.
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
            progress: Notify::new(),
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
            let sess = st
                .sessions
                .entry(id.clone())
                .or_insert_with(|| Sess::new(context));
            sess.targets.insert(target_id.to_string());
            let recording = sess.rec.is_some();
            // A tab that joins a running recording opens with its first page.
            sess.follow
                .entry(target_id.to_string())
                .or_insert_with(|| NavFollow::new(recording));
            recording
        };
        if recording {
            self.spawn_attach(target_id);
        }
    }

    /// From the session manager, before the session's first tab is navigated: attaches to
    /// it (its navigations are followed, no stream until the recording begins), so the
    /// recording knows every page the tab shows from its first one, also a screen-off page
    /// it only passes through. Waits at most `limit`; when the tab is not attached by then,
    /// its first attach assumes it may come from a screen-off page (waits for its paint).
    pub async fn watch_first(
        self: &Arc<Self>,
        id: &SessionId,
        target_id: &str,
        context: &str,
        limit: Duration,
    ) -> bool {
        {
            let mut st = self.state();
            let sess = st
                .sessions
                .entry(id.clone())
                .or_insert_with(|| Sess::new(context));
            sess.watched = true;
            sess.targets.insert(target_id.to_string());
        }
        self.wake.notify_one();
        let me = self.clone();
        let target = target_id.to_string();
        // In a task of its own, so that an attach still running at the limit is not cut.
        let attach =
            tokio::spawn(
                async move { me.connected(limit).await.is_some() && me.attach(&target).await },
            );
        let attached = matches!(tokio::time::timeout(limit, attach).await, Ok(Ok(true)));
        if !attached {
            if let Some(sess) = self.state().sessions.get_mut(id) {
                sess.screens
                    .entry(target_id.to_string())
                    .or_insert_with(|| Screen::new(None))
                    .gate
                    .assume_unseen_past();
            }
        }
        attached
    }

    fn spawn_attach(self: &Arc<Self>, target: &str) -> tokio::task::JoinHandle<bool> {
        let me = self.clone();
        let target = target.to_string();
        tokio::spawn(async move { me.attach(&target).await })
    }

    pub fn remove_target(&self, id: &SessionId, target_id: &str) {
        let detach = {
            let mut st = self.state();
            let State {
                sessions,
                tabs,
                by_cdp,
                conn,
                epoch,
                health,
                ..
            } = &mut *st;
            let Some(sess) = sessions.get_mut(id) else {
                return;
            };
            if sess.targets.remove(target_id) {
                if let Some(rec) = &sess.rec {
                    rec.nav(target_id, NavEvent::Closed, None, None);
                }
            }
            sess.follow.remove(target_id);
            sess.requests.remove(target_id);
            sess.created.remove(target_id);
            health.frame_seen(target_id);
            if let (Some(mut screen), Some(rec)) = (sess.screens.remove(target_id), &sess.rec) {
                self.flush_into(rec, target_id, &mut screen);
            }
            if sess.targets.is_empty() && sess.rec.is_none() {
                sessions.remove(id);
            }
            if !tabs.get(target_id).is_some_and(|t| &t.session == id) {
                return;
            }
            let Some(tab) = tabs.remove(target_id) else {
                return;
            };
            by_cdp.remove(&tab.cdp);
            conn.clone()
                .filter(|_| tab.epoch == *epoch)
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
    /// Waits at most `BEGIN_WAIT` for them; the connection task attaches the rest.
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
            let sess = st
                .sessions
                .entry(id.clone())
                .or_insert_with(|| Sess::new(context));
            if sess.rec.is_some() {
                return;
            }
            sess.context = context.to_string();
            let budget = ScreenBudget::new(self.opts.config.max_per_recording_bytes());
            let rid = manifest.get().recording_id;
            let abandoned = Arc::new(AtomicBool::new(false));
            let last = LastFrames::default();
            let jobs = spawn_writer(manifest, budget.clone(), abandoned.clone(), last.clone());
            sess.rec = Some(Rec {
                jobs,
                budget,
                id: rid,
                abandoned,
                last,
                numbers: Default::default(),
                log,
                extras,
            });
            // `session create` counts as a command: what its start page does is not a person's.
            sess.windows.end(Instant::now());
            let targets: Vec<String> = sess.targets.iter().cloned().collect();
            for target in &targets {
                let url = sess
                    .screens
                    .get(target)
                    .and_then(|s| s.gate.current_url().map(String::from));
                sess.created(target, url.as_deref(), None);
            }
            targets
        };
        self.wake.notify_one();
        let me = self.clone();
        let attach_all = async move {
            // Without a connection yet, the connection task attaches them once it is up.
            if me.connected(BEGIN_WAIT).await.is_some() {
                let attaches: Vec<_> = targets.iter().map(|t| me.spawn_attach(t)).collect();
                futures_util::future::join_all(attaches).await;
            }
        };
        let _ = tokio::time::timeout(BEGIN_WAIT, attach_all).await;
    }

    /// Stops the streams, writes the pending (settled) frames and the screen status; idempotent.
    /// Waits at most `WRITER_LIMIT` for the writer (then says so in daemon.log and goes on).
    pub async fn end_session(&self, id: &SessionId) {
        let (done, stops, rid) = {
            let mut st = self.state();
            let Some(mut sess) = st.sessions.remove(id) else {
                return;
            };
            if let Some(rec) = &sess.rec {
                for (target, screen) in sess.screens.iter_mut() {
                    self.flush_into(rec, target, screen);
                }
            }
            let mut stops = Vec::new();
            for target in &sess.targets {
                let Some(tab) = st.tabs.remove(target) else {
                    continue;
                };
                st.by_cdp.remove(&tab.cdp);
                if let Some(conn) = st.conn.clone().filter(|_| tab.epoch == st.epoch) {
                    stops.push((conn, tab.cdp));
                }
            }
            let rid = sess
                .rec
                .as_ref()
                .map(|r| (r.id.clone(), r.abandoned.clone()));
            let done = sess.rec.map(|rec| {
                let (tx, rx) = oneshot::channel();
                let _ = rec.jobs.send(Job::End(tx));
                rx
            });
            (done, stops, rid)
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
        if let (Some(done), Some((rid, abandoned))) = (done, rid) {
            if tokio::time::timeout(WRITER_LIMIT, done).await.is_err() {
                // The log ends now; the manifest must not change behind it.
                abandoned.store(true, Ordering::SeqCst);
                eprintln!(
                    "pegboard: recording {rid}: the screen writer did not finish within {} s; \
                     its last frames may be missing",
                    WRITER_LIMIT.as_secs()
                );
            }
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
                sess.screens.remove(target_id);
            }
        }
        attached
    }

    /// Test hook (`test.dropRecorder`): closes the connection and makes no new one for
    /// `hold`. Returns whether there was a connection.
    pub fn drop_connection_for_test(&self, hold: Duration) -> bool {
        let conn = {
            let mut st = self.state();
            st.held_until = Some(Instant::now() + hold);
            st.conn.clone()
        };
        if let Some(conn) = &conn {
            conn.close();
        }
        conn.is_some()
    }

    /// A command of the session runs from now until the window is dropped (a command that
    /// passed its checks; a refused one opens none). What happens in its tabs meanwhile and
    /// for `GRACE` after is the command's (`events`).
    pub fn command_window(self: &Arc<Self>, id: &SessionId) -> CommandWindow {
        if let Some(sess) = self.state().sessions.get_mut(id) {
            sess.windows.begin(Instant::now());
        }
        CommandWindow {
            recorder: self.clone(),
            id: id.clone(),
        }
    }

    /// pegboard itself is about to make `target` the active tab of its window (to put a
    /// profile's policy in place): the next switch to it within 5 s is not a person's.
    pub fn expect_activation(&self, target_id: &str) {
        self.state()
            .expected
            .insert(target_id.to_string(), Instant::now() + EXPECT_ACTIVATION);
    }

    /// From the bridge of the profile whose context is `context`: `target` became the
    /// active tab of its window at `at` (the extension's clock; a switch the bridge kept
    /// while its worker restarted arrives late). A switch to a tab of a running recording,
    /// outside the session's command windows, is a possible human intervention. A tab's
    /// first switch to it before its first page (a new popup) is its opening.
    pub fn activated(&self, context: &str, target: &str, at: SystemTime) {
        self.activated_as_of(context, target, at, (SystemTime::now(), Instant::now()));
    }

    /// `activated`, with the clocks' "now" given (wall clock and monotonic, read together).
    fn activated_as_of(
        &self,
        context: &str,
        target: &str,
        at: SystemTime,
        (wall, now): (SystemTime, Instant),
    ) {
        let age = wall.duration_since(at).unwrap_or_default();
        let at_instant = now.checked_sub(age).unwrap_or(now);
        let mut st = self.state();
        let expected = st
            .expected
            .remove(target)
            .is_some_and(|until| until >= at_instant);
        st.expected.retain(|_, until| *until > now);
        let Some((id, sess)) = st
            .sessions
            .iter()
            .find(|(_, s)| s.rec.is_some() && s.context == context && s.targets.contains(target))
        else {
            return;
        };
        if sess.follow.get(target).is_some_and(NavFollow::is_fresh) {
            return;
        }
        let Some(rec) = &sess.rec else {
            return;
        };
        let in_command = sess.windows.covers(at_instant);
        if let Some((what, certain)) = classify(&Signal::Activated, in_command, expected) {
            let ts = time::OffsetDateTime::from(at)
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default();
            let url = sess.screens.get(target).and_then(|s| s.gate.current_url());
            rec.human(id, &ts, target, url, &what, certain);
        }
    }

    /// At the start of an agent command (after its checks passed): the frame before it, the
    /// last frame stored of `target` (no capture is taken: owner decision after spike F7).
    /// `None` when nothing is known to show the tab's current screen: not attached on the
    /// current connection, the gate closed or held, the recording capped or paused, or no
    /// frame since the last Off marker.
    pub fn before_command(&self, id: &SessionId, target: Option<&str>) -> Option<String> {
        let target = target?;
        let st = self.state();
        st.tabs
            .get(target)
            .filter(|t| &t.session == id && t.epoch == st.epoch)?;
        let sess = st.sessions.get(id)?;
        let rec = sess.rec.as_ref()?;
        if sess.paused || rec.budget.capped() {
            return None;
        }
        if !sess
            .screens
            .get(target)?
            .gate
            .stream_open(&self.opts.config, Instant::now())
        {
            return None;
        }
        let last = rec.last.lock().unwrap_or_else(|e| e.into_inner());
        last.get(target).cloned()
    }

    /// At the end of an agent command: one direct capture of `target`, handed to the writer
    /// as an `After` frame; its name (`AfterFrame`). Takes at most `CAPTURE_LIMIT` and never
    /// fails the command; the JPEG is written after it returns. `None` at once without a
    /// connection, for a tab that is not the recording's, on a screen-off or held page, when
    /// paused or capped, and for a target whose last capture timed out (`FROZEN_BACKOFF`);
    /// `None` after the limit.
    pub async fn after_command(
        self: &Arc<Self>,
        id: &SessionId,
        target: Option<&str>,
    ) -> Option<AfterFrame> {
        let target = target?;
        let started = Instant::now();
        // The steps keep to the limit themselves (and mark a frozen tab first); this makes
        // the bound hold whatever a step does.
        let hard = tokio::time::Instant::from_std(started + CAPTURE_LIMIT + LIMIT_GRACE);
        tokio::time::timeout_at(hard, self.capture_after(id, target, started))
            .await
            .ok()
            .flatten()
    }

    async fn capture_after(
        self: &Arc<Self>,
        id: &SessionId,
        target: &str,
        started: Instant,
    ) -> Option<AfterFrame> {
        let deadline = started + CAPTURE_LIMIT;
        if !self.state().health.may_capture(target, started) {
            return None;
        }
        let until = tokio::time::Instant::from_std(deadline);
        let ticket = match tokio::time::timeout_at(until, self.ticket_when_ready(id, target)).await
        {
            Ok(ticket) => ticket?,
            Err(_) => {
                // The tab did not settle (attach, or what it shows) within the limit.
                self.state().health.timed_out(target, Instant::now());
                return None;
            }
        };
        let ready = Instant::now();
        let left = deadline.saturating_duration_since(ready);
        let (data, before) = match tokio::time::timeout(left, self.capture(&ticket)).await {
            Ok(data) => data?,
            Err(_) => {
                if left >= FROZEN_MIN_CAPTURE {
                    self.state().health.timed_out(target, Instant::now());
                }
                return None;
            }
        };
        let captured = Instant::now();
        let frame = self
            .keep_capture_in_order(&ticket.key, data, before, deadline)
            .await;
        if crate::testenv::hooks() {
            let ms = |a: Instant, b: Instant| b.duration_since(a).as_secs_f64() * 1000.0;
            let done = Instant::now();
            eprintln!(
                "pegboard: recorder: capture of {target}: ticket {:.1} ms, cdp {:.1} ms, handoff {:.1} ms",
                ms(started, ready),
                ms(ready, captured),
                ms(captured, done),
            );
        }
        frame
    }

    /// One event of the connection has been handled.
    fn event_handled(&self) {
        self.state().handled += 1;
        self.progress.notify_waiters();
    }

    /// `keep_capture` once every event the connection received before the capture's reply
    /// (`before`, in socket order) has been handled, so that a navigation the browser sent
    /// before answering is judged first (F5 is about the order the browser sends things in).
    /// Dropped when that is not reached by `deadline`, or the connection changed.
    async fn keep_capture_in_order(
        &self,
        key: &CaptureKey,
        data: String,
        before: u64,
        deadline: Instant,
    ) -> Option<AfterFrame> {
        loop {
            let progress = self.progress.notified();
            tokio::pin!(progress);
            progress.as_mut().enable();
            {
                let st = self.state();
                if st.epoch != key.epoch {
                    return None;
                }
                if st.handled >= before {
                    drop(st);
                    return self.keep_capture(key, data);
                }
            }
            let until = tokio::time::Instant::from_std(deadline);
            if tokio::time::timeout_at(until, progress).await.is_err() {
                return None;
            }
        }
    }

    /// A ticket for a capture of `target` once the tab can be told: waits while it is being
    /// attached (a tab the command just opened), and attaches it once if no one is.
    async fn ticket_when_ready(
        self: &Arc<Self>,
        id: &SessionId,
        target: &str,
    ) -> Option<CaptureTicket> {
        let mut attached = false;
        loop {
            let conn = self.state().conn.clone()?;
            match self.ticket_now(id, target) {
                Ticket::Ready(key) => return Some(CaptureTicket { conn, key }),
                Ticket::Never => return None,
                Ticket::Wait => tokio::time::sleep(TICKET_POLL).await,
                Ticket::Attach if !attached => {
                    attached = true;
                    // In a task of its own: a limit reached meanwhile does not cut the attach.
                    let _ = self.spawn_attach(target).await;
                }
                Ticket::Attach => return None,
            }
        }
    }

    /// Whether `target` may be captured now for the session `id` (F5: only on a document
    /// the gate lets through, sent after its frameNavigated; here also not while held).
    fn ticket_now(&self, id: &SessionId, target: &str) -> Ticket {
        let st = self.state();
        let Some(sess) = st.sessions.get(id) else {
            return Ticket::Never;
        };
        let recording = sess
            .rec
            .as_ref()
            .is_some_and(|r| !r.budget.capped() && !sess.paused);
        if !recording || !sess.targets.contains(target) {
            return Ticket::Never;
        }
        let Some(tab) = st.tabs.get(target).filter(|t| t.epoch == st.epoch) else {
            return if st.attaching.contains(target) {
                Ticket::Wait
            } else {
                Ticket::Attach
            };
        };
        if &tab.session != id {
            return Ticket::Never;
        }
        let Some(screen) = sess.screens.get(target) else {
            return Ticket::Never;
        };
        if screen.gate.resolving() {
            return Ticket::Wait;
        }
        if screen.gate.held_at(Instant::now()) {
            return Ticket::Never;
        }
        match screen.gate.capture_ticket(&self.opts.config) {
            Some(navigation) => Ticket::Ready(CaptureKey {
                cdp_session: tab.cdp.clone(),
                target: target.to_string(),
                session: id.clone(),
                epoch: tab.epoch,
                navigation,
            }),
            None => Ticket::Never,
        }
    }

    /// Takes the capture (after the test delay, `PEGBOARD_TEST_CAPTURE_DELAY_MS`): its base64
    /// JPEG and how many events the connection had received before the answer. Reads the
    /// viewport on the first capture of an attach, and on every one that needs a clip (F3).
    async fn capture(&self, t: &CaptureTicket) -> Option<(String, u64)> {
        if let Some(ms) = crate::testenv::var("PEGBOARD_TEST_CAPTURE_DELAY_MS")
            .and_then(|v| v.parse::<u64>().ok())
        {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
        let cdp = t.key.cdp_session.as_str();
        let clip = self
            .state()
            .tabs
            .get(&t.key.target)
            .filter(|tab| tab.cdp == cdp)
            .and_then(|tab| tab.clip);
        let params = if clip == Some(false) {
            capture_params(None)?
        } else {
            let metrics = t
                .conn
                .send("Page.getLayoutMetrics", json!({}), Some(cdp))
                .await
                .ok()?;
            let params = capture_params(Some(&metrics))?;
            if let Some(tab) = self
                .state()
                .tabs
                .get_mut(&t.key.target)
                .filter(|tab| tab.cdp == cdp)
            {
                tab.clip = Some(params.get("clip").is_some());
            }
            params
        };
        let (r, before) = t
            .conn
            .send_counted("Page.captureScreenshot", params, Some(cdp))
            .await
            .ok()?;
        let data = r["data"].as_str().filter(|d| !d.is_empty())?;
        Some((data.to_string(), before))
    }

    /// The capture's answer came: hands it to the writer if the tab is still on the document
    /// it was sent on and that may be recorded (F5), and the screen is on; its name.
    fn keep_capture(&self, key: &CaptureKey, data: String) -> Option<AfterFrame> {
        let st = self.state();
        st.tabs
            .get(&key.target)
            .filter(|t| t.epoch == key.epoch && t.cdp == key.cdp_session)?;
        let sess = st.sessions.get(&key.session)?;
        let rec = sess.rec.as_ref()?;
        if sess.paused || rec.budget.capped() {
            return None;
        }
        let gate = &sess.screens.get(&key.target)?.gate;
        // No separate hold check: a hold begins only with a navigation (which `capture_valid`
        // counts) or an attach (another CDP session, checked above).
        if !gate.capture_valid(&self.opts.config, key.navigation) {
            return None;
        }
        let (reply, written) = oneshot::channel();
        let number = rec.reserve(&key.target);
        rec.jobs
            .send(Job::Capture {
                target: key.target.clone(),
                shot: Shot {
                    ts: now_ts(),
                    url: gate.current_url().map(url_for_log),
                    data,
                },
                number,
                reply,
            })
            .ok()?;
        Some(AfterFrame {
            name: frame_name(&key.target, number),
            written,
        })
    }

    /// Test hook (`test.setScreen`): switches the session's screen off (streams stopped, no
    /// capture, no frame names) or on again. False when the session has no recording.
    pub async fn set_screen_for_test(self: &Arc<Self>, id: &SessionId, on: bool) -> bool {
        let (stops, targets) = {
            let mut st = self.state();
            let State {
                sessions,
                tabs,
                conn,
                epoch,
                ..
            } = &mut *st;
            let Some(sess) = sessions.get_mut(id).filter(|s| s.rec.is_some()) else {
                return false;
            };
            sess.paused = !on;
            if on {
                (Vec::new(), sess.targets.iter().cloned().collect())
            } else {
                for screen in sess.screens.values_mut() {
                    screen.throttle.discard();
                }
                // What the screens show from now on is not recorded: no frame names them.
                if let Some(rec) = &sess.rec {
                    rec.last.lock().unwrap_or_else(|e| e.into_inner()).clear();
                }
                let stops: Vec<_> = tabs
                    .values_mut()
                    .filter(|t| &t.session == id && t.streaming)
                    .map(|t| {
                        t.streaming = false;
                        (t.cdp.clone(), t.epoch)
                    })
                    .collect();
                let stops = match conn.clone() {
                    Some(conn) => stops
                        .into_iter()
                        .filter(|(_, e)| e == epoch)
                        .map(|(cdp, _)| (conn.clone(), cdp))
                        .collect(),
                    None => Vec::new(),
                };
                (stops, Vec::new())
            }
        };
        for (conn, cdp) in stops {
            let stop = conn.send("Page.stopScreencast", json!({}), Some(&cdp));
            let _ = tokio::time::timeout(STOP_LIMIT, stop).await;
        }
        for target in targets {
            let _ = tokio::time::timeout(STOP_LIMIT, self.spawn_attach(&target)).await;
        }
        true
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
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Attaches to the target when it is in a recording session's scope and, by the
    /// browser's own answer on this connection, in that session's context; starts its
    /// stream. True when the tab is (or already was) attached on the current connection.
    async fn attach(self: &Arc<Self>, target: &str) -> bool {
        enum Next {
            Done(bool),
            /// Attached since `watch_first`; the recording runs now: start the stream.
            Stream(CdpConn, String),
            Attach(CdpConn, u64, String),
        }
        let next = {
            let mut st = self.state();
            match st.conn.clone() {
                None => Next::Done(false),
                Some(conn) => match st.tabs.get(target).filter(|t| t.epoch == st.epoch) {
                    Some(tab) => {
                        let recording = st.sessions.get(&tab.session).is_some_and(|s| {
                            !s.paused && s.rec.as_ref().is_some_and(|r| !r.budget.capped())
                        });
                        if tab.streaming || !recording {
                            Next::Done(true)
                        } else {
                            Next::Stream(conn, tab.cdp.clone())
                        }
                    }
                    None => match st
                        .recording_session_of(target)
                        .map(|(_, s)| s.context.clone())
                    {
                        None => Next::Done(false),
                        Some(context) => {
                            if st.attaching.insert(target.to_string()) {
                                Next::Attach(conn, st.epoch, context)
                            } else {
                                Next::Done(false)
                            }
                        }
                    },
                },
            }
        };
        match next {
            Next::Done(attached) => attached,
            Next::Stream(conn, cdp) => {
                self.enable_extras(&conn, target, &cdp).await;
                self.stream(&conn, target, &cdp).await
            }
            Next::Attach(conn, epoch, context) => {
                let attached = self.attach_on(&conn, epoch, target, &context).await;
                self.state().attaching.remove(target);
                attached
            }
        }
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
        let url = info["url"].as_str();
        let Ok(cdp) = conn.attach(target).await else {
            return false;
        };
        // Known before any of its page events can come (they start with Page.enable).
        if self.register(target, &cdp, epoch, url) {
            self.created(target, url, info["openerId"].as_str());
            self.enable_extras(conn, target, &cdp).await;
            if self.start_stream(conn, target, &cdp).await {
                return true;
            }
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

    /// The tab is attached as `cdp` on connection `epoch`: its events are followed from now
    /// on, and its screen stays closed until `resolve` (`ScreenGate::attaching`).
    fn register(&self, target: &str, cdp: &str, epoch: u64, url: Option<&str>) -> bool {
        let mut st = self.state();
        if st.epoch != epoch {
            return false;
        }
        let Some(session) = st.recording_session_of(target).map(|(id, _)| id.clone()) else {
            return false;
        };
        let Some(sess) = st.sessions.get_mut(&session) else {
            return false;
        };
        sess.screens
            .entry(target.to_string())
            .or_insert_with(|| Screen::new(url))
            .gate
            .attaching(url);
        st.by_cdp.insert(cdp.to_string(), target.to_string());
        st.tabs.insert(
            target.to_string(),
            Tab {
                session,
                cdp: cdp.to_string(),
                epoch,
                streaming: false,
                clip: None,
            },
        );
        true
    }

    /// The target's `created` line, if its recording has none yet (a tab that joined it).
    fn created(&self, target: &str, url: Option<&str>, opener: Option<&str>) {
        let mut st = self.state();
        let Some(session) = st.tabs.get(target).map(|t| t.session.clone()) else {
            return;
        };
        if let Some(sess) = st.sessions.get_mut(&session) {
            sess.created(target, url, opener);
        }
    }

    /// Turns on what `--record` asked for on the recorder's session of the tab: the network
    /// metadata and the console. Also on a screen-off page (they are not the screen).
    async fn enable_extras(&self, conn: &CdpConn, target: &str, cdp: &str) {
        let (network, console) = {
            let st = self.state();
            match st.recording_session_of(target) {
                Some((_, s)) => (
                    s.records(RecordExtra::Network),
                    s.records(RecordExtra::Console),
                ),
                None => return,
            }
        };
        if network {
            let _ = conn.send("Network.enable", json!({}), Some(cdp)).await;
        }
        if console {
            let _ = conn.send("Runtime.enable", json!({}), Some(cdp)).await;
        }
    }

    /// What the attach found the tab to show: the main frame's document and whether it has
    /// painted. Returns whether the tab gets no stream (no recording yet, capped or paused),
    /// or `None` when the tab is no longer this attach's.
    fn resolve(
        &self,
        target: &str,
        cdp: &str,
        url: &str,
        loader: &str,
        painted: bool,
        neighbours: &[String],
    ) -> Option<bool> {
        let mut st = self.state();
        let session = st
            .tabs
            .get(target)
            .filter(|t| t.cdp == cdp)?
            .session
            .clone();
        let sess = st.sessions.get_mut(&session)?;
        let capped = sess.paused || sess.rec.as_ref().is_none_or(|r| r.budget.capped());
        let follow = sess.follow.entry(target.to_string()).or_default();
        follow.seen_on(url);
        // A page the tab went to while the recorder was not attached (a popup that
        // committed before the attach, a navigation during a reconnect).
        if let (true, Some(rec)) = (follow.unlogged(url), &sess.rec) {
            rec.nav(target, NavEvent::Navigated, Some(url), None);
        }
        let gate = &mut sess.screens.get_mut(target)?.gate;
        let neighbours: Vec<&str> = neighbours.iter().map(String::as_str).collect();
        gate.attached(&self.opts.config, url, loader, painted, &neighbours);
        if crate::testenv::hooks() {
            let now = Instant::now();
            let stream = if gate.stream_open(&self.opts.config, now) {
                "open"
            } else if gate.held_at(now) {
                "held until the page paints"
            } else {
                "closed"
            };
            eprintln!(
                "pegboard: recorder: attached {target} on {} (painted {painted}, next to {:?}): stream {stream}",
                url_for_log(url),
                neighbours.iter().map(|u| url_for_log(u)).collect::<Vec<_>>(),
            );
        }
        Some(capped)
    }

    async fn start_stream(&self, conn: &CdpConn, target: &str, cdp: &str) -> bool {
        let setup = async {
            conn.send("Page.enable", json!({}), Some(cdp)).await?;
            // Before the tree is read: a paint after it comes as an event.
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
        let frame = &tree["frameTree"]["frame"];
        let url = frame["url"].as_str().unwrap_or_default();
        let loader = frame["loaderId"].as_str().unwrap_or_default();
        let cfg = &self.opts.config;
        // Only a list of screen-off domains can make a document wait for its paint.
        let judged = !cfg.screen_off_domains.is_empty() && !cfg.screen_off(url);
        let painted = judged && painted(conn, cdp, target).await;
        let neighbours = if judged {
            neighbours(conn, cdp).await
        } else {
            Vec::new()
        };
        let Some(capped) = self.resolve(target, cdp, url, loader, painted, &neighbours) else {
            return false;
        };
        // No recording yet (only watched), over its limit or paused: no stream.
        if capped {
            return true;
        }
        self.stream(conn, target, cdp).await
    }

    /// Starts the tab's stream.
    async fn stream(&self, conn: &CdpConn, target: &str, cdp: &str) -> bool {
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

    /// Hands the target's held frame to the writer when it may be written.
    fn flush_into(&self, rec: &Rec, target: &str, screen: &mut Screen) {
        let open = screen.gate.stream_open(&self.opts.config, Instant::now());
        match screen.throttle.flush() {
            Some(shot) if open && !rec.budget.capped() => {
                rec.send_frame(target, shot, FrameKind::Change);
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
            let capped = st
                .sessions
                .get(&session)
                .and_then(|s| s.rec.as_ref())
                .is_some_and(|r| r.budget.capped());
            if frame && capped {
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
        if e.method == "Page.screencastFrame" {
            // The tab's screen is not frozen.
            st.health.frame_seen(target);
        }
        let Some(sess) = st.sessions.get_mut(session) else {
            return;
        };
        let p = &e.params;
        // A page target's main frame has the target's id.
        let main = |key: &str| p[key].as_str() == Some(target);
        // What is not the screen: the extras, and who asked for a navigation.
        if e.method.starts_with("Network.") {
            if sess.records(RecordExtra::Network) {
                let requests = sess.requests.entry(target.to_string()).or_default();
                if let (Some(line), Some(rec)) =
                    (requests.event(&e.method, p, &now_ts(), target), &sess.rec)
                {
                    rec.line(Line::Network(line));
                }
            }
            return;
        }
        if e.method == "Runtime.consoleAPICalled" || e.method == "Runtime.exceptionThrown" {
            if let (true, Some(rec)) = (sess.records(RecordExtra::Console), &sess.rec) {
                if let Some(line) = console_line(&e.method, p, &now_ts(), target) {
                    rec.line(Line::Console(line));
                }
            }
            return;
        }
        let follow = sess.follow.entry(target.to_string()).or_default();
        match e.method.as_str() {
            "Page.frameRequestedNavigation" if main("frameId") => {
                follow.requested(
                    p["url"].as_str().unwrap_or_default(),
                    p["reason"].as_str().unwrap_or_default(),
                    p["disposition"].as_str().unwrap_or_default(),
                );
                return;
            }
            "Page.frameStartedNavigating" if main("frameId") => {
                follow.started(p["url"].as_str().unwrap_or_default());
            }
            "Page.frameNavigated" if p.pointer("/frame/parentId").is_none() => {
                let Some(url) = p.pointer("/frame/url").and_then(Value::as_str) else {
                    return;
                };
                let signal = follow.committed(url);
                if let Some(rec) = &sess.rec {
                    rec.nav(target, NavEvent::Navigated, Some(url), None);
                    let mark = signal.and_then(|s| classify(&s, sess.windows.covers(now), false));
                    if let Some((what, certain)) = mark {
                        rec.human(session, &now_ts(), target, Some(url), &what, certain);
                    }
                }
            }
            _ => {}
        }
        let Sess {
            rec,
            screens,
            paused,
            ..
        } = sess;
        let Some(screen) = screens.get_mut(target) else {
            return;
        };
        // None while the tab is only watched: its navigations count, its frames do not.
        let rec = rec.as_ref();
        let cfg = &self.opts.config;
        match e.method.as_str() {
            "Page.screencastFrame" => {
                let Some(rec) = rec.filter(|_| !*paused) else {
                    // Only watched, or a frame still on its way after the stream stopped.
                    screen.throttle.discard();
                    return;
                };
                if !screen.gate.stream_open(cfg, now) {
                    self.closed(rec, target, screen);
                    return;
                }
                screen.off_marked = false;
                let shot = Shot {
                    ts: now_ts(),
                    url: screen.gate.current_url().map(url_for_log),
                    data: p["data"].as_str().unwrap_or_default().to_string(),
                };
                if let Some(shot) = screen.throttle.offer(now, shot) {
                    rec.send_frame(target, shot, FrameKind::Change);
                }
                return;
            }
            "Page.frameStartedNavigating" if main("frameId") => {
                if let Some(url) = p["url"].as_str() {
                    screen.gate.navigation_started(url);
                }
            }
            "Page.frameStoppedLoading" if main("frameId") => {
                screen.gate.navigation_stopped();
                return;
            }
            "Page.frameNavigated" if p.pointer("/frame/parentId").is_none() => {
                let Some(url) = p.pointer("/frame/url").and_then(Value::as_str) else {
                    return;
                };
                let loader = p.pointer("/frame/loaderId").and_then(Value::as_str);
                let restored = p["type"] == "BackForwardCacheRestore";
                screen
                    .gate
                    .navigated(cfg, url, loader.unwrap_or_default(), restored, now);
            }
            "Page.navigatedWithinDocument" if main("frameId") => {
                if let Some(url) = p["url"].as_str() {
                    screen.gate.committed(url);
                }
            }
            "Page.lifecycleEvent" if main("frameId") => {
                if let (Some(loader), Some(name)) = (p["loaderId"].as_str(), p["name"].as_str()) {
                    screen.gate.lifecycle(loader, name);
                }
                return;
            }
            _ => return,
        }
        // A main-frame navigation event.
        if !screen.gate.stream_open(cfg, now) {
            match rec {
                Some(rec) => self.closed(rec, target, screen),
                None => screen.throttle.discard(),
            }
        }
    }

    /// The gate is closed: the held frame goes, and an Off marker is written once (not while
    /// an attach is still finding out what the tab shows: that short gap has no marker).
    fn closed(&self, rec: &Rec, target: &str, screen: &mut Screen) {
        screen.throttle.discard();
        if !screen.off_marked && !screen.gate.resolving() {
            screen.off_marked = true;
            let _ = rec.jobs.send(Job::Mark {
                target: target.to_string(),
                ts: now_ts(),
                kind: FrameKind::Off,
                url: screen.gate.url().map(url_for_log),
            });
        }
    }

    /// The browser ended the recorder's session on a tab (the tab may live on): a tab still
    /// in a recording session's scope is attached again. Its screen state is kept.
    fn detached(self: &Arc<Self>, cdp: &str) {
        let target = {
            let mut st = self.state();
            let Some(target) = st.by_cdp.remove(cdp) else {
                return;
            };
            st.tabs.remove(&target);
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
            .sessions
            .values()
            .flat_map(|s| s.screens.values())
            .filter_map(|s| s.throttle.due())
            .min()
    }

    /// Hands out the held frames whose screen has been still for the gap.
    fn on_tick(&self, now: Instant) {
        let mut st = self.state();
        for sess in st.sessions.values_mut() {
            let Sess {
                rec,
                screens,
                paused,
                ..
            } = sess;
            for (target, screen) in screens.iter_mut() {
                let Some(rec) = rec.as_ref().filter(|_| !*paused) else {
                    screen.throttle.discard();
                    continue;
                };
                if !screen.throttle.due().is_some_and(|due| due <= now) {
                    continue;
                }
                if !screen.gate.stream_open(&self.opts.config, now) || rec.budget.capped() {
                    screen.throttle.discard();
                    continue;
                }
                if let Some(shot) = screen.throttle.tick(now) {
                    rec.send_frame(target, shot, FrameKind::Change);
                }
            }
        }
    }

    /// A new connection: every tab of a recording session is attached on it.
    fn connected_now(self: &Arc<Self>, conn: CdpConn) {
        let targets: Vec<String> = {
            let mut st = self.state();
            st.conn = Some(conn);
            st.epoch += 1;
            st.handled = 0;
            st.sessions
                .values()
                .filter(|s| s.rec.is_some())
                .flat_map(|s| s.targets.iter().cloned())
                .collect()
        };
        for target in targets {
            self.spawn_attach(&target);
        }
    }

    /// The connection ended: no tab is attached any more. What each recording knows of
    /// its screens stays; frames that would have come meanwhile are missing from the index.
    fn disconnected(&self) {
        let mut st = self.state();
        st.conn = None;
        st.by_cdp.clear();
        st.tabs.clear();
    }

    /// The test hold, if one is running.
    fn held_until(&self) -> Option<Instant> {
        self.state().held_until.filter(|t| *t > Instant::now())
    }
}

/// A command's window (`Recorder::command_window`); it ends when dropped, also when the
/// command's future is.
pub struct CommandWindow {
    recorder: Arc<Recorder>,
    id: SessionId,
}

impl Drop for CommandWindow {
    fn drop(&mut self) {
        if let Some(sess) = self.recorder.state().sessions.get_mut(&self.id) {
            sess.windows.end(Instant::now());
        }
    }
}

/// Whether the main frame's document has had its first contentful paint, read in an
/// isolated world of the recorder's own. False when it cannot be told.
async fn painted(conn: &CdpConn, cdp: &str, frame: &str) -> bool {
    let read = async {
        let world = conn
            .send(
                "Page.createIsolatedWorld",
                json!({ "frameId": frame, "worldName": "pegboard-recorder" }),
                Some(cdp),
            )
            .await?;
        let context = world["executionContextId"].clone();
        conn.send(
            "Runtime.evaluate",
            json!({ "expression": PAINTED_JS, "contextId": context, "returnByValue": true }),
            Some(cdp),
        )
        .await
    };
    match read.await {
        Ok(r) => r.pointer("/result/value") == Some(&Value::Bool(true)),
        Err(_) => false,
    }
}

/// The URLs of the history entries next to the current one (before and after it: a back
/// navigation leaves the page it came from after it). Empty when they cannot be read.
async fn neighbours(conn: &CdpConn, cdp: &str) -> Vec<String> {
    let Ok(h) = conn
        .send("Page.getNavigationHistory", json!({}), Some(cdp))
        .await
    else {
        return Vec::new();
    };
    let Some(current) = h["currentIndex"]
        .as_u64()
        .and_then(|c| usize::try_from(c).ok())
    else {
        return Vec::new();
    };
    [current.checked_sub(1), current.checked_add(1)]
        .into_iter()
        .flatten()
        .filter_map(|i| h["entries"][i]["url"].as_str().map(String::from))
        .collect()
}

/// A capture's connection and what its answer is checked against (`keep_capture`).
struct CaptureTicket {
    conn: CdpConn,
    key: CaptureKey,
}

struct CaptureKey {
    cdp_session: String,
    target: String,
    session: SessionId,
    epoch: u64,
    /// The gate's navigation count when it was sent.
    navigation: u64,
}

/// Whether a target can be captured now.
enum Ticket {
    Ready(CaptureKey),
    /// Being attached, or attached and still finding out what it shows.
    Wait,
    /// In the recording's scope, not attached, and no attach under way.
    Attach,
    Never,
}

/// The recorder's connection: made once a recording begins, made again (with backoff) when
/// it ends while recordings are running.
async fn supervise(recorder: Weak<Recorder>, wake: Arc<Notify>, ws_url: String) {
    let mut backoff = FIRST_BACKOFF;
    loop {
        let (wanted, held) = match recorder.upgrade() {
            Some(r) => {
                let wanted = r.state().wanted();
                (wanted, r.held_until())
            }
            None => return,
        };
        if let Some(until) = held {
            tokio::time::sleep_until(tokio::time::Instant::from_std(until)).await;
            continue;
        }
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
                test_event_delay(&event).await;
                let Some(r) = recorder.upgrade() else { return };
                r.on_event(conn, event);
                r.event_handled();
            }
            () = timer => {
                let Some(r) = recorder.upgrade() else { return };
                r.on_tick(Instant::now());
            }
        }
    }
}

/// Test hook `PEGBOARD_TEST_EVENT_DELAY_MS`: the recorder handles a main-frame
/// `Page.frameNavigated` (and everything after it) that much later, as a busy event loop would.
async fn test_event_delay(event: &CdpEvent) {
    if event.method != "Page.frameNavigated" || event.params.pointer("/frame/parentId").is_some() {
        return;
    }
    if let Some(ms) =
        crate::testenv::var("PEGBOARD_TEST_EVENT_DELAY_MS").and_then(|v| v.parse::<u64>().ok())
    {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}

/// The session's writer thread: owns its targets' `ScreenWriter`s for the whole recording
/// and keeps the manifest's `screen` up to date. A write error drops that frame and is told
/// in daemon.log once.
fn spawn_writer(
    manifest: ManifestHandle,
    budget: Arc<ScreenBudget>,
    abandoned: Arc<AtomicBool>,
    last: LastFrames,
) -> std_mpsc::Sender<Job> {
    let (tx, rx) = std_mpsc::channel::<Job>();
    std::thread::spawn(move || {
        let mut w = Writer {
            screen_dir: manifest.dir().join("screen"),
            recording: manifest.get().recording_id,
            manifest,
            budget,
            abandoned,
            last,
            writers: HashMap::new(),
            logs: HashMap::new(),
            told: HashSet::new(),
        };
        for job in rx {
            match job {
                Job::Frame {
                    target,
                    shot,
                    kind,
                    number,
                } => {
                    w.frame(&target, shot, kind, number);
                }
                Job::Mark {
                    target,
                    ts,
                    kind,
                    url,
                } => {
                    if kind == FrameKind::Off {
                        w.forget(&target);
                    }
                    if let Some(writer) = w.writer(&target) {
                        let result = writer.mark(&ts, kind, url.as_deref());
                        w.result(result);
                    }
                }
                Job::Capture {
                    target,
                    shot,
                    number,
                    reply,
                } => {
                    let written = w.frame(&target, shot, FrameKind::After, Some(number));
                    let _ = reply.send(written.is_some());
                }
                Job::Line(line) => w.line(line),
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
    abandoned: Arc<AtomicBool>,
    last: LastFrames,
    writers: HashMap<String, ScreenWriter>,
    /// navigation.jsonl, network.jsonl, console.jsonl: made on their first line, each at
    /// most `LINE_LOG_LIMIT`.
    logs: HashMap<&'static str, CappedLog>,
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

    /// Writes a frame; its name when it was written.
    fn frame(
        &mut self,
        target: &str,
        shot: Shot,
        kind: FrameKind,
        number: Option<u32>,
    ) -> Option<String> {
        let jpeg = match data_encoding::BASE64.decode(shot.data.as_bytes()) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.tell(&format!("a frame that is not base64: {e}"));
                return None;
            }
        };
        let writer = self.writer(target)?;
        let url = shot.url.as_deref();
        let written = match number {
            Some(n) => writer.write_frame_as(n, &shot.ts, url, &jpeg, kind),
            None => writer.write_frame(&shot.ts, url, &jpeg, kind),
        };
        match written {
            Ok(Some(name)) => {
                self.set_screen(ScreenStatus::Recorded);
                self.lock_last().insert(target.to_string(), name.clone());
                return Some(name);
            }
            Ok(None) => {
                self.lock_last().clear();
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
        None
    }

    /// Appends a line to its log. A line that cannot be written is dropped (told once).
    fn line(&mut self, line: Line) {
        let name = line.file();
        if !self.logs.contains_key(name) {
            match CappedLog::create(&self.manifest.dir().join(name), LINE_LOG_LIMIT) {
                Ok(w) => {
                    self.logs.insert(name, w);
                }
                Err(e) => return self.tell(&format!("{name}: {e}")),
            }
        }
        let Some(log) = self.logs.get_mut(name) else {
            return;
        };
        let ts = now_ts();
        let written = match &line {
            Line::Nav(l) => log.append(l, &ts),
            Line::Network(l) => log.append(l, &ts),
            Line::Console(l) => log.append(l, &ts),
        };
        match written {
            Ok(Appended::Written | Appended::Dropped) => {}
            Ok(Appended::Capped) => self.tell(&format!(
                "{name} reached its limit of {} MB; no more lines are written to it",
                LINE_LOG_LIMIT / (1024 * 1024)
            )),
            Err(e) => self.tell(&format!("a line of {name} was not written: {e}")),
        }
    }

    fn lock_last(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.last.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The target's screen is off: its last frame is not its screen any more.
    fn forget(&mut self, target: &str) {
        self.lock_last().remove(target);
    }

    /// `Recorded` only over `None`; `Capped` over anything but itself.
    fn set_screen(&mut self, status: ScreenStatus) {
        if self.abandoned.load(Ordering::SeqCst) {
            return;
        }
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
        let jobs = spawn_writer(
            manifest.clone(),
            budget,
            Default::default(),
            Default::default(),
        );
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
            number: None,
        })
        .unwrap();
        // Not a target id: nothing is made outside screen/.
        jobs.send(Job::Frame {
            target: "../x".into(),
            shot: shot(2),
            kind: FrameKind::Change,
            number: None,
        })
        .unwrap();
        end(&jobs).await;
        assert_eq!(manifest.get().screen, ScreenStatus::None);
        assert!(!manifest.dir().join("x").exists());
        let jobs = spawn_writer(
            manifest.clone(),
            ScreenBudget::new(1_000_000),
            Default::default(),
            Default::default(),
        );
        jobs.send(Job::Frame {
            target: T2.into(),
            shot: shot(3),
            kind: FrameKind::Change,
            number: None,
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
        let jobs = spawn_writer(
            manifest.clone(),
            budget.clone(),
            Default::default(),
            Default::default(),
        );
        for (i, target) in [T1, T2, T1, T2, T1].into_iter().enumerate() {
            jobs.send(Job::Frame {
                target: target.into(),
                shot: shot(i as u8),
                kind: FrameKind::Change,
                number: None,
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
        rig_with(dir, url, Vec::new())
    }

    fn rig_with(
        dir: &std::path::Path,
        url: &str,
        extras: Vec<RecordExtra>,
    ) -> (Arc<Recorder>, SessionId, ManifestHandle) {
        let recorder = Recorder::start(RecorderOptions {
            ws_url: "ws://127.0.0.1:9/none".into(),
            human_context: "HUMAN".into(),
            config: off_config(),
        });
        let (log, manifest) = open_log(dir);
        let id = SessionId("s_abcdefgh".into());
        let budget = ScreenBudget::new(1_000_000);
        let last = LastFrames::default();
        let jobs = spawn_writer(
            manifest.clone(),
            budget.clone(),
            Default::default(),
            last.clone(),
        );
        {
            let mut st = recorder.state();
            let mut sess = Sess::new("AGENT");
            sess.targets.insert(T1.to_string());
            sess.rec = Some(Rec {
                jobs,
                budget,
                id: "r_abcdefgh".into(),
                abandoned: Default::default(),
                last,
                numbers: Default::default(),
                log,
                extras,
            });
            st.sessions.insert(id.clone(), sess);
        }
        // As an attach does: registered, then the frame tree (a painted document).
        assert!(recorder.register(T1, "C1", 0, Some(url)));
        assert_eq!(
            recorder.resolve(T1, "C1", url, "L0", true, &[]),
            Some(false)
        );
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

    #[tokio::test]
    async fn a_reattach_keeps_what_the_recording_knew_of_the_screen() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, manifest) = rig(tmp.path(), "http://a.test/");
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        feed(&r, &id, shot_event(1), ms(0)); // written
                                             // The connection drops and comes back; the tab still shows the same document.
        r.disconnected();
        assert!(r.register(T1, "C2", 0, Some("http://a.test/")));
        // Until the tree is read nothing is kept.
        feed(&r, &id, shot_event(2), ms(100));
        assert_eq!(
            r.resolve(T1, "C2", "http://a.test/", "L0", false, &[]),
            Some(false)
        );
        // The throttle went on: within the gap of the last frame a frame is held.
        feed(&r, &id, shot_event(3), ms(200));
        assert_eq!(r.next_due(), Some(ms(700)));
        // Off, then away while the tab went on to b.
        feed(
            &r,
            &id,
            navigated("http://screen-off.test/", "L1", "Navigation"),
            ms(1000),
        );
        feed(&r, &id, shot_event(4), ms(1100));
        r.disconnected();
        assert!(r.register(T1, "C3", 0, Some("http://screen-off.test/")));
        assert_eq!(
            r.resolve(T1, "C3", "http://b.test/", "L2", false, &[]),
            Some(false)
        );
        // b has not painted: what is shown may still be the off page.
        feed(&r, &id, shot_event(5), ms(2000));
        feed(&r, &id, shot_event(6), ms(2600));
        feed(
            &r,
            &id,
            event(
                "Page.lifecycleEvent",
                json!({ "frameId": T1, "loaderId": "L2", "name": "firstContentfulPaint" }),
            ),
            ms(2700),
        );
        feed(&r, &id, shot_event(7), ms(2800)); // written
        r.end_session(&id).await;
        let lines = index(&manifest, T1);
        let summary: Vec<(FrameKind, Option<String>)> =
            lines.iter().map(|l| (l.kind, l.url.clone())).collect();
        assert_eq!(
            summary,
            vec![
                (FrameKind::Change, Some("http://a.test/".into())),
                // One marker for the whole closed stretch, across the reattach.
                (FrameKind::Off, Some("http://screen-off.test/".into())),
                (FrameKind::Change, Some("http://b.test/".into())),
            ]
        );
        let last = std::fs::read(
            manifest
                .dir()
                .join("screen")
                .join(T1)
                .join(lines[2].file.as_deref().unwrap()),
        )
        .unwrap();
        assert_eq!(
            last,
            data_encoding::BASE64.decode(frame(7).as_bytes()).unwrap()
        );
    }

    #[tokio::test]
    async fn a_cancelled_navigation_to_an_off_page_lets_frames_through_again() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, manifest) = rig(tmp.path(), "http://a.test/");
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        feed(
            &r,
            &id,
            event(
                "Page.frameStartedNavigating",
                json!({ "frameId": T1, "url": "http://screen-off.test/" }),
            ),
            ms(0),
        );
        feed(&r, &id, shot_event(1), ms(100));
        feed(
            &r,
            &id,
            event("Page.frameStoppedLoading", json!({ "frameId": T1 })),
            ms(200),
        );
        feed(&r, &id, shot_event(2), ms(300)); // written
        r.end_session(&id).await;
        let kinds: Vec<FrameKind> = index(&manifest, T1).iter().map(|l| l.kind).collect();
        assert_eq!(kinds, vec![FrameKind::Off, FrameKind::Change]);
    }

    #[tokio::test]
    async fn a_tab_watched_before_the_recording_remembers_an_off_start_page() {
        // As `session create --url <off page that replaces itself>`: the recorder follows the
        // first tab before it is navigated, the recording begins after.
        let tmp = tempfile::tempdir().unwrap();
        let r = Recorder::start(RecorderOptions {
            ws_url: "ws://127.0.0.1:9/none".into(),
            human_context: "HUMAN".into(),
            config: off_config(),
        });
        let id = SessionId("s_abcdefgh".into());
        {
            let mut st = r.state();
            let mut sess = Sess::new("AGENT");
            sess.watched = true;
            sess.targets.insert(T1.to_string());
            st.sessions.insert(id.clone(), sess);
        }
        assert!(r.register(T1, "C1", 0, Some("about:blank")));
        // No recording yet: no stream (reported as capped).
        assert_eq!(
            r.resolve(T1, "C1", "about:blank", "L0", false, &[]),
            Some(true)
        );
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        feed(
            &r,
            &id,
            navigated("http://screen-off.test/sso", "L1", "Navigation"),
            ms(0),
        );
        feed(&r, &id, shot_event(1), ms(10));
        feed(
            &r,
            &id,
            navigated("http://b.test/", "L2", "Navigation"),
            ms(50),
        );
        // The recording begins (what begin_session adds).
        let (log, manifest) = open_log(tmp.path());
        let budget = ScreenBudget::new(1_000_000);
        let last = LastFrames::default();
        let jobs = spawn_writer(
            manifest.clone(),
            budget.clone(),
            Default::default(),
            last.clone(),
        );
        r.state().sessions.get_mut(&id).unwrap().rec = Some(Rec {
            jobs,
            budget,
            id: "r_abcdefgh".into(),
            abandoned: Default::default(),
            last,
            numbers: Default::default(),
            log,
            extras: Vec::new(),
        });
        // b has not painted: the off page may still be on the screen.
        feed(&r, &id, shot_event(2), ms(100));
        feed(&r, &id, shot_event(3), ms(700));
        feed(
            &r,
            &id,
            event(
                "Page.lifecycleEvent",
                json!({ "frameId": T1, "loaderId": "L2", "name": "firstContentfulPaint" }),
            ),
            ms(800),
        );
        feed(&r, &id, shot_event(4), ms(900)); // written
        r.end_session(&id).await;
        let lines = index(&manifest, T1);
        let kinds: Vec<FrameKind> = lines.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, vec![FrameKind::Off, FrameKind::Change], "{lines:?}");
        let written = std::fs::read(
            manifest
                .dir()
                .join("screen")
                .join(T1)
                .join(lines[1].file.as_deref().unwrap()),
        )
        .unwrap();
        assert_eq!(
            written,
            data_encoding::BASE64.decode(frame(4).as_bytes()).unwrap()
        );
    }

    #[test]
    fn the_paint_probe_reads_paint_entries_only() {
        // `performance.mark('first-contentful-paint')` makes an entry of type `mark`.
        assert!(PAINTED_JS.contains("getEntriesByName('first-contentful-paint', 'paint')"));
    }

    // ---- command frames (task 4) ----

    #[tokio::test(start_paused = true)]
    async fn bounded_gives_up_at_the_limit() {
        let t0 = tokio::time::Instant::now();
        let slow = bounded(CAPTURE_LIMIT, async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Some(1)
        })
        .await;
        assert_eq!(slow, None);
        assert_eq!(t0.elapsed(), CAPTURE_LIMIT);
        let t1 = tokio::time::Instant::now();
        let quick = bounded(CAPTURE_LIMIT, async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Some(2)
        })
        .await;
        assert_eq!(quick, Some(2));
        assert_eq!(t1.elapsed(), Duration::from_millis(10));
    }

    #[test]
    fn a_frozen_target_is_skipped_until_a_frame_or_the_backoff() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut h = CaptureHealth::default();
        assert!(h.may_capture(T1, t0));
        h.timed_out(T1, t0);
        assert!(!h.may_capture(T1, t0 + s(1)));
        // Only that target.
        assert!(h.may_capture(T2, t0 + s(1)));
        h.frame_seen(T1);
        assert!(h.may_capture(T1, t0 + s(1)));
        h.timed_out(T1, t0);
        assert!(!h.may_capture(T1, t0 + FROZEN_BACKOFF - Duration::from_millis(1)));
        assert!(h.may_capture(T1, t0 + FROZEN_BACKOFF));
    }

    fn metrics(device_width: u64, css_width: u64) -> Value {
        json!({
            "contentSize": { "width": device_width, "height": 4000 },
            "cssContentSize": { "width": css_width, "height": 2000 },
            "cssVisualViewport": { "pageX": 0, "pageY": 300, "clientWidth": css_width, "clientHeight": 900 },
        })
    }

    #[test]
    fn a_capture_is_at_most_1280_wide() {
        let plain = json!({ "format": "jpeg", "quality": 60 });
        // A tab known to need no clip: as it is, JPEG 60 (F3, ratio 1).
        assert_eq!(capture_params(None), Some(plain.clone()));
        // Ratio 1 in a window up to 1280 wide: as it is.
        assert_eq!(device_pixel_ratio(&metrics(1200, 1200)), 1.0);
        assert_eq!(
            capture_params(Some(&metrics(1200, 1200))),
            Some(plain.clone())
        );
        // Ratio 2: the viewport, scaled so that it is 1280 wide.
        let m = metrics(2880, 1440);
        assert_eq!(device_pixel_ratio(&m), 2.0);
        let p = capture_params(Some(&m)).unwrap();
        assert_eq!(
            (p["format"].as_str(), p["quality"].as_u64()),
            (Some("jpeg"), Some(60))
        );
        let clip = &p["clip"];
        assert_eq!(clip["x"].as_f64(), Some(0.0));
        assert_eq!(clip["y"].as_f64(), Some(300.0));
        assert_eq!(clip["width"].as_f64(), Some(1440.0));
        assert_eq!(clip["height"].as_f64(), Some(900.0));
        let scale = clip["scale"].as_f64().unwrap();
        assert!((1440.0 * 2.0 * scale - 1280.0).abs() < 1e-6, "{scale}");
        // Ratio 1 in a window wider than 1280: scaled down as well (the stream's width).
        let wide = capture_params(Some(&metrics(1920, 1920))).unwrap();
        let scale = wide["clip"]["scale"].as_f64().unwrap();
        assert!((1920.0 * scale - 1280.0).abs() < 1e-6, "{scale}");
        // Ratio 2 in a small window: 1000 wide as it is, never enlarged.
        assert_eq!(capture_params(Some(&metrics(1000, 500))), Some(plain));
        // The viewport cannot be read: no capture (it might be too wide).
        assert_eq!(capture_params(Some(&json!({}))), None);
        // Unreadable sizes count as ratio 1.
        assert_eq!(device_pixel_ratio(&json!({})), 1.0);
    }

    /// Polls `f` until it gives a value (the writer thread works on its own).
    async fn eventually<T>(mut f: impl FnMut() -> Option<T>) -> T {
        for _ in 0..200 {
            if let Some(v) = f() {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("never came");
    }

    fn fcp(loader: &str) -> CdpEvent {
        event(
            "Page.lifecycleEvent",
            json!({ "frameId": T1, "loaderId": loader, "name": "firstContentfulPaint" }),
        )
    }

    #[tokio::test]
    async fn the_frame_before_a_command_is_the_last_one_stored() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, manifest) = rig(tmp.path(), "http://a.test/");
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        assert_eq!(r.before_command(&id, Some(T1)), None, "nothing stored yet");
        feed(&r, &id, shot_event(1), ms(0));
        let first = eventually(|| r.before_command(&id, Some(T1))).await;
        assert_eq!(first, format!("{T1}/000001.jpg"));
        assert_eq!(r.before_command(&id, None), None);
        assert_eq!(
            r.before_command(&id, Some(T2)),
            None,
            "not a tab of the recording"
        );
        let other = SessionId("s_otherone".into());
        assert_eq!(r.before_command(&other, Some(T1)), None);
        // On a screen-off page the last frame is not its screen.
        feed(
            &r,
            &id,
            navigated("http://screen-off.test/", "L1", "Navigation"),
            ms(600),
        );
        assert_eq!(r.before_command(&id, Some(T1)), None);
        // Back on a page that painted: the frame before the off page does not count.
        feed(
            &r,
            &id,
            navigated("http://b.test/", "L2", "Navigation"),
            ms(700),
        );
        feed(&r, &id, fcp("L2"), ms(750));
        eventually(|| {
            index(&manifest, T1)
                .iter()
                .any(|l| l.kind == FrameKind::Off)
                .then_some(())
        })
        .await;
        assert_eq!(r.before_command(&id, Some(T1)), None);
        feed(&r, &id, shot_event(2), ms(2000));
        let second = eventually(|| r.before_command(&id, Some(T1))).await;
        assert_eq!(second, format!("{T1}/000002.jpg"));
        // Not attached (the connection dropped): unknown what the tab shows.
        r.disconnected();
        assert_eq!(r.before_command(&id, Some(T1)), None);
    }

    #[tokio::test]
    async fn a_capture_is_kept_only_under_the_document_it_was_sent_on() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, manifest) = rig(tmp.path(), "http://a.test/");
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let Ticket::Ready(key) = r.ticket_now(&id, T1) else {
            panic!("an open tab can be captured");
        };
        let after = r.keep_capture(&key, frame(7)).unwrap();
        let name = Some(after.name.clone());
        assert_eq!(name.as_deref(), Some(format!("{T1}/000001.jpg").as_str()));
        // Known before it is written; written meanwhile.
        assert!(after.written.await.unwrap());
        let lines = index(&manifest, T1);
        assert_eq!(lines[0].kind, FrameKind::After);
        assert_eq!(lines[0].url.as_deref(), Some("http://a.test/"));
        // The capture is the last frame stored: the next command's frame before.
        assert_eq!(r.before_command(&id, Some(T1)), name);
        // Sent before a navigation, answered after it: dropped (F5).
        feed(
            &r,
            &id,
            navigated("http://b.test/", "L1", "Navigation"),
            ms(10),
        );
        assert!(r.keep_capture(&key, frame(8)).is_none());
        // On a screen-off page: no capture is sent at all, and an old one is dropped.
        let Ticket::Ready(on_b) = r.ticket_now(&id, T1) else {
            panic!("b can be captured");
        };
        feed(
            &r,
            &id,
            navigated("http://screen-off.test/", "L2", "Navigation"),
            ms(20),
        );
        assert!(matches!(r.ticket_now(&id, T1), Ticket::Never));
        assert!(r.keep_capture(&on_b, frame(9)).is_none());
        // Leaving it: nothing until the new document painted (the gate is held).
        feed(
            &r,
            &id,
            navigated("http://c.test/", "L3", "Navigation"),
            ms(30),
        );
        assert!(matches!(r.ticket_now(&id, T1), Ticket::Never));
        feed(&r, &id, fcp("L3"), ms(40));
        let Ticket::Ready(on_c) = r.ticket_now(&id, T1) else {
            panic!("c painted");
        };
        // Not a tab of this session, or another session's: never.
        assert!(matches!(r.ticket_now(&id, T2), Ticket::Never));
        let other = SessionId("s_otherone".into());
        assert!(matches!(r.ticket_now(&other, T1), Ticket::Never));
        // Paused (the test hook): neither sent nor kept.
        assert!(r.set_screen_for_test(&id, false).await);
        assert!(matches!(r.ticket_now(&id, T1), Ticket::Never));
        assert!(r.keep_capture(&on_c, frame(10)).is_none());
        let files: Vec<_> = index(&manifest, T1)
            .into_iter()
            .filter_map(|l| l.file)
            .collect();
        assert_eq!(files, ["000001.jpg"]);
    }

    #[tokio::test]
    async fn a_tab_being_attached_is_waited_for() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, _manifest) = rig(tmp.path(), "http://a.test/");
        r.state().tabs.remove(T1);
        r.state().attaching.insert(T1.to_string());
        assert!(matches!(r.ticket_now(&id, T1), Ticket::Wait));
        r.state().attaching.remove(T1);
        assert!(matches!(r.ticket_now(&id, T1), Ticket::Attach));
        // Registered, still finding out what it shows.
        assert!(r.register(T1, "C2", 0, Some("http://a.test/")));
        assert!(matches!(r.ticket_now(&id, T1), Ticket::Wait));
    }

    #[tokio::test]
    async fn without_a_connection_a_command_gets_no_after_frame_at_once() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, _manifest) = rig(tmp.path(), "http://a.test/");
        let t = Instant::now();
        assert!(r.after_command(&id, Some(T1)).await.is_none());
        assert!(r.after_command(&id, None).await.is_none());
        assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
    }

    #[tokio::test]
    async fn a_paused_screen_writes_nothing_until_it_is_on_again() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, manifest) = rig(tmp.path(), "http://a.test/");
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        feed(&r, &id, shot_event(1), ms(0));
        eventually(|| r.before_command(&id, Some(T1))).await;
        assert!(r.set_screen_for_test(&id, false).await);
        assert_eq!(r.before_command(&id, Some(T1)), None);
        feed(&r, &id, shot_event(2), ms(1000));
        feed(&r, &id, shot_event(3), ms(1100));
        r.on_tick(ms(5000));
        assert!(r.set_screen_for_test(&id, true).await);
        // The frame from before the pause is not the screen now.
        assert_eq!(r.before_command(&id, Some(T1)), None);
        feed(&r, &id, shot_event(4), ms(6000));
        let next = eventually(|| r.before_command(&id, Some(T1))).await;
        assert_eq!(next, format!("{T1}/000002.jpg"));
        r.end_session(&id).await;
        let lines = index(&manifest, T1);
        assert_eq!(lines.len(), 2, "{lines:?}");
        // No recording: nothing to switch.
        assert!(
            !r.set_screen_for_test(&SessionId("s_otherone".into()), false)
                .await
        );
    }

    #[tokio::test]
    async fn the_writer_names_a_capture_and_forgets_frames_behind_an_off_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let (_log, manifest) = open_log(tmp.path());
        let last = Arc::new(Mutex::new(HashMap::new()));
        let jobs = spawn_writer(
            manifest.clone(),
            ScreenBudget::new(1_000_000),
            Default::default(),
            last.clone(),
        );
        let (tx, rx) = oneshot::channel();
        jobs.send(Job::Capture {
            target: T1.into(),
            shot: shot(1),
            number: 1,
            reply: tx,
        })
        .unwrap();
        assert!(rx.await.unwrap());
        assert_eq!(
            last.lock().unwrap().get(T1).cloned(),
            Some(format!("{T1}/000001.jpg"))
        );
        jobs.send(Job::Mark {
            target: T1.into(),
            ts: now_ts(),
            kind: FrameKind::Off,
            url: None,
        })
        .unwrap();
        // A capture that cannot be written has no name.
        let (tx, rx) = oneshot::channel();
        jobs.send(Job::Capture {
            target: T1.into(),
            shot: Shot {
                data: "not base64!".into(),
                ..shot(2)
            },
            number: 2,
            reply: tx,
        })
        .unwrap();
        assert!(!rx.await.unwrap());
        end(&jobs).await;
        assert_eq!(last.lock().unwrap().get(T1), None);
        let kinds: Vec<_> = index(&manifest, T1).iter().map(|l| l.kind).collect();
        assert_eq!(kinds, [FrameKind::After, FrameKind::Off]);
        assert_eq!(manifest.get().screen, ScreenStatus::Recorded);
    }

    #[tokio::test]
    async fn a_capture_is_named_in_the_order_of_the_frames_around_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, manifest) = rig(tmp.path(), "http://a.test/");
        let t0 = Instant::now();
        feed(&r, &id, shot_event(1), t0);
        let Ticket::Ready(key) = r.ticket_now(&id, T1) else {
            panic!("an open tab can be captured");
        };
        // Named at once, before the writer got to it.
        let after = r.keep_capture(&key, frame(2)).unwrap();
        assert_eq!(after.name, format!("{T1}/000002.jpg"));
        feed(&r, &id, shot_event(3), t0 + Duration::from_secs(2));
        r.end_session(&id).await;
        let lines: Vec<_> = index(&manifest, T1)
            .into_iter()
            .map(|l| (l.file.unwrap(), l.kind))
            .collect();
        assert_eq!(
            lines,
            [
                ("000001.jpg".to_string(), FrameKind::Change),
                ("000002.jpg".to_string(), FrameKind::After),
                ("000003.jpg".to_string(), FrameKind::Change),
            ]
        );
        assert_eq!(after.unless_failed(), Some(format!("{T1}/000002.jpg")));
    }

    #[tokio::test]
    async fn a_frame_after_whose_write_failed_names_nothing() {
        let named = |written: Option<bool>| {
            let (tx, rx) = oneshot::channel();
            if let Some(w) = written {
                tx.send(w).unwrap();
            } else {
                // Still being written: the sender lives on.
                std::mem::forget(tx);
            }
            AfterFrame {
                name: format!("{T1}/000001.jpg"),
                written: rx,
            }
            .unless_failed()
        };
        assert_eq!(
            named(Some(true)).as_deref(),
            Some(format!("{T1}/000001.jpg").as_str())
        );
        assert_eq!(
            named(None).as_deref(),
            Some(format!("{T1}/000001.jpg").as_str())
        );
        assert_eq!(named(Some(false)), None);
        // The writer is gone without an answer.
        let (tx, rx) = oneshot::channel::<bool>();
        drop(tx);
        let gone = AfterFrame {
            name: "x".into(),
            written: rx,
        };
        assert_eq!(gone.unless_failed(), None);
    }

    #[tokio::test]
    async fn a_capture_is_judged_after_the_events_sent_before_its_reply() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, manifest) = rig(tmp.path(), "http://a.test/");
        let t0 = Instant::now();
        let Ticket::Ready(key) = r.ticket_now(&id, T1) else {
            panic!("an open tab can be captured");
        };
        // The reply came after one event that is still in the queue: the capture waits.
        let deadline = Instant::now() + Duration::from_secs(5);
        let waiting = r.keep_capture_in_order(&key, frame(1), 1, deadline);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), waiting.as_mut())
                .await
                .is_err(),
            "judged before the event before it"
        );
        // That event: the tab went to a screen-off page before the browser answered.
        feed(
            &r,
            &id,
            navigated("http://screen-off.test/", "L1", "Navigation"),
            t0,
        );
        r.event_handled();
        assert!(waiting.await.is_none(), "an off page's capture was kept");
        // Not reached in time: dropped.
        feed(&r, &id, navigated("http://b.test/", "L2", "Navigation"), t0);
        feed(&r, &id, fcp("L2"), t0);
        r.event_handled();
        let Ticket::Ready(on_b) = r.ticket_now(&id, T1) else {
            panic!("b can be captured");
        };
        let soon = Instant::now() + Duration::from_millis(30);
        assert!(r
            .keep_capture_in_order(&on_b, frame(2), 3, soon)
            .await
            .is_none());
        // Every event before the reply handled, nothing changed: kept.
        let kept = r
            .keep_capture_in_order(&on_b, frame(3), 2, deadline)
            .await
            .unwrap();
        assert!(kept.written.await.unwrap());
        // The connection changed meanwhile: dropped.
        r.state().epoch += 1;
        assert!(r
            .keep_capture_in_order(&on_b, frame(4), 0, deadline)
            .await
            .is_none());
        let files: Vec<_> = index(&manifest, T1)
            .into_iter()
            .filter_map(|l| l.file)
            .collect();
        assert_eq!(files.len(), 1, "{files:?}");
    }

    #[tokio::test]
    async fn a_stream_frame_ends_a_frozen_screen() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, _manifest) = rig(tmp.path(), "http://a.test/");
        let t0 = Instant::now();
        r.state().health.timed_out(T1, t0);
        assert!(!r.state().health.may_capture(T1, t0));
        feed(&r, &id, shot_event(1), t0);
        assert!(r.state().health.may_capture(T1, t0));
    }

    #[tokio::test]
    async fn a_tab_of_an_earlier_connection_has_no_frame_before() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, _manifest) = rig(tmp.path(), "http://a.test/");
        feed(&r, &id, shot_event(1), Instant::now());
        eventually(|| r.before_command(&id, Some(T1))).await;
        // A new connection whose attach has not come yet: the old attach says nothing.
        r.state().epoch += 1;
        assert_eq!(r.before_command(&id, Some(T1)), None);
    }

    /// A browser stand-in on a local websocket. `answer`: replies to the capture's requests
    /// after sending `events_first` events (never handled: the rig runs no event loop);
    /// otherwise it never replies.
    async fn fake_browser(answer: bool, events_first: usize) -> String {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            while let Some(Ok(Message::Text(t))) = ws.next().await {
                if !answer {
                    continue;
                }
                let v: Value = serde_json::from_str(&t).unwrap();
                for _ in 0..events_first {
                    let e = json!({ "method": "Page.x", "params": {} });
                    ws.send(Message::text(e.to_string())).await.unwrap();
                }
                let result = match v["method"].as_str() {
                    Some("Page.getLayoutMetrics") => metrics(1200, 1200),
                    _ => json!({ "data": frame(1) }),
                };
                let reply = json!({ "id": v["id"], "result": result });
                ws.send(Message::text(reply.to_string())).await.unwrap();
            }
        });
        format!("ws://{addr}")
    }

    async fn rig_on(dir: &std::path::Path, url: &str) -> (Arc<Recorder>, SessionId) {
        let (r, id, _manifest) = rig(dir, "http://a.test/");
        let (conn, _queue) = CdpConn::connect_queued(url).await.unwrap();
        r.state().conn = Some(conn);
        (r, id)
    }

    /// The bound of design 4.6 holds whatever the browser does: no answer at all, or an
    /// answer whose earlier events are never handled.
    #[tokio::test]
    async fn a_command_never_waits_longer_than_the_capture_limit() {
        let cap = CAPTURE_LIMIT + Duration::from_millis(100);
        let tmp = tempfile::tempdir().unwrap();
        let (r, id) = rig_on(tmp.path(), &fake_browser(false, 0).await).await;
        let t = Instant::now();
        assert!(r.after_command(&id, Some(T1)).await.is_none());
        let first = t.elapsed();
        assert!(
            first >= CAPTURE_LIMIT - Duration::from_millis(20) && first < cap,
            "{first:?}"
        );
        // Timed out: frozen, so the next command does not wait at all.
        let t = Instant::now();
        assert!(r.after_command(&id, Some(T1)).await.is_none());
        assert!(t.elapsed() < Duration::from_millis(20), "{:?}", t.elapsed());

        let tmp = tempfile::tempdir().unwrap();
        let (r, id) = rig_on(tmp.path(), &fake_browser(true, 3).await).await;
        let t = Instant::now();
        assert!(r.after_command(&id, Some(T1)).await.is_none());
        let waited = t.elapsed();
        assert!(waited < cap, "{waited:?}");
    }

    /// With every event before the answer handled, the same browser gives a frame.
    #[tokio::test]
    async fn an_answered_capture_is_named() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id) = rig_on(tmp.path(), &fake_browser(true, 0).await).await;
        let after = r.after_command(&id, Some(T1)).await.unwrap();
        assert_eq!(after.name, format!("{T1}/000001.jpg"));
    }

    // ---- navigation, people, extras (task 5) ----

    fn lines<T: serde::de::DeserializeOwned>(manifest: &ManifestHandle, file: &str) -> Vec<T> {
        read_jsonl(&manifest.dir().join(file)).unwrap()
    }

    fn humans(manifest: &ManifestHandle) -> Vec<ActionEntry> {
        lines::<ActionEntry>(manifest, "actions.jsonl")
            .into_iter()
            .filter(|e| e.kind == crate::actionlog::EntryKind::Human)
            .collect()
    }

    fn requested(url: &str, reason: &str) -> CdpEvent {
        event(
            "Page.frameRequestedNavigation",
            json!({ "frameId": T1, "url": url, "reason": reason, "disposition": "currentTab" }),
        )
    }

    fn started(url: &str) -> CdpEvent {
        event(
            "Page.frameStartedNavigating",
            json!({ "frameId": T1, "url": url, "loaderId": "L", "navigationType": "differentDocument" }),
        )
    }

    /// Lets the tasks that append to the action log run.
    async fn settle() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn navigations_outside_a_command_are_possible_human_interventions() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, manifest) = rig(tmp.path(), "http://a.test/");
        // In a command, and within its grace.
        let window = r.command_window(&id);
        feed(
            &r,
            &id,
            navigated("http://a.test/1", "L1", "Navigation"),
            Instant::now(),
        );
        drop(window);
        let ended = Instant::now();
        feed(
            &r,
            &id,
            navigated("http://a.test/2", "L2", "Navigation"),
            ended + Duration::from_millis(900),
        );
        // Outside: a person typing an address (or a script's history.back()).
        let later = ended + Duration::from_secs(2);
        feed(
            &r,
            &id,
            navigated("http://u:p@b.test/x?token=SECRET", "L3", "Navigation"),
            later,
        );
        // A link click outside (or a script's a.click()).
        feed(&r, &id, requested("http://b.test/y", "anchorClick"), later);
        feed(&r, &id, started("http://b.test/y"), later);
        feed(
            &r,
            &id,
            navigated("http://b.test/y", "L4", "Navigation"),
            later,
        );
        // The page's own script: never a person.
        feed(
            &r,
            &id,
            requested("http://b.test/z", "scriptInitiated"),
            later,
        );
        feed(&r, &id, started("http://b.test/z"), later);
        feed(
            &r,
            &id,
            navigated("http://b.test/z", "L5", "Navigation"),
            later,
        );
        // A subframe is not the tab's page.
        feed(
            &r,
            &id,
            event(
                "Page.frameNavigated",
                json!({ "frame": { "id": "F2", "parentId": T1, "url": "http://c.test/" } }),
            ),
            later,
        );
        settle().await;
        r.end_session(&id).await;

        let marks = humans(&manifest);
        let got: Vec<(String, Option<bool>, Option<String>)> = marks
            .iter()
            .map(|e| (e.what.clone().unwrap(), e.certain, e.url.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "navigated outside a command: http://b.test/x".to_string(),
                    Some(false),
                    Some("http://b.test/x".to_string())
                ),
                (
                    "followed a link or form outside a command".to_string(),
                    Some(false),
                    Some("http://b.test/y".to_string())
                ),
            ]
        );
        assert!(marks
            .iter()
            .all(|e| e.target_id.as_deref() == Some(T1) && e.command == "human"));
        let nav: Vec<NavLine> = lines(&manifest, "navigation.jsonl");
        let urls: Vec<(NavEvent, Option<String>)> =
            nav.iter().map(|l| (l.event, l.url.clone())).collect();
        assert_eq!(
            urls,
            [
                // The page the attach found the tab on (the rig's attach).
                "http://a.test/",
                "http://a.test/1",
                "http://a.test/2",
                "http://b.test/x",
                "http://b.test/y",
                "http://b.test/z"
            ]
            .iter()
            .map(|u| (NavEvent::Navigated, Some(u.to_string())))
            .collect::<Vec<_>>()
        );
        let text = std::fs::read_to_string(manifest.dir().join("navigation.jsonl")).unwrap();
        assert!(!text.contains("SECRET") && !text.contains("u:p@"), "{text}");
        assert_eq!(mode(&manifest.dir().join("navigation.jsonl")), 0o600);
    }

    fn mode(p: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[tokio::test]
    async fn tabs_joining_and_leaving_are_in_the_navigation_log() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, manifest) = rig(tmp.path(), "http://a.test/");
        // A popup joins the running recording; its first page is its opening, not a mark.
        r.add_target(&id, T2, "AGENT");
        r.state()
            .sessions
            .get_mut(&id)
            .unwrap()
            .created(T2, Some("about:blank"), Some(T1));
        let later = Instant::now() + Duration::from_secs(5);
        {
            let mut st = r.state();
            st.tabs.insert(
                T2.into(),
                Tab {
                    session: id.clone(),
                    cdp: "C2".into(),
                    epoch: 0,
                    streaming: false,
                    clip: None,
                },
            );
            st.by_cdp.insert("C2".into(), T2.into());
        }
        let popup = |url: &str| CdpEvent {
            method: "Page.frameNavigated".into(),
            params: json!({ "frame": { "id": T2, "url": url, "loaderId": "P1" }, "type": "Navigation" }),
            session_id: Some("C2".into()),
        };
        // A switch to it before its first page: its opening too.
        r.activated("AGENT", T2, SystemTime::now());
        {
            let mut st = r.state();
            r.handle(&mut st, T2, &id, &popup("http://a.test/popup?s=1"), later);
        }
        r.remove_target(&id, T2);
        settle().await;
        r.end_session(&id).await;
        let nav: Vec<NavLine> = lines(&manifest, "navigation.jsonl");
        let got: Vec<(String, NavEvent, Option<String>, Option<String>)> = nav
            .iter()
            .filter(|l| l.target_id == T2)
            .map(|l| {
                (
                    l.target_id.clone(),
                    l.event,
                    l.url.clone(),
                    l.opener.clone(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    T2.into(),
                    NavEvent::Created,
                    Some("about:blank".into()),
                    Some(T1.into())
                ),
                (
                    T2.into(),
                    NavEvent::Navigated,
                    Some("http://a.test/popup".into()),
                    None
                ),
                (T2.into(), NavEvent::Closed, None, None),
            ]
        );
        assert!(humans(&manifest).is_empty(), "{:?}", humans(&manifest));
    }

    #[tokio::test]
    async fn a_tab_switch_outside_a_command_is_marked_unless_pegboard_made_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, manifest) = rig(tmp.path(), "http://a.test/");
        // In a command, and a late report of a switch made during one (a replay, 2 s on).
        let window = r.command_window(&id);
        let during = SystemTime::now();
        r.activated("AGENT", T1, during);
        drop(window);
        let later = (
            SystemTime::now() + Duration::from_secs(2),
            Instant::now() + Duration::from_secs(2),
        );
        r.activated_as_of("AGENT", T1, during, later);
        // Announced by pegboard.
        r.expect_activation(T1);
        r.activated_as_of("AGENT", T1, later.0, later);
        // Another profile's bridge, a tab of no session.
        r.activated_as_of("OTHER", T1, later.0, later);
        r.activated_as_of("AGENT", T2, later.0, later);
        // A person.
        r.activated_as_of("AGENT", T1, later.0, later);
        settle().await;
        r.end_session(&id).await;
        let marks = humans(&manifest);
        assert_eq!(marks.len(), 1, "{marks:?}");
        assert_eq!(marks[0].what.as_deref(), Some("switched to this tab"));
        assert_eq!(marks[0].certain, Some(false));
        assert_eq!(marks[0].target_id.as_deref(), Some(T1));
        assert_eq!(marks[0].url.as_deref(), Some("http://a.test/"));
    }

    #[tokio::test]
    async fn nothing_is_marked_after_the_log_ended() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, manifest) = rig(tmp.path(), "http://a.test/");
        let log = r.state().sessions[&id].rec.as_ref().unwrap().log.clone();
        log.lock().await.finish("2026-01-01T00:00:01Z").unwrap();
        r.activated("AGENT", T1, SystemTime::now() + Duration::from_secs(2));
        settle().await;
        assert!(humans(&manifest).is_empty());
    }

    #[tokio::test]
    async fn network_and_console_lines_only_when_asked() {
        let network = |id: &str, m: &str, p: Value| {
            let mut p = p;
            p["requestId"] = json!(id);
            event(m, p)
        };
        let feed_all = |r: &Arc<Recorder>, id: &SessionId| {
            let now = Instant::now();
            feed(
                r,
                id,
                network(
                    "1",
                    "Network.requestWillBeSent",
                    json!({ "timestamp": 1.0,
                "request": { "method": "POST", "url": "http://a.test/api?token=SECRET1",
                             "headers": { "Authorization": "Bearer SECRET2" }, "postData": "SECRET4" } }),
                ),
                now,
            );
            feed(
                r,
                id,
                network(
                    "1",
                    "Network.responseReceived",
                    json!({ "timestamp": 1.1,
                "response": { "status": 201, "headers": { "set-cookie": "SECRET3" } } }),
                ),
                now,
            );
            feed(
                r,
                id,
                network(
                    "1",
                    "Network.loadingFinished",
                    json!({ "timestamp": 1.2, "encodedDataLength": 10 }),
                ),
                now,
            );
            feed(
                r,
                id,
                event(
                    "Runtime.consoleAPICalled",
                    json!({ "type": "error", "args": [{ "type": "string", "value": "hello" }] }),
                ),
                now,
            );
        };
        let tmp = tempfile::tempdir().unwrap();
        let (r, id, manifest) = rig(tmp.path(), "http://a.test/");
        feed_all(&r, &id);
        r.end_session(&id).await;
        assert!(!manifest.dir().join("network.jsonl").exists());
        assert!(!manifest.dir().join("console.jsonl").exists());

        let tmp = tempfile::tempdir().unwrap();
        let (r, id, manifest) = rig_with(
            tmp.path(),
            "http://a.test/",
            vec![RecordExtra::Network, RecordExtra::Console],
        );
        feed_all(&r, &id);
        r.end_session(&id).await;
        let net: Vec<NetworkLine> = lines(&manifest, "network.jsonl");
        assert_eq!(net.len(), 1, "{net:?}");
        assert_eq!(
            (net[0].method.as_str(), net[0].url.as_str(), net[0].status),
            ("POST", "http://a.test/api", Some(201))
        );
        let text = std::fs::read_to_string(manifest.dir().join("network.jsonl")).unwrap();
        for secret in ["SECRET", "Authorization", "ookie"] {
            assert!(!text.contains(secret), "{text}");
        }
        let console: Vec<ConsoleLine> = lines(&manifest, "console.jsonl");
        assert_eq!(console.len(), 1);
        assert_eq!(
            (console[0].level.as_str(), console[0].text.as_str()),
            ("error", "hello")
        );
        assert_eq!(mode(&manifest.dir().join("console.jsonl")), 0o600);
    }
}
