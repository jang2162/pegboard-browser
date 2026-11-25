//! End-to-end tests of a person's confirmation on the command path, in stock mode: a client
//! of `confirm.subscribe` on the daemon's socket plays the person (stock mode has no
//! confirmation window). They start a browser, so they are `#[ignore]`d:
//! `PEGBOARD_TEST_HOOKS=1 cargo test -p pegboard-core --test e2e_confirm -- --ignored --test-threads=1`.
//!
//! Each test starts a daemon of its own (in this process, its API served on a socket in a
//! temporary home) and stops it at the end.
//!
//! With `PEGBOARD_TEST_BROWSER=fork:<Pegboard.app>` the daemon launches the fork, whose
//! confirmation dialog is a responder beside the API's subscribers. The `fork_*` tests are
//! for that configuration only (skipped in stock mode): the browser's test hooks
//! (`PEGBOARD_TEST_DIALOG_*`, in the browser's own environment) press its buttons and log
//! what it showed. The tests that need nobody to answer are for stock mode only.

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pegboard_core::service::{Daemon, DaemonStart};
use pegboard_protocol::{Request, Response};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::Instant;

/// An environment variable set until this is dropped (also when the test fails), then put
/// back as it was.
struct EnvVar {
    name: &'static str,
    before: Option<std::ffi::OsString>,
}

impl EnvVar {
    fn set(name: &'static str, value: &str) -> EnvVar {
        let before = std::env::var_os(name);
        std::env::set_var(name, value);
        EnvVar { name, before }
    }
}

impl Drop for EnvVar {
    fn drop(&mut self) {
        match &self.before {
            Some(v) => std::env::set_var(self.name, v),
            None => std::env::remove_var(self.name),
        }
    }
}

struct Rig {
    /// The daemon launched the Pegboard fork (which has a confirmation dialog).
    fork: bool,
    daemon: Arc<Daemon>,
    env: common::Env,
    fixture: common::Fixture,
    serve: tokio::task::JoinHandle<()>,
    next: AtomicU64,
    _vars: Vec<EnvVar>,
}

/// A session of the rig: its id and token.
#[derive(Clone)]
struct Session {
    id: String,
    token: String,
}

/// The daemon, its API on its socket, and the test pages. `timeout_ms` shortens the time a
/// person has (the test hook).
async fn rig(timeout_ms: Option<u64>) -> Rig {
    rig_with(timeout_ms, Vec::new()).await
}

/// The rig with the variables `vars` set until it finishes (the browser it launches keeps
/// them).
async fn rig_with(timeout_ms: Option<u64>, extra: Vec<EnvVar>) -> Rig {
    // Zero (or none) is the full time a person has; not inherited from the caller.
    let mut vars = vec![
        EnvVar::set("PEGBOARD_TEST_HOOKS", "1"),
        EnvVar::set(
            "PEGBOARD_TEST_CONFIRM_TIMEOUT_MS",
            &timeout_ms.unwrap_or(0).to_string(),
        ),
    ];
    vars.extend(extra);
    let env = common::env();
    let fork = matches!(
        env.cfg,
        pegboard_core::browser::BrowserConfig::ForkLaunched { .. }
    );
    let fixture = common::fixture().await;
    let daemon = Daemon::start(env.paths.clone(), DaemonStart::Launch(env.cfg.clone()))
        .await
        .unwrap();
    let (paths, d) = (env.paths.clone(), daemon.clone());
    let serve = tokio::spawn(async move {
        pegboard_core::api::serve(&paths, d).await.unwrap();
    });
    // The socket is there once the server listens.
    let deadline = Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(&env.paths.socket).await.is_err() {
        assert!(Instant::now() < deadline, "the API socket did not come up");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Rig {
        fork,
        daemon,
        env,
        fixture,
        serve,
        next: AtomicU64::new(1),
        _vars: vars,
    }
}

impl Rig {
    async fn call(&self, method: &str, params: Value) -> Response {
        call(
            &self.daemon,
            self.next.fetch_add(1, Ordering::SeqCst),
            method,
            params,
        )
        .await
    }

    async fn session(&self) -> Session {
        self.session_at("/counter").await
    }

    /// A session named `name` in the agent profile `profile`, at `path`.
    async fn session_named(&self, path: &str, name: &str, profile: &str) -> Session {
        // Made when missing (an existing one is an error, which does not matter here).
        let _ = self
            .call("profile.create", json!({ "name": profile }))
            .await;
        let r = self
            .call(
                "session.create",
                json!({ "url": format!("{}{path}", self.fixture.origin), "name": name,
                        "profile": profile }),
            )
            .await;
        let info = r.result.unwrap_or_else(|| panic!("{:?}", r.error));
        Session {
            id: info["id"].as_str().unwrap().to_string(),
            token: info["token"].as_str().unwrap().to_string(),
        }
    }

    async fn session_at(&self, path: &str) -> Session {
        let r = self
            .call(
                "session.create",
                json!({ "url": format!("{}{path}", self.fixture.origin) }),
            )
            .await;
        let info = r.result.unwrap_or_else(|| panic!("{:?}", r.error));
        Session {
            id: info["id"].as_str().unwrap().to_string(),
            token: info["token"].as_str().unwrap().to_string(),
        }
    }

    /// A session at `path` with `policy` (the API's camelCase form).
    async fn session_with(&self, path: &str, policy: Value) -> Session {
        let r = self
            .call(
                "session.create",
                json!({ "url": format!("{}{path}", self.fixture.origin), "policy": policy }),
            )
            .await;
        let info = r.result.unwrap_or_else(|| panic!("{:?}", r.error));
        Session {
            id: info["id"].as_str().unwrap().to_string(),
            token: info["token"].as_str().unwrap().to_string(),
        }
    }

    /// What the `/upload` page's file input read (`name:size:text|…`), once it has.
    async fn uploaded(&self, s: &Session) -> Value {
        self.ok(s, &["wait", "--fn", "window.__got !== undefined"])
            .await;
        self.ok(s, &["eval", "window.__got"]).await["data"]["result"].clone()
    }

    /// The copies made for a person's confirmation that are still there.
    fn copies_left(&self, s: &Session) -> usize {
        let dir = self
            .env
            .paths
            .session_confirm_dir(&pegboard_protocol::SessionId(s.id.clone()));
        std::fs::read_dir(dir).map_or(0, |d| d.count())
    }

    /// Runs a command in a task of its own; its response when awaited.
    fn exec(&self, s: &Session, argv: &[&str]) -> tokio::task::JoinHandle<Response> {
        let daemon = self.daemon.clone();
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let params =
            json!({ "sessionId": s.id, "token": s.token, "argv": argv, "timeoutMs": 30000 });
        tokio::spawn(async move { call(&daemon, id, "command.exec", params).await })
    }

    /// A command that must succeed; its output.
    async fn ok(&self, s: &Session, argv: &[&str]) -> Value {
        let r = self.exec(s, argv).await.unwrap();
        let result = r
            .result
            .unwrap_or_else(|| panic!("{argv:?}: {:?}", r.error));
        assert_eq!(result["exitCode"], 0, "{argv:?}: {result}");
        result["output"].clone()
    }

    async fn local_storage(&self, s: &Session, key: &str) -> Value {
        let out = self
            .ok(s, &["eval", &format!("localStorage.getItem('{key}')")])
            .await;
        out["data"]["result"].clone()
    }

    fn entries(&self, s: &Session) -> Vec<pegboard_core::actionlog::ActionEntry> {
        let rid = format!("r_{}", &s.id[2..]);
        pegboard_core::actionlog::read(&self.env.paths, &rid)
            .unwrap()
            .1
    }

    async fn subscribe(&self) -> Person {
        Person::subscribe(&self.env.paths.socket).await
    }

    async fn finish(self) {
        self.daemon.shutdown().await;
        self.serve.abort();
    }
}

async fn call(daemon: &Daemon, id: u64, method: &str, params: Value) -> Response {
    daemon
        .handle(Request {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
        })
        .await
}

/// A client of `confirm.subscribe`: the person who answers.
struct Person {
    socket: std::path::PathBuf,
    notices: tokio::io::Lines<BufReader<UnixStream>>,
}

impl Person {
    async fn subscribe(socket: &std::path::Path) -> Person {
        let mut stream = UnixStream::connect(socket).await.unwrap();
        stream
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"confirm.subscribe\",\"params\":{}}\n",
            )
            .await
            .unwrap();
        let mut notices = BufReader::new(stream).lines();
        let ok: Value = serde_json::from_str(&notices.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(ok["result"], json!({}), "{ok}");
        Person {
            socket: socket.to_path_buf(),
            notices,
        }
    }

    /// The next notice of `method` within `within`; `None` when none came.
    async fn try_next(&mut self, method: &str, within: Duration) -> Option<Value> {
        let deadline = Instant::now() + within;
        loop {
            let line = tokio::time::timeout_at(deadline, self.notices.next_line())
                .await
                .ok()?
                .unwrap()
                .expect("the notice channel closed");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["method"] == method {
                return Some(v["params"].clone());
            }
        }
    }

    async fn next(&mut self, method: &str) -> Value {
        self.try_next(method, Duration::from_secs(30))
            .await
            .unwrap_or_else(|| panic!("no {method} notice"))
    }

    /// One request on a connection of its own; the response line.
    async fn request(&self, method: &str, params: Value) -> Value {
        let mut stream = UnixStream::connect(&self.socket).await.unwrap();
        let req = json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params });
        stream
            .write_all(format!("{req}\n").as_bytes())
            .await
            .unwrap();
        let mut lines = BufReader::new(stream).lines();
        serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap()
    }

    async fn respond(&self, confirm_id: &str, answer: &str) -> Value {
        self.request(
            "confirm.respond",
            json!({ "confirmId": confirm_id, "answer": answer }),
        )
        .await
    }
}

