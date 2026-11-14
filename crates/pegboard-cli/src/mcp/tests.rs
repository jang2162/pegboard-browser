//! Protocol and tool tests against a stubbed core API.

use super::*;
use pegboard_protocol::ActionClass;

const FOREIGN: &str = "s_foreignn";
const FOREIGN_TOKEN: &str = "foreigntokenforeigntokenff";
/// A session the daemon does not have.
const MISSING: &str = "s_missingg";

#[derive(Default)]
struct StubState {
    /// id, token, name, policy.
    sessions: Vec<(String, String, String, Value)>,
    /// Ids of sessions that existed (their recordings remain).
    ended: Vec<String>,
    next: usize,
    calls: Vec<(String, Value)>,
}

/// A core API that answers like the daemon, and puts tokens everywhere it can: a `token`
/// field in every session and recording it returns, and the session's token in the free
/// text of its recording.
#[derive(Clone)]
struct Stub {
    state: Arc<Mutex<StubState>>,
    start: Result<(), String>,
    /// Put own tokens into the free text of recordings (which only the scrub can catch).
    leak_in_text: bool,
    /// Answer session.create with a session but a reply that does not decode.
    bad_create: bool,
}

fn token_of(id: &str) -> String {
    format!("tok{}zzzzzzzzzzzzzzz", &id[2..])
}

fn not_found(id: &str) -> RpcError {
    RpcError {
        code: codes::SESSION_NOT_FOUND.into(),
        message: format!("no session {id}"),
        data: None,
    }
}

impl Stub {
    fn new() -> Stub {
        let stub = Stub {
            state: Arc::default(),
            start: Ok(()),
            leak_in_text: true,
            bad_create: false,
        };
        stub.state.lock().unwrap().sessions.push((
            FOREIGN.into(),
            FOREIGN_TOKEN.into(),
            "theirs".into(),
            json!({}),
        ));
        stub
    }

    fn methods(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .calls
            .iter()
            .map(|(m, _)| m.clone())
            .collect()
    }

    fn calls_of(&self, method: &str) -> Vec<Value> {
        self.state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }

    /// Drops a session as if it had been closed elsewhere.
    fn close_elsewhere(&self, id: &str) {
        let mut st = self.state.lock().unwrap();
        st.sessions.retain(|s| s.0 != id);
        st.ended.push(id.into());
    }

    fn info(s: &(String, String, String, Value)) -> Value {
        json!({
            "id": s.0, "name": s.2, "profile": "default", "tabGroupId": 1,
            "targets": ["T1"], "state": "active", "recordingId": format!("r_{}", &s.0[2..]),
            "createdAt": "2026-01-01T00:00:00Z", "lastActivityAt": "2026-01-01T00:00:00Z",
            "token": s.1, "policy": s.3,
        })
    }
}

impl Core for Stub {
    fn ensure_running(&mut self) -> Result<(), String> {
        self.start.clone()
    }

