//! The tools of `pegboard mcp`: the registry (what `tools/list` shows and `tools/call`
//! dispatches to) and the management tools.
//!
//! Every tool answers with text built from chosen fields; a token never reaches it (and
//! `Server` scrubs what it writes once more). In the default mode a session or recording
//! this server does not own is answered exactly like one that does not exist, and the
//! operator methods of the core API (`session.info_any`, `session.close_any`) are never
//! called.

use std::sync::Mutex;

use pegboard_core::actionlog::{valid_recording_id, ActionEntry, Manifest};
use pegboard_core::policy;
use pegboard_protocol::{
    codes, BrowserStatus, FileInfo, ProfileInfo, RpcError, SessionCreateParams, SessionId,
    SessionInfo, SessionPolicy,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};

use super::{lock, Bundle, Core, Options, Owned};

/// Sessions one MCP server may have open (created by it) at once.
pub const MAX_CREATED_SESSIONS: usize = 16;

/// What `tools/list` shows of a tool.
pub struct ToolDef {
    pub name: &'static str,
    pub description: String,
    /// A JSON Schema of the `arguments` object.
    pub input_schema: Value,
}

/// What a tool does when called. Task 10 adds the browser action tools here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    SessionCreate,
    SessionList,
    SessionInfo,
    SessionClose,
    RecordingList,
    RecordingGet,
    FilesList,
    BrowserStatus,
    ProfileList,
    ProfileCreate,
    ProfileDelete,
    BrowserStop,
}

struct Tool {
    def: ToolDef,
    kind: Kind,
}

/// What a tool call needs besides its arguments.
pub struct Ctx<'a> {
    pub core: &'a mut dyn Core,
    pub owned: &'a Mutex<Owned>,
}

