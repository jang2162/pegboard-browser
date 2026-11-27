//! Phase 3b task 6: the product name in the browser's own strings is Pegboard, in English and
//! in a translation (the generated `rebrand-strings (generated)` commit, which also rewrites
//! the translation IDs; if those were wrong the Korean page would fall back to English).
//! Task 9: sign-in to the browser and the built-in password manager are off by default
//! (`//pegboard/browser_parts/profile_defaults.h`), in every profile, from the first window.
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
            .args(pegboard_core::browser::fork_test_args::<&str>(&[]))
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

impl PipeLink {
    /// A flat session on the page `target`.
    async fn attach(&mut self, target: &str) -> String {
        self.call(
            None,
            "Target.attachToTarget",
            json!({ "targetId": target, "flatten": true }),
        )
        .await["sessionId"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Navigates `session`'s page to `url` and returns what `script` gives there once it is
    /// not null (within the time limit).
    async fn eval_at(&mut self, session: &str, url: &str, script: &str) -> Value {
        self.call(Some(session), "Page.navigate", json!({ "url": url }))
            .await;
        let deadline = Instant::now() + LIMIT;
        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let r = self
                .call(
                    Some(session),
                    "Runtime.evaluate",
                    json!({ "expression": script, "returnByValue": true }),
                )
                .await;
            let v = r["result"]["value"].clone();
            if !v.is_null() {
                return v;
            }
        }
        panic!("nothing from {url}");
    }

    /// The value of the preference `name` of `session`'s profile
    /// (`chrome://prefs-internals/<name>`).
    async fn pref(&mut self, session: &str, name: &str) -> Value {
        let text = self
            .eval_at(
                session,
                &format!("chrome://prefs-internals/{name}"),
                "document.body && document.body.innerText.trim() !== '' ? document.body.innerText : null",
            )
            .await;
        let text = text.as_str().unwrap();
        serde_json::from_str(text).unwrap_or_else(|_| panic!("{name}: {text}"))
    }

    /// The first page target there is (the startup window's tab), within the time limit.
    async fn first_page(&mut self) -> String {
        let deadline = Instant::now() + LIMIT;
        while Instant::now() < deadline {
            let r = self.call(None, "Target.getTargets", json!({})).await;
            if let Some(t) = r["targetInfos"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["type"] == "page")
            {
                return t["targetId"].as_str().unwrap().to_string();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("no page");
    }
}

/// The preferences pegboard turns off by default.
const OFF_BY_DEFAULT: [&str; 5] = [
    "signin.allowed",
    "signin.allowed_on_next_startup",
    "credentials_enable_service",
    "credentials_enable_autosignin",
    "credentials_enable_passkeys",
];

/// The values of `OFF_BY_DEFAULT` in `session`'s profile.
async fn off_values(link: &mut PipeLink, session: &str) -> Vec<(String, Value)> {
    let mut values = Vec::new();
    for name in OFF_BY_DEFAULT {
        values.push((name.to_string(), link.pref(session, name).await));
    }
    values
}

fn all_false(values: &[(String, Value)]) -> bool {
    values.iter().all(|(_, v)| v == &json!(false))
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

/// B13: in a person's profile and in an agent profile, sign-in to the browser, password
/// saving, automatic sign-in and passkey saving are off.
#[tokio::test]
#[ignore]
async fn signin_and_password_saving_are_off_by_default() {
    let Some(binary) = test_fork_binary() else {
        return skip();
    };
    let profile = tempfile::tempdir().unwrap();
    let (_child, mut link) = PipeLink::launch(&binary, profile.path(), "en");
    // A new tab of the person's Default profile.
    let tab = link
        .call(None, "Target.createTarget", json!({ "url": "about:blank" }))
        .await["targetId"]
        .as_str()
        .unwrap()
        .to_string();
    let s = link.attach(&tab).await;
    let person = off_values(&mut link, &s).await;
    // An agent profile, made by the domain as the daemon does.
    link.call(None, "Pegboard.claim", json!({})).await;
    let ctx = link
        .call(
            None,
            "Pegboard.openProfileWindow",
            json!({ "profileDirectory": "PegboardAgent" }),
        )
        .await["browserContextId"]
        .as_str()
        .unwrap()
        .to_string();
    let agent_tab = link
        .call(
            None,
            "Pegboard.createGroup",
            json!({ "browserContextId": ctx, "title": "t", "color": "blue" }),
        )
        .await["targetId"]
        .as_str()
        .unwrap()
        .to_string();
    let s = link.attach(&agent_tab).await;
    let agent = off_values(&mut link, &s).await;
    let _ = link.call(None, "Browser.close", json!({})).await;
    println!("OBSERVED Default {person:?}");
    println!("OBSERVED PegboardAgent {agent:?}");
    assert!(all_false(&person), "{person:?}");
    assert!(all_false(&agent), "{agent:?}");
}

/// The first window, right after the browser starts, already sees the defaults (they are
/// set when the profile's preferences are registered, before its services read them).
#[tokio::test]
#[ignore]
async fn the_first_window_sees_the_defaults() {
    let Some(binary) = test_fork_binary() else {
        return skip();
    };
    let profile = tempfile::tempdir().unwrap();
    let (_child, mut link) = PipeLink::launch(&binary, profile.path(), "en");
    let first = link.first_page().await;
    let s = link.attach(&first).await;
    let values = off_values(&mut link, &s).await;
    let _ = link.call(None, "Browser.close", json!({})).await;
    println!("OBSERVED first window {values:?}");
    assert!(all_false(&values), "{values:?}");
}

/// The people page offers no sign-in (its profile and Google account rows are those of a
/// browser where sign-in is not allowed), and the browser is not managed.
#[tokio::test]
#[ignore]
async fn the_people_page_offers_no_sign_in() {
    let Some(binary) = test_fork_binary() else {
        return skip();
    };
    const PEOPLE: &str = r#"(() => {
      const found = {};
      const walk = (n) => {
        if (n.nodeType === 1) {
          if (n.id) found['#' + n.id] = true;
          found[n.localName] = true;
        }
        if (n.shadowRoot) walk(n.shadowRoot);
        for (const c of n.childNodes) walk(c);
      };
      walk(document);
      return found['#importDataDialogTrigger'] ? found : null;
    })()"#;
    let profile = tempfile::tempdir().unwrap();
    let (_child, mut link) = PipeLink::launch(&binary, profile.path(), "en");
    let first = link.first_page().await;
    let s = link.attach(&first).await;
    let found = link.eval_at(&s, "chrome://settings/people", PEOPLE).await;
    let text = link
        .call(
            Some(&s),
            "Runtime.evaluate",
            json!({ "expression": DEEP_TEXT, "returnByValue": true }),
        )
        .await["result"]["value"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let management = link
        .eval_at(
            &s,
            "chrome://management",
            &DEEP_TEXT.replace(
                "return out.join(' ')",
                "if (!out.join(' ').includes('managed')) return null; return out.join(' ')",
            ),
        )
        .await;
    let management = management.as_str().unwrap_or_default().to_string();
    let _ = link.call(None, "Browser.close", json!({})).await;
    println!("OBSERVED people page: {}", head(&text));
    println!("OBSERVED management page: {}", head(&management));
    for hidden in [
        "settings-sync-account-control",
        "#edit-profile",
        "#manage-google-account",
    ] {
        assert!(
            found.get(hidden).is_none(),
            "{hidden} is on the page: {text}"
        );
    }
    assert!(!text.contains("Turn on sync"), "{text}");
    // Defaults, not policies: chrome://management says so. (The people page's own text holds
    // hidden "managed by your administrator" notes of the sync settings either way.)
    assert!(
        management.contains("Your browser is not managed"),
        "{management}"
    );
}

/// A value the person chose wins over pegboard's default: saved before the browser starts
/// (as from an earlier run, or the settings' switches), it is the value in effect.
#[tokio::test]
#[ignore]
async fn a_chosen_value_wins_over_the_default() {
    let Some(binary) = test_fork_binary() else {
        return skip();
    };
    let profile = tempfile::tempdir().unwrap();
    let default = profile.path().join("Default");
    std::fs::create_dir_all(&default).unwrap();
    std::fs::write(
        default.join("Preferences"),
        r#"{"credentials_enable_service": true, "signin": {"allowed_on_next_startup": true}}"#,
    )
    .unwrap();
    let (_child, mut link) = PipeLink::launch(&binary, profile.path(), "en");
    let first = link.first_page().await;
    let s = link.attach(&first).await;
    let values = off_values(&mut link, &s).await;
    let _ = link.call(None, "Browser.close", json!({})).await;
    println!("OBSERVED chosen {values:?}");
    let value = |name: &str| values.iter().find(|(n, _)| n == name).unwrap().1.clone();
    assert_eq!(
        value("credentials_enable_service"),
        json!(true),
        "{values:?}"
    );
    assert_eq!(
        value("signin.allowed_on_next_startup"),
        json!(true),
        "{values:?}"
    );
    // (signin.allowed follows it only in a build with a Google OAuth client: this one has
    // none, so upstream never allows sign-in here, CanEnableDiceForBuild.)
    // The rest keep pegboard's default.
    assert_eq!(
        value("credentials_enable_autosignin"),
        json!(false),
        "{values:?}"
    );
}
