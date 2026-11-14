//! Downloads of the agent profiles: they land in the session's files directory.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use pegboard_protocol::{FileInfo, SessionId};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::actionlog::{url_for_log, ActionEntry};
use crate::cdp::{page_targets, CdpConn, CdpEvent};
use crate::paths::{ensure_owned_private_dir, ensure_session_files, Paths};

/// Largest file one download may be.
pub const MAX_FILE_BYTES: u64 = 200 << 20;
/// Most bytes one session may download in all.
pub const MAX_SESSION_BYTES: u64 = 1 << 30;
/// Longest file name (bytes) a download is saved under, before a `-N` that keeps it unique.
const MAX_NAME_BYTES: usize = 200;
const FALLBACK_NAME: &str = "download";

/// The name a download is saved under, from the name the browser suggests: the last path
/// component, without control characters, at most `MAX_NAME_BYTES` bytes (keeping a short
/// extension); `download` when nothing usable is left.
pub fn safe_name(suggested: &str) -> String {
    let last = suggested.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = last.chars().filter(|c| !c.is_control()).collect();
    if clean.is_empty() || clean == "." || clean == ".." {
        return FALLBACK_NAME.to_string();
    }
    if clean.len() <= MAX_NAME_BYTES {
        return clean;
    }
    match split_extension(&clean) {
        (stem, Some(ext)) if ext.len() <= MAX_EXTENSION_BYTES => {
            format!("{}.{ext}", cut(stem, MAX_NAME_BYTES - ext.len() - 1))
        }
        _ => cut(&clean, MAX_NAME_BYTES).to_string(),
    }
}

/// Longest extension `safe_name` keeps when it shortens a name.
const MAX_EXTENSION_BYTES: usize = 16;

/// The longest prefix of `s` of at most `max` bytes that ends on a character boundary.
fn cut(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// `("archive.tar", Some("gz"))` for `archive.tar.gz`; a leading dot does not start an
/// extension (`.bashrc` has none).
fn split_extension(name: &str) -> (&str, Option<&str>) {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => (stem, Some(ext)),
        _ => (name, None),
    }
}

/// `name` for `n == 0`, else `name` with `-n` before its extension.
pub fn numbered(name: &str, n: u32) -> String {
    if n == 0 {
        return name.to_string();
    }
    match split_extension(name) {
        (stem, Some(ext)) => format!("{stem}-{n}.{ext}"),
        (stem, None) => format!("{stem}-{n}"),
    }
}

/// Most numbered names `move_into` tries before it gives up.
const MAX_NUMBERED: u32 = 10_000;

/// Moves the staged file `src` into `dir` as `name`, or `name` numbered when that exists
/// (never replacing anything there, a symlink included), with mode 0600. `src` must be a
/// plain file with one link. Returns the name used and the size.
///
/// The file is linked under its new name, which fails when the name exists, and then
/// unlinked from the staging directory: both lie in pegboard's home, on one filesystem.
pub fn move_into(src: &Path, dir: &Path, name: &str) -> std::io::Result<(String, u64)> {
    use std::io::{Error, ErrorKind};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let meta = std::fs::symlink_metadata(src)?;
    if !meta.file_type().is_file() || meta.nlink() != 1 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "the staged download is not a plain file",
        ));
    }
    for n in 0..MAX_NUMBERED {
        let chosen = numbered(name, n);
        let dst = dir.join(&chosen);
        match std::fs::hard_link(src, &dst) {
            Ok(()) => {
                std::fs::remove_file(src)?;
                std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o600))?;
                return Ok((chosen, meta.len()));
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(Error::new(
        ErrorKind::AlreadyExists,
        "too many files of that name",
    ))
}

/// The download limits: one file, and one session's downloads in all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub file: u64,
    pub session: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            file: MAX_FILE_BYTES,
            session: MAX_SESSION_BYTES,
        }
    }
}

impl Limits {
    /// `PEGBOARD_TEST_DOWNLOAD_LIMITS` (tests only, read when the browser starts).
    pub fn from_env() -> Limits {
        Limits::parse(
            std::env::var("PEGBOARD_TEST_DOWNLOAD_LIMITS")
                .ok()
                .as_deref(),
        )
    }

    /// `"<file bytes>,<session bytes>"` lowers the limits (never raises them); anything
    /// else, or nothing, leaves them as they are.
    pub fn parse(spec: Option<&str>) -> Limits {
        let default = Limits::default();
        let parsed = spec.and_then(|spec| {
            let (file, session) = spec.split_once(',')?;
            let file: u64 = file.trim().parse().ok().filter(|n| *n > 0)?;
            let session: u64 = session.trim().parse().ok().filter(|n| *n > 0)?;
            Some(Limits {
                file: file.min(default.file),
                session: session.min(default.session),
            })
        });
        parsed.unwrap_or(default)
    }

    /// Why a download of `bytes` is dropped when its session has `used` bytes already.
    pub fn judge(&self, bytes: u64, used: u64) -> Option<String> {
        if bytes > self.file {
            return Some(format!("file larger than {}", size_text(self.file)));
        }
        if used.saturating_add(bytes) > self.session {
            return Some(format!(
                "session download limit of {} reached",
                size_text(self.session)
            ));
        }
        None
    }
}

/// `200 MiB`, `1 GiB`, `1 KiB`, or a count of bytes.
fn size_text(bytes: u64) -> String {
    for (unit, size) in [("GiB", 1u64 << 30), ("MiB", 1 << 20), ("KiB", 1 << 10)] {
        if bytes >= size && bytes.is_multiple_of(size) {
            return format!("{} {unit}", bytes / size);
        }
    }
    format!("{bytes} bytes")
}

/// Which session a tab belongs to, as far as downloads are concerned. A tab stays known
/// after it is gone (a tab whose first navigation is a download closes, spike F4) until its
/// session is forgotten.
#[derive(Debug, Default)]
pub struct Owners {
    by_target: HashMap<String, SessionId>,
    /// Target -> the tab that opened it (`Target.targetCreated`), for a popup that is gone
    /// before it joined its opener's session (spike L2).
    opener: HashMap<String, String>,
}

