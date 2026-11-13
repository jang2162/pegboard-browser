// Shared browser: every browser test here uses the binary's one browser (own session per test,
// closed at the end). The two binary-discovery tests at the end start no browser.
mod common;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use common::{alive, eventually, ok, s, Harness};
use pegboard_core::argv::{check, Checked};
use pegboard_core::cdp::page_targets;
use pegboard_core::runner::Runner;
use pegboard_protocol::{codes, CommandExecResult};
use serde_json::json;

const BIN: &str = "vendor/agent-browser/agent-browser";

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

#[test]
#[ignore]
fn drives_the_session_tab() {
    common::run(async {
        let h = Harness::start();
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
    });
}

#[test]
#[ignore]
fn stream_server_is_off_after_attach() {
    common::run(async {
        let h = Harness::start();
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
    });
}

/// The stream server is open from the daemon's start until the first `stream disable`;
/// attach runs that before the daemon connects to the session. Measures how long the
/// port listens and checks that no browser is connected during that time.
#[test]
#[ignore]
fn stream_exposure_window_has_no_browser() {
    common::run(async {
        let h = Harness::start();
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
        // Connections other than the daemon's (none expected) are not counted against it.
        // Connections of earlier tests' daemons have closed by now; start from none.
        assert!(
            eventually(5, || async { proxy.open_connections() == 0 }).await,
            "{} connections are open before this test attached anything",
            proxy.open_connections()
        );
        let baseline = proxy.open_connections();
        assert_eq!(baseline, 0);
        let watch = async {
            let t0 = Instant::now();
            let mut first_seen = None;
            let mut last_seen = None;
            let mut connected_while_listening = 0;
            while t0.elapsed() < Duration::from_secs(10) {
                if let Some(pid) = runner.daemon_pid(&id) {
                    let connections = proxy.open_connections().saturating_sub(baseline);
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
    });
}

#[test]
#[ignore]
fn hidden_session_tab_click_is_fast() {
    common::run(async {
        let h = Harness::start();
        let (info, ep) = h.session("/counter").await;
        // A second session of the test's own puts another tab in the agent window.
        let _other_session = h.create("/form").await;
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
    });
}

#[test]
#[ignore]
fn tab_list_shows_only_session_tabs() {
    common::run(async {
        let h = Harness::start();
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
    });
}

#[test]
#[ignore]
fn command_after_tab_was_closed_by_hand_reports_tab_gone() {
    common::run(async {
        let h = Harness::start();
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
    });
}

/// 3a: agent-browser works on the tab the session was created with and opens no other.
#[test]
#[ignore]
fn attach_adopts_the_first_tab() {
    common::run(async {
        let h = Harness::start();
        let (info, ep) = h.session("/counter").await;
        let list = h.exec(&info, &ep, &["tab", "list"]).await;
        let tabs = list.output["data"]["tabs"].as_array().unwrap();
        assert_eq!(tabs.len(), 1, "{list:?}");
        assert_eq!(tabs[0]["targetId"], json!(info.targets[0]));
        assert_eq!(h.manager.info(&info.id).unwrap().targets, info.targets);
        let url = h.exec(&info, &ep, &["get", "url"]).await;
        assert_eq!(url.output["data"]["url"], json!(h.url("/counter")));
        h.finish().await;
    });
}

/// 3b, 3c: a tab agent-browser opens itself (not paused for the debugger) works, and
/// switching between tabs works without `Target.activateTarget`.
#[test]
#[ignore]
fn new_tab_and_tab_switch_work_through_the_proxy() {
    common::run(async {
        let h = Harness::start();
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
    });
}

/// 3d: while agent-browser is attached (its connection auto-attaches to new targets), a
/// tab and a popup in the human profile still load.
#[test]
#[ignore]
fn human_tab_loads_while_agent_browser_is_attached() {
    common::run(async {
        let h = Harness::start();
        let (info, ep) = h.session("/counter").await;
        h.exec(&info, &ep, &["snapshot", "-i"]).await;
        let conn = &h.browser.conn;
        let human_pages: Vec<_> = page_targets(conn)
            .await
            .unwrap()
            .into_iter()
            .filter(|t| t.browser_context_id.as_deref() == Some(h.browser.human_context.as_str()))
            .map(|t| t.target_id)
            .collect();
        let human = human_pages[0].clone();
        let human_before = human_pages;
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
        // Put the human profile back as found: close what this test opened there.
        for t in page_targets(conn).await.unwrap() {
            if t.browser_context_id.as_deref() == Some(human_ctx.as_str())
                && t.target_id != human
                && !human_before.contains(&t.target_id)
            {
                let _ = conn
                    .send(
                        "Target.closeTarget",
                        json!({ "targetId": t.target_id }),
                        None,
                    )
                    .await;
            }
        }
        let _ = conn.navigate(&s, "about:blank").await;
        // agent-browser still works and never saw the human tabs.
        let list = h.exec(&info, &ep, &["tab", "list"]).await;
        assert_eq!(
            list.output["data"]["tabs"].as_array().unwrap().len(),
            1,
            "{list:?}"
        );
        h.finish().await;
    });
}

/// 3e: URLs outside http(s), data: and about:blank fail clearly and leave the tab alone.
#[test]
#[ignore]
fn disallowed_urls_fail_and_leave_the_tab_unchanged() {
    common::run(async {
        let h = Harness::start();
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
    });
}

/// The session token reaches agent-browser through its environment: no process shows it
/// in its arguments, neither the CLI nor the daemon.
#[test]
#[ignore]
fn token_is_not_in_process_arguments() {
    common::run(async {
        let h = Harness::start();
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
    });
}

/// Closing the session while a command runs stops the command and the daemon; later
/// commands are refused.
#[test]
#[ignore]
fn detach_stops_the_daemon_while_a_command_runs() {
    common::run(async {
        let h = Harness::start();
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
    });
}

/// A command over its time limit is stopped and reported; the caller does not hang.
#[test]
#[ignore]
fn command_over_its_time_limit_is_stopped() {
    common::run(async {
        let h = Harness::start();
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
    });
}

/// `cookies set` (page-level `Network.setCookies`) writes to the session's profile only.
#[test]
#[ignore]
fn cookies_set_stays_in_the_agent_profile() {
    common::run(async {
        let h = Harness::start();
        let (info, ep) = h.session("/counter").await;
        let set = h
            .exec(&info, &ep, &["cookies", "set", "pegcookieset", "1"])
            .await;
        assert!(ok(&set), "{set:?}");
        // The agent profile is shared with other tests: look for this test's own cookie among
        // whatever else is there.
        let agent_cookies = h.eval(&info.targets[0], "document.cookie").await;
        assert!(
            agent_cookies
                .as_str()
                .unwrap()
                .split("; ")
                .any(|c| c == "pegcookieset=1"),
            "{agent_cookies}"
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
        let human_cookies = conn.evaluate(&s, "document.cookie").await.unwrap();
        // No test of this binary plants a cookie in the human profile (the only fixture page
        // that sets one, `/cookie/set`, is never opened there), so it has none at all.
        assert_eq!(human_cookies, json!(""));
        // Leave nothing behind for other tests.
        h.eval(
            &info.targets[0],
            "document.cookie = 'pegcookieset=; Max-Age=0; Path=/'",
        )
        .await;
        h.finish().await;
    });
}

/// Files a command writes land in the session's private files directory, under the name
/// the agent gave or one pegboard chose; nothing is written anywhere else.
#[test]
#[ignore]
fn screenshot_lands_in_the_session_files_dir() {
    common::run(async {
        use std::os::unix::fs::PermissionsExt;
        let h = Harness::start();
        let (info, ep) = h.session("/counter").await;
        let files = h.env.paths.session_files(&info.id);
        let checked = |argv: &[&str]| match check(&s(argv), &files).unwrap() {
            Checked::Run(argv) => argv,
            other => panic!("{other:?}"),
        };
        let (runner, id, ep) = (&h.runner, &info.id, &ep);
        let run =
            |argv: Vec<String>| async move { runner.exec(id, ep, &argv, 30_000).await.unwrap() };
        let named = run(checked(&["screenshot", "named.png"])).await;
        assert!(ok(&named), "{named:?}");
        assert!(files.join("named.png").is_file());
        let unnamed_argv = checked(&["screenshot"]);
        let unnamed = run(unnamed_argv.clone()).await;
        assert!(ok(&unnamed), "{unnamed:?}");
        assert!(std::path::Path::new(&unnamed_argv[1]).is_file());
        assert_eq!(
            unnamed.output["data"]["path"],
            json!(unnamed_argv[1]),
            "{unnamed:?}"
        );
        let pdf = run(checked(&["pdf", "page.pdf"])).await;
        assert!(ok(&pdf), "{pdf:?}");
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&files), 0o700);
        assert_eq!(mode(files.parent().unwrap()), 0o700);
        let mut written: Vec<String> = std::fs::read_dir(&files)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        written.sort();
        assert_eq!(written.len(), 3, "{written:?}");
        assert!(
            written.contains(&"named.png".to_string()) && written.contains(&"page.pdf".to_string())
        );
        // Nothing in agent-browser's own HOME or temp dir, nor in the test's working directory.
        let tmp: Vec<_> = std::fs::read_dir(h.env.paths.ab_home.join("tmp"))
            .unwrap()
            .collect();
        assert!(tmp.is_empty(), "{tmp:?}");
        assert!(!std::path::Path::new("named.png").exists());
        h.finish().await;
    });
}

/// A wrong tab id (or another session's target) is an ordinary failure with the CLI's
/// output kept; the session's own tab is fine.
#[test]
#[ignore]
fn wrong_tab_id_is_a_plain_failure_not_tab_gone() {
    common::run(async {
        let h = Harness::start();
        let (info, ep) = h.session("/counter").await;
        let human = page_targets(&h.browser.conn)
            .await
            .unwrap()
            .into_iter()
            .find(|t| t.browser_context_id.as_deref() == Some(h.browser.human_context.as_str()))
            .unwrap()
            .target_id;
        for tab in ["t9", human.as_str()] {
            let r = h
                .runner
                .exec(&info.id, &ep, &s(&["tab", tab]), 30_000)
                .await
                .unwrap_or_else(|e| panic!("tab {tab}: {e:?}"));
            assert_eq!(r.exit_code, 1, "{r:?}");
            assert_eq!(r.output["success"], json!(false), "{r:?}");
            assert!(
                r.output["error"].as_str().is_some_and(|e| !e.is_empty()),
                "{r:?}"
            );
        }
        let url = h.exec(&info, &ep, &["get", "url"]).await;
        assert_eq!(url.output["data"]["url"], json!(h.url("/counter")));
        h.finish().await;
    });
}

/// Not a gate: runs the commands an agent typically uses and prints, per command, whether
/// it worked and which CDP methods the proxy refused meanwhile (the Task 7 compatibility
/// table). Run with --nocapture.
#[test]
#[ignore]
fn compatibility_matrix() {
    common::run(async {
        let h = Harness::start();
        let (info, ep) = h.session("/form").await;
        let files = h.env.paths.session_files(&info.id);
        let data = |html: &str| format!("data:text/html,{}", html.replace('#', "%23"));
        let upload_page = data(r#"<input id="f" type="file">"#);
        let download_page =
            data(r#"<a id="d" href="data:text/plain,hello" download="x.txt">get</a>"#);
        let dialog_page =
            data(r#"<button id="c" onclick="document.title=String(confirm('ok?'))">c</button>"#);
        let frame_page = h.url("/frame");
        let mut rows = Vec::new();
        let mut refused_before = h.proxy.refused_methods();
        // What Task 8 will do: `check`, then `new_tab` or `exec`.
        let run = |args: Vec<String>| {
            let h = &h;
            let info = &info;
            let ep = &ep;
            let files = &files;
            async move {
                let t0 = Instant::now();
                let r = match check(&args, files) {
                    Err(e) => Err(e),
                    Ok(Checked::TabNew { url }) => {
                        h.manager
                            .new_tab(&info.id, &url)
                            .await
                            .map(|t| CommandExecResult {
                                exit_code: 0,
                                output: json!({ "success": true, "data": { "targetId": t } }),
                                stderr: String::new(),
                            })
                    }
                    Ok(Checked::Run(argv)) => h.runner.exec(&info.id, ep, &argv, 20_000).await,
                };
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
            s(&["get", "value", "#name"]),
            s(&["get", "attr", "#name", "id"]),
            s(&["get", "count", "input"]),
            s(&["get", "box", "#name"]),
            s(&["get", "styles", "#name"]),
            s(&["eval", "document.getElementById('name').value"]),
            s(&["screenshot", "shot.png"]),
            s(&["screenshot"]),
            s(&["screenshot", "#name"]),
            s(&["scroll", "down"]),
            s(&["wait", "200"]),
            s(&["wait", "#name", "--state", "visible"]),
            s(&["wait", "--text", "Name"]),
            s(&["wait", "--fn", "document.readyState === 'complete'"]),
            s(&["wait", "--load", "load"]),
            s(&["open", &h.url("/counter")]),
            s(&["back"]),
            s(&["forward"]),
            s(&["reload"]),
            s(&["tab", "list"]),
            s(&["tab", "new", &h.url("/cookie/get")]),
            s(&["tab", "list"]),
            s(&["tab", "t1"]),
            s(&["tab", "1"]),
            s(&["cookies"]),
            s(&["cookies", "set", "pegmatrix", "1"]),
            s(&["cookies", "get"]),
            s(&["cookies", "clear"]),
            s(&["storage", "local"]),
            s(&["storage", "local", "set", "k", "v"]),
            s(&["storage", "local", "get", "k"]),
            s(&["storage", "session", "clear"]),
            s(&["network", "requests"]),
            s(&["network", "requests", "--filter", "counter"]),
            s(&["pdf", "page.pdf"]),
            s(&["open", &upload_page]),
            s(&["upload", "#f", "upload.txt"]),
            s(&["open", &download_page]),
            s(&["download", "#d", "x.txt"]),
            s(&["open", &dialog_page]),
            s(&["click", "#c"]),
            s(&["dialog", "status"]),
            s(&["dialog", "accept"]),
            s(&["get", "title"]),
            s(&["click", "#c"]),
            s(&["dialog", "dismiss"]),
            s(&["open", &frame_page]),
            s(&["frame", "#fr"]),
            s(&["fill", "#name", "in frame"]),
            s(&["frame", "main"]),
            s(&["open", &h.url("/counter")]),
            s(&["dblclick", "#btn"]),
            s(&["focus", "#btn"]),
            s(&["highlight", "#btn"]),
            s(&["keyboard", "type", "abc"]),
            s(&["keyboard", "inserttext", "abc"]),
            s(&["get", "html", "h1"]),
            s(&["is", "visible", "#btn"]),
            s(&["is", "enabled", "#btn"]),
            s(&["find", "role", "button", "click", "--name", "Add"]),
            s(&["find", "nth", "0", "button", "hover"]),
            s(&["scrollintoview", "#btn"]),
            s(&["mouse", "move", "10", "10"]),
            s(&["mouse", "down"]),
            s(&["mouse", "up"]),
            s(&["mouse", "wheel", "-50"]),
            s(&["scroll", "up", "100"]),
            s(&["set", "viewport", "800", "600"]),
            s(&["set", "media", "dark"]),
            s(&["set", "geo", "37.7", "-122.4"]),
            s(&["set", "offline", "on"]),
            s(&["set", "offline", "off"]),
            s(&["screenshot", "--full", "full.png"]),
            s(&["console"]),
            s(&["errors"]),
            s(&["a11y"]),
            s(&["vitals"]),
            s(&["diff", "snapshot"]),
            s(&["read", &h.url("/counter")]),
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
                Ok(r) if ok(r) && args[0] == "tab" => (
                    "pass",
                    format!(
                        "{} tabs",
                        r.output["data"]["tabs"].as_array().map_or(0, Vec::len)
                    ),
                ),
                Ok(r) if ok(r) => ("pass", String::new()),
                Ok(r) => ("fail", r.output["error"].to_string()),
                Err(e) if e.code == codes::ARGV_REJECTED => ("rejected", e.message.clone()),
                Err(e) => ("error", format!("{}: {}", e.code, e.message)),
            };
            let mut shown = args.join(" ").replace(&h.fixture.origin, "");
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
            std::fs::read_dir(&files)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect::<Vec<_>>()
        );
        println!("TARGETS {:?}", h.manager.info(&info.id).unwrap().targets);
        for row in rows {
            println!("ROW {row}");
        }
        h.finish().await;
    });
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
