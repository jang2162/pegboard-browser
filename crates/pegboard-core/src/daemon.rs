//! The path every agent command takes: check, execute, decide what to hide, log.

use std::future::Future;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use pegboard_protocol::{
    codes, ActionClass, CommandExecResult, ConfirmBy, ConfirmOutcome, RpcError, SessionId,
    SessionPolicy,
};
use serde_json::Value;

use crate::actionlog::{
    attrs_are_sensitive, logged_args, rejected_args, rejected_command, truncate_chars, url_for_log,
    ActionEntry, ActionLog, ConfirmLog, MAX_ERROR_CHARS, SENSITIVE_FOCUS_JS,
};
use crate::argv::{self, rejected, Checked, TextDestination};
use crate::confirm::details::{
    display_text, origin_of, state_details, upload_details, NotShown, ShownFile, StatePin, Where,
    NAME_CHARS,
};
use crate::confirm::staging::{self, StageError, Staged};
use crate::confirm::{ConfirmAsk, ConfirmBroker, ConfirmDecision, ConfirmPlace};
use crate::downloads::Report;
use crate::paths::Paths;
use crate::policy;
use crate::proxy::filter::UploadWindow;
use crate::proxy::server::Endpoint;
use crate::recording::recorder::AfterFrame;
use crate::runner::{succeeded, Runner, DEFAULT_TIMEOUT_MS};
use crate::session::SessionManager;
use crate::upload;

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
///
/// A command the policy leaves to a person (`Verdict::Ask`) waits for an answer through
/// `confirm` (design 4.2): denied, it is refused with `policy_denied` and `data.reason`;
/// approved, it runs held to what the person was shown (`StatePin`), and its result says
/// `"confirmed": {"by": …}`. Either way its log entry carries `confirm`.
#[allow(clippy::too_many_arguments)]
pub async fn run_command(
    sessions: &SessionManager,
    runner: &Runner,
    paths: &Paths,
    confirm: &ConfirmBroker,
    log: &mut ActionLog,
    id: &SessionId,
    argv: &[String],
    timeout_ms: Option<u64>,
) -> Result<CommandExecResult, RpcError> {
    run_command_with_hook(
        sessions,
        runner,
        paths,
        confirm,
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
    confirm: &ConfirmBroker,
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
        ..ActionEntry::default()
    };

    let checked = match argv::check(argv, &paths.session_files(id)) {
        Ok(checked) => checked,
        Err(e) => return refuse(log, entry, argv, e, started),
    };
    // The policy judges every command before anything runs for it. (The selector checks
    // below are pegboard's own and are not judged.) A session that cannot be found has no
    // policy to read: refused too. What a person allowed for the rest of the session turns
    // a question into an allow.
    let judged = argv::class_of(argv)
        .ok_or_else(|| RpcError {
            code: codes::INTERNAL.to_string(),
            message: "a checked command line has no action class".into(),
            data: None,
        })
        .and_then(|class| {
            let verdict = judged_verdict(&sessions.policy(id)?, &confirm.granted(id), class);
            Ok((class, verdict))
        });
    let (class, judged) = match judged {
        Ok((_, Judged::Deny(e))) | Err(e) => return refuse(log, entry, argv, e, started),
        Ok(judged) => judged,
    };
    if let Err(e) = require_protections(sessions, id) {
        return refuse(log, entry, argv, e, started);
    }
    let is_tab_new = matches!(checked, Checked::TabNew { .. });
    let mut run_argv = match checked {
        Checked::Run(words) => words,
        Checked::TabNew { url } => vec!["tab".to_string(), "new".to_string(), url],
    };
    // An address the session may not open is refused here with the reason, before the
    // guard would only make the page fail to load. Judged on the normalised words that run.
    if let Err(e) = judge_navigation_urls(sessions, id, argv, &run_argv).await {
        return refuse(log, entry, argv, e, started);
    }
    // A question about a write in a frame cannot be shown truthfully: refused at once.
    if matches!(judged, Judged::Ask)
        && acts_in_a_frame(class, &run_argv)
        && runner.frame_selected(id)
    {
        return refuse(log, entry, argv, frame_selected(class), started);
    }
    // An upload names its files; they become paths only now that the policy allows it.
    let is_upload = run_argv.first().map(String::as_str) == Some("upload");
    let mut path_names = Vec::new();
    // An upload that asks a person: the files it would copy, each with where it may be.
    let mut asked_sources = Vec::new();
    if is_upload && matches!(judged, Judged::Ask) {
        // Nobody to answer: denied before any file is looked at or copied.
        if !confirm.has_responder(question_place(sessions, runner, id).is_some()) {
            entry.confirm = Some(ConfirmLog {
                outcome: ConfirmOutcome::NoResponder,
                by: None,
                wait_ms: 0,
            });
            let e = denial(class, ConfirmOutcome::NoResponder, None, confirm.timeout());
            return refuse(log, entry, argv, e, started);
        }
        match resolve_asked_upload(sessions, paths, id, &run_argv) {
            Ok(sources) => asked_sources = sources,
            Err(e) => return refuse(log, entry, argv, e, started),
        }
    } else if is_upload {
        match resolve_upload(sessions, paths, id, &run_argv) {
            Ok((words, names)) => (run_argv, path_names) = (words, names),
            Err(e) => return refuse(log, entry, argv, e, started),
        }
    }
    // A person is asked, when the policy says so; the question shows the tab the command
    // would act on, read now. An upload's files are copied first (on a thread of its own,
    // raced by the session's close like the rest of the preparation): the person is shown
    // the copies' real paths and sizes, and the copies are what uploads.
    let timeout_ms = timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    let mut staged: Option<Staged> = None;
    // The tab the person is asked about: an approved upload goes there and nowhere else.
    let mut asked_target: Option<String> = None;
    let prepare = async {
        let mut shown = Vec::new();
        if !asked_sources.is_empty() {
            let copies =
                stage_copies(paths, confirm, id, std::mem::take(&mut asked_sources)).await?;
            shown = copies
                .files
                .iter()
                .map(|f| ShownFile {
                    path: f.real.display().to_string(),
                    size: f.size,
                })
                .collect();
            staged = Some(copies);
        }
        asked_target = screen_target(sessions, runner, id);
        prepare_question(
            sessions,
            id,
            asked_target.as_deref(),
            class,
            &run_argv,
            &shown,
        )
        .await
    };
    let cleared = clear_command(confirm, judged, class, id, prepare).await;
    // A command that waited for a person is logged at the answer: its time and duration
    // are its own, not the wait's (which is `confirm.waitMs`), so a recording does not
    // stretch it over the wait.
    let (cleared, started) = match cleared {
        Ok(cleared) => {
            let waited = cleared.confirm_id.is_some();
            (
                cleared,
                if waited {
                    answered(&mut entry)
                } else {
                    started
                },
            )
        }
        Err(refused) => {
            let started = if refused.confirm.is_some() {
                answered(&mut entry)
            } else {
                started
            };
            entry.confirm = refused.confirm;
            return refuse(log, entry, argv, refused.error, started);
        }
    };
    entry.confirm = cleared.confirm.clone();
    // A person may have taken up to the time limit: what was in place before is checked
    // again.
    if cleared.confirm_id.is_some() {
        if let Err(e) = require_protections(sessions, id) {
            return refuse(log, entry, argv, e, started);
        }
    }
    // An approved upload sends the copies the person was shown, under the files' names.
    // Now that a person approved it, the session's oldest kept copies make room for it.
    if let Some(copies) = &staged {
        confirm.copies().make_room(id);
        run_argv.truncate(2);
        for f in &copies.files {
            run_argv.push(f.staged.display().to_string());
            path_names.push((f.given.clone(), f.name.clone()));
            for path in [&f.real, &f.staged] {
                path_names.push((path.display().to_string(), f.name.clone()));
            }
        }
    }

    // The frame before: the tab's last stored frame (no capture), only for a command that
    // passed every check above. From here until the command's end (and a grace after it)
    // what happens in the session's tabs is the command's, not a person's.
    let recorder = sessions.recorder();
    let _window = recorder.command_window(id);
    let frame_before = recorder.before_command(id, screen_target(sessions, runner, id).as_deref());

    runner.set_session_targets_provider(sessions.targets_fn());
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

    // Held to what the person was shown, right before it runs: an upload's tab is the asked
    // one (its window names it), a storage write's the one it acts on now.
    let now = async {
        match (&staged, &asked_target) {
            (Some(_), Some(t)) => target_url(sessions, id, t).await,
            (Some(_), None) => None,
            _ => tab_url(sessions, runner, id).await,
        }
    };
    if !apply_pin(&cleared.pin, &mut run_argv, now).await {
        if let Some(c) = entry.confirm.as_mut() {
            c.outcome = ConfirmOutcome::Stale;
        }
        let e = denial(
            class,
            ConfirmOutcome::Stale,
            cleared.confirm_id.as_deref(),
            confirm.timeout(),
        );
        return refuse(log, entry, argv, e, started);
    }

    // The proxy takes files for a file input only from where this upload may: an approved
    // upload's window (its copies, into the asked tab's top document at the shown origin,
    // once) while it runs, else the session's standing directories. Closed again right after
    // the command, however it ended.
    let window = staged.as_ref().and_then(|copies| {
        let StatePin::Origin(origin) = &cleared.pin else {
            return None;
        };
        Some(UploadWindow {
            files: copies.copies(),
            target: asked_target.clone()?,
            origin: origin.clone(),
        })
    });
    let upload_dirs = is_upload
        .then(|| UploadDirs::open(sessions, paths, confirm, id, window))
        .flatten();
    let had_no_tab = sessions.info(id).is_ok_and(|i| i.targets.is_empty());
    let exec_started = Instant::now();
    let mut outcome = match &endpoint {
        Ok(ep) => runner.exec(id, ep, &run_argv, timeout_ms).await,
        Err(e) => Err(e.clone()),
    };
    drop(upload_dirs);
    // The copies of an upload that ran stay until the session closes: the page reads the
    // files when it uses them (a form submitted later). Those of one that failed go now.
    if let Some(copies) = staged.take() {
        if matches!(&outcome, Ok(r) if succeeded(r)) {
            confirm.copies().keep(id, copies);
        }
    }
    hide_paths(&mut outcome, &path_names);
    track_frame(runner, id, &run_argv, &outcome);
    if let (
        Some(c),
        Ok(CommandExecResult {
            output: Value::Object(output),
            ..
        }),
    ) = (&cleared.confirm, &mut outcome)
    {
        output.insert("confirmed".to_string(), serde_json::json!({ "by": c.by }));
    }
    // A session without a tab: agent-browser opens a blank tab of its own, then `tab new`
    // opens the requested one. Only the tab it ends up bound to stays.
    if is_tab_new && had_no_tab && matches!(&outcome, Ok(r) if succeeded(r)) {
        if let Some(bound) = runner.bound_target(id) {
            sessions.close_blank_tabs_except(id, &bound).await;
        }
    }
    let exec_time = exec_started.elapsed();
    after_exec.await;

    // After: the focus may have moved (a submit, a script that blurs); anything unclear
    // hides the text. The frame after (one capture of the tab the command ended on, at most
    // `CAPTURE_LIMIT`) is taken meanwhile.
    let after_target = screen_target(sessions, runner, id);
    let ((probe, probe_time), (frame_after, after_time)) = tokio::join!(
        timed(probe(sessions, id)),
        timed(recorder.after_command(id, after_target.as_deref()))
    );
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
    entry.frame_before = frame_before;
    entry.duration_ms = elapsed_ms(started);
    if crate::testenv::hooks() {
        // Where the command's time went (the probe and the frame after run at once).
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        eprintln!(
            "pegboard: command {}: checks {:.1} ms, exec {:.1} ms, probe {:.1} ms, frame after {:.1} ms, total {} ms",
            entry.command,
            ms(exec_started.duration_since(started)),
            ms(exec_time),
            ms(probe_time),
            ms(after_time),
            entry.duration_ms,
        );
    }
    entry.ok = succeeded;
    // An expression's error can quote page values (a JSON parse error shows the text).
    entry.error = error_text(entry.redacted, is_expression_command(argv), &outcome);
    // The frame after is being written meanwhile; a write that already failed names nothing.
    entry.frame_after = frame_after.and_then(AfterFrame::unless_failed);
    // The command's own entry first: a failure to write a download report cannot drop it
    // (the reports wait for the next result).
    append(log, entry)?;
    report_downloads(sessions, log, id, &mut outcome)?;
    sessions.touch(id);
    outcome
}

