//! What happens in a session's tabs besides its commands: the navigation log, the marks of
//! a possible human intervention, and the optional network and console lines.
//!
//! Spike F6 found no navigation event or extension signal that tells a person from the
//! agent or from a page script. So the rule is one of time: something that happens in a
//! session tab outside a command window of that session (a command's start to its end plus
//! `GRACE`) is a candidate, and none is ever certain (`certain: false`, "possible
//! intervention"). Page-script navigations the browser does tell apart (a request with the
//! reason `scriptInitiated`, `metaTagRefresh`, ...) are never marks.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::actionlog::url_for_log;
use crate::recording::format::{ConsoleLine, NavEvent, NavLine, NetworkLine};

/// How long after a command ends its navigations still count as the command's.
pub const GRACE: Duration = Duration::from_secs(1);
/// Past windows kept (for signals that arrive late, such as tab switches the bridge
/// replays after its worker restarted).
const KEPT_WINDOWS: usize = 64;
/// Longest console text kept (in characters).
pub const MAX_CONSOLE_CHARS: usize = 1000;
/// Requests followed at once per tab; more are not logged.
const MAX_OPEN_REQUESTS: usize = 1000;

/// A session's command windows.
#[derive(Debug, Default)]
pub struct CommandWindows {
    running: Option<Instant>,
    past: VecDeque<(Instant, Instant)>,
}

impl CommandWindows {
    /// A command starts (a nested start keeps the first).
    pub fn begin(&mut self, now: Instant) {
        self.running.get_or_insert(now);
    }

    /// The command ended. Without a running command this is a window of no length, whose
    /// grace still counts (the end of `session create`).
    pub fn end(&mut self, now: Instant) {
        let start = self.running.take().unwrap_or(now);
        self.past.push_back((start, now));
        while self.past.len() > KEPT_WINDOWS {
            self.past.pop_front();
        }
    }

    /// Whether `at` is in a command window: a command was running then (or still is), or
    /// had ended less than `GRACE` before.
    pub fn covers(&self, at: Instant) -> bool {
        self.running.is_some_and(|start| at >= start)
            || self
                .past
                .iter()
                .any(|(start, end)| at >= *start && at <= *end + GRACE)
    }
}

/// Something that happened in a session tab.
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    /// A main-frame navigation committed; `requested` is the reason of the renderer's request
    /// (Page.frameRequestedNavigation) that preceded it, None when the browser started it.
    Navigated {
        url: String,
        requested: Option<String>,
    },
    /// The tab became the active tab of its window (the bridge's `chrome.tabs.onActivated`).
    Activated,
}

/// Request reasons of a page's own doing that the browser tells apart (F6): never a person.
const PAGE_REASONS: &[&str] = &[
    "scriptInitiated",
    "metaTagRefresh",
    "httpHeaderRefresh",
    "reload",
    "initialFrameNavigation",
];

/// Request reasons of a link or a form (a person's click looks the same as a script's
/// `a.click()`, F6).
const LINK_REASONS: &[&str] = &["anchorClick", "formSubmissionGet", "formSubmissionPost"];

/// None: not a human intervention. Some((what, certain)); by F6 never certain.
pub fn classify(
    signal: &Signal,
    in_command: bool,
    expected_activation: bool,
) -> Option<(String, bool)> {
    if in_command {
        return None;
    }
    match signal {
        Signal::Navigated { url, requested } => match requested.as_deref() {
            Some(r) if PAGE_REASONS.contains(&r) => None,
            Some(r) if LINK_REASONS.contains(&r) => Some((
                "followed a link or form outside a command".to_string(),
                false,
            )),
            _ => Some((
                format!("navigated outside a command: {}", url_for_log(url)),
                false,
            )),
        },
        Signal::Activated if expected_activation => None,
        Signal::Activated => Some(("switched to this tab".to_string(), false)),
    }
}

pub fn nav_line(
    ts: &str,
    target_id: &str,
    event: NavEvent,
    url: Option<&str>,
    opener: Option<&str>,
) -> NavLine {
    NavLine {
        ts: ts.to_string(),
        target_id: target_id.to_string(),
        event,
        url: url.map(url_for_log),
        opener: opener.map(String::from),
    }
}

