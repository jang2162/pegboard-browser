//! End-to-end tests of the replay page (phase 2b), on the shared browser (see common): a
//! recording is planted in the shared home, its link comes from the shared replay server
//! (`Shared::replay`, the daemon's server in miniature), and the page is opened in the human
//! profile's tab through the test's own browser connection (the request guard does not judge
//! the human profile). Run with
//! `cargo test -p pegboard-core --test e2e_replay -- --ignored --test-threads=1`.
mod common;

use std::time::Duration;

use pegboard_core::actionlog::{write_manifest_file, ActionEntry, Manifest, ScreenStatus};
use pegboard_core::cdp::{page_targets, CdpConn};
use pegboard_core::recording::format::{
    ConsoleLine, FrameKind, IndexLine, NavEvent, NavLine, NetworkLine,
};
use serde_json::{json, Value};

const T1: &str = "0123456789ABCDEF0123456789ABCDEF";
const T2: &str = "FEDCBA9876543210FEDCBA9876543210";
const EVIL: &str = "<img src=x onerror=\"window.pwned=1\">";

fn ts(sec: u32) -> String {
    format!("2026-11-17T10:00:{sec:02}Z")
}

fn command(
    seq: u64,
    sec: u32,
    ok: bool,
    error: Option<&str>,
    frames: Option<(u32, u32)>,
) -> ActionEntry {
    ActionEntry {
        seq,
        ts: ts(sec),
        session_id: "s_abcdefgh".into(),
        target_id: Some(T1.into()),
        url: Some("https://example.test/".into()),
        command: "click".into(),
        args: vec!["@e1".into()],
        ok,
        error: error.map(String::from),
        frame_before: frames.map(|(b, _)| format!("{T1}/{b:06}.jpg")),
        frame_after: frames.map(|(_, a)| format!("{T1}/{a:06}.jpg")),
        ..ActionEntry::default()
    }
}

fn frame(sec: u32, n: u32) -> IndexLine {
    IndexLine {
        ts: ts(sec),
        file: Some(format!("{n:06}.jpg")),
        url: Some("https://example.test/".into()),
        width: Some(100),
        height: Some(100),
        kind: FrameKind::Change,
    }
}

fn marker(sec: u32, kind: FrameKind) -> IndexLine {
    IndexLine {
        ts: ts(sec),
        file: None,
        url: None,
        width: None,
        height: None,
        kind,
    }
}

/// A new recording id (`r_` and eight letters).
fn new_id() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let body: String = (0..8)
        .map(|i| (b'a' + ((n >> (5 * i)) % 26) as u8) as char)
        .collect();
    format!("r_{body}")
}

struct Planted<'a> {
    name: &'a str,
    screen: ScreenStatus,
    actions: Vec<ActionEntry>,
    /// Index lines of tab T1; every frame file named in them gets `jpeg` unless listed in
    /// `missing`.
    index: Vec<IndexLine>,
    missing: Vec<u32>,
    navigation: Vec<NavLine>,
}

