//! The browser the daemon drives, in one of three configurations:
//! - stock Chrome, launched by the daemon (DevTools on a port, the bridge extension);
//! - the Pegboard fork launched by the daemon (tests and development, F13: DevTools on a pipe,
//!   the `Pegboard` domain);
//! - the Pegboard fork that launched the daemon and handed it a DevTools pipe (hosted).

use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;
use tokio::process::{Child, Command};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::bridge::domain::{self, ProfileEvent};
use crate::bridge::{DomainBridge, ExtensionBridge, TabBridge};
use crate::cdp::{page_targets, CdpConn, CdpEvent};
use crate::confirm::ConfirmBroker;
use crate::contexts::{is_agent_profile_dir, AgentContexts};
use crate::downloads::Downloads;
use crate::link::BrowserLink;
use crate::paths::{ensure, Paths};

/// The profile stock mode launches Chrome with (a person's). Only the launch uses it:
/// whether a context is a person's is never decided by comparing to it (`AgentContexts`).
const HUMAN_PROFILE_DIR: &str = "Default";

/// The executable inside the fork's app bundle.
const FORK_EXECUTABLE: &str = "Contents/MacOS/Pegboard";

/// A browser the daemon launches itself.
#[derive(Debug, Clone)]
pub enum BrowserConfig {
    Stock {
        chrome: PathBuf,
        extension_dir: PathBuf,
    },
    /// Tests and development (F13): the daemon launches the fork with
    /// `--remote-debugging-pipe --pegboard-no-daemon` and is the parent of the pipe.
    ForkLaunched { binary: PathBuf },
}

/// Features every test launch of the fork turns off. Chromium's code-sign clone
/// (`kMacAppCodeSignClone`, on by default) copies the app bundle at each launch and removes
/// the copy only on a normal quit; tests kill the browser, so the copies piled up in
/// `$TMPDIR/../X/app.pegboard.browser.code_sign_clone/` until a reboot (888 of them, about
/// 35 GB, on the test machine). The product's launch keeps it: a person quits normally, and
/// the clone keeps a running app whole while an update replaces its bundle.
pub const FORK_TEST_DISABLED_FEATURES: &[&str] = &["MacAppCodeSignClone"];

/// `extra` (a test launch's own arguments) with every `--disable-features=` in it merged
/// with `FORK_TEST_DISABLED_FEATURES` into one flag at the end: Chromium reads only the last
/// `--disable-features`, so a second one would undo the first.
pub fn fork_test_args<S: AsRef<str>>(extra: &[S]) -> Vec<String> {
    const FLAG: &str = "--disable-features=";
    let mut features: Vec<String> = FORK_TEST_DISABLED_FEATURES
        .iter()
        .map(|f| f.to_string())
        .collect();
    let mut args = Vec::new();
    for arg in extra.iter().map(AsRef::as_ref) {
        match arg.strip_prefix(FLAG) {
            Some(list) => features.extend(
                list.split(',')
                    .filter(|f| !f.is_empty() && !features.iter().any(|g| g == f))
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
            ),
            None => args.push(arg.to_string()),
        }
    }
    args.push(format!("{FLAG}{}", features.join(",")));
    args
}

/// The fork's app bundle or executable the tests are to launch: the test variable
/// `PEGBOARD_TEST_BROWSER=fork:<Pegboard.app>`. `None`: stock Chrome.
pub fn test_fork_binary() -> Option<PathBuf> {
    let v = crate::testenv::var("PEGBOARD_TEST_BROWSER")?;
    let path = v.strip_prefix("fork:")?;
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// Test-only: `PEGBOARD_TEST_BROWSER_NO_HOOKS` (with test hooks on) launches the fork without
/// `PEGBOARD_TEST_HOOKS` in its environment, so the browser's own test hooks (its dialogs'
/// answer hook) are off while the daemon's are on.
fn test_browser_without_hooks() -> bool {
    crate::testenv::var("PEGBOARD_TEST_BROWSER_NO_HOOKS").is_some()
}

/// The executable of `binary`: itself, or the one inside it when it is an app bundle.
pub fn fork_executable(binary: &Path) -> PathBuf {
    if binary.is_dir() {
        binary.join(FORK_EXECUTABLE)
    } else {
        binary.to_path_buf()
    }
}

/// `$PEGBOARD_CHROME`, else the newest Chrome for Testing under `~/.agent-browser/browsers`.
pub fn find_chrome() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("PEGBOARD_CHROME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    let root = PathBuf::from(home).join(".agent-browser/browsers");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .with_context(|| format!("reading {}", root.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("chrome-"))
        })
        .collect();
    // Version numbers compare numerically, not lexicographically.
    dirs.sort_by_key(|p| version_key(p));
    for dir in dirs.iter().rev() {
        let bin =
            dir.join("Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing");
        if bin.exists() {
            return Ok(bin);
        }
    }
    bail!(
        "no Chrome found under {}; set PEGBOARD_CHROME",
        root.display()
    )
}

fn version_key(p: &std::path::Path) -> Vec<u64> {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .trim_start_matches("chrome-")
        .split('.')
        .map(|c| c.parse().unwrap_or(0))
        .collect()
}

