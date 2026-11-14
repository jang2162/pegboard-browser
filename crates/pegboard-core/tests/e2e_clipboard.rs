//! End-to-end tests of the clipboard defences (phase 2a, Task 7), on the shared browser (see
//! common). They touch the real system clipboard (macOS `pbcopy`/`pbpaste`): each test saves
//! it, puts a marker in it, and puts the saved text back when it ends, also when it fails
//! (a guard dropped on unwinding). A clipboard that holds anything but plain text is never
//! overwritten: the test refuses to run and says why. Run with
//! `cargo test -p pegboard-core --test e2e_clipboard -- --ignored --test-threads=1`.
//!
//! Every way of writing or reading that a denied session is shown not to have is also tried in
//! a session with `--allow clipboard`, where it must work: a defence that is not there, or a
//! test step that never reached the page, fails there.
#![allow(dead_code)]
mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use common::Harness;
use pegboard_core::actionlog::{ActionLog, Manifest};
use pegboard_core::cdp::{page_targets, CdpConn, CdpEvent};
use pegboard_core::daemon::run_command;
use pegboard_protocol::{
    codes, ActionClass, CommandExecResult, RpcError, SessionCreateParams, SessionInfo,
    SessionPolicy,
};
use serde_json::{json, Value};

// ---- the system clipboard, saved and put back ----

static CLIPBOARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The system clipboard for the length of a test: the text it held is put back on drop.
struct Clipboard {
    saved: Vec<u8>,
    /// It held nothing at all (not even empty text): it is emptied again, not given an empty
    /// text item.
    was_empty: bool,
    _one: std::sync::MutexGuard<'static, ()>,
}

fn pasteboard(command: &str, input: Option<&[u8]>) -> Vec<u8> {
    let mut child = Command::new(command)
        .env("LC_ALL", "en_US.UTF-8")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("{command}: {e}"));
    if let Some(bytes) = input {
        child.stdin.take().unwrap().write_all(bytes).unwrap();
    }
    child.wait_with_output().unwrap().stdout
}

/// Why `clipboard info` (osascript) shows something a text restore would lose, if it does:
/// any type but UTF-8/UTF-16 text (the rule of `spikes/lib/clipsafe.mjs`).
fn non_text(info: &str) -> Option<String> {
    let classes: Vec<&str> = info
        .split('«')
        .skip(1)
        .filter_map(|rest| rest.split_once('»').map(|(c, _)| c))
        .collect();
    if let Some(other) = classes
        .iter()
        .find(|c| !matches!(**c, "class utf8" | "class ut16"))
    {
        return Some(format!("«{other}»"));
    }
    let lower = info.to_ascii_lowercase();
    ["picture", "png", "tiff", "alias", "furl", "file"]
        .iter()
        .find(|w| lower.contains(*w))
        .map(|w| format!("a {w} type"))
}

impl Clipboard {
    /// The clipboard for one test. The first test of the process saves what the clipboard
    /// holds (or panics without touching it when that is not plain text); every test puts
    /// THAT back when it ends, so a late write of one test cannot become the "original" of
    /// the next. A test that finds something else there (a write that landed after the
    /// previous restore) puts the original back first and records it.
    fn take() -> Clipboard {
        let one = CLIPBOARD.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((saved, was_empty)) = ORIGINAL.get() {
            if !holds(saved, *was_empty) {
                let what = format!(
                    "found the clipboard changed after the previous test's restore: {:?}",
                    String::from_utf8_lossy(&pasteboard("pbpaste", None))
                );
                eprintln!("{what}");
                RESTORE_FAILURES
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(what);
                restore(saved, *was_empty);
            }
            return Clipboard {
                saved: saved.clone(),
                was_empty: *was_empty,
                _one: one,
            };
        }
        let info = Command::new("osascript")
            .args(["-e", "clipboard info"])
            .output()
            .unwrap_or_else(|e| panic!("refusing to run: osascript: {e}; nothing was changed"));
        if !info.status.success() {
            panic!(
                "refusing to run: the clipboard's types could not be read ({}); nothing was \
                 changed",
                String::from_utf8_lossy(&info.stderr).trim()
            );
        }
        let info = String::from_utf8_lossy(&info.stdout).trim().to_string();
        if let Some(why) = non_text(&info) {
            panic!(
                "refusing to run: the system clipboard holds non-text data ({why}) that a text \
                 restore would lose; copy some plain text and run again. Nothing was changed."
            );
        }
        let saved = pasteboard("pbpaste", None);
        let was_empty = info.is_empty();
        let _ = ORIGINAL.set((saved.clone(), was_empty));
        Clipboard {
            saved,
            was_empty,
            _one: one,
        }
    }

    fn set(&self, text: &str) {
        pasteboard("pbcopy", Some(text.as_bytes()));
    }

    fn get(&self) -> String {
        String::from_utf8_lossy(&pasteboard("pbpaste", None)).into_owned()
    }

    /// A marker no other run uses, put into the clipboard.
    fn mark(&self) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let marker = format!("PEGBOARD-CLIP-MARKER-{nanos}");
        self.set(&marker);
        marker
    }
}

/// What the clipboard held when the first test of this run took it.
static ORIGINAL: OnceLock<(Vec<u8>, bool)> = OnceLock::new();
/// Restores that did not give back what was saved (checked by the last test).
static RESTORE_FAILURES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn clipboard_types() -> String {
    Command::new("osascript")
        .args(["-e", "clipboard info"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|e| format!("unreadable: {e}"))
}

/// Whether the clipboard holds `saved` (nothing at all when `was_empty`).
fn holds(saved: &[u8], was_empty: bool) -> bool {
    if was_empty {
        clipboard_types().is_empty()
    } else {
        pasteboard("pbpaste", None) == saved
    }
}

/// Puts `saved` back (empties the clipboard when it was empty), checks it, and tries once
/// more when it did not take.
fn restore(saved: &[u8], was_empty: bool) -> bool {
    for _ in 0..2 {
        if was_empty {
            let _ = Command::new("osascript")
                .args([
                    "-l",
                    "JavaScript",
                    "-e",
                    "ObjC.import('AppKit'); $.NSPasteboard.generalPasteboard.clearContents(); 1",
                ])
                .output();
        } else {
            pasteboard("pbcopy", Some(saved));
        }
        if holds(saved, was_empty) {
            return true;
        }
    }
    if was_empty {
        // At least no marker is left behind.
        pasteboard("pbcopy", Some(b""));
    }
    false
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        // A copy the test started may still land (a slow page, a broken build under a
        // mutation): restored after it, and checked again a moment later.
        std::thread::sleep(Duration::from_millis(1000));
        let mut restored = restore(&self.saved, self.was_empty);
        std::thread::sleep(Duration::from_millis(500));
        if restored && !holds(&self.saved, self.was_empty) {
            restored = restore(&self.saved, self.was_empty);
        }
        if !restored {
            let what = format!(
                "a restore did not take: the clipboard holds {:?} (types: {})",
                String::from_utf8_lossy(&pasteboard("pbpaste", None)),
                clipboard_types()
            );
            eprintln!("{what}");
            RESTORE_FAILURES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(what);
        }
    }
}

