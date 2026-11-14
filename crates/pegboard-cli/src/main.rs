//! The `pegboard` command line: manages the daemon, profiles, sessions and recordings, and
//! sends every other command to agent-browser through the daemon.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod mcp;

use clap::{Parser, Subcommand};
use pegboard_core::actionlog::{valid_recording_id, ActionEntry, Manifest};
use pegboard_core::api::{serve, Client};
use pegboard_core::daemon::Daemon;
use pegboard_core::lock::DaemonLock;
use pegboard_core::paths::{ensure_home, paths, socket_path_fits, Paths};
use pegboard_core::policy;
use pegboard_core::runner::succeeded;
use pegboard_protocol::{
    codes, ActionClass, BrowserStatus, CommandExecParams, CommandExecResult, FileInfo, ProfileInfo,
    RpcError, SessionCreateParams, SessionId, SessionInfo, SessionPolicy,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

const START_TIMEOUT: Duration = Duration::from_secs(20);
const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// Exit codes: 0 success, 1 command failure, 2 usage error or rejected command, 3 no daemon.
const EXIT_FAILURE: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_NO_DAEMON: i32 = 3;

#[derive(Parser)]
#[command(
    name = "pegboard",
    version,
    about = "Drive a browser through isolated sessions.",
    after_help = "Any other command is an agent-browser command run in a session:\n  pegboard --session <id> snapshot -i",
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    /// Print results as one line of JSON.
    #[arg(long, global = true)]
    json: bool,
    /// Session for an agent-browser command (else $PEGBOARD_SESSION).
    #[arg(long, value_name = "ID")]
    session: Option<String>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start, stop or inspect the browser.
    Browser {
        #[command(subcommand)]
        command: BrowserCmd,
    },
    /// Manage browser profiles.
    Profile {
        #[command(subcommand)]
        command: ProfileCmd,
    },
    /// Manage sessions.
    Session {
        #[command(subcommand)]
        command: SessionCmd,
    },
    /// Read action recordings.
    Recording {
        #[command(subcommand)]
        command: RecordingCmd,
    },
    /// The session's files (its downloads, screenshots and PDFs).
    Files {
        #[command(subcommand)]
        command: FilesCmd,
    },
    /// Serve MCP over stdin and stdout (for MCP clients).
    Mcp {
        /// Act on every session (list, info, close, recordings) and manage profiles and
        /// the browser; drive only the sessions this server created or was given.
        #[arg(long)]
        operator: bool,
        /// Browser action tool bundles to offer: core (page actions), tabs, state
        /// (cookies, storage), network (network, console, errors, vitals, a11y), all. State
        /// and network are off by default. The session and recording tools are always on.
        #[arg(
            long,
            value_enum,
            value_delimiter = ',',
            value_name = "BUNDLE,...",
            default_value = "core,tabs"
        )]
        tools: Vec<mcp::Bundle>,
        /// The policy of every session this server creates.
        #[command(flatten)]
        policy: PolicyArgs,
    },
    #[command(hide = true)]
    Daemon,
    #[command(external_subcommand)]
    Agent(Vec<String>),
}

#[derive(Subcommand)]
enum BrowserCmd {
    Start,
    Stop,
    Status,
}

#[derive(Subcommand)]
enum ProfileCmd {
    List,
    Create { name: String },
    Open { name: String },
    Delete { name: String },
}

/// The session policy options, the same for `session create` and `mcp`.
#[derive(clap::Args, Debug, Default)]
pub(crate) struct PolicyArgs {
    /// Open command classes that are denied by default (upload, state, clipboard).
    #[arg(long, value_delimiter = ',', value_name = "CLASS,...")]
    allow: Vec<ActionClass>,
    /// Close command classes that are allowed by default (e.g. eval, download).
    #[arg(long, value_delimiter = ',', value_name = "CLASS,...")]
    deny: Vec<ActionClass>,
    /// Only these hosts (and their subdomains) can be reached.
    #[arg(long, value_delimiter = ',', value_name = "DOMAIN,...")]
    allow_domain: Vec<String>,
    /// Allow addresses on private networks.
    #[arg(long)]
    allow_private_network: bool,
    /// Do not allow localhost.
    #[arg(long)]
    no_localhost: bool,
    /// Directory the session may upload files from (needs --allow upload).
    #[arg(long, value_name = "DIR")]
    upload_dir: Option<String>,
    /// Do not expire the session after it has been idle.
    #[arg(long)]
    no_expire: bool,
}

