// Shared browser: every test here uses the binary's one browser (own session per test, closed at the end).
#![allow(dead_code)]
mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pegboard_core::actionlog::{self, ActionEntry, ActionLog, Manifest, REDACTED};
use pegboard_core::daemon::{run_command, run_command_with_hook};
use pegboard_core::proxy::server::Endpoint;
use pegboard_core::runner::Runner;
use pegboard_protocol::{codes, CommandExecResult, RpcError, SessionId, SessionInfo};
use serde_json::{json, Value};

const BIN: &str = "vendor/agent-browser/agent-browser";

/// A test's view of the binary's shared browser, proxy, session manager and runner: it owns the
/// sessions and agent-browser daemons the test starts. Dropping it (also on test failure) kills
/// those daemons and closes those sessions; `finish` does so and checks that nothing is left.
struct Harness {
    sh: &'static common::Shared,
    daemons: Daemons,
    sessions: Mutex<Vec<SessionId>>,
}

impl std::ops::Deref for Harness {
    type Target = common::Shared;
    fn deref(&self) -> &common::Shared {
        self.sh
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let ids = std::mem::take(&mut *self.sessions.lock().unwrap());
        if ids.is_empty() {
            return;
        }
        let sh = self.sh;
        common::spawn_shared(async move {
            for id in ids {
                sh.close_session(&id).await;
            }
        });
    }
}

/// Sessions attached in a test, with their daemon's pid; kills those daemons when dropped.
struct Daemons {
    runner: Arc<Runner>,
    list: Mutex<Vec<(SessionId, i32)>>,
}

impl Daemons {
    fn add(&self, id: &SessionId) {
        if let Some(pid) = self.runner.daemon_pid(id) {
            self.list.lock().unwrap().push((id.clone(), pid));
        }
    }

    fn get(&self) -> Vec<(SessionId, i32)> {
        self.list.lock().unwrap().clone()
    }
}

impl Drop for Daemons {
    fn drop(&mut self) {
        for (id, _) in self.list.lock().unwrap().iter() {
            self.runner.kill_daemon(id);
        }
    }
}

impl Harness {
    /// Call inside `common::run`.
    fn start() -> Harness {
        let sh = common::shared();
        Harness {
            sh,
            daemons: Daemons {
                runner: sh.runner.clone(),
                list: Mutex::new(Vec::new()),
            },
            sessions: Mutex::new(Vec::new()),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.fixture.origin)
    }

    /// A session on `path`, without agent-browser.
    async fn create(&self, path: &str) -> (SessionInfo, Endpoint) {
        let (info, ep) = self.sh.session(path).await;
        self.sessions.lock().unwrap().push(info.id.clone());
        (info, ep)
    }

    /// A session on `path` with agent-browser attached.
    async fn session(&self, path: &str) -> (SessionInfo, Endpoint) {
        let (info, ep) = self.create(path).await;
        self.attach(&info.id, &ep).await;
        (info, ep)
    }

    async fn attach(&self, id: &SessionId, ep: &Endpoint) {
        let attached = self.runner.attach(id, ep).await;
        self.daemons.add(id);
        attached.unwrap();
    }

    async fn exec(&self, info: &SessionInfo, ep: &Endpoint, argv: &[&str]) -> CommandExecResult {
        self.runner
            .exec(&info.id, ep, &s(argv), 30_000)
            .await
            .unwrap_or_else(|e| panic!("{argv:?}: {e:?}"))
    }

    /// Evaluates on a fresh session of the test's own (browser-level) connection.
    async fn eval(&self, target: &str, expr: &str) -> Value {
        let conn = &self.browser.conn;
        let s = conn.attach(target).await.unwrap();
        let v = conn.evaluate(&s, expr).await.unwrap();
        let _ = conn
            .send("Target.detachFromTarget", json!({ "sessionId": s }), None)
            .await;
        v
    }