/// The command's entry restarts at the answer to its question: `ts` now, and the instant its
/// duration counts from.
fn answered(entry: &mut ActionEntry) -> Instant {
    entry.ts = now();
    Instant::now()
}

/// What must be in place before any command of the session runs.
fn require_protections(sessions: &SessionManager, id: &SessionId) -> Result<(), RpcError> {
    // Without the request guard the browser checks no request of the session: nothing runs.
    sessions.require_guard()?;
    // Without its download policy the profile's downloads land in the user's ~/Downloads.
    sessions.require_downloads(id)?;
    // Without its clipboard denial a page of the profile can read and write the system
    // clipboard through `navigator.clipboard`.
    sessions.require_clipboard(id)?;
    // Tabs that were there when the guard reconnected may not be set up yet (shielded,
    // judged): every session of the profile waits for them.
    sessions.require_tabs_set_up(id)
}

/// `f` and how long it took.
async fn timed<T>(f: impl Future<Output = T>) -> (T, Duration) {
    let started = Instant::now();
    let out = f.await;
    (out, started.elapsed())
}

/// The tab a command's frames are of: the one agent-browser works on, or else the session's
/// only tab.
fn screen_target(sessions: &SessionManager, runner: &Runner, id: &SessionId) -> Option<String> {
    runner.bound_target(id).or_else(|| {
        let mut targets = sessions.info(id).ok()?.targets;
        (targets.len() == 1).then(|| targets.remove(0))
    })
}

/// Downloads of the session that ended since its last result: one log entry each, and
/// `"downloads": [...]` in this result's output (each told once). Kept for the next result
/// when this one has no output to carry them.
fn report_downloads(
    sessions: &SessionManager,
    log: &mut ActionLog,
    id: &SessionId,
    outcome: &mut Result<CommandExecResult, RpcError>,
) -> Result<(), RpcError> {
    let Ok(CommandExecResult {
        output: Value::Object(output),
        ..
    }) = outcome
    else {
        return Ok(());
    };
    let reports = sessions.take_download_reports(id);
    if reports.is_empty() {
        return Ok(());
    }
    for (i, r) in reports.iter().enumerate() {
        if let Err(e) = append(log, r.log_entry(id)) {
            // Neither logged nor told: they wait for the next result.
            sessions.put_back_download_reports(id, reports[i..].to_vec());
            return Err(e);
        }
    }
    output.insert(
        "downloads".to_string(),
        Value::Array(reports.iter().map(Report::to_json).collect()),
    );
    Ok(())
}

