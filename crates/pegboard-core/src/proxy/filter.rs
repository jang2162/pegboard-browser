//! Session-scoped CDP filter decisions. Pure functions over JSON messages.
//!
//! This is the isolation boundary between an agent session and the rest of the
//! browser: default-deny, allowlisted methods, and only the session's own
//! targets and CDP sessions are visible. Rule tables are ordered; the first
//! matching row wins.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// What a session may see: its browser context, its targets and its CDP sessions.
pub struct Scope {
    pub context: String,
    pub targets: HashSet<String>,
    pub cdp_sessions: HashSet<String>,
    /// The directories `DOM.setFileInputFiles` may take files from: the session's upload
    /// directory and files directory when its policy allows upload, else none.
    pub upload_dirs: Vec<PathBuf>,
}

#[derive(Debug, PartialEq)]
pub enum ClientDecision {
    /// Pass to the browser unchanged.
    Forward,
    /// Pass to the browser after the proxy rewrote the message (the new message).
    ForwardRewritten(Value),
    /// Answer locally with this `result`.
    Reply(Value),
    /// Answer locally with a CDP error.
    Error { code: i64, message: String },
    /// `Target.createTarget`: the daemon creates the tab via the bridge.
    CreateTab { url: String },
}

#[derive(Debug, PartialEq)]
pub enum BrowserDecision {
    Forward(Value),
    Drop,
    /// A new target that belongs to the session (popup of an own tab).
    Adopt {
        target_id: String,
        forward: Value,
    },
}

/// The 23 methods agent-browser used in phase 0 plus the additions for phase 1.
/// `Page.bringToFront` and `Target.activateTarget` are excluded on purpose:
/// they raise windows and steal the human's focus.
pub const ALLOWED_METHODS: &[&str] = &[
    // phase 0
    "Accessibility.enable",
    "Accessibility.getFullAXTree",
    "Browser.getVersion",
    "DOM.enable",
    "Input.dispatchMouseEvent",
    "Input.insertText",
    "Network.enable",
    "Page.captureScreenshot",
    "Page.enable",
    "Page.getFrameTree",
    "Page.navigate",
    "Page.startScreencast",
    "Page.stopScreencast",
    "Runtime.callFunctionOn",
    "Runtime.enable",
    "Runtime.evaluate",
    "Runtime.runIfWaitingForDebugger",
    "Target.attachToTarget",
    "Target.createTarget",
    "Target.getTargets",
    "Target.setAutoAttach",
    "Target.setDiscoverTargets",
    "WebMCP.enable",
    // phase 1 additions
    "Target.closeTarget",
    "Target.getTargetInfo",
    "Target.detachFromTarget",
    "Page.reload",
    "Page.navigateToHistoryEntry",
    "Page.getNavigationHistory",
    "Input.dispatchKeyEvent",
    "Input.dispatchTouchEvent",
    "DOM.getDocument",
    "DOM.querySelector",
    "DOM.scrollIntoViewIfNeeded",
    "DOM.getBoxModel",
    "DOM.focus",
    "DOM.resolveNode",
    "DOM.describeNode",
    "Runtime.getProperties",
    "Runtime.releaseObject",
    "Emulation.setDeviceMetricsOverride",
    "Network.getCookies",
    "Network.setCookie",
    "Network.deleteCookies",
    // Task 7: needed by agent-browser commands; each acts on the page of the CDP session
    // it is sent on (or that page's browser context), never on other targets.
    "DOM.getFrameOwner",
    "Network.setCookies",
    "Page.getLayoutMetrics",
    "Page.handleJavaScriptDialog",
    "Page.printToPDF",
    // Task 7 fix round: `a11y` and `vitals`. Both stay inside the session's page (an
    // isolated JS world of its frame; a script for its next documents).
    "Page.createIsolatedWorld",
    "Page.addScriptToEvaluateOnNewDocument",
    // Task 6: `upload`. Only forwarded after `client_message` has checked every path against
    // the session's upload directories (rule 7b); it reads local files into the page.
    "DOM.setFileInputFiles",
];

const NOT_ALLOWED: &str = "not allowed in a pegboard session";
const REFUSED_METHODS: &[&str] = &[
    "Browser.close",
    "Browser.crash",
    "Target.disposeBrowserContext",
    "Target.createBrowserContext",
];
const TARGET_ID_METHODS: &[&str] = &[
    "Target.attachToTarget",
    "Target.closeTarget",
    "Target.getTargetInfo",
];
const URL_REFUSED: &str = "url scheme is not allowed in a pegboard session";

/// Judge and normalize a URL an agent wants to open or navigate to. Returns the
/// string the browser must receive, or `None` when it is refused.
///
/// Only http(s) with a host, data: and exactly about:blank pass. Like Chrome,
/// only leading/trailing characters <= 0x20 are trimmed (not Unicode
/// whitespace), and control characters inside (tab, CR, LF) are refused rather
/// than silently stripped. http(s) must literally contain "://" after the scheme.
pub fn normalize_url(url: &str) -> Option<String> {
    let trimmed = url.trim_matches(|c: char| c <= ' ');
    if trimmed.chars().any(|c| c < ' ') {
        return None;
    }
    if trimmed == "about:blank" {
        return Some("about:blank".into());
    }
    let parsed = url::Url::parse(trimmed).ok()?;
    match parsed.scheme() {
        "data" => Some(parsed.to_string()),
        "http" | "https" => {
            let rest = trimmed.get(parsed.scheme().len()..)?.strip_prefix("://")?;
            if rest.starts_with(['/', '\\']) || parsed.host_str().is_none_or(str::is_empty) {
                return None;
            }
            Some(parsed.to_string())
        }
        _ => None,
    }
}

