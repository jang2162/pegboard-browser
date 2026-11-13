//! The path every agent command takes: check, execute, decide what to hide, log.

use std::future::Future;
use std::time::{Duration, Instant};

use pegboard_protocol::{codes, CommandExecResult, RpcError, SessionId};
use serde_json::Value;

use crate::actionlog::{
    attrs_are_sensitive, logged_args, rejected_args, rejected_command, truncate_chars, url_for_log,
    ActionEntry, ActionLog, MAX_ERROR_CHARS, SENSITIVE_FOCUS_JS,
};
use crate::argv::{self, Checked, TextDestination};
use crate::paths::Paths;
use crate::policy;
use crate::proxy::server::Endpoint;
use crate::runner::{succeeded, Runner, DEFAULT_TIMEOUT_MS};
use crate::session::SessionManager;

pub use crate::service::Daemon;

/// Time allowed for looking at what is focused after a command.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Longest time (ms) for one `get attr` that looks at a typing destination.
const PRE_CHECK_LIMIT_MS: u64 = 5_000;

fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn probe_js() -> String {
    format!("({{ url: location.href, sensitive: {SENSITIVE_FOCUS_JS} }})")
}

/// What the tabs of the session looked like right after a command.
struct Probe {
    /// Some tab could not be read, or the session has no tab left.
    failed: bool,
    /// Some tab has a sensitive (or unreadable) focused element.
    sensitive: bool,
    /// Per tab: its id and its URL.
    tabs: Vec<(String, Option<String>)>,
}

async fn probe(sessions: &SessionManager, id: &SessionId) -> Probe {
    let results =
        match tokio::time::timeout(PROBE_TIMEOUT, sessions.evaluate_in_targets(id, &probe_js()))
            .await
        {
            Ok(Ok(results)) if !results.is_empty() => results,
            _ => {
                return Probe {
                    failed: true,
                    sensitive: false,
                    tabs: Vec::new(),
                }
            }
        };
    let mut p = Probe {
        failed: false,
        sensitive: false,
        tabs: Vec::new(),
    };
    for (target, result) in results {
        match result {
            Ok(v) => {
                // Anything but a plain `false` is treated as sensitive.
                p.sensitive |= v.get("sensitive") != Some(&Value::Bool(false));
                let url = v.get("url").and_then(Value::as_str).map(String::from);
                p.tabs.push((target, url));
            }
            Err(_) => {
                p.failed = true;
                p.tabs.push((target, None));
            }
        }
    }
    p
}

/// Runs one agent command in the session and logs it. Commands of one session must not run
/// at the same time (the caller serializes them).
///
/// `argv` goes through `argv::check` first; a refused one is logged (`ok: false`) and never
/// reaches the runner. `tab new` is run through agent-browser like every other command.
///
/// What the log keeps of the command line:
/// - Secret values (`cookies set` and `storage set` values, a dialog's answer) are never
///   kept. The strings inside an `eval` expression are masked and the rest cut to 200
///   characters.
/// - Typed text (`fill`, `type`, `select`, `find ... fill`, `keyboard type`,
///   `keyboard inserttext`, `press` of one character) is kept only when all of these hold:
///   the destination was harmless before the command (a selector is asked about with
///   `get attr <selector> type|autocomplete`; the focused element is looked at for the
///   forms that name none; `find ...` cannot be asked about, so it never passes), the
///   command succeeded, and afterwards no tab of the session has a sensitive or
///   unreadable focused element. A check that fails, times out (2 s for a look at the
///   focus, 5 s at most for a `get attr`) or cannot run hides the text.
/// - A redacted entry carries no error text but its code; otherwise `error` is the CLI's
///   own `error` string or pegboard's own error, never output.
pub async fn run_command(
    sessions: &SessionManager,
    runner: &Runner,
    paths: &Paths,
    log: &mut ActionLog,
    id: &SessionId,
    argv: &[String],
    timeout_ms: Option<u64>,
) -> Result<CommandExecResult, RpcError> {
    run_command_with_hook(
        sessions,
        runner,
        paths,
        log,
        id,
        argv,
        timeout_ms,
        std::future::ready(()),
    )
    .await
}

