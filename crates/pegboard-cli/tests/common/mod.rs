//! Helpers shared by the end-to-end test binaries of the `pegboard` CLI.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

pub fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_pegboard")
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head><body>{body}</body></html>"
    )
}

/// (extra headers, body) of a fixture path; the pages mirror spikes/lib/fixture.mjs.
fn route(path: &str) -> Option<(&'static str, String)> {
    Some(match path {
        "/counter" => (
            "",
            page(
                "counter",
                r#"<h1>counter</h1><button id="btn">Add</button> <span id="clicks">0</span><script>
document.getElementById('btn').addEventListener('click', () => {
  const el = document.getElementById('clicks'); el.textContent = String(Number(el.textContent) + 1);
});</script>"#,
            ),
        ),
        "/cookie/get" => (
            "",
            page(
                "cookie-get",
                r#"<pre id="out"></pre><script>
document.getElementById('out').textContent = JSON.stringify({ cookie: document.cookie, ls: localStorage.getItem('spike') });
</script>"#,
            ),
        ),
        "/cookie/set" => (
            "set-cookie: spike=1; Max-Age=86400; Path=/\r\n",
            page(
                "cookie-set",
                "<p>set</p><script>localStorage.setItem('spike', '1');</script>",
            ),
        ),
        "/marker" => (
            "",
            page(
                "marker",
                "<p>marker</p><script>window.pageMarker = 'main';</script>",
            ),
        ),
        "/form" => (
            "",
            page(
                "form",
                r#"<label>Name <input id="name"></label> <label>Password <input id="pw" type="password"></label>"#,
            ),
        ),
        _ => return None,
    })
}

/// Download fixtures keyed by a file name chosen by the test (see `DownloadProbe`):
/// `/dlpage/<name>` links to `/dl/<name>` (served as an attachment) and to `/zip/<name>.zip`
/// (a plain zip link without `Content-Disposition`, as in manual check M5). Returns the full
/// header block (content type included) and the body.
fn download_route(path: &str) -> Option<(String, String)> {
    if let Some(name) = path.strip_prefix("/dlpage/") {
        let body = page(
            "download",
            &format!(
                r#"<a id="att" href="/dl/{name}">attachment</a> <a id="zip" href="/zip/{name}.zip">zip</a>"#
            ),
        );
        return Some(("content-type: text/html\r\n".into(), body));
    }
    if let Some(name) = path.strip_prefix("/dl/") {
        return Some((
            format!(
                "content-type: application/octet-stream\r\ncontent-disposition: attachment; filename=\"{name}\"\r\n"
            ),
            "pegboard download test".into(),
        ));
    }
    if path.starts_with("/zip/") {
        return Some((
            "content-type: application/zip\r\n".into(),
            "pegboard download test".into(),
        ));
    }
    None
}

/// A file name no other run uses, and a read-only look at the real `~/Downloads` (where a
/// browser started by a test saves a download that nothing redirects; the product's browser
/// cannot be given another `HOME`, measured on Chrome for Testing 157). Only entries of
/// `~/Downloads` whose name contains the token are ever removed (on drop, so a failing test
/// cleans up what it caused).
pub struct DownloadProbe {
    pub token: String,
    downloads: std::path::PathBuf,
}

impl DownloadProbe {
    pub fn new() -> DownloadProbe {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let home = std::env::var_os("HOME").expect("HOME");
        DownloadProbe {
            token: format!("pegdl-{}-{nanos:x}", std::process::id()),
            downloads: Path::new(&home).join("Downloads"),
        }
    }