/// Browser-level (no sessionId) calls that would reach beyond the agent's context.
const BROWSER_LEVEL_EXACT: &[&str] = &[
    "Browser.grantPermissions",
    "Browser.resetPermissions",
    "Browser.setDownloadBehavior",
];
const BROWSER_LEVEL_PREFIXES: &[&str] = &["Storage.", "Network.", "Fetch."];
pub(crate) const TARGET_EVENTS: &[&str] = &[
    "Target.targetCreated",
    "Target.targetInfoChanged",
    "Target.targetDestroyed",
    "Target.attachedToTarget",
    "Target.detachedFromTarget",
    "Target.targetCrashed",
];

fn error(code: i64, message: impl Into<String>) -> ClientDecision {
    ClientDecision::Error {
        code,
        message: message.into(),
    }
}

/// Decide what to do with a message from the agent's client. Ordered rules.
pub fn client_message(msg: &Value, scope: &Scope) -> ClientDecision {
    // Default-deny on malformed input.
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        return error(-32600, "Invalid request: missing method");
    };
    let params = match msg.get("params") {
        None => None,
        Some(Value::Object(p)) => Some(p),
        Some(_) => return error(-32602, "Invalid params: must be an object"),
    };

    // 1. Target.createTarget is never forwarded; only the url survives.
    if method == "Target.createTarget" {
        return match params.and_then(|p| p.get("url")) {
            None => ClientDecision::CreateTab {
                url: "about:blank".into(),
            },
            Some(Value::String(url)) => match normalize_url(url) {
                Some(url) => ClientDecision::CreateTab { url },
                None => error(-32000, URL_REFUSED),
            },
            Some(_) => error(-32602, "Invalid params: url must be a string"),
        };
    }

    // 2. Calls that could take down the browser or create contexts.
    if REFUSED_METHODS.contains(&method) {
        return error(-32000, NOT_ALLOWED);
    }

    // 3. Target-addressed calls must name one of the session's targets.
    if TARGET_ID_METHODS.contains(&method) {
        let own = params
            .and_then(|p| p.get("targetId"))
            .and_then(Value::as_str)
            .is_some_and(|id| scope.targets.contains(id));
        if !own {
            return error(-32000, "No target with given id found");
        }
    }

    if method == "Target.detachFromTarget" {
        let p = params;
        if let Some(sid) = p.and_then(|p| p.get("sessionId")) {
            if !sid.as_str().is_some_and(|s| scope.cdp_sessions.contains(s)) {
                return error(-32001, "Session with given id not found.");
            }
        } else if let Some(tid) = p.and_then(|p| p.get("targetId")) {
            if !tid.as_str().is_some_and(|t| scope.targets.contains(t)) {
                return error(-32000, "No target with given id found");
            }
        } else {
            return error(-32000, "No target with given id found");
        }
    }

    // 4. A sessionId must belong to this session.
    let session_id = msg.get("sessionId");
    if let Some(sid) = session_id {
        if !sid.as_str().is_some_and(|s| scope.cdp_sessions.contains(s)) {
            return error(-32001, "Session with given id not found.");
        }
    } else if BROWSER_LEVEL_EXACT.contains(&method)
        || BROWSER_LEVEL_PREFIXES.iter().any(|p| method.starts_with(p))
    {
        // 5. Browser-level (no sessionId) calls on shared state.
        return error(
            -32000,
            "browser-level call is not allowed; use a page session",
        );
    }

    // 6. Browser-level auto-attach must not make new tabs wait for a debugger.
    if method == "Target.setAutoAttach" && session_id.is_none() {
        let mut rewritten = msg.clone();
        if let Some(obj) = rewritten.as_object_mut() {
            let params = obj.entry("params").or_insert_with(|| json!({}));
            if let Some(p) = params.as_object_mut() {
                p.insert("waitForDebuggerOnStart".into(), json!(false));
            }
        }
        return ClientDecision::ForwardRewritten(rewritten);
    }

    // 7. Navigation targets must use an allowed URL; the browser receives the
    // normalized string that was judged, not the raw one.
    if method == "Page.navigate" {
        let Some(url) = params
            .and_then(|p| p.get("url"))
            .and_then(Value::as_str)
            .and_then(normalize_url)
        else {
            return error(-32000, URL_REFUSED);
        };
        let mut rewritten = msg.clone();
        rewritten["params"]["url"] = Value::String(url);
        return ClientDecision::ForwardRewritten(rewritten);
    }

    // 7b. Files for a file input: only regular files inside the session's upload
    // directories, which are empty unless the session's policy allows upload. Checked on the
    // real paths, so a link inside a directory that points out of it does not pass.
    if method == "DOM.setFileInputFiles" {
        if session_id.is_none() {
            return error(
                -32000,
                "browser-level call is not allowed; use a page session",
            );
        }
        let Some(files) = params
            .and_then(|p| p.get("files"))
            .and_then(Value::as_array)
            .and_then(|a| a.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
        else {
            return error(-32602, "Invalid params: files must be an array of strings");
        };
        // The browser gets the real paths that were judged, not the strings that were sent.
        let Some(real) = files
            .iter()
            .map(|f| crate::upload::allowed_real_path(Path::new(f), &scope.upload_dirs))
            .collect::<Option<Vec<_>>>()
        else {
            return error(-32000, "a file outside the session's upload directories");
        };
        let mut rewritten = msg.clone();
        rewritten["params"]["files"] = json!(real
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>());
        return ClientDecision::ForwardRewritten(rewritten);
    }

    // 8. Allowlist.
    if !ALLOWED_METHODS.contains(&method) {
        return error(
            -32601,
            format!("'{method}' is not allowed in a pegboard session"),
        );
    }

    // 9. Everything else.
    ClientDecision::Forward
}