/// Runs last (tests run in name order): every test gave the clipboard back, and it holds what
/// it held before the first test of this run.
#[test]
#[ignore]
fn zz_the_clipboard_is_as_it_was_before_the_run() {
    let _one = CLIPBOARD.lock().unwrap_or_else(|e| e.into_inner());
    // Late writes of the last test land first.
    std::thread::sleep(Duration::from_secs(2));
    let mut problems = RESTORE_FAILURES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some((saved, was_empty)) = ORIGINAL.get() {
        if !holds(saved, *was_empty) {
            problems.push(format!(
                "the clipboard is not as before the run: {:?} (types: {})",
                String::from_utf8_lossy(&pasteboard("pbpaste", None)),
                clipboard_types()
            ));
            // Put back in any case, then report.
            restore(saved, *was_empty);
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn clipboard_info_with_anything_but_text_is_refused() {
    assert_eq!(non_text(""), None);
    assert_eq!(non_text("«class utf8», 5, «class ut16», 12"), None);
    assert_eq!(
        non_text("«class utf8», 5, «class HTML», 40").as_deref(),
        Some("«class HTML»")
    );
    assert!(non_text("«class PNGf», 100, TIFF picture, 200").is_some());
    assert!(non_text("«class furl», 30").is_some());
    assert!(non_text("TIFF picture, 200").is_some());
}

// ---- pages ----

/// `onclick` attributes only: the agent's attack script below replaces `addEventListener`.
const COPY: &str = "var t=document.getElementById(`t`);t.value=`COPIED-`+Date.now();t.focus();t.select();window.__r=String(document.execCommand(`copy`))";
const CUT: &str = "var t=document.getElementById(`t`);t.value=`CUT-`+Date.now();t.focus();t.select();window.__r=String(document.execCommand(`cut`))";
const WRITE: &str = "navigator.clipboard.writeText(`WRITTEN-`+Date.now()).then(()=>window.__r=`ok`,e=>window.__r=`ERR `+e.name)";

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head><body>{body}</body></html>"
    )
}

/// A document whose whole area is a copy button (for frames, clicked at their centre).
fn inner() -> String {
    format!(
        "<textarea id='t'>x</textarea><button id='cp' style='position:fixed;left:0;top:0;width:100%;height:100%' onclick='{COPY}'>copy</button>"
    )
}

/// `inner`, with a script of its own that runs before anything the guard puts into the
/// running document and stops the clipboard events from reaching later listeners (spike
/// C12a, attack ii).
fn inner_listening_first() -> String {
    format!(
        "<script>window.addEventListener(`copy`,function(e){{e.stopImmediatePropagation()}},true)</script>{}",
        inner()
    )
}

fn route(path: &str, port: u16) -> Option<String> {
    let buttons = format!(
        r#"<textarea id="t">text</textarea><br>
<button id="cp" onclick="{COPY}">copy</button>
<button id="ct" onclick="{CUT}">cut</button>
<button id="wt" onclick="{WRITE}">write</button>
<button id="op" onclick="window.open('/clip')">open</button>
<a id="np" target="_blank" rel="noopener" href="http://localhost:{port}/clip">another site, no opener</a>"#
    );
    let frame = "width:320px;height:200px;border:0";
    Some(match path {
        "/clip" => page("clip", &buttons),
        // The page's own copy handler sets what is copied.
        "/handler" => page(
            "handler",
            &format!(
                "{buttons}<script>document.oncopy=function(e){{e.clipboardData.setData('text/plain','HANDLER-'+Date.now());e.preventDefault()}}</script>"
            ),
        ),
        "/inner" => page("inner", &inner()),
        // Another site than 127.0.0.1: its own frame target (OOPIF).
        "/xsite" => page(
            "xsite",
            &format!(
                r#"<iframe id="f" style="{frame}" src="http://localhost:{port}/inner"></iframe>"#
            ),
        ),
        // Attached running, not waiting (spike C12c).
        "/sandbox" => page(
            "sandbox",
            &format!(
                r#"<iframe id="f" style="{frame}" sandbox="allow-scripts" srcdoc="{}"></iframe>"#,
                inner()
            ),
        ),
        // Ten of those: the guard's reloads are counted per frame, not per page (review C1).
        "/sandbox-many" => page(
            "sandbox-many",
            &(0..10)
                .map(|i| {
                    format!(
                        r#"<iframe id="f{i}" style="{frame}" sandbox="allow-scripts" srcdoc="{}"></iframe>"#,
                        inner_listening_first()
                    )
                })
                .collect::<String>(),
        ),
        // One of those whose document the page replaces every 300 ms (a frame that keeps
        // coming back).
        "/sandbox-loop" => page(
            "sandbox-loop",
            &format!(
                r#"<iframe id="f" style="{frame}" sandbox="allow-scripts" srcdoc="{0}"></iframe><script>let n=0;setInterval(()=>{{const f=document.getElementById('f');f.srcdoc=f.srcdoc.replace(/<!--\d*-->|$/,'<!--'+(n++)+'-->')}},300)</script>"#,
                inner_listening_first()
            ),
        ),
        // The same, whose document listens first.
        "/sandbox-first" => page(
            "sandbox-first",
            &format!(
                r#"<iframe id="f" style="{frame}" sandbox="allow-scripts" srcdoc="{}"></iframe>"#,
                inner_listening_first()
            ),
        ),
        _ => return None,
    })
}

fn port() -> u16 {
    static PORT: OnceLock<u16> = OnceLock::new();
    *PORT.get_or_init(|| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split(' ').nth(1))
                        .unwrap_or("/")
                        .to_string();
                    let resp = match route(&path, port) {
                        Some(body) => format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        ),
                        None => "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
                    };
                    let _ = stream.write_all(resp.as_bytes());
                });
            }
        });
        port
    })
}

fn url(path: &str) -> String {
    format!("http://127.0.0.1:{}{path}", port())
}

// ---- an agent: a session and a CDP client of its proxy endpoint ----

struct Agent {
    info: SessionInfo,
    c: CdpConn,
}

fn policy(allow: bool) -> SessionPolicy {
    SessionPolicy {
        allow: if allow {
            vec![ActionClass::Clipboard]
        } else {
            Vec::new()
        },
        ..SessionPolicy::default()
    }
}

async fn create(h: &Harness, allow: bool, profile: Option<&str>) -> Result<Agent, RpcError> {
    let info = h
        .manager
        .create(SessionCreateParams {
            name: None,
            url: None,
            profile: profile.map(String::from),
            policy: policy(allow),
        })
        .await?;
    h.sessions.lock().unwrap().push(info.id.clone());
    let ep = h.manager.endpoint(&info.id).unwrap();
    let c = common::client(&ep).await;
    Ok(Agent { info, c })
}

async fn agent(h: &Harness, allow: bool) -> Agent {
    create(h, allow, None).await.unwrap()
}

impl Agent {
    fn first(&self) -> String {
        self.info.targets[0].clone()
    }

