//! `pegboard mcp`: an MCP server on stdin and stdout (line-delimited JSON-RPC 2.0).
//!
//! It speaks to the daemon through the core API like the CLI does, and keeps the handles
//! (`<id>.<token>`) of the sessions it may drive in `Owned`, in memory only: tool arguments
//! and results carry session ids, never a token. In the default mode it sees only those
//! sessions; with `--operator` it may list, inspect and close any session and read any
//! recording, but still drives only its own. Standard output carries protocol messages
//! only; logs go to standard error.

mod tools;

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pegboard_core::api::Client;
use pegboard_core::paths::Paths;
use pegboard_protocol::{codes, RpcError, SessionId, SessionPolicy};
use serde_json::{json, Map, Value};

use tools::{Ctx, Registry};

/// Longest message line read; a longer one is answered with a parse error and skipped.
pub const MAX_LINE: usize = 4 * 1024 * 1024;
/// Time each of this server's sessions may take to close when it ends.
const CLOSE_LIMIT: Duration = Duration::from_secs(15);
/// MCP protocol versions understood, newest first.
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

// JSON-RPC error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

pub struct Options {
    pub operator: bool,
    pub tools: Vec<Bundle>,
    pub policy: SessionPolicy,
}

/// Groups of browser action tools (`--tools`). The management tools are always offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Bundle {
    Core,
    Tabs,
    State,
    Network,
    All,
}

struct Held {
    token: String,
    created_here: bool,
}

/// Sessions this server may act on. Tokens never leave this type (except to the core API).
#[derive(Default)]
pub struct Owned {
    open: HashMap<SessionId, Held>,
    /// Every session this server has held, open or not: their recordings stay readable.
    known: HashSet<SessionId>,
    /// Every token this server has held, so that none can slip into what it writes.
    spent: Vec<String>,
    shutting_down: bool,
}

impl Owned {
    /// Adopts the session of `PEGBOARD_SESSION` (a handle) and removes the variable, so no
    /// process started from here inherits it. Not set or empty: nothing to adopt.
    pub fn adopt_from_env(&mut self) -> Result<(), String> {
        let handle = std::env::var("PEGBOARD_SESSION").unwrap_or_default();
        std::env::remove_var("PEGBOARD_SESSION");
        if handle.is_empty() {
            return Ok(());
        }
        self.adopt(&handle)
    }

    /// Adopts a session given as `<id>.<token>`. The error does not repeat the handle.
    pub fn adopt(&mut self, handle: &str) -> Result<(), String> {
        let bad = || {
            "PEGBOARD_SESSION is not a session handle (<id>.<token>, the first line printed \
             by `pegboard session create`)"
                .to_string()
        };
        let (id, token) = handle.split_once('.').ok_or_else(bad)?;
        let id = SessionId::parse(id).ok_or_else(bad)?;
        let token_ok = !token.is_empty()
            && token
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b));
        if !token_ok {
            return Err(bad());
        }
        self.insert(id, token.to_string(), false);
        Ok(())
    }

    pub fn insert(&mut self, id: SessionId, token: String, created_here: bool) {
        self.known.insert(id.clone());
        self.spent.push(token.clone());
        self.open.insert(
            id,
            Held {
                token,
                created_here,
            },
        );
    }

    pub fn token(&self, id: &SessionId) -> Option<&str> {
        self.open.get(id).map(|h| h.token.as_str())
    }

    /// The open sessions, sorted.
    pub fn ids(&self) -> Vec<SessionId> {
        let mut ids: Vec<SessionId> = self.open.keys().cloned().collect();
        ids.sort_by(|a, b| a.0.cmp(&b.0));
        ids
    }

    /// Whether this server has ever held the session.
    fn knows(&self, id: &SessionId) -> bool {
        self.known.contains(id)
    }

    /// The session is closed (it stays known).
    fn remove(&mut self, id: &SessionId) {
        self.open.remove(id);
    }

    /// Every session is closed (the browser stopped).
    fn forget_all(&mut self) {
        self.open.clear();
    }

    /// Open sessions this server created.
    fn created_count(&self) -> usize {
        self.open.values().filter(|h| h.created_here).count()
    }

    /// Keeps only the sessions in `open` (the daemon's list).
    fn retain_open(&mut self, open: &[SessionId]) {
        self.open.retain(|id, _| open.contains(id));
    }

    fn shutting_down(&self) -> bool {
        self.shutting_down
    }

    /// `text` with every token this server has held replaced.
    fn scrub(&self, text: &str) -> String {
        let mut text = text.to_string();
        for token in &self.spent {
            if text.contains(token.as_str()) {
                text = text.replace(token.as_str(), "[redacted]");
            }
        }
        text
    }
}