/// Writes the recording under the shared home; its id.
fn plant(p: Planted, jpeg: &[u8]) -> String {
    let sh = common::shared();
    let id = new_id();
    let dir = sh.env.paths.recordings.join(&id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut m = Manifest::new(&id, "s_abcdefgh", p.name, "PegboardAgent", &ts(0), "c", "a");
    m.ended_at = Some(ts(59));
    m.screen = p.screen;
    write_manifest_file(&dir, &m).unwrap();
    let lines = |v: Vec<String>| v.into_iter().map(|l| l + "\n").collect::<String>();
    std::fs::write(
        dir.join("actions.jsonl"),
        lines(
            p.actions
                .iter()
                .map(|a| serde_json::to_string(a).unwrap())
                .collect(),
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("navigation.jsonl"),
        lines(
            p.navigation
                .iter()
                .map(|n| serde_json::to_string(n).unwrap())
                .collect(),
        ),
    )
    .unwrap();
    if p.screen != ScreenStatus::Pruned {
        let tab = dir.join("screen").join(T1);
        std::fs::create_dir_all(&tab).unwrap();
        std::fs::write(
            tab.join("index.jsonl"),
            lines(
                p.index
                    .iter()
                    .map(|l| serde_json::to_string(l).unwrap())
                    .collect(),
            ),
        )
        .unwrap();
        for l in &p.index {
            if let Some(f) = &l.file {
                let n: u32 = f.trim_end_matches(".jpg").parse().unwrap();
                if !p.missing.contains(&n) {
                    std::fs::write(tab.join(f), jpeg).unwrap();
                }
            }
        }
    }
    id
}

/// Adds a second tab (`T2`, with its index and frames) and network and console lines to a
/// planted recording.
fn plant_more(
    id: &str,
    t2_index: &[IndexLine],
    network: &[NetworkLine],
    console: &[ConsoleLine],
    jpeg: &[u8],
) {
    let dir = common::shared().env.paths.recordings.join(id);
    let lines = |v: Vec<String>| v.into_iter().map(|l| l + "\n").collect::<String>();
    let tab = dir.join("screen").join(T2);
    std::fs::create_dir_all(&tab).unwrap();
    std::fs::write(
        tab.join("index.jsonl"),
        lines(
            t2_index
                .iter()
                .map(|l| serde_json::to_string(l).unwrap())
                .collect(),
        ),
    )
    .unwrap();
    for f in t2_index.iter().filter_map(|l| l.file.as_ref()) {
        std::fs::write(tab.join(f), jpeg).unwrap();
    }
    std::fs::write(
        dir.join("network.jsonl"),
        lines(
            network
                .iter()
                .map(|l| serde_json::to_string(l).unwrap())
                .collect(),
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("console.jsonl"),
        lines(
            console
                .iter()
                .map(|l| serde_json::to_string(l).unwrap())
                .collect(),
        ),
    )
    .unwrap();
}

/// The human profile's tab and a CDP session on it.
async fn human_tab(conn: &CdpConn) -> String {
    let sh = common::shared();
    let target = page_targets(conn)
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.browser_context_id.as_deref() == Some(sh.browser.human_context.as_str()))
        .expect("no human page")
        .target_id;
    conn.attach(&target).await.unwrap()
}

/// A real JPEG (64x40), made once and kept with the tests: a capture of the human tab right
/// after the shared browser started could fail with "Not attached to an active page" (the
/// first test of the binary), and the frames' content does not matter here.
const JPEG: &[u8] = include_bytes!("shared/replay-frame.jpg");

/// Puts the human tab on about:blank, ready to be scripted, and gives the frame bytes.
async fn a_jpeg(conn: &CdpConn, session: &str) -> Vec<u8> {
    conn.navigate(session, "about:blank").await.unwrap();
    assert_eq!(
        until(
            conn,
            session,
            "document.readyState === 'complete' ? true : null"
        )
        .await,
        json!(true)
    );
    assert_eq!(&JPEG[..3], &[0xFF, 0xD8, 0xFF], "not a JPEG");
    JPEG.to_vec()
}

/// Opens the replay page of `id` in the human tab and waits until it has drawn itself.
async fn open_page(conn: &CdpConn, session: &str, id: &str) {
    let (url, _) = common::shared().replay.grant(id);
    conn.navigate(session, &url).await.unwrap();
    assert_eq!(
        until(conn, session, "document.body.dataset.ready || null").await,
        json!("true")
    );
}

/// Polls `expr` until it is not null (at most 10 s); its value.
async fn until(conn: &CdpConn, session: &str, expr: &str) -> Value {
    for _ in 0..100 {
        let v = conn.evaluate(session, expr).await.unwrap();
        if !v.is_null() {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{expr} stayed null");
}

/// The name of the frame the page shows once it has loaded (naturalWidth > 0), else null.
const LOADED_FRAME: &str = "(() => { const i = document.getElementById('frame'); \
     return !i.hidden && i.complete && i.naturalWidth > 0 ? i.dataset.name : null; })()";

/// Why the page shows no frame, once it says so: `[reason, text]`.
const NO_FRAME: &str = "(() => { const p = document.getElementById('no-frame'); \
     return p.hidden ? null : [p.dataset.reason, p.textContent]; })()";

async fn click(conn: &CdpConn, session: &str, selector: &str) {
    let expr = format!(
        "(() => {{ const e = document.querySelector('{selector}'); if (!e) return false; e.click(); return true; }})()"
    );
    assert_eq!(
        conn.evaluate(session, &expr).await.unwrap(),
        json!(true),
        "{selector}"
    );
}

#[test]
#[ignore]
fn replay_page_starts_at_the_first_incident() {
    common::run(async {
        let sh = common::shared();
        let conn = &sh.browser.conn;
        let scope = common::PageScope::begin(conn).await;
        let session = human_tab(conn).await;
        let jpeg = a_jpeg(conn, &session).await;
        let id = plant(
            Planted {
                name: "first incident",
                screen: ScreenStatus::Recorded,
                actions: vec![
                    command(1, 1, true, None, Some((1, 2))),
                    command(2, 3, false, Some("timeout: no element"), Some((3, 4))),
                    command(3, 5, false, Some("policy_denied: no"), None),
                ],
                index: (1..=4).map(|n| frame(n, n)).collect(),
                missing: vec![],
                navigation: vec![],
            },
            &jpeg,
        );
        open_page(conn, &session, &id).await;
        let state = conn
            .evaluate(
                &session,
                "[document.body.dataset.seq, document.body.dataset.side]",
            )
            .await
            .unwrap();
        assert_eq!(state, json!(["2", "before"]));
        assert_eq!(
            until(conn, &session, LOADED_FRAME).await,
            json!(format!("{T1}/000003.jpg"))
        );
        // The list and the timeline mark the selected incident.
        let current = conn
            .evaluate(
                &session,
                "[...document.querySelectorAll('[aria-current=\"true\"]')].map(e => e.dataset.seq)",
            )
            .await
            .unwrap();
        assert_eq!(current, json!(["2", "2"]));
        // Its timeline mark: once the screen before, once more the screen after.
        click(conn, &session, "#timeline .mark[data-seq=\"2\"]").await;
        assert_eq!(
            until(conn, &session, LOADED_FRAME).await,
            json!(format!("{T1}/000003.jpg"))
        );
        click(conn, &session, "#timeline .mark[data-seq=\"2\"]").await;
        let after = format!(
            "(() => {{ const n = {LOADED_FRAME}; return n === '{T1}/000004.jpg' ? n : null; }})()"
        );
        assert_eq!(
            until(conn, &session, &after).await,
            json!(format!("{T1}/000004.jpg"))
        );
        assert_eq!(
            conn.evaluate(&session, "document.body.dataset.side")
                .await
                .unwrap(),
            json!("after")
        );
        // The header counts the failure and the refusal.
        let summary = conn
            .evaluate(&session, "document.getElementById('summary').textContent")
            .await
            .unwrap();
        let summary = summary.as_str().unwrap();
        assert!(
            summary.contains("명령 3") && summary.contains("실패 1") && summary.contains("거부 1"),
            "{summary}"
        );
        // The recording is deleted while the page is open: the next frame it asks for is
        // refused, and the page says the link no longer works (not "화면 없음").
        std::fs::remove_dir_all(sh.env.paths.recordings.join(&id)).unwrap();
        click(conn, &session, "#actions li[data-seq=\"1\"]").await;
        let error = until(
            conn,
            &session,
            "document.body.dataset.ready === 'error' ? document.getElementById('error').textContent : null",
        )
        .await;
        assert!(
            error.as_str().unwrap().contains("녹화를 열 수 없습니다"),
            "{error}"
        );
        conn.navigate(&session, "about:blank").await.unwrap();
        scope.finish(conn).await;
    });
}

#[test]
#[ignore]
fn replay_page_shows_page_text_as_text() {
    common::run(async {
        let sh = common::shared();
        let conn = &sh.browser.conn;
        let scope = common::PageScope::begin(conn).await;
        let session = human_tab(conn).await;
        let jpeg = a_jpeg(conn, &session).await;
        let mut failed = command(1, 2, false, Some(&format!("timeout: {EVIL}")), Some((1, 1)));
        failed.url = Some(format!("https://example.test/{EVIL}"));
        failed.args = vec![EVIL.into()];
        let human = ActionEntry {
            seq: 2,
            ..ActionEntry::human("s_abcdefgh", &ts(4), T1, None, EVIL, false)
        };
        let mut index = vec![frame(1, 1)];
        index[0].url = Some(EVIL.into());
        let id = plant(
            Planted {
                name: EVIL,
                screen: ScreenStatus::Recorded,
                actions: vec![failed, human],
                index,
                missing: vec![],
                navigation: vec![NavLine {
                    ts: ts(3),
                    target_id: T1.into(),
                    event: NavEvent::Navigated,
                    url: Some(EVIL.into()),
                    opener: None,
                }],
            },
            &jpeg,
        );
        open_page(conn, &session, &id).await;
        until(conn, &session, LOADED_FRAME).await;
        // The human entry too (its tab label holds the navigated URL).
        click(conn, &session, "#actions li[data-seq=\"2\"]").await;
        tokio::time::sleep(Duration::from_millis(1000)).await;
        let checks = conn
            .evaluate(
                &session,
                "({ pwned: typeof window.pwned, imgs: document.querySelectorAll('img').length, \
                   frameImgs: document.querySelectorAll('img#frame').length, \
                   text: document.body.innerText })",
            )
            .await
            .unwrap();
        assert_eq!(checks["pwned"], "undefined", "{checks}");
        assert_eq!(checks["imgs"], 1, "{checks}");
        assert_eq!(checks["frameImgs"], 1, "{checks}");
        let text = checks["text"].as_str().unwrap();
        // The session name, the error, the argument and what the person did, as text.
        assert!(text.matches(EVIL).count() >= 4, "{text}");
        conn.navigate(&session, "about:blank").await.unwrap();
        scope.finish(conn).await;
    });
}

#[test]
#[ignore]
fn replay_page_marks_screen_off_missing_and_pruned() {
    common::run(async {
        let sh = common::shared();
        let conn = &sh.browser.conn;
        let scope = common::PageScope::begin(conn).await;
        let session = human_tab(conn).await;
        let jpeg = a_jpeg(conn, &session).await;
        let id = plant(
            Planted {
                name: "screen states",
                screen: ScreenStatus::Capped,
                actions: vec![
                    // 1: after an off marker.
                    command(1, 3, true, None, None),
                    // 2: names a frame whose file never came.
                    command(2, 5, true, None, Some((9, 9))),
                    // 3: its tab navigated after the last frame.
                    command(3, 8, true, None, None),
                    // 4: after the cap.
                    command(4, 11, true, None, None),
                    // 5: a frame (control).
                    command(5, 6, true, None, None),
                ],
                index: vec![
                    frame(1, 1),
                    marker(2, FrameKind::Off),
                    frame(6, 2),
                    frame(9, 9),
                    marker(10, FrameKind::Capped),
                ],
                missing: vec![9],
                navigation: vec![NavLine {
                    ts: ts(7),
                    target_id: T1.into(),
                    event: NavEvent::Navigated,
                    url: Some("https://example.test/next".into()),
                    opener: None,
                }],
            },
            &jpeg,
        );
        open_page(conn, &session, &id).await;
        // No incident: the first action (by time), which is after the off marker.
        assert_eq!(
            until(conn, &session, NO_FRAME).await,
            json!(["off", "화면 기록 꺼짐"])
        );
        click(conn, &session, "#actions li[data-seq=\"2\"]").await;
        assert_eq!(
            until(conn, &session, NO_FRAME).await,
            json!(["missing", "화면 없음"])
        );
        click(conn, &session, "#actions li[data-seq=\"5\"]").await;
        assert_eq!(
            until(conn, &session, LOADED_FRAME).await,
            json!(format!("{T1}/000002.jpg"))
        );
        click(conn, &session, "#actions li[data-seq=\"3\"]").await;
        assert_eq!(
            until(conn, &session, NO_FRAME).await,
            json!(["missing", "화면 없음"])
        );
        click(conn, &session, "#actions li[data-seq=\"4\"]").await;
        assert_eq!(
            until(conn, &session, NO_FRAME).await,
            json!(["capped", "화면 기록 멈춤(녹화 하나 1GB)"])
        );

        // A recording whose screen was pruned: every frame is gone.
        let pruned = plant(
            Planted {
                name: "pruned",
                screen: ScreenStatus::Pruned,
                actions: vec![command(1, 1, false, Some("timeout: x"), Some((1, 2)))],
                index: vec![],
                missing: vec![],
                navigation: vec![],
            },
            &jpeg,
        );
        open_page(conn, &session, &pruned).await;
        assert_eq!(
            until(conn, &session, NO_FRAME).await,
            json!(["pruned", "화면 지워짐"])
        );
        let summary = conn
            .evaluate(&session, "document.getElementById('summary').textContent")
            .await
            .unwrap();
        assert!(
            summary.as_str().unwrap().contains("화면 지워짐"),
            "{summary}"
        );
        conn.navigate(&session, "about:blank").await.unwrap();
        scope.finish(conn).await;
    });
}

/// `ts` at second `t` (`"03"`, `"01.5"`).
fn at(t: &str) -> String {
    format!("2026-11-17T10:00:{t}Z")
}

async fn key(conn: &CdpConn, session: &str, key: &str) {
    let expr = format!(
        "document.dispatchEvent(new KeyboardEvent('keydown', {{ key: '{key}', bubbles: true }})); true"
    );
    assert_eq!(conn.evaluate(session, &expr).await.unwrap(), json!(true));
}

/// Waits until the page shows frame `want` loaded, or (for "off", "capped", "missing",
/// "pruned") says that it shows none for that reason.
async fn shows(conn: &CdpConn, session: &str, want: &str) {
    let expr = format!(
        "(() => {{ const i = document.getElementById('frame'); const p = document.getElementById('no-frame'); \
         const got = !i.hidden ? (i.complete && i.naturalWidth > 0 ? i.dataset.name : null) : (p.hidden ? null : p.dataset.reason); \
         return got === '{want}' ? got : null; }})()"
    );
    for _ in 0..100 {
        if !conn.evaluate(session, &expr).await.unwrap().is_null() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let state = conn
        .evaluate(
            session,
            "[document.body.dataset.seq, document.body.dataset.side, document.body.dataset.tab, \
              document.getElementById('frame').dataset.name, document.getElementById('no-frame').dataset.reason]",
        )
        .await
        .unwrap();
    panic!("the page does not show {want}: {state}");
}

/// The lines of the network/console box: `<kind>:<text>`.
const EXTRAS: &str =
    "[...document.querySelectorAll('#extra-lines li')].map(l => l.dataset.kind + ':' + l.textContent)";

#[test]
#[ignore]
fn replay_page_follows_tabs_keys_extras_and_same_time_rules() {
    common::run(async {
        let sh = common::shared();
        let conn = &sh.browser.conn;
        let scope = common::PageScope::begin(conn).await;
        let session = human_tab(conn).await;
        let jpeg = a_jpeg(conn, &session).await;
        let human = |seq: u64, ts: String, target: &str| ActionEntry {
            seq,
            ..ActionEntry::human("s_abcdefgh", &ts, target, None, "clicked", false)
        };
        let mut fill = command(1, 2, true, None, Some((1, 1)));
        fill.command = "fill".into();
        fill.args = vec!["@e1".into(), "[redacted]".into()];
        fill.redacted = true;
        let mut late = frame(0, 3);
        late.ts = at("03.5");
        let nav = |ts: String, target: &str, url: &str| NavLine {
            ts,
            target_id: target.into(),
            event: NavEvent::Navigated,
            url: Some(url.into()),
            opener: None,
        };
        let id = plant(
            Planted {
                name: "two tabs",
                screen: ScreenStatus::Recorded,
                // seq 4 is written last but happened first, in T2.
                actions: vec![
                    fill,
                    human(2, at("03"), T1),
                    human(3, at("04"), T1),
                    human(4, at("01"), T2),
                ],
                index: vec![frame(1, 1), late],
                missing: vec![],
                navigation: vec![
                    nav(at("00.5"), T1, "https://one.test/"),
                    nav(at("03"), T1, "https://one.test/next"),
                    nav(at("03"), T2, "https://two.test/"),
                ],
            },
            &jpeg,
        );
        let mut early = frame(0, 1);
        early.ts = at("01.5");
        plant_more(
            &id,
            &[early, frame(3, 2)],
            &[NetworkLine {
                ts: at("01"),
                target_id: T2.into(),
                method: "GET".into(),
                url: "https://two.test/api".into(),
                status: Some(200),
                bytes: None,
                duration_ms: None,
                error: None,
            }],
            &[ConsoleLine {
                ts: at("03.5"),
                target_id: T1.into(),
                level: "warn".into(),
                text: "late warning".into(),
            }],
            &jpeg,
        );
        let t1 = |n: u32| format!("{T1}/{n:06}.jpg");
        let t2 = |n: u32| format!("{T2}/{n:06}.jpg");
        let one_extra = |got: &Value, kind: &str, text: &str| {
            let lines = got.as_array().unwrap();
            assert!(
                lines.len() == 1
                    && lines[0].as_str().unwrap().starts_with(&format!("{kind}:"))
                    && lines[0].as_str().unwrap().contains(text),
                "{got}"
            );
        };
        open_page(conn, &session, &id).await;
        // The first incident by time: seq 4 at :01 in T2, whose first frame is at :01.5.
        assert_eq!(
            conn.evaluate(
                &session,
                "[document.body.dataset.seq, document.body.dataset.tab]"
            )
            .await
            .unwrap(),
            json!(["4", T2])
        );
        shows(conn, &session, "missing").await;
        one_extra(
            &conn.evaluate(&session, EXTRAS).await.unwrap(),
            "network",
            "two.test/api",
        );
        // Its mark twice: the screen after (at the next action, :02).
        click(conn, &session, "#timeline .mark[data-seq=\"4\"]").await;
        click(conn, &session, "#timeline .mark[data-seq=\"4\"]").await;
        shows(conn, &session, &t2(1)).await;
        // The other tab at the same moment.
        click(
            conn,
            &session,
            &format!("#tabs button[data-target=\"{T1}\"]"),
        )
        .await;
        shows(conn, &session, &t1(1)).await;
        // Arrow right: seq 1, the fill, with its lock.
        key(conn, &session, "ArrowRight").await;
        shows(conn, &session, &t1(1)).await;
        let line = conn
            .evaluate(
                &session,
                "document.querySelector('#actions li[data-seq=\"1\"] .what').textContent",
            )
            .await
            .unwrap();
        assert_eq!(line, json!("fill @e1 [redacted] 🔒"));
        // Arrow right: seq 2 at :03 in T1, which navigated at :03 after its :01 frame.
        key(conn, &session, "ArrowRight").await;
        assert_eq!(
            conn.evaluate(&session, "document.body.dataset.seq")
                .await
                .unwrap(),
            json!("2")
        );
        shows(conn, &session, "missing").await;
        one_extra(
            &conn.evaluate(&session, EXTRAS).await.unwrap(),
            "console",
            "late warning",
        );
        click(conn, &session, "#after").await;
        shows(conn, &session, &t1(3)).await;
        // T2 at :04, then at :03, where a frame and a navigation have the same time: the
        // frame shows the new page.
        click(
            conn,
            &session,
            &format!("#tabs button[data-target=\"{T2}\"]"),
        )
        .await;
        shows(conn, &session, &t2(2)).await;
        click(conn, &session, "#before").await;
        shows(conn, &session, &t2(2)).await;
        // The tab buttons are named by each tab's address at that moment.
        let labels = conn
            .evaluate(
                &session,
                "[...document.querySelectorAll('#tabs button')].map(b => b.textContent)",
            )
            .await
            .unwrap();
        assert_eq!(labels, json!(["1: one.test", "2: two.test"]));
        // Arrow left twice: back at seq 4.
        key(conn, &session, "ArrowLeft").await;
        key(conn, &session, "ArrowLeft").await;
        assert_eq!(
            conn.evaluate(&session, "document.body.dataset.seq")
                .await
                .unwrap(),
            json!("4")
        );
        conn.navigate(&session, "about:blank").await.unwrap();
        scope.finish(conn).await;
    });
}
