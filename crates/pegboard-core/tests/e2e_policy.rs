// Shared browser (see common): the server path of `session.create` judges a policy itself,
// whatever the client checked. Run with
// `cargo test -p pegboard-core --test e2e_policy -- --ignored --test-threads=1`.
#![allow(dead_code)]
mod common;

use common::Harness;
use pegboard_protocol::{codes, ActionClass, SessionCreateParams, SessionPolicy};

fn create(policy: SessionPolicy) -> SessionCreateParams {
    SessionCreateParams {
        name: None,
        url: None,
        profile: None,
        policy,
    }
}

#[test]
#[ignore]
fn the_server_rejects_invalid_policies_and_keeps_nothing() {
    common::run(async {
        let h = Harness::start();
        let before = h.manager.list().len();
        let dir = std::env::temp_dir().display().to_string();
        let bad = [
            SessionPolicy {
                deny: vec![ActionClass::Upload],
                ..SessionPolicy::default()
            },
            SessionPolicy {
                allow: vec![ActionClass::Navigate],
                ..SessionPolicy::default()
            },
            SessionPolicy {
                upload_dir: Some(dir),
                ..SessionPolicy::default()
            },
            SessionPolicy {
                allow_domains: vec!["127.1".into()],
                ..SessionPolicy::default()
            },
        ];
        for policy in bad {
            // With a clipboard pin that would conflict with the shared sessions' if it leaked.
            let policy = SessionPolicy {
                allow: [policy.allow.clone(), vec![ActionClass::Clipboard]].concat(),
                ..policy
            };
            let e = h.manager.create(create(policy.clone())).await.unwrap_err();
            assert_eq!(e.code, codes::INVALID_PARAMS, "{policy:?}: {e:?}");
        }
        assert_eq!(h.manager.list().len(), before, "a session was created");
        // No pin was left: a clipboard session and a plain one can both be made in the
        // profile in turn (each closed before the next).
        let a = h
            .manager
            .create(create(SessionPolicy {
                allow: vec![ActionClass::Clipboard],
                ..SessionPolicy::default()
            }))
            .await
            .unwrap();
        h.close_session(&a.id).await;
        let b = h
            .manager
            .create(create(SessionPolicy::default()))
            .await
            .unwrap();
        h.close_session(&b.id).await;
    });
}

/// A session client (what agent-browser is) cannot switch off, take over or hide the daemon's
/// request checks: every method of `GUARD_METHODS` and the `Fetch` domain is refused on the
/// browser level and on its own page session, and afterwards a request to a private address
/// from the page is still refused by the guard.
#[test]
#[ignore]
fn agent_cannot_turn_the_guard_off() {
    use pegboard_core::address::PRIVATE;
    use pegboard_core::proxy::filter::GUARD_METHODS;
    use serde_json::json;

    common::run(async {
        let h = Harness::start();
        let (info, ep) = h.create("/counter").await;
        let c = common::client(&ep).await;
        let first = info.targets[0].clone();
        let session = c.attach(&first).await.unwrap();
        let fetch = [
            "Fetch.enable",
            "Fetch.disable",
            "Fetch.continueRequest",
            "Fetch.failRequest",
            "Fetch.fulfillRequest",
            "Fetch.continueResponse",
            "Fetch.getResponseBody",
        ];
        for method in GUARD_METHODS.iter().copied().chain(fetch) {
            let params = json!({ "patterns": [{ "urlPattern": "*" }], "urls": ["*"], "offline": true, "latency": 0, "downloadThroughput": -1, "uploadThroughput": -1, "bypass": true, "enabled": true });
            for sid in [None, Some(session.as_str())] {
                let r = c.send(method, params.clone(), sid).await;
                let e = format!("{:#}", r.expect_err(method));
                assert!(
                    e.contains(pegboard_core::proxy::filter::GUARD_REFUSED),
                    "{method} (session {sid:?}) was not refused by the guard rule: {e}"
                );
            }
        }
        let denied = |h: &Harness| h.manager.guard_denied().get(PRIVATE).copied().unwrap_or(0);
        let before = denied(&h);
        // An unroutable private address: a request the guard let through would hang (TIMEOUT),
        // one it refused fails at once.
        let got = c
            .evaluate(
                &session,
                "Promise.race([fetch('http://10.255.255.1:81/x', {mode: 'no-cors'}).then(() => 'REACHED', e => 'FAIL:' + e.message), new Promise(r => setTimeout(() => r('TIMEOUT'), 5000))])",
            )
            .await
            .unwrap();
        assert_eq!(got, "FAIL:Failed to fetch");
        assert!(denied(&h) > before, "the guard did not count the refusal");
    });
}

// ---- Review fixes: oversized documents, the human profile, the guard going down ----

