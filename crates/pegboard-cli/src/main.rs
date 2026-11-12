//! The `pegboard` command line: manages the daemon, profiles, sessions and recordings, and
//! sends every other command to agent-browser through the daemon.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use pegboard_core::actionlog::{valid_recording_id, ActionEntry, Manifest};
use pegboard_core::api::{serve, Client};
use pegboard_core::daemon::Daemon;
use pegboard_core::lock::DaemonLock;
use pegboard_core::paths::{ensure_home, paths, socket_path_fits, Paths};
use pegboard_core::runner::succeeded;
use pegboard_protocol::{
    codes, BrowserStatus, CommandExecParams, CommandExecResult, ProfileInfo, RpcError,
    SessionCreateParams, SessionId, SessionInfo,
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
    match e.code.as_str() {
        codes::NOT_RUNNING => EXIT_NO_DAEMON,
        codes::ARGV_REJECTED | codes::INVALID_PARAMS => EXIT_USAGE,
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
        Cmd::Browser { command } => match command {
            BrowserCmd::Start => browser_start(paths),
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
        Cmd::Agent(argv) => {
            let id = cli
                .session
                .or_else(|| std::env::var("PEGBOARD_SESSION").ok())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| usage("no session: pass --session or set PEGBOARD_SESSION"));
            if !id
                .split_once('.')
                .is_some_and(|(_, token)| !token.is_empty())
            {
                usage("session handle must be <id>.<token> (the first line printed by `pegboard session create`)");
            }
            agent_command(paths, id, argv, json)
        }
    }
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
        SessionCmd::Create { name, url, profile } => {
            let s: SessionInfo = call(
                paths,
                "session.create",
                SessionCreateParams { name, url, profile },
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

/// The text of an agent-browser result: its data when that is one text, else JSON.
fn render_output(output: &Value) -> String {
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default();
    match output.get("data") {
        Some(Value::String(s)) => s.clone(),
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
    } else if succeeded(&r) {
        let text = render_output(&r.output);
        if !text.is_empty() {
            println!("{text}");
        }
    }
    if succeeded(&r) {
        return Ok(());
    }
    let why = r
        .output
        .get("error")
        .and_then(Value::as_str)
        .map(String::from)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| r.stderr.trim().to_string());
    eprintln!("pegboard: command failed: {why}");
    std::process::exit(EXIT_FAILURE)
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

fn browser_start(paths: &Paths) -> Result<(), RpcError> {
    ensure_home(paths).map_err(|e| io_failure("creating the pegboard home", e))?;
    let log_path = paths.home.join("daemon.log");
    let deadline = Instant::now() + START_TIMEOUT;
    let mut child: Option<std::process::Child> = None;
    let mut spawned = false;
    loop {
        if socket_answers(paths) {
            // A daemon that is shutting down is waited out, then replaced.
            if daemon_is_usable(paths) {
                println!(
                    "{}",
                    if spawned {
                        "started"
                    } else {
                        "already running"
                    }
                );
                return Ok(());
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