    /// Detaches every daemon, closes the test's sessions and checks that no daemon of this
    /// test and no tab of this test's sessions is left.
    async fn finish(self) {
        let daemons = self.daemons.get();
        for (id, _) in &daemons {
            self.runner.detach(id).await.unwrap();
        }
        let ids = std::mem::take(&mut *self.sessions.lock().unwrap());
        let mut targets = Vec::new();
        for id in &ids {
            targets.extend(self.sh.close_session(id).await);
        }
        let conn = &self.browser.conn;
        let gone = eventually(5, || async {
            daemons.iter().all(|(_, pid)| !alive(*pid))
                && pegboard_core::cdp::page_targets(conn)
                    .await
                    .unwrap()
                    .iter()
                    .all(|t| !targets.contains(&t.target_id))
        })
        .await;
        assert!(
            gone,
            "left behind: daemons {daemons:?}, tabs {targets:?} of the test's sessions"
        );
    }
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn ok(r: &CommandExecResult) -> bool {
    r.exit_code == 0 && r.output["success"] == json!(true)
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Polls `check` every 100 ms for up to `secs` seconds.
async fn eventually<F, Fut>(secs: u64, mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        if check().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A session on `path` with agent-browser attached and its action log open.
async fn logged(h: &Harness, path: &str) -> (SessionInfo, Endpoint, ActionLog) {
    let (info, ep) = h.session(path).await;
    let manifest = Manifest::new(
        &info.recording_id,
        &info.id.0,
        &info.name,
        &info.profile,
        &info.created_at,
        "test",
        "0.42.0",
    );
    let log = ActionLog::open(&h.env.paths, manifest).unwrap();
    (info, ep, log)
}

impl Harness {
    async fn run(
        &self,
        log: &mut ActionLog,
        info: &SessionInfo,
        argv: &[&str],
    ) -> Result<CommandExecResult, RpcError> {
        run_command(
            &self.manager,
            &self.runner,
            &self.env.paths,
            log,
            &info.id,
            &s(argv),
            Some(30_000),
        )
        .await
    }

    fn entries(&self, info: &SessionInfo) -> Vec<ActionEntry> {
        actionlog::read(&self.env.paths, &info.recording_id)
            .unwrap()
            .1
    }

    fn log_text(&self, info: &SessionInfo) -> String {
        let dir = self.env.paths.recordings.join(&info.recording_id);
        let mut all = std::fs::read_to_string(dir.join("actions.jsonl")).unwrap();
        all.push_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap());
        all
    }
}

#[test]
#[ignore]
fn password_text_never_reaches_the_log() {
    common::run(async {
        let h = Harness::start();
        let (info, _ep, mut log) = logged(&h, "/form").await;
        let r = h
            .run(&mut log, &info, &["fill", "#pw", "hunter2-secret"])
            .await
            .unwrap();
        assert!(ok(&r), "{r:?}");
        let r = h
            .run(&mut log, &info, &["fill", "#name", "alice"])
            .await
            .unwrap();
        assert!(ok(&r), "{r:?}");
        let text = h.log_text(&info);
        assert!(!text.contains("hunter2-secret"), "{text}");
        let entries = h.entries(&info);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].redacted);
        assert_eq!(entries[0].args, s(&["fill", "#pw", REDACTED]));
        assert!(!entries[1].redacted);
        assert_eq!(entries[1].args, s(&["fill", "#name", "alice"]));
        // Output is never logged: the filled page text is not in the file either.
        h.finish().await;
    });
}

#[test]
#[ignore]
fn failed_sensitivity_check_redacts() {
    common::run(async {
        let h = Harness::start();
        let (info, _ep, mut log) = logged(&h, "/form").await;
        let target = info.targets[0].clone();
        let conn = h.browser.conn.clone();
        let proxy = h.proxy.clone();
        let id = info.id.clone();
        // The tab goes away right after the fill completes, before the focus is looked at.
        let hook = async move {
            conn.send("Target.closeTarget", json!({ "targetId": target }), None)
                .await
                .unwrap();
            for _ in 0..50 {
                if proxy.targets(&id).is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        let r = run_command_with_hook(
            &h.manager,
            &h.runner,
            &h.env.paths,
            &mut log,
            &info.id,
            &s(&["fill", "#name", "plain-but-unverifiable"]),
            Some(30_000),
            hook,
        )
        .await
        .unwrap();
        assert!(ok(&r), "{r:?}");
        let entries = h.entries(&info);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].redacted, "{:?}", entries[0]);
        assert_eq!(entries[0].args, s(&["fill", "#name", REDACTED]));
        assert!(!h.log_text(&info).contains("plain-but-unverifiable"));
        h.finish().await;
    });
}