/// The tools of one server: fixed when it starts (the list never changes while it runs).
pub struct Registry {
    operator: bool,
    policy: SessionPolicy,
    tools: Vec<Tool>,
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn session_id_schema() -> Value {
    json!({ "type": "string", "description": "The session's id (s_ and 8 characters)." })
}

fn policy_schema() -> Value {
    let classes: Vec<&str> = pegboard_protocol::ActionClass::ALL
        .iter()
        .map(|c| c.as_str())
        .collect();
    json!({
        "type": "object",
        "description": "The session's policy (as the options of `pegboard session create`). \
                        Without it the session gets the policy pegboard mcp was started with.",
        "properties": {
            "allow": { "type": "array", "items": { "enum": classes },
                       "description": "Open classes that are denied by default (upload, state, clipboard)." },
            "deny": { "type": "array", "items": { "enum": classes },
                      "description": "Close classes that are allowed by default (e.g. eval, download)." },
            "allowDomains": { "type": "array", "items": { "type": "string" },
                              "description": "Only these hosts (and their subdomains) can be reached." },
            "allowPrivateNetwork": { "type": "boolean" },
            "noLocalhost": { "type": "boolean" },
            "uploadDir": { "type": "string", "description": "Absolute path; needs allow upload." },
            "noExpire": { "type": "boolean" }
        },
        "additionalProperties": false,
    })
}

/// The management tools of a mode, in the order `tools/list` shows them.
fn management_tools(operator: bool) -> Vec<Tool> {
    let scope = if operator {
        "every session"
    } else {
        "the sessions this server created or was given"
    };
    let mut create_props = json!({
        "name": { "type": "string", "description": "A name to recognise the session by." },
        "url": { "type": "string", "description": "The page to open (http, https, data or about:blank)." },
        "profile": { "type": "string", "description": "An existing browser profile (default: the default agent profile)." },
    });
    let create_text = if operator {
        create_props["policy"] = policy_schema();
        "Create a browser session (its own tab group) and return its id. `policy` sets what \
         the session may do; without it the session gets the policy pegboard mcp was \
         started with."
            .to_string()
    } else {
        "Create a browser session (its own tab group) and return its id and the policy it \
         got. The policy is set by whoever started pegboard mcp and cannot be changed here."
            .to_string()
    };
    let none = || schema(json!({}), &[]);
    let id_only = || schema(json!({ "sessionId": session_id_schema() }), &["sessionId"]);
    let name_only = || {
        schema(
            json!({ "name": { "type": "string", "description": "The profile's name." } }),
            &["name"],
        )
    };
    let mut tools = vec![
        (
            Kind::SessionCreate,
            "pegboard_session_create",
            create_text,
            schema(create_props, &[]),
        ),
        (
            Kind::SessionList,
            "pegboard_session_list",
            format!("List open sessions ({scope})."),
            none(),
        ),
        (
            Kind::SessionInfo,
            "pegboard_session_info",
            format!("Show a session and its policy ({scope})."),
            id_only(),
        ),
        (
            Kind::SessionClose,
            "pegboard_session_close",
            format!(
                "Close a session ({scope}) and its tabs; keepTabs leaves the tabs open \
                 outside any group. Its recording stays."
            ),
            schema(
                json!({ "sessionId": session_id_schema(), "keepTabs": { "type": "boolean" } }),
                &["sessionId"],
            ),
        ),
        (
            Kind::RecordingList,
            "pegboard_recording_list",
            format!("List the action recordings of {scope}."),
            none(),
        ),
        (
            Kind::RecordingGet,
            "pegboard_recording_get",
            format!("Show the actions of a recording ({scope}). Secrets in it are already hidden."),
            schema(
                json!({ "recordingId": { "type": "string", "description": "r_ and 8 characters." } }),
                &["recordingId"],
            ),
        ),
        (
            Kind::FilesList,
            "pegboard_files_list",
            "List the files of a session (one this server created or was given): its \
             downloads, screenshots and PDFs. This list is the source of truth for downloads: \
             a download is there once it finished and was kept."
                .to_string(),
            id_only(),
        ),
        (
            Kind::BrowserStatus,
            "pegboard_browser_status",
            "Show whether the browser is running.".to_string(),
            none(),
        ),
    ];
    if operator {
        tools.extend([
            (
                Kind::ProfileList,
                "pegboard_profile_list",
                "List the browser profiles and the sessions using each.".to_string(),
                none(),
            ),
            (
                Kind::ProfileCreate,
                "pegboard_profile_create",
                "Create a browser profile.".to_string(),
                name_only(),
            ),
            (
                Kind::ProfileDelete,
                "pegboard_profile_delete",
                "Delete a browser profile that no session uses.".to_string(),
                name_only(),
            ),
            (
                Kind::BrowserStop,
                "pegboard_browser_stop",
                "Close every session and stop the browser.".to_string(),
                none(),
            ),
        ]);
    }
    tools
        .into_iter()
        .map(|(kind, name, description, input_schema)| Tool {
            def: ToolDef {
                name,
                description,
                input_schema,
            },
            kind,
        })
        .collect()
}

/// The browser action tools of the bundles. Task 10 generates them from the grammar.
fn action_tools(bundles: &[Bundle]) -> Vec<Tool> {
    let _ = bundles;
    Vec::new()
}

fn error(code: &str, message: impl Into<String>) -> RpcError {
    RpcError {
        code: code.to_string(),
        message: message.into(),
        data: None,
    }
}

fn invalid(message: impl Into<String>) -> RpcError {
    error(codes::INVALID_PARAMS, message)
}

/// The answer for a session that does not exist, word for word the daemon's.
pub fn no_session(id: &SessionId) -> RpcError {
    error(codes::SESSION_NOT_FOUND, format!("no session {}", id.0))
}

/// The answer for a recording that does not exist, word for word the daemon's.
fn no_recording(id: &str) -> RpcError {
    error(codes::RECORDING_NOT_FOUND, format!("no recording {id}"))
}

/// The arguments of a call, checked against the keys the tool takes. A key it does not
/// take is an error, never ignored.
struct Args<'a>(&'a Map<String, Value>);

impl<'a> Args<'a> {
    fn new(map: &'a Map<String, Value>, keys: &[&str], hint: &str) -> Result<Self, RpcError> {
        if let Some(bad) = map.keys().find(|k| !keys.contains(&k.as_str())) {
            let takes = if keys.is_empty() {
                "no arguments".to_string()
            } else {
                keys.join(", ")
            };
            return Err(invalid(format!(
                "unknown argument {bad:?} (this tool takes {takes}){hint}"
            )));
        }
        Ok(Args(map))
    }

    fn string(&self, key: &str) -> Result<Option<String>, RpcError> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(invalid(format!("{key} must be a string"))),
        }
    }

    fn required(&self, key: &str) -> Result<String, RpcError> {
        self.string(key)?
            .ok_or_else(|| invalid(format!("{key} is required")))
    }

    fn flag(&self, key: &str) -> Result<bool, RpcError> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(false),
            Some(Value::Bool(b)) => Ok(*b),
            Some(_) => Err(invalid(format!("{key} must be true or false"))),
        }
    }

    fn session_id(&self) -> Result<SessionId, RpcError> {
        let id = self.required("sessionId")?;
        SessionId::parse(&id).ok_or_else(|| invalid(format!("{id:?} is not a session id")))
    }
}

