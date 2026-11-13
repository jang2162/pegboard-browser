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
                assert!(r.is_err(), "{method} (session {sid:?}) was accepted: {r:?}");
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