fn reason(r: &Response) -> String {
    let e = r
        .error
        .as_ref()
        .unwrap_or_else(|| panic!("not refused: {r:?}"));
    assert_eq!(e.code, "policy_denied", "{e:?}");
    e.data.as_ref().unwrap()["reason"]
        .as_str()
        .unwrap()
        .to_string()
}

fn shown(asked: &Value, label: &str) -> String {
    asked["details"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["label"] == label)
        .unwrap_or_else(|| panic!("no {label}: {asked}"))["value"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn state_waits_for_an_api_answer_and_runs() {
    let rig = rig(None).await;
    let s = rig.session().await;
    let mut person = rig.subscribe().await;
    let run = rig.exec(&s, &["storage", "local", "set", "pegconfirm", "v1"]);
    let asked = person.next("confirm.required").await;
    assert_eq!(asked["sessionId"], s.id, "{asked}");
    assert_eq!(asked["class"], "state", "{asked}");
    assert_eq!(asked["offerAllowForSession"], true, "{asked}");
    assert_eq!(shown(&asked, "출처"), rig.fixture.origin);
    assert_eq!(shown(&asked, "키"), "pegconfirm");
    assert_eq!(shown(&asked, "값"), "v1");
    // It waits for the answer.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!run.is_finished());
    let cid = asked["confirmId"].as_str().unwrap();
    assert_eq!(person.respond(cid, "approve").await["result"], json!({}));
    let r = run.await.unwrap();
    let result = r.result.unwrap_or_else(|| panic!("{:?}", r.error));
    assert_eq!(result["exitCode"], 0, "{result}");
    assert_eq!(
        result["output"]["confirmed"],
        json!({ "by": "api" }),
        "{result}"
    );
    let resolved = person.next("confirm.resolved").await;
    assert_eq!(
        (
            resolved["confirmId"].as_str(),
            &resolved["outcome"],
            &resolved["by"]
        ),
        (Some(cid), &json!("approved"), &json!("api"))
    );
    assert_eq!(rig.local_storage(&s, "pegconfirm").await, "v1");
    let entry = rig
        .entries(&s)
        .into_iter()
        .find(|e| e.command == "storage")
        .unwrap();
    assert!(entry.ok, "{entry:?}");
    let c = entry.confirm.unwrap();
    assert_eq!(
        (c.outcome, c.by),
        (
            pegboard_protocol::ConfirmOutcome::Approved,
            Some(pegboard_protocol::ConfirmBy::Api)
        )
    );
    assert!(c.wait_ms >= 400, "{c:?}");
    rig.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn state_denied_by_api_is_denied_with_reason_denied() {
    let rig = rig(None).await;
    let s = rig.session().await;
    let mut person = rig.subscribe().await;
    let run = rig.exec(&s, &["storage", "local", "set", "pegdenied", "secret-v"]);
    let asked = person.next("confirm.required").await;
    let cid = asked["confirmId"].as_str().unwrap().to_string();
    assert_eq!(person.respond(&cid, "deny").await["result"], json!({}));
    let r = run.await.unwrap();
    assert_eq!(reason(&r), "denied");
    let e = r.error.unwrap();
    assert_eq!(e.data.unwrap()["confirmId"], cid);
    assert!(
        e.message.contains("a person denied this state command"),
        "{}",
        e.message
    );
    assert_eq!(rig.local_storage(&s, "pegdenied").await, Value::Null);
    // Logged like any refusal (no words after the command), with who answered.
    let entry = rig
        .entries(&s)
        .into_iter()
        .find(|e| e.command == "storage")
        .unwrap();
    assert!(!entry.ok && entry.redacted, "{entry:?}");
    let c = entry.confirm.unwrap();
    assert_eq!(c.outcome, pegboard_protocol::ConfirmOutcome::Denied);
    assert_eq!(c.by, Some(pegboard_protocol::ConfirmBy::Api));
    let text = std::fs::read_to_string(
        rig.env
            .paths
            .recordings
            .join(format!("r_{}", &s.id[2..]))
            .join("actions.jsonl"),
    )
    .unwrap();
    assert!(
        !text.contains("secret-v") && !text.contains("pegdenied"),
        "{text}"
    );
    assert!(
        text.contains("\"confirm\":{\"outcome\":\"denied\",\"by\":\"api\""),
        "{text}"
    );
    rig.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn no_responder_denies_at_once_with_the_browser_hint() {
    let rig = rig(None).await;
    if rig.fork {
        eprintln!("skipped: stock only (the fork's dialog is a responder)");
        rig.finish().await;
        return;
    }
    let s = rig.session().await;
    let started = Instant::now();
    let r = rig
        .exec(&s, &["cookies", "set", "pegnobody", "1"])
        .await
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(reason(&r), "no_responder");
    let e = r.error.unwrap();
    assert!(
        e.message
            .contains("confirmation needs the Pegboard browser"),
        "{}",
        e.message
    );
    rig.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn timeout_denies_and_frees_the_session() {
    let rig = rig(Some(2000)).await;
    let s = rig.session().await;
    let mut person = rig.subscribe().await;
    let started = Instant::now();
    let run = rig.exec(&s, &["storage", "local", "set", "pegslow", "v"]);
    let asked = person.next("confirm.required").await;
    let r = run.await.unwrap();
    let waited = started.elapsed();
    assert_eq!(reason(&r), "timed_out");
    assert!(
        r.error.unwrap().message.contains("no answer within 2 s"),
        "the limit as set"
    );
    assert!(
        waited >= Duration::from_millis(1900) && waited < Duration::from_secs(8),
        "{waited:?}"
    );
    let resolved = person.next("confirm.resolved").await;
    assert_eq!(resolved["confirmId"], asked["confirmId"]);
    assert_eq!(resolved["outcome"], "timedOut");
    // Late: not found.
    let late = person
        .respond(asked["confirmId"].as_str().unwrap(), "approve")
        .await;
    assert_eq!(late["error"]["code"], "confirm_not_found", "{late}");
    // The session takes the next command at once.
    let started = Instant::now();
    rig.ok(&s, &["get", "url"]).await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(rig.local_storage(&s, "pegslow").await, Value::Null);
    rig.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn closing_the_session_ends_a_pending_command() {
    let rig = rig(None).await;
    let s = rig.session().await;
    let mut person = rig.subscribe().await;
    let run = rig.exec(&s, &["storage", "local", "set", "pegclose", "v"]);
    let asked = person.next("confirm.required").await;
    // The command holds the session's log while it waits; the close must not wait for it.
    let started = Instant::now();
    let close = rig.call(
        "session.close",
        json!({ "sessionId": s.id, "token": s.token }),
    );
    let close = tokio::time::timeout(Duration::from_secs(5), close)
        .await
        .expect("the close ended within 5 s");
    assert!(close.error.is_none(), "{:?}", close.error);
    let r = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("the command ended within 5 s")
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(reason(&r), "cancelled");
    let resolved = person.next("confirm.resolved").await;
    assert_eq!(resolved["confirmId"], asked["confirmId"]);
    assert_eq!(resolved["outcome"], "cancelled");
    // Approving it now finds nothing.
    let late = person
        .respond(asked["confirmId"].as_str().unwrap(), "approve")
        .await;
    assert_eq!(late["error"]["code"], "confirm_not_found", "{late}");
    // The refusal was logged before the recording ended.
    let entry = rig
        .entries(&s)
        .into_iter()
        .find(|e| e.command == "storage")
        .expect("the cancelled command is in the log");
    assert_eq!(
        entry.confirm.map(|c| c.outcome),
        Some(pegboard_protocol::ConfirmOutcome::Cancelled)
    );
    rig.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn allow_for_session_skips_the_next_question_only_in_that_session() {
    let rig = rig(None).await;
    let (a, b) = (rig.session().await, rig.session().await);
    let mut person = rig.subscribe().await;
    let run = rig.exec(&a, &["storage", "local", "set", "pegallow", "1"]);
    let asked = person.next("confirm.required").await;
    assert_eq!(asked["sessionId"], a.id);
    let r = person
        .respond(asked["confirmId"].as_str().unwrap(), "allowForSession")
        .await;
    assert_eq!(r["result"], json!({}), "{r}");
    let r = run.await.unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    // A again: no question, and the result says the session's grant let it run.
    let out = rig
        .ok(&a, &["storage", "local", "set", "pegallow", "2"])
        .await;
    assert_eq!(out["confirmed"], json!({ "by": "session" }), "{out}");
    assert!(
        person
            .try_next("confirm.required", Duration::from_millis(300))
            .await
            .is_none(),
        "A was asked again"
    );
    let info = rig
        .call(
            "session.info",
            json!({ "sessionId": a.id, "token": a.token }),
        )
        .await
        .result
        .unwrap();
    assert_eq!(info["allowedForSession"], json!(["state"]), "{info}");
    // B is still asked.
    let run = rig.exec(&b, &["storage", "local", "set", "pegallow", "3"]);
    let asked = person.next("confirm.required").await;
    assert_eq!(asked["sessionId"], b.id);
    person
        .respond(asked["confirmId"].as_str().unwrap(), "deny")
        .await;
    assert_eq!(reason(&run.await.unwrap()), "denied");
    let info = rig
        .call(
            "session.info",
            json!({ "sessionId": b.id, "token": b.token }),
        )
        .await
        .result
        .unwrap();
    assert_eq!(info["allowedForSession"], json!([]), "{info}");
    let logged: Vec<_> = rig
        .entries(&a)
        .into_iter()
        .filter(|e| e.command == "storage")
        .map(|e| e.confirm.map(|c| (c.outcome, c.by)))
        .collect();
    use pegboard_protocol::{ConfirmBy, ConfirmOutcome};
    assert_eq!(
        logged,
        [
            Some((ConfirmOutcome::ApprovedForSession, Some(ConfirmBy::Api))),
            Some((ConfirmOutcome::Approved, Some(ConfirmBy::Session))),
        ]
    );
    let _ = rig
        .ok(&a, &["eval", "localStorage.removeItem('pegallow')"])
        .await;
    rig.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn an_answer_for_session_a_does_not_release_session_b() {
    let rig = rig(None).await;
    let (a, b) = (rig.session().await, rig.session().await);
    let mut person = rig.subscribe().await;
    let run_a = rig.exec(&a, &["storage", "local", "set", "pegab", "a"]);
    let asked_a = person.next("confirm.required").await;
    let run_b = rig.exec(&b, &["storage", "local", "set", "pegab", "b"]);
    let asked_b = person.next("confirm.required").await;
    assert_eq!(
        (&asked_a["sessionId"], &asked_b["sessionId"]),
        (&json!(a.id), &json!(b.id))
    );
    person
        .respond(asked_a["confirmId"].as_str().unwrap(), "approve")
        .await;
    assert!(run_a.await.unwrap().error.is_none());
    // B still waits.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!run_b.is_finished());
    let pending = person.request("confirm.list", json!({})).await;
    let ids: Vec<&Value> = pending["result"]["pending"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| &p["confirmId"])
        .collect();
    assert_eq!(ids, [&asked_b["confirmId"]], "{pending}");
    // A's id, used again, releases nothing.
    let again = person
        .respond(asked_a["confirmId"].as_str().unwrap(), "approve")
        .await;
    assert_eq!(again["error"]["code"], "confirm_not_found", "{again}");
    assert!(!run_b.is_finished());
    person
        .respond(asked_b["confirmId"].as_str().unwrap(), "deny")
        .await;
    assert_eq!(reason(&run_b.await.unwrap()), "denied");
    let _ = rig
        .ok(&a, &["eval", "localStorage.removeItem('pegab')"])
        .await;
    rig.finish().await;
}

/// While a frame is selected (`frame <sel>`), a storage write or an upload that would ask
/// is refused at once, without a question: agent-browser's `upload` follows the frame, whose
/// origin pegboard cannot read (its `eval` runs in the tab's top frame; measured on the test
/// machine: `storage` writes in the top frame, `upload` looks for its input in the frame).
/// A cookie, set for the pinned tab address, still asks. After `frame main` a storage write
/// asks, shows the tab's origin, and lands there, not in the frame's origin.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn a_write_or_upload_in_a_selected_frame_is_refused_without_asking() {
    let rig = rig(None).await;
    // The page frames `/upload` of another fixture: another origin (its port) of the same
    // site, so the frame is in the page's process, where agent-browser's `frame` finds it (a
    // cross-site frame, such as one of localhost, is a target of its own: "Frame not found").
    let other = common::fixture().await;
    let port = other.origin.rsplit(':').next().unwrap().to_string();
    let s = rig.session_at(&format!("/xorigin/{port}/upload")).await;
    assert_ne!(other.origin, rig.fixture.origin);
    rig.ok(&s, &["wait", "--fn", "window.__frLoaded === true"])
        .await;
    rig.ok(&s, &["frame", "#fr"]).await;
    let mut person = rig.subscribe().await;
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("fr.txt");
    std::fs::write(&file, "fr").unwrap();
    for argv in [
        vec!["storage", "local", "set", "pegframe", "f1"],
        vec!["storage", "session", "clear"],
        vec!["upload", "#f", file.to_str().unwrap()],
    ] {
        let r = rig.exec(&s, &argv).await.unwrap();
        assert_eq!(reason(&r), "unknown_origin", "{argv:?}");
        assert!(r.error.unwrap().message.contains("frame main"), "{argv:?}");
    }
    assert!(
        person
            .try_next("confirm.required", Duration::from_secs(1))
            .await
            .is_none(),
        "asked while a frame was selected"
    );
    assert_eq!(rig.copies_left(&s), 0);
    // A cookie is held to the tab's address: it still asks.
    let run = rig.exec(&s, &["cookies", "set", "pegframeck", "1"]);
    let asked = person.next("confirm.required").await;
    assert_eq!(shown(&asked, "호스트"), "127.0.0.1", "{asked}");
    person
        .respond(asked["confirmId"].as_str().unwrap(), "deny")
        .await;
    assert_eq!(reason(&run.await.unwrap()), "denied");
    // Back at the top: asked, shown the tab's origin, and written there.
    rig.ok(&s, &["frame", "main"]).await;
    let run = rig.exec(&s, &["storage", "local", "set", "pegframe", "top"]);
    let asked = person.next("confirm.required").await;
    assert_eq!(shown(&asked, "출처"), rig.fixture.origin, "{asked}");
    person
        .respond(asked["confirmId"].as_str().unwrap(), "approve")
        .await;
    let r = run.await.unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(rig.local_storage(&s, "pegframe").await, "top");
    // Not in the frame's origin.
    rig.ok(&s, &["open", &format!("{}/counter", other.origin)])
        .await;
    assert_eq!(rig.local_storage(&s, "pegframe").await, Value::Null);
    rig.finish().await;
}

/// Every object in `v` (at any depth) whose `name` is `name`: the cookies `cookies get`
/// lists, whatever its exact shape.
fn cookies_named<'a>(v: &'a Value, name: &str, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(o) => {
            if o.get("name").and_then(Value::as_str) == Some(name) {
                out.push(v);
            }
            o.values().for_each(|x| cookies_named(x, name, out));
        }
        Value::Array(a) => a.iter().for_each(|x| cookies_named(x, name, out)),
        _ => {}
    }
}

/// A session warmed up (agent-browser's daemon running) at `/hop/<ms>`, which goes to
/// another origin (localhost) `ms` after it loads.
async fn hopping_session(rig: &Rig, ms: u64) -> Session {
    let s = rig.session().await;
    rig.ok(&s, &["get", "url"]).await;
    rig.ok(&s, &["open", &format!("{}/hop/{ms}", rig.fixture.origin)])
        .await;
    s
}

/// Approved after the tab went to another origin: a storage write does not run (`stale`),
/// and the log says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn a_storage_write_approved_after_the_tab_left_is_stale() {
    let rig = rig(None).await;
    let s = hopping_session(&rig, 2500).await;
    let mut person = rig.subscribe().await;
    let run = rig.exec(&s, &["storage", "local", "set", "peghop", "v"]);
    let asked = person.next("confirm.required").await;
    assert_eq!(
        shown(&asked, "출처"),
        rig.fixture.origin,
        "asked before the hop: {asked}"
    );
    // The tab goes to localhost meanwhile.
    tokio::time::sleep(Duration::from_millis(3500)).await;
    person
        .respond(asked["confirmId"].as_str().unwrap(), "approve")
        .await;
    let r = run.await.unwrap();
    assert_eq!(reason(&r), "stale");
    assert_eq!(
        r.error.unwrap().data.unwrap()["confirmId"],
        asked["confirmId"]
    );
    // Not written where the tab is now, nor where it was.
    let out = rig.ok(&s, &["get", "url"]).await;
    assert!(out.to_string().contains("localhost"), "{out}");
    assert_eq!(rig.local_storage(&s, "peghop").await, Value::Null);
    rig.ok(&s, &["open", &format!("{}/counter", rig.fixture.origin)])
        .await;
    assert_eq!(rig.local_storage(&s, "peghop").await, Value::Null);
    let entry = rig
        .entries(&s)
        .into_iter()
        .find(|e| e.command == "storage")
        .unwrap();
    assert!(!entry.ok, "{entry:?}");
    assert_eq!(
        entry.confirm.map(|c| c.outcome),
        Some(pegboard_protocol::ConfirmOutcome::Stale)
    );
    rig.finish().await;
}

/// Approved after the tab went to another origin: a cookie lands on the host the person was
/// shown, with the path they were shown.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn a_cookie_approved_after_the_tab_left_lands_on_the_asked_host() {
    let rig = rig(None).await;
    let s = hopping_session(&rig, 2500).await;
    let mut person = rig.subscribe().await;
    let run = rig.exec(&s, &["cookies", "set", "peghopck", "v1"]);
    let asked = person.next("confirm.required").await;
    assert_eq!(asked["details"][0]["label"], "호스트", "{asked}");
    assert_eq!(shown(&asked, "호스트"), "127.0.0.1", "{asked}");
    let shown_path = shown(&asked, "경로");
    assert_eq!(shown_path, "/hop", "{asked}");
    tokio::time::sleep(Duration::from_millis(3500)).await;
    person
        .respond(asked["confirmId"].as_str().unwrap(), "approve")
        .await;
    let r = run.await.unwrap();
    let result = r.result.unwrap_or_else(|| panic!("{:?}", r.error));
    assert_eq!(result["exitCode"], 0, "{result}");
    // Now at localhost: no such cookie there (one listed is the 127.0.0.1 one).
    let out = rig.ok(&s, &["get", "url"]).await;
    assert!(out.to_string().contains("localhost"), "{out}");
    let here = rig.ok(&s, &["cookies", "get"]).await;
    let mut found = Vec::new();
    cookies_named(&here, "peghopck", &mut found);
    assert!(
        found.iter().all(|c| c["domain"] == "127.0.0.1"),
        "set on another host: {here}"
    );
    // Back on the asked host, under the shown path: there, with that path.
    rig.ok(&s, &["open", &format!("{}/hop/600000", rig.fixture.origin)])
        .await;
    let there = rig.ok(&s, &["cookies", "get"]).await;
    let mut found = Vec::new();
    cookies_named(&there, "peghopck", &mut found);
    assert_eq!(found.len(), 1, "{there}");
    assert_eq!(found[0]["domain"], "127.0.0.1", "{there}");
    assert_eq!(found[0]["path"], shown_path.as_str(), "{there}");
    assert_eq!(found[0]["value"], "v1", "{there}");
    rig.finish().await;
}

/// An upload from a session without an upload directory: the person sees the real path (a
/// link in the given one resolved), its size and where it goes; what uploads is the content
/// they were shown, under its own name, even though the original changed after the question.
/// The agent never sees the copy's path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn upload_shows_real_paths_and_uploads_the_bytes_shown() {
    let rig = rig(None).await;
    let s = rig.session_at("/upload").await;
    let tmp = tempfile::tempdir().unwrap();
    let real_dir = tmp.path().canonicalize().unwrap().join("real");
    std::fs::create_dir(&real_dir).unwrap();
    std::fs::write(real_dir.join("a.txt"), "first").unwrap();
    std::os::unix::fs::symlink(&real_dir, tmp.path().join("via")).unwrap();
    let given = tmp.path().join("via/a.txt").display().to_string();
    let real = real_dir.join("a.txt").display().to_string();
    assert_ne!(given, real);
    let mut person = rig.subscribe().await;
    let run = rig.exec(&s, &["upload", "#f", &given]);
    let asked = person.next("confirm.required").await;
    assert_eq!(asked["class"], "upload", "{asked}");
    assert_eq!(
        asked["offerAllowForSession"], false,
        "no upload directory: {asked}"
    );
    assert_eq!(asked["details"][0]["label"], "호스트", "{asked}");
    assert_eq!(shown(&asked, "호스트"), "127.0.0.1");
    assert_eq!(shown(&asked, "대상 페이지"), rig.fixture.origin);
    assert_eq!(shown(&asked, "파일"), real, "the real path: {asked}");
    assert_eq!(shown(&asked, "크기"), "5 B");
    assert!(!asked.to_string().contains("/confirm/"), "{asked}");
    // The original changes after the question, then becomes a link elsewhere.
    std::fs::write(real_dir.join("a.txt"), "changed after").unwrap();
    std::fs::write(tmp.path().join("other.txt"), "other").unwrap();
    std::fs::remove_file(real_dir.join("a.txt")).unwrap();
    std::os::unix::fs::symlink(tmp.path().join("other.txt"), real_dir.join("a.txt")).unwrap();
    person
        .respond(asked["confirmId"].as_str().unwrap(), "approve")
        .await;
    let r = run.await.unwrap();
    let result = r.result.unwrap_or_else(|| panic!("{:?}", r.error));
    assert_eq!(result["exitCode"], 0, "{result}");
    assert_eq!(
        result["output"]["confirmed"],
        json!({ "by": "api" }),
        "{result}"
    );
    assert!(
        !result.to_string().contains("/confirm/"),
        "a copy's path: {result}"
    );
    assert_eq!(rig.uploaded(&s).await, "a.txt:5:first");
    // Kept for the page until the session closes.
    assert_eq!(rig.copies_left(&s), 1);
    let entry = rig
        .entries(&s)
        .into_iter()
        .find(|e| e.command == "upload")
        .unwrap();
    assert!(entry.ok, "{entry:?}");
    assert_eq!(
        entry.confirm.map(|c| (c.outcome, c.by)),
        Some((
            pegboard_protocol::ConfirmOutcome::Approved,
            Some(pegboard_protocol::ConfirmBy::Api)
        ))
    );
    rig.finish().await;
}

/// With nobody to answer, an upload is denied at once, before any file is looked at (a
/// missing file is not even reported), and a denied one leaves no copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn an_upload_with_nobody_to_answer_is_denied_before_its_files_are_read() {
    let rig = rig(None).await;
    if rig.fork {
        eprintln!("skipped: stock only (the fork's dialog is a responder)");
        rig.finish().await;
        return;
    }
    let s = rig.session_at("/upload").await;
    let r = rig
        .exec(&s, &["upload", "#f", "/no/such/file.txt"])
        .await
        .unwrap();
    assert_eq!(reason(&r), "no_responder");
    assert!(
        r.error
            .unwrap()
            .message
            .contains("--allow upload and --upload-dir"),
        "both options are named"
    );
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("d.txt");
    std::fs::write(&file, "d").unwrap();
    let mut person = rig.subscribe().await;
    let run = rig.exec(&s, &["upload", "#f", file.to_str().unwrap()]);
    let asked = person.next("confirm.required").await;
    assert_eq!(rig.copies_left(&s), 1, "copied before asking");
    person
        .respond(asked["confirmId"].as_str().unwrap(), "deny")
        .await;
    assert_eq!(reason(&run.await.unwrap()), "denied");
    assert_eq!(rig.copies_left(&s), 0);
    assert_eq!(
        rig.ok(&s, &["eval", "String(window.__got)"]).await["data"]["result"],
        "undefined"
    );
    rig.finish().await;
}

/// Approved after the tab went to another origin: the upload does not run (`stale`), and its
/// copy is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn upload_origin_change_is_stale() {
    let rig = rig(None).await;
    let s = hopping_session(&rig, 2500).await;
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("h.txt");
    std::fs::write(&file, "h").unwrap();
    let mut person = rig.subscribe().await;
    let run = rig.exec(&s, &["upload", "#f", file.to_str().unwrap()]);
    let asked = person.next("confirm.required").await;
    assert_eq!(shown(&asked, "대상 페이지"), rig.fixture.origin, "{asked}");
    tokio::time::sleep(Duration::from_millis(3500)).await;
    person
        .respond(asked["confirmId"].as_str().unwrap(), "approve")
        .await;
    let r = run.await.unwrap();
    assert_eq!(reason(&r), "stale");
    assert_eq!(rig.copies_left(&s), 0);
    let entry = rig
        .entries(&s)
        .into_iter()
        .find(|e| e.command == "upload")
        .unwrap();
    assert_eq!(
        entry.confirm.map(|c| c.outcome),
        Some(pegboard_protocol::ConfirmOutcome::Stale)
    );
    rig.finish().await;
}

