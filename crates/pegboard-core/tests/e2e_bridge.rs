// Shared browser: every test here except the idle one uses the binary's one browser (own bridge
// group, tabs closed at the end). Own browser: `bridge_still_answers_after_two_minutes_idle`,
// which needs a bridge no other test has touched for two minutes.
//
// The bridge under test is the extension bridge on stock Chrome and the domain bridge
// (`Pegboard.*`) on the Pegboard fork (`PEGBOARD_TEST_BROWSER=fork:<Pegboard.app>`). The fork
// cannot be launched by the tests yet (task 6): there every test here says so and returns.
mod common;

use std::sync::Arc;
use std::time::Duration;

use pegboard_core::bridge::{ExtensionBridge, TabBridge};
use pegboard_core::cdp::page_targets;
use serde_json::json;

/// Whether the tests are to run on the Pegboard fork. Test variables are read only with test
/// hooks on, which every test of this binary turns on anyway (`common::shared`).
fn on_fork() -> bool {
    std::env::set_var("PEGBOARD_TEST_HOOKS", "1");
    pegboard_core::testenv::var("PEGBOARD_TEST_BROWSER").is_some_and(|v| v.starts_with("fork:"))
}

/// Says that the test is skipped on the fork, which the tests cannot launch yet (task 6).
fn skip_on_fork() -> bool {
    let fork = on_fork();
    if fork {
        eprintln!("skipped: PEGBOARD_TEST_BROWSER=fork:… needs the fork launch of task 6");
    }
    fork
}

/// The agent profile scope of the shared browser and the bridge under test of that profile:
/// the extension bridge on stock Chrome. `None` (the test is skipped) on the fork.
async fn bridge_under_test() -> Option<(common::AgentScope, Arc<dyn TabBridge>)> {
    if skip_on_fork() {
        return None;
    }
    let a = common::AgentScope::start().await;
    let br = a.bridge.clone();
    Some((a, br))
}

#[test]
#[ignore]
fn group_and_tabs_land_in_agent_profile() {
    common::run(async {
        let Some((a, br)) = bridge_under_test().await else {
            return;
        };
        let sh = a.sh;
        let b = &sh.browser;
        let f = &sh.fixture;
        let agent = a.agent.clone();
        let (g, first) = br.create_group("조사", "blue").await.unwrap();
        let second = br
            .create_tab(&g, &format!("{}/counter", f.origin), "조사", "blue")
            .await
            .unwrap()
            .0;
        let all = page_targets(&b.conn).await.unwrap();
        for id in [&first, &second] {
            assert_eq!(
                all.iter()
                    .find(|t| &t.target_id == id)
                    .unwrap()
                    .browser_context_id
                    .as_deref(),
                Some(agent.as_str())
            );
        }
        let m = br.members(&g).await.unwrap();
        assert!(m.contains(&first) && m.contains(&second));
        a.finish().await;
    });
}

#[test]
#[ignore]
fn create_tab_ignores_which_window_was_last_active() {
    common::run(async {
        let Some((a, br)) = bridge_under_test().await else {
            return;
        };
        let sh = a.sh;
        let b = &sh.browser;
        let agent = a.agent.clone();
        let (g, _) = br.create_group("a", "red").await.unwrap();
        let human = common::human_context(b).await;
        common::activate_any_target_in(&b.conn, &human).await;
        let (t, moved) = br.create_tab(&g, "about:blank", "a", "red").await.unwrap();
        assert_eq!(moved, None);
        assert_eq!(common::context_of(&b.conn, &t).await, agent);
        a.finish().await;
    });
}

#[tokio::test]
#[ignore]
async fn bridge_still_answers_after_two_minutes_idle() {
    if skip_on_fork() {
        return;
    }
    let (e, _f, b) = common::launched().await;
    let agent = b
        .open_profile_window(&e.paths, &e.cfg, "PegboardAgent")
        .await
        .unwrap();
    let br: Arc<dyn TabBridge> = Arc::new(ExtensionBridge::attach(&b.conn, &agent).await.unwrap());
    let (g, first) = br.create_group("idle", "grey").await.unwrap();
    tokio::time::sleep(Duration::from_secs(120)).await;
    assert_eq!(br.members(&g).await.unwrap(), vec![first]);
    b.shutdown().await.unwrap();
}

#[test]
#[ignore]
fn close_and_ungroup() {
    common::run(async {
        let Some((a, br)) = bridge_under_test().await else {
            return;
        };
        let (g, first) = br.create_group("x", "blue").await.unwrap();
        let t = br
            .create_tab(&g, "about:blank", "x", "blue")
            .await
            .unwrap()
            .0;
        br.close_targets(std::slice::from_ref(&t)).await.unwrap();
        assert_eq!(br.members(&g).await.unwrap(), vec![first.clone()]);
        br.ungroup(&g).await.unwrap();
        assert!(br.members(&g).await.unwrap().is_empty());
        a.finish().await;
    });
}