#[test]
#[ignore]
fn every_command_is_logged_with_url_and_outcome() {
    common::run(async {
        let h = Harness::start();
        let (info, _ep, mut log) = logged(&h, "/form").await;
        let url = h.url("/counter");
        assert!(ok(&h.run(&mut log, &info, &["open", &url]).await.unwrap()));
        assert!(ok(&h
            .run(&mut log, &info, &["snapshot", "-i"])
            .await
            .unwrap()));
        let r = h.run(&mut log, &info, &["click", "#nope"]).await.unwrap();
        assert!(!ok(&r), "{r:?}");
        let entries = h.entries(&info);
        assert_eq!(
            entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(
            entries
                .iter()
                .map(|e| e.command.as_str())
                .collect::<Vec<_>>(),
            vec!["open", "snapshot", "click"]
        );
        assert!(
            entries[1]
                .url
                .as_deref()
                .is_some_and(|u| u.ends_with("/counter")),
            "{:?}",
            entries[1]
        );
        assert_eq!(
            entries[1].target_id.as_deref(),
            Some(info.targets[0].as_str())
        );
        assert!(entries[0].ok && entries[1].ok);
        assert!(!entries[2].ok);
        assert!(entries[2].error.as_deref().is_some_and(|e| !e.is_empty()));
        assert!(entries
            .iter()
            .all(|e| e.session_id == info.id.0 && !e.redacted));
        // The snapshot's output (page text) is not in the log.
        assert!(!h.log_text(&info).contains("Add"), "{}", h.log_text(&info));
        h.finish().await;
    });
}

#[test]
#[ignore]
fn refused_command_is_logged_and_never_runs() {
    common::run(async {
        let h = Harness::start();
        let (info, _ep, mut log) = logged(&h, "/form").await;
        let e = h
            .run(&mut log, &info, &["fill", "#pw", "-hunter2-secret"])
            .await
            .unwrap_err();
        assert_eq!(e.code, codes::ARGV_REJECTED);
        let e = h
            .run(&mut log, &info, &["open", "file:///etc/passwd"])
            .await
            .unwrap_err();
        assert_eq!(e.code, codes::ARGV_REJECTED);
        let entries = h.entries(&info);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| !e.ok && e.error.is_some()));
        assert!(!h.log_text(&info).contains("hunter2-secret"));
        // No agent-browser command ran for them: the page still has its first tab untouched.
        assert!(h.entries(&info).iter().all(|e| e.url.is_none()));
        h.finish().await;
    });
}

#[test]
#[ignore]
fn tab_new_goes_through_agent_browser() {
    common::run(async {
        let h = Harness::start();
        let (info, _ep, mut log) = logged(&h, "/form").await;
        let url = h.url("/counter");
        let r = h.run(&mut log, &info, &["tab", "new", &url]).await.unwrap();
        assert!(ok(&r), "{r:?}");
        assert_eq!(h.manager.info(&info.id).unwrap().targets.len(), 2);
        let entries = h.entries(&info);
        assert_eq!(entries[0].args, s(&["tab", "new", &url]));
        assert!(entries[0].ok);
        h.finish().await;
    });
}

/// Focuses `js` (a script that focuses a password input) and types into it.
async fn typed_into_focused_password(path: &str, focus_js: &str) {
    let h = Harness::start();
    let (info, _ep, mut log) = logged(&h, path).await;
    let r = h.run(&mut log, &info, &["eval", focus_js]).await.unwrap();
    assert!(ok(&r), "{r:?}");
    let r = h
        .run(&mut log, &info, &["keyboard", "type", "deep-secret-9"])
        .await
        .unwrap();
    assert!(ok(&r), "{r:?}");
    let entries = h.entries(&info);
    assert_eq!(entries.len(), 2);
    assert!(entries[1].redacted, "{:?}", entries[1]);
    assert_eq!(entries[1].args, s(&["keyboard", "type", REDACTED]));
    assert!(!h.log_text(&info).contains("deep-secret-9"));
    h.finish().await;
}

#[test]
#[ignore]
fn password_inside_shadow_root_is_redacted() {
    common::run(async {
        typed_into_focused_password(
            "/form-shadow",
            "document.getElementById('host').shadowRoot.getElementById('spw').focus()",
        )
        .await;
    });
}

#[test]
#[ignore]
fn password_inside_same_origin_iframe_is_redacted() {
    common::run(async {
        typed_into_focused_password(
            "/form-iframe",
            "document.getElementById('fr').contentDocument.getElementById('pw').focus()",
        )
        .await;
    });
}