impl PolicyArgs {
    /// The policy, checked as the daemon will check it again (usage error otherwise).
    fn checked(self) -> SessionPolicy {
        let policy = SessionPolicy {
            allow: self.allow,
            deny: self.deny,
            allow_domains: self.allow_domain,
            allow_private_network: self.allow_private_network,
            no_localhost: self.no_localhost,
            upload_dir: self.upload_dir,
            no_expire: self.no_expire,
        };
        if let Err(e) = policy::validate(&policy) {
            usage(&e.message);
        }
        policy
    }
}

#[derive(Subcommand)]
enum SessionCmd {
    /// Create a session and print its id.
    Create {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[command(flatten)]
        policy: PolicyArgs,
    },
    List,
    Info {
        id: String,
    },
    Close {
        id: String,
        /// Leave the session's tabs open (ungrouped).
        #[arg(long)]
        keep_tabs: bool,
    },
}

#[derive(Subcommand)]
enum FilesCmd {
    /// List the files of the session (--session or $PEGBOARD_SESSION).
    List,
}

#[derive(Subcommand)]
enum RecordingCmd {
    List,
    Show { recording_id: String },
}

fn main() {
    let cli = Cli::parse();
    let paths = paths();
    if !socket_path_fits(&paths) {
        usage("PEGBOARD_HOME is too long for a unix socket path");
    }
    let code = match run(cli, &paths) {
        Ok(()) => 0,
        Err(e) => report(&e),
    };
    std::process::exit(code);
}

/// Prints the error and returns the exit code it stands for.
fn report(e: &RpcError) -> i32 {
    eprintln!("pegboard: {}: {}", e.code, e.message);
    exit_code(&e.code)
}

fn exit_code(code: &str) -> i32 {
    match code {
        codes::NOT_RUNNING => EXIT_NO_DAEMON,
        codes::ARGV_REJECTED
        | codes::INVALID_PARAMS
        | codes::LIMIT_EXCEEDED
        | codes::PROFILE_POLICY_CONFLICT => EXIT_USAGE,
        _ => EXIT_FAILURE,
    }
}

fn error(code: &str, message: impl Into<String>) -> RpcError {
    RpcError {
        code: code.to_string(),
        message: message.into(),
        data: None,
    }
}

fn usage(message: &str) -> ! {
    eprintln!("pegboard: {message}");
    std::process::exit(EXIT_USAGE)
}

/// A session as the user gives it: the id, and the token after a `.` when there is one.
/// A wrong or missing token is for the daemon to refuse; a malformed id is a usage error.
fn parse_handle(text: &str) -> (SessionId, Option<String>) {
    let (id, token) = match text.split_once('.') {
        Some((id, token)) => (id, Some(token.to_string())),
        None => (text, None),
    };
    match SessionId::parse(id) {
        Some(id) => (id, token),
        None => usage(&format!("{id:?} is not a session id")),
    }
}

fn call<P: Serialize, R: DeserializeOwned>(
    paths: &Paths,
    method: &str,
    params: P,
) -> Result<R, RpcError> {
    Client::connect(paths)?.call(method, params)
}

fn print_json<T: Serialize>(value: &T) {
    println!("{}", serde_json::to_string(value).unwrap_or_default());
}

