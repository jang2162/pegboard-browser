mod common;

use std::time::Duration;

use pegboard_core::bridge::Bridge;
use pegboard_core::cdp::page_targets;

#[tokio::test]
#[ignore]
async fn group_and_tabs_land_in_agent_profile() {
    let (e, f, b) = common::launched().await;
    let agent = b.open_profile_window(&e.paths, &e.cfg, "PegboardAgent").await.unwrap();
    let br = Bridge::attach(&b.conn, &agent).await.unwrap();
    let (g, first) = br.create_group("조사", "blue").await.unwrap();
    let second = br.create_tab(&g, &format!("{}/counter", f.origin)).await.unwrap();
    let all = page_targets(&b.conn).await.unwrap();
    for id in [&first, &second] {
        assert_eq!(all.iter().find(|t| &t.target_id == id).unwrap().browser_context_id.as_deref(), Some(agent.as_str()));
    }
    let m = br.members(&g).await.unwrap();
    assert!(m.contains(&first) && m.contains(&second));
    b.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn create_tab_ignores_which_window_was_last_active() {
    let (e, _f, b) = common::launched().await;
    let agent = b.open_profile_window(&e.paths, &e.cfg, "PegboardAgent").await.unwrap();
    let br = Bridge::attach(&b.conn, &agent).await.unwrap();
    let (g, _) = br.create_group("a", "red").await.unwrap();
    common::activate_any_target_in(&b.conn, &b.human_context).await;
    let t = br.create_tab(&g, "about:blank").await.unwrap();
    assert_eq!(common::context_of(&b.conn, &t).await, agent);
    b.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn bridge_still_answers_after_two_minutes_idle() {
    let (e, _f, b) = common::launched().await;
    let agent = b.open_profile_window(&e.paths, &e.cfg, "PegboardAgent").await.unwrap();
    let br = Bridge::attach(&b.conn, &agent).await.unwrap();
    let (g, first) = br.create_group("idle", "grey").await.unwrap();
    tokio::time::sleep(Duration::from_secs(120)).await;
    assert_eq!(br.members(&g).await.unwrap(), vec![first]);
    b.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn close_and_ungroup() {
    let (e, _f, b) = common::launched().await;
    let agent = b.open_profile_window(&e.paths, &e.cfg, "PegboardAgent").await.unwrap();
    let br = Bridge::attach(&b.conn, &agent).await.unwrap();
    let (g, first) = br.create_group("x", "blue").await.unwrap();
    let t = br.create_tab(&g, "about:blank").await.unwrap();
    br.close_targets(&[t.clone()]).await.unwrap();
    assert_eq!(br.members(&g).await.unwrap(), vec![first.clone()]);
    br.ungroup(&g).await.unwrap();
    assert!(br.members(&g).await.unwrap().is_empty());
    b.shutdown().await.unwrap();
}