    async fn tab(&self, target: &str) -> Tab<'_> {
        let sid = self.c.attach(target).await.unwrap();
        Tab { c: &self.c, sid }
    }

    /// A new tab of the session (`Target.createTarget`, as agent-browser's `tab new`).
    async fn new_tab(&self) -> String {
        let r = self
            .c
            .send("Target.createTarget", json!({ "url": "about:blank" }), None)
            .await
            .unwrap();
        r["targetId"].as_str().unwrap().to_string()
    }

    /// A popup `opener` opened that is not in `known` (adopted into the session's scope by
    /// the listing).
    async fn popup_of(&self, opener: &str, known: &[String]) -> String {
        for _ in 0..50 {
            let r = self
                .c
                .send("Target.getTargets", json!({}), None)
                .await
                .unwrap();
            let found = r["targetInfos"].as_array().and_then(|infos| {
                infos
                    .iter()
                    .find(|t| {
                        t["type"] == "page"
                            && t["openerId"] == opener
                            && !known.iter().any(|k| t["targetId"] == k.as_str())
                    })
                    .and_then(|t| t["targetId"].as_str().map(String::from))
            });
            if let Some(id) = found {
                return id;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("no popup of {opener} appeared");
    }
}

struct Tab<'a> {
    c: &'a CdpConn,
    sid: String,
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(400)).await;
}

impl Tab<'_> {
    /// Evaluates (with a user gesture when `gesture`); an exception comes back as `EXC ...`.
    async fn eval(&self, expr: &str, gesture: bool) -> String {
        let r = self
            .c
            .send(
                "Runtime.evaluate",
                json!({ "expression": expr, "awaitPromise": true, "returnByValue": true, "userGesture": gesture }),
                Some(&self.sid),
            )
            .await;
        match r {
            Ok(r) if r.get("exceptionDetails").is_some() => {
                format!("EXC {}", r["exceptionDetails"])
            }
            Ok(r) => match &r["result"]["value"] {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            },
            Err(e) => format!("CDPERR {e:#}"),
        }
    }

    async fn load(&self, path: &str) {
        self.c.navigate(&self.sid, &url(path)).await.unwrap();
        settle().await;
    }

    async fn click_at(&self, x: f64, y: f64) {
        for (kind, count) in [("mouseMoved", 0), ("mousePressed", 1), ("mouseReleased", 1)] {
            self.c
                .send(
                    "Input.dispatchMouseEvent",
                    json!({ "type": kind, "x": x, "y": y, "button": "left", "clickCount": count }),
                    Some(&self.sid),
                )
                .await
                .unwrap();
        }
    }

    /// A real mouse click at the centre of `selector`.
    async fn click(&self, selector: &str) {
        let at = self
            .eval(
                &format!("JSON.stringify((()=>{{const r=document.querySelector('{selector}').getBoundingClientRect();return [r.x+r.width/2,r.y+r.height/2]}})())"),
                false,
            )
            .await;
        let at: Vec<f64> = serde_json::from_str(&at).unwrap_or_else(|_| panic!("{at}"));
        self.click_at(at[0], at[1]).await;
        settle().await;
    }

    /// keyDown and keyUp of one key, as `Input.dispatchKeyEvent` takes it.
    async fn key(&self, key: &str, code: &str, vk: u32, modifiers: u32, commands: &[&str]) {
        let mut base = json!({ "key": key, "code": code, "windowsVirtualKeyCode": vk, "modifiers": modifiers });
        if !commands.is_empty() {
            base["commands"] = json!(commands);
        }
        for kind in ["rawKeyDown", "keyUp"] {
            let mut event = base.clone();
            event["type"] = json!(kind);
            self.c
                .send("Input.dispatchKeyEvent", event, Some(&self.sid))
                .await
                .unwrap();
        }
        settle().await;
    }

    async fn select_text(&self) {
        self.eval(
            "(()=>{const t=document.getElementById('t');t.value='SELECTED-'+Date.now();t.focus();t.select();return 1})()",
            false,
        )
        .await;
    }

    async fn empty_focused_textarea(&self) {
        self.eval(
            "(()=>{const t=document.getElementById('t');t.value='';t.focus();return 1})()",
            false,
        )
        .await;
    }

    async fn textarea(&self) -> String {
        self.eval("document.getElementById('t').value", false).await
    }
}

const META: u32 = 4;
const CONTROL: u32 = 2;
const SHIFT: u32 = 8;

/// Each way of writing the system clipboard, on a fresh load with a marker in the clipboard:
/// (what, whether the clipboard changed, what the page said).
async fn writes(tab: &Tab<'_>, clip: &Clipboard) -> Vec<(&'static str, bool, String)> {
    let mut out = Vec::new();
    let steps: &[&str] = &[
        "eval writeText (gesture)",
        "click writeText",
        "click execCommand copy",
        "click execCommand cut",
        "eval execCommand copy (gesture)",
        "eval execCommand cut (gesture)",
        "Meta+c with commands copy",
        "Meta+x with commands cut",
        "Control+c with commands copy",
        "Control+Insert",
        "Shift+Delete",
        "page copy handler, click",
        "page copy handler, Meta+c with commands",
    ];
    for step in steps {
        let page = if step.starts_with("page copy handler") {
            "/handler"
        } else {
            "/clip"
        };
        tab.load(page).await;
        let marker = clip.mark();
        let said = match *step {
            "eval writeText (gesture)" => {
                tab.eval(
                    "Promise.race([navigator.clipboard.writeText('EVAL-'+Date.now()).then(()=>'ok',e=>'ERR '+e.name), new Promise(r=>setTimeout(()=>r('PENDING'),3000))])",
                    true,
                )
                .await
            }
            "click writeText" => {
                tab.click("#wt").await;
                tab.eval("String(window.__r)", false).await
            }
            "click execCommand copy" | "page copy handler, click" => {
                tab.click("#cp").await;
                tab.eval("String(window.__r)", false).await
            }
            "click execCommand cut" => {
                tab.click("#ct").await;
                tab.eval("String(window.__r)", false).await
            }
            "eval execCommand copy (gesture)" => {
                tab.select_text().await;
                tab.eval("String(document.execCommand('copy'))", true).await
            }
            "eval execCommand cut (gesture)" => {
                tab.select_text().await;
                tab.eval("String(document.execCommand('cut'))", true).await
            }
            "Meta+c with commands copy" | "page copy handler, Meta+c with commands" => {
                tab.select_text().await;
                tab.key("c", "KeyC", 67, META, &["copy"]).await;
                String::new()
            }
            "Meta+x with commands cut" => {
                tab.select_text().await;
                tab.key("x", "KeyX", 88, META, &["cut"]).await;
                String::new()
            }
            "Control+c with commands copy" => {
                tab.select_text().await;
                tab.key("c", "KeyC", 67, CONTROL, &["copy"]).await;
                String::new()
            }
            "Control+Insert" => {
                tab.select_text().await;
                tab.key("Insert", "Insert", 45, CONTROL, &[]).await;
                String::new()
            }
            "Shift+Delete" => {
                tab.select_text().await;
                tab.key("Delete", "Delete", 46, SHIFT, &[]).await;
                String::new()
            }
            other => unreachable!("{other}"),
        };
        settle().await;
        out.push((*step, clip.get() != marker, said));
    }
    out
}