#[test]
#[ignore]
fn bridge_refuses_targets_of_the_human_profile() {
    common::run(async {
        let Some((a, br)) = bridge_under_test().await else {
            return;
        };
        let sh = a.sh;
        let b = &sh.browser;
        let (g, _) = br.create_group("iso", "blue").await.unwrap();
        let human_context = common::human_context(b).await;
        let human = page_targets(&b.conn)
            .await
            .unwrap()
            .into_iter()
            .find(|t| t.browser_context_id.as_deref() == Some(human_context.as_str()))
            .expect("no human page")
            .target_id;
        let closed = br.close_targets(std::slice::from_ref(&human)).await;
        eprintln!("close_targets on human target: {closed:?}");
        assert!(closed.is_err());
        let added = br.add_to_group(&g, &human).await;
        eprintln!("add_to_group on human target: {added:?}");
        assert!(added.is_err());
        let after = page_targets(&b.conn).await.unwrap();
        let t = after
            .iter()
            .find(|t| t.target_id == human)
            .expect("human target vanished");
        assert_eq!(
            t.browser_context_id.as_deref(),
            Some(human_context.as_str())
        );
        assert!(!br.members(&g).await.unwrap().contains(&human));
        a.finish().await;
    });
}

#[test]
#[ignore]
fn bridge_refuses_relative_and_extension_urls() {
    common::run(async {
        let Some((a, br)) = bridge_under_test().await else {
            return;
        };
        let sh = a.sh;
        let b = &sh.browser;
        let (g, first) = br.create_group("urls", "blue").await.unwrap();
        let pages_before = page_targets(&b.conn).await.unwrap().len();
        for url in [
            "sw.js",
            "/sw.js",
            "chrome-extension://x/sw.js",
            "\u{a0}https:/../../sw.js",
            "file:///etc/passwd",
        ] {
            let r = br.create_tab(&g, url, "urls", "blue").await;
            eprintln!("create_tab({url:?}): {r:?}");
            assert!(r.is_err(), "{url:?} was accepted");
        }
        assert_eq!(br.members(&g).await.unwrap(), vec![first]);
        assert_eq!(page_targets(&b.conn).await.unwrap().len(), pages_before);
        a.finish().await;
    });
}

#[test]
#[ignore]
fn create_tab_starts_a_new_group_when_the_old_one_is_gone() {
    common::run(async {
        let Some((a, br)) = bridge_under_test().await else {
            return;
        };
        let sh = a.sh;
        let b = &sh.browser;
        let agent = a.agent.clone();
        let (g, first) = br.create_group("old", "blue").await.unwrap();
        // Chrome deletes the group with its last tab.
        b.conn
            .send("Target.closeTarget", json!({ "targetId": first }), None)
            .await
            .unwrap();
        for _ in 0..50 {
            if page_targets(&b.conn)
                .await
                .unwrap()
                .iter()
                .all(|t| t.target_id != first)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let (t, moved) = br
            .create_tab(&g, "about:blank", "again", "red")
            .await
            .unwrap();
        let new_group = moved.expect("a new group id");
        assert_ne!(new_group, g);
        assert_eq!(br.members(&new_group).await.unwrap(), vec![t.clone()]);
        assert_eq!(common::context_of(&b.conn, &t).await, agent);
        a.finish().await;
    });
}

#[test]
#[ignore]
fn failed_tab_creation_leaves_no_orphan() {
    common::run(async {
        let Some((a, br)) = bridge_under_test().await else {
            return;
        };
        let sh = a.sh;
        let b = &sh.browser;
        let (g, first) = br.create_group("old", "blue").await.unwrap();
        b.conn
            .send("Target.closeTarget", json!({ "targetId": first }), None)
            .await
            .unwrap();
        for _ in 0..50 {
            if page_targets(&b.conn)
                .await
                .unwrap()
                .iter()
                .all(|t| t.target_id != first)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let before: Vec<String> = page_targets(&b.conn)
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.target_id)
            .collect();
        // The group is gone, so the worker starts a new one; naming it with a colour Chrome does
        // not know makes that step fail after the tab exists.
        let r = br.create_tab(&g, "about:blank", "t", "not-a-colour").await;
        assert!(r.is_err(), "{r:?}");
        let mut gone = false;
        for _ in 0..50 {
            let now: Vec<String> = page_targets(&b.conn)
                .await
                .unwrap()
                .into_iter()
                .map(|t| t.target_id)
                .collect();
            if now.iter().all(|t| before.contains(t)) {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(gone, "the tab of the failed creation is still open");
        a.finish().await;
    });
}
