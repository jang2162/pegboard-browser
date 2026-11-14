//! The system clipboard (macOS `pbcopy`/`pbpaste`), saved once per test binary and put back
//! after every test, also when the test failed. Shared by pegboard-core's `e2e_clipboard`
//! and pegboard-cli's `e2e_mcp_isolation` (included with `#[path]`).
//!
//! A clipboard that holds anything but plain text is never overwritten: the test refuses to
//! run and says why. A clipboard that holds a marker a test writes (left by an earlier test
//! binary) is not taken as the original: it is emptied at the end instead.
#![allow(dead_code)]

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

static CLIPBOARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What the clipboard held when the first test of this binary took it.
static ORIGINAL: OnceLock<(Vec<u8>, bool)> = OnceLock::new();
/// Restores that did not give back what was saved (checked by `assert_as_before_the_run`).
static RESTORE_FAILURES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Prefixes of the texts the tests put into the clipboard or make pages copy; each is
/// followed by a number (a timestamp).
const MARKER_PREFIXES: [&str; 7] = [
    "PEGBOARD-CLIP-MARKER-",
    "COPIED-",
    "CUT-",
    "WRITTEN-",
    "EVAL-",
    "HANDLER-",
    "SELECTED-",
];

/// Whether `text` is one of the tests' own markers: a known prefix and then digits only.
pub fn is_test_marker(text: &str) -> bool {
    let text = text.trim();
    MARKER_PREFIXES.iter().any(|p| {
        text.strip_prefix(p)
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// The system clipboard for the length of a test: the original text is put back on drop.
pub struct Clipboard {
    saved: Vec<u8>,
    /// It held nothing at all (not even empty text), or a test's marker: it is emptied
    /// again, not given an empty text item.
    was_empty: bool,
    _one: std::sync::MutexGuard<'static, ()>,
}

pub fn pasteboard(command: &str, input: Option<&[u8]>) -> Vec<u8> {
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
pub fn non_text(info: &str) -> Option<String> {
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
    /// The clipboard for one test. The first test of the binary saves what the clipboard
    /// holds (or panics without touching it when that is not plain text); every test puts
    /// THAT back when it ends, so a late write of one test cannot become the "original" of
    /// the next. A test that finds something else there (a write that landed after the
    /// previous restore) puts the original back first and records it.
    pub fn take() -> Clipboard {
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
        let mut saved = pasteboard("pbpaste", None);
        let mut was_empty = info.is_empty();
        if is_test_marker(&String::from_utf8_lossy(&saved)) {
            eprintln!(
                "WARNING: the clipboard holds a test marker ({:?}), left by an earlier run; it is \
                 not kept as the original: the clipboard is emptied when the tests end",
                String::from_utf8_lossy(&saved)
            );
            saved.clear();
            was_empty = true;
        }
        let _ = ORIGINAL.set((saved.clone(), was_empty));
        Clipboard {
            saved,
            was_empty,
            _one: one,
        }
    }

    pub fn set(&self, text: &str) {
        pasteboard("pbcopy", Some(text.as_bytes()));
    }

    pub fn get(&self) -> String {
        String::from_utf8_lossy(&pasteboard("pbpaste", None)).into_owned()
    }

    /// A marker no other run uses, put into the clipboard.
    pub fn mark(&self) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let marker = format!("PEGBOARD-CLIP-MARKER-{nanos}");
        self.set(&marker);
        marker
    }
}

pub fn clipboard_types() -> String {
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

/// For the binary's last test (named `zz_...`, tests run in name order): every test gave the
/// clipboard back, and it holds what it held before the first test of this binary. Puts it
/// back in any case before failing.
pub fn assert_as_before_the_run() {
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

#[test]
fn test_markers_are_recognised() {
    for marker in [
        "PEGBOARD-CLIP-MARKER-1794646970123456789",
        "COPIED-1794646970123",
        "CUT-1794646970123",
        "WRITTEN-1794646970123",
        "EVAL-1794646970123",
        "HANDLER-1794646970123",
        "SELECTED-1794646970123",
        "COPIED-1794646970123\n",
    ] {
        assert!(is_test_marker(marker), "{marker:?}");
    }
    for text in [
        "",
        "hello",
        "COPIED-",
        "COPIED-abc",
        "copied-123",
        "PEGBOARD-CLIP-MARKER-12 and more",
        "see COPIED-123",
    ] {
        assert!(!is_test_marker(text), "{text:?}");
    }
}