/// Each way of reading the system clipboard into the page: (what, whether the page got the
/// marker, what the page said).
async fn reads(tab: &Tab<'_>, clip: &Clipboard) -> Vec<(&'static str, bool, String)> {
    let mut out = Vec::new();
    for step in [
        "eval readText (gesture)",
        "Meta+v with commands paste",
        "Control+v with commands paste",
        "Shift+Insert",
    ] {
        tab.load("/clip").await;
        let marker = clip.mark();
        let said = if step == "eval readText (gesture)" {
            tab.eval(
                "Promise.race([navigator.clipboard.readText().then(t=>t,e=>'ERR '+e.name), new Promise(r=>setTimeout(()=>r('PENDING'),3000))])",
                true,
            )
            .await
        } else {
            tab.empty_focused_textarea().await;
            match step {
                "Meta+v with commands paste" => tab.key("v", "KeyV", 86, META, &["paste"]).await,
                "Control+v with commands paste" => {
                    tab.key("v", "KeyV", 86, CONTROL, &["paste"]).await
                }
                _ => tab.key("Insert", "Insert", 45, SHIFT, &[]).await,
            }
            tab.textarea().await
        };
        out.push((step, said.contains(&marker), said));
    }
    out
}

/// The clipboard-changing click of `/clip` (`#cp`) in `tab`, with a marker in the clipboard:
/// whether the clipboard changed.
async fn click_copy_changes(tab: &Tab<'_>, selector: &str, clip: &Clipboard) -> bool {
    let marker = clip.mark();
    tab.click(selector).await;
    settle().await;
    clip.get() != marker
}

fn log_for(h: &Harness, info: &SessionInfo) -> ActionLog {
    let manifest = Manifest::new(
        &info.recording_id,
        &info.id.0,
        &info.name,
        &info.profile,
        &info.created_at,
        "test",
        "0.42.0",
    );
    ActionLog::open(&h.env.paths, manifest).unwrap()
}

async fn command(
    h: &Harness,
    log: &mut ActionLog,
    info: &SessionInfo,
    argv: &[&str],
) -> Result<CommandExecResult, RpcError> {
    run_command(
        &h.manager,
        &h.runner,
        &h.env.paths,
        log,
        &info.id,
        &common::s(argv),
        Some(30_000),
    )
    .await
}

/// Whether a page of the session's first tab can write the clipboard by a click: with
/// `writeText` (stopped by the permission) and with `execCommand('copy')` (stopped by the
/// shield).
async fn writes_in(a: &Agent, clip: &Clipboard) -> (bool, bool) {
    let tab = a.tab(&a.first()).await;
    tab.load("/clip").await;
    let api = click_copy_changes(&tab, "#wt", clip).await;
    (api, click_copy_changes(&tab, "#cp", clip).await)
}

fn clipboard_keys() -> [&'static str; 6] {
    [
        "Meta+c",
        "Meta+v",
        "Control+x",
        "Shift+Insert",
        "Control+Insert",
        "Shift+Delete",
    ]
}

// ---- the tests ----

/// The plan's first test: without `--allow clipboard` no way of writing works (the permission
/// stops `navigator.clipboard`, the shield every `copy`/`cut` event path), and the key
/// combinations are refused before anything runs.
#[test]
#[ignore]
fn agent_cannot_write_the_clipboard_by_default() {
    common::run(async {
        let clip = Clipboard::take();
        let h = Harness::start();
        let a = agent(&h, false).await;
        let tab = a.tab(&a.first()).await;
        let got = writes(&tab, &clip).await;
        eprintln!("denied session, writes: {got:#?}");
        for (step, changed, said) in &got {
            assert!(!changed, "{step} wrote the system clipboard (page: {said})");
        }
        let mut log = log_for(&h, &a.info);
        for key in clipboard_keys() {
            let e = command(&h, &mut log, &a.info, &["press", key])
                .await
                .expect_err(key);
            assert_eq!(e.code, codes::POLICY_DENIED, "{key}: {e:?}");
            assert!(e.message.contains("--allow clipboard"), "{key}: {e:?}");
        }
    });
}

/// The plan's second test: without `--allow clipboard` the page reads nothing of the system
/// clipboard (`readText` is refused at once, not left waiting for a prompt).
#[test]
#[ignore]
fn agent_cannot_read_the_clipboard_by_default() {
    common::run(async {
        let clip = Clipboard::take();
        let h = Harness::start();
        let a = agent(&h, false).await;
        let tab = a.tab(&a.first()).await;
        let got = reads(&tab, &clip).await;
        eprintln!("denied session, reads: {got:#?}");
        for (step, saw, said) in &got {
            assert!(!saw, "{step} read the system clipboard (page: {said})");
        }
        assert!(
            got[0].2.contains("NotAllowedError"),
            "readText was not refused: {}",
            got[0].2
        );
    });
}

/// The positive control: with `--allow clipboard` every write path and the key paste paths
/// work (so the denied runs above prove the defences, not a test that never acted), and the
/// key combinations are not refused.
#[test]
#[ignore]
fn allow_clipboard_opens_it() {
    common::run(async {
        let clip = Clipboard::take();
        let h = Harness::start();
        let a = agent(&h, true).await;
        let tab = a.tab(&a.first()).await;
        let got = writes(&tab, &clip).await;
        eprintln!("allowed session, writes: {got:#?}");
        for (step, changed, said) in &got {
            assert!(changed, "{step} did not write the clipboard (page: {said})");
        }
        let got = reads(&tab, &clip).await;
        eprintln!("allowed session, reads: {got:#?}");
        // readText asks the person (a prompt nobody answers, spike C1): not a control.
        for (step, saw, said) in &got[1..] {
            assert!(saw, "{step} did not read the clipboard (page: {said})");
        }
        let mut log = log_for(&h, &a.info);
        for key in clipboard_keys() {
            if let Err(e) = command(&h, &mut log, &a.info, &["press", key]).await {
                assert_ne!(e.code, codes::POLICY_DENIED, "{key}: {e:?}");
            }
        }
    });
}

/// Tabs and documents that appear after the session started are covered as well: a tab the
/// agent opens, a popup a page opens, a cross-site frame (its own target) and a sandboxed
/// srcdoc frame (attached running). Each is tried in a denied and in an allowed session.
#[test]
#[ignore]
fn new_tabs_popups_and_frames_are_covered() {
    common::run(async {
        let clip = Clipboard::take();
        let h = Harness::start();
        for allow in [false, true] {
            let a = agent(&h, allow).await;
            let mut got = Vec::new();

            let tab_id = a.new_tab().await;
            let tab = a.tab(&tab_id).await;
            tab.load("/clip").await;
            got.push(("new tab", click_copy_changes(&tab, "#cp", &clip).await));

            let first = a.tab(&a.first()).await;
            first.load("/clip").await;
            first.click("#op").await;
            let popup_id = a.popup_of(&a.first(), &[]).await;
            let popup = a.tab(&popup_id).await;
            settle().await;
            got.push(("popup", click_copy_changes(&popup, "#cp", &clip).await));

            // A link to another site without opener: a popup with no live renderer while the
            // guard holds it (the shield is answered only after the resume).
            first.load("/clip").await;
            first.click("#np").await;
            let other = a
                .popup_of(&a.first(), std::slice::from_ref(&popup_id))
                .await;
            let other = a.tab(&other).await;
            let loaded = common::eventually(10, || async {
                other.eval("document.readyState", false).await == "complete"
            })
            .await;
            assert!(loaded, "the popup without opener did not load");
            got.push((
                "popup to another site without opener",
                click_copy_changes(&other, "#cp", &clip).await,
            ));

            first.load("/xsite").await;
            got.push((
                "cross-site frame",
                click_copy_changes(&first, "#f", &clip).await,
            ));

            first.load("/sandbox").await;
            got.push((
                "sandboxed srcdoc frame",
                click_copy_changes(&first, "#f", &clip).await,
            ));

            eprintln!("allow={allow}: {got:?}");
            for (what, changed) in got {
                assert_eq!(changed, allow, "{what}: allow={allow}");
            }
            h.close_session(&a.info.id).await;
        }
        let log = h.manager.guard_attach_log().join("\n");
        assert!(
            log.contains("iframe agent waiting=false"),
            "no frame was attached running (the sandboxed frame case did not happen):\n{log}"
        );
    });
}