/// A session with an upload directory: an upload asks, by file name only, and offers "allow
/// for the rest of the session"; once given, the next upload goes unasked (as with
/// `--allow upload`), and an absolute path is still refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn upload_with_an_upload_dir_offers_allow_for_session_and_then_stops_asking() {
    let rig = rig(None).await;
    let dir = tempfile::tempdir().unwrap();
    let up = dir.path().canonicalize().unwrap();
    std::fs::write(up.join("a.txt"), "aa").unwrap();
    std::fs::write(up.join("b.txt"), "bbb").unwrap();
    let s = rig
        .session_with("/upload", json!({ "uploadDir": up.display().to_string() }))
        .await;
    let mut person = rig.subscribe().await;
    let run = rig.exec(&s, &["upload", "#f", "a.txt"]);
    let asked = person.next("confirm.required").await;
    assert_eq!(asked["offerAllowForSession"], true, "{asked}");
    assert_eq!(
        shown(&asked, "파일"),
        up.join("a.txt").display().to_string()
    );
    person
        .respond(asked["confirmId"].as_str().unwrap(), "allowForSession")
        .await;
    let r = run.await.unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(rig.uploaded(&s).await, "a.txt:2:aa");
    // Unasked now, from the directory.
    rig.ok(&s, &["eval", "window.__got = undefined"]).await;
    let out = rig.ok(&s, &["upload", "#f", "b.txt"]).await;
    assert_eq!(out["confirmed"], json!({ "by": "session" }), "{out}");
    assert!(
        person
            .try_next("confirm.required", Duration::from_secs(1))
            .await
            .is_none(),
        "asked again"
    );
    assert_eq!(rig.uploaded(&s).await, "b.txt:3:bbb");
    // By name only.
    let other = tempfile::tempdir().unwrap();
    let c = other.path().join("c.txt");
    std::fs::write(&c, "c").unwrap();
    let r = rig
        .exec(&s, &["upload", "#f", c.to_str().unwrap()])
        .await
        .unwrap();
    assert_eq!(
        r.error.as_ref().map(|e| e.code.as_str()),
        Some("argv_rejected"),
        "{r:?}"
    );
    rig.finish().await;
}

