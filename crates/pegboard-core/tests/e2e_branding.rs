//! Phase 3b task 6: the product name in the browser's own strings is Pegboard, in English and
//! in a translation (the generated `rebrand-strings (generated)` commit, which also rewrites
//! the translation IDs; if those were wrong the Korean page would fall back to English).
//!
//! Fork only: `PEGBOARD_TEST_HOOKS=1 PEGBOARD_TEST_BROWSER=fork:<Pegboard.app> cargo test -p
//! pegboard-core --test e2e_branding -- --ignored --test-threads=1`. Without a fork the tests
//! print a line and pass. The browser is launched with its DevTools on a pipe, no daemon, a
//! temporary profile and the mock keychain; the UI language comes from `-AppleLanguages`
//! (on macOS the browser takes its locale from Cocoa, not from `--lang`).
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use pegboard_core::browser::{fork_executable, test_fork_binary, DevToolsPipes};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::pipe;
use tokio::process::{Child, Command};
use tokio::time::Instant;

const LIMIT: Duration = Duration::from_secs(10);

/// The DevTools pipe of a launched fork (NUL-terminated JSON messages).
struct PipeLink {
    read: pipe::Receiver,
    write: pipe::Sender,
    buf: Vec<u8>,
    next_id: u64,
}

impl PipeLink {
    /// Launches the fork whose UI language is `lang` (a macOS language code).
    fn launch(binary: &Path, user_data_dir: &Path, lang: &str) -> (Child, PipeLink) {
        let pipes = DevToolsPipes::new().unwrap();
        let mut cmd = Command::new(fork_executable(binary));
        cmd.arg("-AppleLanguages")
            .arg(format!("({lang})"))
            .arg(format!("--user-data-dir={}", user_data_dir.display()))
            .arg("--remote-debugging-pipe")
            .arg("--pegboard-no-daemon")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--use-mock-keychain")
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        // SAFETY: the closure only makes async-signal-safe system calls.
        unsafe { cmd.pre_exec(pipes.child_setup()) };
        let child = cmd.spawn().unwrap();
        let (read, write) = pipes.into_parent_ends();
        let link = PipeLink {
            read: pipe::Receiver::from_owned_fd(read).unwrap(),
            write: pipe::Sender::from_owned_fd(write).unwrap(),
            buf: Vec::new(),
            next_id: 0,
        };
        (child, link)
    }

    /// Sends a command and returns its `result` (events are dropped). The first call may wait
    /// for the browser to start reading its pipe.
    async fn call(&mut self, session: Option<&str>, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let mut bytes = msg.to_string().into_bytes();
        bytes.push(0);
        self.write.write_all(&bytes).await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            while let Some(end) = self.buf.iter().position(|&b| b == 0) {
                let raw: Vec<u8> = self.buf.drain(..=end).collect();
                let v: Value = serde_json::from_slice(&raw[..end]).unwrap();
                if v["id"] == json!(id) {
                    return match v.get("result") {
                        Some(r) => r.clone(),
                        None => panic!("{method} failed: {v}"),
                    };
                }
            }
            self.buf.reserve(64 * 1024);
            match tokio::time::timeout_at(deadline, self.read.read_buf(&mut self.buf)).await {
                Err(_) => panic!("no answer to {method}"),
                Ok(Ok(0)) => panic!("pipe closed"),
                Ok(Err(e)) => panic!("pipe error: {e}"),
                Ok(Ok(_)) => {}
            }
        }
    }
}

/// All text on the page, shadow roots included (the settings pages are web components).
const DEEP_TEXT: &str = r#"(() => {
  const out = [];
  const walk = (n) => {
    if (n.nodeType === Node.TEXT_NODE) { out.push(n.textContent); return; }
    if (n.nodeName === 'STYLE' || n.nodeName === 'SCRIPT') return;
    if (n.shadowRoot) walk(n.shadowRoot);
    for (const c of n.childNodes) walk(c);
  };
  walk(document);
  return out.join(' ').replace(/\s+/g, ' ');
})()"#;

/// Opens `url` in a new tab of a fork running in `lang` and returns the page's text once it
/// contains `wait_for` (or whatever it has after the time limit).
async fn page_text(lang: &str, url: &str, wait_for: &str) -> Option<String> {
    let binary = test_fork_binary()?;
    let profile = tempfile::tempdir().unwrap();
    let (_child, mut link) = PipeLink::launch(&binary, profile.path(), lang);
    let target = link
        .call(None, "Target.createTarget", json!({ "url": url }))
        .await["targetId"]
        .as_str()
        .unwrap()
        .to_string();
    let session = link
        .call(
            None,
            "Target.attachToTarget",
            json!({ "targetId": target, "flatten": true }),
        )
        .await["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let deadline = Instant::now() + LIMIT;
    let mut text = String::new();
    while Instant::now() < deadline {
        let r = link
            .call(
                Some(&session),
                "Runtime.evaluate",
                json!({ "expression": DEEP_TEXT, "returnByValue": true }),
            )
            .await;
        text = r["result"]["value"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if text.contains(wait_for) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let _ = link.call(None, "Browser.close", json!({})).await;
    Some(text)
}

/// "Chromium" left on the page as the product name: everything except the copyright holder
/// and the Chromium project named in the open source notice.
fn product_chromium(text: &str) -> usize {
    text.replace("The Chromium Authors", "")
        .replace("Chromium open source project", "")
        .replace("Chromium 오픈소스 프로젝트", "")
        .matches("Chromium")
        .count()
}

fn head(text: &str) -> String {
    text.chars().take(400).collect()
}

fn skip() {
    println!("SKIPPED: e2e_branding needs PEGBOARD_TEST_BROWSER=fork:<Pegboard.app>");
}

#[tokio::test]
#[ignore]
async fn about_page_says_pegboard_in_english() {
    let Some(text) = page_text("en", "chrome://settings/help", "About Pegboard").await else {
        return skip();
    };
    println!("OBSERVED en: {}", head(&text));
    assert!(text.contains("About Pegboard"), "{text}");
    assert_eq!(product_chromium(&text), 0, "{text}");
}

#[tokio::test]
#[ignore]
async fn about_page_says_pegboard_in_korean() {
    let Some(text) = page_text("ko", "chrome://settings/help", "Pegboard 정보").await else {
        return skip();
    };
    println!("OBSERVED ko: {}", head(&text));
    assert!(text.contains("Pegboard 정보"), "{text}");
    assert!(
        !text.contains("About Pegboard"),
        "fell back to English: {text}"
    );
    assert_eq!(product_chromium(&text), 0, "{text}");
}