#[test]
#[ignore]
fn plain_input_inside_shadow_root_is_not_redacted() {
    common::run(async {
        let h = Harness::start();
        let (info, _ep, mut log) = logged(&h, "/form-shadow").await;
        let focus = "document.getElementById('host').shadowRoot.getElementById('stext').focus()";
        assert!(ok(&h.run(&mut log, &info, &["eval", focus]).await.unwrap()));
        assert!(ok(&h
            .run(&mut log, &info, &["keyboard", "type", "visible-text"])
            .await
            .unwrap()));
        let entries = h.entries(&info);
        assert!(!entries[1].redacted, "{:?}", entries[1]);
        assert_eq!(entries[1].args, s(&["keyboard", "type", "visible-text"]));
        h.finish().await;
    });
}

#[test]
#[ignore]
fn failed_fill_into_password_field_is_redacted() {
    common::run(async {
        let h = Harness::start();
        let (info, _ep, mut log) = logged(&h, "/form").await;
        // The element is not found: the command fails and the focus never moved.
        let r = h
            .run(&mut log, &info, &["fill", "#nope", "hunter2-secret"])
            .await;
        assert!(r.as_ref().map_or(true, |r| !ok(r)), "{r:?}");
        // A reference that does not exist.
        let r = h
            .run(&mut log, &info, &["fill", "@e99", "hunter2-secret"])
            .await;
        assert!(r.as_ref().map_or(true, |r| !ok(r)), "{r:?}");
        assert!(
            !h.log_text(&info).contains("hunter2-secret"),
            "{}",
            h.log_text(&info)
        );
        let entries = h.entries(&info);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.redacted && !e.ok));
        h.finish().await;
    });
}

/// The form page went on to /counter (the submit is asynchronous).
async fn submitted(h: &Harness, info: &SessionInfo) -> bool {
    eventually(5, || async {
        h.eval(&info.targets[0], "location.pathname").await == json!("/counter")
    })
    .await
}

#[test]
#[ignore]
fn type_with_newline_that_submits_is_redacted() {
    common::run(async {
        let h = Harness::start();
        let (info, _ep, mut log) = logged(&h, "/form-submit").await;
        let r = h
            .run(&mut log, &info, &["type", "#pw", "hunter2-secret\n"])
            .await
            .unwrap();
        assert!(ok(&r), "{r:?}");
        assert!(!h.log_text(&info).contains("hunter2-secret"));
        let entries = h.entries(&info);
        assert!(entries[0].redacted);
        assert!(submitted(&h, &info).await, "the form did not submit");
        h.finish().await;
    });
}

#[test]
#[ignore]
fn keyboard_type_into_focused_password_that_submits_is_redacted() {
    common::run(async {
        let h = Harness::start();
        let (info, _ep, mut log) = logged(&h, "/form-submit").await;
        assert!(ok(&h
            .run(&mut log, &info, &["click", "#pw"])
            .await
            .unwrap()));
        let _ = h
            .run(&mut log, &info, &["keyboard", "type", "hunter2-secret\n"])
            .await;
        assert!(!h.log_text(&info).contains("hunter2-secret"));
        let entries = h.entries(&info);
        assert!(entries[1].redacted);
        assert!(submitted(&h, &info).await, "the form did not submit");
        h.finish().await;
    });
}

#[test]
#[ignore]
fn otp_field_that_blurs_is_redacted() {
    common::run(async {
        let h = Harness::start();
        let (info, _ep, mut log) = logged(&h, "/form-otp").await;
        assert!(ok(&h
            .run(&mut log, &info, &["click", "#otp"])
            .await
            .unwrap()));
        assert!(ok(&h
            .run(&mut log, &info, &["keyboard", "type", "424242"])
            .await
            .unwrap()));
        // The same field through `fill`: the checks before and after both see it as sensitive.
        assert!(ok(&h
            .run(&mut log, &info, &["fill", "#otp", "737373"])
            .await
            .unwrap()));
        let text = h.log_text(&info);
        assert!(
            !text.contains("424242") && !text.contains("737373"),
            "{text}"
        );
        h.finish().await;
    });
}