    /// Paths under `root` (recursively, not following symlinks) whose file name contains the token.
    pub fn find_under(&self, root: &Path) -> Vec<std::path::PathBuf> {
        let mut found = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if entry.file_name().to_string_lossy().contains(&self.token) {
                    found.push(path.clone());
                }
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    stack.push(path);
                }
            }
        }
        found
    }

    /// Entries of the real `~/Downloads` carrying the token (not recursive).
    pub fn in_real_downloads(&self) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(&self.downloads)
            .map(|d| {
                d.flatten()
                    .filter(|e| e.file_name().to_string_lossy().contains(&self.token))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Drop for DownloadProbe {
    fn drop(&mut self) {
        for path in self.in_real_downloads() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Serves the fixture pages on a loopback port for as long as the test process lives.
pub fn fixture() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req
                    .lines()
                    .next()
                    .and_then(|l| l.split(' ').nth(1))
                    .and_then(|p| p.split('?').next())
                    .unwrap_or("/");
                let resp = match (download_route(path), route(path)) {
                    (Some((headers, body)), _) => format!(
                        "HTTP/1.1 200 OK\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    ),
                    (None, Some((headers, body))) => format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    ),
                    (None, None) => "HTTP/1.1 404 Not Found\r\ncontent-length: 9\r\nconnection: close\r\n\r\nnot found"
                        .to_string(),
                };
                let _ = stream.write_all(resp.as_bytes());
            });
        }
    });
    origin
}
/// A fresh pegboard home. Dropping it stops the daemon and its browser, also when the
/// test failed.
pub struct Home {
    tmp: tempfile::TempDir,
}

impl Home {
    pub fn new() -> Home {
        Home {
            tmp: tempfile::tempdir().unwrap(),
        }
    }

    pub fn path(&self) -> &Path {
        self.tmp.path()
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(bin());
        cmd.env("PEGBOARD_HOME", self.path())
            .env_remove("PEGBOARD_SESSION")
            .args(args)
            .stdin(Stdio::null());
        cmd
    }

    pub fn run(&self, args: &[&str]) -> (i32, String, String) {
        self.run_env(&[], args)
    }