/// The two pipes of a DevTools pipe connection. The child reads fd 3 and writes fd 4; the
/// parent writes `parent_write` and reads `parent_read`. Every descriptor is close-on-exec
/// here, so no other child inherits one.
pub struct DevToolsPipes {
    parent_read: OwnedFd,
    parent_write: OwnedFd,
    child_read: OwnedFd,
    child_write: OwnedFd,
}

fn cloexec_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: `fds` has room for the two descriptors pipe writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: pipe returned two open descriptors that nothing else owns.
    let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&r, &w] {
        // SAFETY: plain fcntl on an open descriptor.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok((r, w))
}

impl DevToolsPipes {
    pub fn new() -> std::io::Result<DevToolsPipes> {
        let (child_read, parent_write) = cloexec_pipe()?;
        let (parent_read, child_write) = cloexec_pipe()?;
        Ok(DevToolsPipes {
            parent_read,
            parent_write,
            child_read,
            child_write,
        })
    }

    /// For `pre_exec` of the child's command: puts the child's ends at fds 3 and 4 (open
    /// across exec). Runs between fork and exec, so it only makes system calls.
    pub fn child_setup(&self) -> impl FnMut() -> std::io::Result<()> + Send + Sync + 'static {
        let (r, w) = (self.child_read.as_raw_fd(), self.child_write.as_raw_fd());
        move || {
            // Out of the way first: either end may sit at 3 or 4 already.
            // SAFETY: async-signal-safe calls on descriptors open in the forked child.
            unsafe {
                let r2 = libc::fcntl(r, libc::F_DUPFD, 10);
                let w2 = libc::fcntl(w, libc::F_DUPFD, 10);
                if r2 < 0 || w2 < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // dup2 to another number leaves the new descriptor open across exec.
                if libc::dup2(r2, 3) < 0 || libc::dup2(w2, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(r2);
                libc::close(w2);
            }
            Ok(())
        }
    }

    /// The parent's ends (read, write); the child's ends are closed here, so the parent
    /// sees the end of the pipe when the child goes.
    pub fn into_parent_ends(self) -> (OwnedFd, OwnedFd) {
        (self.parent_read, self.parent_write)
    }
}

/// What differs between stock Chrome and the fork.
enum Mode {
    Stock {
        chrome: PathBuf,
        user_data_dir: PathBuf,
        /// The context of the profile Chrome was launched with: never taken for an agent
        /// profile's window (only that; it does not decide what is a person's).
        launch_context: String,
    },
    /// The fork: the `Pegboard` domain on `conn` (claimed), profiles from the domain.
    Fork,
}

pub struct Browser {
    /// The browser process (hosted: the daemon's parent).
    pub pid: u32,
    /// The DevTools websocket (stock mode only: the browser listens on a port).
    pub ws_url: Option<String>,
    /// The daemon's one connection to the browser; every connection it makes rides it.
    pub link: BrowserLink,
    /// The control connection. On the fork it holds the `Pegboard` domain.
    pub conn: CdpConn,
    /// The agent profiles' contexts. Every other context is a person's.
    pub agents: AgentContexts,
    /// The browser process when the daemon launched it; `None` when the browser hosts the
    /// daemon (it is not the daemon's to close).
    child: Option<Mutex<Child>>,
    mode: Mode,
    /// Agent profile directory -> browserContextId, for profiles whose window we opened.
    profiles: Mutex<HashMap<String, String>>,
    /// The agent profiles' download policies and their downloads (`downloads`).
    pub downloads: Downloads,
    /// Fork: keeps `agents` in step with `Pegboard.profileLoaded`/`profileUnloaded`.
    profile_watch: Option<JoinHandle<()>>,
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Some(w) = &self.profile_watch {
            w.abort();
        }
    }
}

/// What the fork's control connection gives at start.
struct ForkParts {
    conn: CdpConn,
    agents: AgentContexts,
    downloads: Downloads,
    profile_watch: JoinHandle<()>,
}

impl Browser {
    /// Launches the browser of `cfg` and connects to it.
    pub async fn launch(paths: &Paths, cfg: &BrowserConfig) -> Result<Browser> {
        match cfg {
            BrowserConfig::Stock {
                chrome,
                extension_dir,
            } => Self::launch_stock(paths, chrome, extension_dir).await,
            BrowserConfig::ForkLaunched { binary } => Self::launch_fork(paths, binary).await,
        }
    }

    async fn launch_stock(paths: &Paths, chrome: &Path, extension_dir: &Path) -> Result<Browser> {
        ensure(paths)?;
        record_launch();
        let port_file = paths.chrome_udd.join("DevToolsActivePort");
        let _ = std::fs::remove_file(&port_file);
        let mut child = Command::new(chrome)
            .arg(format!("--user-data-dir={}", paths.chrome_udd.display()))
            .arg(format!("--profile-directory={HUMAN_PROFILE_DIR}"))
            .arg("--remote-debugging-port=0")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg(format!("--load-extension={}", extension_dir.display()))
            .arg("--disable-features=DisableLoadExtensionCommandLineSwitch")
            .args(test_host_rules())
            .args(test_address_spaces())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting {}", chrome.display()))?;
        let pid = child
            .id()
            .ok_or_else(|| anyhow!("chrome exited immediately"))?;

        let ws_url = match wait_for_ws_url(&port_file, &mut child).await {
            Ok(u) => u,
            Err(e) => {
                let _ = child.kill().await;
                return Err(e);
            }
        };
        let agents = AgentContexts::default();
        let setup = async {
            let link = BrowserLink::websocket(&ws_url).await?;
            let conn = CdpConn::over(&link).await?;
            let launch_context = wait_for_first_context(&conn).await?;
            let downloads = Downloads::new(&link, agents.clone(), conn.clone(), paths)?;
            anyhow::Ok((link, conn, launch_context, downloads))
        };
        let (link, conn, launch_context, downloads) = match setup.await {
            Ok(v) => v,
            Err(e) => {
                let _ = child.kill().await;
                return Err(e);
            }
        };
        Ok(Browser {
            pid,
            ws_url: Some(ws_url),
            link,
            conn,
            agents,
            child: Some(Mutex::new(child)),
            mode: Mode::Stock {
                chrome: chrome.to_path_buf(),
                user_data_dir: paths.chrome_udd.clone(),
                launch_context,
            },
            profiles: Mutex::new(HashMap::new()),
            downloads,
            profile_watch: None,
        })
    }