fn target_id_of(params: &Value) -> Option<&str> {
    params
        .pointer("/targetInfo/targetId")
        .or_else(|| params.get("targetId"))
        .and_then(Value::as_str)
}

/// Decide what to do with a message from the browser. `_request_method` (the
/// request method for a response; unused now that filtering keys off the message). Ordered rules.
pub fn browser_message(
    mut msg: Value,
    _request_method: Option<&str>,
    scope: &Scope,
) -> BrowserDecision {
    let method = msg.get("method").and_then(Value::as_str).map(str::to_owned);

    // 1. Anything on a CDP session that is not this session's, whatever its body.
    if let Some(sid) = msg.get("sessionId") {
        if !sid.as_str().is_some_and(|s| scope.cdp_sessions.contains(s)) {
            return BrowserDecision::Drop;
        }
    }

    // 2. Any response carrying targetInfos: hide targets outside the session.
    if method.is_none()
        && msg
            .pointer("/result/targetInfos")
            .is_some_and(Value::is_array)
    {
        if let Some(infos) = msg
            .pointer_mut("/result/targetInfos")
            .and_then(Value::as_array_mut)
        {
            infos.retain(|t| {
                t.get("targetId")
                    .and_then(Value::as_str)
                    .is_some_and(|id| scope.targets.contains(id))
            });
        }
        return BrowserDecision::Forward(msg);
    }

    if let Some(method) = method.as_deref() {
        let params = msg.get("params").unwrap_or(&Value::Null);

        // 3. Popup of an own tab in the session's context is adopted, whether discovery
        // (targetCreated) or browser-level auto-attach (attachedToTarget) reports it first.
        let top_level = msg.get("sessionId").is_none();
        if method == "Target.targetCreated" || (method == "Target.attachedToTarget" && top_level) {
            let info = params.get("targetInfo");
            let opener = info.and_then(|i| i.get("openerId")).and_then(Value::as_str);
            let context = info
                .and_then(|i| i.get("browserContextId"))
                .and_then(Value::as_str);
            if let (Some(opener), Some(context), Some(target_id)) =
                (opener, context, target_id_of(params))
            {
                if scope.targets.contains(opener) && context == scope.context {
                    let target_id = target_id.to_owned();
                    return BrowserDecision::Adopt {
                        target_id,
                        forward: msg,
                    };
                }
            }
        }

        // 4. Child targets (iframe, worker) reported inside an own session envelope.
        if matches!(
            method,
            "Target.attachedToTarget" | "Target.detachedFromTarget"
        ) && msg
            .get("sessionId")
            .and_then(Value::as_str)
            .is_some_and(|s| scope.cdp_sessions.contains(s))
        {
            return BrowserDecision::Forward(msg);
        }

        // 5. Relayed messages of a non-flat session must come from one of this session's.
        if method == "Target.receivedMessageFromTarget"
            && !params
                .get("sessionId")
                .and_then(Value::as_str)
                .is_some_and(|s| scope.cdp_sessions.contains(s))
        {
            return BrowserDecision::Drop;
        }

        // 6. Target events about foreign (or unidentifiable) targets.
        if TARGET_EVENTS.contains(&method)
            && !target_id_of(params).is_some_and(|id| scope.targets.contains(id))
        {
            return BrowserDecision::Drop;
        }
    }

    // 7. Everything else (including responses to forwarded requests).
    BrowserDecision::Forward(msg)
}

/// The targets and CDP sessions one client connection was told about (what was sent to it:
/// listings, events, attach and create replies). A target that left the session scope
/// because the human closed it is still reported as gone to the connections that knew it,
/// and to no other.
#[derive(Debug, Default)]
pub struct Told {
    targets: HashSet<String>,
    sessions: HashSet<String>,
}

