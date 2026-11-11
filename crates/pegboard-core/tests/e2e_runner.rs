mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pegboard_core::browser::Browser;
use pegboard_core::cdp::page_targets;
use pegboard_core::profiles::Profiles;
use pegboard_core::proxy::server::{Endpoint, ProxyServer, WsUpstream};
use pegboard_core::runner::Runner;
use pegboard_core::session::SessionManager;
use pegboard_protocol::{codes, CommandExecResult, SessionCreateParams, SessionId, SessionInfo};
use serde_json::{json, Value};

const BIN: &str = "vendor/agent-browser/agent-browser";

/// A launched browser, a proxy, a session manager and a runner. Dropping it (also on test
/// failure) kills every agent-browser daemon it started and the browser.
/// Fields drop in declaration order: the browser goes before the temp dir.
struct Harness {
    daemons: Daemons,
    runner: Arc<Runner>,
    manager: SessionManager,
    proxy: Arc<ProxyServer>,
    browser: Arc<Browser>,
    fixture: common::Fixture,
    env: common::Env,
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
    async fn start() -> Harness {
        let (env, fixture, browser) = common::launched().await;
        let upstream = Arc::new(WsUpstream {
            browser_ws_url: browser.ws_url.clone(),
        });
        let proxy = Arc::new(ProxyServer::start(upstream).await.unwrap());
        let profiles = Profiles::load(&env.paths).unwrap();
        let browser = Arc::new(browser);
        let manager = SessionManager::new(
            browser.clone(),
            proxy.clone(),
            profiles,
            env.paths.clone(),
            env.cfg.clone(),
        );
        let runner = Arc::new(Runner::new(env.paths.clone()).unwrap());
        Harness {
            daemons: Daemons {
                runner: runner.clone(),
                list: Mutex::new(Vec::new()),
            },
            runner,
            manager,
            proxy,
            browser,
            fixture,
            env,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.fixture.origin)
    }