/// The core API as the server uses it (a stub in tests).
pub trait Core: Send {
    /// Makes sure the daemon is running (starting it if need be); the reason otherwise.
    fn ensure_running(&mut self) -> Result<(), String>;
    fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcError>;
}

/// Handles one message at a time.
pub struct Server {
    registry: Registry,
    core: Box<dyn Core>,
    owned: Arc<Mutex<Owned>>,
}

fn error_reply(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn too_long_reply() -> String {
    error_reply(Value::Null, PARSE_ERROR, "message line too long").to_string()
}

fn tool_result(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

impl Server {
    pub fn new(opts: &Options, core: Box<dyn Core>, owned: Arc<Mutex<Owned>>) -> Server {
        Server {
            registry: Registry::new(opts),
            core,
            owned,
        }
    }

    /// The answer to one line, as the text of one line (none for a notification).
    pub fn handle_line(&mut self, line: &[u8]) -> Option<String> {
        let reply = self.reply(line)?;
        Some(self.owned.lock().unwrap().scrub(&reply.to_string()))
    }

    fn reply(&mut self, line: &[u8]) -> Option<Value> {
        if line.iter().all(u8::is_ascii_whitespace) {
            return None;
        }
        let msg: Value = match serde_json::from_slice(line) {
            Ok(v) => v,
            Err(_) => return Some(error_reply(Value::Null, PARSE_ERROR, "not JSON")),
        };
        let Value::Object(msg) = msg else {
            let why = if msg.is_array() {
                "batches are not supported"
            } else {
                "not a request"
            };
            return Some(error_reply(Value::Null, INVALID_REQUEST, why));
        };
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // A response (this server sends no requests) or nonsense with no method: a
            // reply is owed only to a request.
            if msg.contains_key("result") || msg.contains_key("error") {
                return None;
            }
            let id = msg.get("id").cloned()?;
            let id = if id.is_string() || id.is_number() {
                id
            } else {
                Value::Null
            };
            return Some(error_reply(id, INVALID_REQUEST, "no method"));
        };
        // No id: a notification, never answered.
        let id = msg.get("id")?.clone();
        if !(id.is_string() || id.is_number()) || msg.get("jsonrpc") != Some(&json!("2.0")) {
            let id = if id.is_string() || id.is_number() {
                id
            } else {
                Value::Null
            };
            return Some(error_reply(
                id,
                INVALID_REQUEST,
                "not a JSON-RPC 2.0 request",
            ));
        }
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.tools_list()),
            "tools/call" => self.tools_call(&params),
            _ => Err((
                METHOD_NOT_FOUND,
                format!("method {method:?} is not supported"),
            )),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => error_reply(id, code, &message),
        })
    }

    fn initialize(&self, params: &Value) -> Value {
        let asked = params["protocolVersion"].as_str().unwrap_or("");
        let version = PROTOCOL_VERSIONS
            .into_iter()
            .find(|v| *v == asked)
            .unwrap_or(PROTOCOL_VERSIONS[0]);
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "pegboard", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "Drive a browser through isolated sessions: create a session with \
                             pegboard_session_create and pass its sessionId to the other tools. \
                             What a session may do is fixed when it is created.",
        })
    }

    fn tools_list(&self) -> Value {
        let tools: Vec<Value> = self
            .registry
            .list()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema,
                })
            })
            .collect();
        json!({ "tools": tools })
    }

    fn tools_call(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params["name"]
            .as_str()
            .ok_or((INVALID_PARAMS, "tools/call needs a tool name".to_string()))?;
        let empty = Map::new();
        let args = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(map)) => map,
            Some(_) => return Err((INVALID_PARAMS, "arguments must be an object".to_string())),
        };
        if !self.registry.list().any(|t| t.name == name) {
            return Err((INVALID_PARAMS, format!("unknown tool {name:?}")));
        }
        if let Err(why) = self.core.ensure_running() {
            let text = format!(
                "{}: pegboard is not running and could not be started: {why}",
                codes::NOT_RUNNING
            );
            return Ok(tool_result(text, true));
        }
        let mut ctx = Ctx {
            core: self.core.as_mut(),
            owned: &self.owned,
        };
        let outcome = self.registry.call(&mut ctx, name, args);
        Ok(match outcome {
            Some(Ok(text)) => tool_result(text, false),
            Some(Err(e)) => tool_result(format!("{}: {}", e.code, e.message), true),
            None => tool_result(format!("{}: unknown tool", codes::INVALID_PARAMS), true),
        })
    }
}