/// Openers kept at most; past this the table starts again (an old popup is then unknown and
/// its download deleted: fail closed).
const MAX_OPENERS: usize = 4096;
/// Longest chain of openers followed.
const MAX_OPENER_HOPS: usize = 16;

impl Owners {
    pub fn add(&mut self, target: &str, session: &SessionId) {
        self.by_target.insert(target.to_string(), session.clone());
    }

    /// A tab that turned out not to be the session's after all.
    pub fn remove(&mut self, target: &str) {
        self.by_target.remove(target);
    }

    pub fn forget_session(&mut self, session: &SessionId) {
        self.by_target.retain(|_, s| s != session);
    }

    /// `target` was opened by `opener` (a popup).
    pub fn opened(&mut self, target: &str, opener: &str) {
        if self.opener.len() >= MAX_OPENERS {
            self.opener.clear();
        }
        self.opener.insert(target.to_string(), opener.to_string());
    }

    /// The session of the tab, or of the tab that opened it (and so on).
    pub fn session_of(&self, target: &str) -> Option<SessionId> {
        let mut at = target;
        for _ in 0..MAX_OPENER_HOPS {
            if let Some(s) = self.by_target.get(at) {
                return Some(s.clone());
            }
            at = self.opener.get(at)?;
        }
        None
    }
}

/// The plain files of a session's files directory, by name (none when it does not exist).
pub fn list_files(dir: &Path) -> Vec<FileInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<FileInfo> = entries
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            if !meta.file_type().is_file() {
                return None;
            }
            let modified = meta.modified().ok().map(time::OffsetDateTime::from)?;
            Some(FileInfo {
                name: entry.file_name().into_string().ok()?,
                bytes: meta.len(),
                modified_at: modified
                    .format(&time::format_description::well_known::Rfc3339)
                    .ok()?,
            })
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    files
}

// ---- the browser side: one policy connection per agent profile ----

/// How long a profile may be without its download policy before its sessions are closed.
const REAPPLY_LIMIT: Duration = Duration::from_secs(5);
const RETRY_PAUSE: Duration = Duration::from_millis(250);
/// Pause between attempts once the profile's sessions were closed for it.
const SLOW_RETRY_PAUSE: Duration = Duration::from_secs(1);
/// Attempts at `Browser.setDownloadBehavior` when a profile's window was just opened (it
/// takes a moment to become the default context), and when the policy is set again.
const FIRST_TRIES: u32 = 20;
const AGAIN_TRIES: u32 = 4;
const SWEEP_EVERY: Duration = Duration::from_secs(10);
/// A staged file no running download claims is deleted once it is this old.
const ORPHAN_AGE: Duration = Duration::from_secs(30);
/// A download that never ends is given up (its staged file deleted) after this long.
const STALE: Duration = Duration::from_secs(6 * 3600);
/// A download's tab may join its session just after the download begins (a new tab whose
/// first navigation is the download): finding it is tried this often, this far apart.
const RESOLVE_TRIES: u32 = 4;
const RESOLVE_PAUSE: Duration = Duration::from_millis(500);
/// Longest time spent looking through tabs' frame trees for one download.
const FRAME_SEARCH_LIMIT: Duration = Duration::from_secs(5);
/// Most frames walked up from a download's frame to its tab.
const MAX_FRAME_HOPS: usize = 8;

/// Why a download was deleted, as the agent and the action log are told.
pub const NOT_A_SESSION_TAB: &str = "not started by a tab of a session";
pub const DENIED: &str = "the session's policy denies downloads (--deny download)";
pub const POLICY_LOST: &str = "pegboard lost its download policy while the download ran";
pub const CANCELED: &str = "canceled";
pub const NOT_SAVED: &str = "the file could not be saved";

/// What became of a download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Saved { bytes: u64 },
    Dropped(String),
}

/// A finished download, for its session's next command result and its action log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The name in the session's files directory (or the name it would have had).
    pub name: String,
    pub url: String,
    pub outcome: Outcome,
    /// RFC 3339, UTC.
    pub ended_at: String,
}

impl Report {
    /// `{ "name", "bytes" }` or `{ "name", "dropped": <why> }`.
    pub fn to_json(&self) -> Value {
        match &self.outcome {
            Outcome::Saved { bytes } => json!({ "name": self.name, "bytes": bytes }),
            Outcome::Dropped(why) => json!({ "name": self.name, "dropped": why }),
        }
    }

    /// One action log entry: `download <name> <bytes>`, or `download <name>` failed with
    /// the reason. The URL is cleaned like every logged URL.
    pub fn log_entry(&self, session: &SessionId) -> ActionEntry {
        let (args, ok, error) = match &self.outcome {
            Outcome::Saved { bytes } => (vec![self.name.clone(), bytes.to_string()], true, None),
            Outcome::Dropped(why) => (
                vec![self.name.clone()],
                false,
                Some(format!("dropped: {why}")),
            ),
        };
        ActionEntry {
            seq: 0,
            ts: self.ended_at.clone(),
            session_id: session.0.clone(),
            target_id: None,
            url: Some(url_for_log(&self.url)),
            command: "download".to_string(),
            args,
            ok,
            error,
            duration_ms: 0,
            redacted: false,
        }
    }
}

/// What downloads need to know of a session.
#[derive(Debug, Clone)]
pub struct SessionView {
    pub allows_downloads: bool,
    /// The browser context of the session's profile.
    pub context: String,
}

/// A session's view, `None` once it is closed.
pub type SessionLookup = Arc<dyn Fn(&SessionId) -> Option<SessionView> + Send + Sync>;
/// Called with a browser context whose download policy could not be set again in time.
pub type PolicyLost = Arc<dyn Fn(&str) + Send + Sync>;

/// A profile's download policy: the connection that holds it (Chrome lifts the policy when
/// it closes) and the staging directory downloads land in.
struct Policy {
    profile_dir: String,
    staging: PathBuf,
    conn: Option<CdpConn>,
    ready: bool,
    /// Bumped with every connection, so the end of an old one is told from the current one's.
    generation: u64,
    /// A task is setting the policy again.
    reapplying: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Completed,
    Canceled,
}