    /// F13: the fork with its DevTools on a pipe whose parent is the daemon, and no daemon
    /// of its own. The test flags go with it as with stock Chrome.
    async fn launch_fork(paths: &Paths, binary: &Path) -> Result<Browser> {
        ensure(paths)?;
        record_launch();
        let exe = fork_executable(binary);
        let pipes = DevToolsPipes::new().context("making the DevTools pipes")?;
        let mut cmd = Command::new(&exe);
        cmd.arg(format!("--user-data-dir={}", paths.chrome_udd.display()))
            .arg("--remote-debugging-pipe")
            .arg("--pegboard-no-daemon")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            // Test and development only: a build's first "Pegboard Safe Storage" keychain
            // prompt would hold every HTTP request until someone answers it.
            .arg("--use-mock-keychain")
            .args(fork_test_args::<&str>(&[]))
            .args(test_host_rules())
            .args(test_address_spaces())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if test_browser_without_hooks() {
            if let Some(name) = crate::testenv::HOOKS_ENV {
                cmd.env_remove(name);
            }
        }
        // SAFETY: the closure only makes async-signal-safe system calls.
        unsafe { cmd.pre_exec(pipes.child_setup()) };
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting {}", exe.display()))?;
        let (read, write) = pipes.into_parent_ends();
        let pid = child
            .id()
            .ok_or_else(|| anyhow!("the browser exited immediately"))?;
        let setup = async {
            let link = BrowserLink::pipe(read, write)?;
            let parts = fork_parts(paths, &link, false).await?;
            anyhow::Ok((link, parts))
        };
        let (link, parts) = match setup.await {
            Ok(v) => v,
            Err(e) => {
                let _ = child.kill().await;
                return Err(e);
            }
        };
        Ok(Self::fork(pid, link, parts, Some(child)))
    }

    /// The fork that launched the daemon (its parent), on the pipe it handed over. The
    /// agent profiles' pages left from before (an earlier daemon, a restored session) are
    /// closed first (F11). Starts without any window open.
    pub async fn hosted(paths: &Paths, link: BrowserLink) -> Result<Browser> {
        ensure(paths)?;
        let parts = fork_parts(paths, &link, true).await?;
        Ok(Self::fork(
            std::os::unix::process::parent_id(),
            link,
            parts,
            None,
        ))
    }

    fn fork(pid: u32, link: BrowserLink, parts: ForkParts, child: Option<Child>) -> Browser {
        Browser {
            pid,
            ws_url: None,
            link,
            conn: parts.conn,
            agents: parts.agents,
            child: child.map(Mutex::new),
            mode: Mode::Fork,
            profiles: Mutex::new(HashMap::new()),
            downloads: parts.downloads,
            profile_watch: Some(parts.profile_watch),
        }
    }

    /// Whether the browser launched the daemon (and hands it its DevTools pipe).
    pub fn is_hosted(&self) -> bool {
        self.child.is_none()
    }

    /// Whether this is the Pegboard fork (launched or hosting).
    pub fn is_fork(&self) -> bool {
        matches!(self.mode, Mode::Fork)
    }

    /// Opens (or focuses) a window of the agent profile `profile_dir` and returns that
    /// profile's browserContextId, which it records in `agents` (dropping the profile's old
    /// one when the browser unloaded it). Stock Chrome: by invoking Chrome again; the fork:
    /// by its domain.
    pub async fn open_profile_window(&self, profile_dir: &str) -> Result<String> {
        match &self.mode {
            Mode::Stock {
                chrome,
                user_data_dir,
                launch_context,
            } => {
                self.open_profile_window_stock(chrome, user_data_dir, launch_context, profile_dir)
                    .await
            }
            Mode::Fork => self.open_profile_window_by_domain(profile_dir).await,
        }
    }

    /// Makes the browser a responder of `broker` while the link lives (the fork's domain:
    /// `domain::serve_confirmations`). Stock Chrome has no way to ask a person: `None`.
    pub fn serve_confirmations(&self, broker: &Arc<ConfirmBroker>) -> Option<JoinHandle<()>> {
        match self.mode {
            Mode::Stock { .. } => None,
            Mode::Fork => Some(domain::serve_confirmations(
                broker,
                self.conn.clone(),
                self.link.clone(),
            )),
        }
    }