use pegboard_core::actionlog::{ActionLog, Manifest};
use pegboard_core::cdp::{page_targets, CdpConn};
use pegboard_core::daemon::run_command;
use pegboard_core::guard::NOT_SERVED;
use pegboard_core::proxy::server::Endpoint;
use pegboard_protocol::SessionInfo;
use serde_json::json;
use std::time::Duration;

fn fixture_port(h: &Harness) -> String {
    h.fixture.origin.rsplit(':').next().unwrap().to_string()
}

/// A session of the test's own whose first page is `path` on `common::PUBLIC_NAME` (served
/// again with the CSP), and a client attached to its first tab.
async fn public_session(h: &Harness, path: &str) -> (SessionInfo, Endpoint, CdpConn, String) {
    let url = format!("http://{}:{}{path}", common::PUBLIC_NAME, fixture_port(h));
    let info = h
        .manager
        .create(SessionCreateParams {
            url: Some(url),
            ..create(SessionPolicy::default())
        })
        .await
        .unwrap();
    h.sessions.lock().unwrap().push(info.id.clone());
    let ep = h.manager.endpoint(&info.id).unwrap();
    let c = common::client(&ep).await;
    let session = c.attach(&info.targets[0]).await.unwrap();
    (info, ep, c, session)
}

fn not_served(h: &Harness) -> u64 {
    h.manager
        .guard_denied()
        .get(NOT_SERVED)
        .copied()
        .unwrap_or(0)
}

const WS_CSP_JS: &str = "new Promise(r => { const v = []; document.addEventListener('securitypolicyviolation', e => v.push(e.violatedDirective)); \
     let out = ''; try { const w = new WebSocket('ws://10.255.255.1:81/ws'); w.onopen = () => { out = 'OPEN'; w.close(); }; w.onerror = () => { out = out || 'ERROR'; }; } \
     catch (e) { out = 'THROW:' + e.name; } setTimeout(() => r((out || 'PENDING') + '|' + v.join(',')), 2000); })";