/// The URLs a navigating command line opens (`open`, `tab new`, `a11y <url>`, `vitals <url>`),
/// each judged by the session's address policy. The grammar finds them in `argv`; the words
/// judged are those of `run` (the normalised line that runs), at the same positions.
async fn judge_navigation_urls(
    sessions: &SessionManager,
    id: &SessionId,
    argv: &[String],
    run: &[String],
) -> Result<(), RpcError> {
    if argv::class_of(argv) != Some(ActionClass::Navigate) {
        return Ok(());
    }
    for &i in &argv::text_at(argv).urls {
        let url = run.get(i).ok_or_else(|| RpcError {
            code: codes::INTERNAL.to_string(),
            message: "a URL of the command line is missing from the line that runs".into(),
            data: None,
        })?;
        sessions.judge_navigation(id, url).await?;
    }
    Ok(())
}

/// Keeps track of whether agent-browser has a frame selected: `frame <selector>` that
/// succeeded selects one, `frame main` that succeeded goes back to the top. Anything else
/// leaves it (a restarted agent-browser daemon is back at the top, which only makes pegboard
/// refuse more than it must).
fn track_frame(
    runner: &Runner,
    id: &SessionId,
    run_argv: &[String],
    outcome: &Result<CommandExecResult, RpcError>,
) {
    if let ["frame", target] = run_argv.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        if matches!(outcome, Ok(r) if succeeded(r)) {
            runner.set_frame_selected(id, target != "main");
        }
    }
}

/// The words of a resolved upload and, per file, its path and its name.
type Resolved = (Vec<String>, Vec<(String, String)>);

/// Turns the file names of an `upload` command line (`upload <selector> <name>...`) into
/// paths: each name is looked up in the session's upload directory, then its files
/// directory. Returns the words to run and, per file, its path and its name (so that the
/// path can be kept from the agent). A name found in neither is refused without naming a
/// directory.
fn resolve_upload(
    sessions: &SessionManager,
    paths: &Paths,
    id: &SessionId,
    words: &[String],
) -> Result<Resolved, RpcError> {
    let policy = sessions.policy(id)?;
    resolve_names(policy.upload_dir.as_deref(), paths.session_files(id), words)
}

fn resolve_names(
    upload_dir: Option<&str>,
    files_dir: PathBuf,
    words: &[String],
) -> Result<Resolved, RpcError> {
    // `validate` ties the two together; a policy without the directory cannot upload.
    let Some(upload_dir) = upload_dir else {
        return Err(RpcError {
            code: codes::INTERNAL.to_string(),
            message: "the session has no upload directory".into(),
            data: None,
        });
    };
    let dirs = [PathBuf::from(upload_dir), files_dir];
    let mut out = words[..2].to_vec();
    let mut names = Vec::new();
    for name in &words[2..] {
        // Unasked uploads, and every upload of a session bound to a directory, go by name.
        if name.starts_with('/') {
            return Err(rejected(
                "this session uploads only by file name from its upload directory".to_string(),
            ));
        }
        let path = upload::resolve(name, &dirs).ok_or_else(|| {
            rejected(format!(
                "{name:?} was not found in the upload directory or the session's files"
            ))
        })?;
        let path = path.display().to_string();
        out.push(path.clone());
        names.push((path, name.clone()));
    }
    Ok((out, names))
}

/// Replaces the paths of uploaded files in what the command answered with their names:
/// agent-browser may echo them. The agent can learn the upload directory's path (the
/// session's policy shows it) but not how a name resolved, and a name is all it may pass:
/// anything but a bare file name is refused (`argv`).
fn hide_paths(outcome: &mut Result<CommandExecResult, RpcError>, names: &[(String, String)]) {
    if names.is_empty() {
        return;
    }
    let mut pairs = Vec::new();
    for (path, name) in names {
        // The CLI may print the real path (a link in the folder's path resolved).
        if let Ok(real) = std::fs::canonicalize(path) {
            pairs.push((real.display().to_string(), name.clone()));
        }
        pairs.push((path.clone(), name.clone()));
    }
    // The longest first, so a path is never half replaced by one it starts with.
    pairs.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
    let scrub = |s: &mut String| {
        for (path, name) in &pairs {
            if s.contains(path.as_str()) {
                *s = s.replace(path.as_str(), name);
            }
        }
    };
    fn walk(v: &mut Value, scrub: &dyn Fn(&mut String)) {
        match v {
            Value::String(s) => scrub(s),
            Value::Array(a) => a.iter_mut().for_each(|x| walk(x, scrub)),
            Value::Object(o) => o.values_mut().for_each(|x| walk(x, scrub)),
            _ => {}
        }
    }
    match outcome {
        Ok(r) => {
            walk(&mut r.output, &scrub);
            scrub(&mut r.stderr);
        }
        Err(e) => {
            scrub(&mut e.message);
            if let Some(d) = e.data.as_mut() {
                walk(d, &scrub);
            }
        }
    }
}

/// The policy's verdict for a command of the session, given what a person allowed for the
/// rest of it.
#[derive(Debug)]
enum Judged {
    Allow,
    /// Asking, but a person allowed the class for the rest of the session.
    AllowedForSession,
    Ask,
    Deny(RpcError),
}

fn judged_verdict(policy: &SessionPolicy, granted: &[ActionClass], class: ActionClass) -> Judged {
    match policy::judge_action(policy, granted, class) {
        policy::Verdict::Allow
            if matches!(
                policy::judge_action(policy, &[], class),
                policy::Verdict::Ask
            ) =>
        {
            // An upload grant opens the session's upload directory, so one without a
            // directory still asks (the broker never grants it; this does not rely on that).
            if class == ActionClass::Upload && policy.upload_dir.is_none() {
                Judged::Ask
            } else {
                Judged::AllowedForSession
            }
        }
        policy::Verdict::Allow => Judged::Allow,
        policy::Verdict::Ask => Judged::Ask,
        policy::Verdict::Deny(e) => Judged::Deny(e),
    }
}

/// Where the browser would show a question of the session: the window of the tab the
/// command acts on (else of another of its tabs, else of its profile). `None` (only API
/// subscribers answer) on stock Chrome and when the profile's context is not known.
fn question_place(
    sessions: &SessionManager,
    runner: &Runner,
    id: &SessionId,
) -> Option<ConfirmPlace> {
    sessions.confirm_place(id, screen_target(sessions, runner, id).as_deref())
}

/// The files of an upload that asks a person (`upload <selector> <file>...`), each with
/// where it may be (design 4.3): a bare name is looked up in the upload directory and the
/// session's files (without an upload directory, the session's files only) and must be
/// directly inside one of them; an absolute path is taken only by a session without an
/// upload directory, and must not be inside pegboard's home or the browser's user data
/// directory (`upload::check_absolute`). At most `MAX_UPLOAD_FILES`. Checked again on the
/// descriptor each is copied from (`staging::stage`).
fn resolve_asked_upload(
    sessions: &SessionManager,
    paths: &Paths,
    id: &SessionId,
    words: &[String],
) -> Result<Vec<staging::Source>, RpcError> {
    let policy = sessions.policy(id)?;
    asked_upload_sources(
        policy.upload_dir.as_deref(),
        paths.session_files(id),
        paths.upload_forbidden(),
        words,
    )
}