struct Download {
    name: String,
    url: String,
    started: Instant,
    /// Whose it is: decided once (`resolved`).
    resolved: bool,
    session: Option<SessionId>,
    /// The browser context to cancel it in (its session's profile).
    context: Option<String>,
    dropped: Option<String>,
    received: u64,
    total: u64,
    end: Option<End>,
}

#[derive(Default)]
struct State {
    /// Browser context -> its policy.
    policies: HashMap<String, Policy>,
    /// Download guid -> the download, until it is settled.
    downloads: HashMap<String, Download>,
    owners: Owners,
    /// Bytes saved into each open session's files directory.
    used: HashMap<SessionId, u64>,
    /// Sessions that ended (ids are never reused): nothing is saved or reported for them.
    gone: HashSet<SessionId>,
    reports: HashMap<SessionId, Vec<Report>>,
}

struct Inner {
    ws_url: String,
    human_context: String,
    /// The daemon's browser connection, for looking at targets and cancelling downloads.
    conn: CdpConn,
    paths: Paths,
    /// `<home>/downloads`: one staging directory per agent profile under it.
    root: PathBuf,
    limits: Limits,
    state: Mutex<State>,
    lookup: Mutex<Option<SessionLookup>>,
    lost: Mutex<Option<PolicyLost>>,
    /// Setting a policy is done by one caller at a time.
    applying: tokio::sync::Mutex<()>,
    /// Test hook (`drop_connection_for_test`): no new connection before this instant.
    hold_until: Mutex<Option<tokio::time::Instant>>,
}

/// The agent profiles' download policies and their downloads.
///
/// How (measured in the phase 2a spikes, sections L, E, F):
/// - Each agent profile gets `Browser.setDownloadBehavior { allowAndName, eventsEnabled }`
///   into a staging directory only `pegboardd` knows (`<home>/downloads/<profile dir>`),
///   on a connection of its own that sends nothing else and is kept open: Chrome lifts the
///   policy when that connection closes (L1b, L5, E3), and one connection holds the policy
///   of one context only (phase 1). Chrome saves each download there under its guid (L4).
/// - Chrome takes a `browserContextId` only for the profile that is the default context at
///   that moment (E); opening or activating a tab of the profile makes it the default (E3).
///   A policy set then stays when the human's profile becomes the default (E2, E3). The
///   human's context is never given a policy, and no call goes without a context id.
/// - Every policy connection gets the download events of every profile (L1): a download is
///   handled once, by its guid, and one no session's tab started is deleted. The human's
///   downloads never land in a staging directory, so deleting touches nothing of theirs.
/// - A download's session is found from its frame (T3 of the task): the tab whose target id
///   is the frame id, the tab that opened it, the frame a cross-site frame's own target sits
///   in (F1b), or a frame in a tab's frame tree (F1).
/// - When it completes it is moved into the session's files directory (F2), unless the
///   session denies downloads, is gone, or it is over a limit (checked on every progress
///   event, about 500 ms apart, and cancelled then: L3).
/// - When a policy connection ends, the profile is marked not ready (the daemon refuses its
///   sessions' commands), its running downloads are cancelled and the policy set again on a
///   new connection; after `REAPPLY_LIMIT` the `PolicyLost` callback closes its sessions.
pub struct Downloads {
    inner: Arc<Inner>,
    sweeper: JoinHandle<()>,
}

impl Drop for Downloads {
    fn drop(&mut self) {
        self.sweeper.abort();
        let conns: Vec<CdpConn> = self
            .inner
            .state
            .lock()
            .unwrap()
            .policies
            .values_mut()
            .filter_map(|p| p.conn.take())
            .collect();
        for conn in conns {
            conn.close();
        }
    }
}

impl Downloads {
    /// Empties the staging root (`<home>/downloads`) left by an earlier run and starts the
    /// sweeper. Must be called inside a tokio runtime.
    pub fn new(
        ws_url: &str,
        human_context: &str,
        conn: CdpConn,
        paths: &Paths,
    ) -> Result<Downloads> {
        let root = paths.home.join("downloads");
        // Never followed: a link in its place is removed, not what it points at.
        match std::fs::symlink_metadata(&root) {
            Ok(m) if m.is_dir() => std::fs::remove_dir_all(&root)?,
            Ok(_) => std::fs::remove_file(&root)?,
            Err(_) => {}
        }
        ensure_owned_private_dir(&root)?;
        let inner = Arc::new(Inner {
            ws_url: ws_url.to_string(),
            human_context: human_context.to_string(),
            conn,
            paths: paths.clone(),
            root,
            limits: Limits::from_env(),
            state: Mutex::new(State::default()),
            lookup: Mutex::new(None),
            lost: Mutex::new(None),
            applying: tokio::sync::Mutex::new(()),
            hold_until: Mutex::new(None),
        });
        let sweeper = tokio::spawn(sweep(Arc::downgrade(&inner)));
        Ok(Downloads { inner, sweeper })
    }

    /// Puts the profile `profile_dir` (browser context `context`) under pegboard's download
    /// policy, unless it is already. Call it right after opening the profile's window. An
    /// error leaves the profile unprotected: the caller must not use it.
    pub async fn apply(&self, context: &str, profile_dir: &str) -> Result<()> {
        if context.is_empty() || context == self.inner.human_context {
            bail!("refusing to set a download policy on the human or an unknown context");
        }
        if !valid_dir_name(profile_dir) {
            bail!("not a profile directory name");
        }
        let _one = self.inner.applying.lock().await;
        if self.inner.is_ready(context) {
            return Ok(());
        }
        self.inner
            .establish(context, profile_dir, FIRST_TRIES)
            .await
            .context("could not set pegboard's download policy in the agent profile")
    }

    /// The context is gone (Chrome unloaded the profile): its policy went with it.
    pub fn forget_context(&self, context: &str) {
        let removed = self.inner.state.lock().unwrap().policies.remove(context);
        if let Some(conn) = removed.and_then(|p| p.conn) {
            conn.close();
        }
    }

    /// Whether the context's download policy is in place.
    pub fn is_ready(&self, context: &str) -> bool {
        self.inner.is_ready(context)
    }

    pub fn set_session_lookup(&self, lookup: SessionLookup) {
        *self.inner.lookup.lock().unwrap() = Some(lookup);
    }

