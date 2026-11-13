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
