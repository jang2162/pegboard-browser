//! The confirmation broker: a command that asks a person waits here for an answer
//! (design 4.2).
//!
//! The broker holds the table of waiting confirmations, the clock and the race between
//! responders: API subscribers (`confirm.subscribe`, any configuration) and the browser
//! (the fork's `Pegboard` domain). The first answer ends a confirmation; a late one is
//! `confirm_not_found`. A confirmation also ends when its time is up, when its session
//! closes or the daemon stops, and when nobody is left who could answer it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use pegboard_protocol::{
    codes, ActionClass, ConfirmAnswer, ConfirmBy, ConfirmDetail, ConfirmOutcome, ConfirmRequired,
    ConfirmResolved, ConfirmRespondParams, RpcError, SessionId, SessionPolicy,
};
use rand::RngCore;
use tokio::sync::{broadcast, oneshot};
use tokio::time::Instant;

/// How long a person has to answer; then the command is denied.
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);

/// How long closing the browser's window may hold up the end of a confirmation.
const CANCEL_LIMIT: Duration = Duration::from_secs(5);

/// Notices a subscriber may fall behind by before its connection is dropped.
const NOTICE_BACKLOG: usize = 256;

/// `PEGBOARD_TEST_CONFIRM_TIMEOUT_MS` (a test hook, with `PEGBOARD_TEST_HOOKS`), else
/// `CONFIRM_TIMEOUT`.
pub fn confirm_timeout() -> Duration {
    timeout_with(
        crate::testenv::hooks(),
        std::env::var("PEGBOARD_TEST_CONFIRM_TIMEOUT_MS")
            .ok()
            .as_deref(),
    )
}

fn timeout_with(hooks: bool, value: Option<&str>) -> Duration {
    match value
        .filter(|_| hooks)
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        Some(ms) if ms > 0 => Duration::from_millis(ms),
        _ => CONFIRM_TIMEOUT,
    }
}

/// 128 random bits from the OS generator as 32 lowercase hex characters.
fn new_confirm_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    data_encoding::HEXLOWER.encode(&bytes)
}

/// Where the browser shows a confirmation: the session's browser context and tabs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmPlace {
    pub browser_context_id: String,
    pub target_ids: Vec<String>,
}

/// A question for a person about one command.
#[derive(Debug, Clone)]
pub struct ConfirmAsk {
    pub session_id: SessionId,
    pub session_name: String,
    pub profile: String,
    pub class: ActionClass,
    pub details: Vec<ConfirmDetail>,
    /// Whether "allow for the rest of the session" is offered. The broker narrows it: an
    /// upload grant needs the session's upload directory.
    pub offer_allow_for_session: bool,
    /// Where the browser can show it; `None`: the browser is no responder for it.
    pub place: Option<ConfirmPlace>,
}

/// How a question ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmDecision {
    pub confirm_id: String,
    pub outcome: ConfirmOutcome,
    pub by: Option<ConfirmBy>,
    pub waited: Duration,
}

/// The browser as a responder: it shows a window for a confirmation and closes it again.
/// Its answers come back through `ConfirmBroker::respond` with `ConfirmBy::Browser`.
#[async_trait]
pub trait BrowserResponder: Send + Sync {
    /// Shows the window; an error means it cannot (the browser drops out of that
    /// confirmation).
    async fn show(&self, req: &ConfirmRequired, place: &ConfirmPlace) -> anyhow::Result<()>;
    /// Closes the window, if it is still open.
    async fn cancel(&self, confirm_id: &str);
}

/// What subscribers are told (`confirm.required`, `confirm.resolved`).
#[derive(Debug, Clone)]
pub enum ConfirmNotice {
    Required(ConfirmRequired),
    Resolved(ConfirmResolved),
}

/// The policy of an open session (the daemon's session manager); `None` for one it does
/// not know.
pub type PolicyLookup = Arc<dyn Fn(&SessionId) -> Option<SessionPolicy> + Send + Sync>;

/// How a confirmation ended, as sent to its asker.
#[derive(Debug, Clone, Copy)]
struct Ending {
    outcome: ConfirmOutcome,
    by: Option<ConfirmBy>,
    /// The browser still had it (so its window is closed).
    in_browser: bool,
}

struct Entry {
    req: ConfirmRequired,
    seq: u64,
    tx: oneshot::Sender<Ending>,
    /// The browser is one of its responders.
    in_browser: bool,
}

#[derive(Default)]
struct Table {
    pending: HashMap<String, Entry>,
    /// Classes a person allowed for the rest of a session.
    granted: HashMap<SessionId, Vec<ActionClass>>,
    browser: Option<Arc<dyn BrowserResponder>>,
    subscribers: usize,
    /// The daemon is stopping: every question is cancelled.
    closed: bool,
    seq: u64,
    lookup: Option<PolicyLookup>,
}