/// What the recording follows of a tab's main-frame navigations between the request and
/// the commit: who asked for the navigation under way, and whether the tab is new.
#[derive(Debug, Default)]
pub struct NavFollow {
    /// The page's last request to navigate this tab: (url, reason).
    requested: Option<(String, String)>,
    /// The reason of the navigation under way, when the page asked for it.
    underway: Option<String>,
    /// The tab joined the recording before its first page committed: that commit is its
    /// opening, not a navigation (a popup's first page, `tab new`'s page).
    fresh: bool,
    /// The page the navigation log last named for the tab.
    logged: Option<String>,
}

impl NavFollow {
    pub fn new(fresh: bool) -> NavFollow {
        NavFollow {
            fresh,
            ..NavFollow::default()
        }
    }

    /// `Page.frameRequestedNavigation` of the main frame. Only a request for this tab
    /// counts (not one that opens a new window).
    pub fn requested(&mut self, url: &str, reason: &str, disposition: &str) {
        if disposition == "currentTab" {
            self.requested = Some((url.to_string(), reason.to_string()));
        }
    }

    /// `Page.frameStartedNavigating` of the main frame: the request it carries out, if any.
    pub fn started(&mut self, url: &str) {
        self.underway = self
            .requested
            .take()
            .filter(|(u, _)| u == url)
            .map(|(_, reason)| reason);
    }

    /// Its first page has not committed yet.
    pub fn is_fresh(&self) -> bool {
        self.fresh
    }

    /// The tab was found on a page (an attach): its first page has committed.
    pub fn seen_on(&mut self, url: &str) {
        if !is_blank(url) {
            self.fresh = false;
        }
    }

    /// Whether the navigation log has yet to name `url` as the tab's page (an attach found
    /// the tab on a page whose commit it did not see); from now on it has.
    pub fn unlogged(&mut self, url: &str) -> bool {
        if is_blank(url) || self.logged.as_deref() == Some(url) {
            return false;
        }
        self.logged = Some(url.to_string());
        true
    }

    /// A main-frame commit of `url`: the signal, or None for the tab's first page.
    pub fn committed(&mut self, url: &str) -> Option<Signal> {
        self.logged = Some(url.to_string());
        let requested = self.underway.take().or_else(|| {
            self.requested
                .take()
                .filter(|(u, _)| u == url)
                .map(|(_, reason)| reason)
        });
        self.requested = None;
        if self.fresh {
            if !is_blank(url) {
                self.fresh = false;
            }
            return None;
        }
        Some(Signal::Navigated {
            url: url.to_string(),
            requested,
        })
    }
}

fn is_blank(url: &str) -> bool {
    url.is_empty() || url == "about:blank"
}

/// A request being followed for the network log.
#[derive(Debug)]
struct Request {
    method: String,
    url: String,
    /// The browser's monotonic clock, in seconds.
    started: f64,
    status: Option<u16>,
}

/// A tab's requests from `Network.requestWillBeSent` to `loadingFinished`/`loadingFailed`.
/// Only metadata is kept: method, URL (`url_for_log`), status, size, time, error. Never a
/// header or a body.
#[derive(Debug, Default)]
pub struct Requests {
    open: HashMap<String, Request>,
}

impl Requests {
    /// A `Network.*` event of the tab: the line it completes, if any (a redirect completes
    /// the hop before it).
    pub fn event(
        &mut self,
        method: &str,
        p: &Value,
        ts: &str,
        target: &str,
    ) -> Option<NetworkLine> {
        let id = p["requestId"].as_str()?;
        let time = p["timestamp"].as_f64().unwrap_or_default();
        match method {
            "Network.requestWillBeSent" => {
                let hop = p.get("redirectResponse").and_then(|r| {
                    let mut req = self.open.remove(id)?;
                    req.status = status(r);
                    Some(line(req, ts, target, time, None, None))
                });
                if self.open.len() < MAX_OPEN_REQUESTS {
                    self.open.insert(
                        id.to_string(),
                        Request {
                            method: p["request"]["method"].as_str().unwrap_or("").to_string(),
                            url: url_for_log(p["request"]["url"].as_str().unwrap_or("")),
                            started: time,
                            status: None,
                        },
                    );
                }
                hop
            }
            "Network.responseReceived" => {
                if let Some(req) = self.open.get_mut(id) {
                    req.status = status(&p["response"]);
                }
                None
            }
            "Network.loadingFinished" => {
                let req = self.open.remove(id)?;
                let bytes = p["encodedDataLength"].as_f64().map(|b| b.max(0.0) as u64);
                Some(line(req, ts, target, time, bytes, None))
            }
            "Network.loadingFailed" => {
                let req = self.open.remove(id)?;
                let error = p["errorText"].as_str().unwrap_or("failed").to_string();
                Some(line(req, ts, target, time, None, Some(error)))
            }
            _ => None,
        }
    }
}