/// The browser reads a file input's files when the page uses them: an approved upload's copy
/// stays until the session closes, so a form submitted after the command sends the bytes the
/// person was shown (not the original, changed since). Closing the session removes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn an_approved_upload_is_sent_when_the_form_is_submitted_later() {
    let rig = rig(None).await;
    let s = rig.session_at("/upload-form").await;
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("later.txt");
    std::fs::write(&file, "shown-bytes-4711").unwrap();
    let mut person = rig.subscribe().await;
    let run = rig.exec(&s, &["upload", "#f", file.to_str().unwrap()]);
    let asked = person.next("confirm.required").await;
    person
        .respond(asked["confirmId"].as_str().unwrap(), "approve")
        .await;
    let r = run.await.unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.result.unwrap()["exitCode"], 0);
    std::fs::write(&file, "changed-after-9999").unwrap();
    // Submitted after the command ended.
    tokio::time::sleep(Duration::from_millis(500)).await;
    rig.ok(&s, &["eval", "document.getElementById('fm').submit()"])
        .await;
    rig.ok(&s, &["wait", "--fn", "location.pathname === '/echo'"])
        .await;
    let body = rig.ok(&s, &["get", "text", "body"]).await.to_string();
    assert!(body.contains("shown-bytes-4711"), "{body}");
    assert!(body.contains("later.txt"), "{body}");
    assert!(!body.contains("changed-after"), "{body}");
    assert_eq!(rig.copies_left(&s), 1, "kept for the page");
    let close = rig
        .call(
            "session.close",
            json!({ "sessionId": s.id, "token": s.token }),
        )
        .await;
    assert!(close.error.is_none(), "{:?}", close.error);
    assert_eq!(rig.copies_left(&s), 0);
    rig.finish().await;
}