impl Table {
    /// Ends a waiting confirmation; false when it was not waiting any more.
    fn end(&mut self, id: &str, outcome: ConfirmOutcome, by: Option<ConfirmBy>) -> bool {
        let Some(entry) = self.pending.remove(id) else {
            return false;
        };
        let _ = entry.tx.send(Ending {
            outcome,
            by,
            in_browser: entry.in_browser,
        });
        true
    }

    /// Ends every confirmation nobody can answer any more.
    fn end_unanswerable(&mut self) {
        if self.subscribers > 0 {
            return;
        }
        let orphans: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, e)| !e.in_browser)
            .map(|(id, _)| id.clone())
            .collect();
        for id in orphans {
            self.end(&id, ConfirmOutcome::NoResponder, None);
        }
    }
}

/// Whether a person may allow `class` for the rest of the session. An upload grant makes
/// every later upload of the session go unasked: only a session bound to an upload
/// directory may have one (design 4.3), and one whose policy is unknown may not.
fn grantable(lookup: Option<&PolicyLookup>, session: &SessionId, class: ActionClass) -> bool {
    class != ActionClass::Upload
        || lookup
            .and_then(|f| f(session))
            .is_some_and(|p| p.upload_dir.is_some())
}

fn rfc3339_in(after: Duration) -> String {
    (time::OffsetDateTime::now_utc() + after)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

pub struct ConfirmBroker {
    timeout: Duration,
    table: Mutex<Table>,
    notices: broadcast::Sender<ConfirmNotice>,
    me: Weak<ConfirmBroker>,
}

/// Notices for one subscriber. While it lives the broker counts it as a responder;
/// dropping it ends the confirmations nobody else can answer.
pub struct ConfirmSubscription {
    rx: broadcast::Receiver<ConfirmNotice>,
    broker: Weak<ConfirmBroker>,
}

impl ConfirmSubscription {
    /// The next notice. `Lagged` means notices were lost (the subscriber fell behind).
    pub async fn recv(&mut self) -> Result<ConfirmNotice, broadcast::error::RecvError> {
        self.rx.recv().await
    }

    pub fn try_recv(&mut self) -> Result<ConfirmNotice, broadcast::error::TryRecvError> {
        self.rx.try_recv()
    }
}

impl Drop for ConfirmSubscription {
    fn drop(&mut self) {
        if let Some(broker) = self.broker.upgrade() {
            let mut t = broker.table.lock().unwrap();
            t.subscribers -= 1;
            t.end_unanswerable();
        }
    }
}

/// Ends the confirmation if its asker goes away before it ended (the command's future was
/// dropped), so nothing waits for an answer nobody reads.
struct Abandon<'a> {
    broker: &'a ConfirmBroker,
    id: &'a str,
    browser: Option<Arc<dyn BrowserResponder>>,
    armed: bool,
}

impl Drop for Abandon<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let in_browser = {
            let mut t = self.broker.table.lock().unwrap();
            match t.pending.remove(self.id) {
                Some(entry) => entry.in_browser,
                None => return,
            }
        };
        if let (true, Some(browser), Ok(rt)) = (
            in_browser,
            self.browser.clone(),
            tokio::runtime::Handle::try_current(),
        ) {
            let id = self.id.to_string();
            rt.spawn(async move {
                let _ = tokio::time::timeout(CANCEL_LIMIT, browser.cancel(&id)).await;
            });
        }
        self.broker
            .notify_resolved(self.id, ConfirmOutcome::Cancelled, None);
    }
}

impl ConfirmBroker {
    pub fn new(timeout: Duration) -> Arc<ConfirmBroker> {
        let (notices, _) = broadcast::channel(NOTICE_BACKLOG);
        Arc::new_cyclic(|me| ConfirmBroker {
            timeout,
            table: Mutex::new(Table::default()),
            notices,
            me: me.clone(),
        })
    }

    /// Tells the broker the policies of the open sessions (what may be granted for a
    /// session). Without it no upload is ever granted for a session.
    pub fn set_policy_lookup(&self, lookup: PolicyLookup) {
        self.table.lock().unwrap().lookup = Some(lookup);
    }

    fn notify_resolved(&self, id: &str, outcome: ConfirmOutcome, by: Option<ConfirmBy>) {
        let _ = self.notices.send(ConfirmNotice::Resolved(ConfirmResolved {
            confirm_id: id.to_string(),
            outcome,
            by,
        }));
    }

