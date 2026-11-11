//! The path every agent command takes: check, execute, decide what to hide, log.

use std::future::Future;
use std::time::{Duration, Instant};

use pegboard_protocol::{codes, CommandExecResult, RpcError, SessionId};
use serde_json::Value;

use crate::actionlog::{
    logged_args, rejected_args, truncate_chars, ActionEntry, ActionLog, MAX_ERROR_CHARS, REDACTED,
    SENSITIVE_FOCUS_JS,
};
use crate::argv::{self, Checked};
use crate::paths::Paths;
use crate::runner::{failure_text, succeeded, Runner, DEFAULT_TIMEOUT_MS};
use crate::session::SessionManager;

/// Time allowed for looking at what is focused after a command.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

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
/// reaches the runner. `tab new` is run through agent-browser like every other command. After a command that types
/// text, every tab of the session is asked, on the daemon's own CDP connection, whether
/// its focused element is sensitive; the typed text is not logged when it is, and also when
/// that cannot be found out (the check fails or times out, the tab is gone, the command
/// itself errored).
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
        Err(e) => {
            // The message names the offending word, which may be typed text.
            entry.args = rejected_args(argv);
            entry.error = Some(format!("{}: command line refused", e.code));
            entry.duration_ms = elapsed_ms(started);
            append(log, entry)?;
            return Err(e);
        }
    };
    let run_argv = match checked {
        Checked::Run(words) => words,
        Checked::TabNew { url } => vec!["tab".to_string(), "new".to_string(), url],
    };

    runner.set_session_targets_provider(sessions.targets_fn());
    let outcome = match sessions.endpoint(id) {
        Ok(endpoint) => {
            runner
                .exec(
                    id,
                    &endpoint,
                    &run_argv,
                    timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
                )
                .await
        }
        Err(e) => Err(e),
    };
    after_exec.await;

    let positions = argv::text_positions(argv);
    let types_text = !positions.is_empty();
    let probe = probe(sessions, id).await;
    // Fail closed: typed text stays out of the log unless the focused element is known to
    // be harmless.
    let redacted = types_text
        && (argv::text_always_hidden(argv) || outcome.is_err() || probe.failed || probe.sensitive);

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
    entry.url = tab.and_then(|(_, u)| u.clone());
    entry.args = logged_args(argv, redacted);
    entry.redacted = redacted;
    entry.duration_ms = elapsed_ms(started);
    let failure = match &outcome {
        Ok(r) if succeeded(r) => None,
        Ok(r) => Some(failure_text(r)),
        Err(e) => Some(e.message.clone()),
    };
    entry.ok = failure.is_none();
    entry.error = failure.map(|text| {
        let mut text = text;
        if redacted {
            for &i in &positions {
                if let Some(word) = argv.get(i).filter(|w| !w.is_empty()) {
                    text = text.replace(word.as_str(), REDACTED);
                }
            }
        }
        truncate_chars(&text, MAX_ERROR_CHARS)
    });
    append(log, entry)?;
    sessions.touch(id);
    outcome
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