/// A ref from a snapshot reaches into an iframe (measured: `upload @eN` with the iframe's
/// file button works without `frame`). The person was shown the tab's origin, so an approved
/// upload's file may go only into a file input of the tab's top document: the proxy checks the
/// node, in a world of its own, and refuses this one. Nothing reaches the frame's input.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn an_upload_through_a_ref_into_an_iframe_is_refused() {
    let rig = rig(None).await;
    let other = common::fixture().await;
    let port = other.origin.rsplit(':').next().unwrap().to_string();
    let s = rig.session_at(&format!("/xorigin/{port}/upload")).await;
    rig.ok(&s, &["wait", "--fn", "window.__frLoaded === true"])
        .await;
    let snap = rig.ok(&s, &["snapshot"]).await;
    let refs = snap["data"]["refs"].as_object().expect("refs").clone();
    let (button, _) = refs
        .iter()
        .find(|(_, v)| v["role"] == "button")
        .unwrap_or_else(|| panic!("no file button in {snap}"));
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("framed.txt");
    std::fs::write(&file, "framed").unwrap();
    let mut person = rig.subscribe().await;
    let run = rig.exec(
        &s,
        &["upload", &format!("@{button}"), file.to_str().unwrap()],
    );
    let asked = person.next("confirm.required").await;
    assert_eq!(shown(&asked, "대상 페이지"), rig.fixture.origin, "{asked}");
    person
        .respond(asked["confirmId"].as_str().unwrap(), "approve")
        .await;
    let r = run.await.unwrap();
    eprintln!("the refused upload: {r:?}");
    let failed = r.error.is_some() || r.result.as_ref().is_some_and(|v| v["exitCode"] != 0);
    assert!(failed, "the file went into the frame: {r:?}");
    let after = rig.ok(&s, &["snapshot"]).await.to_string();
    assert!(!after.contains("framed.txt"), "{after}");
    rig.finish().await;
}

