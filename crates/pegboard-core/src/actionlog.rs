//! The action log of a session: one JSON object per line in
//! `<recordings>/<recording id>/actions.jsonl`, next to a `manifest.json`.
//!
//! What is written is the command line, never what a command returned. Typed text is kept
//! only when it provably went into a harmless field (see `daemon::run_command`); secret
//! values (`cookies set`, `storage set`) and the strings inside `eval` expressions are
//! never kept. The
//! session endpoint's token is not a field of anything here.
//!
//! Format 2 (phase 2b) adds, all optional so a format-1 recording reads as before: the
//! kind of an entry (a command, or something a person did in the browser), the frames
//! taken around a command, and the manifest's `keep`, `screen`, `record` and `sizes`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pegboard_protocol::{ConfirmBy, ConfirmOutcome, RecordExtra, SessionId};
use serde::{Deserialize, Serialize};

use crate::argv;
use crate::paths::Paths;
use crate::recording::format::jsonl_lines;

pub const REDACTED: &str = "[redacted]";

/// Longest `eval` expression (in characters) kept in the log.
pub const MAX_EVAL_CHARS: usize = 200;

/// Longest error text (in characters) kept in the log.
pub const MAX_ERROR_CHARS: usize = 200;

/// Longest line (with its newline) `ActionLog::append` writes.
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// Format of the recordings this build writes. Format 1 (no screen, no entry kinds) is
/// still read.
pub const MANIFEST_FORMAT: u32 = 2;

/// A session's action log. Holding the lock is what serializes the session's commands.
pub type SessionLog = Arc<tokio::sync::Mutex<ActionLog>>;

/// Who did what an entry records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    /// A command of the session (every entry of a format-1 recording).
    #[default]
    Command,
    /// Something a person did in one of the session's tabs.
    Human,
}

impl EntryKind {
    fn is_command(&self) -> bool {
        *self == EntryKind::Command
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionEntry {
    /// Assigned by `ActionLog::append`, from 1.
    pub seq: u64,
    pub ts: String,
    pub session_id: String,
    pub target_id: Option<String>,
    pub url: Option<String>,
    pub command: String,
    pub args: Vec<String>,
    pub ok: bool,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub redacted: bool,
    #[serde(default, skip_serializing_if = "EntryKind::is_command")]
    pub kind: EntryKind,
    /// The screen just before and just after the command: `<targetId>/<file>`, relative to
    /// the recording's `screen/` directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_after: Option<String>,
    /// What a person did (`EntryKind::Human` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub what: Option<String>,
    /// Whether it was certainly a person (`EntryKind::Human` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certain: Option<bool>,
    /// How a person's confirmation of the command ended; none for a command that asked
    /// nobody. What the confirmation showed is not kept (it can hold secrets). An entry
    /// with one has `ts` at the answer and `duration_ms` from then: the wait is `wait_ms`,
    /// not part of the command's time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<ConfirmLog>,
}

/// The confirmation of one command, as the log keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmLog {
    pub outcome: ConfirmOutcome,
    pub by: Option<ConfirmBy>,
    pub wait_ms: u64,
}

impl ActionEntry {
    /// An entry for something a person did in tab `target_id`: command `human`, no
    /// arguments, `ok`. The URL is cleaned like every logged URL (`url_for_log`).
    pub fn human(
        session_id: &str,
        ts: &str,
        target_id: &str,
        url: Option<String>,
        what: &str,
        certain: bool,
    ) -> ActionEntry {
        ActionEntry {
            ts: ts.to_string(),
            session_id: session_id.to_string(),
            target_id: Some(target_id.to_string()),
            url: url.as_deref().map(url_for_log),
            command: "human".to_string(),
            ok: true,
            kind: EntryKind::Human,
            what: Some(what.to_string()),
            certain: Some(certain),
            ..ActionEntry::default()
        }
    }
}

/// What became of a recording's screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScreenStatus {
    /// No screen was recorded (every format-1 recording).
    #[default]
    None,
    Recorded,
    /// Recording the screen stopped at the per-recording limit; the actions went on.
    Capped,
    /// The screen was deleted to keep within the total limit.
    Pruned,
}

/// Bytes on disk, counted when the recording ends and after a prune.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sizes {
    pub screen_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub format: u32,
    pub recording_id: String,
    pub session_id: String,
    pub session_name: String,
    pub profile: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub chrome_version: String,
    pub agent_browser_version: String,
    /// Kept by a person: never deleted by the retention rules, not counted in their limits.
    #[serde(default)]
    pub keep: bool,
    #[serde(default)]
    pub screen: ScreenStatus,
    /// What is recorded besides actions, navigation and screen.
    #[serde(default)]
    pub record: Vec<RecordExtra>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sizes: Option<Sizes>,
}

impl Manifest {
    pub fn new(
        recording_id: &str,
        session_id: &str,
        session_name: &str,
        profile: &str,
        started_at: &str,
        chrome_version: &str,
        agent_browser_version: &str,
    ) -> Manifest {
        Manifest {
            format: MANIFEST_FORMAT,
            recording_id: recording_id.to_string(),
            session_id: session_id.to_string(),
            session_name: session_name.to_string(),
            profile: profile.to_string(),
            started_at: started_at.to_string(),
            ended_at: None,
            chrome_version: chrome_version.to_string(),
            agent_browser_version: agent_browser_version.to_string(),
            keep: false,
            screen: ScreenStatus::None,
            record: Vec::new(),
            sizes: None,
        }
    }
}

/// `r_` and the body of a session id. Checked before a recording id is used in a path.
pub fn valid_recording_id(id: &str) -> bool {
    id.strip_prefix("r_")
        .is_some_and(|body| SessionId::parse(&format!("s_{body}")).is_some())
}

fn invalid_id(id: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("{id:?} is not a recording id"),
    )
}