fn run(cli: Cli, paths: &Paths) -> Result<(), RpcError> {
    let json = cli.json;
    match cli.command {
        Cmd::Daemon => daemon_main(paths),
        Cmd::Mcp {
            operator,
            tools,
            policy,
        } => {
            let mut bundles: Vec<mcp::Bundle> = Vec::new();
            for b in tools {
                if !bundles.contains(&b) {
                    bundles.push(b);
                }
            }
            let opts = mcp::Options {
                operator,
                tools: bundles,
                policy: policy.checked(),
            };
            std::process::exit(mcp::run(opts, paths.clone()))
        }
        Cmd::Browser { command } => match command {
            BrowserCmd::Start => {
                let started = browser_start(paths)?;
                println!(
                    "{}",
                    if started {
                        "started"
                    } else {
                        "already running"
                    }
                );
                Ok(())
            }
            BrowserCmd::Stop => browser_stop(paths),
            BrowserCmd::Status => {
                let s: BrowserStatus = call(paths, "browser.status", json!({}))?;
                if json {
                    print_json(&s);
                } else {
                    println!(
                        "{} pid {} chrome {} sessions {}",
                        if s.stopping {
                            "stopping"
                        } else if s.running {
                            "running"
                        } else {
                            "stopped"
                        },
                        s.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                        s.chrome_version.as_deref().unwrap_or("-"),
                        s.sessions
                    );
                }
                Ok(())
            }
        },
        Cmd::Profile { command } => profile(paths, command, json),
        Cmd::Session { command } => session(paths, command, json),
        Cmd::Recording { command } => recording(paths, command, json),
        Cmd::Files {
            command: FilesCmd::List,
        } => files_list(paths, session_of(cli.session), json),
        Cmd::Agent(argv) => agent_command(paths, session_of(cli.session), argv, json),
    }
}

/// The session handle of `--session`, else `$PEGBOARD_SESSION`: `<id>.<token>`.
fn session_of(flag: Option<String>) -> String {
    let id = flag
        .or_else(|| std::env::var("PEGBOARD_SESSION").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| usage("no session: pass --session or set PEGBOARD_SESSION"));
    if !id
        .split_once('.')
        .is_some_and(|(_, token)| !token.is_empty())
    {
        usage("session handle must be <id>.<token> (the first line printed by `pegboard session create`)");
    }
    id
}

fn files_list(paths: &Paths, session: String, json: bool) -> Result<(), RpcError> {
    let (id, token) = parse_handle(&session);
    let files: Vec<FileInfo> = call(
        paths,
        "files.list",
        json!({ "sessionId": id, "token": token }),
    )?;
    if json {
        print_json(&files);
    } else {
        for f in &files {
            println!("{}  {}  {}", f.bytes, f.modified_at, f.name);
        }
    }
    Ok(())
}

/// One line per download the result tells about (on stderr, so stdout keeps the output).
fn tell_downloads(output: &Value) {
    for line in download_lines(output) {
        eprintln!("pegboard: {line}");
    }
}

/// What a command's result tells about downloads that ended (each told once).
pub(crate) fn download_lines(output: &Value) -> Vec<String> {
    let Some(list) = output.get("downloads").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|d| {
            let name = d["name"].as_str().unwrap_or("?");
            match (d["bytes"].as_u64(), d["dropped"].as_str()) {
                (Some(bytes), _) => Some(format!("downloaded {name} ({bytes} bytes)")),
                (_, Some(why)) => Some(format!("download {name} was deleted: {why}")),
                _ => None,
            }
        })
        .collect()
}

fn profile(paths: &Paths, command: ProfileCmd, json: bool) -> Result<(), RpcError> {
    match command {
        ProfileCmd::List => {
            let list: Vec<ProfileInfo> = call(paths, "profile.list", json!({}))?;
            if json {
                print_json(&list);
            }
            for p in list.iter().filter(|_| !json) {
                let sessions: Vec<&str> = p.sessions.iter().map(|s| s.0.as_str()).collect();
                println!("{}  {:?}  {}", p.name, p.kind, sessions.join(","));
            }
        }
        ProfileCmd::Create { name } => {
            let p: ProfileInfo = call(paths, "profile.create", json!({ "name": name }))?;
            if json {
                print_json(&p);
            } else {
                println!("{}", p.name);
            }
        }
        ProfileCmd::Open { name } => {
            let _: Value = call(paths, "profile.open", json!({ "name": name }))?;
        }
        ProfileCmd::Delete { name } => {
            let _: Value = call(paths, "profile.delete", json!({ "name": name }))?;
        }
    }
    Ok(())
}