    /// The tab bridge of the agent profile whose context is `context`: the bridge extension
    /// on stock Chrome, the `Pegboard` domain on the fork.
    pub async fn tab_bridge(&self, context: &str) -> Result<Arc<dyn TabBridge>> {
        Ok(match self.mode {
            Mode::Stock { .. } => Arc::new(ExtensionBridge::attach(&self.conn, context).await?),
            Mode::Fork => Arc::new(DomainBridge::new(self.conn.clone(), context)),
        })
    }

    async fn open_profile_window_stock(
        &self,
        chrome: &Path,
        user_data_dir: &Path,
        launch_context: &str,
        profile_dir: &str,
    ) -> Result<String> {
        if !is_agent_profile_dir(profile_dir) {
            bail!("{profile_dir} is not an agent profile");
        }
        let known = self.profiles.lock().unwrap().get(profile_dir).cloned();
        if let Some(ctx) = known {
            let live = page_targets(&self.conn).await?;
            if live
                .iter()
                .any(|t| t.browser_context_id.as_deref() == Some(&ctx))
            {
                // Normally in place already; set when an earlier attempt failed.
                self.agents.set(profile_dir, &ctx);
                self.downloads.apply(&ctx, profile_dir).await?;
                return Ok(ctx);
            }
            // The context is gone (Chrome unloaded the profile): its policy went with it,
            // and it is no agent's any more.
            self.agents.forget_context(&ctx);
            self.downloads.forget_context(&ctx);
        }
        let before: Vec<String> = page_targets(&self.conn)
            .await?
            .into_iter()
            .map(|t| t.target_id)
            .collect();
        // Invoking the binary again with the same user data dir hands the request to the
        // running browser and exits.
        tokio::time::timeout(
            Duration::from_secs(20),
            Command::new(chrome)
                .arg(format!("--user-data-dir={}", user_data_dir.display()))
                .arg(format!("--profile-directory={profile_dir}"))
                .arg("--no-first-run")
                .arg("--new-window")
                .arg("about:blank")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await
        .map_err(|_| anyhow!("opening profile window timed out"))?
        .context("invoking chrome for profile window")?;
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let now = page_targets(&self.conn).await?;
            // Accept only a context that is neither the launch profile's nor another known
            // profile's, so a tab the person opens meanwhile cannot be mistaken for this one.
            let taken: Vec<String> = {
                let map = self.profiles.lock().unwrap();
                map.iter()
                    .filter(|(k, _)| k.as_str() != profile_dir)
                    .map(|(_, v)| v.clone())
                    .collect()
            };
            let fresh = now.iter().find(|t| {
                !before.contains(&t.target_id)
                    && t.browser_context_id
                        .as_ref()
                        .is_some_and(|c| !c.is_empty() && c != launch_context && !taken.contains(c))
            });
            if let Some(t) = fresh {
                let ctx = t.browser_context_id.clone().unwrap();
                self.adopt(profile_dir, &ctx).await?;
                return Ok(ctx);
            }
        }
        bail!("no window appeared for profile {profile_dir}")
    }
    /// `open_profile_window` on the Pegboard fork: one `Pegboard.openProfileWindow` on the
    /// control connection, which loads the profile or finds it loaded. Its context is
    /// recorded as in stock mode (a new context replaces the profile's old one).
    pub async fn open_profile_window_by_domain(&self, profile_dir: &str) -> Result<String> {
        if !is_agent_profile_dir(profile_dir) {
            bail!("{profile_dir} is not an agent profile");
        }
        let ctx = crate::bridge::domain::open_profile_window(&self.conn, profile_dir).await?;
        let known = self.profiles.lock().unwrap().get(profile_dir).cloned();
        match known {
            Some(old) if old == ctx => {
                self.agents.set(profile_dir, &ctx);
                self.downloads.apply(&ctx, profile_dir).await?;
            }
            old => {
                if let Some(old) = old {
                    self.agents.forget_context(&old);
                    self.downloads.forget_context(&old);
                }
                self.adopt(profile_dir, &ctx).await?;
            }
        }
        Ok(ctx)
    }

    /// `ctx` is the agent profile `profile_dir`'s new context: recorded, and given the
    /// profile's download policy.
    async fn adopt(&self, profile_dir: &str, ctx: &str) -> Result<()> {
        self.profiles
            .lock()
            .unwrap()
            .insert(profile_dir.to_string(), ctx.to_string());
        // Before the policy: `apply` takes only an agent profile's context.
        self.agents.set(profile_dir, ctx);
        self.downloads.forget_context(ctx);
        self.downloads.apply(ctx, profile_dir).await
    }

    /// The browser's remote debugging port.
    pub fn debugging_port(&self) -> Option<u16> {
        url::Url::parse(self.ws_url.as_deref()?).ok()?.port()
    }

    /// Whether the browser has ended: its process, when the daemon launched it; the
    /// connection, when the browser hosts the daemon.
    pub fn has_exited(&self) -> bool {
        match &self.child {
            Some(child) => matches!(child.lock().unwrap().try_wait(), Ok(Some(_)) | Err(_)),
            None => self.link.is_closed(),
        }
    }