/// `<recordings>/<recording id>`, once the id is checked.
pub fn recording_dir(paths: &Paths, recording_id: &str) -> std::io::Result<PathBuf> {
    if !valid_recording_id(recording_id) {
        return Err(invalid_id(recording_id));
    }
    Ok(paths.recordings.join(recording_id))
}

/// The manifest of an open recording, shared by its writers; every update rewrites
/// manifest.json atomically (tmp + rename, 0600).
#[derive(Clone)]
pub struct ManifestHandle {
    dir: PathBuf,
    manifest: Arc<Mutex<Manifest>>,
}

impl ManifestHandle {
    pub fn get(&self) -> Manifest {
        self.lock().clone()
    }

    /// Changes the manifest and writes it. When it cannot be written, neither the file nor
    /// the shared copy changes. The lock is held while writing, so updates never cross.
    pub fn update(&self, f: impl FnOnce(&mut Manifest)) -> std::io::Result<()> {
        let mut shared = self.lock();
        let mut next = shared.clone();
        f(&mut next);
        write_manifest_file(&self.dir, &next)?;
        *shared = next;
        Ok(())
    }

    /// The recording's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Manifest> {
        // A writer that panicked mid-update left the previous manifest in place.
        self.manifest.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct ActionLog {
    manifest: ManifestHandle,
    actions: File,
    next_seq: u64,
}

impl ActionLog {
    /// Creates `<recordings>/<recording id>/` (mode 0700) with `manifest.json` and an empty
    /// `actions.jsonl` (mode 0600). The recording must not exist yet.
    pub fn open(paths: &Paths, manifest: Manifest) -> std::io::Result<ActionLog> {
        let dir = recording_dir(paths, &manifest.recording_id)?;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&paths.recordings)?;
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let actions = OpenOptions::new()
            .create_new(true)
            .append(true)
            .mode(0o600)
            .open(dir.join("actions.jsonl"))?;
        write_manifest_file(&dir, &manifest)?;
        Ok(ActionLog {
            manifest: ManifestHandle {
                dir,
                manifest: Arc::new(Mutex::new(manifest)),
            },
            actions,
            next_seq: 1,
        })
    }

    /// Writes the entry as one line, numbering it (`seq` is overwritten), and flushes.
    pub fn append(&mut self, mut entry: ActionEntry) -> std::io::Result<()> {
        entry.seq = self.next_seq;
        let line = capped_line(&mut entry)?;
        self.actions.write_all(&line)?;
        self.actions.flush()?;
        self.next_seq += 1;
        Ok(())
    }

    /// Rewrites `manifest.json` with the end time.
    pub fn finish(&mut self, ended_at: &str) -> std::io::Result<()> {
        self.manifest
            .update(|m| m.ended_at = Some(ended_at.to_string()))
    }

    /// The manifest as it is now (other writers may change it through their handles).
    pub fn manifest(&self) -> Manifest {
        self.manifest.get()
    }

    /// A handle on the same manifest, for the recording's other writers.
    pub fn manifest_handle(&self) -> ManifestHandle {
        self.manifest.clone()
    }

    /// The recording's directory.
    pub fn dir(&self) -> &Path {
        self.manifest.dir()
    }
}

/// Replaces `<dir>/manifest.json` whole: written to `manifest.json.tmp` (a leftover one is
/// removed first, so the new file is always created 0600 and never through a link), then
/// renamed over it.
pub fn write_manifest_file(dir: &Path, m: &Manifest) -> std::io::Result<()> {
    let tmp = dir.join("manifest.json.tmp");
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let written = (|| {
        let bytes = serde_json::to_vec_pretty(m).map_err(std::io::Error::other)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&bytes)?;
        // On disk before it replaces the old one, so a crash leaves one whole manifest.
        file.sync_all()?;
        std::fs::rename(&tmp, dir.join("manifest.json"))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written?;
    // The rename itself.
    File::open(dir)?.sync_all()
}

/// The entry as one line of at most `MAX_LINE_BYTES`. Words are redacted before they get
/// here; when the line is still too long the longest word (an argument, the command or the
/// URL) is replaced by `[<n> bytes]` until it fits. A redaction marker and a word no longer
/// than its replacement are left alone, so the cap never puts content where a marker was.
fn capped_line(entry: &mut ActionEntry) -> std::io::Result<Vec<u8>> {
    loop {
        let mut line = serde_json::to_vec(&*entry).map_err(std::io::Error::other)?;
        line.push(b'\n');
        if line.len() <= MAX_LINE_BYTES {
            return Ok(line);
        }
        let mut words: Vec<&mut String> = entry.args.iter_mut().collect();
        words.push(&mut entry.command);
        if let Some(url) = entry.url.as_mut() {
            words.push(url);
        }
        if let Some(what) = entry.what.as_mut() {
            words.push(what);
        }
        let longest = words
            .into_iter()
            .filter(|w| w.as_str() != REDACTED)
            .max_by_key(|w| w.len());
        match longest {
            Some(word) if word.len() > format!("[{} bytes]", word.len()).len() => {
                *word = format!("[{} bytes]", word.len());
            }
            _ => {
                return Err(std::io::Error::other(
                    "an action entry cannot be cut to the line limit",
                ))
            }
        }
    }
}

/// `<dir>/manifest.json`, of any format.
pub fn read_manifest(dir: &Path) -> std::io::Result<Manifest> {
    let text = std::fs::read_to_string(dir.join("manifest.json"))?;
    serde_json::from_str(&text).map_err(std::io::Error::other)
}

pub fn read(paths: &Paths, recording_id: &str) -> std::io::Result<(Manifest, Vec<ActionEntry>)> {
    let dir = recording_dir(paths, recording_id)?;
    let manifest = read_manifest(&dir)?;
    // A line cut off by a crash, or still being written, is skipped like any bad line.
    let entries = jsonl_lines(File::open(dir.join("actions.jsonl"))?)?;
    Ok((manifest, entries))
}

/// The manifests of all recordings, oldest first. Directories that are not recordings are
/// skipped.
pub fn list(paths: &Paths) -> std::io::Result<Vec<Manifest>> {
    let entries = match std::fs::read_dir(&paths.recordings) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut manifests = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(String::from) else {
            continue;
        };
        if !valid_recording_id(&name) {
            continue;
        }
        if let Ok(m) = read_manifest(&entry.path()) {
            manifests.push(m);
        }
    }
    manifests
        .sort_by(|a, b| (&a.started_at, &a.recording_id).cmp(&(&b.started_at, &b.recording_id)));
    Ok(manifests)
}

