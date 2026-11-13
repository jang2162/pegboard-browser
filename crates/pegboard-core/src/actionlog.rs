//! The action log of a session: one JSON object per line in
//! `<recordings>/<recording id>/actions.jsonl`, next to a `manifest.json`.
//!
//! What is written is the command line, never what a command returned. Typed text is kept
//! only when it provably went into a harmless field (see `daemon::run_command`); secret
//! values (`cookies set`, `storage set`) and the strings inside `eval` expressions are
//! never kept. The
//! session endpoint's token is not a field of anything here.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;

use pegboard_protocol::SessionId;
use serde::{Deserialize, Serialize};

use crate::argv;
use crate::paths::Paths;

pub const REDACTED: &str = "[redacted]";

/// Longest `eval` expression (in characters) kept in the log.
pub const MAX_EVAL_CHARS: usize = 200;

/// Longest error text (in characters) kept in the log.
pub const MAX_ERROR_CHARS: usize = 200;

/// Longest line (with its newline) `ActionLog::append` writes.
pub const MAX_LINE_BYTES: usize = 64 * 1024;

const MANIFEST_FORMAT: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

fn recording_dir(paths: &Paths, recording_id: &str) -> std::io::Result<PathBuf> {
    if !valid_recording_id(recording_id) {
        return Err(invalid_id(recording_id));
    }
    Ok(paths.recordings.join(recording_id))
}

pub struct ActionLog {
    dir: PathBuf,
    manifest: Manifest,
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
        let log = ActionLog {
            dir,
            manifest,
            actions,
            next_seq: 1,
        };
        log.write_manifest()?;
        Ok(log)
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
        self.manifest.ended_at = Some(ended_at.to_string());
        self.write_manifest()
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn write_manifest(&self) -> std::io::Result<()> {
        let tmp = self.dir.join("manifest.json.tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(&self.manifest).map_err(std::io::Error::other)?)?;
        file.flush()?;
        std::fs::rename(&tmp, self.dir.join("manifest.json"))
    }
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

fn read_manifest(dir: &std::path::Path) -> std::io::Result<Manifest> {
    let text = std::fs::read_to_string(dir.join("manifest.json"))?;
    serde_json::from_str(&text).map_err(std::io::Error::other)
}

pub fn read(paths: &Paths, recording_id: &str) -> std::io::Result<(Manifest, Vec<ActionEntry>)> {
    let dir = recording_dir(paths, recording_id)?;
    let manifest = read_manifest(&dir)?;
    let mut entries = Vec::new();
    for line in BufReader::new(File::open(dir.join("actions.jsonl"))?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        entries.push(serde_json::from_str(&line).map_err(std::io::Error::other)?);
    }
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
        assert_eq!(m.format, 1);
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
                "profile",
                "recordingId",
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
        assert!(lines[0].len() + 1 <= MAX_LINE_BYTES, "{}", lines[0].len());
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
}