/// What is in force for localhost and for private networks (spec D7): with an allow-list the
/// list alone decides (localhost only when listed; a private address only when listed and
/// `--allow-private-network` is given); without one, localhost is open unless
/// `--no-localhost`, and private networks are closed unless `--allow-private-network`.
fn address_rules(p: &SessionPolicy) -> (&'static str, &'static str) {
    if p.allow_domains.is_empty() {
        (
            if p.no_localhost { "blocked" } else { "allowed" },
            if p.allow_private_network {
                "allowed"
            } else {
                "blocked"
            },
        )
    } else {
        (
            "only if listed",
            if p.allow_private_network {
                "only if listed"
            } else {
                "blocked"
            },
        )
    }
}

fn describe_policy(p: &SessionPolicy) -> String {
    let names = |list: &[ActionClass]| {
        if list.is_empty() {
            "-".to_string()
        } else {
            list.iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join(",")
        }
    };
    let (localhost, private) = address_rules(p);
    format!(
        "policy: allow {}; deny {}; allow-domain {}; localhost {localhost}; \
         private network {private}; upload-dir {}; expires {}",
        names(&p.allow),
        names(&p.deny),
        if p.allow_domains.is_empty() {
            "-".to_string()
        } else {
            p.allow_domains.join(",")
        },
        p.upload_dir.as_deref().unwrap_or("-"),
        if p.no_expire { "never" } else { "when idle" },
    )
}

fn describe(s: &SessionInfo) -> String {
    format!(
        "{}  {}  {}  {:?}  {} tabs",
        s.id.0,
        s.name,
        s.profile,
        s.state,
        s.targets.len()
    )
}

fn session(paths: &Paths, command: SessionCmd, json: bool) -> Result<(), RpcError> {
    match command {
        SessionCmd::Create {
            name,
            url,
            profile,
            policy,
        } => {
            // The daemon judges it again; this saves a round trip and works without one.
            let policy = policy.checked();
            let s: SessionInfo = call(
                paths,
                "session.create",
                SessionCreateParams {
                    name,
                    url,
                    profile,
                    policy,
                },
            )?;
            // The handle is `<id>.<token>`; this is the only place a token is printed.
            let handle = format!("{}.{}", s.id.0, s.token.clone().unwrap_or_default());
            if json {
                let mut v = serde_json::to_value(&s).unwrap_or_default();
                if let Some(map) = v.as_object_mut() {
                    map.remove("token");
                    map.insert("handle".into(), Value::String(handle));
                }
                print_json(&v);
            } else {
                // First line: the handle, so `export PEGBOARD_SESSION=$(pegboard session create)` works.
                println!("{handle}");
            }
        }
        SessionCmd::List => {
            let list: Vec<SessionInfo> = call(paths, "session.list", json!({}))?;
            if json {
                print_json(&list);
            }
            for s in list.iter().filter(|_| !json) {
                println!("{}", describe(s));
            }
        }
        SessionCmd::Info { id } => {
            let (id, token) = parse_handle(&id);
            let s: SessionInfo = call(
                paths,
                "session.info",
                json!({ "sessionId": id, "token": token }),
            )?;
            if json {
                print_json(&s);
            } else {
                println!("{}", describe(&s));
                println!("{}", describe_policy(&s.policy));
            }
        }
        SessionCmd::Close { id, keep_tabs } => {
            let (id, token) = parse_handle(&id);
            let s: SessionInfo = call(
                paths,
                "session.close",
                json!({ "sessionId": id, "keepTabs": keep_tabs, "token": token }),
            )?;
            if json {
                print_json(&s);
            } else {
                println!("closed {}", s.id.0);
            }
        }
    }
    Ok(())
}