    fn call(&mut self, method: &str, p: Value) -> Result<Value, RpcError> {
        let mut st = self.state.lock().unwrap();
        st.calls.push((method.to_string(), p.clone()));
        let id = p["sessionId"].as_str().unwrap_or("").to_string();
        let find = |st: &StubState| st.sessions.iter().position(|s| s.0 == id);
        let with_token = |st: &StubState| {
            find(st).filter(|&i| p["token"].as_str() == Some(st.sessions[i].1.as_str()))
        };
        match method {
            "session.create" => {
                st.next += 1;
                let n = st.next;
                let alphabet = b"abcdefghijklmnopqrstuvwxyz234567";
                let id = format!(
                    "s_mine{}{}aa",
                    alphabet[n / 32] as char,
                    alphabet[n % 32] as char
                );
                let s = (
                    id.clone(),
                    token_of(&id),
                    p["name"].as_str().unwrap_or("").to_string(),
                    p["policy"].clone(),
                );
                st.sessions.push(s.clone());
                if self.bad_create {
                    return Ok(json!({ "id": s.0, "token": s.1, "state": 7 }));
                }
                Ok(Stub::info(&s))
            }
            "session.list" => Ok(Value::Array(st.sessions.iter().map(Stub::info).collect())),
            "session.info" => match with_token(&st) {
                Some(i) => Ok(Stub::info(&st.sessions[i])),
                None => Err(not_found(&id)),
            },
            "session.info_any" => match find(&st) {
                Some(i) => Ok(Stub::info(&st.sessions[i])),
                None => Err(not_found(&id)),
            },
            "session.close" | "session.close_any" => {
                let i = if method == "session.close" {
                    with_token(&st)
                } else {
                    find(&st)
                };
                match i {
                    Some(i) => {
                        let s = st.sessions.remove(i);
                        st.ended.push(s.0.clone());
                        Ok(Stub::info(&s))
                    }
                    None => Err(not_found(&id)),
                }
            }
            "files.list" => match with_token(&st) {
                Some(i) => Ok(json!([{
                    "name": "a.txt", "bytes": 3, "modifiedAt": "2026-01-01T00:00:00Z",
                    "token": st.sessions[i].1,
                }])),
                None => Err(not_found(&id)),
            },
            "recording.list" => {
                let all: Vec<Value> = st
                    .sessions
                    .iter()
                    .map(|s| (s.0.clone(), s.1.clone()))
                    .chain(st.ended.iter().map(|id| (id.clone(), token_of(id))))
                    .map(|(id, token)| {
                        json!({
                            "format": 1, "recordingId": format!("r_{}", &id[2..]),
                            "sessionId": id, "sessionName": "n", "profile": "default",
                            "startedAt": "t0", "endedAt": null, "chromeVersion": "157",
                            "agentBrowserVersion": "0.42.0", "token": token,
                        })
                    })
                    .collect();
                Ok(Value::Array(all))
            }
            "recording.get" => {
                let rid = p["recordingId"].as_str().unwrap_or("").to_string();
                let sid = format!("s_{}", &rid[2..]);
                let exists = st.sessions.iter().any(|s| s.0 == sid) || st.ended.contains(&sid);
                if !exists {
                    return Err(RpcError {
                        code: codes::RECORDING_NOT_FOUND.into(),
                        message: format!("no recording {rid}"),
                        data: None,
                    });
                }
                let token = st
                    .sessions
                    .iter()
                    .find(|s| s.0 == sid)
                    .map(|s| s.1.clone())
                    .unwrap_or_else(|| token_of(&sid));
                // Only an own session's token goes into free text (a foreign one cannot
                // be known to the server); every token goes into a `token` field.
                let free = if sid == FOREIGN || !self.leak_in_text {
                    "x".to_string()
                } else {
                    token.clone()
                };
                Ok(json!({
                    "manifest": {
                        "format": 1, "recordingId": rid, "sessionId": sid, "sessionName": "n",
                        "profile": "default", "startedAt": "t0", "endedAt": null,
                        "chromeVersion": "157", "agentBrowserVersion": "0.42.0", "token": token,
                    },
                    "actions": [{
                        "seq": 1, "ts": "t1", "sessionId": sid, "targetId": null, "url": null,
                        "command": "fill", "args": ["fill", "@e1", free], "ok": false,
                        "error": format!("failed near {free}"), "durationMs": 5,
                        "redacted": false, "token": token,
                    }],
                    "token": token,
                }))
            }
            "browser.status" => Ok(json!({
                "running": true, "pid": 42, "chromeVersion": "157", "sessions": st.sessions.len(),
                "stopping": false,
            })),
            "browser.stop" => {
                st.sessions.clear();
                Ok(json!({}))
            }
            "profile.list" => Ok(json!([{ "name": "default", "kind": "default", "sessions": [] }])),
            "profile.create" => Ok(json!({ "name": p["name"], "kind": "named", "sessions": [] })),
            "profile.delete" => Ok(json!({})),
            other => Err(RpcError {
                code: codes::UNKNOWN_METHOD.into(),
                message: format!("unknown method {other:?}"),
                data: None,
            }),
        }
    }
}

fn opts(operator: bool, policy: SessionPolicy) -> Options {
    Options {
        operator,
        tools: vec![Bundle::Core, Bundle::Tabs],
        policy,
    }
}

struct Harness {
    server: Server,
    stub: Stub,
    owned: Arc<Mutex<Owned>>,
    next_id: u64,
    /// Every line the server wrote.
    out: Vec<String>,
}

impl Harness {
    fn new(operator: bool) -> Harness {
        Harness::with(
            opts(operator, SessionPolicy::default()),
            Stub::new(),
            Owned::default(),
        )
    }

    fn with(opts: Options, stub: Stub, owned: Owned) -> Harness {
        let owned = Arc::new(Mutex::new(owned));
        Harness {
            server: Server::new(&opts, Box::new(stub.clone()), owned.clone()),
            stub,
            owned,
            next_id: 0,
            out: Vec::new(),
        }
    }