impl Told {
    /// Notes what a message that is about to be sent to the client reveals.
    pub fn on_sent(&mut self, msg: &Value) {
        let params = msg.get("params").unwrap_or(&Value::Null);
        let string = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_owned);
        match msg.get("method").and_then(Value::as_str) {
            Some("Target.targetDestroyed") => {
                if let Some(t) = string(params.get("targetId")) {
                    self.targets.remove(&t);
                }
            }
            Some("Target.detachedFromTarget") => {
                if let Some(s) = string(params.get("sessionId")) {
                    self.sessions.remove(&s);
                }
            }
            Some("Target.attachedToTarget") => {
                self.sessions.extend(string(params.get("sessionId")));
                self.targets
                    .extend(string(params.pointer("/targetInfo/targetId")));
            }
            Some("Target.targetCreated" | "Target.targetInfoChanged") => {
                self.targets
                    .extend(string(params.pointer("/targetInfo/targetId")));
            }
            Some(_) => {}
            None => {
                let result = msg.get("result").unwrap_or(&Value::Null);
                self.targets.extend(string(result.get("targetId")));
                self.sessions.extend(string(result.get("sessionId")));
                for info in result
                    .get("targetInfos")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    self.targets.extend(string(info.get("targetId")));
                }
            }
        }
    }

    /// A top-level `Target.targetDestroyed` or `Target.detachedFromTarget` about something
    /// this connection was told about.
    fn knows_ending(&self, msg: &Value) -> bool {
        if msg.get("sessionId").is_some() {
            return false;
        }
        let params = msg.get("params").unwrap_or(&Value::Null);
        let has = |set: &HashSet<String>, key: &str| {
            params
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|v| set.contains(v))
        };
        match msg.get("method").and_then(Value::as_str) {
            Some("Target.targetDestroyed") => has(&self.targets, "targetId"),
            // One that names a session is about that session only.
            Some("Target.detachedFromTarget") => {
                if params.get("sessionId").is_some() {
                    has(&self.sessions, "sessionId")
                } else {
                    has(&self.targets, "targetId")
                }
            }
            _ => false,
        }
    }
}

