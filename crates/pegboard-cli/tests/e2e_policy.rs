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

// ---------------------------------------------------------------------------------------------
// Address rules (task 4). The shared browser resolves `PRIVATE_NAME` and `PUBLIC_LOOKING_NAME`
// to this machine's private LAN address and `LISTED_NAME`/`UNLISTED_NAME` to 127.0.0.1
// (`host_rules`). `AddressServer` listens on two loopback ports and on the LAN address and
// records every request it gets, so "blocked" is told from "reached" by the server, not only by
// what the page saw. Every request a test sends carries a marker of its own (`m=`).
// ---------------------------------------------------------------------------------------------

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};

/// A tiny HTTP and WebSocket server that records the path and query of every request.
struct AddressServer {
    a: u16,
    b: u16,
    lan: Option<u16>,
    hits: Arc<Mutex<Vec<String>>>,
}

/// Percent-encodes everything but ASCII letters and digits.
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn dec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, name: &str) -> Option<String> {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix(&format!("{name}=")).map(dec))
}

const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";

impl AddressServer {
    fn start() -> AddressServer {
        let hits = Arc::new(Mutex::new(Vec::new()));
        let listen = |ip: &str| {
            let l = std::net::TcpListener::bind((ip, 0)).unwrap();
            let port = l.local_addr().unwrap().port();
            let hits = hits.clone();
            std::thread::spawn(move || {
                for stream in l.incoming() {
                    let Ok(stream) = stream else { continue };
                    let hits = hits.clone();
                    std::thread::spawn(move || serve_one(stream, hits));
                }
            });
            port
        };
        let a = listen("127.0.0.1");
        let b = listen("127.0.0.1");
        let lan = lan_ip().map(|ip| listen(&ip.to_string()));
        AddressServer { a, b, lan, hits }
    }

    /// The LAN port; the test cannot run without a private address on this machine.
    fn lan(&self) -> u16 {
        self.lan.expect(
            "this test needs a private (RFC 1918) IPv4 address on the default route's interface",
        )
    }

    fn reached(&self, marker: &str) -> bool {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .any(|h| h.contains(&format!("m={marker}")))
    }
}

