// Shared browser: every test here except `profile_cannot_be_deleted_while_a_session_uses_it`
// uses the binary's one browser and session manager (own sessions, closed at the end). Own
// browser: that test asserts on the whole profile list and the profile registry on disk.
mod common;

use std::sync::Arc;
use std::time::Duration;

use common::eventually;
use pegboard_core::bridge::{Bridge, GroupId};
use pegboard_core::browser::Browser;
use pegboard_core::cdp::page_targets;
use pegboard_core::profiles::Profiles;
use pegboard_core::proxy::server::{ProxyServer, WsUpstream};
use pegboard_core::session::SessionManager;
use pegboard_protocol::{
    codes, ProfileKind, SessionCloseParams, SessionCreateParams, SessionId, SessionInfo,
    SessionState,
};
use serde_json::{json, Value};

/// The test's view of the shared browser: its sessions (closed by `finish`, which also checks
/// that their tabs are gone) and a page scope that closes every other tab the test opened.
struct T {
    h: common::Harness,
    scope: common::PageScope,
}

impl std::ops::Deref for T {
    type Target = common::Harness;
    fn deref(&self) -> &common::Harness {
        &self.h
    }
}

impl T {
    /// Call inside `common::run`.
    async fn start() -> T {
        let h = common::Harness::start();
        // The agent window exists before the scope's baseline, so closing the test's tabs
        // never closes the window.
        h.agent().await;
        let scope = common::PageScope::begin(&h.browser.conn).await;
        T { h, scope }
    }

    /// A session through the manager, closed by `finish`.
    async fn make(&self, p: SessionCreateParams) -> SessionInfo {
        let info = self.h.manager.create(p).await.unwrap();
        self.h.sessions.lock().unwrap().push(info.id.clone());
        info
    }

    /// Bridge of the agent profile, for the tests' own checks.
    async fn bridge(&self) -> Arc<Bridge> {
        self.h.agent().await.1
    }

    async fn finish(self) {
        let T { h, scope } = self;
        let conn = h.browser.conn.clone();
        h.finish().await;
        scope.finish(&conn).await;
    }
}

/// A launched browser of its own, a proxy and a session manager (for the one test that
/// asserts on the whole profile list). Everything is killed when this is
/// dropped (also on test failure); `shutdown` closes the browser gracefully.
/// Fields drop in declaration order: manager, proxy and browser go before the temp dir.
struct OwnHarness {
    manager: SessionManager,
    proxy: Arc<ProxyServer>,
    /// Bridge of the default agent profile, for the tests' own checks.
    bridge: Arc<Bridge>,
    browser: Arc<Browser>,
    _fixture: common::Fixture,
    env: common::Env,
}

impl OwnHarness {
    async fn start() -> OwnHarness {
        let (env, fixture, browser) = common::launched().await;
        let agent = browser
            .open_profile_window(&env.paths, &env.cfg, "PegboardAgent")
            .await
            .unwrap();
        let bridge = Arc::new(Bridge::attach(&browser.conn, &agent).await.unwrap());
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
        OwnHarness {
            manager,
            proxy,
            bridge,
            browser,
            _fixture: fixture,
            env,
        }
    }