fn call<R: DeserializeOwned>(
    core: &mut dyn Core,
    method: &str,
    params: Value,
) -> Result<R, RpcError> {
    let v = core.call(method, params)?;
    serde_json::from_value(v).map_err(|e| {
        error(
            codes::INTERNAL,
            format!("unexpected answer to {method}: {e}"),
        )
    })
}

fn describe_session(s: &SessionInfo) -> String {
    format!("{}  recording {}", crate::describe(s), s.recording_id)
}

impl Registry {
    pub fn new(opts: &Options) -> Registry {
        let mut tools = management_tools(opts.operator);
        tools.extend(action_tools(&opts.tools));
        Registry {
            operator: opts.operator,
            policy: opts.policy.clone(),
            tools,
        }
    }

    pub fn list(&self) -> impl Iterator<Item = &ToolDef> {
        self.tools.iter().map(|t| &t.def)
    }

    /// `None` when there is no tool of that name.
    pub fn call(
        &self,
        ctx: &mut Ctx,
        name: &str,
        args: &Map<String, Value>,
    ) -> Option<Result<String, RpcError>> {
        let kind = self.tools.iter().find(|t| t.def.name == name)?.kind;
        Some(self.run(ctx, kind, args))
    }

    fn run(
        &self,
        ctx: &mut Ctx,
        kind: Kind,
        args: &Map<String, Value>,
    ) -> Result<String, RpcError> {
        let plain = |keys: &[&str]| Args::new(args, keys, "");
        match kind {
            Kind::SessionCreate => self.session_create(ctx, args),
            Kind::SessionList => {
                plain(&[])?;
                self.session_list(ctx)
            }
            Kind::SessionInfo => {
                let id = plain(&["sessionId"])?.session_id()?;
                self.session_info(ctx, &id)
            }
            Kind::SessionClose => {
                let a = plain(&["sessionId", "keepTabs"])?;
                let (id, keep_tabs) = (a.session_id()?, a.flag("keepTabs")?);
                self.session_close(ctx, &id, keep_tabs)
            }
            Kind::RecordingList => {
                plain(&[])?;
                self.recording_list(ctx)
            }
            Kind::RecordingGet => {
                let id = plain(&["recordingId"])?.required("recordingId")?;
                self.recording_get(ctx, &id)
            }
            Kind::FilesList => {
                let id = plain(&["sessionId"])?.session_id()?;
                files_list(ctx, &id)
            }
            Kind::BrowserStatus => {
                plain(&[])?;
                self.browser_status(ctx)
            }
            Kind::ProfileList => {
                plain(&[])?;
                let list: Vec<ProfileInfo> = call(ctx.core, "profile.list", json!({}))?;
                let lines: Vec<String> = list
                    .iter()
                    .map(|p| {
                        let ids: Vec<&str> = p.sessions.iter().map(|s| s.0.as_str()).collect();
                        format!("{}  {:?}  {}", p.name, p.kind, ids.join(","))
                    })
                    .collect();
                Ok(lines.join("\n"))
            }
            Kind::ProfileCreate => {
                let name = plain(&["name"])?.required("name")?;
                let p: ProfileInfo = call(ctx.core, "profile.create", json!({ "name": name }))?;
                Ok(format!("created profile {}", p.name))
            }
            Kind::ProfileDelete => {
                let name = plain(&["name"])?.required("name")?;
                let _: Value = call(ctx.core, "profile.delete", json!({ "name": name }))?;
                Ok(format!("deleted profile {name}"))
            }
            Kind::BrowserStop => {
                plain(&[])?;
                let _: Value = call(ctx.core, "browser.stop", json!({}))?;
                lock(ctx.owned).forget_all();
                Ok("stopped".to_string())
            }
        }
    }

