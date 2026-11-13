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

/// A page with one file input (`#f`).
const UPLOAD_PAGE: &str = "data:text/html,<input%20id=f%20type=file%20multiple>";

/// What the page's `#f` holds: `name:size` of each chosen file, comma separated.
fn chosen(handle: &str) -> String {
    let (code, out, err) = shared_home().run(&[
        "--session",
        handle,
        "eval",
        "Array.from(document.getElementById('f').files).map(f => f.name + ':' + f.size).join(',')",
    ]);
    assert_eq!(code, 0, "{err}");
    json(&out)["result"].as_str().unwrap().to_string()
}

/// A session with an upload directory of its own, opened on the upload page.
fn upload_session(dir: &std::path::Path) -> String {
    let handle = create(&[
        "--url",
        UPLOAD_PAGE,
        "--allow",
        "upload",
        "--upload-dir",
        dir.to_str().unwrap(),
    ]);
    // The first command makes the session's files directory.
    let (code, _, err) = shared_home().run(&["--session", &handle, "get", "url"]);
    assert_eq!(code, 0, "{err}");
    handle
}

fn files_dir_of(handle: &str) -> std::path::PathBuf {
    shared_home()
        .path()
        .join("sessions")
        .join(id_of(handle))
        .join("files")
}

#[test]
#[ignore]
fn upload_needs_allow_and_a_directory() {
    let home = shared_home();
    let plain = create(&["--url", UPLOAD_PAGE]);
    let (code, _, err) = home.run(&["--session", &plain, "upload", "#f", "a.txt"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("policy_denied"), "{err}");
    assert!(err.contains("--allow upload"), "{err}");
    assert!(err.contains("--upload-dir"), "{err}");
    assert_eq!(chosen(&plain), "");
    close(&plain);

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello upload").unwrap();
    let handle = upload_session(dir.path());
    let (code, out, err) = home.run(&["--session", &handle, "upload", "#f", "a.txt"]);
    assert_eq!(code, 0, "{out} {err}");
    assert_eq!(chosen(&handle), "a.txt:12");
    close(&handle);
}

#[test]
#[ignore]
fn upload_cannot_reach_outside_the_directory() {
    let home = shared_home();
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
    std::fs::write(dir.path().join("ok.txt"), "ok").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.txt"),
        dir.path().join("link.txt"),
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("linkdir")).unwrap();
    let handle = upload_session(dir.path());
    let absolute = outside.path().join("secret.txt").display().to_string();
    for name in [
        "link.txt",
        "linkdir",
        "../secret.txt",
        "linkdir/secret.txt",
        "/etc/passwd",
        absolute.as_str(),
        "missing.txt",
        ".",
        "",
    ] {
        let (code, out, err) = home.run(&["--session", &handle, "upload", "#f", name]);
        assert_eq!(code, 2, "{name:?}: {out} {err}");
        assert!(err.contains("argv_rejected"), "{name:?}: {err}");
        assert_eq!(chosen(&handle), "", "{name:?}");
    }
    // Neither refusal names a directory.
    let (_, _, err) = home.run(&["--session", &handle, "upload", "#f", "missing.txt"]);
    let dir_s = dir.path().to_str().unwrap();
    assert!(!err.contains(dir_s), "{err}");
    // One bad name among good ones uploads nothing.
    let (code, _, _) = home.run(&["--session", &handle, "upload", "#f", "ok.txt", "link.txt"]);
    assert_eq!(code, 2);
    assert_eq!(chosen(&handle), "");
    // The good name still works.
    let (code, _, err) = home.run(&["--session", &handle, "upload", "#f", "ok.txt"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(chosen(&handle), "ok.txt:2");
    close(&handle);
}

#[test]
#[ignore]
fn a_downloaded_file_can_be_uploaded() {
    // Downloads land in the session's files directory (a later task); a file put there
    // stands in for one. The upload directory has no file of that name.
    let home = shared_home();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("mine.txt"), "mine").unwrap();
    let handle = upload_session(dir.path());
    std::fs::write(files_dir_of(&handle).join("got.txt"), "downloaded").unwrap();
    let (code, out, err) = home.run(&["--session", &handle, "upload", "#f", "got.txt"]);
    assert_eq!(code, 0, "{out} {err}");
    assert_eq!(chosen(&handle), "got.txt:10");
    // Both places in one call; the upload directory comes first for a name in both.
    std::fs::write(files_dir_of(&handle).join("mine.txt"), "from files").unwrap();
    let (code, out, err) = home.run(&["--session", &handle, "upload", "#f", "mine.txt", "got.txt"]);
    assert_eq!(code, 0, "{out} {err}");
    assert_eq!(chosen(&handle), "mine.txt:4,got.txt:10");
    close(&handle);

    // Without --allow upload the same file cannot be uploaded.
    let plain = create(&["--url", UPLOAD_PAGE]);
    let (code, _, err) = home.run(&["--session", &plain, "upload", "#f", "got.txt"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("policy_denied"), "{err}");
    close(&plain);
}

#[test]
#[ignore]
fn upload_is_logged_by_file_name_only() {
    let home = shared_home();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("report.txt"), "r").unwrap();
    let handle = upload_session(dir.path());
    let (code, out, err) = home.run(&["--session", &handle, "upload", "#f", "report.txt"]);
    assert_eq!(code, 0, "{out} {err}");
    // What the agent is told never has the person's directory in it.
    let dir_s = dir.path().to_str().unwrap();
    let real = dir.path().canonicalize().unwrap();
    for text in [&out, &err] {
        assert!(!text.contains(dir_s), "{text}");
        assert!(!text.contains(real.to_str().unwrap()), "{text}");
    }
    let rid = format!("r_{}", &id_of(&handle)[2..]);
    let (code, shown, _) = home.run(&["recording", "show", &rid, "--json"]);
    assert_eq!(code, 0);
    let entry = json(&shown)["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["command"] == "upload")
        .unwrap_or_else(|| panic!("no upload entry: {shown}"))
        .clone();
    assert_eq!(
        entry["args"],
        json!(["upload", "#f", "report.txt"]),
        "{entry}"
    );
    assert_eq!(entry["ok"], true, "{entry}");
    assert!(
        !shown.contains(dir_s) && !shown.contains(real.to_str().unwrap()),
        "{shown}"
    );
    // A refused name is logged without its arguments.
    let _ = home.run(&["--session", &handle, "upload", "#f", "../report.txt"]);
    let (_, shown, _) = home.run(&["recording", "show", &rid, "--json"]);
    assert!(!shown.contains("../report.txt"), "{shown}");
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
