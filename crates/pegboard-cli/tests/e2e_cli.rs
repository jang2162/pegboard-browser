//! End-to-end tests of the `pegboard` binary. Tests that start a browser are `#[ignore]`d:
//! run them with `cargo test -p pegboard-cli -- --ignored --test-threads=1`.
//!
//! Browser tests use the binary's one daemon and browser (`shared_home()`, own sessions closed
//! at the end), except those that make their own `Home` because they test starting, stopping,
//! locking, signals or leftovers: `full_flow`, `daemon_exits_when_browser_is_closed`,
//! `leftovers_of_a_dead_pegboard_are_cleaned_on_start`, `concurrent_browser_start_yields_one_daemon`,
//! `sigterm_closes_sessions_then_removes_socket`, `waiting_start_spawns_when_lock_is_released`,
//! `browser_start_waits_out_a_stopping_daemon`, `idle_session_is_closed_after_the_expiry` (the
//! daemon needs `PEGBOARD_TEST_IDLE_SECS` in its environment, which is read once at its start).

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::*;

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
    let list = json(&out);
    let manifests = &list["recordings"];
    assert_eq!(manifests.as_array().unwrap().len(), 1);
    let rid = manifests[0]["recordingId"].as_str().unwrap().to_string();
    let (code, out, _) = home.run(&["recording", "show", &rid]);
    assert_eq!(code, 0);
    for word in ["snapshot", "click", "get"] {
        assert!(
            out.lines().any(|l| l.contains(word)),
            "no {word} line in:\n{out}"
        );
    }

    let (code, _, err) = home.run(&["session", "close", &sid]);
    assert_eq!(code, 0, "{err}");
    assert!(
        !home
            .path()
            .join("sessions")
            .join(id_of(&sid))
            .join("files")
            .exists(),
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
    let home = shared_home();
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

    let rid = format!("r_{}", &id_of(&sid)[2..]);
    let (code, out, _) = home.run(&["recording", "show", &rid, "--json"]);
    assert_eq!(code, 0);
    let actions = json(&out)["actions"].as_array().unwrap().clone();
    assert!(actions.iter().any(|a| a["ok"] == false), "{actions:?}");
    assert_eq!(home.run(&["session", "close", &sid]).0, 0);
}

#[test]
#[ignore]
fn missing_session_is_a_usage_error() {
    let home = shared_home();
    let (code, _, err) = home.run(&["snapshot"]);
    assert_eq!(code, 2);
    assert!(err.contains("no session"), "{err}");
}

