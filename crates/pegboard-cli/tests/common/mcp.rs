//! `Mcp`: a `pegboard mcp` child process driven over stdin and stdout as an MCP client does,
//! and helpers for the page-content markers it puts around page text. Used by e2e_mcp and
//! e2e_mcp_isolation.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::Home;

/// A `pegboard mcp` child process.
pub struct Mcp {
    pub child: Child,
    stdin: Option<ChildStdin>,
    replies: Receiver<String>,
    next_id: u64,
    /// Every line it wrote to stdout.
    pub lines: Vec<String>,
    stderr: Arc<Mutex<String>>,
    /// While set, nothing reads the server's stdout (a client that stopped reading).
    pub paused: Arc<AtomicBool>,
}

impl Mcp {
    pub fn start(home: &Home, args: &[&str]) -> Mcp {
        Mcp::start_env(home, &[], args)
    }

    pub fn start_env(home: &Home, env: &[(&str, &str)], args: &[&str]) -> Mcp {
        let mut full = vec!["mcp"];
        full.extend_from_slice(args);
        let mut child = home
            .command(&full)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, replies) = channel();
        let stdout = child.stdout.take().unwrap();
        let paused = Arc::new(AtomicBool::new(false));
        let hold = paused.clone();
        std::thread::spawn(move || {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                while hold.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(20));
                }
                let Some(Ok(line)) = lines.next() else { return };
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let mut err = child.stderr.take().unwrap();
        let sink = stderr.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 {
                    return;
                }
                sink.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        let mut mcp = Mcp {
            stdin: child.stdin.take(),
            child,
            replies,
            next_id: 0,
            lines: Vec::new(),
            stderr,
            paused,
        };
        let r = mcp.request(
            "initialize",
            json!({ "protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": { "name": "e2e", "version": "1" } }),
        );
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18", "{r}");
        mcp.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        mcp
    }

    pub fn send(&mut self, msg: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    /// Sends a request without waiting for its answer.
    pub fn request_no_wait(&mut self, method: &str, params: Value) {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
    }

    pub fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let deadline = Instant::now() + Duration::from_secs(240);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .replies
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no answer to {method}; stderr: {}", self.stderr()));
            self.lines.push(line.clone());
            let v: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("stdout carries a non-protocol line ({e}): {line:?}"));
            assert_eq!(v["jsonrpc"], "2.0", "{line}");
            if v["id"] == id {
                return v;
            }
        }
    }

    /// (isError, text) of a tool call.
    pub fn call(&mut self, tool: &str, args: Value) -> (bool, String) {
        let r = self.request("tools/call", json!({ "name": tool, "arguments": args }));
        let result = &r["result"];
        assert!(result.is_object(), "{r}");
        (
            result["isError"] == true,
            result["content"][0]["text"].as_str().unwrap().to_string(),
        )
    }

    /// Creates a session; its id.
    pub fn create(&mut self, args: Value) -> String {
        let (err, text) = self.call("pegboard_session_create", args);
        assert!(!err, "{text}");
        text.split_whitespace()
            .find(|w| w.starts_with("s_"))
            .unwrap()
            .to_string()
    }

    pub fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    pub fn signal(&self, sig: i32) {
        // SAFETY: plain syscall on our child process.
        unsafe { libc::kill(self.child.id() as i32, sig) };
    }

    /// Its exit code; the test fails if it has not exited within `secs`.
    pub fn wait_exit(&mut self, secs: u64) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code().unwrap_or(-1);
            }
            assert!(
                Instant::now() < deadline,
                "pegboard mcp did not exit within {secs} s"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Mcp {
    /// Ends the server the way a client does (SIGTERM: it closes the sessions it created),
    /// also when the test failed, so no session of a failed test stays open and pins the
    /// shared profile's policy for the tests after it. Killed if it does not end in 20 s.
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.signal(libc::SIGTERM);
            let deadline = Instant::now() + Duration::from_secs(20);
            while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Mcp {
    /// The `result` of a tool call (null when the call was refused as a JSON-RPC error).
    pub fn call_result(&mut self, tool: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({ "name": tool, "arguments": args }));
        r["result"].clone()
    }

    /// Whether a call was refused one way or another: a JSON-RPC error (no such tool) or a
    /// tool error.
    pub fn refused(&mut self, tool: &str, args: Value) -> (bool, String) {
        let r = self.request("tools/call", json!({ "name": tool, "arguments": args }));
        if r["error"].is_object() {
            return (
                true,
                r["error"]["message"].as_str().unwrap_or("").to_string(),
            );
        }
        (
            r["result"]["isError"] == true,
            r["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        )
    }
}

pub const START_MARKER: &str = "--- AGENT_BROWSER_PAGE_CONTENT nonce=";

/// (nonce, inner text, what follows the end marker) of a text pegboard wrapped in
/// page-content markers; `None` when it is not wrapped.
pub fn wrapped(text: &str) -> Option<(String, String, String)> {
    let first = text.lines().next()?;
    let nonce = first
        .strip_prefix(START_MARKER)?
        .split(' ')
        .next()?
        .to_string();
    let end = format!("\n--- END_AGENT_BROWSER_PAGE_CONTENT nonce={nonce} ---");
    let body = &text[first.len() + 1..];
    let at = body.rfind(&end)?;
    let after = body[at + end.len()..].trim_start_matches('\n').to_string();
    Some((nonce, body[..at].to_string(), after))
}

/// The text without pegboard's page-content markers (a failure's reason is wrapped after
/// `command_failed:` and a line break).
pub fn unwrapped(text: &str) -> String {
    if let Some(rest) = text.strip_prefix("command_failed:\n") {
        return format!("command_failed: {}", unwrapped(rest));
    }
    match wrapped(text) {
        Some((_, inner, after)) if after.is_empty() => inner,
        Some((_, inner, after)) => format!("{inner}\n{after}"),
        None => text.to_string(),
    }
}