fn serve_one(mut stream: std::net::TcpStream, hits: Arc<Mutex<Vec<String>>>) {
    let mut head = Vec::new();
    let mut buf = [0u8; 4096];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
        if head.len() > 65536 {
            return;
        }
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let target = text
        .lines()
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .unwrap_or("/")
        .to_string();
    hits.lock().unwrap().push(target.clone());
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let header = |name: &str| {
        text.lines().find_map(|l| {
            let (n, v) = l.split_once(':')?;
            n.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_string())
        })
    };
    let to = query_param(query, "to").unwrap_or_default();
    let to2 = query_param(query, "to2").unwrap_or_default();
    if path == "/ws" {
        let key = header("sec-websocket-key").unwrap_or_default();
        let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
        let _ = write!(
            stream,
            "HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: {accept}\r\n\r\n"
        );
        // Hold the socket until the page closes it.
        while matches!(stream.read(&mut buf), Ok(n) if n > 0) {}
        return;
    }
    let (status, ctype, body): (&str, &str, Vec<u8>) = match path {
        "/page" => (
            "200 OK",
            "text/html",
            format!(
                "<!doctype html><title>policy</title><a id=pop target=_blank href=\"{to}\">pop</a>\
                 <button id=wopen onclick=\"window.open('{to2}')\">open</button>"
            )
            .into_bytes(),
        ),
        "/secret" => ("200 OK", "text/plain", b"SECRET-BODY".to_vec()),
        "/img" => ("200 OK", "image/gif", GIF.to_vec()),
        "/sw.js" => (
            "200 OK",
            "text/javascript",
            format!(
                "self.addEventListener('install', e => e.waitUntil(fetch('{to}', {{mode: 'no-cors'}}).catch(() => {{}})));"
            )
            .into_bytes(),
        ),
        "/redirect" => {
            let _ = write!(
                stream,
                "HTTP/1.1 302 Found\r\nlocation: {to}\r\naccess-control-allow-origin: *\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            return;
        }
        _ => ("404 Not Found", "text/plain", b"not found".to_vec()),
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: {ctype}\r\naccess-control-allow-origin: *\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(&body);
}

fn marker(name: &str) -> String {
    format!(
        "{name}{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

/// Creates the profile unless it exists.
fn profile(name: &str) -> &str {
    let _ = shared_home().run(&["profile", "create", name]);
    name
}

/// Runs `promise` (a JavaScript expression that gives a promise) in the session's page and
/// returns what it settled to, as a string. The page keeps the outcome and it is polled, so
/// this does not depend on how `eval` treats promises.
fn settle(handle: &str, promise: &str) -> String {
    let start = format!(
        "(() => {{ window.__peg = window.__peg || {{}}; const k = 'k' + Math.random().toString(36).slice(2); \
         Promise.resolve().then(() => ({promise})).then(v => {{ window.__peg[k] = String(v); }}, e => {{ window.__peg[k] = 'THROW:' + e; }}); return k; }})()"
    );
    let (code, out, err) = shared_home().run(&["--session", handle, "eval", &start]);
    assert_eq!(code, 0, "{err}");
    let key = json(&out)["result"].as_str().unwrap().to_string();
    let read = format!("(window.__peg && window.__peg['{key}']) || null");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let (code, out, err) = shared_home().run(&["--session", handle, "eval", &read]);
        assert_eq!(code, 0, "{err}");
        if let Some(v) = json(&out)["result"].as_str() {
            return v.to_string();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{promise} did not settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

const TIMEOUT_JS: &str = "new Promise(r => setTimeout(() => r('TIMEOUT'), 5000))";

/// `REACHED` when the request got an answer (opaque or not), `FAIL:...` when it failed.
fn fetch_js(url: &str) -> String {
    format!(
        "Promise.race([fetch('{url}', {{mode: 'no-cors'}}).then(() => 'REACHED', e => 'FAIL:' + e.message), {TIMEOUT_JS}])"
    )
}

/// The body when the request got one.
fn read_js(url: &str) -> String {
    format!(
        "Promise.race([fetch('{url}').then(r => r.text()).then(t => 'BODY:' + t, e => 'FAIL:' + e.message), {TIMEOUT_JS}])"
    )
}

fn img_js(url: &str) -> String {
    format!(
        "Promise.race([new Promise(r => {{ const i = new Image(); i.onload = () => r('LOADED'); i.onerror = () => r('ERROR'); i.src = '{url}'; }}), {TIMEOUT_JS}])"
    )
}

fn ws_js(url: &str) -> String {
    format!(
        "Promise.race([new Promise(r => {{ let w; try {{ w = new WebSocket('{url}'); }} catch (e) {{ return r('THROW:' + e.name); }} \
         w.onopen = () => {{ r('OPEN'); w.close(); }}; w.onerror = () => r('ERROR'); }}), {TIMEOUT_JS}])"
    )
}

/// A dedicated (`shared: false`) or shared worker made from a blob fetches `url`.
fn worker_js(url: &str, shared: bool) -> String {
    let body = if shared {
        format!("onconnect = e => {{ const p = e.ports[0]; fetch('{url}', {{mode: 'no-cors'}}).then(() => p.postMessage('REACHED'), x => p.postMessage('FAIL:' + x.message)); }};")
    } else {
        format!("fetch('{url}', {{mode: 'no-cors'}}).then(() => postMessage('REACHED'), x => postMessage('FAIL:' + x.message));")
    };
    let make = if shared {
        "const w = new SharedWorker(src); w.port.onmessage = m => r(m.data); w.port.start();"
    } else {
        "const w = new Worker(src); w.onmessage = m => r(m.data);"
    };
    format!(
        "Promise.race([new Promise(r => {{ const src = URL.createObjectURL(new Blob([{body:?}], {{type: 'text/javascript'}})); {make} }}), {TIMEOUT_JS}])"
    )
}

fn open(handle: &str, url: &str) -> (i32, String, String) {
    shared_home().run(&["--session", handle, "open", url])
}

fn assert_denied(result: (i32, String, String), reason: &str) {
    let (code, out, err) = result;
    assert_ne!(code, 0, "{out}");
    assert!(err.contains("policy_denied"), "{err}");
    assert!(err.contains(reason), "{err}");
}

/// The browser's debugging port and the daemon's TCP listening ports (the session proxy).
fn own_ports() -> Vec<u16> {
    let home = shared_home();
    let active = std::fs::read_to_string(home.path().join("chrome").join("DevToolsActivePort"))
        .expect("DevToolsActivePort");
    let mut ports = vec![active.lines().next().unwrap().trim().parse().unwrap()];
    let pid = home.daemon_pid().expect("daemon pid");
    let out = std::process::Command::new("lsof")
        .args(["-nP", "-a", "-p", &pid.to_string(), "-iTCP", "-sTCP:LISTEN"])
        .output()
        .unwrap();
    for line in String::from_utf8_lossy(&out.stdout).lines().skip(1) {
        if let Some(port) = line
            .split_whitespace()
            .find_map(|w| w.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok()))
        {
            ports.push(port);
        }
    }
    assert!(ports.len() >= 2, "no proxy port found: {out:?}");
    ports
}

#[test]
#[ignore]
fn own_ports_are_never_reachable() {
    let srv = AddressServer::start();
    let page = format!("http://127.0.0.1:{}/page", srv.a);
    let ports = own_ports();
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("pol-own-default", vec![]),
        ("pol-own-private", vec!["--allow-private-network"]),
        ("pol-own-listed", vec!["--allow-domain", "127.0.0.1"]),
    ];
    for (prof, extra) in cases {
        let mut args = vec!["--profile", profile(prof), "--url", &page];
        args.extend(&extra);
        let h = create(&args);
        for port in &ports {
            for host in ["127.0.0.1", "localhost", "[::1]"] {
                let url = format!("http://{host}:{port}/json/version");
                assert!(
                    settle(&h, &fetch_js(&url)).starts_with("FAIL:"),
                    "{extra:?}: {url} was reached"
                );
                assert_ne!(settle(&h, &img_js(&url)), "LOADED", "{extra:?}: {url}");
                // WebSocket handshakes never reach the guard: Chrome refuses page origins on
                // its debugging port and the proxy wants a token. Neither may open.
                let ws = format!("ws://{host}:{port}/devtools/browser");
                assert_ne!(settle(&h, &ws_js(&ws)), "OPEN", "{extra:?}: {ws}");
            }
            assert_denied(
                open(&h, &format!("http://127.0.0.1:{port}/json")),
                "pegboard's own port",
            );
        }
        // Positive control: the page's own server is reachable from the same page.
        let ok = format!("http://127.0.0.1:{}/secret?m={}", srv.b, marker("own"));
        assert_eq!(settle(&h, &read_js(&ok)), "BODY:SECRET-BODY", "{extra:?}");
        close(&h);
    }
}

#[test]
#[ignore]
fn private_network_is_closed_unless_allowed() {
    let srv = AddressServer::start();
    let lan = srv.lan();
    let page = format!("http://localhost:{}/page", srv.a);
    let private = |path: &str, m: &str| format!("http://{PRIVATE_NAME}:{lan}{path}?m={m}");

    let h = create(&["--profile", profile("pol-priv-closed"), "--url", &page]);
    let m = marker("open");
    assert_denied(open(&h, &private("/page", &m)), "private network address");
    let (fm, im, wm) = (marker("f"), marker("i"), marker("w"));
    assert!(settle(&h, &fetch_js(&private("/secret", &fm))).starts_with("FAIL:"));
    assert_ne!(settle(&h, &img_js(&private("/img", &im))), "LOADED");
    // The literal address is refused the same way.
    let ip = lan_ip().unwrap();
    let lm = marker("lit");
    assert!(
        settle(&h, &fetch_js(&format!("http://{ip}:{lan}/secret?m={lm}"))).starts_with("FAIL:")
    );
    // WebSockets are covered on pages from public addresses only (the CSP): a page on a local
    // origin is passed unchanged (owner's ruling).
    let (code, _, err) = open(&h, &format!("http://{PUBLIC_NAME}:{}/page", srv.a));
    assert_eq!(code, 0, "{err}");
    let ws = settle(
        &h,
        &ws_csp_js(&format!("ws://{PRIVATE_NAME}:{lan}/ws?m={wm}")),
    );
    assert!(
        !ws.starts_with("OPEN") && ws.contains("connect-src"),
        "{ws}"
    );
    for m in [&m, &fm, &im, &wm, &lm] {
        assert!(!srv.reached(m), "{m} reached the private server");
    }
    close(&h);

    let h = create(&[
        "--profile",
        profile("pol-priv-open"),
        "--url",
        &page,
        "--allow-private-network",
    ]);
    let (fm, im, wm) = (marker("f"), marker("i"), marker("w"));
    assert_eq!(
        settle(&h, &read_js(&private("/secret", &fm))),
        "BODY:SECRET-BODY"
    );
    assert_eq!(settle(&h, &img_js(&private("/img", &im))), "LOADED");
    let ws = format!("ws://{PRIVATE_NAME}:{lan}/ws?m={wm}");
    assert_eq!(settle(&h, &ws_js(&ws)), "OPEN");
    for m in [&fm, &im, &wm] {
        assert!(srv.reached(m), "{m} did not reach the private server");
    }
    let (code, _, err) = open(&h, &private("/page", &marker("open")));
    assert_eq!(code, 0, "{err}");
    close(&h);
}

#[test]
#[ignore]
fn public_name_resolving_to_a_private_address_is_blocked() {
    let srv = AddressServer::start();
    let lan = srv.lan();
    let page = format!("http://localhost:{}/page", srv.a);
    let h = create(&["--profile", profile("pol-priv-closed"), "--url", &page]);
    let (om, fm) = (marker("open"), marker("f"));
    assert_denied(
        open(
            &h,
            &format!("http://{PUBLIC_LOOKING_NAME}:{lan}/page?m={om}"),
        ),
        "private network address",
    );
    let url = format!("http://{PUBLIC_LOOKING_NAME}:{lan}/secret?m={fm}");
    assert!(settle(&h, &fetch_js(&url)).starts_with("FAIL:"));
    assert!(!srv.reached(&om) && !srv.reached(&fm));
    close(&h);
}

#[test]
#[ignore]
fn redirect_to_private_address_is_blocked() {
    let srv = AddressServer::start();
    let lan = srv.lan();
    let page = format!("http://localhost:{}/page", srv.a);
    let h = create(&["--profile", profile("pol-priv-closed"), "--url", &page]);
    let (fm, nm) = (marker("f"), marker("n"));
    let redirect = |m: &str| {
        let to = format!("http://{PRIVATE_NAME}:{lan}/secret?m={m}");
        format!("http://localhost:{}/redirect?to={}", srv.a, enc(&to))
    };
    // A subrequest that is redirected.
    let got = settle(&h, &read_js(&redirect(&fm)));
    assert!(!got.contains("SECRET-BODY"), "{got}");
    // A navigation that is redirected: the page never shows the body.
    let _ = open(&h, &redirect(&nm));
    // (The tab may show Chrome's error page; whatever it shows is not the body.)
    let (_, out, _) = shared_home().run(&[
        "--session",
        &h,
        "eval",
        "document.body ? document.body.innerText : ''",
    ]);
    assert!(!out.contains("SECRET-BODY"), "{out}");
    assert!(!srv.reached(&fm) && !srv.reached(&nm));
    // Positive control, from the page again (the tab now shows the failed navigation): the
    // redirect itself works to an allowed address.
    let (code, _, err) = open(&h, &page);
    assert_eq!(code, 0, "{err}");
    let to = format!("http://127.0.0.1:{}/secret?m={}", srv.b, marker("ok"));
    let ok = format!("http://localhost:{}/redirect?to={}", srv.a, enc(&to));
    assert_eq!(settle(&h, &read_js(&ok)), "BODY:SECRET-BODY");
    close(&h);
}

fn is_secure_context(handle: &str) -> bool {
    let (code, out, err) =
        shared_home().run(&["--session", handle, "eval", "window.isSecureContext"]);
    assert_eq!(code, 0, "{err}");
    json(&out)["result"] == true
}

#[test]
#[ignore]
fn allow_domain_closes_everything_else_including_localhost() {
    let srv = AddressServer::start();
    // A listed name that resolves to loopback is not open unless the list also opens
    // loopback (DNS rebinding of a listed name).
    let (code, out, err) = shared_home().run(&[
        "session",
        "create",
        "--profile",
        profile("pol-listed-only"),
        "--allow-domain",
        LISTED_NAME,
        "--url",
        &format!("http://{LISTED_NAME}:{}/page", srv.a),
    ]);
    assert_denied((code, out, err), "localhost is closed for this session");

    // `listed.localhost` opens loopback for the listed names, and its page is a secure
    // context (Chrome refuses local requests from an insecure one, whatever the policy).
    let local_name = "listed.localhost";
    let h = create(&[
        "--profile",
        profile("pol-listed"),
        "--allow-domain",
        &format!("{LISTED_NAME},{local_name},{PUBLIC_NAME}"),
        "--url",
        &format!("http://{local_name}:{}/page", srv.a),
    ]);
    assert!(is_secure_context(&h));
    assert_denied(
        open(&h, &format!("http://localhost:{}/page", srv.a)),
        "not in the session's allowed domains",
    );
    assert_denied(
        open(&h, &format!("http://127.0.0.1:{}/page", srv.a)),
        "not in the session's allowed domains",
    );
    let (fm, wm, im) = (marker("f"), marker("w"), marker("i"));
    let unlisted = |path: &str, m: &str| format!("http://{UNLISTED_NAME}:{}{path}?m={m}", srv.b);
    assert!(settle(&h, &fetch_js(&unlisted("/secret", &fm))).starts_with("FAIL:"));
    assert_ne!(settle(&h, &img_js(&unlisted("/img", &im))), "LOADED");
    // WebSockets, from a page of the listed public name (served again with the CSP; a page
    // on a local origin is passed unchanged by the owner's ruling).
    let page = |host: &str| format!("http://{host}:{}/page", srv.a);
    let (code, _, err) = open(&h, &page(PUBLIC_NAME));
    assert_eq!(code, 0, "{err}");
    let lm = marker("lw");
    for url in [
        format!("ws://{UNLISTED_NAME}:{}/ws?m={wm}", srv.b),
        format!("ws://localhost:{}/ws?m={lm}", srv.b),
    ] {
        let got = settle(&h, &ws_csp_js(&url));
        assert!(
            !got.starts_with("OPEN") && got.contains("connect-src"),
            "{url}: {got}"
        );
    }
    let got = settle(
        &h,
        &ws_csp_js(&format!(
            "ws://{LISTED_NAME}:{}/ws?m={}",
            srv.b,
            marker("x")
        )),
    );
    assert!(
        !got.contains("connect-src"),
        "a listed host is refused by the CSP: {got}"
    );
    for m in [&fm, &wm, &im, &lm] {
        assert!(!srv.reached(m), "{m} reached an unlisted host");
    }
    // What is listed works, WebSocket included, on another port of the listed hosts.
    let (code, _, err) = open(&h, &page(local_name));
    assert_eq!(code, 0, "{err}");
    let (fm, wm) = (marker("f"), marker("w"));
    let listed = format!("http://{LISTED_NAME}:{}/secret?m={fm}", srv.b);
    assert_eq!(settle(&h, &read_js(&listed)), "BODY:SECRET-BODY");
    let ws = format!("ws://{LISTED_NAME}:{}/ws?m={wm}", srv.b);
    assert_eq!(settle(&h, &ws_js(&ws)), "OPEN");
    assert!(srv.reached(&fm) && srv.reached(&wm));
    close(&h);

    // Listing localhost opens it.
    let h = create(&[
        "--profile",
        profile("pol-listed-local"),
        "--allow-domain",
        &format!("{LISTED_NAME},localhost"),
        "--url",
        &format!("http://localhost:{}/page", srv.a),
    ]);
    let ws = format!("ws://localhost:{}/ws?m={}", srv.b, marker("w"));
    assert_eq!(settle(&h, &ws_js(&ws)), "OPEN");
    close(&h);
}

/// Pages on an insecure (`http:`, not `localhost`) origin whose host is an allowed loopback or
/// private address are not served again, so they keep their address space and reach the local
/// hosts the policy allows (a served-again one could not: second-machine round 2).
#[test]
#[ignore]
fn pages_on_allowed_local_origins_are_not_served_again() {
    let srv = AddressServer::start();
    // An allow-list (served again): an `http:` name that resolves to loopback.
    let h = create(&[
        "--profile",
        profile("pol-listed-local"),
        "--allow-domain",
        &format!("{LISTED_NAME},localhost"),
        "--url",
        &format!("http://{LISTED_NAME}:{}/page", srv.a),
    ]);
    eprintln!("listed page secure context: {}", is_secure_context(&h));
    let same = format!("http://{LISTED_NAME}:{}/secret?m={}", srv.a, marker("s"));
    let other = format!("http://{LISTED_NAME}:{}/secret?m={}", srv.b, marker("o"));
    let got_same = settle(&h, &read_js(&same));
    let got_other = settle(&h, &read_js(&other));
    close(&h);
    // The default policy (served again): the same name, no allow-list.
    let h = create(&[
        "--profile",
        profile("pol-insecure-default"),
        "--url",
        &format!("http://{UNLISTED_NAME}:{}/page", srv.a),
    ]);
    let other_default = format!("http://{UNLISTED_NAME}:{}/secret?m={}", srv.b, marker("d"));
    let got_default = settle(&h, &read_js(&other_default));
    close(&h);
    // `--allow-private-network`, no allow-list: an `http:` page on the private name.
    let lan = srv.lan();
    let h = create(&[
        "--profile",
        profile("pol-priv-open"),
        "--allow-private-network",
        "--url",
        &format!("http://{PRIVATE_NAME}:{lan}/page"),
    ]);
    let control = format!("http://{PRIVATE_NAME}:{lan}/secret?m={}", marker("c"));
    let got_control = settle(&h, &read_js(&control));
    close(&h);
    eprintln!(
        "listed same-origin: {got_same}; listed other port: {got_other}; \
         default other port: {got_default}; private page: {got_control}"
    );
    assert_eq!(got_control, "BODY:SECRET-BODY", "private page");
    assert_eq!(got_same, "BODY:SECRET-BODY", "listed, same origin");
    assert_eq!(got_other, "BODY:SECRET-BODY", "listed, other port");
    assert_eq!(
        got_default, "BODY:SECRET-BODY",
        "default policy, other port"
    );
}

#[test]
#[ignore]
fn new_tabs_and_popups_are_guarded_before_their_first_request() {
    let srv = AddressServer::start();
    let lan = srv.lan();
    let private = |m: &str| format!("http://{PRIVATE_NAME}:{lan}/secret?m={m}");
    // A page whose `#pop` link (target=_blank) and `#wopen` button (window.open) open a new
    // window whose first request is `link` and `button`.
    let page_to = |link: &str, button: &str| {
        format!(
            "http://localhost:{}/page?to={}&to2={}",
            srv.a,
            enc(link),
            enc(button)
        )
    };
    let wait_reached = |markers: &[&str]| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !markers.iter().all(|m| srv.reached(m)) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    };
    // A click that opens a window can leave agent-browser on the new tab, so the page is
    // opened again in the current tab before each click.
    let click = |h: &str, page: &str, selector: &str| {
        let (code, _, err) = open(h, page);
        assert_eq!(code, 0, "{page}: {err}");
        let (code, _, err) = shared_home().run(&["--session", h, "click", selector]);
        assert_eq!(code, 0, "{selector}: {err}");
    };

    let (tm, pm, wm) = (marker("tab"), marker("pop"), marker("wopen"));
    let page = page_to(&private(&pm), &private(&wm));
    let h = create(&["--profile", profile("pol-priv-closed"), "--url", &page]);
    assert_denied(
        shared_home().run(&["--session", &h, "tab", "new", &private(&tm)]),
        "private network address",
    );
    click(&h, &page, "#pop");
    click(&h, &page, "#wopen");
    std::thread::sleep(std::time::Duration::from_secs(3));
    for m in [&tm, &pm, &wm] {
        assert!(!srv.reached(m), "{m} reached the private server");
    }
    close(&h);

    // Positive control: with the private network allowed the same popups load.
    let (pm, wm) = (marker("pop"), marker("wopen"));
    let page = page_to(&private(&pm), &private(&wm));
    let h = create(&[
        "--profile",
        profile("pol-priv-open"),
        "--allow-private-network",
        "--url",
        &page,
    ]);
    click(&h, &page, "#pop");
    click(&h, &page, "#wopen");
    wait_reached(&[&pm, &wm]);
    assert!(srv.reached(&pm), "the link popup did not load (control)");
    assert!(
        srv.reached(&wm),
        "the window.open popup did not load (control)"
    );
    close(&h);
}

#[test]
#[ignore]
fn workers_follow_the_rules_task_2_established() {
    let srv = AddressServer::start();
    let lan = srv.lan();
    let private = |m: &str| format!("http://{PRIVATE_NAME}:{lan}/secret?m={m}");
    // Service workers need a secure context: localhost is one.
    let page = format!("http://localhost:{}/page", srv.a);
    let sw = |m: &str| {
        format!(
            "navigator.serviceWorker.register('/sw.js?to={}&n={m}').then(r => new Promise(res => {{ \
             const w = r.installing || r.waiting || r.active; if (!w || w.state !== 'installing') return res('STATE:' + (w && w.state)); \
             w.onstatechange = () => {{ if (w.state !== 'installing') res('STATE:' + w.state); }}; setTimeout(() => res('TIMEOUT'), 5000); }}))",
            enc(&private(m))
        )
    };
    let unregister = "navigator.serviceWorker.getRegistrations().then(rs => Promise.all(rs.map(r => r.unregister()))).then(() => 'done')";

    let h = create(&["--profile", profile("pol-priv-closed"), "--url", &page]);
    let (dm, sm, vm) = (marker("dw"), marker("sw"), marker("svc"));
    assert!(settle(&h, &worker_js(&private(&dm), false)).starts_with("FAIL:"));
    assert!(settle(&h, &worker_js(&private(&sm), true)).starts_with("FAIL:"));
    let state = settle(&h, &sw(&vm));
    eprintln!("service worker (closed): {state}");
    assert_eq!(settle(&h, unregister), "done");
    for m in [&dm, &sm, &vm] {
        assert!(!srv.reached(m), "{m} reached the private server");
    }
    close(&h);

    // Positive control.
    let h = create(&[
        "--profile",
        profile("pol-priv-open"),
        "--allow-private-network",
        "--url",
        &page,
    ]);
    let (dm, sm, vm) = (marker("dw"), marker("sw"), marker("svc"));
    assert_eq!(settle(&h, &worker_js(&private(&dm), false)), "REACHED");
    assert_eq!(settle(&h, &worker_js(&private(&sm), true)), "REACHED");
    let state = settle(&h, &sw(&vm));
    eprintln!("service worker (open): {state}");
    assert_eq!(settle(&h, unregister), "done");
    assert!(srv.reached(&dm) && srv.reached(&sm) && srv.reached(&vm));
    close(&h);
}

/// Records the outcome of `new WebSocket(url)` and the CSP violations the page reported, as
/// `<outcome>|<violated directives>`.
fn ws_csp_js(url: &str) -> String {
    format!(
        "new Promise(r => {{ const v = []; document.addEventListener('securitypolicyviolation', e => v.push(e.violatedDirective)); \
         let out = ''; try {{ const w = new WebSocket('{url}'); w.onopen = () => {{ out = 'OPEN'; w.close(); }}; w.onerror = () => {{ out = out || 'ERROR'; }}; }} \
         catch (e) {{ out = 'THROW:' + e.name; }} setTimeout(() => r((out || 'PENDING') + '|' + v.join(',')), 2000); }})"
    )
}

/// A page from a public address (the daemon resolves `PUBLIC_NAME` through DNS; the browser
/// reaches the local test server under it) is served again with the CSP: a plain WebSocket to
/// a host the default policy does not open is refused by `connect-src`, while one to loopback
/// raises no violation. A page on a local origin is not served again, and the same WebSocket
/// opens there (not covered, by the owner's ruling). Needs DNS for `example.net`.
#[test]
#[ignore]
fn pages_from_a_public_address_get_the_csp() {
    let srv = AddressServer::start();
    let h = create(&[
        "--profile",
        profile("pol-csp-default"),
        "--url",
        &format!("http://{PUBLIC_NAME}:{}/page", srv.a),
    ]);
    let wm = marker("w");
    let refused = settle(
        &h,
        &ws_csp_js(&format!("ws://{UNLISTED_NAME}:{}/ws?m={wm}", srv.b)),
    );
    let local = settle(
        &h,
        &ws_csp_js(&format!("ws://localhost:{}/ws?m={}", srv.b, marker("l"))),
    );
    eprintln!("public page: ws to unlisted: {refused}; ws to localhost: {local}");
    assert!(!refused.starts_with("OPEN"), "{refused}");
    assert!(
        refused.contains("connect-src"),
        "no CSP violation: {refused}"
    );
    assert!(!srv.reached(&wm));
    assert!(
        !local.contains("connect-src"),
        "loopback is open in the CSP: {local}"
    );
    close(&h);

    let h = create(&[
        "--profile",
        profile("pol-csp-default"),
        "--url",
        &format!("http://localhost:{}/page", srv.a),
    ]);
    let wm = marker("w");
    let open_ws = settle(
        &h,
        &ws_csp_js(&format!("ws://{UNLISTED_NAME}:{}/ws?m={wm}", srv.b)),
    );
    eprintln!("local page: ws to unlisted: {open_ws}");
    assert!(open_ws.starts_with("OPEN|"), "{open_ws}");
    assert!(srv.reached(&wm));
    close(&h);
}
