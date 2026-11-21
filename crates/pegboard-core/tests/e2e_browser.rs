mod common;

use pegboard_core::browser::Browser;
use pegboard_core::cdp::page_targets;
use serde_json::json;

#[tokio::test]
#[ignore]
async fn launches_and_reports_version() {
    let e = common::env();
    let b = Browser::launch(&e.paths, &e.cfg).await.unwrap();
    let v = b
        .conn
        .send("Browser.getVersion", json!({}), None)
        .await
        .unwrap();
    assert!(v["product"].as_str().unwrap().starts_with("Chrome/"));
    b.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn agent_profile_window_has_its_own_context() {
    let e = common::env();
    let b = Browser::launch(&e.paths, &e.cfg).await.unwrap();
    let human = common::human_context(&b).await;
    let agent = b.open_profile_window("PegboardAgent").await.unwrap();
    assert_ne!(agent, human);
    let again = b.open_profile_window("PegboardAgent").await.unwrap();
    assert_eq!(again, agent); // same profile, same context
    b.shutdown().await.unwrap();
}

/// Opening an agent profile's window makes its context an agent context; the human
/// profile's stays a person's, and a non-agent profile directory is refused.
#[tokio::test]
#[ignore]
async fn opened_agent_profiles_are_agent_contexts() {
    let e = common::env();
    let b = Browser::launch(&e.paths, &e.cfg).await.unwrap();
    let human = common::human_context(&b).await;
    assert!(!b.agents.is_agent(&human));
    let agent = b.open_profile_window("PegboardAgent").await.unwrap();
    assert!(b.agents.is_agent(&agent));
    assert_eq!(b.agents.context_of("PegboardAgent"), Some(agent.clone()));
    assert!(b.downloads.is_ready(&agent));
    assert!(!b.agents.is_agent(&human));
    assert!(b.open_profile_window("Default").await.is_err());
    assert!(!b.agents.is_agent(&human));
    b.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn navigate_waits_for_load() {
    let e = common::env();
    let f = common::fixture().await;
    let b = Browser::launch(&e.paths, &e.cfg).await.unwrap();
    let t = &page_targets(&b.conn).await.unwrap()[0];
    let s = b.conn.attach(&t.target_id).await.unwrap();
    b.conn
        .navigate(&s, &format!("{}/counter", f.origin))
        .await
        .unwrap();
    assert_eq!(
        b.conn.evaluate(&s, "document.title").await.unwrap(),
        "counter"
    );
    b.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn human_tab_opened_meanwhile_is_not_mistaken_for_the_agent_profile() {
    let e = common::env();
    let b = Browser::launch(&e.paths, &e.cfg).await.unwrap();
    // Before any agent window: the launch profile's pages are the only ones.
    let human = common::human_context(&b).await;
    let human_tab = async {
        let r = b
            .conn
            .send("Target.createTarget", json!({ "url": "about:blank" }), None)
            .await
            .unwrap();
        let id = r["targetId"].as_str().unwrap().to_string();
        let t = page_targets(&b.conn)
            .await
            .unwrap()
            .into_iter()
            .find(|t| t.target_id == id)
            .unwrap();
        assert_eq!(t.browser_context_id.as_deref(), Some(human.as_str()));
    };
    let agent = b.open_profile_window("PegboardAgent");
    let (_, agent) = tokio::join!(human_tab, agent);
    let agent = agent.unwrap();
    assert_ne!(agent, human);
    let work = b.open_profile_window("PegboardAgent-work").await.unwrap();
    assert_ne!(work, human);
    assert_ne!(work, agent);
    b.shutdown().await.unwrap();
}

/// F11 on the fork (needs `PEGBOARD_TEST_BROWSER=fork:<Pegboard.app>` with
/// `PEGBOARD_TEST_HOOKS=1`; skipped otherwise). The test is the parent of the pipe, as the
/// browser is of a hosted daemon. A first daemon opens two agent tabs; a second one, started
/// on the same browser after the first let go of its control connection, closes them and
/// leaves the person's tab be.
#[tokio::test]
#[ignore]
async fn hosted_start_closes_leftover_agent_tabs() {
    use pegboard_core::browser::{fork_executable, test_fork_binary, DevToolsPipes};
    use pegboard_core::link::BrowserLink;

    std::env::set_var("PEGBOARD_TEST_HOOKS", "1");
    let Some(binary) = test_fork_binary() else {
        eprintln!("skipped: needs PEGBOARD_TEST_BROWSER=fork:<Pegboard.app>");
        return;
    };
    let exe = fork_executable(&binary);
    let e = common::env();
    let pipes = DevToolsPipes::new().unwrap();
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg(format!("--user-data-dir={}", e.paths.chrome_udd.display()))
        .arg("--remote-debugging-pipe")
        .arg("--pegboard-no-daemon")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    // SAFETY: the closure only makes async-signal-safe system calls.
    unsafe { cmd.pre_exec(pipes.child_setup()) };
    let mut child = cmd.spawn().unwrap();
    let (read, write) = pipes.into_parent_ends();
    let link = BrowserLink::pipe(read, write).unwrap();

    let first = Browser::hosted(&e.paths, link.clone()).await.unwrap();
    let human_tab = first
        .conn
        .send("Target.createTarget", json!({ "url": "about:blank" }), None)
        .await
        .unwrap()["targetId"]
        .as_str()
        .unwrap()
        .to_string();
    let agent = first.open_profile_window("PegboardAgent").await.unwrap();
    let bridge = first.tab_bridge(&agent).await.unwrap();
    let (group, t1) = bridge.create_group("left", "blue").await.unwrap();
    let (t2, _) = bridge
        .create_tab(&group, "about:blank", "left", "blue")
        .await
        .unwrap();
    let human = common::context_of(&first.conn, &human_tab).await;
    assert!(
        !first.agents.is_agent(&human),
        "the person's tab is in {human}"
    );
    // The first daemon goes: its control session with it, so the domain is free again.
    drop(bridge);
    first.conn.close();
    drop(first);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let second = Browser::hosted(&e.paths, link.clone()).await.unwrap();
    let left: Vec<String> = page_targets(&second.conn)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.target_id)
        .collect();
    assert!(!left.contains(&t1) && !left.contains(&t2), "{left:?}");
    assert!(
        left.contains(&human_tab),
        "the person's tab was closed: {left:?}"
    );

    second.quit().await;
    let _ = child.kill().await;
}
