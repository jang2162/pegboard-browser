//! End-to-end tests of a person's confirmation on the command path, in stock mode: a client
//! of `confirm.subscribe` on the daemon's socket plays the person (stock mode has no
//! confirmation window). They start a browser, so they are `#[ignore]`d:
//! `PEGBOARD_TEST_HOOKS=1 cargo test -p pegboard-core --test e2e_confirm -- --ignored --test-threads=1`.
//!
//! Each test starts a daemon of its own (in this process, its API served on a socket in a
//! temporary home) and stops it at the end.

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
    // Zero (or none) is the full time a person has; not inherited from the caller.
    let vars = vec![
        EnvVar::set("PEGBOARD_TEST_HOOKS", "1"),
        EnvVar::set(
            "PEGBOARD_TEST_CONFIRM_TIMEOUT_MS",
            &timeout_ms.unwrap_or(0).to_string(),
        ),
    ];
    let env = common::env();
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

/// After `frame <sel>` agent-browser writes in that frame: the person is shown (and the
/// command pinned to) the frame's origin, with the tab's beside it, and the write lands
/// there, not in the tab's top frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn a_write_inside_a_frame_shows_the_frames_origin() {
    let rig = rig(None).await;
    // The page frames a page of another fixture: another origin (its port) of the same site,
    // so the frame is in the page's process, where agent-browser's `frame` finds it (a
    // cross-site frame, such as one of localhost, is a target of its own: "Frame not found").
    let other = common::fixture().await;
    let port = other.origin.rsplit(':').next().unwrap().to_string();
    let s = rig.session_at(&format!("/xorigin/{port}")).await;
    let frame_origin = other.origin.clone();
    assert_ne!(frame_origin, rig.fixture.origin);
    rig.ok(&s, &["wait", "--fn", "window.__frLoaded === true"])
        .await;
    rig.ok(&s, &["frame", "#fr"]).await;
    // Premise: commands now act in the frame.
    let inside = rig.ok(&s, &["eval", "location.origin"]).await;
    assert_eq!(inside["data"]["result"], frame_origin.as_str(), "{inside}");
    let mut person = rig.subscribe().await;
    let run = rig.exec(&s, &["storage", "local", "set", "pegframe", "f1"]);
    let asked = person.next("confirm.required").await;
    assert_eq!(shown(&asked, "출처"), frame_origin, "{asked}");
    assert_eq!(
        shown(&asked, "탭 출처 (쓰기는 그 안의 프레임)"),
        rig.fixture.origin,
        "{asked}"
    );
    person
        .respond(asked["confirmId"].as_str().unwrap(), "approve")
        .await;
    let r = run.await.unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    // Where it landed: in the frame's origin, not the page's.
    assert_eq!(rig.local_storage(&s, "pegframe").await, "f1");
    rig.ok(&s, &["frame", "main"]).await;
    assert_eq!(rig.local_storage(&s, "pegframe").await, Value::Null);
    // Back in the top frame, a write shows the page's origin alone.
    let run = rig.exec(&s, &["storage", "local", "set", "pegframe", "top"]);
    let asked = person.next("confirm.required").await;
    assert_eq!(shown(&asked, "출처"), rig.fixture.origin, "{asked}");
    assert!(!asked["details"].to_string().contains("탭 출처"), "{asked}");
    person
        .respond(asked["confirmId"].as_str().unwrap(), "deny")
        .await;
    assert_eq!(reason(&run.await.unwrap()), "denied");
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
