//! The `pegboard` command line: manages the daemon, profiles, sessions and recordings, and
//! sends every other command to agent-browser through the daemon.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use pegboard_core::actionlog::{ActionEntry, Manifest};
use pegboard_core::api::{serve, Client};
use pegboard_core::daemon::Daemon;
use pegboard_core::paths::{ensure, paths, Paths};
use pegboard_core::runner::succeeded;
use pegboard_protocol::{
    codes, BrowserStatus, CommandExecParams, CommandExecResult, ProfileInfo, RpcError,
    SessionCloseParams, SessionCreateParams, SessionId, SessionInfo,
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
        codes::ARGV_REJECTED => EXIT_USAGE,
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
                        if s.running { "running" } else { "stopped" },
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
            if json {
                print_json(&s);
            } else {
                // First line: the id, so `export PEGBOARD_SESSION=$(pegboard session create)` works.
                println!("{}", s.id.0);
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
            let s: SessionInfo = call(paths, "session.info", json!({ "sessionId": id }))?;
            if json {
                print_json(&s);
            } else {
                println!("{}", describe(&s));
            }
        }
        SessionCmd::Close { id, keep_tabs } => {
            let s: SessionInfo = call(
                paths,
                "session.close",
                SessionCloseParams {
                    session_id: SessionId(id),
                    keep_tabs,
                },
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
            let v: Value = call(paths, "recording.get", json!({ "recordingId": recording_id }))?;
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
                println!("{}  {}  {}  {}  {}ms", a.seq, a.ts, words, result, a.duration_ms);
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

fn agent_command(paths: &Paths, session: String, argv: Vec<String>, json: bool) -> Result<(), RpcError> {
    let r: CommandExecResult = call(
        paths,
        "command.exec",
        CommandExecParams {
            session_id: SessionId(session),
            argv,
            timeout_ms: None,
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

fn browser_start(paths: &Paths) -> Result<(), RpcError> {
    if socket_answers(paths) {
        println!("already running");
        return Ok(());
    }
    ensure(paths).map_err(|e| io_failure("creating the pegboard home", e))?;
    let log_path = paths.home.join("daemon.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log_path)
        .map_err(|e| io_failure("opening daemon.log", e))?;
    let exe = std::env::current_exe().map_err(|e| io_failure("finding the executable", e))?;
    let mut child = Command::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| io_failure("daemon.log", e))?)
        .stderr(log)
        // Its own process group, so a Ctrl-C in this terminal does not stop it.
        .process_group(0)
        .spawn()
        .map_err(|e| io_failure("starting the daemon", e))?;
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        // Not `Client::connect`: it deletes a socket that refuses connections, and a
        // daemon that is just binding would lose its own.
        if socket_answers(paths) {
            println!("started (daemon pid {})", child.id());
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(error(
                codes::BROWSER_FAILED,
                format!("the daemon exited ({status}); see {}", log_path.display()),
            ));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(error(
                codes::BROWSER_FAILED,
                format!("the daemon did not answer within 20 s; see {}", log_path.display()),
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

/// `pegboard daemon`: runs until `browser.stop`, SIGTERM/SIGINT or the browser's end.
fn daemon_main(paths: &Paths) -> Result<(), RpcError> {
    if socket_answers(paths) {
        return Err(error(codes::INTERNAL, "already running"));
    }
    let runtime = tokio::runtime::Runtime::new().map_err(|e| io_failure("starting the runtime", e))?;
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
        eprintln!("pegboard: daemon {} serving {}", std::process::id(), paths.socket.display());
        let served = tokio::select! {
            r = serve(paths, daemon.clone()) => r,
            _ = daemon.wait_stopped() => Ok(()),
            _ = term.recv() => Ok(()),
            _ = int.recv() => Ok(()),
        };
        // `serve` is dropped by now, so its socket file is gone.
        daemon.shutdown().await;
        served
    });
    let _ = std::fs::remove_file(&pid_file);
    result.map_err(|e: anyhow::Error| error(codes::INTERNAL, format!("{e:#}")))
}
