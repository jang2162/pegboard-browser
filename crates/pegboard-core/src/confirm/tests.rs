use std::sync::Mutex as StdMutex;

use pegboard_protocol::SessionPolicy;

use super::*;

/// A browser responder whose `show` succeeds or fails as told, and which records what it
/// was asked.
#[derive(Default)]
struct FakeBrowser {
    fail: bool,
    shown: StdMutex<Vec<String>>,
    cancelled: StdMutex<Vec<String>>,
}

#[async_trait]
impl BrowserResponder for FakeBrowser {
    async fn show(&self, req: &ConfirmRequired, _place: &ConfirmPlace) -> anyhow::Result<()> {
        self.shown.lock().unwrap().push(req.confirm_id.clone());
        if self.fail {
            anyhow::bail!("no window for that profile");
        }
        Ok(())
    }

    async fn cancel(&self, confirm_id: &str) {
        self.cancelled.lock().unwrap().push(confirm_id.to_string());
    }
}

fn sid(s: &str) -> SessionId {
    SessionId(format!("s_{s}"))
}

const A: &str = "aaaaaaaa";
const B: &str = "bbbbbbbb";

fn place() -> ConfirmPlace {
    ConfirmPlace {
        browser_context_id: "C1".into(),
        target_ids: vec!["T1".into()],
    }
}

fn asking(session: &str, class: ActionClass, with_place: bool) -> ConfirmAsk {
    ConfirmAsk {
        session_id: sid(session),
        session_name: format!("name-{session}"),
        profile: "default".into(),
        class,
        details: vec![ConfirmDetail {
            label: "key".into(),
            value: "value".into(),
        }],
        offer_allow_for_session: true,
        place: with_place.then(place),
    }
}

fn broker() -> Arc<ConfirmBroker> {
    ConfirmBroker::new(CONFIRM_TIMEOUT)
}

fn answer(id: &str, answer: ConfirmAnswer) -> ConfirmRespondParams {
    ConfirmRespondParams {
        confirm_id: id.to_string(),
        answer,
    }
}

/// Asks in a task of its own and returns once the confirmation is in the table.
async fn ask_in_background(
    broker: &Arc<ConfirmBroker>,
    ask: ConfirmAsk,
) -> (String, tokio::task::JoinHandle<ConfirmDecision>) {
    let before: Vec<String> = broker.pending().into_iter().map(|r| r.confirm_id).collect();
    let b = broker.clone();
    let handle = tokio::spawn(async move { b.ask(ask).await });
    loop {
        tokio::task::yield_now().await;
        if let Some(r) = broker
            .pending()
            .into_iter()
            .find(|r| !before.contains(&r.confirm_id))
        {
            return (r.confirm_id, handle);
        }
        assert!(!handle.is_finished(), "the ask ended without waiting");
    }
}

/// Every notice the subscription has now.
fn drain(sub: &mut ConfirmSubscription) -> Vec<ConfirmNotice> {
    let mut out = Vec::new();
    while let Ok(n) = sub.try_recv() {
        out.push(n);
    }
    out
}

#[tokio::test(start_paused = true)]
async fn no_responder_denies_at_once() {
    let b = broker();
    let started = tokio::time::Instant::now();
    let d = b.ask(asking(A, ActionClass::State, false)).await;
    assert_eq!(d.outcome, ConfirmOutcome::NoResponder);
    assert_eq!(d.by, None);
    assert!(d.waited < Duration::from_millis(10), "{:?}", d.waited);
    assert!(started.elapsed() < Duration::from_millis(10));
    assert!(b.pending().is_empty());
    // A browser is no responder for a confirmation it has no place for.
    b.set_browser(Some(Arc::new(FakeBrowser::default())));
    let d = b.ask(asking(A, ActionClass::State, false)).await;
    assert_eq!(d.outcome, ConfirmOutcome::NoResponder);
}

