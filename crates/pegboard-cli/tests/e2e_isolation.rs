//! Isolation suite: attacks pegboard through the `pegboard` CLI only, doing what an agent
//! holding a session handle can do, and checks that it reaches neither the human profile
//! nor another session's tabs. It is the standing proof of two of the project's failure
//! conditions and is kept for every later phase. (The third, secrets in the action log, is
//! covered by the action log tests in pegboard-core.)
//!
//! The "human" is played by `common::Human`, which attaches to the browser directly through
//! the `DevToolsActivePort` file in the test's own home: test-only knowledge, the CLI never
//! exposes that port.
//!
//! Proxy-level counterpart of the isolated-world test: `universal_access_isolated_world_cannot_escape`
//! in crates/pegboard-core/tests/e2e_isolated_world.rs.
//!
//! All tests start a browser: `cargo test -p pegboard-cli --test e2e_isolation -- --ignored --test-threads=1`.

mod common;

use std::time::Duration;

use common::*;
use serde_json::Value;

/// A running browser with a human tab on `/cookie/set` and three sessions: A and B in the
/// default profile, C in the profile `work`.
struct Rig {
    // Dropped first: the human's connection goes before the browser does.
    human: Human,
    origin: String,
    a: String,
    b: String,
    c: String,
    home: Home,
}

impl Rig {
    fn new() -> Rig {
        let origin = fixture();
        let home = Home::new();
        let (code, _, err) = home.run(&["browser", "start"]);
        assert_eq!(code, 0, "{err}");
        let human = Human::attach(&home);
        human.navigate(&human.target, &format!("{origin}/cookie/set"));
        // The human really holds the data the agents must not see.
        assert_eq!(human.evaluate(&human.target, "document.cookie"), "spike=1");

        let create = |args: &[&str]| {
            let (code, out, err) = home.run(args);
            assert_eq!(code, 0, "{err}");
            session_handle(&out)
        };
        let a = create(&["session", "create", "--name", "a"]);
        let b = create(&["session", "create", "--name", "b"]);
        let (code, _, err) = home.run(&["profile", "create", "work"]);
        assert_eq!(code, 0, "{err}");
        let c = create(&["session", "create", "--name", "c", "--profile", "work"]);
        Rig {
            human,
            origin,
            a,
            b,
            c,
            home,
        }
    }

    /// Runs an agent-browser command as `handle`, the way the README says (environment).
    fn ab(&self, handle: &str, args: &[&str]) -> (i32, String, String) {
        self.home
            .run_limited(&[("PEGBOARD_SESSION", handle)], args, 90)
    }

    fn ok(&self, handle: &str, args: &[&str]) -> String {
        let (code, out, err) = self.ab(handle, args);
        assert_eq!(code, 0, "{args:?}: {err}");
        out
    }