    pub fn on_policy_lost(&self, lost: PolicyLost) {
        *self.inner.lost.lock().unwrap() = Some(lost);
    }

    /// The tab `target` belongs to `session`.
    pub fn own(&self, target: &str, session: &SessionId) {
        self.inner.state.lock().unwrap().owners.add(target, session);
    }

    /// The tab `target` turned out not to be a session's.
    pub fn disown(&self, target: &str) {
        self.inner.state.lock().unwrap().owners.remove(target);
    }

    /// The tab `target` was opened by `opener`.
    pub fn opened(&self, target: &str, opener: &str) {
        self.inner
            .state
            .lock()
            .unwrap()
            .owners
            .opened(target, opener);
    }

    /// The session ended: its downloads still running are deleted when they end, and its
    /// reports not yet taken are dropped.
    pub fn forget_session(&self, session: &SessionId) {
        let mut state = self.inner.state.lock().unwrap();
        state.owners.forget_session(session);
        state.used.remove(session);
        state.reports.remove(session);
        state.gone.insert(session.clone());
    }

    /// The session's reports since the last call, oldest first.
    pub fn take_reports(&self, session: &SessionId) -> Vec<Report> {
        self.inner
            .state
            .lock()
            .unwrap()
            .reports
            .remove(session)
            .unwrap_or_default()
    }

    /// The staging directory of a profile (diagnostics and tests).
    #[doc(hidden)]
    pub fn staging_dir(&self, profile_dir: &str) -> PathBuf {
        self.inner.root.join(profile_dir)
    }

    /// Test hook: closes the context's policy connection as a failure would and keeps a new
    /// one from being made for `hold`. The caller gates it.
    #[doc(hidden)]
    pub fn drop_connection_for_test(&self, context: &str, hold: Duration) -> bool {
        *self.inner.hold_until.lock().unwrap() = Some(tokio::time::Instant::now() + hold);
        let conn = {
            let mut state = self.inner.state.lock().unwrap();
            match state.policies.get_mut(context) {
                Some(p) => {
                    p.ready = false;
                    p.conn.clone()
                }
                None => return false,
            }
        };
        if let Some(conn) = conn {
            conn.close();
        }
        true
    }
}