fn recording(paths: &Paths, command: RecordingCmd, json: bool) -> Result<(), RpcError> {
    match command {
        RecordingCmd::List => {
            let list: Vec<Manifest> = call(paths, "recording.list", json!({}))?;
            if json {
                print_json(&list);
            }
            for m in list.iter().filter(|_| !json) {
                println!(
                    "{}  {}  {}  {}  {}",
                    m.recording_id,
                    m.session_id,
                    m.started_at,
                    m.ended_at.as_deref().unwrap_or("-"),
                    m.session_name
                );
            }
        }
        RecordingCmd::Show { recording_id } => {
            if !valid_recording_id(&recording_id) {
                usage(&format!("{recording_id:?} is not a recording id"));
            }
            let v: Value = call(
                paths,
                "recording.get",
                json!({ "recordingId": recording_id }),
            )?;
            if json {
                print_json(&v);
                return Ok(());
            }
            let actions: Vec<ActionEntry> = serde_json::from_value(v["actions"].clone())
                .map_err(|e| error(codes::INTERNAL, e.to_string()))?;
            println!("seq  time  command  result  duration");
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
                println!(
                    "{}  {}  {}  {}  {}ms",
                    a.seq, a.ts, words, result, a.duration_ms
                );
            }
        }
    }
    Ok(())
}

/// The fields of agent-browser's `data` that are the answer of a command (eval's `result`,
/// `get text`/`value`/`title`/`url`), in the order one is chosen when several are there.
/// Whatever else `data` holds (`lifecycle`, `origin`, the selector) is not shown.
const PRIMARY_FIELDS: [&str; 5] = ["result", "text", "value", "title", "url"];

/// The text of an agent-browser result: its primary value when its data has one (a string as
/// it is, anything else as JSON), its data when that is one text, else JSON. `--json` shows
/// the whole output instead.
pub(crate) fn render_output(output: &Value) -> String {
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default();
    match output.get("data") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(map)) if PRIMARY_FIELDS.iter().any(|k| map.contains_key(*k)) => {
            let value = PRIMARY_FIELDS
                .iter()
                .find_map(|k| map.get(*k))
                .unwrap_or(&Value::Null);
            match value {
                Value::String(s) => s.clone(),
                other => pretty(other),
            }
        }
        Some(Value::Object(map)) => {
            let texts: Vec<&str> = map.values().filter_map(Value::as_str).collect();
            match (map.len(), texts.as_slice()) {
                (1, [text]) => (*text).to_string(),
                _ => match map.get("snapshot").and_then(Value::as_str) {
                    Some(s) => s.to_string(),
                    None => pretty(&Value::Object(map.clone())),
                },
            }
        }
        Some(Value::Null) | None => match output {
            Value::String(s) => s.clone(),
            other => pretty(other),
        },
        Some(other) => pretty(other),
    }
}

fn agent_command(
    paths: &Paths,
    session: String,
    argv: Vec<String>,
    json: bool,
) -> Result<(), RpcError> {
    let (session_id, token) = parse_handle(&session);
    let r: CommandExecResult = call(
        paths,
        "command.exec",
        CommandExecParams {
            session_id,
            argv,
            timeout_ms: None,
            token,
        },
    )?;
    if json {
        print_json(&r.output);
    } else {
        if succeeded(&r) {
            let text = render_output(&r.output);
            if !text.is_empty() {
                println!("{text}");
            }
        }
        tell_downloads(&r.output);
    }
    if succeeded(&r) {
        return Ok(());
    }
    eprintln!("pegboard: command failed: {}", failure_reason(&r));
    std::process::exit(EXIT_FAILURE)
}

/// What a command that ran but failed said: its `error`, else its stderr.
pub(crate) fn failure_reason(r: &CommandExecResult) -> String {
    r.output
        .get("error")
        .and_then(Value::as_str)
        .map(String::from)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| r.stderr.trim().to_string())
}

fn socket_answers(paths: &Paths) -> bool {
    UnixStream::connect(&paths.socket).is_ok()
}

fn io_failure(what: &str, e: impl std::fmt::Display) -> RpcError {
    error(codes::INTERNAL, format!("{what}: {e}"))
}

/// Whether some process holds the daemon lock (a daemon is running or starting).
fn lock_is_held(paths: &Paths) -> bool {
    matches!(DaemonLock::try_acquire(paths), Ok(None))
}