#[test]
#[ignore]
fn press_key_by_key_into_password_is_redacted() {
    common::run(async {
        let h = Harness::start();
        let (info, _ep, mut log) = logged(&h, "/form").await;
        assert!(ok(&h
            .run(&mut log, &info, &["click", "#pw"])
            .await
            .unwrap()));
        assert!(ok(&h.run(&mut log, &info, &["press", "h"]).await.unwrap()));
        assert!(ok(&h.run(&mut log, &info, &["press", "u"]).await.unwrap()));
        assert!(ok(&h
            .run(&mut log, &info, &["press", "Tab"])
            .await
            .unwrap()));
        let entries = h.entries(&info);
        assert_eq!(entries[1].args, s(&["press", REDACTED]));
        assert_eq!(entries[2].args, s(&["press", REDACTED]));
        assert_eq!(entries[3].args, s(&["press", "Tab"]));
        h.finish().await;
    });
}

#[test]
#[ignore]
fn non_sensitive_text_is_still_logged() {
    common::run(async {
        let h = Harness::start();
        let (info, _ep, mut log) = logged(&h, "/form-submit").await;
        assert!(ok(&h
            .run(&mut log, &info, &["fill", "#name", "alice"])
            .await
            .unwrap()));
        // Typed into a plain field, with a trailing newline: both checks pass, so the text is
        // logged (this field does not submit the form; a submit would leave nothing focused
        // on the new page, which also passes the check after the command).
        assert!(ok(&h
            .run(&mut log, &info, &["type", "#name", "bob\n"])
            .await
            .unwrap_or_else(|e| panic!("{e:?}"))));
        let entries = h.entries(&info);
        assert_eq!(entries[0].args, s(&["fill", "#name", "alice"]));
        assert!(!entries[0].redacted);
        assert_eq!(entries[1].args, s(&["type", "#name", "bob\n"]));
        assert!(!entries[1].redacted);
        h.finish().await;
    });
}

#[test]
#[ignore]
fn cookie_value_never_reaches_the_log() {
    common::run(async {
        let h = Harness::start();
        let (info, _ep, mut log) = logged(&h, "/form").await;
        let r = h
            .run(
                &mut log,
                &info,
                &["cookies", "set", "sid", "cookie-secret-77"],
            )
            .await
            .unwrap();
        assert!(ok(&r), "{r:?}");
        let _ = h
            .run(
                &mut log,
                &info,
                &["storage", "local", "set", "k", "storage-secret-88"],
            )
            .await;
        let text = h.log_text(&info);
        assert!(
            !text.contains("cookie-secret-77") && !text.contains("storage-secret-88"),
            "{text}"
        );
        assert_eq!(
            h.entries(&info)[0].args,
            s(&["cookies", "set", "sid", REDACTED])
        );
        h.finish().await;
    });
}

/// A password field with no autocomplete that blurs itself on input: after the fill nothing
/// is focused, so only the check of the selector's `type` before the command can tell.
#[test]
#[ignore]
fn fill_into_password_that_blurs_on_input_is_redacted() {
    common::run(async {
        let h = Harness::start();
        let (info, _ep, mut log) = logged(&h, "/form-blur").await;
        let r = h
            .run(&mut log, &info, &["fill", "#pw", "hunter2-secret"])
            .await
            .unwrap();
        assert!(ok(&r), "{r:?}");
        assert!(!h.log_text(&info).contains("hunter2-secret"));
        let entries = h.entries(&info);
        assert!(entries[0].redacted);
        assert_eq!(entries[0].args, s(&["fill", "#pw", REDACTED]));
        h.finish().await;
    });
}

#[test]
#[ignore]
fn rejected_set_credentials_never_reaches_the_log() {
    common::run(async {
        let h = Harness::start();
        let (info, _ep, mut log) = logged(&h, "/form").await;
        for argv in [
            &["set", "credentials", "admin", "hunter2-secret"][..],
            &[
                "set",
                "headers",
                r#"{"Authorization":"Bearer hunter2-secret"}"#,
            ],
        ] {
            let e = h.run(&mut log, &info, argv).await.unwrap_err();
            assert_eq!(e.code, codes::ARGV_REJECTED);
        }
        let text = h.log_text(&info);
        assert!(
            !text.contains("hunter2-secret") && !text.contains("admin"),
            "{text}"
        );
        let entries = h.entries(&info);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.redacted && !e.ok));
        assert_eq!(entries[0].args, s(&["set", REDACTED, REDACTED, REDACTED]));
        h.finish().await;
    });
}