    /// Closes a browser the daemon launched: `Browser.close`, then SIGKILL after 5 s. Does
    /// nothing when the browser has ended, or when it hosts the daemon (it is not the
    /// daemon's to close: `quit`).
    pub async fn close(&self) {
        let Some(child) = &self.child else {
            return;
        };
        if self.has_exited() {
            return;
        }
        let _ = self.conn.send("Browser.close", json!({}), None).await;
        for _ in 0..50 {
            if self.has_exited() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = child.lock().unwrap().start_kill();
        for _ in 0..20 {
            if self.has_exited() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Ends the browser whoever launched it (`browser.quit`): `close` for a browser the
    /// daemon launched; a hosting browser is asked with `Browser.close` on the control
    /// connection, and waited for until its connection ends (at most `QUIT_WAIT`).
    pub async fn quit(&self) {
        if self.child.is_some() {
            return self.close().await;
        }
        if self.link.is_closed() {
            return;
        }
        let _ = self.conn.send("Browser.close", json!({}), None).await;
        let _ = tokio::time::timeout(QUIT_WAIT, self.link.closed()).await;
    }

    /// `close`, for callers that own the browser.
    pub async fn shutdown(self) -> Result<()> {
        self.close().await;
        Ok(())
    }
}

/// How long `quit` waits for a hosting browser to go.
const QUIT_WAIT: Duration = Duration::from_secs(30);

/// The fork's start on the control connection: the `Pegboard` domain is taken
/// (`Pegboard.claim`; a domain another session holds fails the start), the agent contexts
/// come from `Pegboard.getProfiles` and follow `profileLoaded`/`profileUnloaded` from then
/// on. With `close_leftovers`, every page of an agent profile is closed (F11). No extension,
/// and no window needs to be open.
async fn fork_parts(paths: &Paths, link: &BrowserLink, close_leftovers: bool) -> Result<ForkParts> {
    let conn = CdpConn::over(link).await?;
    // Before the claim: no profile event is missed between the snapshot and the watch.
    let events = conn.events();
    domain::claim(&conn)
        .await
        .context("Pegboard.claim failed: another daemon may hold the browser's Pegboard domain")?;
    let agents = AgentContexts::default();
    let profiles = domain::get_profiles(&conn)
        .await
        .context("Pegboard.getProfiles")?;
    take_profiles(&agents, &profiles);
    let profile_watch = tokio::spawn(follow_profiles(events, conn.clone(), agents.clone()));
    if close_leftovers {
        close_agent_pages(&conn, &agents).await?;
    }
    let downloads = Downloads::new(link, agents.clone(), conn.clone(), paths)?;
    Ok(ForkParts {
        conn,
        agents,
        downloads,
        profile_watch,
    })
}

/// The agent contexts are exactly the agent profiles of `profiles` (a `Pegboard.getProfiles`
/// answer): whether one is an agent's is decided by its directory (`AgentContexts::set`).
fn take_profiles(agents: &AgentContexts, profiles: &[domain::DomainProfile]) {
    let now: Vec<&str> = profiles
        .iter()
        .map(|p| p.browser_context_id.as_str())
        .collect();
    for gone in agents.contexts() {
        if !now.contains(&gone.as_str()) {
            agents.forget_context(&gone);
        }
    }
    for p in profiles {
        agents.set(&p.profile_directory, &p.browser_context_id);
    }
}

/// Keeps `agents` in step with the domain's profile events. Events that were missed (the
/// queue overflowed) are made up for by reading `Pegboard.getProfiles` again.
async fn follow_profiles(
    mut events: broadcast::Receiver<CdpEvent>,
    conn: CdpConn,
    agents: AgentContexts,
) {
    loop {
        match events.recv().await {
            Ok(e) => match domain::profile_event(&e) {
                Some(ProfileEvent::Loaded {
                    profile_directory,
                    browser_context_id,
                }) => agents.set(&profile_directory, &browser_context_id),
                Some(ProfileEvent::Unloaded {
                    profile_directory,
                    browser_context_id,
                }) if agents.profile_of(&browser_context_id).as_deref()
                    == Some(profile_directory.as_str()) =>
                {
                    agents.forget_context(&browser_context_id)
                }
                _ => {}
            },
            Err(broadcast::error::RecvError::Lagged(_)) => {
                match domain::get_profiles(&conn).await {
                    Ok(profiles) => take_profiles(&agents, &profiles),
                    Err(e) => eprintln!("pegboard: reading the browser's profiles again: {e:#}"),
                }
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Closes every page of an agent profile (`Target.closeTarget`): what an earlier daemon or a
/// restored session left. A page that went meanwhile is no failure.
async fn close_agent_pages(conn: &CdpConn, agents: &AgentContexts) -> Result<()> {
    let pages = page_targets(conn)
        .await
        .context("listing the pages left from before")?;
    let mut closed = 0;
    for t in pages {
        if !t
            .browser_context_id
            .as_deref()
            .is_some_and(|c| agents.is_agent(c))
        {
            continue;
        }
        match conn
            .send(
                "Target.closeTarget",
                json!({ "targetId": t.target_id }),
                None,
            )
            .await
        {
            Ok(_) => closed += 1,
            Err(e) => eprintln!("pegboard: closing a page left from before: {e:#}"),
        }
    }
    if closed > 0 {
        eprintln!("pegboard: closed {closed} agent page(s) left from before");
    }
    Ok(())
}

async fn wait_for_ws_url(port_file: &std::path::Path, child: &mut Child) -> Result<String> {
    for _ in 0..150 {
        if let Ok(text) = std::fs::read_to_string(port_file) {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                return Ok(format!("ws://127.0.0.1:{}{}", port.trim(), path.trim()));
            }
        }
        if let Some(status) = child.try_wait()? {
            bail!("chrome exited during startup: {status}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("DevToolsActivePort was not written within 15s")
}

/// The first page target present after launch belongs to the launch profile (a person's).
async fn wait_for_first_context(conn: &CdpConn) -> Result<String> {
    for _ in 0..100 {
        let pages = page_targets(conn).await?;
        if let Some(ctx) = pages.iter().find_map(|t| t.browser_context_id.clone()) {
            return Ok(ctx);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("no page target appeared after launch")
}

/// Test-only (with `PEGBOARD_TEST_HOOKS`, as every test variable): `PEGBOARD_TEST_HOST_RULES` is passed to the browser as `--host-resolver-rules`
/// (and read by the guard's resolver), so tests can give names private addresses.
/// `PEGBOARD_TEST_BROWSER_HOST_RULES` is passed to the browser only, so a test can make the
/// browser and the daemon disagree (the daemon resolving a name to a public address while the
/// browser reaches a local test server under it).
fn test_host_rules() -> Vec<String> {
    // The browser-only rules go first: Chrome applies the first rule that matches a name.
    let rules: Vec<String> = [
        "PEGBOARD_TEST_BROWSER_HOST_RULES",
        "PEGBOARD_TEST_HOST_RULES",
    ]
    .iter()
    .filter_map(|name| crate::testenv::var(name))
    .filter(|r| !r.is_empty())
    .collect();
    if rules.is_empty() {
        Vec::new()
    } else {
        vec![format!("--host-resolver-rules={}", rules.join(", "))]
    }
}

/// Test-only: `PEGBOARD_TEST_IP_SPACE_OVERRIDES` is passed as `--ip-address-space-overrides`
/// (e.g. `127.0.0.1:<port>=public`), so a local test server can play a server on a public
/// address for Chrome's local-network checks (spike round 2, N2).
fn test_address_spaces() -> Vec<String> {
    match crate::testenv::var("PEGBOARD_TEST_IP_SPACE_OVERRIDES") {
        Some(v) if !v.is_empty() => vec![format!("--ip-address-space-overrides={v}")],
        _ => Vec::new(),
    }
}

/// Test-only counter: when `PEGBOARD_TEST_LAUNCH_LOG` names a file, every launch appends one line to it.
fn record_launch() {
    if let Some(path) = crate::testenv::var("PEGBOARD_TEST_LAUNCH_LOG") {
        append_launch_line(std::path::Path::new(&path));
    }
}

fn append_launch_line(path: &std::path::Path) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "launch pid={}", std::process::id());
    }
}

#[cfg(test)]
mod launch_log_tests {
    use super::append_launch_line;

    #[test]
    fn each_launch_appends_one_line() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("launches.log");
        append_launch_line(&log);
        append_launch_line(&log);
        let text = std::fs::read_to_string(&log).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.lines().all(|l| l.starts_with("launch pid=")));
    }
}

#[cfg(test)]
mod fork_tests {
    use super::*;

    use serde_json::{json, Value};

    use crate::link::fake::FakeBrowser;

    /// The next request, which must be `method` on the control connection's own session.
    async fn expect(fake: &mut FakeBrowser, method: &str) -> Value {
        let m = fake.next().await;
        assert_eq!(m["method"], method, "{m}");
        assert_eq!(m["sessionId"], "B1", "{m}");
        m
    }

    fn page(id: &str, context: &str) -> Value {
        json!({ "targetId": id, "type": "page", "url": "about:blank",
                "browserContextId": context })
    }

    async fn eventually(what: &str, check: impl Fn() -> bool) {
        for _ in 0..100 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("{what} did not happen");
    }

    #[tokio::test]
    async fn hosted_start_claims_reads_profiles_and_closes_leftover_agent_pages() {
        let home = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(home.path());
        let (link, mut fake) = FakeBrowser::start();
        let start = tokio::spawn(async move { Browser::hosted(&paths, link).await });
        let m = expect(&mut fake, "Pegboard.claim").await;
        fake.answer(&m, json!({}));
        let m = expect(&mut fake, "Pegboard.getProfiles").await;
        fake.answer(
            &m,
            json!({ "profiles": [
                { "profileDirectory": "Default", "browserContextId": "H0", "isAgent": false },
                { "profileDirectory": "Profile 1", "browserContextId": "H1", "isAgent": false },
                { "profileDirectory": "PegboardAgent", "browserContextId": "A1", "isAgent": true },
                // The browser's word is not taken: the directory decides.
                { "profileDirectory": "Sneaky", "browserContextId": "H2", "isAgent": true },
            ] }),
        );
        let m = expect(&mut fake, "Target.getTargets").await;
        fake.answer(
            &m,
            json!({ "targetInfos": [
                page("T-H0", "H0"), page("T-A1", "A1"), page("T-H1", "H1"),
                page("T-A2", "A1"), page("T-H2", "H2"), page("T-X", ""),
                { "targetId": "W-A1", "type": "service_worker", "browserContextId": "A1" },
            ] }),
        );
        // Only the agent profile's pages are closed.
        for id in ["T-A1", "T-A2"] {
            let m = expect(&mut fake, "Target.closeTarget").await;
            assert_eq!(m["params"], json!({ "targetId": id }), "{m}");
            fake.answer(&m, json!({ "success": true }));
        }
        let b = start.await.unwrap().unwrap();
        fake.quiet().await;
        assert!(b.is_fork());
        assert_eq!(b.ws_url, None);
        assert_eq!(b.debugging_port(), None);
        assert!(b.agents.is_agent("A1"));
        for human in ["H0", "H1", "H2", ""] {
            assert!(!b.agents.is_agent(human), "{human}");
        }
        // The control connection's own events come without a session id (the link takes
        // the browser session's off): the profile events reach the table.
        fake.send(
            json!({ "method": "Pegboard.profileUnloaded", "sessionId": "B1",
            "params": { "profileDirectory": "PegboardAgent", "browserContextId": "A1" } }),
        );
        fake.send(
            json!({ "method": "Pegboard.profileLoaded", "sessionId": "B1",
            "params": { "profileDirectory": "PegboardAgent-w", "browserContextId": "A2" } }),
        );
        fake.send(
            json!({ "method": "Pegboard.profileLoaded", "sessionId": "B1",
            "params": { "profileDirectory": "Profile 2", "browserContextId": "H3" } }),
        );
        eventually("the profile events", || {
            !b.agents.is_agent("A1") && b.agents.is_agent("A2")
        })
        .await;
        assert!(!b.agents.is_agent("H3"));
        // An event of another session (a tab's) is not the domain's.
        fake.send(
            json!({ "method": "Pegboard.profileLoaded", "sessionId": "S-TAB",
            "params": { "profileDirectory": "PegboardAgent", "browserContextId": "A9" } }),
        );
        // A reloaded profile: its new context, and the old one is no agent's any more.
        fake.send(
            json!({ "method": "Pegboard.profileLoaded", "sessionId": "B1",
            "params": { "profileDirectory": "PegboardAgent-w", "browserContextId": "A3" } }),
        );
        eventually("the reload", || b.agents.is_agent("A3")).await;
        assert!(!b.agents.is_agent("A2") && !b.agents.is_agent("A9"));
        // The hosting browser is not the daemon's: it has ended when its connection has.
        assert!(!b.has_exited());
        fake.disconnect();
        eventually("the end of the link", || b.has_exited()).await;
        b.close().await;
    }

    #[tokio::test]
    async fn hosted_start_fails_when_the_domain_is_claimed() {
        let home = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(home.path());
        let (link, mut fake) = FakeBrowser::start();
        let start = tokio::spawn(async move { Browser::hosted(&paths, link).await });
        let m = expect(&mut fake, "Pegboard.claim").await;
        fake.send(json!({ "id": m["id"], "sessionId": "B1",
            "error": { "code": -32000, "message": "already claimed" } }));
        let e = format!("{:#}", start.await.unwrap().err().expect("must fail"));
        assert!(
            e.contains("Pegboard.claim") && e.contains("already claimed"),
            "{e}"
        );
        // Nothing else was asked of the browser.
        fake.quiet().await;
    }

    #[tokio::test]
    async fn quit_asks_a_hosting_browser_to_close() {
        let home = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(home.path());
        let (link, mut fake) = FakeBrowser::start();
        let start = tokio::spawn(async move { Browser::hosted(&paths, link).await });
        let m = expect(&mut fake, "Pegboard.claim").await;
        fake.answer(&m, json!({}));
        let m = expect(&mut fake, "Pegboard.getProfiles").await;
        fake.answer(&m, json!({ "profiles": [] }));
        let m = expect(&mut fake, "Target.getTargets").await;
        fake.answer(&m, json!({ "targetInfos": [] }));
        let b = start.await.unwrap().unwrap();
        // `close` leaves it be: it is not the daemon's.
        b.close().await;
        fake.quiet().await;
        let b = Arc::new(b);
        let quit = {
            let b = b.clone();
            tokio::spawn(async move { b.quit().await })
        };
        let m = expect(&mut fake, "Browser.close").await;
        fake.answer(&m, json!({}));
        fake.disconnect();
        tokio::time::timeout(Duration::from_secs(3), quit)
            .await
            .expect("quit returns when the connection ends")
            .unwrap();
    }

    /// Stock Chrome has no `Pegboard` domain: the browser is never a responder, so a
    /// question there has only API subscribers (and with none is `no_responder`).
    #[tokio::test]
    async fn stock_has_no_browser_responder() {
        let home = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(home.path());
        let (link, mut fake) = FakeBrowser::start();
        let conn = CdpConn::over(&link).await.unwrap();
        let agents = AgentContexts::default();
        let downloads = Downloads::new(&link, agents.clone(), conn.clone(), &paths).unwrap();
        let stock = Browser {
            pid: 0,
            ws_url: None,
            link,
            conn,
            agents,
            child: None,
            mode: Mode::Stock {
                chrome: PathBuf::from("/nonexistent/chrome"),
                user_data_dir: paths.chrome_udd.clone(),
                launch_context: "H0".into(),
            },
            profiles: Mutex::new(HashMap::new()),
            downloads,
            profile_watch: None,
        };
        let broker = crate::confirm::ConfirmBroker::new(Duration::from_secs(20));
        assert!(stock.serve_confirmations(&broker).is_none());
        assert!(!broker.has_responder(true));
        fake.quiet().await;
    }

    /// The fork's domain is: from the start until its link ends.
    #[tokio::test]
    async fn the_fork_serves_confirmations_while_its_link_lives() {
        let home = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(home.path());
        let (link, mut fake) = FakeBrowser::start();
        let start = tokio::spawn(async move { Browser::hosted(&paths, link).await });
        let m = expect(&mut fake, "Pegboard.claim").await;
        fake.answer(&m, json!({}));
        let m = expect(&mut fake, "Pegboard.getProfiles").await;
        fake.answer(&m, json!({ "profiles": [] }));
        let m = expect(&mut fake, "Target.getTargets").await;
        fake.answer(&m, json!({ "targetInfos": [] }));
        let b = start.await.unwrap().unwrap();
        let broker = crate::confirm::ConfirmBroker::new(Duration::from_secs(20));
        let serving = b.serve_confirmations(&broker).expect("the fork answers");
        assert!(broker.has_responder(true));
        assert!(!broker.has_responder(false));
        fake.disconnect();
        tokio::time::timeout(Duration::from_secs(3), serving)
            .await
            .unwrap()
            .unwrap();
        assert!(!broker.has_responder(true));
    }

    #[test]
    fn the_fork_binary_is_found_in_its_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Pegboard.app");
        std::fs::create_dir(&app).unwrap();
        assert_eq!(fork_executable(&app), app.join("Contents/MacOS/Pegboard"));
        let exe = dir.path().join("Pegboard");
        std::fs::write(&exe, "").unwrap();
        assert_eq!(fork_executable(&exe), exe);
    }

    /// A child gets the pipes at fds 3 (it reads) and 4 (it writes), and nothing else of
    /// them: the parent sees the end of the pipe when the child goes.
    #[test]
    fn a_child_reads_fd_3_and_writes_fd_4() {
        use std::io::{Read, Write};
        use std::os::unix::process::CommandExt;

        let pipes = DevToolsPipes::new().unwrap();
        let mut cmd = std::process::Command::new("/bin/sh");
        // Echoes what it reads on fd 3 to fd 4.
        cmd.args(["-c", "cat <&3 >&4"]);
        // SAFETY: the closure only makes async-signal-safe system calls.
        unsafe { cmd.pre_exec(pipes.child_setup()) };
        let mut child = cmd.spawn().unwrap();
        let (read, write) = pipes.into_parent_ends();
        let mut to_child = std::fs::File::from(write);
        to_child.write_all(b"{\"id\":1}\0").unwrap();
        drop(to_child);
        let mut got = Vec::new();
        std::fs::File::from(read).read_to_end(&mut got).unwrap();
        assert_eq!(got, b"{\"id\":1}\0");
        assert!(child.wait().unwrap().success());
    }
}

#[cfg(test)]
mod code_sign_clone_tests {
    use super::fork_test_args;
    use std::path::Path;

    #[test]
    fn fork_test_args_merge_into_one_disable_features() {
        assert_eq!(
            fork_test_args::<&str>(&[]),
            vec!["--disable-features=MacAppCodeSignClone"]
        );
        assert_eq!(
            fork_test_args(&[
                "--load-extension=x",
                "--disable-features=A,B",
                "--no-startup-window",
                "--disable-features=MacAppCodeSignClone,C",
            ]),
            vec![
                "--load-extension=x",
                "--no-startup-window",
                "--disable-features=MacAppCodeSignClone,A,B,C",
            ]
        );
    }

    /// Every launch of the fork in the tests (each passes the test-only mock keychain flag)
    /// goes with `fork_test_args`, so the code-sign clone is off; so does the manual-check
    /// script. A new launch without it fails here.
    #[test]
    fn every_fork_test_launch_turns_off_the_code_sign_clone() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo = manifest.join("../..");
        // Built here, so this test's own text does not count.
        let launch = format!("\"--use-{}\"", "mock-keychain");
        let calls = [
            format!("{}(", "fork_test_args"),
            format!("{}::<", "fork_test_args"),
        ];
        let mut files = vec![manifest.join("src/browser.rs")];
        for dir in ["crates/pegboard-core/tests", "crates/pegboard-cli/tests"] {
            for entry in std::fs::read_dir(repo.join(dir)).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_some_and(|e| e == "rs") {
                    files.push(path);
                }
            }
        }
        let mut launches = 0;
        for file in &files {
            let text = std::fs::read_to_string(file).unwrap();
            let code: Vec<&str> = text
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect();
            let n = code.iter().filter(|l| l.contains(&launch)).count();
            let with = code
                .iter()
                .filter(|l| calls.iter().any(|c| l.contains(c.as_str())))
                .count();
            assert!(
                with >= n,
                "{}: {n} fork launches, {with} with fork_test_args",
                file.display()
            );
            launches += n;
        }
        assert!(launches >= 7, "only {launches} fork launches found");
        let script =
            std::fs::read_to_string(repo.join("docs/plans/phase3b-notes/manual-3b.sh")).unwrap();
        let opens = script.matches("--use-mock-keychain").count();
        assert_eq!(
            script
                .matches("--disable-features=MacAppCodeSignClone")
                .count(),
            opens,
            "manual-3b.sh"
        );
    }
}