/// `browser_message`, except that the end of a target or CDP session this connection was
/// told about is passed on even when the target already left the session scope.
pub fn browser_message_told(
    msg: Value,
    request_method: Option<&str>,
    scope: &Scope,
    told: &Told,
) -> BrowserDecision {
    let ending = told.knows_ending(&msg);
    match browser_message(msg.clone(), request_method, scope) {
        BrowserDecision::Drop if ending => BrowserDecision::Forward(msg),
        decision => decision,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope {
            context: "AGENT".into(),
            targets: ["T1".to_string()].into(),
            cdp_sessions: ["S1".to_string()].into(),
            upload_dirs: Vec::new(),
        }
    }

    fn err_code(d: &ClientDecision) -> i64 {
        match d {
            ClientDecision::Error { code, .. } => *code,
            other => panic!("expected Error, got {other:?}"),
        }
    }

    fn set_files(files: Value) -> Value {
        json!({"id":1,"sessionId":"S1","method":"DOM.setFileInputFiles",
               "params":{"files":files,"backendNodeId":7}})
    }

    #[test]
    fn set_file_input_files_only_takes_files_inside_the_upload_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (up, files, out) = (root.join("up"), root.join("files"), root.join("out"));
        for d in [&up, &files, &out] {
            std::fs::create_dir(d).unwrap();
            std::fs::write(d.join("a.txt"), "a").unwrap();
        }
        std::os::unix::fs::symlink(out.join("a.txt"), up.join("link")).unwrap();
        let mut sc = scope();
        let path = |p: PathBuf| json!([p.display().to_string()]);

        // No directory (a session that may not upload): refused, whatever the path.
        let m = set_files(path(up.join("a.txt")));
        assert_eq!(err_code(&client_message(&m, &sc)), -32000);

        sc.upload_dirs = vec![up.clone(), files.clone()];
        for ok in [up.join("a.txt"), files.join("a.txt")] {
            let m = set_files(path(ok.clone()));
            assert_eq!(
                client_message(&m, &sc),
                ClientDecision::ForwardRewritten(m.clone()),
                "{ok:?}"
            );
        }
        // What is forwarded is the real path that was judged.
        let m = set_files(path(up.join("../up/a.txt")));
        assert_eq!(
            client_message(&m, &sc),
            ClientDecision::ForwardRewritten(set_files(path(up.join("a.txt"))))
        );
        // Not in a subdirectory of a directory.
        std::fs::create_dir(up.join("sub")).unwrap();
        std::fs::write(up.join("sub/b.txt"), "b").unwrap();
        let m = set_files(path(up.join("sub/b.txt")));
        assert_eq!(err_code(&client_message(&m, &sc)), -32000);
        for bad in [
            out.join("a.txt"),
            up.join("link"),
            up.join("../out/a.txt"),
            up.join("missing"),
            PathBuf::from("a.txt"),
            PathBuf::from("/etc/passwd"),
        ] {
            let m = set_files(path(bad.clone()));
            assert_eq!(err_code(&client_message(&m, &sc)), -32000, "{bad:?}");
        }
        // One path outside refuses the whole call.
        let m = set_files(json!([
            up.join("a.txt").display().to_string(),
            out.join("a.txt").display().to_string()
        ]));
        assert_eq!(err_code(&client_message(&m, &sc)), -32000);
        // Clearing the input needs no file.
        let m = set_files(json!([]));
        assert_eq!(
            client_message(&m, &sc),
            ClientDecision::ForwardRewritten(m.clone())
        );
        // Malformed: files missing, not an array, not strings; no CDP session.
        for files in [json!(null), json!("x"), json!([1]), json!({"a":1})] {
            let mut m = set_files(files);
            assert_eq!(err_code(&client_message(&m, &sc)), -32602);
            m["params"].as_object_mut().unwrap().remove("files");
            assert_eq!(err_code(&client_message(&m, &sc)), -32602);
        }
        let mut m = set_files(path(up.join("a.txt")));
        m.as_object_mut().unwrap().remove("sessionId");
        assert_eq!(err_code(&client_message(&m, &sc)), -32000);
    }

    #[test]
    fn create_target_is_never_forwarded() {
        let m = json!({"method":"Target.createTarget","params":{"url":"https://a.test","browserContextId":"HUMAN"}});
        assert_eq!(
            client_message(&m, &scope()),
            ClientDecision::CreateTab {
                url: "https://a.test/".into()
            }
        );
    }

    #[test]
    fn create_target_without_url_uses_blank() {
        let m = json!({"method":"Target.createTarget","params":{}});
        assert_eq!(
            client_message(&m, &scope()),
            ClientDecision::CreateTab {
                url: "about:blank".into()
            }
        );
        let m = json!({"method":"Target.createTarget"});
        assert_eq!(
            client_message(&m, &scope()),
            ClientDecision::CreateTab {
                url: "about:blank".into()
            }
        );
    }

    #[test]
    fn foreign_target_cannot_be_attached() {
        let m = json!({"method":"Target.attachToTarget","params":{"targetId":"H1"}});
        assert_eq!(err_code(&client_message(&m, &scope())), -32000);
    }

    #[test]
    fn own_target_can_be_attached() {
        let m = json!({"method":"Target.attachToTarget","params":{"targetId":"T1"}});
        assert_eq!(client_message(&m, &scope()), ClientDecision::Forward);
    }

    #[test]
    fn browser_close_is_refused() {
        let m = json!({"method":"Browser.close"});
        assert_eq!(err_code(&client_message(&m, &scope())), -32000);
    }

    #[test]
    fn browser_level_cookies_are_refused() {
        let m = json!({"method":"Storage.getCookies"});
        match client_message(&m, &scope()) {
            ClientDecision::Error { message, .. } => assert!(message.contains("browser-level")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// The daemon denies downloads in agent profiles; a client must not be able to lift that.
    #[test]
    fn download_control_is_refused_with_and_without_a_session() {
        for method in [
            "Browser.setDownloadBehavior",
            "Page.setDownloadBehavior",
            "Browser.cancelDownload",
        ] {
            for m in [
                json!({"method":method,"params":{"behavior":"allow","downloadPath":"/tmp"}}),
                json!({"method":method,"params":{"behavior":"allow","browserContextId":"C1"}}),
                json!({"method":method,"sessionId":"S1","params":{"behavior":"allow"}}),
            ] {
                assert!(
                    matches!(client_message(&m, &scope()), ClientDecision::Error { .. }),
                    "{m}"
                );
            }
        }
    }

    #[test]
    fn page_level_cookies_pass() {
        let m = json!({"method":"Network.getCookies","sessionId":"S1"});
        assert_eq!(client_message(&m, &scope()), ClientDecision::Forward);
    }

    #[test]
    fn unknown_session_is_refused() {
        let m = json!({"method":"Network.getCookies","sessionId":"S9"});
        assert_eq!(err_code(&client_message(&m, &scope())), -32001);
    }

    #[test]
    fn unlisted_method_is_refused() {
        let m = json!({"method":"SystemInfo.getInfo"});
        assert_eq!(err_code(&client_message(&m, &scope())), -32601);
    }

    #[test]
    fn bring_to_front_is_refused() {
        let m = json!({"method":"Page.bringToFront","sessionId":"S1"});
        assert_eq!(err_code(&client_message(&m, &scope())), -32601);
    }

    #[test]
    fn missing_or_malformed_method_is_refused() {
        for m in [
            json!({"id":1}),
            json!({"id":1,"method":5}),
            json!({"id":1,"method":null}),
        ] {
            assert!(
                matches!(client_message(&m, &scope()), ClientDecision::Error { .. }),
                "{m}"
            );
        }
    }

    #[test]
    fn non_object_params_are_refused() {
        for p in [json!("x"), json!([1]), json!(null), json!(3)] {
            let m = json!({"method":"Page.navigate","sessionId":"S1","params":p});
            assert!(
                matches!(client_message(&m, &scope()), ClientDecision::Error { .. }),
                "{m}"
            );
        }
    }

    #[test]
    fn get_targets_response_hides_foreign_targets() {
        let m = json!({"id":1,"result":{"targetInfos":[{"targetId":"T1"},{"targetId":"H1"}]}});
        let d = browser_message(m, Some("Target.getTargets"), &scope());
        assert_eq!(
            d,
            BrowserDecision::Forward(json!({"id":1,"result":{"targetInfos":[{"targetId":"T1"}]}}))
        );
    }

    #[test]
    fn unsolicited_response_is_forwarded_as_is() {
        let m = json!({"id":7,"result":{}});
        assert_eq!(
            browser_message(m.clone(), None, &scope()),
            BrowserDecision::Forward(m)
        );
    }

    #[test]
    fn popup_of_own_tab_is_adopted() {
        let m = json!({"method":"Target.targetCreated","params":{"targetInfo":{
            "targetId":"T2","openerId":"T1","browserContextId":"AGENT"}}});
        assert_eq!(
            browser_message(m.clone(), None, &scope()),
            BrowserDecision::Adopt {
                target_id: "T2".into(),
                forward: m
            }
        );
    }

    #[test]
    fn auto_attached_popup_of_own_tab_is_adopted() {
        let m = json!({"method":"Target.attachedToTarget","params":{"sessionId":"S2","targetInfo":{
            "targetId":"T2","openerId":"T1","browserContextId":"AGENT"},"waitingForDebugger":false}});
        assert_eq!(
            browser_message(m.clone(), None, &scope()),
            BrowserDecision::Adopt {
                target_id: "T2".into(),
                forward: m
            }
        );
        let foreign = json!({"method":"Target.attachedToTarget","params":{"sessionId":"S3","targetInfo":{
            "targetId":"H2","openerId":"H1","browserContextId":"AGENT"}}});
        assert_eq!(
            browser_message(foreign, None, &scope()),
            BrowserDecision::Drop
        );
    }

    #[test]
    fn popup_in_other_context_is_dropped() {
        let m = json!({"method":"Target.targetCreated","params":{"targetInfo":{
            "targetId":"T2","openerId":"T1","browserContextId":"HUMAN"}}});
        assert_eq!(browser_message(m, None, &scope()), BrowserDecision::Drop);
    }

    #[test]
    fn foreign_target_events_are_dropped() {
        let created =
            json!({"method":"Target.targetCreated","params":{"targetInfo":{"targetId":"H2"}}});
        assert_eq!(
            browser_message(created, None, &scope()),
            BrowserDecision::Drop
        );
        let destroyed = json!({"method":"Target.targetDestroyed","params":{"targetId":"H1"}});
        assert_eq!(
            browser_message(destroyed, None, &scope()),
            BrowserDecision::Drop
        );
    }

    #[test]
    fn events_of_foreign_sessions_are_dropped() {
        let m = json!({"method":"Page.loadEventFired","sessionId":"S9"});
        assert_eq!(browser_message(m, None, &scope()), BrowserDecision::Drop);
    }

    fn sess_ok(d: &ClientDecision) {
        assert_eq!(*d, ClientDecision::Forward);
    }

    #[test]
    fn detach_from_foreign_session_is_refused() {
        let m = json!({"method":"Target.detachFromTarget","params":{"sessionId":"S9"}});
        assert_eq!(err_code(&client_message(&m, &scope())), -32001);
    }

    #[test]
    fn detach_from_foreign_target_is_refused() {
        let m = json!({"method":"Target.detachFromTarget","params":{"targetId":"H1"}});
        assert_eq!(err_code(&client_message(&m, &scope())), -32000);
        let m = json!({"method":"Target.detachFromTarget","params":{}});
        assert_eq!(err_code(&client_message(&m, &scope())), -32000);
    }

    #[test]
    fn detach_from_own_session_passes() {
        sess_ok(&client_message(
            &json!({"method":"Target.detachFromTarget","params":{"sessionId":"S1"}}),
            &scope(),
        ));
        sess_ok(&client_message(
            &json!({"method":"Target.detachFromTarget","params":{"targetId":"T1"}}),
            &scope(),
        ));
    }

    #[test]
    fn browser_level_auto_attach_never_waits_for_debugger() {
        let m = json!({"id":1,"method":"Target.setAutoAttach","params":{"autoAttach":true,"waitForDebuggerOnStart":true,"flatten":true}});
        assert_eq!(
            client_message(&m, &scope()),
            ClientDecision::ForwardRewritten(
                json!({"id":1,"method":"Target.setAutoAttach","params":{"autoAttach":true,"waitForDebuggerOnStart":false,"flatten":true}})
            )
        );
    }

    #[test]
    fn session_level_auto_attach_passes_unchanged() {
        let m = json!({"method":"Target.setAutoAttach","sessionId":"S1","params":{"autoAttach":true,"waitForDebuggerOnStart":true}});
        sess_ok(&client_message(&m, &scope()));
    }

    #[test]
    fn child_target_events_of_own_session_are_forwarded() {
        for method in ["Target.attachedToTarget", "Target.detachedFromTarget"] {
            let m = json!({"method":method,"sessionId":"S1","params":{"targetInfo":{"targetId":"CHILD"}}});
            assert_eq!(
                browser_message(m.clone(), None, &scope()),
                BrowserDecision::Forward(m)
            );
        }
    }

    #[test]
    fn attached_event_for_foreign_target_is_dropped() {
        let m = json!({"method":"Target.attachedToTarget","params":{"sessionId":"X","targetInfo":{"targetId":"H1"}}});
        assert_eq!(browser_message(m, None, &scope()), BrowserDecision::Drop);
    }

    #[test]
    fn relayed_messages_of_foreign_non_flat_sessions_are_dropped() {
        let foreign = json!({"method":"Target.receivedMessageFromTarget","params":{"sessionId":"X","message":"{}"}});
        assert_eq!(
            browser_message(foreign, None, &scope()),
            BrowserDecision::Drop
        );
        let missing =
            json!({"method":"Target.receivedMessageFromTarget","params":{"message":"{}"}});
        assert_eq!(
            browser_message(missing, None, &scope()),
            BrowserDecision::Drop
        );
        let own = json!({"method":"Target.receivedMessageFromTarget","params":{"sessionId":"S1","message":"{}"}});
        assert_eq!(
            browser_message(own.clone(), None, &scope()),
            BrowserDecision::Forward(own)
        );
    }

    #[test]
    fn activate_target_is_refused() {
        let m = json!({"method":"Target.activateTarget","params":{"targetId":"T1"}});
        assert_eq!(err_code(&client_message(&m, &scope())), -32601);
    }

    #[test]
    fn file_and_chrome_urls_are_refused_for_navigate_and_create() {
        for url in [
            "file:///etc/passwd",
            "chrome://settings",
            "javascript:alert(1)",
            " FILE:///x",
            "chrome-extension://abc/x.html",
        ] {
            let nav = json!({"method":"Page.navigate","sessionId":"S1","params":{"url":url}});
            let create = json!({"method":"Target.createTarget","params":{"url":url}});
            for m in [nav, create] {
                match client_message(&m, &scope()) {
                    ClientDecision::Error { code, message } => {
                        assert_eq!(code, -32000, "{url}");
                        assert!(message.contains("url scheme"));
                    }
                    other => panic!("{url}: {other:?}"),
                }
            }
        }
        let nav = json!({"method":"Page.navigate","sessionId":"S1","params":{}});
        assert_eq!(err_code(&client_message(&nav, &scope())), -32000);
    }

    #[test]
    fn http_https_data_and_blank_urls_pass() {
        for url in [
            "http://a.test",
            "HTTPS://a.test/x",
            "data:text/html,hi",
            "about:blank",
            "  https://a.test",
        ] {
            assert!(normalize_url(url).is_some(), "{url}");
            let nav = json!({"method":"Page.navigate","sessionId":"S1","params":{"url":url}});
            assert!(
                matches!(
                    client_message(&nav, &scope()),
                    ClientDecision::ForwardRewritten(_)
                ),
                "{url}"
            );
        }
        assert_eq!(normalize_url("about:srcdoc"), None);
    }

    fn refused_both(url: &str) {
        let nav = json!({"method":"Page.navigate","sessionId":"S1","params":{"url":url}});
        let create = json!({"method":"Target.createTarget","params":{"url":url}});
        for m in [nav, create] {
            assert_eq!(err_code(&client_message(&m, &scope())), -32000, "{url:?}");
        }
    }

    #[test]
    fn unicode_whitespace_prefix_does_not_bypass_url_check() {
        for url in [
            "\u{a0}https:/../../sw.js",
            "\u{3000}file:///etc/passwd",
            "\u{2003}chrome://settings",
        ] {
            refused_both(url);
        }
    }

    #[test]
    fn urls_are_normalized_before_use() {
        let create =
            json!({"method":"Target.createTarget","params":{"url":"  HTTPS://Example.com/a b "}});
        assert_eq!(
            client_message(&create, &scope()),
            ClientDecision::CreateTab {
                url: "https://example.com/a%20b".into()
            }
        );
        let nav = json!({"id":2,"method":"Page.navigate","sessionId":"S1","params":{"url":"  HTTPS://Example.com/a b ","frameId":"F"}});
        assert_eq!(
            client_message(&nav, &scope()),
            ClientDecision::ForwardRewritten(
                json!({"id":2,"method":"Page.navigate","sessionId":"S1","params":{"url":"https://example.com/a%20b","frameId":"F"}})
            )
        );
    }

    #[test]
    fn scheme_less_and_relative_strings_are_refused() {
        for url in [
            "example.com",
            "/x",
            "//evil.test/x",
            "https:/../../sw.js",
            "https:sw.js",
            "https:///x",
        ] {
            refused_both(url);
        }
    }

    #[test]
    fn embedded_tab_or_newline_is_refused() {
        for url in [
            "ht\ttps://a.test",
            "https://a.test/\nx",
            "https://a.test/\rx",
        ] {
            refused_both(url);
        }
    }

    #[test]
    fn foreign_session_response_with_target_infos_is_dropped() {
        let m = json!({"id":3,"sessionId":"S9","result":{"targetInfos":[{"targetId":"T1"}]}});
        assert_eq!(browser_message(m, None, &scope()), BrowserDecision::Drop);
        let ok = json!({"id":3,"sessionId":"S1","result":{"targetInfos":[{"targetId":"H1"},{"targetId":"T1"}]}});
        assert_eq!(
            browser_message(ok, None, &scope()),
            BrowserDecision::Forward(
                json!({"id":3,"sessionId":"S1","result":{"targetInfos":[{"targetId":"T1"}]}})
            )
        );
    }

    #[test]
    fn target_infos_are_filtered_even_without_request_method() {
        let m = json!({"id":1,"result":{"targetInfos":[{"targetId":"H1"},{"targetId":"T1"}]}});
        assert_eq!(
            browser_message(m, None, &scope()),
            BrowserDecision::Forward(json!({"id":1,"result":{"targetInfos":[{"targetId":"T1"}]}}))
        );
    }

    #[test]
    fn response_for_foreign_session_is_dropped() {
        let m = json!({"id":3,"sessionId":"S9","result":{}});
        assert_eq!(
            browser_message(m, Some("Page.enable"), &scope()),
            BrowserDecision::Drop
        );
    }

    #[test]
    fn browser_level_network_enable_is_refused() {
        let m = json!({"method":"Network.enable"});
        match client_message(&m, &scope()) {
            ClientDecision::Error { message, .. } => assert!(message.contains("browser-level")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn storage_get_cookies_on_own_session_is_not_allowed() {
        let m = json!({"method":"Storage.getCookies","sessionId":"S1"});
        assert_eq!(err_code(&client_message(&m, &scope())), -32601);
    }

    #[test]
    fn end_of_a_target_the_connection_was_told_about_is_forwarded_once_it_left_the_scope() {
        let mut told = Told::default();
        told.on_sent(
            &json!({"method":"Target.targetCreated","params":{"targetInfo":{"targetId":"GONE"}}}),
        );
        told.on_sent(&json!({"method":"Target.attachedToTarget","params":{"sessionId":"SG","targetInfo":{"targetId":"GONE"}}}));
        let destroyed = json!({"method":"Target.targetDestroyed","params":{"targetId":"GONE"}});
        // GONE is not in scope() any more.
        assert_eq!(
            browser_message_told(destroyed.clone(), None, &scope(), &told),
            BrowserDecision::Forward(destroyed.clone())
        );
        let detached = json!({"method":"Target.detachedFromTarget","params":{"sessionId":"SG"}});
        assert_eq!(
            browser_message_told(detached.clone(), None, &scope(), &told),
            BrowserDecision::Forward(detached)
        );
        // Once told, it is forgotten: a repeat is dropped again.
        told.on_sent(&destroyed);
        told.on_sent(&json!({"method":"Target.detachedFromTarget","params":{"sessionId":"SG"}}));
        assert_eq!(
            browser_message_told(destroyed, None, &scope(), &told),
            BrowserDecision::Drop
        );
    }

    #[test]
    fn detached_with_a_session_id_needs_the_session_to_be_known() {
        let detached = json!({"method":"Target.detachedFromTarget","params":{"sessionId":"SZ","targetId":"GONE"}});
        let mut told = Told::default();
        told.on_sent(
            &json!({"method":"Target.targetCreated","params":{"targetInfo":{"targetId":"GONE"}}}),
        );
        assert_eq!(
            browser_message_told(detached.clone(), None, &scope(), &told),
            BrowserDecision::Drop
        );
        told.on_sent(&json!({"id":1,"result":{"sessionId":"SZ"}}));
        assert_eq!(
            browser_message_told(detached.clone(), None, &scope(), &told),
            BrowserDecision::Forward(detached)
        );
    }

    #[test]
    fn end_of_a_target_the_connection_never_heard_of_is_dropped() {
        let mut told = Told::default();
        told.on_sent(
            &json!({"method":"Target.targetCreated","params":{"targetInfo":{"targetId":"T1"}}}),
        );
        for m in [
            json!({"method":"Target.targetDestroyed","params":{"targetId":"H1"}}),
            json!({"method":"Target.detachedFromTarget","params":{"sessionId":"S9"}}),
            json!({"method":"Target.detachedFromTarget","params":{"sessionId":"S9","targetId":"H1"}}),
            json!({"method":"Target.targetDestroyed","params":{}}),
        ] {
            assert_eq!(
                browser_message_told(m, None, &scope(), &told),
                BrowserDecision::Drop
            );
        }
    }

    #[test]
    fn listings_and_replies_make_a_target_known() {
        let mut told = Told::default();
        told.on_sent(&json!({"id":1,"result":{"targetInfos":[{"targetId":"A"},{"targetId":"B"}]}}));
        told.on_sent(&json!({"id":2,"result":{"targetId":"C"}}));
        told.on_sent(&json!({"id":3,"result":{"sessionId":"SX"}}));
        for t in ["A", "B", "C"] {
            let m = json!({"method":"Target.targetDestroyed","params":{"targetId":t}});
            assert!(matches!(
                browser_message_told(m, None, &scope(), &told),
                BrowserDecision::Forward(_)
            ));
        }
        let m = json!({"method":"Target.detachedFromTarget","params":{"sessionId":"SX"}});
        assert!(matches!(
            browser_message_told(m, None, &scope(), &told),
            BrowserDecision::Forward(_)
        ));
    }
}