#[tokio::test(start_paused = true)]
async fn first_answer_wins_and_a_late_one_is_not_found() {
    let b = broker();
    let _sub = b.subscribe();
    let (id, handle) = ask_in_background(&b, asking(A, ActionClass::State, false)).await;
    b.respond(&answer(&id, ConfirmAnswer::Approve), ConfirmBy::Api)
        .unwrap();
    let e = b
        .respond(&answer(&id, ConfirmAnswer::Deny), ConfirmBy::Browser)
        .unwrap_err();
    assert_eq!(e.code, codes::CONFIRM_NOT_FOUND);
    let d = handle.await.unwrap();
    assert_eq!(d.confirm_id, id);
    assert_eq!(d.outcome, ConfirmOutcome::Approved);
    assert_eq!(d.by, Some(ConfirmBy::Api));
    // After it ended, too.
    let e = b
        .respond(&answer(&id, ConfirmAnswer::Approve), ConfirmBy::Api)
        .unwrap_err();
    assert_eq!(e.code, codes::CONFIRM_NOT_FOUND);
}

#[tokio::test(start_paused = true)]
async fn an_answer_only_resolves_its_own_confirmation() {
    let b = broker();
    let _sub = b.subscribe();
    let (a, ha) = ask_in_background(&b, asking(A, ActionClass::State, false)).await;
    let (bid, hb) = ask_in_background(&b, asking(B, ActionClass::State, false)).await;
    b.respond(&answer(&a, ConfirmAnswer::Approve), ConfirmBy::Api)
        .unwrap();
    assert_eq!(ha.await.unwrap().outcome, ConfirmOutcome::Approved);
    let pending = b.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].confirm_id, bid);
    assert_eq!(pending[0].session_id, sid(B));
    assert!(!hb.is_finished());
    b.respond(&answer(&bid, ConfirmAnswer::Deny), ConfirmBy::Api)
        .unwrap();
    assert_eq!(hb.await.unwrap().outcome, ConfirmOutcome::Denied);
}