/// The clipboard is a profile setting (spike C2: no tab level). A session with another
/// clipboard setting cannot join a profile (`profile_policy_conflict` naming the clipboard);
/// in another profile it is created and each keeps its own setting at the same time. A
/// profile changes its setting with its sessions: the denial is lifted when the next session
/// allows the clipboard, and set again when the one after denies it.
#[test]
#[ignore]
fn clipboard_setting_of_one_session_does_not_leak_to_another() {
    common::run(async {
        let clip = Clipboard::take();
        let h = Harness::start();
        let (p, q) = ("clip-pin-a", "clip-pin-b");
        for name in [p, q] {
            h.manager.create_profile(name).await.unwrap();
        }

        let denied = create(&h, false, Some(p)).await.unwrap();
        let e = create(&h, true, Some(p))
            .await
            .err()
            .expect("a session allowing the clipboard joined a denied profile");
        assert_eq!(e.code, codes::PROFILE_POLICY_CONFLICT, "{e:?}");
        assert!(e.message.contains("clipboard"), "{e:?}");
        let allowed = create(&h, true, Some(q)).await.unwrap();
        // Both at once, each with its own setting.
        assert_eq!(writes_in(&denied, &clip).await, (false, false), "denied");
        assert_eq!(writes_in(&allowed, &clip).await, (true, true), "allowed");
        assert_eq!(writes_in(&denied, &clip).await, (false, false), "denied");
        h.close_session(&denied.info.id).await;
        h.close_session(&allowed.info.id).await;

        // The profile follows its next session: lifted, then denied again.
        let now_allowed = create(&h, true, Some(p)).await.unwrap();
        assert!(
            writes_in(&now_allowed, &clip).await == (true, true),
            "the denial of the profile was not lifted (writeText, execCommand)"
        );
        h.close_session(&now_allowed.info.id).await;
        let denied_again = create(&h, false, Some(p)).await.unwrap();
        assert!(
            writes_in(&denied_again, &clip).await == (false, false),
            "the profile was not denied again (writeText, execCommand)"
        );
        h.close_session(&denied_again.info.id).await;
        for name in [p, q] {
            let _ = h.manager.delete_profile(name).await;
        }
    });
}

/// The agent's attack on the shield, as a new-document script (spike C12a: i1, i2, i3, ii).
/// It marks that it ran.
const ATTACK: &str = "(()=>{window.__agentRan=1;const noop=function(){};\
    window.addEventListener('copy',function(e){e.stopImmediatePropagation()},true);\
    window.addEventListener('cut',function(e){e.stopImmediatePropagation()},true);\
    Event.prototype.preventDefault=noop;Event.prototype.stopImmediatePropagation=noop;\
    Function.prototype.call=noop;EventTarget.prototype.addEventListener=noop})()";

/// Replaces K3's refusal of the agent's `Page.addScriptToEvaluateOnNewDocument` (agent-browser's
/// `vitals` needs it): the agent's own new-document script cannot run before the shield, so it
/// cannot disarm it (spike C12a: the older session's script runs first, and the guard holds
/// every new target until its own session has set the shield). Tried on the agent's tab and on
/// a popup the agent is attached to from its start.
#[test]
#[ignore]
fn agent_scripts_cannot_run_before_the_shield() {
    common::run(async {
        let clip = Clipboard::take();
        let h = Harness::start();
        let a = agent(&h, false).await;
        let first = a.first();
        let tab = a.tab(&first).await;
        a.c.send("Page.enable", json!({}), Some(&tab.sid))
            .await
            .unwrap();
        a.c.send(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": ATTACK }),
            Some(&tab.sid),
        )
        .await
        .expect("the agent's own script is refused; K3 was dropped on purpose");
        tab.load("/clip").await;
        assert_eq!(tab.eval("String(window.__agentRan)", false).await, "1");
        assert!(
            !click_copy_changes(&tab, "#cp", &clip).await,
            "the agent's script disarmed the shield on its tab"
        );

        // A popup the agent is attached to from its start (auto-attach on the browser level,
        // where the proxy turns waiting off, and on its page session, where it may wait), and
        // gives its script as soon as it can.
        let mut events = a.c.events();
        for session in [None, Some(tab.sid.as_str())] {
            a.c.send(
                "Target.setAutoAttach",
                json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
                session,
            )
            .await
            .unwrap();
        }
        tab.eval("document.getElementById('op').click(); 1", true)
            .await;
        let attached = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let e: CdpEvent = events.recv().await.unwrap();
                if e.method == "Target.attachedToTarget"
                    && e.params["targetInfo"]["type"] == "page"
                    && e.params["targetInfo"]["openerId"] == first.as_str()
                {
                    return e.params;
                }
            }
        })
        .await
        .expect("the agent was not attached to the popup");
        let popup_sid = attached["sessionId"].as_str().unwrap().to_string();
        eprintln!(
            "agent attached to the popup, waiting={}",
            attached["waitingForDebugger"]
        );
        a.c.send(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": ATTACK }),
            Some(&popup_sid),
        )
        .await
        .unwrap();
        // Harmless when it was not waiting.
        let _ =
            a.c.send(
                "Runtime.runIfWaitingForDebugger",
                json!({}),
                Some(&popup_sid),
            )
            .await;
        let popup = Tab {
            c: &a.c,
            sid: popup_sid,
        };
        let loaded = common::eventually(10, || async {
            popup.eval("document.readyState", false).await == "complete"
        })
        .await;
        assert!(loaded, "the popup did not load");
        // Measured on the second machine: the popup's first document ran without the agent's
        // script (its attachment did not hold the popup), so the script only applies from the
        // next document on. That one is what is tried here; whether the agent's session on
        // the popup is older than the guard's decides it either way.
        eprintln!(
            "agent script on the popup's first document: {}",
            popup.eval("String(window.__agentRan)", false).await
        );
        popup.load("/clip").await;
        assert_eq!(popup.eval("String(window.__agentRan)", false).await, "1");
        assert!(
            !click_copy_changes(&popup, "#cp", &clip).await,
            "the agent's script disarmed the shield on the popup"
        );
    });
}