#[test]
fn missing_session_is_a_usage_error_without_daemon() {
    let home = Home::new();
    let (code, _, err) = home.run(&["snapshot"]);
    assert_eq!(code, 2, "{err}");
    assert!(
        err.contains("no session: pass --session or set PEGBOARD_SESSION"),
        "{err}"
    );
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
    stale_socket(&socket);
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
    let home = shared_home();
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
    let (code, _, err) = home.run_env(
        &[("PEGBOARD_HOME", long.to_str().unwrap())],
        &["session", "list"],
    );
    assert_eq!(code, 2, "{err}");
    assert!(
        err.contains("PEGBOARD_HOME is too long for a unix socket path"),
        "{err}"
    );
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

/// Leaves a socket file nobody listens on. Cause of the flake this guards against (measured,
/// 2 of 4 runs): macOS has no SOCK_CLOEXEC, so `UnixListener::bind` sets close-on-exec in a
/// second step; another test thread that spawns a `pegboard` child between the two steps leaks
/// the listener's descriptor into it, the "stale" socket keeps accepting connections until that
/// child exits, and `Client::connect` returned Ok instead of `not_running`. Test-side only: the
/// product code never binds and spawns concurrently like this. The helper retries until a
/// connect is really refused.
fn stale_socket(path: &Path) {
    for _ in 0..100 {
        let _ = std::fs::remove_file(path);
        drop(std::os::unix::net::UnixListener::bind(path).unwrap());
        match std::os::unix::net::UnixStream::connect(path) {
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => return,
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    panic!("could not leave a stale socket at {}", path.display());
}

#[test]
fn stale_socket_is_removed_only_when_lock_is_free() {
    use pegboard_core::api::Client;
    use pegboard_core::lock::DaemonLock;
    use pegboard_core::paths::paths_in;

    let home = Home::new();
    let paths = paths_in(home.path());
    stale_socket(&paths.socket);
    {
        let _held = DaemonLock::try_acquire(&paths)
            .unwrap()
            .expect("lock is free");
        let e = Client::connect(&paths).err().expect("not running");
        assert_eq!(e.code, "not_running");
        assert!(
            paths.socket.exists(),
            "a starting daemon's socket is not deleted"
        );
    }
    // The stale socket may still be held open by a concurrently spawned child that inherited
    // the listener descriptor (macOS sets close-on-exec in a second step); it goes when that
    // child exits, so the removal may take a moment to show.
    let mut last_lock_free = false;
    for _ in 0..40 {
        // Tell "the lock is still held" (a spawning test thread's copy of the descriptor)
        // from "connect returned something else".
        let lock_free = DaemonLock::try_acquire(&paths).unwrap().is_some();
        let e = Client::connect(&paths).err().expect("not running");
        assert_eq!(e.code, "not_running");
        if !paths.socket.exists() {
            break;
        }
        last_lock_free = lock_free;
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !paths.socket.exists(),
        "with the lock free the stale file goes (lock free at the last check: {last_lock_free})"
    );
}

#[test]
fn second_daemon_refuses_when_lock_is_held() {
    use pegboard_core::lock::DaemonLock;
    use pegboard_core::paths::paths_in;

    let home = Home::new();
    let paths = paths_in(home.path());
    let _held = DaemonLock::try_acquire(&paths)
        .unwrap()
        .expect("lock is free");
    let started = Instant::now();
    // No browser can start even if the lock check regresses, and the run has a hard limit.
    let (code, _, err) = home.run_limited(
        &[("PEGBOARD_CHROME", "/nonexistent/chrome")],
        &["daemon"],
        10,
    );
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
    let home = shared_home();
    let url = format!("{origin}/counter");
    let (code, out, err) = home.run(&["session", "create", "--url", &url]);
    assert_eq!(code, 0, "{err}");
    let a = session_handle(&out);
    let (code, out, err) = home.run(&["session", "create", "--url", &url]);
    assert_eq!(code, 0, "{err}");
    let b = session_handle(&out);

    // Only the id, or the id with another session's token: the same as no such session.
    // The id alone is a usage error, caught by the CLI before it asks the daemon.
    let (code, _, err) = home.run(&["--session", id_of(&a), "snapshot"]);
    assert_eq!(code, 2);
    assert!(err.contains("session handle must be"), "{err}");
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
    assert!(
        ids.contains(&id_of(&b).to_string()),
        "B is still listed: {ids:?}"
    );

    // With the right handles everything works.
    let (code, out, err) = home.run(&["--session", &a, "get", "url"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("/counter"));
    assert_eq!(home.run(&["session", "info", &b]).0, 0);
    // No listing shows a token.
    let (_, listed, _) = home.run(&["session", "list", "--json"]);
    let (_, info, _) = home.run(&["session", "info", &b, "--json"]);
    for text in [&listed, &info] {
        assert!(
            !text.contains(token_of(&a)) && !text.contains(token_of(&b)),
            "{text}"
        );
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
    // Daemons of other homes may be running (the shared home's): only the ones this test
    // starts count, found as the pids that were not there before.
    let daemon_pids = || -> Vec<i32> {
        processes(&format!("{} daemon", bin()))
            .iter()
            .filter_map(|l| l.split_whitespace().next()?.parse().ok())
            .collect()
    };
    let others = daemon_pids();
    let ours = || -> Vec<i32> {
        daemon_pids()
            .into_iter()
            .filter(|p| !others.contains(p))
            .collect()
    };
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
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // The daemon that lost the lock retries it for about a second before it gives up.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut daemons = ours();
    while daemons.len() > 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        daemons = ours();
    }
    assert_eq!(daemons.len(), 1, "{daemons:?}");
    assert_eq!(
        Some(daemons[0]),
        home.daemon_pid(),
        "the survivor serves the home"
    );
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
    if let Some(pid) = std::fs::read_to_string(pid_file)
        .ok()
        .and_then(|t| t.trim().parse().ok())
    {
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

#[test]
fn session_handle_without_token_is_a_usage_error() {
    let home = Home::new();
    for value in ["s_abcdefgh", "s_abcdefgh."] {
        let (code, _, err) = home.run(&["--session", value, "snapshot"]);
        assert_eq!(code, 2, "{value}: {err}");
        assert!(
            err.contains("session handle must be <id>.<token> (the first line printed by `pegboard session create`)"),
            "{err}"
        );
    }
}

#[test]
fn waiting_start_spawns_when_lock_is_released() {
    use pegboard_core::lock::DaemonLock;
    use pegboard_core::paths::paths_in;

    let home = Home::new();
    let paths = paths_in(home.path());
    let held = DaemonLock::try_acquire(&paths)
        .unwrap()
        .expect("lock is free");
    let child = home
        .command(&["browser", "start"])
        .env("PEGBOARD_CHROME", "/nonexistent/chrome")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    // The "daemon" that held the lock dies without ever serving.
    std::thread::sleep(Duration::from_secs(2));
    drop(held);
    let out = child.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    assert!(err.contains("the daemon exited"), "{err}");
    assert!(!err.contains("did not answer"), "{err}");
}

#[test]
#[ignore]
fn browser_start_waits_out_a_stopping_daemon() {
    let home = Home::new();
    assert_eq!(home.run(&["browser", "start"]).0, 0);
    let first = home.daemon_pid().expect("daemon.pid");
    let (_, out, _) = home.run(&["browser", "status", "--json"]);
    assert_eq!(json(&out)["stopping"], false);
    let mut stop = home
        .command(&["browser", "stop"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    // Let the stop begin.
    std::thread::sleep(Duration::from_millis(100));
    let (code, _, err) = home.run(&["browser", "start"]);
    assert_eq!(code, 0, "{err}");
    let _ = stop.wait();
    let (_, out, _) = home.run(&["browser", "status", "--json"]);
    let status = json(&out);
    assert_eq!(status["running"], true);
    assert_eq!(status["stopping"], false);
    let second = home.daemon_pid().expect("daemon.pid");
    assert_ne!(first, second, "a new daemon serves");
}

#[test]
#[ignore]
fn undelivered_session_create_is_cleaned_up() {
    let origin = fixture();
    let home = shared_home();
    let (_, out, _) = home.run(&["session", "list", "--json"]);
    let before = json(&out).as_array().unwrap().len();
    assert_eq!(home.run(&["profile", "create", "solo"]).0, 0);
    // Creation takes a page load: the CLI is killed while the daemon is still working.
    let mut child = home
        .command(&[
            "session",
            "create",
            "--profile",
            "solo",
            "--url",
            &format!("{origin}/counter"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let _ = child.kill();
    let _ = child.wait();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (_, out, _) = home.run(&["session", "list", "--json"]);
        let empty = json(&out)
            .as_array()
            .map(|a| a.len() == before)
            .unwrap_or(false);
        let (code, _, _) = home.run(&["profile", "delete", "solo"]);
        if empty && code == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the abandoned session was not discarded"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
#[ignore]
fn handle_in_command_position_never_reaches_the_log() {
    let origin = fixture();
    let home = shared_home();
    let (code, out, err) = home.run(&["session", "create", "--url", &format!("{origin}/counter")]);
    assert_eq!(code, 0, "{err}");
    let handle = session_handle(&out);

    // The handle typed where the command goes: the command is refused, and its first word
    // (the handle) must not be logged.
    let (code, _, err) = home.run_env(&[("PEGBOARD_SESSION", &handle)], &[&handle, "snapshot"]);
    // (The refusal message names the word to the caller who typed it; the log never does.)
    assert_eq!(code, 2, "{err}");
    // Positive control: the same session logs an accepted command.
    assert_eq!(
        home.run_env(&[("PEGBOARD_SESSION", &handle)], &["get", "url"])
            .0,
        0
    );

    let rid = format!("r_{}", &id_of(&handle)[2..]);
    let (code, shown, _) = home.run(&["recording", "show", &rid]);
    assert_eq!(code, 0);
    let (_, shown_json, _) = home.run(&["recording", "show", &rid, "--json"]);
    assert!(
        json(&shown_json)["actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["ok"] == false),
        "the refused command is logged"
    );
    for text in [&shown, &shown_json] {
        assert!(!text.contains(token_of(&handle)), "{text}");
    }
    let mut stack = vec![home.path().join("recordings")];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else {
                let text = std::fs::read_to_string(e.path()).unwrap_or_default();
                assert!(
                    !text.contains(token_of(&handle)),
                    "token in {}",
                    e.path().display()
                );
            }
        }
    }
    assert_eq!(home.run(&["session", "close", &handle]).0, 0);
}

#[test]
#[ignore]
fn tab_new_opens_a_data_url() {
    let home = shared_home();
    let (code, out, err) = home.run(&["session", "create"]);
    assert_eq!(code, 0, "{err}");
    let sid = session_handle(&out);
    let (code, _, err) = home.run(&[
        "--session",
        &sid,
        "tab",
        "new",
        "data:text/html,<title>d</title>",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = home.run(&["--session", &sid, "get", "title"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains('d') && !out.contains("about:blank"), "{out}");
    let (code, out, err) = home.run(&["--session", &sid, "get", "url"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("data:text/html"), "{out}");
    assert_eq!(home.run(&["session", "close", &sid]).0, 0);
}

#[test]
#[ignore]
fn oversized_command_line_exits_2_with_limit_exceeded() {
    let home = shared_home();
    let (code, out, err) = home.run(&["session", "create"]);
    assert_eq!(code, 0, "{err}");
    let sid = session_handle(&out);
    let word = "x".repeat(64 * 1024 + 1);
    let (code, _, err) = home.run(&["--session", &sid, "eval", &word]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("limit_exceeded"), "{err}");
    assert!(!err.contains("xxxx"), "the error quotes the word");
    // The session still works.
    assert_eq!(home.run(&["--session", &sid, "get", "url"]).0, 0);
    assert_eq!(home.run(&["session", "close", &sid]).0, 0);
}

#[test]
#[ignore]
fn idle_session_is_closed_after_the_expiry() {
    let home = Home::new();
    let (code, _, err) = home.run_env(
        &[
            ("PEGBOARD_TEST_IDLE_SECS", "3"),
            ("PEGBOARD_TEST_HOOKS", "1"),
        ],
        &["browser", "start"],
    );
    assert_eq!(code, 0, "{err}");
    let create = |extra: &[&str]| {
        let mut args = vec!["session", "create"];
        args.extend_from_slice(extra);
        let (code, out, err) = home.run(&args);
        assert_eq!(code, 0, "{err}");
        session_handle(&out)
    };
    let idle = create(&[]);
    let kept = create(&["--no-expire"]);
    let listed = |id: &str| {
        let (_, out, _) = home.run(&["session", "list", "--json"]);
        json(&out).as_array().unwrap().iter().any(|s| s["id"] == id)
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while listed(id_of(&idle)) {
        assert!(Instant::now() < deadline, "the idle session was not closed");
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(listed(id_of(&kept)), "a no-expire session was closed");
    // A command that runs for much longer than the period (and the check interval) is
    // neither cut nor is the session closed under it; the idle time counts from its END.
    // The session sat idle for a moment first.
    let busy = create(&[]);
    std::thread::sleep(Duration::from_secs(1));
    let started = Instant::now();
    let (code, _, err) = home.run(&["--session", &busy, "wait", "8000"]);
    assert_eq!(code, 0, "the long command was cut or refused: {err}");
    assert!(started.elapsed() >= Duration::from_secs(7));
    // It started more than 8 s ago but ended just now: not expired at the next checks.
    assert!(listed(id_of(&busy)), "closed right after its long command");
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        listed(id_of(&busy)),
        "closed 1 s after its last command ended"
    );
    assert_eq!(home.run(&["--session", &busy, "get", "url"]).0, 0);
    let ended = Instant::now();
    // It expires after a further idle period, not before.
    let deadline = Instant::now() + Duration::from_secs(30);
    while listed(id_of(&busy)) {
        assert!(Instant::now() < deadline, "the session never expired");
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(
        ended.elapsed() >= Duration::from_millis(2500),
        "expired {:?} after its last command",
        ended.elapsed()
    );
    assert_eq!(home.run(&["--session", &kept, "get", "url"]).0, 0);
    // Its recording is finalized.
    let rid = format!("r_{}", &id_of(&idle)[2..]);
    let (code, out, _) = home.run(&["recording", "show", &rid, "--json"]);
    assert_eq!(code, 0);
    assert!(json(&out)["manifest"]["endedAt"].is_string());
    // The expired handle no longer works.
    let (code, _, err) = home.run(&["--session", &idle, "get", "url"]);
    assert_ne!(code, 0, "{err}");
    assert_eq!(home.run(&["session", "close", &kept]).0, 0);
}

#[test]
fn browser_start_refuses_a_bad_config() {
    let home = Home::new();
    std::fs::write(
        home.path().join("config.toml"),
        "[recording]\nmax_age_days = 0\n",
    )
    .unwrap();
    // No browser is needed: the check comes before the daemon is started. A missing Chrome
    // keeps a daemon that did start from opening a window.
    let env = [("PEGBOARD_CHROME", "/nonexistent/chrome")];
    let (code, out, err) = home.run_limited(&env, &["browser", "start"], 30);
    assert_eq!(code, 2, "{err}");
    assert!(out.is_empty(), "{out}");
    assert!(
        err.contains("config.toml") && err.contains("max_age_days"),
        "{err}"
    );
    for leftover in ["daemon.log", "daemon.pid", "pegboardd.sock"] {
        assert!(!home.path().join(leftover).exists(), "{leftover}");
    }
    // The daemon reads it again when it starts, and does not start either.
    let (code, _, err) = home.run_limited(&env, &["daemon"], 30);
    assert_ne!(code, 0, "{err}");
    assert!(
        err.contains("config.toml") && err.contains("max_age_days"),
        "{err}"
    );
    assert!(!home.path().join("pegboardd.sock").exists());
    // A missing file is the defaults (here the start fails later, on the missing Chrome).
    std::fs::remove_file(home.path().join("config.toml")).unwrap();
    let (_, _, err) = home.run_limited(&env, &["browser", "start"], 30);
    assert!(!err.contains("config.toml"), "{err}");
}

/// Review Focus 5: a browser that starts a daemon while another daemon holds the lock (a
/// stock-mode daemon, or a second browser instance's) gets exit code 4 and the daemon that
/// holds the lock is left be. The pipe is the browser's: fds 3 and 4 of the child.
#[test]
fn hosted_daemon_exits_4_when_the_lock_is_held() {
    use std::os::unix::process::CommandExt;

    let home = Home::new();
    let paths = pegboard_core::paths::paths_in(home.path());
    let held = pegboard_core::lock::DaemonLock::try_acquire(&paths)
        .unwrap()
        .expect("the lock is free in a new home");
    let pipes = pegboard_core::browser::DevToolsPipes::new().unwrap();
    let mut cmd = home.command(&["daemon", "--cdp-pipe"]);
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    // SAFETY: the closure only makes async-signal-safe system calls.
    unsafe { cmd.pre_exec(pipes.child_setup()) };
    let mut child = cmd.spawn().unwrap();
    // The browser's ends stay open: the daemon must not wait for the pipe to end.
    let _browser_ends = pipes.into_parent_ends();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("the daemon did not end within 10 s");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(pegboard_core::lock::EXIT_LOCK_HELD));
    assert_eq!(pegboard_core::lock::EXIT_LOCK_HELD, 4);
    // It wrote why in its own log (the browser gives it none), and the log is the owner's.
    let log_path = home.path().join("daemon.log");
    let log = std::fs::read_to_string(&log_path).unwrap();
    assert!(log.contains("another daemon holds"), "{log}");
    let mode = std::fs::metadata(&log_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    // The lock is still the holder's.
    assert!(pegboard_core::lock::DaemonLock::try_acquire(&paths)
        .unwrap()
        .is_none());
    drop(held);
}

/// `pegboard daemon --cdp-pipe` without the pipe is a usage error.
#[test]
fn hosted_daemon_without_the_pipe_is_a_usage_error() {
    let home = Home::new();
    // The test runner's fds 3 and 4 are not handed on: `Command` closes them on exec.
    let (code, _, _) = home.run(&["daemon", "--cdp-pipe"]);
    assert_eq!(code, 2);
    let log = std::fs::read_to_string(home.path().join("daemon.log")).unwrap();
    assert!(log.contains("fds 3 and 4"), "{log}");
}

/// A daemon stand-in on the home's socket: answers every request with an empty result and
/// records the methods. After `browser.stop` or `browser.quit` it removes the socket, as the
/// daemon does.
fn fake_daemon(home: &Home) -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
    use std::io::{BufRead, BufReader, Write};
    let socket = home.path().join("pegboardd.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                let req = json(&line);
                let method = req["method"].as_str().unwrap_or("").to_string();
                log.lock().unwrap().push(method.clone());
                let answer = serde_json::json!({ "jsonrpc": "2.0", "id": req["id"], "result": {} });
                let _ = writeln!(writer, "{answer}");
                if method == "browser.stop" || method == "browser.quit" {
                    let _ = std::fs::remove_file(&socket);
                    return;
                }
                line.clear();
            }
        }
    });
    seen
}

/// On the fork, `browser stop` closes the person's windows too: without a terminal it needs
/// `--yes` (usage error, nothing asked of the daemon); with it, it asks `browser.quit`.
#[test]
fn stop_without_a_terminal_needs_yes() {
    let home = Home::new();
    std::fs::write(
        home.path().join("config.toml"),
        "[browser]\nkind = \"fork\"\n",
    )
    .unwrap();
    let seen = fake_daemon(&home);
    let (code, _, err) = home.run(&["browser", "stop"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("--yes"), "{err}");
    assert!(
        seen.lock().unwrap().is_empty(),
        "{:?}",
        seen.lock().unwrap()
    );
    let (code, out, err) = home.run(&["browser", "stop", "--yes"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.trim(), "stopped");
    assert_eq!(*seen.lock().unwrap(), vec!["browser.quit".to_string()]);
}

/// Stock mode stops as before: `browser.stop`, no question, `--yes` or not.
#[test]
fn stock_stop_asks_nothing() {
    let home = Home::new();
    std::fs::write(
        home.path().join("config.toml"),
        "[browser]\nkind = \"stock\"\n",
    )
    .unwrap();
    let seen = fake_daemon(&home);
    let (code, out, err) = home.run(&["browser", "stop"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.trim(), "stopped");
    assert_eq!(*seen.lock().unwrap(), vec!["browser.stop".to_string()]);
}

/// The fork chosen in the file and not installed: a clear error, and no daemon is started.
#[test]
fn fork_start_without_the_app_says_how_to_go_on() {
    let home = Home::new();
    let apps = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[browser]\nkind = \"fork\"\n",
    )
    .unwrap();
    let apps_dir = apps.path().display().to_string();
    let (code, _, err) = home.run_env(
        &[
            ("PEGBOARD_TEST_HOOKS", "1"),
            ("PEGBOARD_TEST_APPLICATIONS", apps_dir.as_str()),
        ],
        &["browser", "start"],
    );
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("Pegboard.app not found; set [browser] kind = \"stock\""),
        "{err}"
    );
    assert!(!home.path().join("daemon.pid").exists());
    // A wrong kind is a usage error, before anything starts.
    std::fs::write(home.path().join("config.toml"), "[browser]\nkind = \"x\"\n").unwrap();
    let (code, _, err) = home.run(&["browser", "start"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("config.toml") && err.contains("kind"), "{err}");
}