/// A document over the 16 MiB limit (declared, or only once decoded: a gzip bomb) is failed,
/// and reading it does not cost the guard its connection. A normal page from the same public
/// address loads and has the CSP.
#[test]
#[ignore]
fn oversized_documents_fail_and_the_guard_stays_up() {
    common::run(async {
        let h = Harness::start();
        let (_, _, c, session) = public_session(&h, "/counter").await;
        let connections = h.manager.guard_connections();
        let failed = not_served(&h);
        for path in ["/big", "/bomb"] {
            let url = format!("http://{}:{}{path}", common::PUBLIC_NAME, fixture_port(&h));
            let r = c
                .send("Page.navigate", json!({ "url": url }), Some(&session))
                .await
                .unwrap();
            let error = r["errorText"].as_str().unwrap_or_default();
            assert!(error.contains("BLOCKED_BY_CLIENT"), "{path}: {r}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(
            h.manager.guard_connections(),
            connections,
            "the guard reconnected"
        );
        assert!(h.manager.require_guard().is_ok());
        assert_eq!(not_served(&h), failed + 2);
        // The same address still serves normal pages, with the CSP.
        c.navigate(
            &session,
            &format!(
                "http://{}:{}/counter",
                common::PUBLIC_NAME,
                fixture_port(&h)
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            c.evaluate(&session, "document.title").await.unwrap(),
            "counter"
        );
        let ws = c.evaluate(&session, WS_CSP_JS).await.unwrap();
        let ws = ws.as_str().unwrap_or_default().to_string();
        assert!(
            ws.contains("connect-src") && !ws.starts_with("OPEN"),
            "{ws}"
        );
    });
}

/// This machine's private IPv4 address on the default route's interface.
fn lan_ip() -> std::net::Ipv4Addr {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    sock.connect("192.0.2.1:9").unwrap();
    match sock.local_addr().unwrap().ip() {
        std::net::IpAddr::V4(v4) if v4.is_private() => v4,
        other => panic!("this test needs a private IPv4 address, not {other}"),
    }
}

/// The human profile is never judged: a human page reaches a private address the default
/// policy would refuse, and a popup the human opens loads at once.
#[test]
#[ignore]
fn the_human_profile_is_untouched() {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    common::run(async {
        let h = Harness::start();
        let ip = lan_ip();
        let listener = std::net::TcpListener::bind((ip, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                counted.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\naccess-control-allow-origin: *\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok");
            }
        });
        let conn = &h.browser.conn;
        let human = page_targets(conn)
            .await
            .unwrap()
            .into_iter()
            .find(|t| t.browser_context_id.as_deref() == Some(h.browser.human_context.as_str()))
            .expect("a human page")
            .target_id;
        let s = conn.attach(&human).await.unwrap();
        conn.navigate(&s, &format!("{}/counter", h.fixture.origin))
            .await
            .unwrap();
        let denied_before: u64 = h.manager.guard_denied().values().sum();
        let got = conn
            .evaluate(
                &s,
                &format!("Promise.race([fetch('http://{ip}:{port}/x', {{mode: 'no-cors'}}).then(() => 'REACHED', e => 'FAIL:' + e.message), new Promise(r => setTimeout(() => r('TIMEOUT'), 5000))])"),
            )
            .await
            .unwrap();
        assert_eq!(got, "REACHED");
        assert!(hits.load(Ordering::SeqCst) > 0);
        assert_eq!(
            h.manager.guard_denied().values().sum::<u64>(),
            denied_before
        );

        // A popup of the human page: there and loaded within 3 s.
        let before: Vec<String> = page_targets(conn)
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.target_id)
            .collect();
        let started = std::time::Instant::now();
        let r = conn
            .send(
                "Runtime.evaluate",
                json!({ "expression": format!("String(!!window.open('{}/marker'))", h.fixture.origin), "userGesture": true, "returnByValue": true }),
                Some(&s),
            )
            .await
            .unwrap();
        assert_eq!(r["result"]["value"], "true", "{r}");
        let popup = loop {
            let found = page_targets(conn)
                .await
                .unwrap()
                .into_iter()
                .find(|t| !before.contains(&t.target_id) && t.url.ends_with("/marker"));
            if let Some(t) = found {
                break t.target_id;
            }
            assert!(started.elapsed() < Duration::from_secs(3), "no human popup");
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let ps = conn.attach(&popup).await.unwrap();
        loop {
            let marker = conn
                .evaluate(&ps, "window.pageMarker || null")
                .await
                .unwrap();
            if marker == "main" {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "the human popup did not load in 3 s"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let _ = conn
            .send("Target.closeTarget", json!({ "targetId": popup }), None)
            .await;
        let _ = conn.navigate(&s, "about:blank").await;
        let _ = conn
            .send("Target.detachFromTarget", json!({ "sessionId": s }), None)
            .await;
    });
}

/// Waits up to 15 s for the guard to be back.
async fn guard_back(h: &Harness) {
    let back = common::eventually(15, || async { h.manager.require_guard().is_ok() }).await;
    assert!(back, "the guard did not come back");
}

/// While the guard's connection is down, every command is refused; once it is back,
/// commands run again.
#[test]
#[ignore]
fn commands_are_refused_while_the_guard_is_down() {
    common::run(async {
        let h = Harness::start();
        let (info, _) = h.create("/counter").await;
        let manifest = Manifest::new(
            &info.recording_id,
            &info.id.0,
            &info.name,
            &info.profile,
            &info.created_at,
            "test",
            "0.42.0",
        );
        let mut log = ActionLog::open(&h.env.paths, manifest).unwrap();
        let connections = h.manager.guard_connections();
        assert!(h.manager.drop_guard_for_test(Duration::from_secs(3)));
        let r = run_command(
            &h.manager,
            &h.runner,
            &h.env.paths,
            &mut log,
            &info.id,
            &common::s(&["get", "url"]),
            Some(30_000),
        )
        .await;
        let e = r.unwrap_err();
        assert_eq!(e.code, codes::BROWSER_FAILED, "{e:?}");
        assert!(e.message.contains("request guard"), "{e:?}");
        guard_back(&h).await;
        assert!(h.manager.guard_connections() > connections);
        // Back: the same command gets past the guard check (it may still fail later for
        // lack of an attached agent-browser, but not with the guard's refusal).
        let r = run_command(
            &h.manager,
            &h.runner,
            &h.env.paths,
            &mut log,
            &info.id,
            &common::s(&["get", "url"]),
            Some(30_000),
        )
        .await;
        if let Err(e) = r {
            assert!(!e.message.contains("request guard"), "{e:?}");
        }
    });
}

/// A tab a session client creates (`Target.createTarget`, made by the bridge) joins the
/// scope only once the guard holds it: while the guard is down the creation fails and the tab
/// is closed; afterwards it works.
#[test]
#[ignore]
fn created_tabs_wait_for_the_guard() {
    common::run(async {
        let h = Harness::start();
        let (info, ep) = h.create("/counter").await;
        let c = common::client(&ep).await;
        let before = h.manager.info(&info.id).unwrap().targets;
        assert!(h.manager.drop_guard_for_test(Duration::from_secs(8)));
        let r = c
            .send("Target.createTarget", json!({ "url": "about:blank" }), None)
            .await;
        let e = format!("{:#}", r.unwrap_err());
        assert!(e.contains("could not check"), "{e}");
        assert_eq!(h.manager.info(&info.id).unwrap().targets, before);
        guard_back(&h).await;
        let r = c
            .send("Target.createTarget", json!({ "url": "about:blank" }), None)
            .await
            .unwrap();
        let created = r["targetId"].as_str().unwrap().to_string();
        assert!(h.manager.info(&info.id).unwrap().targets.contains(&created));
    });
}