/// One line of input.
#[derive(Debug, PartialEq)]
pub enum Line {
    Text(Vec<u8>),
    /// Longer than the limit; what was read of it is dropped.
    TooLong,
}

/// Reads up to the next newline, holding at most `max` bytes. `None` at the end.
pub fn read_line_capped(r: &mut impl BufRead, max: usize) -> std::io::Result<Option<Line>> {
    let mut line = Vec::new();
    let mut too_long = false;
    let mut read_any = false;
    loop {
        let buf = match r.fill_buf() {
            Ok(buf) => buf,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if buf.is_empty() {
            break;
        }
        read_any = true;
        let (chunk, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (&buf[..i], i + 1),
            None => (buf, buf.len()),
        };
        if !too_long {
            if line.len() + chunk.len() > max {
                too_long = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(chunk);
            }
        }
        let end = done > chunk.len();
        r.consume(done);
        if end {
            break;
        }
    }
    Ok(match (read_any, too_long) {
        (false, _) => None,
        (true, true) => Some(Line::TooLong),
        (true, false) => Some(Line::Text(line)),
    })
}

/// Closes the sessions this server created (in parallel, each within `CLOSE_LIMIT` as the
/// connection made by `connect` enforces) and marks the server as shutting down, so a
/// session created after this is closed at once. Adopted sessions stay open.
pub fn close_created(owned: &Mutex<Owned>, connect: &(dyn Fn() -> Box<dyn Core> + Sync)) {
    let created: Vec<(SessionId, String)> = {
        let mut owned = owned.lock().unwrap();
        owned.shutting_down = true;
        let created: Vec<(SessionId, String)> = owned
            .open
            .iter()
            .filter(|(_, h)| h.created_here)
            .map(|(id, h)| (id.clone(), h.token.clone()))
            .collect();
        for (id, _) in &created {
            owned.remove(id);
        }
        created
    };
    if created.is_empty() {
        return;
    }
    eprintln!("pegboard mcp: closing {} sessions", created.len());
    std::thread::scope(|scope| {
        for (id, token) in &created {
            scope.spawn(move || {
                let params = json!({ "sessionId": id, "token": token, "keepTabs": false });
                if let Err(e) = connect().call("session.close", params) {
                    // The daemon's messages carry no tokens.
                    eprintln!("pegboard mcp: closing {}: {}: {}", id.0, e.code, e.message);
                }
            });
        }
    });
}

/// The core API through `Client`, connected on first use. The daemon is started in the
/// background when the server starts; the first tool call waits for that.
struct ClientCore {
    paths: Paths,
    client: Option<Client>,
    start: Start,
    limit: Option<Duration>,
}

enum Start {
    Pending(std::thread::JoinHandle<Result<bool, RpcError>>),
    Done,
    Failed,
}

impl ClientCore {
    /// Starts the daemon in the background, as `pegboard browser start` does.
    fn starting(paths: Paths) -> ClientCore {
        let p = paths.clone();
        ClientCore {
            paths,
            client: None,
            start: Start::Pending(std::thread::spawn(move || crate::browser_start(&p))),
            limit: None,
        }
    }

    /// A connection for closing sessions at the end: no start, a short time limit.
    fn for_closing(paths: Paths) -> ClientCore {
        ClientCore {
            paths,
            client: None,
            start: Start::Done,
            limit: Some(CLOSE_LIMIT),
        }
    }
}

impl Core for ClientCore {
    fn ensure_running(&mut self) -> Result<(), String> {
        let result = match std::mem::replace(&mut self.start, Start::Done) {
            Start::Done => return Ok(()),
            Start::Pending(handle) => handle.join().unwrap_or_else(|_| {
                Err(RpcError {
                    code: codes::INTERNAL.into(),
                    message: "starting the daemon failed".into(),
                    data: None,
                })
            }),
            // It failed before: try again (Chrome may have been installed since).
            Start::Failed => crate::browser_start(&self.paths),
        };
        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                self.start = Start::Failed;
                Err(e.message)
            }
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        if self.client.is_none() {
            let mut client = Client::connect(&self.paths)?;
            if let Some(limit) = self.limit {
                client.set_call_limit(limit);
            }
            self.client = Some(client);
        }
        let client = self.client.as_mut().expect("connected above");
        let result = client.call::<_, Value>(method, params);
        if matches!(&result, Err(e) if e.code == codes::NOT_RUNNING) {
            // The daemon went away: the next call connects anew.
            self.client = None;
        }
        result
    }
}

