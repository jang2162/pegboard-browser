//! End-to-end tests of the `pegboard` binary. Tests that start a browser are `#[ignore]`d:
//! run them with `cargo test -p pegboard-cli -- --ignored --test-threads=1`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_pegboard")
}

const COUNTER: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>counter</title></head><body><h1>counter</h1><button id="btn">Add</button> <span id="clicks">0</span><script>
document.getElementById('btn').addEventListener('click', () => {
  const el = document.getElementById('clicks'); el.textContent = String(Number(el.textContent) + 1);
});</script></body></html>"#;

/// Serves the counter page on a loopback port for as long as the test process lives.
fn fixture() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{COUNTER}",
                    COUNTER.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            });
        }
    });
    origin
}

/// A fresh pegboard home. Dropping it stops the daemon and its browser, also when the
/// test failed.
struct Home {
    tmp: tempfile::TempDir,
}

impl Home {
    fn new() -> Home {
        Home {
            tmp: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self) -> &Path {
        self.tmp.path()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(bin());
        cmd.env("PEGBOARD_HOME", self.path())
            .env_remove("PEGBOARD_SESSION")
            .args(args)
            .stdin(Stdio::null());
        cmd
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        self.run_env(&[], args)
    }

    fn run_env(&self, env: &[(&str, &str)], args: &[&str]) -> (i32, String, String) {
        let out = self.command(args).envs(env.iter().copied()).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn daemon_pid(&self) -> Option<i32> {
        std::fs::read_to_string(self.path().join("daemon.pid"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.command(&["browser", "stop"]).output();
        if let Some(pid) = self.daemon_pid() {
            // A stale pid file could name an unrelated process: only a `pegboard daemon`
            // of this test binary is killed.
            let cmd = Command::new("ps")
                .args(["-o", "command=", "-p", &pid.to_string()])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            if cmd == format!("{} daemon", bin()) {
                // SAFETY: plain syscall on a verified pegboard daemon.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
        // Only a browser that uses this home's profile directory.
        let _ = Command::new("pkill")
            .args(["-f", "--"])
            .arg(format!("--user-data-dir={}", self.path().join("chrome").display()))
            .output();
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn json(text: &str) -> Value {
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("not json ({e}): {text:?}"))
}

fn base32(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}

/// Line 1 of `session create`: `<session id>.<token>`.
fn session_handle(stdout: &str) -> String {
    let first = stdout.lines().next().unwrap_or("");
    let ok = first.split_once('.').is_some_and(|(id, token)| {
        id.len() == 10 && id.starts_with("s_") && base32(&id[2..]) && token.len() == 26 && base32(token)
    });
    assert!(ok, "first line is not a session handle: {stdout:?}");
    first.to_string()
}

fn id_of(handle: &str) -> &str {
    handle.split('.').next().unwrap()
}

fn token_of(handle: &str) -> &str {
    handle.split_once('.').unwrap().1
}

/// The `ps` lines of processes whose command line contains `needle`.
fn processes(needle: &str) -> Vec<String> {
    let out = Command::new("ps").args(["-axo", "pid,command"]).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(needle))
        .map(String::from)
        .collect()
}

#[test]
#[ignore]
fn full_flow() {
    let origin = fixture();
    let home = Home::new();
    let (code, _, err) = home.run(&["browser", "start"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, _) = home.run(&["browser", "status", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(json(&out)["running"], true);

    let url = format!("{origin}/counter");
    let (code, out, err) = home.run(&["session", "create", "--name", "조사", "--url", &url]);
    assert_eq!(code, 0, "{err}");
    let sid = session_handle(&out);

    let (code, out, err) = home.run(&["--session", &sid, "snapshot", "-i"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("button"), "{out}");
    let (code, _, err) = home.run(&["--session", &sid, "click", "#btn"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = home.run(&["--session", &sid, "get", "url"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("/counter"), "{out}");

    // The session can also come from the environment.
    let (code, out, _) = home.run_env(&[("PEGBOARD_SESSION", &sid)], &["get", "url"]);
    assert_eq!(code, 0);
    assert!(out.contains("/counter"), "{out}");

    let (code, out, _) = home.run(&["session", "list", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(json(&out).as_array().unwrap().len(), 1);

    let (code, out, _) = home.run(&["recording", "list", "--json"]);
    assert_eq!(code, 0);
    let manifests = json(&out);
    assert_eq!(manifests.as_array().unwrap().len(), 1);
    let rid = manifests[0]["recordingId"].as_str().unwrap().to_string();
    let (code, out, _) = home.run(&["recording", "show", &rid]);
    assert_eq!(code, 0);
    for word in ["snapshot", "click", "get"] {
        assert!(out.lines().any(|l| l.contains(word)), "no {word} line in:\n{out}");
    }

    let (code, _, err) = home.run(&["session", "close", &sid]);
    assert_eq!(code, 0, "{err}");
    assert!(
        !home.path().join("sessions").join(id_of(&sid)).join("files").exists(),
        "the session's files directory is deleted on close"
    );
    let (code, _, err) = home.run(&["--session", &sid, "snapshot"]);
    assert_eq!(code, 1);
    assert!(err.contains("session_not_found"), "{err}");

    let (code, _, err) = home.run(&["browser", "stop"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, _) = home.run(&["browser", "status"]);
    assert_eq!(code, 3);
}

#[test]
#[ignore]
fn rejected_flags_exit_2_and_are_logged() {
    let origin = fixture();
    let home = Home::new();
    assert_eq!(home.run(&["browser", "start"]).0, 0);
    let (code, out, err) = home.run(&["session", "create", "--url", &format!("{origin}/counter")]);
    assert_eq!(code, 0, "{err}");
    let sid = session_handle(&out);
    let (code, _, err) = home.run(&["--session", &sid, "snapshot", "--cdp", "9222"]);
    assert_eq!(code, 2);
    assert!(err.contains("argv_rejected"), "{err}");

    // Ids from a request are validated before they name a file or a socket.
    let (code, _, err) = home.run(&["--session", "../escape", "snapshot"]);
    assert_eq!(code, 2);
    assert!(err.contains("not a session id"), "{err}");

    let (_, out, _) = home.run(&["recording", "list", "--json"]);
    let rid = json(&out)[0]["recordingId"].as_str().unwrap().to_string();
    let (code, out, _) = home.run(&["recording", "show", &rid, "--json"]);
    assert_eq!(code, 0);
    let actions = json(&out)["actions"].as_array().unwrap().clone();
    assert!(actions.iter().any(|a| a["ok"] == false), "{actions:?}");
}

#[test]
#[ignore]
fn missing_session_is_a_usage_error() {
    let home = Home::new();
    assert_eq!(home.run(&["browser", "start"]).0, 0);
    let (code, _, err) = home.run(&["snapshot"]);
    assert_eq!(code, 2);
    assert!(err.contains("no session"), "{err}");
}

#[test]
fn missing_session_is_a_usage_error_without_daemon() {
    let home = Home::new();
    let (code, _, err) = home.run(&["snapshot"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("no session: pass --session or set PEGBOARD_SESSION"), "{err}");
}

#[test]
fn commands_without_daemon_exit_3() {
    let home = Home::new();
    let (code, _, err) = home.run(&["session", "list"]);
    assert_eq!(code, 3);
    assert!(err.contains("pegboard browser start"), "{err}");
}

#[test]
fn stale_socket_is_cleaned_up() {
    let home = Home::new();
    let socket: PathBuf = home.path().join("pegboardd.sock");
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    assert!(socket.exists());
    let started = Instant::now();
    let (code, _, err) = home.run(&["session", "list"]);
    assert_eq!(code, 3, "{err}");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!socket.exists(), "the stale socket file is removed");
}

#[test]
#[ignore]
fn daemon_exits_when_browser_is_closed() {
    let home = Home::new();
    assert_eq!(home.run(&["browser", "start"]).0, 0);
    let daemon = home.daemon_pid().expect("daemon.pid");
    let (_, out, _) = home.run(&["browser", "status", "--json"]);
    let chrome = json(&out)["pid"].as_i64().unwrap() as i32;
    // SAFETY: the pid is the Chrome this test's daemon started.
    unsafe { libc::kill(chrome, libc::SIGKILL) };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let gone = home.run(&["browser", "status"]).0 == 3 && !alive(daemon);
        if gone {
            break;
        }
        assert!(Instant::now() < deadline, "the daemon did not end");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
#[ignore]
fn socket_is_owner_only() {
    let home = Home::new();
    assert_eq!(home.run(&["browser", "start"]).0, 0);
    let mode = std::fs::metadata(home.path().join("pegboardd.sock"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
#[ignore]
fn leftovers_of_a_dead_pegboard_are_cleaned_on_start() {
    let home = Home::new();
    let left = home.path().join("sessions/s_aaaaaaaa/files");
    std::fs::create_dir_all(&left).unwrap();
    assert_eq!(home.run(&["browser", "start"]).0, 0);
    assert!(!home.path().join("sessions/s_aaaaaaaa").exists());
}

fn long_home(base: &Path) -> PathBuf {
    base.join("a".repeat(100))
}

#[test]
fn long_home_is_a_clear_usage_error() {
    let home = Home::new();
    let long = long_home(home.path());
    let (code, _, err) = home.run_env(&[("PEGBOARD_HOME", long.to_str().unwrap())], &["session", "list"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("PEGBOARD_HOME is too long for a unix socket path"), "{err}");
    assert!(!long.exists(), "nothing is created");
}

#[test]
fn invalid_ids_exit_2() {
    let home = Home::new();
    for args in [
        vec!["--session", "../x", "snapshot"],
        vec!["--session", "s_short.tok", "snapshot"],
        vec!["session", "info", "bad"],
        vec!["session", "close", "s_UPPERCASE"],
        vec!["recording", "show", "r_bad"],
    ] {
        let (code, _, err) = home.run(&args);
        assert_eq!(code, 2, "{args:?}: {err}");
    }
}

#[test]
fn recording_id_traversal_is_rejected() {
    let home = Home::new();
    let (code, out, err) = home.run(&["recording", "show", "../../etc"]);
    assert_eq!(code, 2, "{err}");
    assert!(out.is_empty());
}

#[test]
fn stale_socket_is_removed_only_when_lock_is_free() {
    use pegboard_core::api::Client;
    use pegboard_core::lock::DaemonLock;
    use pegboard_core::paths::paths_in;

    let home = Home::new();
    let paths = paths_in(home.path());
    drop(std::os::unix::net::UnixListener::bind(&paths.socket).unwrap());
    {
        let _held = DaemonLock::try_acquire(&paths).unwrap().expect("lock is free");
        let e = Client::connect(&paths).err().expect("not running");
        assert_eq!(e.code, "not_running");
        assert!(paths.socket.exists(), "a starting daemon's socket is not deleted");
    }
    let e = Client::connect(&paths).err().expect("not running");
    assert_eq!(e.code, "not_running");
    assert!(!paths.socket.exists(), "with the lock free the stale file goes");
}

#[test]
fn second_daemon_refuses_when_lock_is_held() {
    use pegboard_core::lock::DaemonLock;
    use pegboard_core::paths::paths_in;

    let home = Home::new();
    let paths = paths_in(home.path());
    let _held = DaemonLock::try_acquire(&paths).unwrap().expect("lock is free");
    let started = Instant::now();
    let (code, _, err) = home.run(&["daemon"]);
    assert_ne!(code, 0);
    assert!(err.contains("already running"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!paths.chrome_udd.exists(), "no browser was launched");
    assert!(!home.path().join("daemon.pid").exists());
}

#[test]
#[ignore]
fn session_handle_is_required() {
    let origin = fixture();
    let home = Home::new();
    assert_eq!(home.run(&["browser", "start"]).0, 0);
    let url = format!("{origin}/counter");
    let (code, out, err) = home.run(&["session", "create", "--url", &url]);
    assert_eq!(code, 0, "{err}");
    let a = session_handle(&out);
    let (code, out, err) = home.run(&["session", "create", "--url", &url]);
    assert_eq!(code, 0, "{err}");
    let b = session_handle(&out);

    // Only the id, or the id with another session's token: the same as no such session.
    let (code, _, err) = home.run(&["--session", id_of(&a), "snapshot"]);
    assert_eq!(code, 1);
    assert!(err.contains("session_not_found"), "{err}");
    let mixed = format!("{}.{}", id_of(&a), token_of(&b));
    let (code, _, err) = home.run(&["--session", &mixed, "snapshot"]);
    assert_eq!(code, 1);
    assert!(err.contains("session_not_found"), "{err}");
    let (code, _, err) = home.run(&["session", "info", id_of(&b)]);
    assert_eq!(code, 1);
    assert!(err.contains("session_not_found"), "{err}");
    let (code, _, err) = home.run(&["session", "close", id_of(&b)]);
    assert_eq!(code, 1);
    assert!(err.contains("session_not_found"), "{err}");
    let (_, out, _) = home.run(&["session", "list", "--json"]);
    let ids: Vec<String> = json(&out)
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&id_of(&b).to_string()), "B is still listed: {ids:?}");

    // With the right handles everything works.
    let (code, out, err) = home.run(&["--session", &a, "get", "url"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("/counter"));
    assert_eq!(home.run(&["session", "info", &b]).0, 0);
    // No listing shows a token.
    let (_, listed, _) = home.run(&["session", "list", "--json"]);
    let (_, info, _) = home.run(&["session", "info", &b, "--json"]);
    for text in [&listed, &info] {
        assert!(!text.contains(token_of(&a)) && !text.contains(token_of(&b)), "{text}");
    }
    assert_eq!(home.run(&["session", "close", &b]).0, 0);
    assert_eq!(home.run(&["session", "close", &a]).0, 0);

    // The tokens are in memory only.
    let mut files = vec![home.path().join("daemon.log")];
    let mut stack = vec![home.path().join("recordings")];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else {
                files.push(e.path());
            }
        }
    }
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        for h in [&a, &b] {
            assert!(!text.contains(token_of(h)), "token in {}", f.display());
        }
    }
}

#[test]
#[ignore]
fn concurrent_browser_start_yields_one_daemon() {
    let home = Home::new();
    let children: Vec<_> = (0..2)
        .map(|_| {
            home.command(&["browser", "start"])
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    let daemons = processes(&format!("{} daemon", bin()));
    assert_eq!(daemons.len(), 1, "{daemons:?}");
    let udd = format!("--user-data-dir={}", home.path().join("chrome").display());
    let chromes: Vec<String> = processes(&udd)
        .into_iter()
        .filter(|l| !l.contains("--type="))
        .collect();
    assert_eq!(chromes.len(), 1, "{chromes:?}");
}

#[test]
#[ignore]
fn sigterm_closes_sessions_then_removes_socket() {
    let origin = fixture();
    let home = Home::new();
    assert_eq!(home.run(&["browser", "start"]).0, 0);
    let (code, out, err) = home.run(&["session", "create", "--url", &format!("{origin}/counter")]);
    assert_eq!(code, 0, "{err}");
    let handle = session_handle(&out);
    // Makes the session's agent-browser daemon start.
    assert_eq!(home.run(&["--session", &handle, "get", "url"]).0, 0);
    let daemon = home.daemon_pid().expect("daemon.pid");
    // SAFETY: the pid is this test's own daemon.
    unsafe { libc::kill(daemon, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(30);
    while alive(daemon) {
        assert!(Instant::now() < deadline, "the daemon did not end");
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(!home.path().join("pegboardd.sock").exists());
    let udd = format!("--user-data-dir={}", home.path().join("chrome").display());
    assert!(processes(&udd).is_empty(), "a browser of this home is left");
    let pid_file = pegboard_core::paths::paths_in(home.path())
        .ab_socket_dir
        .join(format!("peg-{}.pid", id_of(&handle)));
    if let Some(pid) = std::fs::read_to_string(pid_file).ok().and_then(|t| t.trim().parse().ok()) {
        assert!(!alive(pid), "an agent-browser daemon is left");
    }
    let manifest = home
        .path()
        .join("recordings")
        .join(format!("r_{}", &id_of(&handle)[2..]))
        .join("manifest.json");
    let m = json(&std::fs::read_to_string(manifest).unwrap());
    assert!(m["endedAt"].is_string(), "recording is finalized: {m}");
}