/// Profile directory names become a directory name under the staging root.
fn valid_dir_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Chrome's download guids look like upper-case UUIDs (L4); anything else is not used as a
/// file name.
fn valid_guid(guid: &str) -> bool {
    (1..=64).contains(&guid.len()) && guid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

impl Inner {
    fn is_ready(&self, context: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .policies
            .get(context)
            .is_some_and(|p| p.ready)
    }

    fn lookup(&self, session: &SessionId) -> Option<SessionView> {
        let lookup = self.lookup.lock().unwrap().clone()?;
        lookup(session)
    }

    /// Sets the policy of `context` on a new connection: the profile is made the default
    /// context (a tab of it activated) and checked to be it, its downloads still running
    /// under an earlier connection are cancelled, then the policy is set. The caller holds
    /// `applying`.
    async fn establish(
        self: &Arc<Self>,
        context: &str,
        profile_dir: &str,
        tries: u32,
    ) -> Result<()> {
        let staging = self.root.join(profile_dir);
        ensure_owned_private_dir(&self.root)?;
        ensure_owned_private_dir(&staging)?;
        let staging = staging.canonicalize()?;
        let (conn, events) = CdpConn::connect_queued(&self.ws_url).await?;
        let mut last = anyhow!("the profile did not become the default context");
        for attempt in 0..tries {
            if attempt > 0 {
                tokio::time::sleep(RETRY_PAUSE).await;
            }
            if !self.is_default(context).await {
                if let Err(e) = self.activate(context).await {
                    last = e;
                    continue;
                }
                if !self.is_default(context).await {
                    continue;
                }
            }
            self.cancel_running(context, &staging).await;
            let set = conn
                .send(
                    "Browser.setDownloadBehavior",
                    json!({
                        "behavior": "allowAndName",
                        "downloadPath": staging,
                        "eventsEnabled": true,
                        "browserContextId": context,
                    }),
                    None,
                )
                .await;
            match set {
                Ok(_) => {
                    self.install(context, profile_dir, staging, conn, events);
                    return Ok(());
                }
                Err(e) => last = e,
            }
        }
        conn.close();
        Err(last)
    }

    /// Whether `context` is the browser's default context now.
    async fn is_default(&self, context: &str) -> bool {
        self.conn
            .send("Target.getBrowserContexts", json!({}), None)
            .await
            .ok()
            .and_then(|r| r["defaultBrowserContextId"].as_str().map(String::from))
            .is_some_and(|d| d == context)
    }

    /// Activates a tab of the agent `context`, which makes it the default context (E3).
    async fn activate(&self, context: &str) -> Result<()> {
        if context == self.human_context {
            bail!("refusing to activate the human's tabs");
        }
        let pages = page_targets(&self.conn).await?;
        let page = pages
            .iter()
            .find(|t| t.browser_context_id.as_deref() == Some(context))
            .ok_or_else(|| anyhow!("the agent profile has no open tab"))?;
        self.conn
            .send(
                "Target.activateTarget",
                json!({ "targetId": page.target_id }),
                None,
            )
            .await?;
        Ok(())
    }

    /// Records the new connection as the context's policy and follows its events.
    fn install(
        self: &Arc<Self>,
        context: &str,
        profile_dir: &str,
        staging: PathBuf,
        conn: CdpConn,
        mut events: mpsc::UnboundedReceiver<CdpEvent>,
    ) {
        let (generation, old) = {
            let mut state = self.state.lock().unwrap();
            let policy = state
                .policies
                .entry(context.to_string())
                .or_insert_with(|| Policy {
                    profile_dir: profile_dir.to_string(),
                    staging: staging.clone(),
                    conn: None,
                    ready: false,
                    generation: 0,
                    reapplying: false,
                });
            policy.generation += 1;
            policy.staging = staging;
            policy.ready = true;
            // A loss of this connection starts a new attempt.
            policy.reapplying = false;
            let old = policy.conn.replace(conn);
            (policy.generation, old)
        };
        // Never two policy connections on one context (F3).
        if let Some(old) = old {
            old.close();
        }
        let weak = Arc::downgrade(self);
        let context = context.to_string();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                inner.on_event(event);
            }
            if let Some(inner) = weak.upgrade() {
                inner.connection_ended(&context, generation);
            }
        });
    }

    fn connection_ended(self: &Arc<Self>, context: &str, generation: u64) {
        let start = {
            let mut state = self.state.lock().unwrap();
            match state.policies.get_mut(context) {
                Some(p) if p.generation == generation => {
                    p.ready = false;
                    p.conn = None;
                    !std::mem::replace(&mut p.reapplying, true)
                }
                _ => false,
            }
        };
        if start {
            eprintln!("pegboard: a download policy connection ended; setting it again");
            tokio::spawn(reapply(Arc::downgrade(self), context.to_string()));
        }
    }

    /// Cancels the downloads of `context` that are still running (they would finish
    /// without telling anyone, L5) and settles them as dropped.
    async fn cancel_running(self: &Arc<Self>, context: &str, staging: &Path) {
        let running: Vec<String> = {
            let mut state = self.state.lock().unwrap();
            let guids: Vec<String> = state
                .downloads
                .iter()
                .filter(|(guid, d)| {
                    d.end.is_none()
                        && (d.context.as_deref() == Some(context) || staging.join(guid).exists())
                })
                .map(|(guid, _)| guid.clone())
                .collect();
            for guid in &guids {
                if let Some(d) = state.downloads.get_mut(guid) {
                    d.dropped.get_or_insert_with(|| POLICY_LOST.to_string());
                    d.end = Some(End::Canceled);
                }
            }
            guids
        };
        for guid in running {
            self.cancel(&guid, context).await;
            self.settle(&guid);
        }
    }

    async fn cancel(&self, guid: &str, context: &str) {
        if context == self.human_context {
            return;
        }
        let r = self
            .conn
            .send(
                "Browser.cancelDownload",
                json!({ "guid": guid, "browserContextId": context }),
                None,
            )
            .await;
        if let Err(e) = r {
            // The staged file is deleted when the download ends or by the sweeper.
            eprintln!("pegboard: could not cancel a download: {e:#}");
        }
    }

    fn on_event(self: &Arc<Self>, event: CdpEvent) {
        let p = &event.params;
        let text = |k: &str| p[k].as_str().unwrap_or_default().to_string();
        let guid = text("guid");
        if !valid_guid(&guid) {
            return;
        }
        match event.method.as_str() {
            "Browser.downloadWillBegin" => {
                let fresh = {
                    let mut state = self.state.lock().unwrap();
                    if state.downloads.contains_key(&guid) {
                        false
                    } else {
                        state.downloads.insert(
                            guid.clone(),
                            Download {
                                name: safe_name(&text("suggestedFilename")),
                                url: text("url"),
                                started: Instant::now(),
                                resolved: false,
                                session: None,
                                context: None,
                                dropped: None,
                                received: 0,
                                total: 0,
                                end: None,
                            },
                        );
                        true
                    }
                };
                if fresh {
                    let me = self.clone();
                    let frame = text("frameId");
                    tokio::spawn(async move { me.judge(&guid, &frame).await });
                }
            }
            "Browser.downloadProgress" => {
                let bytes = |k: &str| p[k].as_f64().unwrap_or(0.0).max(0.0) as u64;
                let end = match p["state"].as_str() {
                    Some("completed") => Some(End::Completed),
                    Some("canceled") => Some(End::Canceled),
                    _ => None,
                };
                self.on_progress(&guid, bytes("receivedBytes"), bytes("totalBytes"), end);
            }
            _ => {}
        }
    }

    fn on_progress(self: &Arc<Self>, guid: &str, received: u64, total: u64, end: Option<End>) {
        let (cancel, settle) = {
            let mut state = self.state.lock().unwrap();
            let used_of = |state: &State, s: &Option<SessionId>| {
                s.as_ref()
                    .and_then(|s| state.used.get(s).copied())
                    .unwrap_or(0)
            };
            let Some(session) = state.downloads.get(guid).map(|d| d.session.clone()) else {
                return;
            };
            let used = used_of(&state, &session);
            let d = state.downloads.get_mut(guid).unwrap();
            if d.end.is_some() {
                return;
            }
            d.received = received;
            d.total = total;
            d.end = end;
            let mut cancel = None;
            if end.is_none() && d.resolved && d.dropped.is_none() && session.is_some() {
                if let Some(why) = self.limits.judge(received.max(total), used) {
                    d.dropped = Some(why);
                    cancel = d.context.clone();
                }
            }
            (cancel, d.end.is_some() && d.resolved)
        };
        if let Some(context) = cancel {
            let me = self.clone();
            let guid = guid.to_string();
            tokio::spawn(async move { me.cancel(&guid, &context).await });
        }
        if settle {
            self.settle(guid);
        }
    }

    /// Finds the download's session and decides whether it may be kept.
    async fn judge(self: &Arc<Self>, guid: &str, frame: &str) {
        let mut session = None;
        // A download of the human's own tab is not looked for any further.
        let tries = if self.in_human_tab(frame).await {
            0
        } else {
            RESOLVE_TRIES
        };
        for attempt in 0..tries {
            if attempt > 0 {
                tokio::time::sleep(RESOLVE_PAUSE).await;
            }
            session = self.session_of_frame(frame).await;
            if session.is_some() {
                break;
            }
        }
        let view = session.as_ref().and_then(|s| self.lookup(s));
        let (cancel, settle) = {
            let mut state = self.state.lock().unwrap();
            let used = session
                .as_ref()
                .and_then(|s| state.used.get(s).copied())
                .unwrap_or(0);
            let Some(d) = state.downloads.get_mut(guid) else {
                return;
            };
            let why = match (&session, &view) {
                (None, _) => Some(NOT_A_SESSION_TAB.to_string()),
                (Some(_), None) => Some(NOT_A_SESSION_TAB.to_string()),
                (Some(_), Some(v)) if !v.allows_downloads => Some(DENIED.to_string()),
                (Some(_), Some(_)) => self.limits.judge(d.received.max(d.total), used),
            };
            d.resolved = true;
            d.session = session;
            d.context = view.map(|v| v.context);
            if d.dropped.is_none() {
                d.dropped = why;
            }
            let cancel = match (&d.dropped, d.end) {
                (Some(_), None) => d.context.clone(),
                _ => None,
            };
            (cancel, d.end.is_some())
        };
        if let Some(context) = cancel {
            self.cancel(guid, &context).await;
        }
        if settle {
            self.settle(guid);
        }
    }

    /// The session whose tab started a download in `frame`.
    async fn session_of_frame(&self, frame: &str) -> Option<SessionId> {
        let mut frame = frame.to_string();
        for _ in 0..MAX_FRAME_HOPS {
            if let Some(s) = self.state.lock().unwrap().owners.session_of(&frame) {
                return Some(s);
            }
            // A cross-site frame's own target names the frame it sits in (F1b).
            if let Some(parent) = self.parent_of_frame_target(&frame).await {
                frame = parent;
                continue;
            }
            // A frame inside a tab's (or a cross-site frame's) frame tree (F1).
            let holder = tokio::time::timeout(FRAME_SEARCH_LIMIT, self.holder_of(&frame))
                .await
                .ok()
                .flatten()?;
            frame = holder;
        }
        None
    }

    /// Whether `frame` is (the main frame of) a target of the human's profile.
    async fn in_human_tab(&self, frame: &str) -> bool {
        self.conn
            .send("Target.getTargetInfo", json!({ "targetId": frame }), None)
            .await
            .ok()
            .is_some_and(|r| r["targetInfo"]["browserContextId"] == self.human_context.as_str())
    }

    async fn parent_of_frame_target(&self, frame: &str) -> Option<String> {
        let r = self
            .conn
            .send("Target.getTargetInfo", json!({ "targetId": frame }), None)
            .await
            .ok()?;
        let info = &r["targetInfo"];
        let context = info["browserContextId"].as_str().unwrap_or_default();
        if info["type"] != "iframe" || context.is_empty() || context == self.human_context {
            return None;
        }
        info["parentFrameId"].as_str().map(String::from)
    }

    /// The agent tab or frame target whose frame tree holds `frame` below its own frame.
    async fn holder_of(&self, frame: &str) -> Option<String> {
        let r = self
            .conn
            .send("Target.getTargets", json!({}), None)
            .await
            .ok()?;
        let targets: Vec<String> = r["targetInfos"]
            .as_array()?
            .iter()
            .filter(|t| matches!(t["type"].as_str(), Some("page" | "iframe")))
            .filter(|t| {
                let c = t["browserContextId"].as_str().unwrap_or_default();
                !c.is_empty() && c != self.human_context
            })
            .filter_map(|t| t["targetId"].as_str().map(String::from))
            .filter(|t| t != frame)
            .collect();
        for target in targets {
            let Ok(session) = self.conn.attach(&target).await else {
                continue;
            };
            let s = Some(session.as_str());
            let _ = self.conn.send("Page.enable", json!({}), s).await;
            let tree = self.conn.send("Page.getFrameTree", json!({}), s).await;
            let _ = self
                .conn
                .send(
                    "Target.detachFromTarget",
                    json!({ "sessionId": session }),
                    None,
                )
                .await;
            if tree.is_ok_and(|t| holds_frame(&t["frameTree"], frame)) {
                return Some(target);
            }
        }
        None
    }

    /// The staged file of a download, in whichever profile's staging directory it is.
    fn staged(state: &State, guid: &str) -> Option<PathBuf> {
        state
            .policies
            .values()
            .map(|p| p.staging.join(guid))
            .find(|path| std::fs::symlink_metadata(path).is_ok())
    }

    /// A download that is decided and has ended: its file is moved into its session's files
    /// directory or deleted, and its session (if it is still open) gets a report.
    fn settle(&self, guid: &str) {
        // Looked up before the state is locked (the lookup takes the session table's lock).
        let session = {
            let state = self.state.lock().unwrap();
            match state.downloads.get(guid) {
                Some(d) if d.resolved && d.end.is_some() => d.session.clone(),
                _ => return,
            }
        };
        let view = session.as_ref().and_then(|s| self.lookup(s));
        let mut state = self.state.lock().unwrap();
        let Some(d) = state.downloads.remove(guid) else {
            return;
        };
        let staged = Self::staged(&state, guid);
        let open = session
            .as_ref()
            .filter(|s| view.is_some() && !state.gone.contains(*s))
            .cloned();
        let mut name = d.name.clone();
        let outcome = match (d.end, d.dropped, &open, &view) {
            (Some(End::Completed), None, Some(s), Some(v)) if v.allows_downloads => match staged {
                Some(path) => {
                    let used = state.used.get(s).copied().unwrap_or(0);
                    let size = std::fs::symlink_metadata(&path)
                        .map(|m| m.len())
                        .unwrap_or(0);
                    match self.limits.judge(size, used) {
                        Some(why) => {
                            let _ = std::fs::remove_file(&path);
                            Outcome::Dropped(why)
                        }
                        None => match ensure_session_files(&self.paths, s)
                            .and_then(|dir| move_into(&path, &dir, &d.name))
                        {
                            Ok((saved, bytes)) => {
                                name = saved;
                                *state.used.entry(s.clone()).or_default() += bytes;
                                Outcome::Saved { bytes }
                            }
                            Err(e) => {
                                eprintln!("pegboard: saving a download failed: {e}");
                                let _ = std::fs::remove_file(&path);
                                Outcome::Dropped(NOT_SAVED.to_string())
                            }
                        },
                    }
                }
                None => Outcome::Dropped(NOT_SAVED.to_string()),
            },
            (end, dropped, _, view) => {
                if let Some(path) = staged {
                    let _ = std::fs::remove_file(path);
                }
                Outcome::Dropped(dropped.unwrap_or_else(|| match (end, view) {
                    (Some(End::Canceled), _) => CANCELED.to_string(),
                    (_, Some(v)) if !v.allows_downloads => DENIED.to_string(),
                    _ => NOT_A_SESSION_TAB.to_string(),
                }))
            }
        };
        if let Some(s) = open {
            state.reports.entry(s).or_default().push(Report {
                name,
                url: d.url,
                outcome,
                ended_at: now(),
            });
        }
    }
}

