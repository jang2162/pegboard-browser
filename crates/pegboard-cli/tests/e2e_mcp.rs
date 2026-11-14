//! End-to-end tests of `pegboard mcp`: each starts the server as a child process and talks
//! to it over stdin and stdout, as an MCP client does.
//!
//! Tests that start a browser are `#[ignore]`d:
//! `cargo test -p pegboard-cli --test e2e_mcp -- --ignored --test-threads=1`.
//! `mcp_starts_the_daemon_and_creates_a_session` uses a home of its own (it starts the
//! daemon); the others share the binary's one daemon (`shared_home()`) and close what they
//! open.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};

/// A `pegboard mcp` child process.
struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    replies: Receiver<String>,
    next_id: u64,
    /// Every line it wrote to stdout.
    lines: Vec<String>,
    stderr: Arc<Mutex<String>>,
    /// While set, nothing reads the server's stdout (a client that stopped reading).
    paused: Arc<AtomicBool>,
}

impl Mcp {
    fn start(home: &Home, args: &[&str]) -> Mcp {
        Mcp::start_env(home, &[], args)
    }

    fn start_env(home: &Home, env: &[(&str, &str)], args: &[&str]) -> Mcp {
        let mut full = vec!["mcp"];
        full.extend_from_slice(args);
        let mut child = home
            .command(&full)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, replies) = channel();
        let stdout = child.stdout.take().unwrap();
        let paused = Arc::new(AtomicBool::new(false));
        let hold = paused.clone();
        std::thread::spawn(move || {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                while hold.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(20));
                }
                let Some(Ok(line)) = lines.next() else { return };
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let mut err = child.stderr.take().unwrap();
        let sink = stderr.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 {
                    return;
                }
                sink.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        let mut mcp = Mcp {
            stdin: child.stdin.take(),
            child,
            replies,
            next_id: 0,
            lines: Vec::new(),
            stderr,
            paused,
        };
        let r = mcp.request(
            "initialize",
            json!({ "protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": { "name": "e2e", "version": "1" } }),
        );
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18", "{r}");
        mcp.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        mcp
    }

    fn send(&mut self, msg: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    /// Sends a request without waiting for its answer.
    fn request_no_wait(&mut self, method: &str, params: Value) {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let deadline = Instant::now() + Duration::from_secs(240);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .replies
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no answer to {method}; stderr: {}", self.stderr()));
            self.lines.push(line.clone());
            let v: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("stdout carries a non-protocol line ({e}): {line:?}"));
            assert_eq!(v["jsonrpc"], "2.0", "{line}");
            if v["id"] == id {
                return v;
            }
        }
    }

    /// (isError, text) of a tool call.
    fn call(&mut self, tool: &str, args: Value) -> (bool, String) {
        let r = self.request("tools/call", json!({ "name": tool, "arguments": args }));
        let result = &r["result"];
        assert!(result.is_object(), "{r}");
        (
            result["isError"] == true,
            result["content"][0]["text"].as_str().unwrap().to_string(),
        )
    }

    /// Creates a session; its id.
    fn create(&mut self, args: Value) -> String {
        let (err, text) = self.call("pegboard_session_create", args);
        assert!(!err, "{text}");
        text.split_whitespace()
            .find(|w| w.starts_with("s_"))
            .unwrap()
            .to_string()
    }

    fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    fn close_stdin(&mut self) {
        self.stdin = None;
    }

    fn signal(&self, sig: i32) {
        // SAFETY: plain syscall on our child process.
        unsafe { libc::kill(self.child.id() as i32, sig) };
    }

    /// Its exit code; the test fails if it has not exited within `secs`.
    fn wait_exit(&mut self, secs: u64) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code().unwrap_or(-1);
            }
            assert!(
                Instant::now() < deadline,
                "pegboard mcp did not exit within {secs} s"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Ids of the open sessions, as the CLI lists them.
fn cli_session_ids(home: &Home) -> Vec<String> {
    let (code, out, err) = home.run(&["session", "list", "--json"]);
    assert_eq!(code, 0, "{err}");
    json(&out)
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect()
}

fn cli_create(home: &Home, args: &[&str]) -> String {
    let mut full = vec!["session", "create"];
    full.extend_from_slice(args);
    let (code, out, err) = home.run(&full);
    assert_eq!(code, 0, "{err}");
    session_handle(&out)
}

/// Nothing the server wrote holds the token.
fn assert_no_token(mcp: &Mcp, home: &Home, handle: &str) {
    let token = handle.split_once('.').unwrap().1;
    let out = mcp.lines.join("\n");
    assert!(!out.contains(token), "token in stdout: {out}");
    assert!(!mcp.stderr().contains(token), "token in stderr");
    let log = std::fs::read_to_string(home.path().join("daemon.log")).unwrap_or_default();
    assert!(!log.contains(token), "token in daemon.log");
}

#[test]
fn bad_options_exit_2_before_anything_starts() {
    let home = Home::new();
    for args in [
        vec!["mcp", "--bogus"],
        vec!["mcp", "--allow", "eval"],
        vec!["mcp", "--deny", "upload"],
        vec!["mcp", "--upload-dir", "/tmp"],
        vec!["mcp", "--tools", "nonsense"],
        vec!["mcp", "--allow-domain", "http://a.test/"],
    ] {
        let (code, out, err) = home.run(&args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(out.is_empty(), "{args:?}: {out}");
    }
    // A malformed handle in PEGBOARD_SESSION is refused, and not repeated.
    let (code, out, err) =
        home.run_env(&[("PEGBOARD_SESSION", "s_abcdefgh.NOT-A-TOKEN")], &["mcp"]);
    assert_eq!(code, 2, "{err}");
    assert!(out.is_empty(), "{out}");
    assert!(!err.contains("NOT-A-TOKEN"), "{err}");
    // None of that started a daemon.
    assert!(!home.path().join("daemon.pid").exists());
}

#[test]
#[ignore]
fn mcp_starts_the_daemon_and_creates_a_session() {
    let origin = fixture();
    let home = Home::new();
    let mut mcp = Mcp::start(&home, &[]);
    let (err, status) = mcp.call("pegboard_browser_status", json!({}));
    assert!(!err && status.starts_with("running"), "{status}");
    let id = mcp.create(json!({ "name": "first", "url": format!("{origin}/counter") }));
    assert!(cli_session_ids(&home).contains(&id));
    let (err, list) = mcp.call("pegboard_session_list", json!({}));
    assert!(
        !err && list.contains(&id) && list.contains("first"),
        "{list}"
    );
    let (err, info) = mcp.call("pegboard_session_info", json!({ "sessionId": id }));
    assert!(!err && info.contains("policy:"), "{info}");
    let (err, files) = mcp.call("pegboard_files_list", json!({ "sessionId": id }));
    assert!(!err, "{files}");
    // Every stdout line was a protocol message (checked in `request`), and the session's
    // token is nowhere: no 26-character base32 word follows the id.
    let out = mcp.lines.join("\n");
    assert!(!out.contains(&format!("{id}.")), "{out}");
    mcp.close_stdin();
    assert_eq!(mcp.wait_exit(20), 0, "{}", mcp.stderr());
    assert!(!cli_session_ids(&home).contains(&id));
}

#[test]
#[ignore]
fn sessions_are_closed_when_stdin_closes() {
    let origin = fixture();
    let home = shared_home();
    let given = cli_create(home, &["--url", &format!("{origin}/counter")]);
    let mut mcp = Mcp::start_env(home, &[("PEGBOARD_SESSION", given.as_str())], &[]);
    let a = mcp.create(json!({ "url": format!("{origin}/counter") }));
    let b = mcp.create(json!({}));
    let (_, list) = mcp.call("pegboard_session_list", json!({}));
    for id in [&a, &b, &id_of(&given).to_string()] {
        assert!(list.contains(id.as_str()), "{list}");
    }
    let ids = cli_session_ids(home);
    assert!(ids.contains(&a) && ids.contains(&b));
    mcp.close_stdin();
    assert_eq!(mcp.wait_exit(20), 0, "{}", mcp.stderr());
    let ids = cli_session_ids(home);
    assert!(!ids.contains(&a) && !ids.contains(&b), "{ids:?}");
    assert!(
        ids.contains(&id_of(&given).to_string()),
        "the given session stays: {ids:?}"
    );
    assert_no_token(&mcp, home, &given);
    assert_eq!(home.run(&["session", "close", &given]).0, 0);
}

#[test]
#[ignore]
fn sessions_are_closed_on_sigterm() {
    let home = shared_home();
    let mut mcp = Mcp::start(home, &[]);
    let a = mcp.create(json!({}));
    assert!(cli_session_ids(home).contains(&a));
    mcp.signal(libc::SIGTERM);
    assert_eq!(mcp.wait_exit(20), 0, "{}", mcp.stderr());
    assert!(!cli_session_ids(home).contains(&a));
}

#[test]
#[ignore]
fn default_mode_cannot_see_or_touch_another_servers_session() {
    let home = shared_home();
    let mut one = Mcp::start(home, &[]);
    let mut two = Mcp::start(home, &[]);
    let theirs = one.create(json!({ "name": "theirs" }));
    let mine = two.create(json!({ "name": "mine" }));
    let made_up = "s_zzzzzzzz";
    let (_, list) = two.call("pegboard_session_list", json!({}));
    assert!(list.contains(&mine) && !list.contains(&theirs), "{list}");
    let (_, recs) = two.call("pegboard_recording_list", json!({}));
    assert!(
        recs.contains(&mine[2..]) && !recs.contains(&theirs[2..]),
        "{recs}"
    );
    for (tool, key, prefix) in [
        ("pegboard_session_info", "sessionId", "s_"),
        ("pegboard_session_close", "sessionId", "s_"),
        ("pegboard_files_list", "sessionId", "s_"),
        ("pegboard_recording_get", "recordingId", "r_"),
    ] {
        let arg = |id: &str| format!("{prefix}{}", &id[2..]);
        let (e1, t1) = two.call(tool, json!({ key: arg(&theirs) }));
        let (e2, t2) = two.call(tool, json!({ key: arg(made_up) }));
        assert!(e1 && e2, "{tool}: {t1} / {t2}");
        assert_eq!(
            t1.replace(&theirs[2..], "X"),
            t2.replace(&made_up[2..], "X"),
            "{tool}"
        );
        assert!(
            t1.starts_with("session_not_found: ") || t1.starts_with("recording_not_found: "),
            "{t1}"
        );
    }
    // Still there, and still the first server's.
    assert!(cli_session_ids(home).contains(&theirs));
    let (err, info) = one.call("pegboard_session_info", json!({ "sessionId": theirs }));
    assert!(!err, "{info}");
    one.close_stdin();
    two.close_stdin();
    assert_eq!(one.wait_exit(20), 0);
    assert_eq!(two.wait_exit(20), 0);
}

#[test]
#[ignore]
fn operator_mode_lists_and_closes_but_does_not_drive_other_sessions() {
    let origin = fixture();
    let home = shared_home();
    let mut one = Mcp::start(home, &["--deny", "eval"]);
    let theirs = one.create(json!({ "url": format!("{origin}/counter") }));
    let mut op = Mcp::start(home, &["--operator"]);
    let (_, list) = op.call("pegboard_session_list", json!({}));
    assert!(list.contains(&theirs), "{list}");
    let (err, info) = op.call("pegboard_session_info", json!({ "sessionId": theirs }));
    assert!(!err && info.contains("deny eval"), "{info}");
    let rid = format!("r_{}", &theirs[2..]);
    let (err, rec) = op.call("pegboard_recording_get", json!({ "recordingId": rid }));
    assert!(!err && rec.contains(&theirs), "{rec}");
    // Driving needs the session's token, which the operator does not get.
    let (err, files) = op.call("pegboard_files_list", json!({ "sessionId": theirs }));
    assert!(err && files.starts_with("session_not_found: "), "{files}");
    let names: Vec<String> = op.request("tools/list", json!({}))["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        names.contains(&"pegboard_browser_stop".to_string()),
        "{names:?}"
    );
    let (err, profiles) = op.call("pegboard_profile_list", json!({}));
    assert!(!err && profiles.contains("default"), "{profiles}");
    let (err, text) = op.call("pegboard_session_close", json!({ "sessionId": theirs }));
    assert!(!err, "{text}");
    assert!(!cli_session_ids(home).contains(&theirs));
    let (err, text) = one.call("pegboard_session_info", json!({ "sessionId": theirs }));
    assert!(err && text.starts_with("session_not_found: "), "{text}");
    one.close_stdin();
    op.close_stdin();
    assert_eq!(one.wait_exit(20), 0);
    assert_eq!(op.wait_exit(20), 0);
}

#[test]
#[ignore]
fn server_policy_is_attached_to_every_session() {
    let home = shared_home();
    let mut mcp = Mcp::start(home, &["--deny", "eval", "--allow-domain", "listed.test"]);
    let (err, text) = mcp.call("pegboard_session_create", json!({}));
    assert!(
        !err && text.contains("deny eval") && text.contains("allow-domain listed.test"),
        "{text}"
    );
    let id = text
        .split_whitespace()
        .find(|w| w.starts_with("s_"))
        .unwrap()
        .to_string();
    // As the daemon keeps it (seen by an operator, which reads it from the daemon).
    let mut op = Mcp::start(home, &["--operator"]);
    let (err, info) = op.call("pegboard_session_info", json!({ "sessionId": id }));
    assert!(
        !err && info.contains("deny eval") && info.contains("allow-domain listed.test"),
        "{info}"
    );
    // Policy keys are refused, and no session is made.
    let before = cli_session_ids(home).len();
    let (err, text) = mcp.call(
        "pegboard_session_create",
        json!({ "policy": { "deny": [] } }),
    );
    assert!(err && text.starts_with("invalid_params: "), "{text}");
    let (err, text) = mcp.call("pegboard_session_create", json!({ "allowDomains": [] }));
    assert!(err && text.starts_with("invalid_params: "), "{text}");
    assert_eq!(cli_session_ids(home).len(), before);
    mcp.close_stdin();
    op.close_stdin();
    assert_eq!(mcp.wait_exit(20), 0);
    assert_eq!(op.wait_exit(20), 0);
}

#[test]
#[ignore]
fn adopted_session_keeps_its_own_wider_policy() {
    let home = shared_home();
    let given = cli_create(home, &["--allow", "state"]);
    let mut mcp = Mcp::start_env(
        home,
        &[("PEGBOARD_SESSION", given.as_str())],
        &["--deny", "eval"],
    );
    let (err, info) = mcp.call(
        "pegboard_session_info",
        json!({ "sessionId": id_of(&given) }),
    );
    assert!(!err, "{info}");
    assert!(
        info.contains("allow state") && info.contains("deny -"),
        "{info}"
    );
    let made = mcp.create(json!({}));
    let (_, info) = mcp.call("pegboard_session_info", json!({ "sessionId": made }));
    assert!(
        info.contains("allow -") && info.contains("deny eval"),
        "{info}"
    );
    mcp.close_stdin();
    assert_eq!(mcp.wait_exit(20), 0);
    assert_no_token(&mcp, home, &given);
    assert!(cli_session_ids(home).contains(&id_of(&given).to_string()));
    assert_eq!(home.run(&["session", "close", &given]).0, 0);
}

/// The name `PEGBOARD_TEST_MCP_SLOW_CREATE` makes a create of wait two minutes.
const SLOW: &str = "pegboard-test-slow-create";

fn session_names(home: &Home) -> Vec<String> {
    let (code, out, err) = home.run(&["session", "list", "--json"]);
    assert_eq!(code, 0, "{err}");
    json(&out)
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap_or("").to_string())
        .collect()
}

#[test]
#[ignore]
fn a_client_that_stops_reading_still_ends_on_sigterm() {
    let home = shared_home();
    let mut mcp = Mcp::start(home, &[]);
    let a = mcp.create(json!({}));
    mcp.paused.store(true, Ordering::SeqCst);
    // Enough answers to fill the stdout pipe and the server's queue behind it.
    for _ in 0..300 {
        mcp.request_no_wait("tools/list", json!({}));
    }
    std::thread::sleep(Duration::from_secs(2));
    let started = Instant::now();
    mcp.signal(libc::SIGTERM);
    assert_eq!(mcp.wait_exit(20), 0, "{}", mcp.stderr());
    eprintln!("ended {:?} after SIGTERM", started.elapsed());
    assert!(!cli_session_ids(home).contains(&a));
}

/// A server with one session and a create running (held for two minutes by the hook).
fn with_a_call_running(home: &Home) -> (Mcp, String) {
    let mut mcp = Mcp::start_env(home, &[("PEGBOARD_TEST_MCP_SLOW_CREATE", SLOW)], &[]);
    let a = mcp.create(json!({}));
    mcp.request_no_wait(
        "tools/call",
        json!({ "name": "pegboard_session_create", "arguments": { "name": SLOW } }),
    );
    std::thread::sleep(Duration::from_secs(1));
    (mcp, a)
}

#[test]
#[ignore]
fn stdin_end_during_a_running_call_ends_the_server() {
    let home = shared_home();
    let (mut mcp, a) = with_a_call_running(home);
    mcp.close_stdin();
    assert_eq!(mcp.wait_exit(20), 0, "{}", mcp.stderr());
    assert!(!cli_session_ids(home).contains(&a));
    assert!(!session_names(home).contains(&SLOW.to_string()));
}

#[test]
#[ignore]
fn sigterm_during_a_running_call_ends_the_server() {
    let home = shared_home();
    let (mut mcp, a) = with_a_call_running(home);
    mcp.signal(libc::SIGTERM);
    assert_eq!(mcp.wait_exit(20), 0, "{}", mcp.stderr());
    assert!(!cli_session_ids(home).contains(&a));
}

#[test]
#[ignore]
fn sessions_are_closed_on_sigint_and_sighup() {
    let home = shared_home();
    for sig in [libc::SIGINT, libc::SIGHUP] {
        let mut mcp = Mcp::start(home, &[]);
        let a = mcp.create(json!({}));
        mcp.signal(sig);
        assert_eq!(mcp.wait_exit(20), 0, "signal {sig}: {}", mcp.stderr());
        assert!(!cli_session_ids(home).contains(&a), "signal {sig}");
    }
}

#[test]
#[ignore]
fn a_second_sigint_ends_at_once() {
    let home = shared_home();
    let (mut mcp, a) = with_a_call_running(home);
    mcp.signal(libc::SIGINT);
    // The session is closed; the server now waits (up to 10 s) for the running call.
    let deadline = Instant::now() + Duration::from_secs(15);
    while cli_session_ids(home).contains(&a) {
        assert!(Instant::now() < deadline, "the session was not closed");
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(500));
    let started = Instant::now();
    mcp.signal(libc::SIGINT);
    assert_eq!(mcp.wait_exit(3), 130, "{}", mcp.stderr());
    assert!(started.elapsed() < Duration::from_secs(3));
}

/// A client that writes its requests and closes stdin at once (`printf ... | pegboard mcp`)
/// gets every answer. Needs no browser: `PEGBOARD_CHROME` names nothing, so the daemon the
/// server starts in the background fails on its own; these requests never need it.
#[test]
fn requests_written_before_stdin_closes_are_all_answered() {
    for round in 0..20 {
        let home = Home::new();
        let no_chrome = home.path().join("no-chrome");
        let mut child = home
            .command(&["mcp"])
            .env("PEGBOARD_CHROME", &no_chrome)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = [
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                                "clientInfo": { "name": "e2e", "version": "1" } } }),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" }),
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }),
        ]
        .iter()
        .map(|m| format!("{m}\n"))
        .collect::<String>();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input.as_bytes()).unwrap();
        drop(stdin);
        let mut out = String::new();
        let mut stdout = child.stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            let _ = stdout.read_to_string(&mut out);
            out
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("round {round}: pegboard mcp did not exit after stdin closed");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(status.code(), Some(0), "round {round}");
        let out = reader.join().unwrap();
        let ids: Vec<Value> = out
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap()["id"].clone())
            .collect();
        assert_eq!(ids, [json!(1), json!(2), json!(3)], "round {round}: {out}");
    }
}