/// SIGTERM, then SIGKILL after 10 s, so the daemon can close its browser first.
fn stop_child(child: &mut std::process::Child) {
    // SAFETY: plain syscall on a child process of ours.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn spawn_daemon(paths: &Paths) -> Result<std::process::Child, RpcError> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(paths.home.join("daemon.log"))
        .map_err(|e| io_failure("opening daemon.log", e))?;
    let exe = std::env::current_exe().map_err(|e| io_failure("finding the executable", e))?;
    Command::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| io_failure("daemon.log", e))?)
        .stderr(log)
        // Its own process group, so a Ctrl-C in this terminal does not stop it.
        .process_group(0)
        .spawn()
        .map_err(|e| io_failure("starting the daemon", e))
}

/// A daemon that answers and is not shutting down.
fn daemon_is_usable(paths: &Paths) -> bool {
    Client::connect(paths)
        .and_then(|mut c| c.call::<_, BrowserStatus>("browser.status", json!({})))
        .is_ok_and(|s| !s.stopping)
}

/// Starts the daemon unless one is usable; `true` when this call started it.
pub(crate) fn browser_start(paths: &Paths) -> Result<bool, RpcError> {
    ensure_home(paths).map_err(|e| io_failure("creating the pegboard home", e))?;
    let log_path = paths.home.join("daemon.log");
    let deadline = Instant::now() + START_TIMEOUT;
    let mut child: Option<std::process::Child> = None;
    let mut spawned = false;
    loop {
        if socket_answers(paths) {
            // A daemon that is shutting down is waited out, then replaced.
            if daemon_is_usable(paths) {
                return Ok(spawned);
            }
        } else if !spawned && !lock_is_held(paths) {
            // Nobody serves and nobody is starting (or the one that was has gone): start one.
            child = Some(spawn_daemon(paths)?);
            spawned = true;
        }
        if let Some(c) = child.as_mut() {
            if let Ok(Some(status)) = c.try_wait() {
                // It lost the race to another daemon, which is fine; otherwise it failed.
                if lock_is_held(paths) {
                    child = None;
                } else {
                    return Err(error(
                        codes::BROWSER_FAILED,
                        format!("the daemon exited ({status}); see {}", log_path.display()),
                    ));
                }
            }
        }
        if Instant::now() >= deadline {
            if let Some(c) = child.as_mut() {
                stop_child(c);
            }
            return Err(error(
                codes::BROWSER_FAILED,
                format!(
                    "the daemon did not answer within 20 s; see {}",
                    log_path.display()
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn browser_stop(paths: &Paths) -> Result<(), RpcError> {
    let _: Value = call(paths, "browser.stop", json!({}))?;
    // The daemon answers first and then removes its socket.
    let deadline = Instant::now() + STOP_TIMEOUT;
    while socket_answers(paths) {
        if Instant::now() >= deadline {
            return Err(error(codes::INTERNAL, "the daemon did not stop in time"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("stopped");
    Ok(())
}

/// `pegboard daemon`: runs until `browser.stop`, SIGTERM/SIGINT or the browser's end. On
/// every path the sessions close first, then the browser, then the socket goes, and the
/// lock is released when the process exits.
fn daemon_main(paths: &Paths) -> Result<(), RpcError> {
    ensure_home(paths).map_err(|e| io_failure("creating the pegboard home", e))?;
    // Before anything is started or cleaned up, and held until the process ends.
    // A short grace: a client probing the lock (see `lock_is_held`) holds it for an instant.
    // It must stay short: a daemon that waits long could take over the lock from a daemon
    // that is being stopped, and nobody would be left to stop it.
    let mut tries = 0;
    let _lock = loop {
        match DaemonLock::try_acquire(paths) {
            Ok(Some(lock)) => break lock,
            Ok(None) if tries < 5 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => return Err(error(codes::INTERNAL, "already running")),
            Err(e) => return Err(io_failure("taking the daemon lock", e)),
        }
    };
    let runtime =
        tokio::runtime::Runtime::new().map_err(|e| io_failure("starting the runtime", e))?;
    let pid_file = paths.home.join("daemon.pid");
    let result = runtime.block_on(async {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate())?;
        let mut int = signal(SignalKind::interrupt())?;
        let daemon = Daemon::start(paths.clone()).await?;
        let mut pid = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&pid_file)?;
        writeln!(pid, "{}", std::process::id())?;
        eprintln!(
            "pegboard: daemon {} serving {}",
            std::process::id(),
            paths.socket.display()
        );
        // The server keeps answering (new sessions are refused) while the daemon shuts
        // down; it is dropped, and its socket removed, only after that.
        let stopper = async {
            tokio::select! {
                _ = daemon.wait_stopped() => {}
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
            daemon.shutdown().await;
            Ok(())
        };
        tokio::select! {
            r = serve(paths, daemon.clone()) => {
                daemon.shutdown().await;
                r
            }
            r = stopper => r,
        }
    });
    let _ = std::fs::remove_file(&pid_file);
    result.map_err(|e: anyhow::Error| error(codes::INTERNAL, format!("{e:#}")))
}

#[cfg(test)]
mod tests {
    #[test]
    fn limit_exceeded_is_a_usage_exit() {
        assert_eq!(super::exit_code("limit_exceeded"), 2);
        assert_eq!(super::exit_code("argv_rejected"), 2);
        assert_eq!(super::exit_code("timeout"), super::EXIT_FAILURE);
    }

    use super::*;

    #[test]
    fn output_shows_the_primary_value_only() {
        let r = |data: Value| render_output(&json!({ "success": true, "data": data }));
        // Lifecycle and other fields next to the value are left out.
        let noise = json!({ "restarted": false });
        assert_eq!(
            r(json!({ "url": "https://a.test/", "lifecycle": noise })),
            "https://a.test/"
        );
        assert_eq!(r(json!({ "title": "T", "lifecycle": noise })), "T");
        assert_eq!(
            r(json!({ "text": "0", "selector": "#a", "lifecycle": noise })),
            "0"
        );
        assert_eq!(r(json!({ "value": "v", "lifecycle": noise })), "v");
        // An eval result: a string as it is, anything else as JSON.
        assert_eq!(
            r(json!({ "result": "main", "origin": "o", "lifecycle": noise })),
            "main"
        );
        assert_eq!(r(json!({ "result": "", "lifecycle": noise })), "");
        assert_eq!(r(json!({ "result": 42, "lifecycle": noise })), "42");
        assert_eq!(r(json!({ "result": null, "lifecycle": noise })), "null");
        assert_eq!(
            r(json!({ "result": { "a": [1] }, "lifecycle": noise })),
            "{\n  \"a\": [\n    1\n  ]\n}"
        );
        // The result of eval comes before what else it tells.
        assert_eq!(r(json!({ "result": "r", "url": "u", "title": "t" })), "r");
        // Without a primary value: as before.
        assert_eq!(r(json!({ "snapshot": "- a", "refs": {} })), "- a");
        assert_eq!(r(json!({ "only": "one" })), "one");
        assert_eq!(
            r(json!({ "tabs": [], "lifecycle": noise })),
            "{\n  \"lifecycle\": {\n    \"restarted\": false\n  },\n  \"tabs\": []\n}"
        );
        assert_eq!(r(json!("plain")), "plain");
    }

    fn policy(domains: &[&str], private: bool, no_localhost: bool) -> SessionPolicy {
        SessionPolicy {
            allow_domains: domains.iter().map(|d| d.to_string()).collect(),
            allow_private_network: private,
            no_localhost,
            ..SessionPolicy::default()
        }
    }

    #[test]
    fn address_rules_say_what_is_in_force() {
        // No list: localhost open unless --no-localhost; private closed unless allowed.
        assert_eq!(
            address_rules(&policy(&[], false, false)),
            ("allowed", "blocked")
        );
        assert_eq!(
            address_rules(&policy(&[], true, false)),
            ("allowed", "allowed")
        );
        assert_eq!(
            address_rules(&policy(&[], false, true)),
            ("blocked", "blocked")
        );
        // A list: it alone decides, localhost included.
        assert_eq!(
            address_rules(&policy(&["a.test"], false, false)),
            ("only if listed", "blocked")
        );
        assert_eq!(
            address_rules(&policy(&["a.test"], true, false)),
            ("only if listed", "only if listed")
        );
        let line = describe_policy(&policy(&["a.test"], false, false));
        assert!(line.contains("localhost only if listed"), "{line}");
        assert!(!line.contains("localhost allowed"), "{line}");
    }
}
