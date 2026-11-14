//! Launch and supervise the headed Chrome that pegboard drives.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;
use tokio::process::{Child, Command};

use crate::cdp::{page_targets, CdpConn};
use crate::downloads::Downloads;
use crate::paths::{ensure, Paths};

pub const HUMAN_PROFILE_DIR: &str = "Default";

#[derive(Debug, Clone)]
pub struct BrowserConfig {
    pub chrome: PathBuf,
    pub extension_dir: PathBuf,
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

pub struct Browser {
    pub pid: u32,
    pub ws_url: String,
    pub conn: CdpConn,
    pub human_context: String,
    child: Mutex<Child>,
    /// profile directory -> browserContextId, for profiles whose window we opened.
    profiles: Mutex<HashMap<String, String>>,
    /// The agent profiles' download policies and their downloads (`downloads`).
    pub downloads: Downloads,
}

impl Browser {
    pub async fn launch(paths: &Paths, cfg: &BrowserConfig) -> Result<Browser> {
        ensure(paths)?;
        record_launch();
        let port_file = paths.chrome_udd.join("DevToolsActivePort");
        let _ = std::fs::remove_file(&port_file);
        let mut child = Command::new(&cfg.chrome)
            .arg(format!("--user-data-dir={}", paths.chrome_udd.display()))
            .arg(format!("--profile-directory={HUMAN_PROFILE_DIR}"))
            .arg("--remote-debugging-port=0")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg(format!("--load-extension={}", cfg.extension_dir.display()))
            .arg("--disable-features=DisableLoadExtensionCommandLineSwitch")
            .args(test_host_rules())
            .args(test_address_spaces())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting {}", cfg.chrome.display()))?;
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
        let setup = async {
            let conn = CdpConn::connect(&ws_url).await?;
            let human_context = wait_for_first_context(&conn).await?;
            let downloads = Downloads::new(&ws_url, &human_context, conn.clone(), paths)?;
            anyhow::Ok((conn, human_context, downloads))
        };
        let (conn, human_context, downloads) = match setup.await {
            Ok(v) => v,
            Err(e) => {
                let _ = child.kill().await;
                return Err(e);
            }
        };
        Ok(Browser {
            pid,
            ws_url,
            conn,
            human_context,
            child: Mutex::new(child),
            profiles: Mutex::new(HashMap::from([(
                HUMAN_PROFILE_DIR.to_string(),
                String::new(),
            )])),
            downloads,
        })
    }

    /// Opens (or focuses) a window of `profile_dir` and returns that profile's browserContextId.
    pub async fn open_profile_window(
        &self,
        paths: &Paths,
        cfg: &BrowserConfig,
        profile_dir: &str,
    ) -> Result<String> {
        if profile_dir == HUMAN_PROFILE_DIR {
            return Ok(self.human_context.clone());
        }
        let known = self.profiles.lock().unwrap().get(profile_dir).cloned();
        if let Some(ctx) = known {
            let live = page_targets(&self.conn).await?;
            if live
                .iter()
                .any(|t| t.browser_context_id.as_deref() == Some(&ctx))
            {
                // Normally in place already; set when an earlier attempt failed.
                self.downloads.apply(&ctx, profile_dir).await?;
                return Ok(ctx);
            }
            // The context is gone (Chrome unloaded the profile): its policy went with it.
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
            Command::new(&cfg.chrome)
                .arg(format!("--user-data-dir={}", paths.chrome_udd.display()))
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
            // Accept only a context that is neither the human profile nor another known
            // profile, so a tab the human opens meanwhile cannot be mistaken for this one.
            let taken: Vec<String> = {
                let map = self.profiles.lock().unwrap();
                map.iter()
                    .filter(|(k, _)| k.as_str() != profile_dir)
                    .map(|(_, v)| v.clone())
                    .collect()
            };
            let fresh = now.iter().find(|t| {
                !before.contains(&t.target_id)
                    && t.browser_context_id.as_ref().is_some_and(|c| {
                        !c.is_empty() && *c != self.human_context && !taken.contains(c)
                    })
            });
            if let Some(t) = fresh {
                let ctx = t.browser_context_id.clone().unwrap();
                self.profiles
                    .lock()
                    .unwrap()
                    .insert(profile_dir.to_string(), ctx.clone());
                self.downloads.forget_context(&ctx);
                self.downloads.apply(&ctx, profile_dir).await?;
                return Ok(ctx);
            }
        }
        bail!("no window appeared for profile {profile_dir}")
    }

    /// The browser's remote debugging port.
    pub fn debugging_port(&self) -> Option<u16> {
        url::Url::parse(&self.ws_url).ok()?.port()
    }

    /// Whether the browser process has ended.
    pub fn has_exited(&self) -> bool {
        matches!(self.child.lock().unwrap().try_wait(), Ok(Some(_)) | Err(_))
    }

    /// `Browser.close`, then SIGKILL after 5 s. Does nothing when the browser has ended.
    pub async fn close(&self) {
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
        let _ = self.child.lock().unwrap().start_kill();
        for _ in 0..20 {
            if self.has_exited() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// `close`, for callers that own the browser.
    pub async fn shutdown(self) -> Result<()> {
        self.close().await;
        Ok(())
    }
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

/// The first page target present after launch belongs to the human profile.
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

/// Test-only: `PEGBOARD_TEST_HOST_RULES` is passed to the browser as `--host-resolver-rules`
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
    .filter_map(|name| std::env::var(name).ok())
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
    match std::env::var("PEGBOARD_TEST_IP_SPACE_OVERRIDES") {
        Ok(v) if !v.is_empty() => vec![format!("--ip-address-space-overrides={v}")],
        _ => Vec::new(),
    }
}

/// Test-only counter: when `PEGBOARD_TEST_LAUNCH_LOG` names a file, every launch appends one line to it.
fn record_launch() {
    if let Some(path) = std::env::var_os("PEGBOARD_TEST_LAUNCH_LOG") {
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