fn asked_upload_sources(
    upload_dir: Option<&str>,
    files_dir: PathBuf,
    forbidden: Vec<PathBuf>,
    words: &[String],
) -> Result<Vec<staging::Source>, RpcError> {
    let names = &words[2..];
    if names.len() > upload::MAX_UPLOAD_FILES {
        return Err(rejected(format!(
            "an upload that asks a person takes at most {} files",
            upload::MAX_UPLOAD_FILES
        )));
    }
    let dirs: Vec<PathBuf> = upload_dir
        .map(PathBuf::from)
        .into_iter()
        .chain([files_dir])
        .collect();
    let mut out = Vec::new();
    for name in names {
        if name.starts_with('/') {
            if upload_dir.is_some() {
                return Err(rejected(
                    "this session uploads only by file name from its upload directory".to_string(),
                ));
            }
            let real =
                upload::check_absolute(name, &forbidden).map_err(|why| not_taken(name, &why))?;
            // The real path is opened (and checked again there); the person sees it.
            out.push(staging::Source {
                given: name.clone(),
                path: real,
                allowed: upload::Allowed::Outside(forbidden.clone()),
            });
        } else {
            let path = upload::resolve(name, &dirs).ok_or_else(|| {
                rejected(if upload_dir.is_some() {
                    format!("{name:?} was not found in the upload directory or the session's files")
                } else {
                    format!(
                        "{name:?} was not found in the session's files (give any other file \
                         by its absolute path)"
                    )
                })
            })?;
            out.push(staging::Source {
                given: name.clone(),
                path,
                allowed: upload::Allowed::DirectlyIn(dirs.clone()),
            });
        }
    }
    Ok(out)
}

/// A file an asking upload does not take: the agent is told the same whatever the reason
/// (`upload::NOT_TAKEN`, quoting its own text only), so it cannot probe the file system; the
/// reason goes to the daemon's log, on the person's side.
fn not_taken(given: &str, why: &str) -> RpcError {
    eprintln!("pegboard: an upload of {given:?} was refused: {why}");
    rejected(format!("{given}: {}", upload::NOT_TAKEN))
}

/// Copies an asking upload's files (`staging::stage`) on a blocking thread, into the
/// session's confirmation directory, within the copy budgets.
async fn stage_copies(
    paths: &Paths,
    confirm: &ConfirmBroker,
    id: &SessionId,
    sources: Vec<staging::Source>,
) -> Result<Staged, RpcError> {
    let failed = |e: String| {
        eprintln!("pegboard: copying files for a confirmation failed: {e}");
        RpcError {
            code: codes::INTERNAL.to_string(),
            message: "copying the files for a person's confirmation failed".to_string(),
            data: None,
        }
    };
    let root =
        crate::paths::ensure_session_confirm(paths, id).map_err(|e| failed(e.to_string()))?;
    let (store, session) = (confirm.copies(), id.clone());
    tokio::task::spawn_blocking(move || staging::stage(&sources, &root, &store, &session))
        .await
        .map_err(|e| failed(e.to_string()))?
        .map_err(|e| match e {
            StageError::Refused { given, why } => not_taken(&given, &why),
            StageError::NoRoom(why) => RpcError {
                code: codes::LIMIT_EXCEEDED.to_string(),
                message: why,
                data: None,
            },
            StageError::Io(e) => failed(e.to_string()),
        })
}

/// What the proxy lets the session's `DOM.setFileInputFiles` do while an upload runs: an
/// approved upload's window (`UploadWindow`: its copies, its tab, its origin, once), and the
/// session's standing directories (`upload::standing_dirs`, given what a person allowed by
/// now), which it is put back to, the window closed, when dropped.
struct UploadDirs<'a> {
    sessions: &'a SessionManager,
    id: &'a SessionId,
    after: Vec<PathBuf>,
}

impl<'a> UploadDirs<'a> {
    fn open(
        sessions: &'a SessionManager,
        paths: &Paths,
        confirm: &ConfirmBroker,
        id: &'a SessionId,
        window: Option<UploadWindow>,
    ) -> Option<UploadDirs<'a>> {
        let policy = sessions.policy(id).ok()?;
        let granted = confirm.granted(id).contains(&ActionClass::Upload);
        let after = upload::standing_dirs(&policy, paths.session_files(id), granted);
        sessions.set_upload_dirs(id, after.clone());
        sessions.set_upload_window(id, window);
        Some(UploadDirs {
            sessions,
            id,
            after,
        })
    }
}

impl Drop for UploadDirs<'_> {
    fn drop(&mut self) {
        self.sessions.set_upload_window(self.id, None);
        self.sessions
            .set_upload_dirs(self.id, std::mem::take(&mut self.after));
    }
}

/// How a command was cleared to run.
#[derive(Debug, Default)]
struct Cleared {
    /// What the log keeps of the question; none when nobody was asked.
    confirm: Option<ConfirmLog>,
    confirm_id: Option<String>,
    /// What it is held to when it runs.
    pin: StatePin,
}

/// A refused command and what the log keeps of its question.
#[derive(Debug)]
struct Refused {
    error: RpcError,
    confirm: Option<ConfirmLog>,
}

/// Runs the policy's verdict: an allowed command is cleared; an asking one waits for a
/// person (`prepare` builds the question). The session's close ends the wait whenever it
/// comes: while the question is prepared (the race below) and while it waits (the broker
/// cancels it).
async fn clear_command(
    confirm: &ConfirmBroker,
    judged: Judged,
    class: ActionClass,
    id: &SessionId,
    prepare: impl Future<Output = Result<(ConfirmAsk, StatePin), RpcError>>,
) -> Result<Cleared, Refused> {
    match judged {
        Judged::Allow => Ok(Cleared::default()),
        Judged::AllowedForSession => Ok(Cleared {
            confirm: Some(ConfirmLog {
                outcome: ConfirmOutcome::Approved,
                by: Some(ConfirmBy::Session),
                wait_ms: 0,
            }),
            ..Cleared::default()
        }),
        Judged::Deny(error) => Err(Refused {
            error,
            confirm: None,
        }),
        Judged::Ask => {
            let started = Instant::now();
            let prepared = tokio::select! {
                biased;
                () = confirm.until_closing(id) => {
                    let log = ConfirmLog {
                        outcome: ConfirmOutcome::Cancelled,
                        by: None,
                        wait_ms: elapsed_ms(started),
                    };
                    return Err(Refused {
                        error: denial(class, ConfirmOutcome::Cancelled, None, confirm.timeout()),
                        confirm: Some(log),
                    });
                }
                prepared = prepare => prepared,
            };
            let (ask, pin) = prepared.map_err(|error| Refused {
                error,
                confirm: None,
            })?;
            let decision = confirm.ask(ask).await;
            let log = confirm_log(&decision);
            match decision.outcome {
                ConfirmOutcome::Approved | ConfirmOutcome::ApprovedForSession => Ok(Cleared {
                    confirm: Some(log),
                    confirm_id: Some(decision.confirm_id),
                    pin,
                }),
                outcome => Err(Refused {
                    error: denial(
                        class,
                        outcome,
                        Some(&decision.confirm_id),
                        confirm.timeout(),
                    ),
                    confirm: Some(log),
                }),
            }
        }
    }
}

fn confirm_log(d: &ConfirmDecision) -> ConfirmLog {
    ConfirmLog {
        outcome: d.outcome,
        by: d.by,
        wait_ms: u64::try_from(d.waited.as_millis()).unwrap_or(u64::MAX),
    }
}

/// `data.reason` of a denial: the outcome in snake case.
fn reason_of(outcome: ConfirmOutcome) -> &'static str {
    match outcome {
        ConfirmOutcome::Approved => "approved",
        ConfirmOutcome::ApprovedForSession => "approved_for_session",
        ConfirmOutcome::Denied => "denied",
        ConfirmOutcome::TimedOut => "timed_out",
        ConfirmOutcome::NoResponder => "no_responder",
        ConfirmOutcome::Cancelled => "cancelled",
        ConfirmOutcome::Stale => "stale",
    }
}

/// `n s` for the time limit (`1.5 s` when it is not whole seconds).
fn seconds(d: Duration) -> String {
    if d.subsec_millis() == 0 {
        format!("{} s", d.as_secs())
    } else {
        format!("{:.1} s", d.as_secs_f64())
    }
}

