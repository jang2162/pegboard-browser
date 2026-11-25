//! The Pegboard app starts pegboardd itself and hands it the DevTools pipe (phase 3a task 9,
//! design 4.2, F8, F11). Every test needs `PEGBOARD_TEST_APP=<Pegboard.app>` (a bundle made
//! by `browser/scripts/package-dev.sh`, read with test hooks on, which the tests turn on)
//! and is skipped otherwise. Each test opens its own instance of the app the way a person
//! does (`open -n -a`), with a temporary `--user-data-dir` and `PEGBOARD_HOME`, and talks
//! to its daemon with the app's own `Contents/Helpers/pegboard`.
//!
//! Run in a GUI session: `PEGBOARD_TEST_APP=<app> cargo test -p pegboard-core --test
//! e2e_bootstrap -- --ignored --test-threads=1`.
//!
//! Phase 3b task 10: when the browser gives up restarting the daemon it shows the person a
//! notice with a restart button (`//pegboard/ui/daemon_notice.h`), seen through the
//! browser's dialog log and answered by its answer hook (test hooks of the browser).
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

/// How long the app may take to have its daemon answering.
const DAEMON_START: Duration = Duration::from_secs(10);
/// How long a quit, a kill or a restart may take to show.
const SETTLE: Duration = Duration::from_secs(10);
/// The name the browser and the daemon's guard both resolve to the test server.
const TEST_NAME: &str = "example.test";

fn app_under_test() -> Option<PathBuf> {
    std::env::set_var("PEGBOARD_TEST_HOOKS", "1");
    let app = pegboard_core::testenv::var("PEGBOARD_TEST_APP").map(PathBuf::from);
    if app.is_none() {
        eprintln!("skipped: needs PEGBOARD_TEST_APP=<Pegboard.app>");
    }
    app
}

/// A loopback HTTP server that answers every path with a page and remembers when each path
/// was last asked for. `/ping/<name>` pages ask for themselves again every 300 ms, so a tab
/// showing one is alive exactly while its pings keep coming.
struct Server {
    port: u16,
    seen: Arc<Mutex<HashMap<String, Instant>>>,
}

impl Server {
    fn start() -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<HashMap<String, Instant>>> = Arc::default();
        let record = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let record = record.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split(' ').nth(1))
                        .and_then(|p| p.split('?').next())
                        .unwrap_or("/")
                        .to_string();
                    record.lock().unwrap().insert(path.clone(), Instant::now());
                    let body = if path.starts_with("/ping/") {
                        format!(
                            "<!doctype html><title>ping</title><script>\
                             setInterval(() => fetch('{path}?' + Date.now()), 300);</script>"
                        )
                    } else {
                        "<!doctype html><title>bootstrap</title><p>bootstrap</p>".to_string()
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncache-control: no-store\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                });
            }
        });
        Server { port, seen }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{TEST_NAME}:{}{path}", self.port)
    }

    /// How long ago `path` was last asked for.
    fn since(&self, path: &str) -> Option<Duration> {
        self.seen.lock().unwrap().get(path).map(|t| t.elapsed())
    }
}

/// One running instance of the app with its own pegboard home and user data directory.
/// Dropping it kills the instance and every daemon of its home.
struct App {
    app: PathBuf,
    home: tempfile::TempDir,
    udd: tempfile::TempDir,
    server: Server,
    /// The browser's main process, known once its first daemon answered.
    browser: Option<i32>,
    /// More variables for the app (test hooks of the daemon).
    extra_env: Vec<(String, String)>,
}

impl App {
    fn new(app: PathBuf) -> App {
        App {
            app: app.canonicalize().unwrap(),
            home: tempfile::tempdir().unwrap(),
            udd: tempfile::tempdir().unwrap(),
            server: Server::start(),
            browser: None,
            extra_env: Vec::new(),
        }
    }