    /// A session on `path`, without agent-browser.
    async fn create(&self, path: &str) -> (SessionInfo, Endpoint) {
        let info = self
            .manager
            .create(SessionCreateParams {
                name: None,
                url: Some(self.url(path)),
                profile: None,
            })
            .await
            .unwrap();
        let ep = self.manager.endpoint(&info.id).unwrap();
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

    /// Detaches every session, checks that no daemon of this test and no browser process
    /// of this test is left, and shuts the browser down.
    async fn finish(self) {
        let daemons = self.daemons.get();
        for (id, _) in &daemons {
            self.runner.detach(id).await.unwrap();
        }
        let udd = self.env.paths.chrome_udd.display().to_string();
        let Harness {
            daemons: guard,
            manager,
            proxy,
            browser,
            ..
        } = self;
        drop(manager);
        drop(proxy);
        let mut browser = browser;
        let mut shut = false;
        for _ in 0..50 {
            match Arc::try_unwrap(browser) {
                Ok(b) => {
                    b.shutdown().await.unwrap();
                    shut = true;
                    break;
                }
                Err(b) => browser = b,
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(shut, "the browser is still referenced");
        let gone = eventually(5, || async {
            daemons.iter().all(|(_, pid)| !alive(*pid)) && pgrep(&udd).is_empty()
        })
        .await;
        drop(guard);
        assert!(
            gone,
            "left behind: daemons {daemons:?}, chrome {:?}",
            pgrep(&udd)
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

fn pgrep(pattern: &str) -> Vec<String> {
    let out = std::process::Command::new("pgrep")
        .args(["-f", pattern])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(String::from)
        .collect()
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

/// TCP ports the process listens on.
fn listening_ports(pid: i32) -> Vec<String> {
    let out = std::process::Command::new("lsof")
        .args([
            "-nP",
            "-a",
            "-p",
            &pid.to_string(),
            "-iTCP",
            "-sTCP:LISTEN",
            "-Fn",
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix('n').map(String::from))
        .collect()
}

#[tokio::test]
#[ignore]
async fn drives_the_session_tab() {
    let h = Harness::start().await;
    let (info, ep) = h.session("/counter").await;
    let snap = h.exec(&info, &ep, &["snapshot", "-i"]).await;
    let text = snap.output["data"]["snapshot"].as_str().unwrap_or_default();
    assert!(text.contains("button"), "{snap:?}");
    let click = h.exec(&info, &ep, &["click", "#btn"]).await;
    assert!(ok(&click), "{click:?}");
    let clicks = h
        .eval(
            &info.targets[0],
            "document.getElementById('clicks').textContent",
        )
        .await;
    assert_eq!(clicks, json!("1"));
    h.finish().await;
}

#[tokio::test]
#[ignore]
async fn stream_server_is_off_after_attach() {
    let h = Harness::start().await;
    let (info, ep) = h.create("/counter").await;
    let started = Instant::now();
    h.attach(&info.id, &ep).await;
    let attach_ms = started.elapsed().as_millis();
    let status = h.exec(&info, &ep, &["stream", "status"]).await;
    assert_eq!(status.output["data"]["enabled"], json!(false), "{status:?}");
    assert_eq!(
        status.output["data"]["connected"],
        json!(true),
        "{status:?}"
    );
    let pid = h.runner.daemon_pid(&info.id).unwrap();
    let ports = listening_ports(pid);
    assert!(ports.is_empty(), "the daemon listens on {ports:?}");
    // Still off after commands that use the page.
    h.exec(&info, &ep, &["snapshot", "-i"]).await;
    assert!(listening_ports(pid).is_empty());
    println!("attach took {attach_ms} ms");
    h.finish().await;
}

/// The stream server is open from the daemon's start until the first `stream disable`;
/// attach runs that before the daemon connects to the session. Measures how long the
/// port listens and checks that no browser is connected during that time.
#[tokio::test]
#[ignore]
async fn stream_exposure_window_has_no_browser() {
    let h = Harness::start().await;
    let (info, ep) = h.create("/counter").await;
    let runner = &h.runner;
    let id = info.id.clone();
    let attach = async {
        let t0 = Instant::now();
        let r = runner.attach(&id, &ep).await;
        (r, t0.elapsed())
    };
    // Watch the daemon's listening ports while attach runs, and whether the daemon has a
    // connection to the session endpoint meanwhile.
    let proxy = &h.proxy;
    let watch = async {
        let t0 = Instant::now();
        let mut first_seen = None;
        let mut last_seen = None;
        let mut connected_while_listening = 0;
        while t0.elapsed() < Duration::from_secs(10) {
            if let Some(pid) = runner.daemon_pid(&id) {
                let connections = proxy.open_connections();
                if !listening_ports(pid).is_empty() {
                    first_seen.get_or_insert(t0.elapsed());
                    last_seen = Some(t0.elapsed());
                    connected_while_listening += connections;
                } else if first_seen.is_some() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        (first_seen, last_seen, connected_while_listening)
    };
    let ((attached, took), (first, last, connected)) = tokio::join!(attach, watch);
    h.daemons.add(&id);
    attached.unwrap();
    println!("attach took {took:?}; stream port seen listening from {first:?} to {last:?}");
    assert_eq!(
        connected, 0,
        "the daemon was connected to the session while its stream port listened"
    );
    let status = h.exec(&info, &ep, &["stream", "status"]).await;
    assert_eq!(status.output["data"]["enabled"], json!(false));
    h.finish().await;
}

#[tokio::test]
#[ignore]
async fn hidden_session_tab_click_is_fast() {
    let h = Harness::start().await;
    let (info, ep) = h.session("/counter").await;
    let t = info.targets[0].clone();
    let ctx = common::context_of(&h.browser.conn, &t).await;
    let other = page_targets(&h.browser.conn)
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.browser_context_id.as_deref() == Some(ctx.as_str()) && p.target_id != t)
        .expect("the agent window has another tab")
        .target_id;
    h.browser
        .conn
        .send("Target.activateTarget", json!({ "targetId": other }), None)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    let click = h.exec(&info, &ep, &["click", "#btn"]).await;
    let elapsed = started.elapsed();
    assert!(ok(&click), "{click:?}");
    assert!(
        elapsed < Duration::from_millis(1500),
        "click took {elapsed:?}"
    );
    h.finish().await;
}

#[tokio::test]
#[ignore]
async fn tab_list_shows_only_session_tabs() {
    let h = Harness::start().await;
    // The browser's first window is the human profile and has a tab.
    let human: Vec<String> = page_targets(&h.browser.conn)
        .await
        .unwrap()
        .into_iter()
        .filter(|t| t.browser_context_id.as_deref() == Some(h.browser.human_context.as_str()))
        .map(|t| t.target_id)
        .collect();
    assert!(!human.is_empty());
    let (info, ep) = h.session("/counter").await;
    let list = h.exec(&info, &ep, &["tab", "list"]).await;
    let tabs = list.output["data"]["tabs"].as_array().unwrap().clone();
    assert!(!tabs.is_empty(), "{list:?}");
    let session_targets = h.manager.info(&info.id).unwrap().targets;
    for tab in &tabs {
        let target = tab["targetId"].as_str().unwrap();
        assert!(session_targets.iter().any(|t| t == target), "{tab}");
        assert!(
            tab["url"].as_str().unwrap().starts_with(&h.fixture.origin),
            "{tab}"
        );
    }
    h.finish().await;
}

#[tokio::test]
#[ignore]
async fn command_after_tab_was_closed_by_hand_reports_tab_gone() {
    let h = Harness::start().await;
    let (info, ep) = h.session("/counter").await;
    h.browser
        .conn
        .send(
            "Target.closeTarget",
            json!({ "targetId": info.targets[0] }),
            None,
        )
        .await
        .unwrap();
    let emptied = eventually(3, || async {
        h.manager.info(&info.id).unwrap().targets.is_empty()
    })
    .await;
    assert!(emptied);
    let e = h
        .runner
        .exec(&info.id, &ep, &s(&["snapshot", "-i"]), 30_000)
        .await
        .unwrap_err();
    assert_eq!(e.code, codes::TAB_GONE, "{e:?}");
    h.finish().await;
}

/// 3a: agent-browser works on the tab the session was created with and opens no other.
#[tokio::test]
#[ignore]
async fn attach_adopts_the_first_tab() {
    let h = Harness::start().await;
    let (info, ep) = h.session("/counter").await;
    let list = h.exec(&info, &ep, &["tab", "list"]).await;
    let tabs = list.output["data"]["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 1, "{list:?}");
    assert_eq!(tabs[0]["targetId"], json!(info.targets[0]));
    assert_eq!(h.manager.info(&info.id).unwrap().targets, info.targets);
    let url = h.exec(&info, &ep, &["get", "url"]).await;
    assert_eq!(url.output["data"]["url"], json!(h.url("/counter")));
    h.finish().await;
}

/// 3b, 3c: a tab agent-browser opens itself (not paused for the debugger) works, and
/// switching between tabs works without `Target.activateTarget`.
#[tokio::test]
#[ignore]
async fn new_tab_and_tab_switch_work_through_the_proxy() {
    let h = Harness::start().await;
    let (info, ep) = h.session("/counter").await;
    let created = h.exec(&info, &ep, &["tab", "new", &h.url("/form")]).await;
    assert!(ok(&created), "{created:?}");
    let targets = h.manager.info(&info.id).unwrap().targets;
    assert_eq!(targets.len(), 2, "{targets:?}");
    let url = h.exec(&info, &ep, &["get", "url"]).await;
    assert_eq!(url.output["data"]["url"], json!(h.url("/form")), "{url:?}");
    let fill = h.exec(&info, &ep, &["fill", "#name", "typed"]).await;
    assert!(ok(&fill), "{fill:?}");
    let first = h.exec(&info, &ep, &["tab", "t1"]).await;
    assert!(ok(&first), "{first:?}");
    let url = h.exec(&info, &ep, &["get", "url"]).await;
    assert_eq!(
        url.output["data"]["url"],
        json!(h.url("/counter")),
        "{url:?}"
    );
    let click = h.exec(&info, &ep, &["click", "#btn"]).await;
    assert!(ok(&click), "{click:?}");
    assert_eq!(
        h.eval(
            &info.targets[0],
            "document.getElementById('clicks').textContent"
        )
        .await,
        json!("1")
    );
    let refused = h.proxy.refused_methods();
    assert!(
        !refused.contains_key("Target.activateTarget"),
        "{refused:?}"
    );
    h.finish().await;
}

/// 3d: while agent-browser is attached (its connection auto-attaches to new targets), a
/// tab and a popup in the human profile still load.
#[tokio::test]
#[ignore]
async fn human_tab_loads_while_agent_browser_is_attached() {
    let h = Harness::start().await;
    let (info, ep) = h.session("/counter").await;
    h.exec(&info, &ep, &["snapshot", "-i"]).await;
    let conn = &h.browser.conn;
    let human = page_targets(conn)
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.browser_context_id.as_deref() == Some(h.browser.human_context.as_str()))
        .unwrap()
        .target_id;
    let s = conn.attach(&human).await.unwrap();
    conn.navigate(&s, &h.url("/opener")).await.unwrap();
    // A new human tab (no opener), opened while agent-browser commands run. CDP cannot
    // create a target in the default profile by context id, so the human page opens it.
    let busy = async {
        for _ in 0..3 {
            h.exec(&info, &ep, &["get", "title"]).await;
        }
    };
    let open = conn.send(
        "Runtime.evaluate",
        json!({ "expression": "window.open('/form', '_blank', 'noopener'); 1", "userGesture": true }),
        Some(&s),
    );
    let ((), opened) = tokio::join!(busy, open);
    opened.unwrap();
    let human_ctx = h.browser.human_context.clone();
    let loaded = eventually(5, || async {
        for t in page_targets(conn).await.unwrap() {
            if t.browser_context_id.as_deref() == Some(human_ctx.as_str())
                && t.target_id != human
                && h.eval(&t.target_id, "document.readyState").await == json!("complete")
                && h.eval(&t.target_id, "location.pathname").await == json!("/form")
            {
                return true;
            }
        }
        false
    })
    .await;
    assert!(loaded, "the new human tab did not load");
    // A popup of the human tab.
    conn.send(
        "Runtime.evaluate",
        json!({ "expression": "document.getElementById('open').click()", "userGesture": true }),
        Some(&s),
    )
    .await
    .unwrap();
    let popup = eventually(5, || async {
        for t in page_targets(conn).await.unwrap() {
            if t.opener_id.as_deref() == Some(human.as_str())
                && h.eval(&t.target_id, "document.readyState").await == json!("complete")
                && h.eval(&t.target_id, "location.pathname").await == json!("/counter")
            {
                return true;
            }
        }
        false
    })
    .await;
    assert!(popup, "the human popup did not load");
    // agent-browser still works and never saw the human tabs.
    let list = h.exec(&info, &ep, &["tab", "list"]).await;
    assert_eq!(
        list.output["data"]["tabs"].as_array().unwrap().len(),
        1,
        "{list:?}"
    );
    h.finish().await;
}

/// 3e: URLs outside http(s), data: and about:blank fail clearly and leave the tab alone.
#[tokio::test]
#[ignore]
async fn disallowed_urls_fail_and_leave_the_tab_unchanged() {
    let h = Harness::start().await;
    let (info, ep) = h.session("/counter").await;
    for url in ["file:///etc/hosts", "chrome://settings"] {
        let r = h.exec(&info, &ep, &["open", url]).await;
        assert!(!ok(&r), "{url}: {r:?}");
        let error = r.output["error"].as_str().unwrap_or_default().to_string();
        println!("open {url}: {error}");
        assert!(!error.is_empty(), "{url}: {r:?}");
        assert_eq!(
            h.eval(&info.targets[0], "location.href").await,
            json!(h.url("/counter")),
            "{url}"
        );
    }
    let url = h.exec(&info, &ep, &["get", "url"]).await;
    assert_eq!(url.output["data"]["url"], json!(h.url("/counter")));
    h.finish().await;
}

/// The session token reaches agent-browser through its environment: no process shows it
/// in its arguments, neither the CLI nor the daemon.
#[tokio::test]
#[ignore]
async fn token_is_not_in_process_arguments() {
    let h = Harness::start().await;
    let (info, ep) = h.session("/counter").await;
    let wait = h.exec(&info, &ep, &["wait", "2000"]);
    let look = async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let out = std::process::Command::new("ps")
            .args(["-axww", "-o", "pid=,command="])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let (_, ps) = tokio::join!(wait, look);
    let cli: Vec<&str> = ps
        .lines()
        .filter(|l| l.contains("wait 2000") && l.contains(BIN))
        .collect();
    assert_eq!(cli.len(), 1, "the CLI was not seen running: {cli:?}");
    assert!(cli[0].contains(&ep.url), "{}", cli[0]);
    assert!(
        !ps.contains(&ep.token),
        "the token is in some process's arguments"
    );
    h.finish().await;
}

/// Closing the session while a command runs stops the command and the daemon; later
/// commands are refused.
#[tokio::test]
#[ignore]
async fn detach_stops_the_daemon_while_a_command_runs() {
    let h = Harness::start().await;
    let (info, ep) = h.session("/counter").await;
    let pid = h.runner.daemon_pid(&info.id).unwrap();
    let argv = s(&["wait", "20000"]);
    let running = h.runner.exec(&info.id, &ep, &argv, 30_000);
    let close = async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let t0 = Instant::now();
        let r = h.runner.detach(&info.id).await;
        (r, t0.elapsed())
    };
    let (ran, (detached, took)) = tokio::join!(running, close);
    assert_eq!(ran.unwrap_err().code, codes::SESSION_NOT_FOUND);
    detached.unwrap();
    println!("detach took {took:?}");
    assert!(took < Duration::from_secs(10), "{took:?}");
    assert!(
        eventually(3, || async { !alive(pid) }).await,
        "daemon {pid} still runs"
    );
    let later = h
        .runner
        .exec(&info.id, &ep, &s(&["get", "url"]), 30_000)
        .await
        .unwrap_err();
    assert_eq!(later.code, codes::SESSION_NOT_FOUND);
    assert_eq!(h.runner.daemon_pid(&info.id), None);
    h.finish().await;
}

/// A command over its time limit is stopped and reported; the caller does not hang.
#[tokio::test]
#[ignore]
async fn command_over_its_time_limit_is_stopped() {
    let h = Harness::start().await;
    let (info, ep) = h.session("/counter").await;
    let t0 = Instant::now();
    let e = h
        .runner
        .exec(&info.id, &ep, &s(&["wait", "10000"]), 1000)
        .await
        .unwrap_err();
    let took = t0.elapsed();
    assert_eq!(e.code, codes::TIMEOUT, "{e:?}");
    assert!(took < Duration::from_secs(3), "{took:?}");
    let cli = pgrep(&format!("{BIN} --session peg-{} .*wait 10000", info.id.0));
    assert!(cli.is_empty(), "the CLI still runs: {cli:?}");
    // The daemon may still be busy with the wait; the next command eventually runs.
    let t1 = Instant::now();
    let next = h.exec(&info, &ep, &["get", "url"]).await;
    println!("next command after a timeout took {:?}", t1.elapsed());
    assert!(ok(&next), "{next:?}");
    h.finish().await;
}

/// `cookies set` (page-level `Network.setCookies`) writes to the session's profile only.
#[tokio::test]
#[ignore]
async fn cookies_set_stays_in_the_agent_profile() {
    let h = Harness::start().await;
    let (info, ep) = h.session("/counter").await;
    let set = h
        .exec(&info, &ep, &["cookies", "set", "pegtest", "1"])
        .await;
    assert!(ok(&set), "{set:?}");
    assert_eq!(
        h.eval(&info.targets[0], "document.cookie").await,
        json!("pegtest=1")
    );
    let conn = &h.browser.conn;
    let human = page_targets(conn)
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.browser_context_id.as_deref() == Some(h.browser.human_context.as_str()))
        .unwrap()
        .target_id;
    let s = conn.attach(&human).await.unwrap();
    conn.navigate(&s, &h.url("/counter")).await.unwrap();
    assert_eq!(
        conn.evaluate(&s, "document.cookie").await.unwrap(),
        json!("")
    );
    h.finish().await;
}

/// Not a gate: runs the commands an agent typically uses and prints, per command, whether
/// it worked and which CDP methods the proxy refused meanwhile (the Task 7 compatibility
/// table). Run with --nocapture.
#[tokio::test]
#[ignore]
async fn compatibility_matrix() {
    let h = Harness::start().await;
    let (info, ep) = h.session("/form").await;
    let out = tempfile::tempdir().unwrap();
    let file = |name: &str| out.path().join(name).display().to_string();
    std::fs::write(file("upload.txt"), "upload me").unwrap();
    let data = |html: &str| format!("data:text/html,{}", html.replace('#', "%23"));
    let upload_page = data(r#"<input id="f" type="file">"#);
    let download_page = data(r#"<a id="d" href="data:text/plain,hello" download="x.txt">get</a>"#);
    let dialog_page =
        data(r#"<button id="c" onclick="document.title=String(confirm('ok?'))">c</button>"#);
    let frame_page = h.url("/frame");
    let mut rows = Vec::new();
    let mut refused_before = h.proxy.refused_methods();
    let run = |args: Vec<String>| {
        let h = &h;
        let info = &info;
        let ep = &ep;
        async move {
            let t0 = Instant::now();
            let r = h.runner.exec(&info.id, ep, &args, 20_000).await;
            (args, r, t0.elapsed())
        }
    };
    let steps: Vec<Vec<String>> = vec![
        s(&["open", &h.url("/form")]),
        s(&["snapshot", "-i"]),
        s(&["click", "#name"]),
        s(&["click", "@e1"]),
        s(&["fill", "#name", "hello"]),
        s(&["type", "#name", " world"]),
        s(&["press", "Enter"]),
        s(&["hover", "#name"]),
        s(&["get", "url"]),
        s(&["get", "title"]),
        s(&["get", "text", "label"]),
        s(&["eval", "document.getElementById('name').value"]),
        s(&["screenshot", &file("shot.png")]),
        s(&["scroll", "down"]),
        s(&["wait", "200"]),
        s(&["open", &h.url("/counter")]),
        s(&["back"]),
        s(&["forward"]),
        s(&["reload"]),
        s(&["tab", "list"]),
        s(&["tab", "new", &h.url("/cookie/get")]),
        s(&["tab", "t1"]),
        s(&["tab", "1"]),
        s(&["cookies"]),
        s(&["cookies", "set", "pegtest", "1"]),
        s(&["cookies"]),
        s(&["storage", "local"]),
        s(&["network", "requests"]),
        s(&["pdf", &file("page.pdf")]),
        s(&["open", &upload_page]),
        s(&["upload", "#f", &file("upload.txt")]),
        s(&["eval", "document.getElementById('f').files.length"]),
        s(&["open", &download_page]),
        s(&["download", "#d", &file("x.txt")]),
        s(&["open", &dialog_page]),
        s(&["click", "#c"]),
        s(&["dialog", "status"]),
        s(&["dialog", "accept"]),
        s(&["get", "title"]),
        s(&["open", &frame_page]),
        s(&["frame", "#fr"]),
        s(&["fill", "#name", "in frame"]),
        s(&["frame", "main"]),
        // Beyond the required list.
        s(&["open", &h.url("/counter")]),
        s(&["dblclick", "#btn"]),
        s(&["focus", "#btn"]),
        s(&["keyboard", "type", "abc"]),
        s(&["get", "html", "h1"]),
        s(&["is", "visible", "#btn"]),
        s(&["find", "role", "button", "click", "--name", "Add"]),
        s(&["scrollintoview", "#btn"]),
        s(&["mouse", "move", "10", "10"]),
        s(&["set", "viewport", "800", "600"]),
        s(&["screenshot", "--full", &file("full.png")]),
        s(&["console"]),
        s(&["errors"]),
        s(&["diff", "snapshot"]),
        s(&["tab", "close"]),
    ];
    for args in steps {
        let (args, r, took) = run(args).await;
        let refused_now = h.proxy.refused_methods();
        let refused: Vec<String> = refused_now
            .iter()
            .filter(|(m, n)| refused_before.get(*m) != Some(n))
            .map(|(m, n)| format!("{m} x{}", n - refused_before.get(m).copied().unwrap_or(0)))
            .collect();
        refused_before = refused_now;
        let (status, detail) = match &r {
            Ok(r) if ok(r) => ("pass", String::new()),
            Ok(r) => ("fail", r.output["error"].to_string()),
            Err(e) => ("error", format!("{}: {}", e.code, e.message)),
        };
        let mut shown = args.join(" ").replace(&h.fixture.origin, "");
        shown = shown.replace(out.path().to_str().unwrap(), "<tmp>");
        if shown.len() > 60 {
            shown = format!("{}...", &shown[..57]);
        }
        let detail: String = detail.chars().take(160).collect();
        rows.push(format!(
            "| `{shown}` | {status} | {} ms | {} | {} |",
            took.as_millis(),
            refused.join(", "),
            detail.replace('|', "/")
        ));
    }
    println!(
        "FILES {:?}",
        std::fs::read_dir(out.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>()
    );
    println!("TARGETS {:?}", h.manager.info(&info.id).unwrap().targets);
    for row in rows {
        println!("ROW {row}");
    }
    h.finish().await;
}

// ---------------------------------------------------------------------------
// Binary discovery (no browser)

/// Serializes the tests that change PEGBOARD_AGENT_BROWSER.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn test_paths() -> (tempfile::TempDir, pegboard_core::paths::Paths) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = pegboard_core::paths::paths_in(tmp.path());
    (tmp, paths)
}

#[test]
fn missing_binary_is_a_clear_error() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("PEGBOARD_AGENT_BROWSER", "/nonexistent/agent-browser");
    let (_tmp, paths) = test_paths();
    let e = Runner::new(paths).err().unwrap();
    std::env::remove_var("PEGBOARD_AGENT_BROWSER");
    assert_eq!(e.code, "agent_browser_missing");
    assert!(
        e.message.contains("fetch-agent-browser.sh"),
        "{}",
        e.message
    );
}

#[test]
fn wrong_version_is_a_clear_error() {
    use std::os::unix::fs::PermissionsExt;
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("agent-browser");
    std::fs::write(&script, "#!/bin/sh\necho 'agent-browser 0.1.0'\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("PEGBOARD_AGENT_BROWSER", &script);
    let (_tmp, paths) = test_paths();
    let e = Runner::new(paths).err().unwrap();
    std::env::remove_var("PEGBOARD_AGENT_BROWSER");
    assert_eq!(e.code, "agent_browser_version");
    assert!(e.message.contains("0.1.0"), "{}", e.message);
    assert!(e.message.contains("0.42.0"), "{}", e.message);
}
