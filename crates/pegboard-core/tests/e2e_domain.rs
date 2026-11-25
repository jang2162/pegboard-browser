//! The Pegboard fork's own CDP domain (`Pegboard.*`, phase 3a task 8), measured on the fork.
//! Every test needs `PEGBOARD_TEST_BROWSER=fork:<Pegboard.app>` (read with test hooks on,
//! which the tests turn on) and is skipped otherwise. Each test launches its own fork with
//! its DevTools on a pipe whose parent end is the test (F13), and opens virtual connections
//! (browser sessions) on that link as the daemon does.
mod common;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use pegboard_core::browser::{fork_executable, test_fork_binary, DevToolsPipes};
use pegboard_core::cdp::{page_targets, CdpConn, CdpEvent};
use pegboard_core::link::BrowserLink;
use serde_json::{json, Value};
use tokio::process::{Child, Command};
use tokio::sync::broadcast;

const AGENT: &str = "PegboardAgent";

/// A fork launched by the test, with the parent ends of its DevTools pipe as `link`. The
/// browser is killed when this is dropped.
struct Fork {
    link: BrowserLink,
    exe: PathBuf,
    udd: PathBuf,
    _child: Child,
    _env: common::Env,
}

impl Fork {
    /// The fork with `extra` flags, or `None` (the test is skipped) without
    /// `PEGBOARD_TEST_BROWSER=fork:…`.
    async fn start(extra: &[String]) -> Option<Fork> {
        std::env::set_var("PEGBOARD_TEST_HOOKS", "1");
        let Some(binary) = test_fork_binary() else {
            eprintln!("skipped: needs PEGBOARD_TEST_BROWSER=fork:<Pegboard.app>");
            return None;
        };
        let env = common::env();
        let exe = fork_executable(&binary);
        let udd = env.paths.chrome_udd.clone();
        let pipes = DevToolsPipes::new().unwrap();
        let mut cmd = Command::new(&exe);
        cmd.arg(format!("--user-data-dir={}", udd.display()))
            .arg("--remote-debugging-pipe")
            .arg("--pegboard-no-daemon")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            // No "Pegboard Safe Storage" keychain prompt holding every HTTP request.
            .arg("--use-mock-keychain")
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        // SAFETY: the closure only makes async-signal-safe system calls.
        unsafe { cmd.pre_exec(pipes.child_setup()) };
        let child = cmd.spawn().unwrap();
        let (read, write) = pipes.into_parent_ends();
        Some(Fork {
            link: BrowserLink::pipe(read, write).unwrap(),
            exe,
            udd,
            _child: child,
            _env: env,
        })
    }

    /// The fork with its confirmation dialogs' test hooks: the dialog log, and the answer
    /// hook `answer` (`PEGBOARD_TEST_DIALOG_ANSWER`) pressing `after_ms` after a dialog shows.
    async fn with_dialogs(
        answer: &str,
        after_ms: Option<u64>,
    ) -> Option<(Fork, common::DialogLog)> {
        let log = common::DialogLog::new();
        let _vars = [
            common::EnvVar::set("PEGBOARD_TEST_DIALOG_LOG", log.path.to_str().unwrap()),
            common::EnvVar::set("PEGBOARD_TEST_DIALOG_ANSWER", answer),
            common::EnvVar::set(
                "PEGBOARD_TEST_DIALOG_ANSWER_AFTER_MS",
                &after_ms.map_or(String::new(), |ms| ms.to_string()),
            ),
        ];
        // The browser keeps the environment it was started with.
        let f = Fork::start(&[]).await?;
        Some((f, log))
    }

    /// A new virtual connection (browser session).
    async fn session(&self) -> CdpConn {
        CdpConn::over(&self.link).await.unwrap()
    }

    /// A new virtual connection that owns the domain.
    async fn control(&self) -> CdpConn {
        let c = self.session().await;
        c.send("Pegboard.claim", json!({}), None).await.unwrap();
        c
    }