// ---------------------------------------------------------------------------------------------
// The fork's confirmation dialog (phase 3b task 7). Answers come from the browser's answer
// hook, by session name; nobody subscribes to `confirm.*`, so the dialog is the only responder.
// ---------------------------------------------------------------------------------------------

/// A rig whose daemon launches the fork with the dialog log and `answer`
/// (`PEGBOARD_TEST_DIALOG_ANSWER`) in the browser's environment; `None` in stock mode (the
/// test is skipped).
async fn fork_rig(
    answer: &str,
    timeout_ms: Option<u64>,
    extra: Vec<EnvVar>,
) -> Option<(Rig, common::DialogLog)> {
    let log = common::DialogLog::new();
    let mut vars = vec![
        EnvVar::set("PEGBOARD_TEST_DIALOG_LOG", log.path.to_str().unwrap()),
        EnvVar::set("PEGBOARD_TEST_DIALOG_ANSWER", answer),
    ];
    vars.extend(extra);
    let rig = rig_with(timeout_ms, vars).await;
    if !rig.fork {
        eprintln!("skipped: needs PEGBOARD_TEST_BROWSER=fork:<Pegboard.app>");
        rig.finish().await;
        return None;
    }
    Some((rig, log))
}

fn confirm_id_of(r: &Response) -> String {
    r.error.as_ref().unwrap().data.as_ref().unwrap()["confirmId"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The `confirm` entries of the log for the session `id`.
fn dialogs_of(log: &common::DialogLog, id: &str) -> Vec<Value> {
    log.entries()
        .into_iter()
        .filter(|e| e["kind"] == "confirm" && e["sessionId"] == id)
        .collect()
}

/// An upload asks in the session's own agent window: the dialog shows the session's ID, its
/// name escaped, its profile, the real path of the file and the page's origin; the answer
/// (deny, by the browser) is the command's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn dialog_appears_in_the_sessions_agent_window() {
    let Some((rig, log)) = fork_rig("*=deny", None, Vec::new()).await else {
        return;
    };
    let s = rig
        .session_named("/upload", "show\u{202E}gpj.exe", "dlg")
        .await;
    let tmp = tempfile::tempdir().unwrap();
    let real_dir = tmp.path().canonicalize().unwrap().join("real");
    std::fs::create_dir(&real_dir).unwrap();
    std::fs::write(real_dir.join("a.txt"), "first").unwrap();
    std::os::unix::fs::symlink(&real_dir, tmp.path().join("via")).unwrap();
    let given = tmp.path().join("via/a.txt").display().to_string();
    let real = real_dir.join("a.txt").display().to_string();
    let r = rig.exec(&s, &["upload", "#f", &given]).await.unwrap();
    assert_eq!(reason(&r), "denied");
    let id = confirm_id_of(&r);
    let e = log.wait("confirm", &id, 5).await;
    assert_eq!(e["profile"], "PegboardAgent-dlg", "{e}");
    assert_eq!(e["sessionId"], s.id, "{e}");
    assert_eq!((&e["pressed"], &e["by"]), (&json!("deny"), &json!("hook")));
    let lines = common::DialogLog::lines(&e);
    assert!(
        lines[1].contains(&s.id) && lines[1].contains("PegboardAgent-dlg"),
        "{lines:?}"
    );
    assert!(lines[2].contains("show\\u{202e}gpj.exe"), "{lines:?}");
    assert!(lines.iter().all(|l| !l.contains('\u{202E}')), "{lines:?}");
    assert!(lines.contains(&real), "the real path: {lines:?}");
    assert!(lines.contains(&rig.fixture.origin), "{lines:?}");
    assert_eq!(e["buttons"], json!(["허용", "거부"]), "no upload directory");
    assert_eq!(rig.copies_left(&s), 0);
    let entry = rig
        .entries(&s)
        .into_iter()
        .find(|e| e.command == "upload")
        .unwrap();
    assert_eq!(
        entry.confirm.map(|c| (c.outcome, c.by)),
        Some((
            pegboard_protocol::ConfirmOutcome::Denied,
            Some(pegboard_protocol::ConfirmBy::Browser)
        ))
    );
    rig.finish().await;
}

/// 허용 runs the command, 거부 refuses it with the reason `denied`; both by the browser.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn approve_runs_and_deny_denies_with_reason_denied() {
    let Some((rig, log)) = fork_rig("yes=approve,no=deny", None, Vec::new()).await else {
        return;
    };
    let yes = rig.session_named("/counter", "yes", "work").await;
    let no = rig.session_named("/counter", "no", "work").await;
    let out = rig
        .ok(&yes, &["storage", "local", "set", "pegdlg", "y"])
        .await;
    assert_eq!(out["confirmed"], json!({ "by": "browser" }), "{out}");
    assert_eq!(rig.local_storage(&yes, "pegdlg").await, "y");
    let r = rig
        .exec(&no, &["storage", "local", "set", "pegdlgno", "n"])
        .await
        .unwrap();
    assert_eq!(reason(&r), "denied");
    assert_eq!(rig.local_storage(&no, "pegdlgno").await, Value::Null);
    use pegboard_protocol::{ConfirmBy, ConfirmOutcome};
    for (s, outcome) in [
        (&yes, ConfirmOutcome::Approved),
        (&no, ConfirmOutcome::Denied),
    ] {
        let entry = rig
            .entries(s)
            .into_iter()
            .find(|e| e.command == "storage")
            .unwrap();
        assert_eq!(
            entry.confirm.map(|c| (c.outcome, c.by)),
            Some((outcome, Some(ConfirmBy::Browser)))
        );
        assert_eq!(dialogs_of(&log, &s.id).len(), 1);
    }
    let _ = rig
        .ok(&yes, &["eval", "localStorage.removeItem('pegdlg')"])
        .await;
    rig.finish().await;
}