/// Which argv positions hold text the log may not keep as it is (typed text and secret
/// values; `argv::text_at` tells them apart), if any.
pub fn text_positions(argv: &[String]) -> Vec<usize> {
    argv::text_positions(argv)
}

fn mask(argv: &[String], positions: &[usize]) -> Vec<String> {
    argv.iter()
        .enumerate()
        .map(|(i, w)| {
            if positions.contains(&i) {
                REDACTED.to_string()
            } else {
                w.clone()
            }
        })
        .collect()
}

/// The command line with all those positions replaced by `REDACTED`.
pub fn redact(argv: &[String]) -> Vec<String> {
    mask(argv, &text_positions(argv))
}

/// `autocomplete` tokens of fields that hold secrets.
pub const SENSITIVE_AUTOCOMPLETE: &[&str] = &[
    "current-password",
    "new-password",
    "one-time-code",
    "cc-number",
    "cc-csc",
    "cc-exp",
    "cc-exp-month",
    "cc-exp-year",
];

/// Whether an element with this `type` and `autocomplete` attribute holds secrets (the
/// same rule as `SENSITIVE_FOCUS_JS`, for an element found by a selector).
pub fn attrs_are_sensitive(input_type: Option<&str>, autocomplete: Option<&str>) -> bool {
    input_type.is_some_and(|t| t.trim().eq_ignore_ascii_case("password"))
        || autocomplete.is_some_and(|a| {
            a.split_ascii_whitespace()
                .any(|tok| SENSITIVE_AUTOCOMPLETE.contains(&tok.to_ascii_lowercase().as_str()))
        })
}

/// A JS expression evaluated in a tab; true when its focused element is sensitive: an
/// `input[type=password]`, or an element whose `autocomplete` has one of
/// `SENSITIVE_AUTOCOMPLETE`. It follows the active element into open shadow roots and
/// same-origin iframes. What it cannot look into (a cross-origin iframe has focus, or the
/// nesting is implausibly deep) counts as sensitive.
pub const SENSITIVE_FOCUS_JS: &str = r#"(() => {
  const sensitive = ['current-password', 'new-password', 'one-time-code', 'cc-number', 'cc-csc', 'cc-exp', 'cc-exp-month', 'cc-exp-year'];
  let el = document.activeElement;
  for (let depth = 0; el; depth++) {
    if (depth > 16) return true;
    if (el.shadowRoot && el.shadowRoot.activeElement) { el = el.shadowRoot.activeElement; continue; }
    if (el.tagName === 'IFRAME' || el.tagName === 'FRAME') {
      let inner = null;
      try { inner = el.contentDocument; } catch (e) { inner = null; }
      if (!inner) return true;
      if (inner.activeElement) { el = inner.activeElement; continue; }
      return false;
    }
    break;
  }
  if (!el) return false;
  if (el.tagName === 'INPUT' && String(el.type).toLowerCase() === 'password') return true;
  const tokens = (el.getAttribute && el.getAttribute('autocomplete') || '').toLowerCase().split(/\s+/);
  return tokens.some((t) => sensitive.includes(t));
})()"#;

/// The expression with the contents of its string literals (single, double and backtick
/// quoted, backslash escapes honoured) replaced by '…'. Fail closed: `None` when a literal
/// is not closed, when a `/` occurs outside a literal (a comment or a regular expression
/// literal can hide a quote from this scan; a division is collateral), or when a template
/// literal has a `${` substitution (it may nest literals).
pub fn mask_eval(expression: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = expression.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' {
            return None;
        }
        out.push(c);
        if !matches!(c, '\'' | '"' | '`') {
            continue;
        }
        out.push('…');
        let mut closed = false;
        while let Some(d) = chars.next() {
            if d == '\\' {
                chars.next();
            } else if d == c {
                out.push(c);
                closed = true;
                break;
            } else if c == '`' && d == '$' && chars.peek() == Some(&'{') {
                return None;
            }
        }
        if !closed {
            return None;
        }
    }
    Some(out)
}

/// An expression as logged: its literals masked and the rest cut to `MAX_EVAL_CHARS`, or
/// only its size when it cannot be masked reliably.
fn masked_expression(expression: &str) -> String {
    match mask_eval(expression) {
        Some(masked) => truncate_chars(&masked, MAX_EVAL_CHARS),
        None => format!("[eval {} chars]", expression.chars().count()),
    }
}

/// A URL as logged: without userinfo, query and fragment; a `data:` URL only by its scheme.
/// `None` when it does not parse.
pub fn sanitize_url(url: &str) -> Option<String> {
    if url.trim_start().to_ascii_lowercase().starts_with("data:") {
        return Some("data:".to_string());
    }
    let mut parsed = url::Url::parse(url.trim()).ok()?;
    if parsed.cannot_be_a_base() {
        // about:blank and the like carry no userinfo, but may carry a query.
        parsed.set_query(None);
        parsed.set_fragment(None);
        return Some(parsed.to_string());
    }
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    parsed.set_query(None);
    parsed.set_fragment(None);
    Some(parsed.to_string())
}

/// `sanitize_url`, or `REDACTED` for a string in a URL position that does not parse.
pub fn url_for_log(url: &str) -> String {
    sanitize_url(url).unwrap_or_else(|| REDACTED.to_string())
}

