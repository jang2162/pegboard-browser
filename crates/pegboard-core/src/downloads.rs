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
/// Most bytes the running downloads of one profile may hold at once in its staging
/// directory, all its sessions (and unattributed downloads) together. One session's running
/// downloads can use it up for the other sessions of the profile.
pub const MAX_PROFILE_RUNNING_BYTES: u64 = 2 << 30;
/// Longest file name (bytes) a download is saved under, before a `-N` that keeps it unique.
const MAX_NAME_BYTES: usize = 200;
const FALLBACK_NAME: &str = "download";

/// The name a download is saved under, from the name the browser suggests: the last path
/// component, without control and invisible format characters (bidi overrides, zero-width),
/// without leading `-`, `.` or spaces (as upload names start with a letter or digit), at
/// most `MAX_NAME_BYTES` bytes (keeping a short extension); `download` when nothing usable
/// is left. Non-ASCII letters are kept (such a name cannot be uploaded again, whose rule is
/// ASCII only).
pub fn safe_name(suggested: &str) -> String {
    let last = suggested.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = last
        .chars()
        .filter(|c| !c.is_control() && !is_format_char(*c))
        .collect();
    let clean = clean
        .trim_start_matches(|c: char| c == '-' || c == '.' || c.is_whitespace())
        .to_string();
    if clean.is_empty() {
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

/// Unicode format characters (category Cf) that change how a name looks without showing.
fn is_format_char(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 | 0x08E2
        | 0x180E | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F
        | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F
        | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
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
                let done = std::fs::remove_file(src).and_then(|()| {
                    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o600))
                });
                if let Err(e) = done {
                    // Nothing half-moved stays in the files directory.
                    let _ = std::fs::remove_file(&dst);
                    return Err(e);
                }
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

/// The download limits: one file, one session's downloads in all, and one profile's
/// running downloads at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub file: u64,
    pub session: u64,
    pub profile: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            file: MAX_FILE_BYTES,
            session: MAX_SESSION_BYTES,
            profile: MAX_PROFILE_RUNNING_BYTES,
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

    /// `"<file bytes>,<session bytes>[,<profile bytes>]"` lowers the limits (never raises
    /// them); anything else, or nothing, leaves them as they are.
    pub fn parse(spec: Option<&str>) -> Limits {
        let default = Limits::default();
        let parsed = spec.and_then(|spec| {
            let values: Vec<u64> = spec
                .split(',')
                .map(|v| v.trim().parse().ok().filter(|n| *n > 0))
                .collect::<Option<_>>()?;
            let (file, session, profile) = match values.as_slice() {
                [f, s] => (*f, *s, default.profile),
                [f, s, p] => (*f, *s, *p),
                _ => return None,
            };
            Some(Limits {
                file: file.min(default.file),
                session: session.min(default.session),
                profile: profile.min(default.profile),
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

/// A running download as the limits see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Running {
    pub session: Option<SessionId>,
    /// The browser context (profile) it is saved in, when known.
    pub context: Option<String>,
    /// Bytes it has (or has declared, whichever is more).
    pub bytes: u64,
}

/// Why the running download `me` is dropped: over the file limit; with its session's saved
/// bytes (`used`) and the session's other running downloads over the session limit; or with
/// the other running downloads of its profile over the profile limit (what one profile's
/// staging directory may hold at once). `others` are the other running downloads not
/// already dropped.
pub fn judge_running(
    limits: &Limits,
    me: &Running,
    others: &[Running],
    used: u64,
) -> Option<String> {
    if me.bytes > limits.file {
        return Some(format!("file larger than {}", size_text(limits.file)));
    }
    if let Some(session) = &me.session {
        let running: u64 = others
            .iter()
            .filter(|o| o.session.as_ref() == Some(session))
            .map(|o| o.bytes)
            .sum();
        if used.saturating_add(running).saturating_add(me.bytes) > limits.session {
            return Some(format!(
                "session download limit of {} reached",
                size_text(limits.session)
            ));
        }
    }
    if let Some(context) = &me.context {
        let running: u64 = others
            .iter()
            .filter(|o| o.context.as_ref() == Some(context))
            .map(|o| o.bytes)
            .sum();
        if me.bytes.saturating_add(running) > limits.profile {
            return Some(format!(
                "downloads running in the profile exceed {}",
                size_text(limits.profile)
            ));
        }
    }
    None
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
/// Looking for a dropped download's staged file (to learn its profile and cancel it).
const LOCATE_TRIES: u32 = 10;
const LOCATE_PAUSE: Duration = Duration::from_millis(200);
/// Ended sessions remembered (`State::gone`).
const MAX_GONE: usize = 4096;
/// Settled guids remembered (`State::settled`).
const MAX_SETTLED: usize = 1024;
/// Longest time spent looking through tabs' frame trees for one download.
const FRAME_SEARCH_LIMIT: Duration = Duration::from_secs(5);
/// Most frames walked up from a download's frame to its tab.
const MAX_FRAME_HOPS: usize = 8;

/// Why a download was deleted, as the agent and the action log are told.
pub const NOT_A_SESSION_TAB: &str = "not started by a tab of a session";
pub const DENIED: &str = "the session's policy denies downloads (--deny download)";
pub const POLICY_LOST: &str = "pegboard lost its download policy while the download ran";
pub const CANCELED: &str = "canceled";
pub const SESSION_CLOSED: &str = "the session was closed";
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
    /// Whether the profile's sessions deny the clipboard (`apply_clipboard`). Then
    /// `clipboard` is the connection that set `clipboard-read` and `clipboard-write` to
    /// denied for the profile: Chrome lifts the denial when it closes (spike C2), and closing
    /// it is how the denial is lifted on purpose.
    clipboard_denied: bool,
    clipboard: Option<CdpConn>,
    /// Bumped with every clipboard connection, like `generation`.
    clipboard_generation: u64,
    /// The profile may carry a clipboard denial in Chrome: set since a denial was set and
    /// until it was lifted by a reset. Closing (or losing) the connection does not lift it
    /// (Task 7, measured), so a profile whose connection was lost is reset anew.
    clipboard_maybe_denied: bool,
}

impl Policy {
    /// A profile's policy as first seen by this daemon. Its clipboard counts as maybe denied:
    /// the browser may have outlived an earlier daemon that denied it (a denial outlives the
    /// connection that set it), so the first session allowing the clipboard resets it.
    fn new(profile_dir: &str, staging: PathBuf) -> Policy {
        Policy {
            profile_dir: profile_dir.to_string(),
            staging,
            conn: None,
            ready: false,
            generation: 0,
            reapplying: false,
            clipboard_denied: false,
            clipboard: None,
            clipboard_generation: 0,
            clipboard_maybe_denied: true,
        }
    }

    /// Whether the clipboard is as the profile's sessions want it.
    fn clipboard_ready(&self) -> bool {
        !self.clipboard_denied || self.clipboard.is_some()
    }
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
    /// The oldest are forgotten past `MAX_GONE` (a closed session has no view any more, so
    /// this is the second check).
    gone: HashSet<SessionId>,
    gone_order: std::collections::VecDeque<SessionId>,
    /// Cancels that worked and that were refused (diagnostics: `cancel_counts`).
    cancels: [u64; 2],
    /// Tabs activated to make a profile the default (diagnostics: `activations`).
    activations: u64,
    /// Guids settled lately: every policy connection reports every download (L1), so a
    /// late copy of an event must not start a settled download again.
    settled: HashSet<String>,
    settled_order: std::collections::VecDeque<String>,
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
/// - The clipboard denial of a profile whose sessions deny the clipboard
///   (`apply_clipboard`) lives the same way on a second connection of its own: one that only
///   ever sets permissions, so closing it to lift the denial leaves the download policy be.
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
            .flat_map(|p| [p.conn.take(), p.clipboard.take()])
            .flatten()
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

    /// Sets whether the profile (browser context `context`, under pegboard's download
    /// policy already: `apply`) denies the clipboard, as its sessions' policies say. Denied:
    /// `Browser.setPermission` sets `clipboard-read` and `clipboard-write` to denied for the
    /// whole profile on a connection of its own, kept open (spikes C2, E2, E3); the profile
    /// is made the default context for it if it is not (a tab of it activated: never call
    /// this from a path a page can trigger). Allowed: that connection is closed, which lifts
    /// the denial. An error leaves the profile not ready: the caller must not use it.
    pub async fn apply_clipboard(&self, context: &str, denied: bool) -> Result<()> {
        if context.is_empty() || context == self.inner.human_context {
            bail!("refusing to set a clipboard policy on the human or an unknown context");
        }
        let _one = self.inner.applying.lock().await;
        // What lifting needs: the connection that holds the denial, or (when that was lost
        // and the denial may still be in Chrome) a new connection only for the reset.
        enum Lift {
            Nothing,
            Held(CdpConn),
            Lost,
        }
        let lift = {
            let mut state = self.inner.state.lock().unwrap();
            let Some(policy) = state.policies.get_mut(context) else {
                bail!("the profile has no download policy");
            };
            if denied {
                policy.clipboard_denied = true;
                Lift::Nothing
            } else if let Some(conn) = policy.clipboard.clone() {
                Lift::Held(conn)
            } else if policy.clipboard_maybe_denied {
                Lift::Lost
            } else {
                Lift::Nothing
            }
        };
        let held = match lift {
            Lift::Nothing => None,
            Lift::Held(conn) => Some(conn),
            Lift::Lost => Some(
                CdpConn::connect(&self.inner.ws_url)
                    .await
                    .context("could not lift the clipboard denial of the agent profile")?,
            ),
        };
        if let Some(conn) = held {
            // Closing the connection alone did not lift the denial when measured (second
            // machine, Task 7: a session allowing the clipboard after denied ones could not
            // write); the permissions are reset explicitly first, which Chrome takes only
            // while the profile is the default context (E).
            self.inner
                .lift_clipboard(context, &conn, FIRST_TRIES)
                .await
                .context("could not lift the clipboard denial of the agent profile")?;
            let mut state = self.inner.state.lock().unwrap();
            if let Some(policy) = state.policies.get_mut(context) {
                // Its end is not a loss.
                policy.clipboard_generation += 1;
                policy.clipboard.take();
                policy.clipboard_maybe_denied = false;
            }
            drop(state);
            conn.close();
        }
        if !denied {
            if let Some(policy) = self.inner.state.lock().unwrap().policies.get_mut(context) {
                policy.clipboard_denied = false;
            }
        }
        if self.inner.clipboard_ready(context) {
            return Ok(());
        }
        self.inner
            .deny_clipboard(context, FIRST_TRIES)
            .await
            .context("could not deny the clipboard in the agent profile")
    }

    /// The context is gone (Chrome unloaded the profile): its policy went with it.
    pub fn forget_context(&self, context: &str) {
        let removed = self.inner.state.lock().unwrap().policies.remove(context);
        for conn in removed.into_iter().flat_map(|p| [p.conn, p.clipboard]) {
            conn.into_iter().for_each(|c| c.close());
        }
    }

    /// Whether the context's download policy is in place.
    pub fn is_ready(&self, context: &str) -> bool {
        self.inner.is_ready(context)
    }

    /// Whether the context's clipboard is as its sessions want it (denied unless allowed).
    pub fn clipboard_ready(&self, context: &str) -> bool {
        self.inner.clipboard_ready(context)
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

    /// The session ended: its downloads still running are cancelled and deleted, nothing
    /// is saved for it from now on. Reports not yet taken stay for `take_reports` (the
    /// daemon writes them to the closing session's log).
    pub fn forget_session(&self, session: &SessionId) {
        let running: Vec<(String, String)> = {
            let mut state = self.inner.state.lock().unwrap();
            state.owners.forget_session(session);
            state.used.remove(session);
            if state.gone.insert(session.clone()) {
                state.gone_order.push_back(session.clone());
            }
            if state.gone_order.len() > MAX_GONE {
                if let Some(old) = state.gone_order.pop_front() {
                    state.gone.remove(&old);
                    state.reports.remove(&old);
                }
            }
            state
                .downloads
                .iter_mut()
                .filter(|(_, d)| d.session.as_ref() == Some(session) && d.end.is_none())
                .filter_map(|(guid, d)| {
                    d.dropped.get_or_insert_with(|| SESSION_CLOSED.to_string());
                    d.context.clone().map(|c| (guid.clone(), c))
                })
                .collect()
        };
        for (guid, context) in running {
            let inner = self.inner.clone();
            tokio::spawn(async move { inner.cancel(&guid, &context).await });
        }
    }

    /// Reports taken but not delivered (the log could not be written): they come first
    /// next time.
    pub fn put_back_reports(&self, session: &SessionId, mut reports: Vec<Report>) {
        let mut state = self.inner.state.lock().unwrap();
        if state.gone.contains(session) && !state.reports.contains_key(session) {
            return;
        }
        let entry = state.reports.entry(session.clone()).or_default();
        reports.append(entry);
        *entry = reports;
    }

    /// Cancels that worked and that were refused (diagnostics and tests).
    #[doc(hidden)]
    pub fn cancel_counts(&self) -> [u64; 2] {
        self.inner.state.lock().unwrap().cancels
    }

    /// Tabs this module has activated (each can bring an agent window forward) since the
    /// browser started (diagnostics and tests).
    #[doc(hidden)]
    pub fn activations(&self) -> u64 {
        self.inner.state.lock().unwrap().activations
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

    /// Test hook: closes the context's clipboard connection as a failure would and keeps a
    /// new one from being made for `hold`. The caller gates it. False when there is none.
    #[doc(hidden)]
    pub fn drop_clipboard_for_test(&self, context: &str, hold: Duration) -> bool {
        // Taken at once (a real loss is noticed when the connection's events end, a moment
        // later): a command right after this must see it gone.
        let conn = {
            let mut state = self.inner.state.lock().unwrap();
            state
                .policies
                .get_mut(context)
                .and_then(|p| p.clipboard.take())
        };
        match conn {
            Some(conn) => {
                *self.inner.hold_until.lock().unwrap() = Some(tokio::time::Instant::now() + hold);
                conn.close();
                true
            }
            None => false,
        }
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

    fn clipboard_ready(&self, context: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .policies
            .get(context)
            .is_some_and(Policy::clipboard_ready)
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
            // Not page-triggered (a lost connection, a new window): activating a tab of the
            // profile is the only measured way to make it the default; it brings the agent
            // window forward and can take the keyboard from the human. (A background tab
            // created in the profile is refused while it is not the default: measured.)
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
        self.state.lock().unwrap().activations += 1;
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
                .or_insert_with(|| Policy::new(profile_dir, staging.clone()));
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

    /// Denies the clipboard of `context` on a new connection: the profile is made the
    /// default context (as `establish` does), then `clipboard-read` and `clipboard-write` are
    /// set to denied for it. The caller holds `applying`.
    async fn deny_clipboard(self: &Arc<Self>, context: &str, tries: u32) -> Result<()> {
        let (conn, events) = CdpConn::connect_queued(&self.ws_url).await?;
        let mut last = anyhow!("the profile did not become the default context");
        'attempt: for attempt in 0..tries {
            if attempt > 0 {
                tokio::time::sleep(RETRY_PAUSE).await;
            }
            // Not page-triggered (a session being created, a lost connection): see
            // `establish`.
            if !self.is_default(context).await {
                if let Err(e) = self.activate(context).await {
                    last = e;
                    continue;
                }
                if !self.is_default(context).await {
                    continue;
                }
            }
            // From the first permission sent on, the profile may carry a denial.
            if let Some(p) = self.state.lock().unwrap().policies.get_mut(context) {
                p.clipboard_maybe_denied = true;
            }
            for name in ["clipboard-read", "clipboard-write"] {
                let set = conn
                    .send(
                        "Browser.setPermission",
                        json!({
                            "permission": { "name": name },
                            "setting": "denied",
                            "browserContextId": context,
                        }),
                        None,
                    )
                    .await;
                if let Err(e) = set {
                    last = e;
                    continue 'attempt;
                }
            }
            let installed = {
                let mut state = self.state.lock().unwrap();
                match state.policies.get_mut(context) {
                    // Still wanted (an `apply_clipboard` that allows waits for `applying`).
                    Some(p) if p.clipboard_denied => {
                        p.clipboard_maybe_denied = true;
                        p.clipboard_generation += 1;
                        let old = p.clipboard.replace(conn.clone());
                        Some((p.clipboard_generation, old))
                    }
                    _ => None,
                }
            };
            let Some((generation, old)) = installed else {
                conn.close();
                return Ok(());
            };
            if let Some(old) = old {
                old.close();
            }
            let weak = Arc::downgrade(self);
            let context = context.to_string();
            let mut events = events;
            tokio::spawn(async move {
                while events.recv().await.is_some() {}
                if let Some(inner) = weak.upgrade() {
                    inner.clipboard_ended(&context, generation);
                }
            });
            return Ok(());
        }
        conn.close();
        Err(last)
    }

    /// Resets the permissions of `context` on its clipboard connection, the profile made the
    /// default context first (as `establish` does). The caller holds `applying`.
    async fn lift_clipboard(&self, context: &str, conn: &CdpConn, tries: u32) -> Result<()> {
        let mut last = anyhow!("the profile did not become the default context");
        for attempt in 0..tries {
            if attempt > 0 {
                tokio::time::sleep(RETRY_PAUSE).await;
            }
            // Not page-triggered (a session being created): see `establish`.
            if !self.is_default(context).await {
                if let Err(e) = self.activate(context).await {
                    last = e;
                    continue;
                }
                if !self.is_default(context).await {
                    continue;
                }
            }
            match conn
                .send(
                    "Browser.resetPermissions",
                    json!({ "browserContextId": context }),
                    None,
                )
                .await
            {
                Ok(_) => return Ok(()),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn clipboard_ended(self: &Arc<Self>, context: &str, generation: u64) {
        let start = {
            let mut state = self.state.lock().unwrap();
            match state.policies.get_mut(context) {
                Some(p) if p.clipboard_generation == generation => {
                    p.clipboard = None;
                    p.clipboard_denied && !std::mem::replace(&mut p.reapplying, true)
                }
                _ => false,
            }
        };
        if start {
            eprintln!("pegboard: a clipboard policy connection ended; setting it again");
            tokio::spawn(reapply(Arc::downgrade(self), context.to_string()));
        }
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
        let candidates: Vec<(String, bool)> = {
            let state = self.state.lock().unwrap();
            state
                .downloads
                .iter()
                .filter(|(_, d)| d.end.is_none())
                .map(|(guid, d)| (guid.clone(), d.context.as_deref() == Some(context)))
                .collect()
        };
        // Looked at outside the lock: a download of this profile has its file here.
        let running: Vec<String> = candidates
            .into_iter()
            .filter(|(guid, ours)| *ours || staging.join(guid).exists())
            .map(|(guid, _)| guid)
            .collect();
        {
            let mut state = self.state.lock().unwrap();
            for guid in &running {
                if let Some(d) = state.downloads.get_mut(guid) {
                    d.dropped.get_or_insert_with(|| POLICY_LOST.to_string());
                    d.end = Some(End::Canceled);
                }
            }
        }
        for guid in running {
            self.cancel(&guid, context).await;
            self.settle(&guid);
        }
    }

    /// Cancels a download of the agent profile `context`. Chrome takes a context id only for
    /// the profile that is the default context (measured for cancelling too, Task 5 runs at
    /// cb39d0c and a2ced1c). A page can start this path at will, so when the cancel is
    /// refused nothing more is tried: the only measured way to make the profile the default
    /// is activating one of its tabs, which brings a window forward and can take the keys
    /// the human is typing (a background tab is refused too). The download then runs on
    /// into staging, within the profile's running cap, and is deleted when it ends (or by
    /// the sweeper). Each outcome is counted (`cancel_counts`) and logged.
    async fn cancel(&self, guid: &str, context: &str) {
        if context.is_empty() || context == self.human_context {
            return;
        }
        let send = || {
            self.conn.send(
                "Browser.cancelDownload",
                json!({ "guid": guid, "browserContextId": context }),
                None,
            )
        };
        let first = match send().await {
            Ok(_) => {
                self.state.lock().unwrap().cancels[0] += 1;
                return;
            }
            Err(e) => e,
        };
        self.state.lock().unwrap().cancels[1] += 1;
        eprintln!(
            "pegboard: could not cancel a download (it runs on into staging and is deleted \
             when it ends): {first:#}"
        );
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
                    if state.downloads.contains_key(&guid) || state.settled.contains(&guid) {
                        false
                    } else {
                        state.downloads.insert(
                            guid.clone(),
                            Download {
                                name: safe_name(&text("suggestedFilename")),
                                // Kept only in the form the action log takes.
                                url: url_for_log(&text("url")),
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

    /// Why the running download `guid` must be dropped, judged with every other running
    /// download (`judge_running`). The caller holds the state.
    fn over_limit(&self, state: &State, guid: &str) -> Option<String> {
        let d = state.downloads.get(guid)?;
        let running = |d: &Download| Running {
            session: d.session.clone(),
            context: d.context.clone(),
            bytes: d.received.max(d.total),
        };
        let others: Vec<Running> = state
            .downloads
            .iter()
            .filter(|(g, o)| g.as_str() != guid && o.end.is_none() && o.dropped.is_none())
            .map(|(_, o)| running(o))
            .collect();
        let used = d
            .session
            .as_ref()
            .and_then(|s| state.used.get(s).copied())
            .unwrap_or(0);
        judge_running(&self.limits, &running(d), &others, used)
    }

    fn on_progress(self: &Arc<Self>, guid: &str, received: u64, total: u64, end: Option<End>) {
        let (cancel, settle) = {
            let mut state = self.state.lock().unwrap();
            let Some(d) = state.downloads.get_mut(guid) else {
                return;
            };
            if d.end.is_some() {
                return;
            }
            d.received = received;
            d.total = total;
            d.end = end;
            let judged = end.is_none() && d.resolved && d.dropped.is_none();
            let why = if judged {
                self.over_limit(&state, guid)
            } else {
                None
            };
            let d = state.downloads.get_mut(guid).unwrap();
            let mut cancel = None;
            if let Some(why) = why {
                d.dropped = Some(why);
                cancel = d.context.clone();
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

    /// Finds the download's session and decides whether it may be kept. A download that is
    /// dropped while it runs is cancelled at once, unattributed ones too (in the profile
    /// their tab or their staged file is in); the human's are never touched.
    async fn judge(self: &Arc<Self>, guid: &str, frame: &str) {
        let mut session = None;
        // A download of the human's own tab is not looked for any further.
        let human = self.in_human_tab(frame).await;
        let tries = if human { 0 } else { RESOLVE_TRIES };
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
        let frame_context = match (&session, human) {
            (None, false) => self.agent_context_of(frame).await,
            _ => None,
        };
        let (cancel, locate, settle) = {
            let mut state = self.state.lock().unwrap();
            let ready = view
                .as_ref()
                .is_some_and(|v| state.policies.get(&v.context).is_some_and(|p| p.ready));
            let Some(d) = state.downloads.get_mut(guid) else {
                return;
            };
            let why = match (&session, &view) {
                (None, _) => Some(NOT_A_SESSION_TAB.to_string()),
                (Some(_), None) => Some(SESSION_CLOSED.to_string()),
                (Some(_), Some(v)) if !v.allows_downloads => Some(DENIED.to_string()),
                // Its profile is without its policy right now: the file may not land in
                // staging at all.
                (Some(_), Some(_)) if !ready => Some(POLICY_LOST.to_string()),
                (Some(_), Some(_)) => None,
            };
            d.resolved = true;
            d.session = session;
            d.context = view.map(|v| v.context).or(frame_context);
            if d.dropped.is_none() {
                d.dropped = why;
            }
            if d.dropped.is_none() && d.end.is_none() {
                if let Some(why) = self.over_limit(&state, guid) {
                    state.downloads.get_mut(guid).unwrap().dropped = Some(why);
                }
            }
            let d = &state.downloads[guid];
            let running_dropped = d.dropped.is_some() && d.end.is_none();
            let cancel = d.context.clone().filter(|_| running_dropped);
            let locate = running_dropped && d.context.is_none() && !human;
            (cancel, locate, d.end.is_some())
        };
        if let Some(context) = cancel {
            self.cancel(guid, &context).await;
        } else if locate {
            self.locate_and_cancel(guid).await;
        }
        if settle {
            self.settle(guid);
        }
    }

    /// The agent profile of a tab or frame target (`None` for the human's or an unknown one).
    async fn agent_context_of(&self, target: &str) -> Option<String> {
        let r = self
            .conn
            .send("Target.getTargetInfo", json!({ "targetId": target }), None)
            .await
            .ok()?;
        let context = r["targetInfo"]["browserContextId"].as_str()?;
        (!context.is_empty() && context != self.human_context).then(|| context.to_string())
    }

    /// A dropped download whose profile is not known yet (its frame is no target): its
    /// profile is the one whose staging directory its file appears in.
    async fn locate_and_cancel(&self, guid: &str) {
        for _ in 0..LOCATE_TRIES {
            let (dirs, running) = {
                let state = self.state.lock().unwrap();
                let running = state.downloads.get(guid).is_some_and(|d| d.end.is_none());
                let dirs: Vec<(String, PathBuf)> = state
                    .policies
                    .iter()
                    .map(|(c, p)| (c.clone(), p.staging.clone()))
                    .collect();
                (dirs, running)
            };
            if !running {
                return;
            }
            if let Some((context, _)) = dirs.iter().find(|(_, dir)| dir.join(guid).exists()) {
                if let Some(d) = self.state.lock().unwrap().downloads.get_mut(guid) {
                    d.context = Some(context.clone());
                }
                self.cancel(guid, context).await;
                return;
            }
            tokio::time::sleep(LOCATE_PAUSE).await;
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

    /// A download that is decided and has ended: its file is moved into its session's files
    /// directory or deleted, and its session (if it is still open) gets a report. Files are
    /// touched outside the state lock; the session's bytes are reserved under it first, and
    /// a session that ended while its file was moved gets the file deleted again.
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
        let (d, dirs, open) = {
            let mut state = self.state.lock().unwrap();
            let Some(d) = state.downloads.remove(guid) else {
                return;
            };
            if state.settled_order.len() >= MAX_SETTLED {
                if let Some(old) = state.settled_order.pop_front() {
                    state.settled.remove(&old);
                }
            }
            state.settled.insert(guid.to_string());
            state.settled_order.push_back(guid.to_string());
            let dirs: Vec<PathBuf> = state.policies.values().map(|p| p.staging.clone()).collect();
            let open = session
                .as_ref()
                .filter(|s| view.is_some() && !state.gone.contains(*s))
                .cloned();
            (d, dirs, open)
        };
        let staged = dirs
            .iter()
            .map(|dir| dir.join(guid))
            .find(|path| std::fs::symlink_metadata(path).is_ok());
        let delete_staged = || {
            if let Some(path) = &staged {
                let _ = std::fs::remove_file(path);
            }
        };
        let keep = matches!(
            (d.end, &d.dropped, &open, &view),
            (Some(End::Completed), None, Some(_), Some(v)) if v.allows_downloads
        );
        let mut name = d.name.clone();
        let outcome = match (keep, &open, &staged) {
            (true, Some(s), Some(path)) => {
                let size = std::fs::symlink_metadata(path)
                    .map(|m| m.len())
                    .unwrap_or(0);
                // Reserved under the lock, so two downloads of one session cannot both fit.
                let reserved = {
                    let mut state = self.state.lock().unwrap();
                    if state.gone.contains(s) {
                        Err(None)
                    } else {
                        let used = state.used.get(s).copied().unwrap_or(0);
                        match self.limits.judge(size, used) {
                            Some(why) => Err(Some(why)),
                            None => {
                                *state.used.entry(s.clone()).or_default() += size;
                                Ok(())
                            }
                        }
                    }
                };
                match reserved {
                    Err(why) => {
                        delete_staged();
                        why.map(Outcome::Dropped)
                    }
                    Ok(()) => match ensure_session_files(&self.paths, s)
                        .and_then(|dir| move_into(path, &dir, &d.name).map(|r| (dir, r)))
                    {
                        Ok((dir, (saved, bytes))) => {
                            let gone = self.state.lock().unwrap().gone.contains(s);
                            if gone {
                                // Closed while the file moved: its files go with it.
                                let _ = std::fs::remove_file(dir.join(&saved));
                                let _ = std::fs::remove_dir(&dir);
                                if let Some(parent) = dir.parent() {
                                    let _ = std::fs::remove_dir(parent);
                                }
                                None
                            } else {
                                name = saved;
                                Some(Outcome::Saved { bytes })
                            }
                        }
                        Err(e) => {
                            eprintln!("pegboard: saving a download failed: {e}");
                            delete_staged();
                            if let Some(n) = self.state.lock().unwrap().used.get_mut(s) {
                                *n = n.saturating_sub(size);
                            }
                            Some(Outcome::Dropped(NOT_SAVED.to_string()))
                        }
                    },
                }
            }
            (true, _, _) => Some(Outcome::Dropped(NOT_SAVED.to_string())),
            (false, _, _) => {
                delete_staged();
                Some(Outcome::Dropped(d.dropped.clone().unwrap_or_else(
                    || match (d.end, &view) {
                        (Some(End::Canceled), _) => CANCELED.to_string(),
                        (_, Some(v)) if !v.allows_downloads => DENIED.to_string(),
                        _ => NOT_A_SESSION_TAB.to_string(),
                    },
                )))
            }
        };
        if let (Some(s), Some(outcome)) = (open, outcome) {
            let mut state = self.state.lock().unwrap();
            if !state.gone.contains(&s) {
                state.reports.entry(s).or_default().push(Report {
                    name,
                    url: d.url,
                    outcome,
                    ended_at: now(),
                });
            }
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

/// Sets a context's policy (its download policy, and its clipboard denial when its sessions
/// deny the clipboard) again after its connection ended, until it is in place. When
/// that takes longer than `REAPPLY_LIMIT` it tells `PolicyLost` (the daemon closes the
/// profile's sessions) and stops: no window is brought forward again and again. The
/// profile stays not ready; the next session opened in it sets the policy anew (`apply`).
/// While the test hold lasts, every attempt counts as failed.
async fn reapply(weak: Weak<Inner>, context: String) {
    let started = tokio::time::Instant::now();
    loop {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let profile_dir = {
            let mut state = inner.state.lock().unwrap();
            match state.policies.get_mut(&context) {
                Some(p) if !(p.ready && p.clipboard_ready()) => p.profile_dir.clone(),
                Some(p) => {
                    p.reapplying = false;
                    return;
                }
                None => return,
            }
        };
        let held = {
            let mut hold = inner.hold_until.lock().unwrap();
            match *hold {
                Some(until) if tokio::time::Instant::now() < until => true,
                Some(_) => {
                    *hold = None;
                    false
                }
                None => false,
            }
        };
        let result = if held {
            Err(anyhow!("held by a test"))
        } else {
            let _one = inner.applying.lock().await;
            let mut result = Ok(());
            if !inner.is_ready(&context) {
                result = inner.establish(&context, &profile_dir, AGAIN_TRIES).await;
            }
            if result.is_ok() && !inner.clipboard_ready(&context) {
                result = inner
                    .deny_clipboard(&context, AGAIN_TRIES)
                    .await
                    .context("denying the clipboard");
            }
            result
        };
        match result {
            Ok(()) => {
                eprintln!("pegboard: the download policy (and clipboard denial) is set again");
                // The top of the loop clears `reapplying` (so that a later loss starts a new
                // attempt) if nothing was lost meanwhile, and tries again if something was.
                continue;
            }
            Err(e) if !held => {
                eprintln!("pegboard: setting the download policy (or clipboard denial) again failed: {e:#}")
            }
            Err(_) => {}
        }
        if started.elapsed() >= REAPPLY_LIMIT {
            if let Some(p) = inner.state.lock().unwrap().policies.get_mut(&context) {
                p.reapplying = false;
            }
            eprintln!("pegboard: gave up setting the download policy (or clipboard denial) again");
            let lost = inner.lost.lock().unwrap().clone();
            if let Some(lost) = lost {
                lost(&context);
            }
            return;
        }
        drop(inner);
        tokio::time::sleep(RETRY_PAUSE).await;
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
    fn a_profile_first_seen_may_carry_a_clipboard_denial() {
        let p = Policy::new("Agent", PathBuf::from("/x"));
        assert!(
            p.clipboard_maybe_denied,
            "an allowed session must reset it first"
        );
        assert!(!p.clipboard_denied && p.clipboard_ready());
    }

    #[test]
    fn file_names_are_made_safe() {
        assert_eq!(safe_name("../../x.sh"), "x.sh");
        assert_eq!(safe_name("a/b\\c.txt"), "c.txt");
        for empty in ["", ".", "..", "a/", "a/..", "\n\0"] {
            assert_eq!(safe_name(empty), "download", "{empty:?}");
        }
        assert_eq!(safe_name("re\nport\0.pdf"), "report.pdf");
        assert_eq!(safe_name("한글-이름.txt"), "한글-이름.txt");
        // Invisible format characters (bidi overrides, zero-width) and a leading '-' or '.'.
        assert_eq!(safe_name("evil\u{202e}gpj.exe"), "evilgpj.exe");
        assert_eq!(safe_name("a\u{200b}b\u{feff}.txt"), "ab.txt");
        assert_eq!(safe_name("--rf.txt"), "rf.txt");
        assert_eq!(safe_name(".hidden"), "hidden");
        assert_eq!(safe_name("-"), "download");
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
            profile: MAX_PROFILE_RUNNING_BYTES,
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
                session: 4096,
                profile: MAX_PROFILE_RUNNING_BYTES,
            }
        );
        assert_eq!(
            Limits::parse(Some("1024,4096,8192")),
            Limits {
                file: 1024,
                session: 4096,
                profile: 8192,
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
            "1,2,3,4",
            "1,2,0",
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

    #[test]
    fn running_downloads_count_together() {
        let l = Limits {
            file: 100,
            session: 250,
            profile: 260,
        };
        let (a, b) = (
            SessionId("s_aaaaaaaa".into()),
            SessionId("s_bbbbbbbb".into()),
        );
        let run = |s: Option<&SessionId>, ctx: &str, bytes: u64| Running {
            session: s.cloned(),
            context: Some(ctx.to_string()),
            bytes,
        };
        let me = run(Some(&a), "P", 90);
        // Alone: fine; over the file limit: dropped.
        assert_eq!(judge_running(&l, &me, &[], 0), None);
        assert_eq!(
            judge_running(&l, &run(Some(&a), "P", 101), &[], 0).as_deref(),
            Some("file larger than 100 bytes")
        );
        // Parallel downloads of one session count together with what it saved.
        let others = [run(Some(&a), "P", 90), run(Some(&b), "Q", 99)];
        assert_eq!(judge_running(&l, &me, &others, 70), None);
        assert_eq!(
            judge_running(&l, &me, &others, 71).as_deref(),
            Some("session download limit of 250 bytes reached")
        );
        // A profile's running downloads together, whoever's (unattributed ones too).
        let crowd = [
            run(Some(&b), "P", 90),
            run(None, "P", 90),
            run(Some(&b), "Q", 99),
        ];
        assert_eq!(
            judge_running(&l, &me, &crowd, 0).as_deref(),
            Some("downloads running in the profile exceed 260 bytes")
        );
        // The profile's cap is its own value, above the session limit.
        assert!(judge_running(&l, &run(Some(&a), "P", 75), &crowd, 0).is_none());
        let unattributed = run(None, "P", 10);
        assert!(judge_running(&l, &unattributed, &crowd, 0).is_none());
    }
}