    /// (targetId, url) of the session's tabs as `tab list` shows them.
    fn tabs(&self, handle: &str) -> Vec<(String, String)> {
        let out = self.ok(handle, &["tab", "list"]);
        json(&out)["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t["targetId"].as_str().unwrap().to_string(),
                    t["url"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    fn page_state(&self, handle: &str) -> String {
        let out = self.ok(handle, &["get", "text", "#out"]);
        json(&out)["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text in {out}"))
            .to_string()
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }
}

#[test]
#[ignore]
fn agent_cannot_list_or_switch_to_human_tabs() {
    let rig = Rig::new();
    assert!(rig
        .human
        .human_pages()
        .iter()
        .any(|t| t.url.contains("/cookie/set")));

    rig.ok(&rig.a, &["open", &rig.url("/counter")]);
    let tabs = rig.tabs(&rig.a);
    assert!(!tabs.is_empty());
    for (id, url) in &tabs {
        assert!(
            !url.contains("/cookie/set"),
            "the human's tab is listed: {url}"
        );
        assert_ne!(id, &rig.human.target, "the human's target is listed");
    }

    // Switching to or closing the human's tab by its target id fails, and changes nothing.
    let (code, out, err) = rig.ab(&rig.a, &["tab", &rig.human.target]);
    assert_ne!(code, 0, "switched to the human tab: {out}");
    assert!(!err.is_empty());
    let (code, _, _) = rig.ab(&rig.a, &["tab", "close", &rig.human.target]);
    assert_ne!(code, 0, "closed the human tab");
    let still: Vec<_> = rig
        .human
        .human_pages()
        .into_iter()
        .map(|t| t.target_id)
        .collect();
    assert_eq!(
        still,
        vec![rig.human.target.clone()],
        "the human's tabs changed"
    );

    // The session still works on its own tab.
    let url = rig.ok(&rig.a, &["get", "url"]);
    assert!(
        url.contains("/counter") && !url.contains("/cookie/set"),
        "{url}"
    );
}

#[test]
#[ignore]
fn agent_cannot_read_human_cookies_or_storage() {
    let rig = Rig::new();
    rig.ok(&rig.a, &["open", &rig.url("/cookie/get")]);
    let seen = rig.page_state(&rig.a);
    assert!(seen.contains(r#""cookie":"""#), "{seen}");
    assert!(seen.contains(r#""ls":null"#), "{seen}");

    let cookies = rig.ok(&rig.a, &["cookies"]);
    assert!(!cookies.contains("spike"), "{cookies}");
    assert_eq!(
        json(&cookies)["cookies"].as_array().unwrap().len(),
        0,
        "{cookies}"
    );

    let out = rig.ok(&rig.a, &["eval", "document.cookie"]);
    assert_eq!(json(&out)["result"], "", "{out}");
    let out = rig.ok(&rig.a, &["eval", "localStorage.getItem('spike')"]);
    assert!(json(&out)["result"].is_null(), "{out}");

    // The human's data is untouched.
    assert_eq!(
        rig.human.evaluate(&rig.human.target, "document.cookie"),
        "spike=1"
    );
}

#[test]
#[ignore]
fn new_tabs_always_open_in_the_agent_profile() {
    let rig = Rig::new();
    // The human window is in front when the agent opens tabs: nothing may land in it.
    rig.human.activate(&rig.human.target);
    rig.ok(&rig.a, &["tab", "new", &rig.url("/counter")]);
    rig.ok(&rig.a, &["open", &rig.url("/form")]);
    rig.ok(&rig.b, &["tab", "new", &rig.url("/counter")]);
    rig.ok(&rig.c, &["tab", "new", &rig.url("/form")]);

    let human: Vec<_> = rig
        .human
        .human_pages()
        .into_iter()
        .map(|t| t.target_id)
        .collect();
    assert_eq!(
        human,
        vec![rig.human.target.clone()],
        "a tab appeared in the human profile"
    );
    let agent = rig.human.agent_pages();
    // Three sessions with their first tab each, plus the new tabs.
    assert!(agent.len() >= 6, "{agent:?}");
    for t in &agent {
        assert_ne!(
            t.browser_context_id.as_deref(),
            Some(rig.human.context.as_str())
        );
    }
    for (handle, extra) in [(&rig.a, "/form"), (&rig.b, "/counter"), (&rig.c, "/form")] {
        assert!(
            rig.tabs(handle).iter().any(|(_, u)| u.ends_with(extra)),
            "{handle}: its new tab is missing"
        );
    }
}

#[test]
#[ignore]
fn sessions_in_one_profile_share_cookies_but_not_tabs() {
    let rig = Rig::new();
    rig.ok(&rig.a, &["open", &rig.url("/counter")]);
    // The agent's own cookie, so it cannot be mistaken for the human's `spike=1`.
    rig.ok(&rig.a, &["eval", "document.cookie = 'agentck=1; path=/'"]);
    rig.ok(&rig.b, &["open", &rig.url("/cookie/get")]);
    let seen = rig.page_state(&rig.b);
    assert!(
        seen.contains("agentck=1"),
        "same profile shares cookies by design: {seen}"
    );
    assert!(!seen.contains("spike"), "{seen}");

    let a_ids: Vec<String> = rig.tabs(&rig.a).into_iter().map(|t| t.0).collect();
    let b_ids: Vec<String> = rig.tabs(&rig.b).into_iter().map(|t| t.0).collect();
    assert!(!a_ids.is_empty() && !b_ids.is_empty());
    for id in &a_ids {
        assert!(!b_ids.contains(id), "B lists A's tab {id}");
    }
}

#[test]
#[ignore]
fn sessions_in_different_profiles_share_nothing() {
    let rig = Rig::new();
    rig.ok(&rig.a, &["open", &rig.url("/cookie/set")]);
    rig.ok(&rig.a, &["eval", "document.cookie = 'agentck=1; path=/'"]);
    rig.ok(&rig.c, &["open", &rig.url("/cookie/get")]);
    let seen = rig.page_state(&rig.c);
    assert!(seen.contains(r#""cookie":"""#), "{seen}");
    assert!(seen.contains(r#""ls":null"#), "{seen}");
    let cookies = rig.ok(&rig.c, &["cookies"]);
    assert_eq!(
        json(&cookies)["cookies"].as_array().unwrap().len(),
        0,
        "{cookies}"
    );
}

#[test]
#[ignore]
fn escape_flags_and_commands_are_refused() {
    let rig = Rig::new();
    let port = std::fs::read_to_string(rig.home.path().join("chrome/DevToolsActivePort"))
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    let b_handle = rig.b.clone();
    let cases: Vec<Vec<&str>> = vec![
        vec!["snapshot", "--cdp", &port],
        vec!["snapshot", "--auto-connect"],
        vec!["--auto-connect", "snapshot"],
        vec!["connect", &port],
        vec!["open", "x", "--profile", "Default"],
        vec!["close"],
        vec!["stream", "enable"],
        // Another session named inside the forwarded command.
        vec!["snapshot", "--session", &b_handle],
        vec!["--session", &b_handle, "snapshot"],
        // `--` does not smuggle flags past the filter.
        vec!["snapshot", "--", "--cdp", &port],
        // Schemes that reach local files or browser internals.
        vec!["open", "file:///etc/hosts"],
        vec!["open", "chrome://settings"],
        vec!["tab", "new", "file:///etc/hosts"],
        // Commands outside the phase 1 allow-list.
        vec!["upload", "#f", "x.txt"],
        vec!["inspect"],
        vec!["read"],
        vec!["set", "credentials", "a", "b"],
    ];
    for args in &cases {
        // `--session` (not the environment): `pegboard session list` itself is a pegboard
        // command, which only an `--session`-qualified line forwards to agent-browser.
        let mut argv = vec!["--session", rig.a.as_str()];
        argv.extend(args.iter().copied());
        let (code, out, err) = rig.home.run_limited(&[], &argv, 90);
        assert_eq!(
            code, 2,
            "{args:?} was not refused with exit 2: out={out:?} err={err:?}"
        );
    }
    // `fill` honours `--`: what follows is literal text for the field, not a flag.
    rig.ok(&rig.a, &["open", &rig.url("/form")]);
    rig.ok(&rig.a, &["fill", "#name", "--", "--cdp", &port]);
    let out = rig.ok(&rig.a, &["eval", "document.getElementById('name').value"]);
    assert_eq!(json(&out)["result"], format!("--cdp {port}"), "{out}");
    // `session list` is a pegboard command, not an escape: it answers with ids and names only,
    // never a token, so it gives no way into another session (see "알려진 한계" in the README).
    for args in [&["session", "list"][..], &["session", "list", "--json"][..]] {
        let (code, out, err) = rig.home.run(args);
        assert_eq!(code, 0, "{err}");
        for h in [&rig.a, &rig.b, &rig.c] {
            assert!(out.contains(id_of(h)), "{out}");
            assert!(!out.contains(token_of(h)), "a token is listed: {out}");
        }
    }
    // `eval` is allowed (it runs in the agent's page); it is deliberately not in the list.
    rig.ok(&rig.a, &["open", &rig.url("/counter")]);
    rig.ok(&rig.a, &["eval", "1 + 1"]);

    // Nothing leaked from the refused attempts: A is intact and still alone in its tabs.
    rig.ok(&rig.a, &["snapshot"]);
    assert_eq!(rig.tabs(&rig.a).len(), 1);
    let human: Vec<_> = rig
        .human
        .human_pages()
        .into_iter()
        .map(|t| t.target_id)
        .collect();
    assert_eq!(human, vec![rig.human.target.clone()]);
}

#[test]
#[ignore]
fn agent_does_not_steal_focus() {
    // What this can prove: the browser's own state of the human window does not change when an
    // agent acts: which of the human's tabs is the active one (`visibilityState` of each) stays
    // the same. What it cannot prove: operating-system focus. `document.hasFocus()` and the
    // visibility of a window that another application covers depend on what the person at the
    // machine is doing, so they are printed but not asserted. If the window is covered the whole
    // time, every human tab reads "hidden" before and after, and the assertion holds trivially.
    let rig = Rig::new();
    let second = rig.human.open_tab(&rig.url("/counter"));
    rig.human.activate(&second);
    std::thread::sleep(Duration::from_millis(800));

    let snapshot = |rig: &Rig| -> Vec<(String, Value, Value)> {
        let mut pages = rig.human.human_pages();
        pages.sort_by(|x, y| x.target_id.cmp(&y.target_id));
        pages
            .iter()
            .map(|t| {
                (
                    t.target_id.clone(),
                    rig.human.evaluate(&t.target_id, "document.visibilityState"),
                    rig.human.evaluate(&t.target_id, "document.hasFocus()"),
                )
            })
            .collect()
    };
    let before = snapshot(&rig);
    println!("human tabs before (id, visibilityState, hasFocus): {before:?}");
    assert_eq!(before.len(), 2);

    rig.ok(&rig.a, &["open", &rig.url("/counter")]);
    rig.ok(&rig.a, &["click", "#btn"]);
    rig.ok(&rig.a, &["open", &rig.url("/form")]);
    rig.ok(&rig.a, &["fill", "#name", "agent"]);
    rig.ok(&rig.a, &["tab", "new", &rig.url("/counter")]);
    rig.ok(&rig.a, &["click", "#btn"]);
    rig.ok(&rig.b, &["open", &rig.url("/counter")]);
    std::thread::sleep(Duration::from_millis(500));

    let after = snapshot(&rig);
    println!("human tabs after  (id, visibilityState, hasFocus): {after:?}");
    let states = |v: &Vec<(String, Value, Value)>| -> Vec<(String, Value)> {
        v.iter()
            .map(|(id, vis, _)| (id.clone(), vis.clone()))
            .collect()
    };
    assert_eq!(
        states(&before),
        states(&after),
        "the human window's active tab changed"
    );
    let human: Vec<_> = rig
        .human
        .human_pages()
        .into_iter()
        .map(|t| t.target_id)
        .collect();
    assert_eq!(
        human.len(),
        2,
        "tabs were added to or removed from the human profile"
    );
}

#[test]
#[ignore]
fn stream_port_is_closed_for_every_session() {
    let rig = Rig::new();
    let ab_dir = pegboard_core::paths::paths_in(rig.home.path()).ab_socket_dir;
    // Make sure every session's agent-browser daemon is running.
    for h in [&rig.a, &rig.b, &rig.c] {
        rig.ok(h, &["open", &rig.url("/counter")]);
    }
    let mut pids = Vec::new();
    for h in [&rig.a, &rig.b, &rig.c] {
        let file = ab_dir.join(format!("peg-{}.pid", id_of(h)));
        let pid: i32 = std::fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("{}: {e}", file.display()))
            .trim()
            .parse()
            .unwrap();
        assert!(
            alive(pid),
            "agent-browser daemon {pid} of {h} is not running"
        );
        pids.push(pid.to_string());
    }
    let listening = |pids: &str| -> String {
        let out = std::process::Command::new("lsof")
            .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-a", "-p", pids])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    // The check can see listeners: the browser's own debugging port is one.
    let (_, status, _) = rig.home.run(&["browser", "status", "--json"]);
    let chrome = json(&status)["pid"].as_i64().unwrap().to_string();
    assert!(
        listening(&chrome).contains("LISTEN"),
        "lsof does not see the browser's port"
    );

    let out = listening(&pids.join(","));
    assert!(
        out.trim().is_empty(),
        "an agent-browser daemon listens on TCP:\n{out}"
    );
    // The streaming command is not available either.
    assert_eq!(rig.ab(&rig.a, &["stream", "enable"]).0, 2);
}

#[test]
#[ignore]
fn session_cannot_use_another_sessions_handle() {
    let rig = Rig::new();
    rig.ok(&rig.b, &["open", &rig.url("/counter")]);
    let b_tab = rig.tabs(&rig.b)[0].0.clone();
    let swapped = format!("{}.{}", id_of(&rig.a), token_of(&rig.b)); // A's id, B's token
    let swapped_ba = format!("{}.{}", id_of(&rig.b), token_of(&rig.a)); // B's id, A's token
    let mut near_miss = token_of(&rig.b).to_string();
    let last = near_miss.pop().unwrap();
    near_miss.push(if last == 'a' { 'b' } else { 'a' });
    let near_miss = format!("{}.{near_miss}", id_of(&rig.b)); // B's id, B's token with one char changed

    for wrong in [&swapped, &swapped_ba, &near_miss] {
        let (code, out, err) = rig.home.run(&["--session", wrong, "tab", "list"]);
        assert_eq!(code, 1, "{wrong}: {err}");
        assert!(err.contains("session_not_found"), "{err}");
        assert!(!out.contains(&b_tab), "{out}");
        let (code, out, err) = rig.home.run(&["session", "info", wrong, "--json"]);
        assert_eq!(code, 1, "{wrong}: {err}");
        assert!(
            err.contains("session_not_found") && out.trim().is_empty(),
            "{out} {err}"
        );
        let (code, _, err) = rig.home.run(&["session", "close", wrong]);
        assert_eq!(code, 1, "{wrong}: {err}");
        assert!(err.contains("session_not_found"), "{err}");
    }
    // An id without a token: `--session` refuses it on the client side (usage error, exit 2);
    // `session info` and `session close` leave it to the daemon, which answers like for a
    // wrong token (exit 1, session_not_found).
    for bare in [id_of(&rig.a), id_of(&rig.b)] {
        let (code, _, err) = rig.home.run(&["--session", bare, "tab", "list"]);
        assert_eq!(code, 2, "{bare}: {err}");
        for sub in ["info", "close"] {
            let (code, _, err) = rig.home.run(&["session", sub, bare]);
            assert_eq!(code, 1, "{sub} {bare}: {err}");
            assert!(err.contains("session_not_found"), "{err}");
        }
    }

    // Both sessions are still there and still work with their own handles.
    let (_, out, _) = rig.home.run(&["session", "list", "--json"]);
    let states: Vec<_> = json(&out)
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["id"].as_str().unwrap().to_string(),
                s["state"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    for h in [&rig.a, &rig.b, &rig.c] {
        assert!(
            states.contains(&(id_of(h).to_string(), "active".to_string())),
            "{states:?}"
        );
    }
    assert!(rig.tabs(&rig.b).iter().any(|(id, _)| id == &b_tab));
    rig.ok(&rig.a, &["snapshot"]);
}

#[test]
#[ignore]
fn agent_cannot_reach_other_sessions_tabs_by_id() {
    let rig = Rig::new();
    rig.ok(&rig.b, &["open", &rig.url("/counter")]);
    let b_tab = rig.tabs(&rig.b)[0].0.clone();
    let before = rig.human.agent_pages().len();

    let (code, out, err) = rig.ab(&rig.a, &["tab", &b_tab]);
    assert_ne!(code, 0, "A switched to B's tab: {out}");
    assert!(!err.is_empty());
    let (code, out, _) = rig.ab(&rig.a, &["tab", "close", &b_tab]);
    assert_ne!(code, 0, "A closed B's tab: {out}");

    // B's tab is still there, in B's session and in the browser.
    assert!(rig.tabs(&rig.b).iter().any(|(id, _)| id == &b_tab));
    assert!(rig.human.agent_pages().iter().any(|t| t.target_id == b_tab));
    assert_eq!(rig.human.agent_pages().len(), before);
    // And A still sees none of B's tabs.
    assert!(rig.tabs(&rig.a).iter().all(|(id, _)| id != &b_tab));
}

#[test]
#[ignore]
fn eval_runs_in_the_main_world_and_a11y_works() {
    // Evidence for the comment of `universal_access_isolated_world_cannot_escape` in
    // pegboard-core: a caller of the CLI cannot run its own script in an isolated world.
    // `eval` shares state with the page's own scripts, so it runs in the main world; `a11y`
    // is the command that creates a universal-access world, and it must keep working.
    let rig = Rig::new();
    rig.ok(&rig.a, &["open", &rig.url("/counter")]);
    rig.ok(&rig.a, &["eval", "window.pegboardMarker = 42"]);
    let out = rig.ok(&rig.a, &["eval", "window.pegboardMarker"]);
    assert_eq!(json(&out)["result"], 42, "{out}");
    let out = rig.ok(&rig.a, &["eval", "typeof document.getElementById('btn')"]);
    assert_eq!(json(&out)["result"], "object", "{out}");
    rig.ok(&rig.a, &["a11y"]);
}
