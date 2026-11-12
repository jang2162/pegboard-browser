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
    assert_eq!(code, 1, "switched to the human tab: {out}");
    assert!(!err.is_empty());
    let (code, _, _) = rig.ab(&rig.a, &["tab", "close", &rig.human.target]);
    assert_eq!(code, 1, "closed the human tab");
    // Positive control: the same command shape works for the session's own tab.
    let own = rig.tabs(&rig.a)[0].0.clone();
    rig.ok(&rig.a, &["tab", &own]);
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
    let human_tabs_before: Vec<String> = rig
        .human
        .human_pages()
        .into_iter()
        .map(|t| t.target_id)
        .collect();
    assert_eq!(human_tabs_before, vec![rig.human.target.clone()]);
    let context_of = |id: &str| -> String {
        rig.human
            .pages()
            .into_iter()
            .find(|t| t.target_id == id)
            .and_then(|t| t.browser_context_id)
            .unwrap_or_else(|| panic!("target {id} not found"))
    };
    let ids = |h: &str| -> Vec<String> { rig.tabs(h).into_iter().map(|t| t.0).collect() };
    let (a_ctx, b_ctx, c_ctx) = (
        context_of(&rig.tabs(&rig.a)[0].0),
        context_of(&rig.tabs(&rig.b)[0].0),
        context_of(&rig.tabs(&rig.c)[0].0),
    );
    assert_eq!(a_ctx, b_ctx, "A and B share the default agent profile");
    assert_ne!(a_ctx, c_ctx, "C is in the work profile");
    assert!(a_ctx != rig.human.context && c_ctx != rig.human.context);

    // Each agent-side tab creation is preceded by re-establishing "the human window was the
    // last active one": a context-less Target.createTarget sent to the browser lands in the
    // human profile. When that control cannot be made to hold (it does not while another
    // application has the OS focus) the test still checks every placement and says so on
    // stderr; manual check M8 covers the real "human clicked their window" case.
    let count_in = |c: &str| {
        rig.human
            .pages()
            .iter()
            .filter(|t| t.browser_context_id.as_deref() == Some(c))
            .count()
    };
    let (default_before, work_before) = (count_in(&a_ctx), count_in(&c_ctx));
    let mut created = Vec::new();
    let mut premise_held = 0;
    for (handle, ctx, path) in [
        (&rig.a, &a_ctx, "/counter"),
        (&rig.b, &b_ctx, "/counter"),
        (&rig.c, &c_ctx, "/form"),
        (&rig.a, &a_ctx, "/form"),
    ] {
        let control = rig.human.establish_human_active(&rig.human.target);
        if control == rig.human.context {
            premise_held += 1;
        } else {
            eprintln!(
                "PREMISE NOT ESTABLISHED: after activating the human tab a context-less createTarget landed \
                 in {control}, not the human profile; the human window is not the browser's last active one \
                 (see manual check M8)"
            );
        }
        let before = ids(handle);
        rig.ok(handle, &["tab", "new", &rig.url(path)]);
        let new: Vec<String> = ids(handle)
            .into_iter()
            .filter(|i| !before.contains(i))
            .collect();
        assert_eq!(
            new.len(),
            1,
            "{handle}: expected exactly one new tab, got {new:?}"
        );
        assert_eq!(
            &context_of(&new[0]),
            ctx,
            "the new tab is not in the session's profile"
        );
        created.push(new[0].clone());
    }

    // Per context: the human context holds exactly its original tab; the default agent context
    // A's and B's tabs; the work context only C's.
    let pages = rig.human.pages();
    let in_ctx = |c: &str| -> Vec<String> {
        pages
            .iter()
            .filter(|t| t.browser_context_id.as_deref() == Some(c))
            .map(|t| t.target_id.clone())
            .collect()
    };
    assert_eq!(
        in_ctx(&rig.human.context),
        vec![rig.human.target.clone()],
        "a tab appeared in the human profile"
    );
    let default_ctx = in_ctx(&a_ctx);
    let work_ctx = in_ctx(&c_ctx);
    assert_eq!(
        default_ctx.len(),
        default_before + 3,
        "3 new default-profile tabs: {default_ctx:?}"
    );
    assert_eq!(
        work_ctx.len(),
        work_before + 1,
        "1 new work-profile tab: {work_ctx:?}"
    );
    for id in &created {
        assert!(default_ctx.contains(id) || work_ctx.contains(id), "{id}");
    }
    assert!(work_ctx.contains(&created[2]));
    eprintln!("premise (human window last active) held for {premise_held} of 4 tab creations");
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
    // A really holds cookies, so C's emptiness is not an empty fixture.
    let a_cookie = rig.ok(&rig.a, &["eval", "document.cookie"]);
    let a_cookie = json(&a_cookie)["result"].as_str().unwrap().to_string();
    assert!(
        a_cookie.contains("spike=1") && a_cookie.contains("agentck=1"),
        "{a_cookie}"
    );
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
    let counter = rig.url("/counter");
    // Positive controls: the same shapes without the offending word are accepted, so the
    // refusals below are about the word and not about the shape.
    rig.ok(&rig.a, &["open", &counter]);
    rig.ok(&rig.a, &["snapshot", "-i"]);
    rig.ok(&rig.a, &["tab", "new", &counter]);
    rig.ok(&rig.a, &["tab", "close"]);
    let cases: Vec<Vec<&str>> = vec![
        vec!["snapshot", "--cdp", &port],
        vec!["snapshot", "--auto-connect"],
        vec!["connect", &port],
        vec!["open", &counter, "--profile", "Default"],
        vec!["close"],
        vec!["stream", "enable"],
        // Another session named inside the forwarded command.
        vec!["snapshot", "--session", &b_handle],
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
        assert!(
            err.contains("argv_rejected"),
            "{args:?}: not refused by the filter: {err:?}"
        );
    }
    // Rejected by the CLI's own parser, before the filter (so no `argv_rejected`): a global
    // flag in front of the command, and a second `--session`.
    for args in [
        vec!["--session", rig.a.as_str(), "--auto-connect", "snapshot"],
        vec![
            "--session",
            rig.a.as_str(),
            "--session",
            b_handle.as_str(),
            "snapshot",
        ],
    ] {
        let (code, out, err) = rig.home.run_limited(&[], &args, 90);
        assert_eq!(code, 2, "{args:?}: out={out:?} err={err:?}");
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
fn agent_actions_do_not_change_the_humans_active_tab() {
    // Proves: agent actions (open, click, fill, new tab, in two sessions) do not change which
    // tab is the active one of the human window, and do not change `document.hasFocus()` of
    // that tab. The active tab is told from `visibilityState`: exactly one of the two human
    // tabs must read "visible" (the one just activated) and the other "hidden". If the window
    // is covered so that both read "hidden", the test fails with "cannot determine" instead of
    // passing vacuously. Does NOT prove operating-system focus (the frontmost application);
    // that is manual check M8b in docs/phase1-manual-checks.md.
    let rig = Rig::new();
    let second = rig.human.open_tab(&rig.url("/counter"));
    rig.human.activate(&second);
    std::thread::sleep(Duration::from_millis(800));

    let observe = |rig: &Rig| -> (String, Value) {
        let pages = rig.human.human_pages();
        assert_eq!(
            pages.len(),
            2,
            "tabs were added to or removed from the human profile"
        );
        let visible: Vec<String> = pages
            .iter()
            .filter(|t| rig.human.evaluate(&t.target_id, "document.visibilityState") == "visible")
            .map(|t| t.target_id.clone())
            .collect();
        assert_eq!(
            visible.len(),
            1,
            "cannot determine the human window's active tab (is the window covered?): visible = {visible:?}"
        );
        let focus = rig.human.evaluate(&visible[0], "document.hasFocus()");
        (visible[0].clone(), focus)
    };
    let before = observe(&rig);
    assert_eq!(
        before.0, second,
        "the tab just activated is not the active one"
    );
    println!("human active tab and hasFocus before: {before:?}");

    rig.ok(&rig.a, &["open", &rig.url("/counter")]);
    rig.ok(&rig.a, &["click", "#btn"]);
    rig.ok(&rig.a, &["open", &rig.url("/form")]);
    rig.ok(&rig.a, &["fill", "#name", "agent"]);
    rig.ok(&rig.a, &["tab", "new", &rig.url("/counter")]);
    rig.ok(&rig.a, &["click", "#btn"]);
    rig.ok(&rig.b, &["open", &rig.url("/counter")]);
    rig.ok(&rig.b, &["tab", "new", &rig.url("/form")]);
    std::thread::sleep(Duration::from_millis(500));

    let after = observe(&rig);
    println!("human active tab and hasFocus after:  {after:?}");
    assert_eq!(before.0, after.0, "the human window's active tab changed");
    assert_eq!(
        before.1, after.1,
        "document.hasFocus() of the human's active tab changed"
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
    assert_eq!(code, 1, "A switched to B's tab: {out}");
    assert!(!err.is_empty());
    let (code, out, _) = rig.ab(&rig.a, &["tab", "close", &b_tab]);
    assert_eq!(code, 1, "A closed B's tab: {out}");
    // Positive control: the same command shape works for A's own tab.
    let a_own = rig.tabs(&rig.a)[0].0.clone();
    rig.ok(&rig.a, &["tab", &a_own]);

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
    // The page's own inline script sets `window.pageMarker`; only the main world sees a
    // page-defined global, an isolated world (even a persistent one) would not. `a11y` is the
    // command that creates a universal-access world, and it must keep working.
    let rig = Rig::new();
    rig.ok(&rig.a, &["open", &rig.url("/marker")]);
    let out = rig.ok(&rig.a, &["eval", "window.pageMarker"]);
    assert_eq!(json(&out)["result"], "main", "{out}");
    rig.ok(&rig.a, &["eval", "window.pegboardMarker = 42"]);
    let out = rig.ok(&rig.a, &["eval", "window.pegboardMarker"]);
    assert_eq!(json(&out)["result"], 42, "{out}");
    rig.ok(&rig.a, &["a11y"]);
}