fn status(response: &Value) -> Option<u16> {
    response["status"]
        .as_u64()
        .and_then(|s| u16::try_from(s).ok())
}

fn line(
    req: Request,
    ts: &str,
    target: &str,
    ended: f64,
    bytes: Option<u64>,
    error: Option<String>,
) -> NetworkLine {
    let duration = ended - req.started;
    NetworkLine {
        ts: ts.to_string(),
        target_id: target.to_string(),
        method: req.method,
        url: req.url,
        status: req.status,
        bytes,
        duration_ms: (req.started > 0.0 && duration >= 0.0)
            .then_some((duration * 1000.0).round() as u64),
        error,
    }
}

/// A console line from `Runtime.consoleAPICalled` (level: the call's type) or
/// `Runtime.exceptionThrown` (level `exception`); the text cut to `MAX_CONSOLE_CHARS`.
pub fn console_line(method: &str, p: &Value, ts: &str, target: &str) -> Option<ConsoleLine> {
    let (level, text) = match method {
        "Runtime.consoleAPICalled" => {
            let words: Vec<String> = p["args"]
                .as_array()
                .into_iter()
                .flatten()
                .map(remote_text)
                .collect();
            (
                p["type"].as_str().unwrap_or("log").to_string(),
                words.join(" "),
            )
        }
        "Runtime.exceptionThrown" => {
            let d = &p["exceptionDetails"];
            let text = d["exception"]["description"]
                .as_str()
                .or_else(|| d["text"].as_str())
                .unwrap_or("")
                .to_string();
            ("exception".to_string(), text)
        }
        _ => return None,
    };
    // Cut (with room for URLs that shrink), URLs cleaned, cut again.
    let text: String = text.chars().take(4 * MAX_CONSOLE_CHARS).collect();
    Some(ConsoleLine {
        ts: ts.to_string(),
        target_id: target.to_string(),
        level,
        text: scrub_urls(&text).chars().take(MAX_CONSOLE_CHARS).collect(),
    })
}

/// Where a URL may start in page text.
const URL_STARTS: &[&str] = &["https://", "http://", "wss://", "ws://", "blob:", "data:"];

/// `text` with every URL in it (http, https, ws, wss, blob, data) as logged URLs are
/// (`url_for_log`: no userinfo, query or fragment; `data:` only). A stack trace's
/// `:line:column` after a URL stays.
pub fn scrub_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let lower = rest.to_ascii_lowercase();
        let Some(at) = URL_STARTS.iter().filter_map(|s| lower.find(s)).min() else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let end = tail
            .find(|c: char| c.is_whitespace() || "\"'<>()[]{}`".contains(c))
            .unwrap_or(tail.len());
        let token = &tail[..end];
        let (url, position) = split_position(token);
        out.push_str(&url_for_log(url));
        out.push_str(position);
        rest = &tail[end..];
    }
}

/// A token's `:line:column` suffix (stack traces), split off.
fn split_position(token: &str) -> (&str, &str) {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let mut parts = token.rsplitn(3, ':');
    if let (Some(col), Some(line), Some(url)) = (parts.next(), parts.next(), parts.next()) {
        if digits(col) && digits(line) && url.contains("://") {
            return (url, &token[url.len()..]);
        }
    }
    (token, "")
}