/// A sandboxed srcdoc frame is attached already running (spike C12c), so its own first
/// script runs before the shield that goes into the running document, and a capture listener
/// of its own that stops the event (C12a, ii) comes first (measured on the second machine:
/// the copy went through). The guard makes such a frame reload itself once the shield is
/// registered: its next document has the shield first. Prints what the guard did with
/// frames attached running.
#[test]
#[ignore]
fn sandboxed_srcdoc_frame_listening_first() {
    common::run(async {
        let clip = Clipboard::take();
        let h = Harness::start();
        for allow in [true, false] {
            let a = agent(&h, allow).await;
            let tab = a.tab(&a.first()).await;
            tab.load("/sandbox-first").await;
            let changed = click_copy_changes(&tab, "#f", &clip).await;
            eprintln!("allow={allow}: the frame's copy changed the clipboard: {changed}");
            if allow {
                assert!(changed, "the click did not reach the frame (control)");
            } else {
                assert!(!changed, "the frame's own listener came before the shield");
            }
            h.close_session(&a.info.id).await;
        }
        let log = h.manager.guard_attach_log();
        eprintln!(
            "frames attached running: {:#?}",
            log.iter()
                .filter(|l| l.contains("iframe agent waiting=false"))
                .collect::<Vec<_>>()
        );
    });
}

/// K4: the denial lives on a connection of its own. When it closes, the session's commands
/// are refused (not run with the clipboard open) until pegboard has set it again; afterwards
/// the clipboard is still closed.
#[test]
#[ignore]
fn clipboard_denial_is_set_again_after_its_connection_drops() {
    common::run(async {
        let clip = Clipboard::take();
        let h = Harness::start();
        let a = agent(&h, false).await;
        let mut log = log_for(&h, &a.info);
        assert!(h.manager.require_clipboard(&a.info.id).is_ok());
        assert!(h
            .manager
            .drop_clipboard_policy_for_test(&a.info.id, Duration::from_secs(3)));
        let e = match command(&h, &mut log, &a.info, &["get", "url"]).await {
            Ok(r) => panic!("a command ran while the denial was down: {r:?}"),
            Err(e) => e,
        };
        assert_eq!(e.code, codes::BROWSER_FAILED, "{e:?}");
        assert!(e.message.contains("clipboard policy"), "{e:?}");
        let back = common::eventually(20, || async {
            h.manager.require_clipboard(&a.info.id).is_ok()
        })
        .await;
        assert!(back, "the clipboard denial was not set again");
        let r = command(&h, &mut log, &a.info, &["get", "url"]).await;
        if let Err(e) = r {
            assert!(!e.message.contains("clipboard policy"), "{e:?}");
        }
        let tab = a.tab(&a.first()).await;
        tab.load("/clip").await;
        assert!(
            !click_copy_changes(&tab, "#wt", &clip).await,
            "writeText worked after the denial was set again"
        );
    });
}

/// K2 and the reconnect gap: after a guard reconnect the agents' connections of sessions that
/// deny the clipboard are dropped before the guard sets anything up (their CDP sessions on the
/// tabs, and their auto-attach, would be older than the guard's: C12a). The tabs that were
/// there are shielded and reloaded, so the shield is the first script of their next document:
/// an attack script the agent registered before the reconnect is gone with its session. A
/// session allowing the clipboard keeps its connection, and nothing is closed.
#[test]
#[ignore]
fn after_a_guard_reconnect_agent_connections_are_dropped_and_tabs_reloaded() {
    common::run(async {
        let clip = Clipboard::take();
        let h = Harness::start();
        let profile = "clip-reconnect";
        h.manager.create_profile(profile).await.unwrap();
        let denied = agent(&h, false).await;
        let allowed = create(&h, true, Some(profile)).await.unwrap();
        let (d, k) = (denied.first(), allowed.first());
        let tab = denied.tab(&d).await;
        tab.load("/clip").await;
        denied
            .c
            .send(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({ "source": ATTACK }),
                Some(&tab.sid),
            )
            .await
            .unwrap();
        tab.eval("window.__beforeReconnect = 1; 1", false).await;
        let kept = allowed.tab(&k).await;
        kept.load("/clip").await;

        assert!(h.manager.drop_guard_for_test(Duration::from_secs(2)));
        let back = common::eventually(15, || async { h.manager.require_guard().is_ok() }).await;
        assert!(back, "the guard did not come back");
        let log = h.manager.guard_attach_log().join("\n");

        // The denied session's old connection is gone; the allowed one's is not.
        let dropped = common::eventually(5, || async {
            denied
                .c
                .send("Target.getTargets", json!({}), None)
                .await
                .is_err()
        })
        .await;
        assert!(dropped, "the denied session's connection was not dropped");
        assert!(
            allowed
                .c
                .send("Target.getTargets", json!({}), None)
                .await
                .is_ok(),
            "the allowed session's connection was dropped"
        );
        // Nothing was closed.
        let pages = page_targets(&h.browser.conn).await.unwrap();
        for t in [&d, &k] {
            assert!(
                pages.iter().any(|p| &p.target_id == t),
                "{t} closed:\n{log}"
            );
        }
        assert!(log.contains("reload=ok"), "no tab was reloaded:\n{log}");

        // A new connection, as agent-browser makes: the tab has a new document without the
        // agent's old script, and the shield holds.
        let ep = h.manager.endpoint(&denied.info.id).unwrap();
        let c = common::client(&ep).await;
        let sid = c.attach(&d).await.unwrap();
        let fresh = Tab { c: &c, sid };
        let reloaded = common::eventually(10, || async {
            fresh.eval("String(window.__beforeReconnect)", false).await == "undefined"
                && fresh.eval("document.readyState", false).await == "complete"
        })
        .await;
        assert!(reloaded, "the old tab was not reloaded:\n{log}");
        assert_eq!(
            fresh.eval("String(window.__agentRan)", false).await,
            "undefined",
            "the agent's script from before the reconnect ran"
        );
        assert!(
            !click_copy_changes(&fresh, "#cp", &clip).await,
            "the old tab's copy went through after the reconnect"
        );
        h.close_session(&allowed.info.id).await;
        let _ = h.manager.delete_profile(profile).await;
    });
}

/// The controller's question: agent-browser recovers when the guard's reconnect drops its
/// connection (its next command works, perhaps after one failure, which is printed).
#[test]
#[ignore]
fn agent_browser_recovers_after_its_connection_is_dropped() {
    common::run(async {
        let h = Harness::start();
        let (info, ep) = h.session("/counter").await;
        let r = h.exec(&info, &ep, &["get", "url"]).await;
        assert!(common::ok(&r), "{r:?}");
        assert!(h.manager.drop_guard_for_test(Duration::from_secs(2)));
        let back = common::eventually(15, || async { h.manager.require_guard().is_ok() }).await;
        assert!(back, "the guard did not come back");
        let mut outcomes = Vec::new();
        for _ in 0..3 {
            let r = h
                .runner
                .exec(&info.id, &ep, &common::s(&["get", "url"]), 30_000)
                .await;
            let ok = matches!(&r, Ok(r) if common::ok(r));
            outcomes.push(format!("{r:?}"));
            if ok {
                break;
            }
        }
        eprintln!("commands after the drop: {outcomes:#?}");
        assert!(
            outcomes.len() == 1,
            "agent-browser's first command after its connection was dropped did not work: {outcomes:#?}"
        );
    });
}

