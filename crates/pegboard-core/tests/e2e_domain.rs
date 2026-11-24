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
    // A name apart from case of a known profile is the same directory on macOS.
    let r = open_profile_window(&c, "PegboardAgent-T1").await;
    assert_eq!(code(&r), -32000, "PegboardAgent-T1: {}", message(&r));
    for dir in [
        "Profile 1",
        "PegboardAgentX",
        "pegboardagent",
        "evil",
        "PegboardAgent-..",
        "PegboardAgent-a",
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
    let made = create_group(&c, &ctx).await.unwrap();
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

    let mut events = c.events();
    for t in [&t2, &t1] {
        let before = common_ms();
        c.send("Target.activateTarget", json!({ "targetId": t }), None)
            .await
            .unwrap();
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

/// `Target.exposeDevToolsProtocol {inheritPermissions: true}` from the pipe gives a page a
/// browser session (`window.cdp`). Neither it nor a browser session it opens has the domain,
/// before or after the pipe's claim. (The proxy refuses the method to agents anyway:
/// `GUARD_METHODS`.)
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
    c.send(
        "Target.exposeDevToolsProtocol",
        json!({ "targetId": target, "inheritPermissions": true }),
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
        if out["attach"]["result"]["sessionId"].is_string() {
            assert_eq!(out["childClaim"]["error"]["code"], -32601, "{when}: {out}");
        }
        if when == "before the claim" {
            c.send("Pegboard.claim", json!({}), None).await.unwrap();
        }
    }
}
