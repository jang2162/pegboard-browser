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

/// Port of the fixture server Chrome counts as a public address (serves `PUBLIC_NAME`).
fn public_port(h: &Harness) -> String {
    h.public_fixture
        .origin
        .rsplit(':')
        .next()
        .unwrap()
        .to_string()
}

fn fixture_port(h: &Harness) -> String {
    h.fixture.origin.rsplit(':').next().unwrap().to_string()
}

/// A session of the test's own whose first page is `path` on `common::PUBLIC_NAME` (served
/// again with the CSP), and a client attached to its first tab.
async fn public_session(h: &Harness, path: &str) -> (SessionInfo, Endpoint, CdpConn, String) {
    let url = format!("http://{}:{}{path}", common::PUBLIC_NAME, public_port(h));
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
            let url = format!("http://{}:{}{path}", common::PUBLIC_NAME, public_port(&h));
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
            &format!("http://{}:{}/counter", common::PUBLIC_NAME, public_port(&h)),
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

        // A popup of the human page loads within 3 s, under the guard and, as a control, in
        // a second browser launched the same way that has no guard.
        let (_env2, fixture2, plain) = common::launched().await;
        let plain_human = human_page(&plain).await;
        let ps = plain.conn.attach(&plain_human).await.unwrap();
        plain
            .conn
            .navigate(&ps, &format!("{}/counter", fixture2.origin))
            .await
            .unwrap();
        let control = human_popup_loads(&plain.conn, &ps, &fixture2.origin).await;
        plain.close().await;
        eprintln!("control (no guard): {control:?}");
        let control_popup =
            control.expect("the popup step fails without a guard: the test is wrong");
        let guarded = human_popup_loads(conn, &s, &h.fixture.origin).await;
        let popup = match guarded {
            Ok((popup, took)) => {
                eprintln!(
                    "under the guard: loaded in {took:?} (control: {:?})",
                    control_popup.1
                );
                popup
            }
            Err((popup, state)) => {
                let log: Vec<String> = h
                    .manager
                    .guard_attach_log()
                    .into_iter()
                    .filter(|l| popup.as_deref().is_some_and(|p| l.starts_with(p)))
                    .collect();
                panic!("the human popup did not load in 3 s under the guard: {state}; guard log: {log:?}");
            }
        };
        let log: Vec<String> = h
            .manager
            .guard_attach_log()
            .into_iter()
            .filter(|l| l.starts_with(&popup))
            .collect();
        eprintln!("guard log for the popup: {log:?}");
        // The guard saw the popup as the human's and let it go at once.
        assert!(
            log.iter()
                .any(|l| l.contains(" human ") && l.contains("released resume=ok")),
            "{log:?}"
        );
        let _ = conn
            .send("Target.closeTarget", json!({ "targetId": popup }), None)
            .await;
        let _ = conn.navigate(&s, "about:blank").await;
        let _ = conn
            .send("Target.detachFromTarget", json!({ "sessionId": s }), None)
            .await;
    });
}

/// The first page of the browser's human profile.
async fn human_page(b: &pegboard_core::browser::Browser) -> String {
    page_targets(&b.conn)
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.browser_context_id.as_deref() == Some(b.human_context.as_str()))
        .expect("a human page")
        .target_id
}