#[tokio::test(start_paused = true)]
async fn allow_for_session_grants_that_session_and_class_only() {
    let b = broker();
    let _sub = b.subscribe();
    let (a, ha) = ask_in_background(&b, asking(A, ActionClass::State, false)).await;
    let (bid, hb) = ask_in_background(&b, asking(B, ActionClass::State, false)).await;
    b.respond(&answer(&a, ConfirmAnswer::AllowForSession), ConfirmBy::Api)
        .unwrap();
    let d = ha.await.unwrap();
    assert_eq!(d.outcome, ConfirmOutcome::ApprovedForSession);
    assert_eq!(d.by, Some(ConfirmBy::Api));
    assert_eq!(b.granted(&sid(A)), vec![ActionClass::State]);
    assert_eq!(b.granted(&sid(B)), Vec::<ActionClass>::new());
    // B still waits.
    assert!(!hb.is_finished());
    assert_eq!(b.pending()[0].confirm_id, bid);
    // A second grant of the same class adds nothing; forgetting the session drops it.
    let (a2, ha2) = ask_in_background(&b, asking(A, ActionClass::State, false)).await;
    b.respond(&answer(&a2, ConfirmAnswer::AllowForSession), ConfirmBy::Api)
        .unwrap();
    ha2.await.unwrap();
    assert_eq!(b.granted(&sid(A)), vec![ActionClass::State]);
    b.forget_session(&sid(A));
    assert!(b.granted(&sid(A)).is_empty());
    b.respond(&answer(&bid, ConfirmAnswer::Deny), ConfirmBy::Api)
        .unwrap();
    hb.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn allow_for_session_needs_the_offer() {
    let b = broker();
    let _sub = b.subscribe();
    let ask = ConfirmAsk {
        offer_allow_for_session: false,
        ..asking(A, ActionClass::State, false)
    };
    let (id, handle) = ask_in_background(&b, ask).await;
    assert!(!b.pending()[0].offer_allow_for_session);
    let e = b
        .respond(&answer(&id, ConfirmAnswer::AllowForSession), ConfirmBy::Api)
        .unwrap_err();
    assert_eq!(e.code, codes::INVALID_PARAMS);
    assert_eq!(b.pending().len(), 1, "still waiting");
    assert!(b.granted(&sid(A)).is_empty());
    b.respond(&answer(&id, ConfirmAnswer::Approve), ConfirmBy::Api)
        .unwrap();
    assert_eq!(handle.await.unwrap().outcome, ConfirmOutcome::Approved);
}

/// An upload allowed for the rest of a session lets every later upload through unasked;
/// without the session's upload directory that would be any file, so the broker itself
/// refuses it, whatever the asker offered (design 4.3).
#[tokio::test(start_paused = true)]
async fn an_upload_grant_needs_the_sessions_upload_dir() {
    let b = broker();
    let _sub = b.subscribe();
    let dirs: Arc<StdMutex<HashMap<SessionId, Option<String>>>> = Default::default();
    {
        let dirs = dirs.clone();
        b.set_policy_lookup(Arc::new(move |id: &SessionId| {
            let dir = dirs.lock().unwrap().get(id).cloned()?;
            Some(SessionPolicy {
                upload_dir: dir,
                ..SessionPolicy::default()
            })
        }));
    }
    dirs.lock().unwrap().insert(sid(A), None);
    dirs.lock().unwrap().insert(sid(B), Some("/tmp/up".into()));

    // The asker (wrongly) offers it for a session without an upload directory: the
    // confirmation does not show the offer, and the answer is refused.
    let (id, handle) = ask_in_background(&b, asking(A, ActionClass::Upload, false)).await;
    assert!(!b.pending()[0].offer_allow_for_session);
    let e = b
        .respond(&answer(&id, ConfirmAnswer::AllowForSession), ConfirmBy::Api)
        .unwrap_err();
    assert_eq!(e.code, codes::INVALID_PARAMS);
    assert!(b.granted(&sid(A)).is_empty());
    b.respond(&answer(&id, ConfirmAnswer::Deny), ConfirmBy::Api)
        .unwrap();
    handle.await.unwrap();

    // `respond` checks the session's policy itself, not only the offer made when asking.
    let (id, handle) = ask_in_background(&b, asking(B, ActionClass::Upload, false)).await;
    assert!(b.pending()[0].offer_allow_for_session);
    dirs.lock().unwrap().insert(sid(B), None);
    let e = b
        .respond(&answer(&id, ConfirmAnswer::AllowForSession), ConfirmBy::Api)
        .unwrap_err();
    assert_eq!(e.code, codes::INVALID_PARAMS);
    assert!(b.granted(&sid(B)).is_empty());
    assert_eq!(b.pending().len(), 1, "still waiting");
    // With the directory, the grant goes through.
    dirs.lock().unwrap().insert(sid(B), Some("/tmp/up".into()));
    b.respond(&answer(&id, ConfirmAnswer::AllowForSession), ConfirmBy::Api)
        .unwrap();
    assert_eq!(
        handle.await.unwrap().outcome,
        ConfirmOutcome::ApprovedForSession
    );
    assert_eq!(b.granted(&sid(B)), vec![ActionClass::Upload]);

    // A session the lookup does not know gets no upload grant either.
    let (id, handle) = ask_in_background(&b, asking("cccccccc", ActionClass::Upload, false)).await;
    assert!(!b.pending()[0].offer_allow_for_session);
    let e = b
        .respond(&answer(&id, ConfirmAnswer::AllowForSession), ConfirmBy::Api)
        .unwrap_err();
    assert_eq!(e.code, codes::INVALID_PARAMS);
    b.cancel_all();
    handle.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_upload_grant_is_refused_without_a_policy_lookup() {
    let b = broker();
    let _sub = b.subscribe();
    let (id, handle) = ask_in_background(&b, asking(A, ActionClass::Upload, false)).await;
    assert!(!b.pending()[0].offer_allow_for_session);
    let e = b
        .respond(&answer(&id, ConfirmAnswer::AllowForSession), ConfirmBy::Api)
        .unwrap_err();
    assert_eq!(e.code, codes::INVALID_PARAMS);
    b.cancel_session(&sid(A));
    assert_eq!(handle.await.unwrap().outcome, ConfirmOutcome::Cancelled);
}

#[tokio::test(start_paused = true)]
async fn timeout_denies() {
    let b = broker();
    let browser = Arc::new(FakeBrowser::default());
    b.set_browser(Some(browser.clone()));
    let mut sub = b.subscribe();
    let (id, handle) = ask_in_background(&b, asking(A, ActionClass::State, true)).await;
    tokio::time::advance(Duration::from_secs(59)).await;
    tokio::task::yield_now().await;
    assert!(!handle.is_finished(), "ended before the time limit");
    tokio::time::advance(Duration::from_secs(2)).await;
    let d = handle.await.unwrap();
    assert_eq!(d.outcome, ConfirmOutcome::TimedOut);
    assert_eq!(d.by, None);
    assert!(d.waited >= CONFIRM_TIMEOUT, "{:?}", d.waited);
    assert_eq!(*browser.shown.lock().unwrap(), vec![id.clone()]);
    assert_eq!(*browser.cancelled.lock().unwrap(), vec![id.clone()]);
    let notices = drain(&mut sub);
    assert!(
        matches!(&notices[..], [ConfirmNotice::Required(r), ConfirmNotice::Resolved(e)]
            if r.confirm_id == id && e.confirm_id == id
                && e.outcome == ConfirmOutcome::TimedOut && e.by.is_none()),
        "{notices:?}"
    );
    assert!(b.pending().is_empty());
}

#[tokio::test(start_paused = true)]
async fn cancel_session_ends_its_wait_and_no_other() {
    let b = broker();
    let _sub = b.subscribe();
    let (_, ha) = ask_in_background(&b, asking(A, ActionClass::State, false)).await;
    let (bid, hb) = ask_in_background(&b, asking(B, ActionClass::Upload, false)).await;
    b.cancel_session(&sid(A));
    assert_eq!(ha.await.unwrap().outcome, ConfirmOutcome::Cancelled);
    assert_eq!(b.pending().len(), 1);
    assert_eq!(b.pending()[0].confirm_id, bid);
    assert!(!hb.is_finished());
    // The session can still be asked later (its close may fail and be retried).
    let (a2, ha2) = ask_in_background(&b, asking(A, ActionClass::State, false)).await;
    b.respond(&answer(&a2, ConfirmAnswer::Approve), ConfirmBy::Api)
        .unwrap();
    assert_eq!(ha2.await.unwrap().outcome, ConfirmOutcome::Approved);
    b.respond(&answer(&bid, ConfirmAnswer::Approve), ConfirmBy::Api)
        .unwrap();
    hb.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancel_all_ends_every_wait() {
    let b = broker();
    let _sub = b.subscribe();
    let (_, ha) = ask_in_background(&b, asking(A, ActionClass::State, false)).await;
    let (_, hb) = ask_in_background(&b, asking(B, ActionClass::Upload, false)).await;
    b.cancel_all();
    assert_eq!(ha.await.unwrap().outcome, ConfirmOutcome::Cancelled);
    assert_eq!(hb.await.unwrap().outcome, ConfirmOutcome::Cancelled);
    assert!(b.pending().is_empty());
    // The daemon is stopping: a later question is cancelled at once.
    let d = b.ask(asking(A, ActionClass::State, false)).await;
    assert_eq!(d.outcome, ConfirmOutcome::Cancelled);
}

#[tokio::test(start_paused = true)]
async fn a_browser_that_cannot_show_drops_out() {
    let b = broker();
    let browser = Arc::new(FakeBrowser {
        fail: true,
        ..FakeBrowser::default()
    });
    b.set_browser(Some(browser.clone()));
    // Nobody else: no responder, at once.
    let d = b.ask(asking(A, ActionClass::State, true)).await;
    assert_eq!(d.outcome, ConfirmOutcome::NoResponder);
    assert_eq!(browser.shown.lock().unwrap().len(), 1);
    // No window was shown, so there is none to close.
    assert!(browser.cancelled.lock().unwrap().is_empty());
    // With a subscriber, the confirmation waits for it.
    let _sub = b.subscribe();
    let (id, handle) = ask_in_background(&b, asking(A, ActionClass::State, true)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(!handle.is_finished());
    assert_eq!(b.pending()[0].confirm_id, id);
    b.respond(&answer(&id, ConfirmAnswer::Approve), ConfirmBy::Api)
        .unwrap();
    assert_eq!(handle.await.unwrap().outcome, ConfirmOutcome::Approved);
    assert!(browser.cancelled.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn losing_every_responder_ends_the_wait() {
    let b = broker();
    b.set_browser(Some(Arc::new(FakeBrowser::default())));
    let (_, handle) = ask_in_background(&b, asking(A, ActionClass::State, true)).await;
    b.set_browser(None);
    let d = handle.await.unwrap();
    assert_eq!(d.outcome, ConfirmOutcome::NoResponder);
    assert!(d.waited < CONFIRM_TIMEOUT);
    // The last subscriber going away ends a wait nobody else can answer.
    let sub = b.subscribe();
    let (_, handle) = ask_in_background(&b, asking(A, ActionClass::State, false)).await;
    drop(sub);
    assert_eq!(handle.await.unwrap().outcome, ConfirmOutcome::NoResponder);
    // But not while another subscriber is there.
    let (s1, s2) = (b.subscribe(), b.subscribe());
    let (id, handle) = ask_in_background(&b, asking(A, ActionClass::State, false)).await;
    drop(s1);
    tokio::task::yield_now().await;
    assert!(!handle.is_finished());
    b.respond(&answer(&id, ConfirmAnswer::Deny), ConfirmBy::Api)
        .unwrap();
    assert_eq!(handle.await.unwrap().outcome, ConfirmOutcome::Denied);
    drop(s2);
}

#[tokio::test(start_paused = true)]
async fn resolution_cancels_the_browser_window() {
    let b = broker();
    let browser = Arc::new(FakeBrowser::default());
    b.set_browser(Some(browser.clone()));
    let mut sub = b.subscribe();
    let (id, handle) = ask_in_background(&b, asking(A, ActionClass::State, true)).await;
    b.respond(&answer(&id, ConfirmAnswer::Approve), ConfirmBy::Api)
        .unwrap();
    let d = handle.await.unwrap();
    assert_eq!(
        (d.outcome, d.by),
        (ConfirmOutcome::Approved, Some(ConfirmBy::Api))
    );
    assert_eq!(*browser.cancelled.lock().unwrap(), vec![id.clone()]);
    // And when the browser itself answered.
    let (id2, handle) = ask_in_background(&b, asking(A, ActionClass::State, true)).await;
    b.respond(&answer(&id2, ConfirmAnswer::Deny), ConfirmBy::Browser)
        .unwrap();
    let d = handle.await.unwrap();
    assert_eq!(
        (d.outcome, d.by),
        (ConfirmOutcome::Denied, Some(ConfirmBy::Browser))
    );
    assert_eq!(
        *browser.cancelled.lock().unwrap(),
        vec![id.clone(), id2.clone()]
    );
    let resolved: Vec<ConfirmResolved> = drain(&mut sub)
        .into_iter()
        .filter_map(|n| match n {
            ConfirmNotice::Resolved(r) => Some(r),
            ConfirmNotice::Required(_) => None,
        })
        .collect();
    assert_eq!(
        resolved,
        vec![
            ConfirmResolved {
                confirm_id: id,
                outcome: ConfirmOutcome::Approved,
                by: Some(ConfirmBy::Api)
            },
            ConfirmResolved {
                confirm_id: id2,
                outcome: ConfirmOutcome::Denied,
                by: Some(ConfirmBy::Browser)
            },
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn the_required_notice_carries_what_is_shown() {
    let b = broker();
    let mut sub = b.subscribe();
    let (id, handle) = ask_in_background(&b, asking(A, ActionClass::State, false)).await;
    let notices = drain(&mut sub);
    let [ConfirmNotice::Required(r)] = &notices[..] else {
        panic!("{notices:?}")
    };
    assert_eq!(r.confirm_id, id);
    assert_eq!(r.session_id, sid(A));
    assert_eq!(r.session_name, "name-aaaaaaaa");
    assert_eq!(r.class, ActionClass::State);
    assert_eq!(r.details.len(), 1);
    assert!(r.offer_allow_for_session);
    let expires = time::OffsetDateTime::parse(
        &r.expires_at,
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let ahead = expires - time::OffsetDateTime::now_utc();
    assert!(
        ahead > time::Duration::seconds(55) && ahead <= time::Duration::seconds(61),
        "{ahead}"
    );
    assert_eq!(b.pending(), vec![r.clone()]);
    b.cancel_all();
    handle.await.unwrap();
}

#[test]
fn confirm_ids_are_32_lowercase_hex() {
    let a = new_confirm_id();
    assert_eq!(a.len(), 32);
    assert!(
        a.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "{a}"
    );
    assert_ne!(a, new_confirm_id());
}

#[test]
fn the_timeout_hook_needs_test_hooks() {
    assert_eq!(CONFIRM_TIMEOUT, Duration::from_secs(60));
    assert_eq!(timeout_with(false, Some("2000")), CONFIRM_TIMEOUT);
    assert_eq!(
        timeout_with(true, Some("2000")),
        Duration::from_millis(2000)
    );
    assert_eq!(timeout_with(true, None), CONFIRM_TIMEOUT);
    for bad in ["", "0", "-1", "soon"] {
        assert_eq!(timeout_with(true, Some(bad)), CONFIRM_TIMEOUT, "{bad}");
    }
}
