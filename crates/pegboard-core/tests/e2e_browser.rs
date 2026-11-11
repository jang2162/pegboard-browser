mod common;

use pegboard_core::browser::Browser;
use pegboard_core::cdp::page_targets;
use serde_json::json;

#[tokio::test]
#[ignore]
async fn launches_and_reports_version() {
    let e = common::env();
    let b = Browser::launch(&e.paths, &e.cfg).await.unwrap();
    let v = b.conn.send("Browser.getVersion", json!({}), None).await.unwrap();
    assert!(v["product"].as_str().unwrap().starts_with("Chrome/"));
    b.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn agent_profile_window_has_its_own_context() {
    let e = common::env();
    let b = Browser::launch(&e.paths, &e.cfg).await.unwrap();
    let agent = b.open_profile_window(&e.paths, &e.cfg, "PegboardAgent").await.unwrap();
    assert_ne!(agent, b.human_context);
    let again = b.open_profile_window(&e.paths, &e.cfg, "PegboardAgent").await.unwrap();
    assert_eq!(again, agent); // same profile, same context
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
    b.conn.navigate(&s, &format!("{}/counter", f.origin)).await.unwrap();
    assert_eq!(b.conn.evaluate(&s, "document.title").await.unwrap(), "counter");
    b.shutdown().await.unwrap();
}