    fn raw(&mut self, line: &str) -> Option<Value> {
        let reply = self.server.handle_line(line.as_bytes())?;
        assert!(!reply.contains('\n'), "one line: {reply}");
        self.out.push(reply.clone());
        Some(serde_json::from_str(&reply).unwrap())
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let line =
            json!({ "jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params });
        let reply = self.raw(&line.to_string()).expect("a reply");
        assert_eq!(reply["id"], self.next_id, "{reply}");
        assert_eq!(reply["jsonrpc"], "2.0");
        reply
    }

    /// (isError, text) of a tool call.
    fn call(&mut self, tool: &str, args: Value) -> (bool, String) {
        let reply = self.request("tools/call", json!({ "name": tool, "arguments": args }));
        let result = &reply["result"];
        assert!(result.is_object(), "not a tool result: {reply}");
        let text = result["content"][0]["text"].as_str().unwrap().to_string();
        assert_eq!(result["content"][0]["type"], "text");
        (result["isError"] == true, text)
    }

    fn tool_names(&mut self) -> Vec<String> {
        let reply = self.request("tools/list", json!({}));
        reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn create(&mut self, args: Value) -> String {
        let (err, text) = self.call("pegboard_session_create", args);
        assert!(!err, "{text}");
        let id = text
            .split_whitespace()
            .find(|w| w.starts_with("s_"))
            .unwrap()
            .to_string();
        id
    }
}

const DEFAULT_TOOLS: [&str; 8] = [
    "pegboard_session_create",
    "pegboard_session_list",
    "pegboard_session_info",
    "pegboard_session_close",
    "pegboard_recording_list",
    "pegboard_recording_get",
    "pegboard_files_list",
    "pegboard_browser_status",
];

const OPERATOR_ONLY: [&str; 4] = [
    "pegboard_profile_list",
    "pegboard_profile_create",
    "pegboard_profile_delete",
    "pegboard_browser_stop",
];

#[test]
fn initialize_and_tools_list() {
    let mut h = Harness::new(false);
    for (asked, answered) in [
        ("2025-03-26", "2025-03-26"),
        ("2024-11-05", "2024-11-05"),
        ("2025-06-18", "2025-06-18"),
        ("1999-01-01", "2025-06-18"),
    ] {
        let r = h.request(
            "initialize",
            json!({ "protocolVersion": asked, "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } }),
        );
        assert_eq!(r["result"]["protocolVersion"], answered, "{r}");
        assert!(r["result"]["capabilities"]["tools"].is_object(), "{r}");
        assert_eq!(r["result"]["serverInfo"]["name"], "pegboard");
    }
    assert!(h
        .raw(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .is_none());
    assert_eq!(h.request("ping", json!({}))["result"], json!({}));
    assert_eq!(h.tool_names(), DEFAULT_TOOLS);
    let list = h.request("tools/list", json!({}));
    for t in list["result"]["tools"].as_array().unwrap() {
        assert!(
            t["description"].as_str().is_some_and(|d| !d.is_empty()),
            "{t}"
        );
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        assert_eq!(t["inputSchema"]["additionalProperties"], false, "{t}");
    }
    let mut op = Harness::new(true);
    let mut expected: Vec<&str> = DEFAULT_TOOLS.to_vec();
    expected.extend(OPERATOR_ONLY);
    assert_eq!(op.tool_names(), expected);
    // Listing tools does not start or call anything.
    assert!(h.stub.methods().is_empty());
}

#[test]
fn default_mode_has_no_profile_or_stop_tools() {
    let mut h = Harness::new(false);
    let names = h.tool_names();
    assert!(
        names
            .iter()
            .all(|n| !n.contains("profile") && !n.contains("stop")),
        "{names:?}"
    );
    for tool in OPERATOR_ONLY {
        let r = h.request("tools/call", json!({ "name": tool, "arguments": {} }));
        assert_eq!(r["error"]["code"], INVALID_PARAMS, "{r}");
    }
    assert!(h.stub.methods().is_empty(), "{:?}", h.stub.methods());
}

#[test]
fn policy_keys_are_refused_in_default_mode() {
    let server_policy = SessionPolicy {
        deny: vec![ActionClass::Eval],
        ..SessionPolicy::default()
    };
    let mut h = Harness::with(
        opts(false, server_policy.clone()),
        Stub::new(),
        Owned::default(),
    );
    let schema = h.request("tools/list", json!({}))["result"]["tools"][0]["inputSchema"].clone();
    let mut keys: Vec<&String> = schema["properties"].as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["name", "profile", "url"]);
    for args in [
        json!({ "name": "x", "allow": ["upload"] }),
        json!({ "name": "x", "policy": { "allow": ["state"] } }),
        json!({ "policy": {} }),
        json!({ "allowDomains": [] }),
        json!({ "noExpire": true }),
        json!({ "uploadDir": "/" }),
    ] {
        let (err, text) = h.call("pegboard_session_create", args.clone());
        assert!(err, "{args}: {text}");
        assert!(text.starts_with("invalid_params: "), "{text}");
        assert!(text.contains("policy"), "{text}");
    }
    assert!(h.stub.calls_of("session.create").is_empty());

    // The server's policy is what every session gets, and the result tells it.
    let (err, text) = h.call("pegboard_session_create", json!({ "name": "x" }));
    assert!(!err, "{text}");
    assert!(text.contains("deny eval"), "{text}");
    let sent = h.stub.calls_of("session.create");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["policy"]["deny"], json!(["eval"]), "{}", sent[0]);
    assert_eq!(sent[0]["name"], "x");

    // The operator may give one, which is checked.
    let mut op = Harness::with(opts(true, server_policy), Stub::new(), Owned::default());
    let (err, text) = op.call(
        "pegboard_session_create",
        json!({ "policy": { "allow": ["state"], "allowDomains": ["a.test"] } }),
    );
    assert!(!err, "{text}");
    assert!(
        text.contains("allow state") && text.contains("allow-domain a.test"),
        "{text}"
    );
    let sent = op.stub.calls_of("session.create");
    assert_eq!(sent[0]["policy"]["allow"], json!(["state"]));
    for bad in [
        json!({ "policy": { "allow": ["eval"] } }),
        json!({ "policy": { "denny": ["eval"] } }),
        json!({ "policy": { "allow": ["upload"] } }),
    ] {
        let (err, text) = op.call("pegboard_session_create", bad.clone());
        assert!(err && text.starts_with("invalid_params: "), "{bad}: {text}");
    }
    assert_eq!(op.stub.calls_of("session.create").len(), 1);
}

#[test]
fn foreign_session_ids_look_like_missing_ones() {
    // An adopted handle whose session the daemon does not have: the daemon's own answer.
    let mut owned = Owned::default();
    owned
        .adopt(&format!("{MISSING}.{}", token_of(MISSING)))
        .unwrap();
    let mut h = Harness::with(opts(false, SessionPolicy::default()), Stub::new(), owned);
    let mine = h.create(json!({}));
    let same = |h: &mut Harness, tool: &str, key: &str, foreign: &str, missing: &str| {
        let (e1, t1) = h.call(tool, json!({ key: foreign }));
        let (e2, t2) = h.call(tool, json!({ key: missing }));
        assert!(e1 && e2, "{tool}: {t1} / {t2}");
        assert_eq!(
            t1.replace(&foreign[2..], "ID"),
            t2.replace(&missing[2..], "ID"),
            "{tool}"
        );
        t1
    };
    let t = same(
        &mut h,
        "pegboard_session_info",
        "sessionId",
        FOREIGN,
        MISSING,
    );
    assert_eq!(t, format!("session_not_found: no session {FOREIGN}"));
    same(
        &mut h,
        "pegboard_session_close",
        "sessionId",
        FOREIGN,
        MISSING,
    );
    same(&mut h, "pegboard_files_list", "sessionId", FOREIGN, MISSING);
    // A recording of a session that ended and of one that never existed. (`same` also
    // compares with a missing one the server cannot know of.)
    let t = same(
        &mut h,
        "pegboard_recording_get",
        "recordingId",
        "r_foreignn",
        "r_nothere2",
    );
    assert_eq!(t, "recording_not_found: no recording r_foreignn");

    let (_, list) = h.call("pegboard_session_list", json!({}));
    assert!(list.contains(&mine) && !list.contains(FOREIGN), "{list}");
    let (_, recs) = h.call("pegboard_recording_list", json!({}));
    assert!(
        recs.contains(&mine[2..]) && !recs.contains(&FOREIGN[2..]),
        "{recs}"
    );
    // Its own recording stays readable after the session is closed.
    let (err, text) = h.call("pegboard_session_close", json!({ "sessionId": mine }));
    assert!(!err, "{text}");
    let (err, text) = h.call(
        "pegboard_recording_get",
        json!({ "recordingId": format!("r_{}", &mine[2..]) }),
    );
    assert!(!err, "{text}");
    assert!(text.contains("fill @e1"), "{text}");

    // The foreign session was never touched, and no operator method was used.
    let methods = h.stub.methods();
    assert!(methods.iter().all(|m| !m.ends_with("_any")), "{methods:?}");
    for p in h.stub.state.lock().unwrap().calls.iter().map(|c| &c.1) {
        assert_ne!(p["sessionId"], FOREIGN, "{p}");
        assert_ne!(p["recordingId"], "r_foreignn", "{p}");
    }
    assert!(h
        .stub
        .state
        .lock()
        .unwrap()
        .sessions
        .iter()
        .any(|s| s.0 == FOREIGN));
}

#[test]
fn operator_mode_reaches_other_sessions_but_files_stay_own() {
    let mut h = Harness::new(true);
    let (err, list) = h.call("pegboard_session_list", json!({}));
    assert!(!err && list.contains(FOREIGN), "{list}");
    let (err, text) = h.call("pegboard_session_info", json!({ "sessionId": FOREIGN }));
    assert!(!err && text.contains("policy:"), "{text}");
    let (err, text) = h.call(
        "pegboard_recording_get",
        json!({ "recordingId": "r_foreignn" }),
    );
    assert!(!err, "{text}");
    let (err, text) = h.call("pegboard_files_list", json!({ "sessionId": FOREIGN }));
    assert!(
        err && text == format!("session_not_found: no session {FOREIGN}"),
        "{text}"
    );
    let (err, text) = h.call("pegboard_session_close", json!({ "sessionId": FOREIGN }));
    assert!(!err, "{text}");
    assert_eq!(h.stub.calls_of("session.close_any").len(), 1);
    assert_eq!(h.stub.calls_of("session.info_any").len(), 1);
    // Its own sessions go through the token, as in the default mode.
    let mine = h.create(json!({}));
    let (err, _) = h.call("pegboard_session_info", json!({ "sessionId": mine }));
    assert!(!err);
    assert_eq!(h.stub.calls_of("session.info").len(), 1);
    let (err, text) = h.call("pegboard_profile_create", json!({ "name": "work" }));
    assert!(!err && text.contains("work"), "{text}");
    let (err, text) = h.call("pegboard_browser_stop", json!({}));
    assert!(!err, "{text}");
    assert!(h.owned.lock().unwrap().ids().is_empty());
}

#[test]
fn results_never_contain_a_token() {
    for operator in [false, true] {
        let adopted = "s_adoptedd";
        let mut owned = Owned::default();
        owned
            .adopt(&format!("{adopted}.{}", token_of(adopted)))
            .unwrap();
        let stub = Stub::new();
        stub.state.lock().unwrap().sessions.push((
            adopted.into(),
            token_of(adopted),
            "given".into(),
            json!({}),
        ));
        let mut h = Harness::with(opts(operator, SessionPolicy::default()), stub, owned);
        let a = h.create(json!({ "name": "a" }));
        let b = h.create(json!({ "name": "b" }));
        let ids = [a.as_str(), b.as_str(), adopted, FOREIGN, MISSING];
        h.call("pegboard_session_list", json!({}));
        h.call("pegboard_recording_list", json!({}));
        h.call("pegboard_browser_status", json!({}));
        for id in ids {
            let rid = format!("r_{}", &id[2..]);
            h.call("pegboard_session_info", json!({ "sessionId": id }));
            h.call("pegboard_files_list", json!({ "sessionId": id }));
            h.call("pegboard_recording_get", json!({ "recordingId": rid }));
        }
        h.call("pegboard_session_close", json!({ "sessionId": a }));
        h.call(
            "pegboard_recording_get",
            json!({ "recordingId": format!("r_{}", &a[2..]) }),
        );
        h.call("pegboard_session_close", json!({ "sessionId": FOREIGN }));
        h.call("pegboard_session_create", json!({ "policy": {} }));
        h.call("pegboard_session_info", json!({ "sessionId": "not an id" }));
        if operator {
            h.call("pegboard_profile_list", json!({}));
            h.call("pegboard_browser_stop", json!({}));
        }
        let tokens: Vec<String> = ids
            .iter()
            .map(|id| token_of(id))
            .chain([FOREIGN_TOKEN.to_string()])
            .collect();
        // The stub did hand out tokens, so the test is not vacuous.
        assert!(h.stub.calls_of("recording.get").len() >= 3);
        let all = h.out.join("\n");
        assert!(all.contains(&a[2..]));
        for t in &tokens {
            assert!(
                !all.contains(t.as_str()),
                "operator={operator}: {t} in {all}"
            );
            // Nor in pieces of a handle.
            assert!(!all.contains(&format!(".{}", &t[..8])), "{all}");
        }
    }
}

#[test]
fn session_limit_is_16() {
    let mut owned = Owned::default();
    owned
        .adopt(&format!("s_adoptedd.{}", token_of("s_adoptedd")))
        .unwrap();
    let mut h = Harness::with(opts(false, SessionPolicy::default()), Stub::new(), owned);
    let ids: Vec<String> = (0..16).map(|_| h.create(json!({}))).collect();
    let (err, text) = h.call("pegboard_session_create", json!({}));
    assert!(err, "{text}");
    assert!(text.starts_with("limit_exceeded: "), "{text}");
    assert_eq!(h.stub.calls_of("session.create").len(), 16);
    // Closing one makes room.
    let (err, _) = h.call("pegboard_session_close", json!({ "sessionId": ids[0] }));
    assert!(!err);
    h.create(json!({}));
    // So does one that went away elsewhere (idle expiry, a person).
    h.stub.close_elsewhere(&ids[1]);
    h.create(json!({}));
    let (err, text) = h.call("pegboard_session_create", json!({}));
    assert!(err && text.starts_with("limit_exceeded: "), "{text}");
}

#[test]
fn malformed_input_does_not_stop_the_server() {
    let mut h = Harness::new(false);
    let r = h.raw("this is not json").unwrap();
    assert_eq!(r["error"]["code"], PARSE_ERROR);
    assert_eq!(r["id"], Value::Null);
    let r = h
        .raw(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#)
        .unwrap();
    assert_eq!(r["error"]["code"], INVALID_REQUEST);
    // Notifications (no id) get no answer, whatever they are.
    assert!(h.raw(r#"{"jsonrpc":"2.0","method":"ping"}"#).is_none());
    assert!(h.raw(r#"{"jsonrpc":"2.0","method":"nonsense"}"#).is_none());
    let r = h
        .raw(r#"{"jsonrpc":"2.0","id":"x","method":"resources/list"}"#)
        .unwrap();
    assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);
    assert_eq!(r["id"], "x");
    let r = h
        .raw(r#"{"jsonrpc":"2.0","id":{"a":1},"method":"ping"}"#)
        .unwrap();
    assert_eq!(r["error"]["code"], INVALID_REQUEST);
    let r = h
        .raw(r#"{"jsonrpc":"1.0","id":3,"method":"ping"}"#)
        .unwrap();
    assert_eq!(r["error"]["code"], INVALID_REQUEST);
    let r = h.raw(r#"{"jsonrpc":"2.0","id":4,"result":{}}"#);
    assert!(r.is_none(), "a response from the client is ignored: {r:?}");
    let r = h.raw(r#"{"jsonrpc":"2.0","id":4}"#).unwrap();
    assert_eq!(r["error"]["code"], INVALID_REQUEST);
    let r = h.raw(r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"pegboard_session_list","arguments":[1]}}"#).unwrap();
    assert_eq!(r["error"]["code"], INVALID_PARAMS);
    let r = h
        .raw(r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"arguments":{}}}"#)
        .unwrap();
    assert_eq!(r["error"]["code"], INVALID_PARAMS);
    assert!(h.raw("   ").is_none());

    // A 5 MiB line is skipped without being held, and the next line is read.
    let mut input = vec![b'x'; 5 * 1024 * 1024];
    input.extend_from_slice(b"\n{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\"}\n");
    input.extend_from_slice(b"last line without newline");
    let mut reader = std::io::BufReader::with_capacity(8192, &input[..]);
    assert_eq!(
        read_line_capped(&mut reader, MAX_LINE).unwrap(),
        Some(Line::TooLong)
    );
    let Some(Line::Text(next)) = read_line_capped(&mut reader, MAX_LINE).unwrap() else {
        panic!("no line after the long one");
    };
    let r: Value = serde_json::from_str(&h.server.handle_line(&next).unwrap()).unwrap();
    assert_eq!(r["id"], 9);
    assert_eq!(r["result"], json!({}));
    assert_eq!(
        read_line_capped(&mut reader, MAX_LINE).unwrap(),
        Some(Line::Text(b"last line without newline".to_vec()))
    );
    assert_eq!(read_line_capped(&mut reader, MAX_LINE).unwrap(), None);
    // Exactly at the limit is fine; one more is not.
    let mut exact = vec![b'y'; 100];
    exact.push(b'\n');
    let mut reader = std::io::BufReader::with_capacity(16, &exact[..]);
    assert!(
        matches!(read_line_capped(&mut reader, 100).unwrap(), Some(Line::Text(t)) if t.len() == 100)
    );
    let mut reader = std::io::BufReader::with_capacity(16, &exact[..]);
    assert_eq!(
        read_line_capped(&mut reader, 99).unwrap(),
        Some(Line::TooLong)
    );
    // The server answers the too-long line with a parse error.
    let r: Value = serde_json::from_str(&too_long_reply()).unwrap();
    assert_eq!(r["error"]["code"], PARSE_ERROR);
    assert_eq!(h.request("ping", json!({}))["result"], json!({}));
}

#[test]
fn a_daemon_that_could_not_start_is_told_at_the_first_tool_call() {
    let stub = Stub {
        start: Err("Chrome for Testing was not found".into()),
        ..Stub::new()
    };
    let mut h = Harness::with(
        opts(false, SessionPolicy::default()),
        stub,
        Owned::default(),
    );
    let r = h.request("initialize", json!({ "protocolVersion": "2025-06-18" }));
    assert!(r["result"].is_object(), "{r}");
    let (err, text) = h.call("pegboard_session_list", json!({}));
    assert!(err);
    assert_eq!(
        text,
        "not_running: pegboard is not running and could not be started: Chrome for Testing was not found"
    );
    assert!(h.stub.methods().is_empty());
}

#[test]
fn ending_closes_created_sessions_only() {
    let adopted = "s_adoptedd";
    let mut owned = Owned::default();
    owned
        .adopt(&format!("{adopted}.{}", token_of(adopted)))
        .unwrap();
    let stub = Stub::new();
    stub.state.lock().unwrap().sessions.push((
        adopted.into(),
        token_of(adopted),
        "given".into(),
        json!({}),
    ));
    let mut h = Harness::with(opts(false, SessionPolicy::default()), stub.clone(), owned);
    let a = h.create(json!({}));
    let b = h.create(json!({}));
    let connect_stub = stub.clone();
    close_created(&h.owned, &move || {
        Box::new(connect_stub.clone()) as Box<dyn Core>
    });
    let closed: Vec<Value> = stub.calls_of("session.close");
    let mut ids: Vec<&str> = closed
        .iter()
        .map(|p| p["sessionId"].as_str().unwrap())
        .collect();
    ids.sort();
    assert_eq!(ids, [a.as_str(), b.as_str()]);
    for p in &closed {
        assert_eq!(p["keepTabs"], false);
        assert_eq!(p["token"], token_of(p["sessionId"].as_str().unwrap()));
    }
    assert_eq!(h.owned.lock().unwrap().ids(), [SessionId(adopted.into())]);
    // A session whose creation finishes after that is closed at once.
    let (err, text) = h.call("pegboard_session_create", json!({}));
    assert!(err && text.starts_with("not_running: "), "{text}");
    assert_eq!(stub.calls_of("session.close").len(), 3);
    assert_eq!(h.owned.lock().unwrap().ids(), [SessionId(adopted.into())]);
}

#[test]
fn handles_are_adopted_only_when_well_formed() {
    let mut owned = Owned::default();
    let good = format!("s_adoptedd.{}", token_of("s_adoptedd"));
    owned.adopt(&good).unwrap();
    let id = SessionId("s_adoptedd".into());
    assert_eq!(owned.token(&id), Some(token_of("s_adoptedd").as_str()));
    assert_eq!(owned.created_count(), 0);
    let short = format!("s_adoptedd.{}", &token_of("s_adoptedd")[1..]);
    for bad in [
        short.as_str(),
        "s_adoptedd",
        "s_adoptedd.",
        "nonsense.tok",
        "s_BAD.abc",
        ".abc",
        "s_adoptedd.a b",
    ] {
        let e = Owned::default().adopt(bad).unwrap_err();
        assert!(
            !e.contains(bad) || bad.is_empty(),
            "the value is not echoed: {e}"
        );
    }
}

#[test]
fn bad_arguments_are_tool_errors() {
    let mut h = Harness::new(false);
    for (tool, args) in [
        ("pegboard_session_info", json!({})),
        ("pegboard_session_info", json!({ "sessionId": 3 })),
        (
            "pegboard_session_info",
            json!({ "sessionId": "s_foreignn", "token": "x" }),
        ),
        (
            "pegboard_session_close",
            json!({ "sessionId": "s_foreignn", "keepTabs": "yes" }),
        ),
        ("pegboard_recording_get", json!({ "recordingId": "../x" })),
        ("pegboard_session_list", json!({ "all": true })),
    ] {
        let (err, text) = h.call(tool, args.clone());
        assert!(
            err && text.starts_with("invalid_params: "),
            "{tool} {args}: {text}"
        );
    }
    // Missing arguments mean none.
    let r = h.request("tools/call", json!({ "name": "pegboard_session_list" }));
    assert_eq!(r["result"]["isError"], false, "{r}");
}

#[test]
fn adopted_handles_the_daemon_refuses_are_dropped() {
    let mut owned = Owned::default();
    // Open, with its token; open, with a wrong token; not open at all.
    owned
        .adopt(&format!("s_adoptedd.{}", token_of("s_adoptedd")))
        .unwrap();
    owned
        .adopt(&format!("s_wrongtok.{}", token_of("s_adoptedd")))
        .unwrap();
    owned
        .adopt(&format!("{MISSING}.{}", token_of(MISSING)))
        .unwrap();
    let stub = Stub::new();
    for id in ["s_adoptedd", "s_wrongtok"] {
        stub.state.lock().unwrap().sessions.push((
            id.into(),
            token_of(id),
            "given".into(),
            json!({}),
        ));
    }
    let mut h = Harness::with(opts(false, SessionPolicy::default()), stub, owned);
    let (err, list) = h.call("pegboard_session_list", json!({}));
    assert!(!err, "{list}");
    assert!(list.contains("s_adoptedd"), "{list}");
    assert!(
        !list.contains("s_wrongtok") && !list.contains(MISSING),
        "{list}"
    );
    assert_eq!(
        h.owned.lock().unwrap().ids(),
        [SessionId("s_adoptedd".into())]
    );
    // A dropped one is unknown, recordings included; the check is done once.
    let (err, text) = h.call(
        "pegboard_recording_get",
        json!({ "recordingId": "r_wrongtok" }),
    );
    assert!(
        err && text == "recording_not_found: no recording r_wrongtok",
        "{text}"
    );
    assert_eq!(h.stub.calls_of("session.info").len(), 3);
}

#[test]
fn a_create_reply_that_does_not_decode_closes_the_session() {
    let stub = Stub {
        bad_create: true,
        ..Stub::new()
    };
    let mut h = Harness::with(
        opts(false, SessionPolicy::default()),
        stub,
        Owned::default(),
    );
    let (err, text) = h.call("pegboard_session_create", json!({}));
    assert!(err && text.starts_with("internal: "), "{text}");
    let closed = h.stub.calls_of("session.close");
    assert_eq!(closed.len(), 1);
    let id = closed[0]["sessionId"].as_str().unwrap().to_string();
    assert_eq!(closed[0]["token"], token_of(&id));
    assert!(!text.contains(&token_of(&id)), "{text}");
}

#[test]
fn results_hold_no_token_even_before_the_scrub() {
    for operator in [false, true] {
        let adopted = "s_adoptedd";
        let mut owned = Owned::default();
        owned
            .adopt(&format!("{adopted}.{}", token_of(adopted)))
            .unwrap();
        let stub = Stub {
            leak_in_text: false,
            ..Stub::new()
        };
        stub.state.lock().unwrap().sessions.push((
            adopted.into(),
            token_of(adopted),
            "given".into(),
            json!({}),
        ));
        let mut h = Harness::with(opts(operator, SessionPolicy::default()), stub, owned);
        let mut out = Vec::new();
        let mut ask = |h: &mut Harness, tool: &str, args: Value| {
            let line = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                               "params": { "name": tool, "arguments": args } });
            // `reply`, not `handle_line`: what the tools produce, unscrubbed.
            let reply = h.server.reply(line.to_string().as_bytes()).unwrap();
            out.push(reply.to_string());
            reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_string()
        };
        let created = ask(&mut h, "pegboard_session_create", json!({}));
        let a = created
            .split_whitespace()
            .find(|w| w.starts_with("s_"))
            .unwrap()
            .to_string();
        ask(&mut h, "pegboard_session_list", json!({}));
        ask(&mut h, "pegboard_recording_list", json!({}));
        for id in [a.as_str(), adopted, FOREIGN] {
            let rid = format!("r_{}", &id[2..]);
            ask(&mut h, "pegboard_session_info", json!({ "sessionId": id }));
            ask(&mut h, "pegboard_files_list", json!({ "sessionId": id }));
            ask(
                &mut h,
                "pegboard_recording_get",
                json!({ "recordingId": rid }),
            );
        }
        ask(&mut h, "pegboard_session_close", json!({ "sessionId": a }));
        let all = out.join("\n");
        assert!(all.contains(&a), "{all}");
        for t in [token_of(&a), token_of(adopted), FOREIGN_TOKEN.to_string()] {
            assert!(!all.contains(&t), "operator={operator}: {t} in {all}");
        }
    }
}

#[test]
fn an_overlong_line_is_not_held() {
    let max = 1024 * 1024;
    let mut input = vec![b'x'; 5 * max];
    input.extend_from_slice(b"\nok\n");
    let mut reader = std::io::BufReader::with_capacity(8192, &input[..]);
    let mut line = Vec::new();
    assert_eq!(
        read_line_into(&mut reader, max, &mut line).unwrap(),
        Some(true)
    );
    assert!(line.capacity() <= 2 * max, "held {} bytes", line.capacity());
    assert_eq!(
        read_line_into(&mut reader, max, &mut line).unwrap(),
        Some(false)
    );
    assert_eq!(line, b"ok");
}