    async fn shutdown(self) {
        let OwnHarness {
            manager,
            proxy,
            bridge,
            browser,
            ..
        } = self;
        drop(manager);
        drop(proxy);
        drop(bridge);
        // Tasks of the manager may hold the browser for a moment longer.
        let mut browser = browser;
        for _ in 0..50 {
            match Arc::try_unwrap(browser) {
                Ok(b) => {
                    b.shutdown().await.unwrap();
                    return;
                }
                Err(b) => browser = b,
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the browser is still referenced after the manager was dropped");
    }
}

fn params(name: Option<&str>, url: Option<String>, profile: Option<&str>) -> SessionCreateParams {
    SessionCreateParams {
        name: name.map(String::from),
        url,
        profile: profile.map(String::from),
    }
}

/// Evaluates on a fresh session of the test's own (browser-level) connection.
async fn eval(b: &Browser, target: &str, expr: &str) -> Value {
    let s = b.conn.attach(target).await.unwrap();
    let v = b.conn.evaluate(&s, expr).await.unwrap();
    let _ = b
        .conn
        .send("Target.detachFromTarget", json!({ "sessionId": s }), None)
        .await;
    v
}

async fn live_targets(b: &Browser) -> Vec<String> {
    page_targets(&b.conn)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.target_id)
        .collect()
}

#[test]
#[ignore]
fn create_opens_group_in_agent_window() {
    common::run(async {
        let h = T::start().await;
        let before = h.manager.list().len();
        let bridge = h.bridge().await;
        let info = h
            .make(params(Some("조사"), Some(h.url("/counter")), None))
            .await;
        assert_eq!(info.targets.len(), 1, "{info:?}");
        assert_eq!(info.state, SessionState::Active);
        assert_eq!(info.name, "조사");
        assert_eq!(info.profile, "default");
        assert_eq!(info.recording_id, format!("r_{}", &info.id.0[2..]));
        let t = &info.targets[0];
        let ctx = common::context_of(&h.browser.conn, t).await;
        assert_ne!(ctx, h.browser.human_context);
        let mut members = bridge.members(&GroupId(info.tab_group_id)).await.unwrap();
        members.sort();
        assert_eq!(members, info.targets);
        let href = eval(&h.browser, t, "location.href").await;
        assert_eq!(href, json!(h.url("/counter")));
        assert_eq!(h.manager.list().len(), before + 1);
        let mine: Vec<SessionInfo> = h
            .manager
            .list()
            .into_iter()
            .filter(|i| i.id == info.id)
            .collect();
        assert_eq!(mine, vec![info.clone()]);
        assert_eq!(h.manager.info(&info.id).unwrap(), info);
        assert_eq!(h.proxy.targets(&info.id), info.targets);
        h.finish().await;
    });
}

#[test]
#[ignore]
fn two_sessions_with_the_same_name_get_separate_groups() {
    common::run(async {
        let h = T::start().await;
        let before = h.manager.list().len();
        let a = h.make(params(Some("dup"), None, None)).await;
        let b = h.make(params(Some("dup"), None, None)).await;
        assert_ne!(a.id, b.id);
        assert_ne!(a.tab_group_id, b.tab_group_id);
        assert_ne!(a.targets, b.targets);
        assert_eq!(h.manager.list().len(), before + 2);
        h.finish().await;
    });
}

#[test]
#[ignore]
fn session_tab_stays_responsive_when_hidden() {
    common::run(async {
        let h = T::start().await;
        let info = h.make(params(None, Some(h.url("/counter")), None)).await;
        // A second session of the test's own puts another tab in the agent window.
        let _other = h.make(params(None, None, None)).await;
        let t = info.targets[0].clone();
        let ctx = common::context_of(&h.browser.conn, &t).await;
        // Bring another tab of the agent window to the front, hiding the session tab.
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
        let s = h.browser.conn.attach(&t).await.unwrap();
        let visibility = h
            .browser
            .conn
            .evaluate(&s, "document.visibilityState")
            .await
            .unwrap();
        assert_eq!(visibility, json!("visible"));
        let before = h
            .browser
            .conn
            .evaluate(&s, "window.__frames")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        let after = h
            .browser
            .conn
            .evaluate(&s, "window.__frames")
            .await
            .unwrap();
        let grown = after.as_i64().unwrap() - before.as_i64().unwrap();
        assert!(grown > 50, "only {grown} frames in 2 s");
        h.finish().await;
    });
}

#[test]
#[ignore]
fn named_profile_is_isolated_from_default() {
    common::run(async {
        let h = T::start().await;
        // A profile of this test's own, so the registry and its cookies start empty.
        let work = format!("work{}", std::process::id());
        h.manager.create_profile(&work).await.unwrap();
        let a = h
            .make(params(Some("a"), Some(h.url("/cookie/set")), None))
            .await;
        let b = h
            .make(params(
                Some("b"),
                Some(h.url("/cookie/get")),
                Some(work.as_str()),
            ))
            .await;
        assert_eq!(b.profile, work);
        let text = eval(
            &h.browser,
            &b.targets[0],
            "document.getElementById('out').textContent",
        )
        .await;
        let seen: Value = serde_json::from_str(text.as_str().unwrap()).unwrap();
        assert_eq!(seen, json!({ "cookie": "", "ls": null }));
        let ctx_a = common::context_of(&h.browser.conn, &a.targets[0]).await;
        let ctx_b = common::context_of(&h.browser.conn, &b.targets[0]).await;
        assert_ne!(ctx_a, ctx_b);
        assert_ne!(ctx_b, h.browser.human_context);
        // The work profile's directory is its own, never the human one.
        assert!(h
            .env
            .paths
            .chrome_udd
            .join(format!("PegboardAgent-{work}"))
            .is_dir());
        // Leave nothing behind in the shared browser: the cookie planted in the default profile,
        // the named profile and its session.
        eval(
            &h.browser,
            &a.targets[0],
            "document.cookie = 'spike=; Max-Age=0; Path=/'; localStorage.removeItem('spike'); 1",
        )
        .await;
        h.close_session(&b.id).await;
        let _ = h.manager.delete_profile(&work).await;
        h.finish().await;
    });
}

#[test]
#[ignore]
fn close_removes_tabs_and_keep_tabs_leaves_them() {
    common::run(async {
        let h = T::start().await;
        let bridge = h.bridge().await;
        let before = h.manager.list().len();
        let a = h.make(params(Some("a"), None, None)).await;
        let b = h.make(params(Some("b"), None, None)).await;
        // A tab in the group that the proxy does not know about is closed too.
        let stray = bridge
            .create_tab(&GroupId(a.tab_group_id), "about:blank", "a", "blue")
            .await
            .unwrap()
            .0;
        let closed = h
            .manager
            .close(SessionCloseParams {
                session_id: a.id.clone(),
                keep_tabs: false,
            })
            .await
            .unwrap();
        assert_eq!(closed.state, SessionState::Closed);
        let gone = eventually(2, || async {
            let live = live_targets(&h.browser).await;
            !live.contains(&a.targets[0]) && !live.contains(&stray)
        })
        .await;
        assert!(gone, "the closed session's tabs are still open");
        assert_eq!(
            h.manager.endpoint(&a.id).unwrap_err().code,
            codes::SESSION_NOT_FOUND
        );
        assert_eq!(
            h.manager.info(&a.id).unwrap_err().code,
            codes::SESSION_NOT_FOUND
        );
        assert!(h.proxy.targets(&a.id).is_empty());

        h.manager
            .close(SessionCloseParams {
                session_id: b.id.clone(),
                keep_tabs: true,
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(live_targets(&h.browser).await.contains(&b.targets[0]));
        let members = bridge.members(&GroupId(b.tab_group_id)).await.unwrap();
        assert!(members.is_empty(), "{members:?}");
        assert_eq!(h.manager.list().len(), before);
        let again = h
            .manager
            .close(SessionCloseParams {
                session_id: b.id.clone(),
                keep_tabs: true,
            })
            .await
            .unwrap_err();
        assert_eq!(again.code, codes::SESSION_NOT_FOUND);
        h.finish().await;
    });
}

#[test]
#[ignore]
fn manually_closed_tab_is_reported_and_recoverable() {
    common::run(async {
        let h = T::start().await;
        let bridge = h.bridge().await;
        let info = h.make(params(None, None, None)).await;
        let t = info.targets[0].clone();
        let ctx = common::context_of(&h.browser.conn, &t).await;
        h.browser
            .conn
            .send("Target.closeTarget", json!({ "targetId": t }), None)
            .await
            .unwrap();
        let emptied = eventually(2, || async {
            h.manager.info(&info.id).unwrap().targets.is_empty()
        })
        .await;
        assert!(emptied, "{:?}", h.manager.info(&info.id));
        assert_eq!(
            h.manager.info(&info.id).unwrap().state,
            SessionState::Active
        );

        let n = h.manager.new_tab(&info.id, &h.url("/form")).await.unwrap();
        assert_eq!(common::context_of(&h.browser.conn, &n).await, ctx);
        // Chrome deleted the old group with its last tab; the session moved to a new one.
        let now = h.manager.info(&info.id).unwrap();
        let members = bridge.members(&GroupId(now.tab_group_id)).await.unwrap();
        assert_eq!(members, vec![n.clone()]);
        assert_eq!(
            eval(&h.browser, &n, "location.pathname").await,
            json!("/form")
        );
        assert_eq!(now.targets, vec![n.clone()]);
        // The proxy creates the session's next tabs in the new group too.
        let c = common::client(&h.manager.endpoint(&info.id).unwrap()).await;
        let r = c
            .send("Target.createTarget", json!({ "url": "about:blank" }), None)
            .await
            .unwrap();
        let created = r["targetId"].as_str().unwrap().to_string();
        let mut members = bridge.members(&GroupId(now.tab_group_id)).await.unwrap();
        members.sort();
        let mut expected = vec![n.clone(), created.clone()];
        expected.sort();
        assert_eq!(members, expected);
        assert_eq!(h.manager.info(&info.id).unwrap().targets, expected);
        let refused = h
            .manager
            .new_tab(&info.id, "file:///etc/passwd")
            .await
            .unwrap_err();
        assert_eq!(refused.code, codes::INVALID_PARAMS);
        let unknown = SessionId("s_aaaaaaaa".into());
        assert_eq!(
            h.manager
                .new_tab(&unknown, "about:blank")
                .await
                .unwrap_err()
                .code,
            codes::SESSION_NOT_FOUND
        );
        h.finish().await;
    });
}

#[test]
#[ignore]
fn session_recovers_after_agent_closes_its_last_tab() {
    common::run(async {
        let h = T::start().await;
        let bridge = h.bridge().await;
        let info = h.make(params(Some("recover"), None, None)).await;
        let first = info.targets[0].clone();
        let ctx = common::context_of(&h.browser.conn, &first).await;
        // The agent's own CDP client closes the session's only tab through the proxy.
        let c = common::client(&h.manager.endpoint(&info.id).unwrap()).await;
        c.send("Target.closeTarget", json!({ "targetId": first }), None)
            .await
            .unwrap();
        let emptied = eventually(2, || async {
            h.manager.info(&info.id).unwrap().targets.is_empty()
        })
        .await;
        assert!(emptied, "{:?}", h.manager.info(&info.id));

        // Through the proxy, as any CDP client would: the first tab creation after that works.
        let r = c
            .send(
                "Target.createTarget",
                json!({ "url": h.url("/form") }),
                None,
            )
            .await
            .unwrap();
        let t = r["targetId"].as_str().unwrap().to_string();
        assert_eq!(common::context_of(&h.browser.conn, &t).await, ctx);
        let now = h.manager.info(&info.id).unwrap();
        assert_ne!(
            now.tab_group_id, info.tab_group_id,
            "still the deleted group"
        );
        assert_eq!(now.targets, vec![t.clone()]);
        let members = bridge.members(&GroupId(now.tab_group_id)).await.unwrap();
        assert_eq!(members, vec![t.clone()]);
        // The one after that joins the same new group.
        let r = c
            .send("Target.createTarget", json!({ "url": "about:blank" }), None)
            .await
            .unwrap();
        let t2 = r["targetId"].as_str().unwrap().to_string();
        let mut members = bridge.members(&GroupId(now.tab_group_id)).await.unwrap();
        members.sort();
        let mut expected = vec![t.clone(), t2.clone()];
        expected.sort();
        assert_eq!(members, expected);
        assert_eq!(h.manager.info(&info.id).unwrap().targets, expected);
        // Closing the session closes both (after focus emulation settled on the new tabs, so the
        // close does not race the manager's own attach to a tab that is about to go).
        tokio::time::sleep(Duration::from_millis(500)).await;
        h.manager
            .close(SessionCloseParams {
                session_id: info.id.clone(),
                keep_tabs: false,
            })
            .await
            .unwrap();
        let gone = eventually(2, || async {
            let live = live_targets(&h.browser).await;
            !live.contains(&t) && !live.contains(&t2)
        })
        .await;
        assert!(gone, "the recovered session's tabs are still open");
        drop(c);
        h.finish().await;
    });
}

#[test]
#[ignore]
fn popup_of_a_session_tab_joins_the_session_without_a_client() {
    common::run(async {
        let h = T::start().await;
        let info = h.make(params(None, Some(h.url("/opener")), None)).await;
        let t = info.targets[0].clone();
        let s = h.browser.conn.attach(&t).await.unwrap();
        h.browser
        .conn
        .send(
            "Runtime.evaluate",
            json!({ "expression": "document.getElementById('open').click()", "userGesture": true }),
            Some(&s),
        )
        .await
        .unwrap();
        let joined = eventually(3, || async {
            h.manager.info(&info.id).unwrap().targets.len() == 2
        })
        .await;
        assert!(joined, "{:?}", h.manager.info(&info.id));
        let popup = h
            .manager
            .info(&info.id)
            .unwrap()
            .targets
            .into_iter()
            .find(|id| *id != t)
            .unwrap();
        // The popup got focus emulation too: it stays visible behind its opener.
        let ready = eventually(3, || async {
            eval(&h.browser, &popup, "document.readyState").await == json!("complete")
        })
        .await;
        assert!(ready);
        h.browser
            .conn
            .send("Target.activateTarget", json!({ "targetId": t }), None)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            eval(&h.browser, &popup, "document.visibilityState").await,
            json!("visible")
        );
        h.finish().await;
    });
}

#[test]
#[ignore]
fn unknown_profile_is_an_error() {
    common::run(async {
        let h = T::start().await;
        let before = h.manager.list().len();
        for name in ["nope", "../x", "Default"] {
            let e = h
                .manager
                .create(params(None, None, Some(name)))
                .await
                .unwrap_err();
            assert_eq!(e.code, codes::PROFILE_NOT_FOUND, "{name}");
        }
        let e = h
            .manager
            .create(params(None, Some("javascript:alert(1)".into()), None))
            .await
            .unwrap_err();
        assert_eq!(e.code, codes::INVALID_PARAMS);
        assert_eq!(h.manager.list().len(), before);
        assert_eq!(
            h.manager.open_profile("nope").await.unwrap_err().code,
            codes::PROFILE_NOT_FOUND
        );
        h.finish().await;
    });
}

/// Clicks the `/opener` page's button in `target` (a user gesture), opening a popup.
async fn click_open(b: &Browser, target: &str) {
    let s = b.conn.attach(target).await.unwrap();
    b.conn
        .send(
            "Runtime.evaluate",
            json!({ "expression": "document.getElementById('open').click()", "userGesture": true }),
            Some(&s),
        )
        .await
        .unwrap();
}

#[test]
#[ignore]
fn popup_of_session_a_is_not_adopted_by_session_b() {
    common::run(async {
        let h = T::start().await;
        let a = h
            .make(params(Some("a"), Some(h.url("/opener")), None))
            .await;
        let b = h
            .make(params(Some("b"), Some(h.url("/counter")), None))
            .await;
        let opener = a.targets[0].clone();
        click_open(&h.browser, &opener).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let mut popup = None;
        while tokio::time::Instant::now() < deadline {
            let in_b = h.manager.info(&b.id).unwrap().targets;
            assert_eq!(in_b, b.targets, "session b changed");
            popup = h
                .manager
                .info(&a.id)
                .unwrap()
                .targets
                .into_iter()
                .find(|t| *t != opener);
            if popup.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let popup = popup.expect("the popup did not join session a within 3 s");
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!h.manager.info(&b.id).unwrap().targets.contains(&popup));
        assert!(!h.proxy.targets(&b.id).contains(&popup));
        h.finish().await;
    });
}

#[test]
#[ignore]
fn popup_in_the_human_profile_is_never_adopted() {
    common::run(async {
        let h = T::start().await;
        let session = h.make(params(None, Some(h.url("/opener")), None)).await;
        let human = page_targets(&h.browser.conn)
            .await
            .unwrap()
            .into_iter()
            .find(|t| t.browser_context_id.as_deref() == Some(h.browser.human_context.as_str()))
            .expect("no human page")
            .target_id;
        let s = h.browser.conn.attach(&human).await.unwrap();
        h.browser
            .conn
            .navigate(&s, &h.url("/opener"))
            .await
            .unwrap();
        click_open(&h.browser, &human).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let popup = loop {
            let found = page_targets(&h.browser.conn)
                .await
                .unwrap()
                .into_iter()
                .find(|t| t.opener_id.as_deref() == Some(human.as_str()));
            if let Some(t) = found {
                break t.target_id;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the human popup did not open"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert_eq!(
            common::context_of(&h.browser.conn, &popup).await,
            h.browser.human_context
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
        for info in h.manager.list() {
            assert!(!info.targets.contains(&popup), "{info:?}");
        }
        let focused = h.manager.focused_targets();
        assert!(!focused.contains(&popup), "{focused:?}");
        assert!(!focused.contains(&human), "{focused:?}");
        // The view does show the session's own tab.
        assert!(focused.contains(&session.targets[0]), "{focused:?}");
        let _ = h.browser.conn.navigate(&s, "about:blank").await;
        h.finish().await;
    });
}

#[tokio::test]
#[ignore]
async fn profile_cannot_be_deleted_while_a_session_uses_it() {
    let h = OwnHarness::start().await;
    let created = h.manager.create_profile("work").await.unwrap();
    assert_eq!(created.name, "work");
    assert_eq!(created.kind, ProfileKind::Named);
    assert!(created.sessions.is_empty());
    let s = h
        .manager
        .create(params(None, None, Some("work")))
        .await
        .unwrap();
    let listed = h.manager.list_profiles().await;
    assert_eq!(
        listed.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        vec!["default", "work"]
    );
    assert_eq!(listed[0].kind, ProfileKind::Default);
    assert!(listed[0].sessions.is_empty());
    assert_eq!(listed[1].sessions, vec![s.id.clone()]);
    assert_eq!(
        h.manager.delete_profile("work").await.unwrap_err().code,
        codes::PROFILE_IN_USE
    );
    assert_eq!(
        h.manager.delete_profile("default").await.unwrap_err().code,
        codes::INVALID_PARAMS
    );
    h.manager
        .close(SessionCloseParams {
            session_id: s.id.clone(),
            keep_tabs: false,
        })
        .await
        .unwrap();
    h.manager.delete_profile("work").await.unwrap();
    assert_eq!(
        h.manager
            .create(params(None, None, Some("work")))
            .await
            .unwrap_err()
            .code,
        codes::PROFILE_NOT_FOUND
    );
    assert_eq!(
        h.manager.delete_profile("work").await.unwrap_err().code,
        codes::PROFILE_NOT_FOUND
    );
    // The registry on disk agrees.
    assert_eq!(
        Profiles::load(&h.env.paths).unwrap().list(),
        vec!["default"]
    );
    h.shutdown().await;
}