/// `run_command`, awaiting `after_exec` between the command and the check of the focused
/// element (a test hook).
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn run_command_with_hook(
    sessions: &SessionManager,
    runner: &Runner,
    paths: &Paths,
    log: &mut ActionLog,
    id: &SessionId,
    argv: &[String],
    timeout_ms: Option<u64>,
    after_exec: impl Future<Output = ()>,
) -> Result<CommandExecResult, RpcError> {
    let started = Instant::now();
    let ts = now();
    let command = argv.first().cloned().unwrap_or_default();
    let mut entry = ActionEntry {
        seq: 0,
        ts,
        session_id: id.0.clone(),
        target_id: None,
        url: None,
        command,
        args: Vec::new(),
        ok: false,
        error: None,
        duration_ms: 0,
        redacted: false,
    };

    let checked = match argv::check(argv, &paths.session_files(id)) {
        Ok(checked) => checked,
        Err(e) => return refuse(log, entry, argv, e, started),
    };
    // The policy judges every command before anything runs for it. (The selector checks
    // below are pegboard's own and are not judged.) A session that cannot be found has no
    // policy to read: refused too.
    let judged = argv::class_of(argv)
        .ok_or_else(|| RpcError {
            code: codes::INTERNAL.to_string(),
            message: "a checked command line has no action class".into(),
            data: None,
        })
        .and_then(|class| policy::judge_action(&sessions.policy(id)?, class));
    if let Err(e) = judged {
        return refuse(log, entry, argv, e, started);
    }
    let is_tab_new = matches!(checked, Checked::TabNew { .. });
    let run_argv = match checked {
        Checked::Run(words) => words,
        Checked::TabNew { url } => vec!["tab".to_string(), "new".to_string(), url],
    };

    runner.set_session_targets_provider(sessions.targets_fn());
    let timeout_ms = timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    let endpoint = sessions.endpoint(id);
    let at = argv::text_at(argv);
    let types_text = !at.typed.is_empty();

    // Before: is what gets typed going somewhere harmless?
    let harmless_before = if !types_text {
        true
    } else {
        match (&at.destination, &endpoint) {
            (Some(TextDestination::Selector(selector)), Ok(ep)) => {
                selector_is_harmless(runner, id, ep, selector, timeout_ms).await
            }
            (Some(TextDestination::Focused), _) => {
                let p = probe(sessions, id).await;
                !p.failed && !p.sensitive
            }
            _ => false,
        }
    };

    let had_no_tab = sessions.info(id).is_ok_and(|i| i.targets.is_empty());
    let outcome = match &endpoint {
        Ok(ep) => runner.exec(id, ep, &run_argv, timeout_ms).await,
        Err(e) => Err(e.clone()),
    };
    // A session without a tab: agent-browser opens a blank tab of its own, then `tab new`
    // opens the requested one. Only the tab it ends up bound to stays.
    if is_tab_new && had_no_tab && matches!(&outcome, Ok(r) if succeeded(r)) {
        if let Some(bound) = runner.bound_target(id) {
            sessions.close_blank_tabs_except(id, &bound).await;
        }
    }
    after_exec.await;

    // After: the focus may have moved (a submit, a script that blurs); anything unclear
    // hides the text.
    let probe = probe(sessions, id).await;
    let succeeded = matches!(&outcome, Ok(r) if succeeded(r));
    let hide_typed =
        types_text && !(harmless_before && succeeded && !probe.failed && !probe.sensitive);

    let bound = runner.bound_target(id);
    let tab = probe
        .tabs
        .iter()
        .find(|(t, _)| Some(t) == bound.as_ref())
        .or(if probe.tabs.len() == 1 {
            probe.tabs.first()
        } else {
            None
        });
    entry.target_id = tab.map(|(t, _)| t.clone());
    entry.url = tab.and_then(|(_, u)| u.as_deref()).map(url_for_log);
    entry.args = logged_args(argv, hide_typed);
    entry.redacted = hide_typed || !at.always.is_empty() || eval_masked(argv, &entry.args);
    entry.duration_ms = elapsed_ms(started);
    entry.ok = succeeded;
    // An expression's error can quote page values (a JSON parse error shows the text).
    entry.error = error_text(entry.redacted, is_expression_command(argv), &outcome);
    append(log, entry)?;
    sessions.touch(id);
    outcome
}