/// How a console argument reads (`Runtime.RemoteObject`).
fn remote_text(o: &Value) -> String {
    match o.get("value") {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => o["description"]
            .as_str()
            .or_else(|| o["unserializableValue"].as_str())
            .or_else(|| o["type"].as_str())
            .unwrap_or("")
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn windows_cover_the_command_and_its_grace() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut w = CommandWindows::default();
        assert!(!w.covers(ms(0)));
        w.begin(ms(0));
        // Running: covers whatever comes after its start.
        assert!(w.covers(ms(0)));
        assert!(w.covers(ms(60_000)));
        w.end(ms(100));
        assert!(w.covers(ms(50)));
        assert!(w.covers(ms(1099)));
        assert!(!w.covers(ms(1101)));
        // A later window does not hide the gap before it (a late signal of that time).
        w.begin(ms(5000));
        w.end(ms(5100));
        assert!(!w.covers(ms(3000)));
        assert!(w.covers(ms(5050)));
        // The end of `session create`: a window of no length, with its grace.
        w.end(ms(9000));
        assert!(w.covers(ms(9500)));
        assert!(!w.covers(ms(8999)));
    }

    #[test]
    fn only_the_latest_windows_are_kept() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut w = CommandWindows::default();
        for i in 0..=KEPT_WINDOWS as u64 {
            w.begin(ms(i * 10_000));
            w.end(ms(i * 10_000 + 100));
        }
        // The first window is forgotten; the oldest kept one still counts.
        assert!(!w.covers(ms(50)));
        assert!(w.covers(ms(10_050)));
    }

    #[test]
    fn urls_in_console_text_lose_query_fragment_and_userinfo() {
        let trace = "Error: boom\n    at f (https://u:p@a.test/app.js?token=SECRET1#x:10:5)\n    \
                     at https://a.test/b.js?k=SECRET2:3:1\n    fetch ws://a.test/s?t=SECRET3, \
                     blob:https://a.test/id?SECRET4 data:text/html,SECRET5 and http://a.test:8080/p";
        let got = scrub_urls(trace);
        for secret in ["SECRET", "u:p@", "#x"] {
            assert!(!got.contains(secret), "{got}");
        }
        assert!(got.contains("(https://a.test/app.js:10:5)"), "{got}");
        assert!(got.contains("at https://a.test/b.js:3:1"), "{got}");
        // A comma does not end a URL (queries hold them): it goes with the query.
        assert!(
            got.contains("ws://a.test/s blob:https://a.test/id data:"),
            "{got}"
        );
        assert!(got.contains("http://a.test:8080/p"), "{got}");
        assert!(got.contains("data: and"), "{got}");
        assert_eq!(
            scrub_urls("plain text, no address"),
            "plain text, no address"
        );
        // Through console_line, for both kinds.
        let e = console_line(
            "Runtime.exceptionThrown",
            &json!({ "exceptionDetails": { "exception": { "description": trace } } }),
            "t",
            "T1",
        )
        .unwrap();
        assert!(!e.text.contains("SECRET"), "{}", e.text);
        let l = console_line(
            "Runtime.consoleAPICalled",
            &json!({ "type": "log", "args": [{ "type": "string", "value": "go https://a.test/?token=SECRET9" }] }),
            "t",
            "T1",
        )
        .unwrap();
        assert_eq!(l.text, "go https://a.test/");
    }

    fn nav(requested: Option<&str>) -> Signal {
        Signal::Navigated {
            url: "https://u:p@a.test/x?token=s#f".into(),
            requested: requested.map(String::from),
        }
    }

    #[test]
    fn classification_table() {
        // In a command window nothing is a person's.
        for s in [
            nav(None),
            nav(Some("anchorClick")),
            nav(Some("scriptInitiated")),
            Signal::Activated,
        ] {
            assert_eq!(classify(&s, true, false), None, "{s:?}");
            assert_eq!(classify(&s, true, true), None, "{s:?}");
        }
        // Outside: browser-started and link/form navigations are possible, never certain
        // (F6: a script's history.back() and a.click() look the same).
        assert_eq!(
            classify(&nav(None), false, false),
            Some((
                "navigated outside a command: https://a.test/x".into(),
                false
            ))
        );
        for r in ["anchorClick", "formSubmissionGet", "formSubmissionPost"] {
            assert_eq!(
                classify(&nav(Some(r)), false, false),
                Some(("followed a link or form outside a command".into(), false)),
                "{r}"
            );
        }
        // A reason the browser does not explain: possible, like a browser-started one.
        assert_eq!(
            classify(&nav(Some("other")), false, false).map(|(_, c)| c),
            Some(false)
        );
        // The page's own doing.
        for r in [
            "scriptInitiated",
            "reload",
            "metaTagRefresh",
            "httpHeaderRefresh",
        ] {
            assert_eq!(classify(&nav(Some(r)), false, false), None, "{r}");
        }
        // A tab switch pegboard made itself is not a person's.
        assert_eq!(classify(&Signal::Activated, false, true), None);
        assert_eq!(
            classify(&Signal::Activated, false, false),
            Some(("switched to this tab".into(), false))
        );
    }

    #[test]
    fn a_navigation_carries_the_request_it_started_from() {
        let mut f = NavFollow::new(false);
        // Browser-started.
        assert_eq!(
            f.committed("http://a.test/"),
            Some(nav_to("http://a.test/", None))
        );
        // Asked by the page, then started and committed (the commit may be a redirect's).
        f.requested("http://a.test/1", "scriptInitiated", "currentTab");
        f.started("http://a.test/1");
        assert_eq!(
            f.committed("http://b.test/2"),
            Some(nav_to("http://b.test/2", Some("scriptInitiated")))
        );
        // A request that never started is not carried over to a browser-started one.
        f.requested("http://a.test/3", "scriptInitiated", "currentTab");
        f.started("http://a.test/other");
        assert_eq!(
            f.committed("http://a.test/other"),
            Some(nav_to("http://a.test/other", None))
        );
        // A request for a new window does not concern this tab.
        f.requested("http://a.test/4", "anchorClick", "newForegroundTab");
        f.started("http://a.test/4");
        assert_eq!(
            f.committed("http://a.test/4"),
            Some(nav_to("http://a.test/4", None))
        );
        // Without a started event (it is optional), the request of the same URL counts.
        f.requested("http://a.test/5", "anchorClick", "currentTab");
        assert_eq!(
            f.committed("http://a.test/5"),
            Some(nav_to("http://a.test/5", Some("anchorClick")))
        );
    }

    fn nav_to(url: &str, requested: Option<&str>) -> Signal {
        Signal::Navigated {
            url: url.into(),
            requested: requested.map(String::from),
        }
    }

    #[test]
    fn a_new_tabs_first_page_is_its_opening() {
        let mut f = NavFollow::new(true);
        // The blank page a tab starts on does not end that.
        assert_eq!(f.committed("about:blank"), None);
        assert_eq!(f.committed("http://a.test/"), None);
        assert!(f.committed("http://a.test/2").is_some());
        // Found already on a page when attached: its opening was before.
        let mut g = NavFollow::new(true);
        g.seen_on("about:blank");
        assert_eq!(g.committed("http://a.test/"), None);
        let mut h = NavFollow::new(true);
        h.seen_on("http://a.test/");
        assert!(h.committed("http://a.test/2").is_some());
    }

    #[test]
    fn a_page_found_on_attach_is_logged_once() {
        let mut f = NavFollow::new(true);
        assert!(!f.unlogged("about:blank"));
        assert!(f.unlogged("http://a.test/"));
        assert!(!f.unlogged("http://a.test/"));
        f.committed("http://b.test/");
        assert!(!f.unlogged("http://b.test/"));
        assert!(f.unlogged("http://c.test/"));
    }

    #[test]
    fn nav_lines_drop_query_and_userinfo() {
        let l = nav_line(
            "2026-01-01T00:00:00Z",
            "T1",
            NavEvent::Navigated,
            Some("https://user:pw@a.test/p?token=SECRET#frag"),
            Some("T0"),
        );
        assert_eq!(l.url.as_deref(), Some("https://a.test/p"));
        assert_eq!(l.opener.as_deref(), Some("T0"));
        assert_eq!(l.event, NavEvent::Navigated);
        let data = nav_line(
            "t",
            "T1",
            NavEvent::Created,
            Some("data:text/html,secret"),
            None,
        );
        assert_eq!(data.url.as_deref(), Some("data:"));
        let v = serde_json::to_value(&l).unwrap();
        assert_eq!(v["targetId"], "T1");
        assert!(!v.to_string().contains("SECRET"), "{v}");
    }

    #[test]
    fn console_text_is_cut_to_1000_chars() {
        let long = "x".repeat(3000);
        let l = console_line(
            "Runtime.consoleAPICalled",
            &json!({ "type": "log", "args": [
                { "type": "string", "value": long },
            ] }),
            "t",
            "T1",
        )
        .unwrap();
        assert_eq!(l.text.chars().count(), MAX_CONSOLE_CHARS);
        assert_eq!(l.level, "log");
        let l = console_line(
            "Runtime.consoleAPICalled",
            &json!({ "type": "warning", "args": [
                { "type": "string", "value": "n =" },
                { "type": "number", "value": 3 },
                { "type": "object", "description": "Object" },
                { "type": "number", "unserializableValue": "NaN" },
                { "type": "undefined" },
            ] }),
            "t",
            "T1",
        )
        .unwrap();
        assert_eq!(l.text, "n = 3 Object NaN undefined");
        assert_eq!(l.level, "warning");
        let e = console_line(
            "Runtime.exceptionThrown",
            &json!({ "exceptionDetails": { "text": "Uncaught",
                     "exception": { "description": "Error: boom\n    at x" } } }),
            "t",
            "T1",
        )
        .unwrap();
        assert_eq!(e.level, "exception");
        assert_eq!(e.text, "Error: boom\n    at x");
        assert!(console_line("Runtime.executionContextCreated", &json!({}), "t", "T1").is_none());
    }

    #[test]
    fn network_lines_hold_metadata_only() {
        let mut r = Requests::default();
        let sent = |id: &str, url: &str, t: f64| {
            json!({ "requestId": id, "timestamp": t,
                    "request": { "method": "GET", "url": url,
                                 "headers": { "Authorization": "Bearer SECRET2" },
                                 "postData": "SECRET4" } })
        };
        assert!(r
            .event(
                "Network.requestWillBeSent",
                &sent("1", "https://a.test/api?token=SECRET1", 10.0),
                "t",
                "T1"
            )
            .is_none());
        assert!(r
            .event(
                "Network.responseReceived",
                &json!({ "requestId": "1", "timestamp": 10.1,
                         "response": { "status": 200, "headers": { "Set-Cookie": "SECRET3" } } }),
                "t",
                "T1"
            )
            .is_none());
        let done = r
            .event(
                "Network.loadingFinished",
                &json!({ "requestId": "1", "timestamp": 10.25, "encodedDataLength": 1234.0 }),
                "t",
                "T1",
            )
            .unwrap();
        assert_eq!(
            done,
            NetworkLine {
                ts: "t".into(),
                target_id: "T1".into(),
                method: "GET".into(),
                url: "https://a.test/api".into(),
                status: Some(200),
                bytes: Some(1234),
                duration_ms: Some(250),
                error: None,
            }
        );
        let text = serde_json::to_string(&done).unwrap();
        for secret in ["SECRET", "Authorization", "Cookie"] {
            assert!(!text.contains(secret), "{text}");
        }
        // A redirect ends its hop; a failure carries the error.
        r.event(
            "Network.requestWillBeSent",
            &sent("2", "http://a.test/r", 1.0),
            "t",
            "T1",
        );
        let mut redirected = sent("2", "http://b.test/x?q", 1.5);
        redirected["redirectResponse"] = json!({ "status": 302 });
        let hop = r
            .event("Network.requestWillBeSent", &redirected, "t", "T1")
            .unwrap();
        assert_eq!(
            (hop.url.as_str(), hop.status),
            ("http://a.test/r", Some(302))
        );
        let failed = r
            .event(
                "Network.loadingFailed",
                &json!({ "requestId": "2", "timestamp": 2.0, "errorText": "net::ERR_FAILED" }),
                "t",
                "T1",
            )
            .unwrap();
        assert_eq!(failed.url, "http://b.test/x");
        assert_eq!(failed.error.as_deref(), Some("net::ERR_FAILED"));
        assert_eq!(failed.duration_ms, Some(500));
        // An end without its start is nothing.
        assert!(r
            .event(
                "Network.loadingFinished",
                &json!({ "requestId": "9" }),
                "t",
                "T1"
            )
            .is_none());
    }
}