    /// Asks the responders and waits for the first answer, the time limit, a cancel, or
    /// the last responder leaving. With no responder it returns `NoResponder` at once.
    pub async fn ask(&self, ask: ConfirmAsk) -> ConfirmDecision {
        let started = Instant::now();
        let confirm_id = new_confirm_id();
        let decided = |outcome, by| ConfirmDecision {
            confirm_id: confirm_id.clone(),
            outcome,
            by,
            waited: started.elapsed(),
        };
        // Called outside the table's lock: it reaches into the session manager.
        let lookup = self.table.lock().unwrap().lookup.clone();
        let offer =
            ask.offer_allow_for_session && grantable(lookup.as_ref(), &ask.session_id, ask.class);
        let (tx, mut rx) = oneshot::channel();
        let (browser, req) = {
            let mut t = self.table.lock().unwrap();
            if t.closed {
                return decided(ConfirmOutcome::Cancelled, None);
            }
            let browser = t.browser.clone().filter(|_| ask.place.is_some());
            if browser.is_none() && t.subscribers == 0 {
                return decided(ConfirmOutcome::NoResponder, None);
            }
            let req = ConfirmRequired {
                confirm_id: confirm_id.clone(),
                session_id: ask.session_id.clone(),
                session_name: ask.session_name.clone(),
                profile: ask.profile.clone(),
                class: ask.class,
                details: ask.details.clone(),
                offer_allow_for_session: offer,
                expires_at: rfc3339_in(self.timeout),
            };
            t.seq += 1;
            let seq = t.seq;
            t.pending.insert(
                confirm_id.clone(),
                Entry {
                    req: req.clone(),
                    seq,
                    tx,
                    in_browser: browser.is_some(),
                },
            );
            // Under the lock, so a subscriber either is counted and told, or is neither
            // (and finds it with `confirm.list`).
            let _ = self.notices.send(ConfirmNotice::Required(req.clone()));
            (browser, req)
        };
        let mut abandon = Abandon {
            broker: self,
            id: &confirm_id,
            browser: browser.clone(),
            armed: true,
        };
        let show = async {
            match (&browser, &ask.place) {
                (Some(b), Some(place)) => b.show(&req, place).await,
                _ => Ok(()),
            }
        };
        tokio::pin!(show);
        let mut showing = browser.is_some();
        let deadline = tokio::time::sleep_until(started + self.timeout);
        tokio::pin!(deadline);
        let mut timing = true;
        // Every ending goes through the table, so the first one wins; this loop only
        // reads what it was.
        let ending = loop {
            tokio::select! {
                ending = &mut rx => break ending.unwrap_or(Ending {
                    outcome: ConfirmOutcome::Cancelled,
                    by: None,
                    in_browser: false,
                }),
                shown = &mut show, if showing => {
                    showing = false;
                    if let Err(e) = shown {
                        eprintln!("pegboard: the browser cannot show confirmation {confirm_id}: {e:#}");
                        self.browser_dropped_out(&confirm_id);
                    }
                }
                _ = &mut deadline, if timing => {
                    timing = false;
                    self.table.lock().unwrap().end(&confirm_id, ConfirmOutcome::TimedOut, None);
                }
            }
        };
        abandon.armed = false;
        if ending.in_browser {
            if let Some(b) = &browser {
                let _ = tokio::time::timeout(CANCEL_LIMIT, b.cancel(&confirm_id)).await;
            }
        }
        self.notify_resolved(&confirm_id, ending.outcome, ending.by);
        decided(ending.outcome, ending.by)
    }

    /// The browser could not show the confirmation: it is no responder for it.
    fn browser_dropped_out(&self, id: &str) {
        let mut t = self.table.lock().unwrap();
        let Some(entry) = t.pending.get_mut(id) else {
            return;
        };
        entry.in_browser = false;
        if t.subscribers == 0 {
            t.end(id, ConfirmOutcome::NoResponder, None);
        }
    }

