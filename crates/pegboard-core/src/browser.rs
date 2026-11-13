//! Launch and supervise the headed Chrome that pegboard drives.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;
use tokio::process::{Child, Command};

use crate::cdp::{page_targets, CdpConn};
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
    /// Agent browser contexts that carry the download deny policy (see `deny_downloads`).
    denied: Mutex<HashSet<String>>,
    /// One long-lived connection per agent context that holds its deny policy (see
    /// `deny_downloads`). Kept for the life of the browser.
    deny_conns: Mutex<HashMap<String, CdpConn>>,
}

impl Browser {
    pub async fn launch(paths: &Paths, cfg: &BrowserConfig) -> Result<Browser> {
        ensure(paths)?;
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
            anyhow::Ok((conn, human_context))
        };
        let (conn, human_context) = match setup.await {
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
            denied: Mutex::new(HashSet::new()),
            deny_conns: Mutex::new(HashMap::new()),
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
                // Normally applied already; retried when an earlier attempt failed.
                self.deny_downloads(&ctx).await?;
                return Ok(ctx);
            }
            // The context is gone (Chrome unloaded the profile): its policy went with it.
            self.denied.lock().unwrap().remove(&ctx);
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
                self.denied.lock().unwrap().remove(&ctx);
                self.deny_downloads(&ctx).await?;
                return Ok(ctx);
            }
        }
        bail!("no window appeared for profile {profile_dir}")
    }

    /// Denies every download in the agent profile `context`: phase 1 gives agents no download
    /// folder, and without a policy Chrome saves into the macOS user's `~/Downloads` without
    /// asking. Measured on Chrome for Testing 157:
    /// - Chrome drops the policy when the connection that set it closes, so it is set on a
    ///   connection this browser keeps for its whole life. Other clients (the session
    ///   proxy's) coming and going, new tabs, popups and the human profile becoming the active
    ///   one do not affect it.
    /// - A connection holds the policy of one context only: setting it for a second context on
    ///   the same connection silently lifts it from the first. So each agent context gets its
    ///   own connection (`deny_conns`), not `self.conn`.
    /// - Chrome resolves `browserContextId` only for the profile that is the browser's default
    ///   context at that moment (the last active one) and answers "Failed to find browser
    ///   context" for any other, so a profile whose window was just opened is retried briefly.
    ///
    /// Never called for the human context, and never without a context id: without one the
    /// call would hit the default context, which can be the human's. An error leaves the
    /// profile unprotected, so the caller must not use it.
    async fn deny_downloads(&self, context: &str) -> Result<()> {
        if context.is_empty() || context == self.human_context {
            bail!("refusing to set a download policy on the human or an unknown context");
        }
        if self.denied.lock().unwrap().contains(context) {
            return Ok(());
        }
        let existing = self.deny_conns.lock().unwrap().get(context).cloned();
        let conn = match existing {
            Some(conn) => conn,
            None => {
                let conn = CdpConn::connect(&self.ws_url).await?;
                self.deny_conns
                    .lock()
                    .unwrap()
                    .insert(context.to_string(), conn.clone());
                conn
            }
        };
        let mut last = None;
        for _ in 0..20 {
            let sent = conn
                .send(
                    "Browser.setDownloadBehavior",
                    json!({ "behavior": "deny", "browserContextId": context }),
                    None,
                )
                .await;
            match sent {
                Ok(_) => {
                    self.denied.lock().unwrap().insert(context.to_string());
                    return Ok(());
                }
                Err(e) => last = Some(e),
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err(last
            .unwrap()
            .context("could not block downloads in the agent profile"))
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