/// Whether a `Page.getFrameTree` tree holds `frame` below its root frame.
fn holds_frame(tree: &Value, frame: &str) -> bool {
    tree["childFrames"].as_array().is_some_and(|children| {
        children
            .iter()
            .any(|c| c["frame"]["id"] == frame || holds_frame(c, frame))
    })
}

fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Sets a context's policy again after its connection ended, until it is in place; tells
/// `PolicyLost` once when that takes longer than `REAPPLY_LIMIT`.
async fn reapply(weak: Weak<Inner>, context: String) {
    let started = tokio::time::Instant::now();
    let mut told = false;
    loop {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let hold = *inner.hold_until.lock().unwrap();
        if let Some(until) = hold {
            drop(inner);
            tokio::time::sleep_until(until).await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            inner.hold_until.lock().unwrap().take();
            continue;
        }
        let profile_dir = {
            let mut state = inner.state.lock().unwrap();
            match state.policies.get_mut(&context) {
                Some(p) if !p.ready => p.profile_dir.clone(),
                Some(p) => {
                    p.reapplying = false;
                    return;
                }
                None => return,
            }
        };
        let result = {
            let _one = inner.applying.lock().await;
            if inner.is_ready(&context) {
                Ok(())
            } else {
                inner.establish(&context, &profile_dir, AGAIN_TRIES).await
            }
        };
        match result {
            Ok(()) => {
                eprintln!("pegboard: the download policy is set again");
                return;
            }
            Err(e) => eprintln!("pegboard: setting the download policy again failed: {e:#}"),
        }
        if !told && started.elapsed() >= REAPPLY_LIMIT {
            told = true;
            let lost = inner.lost.lock().unwrap().clone();
            if let Some(lost) = lost {
                lost(&context);
            }
        }
        // Without a tab the profile has nothing to protect (Chrome may unload it); opening
        // its window again sets the policy anew (`apply`).
        let no_tab = page_targets(&inner.conn).await.is_ok_and(|pages| {
            pages
                .iter()
                .all(|t| t.browser_context_id.as_deref() != Some(context.as_str()))
        });
        if told && no_tab {
            inner.state.lock().unwrap().policies.remove(&context);
            return;
        }
        drop(inner);
        tokio::time::sleep(if told { SLOW_RETRY_PAUSE } else { RETRY_PAUSE }).await;
    }
}