/// Reads stdin on a thread of its own (blocking reads), one line at a time.
fn stdin_lines() -> tokio::sync::mpsc::Receiver<Option<Line>> {
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut reader = stdin.lock();
        loop {
            let next = match read_line_capped(&mut reader, MAX_LINE) {
                Ok(next) => next,
                Err(e) => {
                    eprintln!("pegboard mcp: reading stdin: {e}");
                    None
                }
            };
            let end = next.is_none();
            if tx.blocking_send(next).is_err() || end {
                return;
            }
        }
    });
    rx
}

fn write_line(text: &str) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(text.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

/// Runs the server until stdin ends or SIGTERM/SIGINT; returns the exit code.
pub fn run(opts: Options, paths: Paths) -> i32 {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("pegboard mcp: starting the runtime: {e}");
            return 1;
        }
    };
    let code = runtime.block_on(serve(opts, paths));
    // A tool call may still be running on a blocking thread: do not wait for it.
    runtime.shutdown_background();
    code
}

pub async fn serve(opts: Options, paths: Paths) -> i32 {
    use tokio::signal::unix::{signal, SignalKind};
    let mut owned = Owned::default();
    // First, before any thread or process is started.
    if let Err(e) = owned.adopt_from_env() {
        eprintln!("pegboard mcp: {e}");
        return 2;
    }
    let (Ok(mut term), Ok(mut int)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        eprintln!("pegboard mcp: could not handle signals");
        return 1;
    };
    let owned = Arc::new(Mutex::new(owned));
    let core = ClientCore::starting(paths.clone());
    let server = Arc::new(Mutex::new(Server::new(
        &opts,
        Box::new(core),
        owned.clone(),
    )));
    let mut lines = stdin_lines();
    let mut in_flight = None;
    loop {
        let next = tokio::select! {
            next = lines.recv() => next.flatten(),
            _ = term.recv() => break,
            _ = int.recv() => break,
        };
        let reply = match next {
            None => break,
            Some(Line::TooLong) => Some(too_long_reply()),
            Some(Line::Text(line)) => {
                let s = server.clone();
                // A call that panicked leaves the lock poisoned; the server goes on.
                let mut task = tokio::task::spawn_blocking(move || {
                    s.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .handle_line(&line)
                });
                tokio::select! {
                    r = &mut task => r.unwrap_or(None),
                    _ = term.recv() => { in_flight = Some(task); break }
                    _ = int.recv() => { in_flight = Some(task); break }
                }
            }
        };
        if let Some(reply) = reply {
            if write_line(&reply).is_err() {
                // Nobody reads the answers any more.
                break;
            }
        }
    }

    // Ending: close what this server created; a second SIGINT ends at once.
    let closing = {
        let owned = owned.clone();
        tokio::task::spawn_blocking(move || {
            let connect = move || Box::new(ClientCore::for_closing(paths.clone())) as Box<dyn Core>;
            close_created(&owned, &connect);
        })
    };
    tokio::select! {
        _ = closing => {}
        _ = int.recv() => return 130,
    }
    // A call still running (a session being created) closes what it makes once it
    // returns; if it does not return in time, the daemon drops a session whose creation
    // nobody received.
    if let Some(task) = in_flight {
        tokio::select! {
            _ = tokio::time::timeout(CLOSE_LIMIT, task) => {}
            _ = int.recv() => return 130,
        }
    }
    0
}

#[cfg(test)]
mod tests;