/// 이 세션에서 계속 허용: the command runs, and the session's next state command runs without a
/// dialog.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn allow_for_session_skips_the_next_dialog() {
    let Some((rig, log)) = fork_rig("keep=allowForSession", None, Vec::new()).await else {
        return;
    };
    let s = rig.session_named("/counter", "keep", "work").await;
    let out = rig
        .ok(&s, &["storage", "local", "set", "pegkeep", "1"])
        .await;
    assert_eq!(out["confirmed"], json!({ "by": "browser" }), "{out}");
    let out = rig
        .ok(&s, &["storage", "local", "set", "pegkeep", "2"])
        .await;
    assert_eq!(out["confirmed"], json!({ "by": "session" }), "{out}");
    let dialogs = dialogs_of(&log, &s.id);
    assert_eq!(dialogs.len(), 1, "{dialogs:?}");
    assert_eq!(
        dialogs[0]["buttons"],
        json!(["허용", "거부", "이 세션에서 계속 허용"])
    );
    assert_eq!(dialogs[0]["pressed"], "allowForSession");
    let _ = rig
        .ok(&s, &["eval", "localStorage.removeItem('pegkeep')"])
        .await;
    rig.finish().await;
}

/// Two sessions in two agent profiles ask at once: each dialog is in its own session's window
/// and answers only its own command.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn two_sessions_get_their_own_dialogs() {
    let Some((rig, log)) = fork_rig("sa=approve,sb=deny", None, Vec::new()).await else {
        return;
    };
    let a = rig.session_named("/counter", "sa", "pa").await;
    let b = rig.session_named("/counter", "sb", "pb").await;
    let run_a = rig.exec(&a, &["storage", "local", "set", "pegtwo", "a"]);
    let run_b = rig.exec(&b, &["storage", "local", "set", "pegtwo", "b"]);
    let (ra, rb) = (run_a.await.unwrap(), run_b.await.unwrap());
    assert!(ra.error.is_none(), "{:?}", ra.error);
    assert_eq!(reason(&rb), "denied");
    assert_eq!(rig.local_storage(&a, "pegtwo").await, "a");
    for (s, profile, pressed) in [
        (&a, "PegboardAgent-pa", "approve"),
        (&b, "PegboardAgent-pb", "deny"),
    ] {
        let d = dialogs_of(&log, &s.id);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0]["profile"], profile, "{d:?}");
        assert_eq!(d[0]["pressed"], pressed, "{d:?}");
        assert!(common::DialogLog::lines(&d[0])[1].contains(&s.id));
    }
    let _ = rig
        .ok(&a, &["eval", "localStorage.removeItem('pegtwo')"])
        .await;
    rig.finish().await;
}

/// Without `PEGBOARD_TEST_HOOKS` in the browser's own environment (the daemon still has it),
/// the answer hook does nothing and logs nothing: the dialog waits and the command times out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn dialog_hooks_need_test_hooks() {
    let Some((rig, log)) = fork_rig(
        "*=approve",
        Some(3000),
        vec![EnvVar::set("PEGBOARD_TEST_BROWSER_NO_HOOKS", "1")],
    )
    .await
    else {
        return;
    };
    let s = rig.session_named("/counter", "hookless", "work").await;
    let r = rig
        .exec(&s, &["storage", "local", "set", "peghook", "x"])
        .await
        .unwrap();
    assert_eq!(reason(&r), "timed_out");
    assert_eq!(rig.local_storage(&s, "peghook").await, Value::Null);
    assert!(log.entries().is_empty(), "{:?}", log.entries());
    rig.finish().await;
}