/// Logs a command refused before it ran (its line, or the policy) and returns the error.
/// The command word is shown when the grammar knows it; every later word is hidden, since
/// the error message may name one and they may be typed text.
fn refuse(
    log: &mut ActionLog,
    mut entry: ActionEntry,
    argv: &[String],
    e: RpcError,
    started: Instant,
) -> Result<CommandExecResult, RpcError> {
    entry.command = rejected_command(argv);
    entry.args = rejected_args(argv);
    entry.redacted = true;
    entry.error = Some(withheld(&e.code));
    entry.duration_ms = elapsed_ms(started);
    append(log, entry)?;
    Err(e)
}

/// An `eval` whose logged expression is not the one given (strings masked, or cut).
fn eval_masked(argv: &[String], logged: &[String]) -> bool {
    argv.first().map(String::as_str) == Some("eval")
        && argv.len() > 1
        && logged.get(1) != Some(&argv[1..].join(" "))
}

fn withheld(code: &str) -> String {
    format!("{code}: details withheld (entry redacted)")
}

/// `eval` and `wait --fn`: commands whose errors may quote values of the page.
fn is_expression_command(argv: &[String]) -> bool {
    argv.first().map(String::as_str) == Some("eval") || !argv::text_at(argv).expr.is_empty()
}

/// The `error` of an entry for a command that did not succeed. A redacted entry, and an
/// expression command, say only their code. Otherwise a pegboard error is its code alone
/// (its message may carry the CLI's output) and a CLI failure its own `error` string.
fn error_text(
    redacted: bool,
    expression: bool,
    outcome: &Result<CommandExecResult, RpcError>,
) -> Option<String> {
    match outcome {
        Ok(r) if succeeded(r) => None,
        Ok(r) => {
            let code = r
                .output
                .get("code")
                .and_then(Value::as_str)
                .filter(|c| {
                    c.len() <= 40 && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                })
                .unwrap_or("command_failed");
            Some(if redacted {
                withheld(code)
            } else if expression {
                format!("{code}: details withheld")
            } else {
                r.output
                    .get("error")
                    .and_then(Value::as_str)
                    .map(|e| truncate_chars(e, MAX_ERROR_CHARS))
                    .unwrap_or_else(|| "command failed".to_string())
            })
        }
        Err(e) if redacted => Some(withheld(&e.code)),
        Err(e) => Some(e.code.clone()),
    }
}

/// Asks the CLI for the `type` and `autocomplete` attributes of the element `selector`
/// names. Harmless only when both calls succeed and neither marks a secret field. These
/// calls are internal: they are not logged and do not pass the caller-facing grammar.
async fn selector_is_harmless(
    runner: &Runner,
    id: &SessionId,
    endpoint: &Endpoint,
    selector: &str,
    timeout_ms: u64,
) -> bool {
    let mut values = Vec::new();
    // One budget for both calls.
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.min(PRE_CHECK_LIMIT_MS));
    for attr in ["type", "autocomplete"] {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        let argv: Vec<String> = ["get", "attr", selector, attr]
            .iter()
            .map(|w| w.to_string())
            .collect();
        match runner
            .exec(
                id,
                endpoint,
                &argv,
                u64::try_from(left.as_millis()).unwrap_or(u64::MAX).max(1),
            )
            .await
        {
            Ok(r) if succeeded(&r) => match attribute_value(&r.output) {
                Some(v) => values.push(v),
                None => return false,
            },
            _ => return false,
        }
    }
    !attrs_are_sensitive(values[0].as_deref(), values[1].as_deref())
}