    fn with_env(mut self, vars: &[(&str, &str)]) -> App {
        self.extra_env
            .extend(vars.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        self
    }

    /// The browser's dialog log (`PEGBOARD_TEST_DIALOG_LOG`) and its answer hook
    /// (`PEGBOARD_TEST_DIALOG_ANSWER`, `*=restart`, `*=close` or `*=none` for the notice).
    fn with_notice_hooks(self, answer: &str) -> App {
        let log = self.notice_log().display().to_string();
        self.with_env(&[
            ("PEGBOARD_TEST_DIALOG_LOG", &log),
            ("PEGBOARD_TEST_DIALOG_ANSWER", answer),
        ])
    }

    fn notice_log(&self) -> PathBuf {
        self.home.path().join("dialogs.jsonl")
    }

    /// The dialog log's entries of `kind` (`daemon_stopped`, `daemon_notice_closed`).
    fn notices(&self, kind: &str) -> Vec<Value> {
        std::fs::read_to_string(self.notice_log())
            .unwrap_or_default()
            .lines()
            .map(json)
            .filter(|e| e["kind"] == kind)
            .collect()
    }

    /// Kills each daemon the browser starts, four times within the minute (F8: the fourth
    /// end is not followed by a restart), the first being the one answering that is not
    /// `after` (a daemon already gone). The last daemon's pid.
    fn kill_four_times(&mut self, after: Option<i32>) -> i32 {
        let start = Instant::now();
        let mut last = after;
        for kill in 1..=4 {
            let pid = self.wait_for_daemon(last, SETTLE);
            // SAFETY: plain syscall on this test's daemon.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            assert!(eventually(SETTLE, || !alive(pid)), "kill {kill}");
            last = Some(pid);
        }
        assert!(
            start.elapsed() < Duration::from_secs(50),
            "too slow for the window"
        );
        last.unwrap()
    }

    fn helper(&self) -> PathBuf {
        self.app.join("Contents/Helpers/pegboard")
    }

    fn rules(&self) -> String {
        format!("MAP {TEST_NAME} 127.0.0.1")
    }

    /// The test variables the app (and so its daemon) is started with.
    fn test_env(&self) -> Vec<(String, String)> {
        vec![
            (
                "PEGBOARD_HOME".into(),
                self.home.path().display().to_string(),
            ),
            ("PEGBOARD_TEST_HOOKS".into(), "1".into()),
            ("PEGBOARD_TEST_HOST_RULES".into(), self.rules()),
        ]
        .into_iter()
        .chain(self.extra_env.iter().cloned())
        .collect()
    }

    /// `open -n -a <app>` as a person would, with `extra` browser arguments. The test-only
    /// `--use-mock-keychain` keeps the first "Pegboard Safe Storage" keychain prompt from
    /// holding every HTTP request on an unattended screen.
    fn open(&self, extra: &[&str]) {
        let mut cmd = Command::new("open");
        cmd.arg("-n").arg("-a").arg(&self.app);
        for (k, v) in self.test_env() {
            cmd.arg("--env").arg(format!("{k}={v}"));
        }
        cmd.arg("--args")
            .arg(format!("--user-data-dir={}", self.udd.path().display()))
            .arg("--use-mock-keychain")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg(format!("--host-resolver-rules={}", self.rules()))
            .args(extra);
        let status = cmd.status().unwrap();
        assert!(status.success(), "open failed: {status}");
    }

    /// Runs the app's own `pegboard` against this home.
    fn cli(&self, args: &[&str], session: Option<&str>) -> (i32, String, String) {
        let mut cmd = Command::new(self.helper());
        cmd.env("PEGBOARD_HOME", self.home.path())
            .env_remove("PEGBOARD_SESSION")
            .args(args)
            .stdin(Stdio::null());
        if let Some(s) = session {
            cmd.env("PEGBOARD_SESSION", s);
        }
        let out = cmd.output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn daemon_pid(&self) -> Option<i32> {
        std::fs::read_to_string(self.home.path().join("daemon.pid"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// The pid of a daemon that answers and is not `not`, within `limit`.
    fn wait_for_daemon(&mut self, not: Option<i32>, limit: Duration) -> i32 {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(pid) = self.daemon_pid().filter(|p| Some(*p) != not && alive(*p)) {
                let (code, out, _) = self.cli(&["browser", "status", "--json"], None);
                if code == 0 && json(&out)["running"] == true {
                    let parent = parent_of(pid);
                    match self.browser {
                        None => self.browser = Some(parent),
                        Some(b) => assert_eq!(parent, b, "the daemon is not the browser's child"),
                    }
                    assert_eq!(
                        command_of(parent),
                        self.app
                            .join("Contents/MacOS/Pegboard")
                            .display()
                            .to_string(),
                        "the daemon's parent is not the app"
                    );
                    return pid;
                }
            }
            assert!(
                Instant::now() < deadline,
                "no daemon answered within {limit:?}; daemon.log:\n{}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn browser(&self) -> i32 {
        self.browser
            .expect("the browser is known once a daemon answered")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.home.path().join("daemon.log")).unwrap_or_default()
    }

    /// `session create` and the session's handle.
    fn create_session(&self, path: &str) -> String {
        let (code, out, err) = self.cli(
            &["session", "create", "--url", &self.server.url(path)],
            None,
        );
        assert_eq!(
            code,
            0,
            "session create: {err}\ndaemon.log:\n{}",
            self.log()
        );
        out.lines().next().unwrap().to_string()
    }

    /// The daemons of this browser: its children that run `pegboard daemon --cdp-pipe`.
    fn daemons(&self) -> Vec<i32> {
        children_of(self.browser())
            .into_iter()
            .filter(|p| command_of(*p).ends_with("pegboard daemon --cdp-pipe"))
            .collect()
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // The agent-browser daemons of this home's sessions: a daemon killed here leaves
        // them, as nothing starts after it to clean up.
        for pid in agent_browsers_of(self.home.path()) {
            if command_of(pid).starts_with(&self.app.display().to_string()) {
                // SAFETY: plain syscall on this test's agent-browser.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
        let mut pids: Vec<i32> = self.browser.into_iter().collect();
        pids.extend(self.daemon_pid());
        for pid in pids {
            // Only processes of this test: the app's browser or its daemon.
            let cmd = command_of(pid);
            if cmd.starts_with(&self.app.display().to_string()) {
                // SAFETY: plain syscall on a verified process of this test.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
        // This instance, and a browser that a daemon of this home launched itself.
        for udd in [
            self.udd.path().to_path_buf(),
            self.home.path().join("chrome"),
        ] {
            let _ = Command::new("pkill")
                .args(["-KILL", "-f", "--"])
                .arg(format!("--user-data-dir={}", udd.display()))
                .output();
        }
    }
}

/// agent-browser's socket directory, shared by every pegboard home of the user
/// (`paths::Paths::ab_socket_dir`).
fn ab_socket_dir() -> PathBuf {
    // SAFETY: getuid has no failure mode.
    PathBuf::from(format!("/tmp/pegboard-{}/ab", unsafe { libc::getuid() }))
}

/// The pid agent-browser recorded for the session `handle`.
fn agent_browser_of(handle: &str) -> Option<i32> {
    let id = handle.split('.').next()?;
    std::fs::read_to_string(ab_socket_dir().join(format!("peg-{id}.pid")))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The recorded agent-browser pids of the sessions under `<home>/sessions`.
fn agent_browsers_of(home: &Path) -> Vec<i32> {
    std::fs::read_dir(home.join("sessions"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| agent_browser_of(&e.file_name().to_string_lossy()))
        .collect()
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

fn ps(field: &str, pid: i32) -> String {
    Command::new("ps")
        .args(["-o", &format!("{field}="), "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn parent_of(pid: i32) -> i32 {
    ps("ppid", pid).parse().unwrap_or(0)
}

fn command_of(pid: i32) -> String {
    let cmd = ps("command", pid);
    // The main executable's command line goes on with its arguments.
    match cmd.find(" --") {
        Some(i) if !cmd.contains("pegboard daemon") => cmd[..i].to_string(),
        _ => cmd,
    }
}

fn children_of(pid: i32) -> Vec<i32> {
    let out = Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

/// `pid`, and below it every process except the subtree of `skip`.
fn tree_without(pid: i32, skip: &[i32]) -> Vec<i32> {
    let mut all = vec![pid];
    let mut i = 0;
    while i < all.len() {
        for c in children_of(all[i]) {
            if !skip.contains(&c) {
                all.push(c);
            }
        }
        i += 1;
    }
    all
}

/// The TCP ports `pid` listens on (`lsof`), as lines.
fn tcp_listeners(pid: i32) -> Vec<String> {
    let out = Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-a", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1)
        .map(str::to_string)
        .collect()
}

/// `true` once `check` holds, polling until `limit`.
fn eventually(limit: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if check() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The manifests under `<home>/recordings` of the session `handle`.
fn manifests_of(home: &Path, handle: &str) -> Vec<Value> {
    let id = handle.split('.').next().unwrap();
    let mut found = Vec::new();
    let mut dirs = vec![home.join("recordings")];
    while let Some(dir) = dirs.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.file_name().is_some_and(|n| n == "manifest.json") {
                let m = json(&std::fs::read_to_string(&p).unwrap_or_default());
                if m["sessionId"] == id {
                    found.push(m);
                }
            }
        }
    }
    found
}

#[test]
#[ignore]
fn app_start_spawns_the_daemon() {
    let Some(app) = app_under_test() else { return };
    let mut a = App::new(app);
    a.open(&[]);
    a.wait_for_daemon(None, DAEMON_START);
    let s = a.create_session("/first");
    let (code, out, err) = a.cli(&["open", &a.server.url("/second")], Some(&s));
    assert_eq!(code, 0, "open: {out}{err}");
    let (code, out, err) = a.cli(&["get", "url"], Some(&s));
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("/second"), "{out}");
    assert!(a.server.since("/second").is_some());
}

/// Neither the browser nor any of its helpers listens on TCP. The daemon is not counted: its
/// session proxies listen on loopback for agent-browser, as in stock mode.
#[test]
#[ignore]
fn no_tcp_listener() {
    let Some(app) = app_under_test() else { return };
    let mut a = App::new(app);
    a.open(&[]);
    let daemon = a.wait_for_daemon(None, DAEMON_START);
    let s = a.create_session("/page");
    let (code, _, err) = a.cli(&["get", "url"], Some(&s));
    assert_eq!(code, 0, "{err}");
    let procs = tree_without(a.browser(), &[daemon]);
    assert!(procs.len() > 2, "the browser has no helpers? {procs:?}");
    for pid in procs {
        let open = tcp_listeners(pid);
        assert!(
            open.is_empty(),
            "{pid} ({}) listens: {open:?}",
            command_of(pid)
        );
    }
}

#[test]
#[ignore]
fn starts_without_any_window() {
    let Some(app) = app_under_test() else { return };
    let mut a = App::new(app);
    a.open(&["--no-startup-window"]);
    a.wait_for_daemon(None, DAEMON_START);
    let s = a.create_session("/windowless");
    let (code, out, err) = a.cli(&["get", "url"], Some(&s));
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("/windowless"), "{out}");
}

/// The browser quits (SIGINT: Chrome's graceful exit, the path of Cmd-Q): the daemon sees
/// the pipe end, closes its sessions and exits.
#[test]
#[ignore]
fn daemon_exits_when_the_browser_quits() {
    let Some(app) = app_under_test() else { return };
    let mut a = App::new(app);
    a.open(&[]);
    let daemon = a.wait_for_daemon(None, DAEMON_START);
    let s = a.create_session("/quit");
    assert_eq!(manifests_of(a.home.path(), &s).len(), 1);
    let browser = a.browser();
    // SAFETY: plain syscall on this test's browser.
    unsafe { libc::kill(browser, libc::SIGINT) };
    assert!(
        eventually(SETTLE, || !alive(daemon)),
        "the daemon is still there; daemon.log:\n{}",
        a.log()
    );
    assert!(
        eventually(SETTLE, || !alive(browser)),
        "the browser did not quit"
    );
    let manifests = manifests_of(a.home.path(), &s);
    assert!(
        manifests.iter().all(|m| m["endedAt"].is_string()),
        "{manifests:?}"
    );
}

/// A daemon killed while its session has a tab: the browser starts a new one, which closes
/// the agent tab left behind and leaves the person's tab alone; the browser lives on.
#[test]
#[ignore]
fn a_killed_daemon_is_restarted_and_cleans_up() {
    let Some(app) = app_under_test() else { return };
    let mut a = App::new(app);
    // The person's tab, opened by the launch itself.
    let human = a.server.url("/ping/human");
    a.open(&[&human]);
    let first = a.wait_for_daemon(None, DAEMON_START);
    let s = a.create_session("/ping/agent");
    let pinging = |a: &App, path: &str| {
        a.server
            .since(path)
            .is_some_and(|t| t < Duration::from_secs(2))
    };
    assert!(eventually(SETTLE, || pinging(&a, "/ping/human")
        && pinging(&a, "/ping/agent")));

    let (code, _, err) = a.cli(&["get", "url"], Some(&s));
    assert_eq!(code, 0, "{err}");
    let left = agent_browser_of(&s).expect("the session's agent-browser pid");
    assert!(alive(left));
    eprintln!(
        "OBSERVED daemon pid={first} pgid={} sess={} ppid={}; agent-browser pid={left} pgid={} sess={} ppid={}",
        ps("pgid", first),
        ps("sess", first),
        ps("ppid", first),
        ps("pgid", left),
        ps("sess", left),
        ps("ppid", left)
    );

    // SAFETY: plain syscall on this test's daemon.
    unsafe { libc::kill(first, libc::SIGKILL) };
    let second = a.wait_for_daemon(Some(first), SETTLE);
    let restarted = Instant::now();
    assert_ne!(second, first);
    // The new daemon stops the agent-browser the killed one left.
    assert!(
        eventually(SETTLE, || !alive(left)),
        "agent-browser {left} of the killed daemon is still there"
    );
    assert!(
        eventually(SETTLE, || !pinging(&a, "/ping/agent")),
        "the agent tab is still open"
    );
    assert!(pinging(&a, "/ping/human"), "the person's tab went too");
    assert!(alive(a.browser()));
    // F11: the old session is over, its recording closed.
    let manifests = manifests_of(a.home.path(), &s);
    assert!(
        eventually(SETTLE, || manifests_of(a.home.path(), &s)
            .iter()
            .all(|m| m["endedAt"].is_string())),
        "{manifests:?}"
    );
    let (code, out, _) = a.cli(&["session", "list", "--json"], None);
    assert_eq!(code, 0);
    assert_eq!(json(&out).as_array().map(Vec::len), Some(0), "{out}");
    // The new daemon works.
    let s2 = a.create_session("/after");
    let (code, _, err) = a.cli(&["get", "url"], Some(&s2));
    assert_eq!(code, 0, "{err}");
    // And it stays: nothing left over from the first daemon's end (its SIGTERM timer of
    // 5 s) reaches it.
    std::thread::sleep(Duration::from_secs(8).saturating_sub(restarted.elapsed()));
    assert!(
        alive(second),
        "the restarted daemon went; daemon.log:\n{}",
        a.log()
    );
    assert_eq!(a.daemon_pid(), Some(second));
}

/// F8: the daemon is started again three times within a minute, not a fourth.
#[test]
#[ignore]
fn restarts_stop_after_three_in_a_minute() {
    let Some(app) = app_under_test() else { return };
    let mut a = App::new(app);
    a.open(&[]);
    let start = Instant::now();
    let mut last = None;
    for kill in 1..=4 {
        let pid = a.wait_for_daemon(last, SETTLE);
        // SAFETY: plain syscall on this test's daemon.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        assert!(eventually(SETTLE, || !alive(pid)), "kill {kill}");
        last = Some(pid);
    }
    assert!(
        start.elapsed() < Duration::from_secs(50),
        "too slow for the window"
    );
    // Well past the restart delay: nothing came back.
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(a.daemons(), Vec::<i32>::new(), "a fifth daemon started");
    let (code, _, _) = a.cli(&["browser", "status", "--json"], None);
    assert_ne!(code, 0, "something answers on the socket");
    assert!(alive(a.browser()), "the browser died with its daemon");
}

/// Another daemon (here a stock-configuration daemon of the same home, which launched a fork
/// of its own) holds the lock: the app's daemon exits 4 once, is not started again, and the
/// running daemon goes on answering.
#[test]
#[ignore]
fn a_held_lock_is_not_retried() {
    let Some(app) = app_under_test() else { return };
    let a = App::new(app.clone());
    std::fs::write(
        a.home.path().join("config.toml"),
        "[browser]\nkind = \"stock\"\n",
    )
    .unwrap();
    let mut held = Command::new(a.helper());
    held.env("PEGBOARD_HOME", a.home.path())
        .env("PEGBOARD_TEST_BROWSER", format!("fork:{}", app.display()))
        // Started from the command line, not by the app: it is told where agent-browser is.
        .env(
            "PEGBOARD_AGENT_BROWSER",
            app.join("Contents/Helpers/agent-browser"),
        )
        .args(["browser", "start"])
        .stdin(Stdio::null());
    for (k, v) in a.test_env() {
        held.env(k, v);
    }
    let out = held.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let holder = a.daemon_pid().expect("the holding daemon's pid");

    a.open(&[]);
    let refused = |a: &App| a.log().matches("another daemon holds").count();
    assert!(
        eventually(SETTLE, || refused(&a) >= 1),
        "the app's daemon never ran; daemon.log:\n{}",
        a.log()
    );
    // Several restart delays later: still the one refused start.
    std::thread::sleep(Duration::from_secs(6));
    assert_eq!(refused(&a), 1, "daemon.log:\n{}", a.log());
    let (code, out, _) = a.cli(&["browser", "status", "--json"], None);
    assert_eq!(code, 0);
    assert_eq!(json(&out)["running"], true);
    assert_eq!(a.daemon_pid(), Some(holder));
    let browser_up = Command::new("pgrep")
        .args(["-f", "--"])
        .arg(format!("--user-data-dir={}", a.udd.path().display()))
        .output()
        .unwrap();
    assert!(browser_up.status.success(), "the app is gone");

    let (code, _, err) = a.cli(&["browser", "stop"], None);
    assert_eq!(code, 0, "{err}");
}

/// How long after `since` the daemon `pid` was seen gone (polled every 50 ms, up to 15 s).
fn gone_after(pid: i32, since: Instant) -> Duration {
    let deadline = since + Duration::from_secs(15);
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(pid), "{pid} is still there 15 s later");
    since.elapsed()
}

/// The browser quits while its daemon does not leave with the pipe (test hook): the browser
/// waits 5 s, then sends SIGTERM.
#[test]
#[ignore]
fn quit_sigterms_a_daemon_that_stays() {
    let Some(app) = app_under_test() else { return };
    let mut a = App::new(app).with_env(&[("PEGBOARD_TEST_IGNORE_PIPE_EOF", "1")]);
    a.open(&[]);
    let daemon = a.wait_for_daemon(None, DAEMON_START);
    let browser = a.browser();
    let quit = Instant::now();
    // SAFETY: plain syscall on this test's browser.
    unsafe { libc::kill(browser, libc::SIGINT) };
    let after = gone_after(daemon, quit);
    let log = a.log();
    assert!(
        after >= Duration::from_millis(4500) && after <= Duration::from_secs(8),
        "the daemon went {after:?} after the quit; daemon.log:\n{log}"
    );
    assert!(log.contains("staying (test hook)"), "{log}");
    assert!(log.contains("SIGTERM; stopping"), "{log}");
    assert!(
        eventually(SETTLE, || !alive(browser)),
        "the browser did not quit"
    );
}

/// A daemon that closes its end of the pipe and stays (test hooks) gets SIGTERM 5 s later
/// and is replaced.
#[test]
#[ignore]
fn a_daemon_that_hangs_up_is_replaced() {
    let Some(app) = app_under_test() else { return };
    let mut a = App::new(app).with_env(&[
        ("PEGBOARD_TEST_IGNORE_PIPE_EOF", "1"),
        ("PEGBOARD_TEST_PIPE_CLOSE_ON_USR1", "1"),
    ]);
    a.open(&[]);
    let first = a.wait_for_daemon(None, DAEMON_START);
    let hung_up = Instant::now();
    // SAFETY: plain syscall on this test's daemon.
    unsafe { libc::kill(first, libc::SIGUSR1) };
    let after = gone_after(first, hung_up);
    assert!(
        after >= Duration::from_millis(4500) && after <= Duration::from_secs(8),
        "the daemon went {after:?} after hanging up; daemon.log:\n{}",
        a.log()
    );
    assert!(a.log().contains("SIGTERM; stopping"), "{}", a.log());
    let second = a.wait_for_daemon(Some(first), SETTLE);
    assert_ne!(second, first);
    assert!(alive(a.browser()));
}

/// B16: the browser gives up (the fourth end within a minute) and shows the person one
/// notice, over the person's window, without taking the keyboard or activating the app.
#[test]
#[ignore]
fn giving_up_shows_a_notice_without_taking_focus() {
    let Some(app) = app_under_test() else { return };
    let mut a = App::new(app).with_notice_hooks("*=none");
    let person = a.server.url("/person");
    a.open(&[&person]);
    a.kill_four_times(None);
    assert!(
        eventually(SETTLE, || !a.notices("daemon_stopped").is_empty()),
        "no notice; dialog log: {:?}",
        std::fs::read_to_string(a.notice_log())
    );
    let e = a.notices("daemon_stopped").remove(0);
    eprintln!("notice: {e}");
    assert_eq!(e["profile"], "Default", "{e}");
    let before = &e["keyWindowProfileBefore"];
    assert_ne!(before, "other", "{e}");
    assert_eq!(&e["keyWindowProfileAt0"], before, "{e}");
    assert_eq!(&e["keyWindowProfileAt200ms"], before, "{e}");
    assert_eq!(e["appActiveAt0"], e["appActiveBefore"], "{e}");
    assert_eq!(e["appActiveAt200ms"], e["appActiveBefore"], "{e}");
    // One notice, no daemon, the browser lives on.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(a.notices("daemon_stopped").len(), 1);
    assert_eq!(a.daemons(), Vec::<i32>::new());
    assert!(alive(a.browser()));
}

/// 다시 시작 on the notice starts a daemon that answers, and the restarts count from there:
/// four more ends within a minute bring the notice again.
#[test]
#[ignore]
fn restart_from_the_notice_starts_a_daemon() {
    let Some(app) = app_under_test() else { return };
    let mut a = App::new(app).with_notice_hooks("*=restart");
    a.open(&[]);
    let last = a.kill_four_times(None);
    assert!(
        eventually(SETTLE, || a
            .notices("daemon_notice_closed")
            .iter()
            .any(|e| e["pressed"] == "restart")),
        "the notice was not answered; dialog log: {:?}",
        std::fs::read_to_string(a.notice_log())
    );
    let restarted = a.wait_for_daemon(Some(last), SETTLE);
    let s = a.create_session("/after-restart");
    let (code, _, err) = a.cli(&["get", "url"], Some(&s));
    assert_eq!(code, 0, "{err}");
    // Three restarts again, then the notice again (starting with the daemon of the restart).
    assert_eq!(a.wait_for_daemon(Some(last), SETTLE), restarted);
    a.kill_four_times(Some(last));
    assert!(
        eventually(SETTLE, || a.notices("daemon_stopped").len() == 2),
        "no second notice; dialog log: {:?}",
        std::fs::read_to_string(a.notice_log())
    );
}

/// Another daemon holds the lock (exit code 4): the browser does not start one again, and
/// shows no notice either (there is a daemon, just not the browser's).
#[test]
#[ignore]
fn a_held_lock_shows_no_notice() {
    let Some(app) = app_under_test() else { return };
    let a = App::new(app.clone()).with_notice_hooks("*=none");
    std::fs::write(
        a.home.path().join("config.toml"),
        "[browser]\nkind = \"stock\"\n",
    )
    .unwrap();
    let mut held = Command::new(a.helper());
    held.env("PEGBOARD_HOME", a.home.path())
        .env("PEGBOARD_TEST_BROWSER", format!("fork:{}", app.display()))
        .env(
            "PEGBOARD_AGENT_BROWSER",
            app.join("Contents/Helpers/agent-browser"),
        )
        .args(["browser", "start"])
        .stdin(Stdio::null());
    for (k, v) in a.test_env() {
        held.env(k, v);
    }
    let out = held.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    a.open(&[]);
    assert!(
        eventually(SETTLE, || a.log().contains("another daemon holds")),
        "the app's daemon never ran; daemon.log:\n{}",
        a.log()
    );
    // Several restart delays later: no notice.
    std::thread::sleep(Duration::from_secs(6));
    assert_eq!(a.notices("daemon_stopped"), Vec::<Value>::new());
    let (code, _, err) = a.cli(&["browser", "stop"], None);
    assert_eq!(code, 0, "{err}");
}
