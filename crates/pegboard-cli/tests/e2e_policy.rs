//! End-to-end tests of the session policy. Tests that start a browser are `#[ignore]`d: run
//! them with `cargo test -p pegboard-cli --test e2e_policy -- --ignored --test-threads=1`.
//!
//! They share the binary's one daemon and browser (`shared_home()`); each opens its own
//! sessions and closes them. Tests about a profile's pinned settings use a profile of their
//! own, so they cannot meet the sessions of another test.

mod common;

use common::*;
use serde_json::json;

fn create(args: &[&str]) -> String {
    let home = shared_home();
    let mut full = vec!["session", "create"];
    full.extend_from_slice(args);
    let (code, out, err) = home.run(&full);
    assert_eq!(code, 0, "{err}");
    session_handle(&out)
}

fn close(handle: &str) {
    assert_eq!(shared_home().run(&["session", "close", handle]).0, 0);
}

#[test]
#[ignore]
fn state_commands_need_allow_state() {
    let origin = fixture();
    let home = shared_home();
    let url = format!("{origin}/counter");
    let plain = create(&["--url", &url]);
    let (code, _, err) = home.run(&["--session", &plain, "cookies", "set", "pegpolicyck", "1"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("policy_denied"), "{err}");
    assert!(err.contains("--allow state"), "{err}");
    let (code, _, err) = home.run(&["--session", &plain, "storage", "local", "set", "k", "v"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("policy_denied"), "{err}");
    // Reading is not state.
    let (code, _, err) = home.run(&["--session", &plain, "cookies"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = home.run(&["--session", &plain, "storage", "local", "get", "k"]);
    assert_eq!(code, 0, "{err}");

    // How the denial is recorded: the command name, every later word hidden, only the code.
    let rid = format!("r_{}", &id_of(&plain)[2..]);
    let (code, out, _) = home.run(&["recording", "show", &rid, "--json"]);
    assert_eq!(code, 0);
    let actions = json(&out)["actions"].as_array().unwrap().clone();
    let denied = actions
        .iter()
        .find(|a| a["command"] == "cookies" && a["ok"] == false)
        .unwrap_or_else(|| panic!("no denied cookies entry: {actions:?}"));
    assert_eq!(denied["redacted"], true, "{denied}");
    assert_eq!(
        denied["args"],
        json!(["cookies", "[redacted]", "[redacted]", "[redacted]"]),
        "{denied}"
    );
    let error = denied["error"].as_str().unwrap();
    assert_eq!(error, "policy_denied: details withheld (entry redacted)");
    assert!(!out.contains("pegpolicyck"), "{out}");
    close(&plain);

    let allowed = create(&["--url", &url, "--allow", "state"]);
    let (code, _, err) = home.run(&["--session", &allowed, "cookies", "set", "pegpolicyck", "1"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = home.run(&["--session", &allowed, "storage", "local", "set", "k", "v"]);
    assert_eq!(code, 0, "{err}");
    // Take back what this test planted in the shared agent profile.
    let _ = home.run(&[
        "--session",
        &allowed,
        "eval",
        "document.cookie = 'pegpolicyck=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/'; localStorage.removeItem('k')",
    ]);
    close(&allowed);
}

#[test]
#[ignore]
fn deny_eval_blocks_eval_and_wait_fn() {
    let origin = fixture();
    let home = shared_home();
    let url = format!("{origin}/counter");
    let denied = create(&["--url", &url, "--deny", "eval"]);
    for argv in [
        vec!["eval", "1+1"],
        vec!["wait", "--fn", "true"],
        vec!["wait", "#btn", "--fn", "true"],
    ] {
        let mut full = vec!["--session", denied.as_str()];
        full.extend(&argv);
        let (code, _, err) = home.run(&full);
        assert_eq!(code, 1, "{argv:?}: {err}");
        assert!(err.contains("policy_denied"), "{argv:?}: {err}");
        assert!(err.contains("--deny eval"), "{argv:?}: {err}");
    }
    // Other wait forms and other classes are untouched.
    let (code, _, err) = home.run(&["--session", &denied, "wait", "100"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = home.run(&["--session", &denied, "get", "url"]);
    assert_eq!(code, 0, "{err}");
    close(&denied);

    // Without --deny both run.
    let open = create(&["--url", &url]);
    let (code, _, err) = home.run(&["--session", &open, "eval", "1+1"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = home.run(&["--session", &open, "wait", "--fn", "true"]);
    assert_eq!(code, 0, "{err}");
    close(&open);
}

#[test]
#[ignore]
fn second_session_cannot_widen_a_pinned_profile() {
    let home = shared_home();
    let profile = "pol-pinned";
    let (code, _, err) = home.run(&["profile", "create", profile]);
    assert_eq!(code, 0, "{err}");

    let first = create(&["--profile", profile]);
    // Clipboard differs.
    let (code, _, err) = home.run(&[
        "session",
        "create",
        "--profile",
        profile,
        "--allow",
        "clipboard",
    ]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("profile_policy_conflict"), "{err}");
    assert!(err.contains("clipboard"), "{err}");
    assert!(err.contains("--profile"), "{err}");
    // The address policy differs.
    let (code, _, err) = home.run(&[
        "session",
        "create",
        "--profile",
        profile,
        "--allow-domain",
        "a.test",
    ]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("profile_policy_conflict"), "{err}");
    assert!(err.contains("address policy"), "{err}");
    // A session that differs only in what is not pinned joins.
    let second = create(&["--profile", profile, "--deny", "eval", "--allow", "state"]);
    // Another profile is free to choose.
    let other = create(&["--allow", "clipboard"]);
    close(&other);
    // While any session of the profile is open the pin holds ...
    close(&first);
    let (code, _, err) = home.run(&[
        "session",
        "create",
        "--profile",
        profile,
        "--allow",
        "clipboard",
    ]);
    assert_eq!(code, 2, "{err}");
    // ... and it is released when the last one closes.
    close(&second);
    let widened = create(&["--profile", profile, "--allow", "clipboard"]);
    // A create that fails after taking the pin does not keep it.
    close(&widened);
    let (code, _, err) = home.run(&[
        "session",
        "create",
        "--profile",
        profile,
        "--allow",
        "clipboard",
        "--url",
        "ftp://nowhere.test/",
    ]);
    assert_eq!(code, 2, "{err}");
    let plain = create(&["--profile", profile]);
    close(&plain);
    assert_eq!(home.run(&["profile", "delete", profile]).0, 0);
}

#[test]
#[ignore]
fn session_info_shows_the_policy_and_no_token() {
    let home = shared_home();
    let handle = create(&[
        "--allow",
        "state",
        "--deny",
        "eval,download",
        "--allow-domain",
        "B.test,a.test",
        "--no-localhost",
        "--no-expire",
    ]);
    let (code, out, err) = home.run(&["session", "info", &handle, "--json"]);
    assert_eq!(code, 0, "{err}");
    let info = json(&out);
    assert!(!out.contains(token_of(&handle)), "{out}");
    assert!(info.get("token").is_none(), "{out}");
    let policy = &info["policy"];
    assert_eq!(policy["allow"], json!(["state"]), "{policy}");
    assert_eq!(policy["deny"], json!(["eval", "download"]), "{policy}");
    assert_eq!(
        policy["allowDomains"],
        json!(["a.test", "b.test"]),
        "{policy}"
    );
    assert_eq!(policy["noLocalhost"], true, "{policy}");
    assert_eq!(policy["allowPrivateNetwork"], false, "{policy}");
    assert_eq!(policy["noExpire"], true, "{policy}");
    let (code, out, _) = home.run(&["session", "info", &handle]);
    assert_eq!(code, 0);
    assert!(
        out.contains("policy:") && out.contains("allow state"),
        "{out}"
    );
    assert!(!out.contains(token_of(&handle)), "{out}");
    let (_, out, _) = home.run(&["session", "list", "--json"]);
    assert!(!out.contains(token_of(&handle)), "{out}");
    close(&handle);
}

#[test]
fn bad_policy_options_exit_2() {
    // No daemon: the options are checked before one is asked.
    let home = Home::new();
    let dir = std::env::temp_dir().display().to_string();
    let file = std::env::current_exe().unwrap().display().to_string();
    let cases: Vec<Vec<&str>> = vec![
        vec!["--allow", "state", "--deny", "state"],
        vec!["--allow", "navigate"],
        vec!["--deny", "upload"],
        vec!["--allow", "nonsense"],
        vec!["--allow", "State"],
        vec!["--allow-domain", "https://a.test"],
        vec!["--allow-domain", "a.test/x"],
        vec!["--allow-domain", "*.a.test"],
        vec!["--allow-domain", "a.test:80"],
        vec!["--allow-domain", ""],
        vec!["--upload-dir", &dir],
        vec!["--allow", "upload"],
        vec!["--allow", "upload", "--upload-dir", "relative"],
        vec!["--allow", "upload", "--upload-dir", "/no/such/pegboard-dir"],
        vec!["--allow", "upload", "--upload-dir", &file],
        vec!["--no-such-option"],
    ];
    for extra in cases {
        let mut args = vec!["session", "create"];
        args.extend(&extra);
        let (code, out, err) = home.run(&args);
        assert_eq!(code, 2, "{extra:?}: {out} {err}");
        assert!(!err.contains("not_running"), "{extra:?}: {err}");
    }
    // A good policy gets past the options and reaches the missing daemon.
    let (code, _, err) = home.run(&["session", "create", "--allow", "state", "--deny", "eval"]);
    assert_eq!(code, 3, "{err}");
}