/// Makes the human page (CDP session `human`) open `<origin>/counter` with a user gesture and
/// waits up to 3 s for the popup's title. `Ok((popup, time to load))`, or `Err((popup if one
/// appeared, where it stood))`.
async fn human_popup_loads(
    conn: &CdpConn,
    human: &str,
    origin: &str,
) -> Result<(String, Duration), (Option<String>, String)> {
    let limit = Duration::from_secs(3);
    let before: Vec<String> = page_targets(conn)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.target_id)
        .collect();
    let started = std::time::Instant::now();
    // Deferred, so that a held popup cannot hold this evaluate too.
    let open = format!("setTimeout(() => window.open('{origin}/counter'), 0); 'scheduled'");
    conn.send(
        "Runtime.evaluate",
        json!({ "expression": open, "userGesture": true }),
        Some(human),
    )
    .await
    .unwrap();
    let popup = loop {
        let found = page_targets(conn)
            .await
            .unwrap()
            .into_iter()
            .find(|t| !before.contains(&t.target_id));
        if let Some(t) = found {
            break t.target_id;
        }
        if started.elapsed() > limit {
            return Err((None, "no popup target appeared".into()));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let ps = conn.attach(&popup).await.unwrap();
    let mut last = String::new();
    while started.elapsed() < limit {
        let state = tokio::time::timeout(
            Duration::from_millis(500),
            conn.evaluate(
                &ps,
                "JSON.stringify([location.href, document.readyState, document.title])",
            ),
        )
        .await;
        match state {
            Ok(Ok(v)) => {
                last = v.as_str().unwrap_or_default().to_string();
                if last.contains("\"counter\"]") {
                    return Ok((popup, started.elapsed()));
                }
            }
            Ok(Err(e)) => last = format!("evaluate failed: {e:#}"),
            Err(_) => last = "evaluate did not answer in 500 ms".into(),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let info = conn
        .send("Target.getTargetInfo", json!({ "targetId": popup }), None)
        .await
        .map(|v| v["targetInfo"].to_string())
        .unwrap_or_else(|e| format!("{e:#}"));
    Err((Some(popup), format!("last state {last}; target {info}")))
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

/// Frames `src` from the page and, once the frame has loaded, reports from the parent:
/// whether the frame's document can be read (`frame=` its URL, readyState, contentType, or
/// the error), and what the attacker's step gives: `new frames[i].WebSocket(url)` created
/// through the parent, with the CSP violations the frame reported when they can be seen.
fn frame_probe_js(src: &str, url: &str) -> String {
    format!(
        "new Promise(r => {{ const f = document.createElement('iframe'); f.src = '{src}'; \
         f.onload = () => {{ const i = Array.from(frames).indexOf(f.contentWindow); \
           let frame = ''; const v = []; \
           try {{ const d = f.contentWindow.document; frame = [d.URL, d.readyState, d.contentType].join(' '); \
                  d.addEventListener('securitypolicyviolation', e => v.push(e.violatedDirective)); }} \
           catch (e) {{ frame = 'unreadable:' + e.name; }} \
           let out = ''; try {{ const s = new frames[i].WebSocket('{url}'); s.onopen = () => {{ out = 'OPEN'; s.close(); }}; s.onerror = () => {{ out = out || 'ERROR'; }}; }} \
           catch (e) {{ out = 'THROW:' + e.name; }} \
           setTimeout(() => r('ws=' + (out || 'PENDING') + ' violations=' + v.join(',') + ' frame=' + frame), 2000); }}; \
         document.body.appendChild(f); setTimeout(() => r('TIMEOUT no load event'), 8000); }})"
    )
}

/// The child frames of the page `session` is attached to: url, unreachableUrl (set when the
/// frame shows an error page), securityOrigin.
async fn child_frames(conn: &CdpConn, session: &str) -> Vec<String> {
    let tree = conn
        .send("Page.getFrameTree", json!({}), Some(session))
        .await
        .unwrap();
    tree["frameTree"]["childFrames"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|c| {
            let f = &c["frame"];
            format!(
                "url={} unreachable={} origin={}",
                f["url"], f["unreachableUrl"], f["securityOrigin"]
            )
        })
        .collect()
}

/// An image or text document framed by a page from a public address (same origin): it loads
/// (one child frame, not an error page), the guard serves it again with the CSP, and the
/// attacker's step from the parent (`new frames[i].WebSocket(...)` in the frame's realm) is
/// refused by `connect-src`. Two controls: on a local page the frame loads without being
/// served again and no violation is reported; served again without the CSP header (test
/// hook) the frame loads and no violation is reported. The guard traces to stderr
/// (--nocapture).
#[test]
#[ignore]
fn framed_documents_of_a_public_page_get_the_csp() {
    common::run(async {
        let h = Harness::start();
        let (info, _, _client, _) = public_session(&h, "/counter").await;
        let conn = &h.browser.conn;
        let s = conn.attach(&info.targets[0]).await.unwrap();
        conn.send("Page.enable", json!({}), Some(&s)).await.unwrap();
        let ws = "ws://10.255.255.1:81/ws";
        let local = format!("http://127.0.0.1:{}", fixture_port(&h));
        let public = format!("http://{}:{}", common::PUBLIC_NAME, public_port(&h));
        // (label, page origin, CSP header on, served again, CSP expected)
        let setups = [
            ("local page", local, true, false, false),
            ("public page", public.clone(), true, true, true),
            ("public page, no CSP header", public, false, true, false),
        ];
        let mut failures = Vec::new();
        for (label, origin, header, served_again, csp) in setups {
            assert!(h.manager.set_guard_csp_header_for_test(header));
            conn.navigate(&s, &format!("{origin}/counter"))
                .await
                .unwrap();
            assert_eq!(
                conn.evaluate(&s, "document.title").await.unwrap(),
                "counter",
                "{label}: the page did not load"
            );
            for src in ["/pixel.gif", "/plain.txt"] {
                let served_before = h.manager.guard_served();
                let got = conn.evaluate(&s, &frame_probe_js(src, ws)).await.unwrap();
                let got = got.as_str().unwrap_or_default().to_string();
                let frames = child_frames(conn, &s).await;
                let served = h.manager.guard_served() - served_before;
                eprintln!(
                    "{label}, framed {src}: {got}; served again: {served}; frames: {frames:?}"
                );
                let loaded = frames.len() == 1
                    && frames[0].contains(&format!("{src}\""))
                    && frames[0].contains("unreachable=null");
                let readable = got.contains(&format!("frame={origin}{src} complete"));
                let violated = got.contains("violations=connect-src");
                let mut check = |ok: bool, what: &str| {
                    if !ok {
                        failures.push(format!("{label}, {src}: {what}: {got}; frames {frames:?}"));
                    }
                };
                check(loaded, "the framed document did not load");
                check(
                    readable,
                    "the framed document is not readable from its page",
                );
                check(
                    (served > 0) == served_again,
                    "served again or not, unlike expected",
                );
                check(
                    violated == csp,
                    "connect-src violation present or absent, unlike expected",
                );
                check(!got.starts_with("ws=OPEN"), "the WebSocket opened");
                let _ = conn
                    .evaluate(
                        &s,
                        "document.querySelectorAll('iframe').forEach(f => f.remove()); 1",
                    )
                    .await;
            }
        }
        assert!(h.manager.set_guard_csp_header_for_test(true));
        let _ = conn
            .send("Target.detachFromTarget", json!({ "sessionId": s }), None)
            .await;
        assert!(failures.is_empty(), "{failures:#?}");
    });
}

/// Runs `get url` in the session through `run_command` (the daemon's path).
async fn get_url(
    h: &Harness,
    log: &mut ActionLog,
    info: &SessionInfo,
) -> Result<pegboard_protocol::CommandExecResult, pegboard_protocol::RpcError> {
    run_command(
        &h.manager,
        &h.runner,
        &h.env.paths,
        log,
        &info.id,
        &common::s(&["get", "url"]),
        Some(30_000),
    )
    .await
}

/// The download policy of a profile lives on one connection (Chrome lifts it when that
/// connection closes, spike L5). When it closes, the session's commands are refused (not run
/// unprotected) until pegboard has set the policy again; a download afterwards still lands
/// in the session's files directory and never in ~/Downloads.
#[test]
#[ignore]
fn download_policy_is_reapplied_after_its_connection_drops() {
    common::run(async {
        let h = Harness::start();
        let (info, _) = h.session("/counter").await;
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
        assert!(h.manager.require_downloads(&info.id).is_ok());
        assert!(h
            .manager
            .drop_download_policy_for_test(&info.id, Duration::from_secs(3)));
        // While it is down: refused with browser_failed, before anything runs.
        let e = get_url(&h, &mut log, &info).await.unwrap_err();
        assert_eq!(e.code, codes::BROWSER_FAILED, "{e:?}");
        assert!(e.message.contains("download policy"), "{e:?}");
        let back = common::eventually(20, || async {
            h.manager.require_downloads(&info.id).is_ok()
        })
        .await;
        assert!(back, "the download policy was not set again");
        let r = get_url(&h, &mut log, &info).await.unwrap();
        assert!(common::ok(&r), "{r:?}");

        // A download from the session's tab lands in its files directory only.
        let probe = common::DownloadProbe::new();
        let tab = h.manager.info(&info.id).unwrap().targets[0].clone();
        h.eval(&tab, &format!("location.href = '/dl/{}'; 1", probe.token))
            .await;
        let files = h.env.paths.session_files(&info.id);
        let saved = files.join(&probe.token);
        let landed = common::eventually(15, || async { saved.exists() }).await;
        assert!(landed, "the download did not reach {files:?}");
        assert_eq!(
            std::fs::read_to_string(&saved).unwrap(),
            "pegboard download test"
        );
        assert!(probe.in_real_downloads().is_empty(), "saved to ~/Downloads");
        // The next result tells about it, once.
        let r = get_url(&h, &mut log, &info).await.unwrap();
        assert_eq!(
            r.output["downloads"],
            json!([{ "name": probe.token, "bytes": 22 }]),
            "{r:?}"
        );
        let r = get_url(&h, &mut log, &info).await.unwrap();
        assert!(r.output.get("downloads").is_none(), "{r:?}");
        // Nothing is left in the profile's staging directory.
        let staging = h.browser.downloads.staging_dir("PegboardAgent");
        assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
        h.finish().await;
    });
}