    /// An answer. It ends the one confirmation it names; "allow for the session" adds that
    /// confirmation's class to that confirmation's session, nothing else.
    pub fn respond(&self, p: &ConfirmRespondParams, by: ConfirmBy) -> Result<(), RpcError> {
        let not_found = || RpcError {
            code: codes::CONFIRM_NOT_FOUND.to_string(),
            message: "no confirmation with that id is waiting (it was answered, timed out \
                      or never was)"
                .to_string(),
            data: None,
        };
        let invalid = |message: &str| RpcError {
            code: codes::INVALID_PARAMS.to_string(),
            message: message.to_string(),
            data: None,
        };
        let (session, class, offered, lookup) = {
            let t = self.table.lock().unwrap();
            let entry = t.pending.get(&p.confirm_id).ok_or_else(not_found)?;
            (
                entry.req.session_id.clone(),
                entry.req.class,
                entry.req.offer_allow_for_session,
                t.lookup.clone(),
            )
        };
        let outcome = match p.answer {
            ConfirmAnswer::Approve => ConfirmOutcome::Approved,
            ConfirmAnswer::Deny => ConfirmOutcome::Denied,
            ConfirmAnswer::AllowForSession => {
                if !offered {
                    return Err(invalid(
                        "this confirmation does not offer allowForSession; answer approve or deny",
                    ));
                }
                // Checked again here, against the session's policy, not only by what was
                // offered when asking.
                if !grantable(lookup.as_ref(), &session, class) {
                    return Err(invalid(
                        "uploads can be allowed for the rest of a session only when it has \
                         --upload-dir; answer approve or deny",
                    ));
                }
                ConfirmOutcome::ApprovedForSession
            }
        };
        let mut t = self.table.lock().unwrap();
        if !t.pending.contains_key(&p.confirm_id) {
            return Err(not_found());
        }
        if outcome == ConfirmOutcome::ApprovedForSession {
            let granted = t.granted.entry(session).or_default();
            if !granted.contains(&class) {
                granted.push(class);
            }
        }
        t.end(&p.confirm_id, outcome, Some(by));
        Ok(())
    }

    /// Ends the session's waiting confirmations (`Cancelled`); other sessions' wait on.
    pub fn cancel_session(&self, id: &SessionId) {
        let mut t = self.table.lock().unwrap();
        let ids: Vec<String> = t
            .pending
            .iter()
            .filter(|(_, e)| &e.req.session_id == id)
            .map(|(cid, _)| cid.clone())
            .collect();
        for cid in ids {
            t.end(&cid, ConfirmOutcome::Cancelled, None);
        }
    }

    /// The daemon is stopping: every waiting confirmation is cancelled, and so is every
    /// later question.
    pub fn cancel_all(&self) {
        let mut t = self.table.lock().unwrap();
        t.closed = true;
        let ids: Vec<String> = t.pending.keys().cloned().collect();
        for cid in ids {
            t.end(&cid, ConfirmOutcome::Cancelled, None);
        }
    }

    /// Drops what a person allowed for the session (it has closed).
    pub fn forget_session(&self, id: &SessionId) {
        self.table.lock().unwrap().granted.remove(id);
    }

    /// The classes a person allowed for the rest of the session.
    pub fn granted(&self, id: &SessionId) -> Vec<ActionClass> {
        self.table
            .lock()
            .unwrap()
            .granted
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    /// The waiting confirmations, oldest first.
    pub fn pending(&self) -> Vec<ConfirmRequired> {
        let t = self.table.lock().unwrap();
        let mut list: Vec<&Entry> = t.pending.values().collect();
        list.sort_by_key(|e| e.seq);
        list.into_iter().map(|e| e.req.clone()).collect()
    }

    /// A new subscriber: a responder from now on, told of every confirmation asked and
    /// ended after this call.
    pub fn subscribe(&self) -> ConfirmSubscription {
        let mut t = self.table.lock().unwrap();
        t.subscribers += 1;
        ConfirmSubscription {
            rx: self.notices.subscribe(),
            broker: self.me.clone(),
        }
    }

    /// Sets or clears the browser responder. The waiting confirmations lose the browser
    /// they had (its link is gone); those nobody else can answer end as `NoResponder`.
    pub fn set_browser(&self, responder: Option<Arc<dyn BrowserResponder>>) {
        let mut t = self.table.lock().unwrap();
        t.browser = responder;
        for entry in t.pending.values_mut() {
            entry.in_browser = false;
        }
        t.end_unanswerable();
    }
}

/// The confirmation methods a client calls on an ordinary connection: `confirm.list` and
/// `confirm.respond` (by `ConfirmBy::Api`). `None` for any other method. `confirm.subscribe`
/// turns its connection into a notice channel, so the API server handles it
/// (`api::Handler::confirm_subscription`).
pub fn confirm_method(
    broker: &ConfirmBroker,
    method: &str,
    params: &serde_json::Value,
) -> Option<Result<serde_json::Value, RpcError>> {
    match method {
        "confirm.list" => Some(Ok(serde_json::json!({ "pending": broker.pending() }))),
        "confirm.respond" => Some(
            serde_json::from_value::<ConfirmRespondParams>(params.clone())
                .map_err(|e| RpcError {
                    code: codes::INVALID_PARAMS.to_string(),
                    message: format!("bad params: {e}"),
                    data: None,
                })
                .and_then(|p| broker.respond(&p, ConfirmBy::Api))
                .map(|()| serde_json::json!({})),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