/// Why the shield also goes into running documents (`runImmediately`): a tab no session holds
/// any more (kept by a closed session) is not reloaded after a guard reconnect, so a document
/// it loaded while the guard was down (with no shield: it went with the old connection) gets
/// the shield where it stands. The control shows the copy goes through while the guard is down.
#[test]
#[ignore]
fn a_kept_tab_gets_the_shield_into_its_running_document_after_a_reconnect() {
    common::run(async {
        let clip = Clipboard::take();
        let h = Harness::start();
        let profile = "clip-kept";
        h.manager.create_profile(profile).await.unwrap();
        let a = create(&h, false, Some(profile)).await.unwrap();
        let t = a.first();
        h.manager
            .close(pegboard_protocol::SessionCloseParams {
                session_id: a.info.id.clone(),
                keep_tabs: true,
            })
            .await
            .unwrap();
        let conn = &h.browser.conn;
        let sid = conn.attach(&t).await.unwrap();
        // The kept tab left its session, and with it the daemon's focus emulation; it is
        // probably not the active tab of its window. Focused as a session tab would be, so a
        // click copies (spike C1, C3).
        for (method, params) in [
            (
                "Emulation.setFocusEmulationEnabled",
                json!({ "enabled": true }),
            ),
            ("Page.setWebLifecycleState", json!({ "state": "active" })),
        ] {
            conn.send(method, params, Some(&sid)).await.unwrap();
        }
        let tab = Tab { c: conn, sid };
        assert!(h.manager.drop_guard_for_test(Duration::from_secs(4)));
        // Chrome drops the guard's new-document scripts when it has read the closed
        // connection.
        tokio::time::sleep(Duration::from_secs(1)).await;
        tab.load("/clip").await;
        assert!(
            click_copy_changes(&tab, "#cp", &clip).await,
            "control: the document loaded while the guard was down did not copy"
        );
        let back = common::eventually(15, || async { h.manager.require_guard().is_ok() }).await;
        assert!(back, "the guard did not come back");
        assert_eq!(
            tab.eval("document.readyState", false).await,
            "complete",
            "the kept tab was reloaded or closed"
        );
        assert!(
            !click_copy_changes(&tab, "#cp", &clip).await,
            "the running document of the kept tab did not get the shield"
        );
        let _ = conn
            .send("Target.closeTarget", json!({ "targetId": t }), None)
            .await;
        let _ = h.manager.delete_profile(profile).await;
    });
}

/// Review C1: the guard's reloads of frames attached running are counted per frame, so a page
/// with many sandboxed srcdoc frames (each listening first) has every one covered, the last
/// as well as the first; and a frame that keeps coming back running is reloaded a few times
/// and then has its tab closed, never left running unshielded.
#[test]
#[ignore]
fn many_and_returning_sandboxed_frames_stay_covered() {
    common::run(async {
        let clip = Clipboard::take();
        let h = Harness::start();
        for allow in [true, false] {
            let a = agent(&h, allow).await;
            let tab = a.tab(&a.first()).await;
            tab.load("/sandbox-many").await;
            settle().await;
            let first = click_copy_changes(&tab, "#f0", &clip).await;
            let last = click_copy_changes(&tab, "#f9", &clip).await;
            eprintln!("many frames, allow={allow}: first {first}, last {last}");
            assert_eq!((first, last), (allow, allow), "allow={allow}");
            h.close_session(&a.info.id).await;
        }
        let a = agent(&h, false).await;
        let first = a.first();
        let tab = a.tab(&first).await;
        tab.load("/sandbox-loop").await;
        tokio::time::sleep(Duration::from_secs(4)).await;
        let open = page_targets(&h.browser.conn)
            .await
            .unwrap()
            .iter()
            .any(|t| t.target_id == first);
        // Closed is covered too (nothing to click); open, the frame's copy must not land.
        let changed = open && click_copy_changes(&tab, "#f", &clip).await;
        let log = h.manager.guard_attach_log();
        eprintln!(
            "returning frame: tab open={open}, copy changed={changed}, running frames: {:#?}",
            log.iter()
                .filter(|l| l.contains("iframe agent waiting=false"))
                .collect::<Vec<_>>()
        );
        assert!(!changed, "the returning frame's copy went through");
    });
}

/// The HTML of `/clip`'s copy area, to write into a document.
fn copy_area(listen_first: bool) -> String {
    let first = if listen_first {
        "<script>window.addEventListener('copy',function(e){e.stopImmediatePropagation()},true)</script>"
    } else {
        ""
    };
    format!(r#"{first}<textarea id="t">x</textarea><button id="cp" onclick="{COPY}">copy</button>"#)
}

/// Review C2: `document.open()` removes every listener of the document and its window and
/// makes no new global, so no new-document script runs again. The shield wraps `open`,
/// `write` and `writeln` so its listeners are back before anything written runs. Each way is
/// tried in a denied session (the copy must not land) and an allowed one (it must).
#[test]
#[ignore]
fn document_open_and_write_do_not_remove_the_shield() {
    common::run(async {
        let clip = Clipboard::take();
        let h = Harness::start();
        let js = |s: &str| serde_json::to_string(s).unwrap();
        let ways: Vec<(&str, String)> = vec![
            (
                "open, write, close",
                format!(
                    "document.open();document.write({});document.close();1",
                    js(&copy_area(false))
                ),
            ),
            (
                "write on the loaded document (opens it)",
                format!("document.write({});document.close();1", js(&copy_area(false))),
            ),
            (
                "writeln on the loaded document",
                format!("document.writeln({});document.close();1", js(&copy_area(false))),
            ),
            (
                "written content listens first",
                format!(
                    "document.open();document.write({});document.close();1",
                    js(&copy_area(true))
                ),
            ),
            (
                "open from another realm",
                format!(
                    "(()=>{{const f=document.createElement('iframe');document.body.appendChild(f);\
                     const D=f.contentWindow.Document.prototype;D.open.call(document);\
                     D.write.call(document,{});document.close();return 1}})()",
                    js(&copy_area(false))
                ),
            ),
            (
                "open from another realm, written content listening first",
                format!(
                    "(()=>{{const f=document.createElement('iframe');document.body.appendChild(f);\
                     const D=f.contentWindow.Document.prototype;D.open.call(document);\
                     D.write.call(document,{});document.close();return 1}})()",
                    js(&copy_area(true))
                ),
            ),
            (
                "a document made elsewhere, adopted",
                format!(
                    "(()=>{{const d=document.implementation.createHTMLDocument('');\
                     d.body.innerHTML={};\
                     document.replaceChild(document.adoptNode(d.documentElement),document.documentElement);return 1}})()",
                    js(&copy_area(false))
                ),
            ),
        ];
        for allow in [true, false] {
            let a = agent(&h, allow).await;
            let tab = a.tab(&a.first()).await;
            // Measurement: does a fresh same-origin frame's realm have the shield's wrappers
            // when a script reaches it right after creating it?
            tab.load("/clip").await;
            let wrapped = tab
                .eval(
                    "(()=>{const f=document.createElement('iframe');document.body.appendChild(f);\
                     return String(f.contentWindow.Document.prototype.open).includes('own(this)')})()",
                    false,
                )
                .await;
            eprintln!("allow={allow}: a fresh frame's realm has the wrapped open: {wrapped}");
            for (way, expr) in &ways {
                tab.load("/clip").await;
                let before = h.manager.guard_attach_log();
                let said = tab.eval(expr, false).await;
                settle().await;
                let changed = click_copy_changes(&tab, "#cp", &clip).await;
                eprintln!("{way}, allow={allow}: eval {said}, copy changed {changed}");
                let opened: Vec<String> = h
                    .manager
                    .guard_attach_log()
                    .into_iter()
                    .filter(|l| l.contains("document opened") && !before.contains(l))
                    .collect();
                for line in &opened {
                    eprintln!("  guard: {line}");
                }
                assert_eq!(changed, allow, "{way}: allow={allow} (eval said {said})");
                // Through the shield's wrappers (every realm a page reaches here has them),
                // the shield is first again at once: the guard leaves the document alone.
                assert!(
                    opened.iter().all(|l| l.contains("shield first")),
                    "{way}: the guard renewed a document the shield held: {opened:#?}"
                );
            }
            h.close_session(&a.info.id).await;
        }
    });
}

/// Resets a guard test hook when dropped, also when the test fails.
struct Reset<'a>(&'a Harness);