/// The command line as written to the log: the caller's words with secret values replaced
/// (and typed text too when `redacted`), URLs without userinfo, query and fragment, and the
/// expression of `eval` (its words joined) or `wait --fn` with string literals masked and
/// cut to `MAX_EVAL_CHARS` (see `mask_eval`).
pub fn logged_args(argv: &[String], redacted: bool) -> Vec<String> {
    let at = argv::text_at(argv);
    let positions = if redacted {
        text_positions(argv)
    } else {
        at.always.clone()
    };
    let mut args = mask(argv, &positions);
    if args.first().map(String::as_str) == Some("eval") && args.len() > 1 {
        return vec!["eval".to_string(), masked_expression(&args[1..].join(" "))];
    }
    for &i in &at.urls {
        if args[i] != REDACTED {
            args[i] = url_for_log(&args[i]);
        }
    }
    for &i in &at.expr {
        args[i] = masked_expression(&args[i]);
    }
    args
}

/// A command line the grammar refused cannot be read for secrets (it may be anything,
/// anywhere): only its first word is kept, and only when it is a command word of the
/// grammar. Any other first word may be a secret (a session handle typed in the command
/// position, say) and is replaced like the rest.
pub fn rejected_args(argv: &[String]) -> Vec<String> {
    argv.iter()
        .enumerate()
        .map(|(i, w)| {
            if i == 0 && argv::is_command_word(w) {
                w.clone()
            } else {
                REDACTED.to_string()
            }
        })
        .collect()
}