/// `policy_denied` for how a question ended, with `data = {reason, confirmId}` (no id when
/// no question was asked). `timeout` is the time a person had.
fn denial(
    class: ActionClass,
    outcome: ConfirmOutcome,
    confirm_id: Option<&str>,
    timeout: Duration,
) -> RpcError {
    let name = class.as_str();
    let message = match outcome {
        ConfirmOutcome::Denied => format!("a person denied this {name} command"),
        ConfirmOutcome::TimedOut => format!(
            "no answer within {}; the {name} command was denied",
            seconds(timeout)
        ),
        ConfirmOutcome::NoResponder => format!(
            "the {name} command needs a person's confirmation; confirmation needs the Pegboard \
             browser (or a client of confirm.subscribe); a person can allow it without asking \
             when creating the session with --allow {name}{}",
            if class == ActionClass::Upload {
                " and --upload-dir <directory>"
            } else {
                ""
            }
        ),
        ConfirmOutcome::Cancelled => format!(
            "the session closed (or the daemon stopped) while the {name} command waited for a \
             person"
        ),
        ConfirmOutcome::Stale => {
            format!("the page changed after a person approved; the {name} command was not run")
        }
        ConfirmOutcome::Approved | ConfirmOutcome::ApprovedForSession => {
            format!("the {name} command was not run")
        }
    };
    let mut data = serde_json::json!({ "reason": reason_of(outcome) });
    if let Some(cid) = confirm_id {
        data["confirmId"] = Value::String(cid.to_string());
    }
    RpcError {
        code: codes::POLICY_DENIED.to_string(),
        message,
        data: Some(data),
    }
}

/// Where a state command would write cannot be told (design decision: fail closed). It is
/// neither asked about nor run.
fn unknown_origin(class: ActionClass) -> RpcError {
    let name = class.as_str();
    let goes = if class == ActionClass::Upload {
        "its files would go to"
    } else {
        "it would write to"
    };
    RpcError {
        code: codes::POLICY_DENIED.to_string(),
        message: format!(
            "the {name} command was not run: pegboard cannot tell which origin {goes}, or \
             cannot show it truthfully (no tab is selected among several, its \
             address could not be read, or it is an about:, data: or otherwise opaque \
             origin); select a tab (`tab <id>`) at an http(s) page and run it again"
        ),
        data: Some(serde_json::json!({ "reason": "unknown_origin" })),
    }
}

/// A storage command (it writes in the frame agent-browser acts in).
fn is_storage(run_argv: &[String]) -> bool {
    run_argv.first().map(String::as_str) == Some("storage")
}

/// Whether what a command does lands in the frame agent-browser acts in, which is then read
/// for its question and its pin: a storage write, and an upload (its file input may be in a
/// frame of another origin).
fn acts_in_a_frame(class: ActionClass, run_argv: &[String]) -> bool {
    class == ActionClass::Upload || is_storage(run_argv)
}

/// An asking storage write or upload while agent-browser has a frame selected: not asked
/// about, not run. agent-browser's `upload` follows the selected frame, whose origin
/// pegboard cannot read (its `eval` runs in the tab's top frame), so the person could not
/// be shown where the files go; `storage` is refused alike (fail closed).
fn frame_selected(class: ActionClass) -> RpcError {
    let name = class.as_str();
    RpcError {
        code: codes::POLICY_DENIED.to_string(),
        message: format!(
            "the {name} command was not run: a frame is selected (`frame <selector>`), and \
             pegboard cannot show a person which origin it would act in; run `frame main` \
             and try again"
        ),
        data: Some(serde_json::json!({ "reason": "unknown_origin" })),
    }
}

/// The question for an asking command: the session, and for `state` what it would write
/// where (read now: the tab) and what pins it there; for `upload` where the files would go
/// (the tab) and `files`, the copies' real paths and sizes. Where it would write unknown:
/// refused (`unknown_origin`).
async fn prepare_question(
    sessions: &SessionManager,
    id: &SessionId,
    target: Option<&str>,
    class: ActionClass,
    run_argv: &[String],
    files: &[ShownFile],
) -> Result<(ConfirmAsk, StatePin), RpcError> {
    let info = sessions.info(id)?;
    let page = match target {
        Some(t) => target_url(sessions, id, t).await,
        None => None,
    };
    let at = Where {
        page_url: page.as_deref(),
    };
    let shown = if class == ActionClass::Upload {
        upload_details(files, at)
    } else {
        state_details(run_argv, at)
    };
    let (details, pin) = shown.map_err(|e| match e {
        NotShown::UnknownOrigin => unknown_origin(class),
        NotShown::Unexpected => RpcError {
            code: codes::INTERNAL.to_string(),
            message: format!("the {} command cannot be shown to a person", class.as_str()),
            data: None,
        },
    })?;
    Ok((
        ConfirmAsk {
            session_id: id.clone(),
            session_name: display_text(&info.name, NAME_CHARS),
            profile: display_text(&info.profile, NAME_CHARS),
            class,
            details,
            offer_allow_for_session: offers_allow_for_session(class, &info.policy),
            place: sessions.confirm_place(id, target),
        },
        pin,
    ))
}

/// Whether a person is offered "allow for the rest of the session": for an upload only
/// when the session has an upload directory (it then uploads from that directory unasked; a
/// grant without one would let any path go up unasked). The broker narrows it again.
fn offers_allow_for_session(class: ActionClass, policy: &SessionPolicy) -> bool {
    class != ActionClass::Upload || policy.upload_dir.is_some()
}

/// The address of the tab a command acts on (the one agent-browser is bound to, else the
/// session's only tab); `None` when there is none or it cannot be read in time.
async fn tab_url(sessions: &SessionManager, runner: &Runner, id: &SessionId) -> Option<String> {
    let target = screen_target(sessions, runner, id)?;
    target_url(sessions, id, &target).await
}

/// The address of the session's tab `target`; `None` when it cannot be read in time.
async fn target_url(sessions: &SessionManager, id: &SessionId, target: &str) -> Option<String> {
    tokio::time::timeout(PROBE_TIMEOUT, sessions.target_url(id, target))
        .await
        .ok()?
}