/// Deletes staged files no running download claims (a download that ended while no
/// connection told about it), and gives up downloads that never end.
async fn sweep(weak: Weak<Inner>) {
    loop {
        tokio::time::sleep(SWEEP_EVERY).await;
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let (dirs, active) = {
            let mut state = inner.state.lock().unwrap();
            let stale: Vec<String> = state
                .downloads
                .iter()
                .filter(|(_, d)| d.started.elapsed() > STALE)
                .map(|(g, _)| g.clone())
                .collect();
            for guid in &stale {
                state.downloads.remove(guid);
            }
            let dirs: Vec<PathBuf> = state.policies.values().map(|p| p.staging.clone()).collect();
            let active: HashSet<String> = state.downloads.keys().cloned().collect();
            (dirs, active)
        };
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if active.contains(&name) {
                    continue;
                }
                let old = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > ORPHAN_AGE);
                if old && !entry.file_type().is_ok_and(|t| t.is_dir()) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_made_safe() {
        assert_eq!(safe_name("../../x.sh"), "x.sh");
        assert_eq!(safe_name("a/b\\c.txt"), "c.txt");
        for empty in ["", ".", "..", "a/", "a/..", "\n\0"] {
            assert_eq!(safe_name(empty), "download", "{empty:?}");
        }
        assert_eq!(safe_name("re\nport\0.pdf"), "report.pdf");
        assert_eq!(safe_name("한글-이름.txt"), "한글-이름.txt");
        let long = format!("{}.tar.gz", "a".repeat(300));
        let cut = safe_name(&long);
        assert_eq!(cut.len(), 200, "{cut}");
        assert!(cut.ends_with("a.gz"), "{cut}");
        // Cut on a character boundary.
        let wide = format!("{}.txt", "가".repeat(100));
        let cut = safe_name(&wide);
        assert!(cut.len() <= 200 && cut.ends_with(".txt"), "{cut}");
        // A long extension is not kept whole.
        let cut = safe_name(&format!("x.{}", "e".repeat(300)));
        assert_eq!(cut.len(), 200);

        assert_eq!(numbered("report.pdf", 0), "report.pdf");
        assert_eq!(numbered("report.pdf", 1), "report-1.pdf");
        assert_eq!(numbered("archive.tar.gz", 2), "archive.tar-2.gz");
        assert_eq!(numbered("README", 3), "README-3");
        assert_eq!(numbered(".bashrc", 1), ".bashrc-1");

        // Moving: never replaces a file, numbers the name instead; 0600, no link left.
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let tmp = tempfile::tempdir().unwrap();
        let (staging, files) = (tmp.path().join("staging"), tmp.path().join("files"));
        std::fs::create_dir(&staging).unwrap();
        std::fs::create_dir(&files).unwrap();
        for (guid, body) in [("G1", "first"), ("G2", "second!")] {
            std::fs::write(staging.join(guid), body).unwrap();
        }
        let a = move_into(&staging.join("G1"), &files, "report.pdf").unwrap();
        let b = move_into(&staging.join("G2"), &files, "report.pdf").unwrap();
        assert_eq!(a, ("report.pdf".to_string(), 5));
        assert_eq!(b, ("report-1.pdf".to_string(), 7));
        assert_eq!(
            std::fs::read_to_string(files.join("report.pdf")).unwrap(),
            "first"
        );
        let meta = std::fs::metadata(files.join("report-1.pdf")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(meta.nlink(), 1);
        assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
        // A symlink in the files directory under the wanted name is not followed or replaced.
        let outside = tmp.path().join("outside.txt");
        std::os::unix::fs::symlink(&outside, files.join("x.txt")).unwrap();
        std::fs::write(staging.join("G3"), "third").unwrap();
        let c = move_into(&staging.join("G3"), &files, "x.txt").unwrap();
        assert_eq!(c.0, "x-1.txt");
        assert!(!outside.exists());
        // A staged file that is not a plain file is refused.
        std::os::unix::fs::symlink(files.join("report.pdf"), staging.join("G4")).unwrap();
        assert!(move_into(&staging.join("G4"), &files, "y.txt").is_err());
        assert!(!files.join("y.txt").exists());
    }

    #[test]
    fn limits() {
        let l = Limits::default();
        assert_eq!(l.judge(MAX_FILE_BYTES, 0), None);
        assert_eq!(
            l.judge(MAX_FILE_BYTES + 1, 0).as_deref(),
            Some("file larger than 200 MiB")
        );
        assert_eq!(l.judge(10, MAX_SESSION_BYTES - 10), None);
        assert_eq!(
            l.judge(11, MAX_SESSION_BYTES - 10).as_deref(),
            Some("session download limit of 1 GiB reached")
        );
        let small = Limits {
            file: 1024,
            session: 4096,
        };
        assert_eq!(
            small.judge(1025, 0).as_deref(),
            Some("file larger than 1 KiB")
        );
        assert_eq!(
            small.judge(1000, 3500).as_deref(),
            Some("session download limit of 4 KiB reached")
        );
    }

    #[test]
    fn attribution() {
        let (a, b) = (
            SessionId("s_aaaaaaaa".into()),
            SessionId("s_bbbbbbbb".into()),
        );
        let mut owners = Owners::default();
        owners.add("T1", &a);
        owners.add("T2", &b);
        assert_eq!(owners.session_of("T1"), Some(a.clone()));
        assert_eq!(owners.session_of("T2"), Some(b.clone()));
        assert_eq!(owners.session_of("unknown"), None);
        // A popup that died before it joined its opener's session, and its own popup.
        owners.opened("P1", "T1");
        owners.opened("P2", "P1");
        assert_eq!(owners.session_of("P2"), Some(a.clone()));
        // A tab opened by a tab no session owns.
        owners.opened("P3", "unknown");
        assert_eq!(owners.session_of("P3"), None);
        // A loop of openers ends.
        owners.opened("L1", "L2");
        owners.opened("L2", "L1");
        assert_eq!(owners.session_of("L1"), None);
        owners.forget_session(&a);
        assert_eq!(owners.session_of("T1"), None);
        assert_eq!(owners.session_of("P2"), None);
        assert_eq!(owners.session_of("T2"), Some(b));
    }

    #[test]
    fn test_limits_only_lower_and_are_inert_when_unset() {
        assert_eq!(Limits::parse(None), Limits::default());
        assert_eq!(
            Limits::parse(Some("1024,4096")),
            Limits {
                file: 1024,
                session: 4096
            }
        );
        let huge = format!("{},{}", u64::MAX, u64::MAX);
        for bad in [
            "",
            "x",
            "1024",
            "1024,",
            "0,5",
            "5,0",
            "1,2,3",
            " , ",
            huge.as_str(),
        ] {
            assert_eq!(Limits::parse(Some(bad)), Limits::default(), "{bad:?}");
        }
    }

    #[test]
    fn files_are_listed_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("files");
        assert!(list_files(&dir).is_empty());
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("b.txt"), "bb").unwrap();
        std::fs::write(dir.join("a.pdf"), "a").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::os::unix::fs::symlink(dir.join("a.pdf"), dir.join("link.pdf")).unwrap();
        let files = list_files(&dir);
        let names: Vec<(&str, u64)> = files.iter().map(|f| (f.name.as_str(), f.bytes)).collect();
        assert_eq!(names, [("a.pdf", 1), ("b.txt", 2)]);
        assert!(
            files[0].modified_at.ends_with('Z'),
            "{}",
            files[0].modified_at
        );
    }

    #[test]
    fn reports_name_the_file_and_its_size_or_why_it_was_dropped() {
        let saved = Report {
            name: "a.pdf".into(),
            url: "https://user:pw@example.com/f?token=secret#x".into(),
            outcome: Outcome::Saved { bytes: 12 },
            ended_at: "2026-11-14T00:00:00Z".into(),
        };
        assert_eq!(saved.to_json(), json!({ "name": "a.pdf", "bytes": 12 }));
        let s = SessionId("s_aaaaaaaa".into());
        let e = saved.log_entry(&s);
        assert_eq!(e.command, "download");
        assert_eq!(e.args, ["a.pdf", "12"]);
        assert!(e.ok && e.error.is_none());
        let url = e.url.unwrap();
        assert!(!url.contains("secret") && !url.contains("pw"), "{url}");
        let dropped = Report {
            outcome: Outcome::Dropped(DENIED.into()),
            ..saved
        };
        assert_eq!(
            dropped.to_json(),
            json!({ "name": "a.pdf", "dropped": DENIED })
        );
        let e = dropped.log_entry(&s);
        assert_eq!(e.args, ["a.pdf"]);
        assert!(!e.ok);
        assert_eq!(e.error.unwrap(), format!("dropped: {DENIED}"));
    }

    #[test]
    fn frames_are_found_below_the_root_only() {
        let tree = json!({
            "frame": { "id": "TOP" },
            "childFrames": [
                { "frame": { "id": "A" } },
                { "frame": { "id": "B" }, "childFrames": [ { "frame": { "id": "C" } } ] },
            ],
        });
        for f in ["A", "B", "C"] {
            assert!(holds_frame(&tree, f), "{f}");
        }
        assert!(!holds_frame(&tree, "TOP"));
        assert!(!holds_frame(&tree, "D"));
    }

    #[test]
    fn only_guid_and_directory_shaped_names_become_paths() {
        assert!(valid_guid("0F3C2A1B-1234-4ABC-9DEF-0123456789AB"));
        for bad in ["", "../x", "a/b", "G", ".", &"A".repeat(65)] {
            assert!(!valid_guid(bad), "{bad:?}");
        }
        assert!(valid_dir_name("PegboardAgent-work"));
        for bad in ["", ".x", "a/b", "..", "a b"] {
            assert!(!valid_dir_name(bad), "{bad:?}");
        }
    }
}