/// The `command` field of a refused command line's entry: the same first-word rule.
pub fn rejected_command(argv: &[String]) -> String {
    match argv.first() {
        Some(w) if argv::is_command_word(w) => w.clone(),
        Some(_) => REDACTED.to_string(),
        None => String::new(),
    }
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    fn s(v: Vec<&str>) -> Vec<String> {
        v.into_iter().map(String::from).collect()
    }

    fn entry(command: &str, args: Vec<String>) -> ActionEntry {
        ActionEntry {
            seq: 0,
            ts: "2026-01-01T00:00:00Z".into(),
            session_id: "s_abcdefgh".into(),
            target_id: None,
            url: None,
            command: command.into(),
            args,
            ok: true,
            error: None,
            duration_ms: 3,
            redacted: false,
            ..ActionEntry::default()
        }
    }

    fn manifest(rec: &str) -> Manifest {
        Manifest::new(
            rec,
            "s_abcdefgh",
            "demo",
            "default",
            "2026-01-01T00:00:00Z",
            "157",
            "0.42.0",
        )
    }

    #[test]
    fn text_positions_cover_typing_commands() {
        assert_eq!(text_positions(&s(vec!["fill", "#pw", "secret"])), vec![2]);
        assert_eq!(
            text_positions(&s(vec!["type", "@e3", "--clear", "--", "secret"])),
            vec![4]
        );
        assert_eq!(
            text_positions(&s(vec!["keyboard", "type", "secret"])),
            vec![2]
        );
        assert_eq!(
            text_positions(&s(vec!["keyboard", "inserttext", "secret"])),
            vec![2]
        );
        assert_eq!(
            text_positions(&s(vec!["find", "label", "Password", "fill", "secret"])),
            vec![4]
        );
        assert_eq!(
            text_positions(&s(vec!["find", "nth", "2", "input", "fill", "a", "b"])),
            vec![5, 6]
        );
        assert_eq!(text_positions(&s(vec!["select", "#cc", "12"])), vec![2]);
        assert_eq!(
            text_positions(&s(vec!["dialog", "accept", "secret"])),
            vec![2]
        );
        assert!(text_positions(&s(vec!["click", "#pw"])).is_empty());
        assert!(text_positions(&s(vec!["find", "label", "Password", "click"])).is_empty());
        assert!(text_positions(&s(vec!["fill", "#pw"])).is_empty());
    }

    #[test]
    fn redact_replaces_only_the_text() {
        assert_eq!(
            redact(&s(vec!["fill", "#pw", "secret"])),
            s(vec!["fill", "#pw", REDACTED])
        );
        assert_eq!(
            redact(&s(vec!["type", "#pw", "--", "-secret"])),
            s(vec!["type", "#pw", "--", REDACTED])
        );
        assert_eq!(redact(&s(vec!["click", "#a"])), s(vec!["click", "#a"]));
    }

    #[test]
    fn eval_expression_is_truncated() {
        let long = "x".repeat(1000);
        let args = logged_args(&s(vec!["eval", &long]), false);
        assert_eq!(args.len(), 2);
        assert_eq!(args[1].chars().count(), 200);
        let multi = logged_args(&s(vec!["eval", &long, "tail"]), false);
        assert_eq!(multi[1].chars().count(), 200);
        // Cut by characters, not bytes.
        let wide = "é".repeat(300);
        assert_eq!(
            logged_args(&s(vec!["eval", &wide]), false)[1]
                .chars()
                .count(),
            200
        );
    }

    #[test]
    fn refused_text_commands_are_not_logged_with_their_words() {
        assert_eq!(
            rejected_args(&s(vec!["fill", "#pw", "-secret"])),
            s(vec!["fill", REDACTED, REDACTED])
        );
        assert_eq!(
            rejected_args(&s(vec!["open", "file:///x"])),
            s(vec!["open", REDACTED])
        );
    }

    #[test]
    fn every_rejected_argv_is_fully_redacted() {
        for argv in [
            vec!["set", "credentials", "admin", "hunter2"],
            vec!["set", "headers", r#"{"Authorization":"Bearer s3cret"}"#],
            vec!["network", "route", "https://a.test", "--body", "s3cret"],
            vec!["click", "#a", "--bogus"],
        ] {
            let args = rejected_args(&s(argv.clone()));
            assert_eq!(args[0], argv[0]);
            assert_eq!(args.len(), argv.len());
            assert!(args[1..].iter().all(|a| a == REDACTED), "{args:?}");
        }
    }

    #[test]
    fn eval_mask_fails_closed_on_slash_and_template_substitution() {
        let n = |e: &str| format!("[eval {} chars]", e.chars().count());
        for e in [
            "// it's the pw box\ndocument.querySelector('#pw').value = 'hunter2' // don't log",
            "`${`hunter2`}`",
            "x.split(/'/); y = 'hunter2'; z = /'/",
            "a / b",
            "/* ' */ 'hunter2'",
        ] {
            assert_eq!(
                logged_args(&s(vec!["eval", e]), false),
                s(vec!["eval", &n(e)]),
                "{e}"
            );
        }
        assert_eq!(mask_eval("document.title").unwrap(), "document.title");
        assert_eq!(mask_eval("q('#a').value='x'").unwrap(), "q('…').value='…'");
        assert_eq!(mask_eval(r"`a\${b}`").unwrap(), "`…`");
    }

    #[test]
    fn wait_fn_expression_is_masked() {
        assert_eq!(
            logged_args(&s(vec!["wait", "--fn", "window.t === 'hunter2'"]), false),
            s(vec!["wait", "--fn", "window.t === '…'"])
        );
        assert_eq!(
            logged_args(&s(vec!["wait", "--fn", "a / 'hunter2'"]), false),
            s(vec!["wait", "--fn", "[eval 13 chars]"])
        );
        assert_eq!(
            logged_args(&s(vec!["wait", "--timeout", "10"]), false),
            s(vec!["wait", "--timeout", "10"])
        );
    }

    #[test]
    fn url_sanitizer_drops_userinfo_query_fragment_and_data_body() {
        assert_eq!(
            sanitize_url("https://admin:hunter2@a.test/p?t=abc#f").unwrap(),
            "https://a.test/p"
        );
        assert_eq!(
            sanitize_url("http://u@a.test:8080/x").unwrap(),
            "http://a.test:8080/x"
        );
        assert_eq!(sanitize_url("data:text/html,secret").unwrap(), "data:");
        assert_eq!(sanitize_url("about:blank").unwrap(), "about:blank");
        assert!(sanitize_url("not a url").is_none());
        assert_eq!(url_for_log("not a url"), REDACTED);
        assert_eq!(url_for_log("http://a.test/x?p=1"), "http://a.test/x");
    }

    #[test]
    fn url_arguments_are_sanitized_in_args() {
        assert_eq!(
            logged_args(
                &s(vec!["open", "https://admin:hunter2@a.test/p?t=abc#f"]),
                false
            ),
            s(vec!["open", "https://a.test/p"])
        );
        assert_eq!(
            logged_args(&s(vec!["tab", "new", "data:text/html,secret"]), false),
            s(vec!["tab", "new", "data:"])
        );
        assert_eq!(
            logged_args(
                &s(vec![
                    "cookies",
                    "set",
                    "n",
                    "v",
                    "--url",
                    "https://a.test/p?x=1"
                ]),
                false
            ),
            s(vec![
                "cookies",
                "set",
                "n",
                REDACTED,
                "--url",
                "https://a.test/p"
            ])
        );
    }

    #[test]
    fn recording_ids_are_validated() {
        assert!(valid_recording_id("r_abcdefgh"));
        for bad in [
            "",
            "r_",
            "s_abcdefgh",
            "r_../../etc",
            "r_ABCDEFGH",
            "r_abcdefg",
            "r_abcdefghi",
            "../r_abcdefgh",
        ] {
            assert!(!valid_recording_id(bad), "{bad}");
        }
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(tmp.path());
        assert!(read(&paths, "r_../../x").is_err());
        assert!(ActionLog::open(&paths, manifest("../evil")).is_err());
        assert!(!tmp.path().join("evil").exists());
    }

    #[test]
    fn log_roundtrip_and_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(tmp.path());
        let mut log = ActionLog::open(&paths, manifest("r_abcdefgh")).unwrap();
        log.append(entry("open", s(vec!["open", "http://a.test/"])))
            .unwrap();
        log.append(entry("click", s(vec!["click", "#b"]))).unwrap();
        // Visible to a reader before `finish`: each entry is flushed.
        assert_eq!(read(&paths, "r_abcdefgh").unwrap().1.len(), 2);
        log.finish("2026-01-01T00:05:00Z").unwrap();
        let (m, entries) = read(&paths, "r_abcdefgh").unwrap();
        assert_eq!(m.ended_at.as_deref(), Some("2026-01-01T00:05:00Z"));
        assert_eq!(m.format, MANIFEST_FORMAT);
        assert_eq!(
            entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2]
        );
        let dir = paths.recordings.join("r_abcdefgh");
        let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(dir.clone()), 0o700);
        assert_eq!(mode(dir.join("actions.jsonl")), 0o600);
        assert_eq!(mode(dir.join("manifest.json")), 0o600);
        let text = std::fs::read_to_string(dir.join("actions.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("\"sessionId\""));
        assert_eq!(list(&paths).unwrap().len(), 1);
        // A recording is never reopened (it would restart the numbering).
        assert!(ActionLog::open(&paths, manifest("r_abcdefgh")).is_err());
    }

    #[test]
    fn nothing_holds_the_endpoint_token() {
        // The token belongs to the proxy endpoint and is not part of any agent-browser
        // argv, so no entry can be built from it; and neither type has a field for it.
        let token = "tok-0123456789abcdef";
        let e = entry("open", s(vec!["open", "http://a.test/"]));
        assert!(!format!("{e:?}").contains(token));
        let keys = |v: serde_json::Value| {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        assert_eq!(
            keys(serde_json::to_value(&e).unwrap()),
            vec![
                "args",
                "command",
                "durationMs",
                "error",
                "ok",
                "redacted",
                "seq",
                "sessionId",
                "targetId",
                "ts",
                "url"
            ]
        );
        let m = manifest("r_abcdefgh");
        assert!(!format!("{m:?}").contains(token));
        assert_eq!(
            keys(serde_json::to_value(&m).unwrap()),
            vec![
                "agentBrowserVersion",
                "chromeVersion",
                "endedAt",
                "format",
                "keep",
                "profile",
                "record",
                "recordingId",
                "screen",
                "sessionId",
                "sessionName",
                "startedAt"
            ]
        );
    }

    #[test]
    fn eval_string_literals_are_masked() {
        let a = logged_args(
            &s(vec![
                "eval",
                "document.querySelector('#pw').value='hunter2'",
            ]),
            false,
        );
        assert_eq!(a[1], "document.querySelector('…').value='…'");
        assert!(!a[1].contains("hunter2") && !a[1].contains("#pw"));
        assert_eq!(mask_eval(r#"f("a b", `c`)"#).unwrap(), "f(\"…\", `…`)");
        // An escaped quote does not end the literal.
        assert_eq!(mask_eval(r"x = 'it\'s hunter2'; y").unwrap(), "x = '…'; y");
        // Masked first, then cut.
        let long = format!("{}'hunter2'", "x".repeat(300));
        let a = logged_args(&s(vec!["eval", &long]), false);
        assert_eq!(a[1].chars().count(), 200);
        assert!(!a[1].contains("hunter2"));
        // An unterminated literal: only the size is kept.
        let a = logged_args(&s(vec!["eval", "x = 'hunter2"]), false);
        assert_eq!(a, s(vec!["eval", "[eval 12 chars]"]));
    }

    #[test]
    fn cookie_and_storage_values_are_always_redacted() {
        let log = |v: Vec<&str>| logged_args(&s(v), false);
        assert_eq!(
            log(vec!["cookies", "set", "sid", "abc123"]),
            s(vec!["cookies", "set", "sid", REDACTED])
        );
        assert_eq!(
            log(vec![
                "cookies",
                "set",
                "sid",
                "abc123",
                "--same-site",
                "Lax",
                "--secure"
            ]),
            s(vec![
                "cookies",
                "set",
                "sid",
                REDACTED,
                "--same-site",
                "Lax",
                "--secure"
            ])
        );
        assert_eq!(
            log(vec!["storage", "local", "set", "k", "v"]),
            s(vec!["storage", "local", "set", "k", REDACTED])
        );
        assert_eq!(
            log(vec!["storage", "session", "set", "k", "v"]),
            s(vec!["storage", "session", "set", "k", REDACTED])
        );
        assert_eq!(
            log(vec!["storage", "local", "get", "k"]),
            s(vec!["storage", "local", "get", "k"])
        );
        // Also when the command line is refused.
        assert_eq!(
            rejected_args(&s(vec!["cookies", "set", "sid", "abc123", "--bogus"])),
            s(vec!["cookies", REDACTED, REDACTED, REDACTED, REDACTED])
        );
    }

    #[test]
    fn press_single_characters_count_as_typed_text() {
        for k in ["a", "Z", "7", "Shift+a"] {
            assert_eq!(argv::typed_positions(&s(vec!["press", k])), vec![1], "{k}");
        }
        for k in [
            "Enter",
            "Tab",
            "ArrowDown",
            "Control+a",
            "Shift+Tab",
            "Shift+Enter",
        ] {
            assert!(
                argv::typed_positions(&s(vec!["press", k])).is_empty(),
                "{k}"
            );
        }
        assert_eq!(redact(&s(vec!["press", "h"])), s(vec!["press", REDACTED]));
    }

    #[test]
    fn press_named_keys_only_are_not_typed_text() {
        let typed = |k: &str| !argv::typed_positions(&s(vec!["press", k])).is_empty();
        for k in [
            "a",
            "Shift+a",
            "a+Shift",
            "Shift+Shift+a",
            "KeyA",
            "Digit1",
            "Alt+a",
            "Space",
            "Shift+Space",
            "Control+Shift",
            "Alt+Control",
            "Control+ab",
            "enter",
            "f1",
            "F13",
        ] {
            assert!(typed(k), "{k} must count as typed text");
        }
        for k in [
            "Enter",
            "Tab",
            "Escape",
            "Backspace",
            "Delete",
            "ArrowUp",
            "ArrowDown",
            "ArrowLeft",
            "ArrowRight",
            "Home",
            "End",
            "PageUp",
            "PageDown",
            "Insert",
            "F1",
            "F12",
            "Shift+Tab",
            "Shift+Enter",
            "Control+a",
            "Meta+a",
            "Control+Alt+a",
            "Control+Shift+7",
            "Alt+ArrowLeft",
        ] {
            assert!(!typed(k), "{k} must not count as typed text");
        }
    }

    #[test]
    fn rejected_unknown_first_word_is_redacted() {
        // A session handle typed where the command goes, and a random word: neither is a
        // command word, so neither is logged.
        let handle = "s_abcdefgh.abcdefghijklmnopqrstuvwxyz";
        for word in [handle, "hunter2", "fil", "Fill"] {
            let argv = s(vec![word, "#pw", "x"]);
            assert_eq!(
                rejected_args(&argv),
                s(vec![REDACTED, REDACTED, REDACTED]),
                "{word}"
            );
            assert_eq!(rejected_command(&argv), REDACTED, "{word}");
        }
        // A known command with bad arguments keeps its name.
        let argv = s(vec!["click", "#a", "--bogus"]);
        assert_eq!(rejected_args(&argv), s(vec!["click", REDACTED, REDACTED]));
        assert_eq!(rejected_command(&argv), "click");
        assert!(rejected_args(&[]).is_empty());
        assert_eq!(rejected_command(&[]), "");
    }

    #[test]
    fn autocomplete_tokens_include_card_expiry_parts() {
        for token in SENSITIVE_AUTOCOMPLETE {
            assert!(SENSITIVE_FOCUS_JS.contains(token), "{token}");
            assert!(
                attrs_are_sensitive(Some("text"), Some(&format!("section-x {token}"))),
                "{token}"
            );
        }
        for token in ["cc-exp-month", "cc-exp-year", "cc-exp"] {
            assert!(SENSITIVE_AUTOCOMPLETE.contains(&token));
        }
        assert!(attrs_are_sensitive(Some("PassWord"), None));
        assert!(attrs_are_sensitive(None, Some("  CC-Number\t")));
        assert!(!attrs_are_sensitive(Some("text"), Some("email")));
        assert!(!attrs_are_sensitive(None, None));
    }

    #[test]
    fn a_log_line_never_exceeds_64_kib() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(tmp.path());
        let mut log = ActionLog::open(&paths, manifest("r_abcdefgh")).unwrap();
        let huge = "x".repeat(64 * 1024);
        let mut args: Vec<String> = (0..4).map(|_| huge.clone()).collect();
        args.push("short".to_string());
        args.push(REDACTED.to_string());
        // Characters that JSON escapes (six bytes each) must count too.
        args.push("\u{1}".repeat(30_000));
        let mut e = entry("fill", args);
        e.url = Some(huge.clone());
        log.append(e).unwrap();
        log.append(entry("click", s(vec!["@e1"]))).unwrap();
        let text = std::fs::read_to_string(tmp.path().join("recordings/r_abcdefgh/actions.jsonl"))
            .unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].len() < MAX_LINE_BYTES, "{}", lines[0].len());
        let got: ActionEntry = serde_json::from_str(lines[0]).unwrap();
        // The words that had to go are named by their size; a redaction marker and short
        // words stay as they are, and nothing of an over-long word is kept.
        assert!(got.args.iter().any(|a| a == "short"));
        assert!(got.args.iter().any(|a| a == REDACTED));
        assert!(got.args.iter().all(|a| !a.contains("xxxx")));
        assert!(got.args.iter().any(|a| a == "[65536 bytes]"));
        // A line that fits is untouched.
        let ok: ActionEntry = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(ok.args, vec!["@e1"]);
    }

    #[test]
    fn a_redaction_marker_is_never_replaced_by_content() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(tmp.path());
        let mut log = ActionLog::open(&paths, manifest("r_abcdefgh")).unwrap();
        let args: Vec<String> = (0..256)
            .map(|i| {
                if i % 2 == 0 {
                    REDACTED.to_string()
                } else {
                    "y".repeat(2000)
                }
            })
            .collect();
        log.append(entry("fill", args)).unwrap();
        let text = std::fs::read_to_string(tmp.path().join("recordings/r_abcdefgh/actions.jsonl"))
            .unwrap();
        assert!(text.len() <= MAX_LINE_BYTES);
        let got: ActionEntry = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(got.args.iter().filter(|a| *a == REDACTED).count(), 128);
    }

    /// A recording written before format 2: its manifest and its action lines have none of
    /// the new fields.
    fn write_format_1(paths: &Paths, rec: &str) {
        let dir = paths.recordings.join(rec);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            format!(
                r#"{{"format":1,"recordingId":"{rec}","sessionId":"s_abcdefgh","sessionName":"demo",
"profile":"default","startedAt":"2026-01-01T00:00:00Z","endedAt":"2026-01-01T00:05:00Z",
"chromeVersion":"157","agentBrowserVersion":"0.42.0"}}"#
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("actions.jsonl"),
            concat!(
                r#"{"seq":1,"ts":"2026-01-01T00:00:01Z","sessionId":"s_abcdefgh","targetId":"T1","url":"https://a.test/","command":"open","args":["open","https://a.test/"],"ok":true,"error":null,"durationMs":12,"redacted":false}"#,
                "\n",
                r##"{"seq":2,"ts":"2026-01-01T00:00:02Z","sessionId":"s_abcdefgh","targetId":null,"url":null,"command":"click","args":["click","#b"],"ok":false,"error":"x","durationMs":3,"redacted":true}"##,
                "\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn format_1_recording_reads_as_before() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(tmp.path());
        write_format_1(&paths, "r_abcdefgh");
        let (m, entries) = read(&paths, "r_abcdefgh").unwrap();
        assert_eq!(m.format, 1);
        assert!(!m.keep);
        assert_eq!(m.screen, ScreenStatus::None);
        assert!(m.record.is_empty());
        assert_eq!(m.sizes, None);
        assert_eq!(m.ended_at.as_deref(), Some("2026-01-01T00:05:00Z"));
        assert_eq!(entries.len(), 2);
        for e in &entries {
            assert_eq!(e.kind, EntryKind::Command);
            assert_eq!((&e.frame_before, &e.frame_after), (&None, &None));
            assert_eq!((&e.what, e.certain), (&None, None));
        }
        assert_eq!(entries[0].command, "open");
        assert_eq!(entries[0].duration_ms, 12);
        assert!(entries[1].redacted && !entries[1].ok);
        assert_eq!(list(&paths).unwrap().len(), 1);
        assert_eq!(
            read_manifest(&paths.recordings.join("r_abcdefgh")).unwrap(),
            m
        );
    }

    #[test]
    fn a_cut_or_bad_line_does_not_hide_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(tmp.path());
        // Format 1, with a last line cut off by a crash (or still being written).
        write_format_1(&paths, "r_abcdefgh");
        let actions = paths.recordings.join("r_abcdefgh/actions.jsonl");
        let mut f = OpenOptions::new().append(true).open(&actions).unwrap();
        f.write_all(br#"{"seq":3,"ts":"2026-01-01T00:00:03Z","sessionId":"s_ab"#)
            .unwrap();
        let (_, entries) = read(&paths, "r_abcdefgh").unwrap();
        assert_eq!(
            entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2]
        );
        // Format 2, with a bad line in the middle and a cut last one.
        let mut log = ActionLog::open(&paths, manifest("r_bcdefghi")).unwrap();
        log.append(entry("open", s(vec!["open", "http://a.test/"])))
            .unwrap();
        let actions = paths.recordings.join("r_bcdefghi/actions.jsonl");
        let mut f = OpenOptions::new().append(true).open(&actions).unwrap();
        f.write_all(b"not json\n\xff\n").unwrap();
        log.append(entry("click", s(vec!["click", "#b"]))).unwrap();
        f.write_all(br#"{"seq":3,"#).unwrap();
        let (_, entries) = read(&paths, "r_bcdefghi").unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|e| e.command.as_str())
                .collect::<Vec<_>>(),
            vec!["open", "click"]
        );
        // A recording without its actions file is still an error.
        std::fs::remove_file(&actions).unwrap();
        assert!(read(&paths, "r_bcdefghi").is_err());
    }

    #[test]
    fn new_fields_are_left_out_when_empty() {
        let e = entry("click", s(vec!["click", "#b"]));
        let v = serde_json::to_value(&e).unwrap();
        for key in ["kind", "frameBefore", "frameAfter", "what", "certain"] {
            assert!(v.get(key).is_none(), "{key} in {v}");
        }
        // Set, they are written under their camelCase names.
        let mut e = e;
        e.frame_before = Some("T1/000001.jpg".into());
        e.frame_after = Some("T1/000002.jpg".into());
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["frameBefore"], "T1/000001.jpg");
        assert_eq!(v["frameAfter"], "T1/000002.jpg");
        assert!(v.get("kind").is_none());
        assert_eq!(serde_json::from_value::<ActionEntry>(v).unwrap(), e);
        // A manifest leaves out only the sizes until they are known.
        let m = serde_json::to_value(manifest("r_abcdefgh")).unwrap();
        assert!(m.get("sizes").is_none());
        assert_eq!((&m["keep"], &m["screen"]), (&json!(false), &json!("none")));
        assert_eq!(m["record"], json!([]));
    }

    #[test]
    fn human_entry_round_trips() {
        let e = ActionEntry::human(
            "s_abcdefgh",
            "2026-01-01T00:00:03Z",
            "T1",
            Some("https://admin:pw@a.test/p?t=1#f".into()),
            "navigated",
            false,
        );
        assert_eq!(e.kind, EntryKind::Human);
        assert_eq!(e.command, "human");
        assert!(e.args.is_empty() && e.ok && !e.redacted);
        assert_eq!(e.target_id.as_deref(), Some("T1"));
        // The URL is cleaned like every logged URL.
        assert_eq!(e.url.as_deref(), Some("https://a.test/p"));
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "human");
        assert_eq!(v["command"], "human");
        assert_eq!(v["what"], "navigated");
        assert_eq!(v["certain"], false);
        assert_eq!(v["args"], json!([]));
        assert!(v.get("frameBefore").is_none());
        let back: ActionEntry = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);
        // And through a log.
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(tmp.path());
        let mut log = ActionLog::open(&paths, manifest("r_abcdefgh")).unwrap();
        log.append(e.clone()).unwrap();
        let (_, entries) = read(&paths, "r_abcdefgh").unwrap();
        assert_eq!(entries, vec![ActionEntry { seq: 1, ..e }]);
    }

    #[test]
    fn manifest_handle_updates_the_file_atomically_with_0600() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(tmp.path());
        let mut log = ActionLog::open(&paths, manifest("r_abcdefgh")).unwrap();
        let dir = paths.recordings.join("r_abcdefgh");
        assert_eq!(log.dir(), dir.as_path());
        let handle = log.manifest_handle();
        assert_eq!(handle.dir(), dir.as_path());
        // A leftover temporary file with loose permissions does not loosen the manifest.
        std::fs::write(dir.join("manifest.json.tmp"), "junk").unwrap();
        std::fs::set_permissions(
            dir.join("manifest.json.tmp"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        handle
            .update(|m| {
                m.keep = true;
                m.screen = ScreenStatus::Recorded;
                m.record = vec![RecordExtra::Console];
            })
            .unwrap();
        let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(dir.join("manifest.json")), 0o600);
        assert!(!dir.join("manifest.json.tmp").exists());
        let on_disk = read_manifest(&dir).unwrap();
        assert!(on_disk.keep);
        assert_eq!(on_disk.screen, ScreenStatus::Recorded);
        assert_eq!(on_disk.record, vec![RecordExtra::Console]);
        // Every holder sees the same manifest: the log, the handle and a clone of it.
        assert_eq!(log.manifest(), on_disk);
        assert_eq!(handle.clone().get(), on_disk);
        // The log's own updates go through the same manifest.
        log.finish("2026-01-01T00:05:00Z").unwrap();
        let sizes = Sizes {
            screen_bytes: 10,
            total_bytes: 12,
        };
        handle.update(|m| m.sizes = Some(sizes)).unwrap();
        let on_disk = read_manifest(&dir).unwrap();
        assert_eq!(on_disk.ended_at.as_deref(), Some("2026-01-01T00:05:00Z"));
        assert!(on_disk.keep);
        assert_eq!(on_disk.sizes, Some(sizes));
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(v["sizes"], json!({"screenBytes": 10, "totalBytes": 12}));
        // The manifest is replaced whole (a reader never sees half of one): an update that
        // cannot be written leaves the file and the shared copy as they were.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        // (root writes into a read-only directory, so this part needs an ordinary user.)
        // SAFETY: geteuid has no preconditions and cannot fail.
        if unsafe { libc::geteuid() } != 0 {
            let failed = handle.update(|m| m.keep = false);
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert!(failed.is_err());
            assert!(handle.get().keep);
            assert!(read_manifest(&dir).unwrap().keep);
            assert!(!dir.join("manifest.json.tmp").exists());
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn new_recordings_are_format_2() {
        assert_eq!(MANIFEST_FORMAT, 2);
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(tmp.path());
        let log = ActionLog::open(&paths, manifest("r_abcdefgh")).unwrap();
        let m = read_manifest(&paths.recordings.join("r_abcdefgh")).unwrap();
        assert_eq!(m.format, 2);
        assert_eq!(m, log.manifest());
        assert!(!m.keep && m.record.is_empty() && m.sizes.is_none());
        assert_eq!(m.screen, ScreenStatus::None);
        assert_eq!(
            recording_dir(&paths, "r_abcdefgh").unwrap(),
            paths.recordings.join("r_abcdefgh")
        );
        assert!(recording_dir(&paths, "../x").is_err());
    }
}