/// Holds an approved command to what the person was shown: a cookie set gets the asked
/// tab's address (`--url`) and the shown path (`--path`) when it gave none; a storage write
/// or an upload runs only while the tab is still at the asked origin (`now` reads its
/// address). False: it must not run (`stale`).
async fn apply_pin(
    pin: &StatePin,
    run_argv: &mut Vec<String>,
    now: impl Future<Output = Option<String>>,
) -> bool {
    match pin {
        StatePin::None => true,
        StatePin::Cookie { url, path } => {
            for (flag, value) in [("--url", url), ("--path", path)] {
                if let Some(v) = value {
                    run_argv.push(flag.to_string());
                    run_argv.push(v.clone());
                }
            }
            true
        }
        StatePin::Origin(origin) => now.await.is_some_and(|url| origin_of(&url) == *origin),
    }
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
    fn a_denied_command_logs_its_outcome_without_args() {
        use crate::actionlog::{read, Manifest, REDACTED};

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
        assert!(matches!(
            judged_verdict(&SessionPolicy::default(), &[], class),
            Judged::Ask
        ));
        let denied = denial(
            class,
            ConfirmOutcome::Denied,
            Some("0123456789abcdef0123456789abcdef"),
            Duration::from_secs(60),
        );
        let entry = ActionEntry {
            seq: 0,
            ts: now(),
            session_id: "s_abcdefgh".into(),
            command: argv[0].clone(),
            confirm: Some(ConfirmLog {
                outcome: ConfirmOutcome::Denied,
                by: Some(ConfirmBy::Api),
                wait_ms: 1234,
            }),
            ..ActionEntry::default()
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
        let line: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(
            line["confirm"],
            json!({ "outcome": "denied", "by": "api", "waitMs": 1234 }),
            "{line}"
        );
        // A command that asked nobody has no `confirm`.
        let plain = serde_json::to_value(ActionEntry::default()).unwrap();
        assert!(plain.get("confirm").is_none(), "{plain}");
    }

    #[test]
    fn denial_reasons_are_in_error_data() {
        let cid = "0123456789abcdef0123456789abcdef";
        for (outcome, reason, says) in [
            (
                ConfirmOutcome::Denied,
                "denied",
                "a person denied this state command",
            ),
            (
                ConfirmOutcome::TimedOut,
                "timed_out",
                "no answer within 60 s; the state command was denied",
            ),
            (
                ConfirmOutcome::NoResponder,
                "no_responder",
                "confirmation needs the Pegboard browser (or a client of confirm.subscribe)",
            ),
            (
                ConfirmOutcome::Cancelled,
                "cancelled",
                "the session closed (or the daemon stopped) while the state command waited",
            ),
            (
                ConfirmOutcome::Stale,
                "stale",
                "the page changed after a person approved; the state command was not run",
            ),
        ] {
            let e = denial(
                ActionClass::State,
                outcome,
                Some(cid),
                Duration::from_secs(60),
            );
            assert_eq!(e.code, codes::POLICY_DENIED);
            assert_eq!(e.data, Some(json!({ "reason": reason, "confirmId": cid })));
            assert_eq!(e.reason(), Some(reason));
            assert!(e.message.contains(says), "{}", e.message);
        }
        // No question was asked (the session closed first): no id.
        let e = denial(
            ActionClass::State,
            ConfirmOutcome::Cancelled,
            None,
            Duration::from_secs(60),
        );
        assert_eq!(e.data, Some(json!({ "reason": "cancelled" })));
        // The limit as it is (a test hook shortens it).
        let e = denial(
            ActionClass::Upload,
            ConfirmOutcome::TimedOut,
            None,
            Duration::from_millis(2500),
        );
        assert!(
            e.message.starts_with("no answer within 2.5 s; the upload"),
            "{}",
            e.message
        );
        // The no-responder hint still names the way around asking.
        let e = denial(
            ActionClass::State,
            ConfirmOutcome::NoResponder,
            None,
            Duration::from_secs(60),
        );
        assert!(e.message.contains("--allow state"), "{}", e.message);
    }

    #[tokio::test]
    async fn storage_write_after_navigation_is_stale() {
        let pin = StatePin::Origin("https://a.test".into());
        let mut run = words(&["storage", "local", "set", "k", "v"]);
        let at = |page: Option<&str>| {
            let now = page.map(String::from);
            async move { now }
        };
        // Same origin, another page: it runs, unchanged.
        assert!(apply_pin(&pin, &mut run, at(Some("https://a.test/other?x"))).await);
        assert_eq!(run, words(&["storage", "local", "set", "k", "v"]));
        // The tab elsewhere, or unreadable: it must not run.
        for page in [
            Some("https://b.test/"),
            Some("http://a.test/"),
            Some("https://a.test:8443/"),
            None,
        ] {
            assert!(!apply_pin(&pin, &mut run, at(page)).await, "{page:?}");
        }
        let e = denial(
            ActionClass::State,
            ConfirmOutcome::Stale,
            Some("c"),
            Duration::from_secs(60),
        );
        assert_eq!(e.reason(), Some("stale"));
    }

    #[tokio::test]
    async fn a_cookie_runs_at_the_address_it_was_asked_about() {
        let pin = StatePin::Cookie {
            url: Some("https://a.test/a/x".into()),
            path: Some("/a".into()),
        };
        let mut run = words(&["cookies", "set", "n", "v"]);
        // The tab has moved on meanwhile: what runs is still the asked address, with the
        // path that was shown.
        let moved = async { Some("https://evil.test/".to_string()) };
        assert!(apply_pin(&pin, &mut run, moved).await);
        assert_eq!(
            run,
            words(&[
                "cookies",
                "set",
                "n",
                "v",
                "--url",
                "https://a.test/a/x",
                "--path",
                "/a"
            ])
        );
        let mut run = words(&["cookies", "set", "n", "v"]);
        assert!(apply_pin(&StatePin::None, &mut run, async { None }).await);
        assert_eq!(run, words(&["cookies", "set", "n", "v"]));
    }

    fn question(session: &SessionId) -> ConfirmAsk {
        ConfirmAsk {
            session_id: session.clone(),
            session_name: "n".into(),
            profile: "default".into(),
            class: ActionClass::State,
            details: Vec::new(),
            offer_allow_for_session: true,
            place: None,
        }
    }

    fn prepared(
        session: &SessionId,
    ) -> impl Future<Output = Result<(ConfirmAsk, StatePin), RpcError>> {
        std::future::ready(Ok((question(session), origin_pin())))
    }

    fn origin_pin() -> StatePin {
        StatePin::Origin("https://a.test".into())
    }

    /// Answers the next confirmation that shows up with `answer`.
    async fn answer_next(broker: &ConfirmBroker, answer: pegboard_protocol::ConfirmAnswer) {
        loop {
            if let Some(r) = broker.pending().pop() {
                broker
                    .respond(
                        &pegboard_protocol::ConfirmRespondParams {
                            confirm_id: r.confirm_id,
                            answer,
                        },
                        ConfirmBy::Api,
                    )
                    .unwrap();
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn an_asking_command_waits_for_a_person() {
        use pegboard_protocol::ConfirmAnswer;
        let broker = ConfirmBroker::new(crate::confirm::CONFIRM_TIMEOUT);
        let id = SessionId("s_abcdefgh".into());
        // Nobody to ask: refused at once, never cleared.
        let r = clear_command(&broker, Judged::Ask, ActionClass::State, &id, prepared(&id)).await;
        let refused = r.expect_err("an asking command with nobody to ask must not run");
        assert_eq!(refused.error.reason(), Some("no_responder"));
        assert_eq!(
            refused.confirm.map(|c| c.outcome),
            Some(ConfirmOutcome::NoResponder)
        );

        let _sub = broker.subscribe().unwrap();
        // A person denies.
        let (r, ()) = tokio::join!(
            clear_command(&broker, Judged::Ask, ActionClass::State, &id, prepared(&id)),
            answer_next(&broker, ConfirmAnswer::Deny)
        );
        let refused = r.expect_err("denied");
        assert_eq!(refused.error.code, codes::POLICY_DENIED);
        assert_eq!(refused.error.reason(), Some("denied"));
        assert!(refused.error.data.as_ref().unwrap()["confirmId"].is_string());
        let c = refused.confirm.unwrap();
        assert_eq!(
            (c.outcome, c.by),
            (ConfirmOutcome::Denied, Some(ConfirmBy::Api))
        );
        // A person approves: cleared, held to what was asked.
        let (r, ()) = tokio::join!(
            clear_command(&broker, Judged::Ask, ActionClass::State, &id, prepared(&id)),
            answer_next(&broker, ConfirmAnswer::Approve)
        );
        let cleared = r.expect("approved");
        assert_eq!(cleared.pin, origin_pin());
        assert_eq!(cleared.confirm_id.map(|c| c.len()), Some(32));
        let c = cleared.confirm.unwrap();
        assert_eq!(
            (c.outcome, c.by),
            (ConfirmOutcome::Approved, Some(ConfirmBy::Api))
        );
        // Allowed without asking, and allowed by an earlier grant (logged as such).
        let cleared = clear_command(
            &broker,
            Judged::Allow,
            ActionClass::State,
            &id,
            prepared(&id),
        )
        .await
        .unwrap();
        assert!(cleared.confirm.is_none() && cleared.pin == StatePin::None);
        let cleared = clear_command(
            &broker,
            Judged::AllowedForSession,
            ActionClass::State,
            &id,
            prepared(&id),
        )
        .await
        .unwrap();
        let c = cleared.confirm.unwrap();
        assert_eq!(
            (c.outcome, c.by),
            (ConfirmOutcome::Approved, Some(ConfirmBy::Session))
        );
        assert!(broker.pending().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn closing_the_session_ends_a_command_still_preparing_its_question() {
        let broker = ConfirmBroker::new(crate::confirm::CONFIRM_TIMEOUT);
        let _sub = broker.subscribe().unwrap();
        let id = SessionId("s_abcdefgh".into());
        // Reading the tab never finishes.
        let stuck = std::future::pending::<Result<(ConfirmAsk, StatePin), RpcError>>();
        let both = async {
            tokio::join!(
                clear_command(&broker, Judged::Ask, ActionClass::State, &id, stuck),
                async {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    broker.cancel_session(&id);
                }
            )
        };
        let (r, ()) = tokio::time::timeout(Duration::from_secs(5), both)
            .await
            .expect("the close must end the command, not wait for its question");
        let refused = r.expect_err("cancelled");
        assert_eq!(refused.error.reason(), Some("cancelled"));
        assert!(broker.pending().is_empty());
        // And one that starts while the session closes ends at once too.
        let r = clear_command(&broker, Judged::Ask, ActionClass::State, &id, prepared(&id)).await;
        assert_eq!(r.unwrap_err().error.reason(), Some("cancelled"));
    }

    #[tokio::test]
    async fn where_it_writes_unknown_is_denied_without_asking() {
        let broker = ConfirmBroker::new(crate::confirm::CONFIRM_TIMEOUT);
        let mut sub = broker.subscribe().unwrap();
        let id = SessionId("s_abcdefgh".into());
        let unknown = std::future::ready(Err(unknown_origin(ActionClass::State)));
        let refused = clear_command(&broker, Judged::Ask, ActionClass::State, &id, unknown)
            .await
            .expect_err("unknown origin");
        assert_eq!(refused.error.code, codes::POLICY_DENIED);
        assert_eq!(refused.error.reason(), Some("unknown_origin"));
        assert!(
            refused.error.message.contains("select a tab"),
            "{}",
            refused.error.message
        );
        // Nobody was asked.
        assert!(refused.confirm.is_none());
        assert!(sub.try_recv().is_err(), "a person was asked");
        assert!(broker.pending().is_empty());
    }

    #[test]
    fn grants_from_the_broker_turn_asking_into_allowed() {
        let state = SessionPolicy::default();
        assert!(matches!(
            judged_verdict(&state, &[ActionClass::State], ActionClass::State),
            Judged::AllowedForSession
        ));
        assert!(matches!(
            judged_verdict(&state, &[ActionClass::State], ActionClass::Upload),
            Judged::Ask
        ));
        // An upload grant without the session's upload directory still asks.
        assert!(matches!(
            judged_verdict(&state, &[ActionClass::Upload], ActionClass::Upload),
            Judged::Ask
        ));
        let with_dir = SessionPolicy {
            upload_dir: Some("/tmp".into()),
            ..SessionPolicy::default()
        };
        assert!(matches!(
            judged_verdict(&with_dir, &[ActionClass::Upload], ActionClass::Upload),
            Judged::AllowedForSession
        ));
        // A grant never beats a denial, and --allow is no grant.
        let denied = SessionPolicy {
            deny: vec![ActionClass::State],
            ..SessionPolicy::default()
        };
        assert!(matches!(
            judged_verdict(&denied, &[ActionClass::State], ActionClass::State),
            Judged::Deny(_)
        ));
        let allowed = SessionPolicy {
            allow: vec![ActionClass::State],
            ..SessionPolicy::default()
        };
        assert!(matches!(
            judged_verdict(&allowed, &[], ActionClass::State),
            Judged::Allow
        ));
    }

    fn words(v: &[&str]) -> Vec<String> {
        v.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn upload_names_become_paths_and_a_missing_one_names_no_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (up, files) = (root.join("person-folder"), root.join("session-files"));
        std::fs::create_dir(&up).unwrap();
        std::fs::create_dir(&files).unwrap();
        std::fs::write(up.join("a.txt"), "a").unwrap();
        std::fs::write(files.join("b.txt"), "b").unwrap();
        let up_s = up.to_str().unwrap();
        let (run, names) = resolve_names(
            Some(up_s),
            files.clone(),
            &words(&["upload", "#f", "a.txt", "b.txt"]),
        )
        .unwrap();
        assert_eq!(
            run,
            [
                "upload".to_string(),
                "#f".into(),
                up.join("a.txt").display().to_string(),
                files.join("b.txt").display().to_string()
            ]
        );
        assert_eq!(names[0].1, "a.txt");
        let e = resolve_names(
            Some(up_s),
            files.clone(),
            &words(&["upload", "#f", "a.txt", "nope.txt"]),
        )
        .unwrap_err();
        assert_eq!(e.code, codes::ARGV_REJECTED);
        assert!(e.message.contains("nope.txt"), "{}", e.message);
        assert!(
            !e.message.contains("person-folder") && !e.message.contains("session-files"),
            "{}",
            e.message
        );
        // No upload directory in the policy: nothing is looked up.
        let e = resolve_names(None, files, &words(&["upload", "#f", "b.txt"])).unwrap_err();
        assert_eq!(e.code, codes::INTERNAL);
    }

    #[test]
    fn an_upload_is_logged_by_file_name_and_asks_by_default() {
        let argv = words(&["upload", "#f", "a.txt"]);
        assert_eq!(logged_args(&argv, false), argv);
        let class = argv::class_of(&argv).unwrap();
        assert!(matches!(
            judged_verdict(&Default::default(), &[], class),
            Judged::Ask
        ));
    }

    #[test]
    fn an_upload_with_nobody_to_answer_names_both_options() {
        let e = denial(
            ActionClass::Upload,
            ConfirmOutcome::NoResponder,
            None,
            crate::confirm::CONFIRM_TIMEOUT,
        );
        assert_eq!(e.code, codes::POLICY_DENIED);
        assert!(
            e.message
                .contains("confirmation needs the Pegboard browser"),
            "{}",
            e.message
        );
        assert!(
            e.message
                .contains("--allow upload and --upload-dir <directory>"),
            "{}",
            e.message
        );
        assert_eq!(e.data, Some(json!({ "reason": "no_responder" })));
        // A state command's names no directory.
        let e = denial(
            ActionClass::State,
            ConfirmOutcome::NoResponder,
            None,
            crate::confirm::CONFIRM_TIMEOUT,
        );
        assert!(!e.message.contains("--upload-dir"), "{}", e.message);
    }

    /// A temporary layout: a person's folder, the session's files, pegboard's home, and a
    /// folder elsewhere.
    struct UploadRig {
        _tmp: tempfile::TempDir,
        up: PathBuf,
        files: PathBuf,
        home: PathBuf,
        elsewhere: PathBuf,
    }

    fn upload_rig() -> UploadRig {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        let files = home.join("sessions/s_1/files");
        let (up, elsewhere) = (root.join("person-folder"), root.join("elsewhere"));
        for d in [&up, &files, &elsewhere] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(up.join("a.txt"), "a").unwrap();
        std::fs::write(files.join("b.txt"), "b").unwrap();
        std::fs::write(elsewhere.join("c.txt"), "c").unwrap();
        UploadRig {
            _tmp: tmp,
            up,
            files,
            home,
            elsewhere,
        }
    }

    #[test]
    fn absolute_paths_are_refused_when_the_session_has_an_upload_dir() {
        let r = upload_rig();
        let c = r.elsewhere.join("c.txt").display().to_string();
        let up = r.up.display().to_string();
        // Unasked (`--allow upload --upload-dir`): by name only, as before.
        let e = resolve_names(
            Some(&up),
            r.files.clone(),
            &words(&["upload", "#f", "a.txt", &c]),
        )
        .unwrap_err();
        assert_eq!(e.code, codes::ARGV_REJECTED);
        assert!(e.message.contains("only by file name"), "{}", e.message);
        assert!(!e.message.contains(&up), "{}", e.message);
        // Asking, with an upload directory: by name only too.
        let e = asked_upload_sources(
            Some(&up),
            r.files.clone(),
            vec![r.home.clone()],
            &words(&["upload", "#f", &c]),
        )
        .unwrap_err();
        assert_eq!(e.code, codes::ARGV_REJECTED);
        assert!(e.message.contains("only by file name"), "{}", e.message);
        // Asking, with an upload directory: a name is looked up there, then in the files,
        // and must be directly inside one of them when it is copied.
        let sources = asked_upload_sources(
            Some(&up),
            r.files.clone(),
            vec![r.home.clone()],
            &words(&["upload", "#f", "a.txt", "b.txt"]),
        )
        .unwrap();
        let dirs = vec![r.up.clone(), r.files.clone()];
        let src = |given: &str, path: PathBuf, allowed| staging::Source {
            given: given.into(),
            path,
            allowed,
        };
        assert_eq!(
            sources,
            [
                src(
                    "a.txt",
                    r.up.join("a.txt"),
                    upload::Allowed::DirectlyIn(dirs.clone())
                ),
                src(
                    "b.txt",
                    r.files.join("b.txt"),
                    upload::Allowed::DirectlyIn(dirs)
                ),
            ]
        );
    }

    #[test]
    fn an_asking_session_without_a_directory_takes_absolute_paths_outside_pegboard() {
        let r = upload_rig();
        let forbidden = vec![r.home.clone()];
        let c = r.elsewhere.join("c.txt").display().to_string();
        let sources = asked_upload_sources(
            None,
            r.files.clone(),
            forbidden.clone(),
            &words(&["upload", "#f", "b.txt", &c]),
        )
        .unwrap();
        assert_eq!(
            sources,
            [
                staging::Source {
                    given: "b.txt".into(),
                    path: r.files.join("b.txt"),
                    allowed: upload::Allowed::DirectlyIn(vec![r.files.clone()])
                },
                staging::Source {
                    given: c.clone(),
                    path: r.elsewhere.join("c.txt"),
                    allowed: upload::Allowed::Outside(forbidden.clone())
                },
            ]
        );
        // A name is looked up in the session's files only.
        let e = asked_upload_sources(
            None,
            r.files.clone(),
            forbidden.clone(),
            &words(&["upload", "#f", "a.txt"]),
        )
        .unwrap_err();
        assert!(e.message.contains("absolute path"), "{}", e.message);
        // Inside pegboard's home (another session's files), through its absolute path.
        let other = r
            .home
            .join("sessions/s_1/files/b.txt")
            .display()
            .to_string();
        let e = asked_upload_sources(
            None,
            r.files.clone(),
            forbidden.clone(),
            &words(&["upload", "#f", &other]),
        )
        .unwrap_err();
        // The agent is told the same as for any other refused file, quoting its own text.
        assert_eq!(e.code, codes::ARGV_REJECTED);
        assert_eq!(e.message, format!("{other}: {}", upload::NOT_TAKEN));
        for (bad, made) in [
            (r.elsewhere.join("missing.txt"), false),
            (r.elsewhere.clone(), false),
            (r.elsewhere.join("link"), true),
        ] {
            if made {
                std::os::unix::fs::symlink(r.elsewhere.join("c.txt"), &bad).unwrap();
            }
            let bad = bad.display().to_string();
            let e = asked_upload_sources(
                None,
                r.files.clone(),
                forbidden.clone(),
                &words(&["upload", "#f", &bad]),
            )
            .unwrap_err();
            assert_eq!(e.message, format!("{bad}: {}", upload::NOT_TAKEN));
        }
        // Seventeen files.
        let many: Vec<String> =
            std::iter::repeat_n(c.clone(), upload::MAX_UPLOAD_FILES + 1).collect();
        let mut line = words(&["upload", "#f"]);
        line.extend(many);
        let e = asked_upload_sources(None, r.files.clone(), forbidden.clone(), &line).unwrap_err();
        assert!(e.message.contains("at most 16 files"), "{}", e.message);
        line.pop();
        assert!(asked_upload_sources(None, r.files.clone(), forbidden, &line).is_ok());
    }

    #[test]
    fn allow_for_session_is_offered_only_with_an_upload_dir() {
        let plain = SessionPolicy::default();
        let with_dir = SessionPolicy {
            upload_dir: Some("/up".into()),
            ..SessionPolicy::default()
        };
        assert!(!offers_allow_for_session(ActionClass::Upload, &plain));
        assert!(offers_allow_for_session(ActionClass::Upload, &with_dir));
        assert!(offers_allow_for_session(ActionClass::State, &plain));
    }

    #[test]
    fn a_write_in_a_selected_frame_is_not_asked_about() {
        assert!(acts_in_a_frame(
            ActionClass::Upload,
            &words(&["upload", "#f", "/a"])
        ));
        assert!(acts_in_a_frame(
            ActionClass::State,
            &words(&["storage", "local", "clear"])
        ));
        // A cookie is set for the pinned tab address, whatever frame is selected.
        assert!(!acts_in_a_frame(
            ActionClass::State,
            &words(&["cookies", "set", "a", "b"])
        ));
        let e = frame_selected(ActionClass::Upload);
        assert_eq!(e.code, codes::POLICY_DENIED);
        assert_eq!(e.reason(), Some("unknown_origin"));
        assert!(e.message.contains("frame main"), "{}", e.message);
    }

    #[test]
    fn hide_paths_covers_staged_paths() {
        let names = vec![
            ("/Users/p/link/a.txt".to_string(), "a.txt".to_string()),
            ("/Users/p/real/a.txt".to_string(), "a.txt".to_string()),
            (
                "/h/sessions/s_1/confirm/0f/0/a.txt".to_string(),
                "a.txt".to_string(),
            ),
        ];
        let mut out = Ok(CommandExecResult {
            exit_code: 0,
            output: json!({"data": {"files": [
                "/h/sessions/s_1/confirm/0f/0/a.txt", "/Users/p/real/a.txt"
            ]}}),
            stderr: "set /h/sessions/s_1/confirm/0f/0/a.txt".into(),
        });
        hide_paths(&mut out, &names);
        let r = out.unwrap();
        assert_eq!(r.output["data"]["files"], json!(["a.txt", "a.txt"]));
        assert_eq!(r.stderr, "set a.txt");
    }

    #[test]
    fn the_agent_never_gets_an_uploaded_files_path_back() {
        let names = vec![("/p/dir/a.txt".to_string(), "a.txt".to_string())];
        let mut out = Ok(CommandExecResult {
            exit_code: 0,
            output: json!({"success": true, "data": {"uploaded": ["/p/dir/a.txt"]}, "n": 1}),
            stderr: "read /p/dir/a.txt".into(),
        });
        hide_paths(&mut out, &names);
        let r = out.unwrap();
        assert_eq!(r.output["data"]["uploaded"][0], "a.txt");
        assert_eq!(r.stderr, "read a.txt");
        let mut err: Result<CommandExecResult, RpcError> = Err(RpcError {
            code: "x".into(),
            message: "no /p/dir/a.txt".into(),
            data: None,
        });
        hide_paths(&mut err, &names);
        assert_eq!(err.unwrap_err().message, "no a.txt");
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