    fn session_create(&self, ctx: &mut Ctx, args: &Map<String, Value>) -> Result<String, RpcError> {
        let a = if self.operator {
            Args::new(args, &["name", "url", "profile", "policy"], "")?
        } else {
            Args::new(
                args,
                &["name", "url", "profile"],
                "; the session's policy is set by whoever starts pegboard mcp and cannot be \
                 given or changed here",
            )?
        };
        let policy = match args.get("policy") {
            Some(v) if self.operator && !v.is_null() => {
                let p: SessionPolicy = serde_json::from_value(v.clone())
                    .map_err(|e| invalid(format!("bad policy: {e}")))?;
                policy::validate(&p)?;
                p
            }
            _ => self.policy.clone(),
        };
        let params = SessionCreateParams {
            name: a.string("name")?,
            url: a.string("url")?,
            profile: a.string("profile")?,
            policy,
        };
        if lock(ctx.owned).created_count() >= MAX_CREATED_SESSIONS {
            // Sessions closed elsewhere (idle expiry, a person) no longer count.
            let list: Vec<SessionInfo> = call(ctx.core, "session.list", json!({}))?;
            let open: Vec<SessionId> = list.into_iter().map(|s| s.id).collect();
            lock(ctx.owned).retain_open(&open);
        }
        if lock(ctx.owned).created_count() >= MAX_CREATED_SESSIONS {
            return Err(error(
                codes::LIMIT_EXCEEDED,
                format!(
                    "this server already has {MAX_CREATED_SESSIONS} open sessions; close one first"
                ),
            ));
        }
        let params = serde_json::to_value(params).map_err(|e| invalid(e.to_string()))?;
        let raw = ctx.core.call("session.create", params)?;
        let decoded = serde_json::from_value::<SessionInfo>(raw.clone())
            .ok()
            .and_then(|mut s| Some((s.token.take().filter(|t| !t.is_empty())?, s)));
        let Some((token, s)) = decoded else {
            // The daemon made a session this server cannot hold: close it if the reply
            // gives the way, and say which one it was.
            let id = raw["id"].as_str().unwrap_or("?").to_string();
            let closed = match raw["token"].as_str() {
                Some(token) => ctx
                    .core
                    .call(
                        "session.close",
                        json!({ "sessionId": id, "token": token, "keepTabs": false }),
                    )
                    .is_ok(),
                None => false,
            };
            eprintln!(
                "pegboard mcp: the answer to session.create could not be read; session {id} {}",
                if closed {
                    "was closed"
                } else {
                    "may still be open"
                }
            );
            return Err(error(
                codes::INTERNAL,
                "the daemon's answer to session.create could not be read",
            ));
        };
        let shutting_down = {
            let mut owned = lock(ctx.owned);
            owned.insert(s.id.clone(), token.clone(), true);
            owned.shutting_down()
        };
        if shutting_down {
            // Created while the server closes its sessions: this one goes too.
            let _ = ctx.core.call(
                "session.close",
                json!({ "sessionId": s.id, "token": token, "keepTabs": false }),
            );
            lock(ctx.owned).remove(&s.id);
            return Err(error(codes::NOT_RUNNING, "pegboard mcp is shutting down"));
        }
        Ok(format!(
            "created session {} (name {:?}, profile {}, recording {})\n{}",
            s.id.0,
            s.name,
            s.profile,
            s.recording_id,
            crate::describe_policy(&s.policy)
        ))
    }

    fn session_list(&self, ctx: &mut Ctx) -> Result<String, RpcError> {
        let list: Vec<SessionInfo> = call(ctx.core, "session.list", json!({}))?;
        let owned = lock(ctx.owned);
        let lines: Vec<String> = list
            .iter()
            .filter(|s| self.operator || owned.token(&s.id).is_some())
            .map(describe_session)
            .collect();
        Ok(if lines.is_empty() {
            "no sessions".to_string()
        } else {
            lines.join("\n")
        })
    }

    fn session_info(&self, ctx: &mut Ctx, id: &SessionId) -> Result<String, RpcError> {
        let token = lock(ctx.owned).token(id).map(str::to_string);
        let s: SessionInfo = match token {
            Some(token) => call(
                ctx.core,
                "session.info",
                json!({ "sessionId": id, "token": token }),
            )?,
            None if self.operator => {
                call(ctx.core, "session.info_any", json!({ "sessionId": id }))?
            }
            None => return Err(no_session(id)),
        };
        Ok(format!(
            "{}\n{}",
            describe_session(&s),
            crate::describe_policy(&s.policy)
        ))
    }

