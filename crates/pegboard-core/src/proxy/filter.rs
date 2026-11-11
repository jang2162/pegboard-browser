//! Session-scoped CDP filter decisions. Pure functions over JSON messages.
//!
//! This is the isolation boundary between an agent session and the rest of the
//! browser: default-deny, allowlisted methods, and only the session's own
//! targets and CDP sessions are visible. Rule tables are ordered; the first
//! matching row wins.

use std::collections::HashSet;

use serde_json::Value;

/// What a session may see: its browser context, its targets and its CDP sessions.
pub struct Scope {
    pub context: String,
    pub targets: HashSet<String>,
    pub cdp_sessions: HashSet<String>,
}

#[derive(Debug, PartialEq)]
pub enum ClientDecision {
    /// Pass to the browser unchanged.
    Forward,
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
    Adopt { target_id: String, forward: Value },
}

/// The 23 methods agent-browser used in phase 0 plus the additions for phase 1.
/// `Page.bringToFront` is deliberately absent: it steals the human's focus.
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
    "Target.activateTarget",
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
    "Target.activateTarget",
    "Target.getTargetInfo",
];
/// Browser-level (no sessionId) calls that would reach beyond the agent's context.
const BROWSER_LEVEL_EXACT: &[&str] = &[
    "Browser.grantPermissions",
    "Browser.resetPermissions",
    "Browser.setDownloadBehavior",
];
const BROWSER_LEVEL_PREFIXES: &[&str] = &["Storage.", "Network.", "Fetch."];
const TARGET_EVENTS: &[&str] = &[
    "Target.targetCreated",
    "Target.targetInfoChanged",
    "Target.targetDestroyed",
    "Target.attachedToTarget",
    "Target.detachedFromTarget",
    "Target.targetCrashed",
];

fn error(code: i64, message: impl Into<String>) -> ClientDecision {
    ClientDecision::Error { code, message: message.into() }
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
            None => ClientDecision::CreateTab { url: "about:blank".into() },
            Some(Value::String(url)) => ClientDecision::CreateTab { url: url.clone() },
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
        return error(-32000, "browser-level call is not allowed; use a page session");
    }

    // 6. Allowlist.
    if !ALLOWED_METHODS.contains(&method) {
        return error(-32601, format!("'{method}' is not allowed in a pegboard session"));
    }

    // 7. Everything else.
    ClientDecision::Forward
}

fn target_id_of(params: &Value) -> Option<&str> {
    params
        .pointer("/targetInfo/targetId")
        .or_else(|| params.get("targetId"))
        .and_then(Value::as_str)
}

/// Decide what to do with a message from the browser. `request_method` is the
/// request method when `msg` is a response to a forwarded request. Ordered rules.
pub fn browser_message(mut msg: Value, request_method: Option<&str>, scope: &Scope) -> BrowserDecision {
    let method = msg.get("method").and_then(Value::as_str).map(str::to_owned);

    // 1. Target.getTargets response: hide targets outside the session.
    if method.is_none() && request_method == Some("Target.getTargets") {
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

        // 2. Popup of an own tab in the session's context is adopted.
        if method == "Target.targetCreated" {
            let info = params.get("targetInfo");
            let opener = info.and_then(|i| i.get("openerId")).and_then(Value::as_str);
            let context = info.and_then(|i| i.get("browserContextId")).and_then(Value::as_str);
            if let (Some(opener), Some(context), Some(target_id)) =
                (opener, context, target_id_of(params))
            {
                if scope.targets.contains(opener) && context == scope.context {
                    let target_id = target_id.to_owned();
                    return BrowserDecision::Adopt { target_id, forward: msg };
                }
            }
        }

        // 3. Target events about foreign (or unidentifiable) targets.
        if TARGET_EVENTS.contains(&method)
            && !target_id_of(params).is_some_and(|id| scope.targets.contains(id))
        {
            return BrowserDecision::Drop;
        }
    }

    // 4. Anything on a CDP session that is not this session's.
    if let Some(sid) = msg.get("sessionId") {
        if !sid.as_str().is_some_and(|s| scope.cdp_sessions.contains(s)) {
            return BrowserDecision::Drop;
        }
    }

    // 5. Everything else (including responses to forwarded requests).
    BrowserDecision::Forward(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope() -> Scope {
        Scope {
            context: "AGENT".into(),
            targets: ["T1".to_string()].into(),
            cdp_sessions: ["S1".to_string()].into(),
        }
    }

    fn err_code(d: &ClientDecision) -> i64 {
        match d {
            ClientDecision::Error { code, .. } => *code,
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn create_target_is_never_forwarded() {
        let m = json!({"method":"Target.createTarget","params":{"url":"https://a.test","browserContextId":"HUMAN"}});
        assert_eq!(client_message(&m, &scope()), ClientDecision::CreateTab { url: "https://a.test".into() });
    }

    #[test]
    fn create_target_without_url_uses_blank() {
        let m = json!({"method":"Target.createTarget","params":{}});
        assert_eq!(client_message(&m, &scope()), ClientDecision::CreateTab { url: "about:blank".into() });
        let m = json!({"method":"Target.createTarget"});
        assert_eq!(client_message(&m, &scope()), ClientDecision::CreateTab { url: "about:blank".into() });
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
        for m in [json!({"id":1}), json!({"id":1,"method":5}), json!({"id":1,"method":null})] {
            assert!(matches!(client_message(&m, &scope()), ClientDecision::Error { .. }), "{m}");
        }
    }

    #[test]
    fn non_object_params_are_refused() {
        for p in [json!("x"), json!([1]), json!(null), json!(3)] {
            let m = json!({"method":"Page.navigate","sessionId":"S1","params":p});
            assert!(matches!(client_message(&m, &scope()), ClientDecision::Error { .. }), "{m}");
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
        assert_eq!(browser_message(m.clone(), None, &scope()), BrowserDecision::Forward(m));
    }

    #[test]
    fn popup_of_own_tab_is_adopted() {
        let m = json!({"method":"Target.targetCreated","params":{"targetInfo":{
            "targetId":"T2","openerId":"T1","browserContextId":"AGENT"}}});
        assert_eq!(
            browser_message(m.clone(), None, &scope()),
            BrowserDecision::Adopt { target_id: "T2".into(), forward: m }
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
        let created = json!({"method":"Target.targetCreated","params":{"targetInfo":{"targetId":"H2"}}});
        assert_eq!(browser_message(created, None, &scope()), BrowserDecision::Drop);
        let destroyed = json!({"method":"Target.targetDestroyed","params":{"targetId":"H1"}});
        assert_eq!(browser_message(destroyed, None, &scope()), BrowserDecision::Drop);
    }

    #[test]
    fn events_of_foreign_sessions_are_dropped() {
        let m = json!({"method":"Page.loadEventFired","sessionId":"S9"});
        assert_eq!(browser_message(m, None, &scope()), BrowserDecision::Drop);
    }
}