/// The attribute value in the output of `get attr`: `Some(None)` when the element has no
/// such attribute, `None` when the output is not understood.
fn attribute_value(output: &Value) -> Option<Option<String>> {
    match output.pointer("/data/value")? {
        Value::Null => Some(None),
        Value::String(s) => Some(Some(s.clone())),
        _ => None,
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn append(log: &mut ActionLog, entry: ActionEntry) -> Result<(), RpcError> {
    log.append(entry).map_err(|e| RpcError {
        code: codes::INTERNAL.to_string(),
        message: format!("could not write the action log: {e}"),
        data: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn failed(output: Value) -> Result<CommandExecResult, RpcError> {
        Ok(CommandExecResult {
            exit_code: 1,
            output,
            stderr: "stderr says hunter2".into(),
        })
    }

    #[test]
    fn redacted_entries_withhold_error_text() {
        let out = failed(
            json!({"success": false, "error": "no element for hunter2", "code": "not_found"}),
        );
        assert_eq!(
            error_text(true, false, &out).unwrap(),
            "not_found: details withheld (entry redacted)"
        );
        assert_eq!(
            error_text(true, false, &rpc_err()).unwrap(),
            "timeout: details withheld (entry redacted)"
        );
    }

    fn rpc_err() -> Result<CommandExecResult, RpcError> {
        Err(RpcError {
            code: codes::TIMEOUT.into(),
            message: "hunter2 timed out".into(),
            data: None,
        })
    }

    #[test]
    fn log_error_rules() {
        // A pegboard error: the code only.
        assert_eq!(error_text(false, false, &rpc_err()).unwrap(), "timeout");
        // A CLI failure: its own error string, cut to 200 characters.
        let out =
            failed(json!({"success": false, "error": "no element for #x", "code": "not_found"}));
        assert_eq!(error_text(false, false, &out).unwrap(), "no element for #x");
        let long = failed(json!({"success": false, "error": "e".repeat(500)}));
        assert_eq!(
            error_text(false, false, &long).unwrap().chars().count(),
            200
        );
        // eval and `wait --fn` can quote page values: withheld.
        assert_eq!(
            error_text(false, true, &out).unwrap(),
            "not_found: details withheld"
        );
        let no_string = failed(json!({"success": false, "error": {"x": 1}, "stdout": "leak"}));
        assert_eq!(
            error_text(false, false, &no_string).unwrap(),
            "command failed"
        );
        let none = failed(json!({"success": false}));
        assert_eq!(error_text(false, false, &none).unwrap(), "command failed");
        let fine = Ok(CommandExecResult {
            exit_code: 0,
            output: json!({"success": true}),
            stderr: String::new(),
        });
        assert!(
            error_text(false, false, &fine).is_none() && error_text(true, false, &fine).is_none()
        );
    }

    #[test]
    fn a_denied_command_is_logged_like_a_rejected_one() {
        use crate::actionlog::{read, Manifest, REDACTED};
        use pegboard_protocol::{ActionClass, SessionPolicy};

        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(tmp.path());
        let manifest = Manifest::new("r_abcdefgh", "s_abcdefgh", "n", "default", "t", "c", "a");
        let mut log = ActionLog::open(&paths, manifest).unwrap();
        let argv: Vec<String> = ["cookies", "set", "sid", "hunter2-secret"]
            .iter()
            .map(|w| w.to_string())
            .collect();
        let class = argv::class_of(&argv).unwrap();
        assert_eq!(class, ActionClass::State);
        let denied = policy::judge_action(&SessionPolicy::default(), class).unwrap_err();
        let entry = ActionEntry {
            seq: 0,
            ts: now(),
            session_id: "s_abcdefgh".into(),
            target_id: None,
            url: None,
            command: argv[0].clone(),
            args: Vec::new(),
            ok: false,
            error: None,
            duration_ms: 0,
            redacted: false,
        };
        let e = refuse(&mut log, entry, &argv, denied, Instant::now()).unwrap_err();
        assert_eq!(e.code, codes::POLICY_DENIED);
        let (_, entries) = read(&paths, "r_abcdefgh").unwrap();
        assert_eq!(entries.len(), 1);
        let logged = &entries[0];
        assert_eq!(logged.command, "cookies");
        assert_eq!(logged.args, ["cookies", REDACTED, REDACTED, REDACTED]);
        assert!(logged.redacted && !logged.ok);
        assert_eq!(
            logged.error.as_deref(),
            Some("policy_denied: details withheld (entry redacted)")
        );
        let text = std::fs::read_to_string(
            tmp.path()
                .join("recordings")
                .join("r_abcdefgh")
                .join("actions.jsonl"),
        )
        .unwrap();
        assert!(
            !text.contains("hunter2-secret") && !text.contains("sid"),
            "{text}"
        );
    }

    #[test]
    fn expression_commands_are_found() {
        let a = |v: &[&str]| {
            is_expression_command(&v.iter().map(|w| w.to_string()).collect::<Vec<_>>())
        };
        assert!(a(&["eval", "1"]));
        assert!(a(&["wait", "--fn", "x"]));
        assert!(!a(&["wait", "#a"]));
        assert!(!a(&["click", "#a"]));
    }
}