    fn session_close(
        &self,
        ctx: &mut Ctx,
        id: &SessionId,
        keep_tabs: bool,
    ) -> Result<String, RpcError> {
        let token = lock(ctx.owned).token(id).map(str::to_string);
        let result: Result<Value, RpcError> = match token {
            Some(token) => call(
                ctx.core,
                "session.close",
                json!({ "sessionId": id, "token": token, "keepTabs": keep_tabs }),
            ),
            None if self.operator => call(
                ctx.core,
                "session.close_any",
                json!({ "sessionId": id, "keepTabs": keep_tabs }),
            ),
            None => return Err(no_session(id)),
        };
        match result {
            Ok(_) => {
                lock(ctx.owned).remove(id);
                Ok(format!("closed {}", id.0))
            }
            Err(e) => {
                if e.code == codes::SESSION_NOT_FOUND {
                    // Gone already (closed elsewhere or expired).
                    lock(ctx.owned).remove(id);
                }
                Err(e)
            }
        }
    }

    fn recording_list(&self, ctx: &mut Ctx) -> Result<String, RpcError> {
        let list: Vec<Manifest> = call(ctx.core, "recording.list", json!({}))?;
        let owned = lock(ctx.owned);
        let lines: Vec<String> = list
            .iter()
            .filter(|m| {
                self.operator || SessionId::parse(&m.session_id).is_some_and(|id| owned.knows(&id))
            })
            .map(|m| {
                format!(
                    "{}  {}  {}  {}  {}",
                    m.recording_id,
                    m.session_id,
                    m.started_at,
                    m.ended_at.as_deref().unwrap_or("-"),
                    m.session_name
                )
            })
            .collect();
        Ok(if lines.is_empty() {
            "no recordings".to_string()
        } else {
            lines.join("\n")
        })
    }

    fn recording_get(&self, ctx: &mut Ctx, id: &str) -> Result<String, RpcError> {
        if !valid_recording_id(id) {
            return Err(invalid(format!("{id:?} is not a recording id")));
        }
        // A recording is its session's: `r_<x>` belongs to `s_<x>`.
        let session = SessionId(format!("s_{}", &id[2..]));
        if !self.operator && !lock(ctx.owned).knows(&session) {
            return Err(no_recording(id));
        }
        let v: Value = call(ctx.core, "recording.get", json!({ "recordingId": id }))?;
        let manifest: Manifest = serde_json::from_value(v["manifest"].clone())
            .map_err(|e| error(codes::INTERNAL, format!("unexpected recording: {e}")))?;
        let actions: Vec<ActionEntry> = serde_json::from_value(v["actions"].clone())
            .map_err(|e| error(codes::INTERNAL, format!("unexpected recording: {e}")))?;
        let mut out = vec![
            format!(
                "recording {} of session {} ({}), profile {}, {} to {}",
                manifest.recording_id,
                manifest.session_id,
                manifest.session_name,
                manifest.profile,
                manifest.started_at,
                manifest.ended_at.as_deref().unwrap_or("now")
            ),
            "seq  time  command  result  duration".to_string(),
        ];
        for a in &actions {
            let words = std::iter::once(a.command.as_str())
                .chain(a.args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            let result = match (a.ok, &a.error) {
                (true, _) => "ok".to_string(),
                (false, Some(e)) => format!("failed: {e}"),
                (false, None) => "failed".to_string(),
            };
            out.push(format!(
                "{}  {}  {}  {}  {}ms",
                a.seq, a.ts, words, result, a.duration_ms
            ));
        }
        Ok(out.join("\n"))
    }

    fn browser_status(&self, ctx: &mut Ctx) -> Result<String, RpcError> {
        let s: BrowserStatus = call(ctx.core, "browser.status", json!({}))?;
        let state = if s.stopping {
            "stopping"
        } else if s.running {
            "running"
        } else {
            "stopped"
        };
        let mut text = format!(
            "{state}, chrome {}",
            s.chrome_version.as_deref().unwrap_or("-")
        );
        // How many sessions others have is not this server's to tell in the default mode.
        if self.operator {
            text.push_str(&format!(", {} sessions", s.sessions));
        }
        Ok(text)
    }
}

/// Files of a session this server holds (both modes: `files.list` needs the token).
fn files_list(ctx: &mut Ctx, id: &SessionId) -> Result<String, RpcError> {
    let Some(token) = lock(ctx.owned).token(id).map(str::to_string) else {
        return Err(no_session(id));
    };
    let files: Vec<FileInfo> = call(
        ctx.core,
        "files.list",
        json!({ "sessionId": id, "token": token }),
    )?;
    if files.is_empty() {
        return Ok("no files".to_string());
    }
    Ok(files
        .iter()
        .map(|f| format!("{}  {}  {}", f.bytes, f.modified_at, f.name))
        .collect::<Vec<_>>()
        .join("\n"))
}