    /// Opens a window of the person's profile `dir` (made when missing) the way a person
    /// would: the binary invoked again hands the request to the running browser. Returns
    /// once the domain lists the profile, with its context.
    async fn open_human_profile(&self, control: &CdpConn, dir: &str) -> String {
        let status = tokio::time::timeout(
            Duration::from_secs(20),
            Command::new(&self.exe)
                .arg(format!("--user-data-dir={}", self.udd.display()))
                .arg(format!("--profile-directory={dir}"))
                .arg("--no-first-run")
                .arg("--use-mock-keychain")
                .arg("about:blank")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
        )
        .await
        .expect("the second invocation did not end")
        .unwrap();
        assert!(status.success(), "{status}");
        for _ in 0..100 {
            if let Some(p) = profile(control, dir).await {
                assert_eq!(p["isAgent"], false, "{p}");
                return p["browserContextId"].as_str().unwrap().to_string();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the profile {dir} was not loaded");
    }
}

/// The CDP error code of a failed request (`CdpConn` reports `CDP error: {json}`).
fn code(r: &anyhow::Result<Value>) -> i64 {
    let e = format!("{:#}", r.as_ref().expect_err("the request succeeded"));
    let json = e
        .split_once("CDP error: ")
        .unwrap_or_else(|| panic!("not a CDP error: {e}"))
        .1;
    serde_json::from_str::<Value>(json).unwrap()["code"]
        .as_i64()
        .unwrap()
}

fn message(r: &anyhow::Result<Value>) -> String {
    format!("{:#}", r.as_ref().expect_err("the request succeeded"))
}

async fn profiles(c: &CdpConn) -> Vec<Value> {
    c.send("Pegboard.getProfiles", json!({}), None)
        .await
        .unwrap()["profiles"]
        .as_array()
        .unwrap()
        .clone()
}

async fn profile(c: &CdpConn, dir: &str) -> Option<Value> {
    profiles(c)
        .await
        .into_iter()
        .find(|p| p["profileDirectory"] == dir)
}

async fn open_profile_window(c: &CdpConn, dir: &str) -> anyhow::Result<Value> {
    c.send(
        "Pegboard.openProfileWindow",
        json!({ "profileDirectory": dir }),
        None,
    )
    .await
}

async fn agent_context(c: &CdpConn, dir: &str) -> String {
    let ctx = open_profile_window(c, dir).await.unwrap()["browserContextId"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!ctx.is_empty());
    ctx
}

async fn create_group(c: &CdpConn, ctx: &str) -> anyhow::Result<Value> {
    c.send(
        "Pegboard.createGroup",
        json!({ "browserContextId": ctx, "title": "t", "color": "blue" }),
        None,
    )
    .await
}

async fn pages_in(c: &CdpConn, ctx: &str) -> Vec<String> {
    page_targets(c)
        .await
        .unwrap()
        .into_iter()
        .filter(|t| t.browser_context_id.as_deref() == Some(ctx))
        .map(|t| t.target_id)
        .collect()
}

async fn members(c: &CdpConn, group: i64) -> Vec<String> {
    c.send(
        "Pegboard.getGroupMembers",
        json!({ "groupId": group }),
        None,
    )
    .await
    .unwrap()["targetIds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

/// The next event of the connection's own session named `method` whose params match `want`
/// (every key of `want`), within `secs`.
async fn wait_event(
    events: &mut broadcast::Receiver<CdpEvent>,
    method: &str,
    want: &Value,
    secs: u64,
) -> Value {
    let wait = async {
        loop {
            match events.recv().await {
                Ok(e) if e.method == method && e.session_id.is_none() => {
                    let fits = want
                        .as_object()
                        .unwrap()
                        .iter()
                        .all(|(k, v)| &e.params[k] == v);
                    if fits {
                        return e.params;
                    }
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => panic!("connection closed"),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(secs), wait)
        .await
        .unwrap_or_else(|_| panic!("no {method} {want} in {secs}s"))
}

/// The `Pegboard.*` events `events` holds now.
fn pegboard_events(events: &mut broadcast::Receiver<CdpEvent>) -> Vec<String> {
    let mut seen = Vec::new();
    loop {
        match events.try_recv() {
            Ok(e) if e.method.starts_with("Pegboard.") => seen.push(e.method),
            Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => return seen,
        }
    }
}

fn exists(udd: &Path, dir: &str) -> bool {
    udd.join(dir).exists()
}

/// F12: the first browser session to claim the domain is the only one that has it; the
/// others (a session proxy's virtual connection among them) get "method not found", and a
/// second claim is refused. When the owner goes, the domain can be claimed again.
#[tokio::test]
#[ignore]
async fn domain_is_only_on_the_claiming_session() {
    let Some(f) = Fork::start(&[]).await else {
        return;
    };
    let v1 = f.session().await;
    let v2 = f.session().await;

    // Nobody owns it yet: only claim does anything.
    let r = v2.send("Pegboard.getProfiles", json!({}), None).await;
    assert_eq!(code(&r), -32601, "{}", message(&r));

    v1.send("Pegboard.claim", json!({}), None).await.unwrap();
    // Claiming again is no change.
    v1.send("Pegboard.claim", json!({}), None).await.unwrap();
    assert!(
        profiles(&v1)
            .await
            .iter()
            .any(|p| p["profileDirectory"] == "Default" && p["isAgent"] == false),
        "{:?}",
        profiles(&v1).await
    );

    let r = v2.send("Pegboard.claim", json!({}), None).await;
    assert_eq!(code(&r), -32000, "{}", message(&r));
    assert!(message(&r).contains("already claimed"), "{}", message(&r));
    let calls = [
        ("Pegboard.getProfiles", json!({})),
        (
            "Pegboard.openProfileWindow",
            json!({ "profileDirectory": "PegboardAgent-v2" }),
        ),
        (
            "Pegboard.createGroup",
            json!({ "browserContextId": "x", "title": "t", "color": "blue" }),
        ),
        ("Pegboard.getGroupMembers", json!({ "groupId": 1 })),
        ("Pegboard.closeTargets", json!({ "targetIds": [] })),
        ("Pegboard.ungroup", json!({ "groupId": 1 })),
    ];
    for (method, params) in &calls {
        let r = v2.send(method, params.clone(), None).await;
        assert_eq!(code(&r), -32601, "{method}: {}", message(&r));
        assert!(
            message(&r).contains("wasn't found"),
            "{method}: {}",
            message(&r)
        );
    }
    assert!(
        !exists(&f.udd, "PegboardAgent-v2"),
        "a refused openProfileWindow made the profile"
    );

    // The owner goes (a daemon restarting): the domain is free again.
    v1.close();
    let mut claimed = false;
    for _ in 0..50 {
        if v2.send("Pegboard.claim", json!({}), None).await.is_ok() {
            claimed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(claimed, "the domain stayed claimed after its owner went");
    assert!(!profiles(&v2).await.is_empty());
}

/// F10: the domain does nothing to a person's profile, whichever: `Default` and a second
/// one (`Profile 1`). Its contexts get no group or tab, and its tabs are not grouped,
/// ungrouped or closed, not even beside an agent tab in the same call.
#[tokio::test]
#[ignore]
async fn domain_refuses_non_agent_contexts() {
    let Some(f) = Fork::start(&[]).await else {
        return;
    };
    let c = f.control().await;
    let second = f.open_human_profile(&c, "Profile 1").await;
    let default = profile(&c, "Default").await.expect("no Default profile")["browserContextId"]
        .as_str()
        .unwrap()
        .to_string();
    let humans = [default.clone(), second.clone()];
    let agent = agent_context(&c, AGENT).await;
    let made = create_group(&c, &agent).await.unwrap();
    let group = made["groupId"].as_i64().unwrap();
    let agent_tab = made["targetId"].as_str().unwrap().to_string();

    let mut human_tabs = Vec::new();
    for h in &humans {
        human_tabs.extend(pages_in(&c, h).await);
    }
    assert!(
        !pages_in(&c, &default).await.is_empty(),
        "no page of the person's Default profile"
    );
    let before: Vec<Vec<String>> = {
        let mut v = Vec::new();
        for h in &humans {
            v.push(pages_in(&c, h).await);
        }
        v
    };

    for ctx in humans
        .iter()
        .map(String::as_str)
        .chain(["", "not-a-context"])
    {
        let r = create_group(&c, ctx).await;
        assert_eq!(code(&r), -32000, "createGroup in {ctx:?}: {}", message(&r));
        let r = c
            .send(
                "Pegboard.createTab",
                json!({ "browserContextId": ctx, "groupId": group, "url": "about:blank",
                        "title": "t", "color": "blue" }),
                None,
            )
            .await;
        assert_eq!(code(&r), -32000, "createTab in {ctx:?}: {}", message(&r));
    }
    for t in &human_tabs {
        let r = c
            .send(
                "Pegboard.addToGroup",
                json!({ "groupId": group, "targetId": t }),
                None,
            )
            .await;
        assert_eq!(code(&r), -32000, "addToGroup {t}: {}", message(&r));
        for method in ["Pegboard.closeTargets", "Pegboard.ungroupTargets"] {
            let r = c.send(method, json!({ "targetIds": [t] }), None).await;
            assert_eq!(code(&r), -32000, "{method} {t}: {}", message(&r));
            // All or nothing: with an agent tab beside it, neither is touched.
            let r = c
                .send(method, json!({ "targetIds": [&agent_tab, t] }), None)
                .await;
            assert_eq!(code(&r), -32000, "{method} {t} + agent: {}", message(&r));
        }
    }

    let mut after = Vec::new();
    for h in &humans {
        after.push(pages_in(&c, h).await);
    }
    assert_eq!(after, before, "the people's tabs changed");
    assert_eq!(members(&c, group).await, vec![agent_tab.clone()]);
    assert!(pages_in(&c, &agent).await.contains(&agent_tab));
}

/// `openProfileWindow` makes a missing agent profile, opens its window and reports its
/// context, which `getProfiles` lists as an agent's. Asked again, it gives the same context
/// and opens no other window. A person's profile, or a name that is no agent profile's
/// directory, is refused and nothing is made.
#[tokio::test]
#[ignore]
async fn open_profile_window_creates_and_reports_context() {
    let Some(f) = Fork::start(&[]).await else {
        return;
    };
    let c = f.control().await;
    assert!(profile(&c, "PegboardAgent-t1").await.is_none());
    let ctx = agent_context(&c, "PegboardAgent-t1").await;
    let p = profile(&c, "PegboardAgent-t1")
        .await
        .expect("the new profile is not listed");
    assert_eq!(p["browserContextId"], ctx.as_str());
    assert_eq!(p["isAgent"], true);
    assert!(exists(&f.udd, "PegboardAgent-t1"));
    // One window with one about:blank tab: no first-run or welcome page.
    let opened: Vec<_> = page_targets(&c)
        .await
        .unwrap()
        .into_iter()
        .filter(|t| t.browser_context_id.as_deref() == Some(ctx.as_str()))
        .collect();
    assert_eq!(opened.len(), 1, "{opened:?}");
    assert_eq!(opened[0].url, "about:blank", "{opened:?}");
    let pages = pages_in(&c, &ctx).await;

    assert_eq!(agent_context(&c, "PegboardAgent-t1").await, ctx);
    assert_eq!(pages_in(&c, &ctx).await, pages);

    for dir in [
        "Profile 1",
        "Default",
        "PegboardAgentX",
        "pegboardagent",
        "PegboardAgent-../evil",
        "PegboardAgent-a/b",
        "",
    ] {
        let r = open_profile_window(&c, dir).await;
        assert_eq!(code(&r), -32000, "{dir:?}: {}", message(&r));
    }
    // Agent names are lowercase after the prefix: no two of them differ only in case (one
    // directory on macOS).
    for dir in ["PegboardAgent-T1", "PegboardAgent-Work"] {
        let r = open_profile_window(&c, dir).await;
        assert_eq!(code(&r), -32000, "{dir}: {}", message(&r));
    }
    for dir in [
        "Profile 1",
        "PegboardAgentX",
        "pegboardagent",
        "evil",
        "PegboardAgent-..",
        "PegboardAgent-a",
        "PegboardAgent-Work",
    ] {
        assert!(!exists(&f.udd, dir), "{dir} was made");
    }
    assert!(!f.udd.parent().unwrap().join("evil").exists());
}

/// Review focus 4: when its last window closes the agent profile unloads
/// (`profileUnloaded`), and opened again it comes back with a new context
/// (`profileLoaded`). The events go to the owner alone.
#[tokio::test]
#[ignore]
async fn profile_events_follow_unload_and_reload() {
    let Some(f) = Fork::start(&[]).await else {
        return;
    };
    let c = f.control().await;
    let other = f.session().await;
    let mut events = c.events();
    let mut others = other.events();

    let first = agent_context(&c, AGENT).await;
    wait_event(
        &mut events,
        "Pegboard.profileLoaded",
        &json!({ "profileDirectory": AGENT, "browserContextId": first }),
        10,
    )
    .await;

    for t in pages_in(&c, &first).await {
        c.send("Target.closeTarget", json!({ "targetId": t }), None)
            .await
            .unwrap();
    }
    wait_event(
        &mut events,
        "Pegboard.profileUnloaded",
        &json!({ "profileDirectory": AGENT, "browserContextId": first }),
        60,
    )
    .await;
    assert!(
        !profiles(&c)
            .await
            .iter()
            .any(|p| p["browserContextId"] == first.as_str()),
        "the unloaded context is still listed"
    );

    let second = agent_context(&c, AGENT).await;
    assert_ne!(second, first);
    wait_event(
        &mut events,
        "Pegboard.profileLoaded",
        &json!({ "profileDirectory": AGENT, "browserContextId": second }),
        10,
    )
    .await;
    let p = profile(&c, AGENT).await.unwrap();
    assert_eq!(p["browserContextId"], second.as_str());
    assert_eq!(p["isAgent"], true);

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(pegboard_events(&mut others), Vec::<String>::new());
}

/// `tabActivated` reports an agent tab becoming its window's active one, with its context
/// and the time; a person's tab switches are not reported, and nothing goes to another
/// session.
#[tokio::test]
#[ignore]
async fn tab_activated_reports_switches() {
    let Some(f) = Fork::start(&[]).await else {
        return;
    };
    let c = f.control().await;
    let other = f.session().await;
    let mut others = other.events();
    // A second tab in the person's window (while Default is the last used profile, a
    // context-less createTarget lands there), active now: the first one is not.
    let default = profile(&c, "Default").await.unwrap()["browserContextId"]
        .as_str()
        .unwrap()
        .to_string();
    let first_human = pages_in(&c, &default).await;
    let second_human = c
        .send("Target.createTarget", json!({ "url": "about:blank" }), None)
        .await
        .unwrap()["targetId"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(common::context_of(&c, &second_human).await, default);
    let human = first_human.first().expect("no page of Default").clone();
    let ctx = agent_context(&c, AGENT).await;
    let mut steps = vec![("opened", agent_pages(&c, &ctx).await)];
    let made = create_group(&c, &ctx).await.unwrap();
    steps.push(("group", agent_pages(&c, &ctx).await));
    let group = made["groupId"].as_i64().unwrap();
    let t1 = made["targetId"].as_str().unwrap().to_string();
    let t2 = c
        .send(
            "Pegboard.createTab",
            json!({ "browserContextId": ctx, "groupId": group, "url": "about:blank",
                    "title": "t", "color": "blue" }),
            None,
        )
        .await
        .unwrap()["targetId"]
        .as_str()
        .unwrap()
        .to_string();

    steps.push(("tab", agent_pages(&c, &ctx).await));
    let mut events = c.events();
    for t in [&t2, &t1] {
        let before = common_ms();
        if let Err(e) = c
            .send("Target.activateTarget", json!({ "targetId": t }), None)
            .await
        {
            let all = c.send("Target.getTargets", json!({}), None).await;
            let info = c
                .send("Target.getTargetInfo", json!({ "targetId": t }), None)
                .await;
            let members = c
                .send(
                    "Pegboard.getGroupMembers",
                    json!({ "groupId": group }),
                    None,
                )
                .await;
            steps.push(("failed", agent_pages(&c, &ctx).await));
            panic!(
                "activating {t} (t1 {t1}, t2 {t2}, made {made}): {e:#}\n\
                 steps {steps:?}\ntargets {all:?}\ninfo {info:?}\nmembers {members:?}"
            );
        }
        let e = wait_event(
            &mut events,
            "Pegboard.tabActivated",
            &json!({ "browserContextId": ctx, "targetId": t }),
            10,
        )
        .await;
        let at = e["timestamp"].as_f64().unwrap();
        assert!(
            at >= before - 1000.0 && at <= common_ms() + 1000.0,
            "timestamp {at} is not about now ({before})"
        );
    }

    // A person's tab switch: no event of a person's context.
    c.send("Target.activateTarget", json!({ "targetId": human }), None)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;
    loop {
        match events.try_recv() {
            Ok(e) if e.method == "Pegboard.tabActivated" => {
                assert_eq!(e.params["browserContextId"], ctx.as_str(), "{:?}", e.params);
                assert_ne!(e.params["targetId"], human.as_str(), "{:?}", e.params);
            }
            Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => break,
        }
    }
    assert_eq!(pegboard_events(&mut others), Vec::<String>::new());
}

/// The agent context's pages as (id, url, title), for diagnostics.
async fn agent_pages(c: &CdpConn, ctx: &str) -> Vec<(String, String, String)> {
    let r = c.send("Target.getTargets", json!({}), None).await.unwrap();
    r["targetInfos"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["type"] == "page" && t["browserContextId"] == ctx)
        .map(|t| {
            let s = |k: &str| t[k].as_str().unwrap_or("").to_string();
            (s("targetId"), s("url"), s("title"))
        })
        .collect()
}

fn common_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as f64
}

/// An extension's `chrome.debugger` (an untrusted client) has no `Pegboard` domain: not
/// before the daemon claimed it, and not after.
#[tokio::test]
#[ignore]
async fn extension_debugger_cannot_see_domain() {
    let ext = tempfile::tempdir().unwrap();
    std::fs::write(
        ext.path().join("manifest.json"),
        r#"{ "manifest_version": 3, "name": "Pegboard test: debugger", "version": "0.0.1",
             "permissions": ["debugger", "tabs"],
             "background": { "service_worker": "sw.js" } }"#,
    )
    .unwrap();
    // Attaches to a tab of its own and sends each method; the answers (or errors) by method.
    std::fs::write(
        ext.path().join("sw.js"),
        r#"self.probe = async (methods) => {
  // A tab it may debug (not the new tab page, a chrome:// one).
  const tab = await chrome.tabs.create({ url: 'about:blank', active: false });
  const target = { tabId: tab.id };
  await chrome.debugger.attach(target, '1.3');
  const out = {};
  for (const m of methods) {
    try {
      out[m] = { ok: await chrome.debugger.sendCommand(target, m, {}) };
    } catch (e) {
      out[m] = { error: String(e && e.message || e) };
    }
  }
  await chrome.debugger.detach(target);
  await chrome.tabs.remove(tab.id);
  return out;
};"#,
    )
    .unwrap();
    let Some(f) = Fork::start(&[
        format!("--load-extension={}", ext.path().display()),
        "--disable-features=DisableLoadExtensionCommandLineSwitch".to_string(),
    ])
    .await
    else {
        return;
    };
    let c = f.session().await;
    let mut worker = None;
    for _ in 0..100 {
        let r = c.send("Target.getTargets", json!({}), None).await.unwrap();
        worker = r["targetInfos"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| {
                t["type"] == "service_worker"
                    && t["url"]
                        .as_str()
                        .is_some_and(|u| u.starts_with("chrome-extension://"))
            })
            .map(|t| t["targetId"].as_str().unwrap().to_string());
        if worker.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let worker = worker.expect("the test extension's worker did not start");
    let session = c.attach(&worker).await.unwrap();
    let methods = [
        "Pegboard.claim",
        "Pegboard.getProfiles",
        "Target.attachToBrowserTarget",
    ];
    let probe = format!("self.probe({})", json!(methods));
    // The worker's target can be there before its script has run.
    let mut ready = false;
    for _ in 0..100 {
        if c.evaluate(&session, "typeof self.probe").await.ok() == Some(json!("function")) {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ready, "the test extension's worker has no probe");

    let check = |out: &Value, when: &str| {
        eprintln!("chrome.debugger {when}: {out}");
        for m in ["Pegboard.claim", "Pegboard.getProfiles"] {
            let e = out[m]["error"]
                .as_str()
                .unwrap_or_else(|| panic!("{when}: {m} answered through chrome.debugger: {out}"));
            assert!(e.contains("wasn't found"), "{when}: {m}: {e}");
        }
        // Nor can it reach a browser session that would have it.
        assert!(
            out["Target.attachToBrowserTarget"]["error"].is_string(),
            "{when}: {out}"
        );
    };
    let out = c.evaluate(&session, &probe).await.unwrap();
    check(&out, "before the claim");
    // The extension took nothing: the daemon's claim works.
    c.send("Pegboard.claim", json!({}), None).await.unwrap();
    let out = c.evaluate(&session, &probe).await.unwrap();
    check(&out, "after the claim");
    assert!(!profiles(&c).await.is_empty());
}

/// The first wall (F12): a session proxy client gets no `Pegboard.*`, browser-level or on a
/// page session of its own.
#[tokio::test]
#[ignore]
async fn proxy_refuses_pegboard_methods() {
    std::env::set_var("PEGBOARD_TEST_HOOKS", "1");
    if test_fork_binary().is_none() {
        eprintln!("skipped: needs PEGBOARD_TEST_BROWSER=fork:<Pegboard.app>");
        return;
    }
    let fx = common::session_fixture().await;
    let client = common::client(&fx.endpoint).await;
    let page = client.attach(&fx.first_target).await.unwrap();
    let calls = [
        ("Pegboard.claim", json!({})),
        ("Pegboard.getProfiles", json!({})),
        (
            "Pegboard.createTab",
            json!({ "browserContextId": fx.agent_context, "groupId": 1,
                    "url": "about:blank", "title": "t", "color": "blue" }),
        ),
        (
            "Pegboard.closeTargets",
            json!({ "targetIds": [fx.human_target] }),
        ),
    ];
    for (method, params) in &calls {
        for session in [None, Some(page.as_str())] {
            let r = client.send(method, params.clone(), session).await;
            assert_eq!(code(&r), -32601, "{method} ({session:?}): {}", message(&r));
            assert!(
                message(&r).contains("not allowed in a pegboard session"),
                "{method} ({session:?}): {}",
                message(&r)
            );
        }
    }
    // The person's tab is still there, and the daemon still has the domain.
    assert!(page_targets(&fx.browser.conn)
        .await
        .unwrap()
        .iter()
        .any(|t| t.target_id == fx.human_target));
    assert!(!profiles(&fx.browser.conn).await.is_empty());
    fx.shutdown().await;
}

/// The domain is the pipe's alone: a client of the remote debugging port (turned on here
/// beside the pipe, tests only; the product has no port) has no `Pegboard` domain on its own
/// browser session or on one it opens, even while nobody has claimed it.
#[tokio::test]
#[ignore]
async fn remote_debugging_port_has_no_domain() {
    let Some(f) = Fork::start(&["--remote-debugging-port=0".to_string()]).await else {
        return;
    };
    let pipe = f.session().await;
    let port_file = f.udd.join("DevToolsActivePort");
    let mut ws = None;
    for _ in 0..100 {
        if let Ok(text) = std::fs::read_to_string(&port_file) {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                ws = Some(format!("ws://127.0.0.1:{port}{path}"));
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let ws = ws.expect("no DevToolsActivePort");
    let root = CdpConn::connect(&ws).await.unwrap();
    let link = BrowserLink::websocket(&ws).await.unwrap();
    let child = CdpConn::over(&link).await.unwrap();
    for (name, c) in [
        ("port root session", &root),
        ("port browser session", &child),
    ] {
        for method in ["Pegboard.claim", "Pegboard.getProfiles"] {
            let r = c.send(method, json!({}), None).await;
            assert_eq!(code(&r), -32601, "{name} {method}: {}", message(&r));
            assert!(
                message(&r).contains("wasn't found"),
                "{name}: {}",
                message(&r)
            );
        }
    }
    // The port took nothing: the pipe's session can claim.
    pipe.send("Pegboard.claim", json!({}), None).await.unwrap();
    let r = child.send("Pegboard.claim", json!({}), None).await;
    assert_eq!(code(&r), -32601, "{}", message(&r));
}

/// Only browser sessions have the domain: a page session on the pipe gets "method not
/// found", before and after the claim.
#[tokio::test]
#[ignore]
async fn page_sessions_have_no_domain() {
    let Some(f) = Fork::start(&[]).await else {
        return;
    };
    let c = f.session().await;
    let default = c
        .send("Target.createTarget", json!({ "url": "about:blank" }), None)
        .await
        .unwrap()["targetId"]
        .as_str()
        .unwrap()
        .to_string();
    let page = c.attach(&default).await.unwrap();
    let r = c.send("Pegboard.claim", json!({}), Some(&page)).await;
    assert_eq!(code(&r), -32601, "{}", message(&r));
    assert!(message(&r).contains("wasn't found"), "{}", message(&r));
    c.send("Pegboard.claim", json!({}), None).await.unwrap();
    let r = c.send("Pegboard.getProfiles", json!({}), Some(&page)).await;
    assert_eq!(code(&r), -32601, "{}", message(&r));
}

/// `Target.exposeDevToolsProtocol` from the pipe gives a page a browser session
/// (`window.cdp`). The fork refuses `inheritPermissions: true` (it would hand the page the
/// pipe's unsafe operations, the domain's test). Neither the binding nor a browser session it
/// opens has the domain, before or after the pipe's claim. (The proxy refuses the method to
/// agents anyway: `GUARD_METHODS`.)
#[tokio::test]
#[ignore]
async fn exposed_protocol_binding_has_no_domain() {
    let Some(f) = Fork::start(&[]).await else {
        return;
    };
    let c = f.session().await;
    let target = c
        .send("Target.createTarget", json!({ "url": "about:blank" }), None)
        .await
        .unwrap()["targetId"]
        .as_str()
        .unwrap()
        .to_string();
    let r = c
        .send(
            "Target.exposeDevToolsProtocol",
            json!({ "targetId": target, "inheritPermissions": true }),
            None,
        )
        .await;
    assert_eq!(code(&r), -32000, "{}", message(&r));
    assert!(
        message(&r).contains("inheritPermissions is not supported"),
        "{}",
        message(&r)
    );
    c.send(
        "Target.exposeDevToolsProtocol",
        json!({ "targetId": target }),
        None,
    )
    .await
    .unwrap();
    let page = c.attach(&target).await.unwrap();
    // The page's own CDP client over window.cdp: claim on the binding's session, then on a
    // browser session it opens.
    let probe = r#"new Promise((resolve) => {
  const out = {};
  const waiting = {};
  let next = 1;
  window.cdp.onmessage = (m) => {
    const v = JSON.parse(m);
    if (waiting[v.id]) waiting[v.id](v);
  };
  const send = (method, sessionId) => new Promise((r) => {
    const id = next++;
    waiting[id] = r;
    const msg = { id, method, params: {} };
    if (sessionId) msg.sessionId = sessionId;
    window.cdp.send(JSON.stringify(msg));
  });
  setTimeout(() => { out.timedOut = true; resolve(out); }, 5000);
  (async () => {
    out.claim = await send('Pegboard.claim');
    out.attach = await send('Target.attachToBrowserTarget');
    const s = out.attach.result && out.attach.result.sessionId;
    if (s) out.childClaim = await send('Pegboard.claim', s);
    resolve(out);
  })();
})"#;
    for when in ["before the claim", "after the claim"] {
        let out = c.evaluate(&page, probe).await.unwrap();
        eprintln!("window.cdp {when}: {out}");
        assert!(out["timedOut"].is_null(), "{when}: {out}");
        assert_eq!(out["claim"]["error"]["code"], -32601, "{when}: {out}");
        assert!(
            out["attach"]["result"]["sessionId"].is_string(),
            "{when}: {out}"
        );
        assert_eq!(out["childClaim"]["error"]["code"], -32601, "{when}: {out}");
        if when == "before the claim" {
            c.send("Pegboard.claim", json!({}), None).await.unwrap();
        }
    }
}

/// A new agent profile's saved-tab-group service starts a moment after the profile loads,
/// and when it starts it closes the tabs of a local group that its saved copy does not have
/// (`TabGroupSyncCoordinatorImpl::InitializeTabGroupSync`). So `openProfileWindow` answers
/// only once that service has started: groups and tabs made right after it stay. Several
/// fresh profiles, as the race is short.
#[tokio::test]
#[ignore]
async fn tabs_made_right_after_a_new_profile_opens_stay() {
    let Some(f) = Fork::start(&[]).await else {
        return;
    };
    let c = f.control().await;
    for n in 1..=4 {
        let ctx = agent_context(&c, &format!("PegboardAgent-r{n}")).await;
        let made = create_group(&c, &ctx).await.unwrap();
        let group = made["groupId"].as_i64().unwrap();
        let mut tabs = vec![made["targetId"].as_str().unwrap().to_string()];
        for _ in 0..3 {
            let r = c
                .send(
                    "Pegboard.createTab",
                    json!({ "browserContextId": ctx, "groupId": group, "url": "about:blank",
                            "title": "t", "color": "blue" }),
                    None,
                )
                .await
                .unwrap();
            assert!(r["newGroupId"].is_null(), "{r}");
            tabs.push(r["targetId"].as_str().unwrap().to_string());
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let mut now = members(&c, group).await;
        now.sort();
        tabs.sort();
        assert_eq!(now, tabs, "profile r{n}: the group lost tabs");
        let pages = pages_in(&c, &ctx).await;
        for t in &tabs {
            assert!(pages.contains(t), "profile r{n}: {t} is gone: {pages:?}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Confirmation dialogs (phase 3b task 7): Pegboard.requestConfirmation / cancelConfirmation and
// the event Pegboard.confirmationAnswered. What a dialog showed and how it ended is read from
// the browser's test hook log (`PEGBOARD_TEST_DIALOG_LOG`).
// ---------------------------------------------------------------------------------------------

const CONFIRMATION_ANSWERED: &str = "Pegboard.confirmationAnswered";

/// A confirmId as the daemon makes them: 32 lowercase hexadecimal digits.
fn cid(n: u32) -> String {
    format!("{:032x}", 0xc0ff_ee00_u64 * 1000 + u64::from(n))
}

/// requestConfirmation's parameters for a dialog of the session `name` on the agent tab
/// `tab` of the context `ctx` (profile `dir`), with two detail rows.
fn ask(id: &str, ctx: &str, dir: &str, tabs: &[&str], name: &str) -> Value {
    json!({
        "confirmId": id,
        "browserContextId": ctx,
        "targetIds": tabs,
        "sessionId": "s_0123abcd",
        "sessionName": name,
        "profileDirectory": dir,
        "actionClass": "state",
        "details": [
            { "label": "출처", "value": "https://a.test" },
            { "label": "키", "value": "k" },
        ],
        "offerAllowForSession": true,
    })
}

async fn request(c: &CdpConn, params: &Value) -> anyhow::Result<Value> {
    c.send("Pegboard.requestConfirmation", params.clone(), None)
        .await
}

async fn cancel(c: &CdpConn, id: &str) -> anyhow::Result<Value> {
    c.send(
        "Pegboard.cancelConfirmation",
        json!({ "confirmId": id }),
        None,
    )
    .await
}

/// An agent profile's context and a tab of its window.
async fn agent_tab(c: &CdpConn, dir: &str) -> (String, String) {
    let ctx = agent_context(c, dir).await;
    let made = create_group(c, &ctx).await.unwrap();
    (ctx, made["targetId"].as_str().unwrap().to_string())
}

/// The `confirmationAnswered` events `events` gets within `ms`.
async fn answers_within(events: &mut broadcast::Receiver<CdpEvent>, ms: u64) -> Vec<Value> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Err(_) => return seen,
            Ok(Ok(e)) if e.method == CONFIRMATION_ANSWERED => seen.push(json!({
                "sessionId": e.session_id, "params": e.params })),
            Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(broadcast::error::RecvError::Closed)) => return seen,
        }
    }
}

/// F12 for the confirmation methods: only the claiming session can raise or close a dialog;
/// any other browser session of the pipe gets "method not found" and shows nothing.
#[tokio::test]
#[ignore]
async fn confirmation_methods_are_only_on_the_claiming_session() {
    let Some((f, log)) = Fork::with_dialogs("*=none", None).await else {
        return;
    };
    let other = f.session().await;
    let c = f.control().await;
    let (ctx, tab) = agent_tab(&c, AGENT).await;
    let id = cid(1);
    for r in [
        request(&other, &ask(&id, &ctx, AGENT, &[&tab], "n")).await,
        cancel(&other, &id).await,
    ] {
        assert_eq!(code(&r), -32601, "{}", message(&r));
        assert!(message(&r).contains("wasn't found"), "{}", message(&r));
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(log.entries().is_empty(), "{:?}", log.entries());
    // The owner can.
    request(&c, &ask(&id, &ctx, AGENT, &[&tab], "n"))
        .await
        .unwrap();
    log.wait("confirm_shown", &id, 5).await;
    // The other session cannot close it.
    let r = cancel(&other, &id).await;
    assert_eq!(code(&r), -32601, "{}", message(&r));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(log.of("confirm", &id).is_empty(), "{:?}", log.entries());
    cancel(&c, &id).await.unwrap();
    assert_eq!(log.wait("confirm", &id, 5).await["closed"], "cancelled");
}

/// F10 for dialogs: a person's context, a person's tab (also beside an agent tab), another
/// profile's tab, a tab that is gone, or a profile directory that is not the context's refuse
/// the request (-32000) and show nothing.
#[tokio::test]
#[ignore]
async fn domain_refuses_confirmations_for_human_contexts() {
    let Some((f, log)) = Fork::with_dialogs("*=none", None).await else {
        return;
    };
    let c = f.control().await;
    let second = f.open_human_profile(&c, "Profile 1").await;
    let default = profile(&c, "Default").await.expect("no Default profile")["browserContextId"]
        .as_str()
        .unwrap()
        .to_string();
    let human_tab = pages_in(&c, &default).await.pop().expect("no Default tab");
    let (ctx, tab) = agent_tab(&c, AGENT).await;
    let (_, other_tab) = agent_tab(&c, "PegboardAgent-other").await;
    let gone = {
        let (_, t) = agent_tab(&c, AGENT).await;
        c.send("Pegboard.closeTargets", json!({ "targetIds": [&t] }), None)
            .await
            .unwrap();
        t
    };
    let refused = [
        ask(&cid(1), &default, "Default", &[&human_tab], "n"),
        ask(&cid(2), &second, "Profile 1", &[], "n"),
        ask(&cid(3), &default, AGENT, &[&tab], "n"),
        ask(&cid(4), "not-a-context", AGENT, &[&tab], "n"),
        ask(&cid(5), &ctx, AGENT, &[&human_tab], "n"),
        ask(&cid(6), &ctx, AGENT, &[&tab, &human_tab], "n"),
        ask(&cid(7), &ctx, AGENT, &[&other_tab], "n"),
        ask(&cid(8), &ctx, "Default", &[&tab], "n"),
        ask(&cid(9), &ctx, "PegboardAgent-other", &[&tab], "n"),
        ask(&cid(10), &ctx, AGENT, &[&gone], "n"),
    ];
    for p in &refused {
        let r = request(&c, p).await;
        assert_eq!(code(&r), -32000, "{p}: {}", message(&r));
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(log.entries().is_empty(), "{:?}", log.entries());
    // In its own profile it shows, there.
    request(&c, &ask(&cid(11), &ctx, AGENT, &[&tab], "n"))
        .await
        .unwrap();
    assert_eq!(
        log.wait("confirm_shown", &cid(11), 5).await["profile"],
        AGENT
    );
    // No tab of the session: the profile's window.
    request(&c, &ask(&cid(12), &ctx, AGENT, &[], "n"))
        .await
        .unwrap();
    cancel(&c, &cid(11)).await.unwrap();
    assert_eq!(
        log.wait("confirm_shown", &cid(12), 5).await["profile"],
        AGENT
    );
    cancel(&c, &cid(12)).await.unwrap();
}

/// The request's limits (design 4.5): the confirmId's shape and uniqueness, the action class,
/// 40 detail rows, a label of 40 and a value of 1,000 characters, 16 open. Dialogs of one window
/// wait their turn: cancelling the one up shows the next.
#[tokio::test]
#[ignore]
async fn confirmation_limits_are_enforced() {
    let Some((f, log)) = Fork::with_dialogs("*=none", None).await else {
        return;
    };
    let c = f.control().await;
    let (ctx, tab) = agent_tab(&c, AGENT).await;
    let base = ask(&cid(0), &ctx, AGENT, &[&tab], "n");
    let with = |key: &str, v: Value| {
        let mut p = base.clone();
        p[key] = v;
        p
    };
    let row = |label: &str, value: String| json!({ "label": label, "value": value });
    let mut refused = vec![
        with("confirmId", json!("")),
        with("confirmId", json!("0".repeat(31))),
        with("confirmId", json!("0".repeat(33))),
        with("confirmId", json!("A".repeat(32))),
        with("confirmId", json!("g".repeat(32))),
        with("actionClass", json!("network")),
        with("sessionId", json!("")),
        with("sessionId", json!("s_\u{202e}x")),
        with("details", json!(vec![row("l", "v".into()); 41])),
        with("details", json!([row("l", "가".repeat(1001))])),
        with("details", json!([row(&"가".repeat(41), "v".into())])),
    ];
    for p in &mut refused {
        let r = request(&c, p).await;
        assert_eq!(code(&r), -32000, "{p}: {}", message(&r));
    }
    // At the limits it is taken.
    let mut full = with(
        "details",
        json!(vec![row(&"가".repeat(40), "가".repeat(1000)); 40]),
    );
    full["confirmId"] = json!(cid(1));
    request(&c, &full).await.unwrap();
    for n in 2..=16 {
        let mut p = base.clone();
        p["confirmId"] = json!(cid(n));
        request(&c, &p).await.unwrap();
    }
    let mut p17 = base.clone();
    p17["confirmId"] = json!(cid(17));
    let r = request(&c, &p17).await;
    assert_eq!(code(&r), -32000, "the 17th: {}", message(&r));
    let mut dup = base.clone();
    dup["confirmId"] = json!(cid(3));
    let r = request(&c, &dup).await;
    assert_eq!(code(&r), -32000, "an open one again: {}", message(&r));
    // One at a time in the window: only the first is up.
    log.wait("confirm_shown", &cid(1), 5).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        log.entries()
            .iter()
            .filter(|e| e["kind"] == "confirm_shown")
            .count(),
        1,
        "{:?}",
        log.entries()
    );
    cancel(&c, &cid(1)).await.unwrap();
    log.wait("confirm_shown", &cid(2), 5).await;
    // A free place again.
    request(&c, &p17).await.unwrap();
    // The queued ones first (newest first), so none of them comes up.
    for n in (2..=17).rev() {
        cancel(&c, &cid(n)).await.unwrap();
    }
    // A queued one that is cancelled was never shown and has no end line.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(log.of("confirm_shown", &cid(5)).is_empty());
    assert_eq!(log.wait("confirm", &cid(2), 5).await["closed"], "cancelled");
    // Cancelling one that is gone succeeds.
    cancel(&c, &cid(1)).await.unwrap();
    cancel(&c, &cid(99)).await.unwrap();
}

/// What the dialog shows: the session's name (escaped), ID and profile, a sentence, every
/// detail row in the daemon's order, each value whole (a 1,000-character path with a
/// right-to-left directory, wrapped, not cut), the time limit, the three buttons.
#[tokio::test]
#[ignore]
async fn dialog_shows_every_detail_in_order() {
    let Some((f, log)) = Fork::with_dialogs("*=none", None).await else {
        return;
    };
    let c = f.control().await;
    let (ctx, tab) = agent_tab(&c, AGENT).await;
    let mut path = String::from("/tmp/שלום/עולם/");
    while path.chars().count() < 990 {
        path.push_str("dir-abc/");
    }
    path.push_str("report.pdf");
    let path: String = path.chars().take(1000).collect();
    let mut details = vec![
        json!({ "label": "호스트", "value": "a.test" }),
        json!({ "label": "대상 페이지", "value": "https://a.test" }),
        json!({ "label": "파일", "value": path }),
        json!({ "label": "크기", "value": "12 B" }),
    ];
    for n in 0..36 {
        details.push(json!({ "label": format!("줄{n}"), "value": format!("값 {n}") }));
    }
    let mut p = ask(&cid(1), &ctx, AGENT, &[&tab], "build");
    p["actionClass"] = json!("upload");
    p["details"] = json!(details);
    p["offerAllowForSession"] = json!(true);
    request(&c, &p).await.unwrap();
    log.wait("confirm_shown", &cid(1), 5).await;
    cancel(&c, &cid(1)).await.unwrap();
    let e = log.wait("confirm", &cid(1), 5).await;
    let lines = common::DialogLog::lines(&e);
    assert_eq!(lines[0], "에이전트가 확인을 요청합니다", "{lines:?}");
    assert_eq!(lines[1], "세션 «build» (s_0123abcd) · 프로필 PegboardAgent");
    assert_eq!(lines[2], "파일 1개를 https://a.test 에 올리려 합니다");
    let rows: Vec<String> = details
        .iter()
        .flat_map(|d| {
            [
                d["label"].as_str().unwrap().to_string(),
                d["value"].as_str().unwrap().to_string(),
            ]
        })
        .collect();
    assert_eq!(
        &lines[3..3 + rows.len()],
        &rows[..],
        "the rows, in order, whole"
    );
    assert_eq!(lines[3 + rows.len()], "60초 안에 답하지 않으면 거부됩니다");
    assert_eq!(lines.len(), 4 + rows.len());
    // The path is shown left to right: a mark after each '/' of it.
    let shown_path = e["lines"][3 + 5].as_str().unwrap();
    assert_eq!(
        shown_path.matches("/\u{200E}").count(),
        path.matches('/').count(),
        "{shown_path}"
    );
    assert_eq!(
        e["buttons"],
        json!(["허용", "거부", "이 세션에서 계속 허용"])
    );
    assert_eq!(e["defaultButton"], "거부");
    assert_eq!(e["profile"], AGENT);
    assert_eq!(e["sessionId"], "s_0123abcd");
    assert!(e["pressed"].is_null(), "{e}");
    // Not offered: two buttons.
    let mut p = ask(&cid(2), &ctx, AGENT, &[&tab], "build");
    p["offerAllowForSession"] = json!(false);
    request(&c, &p).await.unwrap();
    log.wait("confirm_shown", &cid(2), 5).await;
    cancel(&c, &cid(2)).await.unwrap();
    assert_eq!(
        log.wait("confirm", &cid(2), 5).await["buttons"],
        json!(["허용", "거부"])
    );
}

/// Text an agent chose is shown escaped by the browser too (the daemon already escapes it):
/// a right-to-left override, an isolate, a zero-width space and a control character show as
/// `\u{…}`, and none of them is in what the dialog shows.
#[tokio::test]
#[ignore]
async fn bidi_text_is_shown_escaped() {
    let Some((f, log)) = Fork::with_dialogs("*=none", None).await else {
        return;
    };
    let c = f.control().await;
    let (ctx, tab) = agent_tab(&c, AGENT).await;
    let mut p = ask(&cid(1), &ctx, AGENT, &[&tab], "x\u{202E}gpj.exe");
    p["details"] = json!([
        { "label": "키\u{2066}", "value": "a\u{200B}b\u{7}c" },
        { "label": "경로", "value": "/a/\u{202E}fdp.exe" },
    ]);
    request(&c, &p).await.unwrap();
    log.wait("confirm_shown", &cid(1), 5).await;
    cancel(&c, &cid(1)).await.unwrap();
    let e = log.wait("confirm", &cid(1), 5).await;
    let lines = common::DialogLog::lines(&e);
    assert!(lines[1].contains("x\\u{202e}gpj.exe"), "{lines:?}");
    assert_eq!(lines[3], "키\\u{2066}");
    assert_eq!(lines[4], "a\\u{200b}b\\u{7}c");
    assert_eq!(lines[6], "/a/\\u{202e}fdp.exe");
    // Of the format characters only the dialog's own marks are left.
    for l in &lines {
        assert!(
            !l.chars().any(|ch| matches!(ch as u32,
                0x200B..=0x200F | 0x202A..=0x202E | 0x2066..=0x2069 | 0x07)),
            "{l:?}"
        );
    }
}

/// Answers go to the owner alone, once: the answer hook presses after the 1 s lock, the
/// owner gets one `confirmationAnswered {confirmId, answer}`, another browser session none,
/// and cancelling it afterwards succeeds (it is gone).
#[tokio::test]
#[ignore]
async fn answers_reach_only_the_owner_once() {
    let Some((f, log)) = Fork::with_dialogs(
        "ap=approve,de=deny,al=allowForSession,no=allowForSession",
        None,
    )
    .await
    else {
        return;
    };
    let other = f.session().await;
    let mut other_events = other.events();
    let c = f.control().await;
    let mut events = c.events();
    let (ctx, tab) = agent_tab(&c, AGENT).await;
    for (n, name, offer, answer) in [
        (1, "ap", true, "approve"),
        (2, "de", true, "deny"),
        (3, "al", true, "allowForSession"),
    ] {
        let id = cid(n);
        let mut p = ask(&id, &ctx, AGENT, &[&tab], name);
        p["offerAllowForSession"] = json!(offer);
        let asked = std::time::Instant::now();
        request(&c, &p).await.unwrap();
        let got = answers_within(&mut events, 4000).await;
        assert_eq!(
            got,
            vec![json!({ "sessionId": null, "params": { "confirmId": id, "answer": answer } })],
            "{name}"
        );
        assert!(
            asked.elapsed() >= Duration::from_millis(1000),
            "{:?}",
            asked.elapsed()
        );
        let e = log.wait("confirm", &id, 5).await;
        assert_eq!(
            (&e["pressed"], &e["by"], &e["closed"]),
            (&json!(answer), &json!("hook"), &json!("answered"))
        );
        cancel(&c, &id).await.unwrap();
    }
    // Not offered: the hook has no such button to press.
    let id = cid(4);
    let mut p = ask(&id, &ctx, AGENT, &[&tab], "no");
    p["offerAllowForSession"] = json!(false);
    request(&c, &p).await.unwrap();
    assert!(answers_within(&mut events, 2500).await.is_empty());
    cancel(&c, &id).await.unwrap();
    assert!(log.wait("confirm", &id, 5).await["pressed"].is_null());
    assert!(answers_within(&mut other_events, 200).await.is_empty());
}

/// The 1 s lock: a press 100 ms after the dialog shows is ignored (and logged), the dialog
/// stays and no answer goes out.
#[tokio::test]
#[ignore]
async fn early_presses_are_ignored() {
    let Some((f, log)) = Fork::with_dialogs("*=approve", Some(100)).await else {
        return;
    };
    let c = f.control().await;
    let mut events = c.events();
    let (ctx, tab) = agent_tab(&c, AGENT).await;
    request(&c, &ask(&cid(1), &ctx, AGENT, &[&tab], "n"))
        .await
        .unwrap();
    assert!(answers_within(&mut events, 2500).await.is_empty());
    cancel(&c, &cid(1)).await.unwrap();
    let e = log.wait("confirm", &cid(1), 5).await;
    assert_eq!(e["ignored"], "locked", "{e}");
    assert!(e["pressed"].is_null(), "{e}");
    assert_eq!(e["closed"], "cancelled");
}

/// Nothing an agent can send answers the dialog: keys (Enter, Space, also with the native key
/// code that takes the browser's own key handling), a click, `Page.handleJavaScriptDialog`,
/// on the agent's tab, after the lock is over, and a forged `confirmationAnswered`. The
/// dialog is browser UI, not part of the page.
#[tokio::test]
#[ignore]
async fn agent_input_cannot_answer_the_dialog() {
    let Some((f, log)) = Fork::with_dialogs("*=none", None).await else {
        return;
    };
    let c = f.control().await;
    let mut events = c.events();
    let (ctx, tab) = agent_tab(&c, AGENT).await;
    let agent = f.session().await;
    let page = agent.attach(&tab).await.unwrap();
    agent
        .send(
            "Page.navigate",
            json!({ "url": "data:text/html,<input id=i autofocus>" }),
            Some(&page),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    request(&c, &ask(&cid(1), &ctx, AGENT, &[&tab], "n"))
        .await
        .unwrap();
    log.wait("confirm_shown", &cid(1), 5).await;
    tokio::time::sleep(Duration::from_millis(1300)).await;
    let key = |kind: &str, key: &str, code: &str, vk: i64, native: Option<i64>| {
        let mut k = json!({ "type": kind, "key": key, "code": code,
                            "windowsVirtualKeyCode": vk });
        if let Some(n) = native {
            k["nativeVirtualKeyCode"] = json!(n);
        }
        if key == "Enter" {
            k["text"] = json!("\r");
        }
        if key == " " {
            k["text"] = json!(" ");
        }
        k
    };
    for native in [None, Some(36)] {
        for (k, code, vk) in [
            ("Enter", "Enter", 13),
            (" ", "Space", 32),
            ("Escape", "Escape", 27),
        ] {
            for kind in ["keyDown", "keyUp"] {
                let n = if k == " " {
                    native.map(|_| 49)
                } else if k == "Escape" {
                    native.map(|_| 53)
                } else {
                    native
                };
                let _ = agent
                    .send(
                        "Input.dispatchKeyEvent",
                        key(kind, k, code, vk, n),
                        Some(&page),
                    )
                    .await;
            }
        }
    }
    for kind in ["mousePressed", "mouseReleased"] {
        let _ = agent
            .send(
                "Input.dispatchMouseEvent",
                json!({ "type": kind, "x": 200, "y": 150, "button": "left", "clickCount": 1 }),
                Some(&page),
            )
            .await;
    }
    let r = agent
        .send(
            "Page.handleJavaScriptDialog",
            json!({ "accept": true }),
            Some(&page),
        )
        .await;
    assert!(r.is_err(), "a JavaScript dialog: {r:?}");
    let r = agent
        .send(
            CONFIRMATION_ANSWERED,
            json!({ "confirmId": cid(1), "answer": "approve" }),
            None,
        )
        .await;
    assert!(r.is_err(), "{r:?}");
    assert!(answers_within(&mut events, 1500).await.is_empty());
    cancel(&c, &cid(1)).await.unwrap();
    let e = log.wait("confirm", &cid(1), 5).await;
    assert!(e["pressed"].is_null(), "{e}");
    assert_eq!(e["closed"], "cancelled");
    assert!(e.get("ignored").is_none(), "{e}");
}

/// Closing the tab the command acts on (here the window's last, so the window closes too)
/// denies, once.
#[tokio::test]
#[ignore]
async fn closing_the_tab_denies_once() {
    let Some((f, log)) = Fork::with_dialogs("*=none", None).await else {
        return;
    };
    let c = f.control().await;
    let mut events = c.events();
    let (ctx, tab) = agent_tab(&c, "PegboardAgent-close").await;
    request(&c, &ask(&cid(1), &ctx, "PegboardAgent-close", &[&tab], "n"))
        .await
        .unwrap();
    log.wait("confirm_shown", &cid(1), 5).await;
    // Every tab of the profile, so its window closes.
    let all = pages_in(&c, &ctx).await;
    c.send("Pegboard.closeTargets", json!({ "targetIds": all }), None)
        .await
        .unwrap();
    let got = answers_within(&mut events, 3000).await;
    assert_eq!(
        got,
        vec![json!({ "sessionId": null, "params": { "confirmId": cid(1), "answer": "deny" } })]
    );
    let e = log.wait("confirm", &cid(1), 5).await;
    assert_eq!(e["pressed"], "deny");
    assert!(
        e["closed"] == "tab_closed" || e["closed"] == "window_closed",
        "{e}"
    );
    cancel(&c, &cid(1)).await.unwrap();
}

/// The owner going (the daemon restarting) closes its dialogs without an event; the new
/// owner hears nothing of them.
#[tokio::test]
#[ignore]
async fn a_daemon_restart_closes_open_dialogs() {
    let Some((f, log)) = Fork::with_dialogs("*=none", None).await else {
        return;
    };
    let c = f.control().await;
    let (ctx, tab) = agent_tab(&c, AGENT).await;
    request(&c, &ask(&cid(1), &ctx, AGENT, &[&tab], "a"))
        .await
        .unwrap();
    request(&c, &ask(&cid(2), &ctx, AGENT, &[&tab], "b"))
        .await
        .unwrap();
    log.wait("confirm_shown", &cid(1), 5).await;
    let next = f.session().await;
    let mut events = next.events();
    c.close();
    let mut claimed = false;
    for _ in 0..50 {
        if next.send("Pegboard.claim", json!({}), None).await.is_ok() {
            claimed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(claimed);
    assert_eq!(
        log.wait("confirm", &cid(1), 5).await["closed"],
        "owner_gone"
    );
    assert!(answers_within(&mut events, 1500).await.is_empty());
    // The queued one was never shown.
    assert!(
        log.of("confirm_shown", &cid(2)).is_empty(),
        "{:?}",
        log.entries()
    );
    // The new owner can ask again with the same ID: the old one is gone.
    request(&next, &ask(&cid(1), &ctx, AGENT, &[&tab], "a"))
        .await
        .unwrap();
    cancel(&next, &cid(1)).await.unwrap();
}

/// The dialog does not take the keyboard focus or activate the app: the key window and
/// whether the app is active are the same before it shows, right after and 200 ms later.
/// With the app active and the person's window key, that window stays key; with another app
/// in front, Pegboard stays in the background and its Dock icon bounces once (`attention`).
#[tokio::test]
#[ignore]
async fn the_dialog_does_not_take_focus() {
    let Some((f, log)) = Fork::with_dialogs("*=none", None).await else {
        return;
    };
    let c = f.control().await;
    let default = profile(&c, "Default").await.expect("no Default profile")["browserContextId"]
        .as_str()
        .unwrap()
        .to_string();
    let (ctx, tab) = agent_tab(&c, AGENT).await;
    let osascript = |script: String| async move {
        let _ = tokio::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .status()
            .await;
    };
    for (n, front) in [(1, true), (2, false)] {
        if front {
            // The person's window to the front and key, the app active.
            common::activate_any_target_in(&c, &default).await;
        } else {
            // Another app in front.
            osascript("tell application \"Finder\" to activate".to_string()).await;
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;
        let id = cid(n);
        request(&c, &ask(&id, &ctx, AGENT, &[&tab], "n"))
            .await
            .unwrap();
        log.wait("confirm_shown", &id, 5).await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        cancel(&c, &id).await.unwrap();
        let e = log.wait("confirm", &id, 5).await;
        eprintln!("front={front}: {e}");
        let want = if front {
            json!(["Default", "Default", "Default", true, true, true, false])
        } else {
            json!(["none", "none", "none", false, false, false, true])
        };
        assert_eq!(
            json!([
                e["keyWindowProfileBefore"],
                e["keyWindowProfileAt0"],
                e["keyWindowProfileAt200ms"],
                e["appActiveBefore"],
                e["appActiveAt0"],
                e["appActiveAt200ms"],
                e["attention"],
            ]),
            want,
            "front={front}: {e}"
        );
    }
}

/// The session proxy refuses the confirmation methods (and a forged answer) to an agent, on
/// its browser session and on its page session; the daemon's dialog stays up.
#[tokio::test]
#[ignore]
async fn proxy_refuses_pegboard_confirmation_methods() {
    std::env::set_var("PEGBOARD_TEST_HOOKS", "1");
    if test_fork_binary().is_none() {
        eprintln!("skipped: needs PEGBOARD_TEST_BROWSER=fork:<Pegboard.app>");
        return;
    }
    let log = common::DialogLog::new();
    let fx = {
        let _vars = [
            common::EnvVar::set("PEGBOARD_TEST_DIALOG_LOG", log.path.to_str().unwrap()),
            common::EnvVar::set("PEGBOARD_TEST_DIALOG_ANSWER", "*=none"),
        ];
        common::session_fixture().await
    };
    let owner = &fx.browser.conn;
    let id = cid(1);
    let dir = fx
        .browser
        .agents
        .profile_of(&fx.agent_context)
        .expect("the agent context's profile");
    request(
        owner,
        &ask(&id, &fx.agent_context, &dir, &[&fx.first_target], "n"),
    )
    .await
    .unwrap();
    log.wait("confirm_shown", &id, 5).await;
    let client = common::client(&fx.endpoint).await;
    let page = client.attach(&fx.first_target).await.unwrap();
    let calls = [
        (
            "Pegboard.requestConfirmation",
            ask(&cid(2), &fx.agent_context, &dir, &[], "n"),
        ),
        ("Pegboard.cancelConfirmation", json!({ "confirmId": id })),
        (
            CONFIRMATION_ANSWERED,
            json!({ "confirmId": id, "answer": "approve" }),
        ),
    ];
    for (method, params) in &calls {
        for session in [None, Some(page.as_str())] {
            let r = client.send(method, params.clone(), session).await;
            assert_eq!(code(&r), -32601, "{method} ({session:?}): {}", message(&r));
            assert!(
                message(&r).contains("not allowed in a pegboard session"),
                "{method} ({session:?}): {}",
                message(&r)
            );
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(log.of("confirm", &id).is_empty(), "{:?}", log.entries());
    assert!(log.of("confirm_shown", &cid(2)).is_empty());
    cancel(owner, &id).await.unwrap();
    assert_eq!(log.wait("confirm", &id, 5).await["closed"], "cancelled");
    fx.shutdown().await;
}