    pub fn run_env(&self, env: &[(&str, &str)], args: &[&str]) -> (i32, String, String) {
        let out = self
            .command(args)
            .envs(env.iter().copied())
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Like `run_env`, but killed (and the test failed) after `limit` seconds.
    pub fn run_limited(
        &self,
        env: &[(&str, &str)],
        args: &[&str],
        limit: u64,
    ) -> (i32, String, String) {
        let mut child = self
            .command(args)
            .envs(env.iter().copied())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(limit);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{args:?} did not finish within {limit} s");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let out = child.wait_with_output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    pub fn daemon_pid(&self) -> Option<i32> {
        std::fs::read_to_string(self.path().join("daemon.pid"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }
}

impl Home {
    /// Stops the daemon and its browser: `browser stop`, then SIGKILL of a daemon that is
    /// still there, then every browser that uses this home's profile directory.
    fn stop_everything(&self) {
        let _ = self.command(&["browser", "stop"]).output();
        if let Some(pid) = self.daemon_pid() {
            // A stale pid file could name an unrelated process: only a `pegboard daemon`
            // of this test binary is killed.
            let cmd = Command::new("ps")
                .args(["-o", "command=", "-p", &pid.to_string()])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            if cmd == format!("{} daemon", bin()) {
                // SAFETY: plain syscall on a verified pegboard daemon.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
        // Only a browser that uses this home's profile directory.
        let _ = Command::new("pkill")
            .args(["-f", "--"])
            .arg(format!(
                "--user-data-dir={}",
                self.path().join("chrome").display()
            ))
            .output();
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        self.stop_everything();
    }
}

static SHARED_HOME: std::sync::OnceLock<Home> = std::sync::OnceLock::new();

/// One pegboard home with a running daemon and browser for the whole test binary, started on
/// first use. Statics are never dropped, so an `atexit` hook stops the daemon and browser and
/// removes the home when the binary ends (also after a failing test), and a detached watcher
/// does the same if the binary is killed before the hook can run.
/// Tests that use it open their own sessions and close them; tests of start, stop, locks,
/// signals or stale files make their own `Home`.
pub fn shared_home() -> &'static Home {
    SHARED_HOME.get_or_init(|| {
        let home = Home::new();
        let rules = host_rules();
        let browser_rules = browser_only_host_rules();
        let (code, _, err) = home.run_env(
            &[
                ("PEGBOARD_TEST_HOST_RULES", rules.as_str()),
                ("PEGBOARD_TEST_BROWSER_HOST_RULES", browser_rules.as_str()),
            ],
            &["browser", "start"],
        );
        assert_eq!(code, 0, "{err}");
        guard_shared_home(home.path());
        // SAFETY: registers a plain `extern "C"` function without arguments.
        unsafe {
            libc::atexit(stop_shared_home);
        }
        home
    })
}

/// A name the shared browser (and its daemon) resolve to this machine's private LAN address
/// (`lan_ip`), or to an unroutable private address when the machine has none.
pub const PRIVATE_NAME: &str = "private.test";
/// A name that looks public but resolves like `PRIVATE_NAME`.
pub const PUBLIC_LOOKING_NAME: &str = "looks-public.example";
/// Names that resolve to 127.0.0.1, for allow-list tests.
pub const LISTED_NAME: &str = "listed.test";
pub const UNLISTED_NAME: &str = "unlisted.test";

/// This machine's private IPv4 address (RFC 1918) on the interface of its default route,
/// if it has one. A UDP `connect` only picks the route; nothing is sent.
pub fn lan_ip() -> Option<std::net::Ipv4Addr> {
    static LAN: std::sync::OnceLock<Option<std::net::Ipv4Addr>> = std::sync::OnceLock::new();
    *LAN.get_or_init(|| {
        let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        sock.connect("192.0.2.1:9").ok()?;
        match sock.local_addr().ok()?.ip() {
            std::net::IpAddr::V4(v4) if v4.is_private() => Some(v4),
            _ => None,
        }
    })
}

/// The `--host-resolver-rules` the shared browser starts with (`PEGBOARD_TEST_HOST_RULES`,
/// which the daemon's guard reads too).
pub fn host_rules() -> String {
    let private = lan_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "10.255.255.1".to_string());
    format!(
        "MAP {PRIVATE_NAME} {private}, MAP {PUBLIC_LOOKING_NAME} {private}, \
         MAP {LISTED_NAME} 127.0.0.1, MAP {UNLISTED_NAME} 127.0.0.1"
    )
}

/// A real public name (IANA's reserved example domain). The daemon resolves it through DNS to
/// its public address, while the shared browser connects to 127.0.0.1 under it
/// (`browser_only_host_rules`): pages "from a public address" served by a local test server.
/// Tests that use it need working DNS.
pub const PUBLIC_NAME: &str = "example.net";

/// Host resolver rules given to the browser only (`PEGBOARD_TEST_BROWSER_HOST_RULES`).
pub fn browser_only_host_rules() -> String {
    format!("MAP {PUBLIC_NAME} 127.0.0.1")
}

extern "C" fn stop_shared_home() {
    if let Some(home) = SHARED_HOME.get() {
        home.stop_everything();
        let _ = std::fs::remove_dir_all(home.path());
    }
}

/// A detached process that, once this test binary is gone (even killed), stops the daemon,
/// removes the home and kills the browser of that home, as the exit hook would have.
fn guard_shared_home(home: &Path) {
    let _ = Command::new("sh")
        .arg("-c")
        .arg(
            r#"while kill -0 "$1" 2>/dev/null; do sleep 1; done; PEGBOARD_HOME="$3" "$2" browser stop >/dev/null 2>&1; rm -rf -- "$3"; pkill -f -- "--user-data-dir=$3/chrome""#,
        )
        .arg("pegboard-test-guard")
        .arg(std::process::id().to_string())
        .arg(bin())
        .arg(home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

pub fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

pub fn json(text: &str) -> Value {
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("not json ({e}): {text:?}"))
}

pub fn base32(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}

/// Line 1 of `session create`: `<session id>.<token>`.
pub fn session_handle(stdout: &str) -> String {
    let first = stdout.lines().next().unwrap_or("");
    let ok = first.split_once('.').is_some_and(|(id, token)| {
        id.len() == 10
            && id.starts_with("s_")
            && base32(&id[2..])
            && token.len() == 26
            && base32(token)
    });
    assert!(ok, "first line is not a session handle: {stdout:?}");
    first.to_string()
}

pub fn id_of(handle: &str) -> &str {
    handle.split('.').next().unwrap()
}

pub fn token_of(handle: &str) -> &str {
    handle.split_once('.').unwrap().1
}

/// The `ps` lines of processes whose command line contains `needle`.
pub fn processes(needle: &str) -> Vec<String> {
    let out = Command::new("ps")
        .args(["-axo", "pid,command"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(needle))
        .map(String::from)
        .collect()
}

/// Plays the human: attaches to the browser directly through the `DevToolsActivePort` file
/// in the test's own home. That port is test-only knowledge; the CLI never exposes it.
pub struct Human {
    rt: tokio::runtime::Runtime,
    conn: pegboard_core::cdp::CdpConn,
    /// browserContextId of the human profile.
    pub context: String,
    /// The tab the human had open when the browser started.
    pub target: String,
    home: std::path::PathBuf,
    /// Human-profile tabs this helper opened itself (extra windows made by `establish_premise`).
    pub extra: std::cell::RefCell<Vec<String>>,
}

impl Human {
    /// Call right after `browser start`, before any session exists: the browser's only page
    /// is then the human's.
    pub fn attach(home: &Home) -> Human {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let port_file = home.path().join("chrome/DevToolsActivePort");
        let text = std::fs::read_to_string(&port_file).expect("DevToolsActivePort");
        let mut lines = text.lines();
        let (port, path) = (lines.next().unwrap(), lines.next().unwrap());
        let conn = rt
            .block_on(pegboard_core::cdp::CdpConn::connect(&format!(
                "ws://127.0.0.1:{port}{path}"
            )))
            .unwrap();
        let pages = rt
            .block_on(pegboard_core::cdp::page_targets(&conn))
            .unwrap();
        assert_eq!(
            pages.len(),
            1,
            "expected exactly the human's first tab: {pages:?}"
        );
        Human {
            rt,
            conn,
            home: home.path().to_path_buf(),
            extra: Default::default(),
            context: pages[0]
                .browser_context_id
                .clone()
                .expect("browserContextId"),
            target: pages[0].target_id.clone(),
        }
    }

    /// A CDP command straight to the browser, as the human's own connection.
    pub fn send(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.rt.block_on(self.conn.send(method, params, None))
    }

    /// A subscription to the events this connection receives.
    pub fn events(&self) -> tokio::sync::broadcast::Receiver<pegboard_core::cdp::CdpEvent> {
        self.conn.events()
    }

    /// Waits up to `limit` for an event with this method on `events`.
    pub fn wait_event(
        &self,
        events: &mut tokio::sync::broadcast::Receiver<pegboard_core::cdp::CdpEvent>,
        method: &str,
        limit: Duration,
    ) -> Option<Value> {
        self.rt.block_on(async {
            tokio::time::timeout(limit, async {
                loop {
                    match events.recv().await {
                        Ok(e) if e.method == method => return Some(e.params),
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(_) => return None,
                    }
                }
            })
            .await
            .ok()
            .flatten()
        })
    }

    pub fn pages(&self) -> Vec<pegboard_core::cdp::TargetInfo> {
        self.rt
            .block_on(pegboard_core::cdp::page_targets(&self.conn))
            .unwrap()
    }

    /// Page targets that are not in the human profile.
    pub fn agent_pages(&self) -> Vec<pegboard_core::cdp::TargetInfo> {
        self.pages()
            .into_iter()
            .filter(|t| t.browser_context_id.as_deref() != Some(self.context.as_str()))
            .collect()
    }

    pub fn human_pages(&self) -> Vec<pegboard_core::cdp::TargetInfo> {
        self.pages()
            .into_iter()
            .filter(|t| t.browser_context_id.as_deref() == Some(self.context.as_str()))
            .collect()
    }

    /// Evaluates in any page (the human is allowed to), awaiting promises.
    pub fn evaluate(&self, target: &str, expression: &str) -> Value {
        self.rt.block_on(async {
            let session = self.conn.attach(target).await.unwrap();
            let v = self.conn.evaluate(&session, expression).await.unwrap();
            let _ = self
                .conn
                .send(
                    "Target.detachFromTarget",
                    serde_json::json!({ "sessionId": session }),
                    None,
                )
                .await;
            v
        })
    }

    pub fn navigate(&self, target: &str, url: &str) {
        self.rt.block_on(async {
            let session = self.conn.attach(target).await.unwrap();
            self.conn.navigate(&session, url).await.unwrap();
            let _ = self
                .conn
                .send(
                    "Target.detachFromTarget",
                    serde_json::json!({ "sessionId": session }),
                    None,
                )
                .await;
        });
    }

    /// Opens a tab in the human profile, in front, and returns its target id. CDP cannot
    /// address the human profile's context in `Target.createTarget` (without a context the
    /// browser picks the profile of its last active window), so the human page opens the tab
    /// itself, as a person clicking a link would.
    pub fn open_tab(&self, url: &str) -> String {
        let before: Vec<String> = self.pages().into_iter().map(|t| t.target_id).collect();
        let script = format!("window.open('{url}'); 'opened'");
        self.rt
            .block_on(async {
                let session = self.conn.attach(&self.target).await?;
                let r = self
                    .conn
                    .send(
                        "Runtime.evaluate",
                        serde_json::json!({ "expression": script, "userGesture": true }),
                        Some(&session),
                    )
                    .await;
                let _ = self
                    .conn
                    .send(
                        "Target.detachFromTarget",
                        serde_json::json!({ "sessionId": session }),
                        None,
                    )
                    .await;
                r
            })
            .unwrap();
        for _ in 0..50 {
            let new = self
                .human_pages()
                .into_iter()
                .map(|t| t.target_id)
                .find(|id| !before.contains(id));
            if let Some(id) = new {
                return id;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("the human page did not open a tab");
    }

    /// Opens a new window of the human profile the way a person starting Chrome again would: the
    /// Chrome executable is re-invoked with this home's user-data-dir (never any other), which
    /// hands the request to the running browser and exits. Returns the new tab's target id.
    pub fn open_human_window_via_chrome(&self) -> String {
        let before: Vec<String> = self.pages().into_iter().map(|t| t.target_id).collect();
        let chrome = pegboard_core::browser::find_chrome().expect("chrome");
        let status = Command::new(chrome)
            .arg(format!(
                "--user-data-dir={}",
                self.home.join("chrome").display()
            ))
            .arg("--profile-directory=Default")
            .arg("--no-first-run")
            .arg("--new-window")
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "re-invoking Chrome failed: {status}");
        for _ in 0..50 {
            let new = self
                .human_pages()
                .into_iter()
                .map(|t| t.target_id)
                .find(|id| !before.contains(id));
            if let Some(id) = new {
                self.extra.borrow_mut().push(id.clone());
                return id;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("re-invoking Chrome did not open a human window");
    }

    /// Tries to make the human window the browser's last active one (see
    /// `establish_human_active`); when the control says it is not, re-invokes Chrome for a new
    /// human window and runs the control again. True when the control now lands in the human
    /// profile.
    pub fn establish_premise(&self) -> bool {
        if self.establish_human_active(&self.target) == self.context {
            return true;
        }
        let window = self.open_human_window_via_chrome();
        self.establish_human_active(&window) == self.context
    }

    /// Premise control for "the human window was the last active one": activates the human
    /// tab, then sends a context-less `Target.createTarget` straight to the browser, which
    /// puts the tab in the profile of the last active window. Returns the context that tab
    /// landed in (it is closed again); equal to `self.context` when the premise holds.
    /// Measured on Chrome 157 while another application has the operating-system focus: after
    /// `Target.activateTarget` and `Page.bringToFront` alone (and after minimizing the agent
    /// windows) the control tab still lands in the agent profile; after `establish_premise`
    /// re-invokes Chrome for a new human window it lands in the human profile.
    pub fn establish_human_active(&self, target: &str) -> String {
        self.activate(target);
        self.rt.block_on(async {
            if let Ok(session) = self.conn.attach(target).await {
                let _ = self
                    .conn
                    .send("Page.bringToFront", serde_json::json!({}), Some(&session))
                    .await;
                let _ = self
                    .conn
                    .send(
                        "Target.detachFromTarget",
                        serde_json::json!({ "sessionId": session }),
                        None,
                    )
                    .await;
            }
        });
        let r = self
            .rt
            .block_on(self.conn.send(
                "Target.createTarget",
                serde_json::json!({ "url": "about:blank" }),
                None,
            ))
            .unwrap();
        let id = r["targetId"].as_str().unwrap().to_string();
        let context = self
            .pages()
            .into_iter()
            .find(|t| t.target_id == id)
            .and_then(|t| t.browser_context_id)
            .expect("control tab not listed");
        let _ = self.rt.block_on(self.conn.send(
            "Target.closeTarget",
            serde_json::json!({ "targetId": id }),
            None,
        ));
        for _ in 0..50 {
            if self.pages().iter().all(|t| t.target_id != id) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        context
    }

    /// Closes a tab the way a person does: through the browser-level connection.
    pub fn close_target(&self, target: &str) {
        self.rt
            .block_on(self.conn.send(
                "Target.closeTarget",
                serde_json::json!({ "targetId": target }),
                None,
            ))
            .unwrap();
        for _ in 0..50 {
            if self.pages().iter().all(|t| t.target_id != target) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("tab {target} did not close");
    }

    /// Tab groups and tabs of the agent profile `context` as Chrome itself reports them: evaluated in
    /// the bridge extension's service worker (test-only knowledge, like the debugging port).
    /// `{"groups": [{id, title, windowId}], "tabs": [{id, groupId, windowId, url}]}`; a tab
    /// with `groupId` -1 is ungrouped.
    pub fn agent_tab_state(&self, context: &str) -> Value {
        let expr = "(async () => JSON.stringify({ \
            groups: (await chrome.tabGroups.query({})).map(g => ({id: g.id, title: g.title, windowId: g.windowId})), \
            tabs: (await chrome.tabs.query({})).map(t => ({id: t.id, groupId: t.groupId, windowId: t.windowId, url: t.url})) }))()";
        let text = self.rt.block_on(async {
            let r = self
                .conn
                .send("Target.getTargets", serde_json::json!({}), None)
                .await
                .unwrap();
            let worker = r["targetInfos"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| {
                    t["type"] == "service_worker"
                        && t["browserContextId"].as_str() == Some(context)
                        && t["url"].as_str().is_some_and(|u| u.ends_with("/sw.js"))
                })
                .expect("bridge worker of the profile")["targetId"]
                .as_str()
                .unwrap()
                .to_string();
            let session = self.conn.attach(&worker).await.unwrap();
            let v = self.conn.evaluate(&session, expr).await.unwrap();
            let _ = self
                .conn
                .send(
                    "Target.detachFromTarget",
                    serde_json::json!({ "sessionId": session }),
                    None,
                )
                .await;
            v
        });
        serde_json::from_str(text.as_str().expect("state text")).unwrap()
    }

    /// Brings the target (and its window) to the front, as a person clicking it would.
    pub fn activate(&self, target: &str) {
        self.rt
            .block_on(self.conn.send(
                "Target.activateTarget",
                serde_json::json!({ "targetId": target }),
                None,
            ))
            .unwrap();
    }
}