impl Drop for Reset<'_> {
    fn drop(&mut self) {
        self.0
            .manager
            .set_guard_early_delay_for_test(Duration::ZERO);
        self.0.manager.set_guard_fail_frame_shield_for_test(false);
        self.0.manager.set_guard_shield_wrappers_for_test(true);
    }
}

/// The guard's own check after `document.open()` (the backstop for an `open` that does not go
/// through the shield's wrappers; here the wrappers are left out by a test hook, so every
/// `open` gets past them). With content that listens first, the guard sees the page first and
/// renews the document; the copy does not land.
#[test]
#[ignore]
fn the_guard_renews_a_document_opened_past_the_shield() {
    common::run(async {
        let clip = Clipboard::take();
        let h = Harness::start();
        let _reset = Reset(&h);
        assert!(h.manager.set_guard_shield_wrappers_for_test(false));
        let a = agent(&h, false).await;
        let tab = a.tab(&a.first()).await;
        tab.load("/clip").await;
        let before = h.manager.guard_attach_log();
        let js = serde_json::to_string(&copy_area(true)).unwrap();
        tab.eval(
            &format!("document.open();document.write({js});document.close();1"),
            false,
        )
        .await;
        let renewed = common::eventually(10, || async {
            h.manager
                .guard_attach_log()
                .iter()
                .any(|l| !before.contains(l) && l.contains("page first") && l.contains("renewed"))
        })
        .await;
        let log: Vec<String> = h
            .manager
            .guard_attach_log()
            .into_iter()
            .filter(|l| l.contains("document opened") && !before.contains(l))
            .collect();
        eprintln!("{log:#?}");
        assert!(renewed, "the guard did not renew the document: {log:#?}");
        let loaded = common::eventually(10, || async {
            tab.eval("document.readyState", false).await == "complete"
                && tab
                    .eval("String(!!document.getElementById('cp'))", false)
                    .await
                    == "true"
        })
        .await;
        assert!(loaded, "the renewed page did not load");
        assert!(
            !click_copy_changes(&tab, "#cp", &clip).await,
            "the copy went through after the guard's renewal"
        );
    });
}

/// Review I1: `ready` comes back at most `EARLY_SETUP_LIMIT` (10 s) after a reconnect, but
/// the commands of a clipboard-denied session wait until the tabs that were there are
/// shielded and reloaded (here held 12 s by a test hook, as a busy renderer would).
#[test]
#[ignore]
fn commands_wait_for_the_old_tabs_after_a_reconnect() {
    common::run(async {
        let h = Harness::start();
        let _reset = Reset(&h);
        let a = agent(&h, false).await;
        let mut log = log_for(&h, &a.info);
        assert!(h
            .manager
            .set_guard_early_delay_for_test(Duration::from_secs(12)));
        assert!(h.manager.drop_guard_for_test(Duration::from_secs(1)));
        let back = common::eventually(15, || async { h.manager.require_guard().is_ok() }).await;
        assert!(back, "the guard did not come back");
        let e = match command(&h, &mut log, &a.info, &["get", "url"]).await {
            Ok(r) => panic!("a command ran before the old tabs were shielded: {r:?}"),
            Err(e) => e,
        };
        assert_eq!(e.code, codes::BROWSER_FAILED, "{e:?}");
        assert!(e.message.contains("clipboard policy"), "{e:?}");
        let done = common::eventually(20, || async {
            h.manager.require_clipboard(&a.info.id).is_ok()
        })
        .await;
        assert!(done, "the old tabs were never done");
    });
}

/// Review I2: a frame whose shield fails already runs (it was resumed first), so the tab that
/// holds it is closed (forced by a test hook).
#[test]
#[ignore]
fn a_frame_whose_shield_fails_closes_its_tab() {
    common::run(async {
        let h = Harness::start();
        let _reset = Reset(&h);
        let a = agent(&h, false).await;
        let first = a.first();
        let tab = a.tab(&first).await;
        tab.load("/clip").await;
        assert!(h.manager.set_guard_fail_frame_shield_for_test(true));
        let _ = h
            .browser
            .conn
            .send(
                "Runtime.evaluate",
                json!({ "expression": format!("location.href = {:?}; 1", url("/xsite")) }),
                Some(&h.browser.conn.attach(&first).await.unwrap()),
            )
            .await;
        let closed = common::eventually(15, || async {
            !page_targets(&h.browser.conn)
                .await
                .unwrap()
                .iter()
                .any(|t| t.target_id == first)
        })
        .await;
        let log = h.manager.guard_attach_log().join("\n");
        assert!(
            closed,
            "the tab of a frame without the shield is open:\n{log}"
        );
        assert!(log.contains("shield failed"), "{log}");
    });
}

/// Review I3: a denial whose connection was lost and given up on may still be in Chrome
/// (closing the connection does not lift it). A session allowing the clipboard in that
/// profile afterwards gets it lifted with a reset on a new connection.
#[test]
#[ignore]
fn a_lost_denial_is_lifted_for_a_session_that_allows_the_clipboard() {
    common::run(async {
        let clip = Clipboard::take();
        let h = Harness::start();
        let profile = "clip-lost";
        h.manager.create_profile(profile).await.unwrap();
        let denied = create(&h, false, Some(profile)).await.unwrap();
        assert_eq!(writes_in(&denied, &clip).await, (false, false));
        // Held down past the give-up (5 s).
        assert!(h
            .manager
            .drop_clipboard_policy_for_test(&denied.info.id, Duration::from_secs(7)));
        tokio::time::sleep(Duration::from_secs(8)).await;
        h.close_session(&denied.info.id).await;
        let allowed = create(&h, true, Some(profile)).await.unwrap();
        assert_eq!(
            writes_in(&allowed, &clip).await,
            (true, true),
            "the lost denial was not lifted (writeText, execCommand)"
        );
        h.close_session(&allowed.info.id).await;
        let _ = h.manager.delete_profile(profile).await;
    });
}
