mod common;

use std::time::Duration;

use pegboard_core::cdp::{page_targets, CdpConn};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// HTTP status of a websocket upgrade attempt (101 when it succeeds).
async fn upgrade_status(url: &str, auth: Option<&str>) -> u16 {
    let mut req = url.into_client_request().unwrap();
    if let Some(a) = auth {
        req.headers_mut()
            .insert("Authorization", a.parse().unwrap());
    }
    match tokio_tungstenite::connect_async(req).await {
        Ok(_) => 101,
        Err(tungstenite::Error::Http(resp)) => resp.status().as_u16(),
        Err(e) => panic!("unexpected upgrade failure: {e}"),
    }
}

/// Target ids the client sees through `Target.getTargets`, sorted.
async fn listed(client: &CdpConn) -> Vec<String> {
    let r = client
        .send("Target.getTargets", json!({}), None)
        .await
        .unwrap();
    let mut ids: Vec<String> = r["targetInfos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["targetId"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

async fn create_tab(client: &CdpConn, params: Value) -> String {
    let r = client
        .send("Target.createTarget", params, None)
        .await
        .unwrap();
    r["targetId"].as_str().unwrap().to_string()
}

#[tokio::test]
#[ignore]
async fn rejects_missing_or_wrong_token() {
    let s = common::session_fixture().await;
    let url = &s.endpoint.url;
    assert_eq!(upgrade_status(url, None).await, 401);
    assert_eq!(upgrade_status(url, Some("Bearer wrong")).await, 401);
    let mut short = s.endpoint.token.clone();
    short.pop();
    assert_eq!(
        upgrade_status(url, Some(&format!("Bearer {short}"))).await,
        401
    );
    let unknown = format!("ws://127.0.0.1:{}/s/s_unknown00", s.proxy.port());
    let good = format!("Bearer {}", s.endpoint.token);
    assert_eq!(upgrade_status(&unknown, Some(&good)).await, 401);
    assert_eq!(upgrade_status(url, Some(&good)).await, 101);

    // A plain HTTP request is not a websocket upgrade.
    let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", s.proxy.port()))
        .await
        .unwrap();
    let path = url.split_once(&format!(":{}", s.proxy.port())).unwrap().1;
    sock.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut resp = String::new();
    sock.read_to_string(&mut resp).await.unwrap();
    assert!(resp.starts_with("HTTP/1.1 404"), "{resp}");
    s.shutdown().await;
}

#[tokio::test]
#[ignore]
async fn lists_only_session_targets() {
    let s = common::session_fixture().await;
    let c = common::client(&s.endpoint).await;
    assert_eq!(listed(&c).await, vec![s.first_target.clone()]);
    s.shutdown().await;
}

#[tokio::test]
#[ignore]
async fn cannot_attach_to_human_target() {
    let s = common::session_fixture().await;
    let c = common::client(&s.endpoint).await;
    let r = c
        .send(
            "Target.attachToTarget",
            json!({ "targetId": s.human_target, "flatten": true }),
            None,
        )
        .await;
    let err = format!("{:#}", r.unwrap_err());
    assert!(err.contains("No target with given id"), "{err}");
    s.shutdown().await;
}

#[tokio::test]
#[ignore]
async fn created_tab_lands_in_agent_profile_even_when_human_window_is_active() {
    let s = common::session_fixture().await;
    let c = common::client(&s.endpoint).await;
    let held = common::establish_human_premise(
        &s.env,
        &s.browser.conn,
        &s.browser.human_context,
        &s.human_target,
    )
    .await;
    if !held {
        eprintln!(
            "PREMISE NOT ESTABLISHED: even after re-invoking Chrome for a new human window a \
             context-less createTarget did not land in the human profile (manual check M8)"
        );
    }
    eprintln!("premise (human window last active) held: {held}");
    let t = create_tab(
        &c,
        json!({ "url": format!("{}/counter", s.fixture.origin) }),
    )
    .await;
    assert_eq!(
        common::context_of(&s.browser.conn, &t).await,
        s.agent_context
    );
    assert!(s.bridge.members(&s.group).await.unwrap().contains(&t));
    assert!(listed(&c).await.contains(&t));
    assert!(s.proxy.targets(&s.session).contains(&t));
    s.shutdown().await;
}

/// Drives `finish_tab`'s refusal branch without a test hook: a second session is registered
/// with a context that is not the one the bridge really creates tabs in (as if the bridge had
/// put the tab in the wrong profile). The proxy must refuse the creation and close the tab.
#[tokio::test]
#[ignore]
async fn tab_created_in_another_context_is_refused() {
    let s = common::session_fixture().await;
    let wrong = pegboard_protocol::SessionId::generate();
    let endpoint = s.proxy.register(
        &wrong,
        &s.browser.human_context, // not the agent context the bridge creates tabs in
        s.group.clone(),
        s.bridge.clone(),
        vec![],
    );
    let c = common::client(&endpoint).await;
    let before: Vec<String> = page_targets(&s.browser.conn)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.target_id)
        .collect();
    let err = c
        .send(
            "Target.createTarget",
            json!({ "url": format!("{}/counter", s.fixture.origin) }),
            None,
        )
        .await
        .expect_err("creation in another context must be refused");
    assert!(
        format!("{err:#}").contains("not created in the session's profile"),
        "{err:#}"
    );
    // The stray tab is gone (the proxy closes it and verifies, retrying), and the session
    // never owned it. The error says nothing about a tab remaining.
    let mut stray = Vec::new();
    for _ in 0..30 {
        stray = page_targets(&s.browser.conn)
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.target_id)
            .filter(|id| !before.contains(id))
            .collect();
        if stray.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(stray.is_empty(), "the refused tab was left open: {stray:?}");
    assert!(
        !format!("{err:#}").contains("could not be closed"),
        "{err:#}"
    );
    assert!(s.proxy.targets(&wrong).is_empty());
    s.shutdown().await;
}

#[tokio::test]
#[ignore]
async fn created_tab_is_announced_and_auto_attached() {
    let s = common::session_fixture().await;
    let c = common::client(&s.endpoint).await;
    let mut events = c.events();
    c.send(
        "Target.setDiscoverTargets",
        json!({ "discover": true }),
        None,
    )
    .await
    .unwrap();
    c.send(
        "Target.setAutoAttach",
        json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
        None,
    )
    .await
    .unwrap();
    let t = create_tab(
        &c,
        json!({ "url": format!("{}/counter", s.fixture.origin) }),
    )
    .await;
    // Chrome reports the new tab before the bridge returns its id; the client must still
    // see both events for its own tab, and nothing about anyone else's.
    let mut created = false;
    let mut attached = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !(created && attached.is_some()) && tokio::time::Instant::now() < deadline {
        let Ok(Ok(e)) = tokio::time::timeout(Duration::from_millis(200), events.recv()).await
        else {
            continue;
        };
        if e.method.starts_with("Target.") && e.session_id.is_none() {
            let id = e.params["targetInfo"]["targetId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(id == t || id == s.first_target, "{} about {id}", e.method);
            if id == t && e.method == "Target.targetCreated" {
                created = true;
            }
            if id == t && e.method == "Target.attachedToTarget" {
                attached = e.params["sessionId"].as_str().map(String::from);
            }
        }
    }
    assert!(created, "no Target.targetCreated for the new tab");
    let sid = attached.expect("no Target.attachedToTarget for the new tab");
    // The auto-attached session is in scope and usable.
    c.send("Runtime.runIfWaitingForDebugger", json!({}), Some(&sid))
        .await
        .unwrap();
    assert_eq!(c.evaluate(&sid, "1 + 1").await.unwrap(), 2);
    s.shutdown().await;
}

#[tokio::test]
#[ignore]
async fn browser_level_cookie_read_is_refused_and_page_level_is_scoped() {
    let s = common::session_fixture().await;
    let c = common::client(&s.endpoint).await;
    let err = format!(
        "{:#}",
        c.send("Storage.getCookies", json!({}), None)
            .await
            .unwrap_err()
    );
    assert!(err.contains("browser-level"), "{err}");

    let origin = &s.fixture.origin;
    let names = |r: Value| -> Vec<String> {
        r["cookies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect()
    };
    // The cookie exists in the human profile...
    let human = s.browser.conn.attach(&s.human_target).await.unwrap();
    let r = s
        .browser
        .conn
        .send(
            "Network.getCookies",
            json!({ "urls": [origin] }),
            Some(&human),
        )
        .await
        .unwrap();
    assert!(names(r).contains(&"spike".to_string()));
    // ...but not in the session's page of the same origin.
    let sid = c.attach(&s.first_target).await.unwrap();
    c.navigate(&sid, &format!("{origin}/counter"))
        .await
        .unwrap();
    let r = c
        .send(
            "Network.getCookies",
            json!({ "urls": [origin] }),
            Some(&sid),
        )
        .await
        .unwrap();
    assert!(!names(r).contains(&"spike".to_string()));
    s.shutdown().await;
}

/// Opens a popup from the session's first tab and returns the client's view after it appears.
async fn open_popup(s: &common::SessionFixture, c: &CdpConn) -> Vec<String> {
    let sid = c.attach(&s.first_target).await.unwrap();
    c.navigate(&sid, &format!("{}/opener", s.fixture.origin))
        .await
        .unwrap();
    c.send(
        "Runtime.evaluate",
        json!({ "expression": "document.getElementById('open').click()", "userGesture": true }),
        Some(&sid),
    )
    .await
    .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let ids = listed(c).await;
        if ids.len() == 2 || tokio::time::Instant::now() >= deadline {
            return ids;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
#[ignore]
async fn popup_from_session_tab_becomes_visible() {
    let s = common::session_fixture().await;
    let c = common::client(&s.endpoint).await;
    let mut events = c.events();
    c.send(
        "Target.setDiscoverTargets",
        json!({ "discover": true }),
        None,
    )
    .await
    .unwrap();
    let ids = open_popup(&s, &c).await;
    assert_eq!(ids.len(), 2, "{ids:?}");
    for id in &ids {
        assert_eq!(
            common::context_of(&s.browser.conn, id).await,
            s.agent_context
        );
    }
    let popup = ids
        .iter()
        .find(|id| **id != s.first_target)
        .unwrap()
        .clone();
    // The client learned about the popup from the event, not only from the listing.
    let mut announced = false;
    while let Ok(e) = events.try_recv() {
        announced |=
            e.method == "Target.targetCreated" && e.params["targetInfo"]["targetId"] == popup;
        assert_ne!(e.params["targetInfo"]["targetId"], s.human_target);
    }
    assert!(announced);
    assert!(s.proxy.targets(&s.session).contains(&popup));
    s.shutdown().await;
}

#[tokio::test]
#[ignore]
async fn popup_is_visible_without_target_discovery() {
    let s = common::session_fixture().await;
    let c = common::client(&s.endpoint).await;
    let ids = open_popup(&s, &c).await;
    assert_eq!(ids.len(), 2, "{ids:?}");
    for id in &ids {
        assert_eq!(
            common::context_of(&s.browser.conn, id).await,
            s.agent_context
        );
    }
    s.shutdown().await;
}

#[tokio::test]
#[ignore]
async fn two_sessions_do_not_see_each_other() {
    let s = common::session_fixture().await;
    let (g2, t2) = s.bridge.create_group("second", "red").await.unwrap();
    let id2 = pegboard_protocol::SessionId::generate();
    let ep2 = s.proxy.register(
        &id2,
        &s.agent_context,
        g2,
        s.bridge.clone(),
        vec![t2.clone()],
    );
    let a = common::client(&s.endpoint).await;
    let b = common::client(&ep2).await;
    assert_eq!(listed(&a).await, vec![s.first_target.clone()]);
    assert_eq!(listed(&b).await, vec![t2.clone()]);
    let r = a
        .send(
            "Target.attachToTarget",
            json!({ "targetId": t2, "flatten": true }),
            None,
        )
        .await;
    assert!(r.is_err());
    // A's CDP session is not usable through B's endpoint either.
    let sid = a.attach(&s.first_target).await.unwrap();
    assert!(b.evaluate(&sid, "1").await.is_err());
    // Each endpoint only accepts its own token.
    let wrong = format!("Bearer {}", s.endpoint.token);
    assert_eq!(upgrade_status(&ep2.url, Some(&wrong)).await, 401);
    s.shutdown().await;
}

#[tokio::test]
#[ignore]
async fn unregister_drops_clients() {
    let s = common::session_fixture().await;
    let c = common::client(&s.endpoint).await;
    assert_eq!(listed(&c).await.len(), 1);
    s.proxy.unregister(&s.session);
    let failed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if c.send("Target.getTargets", json!({}), None).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(failed.is_ok(), "client still works 2 s after unregister");
    let auth = format!("Bearer {}", s.endpoint.token);
    assert_eq!(upgrade_status(&s.endpoint.url, Some(&auth)).await, 401);
    s.shutdown().await;
}

#[tokio::test]
#[ignore]
async fn session_tabs_have_no_extension_privileges() {
    let s = common::session_fixture().await;
    let c = common::client(&s.endpoint).await;
    let counter_url = format!("{}/counter", s.fixture.origin);
    let blank = create_tab(&c, json!({})).await;
    let counter = create_tab(&c, json!({ "url": counter_url })).await;
    let apis = r#"typeof chrome === "undefined" ? "none" : (typeof chrome.tabs + "," + typeof chrome.debugger + "," + typeof chrome.tabGroups)"#;
    for (target, expect_origin) in [
        (&s.first_target, None),
        (&blank, None),
        (&counter, Some(s.fixture.origin.as_str())),
    ] {
        let sid = c.attach(target).await.unwrap();
        if let Some(origin) = expect_origin {
            // The bridge navigates after creating the tab; wait for the page itself.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while c.evaluate(&sid, "location.origin").await.unwrap() != origin {
                assert!(tokio::time::Instant::now() < deadline, "page did not load");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let found = c.evaluate(&sid, apis).await.unwrap();
        let origin = c.evaluate(&sid, "location.origin").await.unwrap();
        let href = c.evaluate(&sid, "location.href").await.unwrap();
        eprintln!("{href}: apis={found} origin={origin}");
        assert!(
            !found.as_str().unwrap().contains("object"),
            "{href}: {found}"
        );
        assert!(
            !origin.as_str().unwrap().starts_with("chrome-extension://"),
            "{href}: {origin}"
        );
    }
    s.shutdown().await;
}

#[tokio::test]
#[ignore]
async fn navigate_to_file_url_is_refused() {
    let s = common::session_fixture().await;
    let c = common::client(&s.endpoint).await;
    let sid = c.attach(&s.first_target).await.unwrap();
    let r = c
        .send(
            "Page.navigate",
            json!({ "url": "file:///etc/hosts" }),
            Some(&sid),
        )
        .await;
    let err = format!("{:#}", r.unwrap_err());
    assert!(err.contains("url scheme"), "{err}");
    let href = c.evaluate(&sid, "location.href").await.unwrap();
    assert_eq!(href, "about:blank");
    s.shutdown().await;
}

#[tokio::test]
#[ignore]
async fn human_tab_opened_during_session_loads_normally() {
    let s = common::session_fixture().await;
    let c = common::client(&s.endpoint).await;
    c.send(
        "Target.setAutoAttach",
        json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
        None,
    )
    .await
    .unwrap();
    // CDP cannot address the human profile's context in Target.createTarget, so the human
    // page opens the tab itself, as a person clicking a link would.
    let b = &s.browser.conn;
    let before: Vec<String> = page_targets(b)
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.target_id)
        .collect();
    let human = b.attach(&s.human_target).await.unwrap();
    let open = format!("window.open('{}/counter'); 'opened'", s.fixture.origin);
    b.send(
        "Runtime.evaluate",
        json!({ "expression": open, "userGesture": true }),
        Some(&human),
    )
    .await
    .unwrap();
    let mut t = None;
    for _ in 0..30 {
        t = page_targets(b)
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.target_id)
            .find(|id| !before.contains(id));
        if t.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let t = t.expect("the human page did not open a tab");
    assert_eq!(common::context_of(b, &t).await, s.browser.human_context);
    let sid = b.attach(&t).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let title = tokio::time::timeout(
            Duration::from_millis(500),
            b.evaluate(&sid, "document.title"),
        )
        .await;
        if let Ok(Ok(v)) = title {
            if v == "counter" {
                break;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "human tab did not load within 5 s"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // The session never sees the human tab.
    assert!(!listed(&c).await.contains(&t));
    assert!(page_targets(b)
        .await
        .unwrap()
        .iter()
        .any(|p| p.target_id == t));
    s.shutdown().await;
}

type RawWs =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A websocket client of the endpoint with no CDP machinery: it reads only when asked.
async fn raw_client(endpoint: &pegboard_core::proxy::server::Endpoint) -> RawWs {
    let mut req = endpoint.url.as_str().into_client_request().unwrap();
    let auth = format!("Bearer {}", endpoint.token);
    req.headers_mut()
        .insert("Authorization", auth.parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

/// Sends one request and reads until its response, discarding everything else.
async fn raw_call(ws: &mut RawWs, id: u64, method: &str, params: Value) -> Value {
    use futures_util::{SinkExt, StreamExt};
    let msg = json!({ "id": id, "method": method, "params": params });
    ws.send(tungstenite::Message::text(msg.to_string()))
        .await
        .unwrap();
    loop {
        let m = ws.next().await.expect("closed").unwrap();
        if let tungstenite::Message::Text(t) = m {
            let v: Value = serde_json::from_str(t.as_str()).unwrap();
            if v["id"] == id {
                return v;
            }
        }
    }
}

fn auto_attach_waiting() -> Value {
    json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true })
}

fn page_ids(v: Vec<pegboard_core::cdp::TargetInfo>) -> Vec<String> {
    v.into_iter().map(|p| p.target_id).collect()
}

/// Makes the human page open `/counter` in a new tab, as a click would, and returns the
/// page ids from before. The call is deferred with setTimeout so that a frozen popup
/// cannot block this evaluate.
async fn schedule_human_popup(s: &common::SessionFixture, human_session: &str) -> Vec<String> {
    let b = &s.browser.conn;
    let before = page_ids(page_targets(b).await.unwrap());
    let open = format!(
        "setTimeout(() => window.open('{}/counter'), 0); 'scheduled'",
        s.fixture.origin
    );
    b.send(
        "Runtime.evaluate",
        json!({ "expression": open, "userGesture": true }),
        Some(human_session),
    )
    .await
    .unwrap();
    before
}

async fn new_page(b: &CdpConn, before: &[String]) -> String {
    for _ in 0..50 {
        let now = page_ids(page_targets(b).await.unwrap());
        if let Some(t) = now.into_iter().find(|id| !before.contains(id)) {
            return t;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the human page did not open a tab");
}

async fn open_human_popup(s: &common::SessionFixture, human_session: &str) -> String {
    let before = schedule_human_popup(s, human_session).await;
    new_page(&s.browser.conn, &before).await
}

/// Whether the tab reaches document.title "counter" within `limit`, seen from the browser connection.
async fn loads_within(b: &CdpConn, target: &str, limit: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    let Ok(Ok(sid)) = tokio::time::timeout(limit, b.attach(target)).await else {
        return false;
    };
    while tokio::time::Instant::now() < deadline {
        let title = tokio::time::timeout(
            Duration::from_millis(500),
            b.evaluate(&sid, "document.title"),
        )
        .await;
        if let Ok(Ok(v)) = title {
            if v == "counter" {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn connections_reach_zero(s: &common::SessionFixture, limit: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    while s.proxy.open_connections() != 0 {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    true
}

#[tokio::test]
#[ignore]
async fn client_that_stops_reading_cannot_stall_unregister_or_freeze_human_tabs() {
    use futures_util::SinkExt;
    let s = common::session_fixture().await;
    let mut ws = raw_client(&s.endpoint).await;
    let attached = raw_call(
        &mut ws,
        1,
        "Target.attachToTarget",
        json!({ "targetId": s.first_target, "flatten": true }),
    )
    .await;
    let sid = attached["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    raw_call(&mut ws, 2, "Target.setAutoAttach", auto_attach_waiting()).await;
    assert_eq!(s.proxy.open_connections(), 1);
    // From here on the client never reads: 1 MB answers pile up behind it.
    for id in 3..203u64 {
        let msg = json!({ "id": id, "sessionId": sid, "method": "Runtime.evaluate",
            "params": { "expression": "'x'.repeat(1 << 20)", "returnByValue": true } });
        if ws
            .send(tungstenite::Message::text(msg.to_string()))
            .await
            .is_err()
        {
            break;
        }
    }
    let human = s.browser.conn.attach(&s.human_target).await.unwrap();
    let popup = open_human_popup(&s, &human).await;
    assert!(
        loads_within(&s.browser.conn, &popup, Duration::from_secs(5)).await,
        "human popup froze behind a client that does not read"
    );
    s.proxy.unregister(&s.session);
    assert!(
        connections_reach_zero(&s, Duration::from_secs(3)).await,
        "upstream still open 3 s after unregister"
    );
    drop(ws);
    s.shutdown().await;
}

#[tokio::test]
#[ignore]
async fn human_popup_is_released_while_tab_creation_is_in_flight() {
    let s = common::session_fixture().await;
    // Every tab creation stays in flight for 8 s, longer than the 5 s the popup may take.
    s.proxy.set_tab_creation_delay(Duration::from_secs(8));
    let c = common::client(&s.endpoint).await;
    c.send("Target.setAutoAttach", auto_attach_waiting(), None)
        .await
        .unwrap();
    let creations: Vec<_> = (0..3)
        .map(|_| {
            let c = c.clone();
            tokio::spawn(async move { c.send("Target.createTarget", json!({}), None).await })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let human = s.browser.conn.attach(&s.human_target).await.unwrap();
    let popup = open_human_popup(&s, &human).await;
    assert!(
        loads_within(&s.browser.conn, &popup, Duration::from_secs(5)).await,
        "human popup was held while tab creations were in flight"
    );
    for h in creations {
        let r = h.await.unwrap().unwrap();
        assert!(r["targetId"].is_string());
    }
    assert_eq!(listed(&c).await.len(), 4);
    s.shutdown().await;
}

#[tokio::test]
#[ignore]
async fn disconnect_right_after_human_popup_does_not_freeze_it() {
    let s = common::session_fixture().await;
    let human = s.browser.conn.attach(&s.human_target).await.unwrap();
    for round in 0..5 {
        let mut ws = raw_client(&s.endpoint).await;
        raw_call(&mut ws, 1, "Target.setAutoAttach", auto_attach_waiting()).await;
        // Schedule the popup, then vanish at once without a close handshake.
        let before = schedule_human_popup(&s, &human).await;
        drop(ws);
        let popup = new_page(&s.browser.conn, &before).await;
        assert!(
            loads_within(&s.browser.conn, &popup, Duration::from_secs(5)).await,
            "round {round}: human popup froze after the client disconnected"
        );
    }
    assert!(connections_reach_zero(&s, Duration::from_secs(3)).await);
    s.shutdown().await;
}

#[tokio::test]
#[ignore]
async fn client_turning_auto_attach_off_does_not_freeze_human_popup() {
    let s = common::session_fixture().await;
    let human = s.browser.conn.attach(&s.human_target).await.unwrap();
    let c = common::client(&s.endpoint).await;
    let off = json!({ "autoAttach": false, "waitForDebuggerOnStart": false, "flatten": true });
    for round in 0..5 {
        c.send("Target.setAutoAttach", auto_attach_waiting(), None)
            .await
            .unwrap();
        // Chrome detaches every auto-attached session when auto-attach goes off, also the
        // popup's if the proxy has not resumed it yet.
        let before = schedule_human_popup(&s, &human).await;
        c.send("Target.setAutoAttach", off.clone(), None)
            .await
            .unwrap();
        let popup = new_page(&s.browser.conn, &before).await;
        assert!(
            loads_within(&s.browser.conn, &popup, Duration::from_secs(5)).await,
            "round {round}: human popup froze after the client turned auto-attach off"
        );
    }
    s.shutdown().await;
}

#[tokio::test]
#[ignore]
async fn targets_entering_the_scope_are_reported_before_the_client_hears_of_them() {
    let s = common::session_fixture().await;
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
    let log = seen.clone();
    s.proxy.on_target_added(move |id, target| {
        log.lock().unwrap().push((id.0.clone(), target.to_string()));
    });
    let c = common::client(&s.endpoint).await;
    // A tab created through the client: reported by the time the client has its reply.
    let t = create_tab(&c, json!({ "url": "about:blank" })).await;
    assert_eq!(
        seen.lock().unwrap().clone(),
        vec![(s.session.0.clone(), t.clone())]
    );
    // An adopted popup.
    let ids = open_popup(&s, &c).await;
    let popup = ids
        .iter()
        .find(|id| **id != s.first_target && **id != t)
        .expect("popup listed")
        .clone();
    assert!(seen
        .lock()
        .unwrap()
        .contains(&(s.session.0.clone(), popup.clone())));
    // add_target reports only targets new to the scope; remove_target takes them out.
    seen.lock().unwrap().clear();
    let (_, other) = s.bridge.create_group("other", "red").await.unwrap();
    assert!(s.proxy.add_target(&s.session, &other));
    assert!(s.proxy.add_target(&s.session, &other));
    assert_eq!(
        seen.lock().unwrap().clone(),
        vec![(s.session.0.clone(), other.clone())]
    );
    assert!(listed(&c).await.contains(&other));
    s.proxy.remove_target(&s.session, &other);
    assert!(!s.proxy.targets(&s.session).contains(&other));
    assert!(!listed(&c).await.contains(&other));
    let unknown = pegboard_protocol::SessionId::generate();
    assert!(!s.proxy.add_target(&unknown, &other));
    s.shutdown().await;
}
