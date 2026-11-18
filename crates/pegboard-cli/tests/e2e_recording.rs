//! End-to-end tests of the screen recording (phase 2b). Every test starts a browser, so all
//! are `#[ignore]`d: run them with
//! `cargo test -p pegboard-cli --test e2e_recording -- --ignored --test-threads=1`.
//!
//! The recording's files are read straight from `<home>/recordings/<id>/`.

mod common;

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::*;
use serde_json::{json, Value};

fn create(home: &Home, url: &str) -> String {
    let (code, out, err) = home.run(&["session", "create", "--url", url]);
    assert_eq!(code, 0, "{err}");
    session_handle(&out)
}

fn close(home: &Home, handle: &str) {
    let (code, _, err) = home.run(&["session", "close", handle]);
    assert_eq!(code, 0, "{err}");
}

/// An agent-browser command in the session; its output.
fn ok(home: &Home, handle: &str, args: &[&str]) -> String {
    let (code, out, err) = home.run_limited(&[("PEGBOARD_SESSION", handle)], args, 90);
    assert_eq!(code, 0, "{args:?}: {err}");
    out
}

fn eval(home: &Home, handle: &str, expr: &str) -> Value {
    let (code, out, err) = home.run(&["--session", handle, "--json", "eval", expr]);
    assert_eq!(code, 0, "{err}");
    json(&out)["data"]["result"].clone()
}

/// The session's targets, as `session info` lists them.
fn targets(home: &Home, handle: &str) -> Vec<String> {
    let (code, out, err) = home.run(&["--json", "session", "info", handle]);
    assert_eq!(code, 0, "{err}");
    json(&out)["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect()
}

/// (targetId, url) of the session's tabs as `tab list` shows them.
fn tabs(home: &Home, handle: &str) -> Vec<(String, String)> {
    let out = ok(home, handle, &["tab", "list"]);
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

fn recording_dir(home: &Home, handle: &str) -> PathBuf {
    home.path()
        .join("recordings")
        .join(format!("r_{}", &id_of(handle)[2..]))
}

fn manifest(dir: &Path) -> Value {
    json(&std::fs::read_to_string(dir.join("manifest.json")).unwrap())
}

/// The target directories under `screen/`.
fn screen_dirs(dir: &Path) -> BTreeSet<String> {
    match std::fs::read_dir(dir.join("screen")) {
        Ok(entries) => entries
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => BTreeSet::new(),
    }
}

fn index(dir: &Path, target: &str) -> Vec<Value> {
    let path = dir.join("screen").join(target).join("index.jsonl");
    std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .map(json)
        .collect()
}

fn jpgs(dir: &Path, target: &str) -> BTreeSet<String> {
    std::fs::read_dir(dir.join("screen").join(target))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".jpg"))
        .collect()
}

/// Every index line with a file names an existing file, and every file has its line.
fn assert_index_matches_files(dir: &Path) {
    for target in screen_dirs(dir) {
        let named: BTreeSet<String> = index(dir, &target)
            .iter()
            .filter_map(|l| l["file"].as_str().map(String::from))
            .collect();
        let lines_with_file = index(dir, &target)
            .iter()
            .filter(|l| l["file"].is_string())
            .count();
        assert_eq!(named.len(), lines_with_file, "{target}: a file named twice");
        assert_eq!(named, jpgs(dir, &target), "{target}");
    }
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Milliseconds since the epoch of an RFC 3339 UTC time (`2026-11-17T10:00:00.123Z`).
fn ts_ms(ts: &str) -> i64 {
    let (date, time) = ts.split_once('T').unwrap();
    let mut d = date.split('-').map(|x| x.parse::<i64>().unwrap());
    let (y, m, day) = (d.next().unwrap(), d.next().unwrap(), d.next().unwrap());
    let time = time.trim_end_matches('Z');
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.split(':').map(|x| x.parse::<i64>().unwrap());
    let (h, min, s) = (t.next().unwrap(), t.next().unwrap(), t.next().unwrap());
    let ms: i64 = format!("{frac:0<3}")[..3].parse().unwrap();
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    ((days * 86_400 + h * 3600 + min * 60 + s) * 1000) + ms
}

#[test]
fn ts_ms_reads_rfc3339() {
    assert_eq!(ts_ms("1970-01-01T00:00:00Z"), 0);
    assert_eq!(ts_ms("2026-11-17T10:00:00.123456789Z"), 1_794_909_600_123);
    assert_eq!(ts_ms("2000-03-01T00:00:01.5Z"), 951_868_801_500);
}

/// What the recorder told daemon.log about `target` (test hooks are on in the test homes).
fn recorder_log(home: &Home, target: &str) -> Vec<String> {
    std::fs::read_to_string(home.path().join("daemon.log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("recorder") && l.contains(target))
        .map(String::from)
        .collect()
}

fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or("");
    authority.split(':').next().unwrap_or("").to_string()
}

/// The `change` lines of a target's index that carry a file.
fn changes(dir: &Path, target: &str) -> Vec<Value> {
    index(dir, target)
        .into_iter()
        .filter(|l| l["kind"] == "change" && l["file"].is_string())
        .collect()
}

#[test]
#[ignore]
fn screen_frames_are_recorded_by_default() {
    let origin = fixture();
    let home = shared_home();
    let started = Instant::now();
    let h = create(home, &format!("{origin}/animated"));
    std::thread::sleep(Duration::from_secs(3));
    let first = targets(home, &h)[0].clone();
    close(home, &h);
    let secs = started.elapsed().as_secs_f64();

    let dir = recording_dir(home, &h);
    let m = manifest(&dir);
    assert_eq!(m["format"], 2, "{m}");
    assert_eq!(m["screen"], "recorded", "{m}");
    assert!(
        screen_dirs(&dir).contains(&first),
        "{:?}",
        screen_dirs(&dir)
    );
    assert_index_matches_files(&dir);
    let frames = changes(&dir, &first);
    assert!(frames.len() >= 2, "{frames:?}");
    assert!(
        frames.len() as f64 <= 2.0 * secs + 2.0,
        "{} change frames in {secs:.1} s",
        frames.len()
    );
    // At most two a second: no second holds more than two frames, plus the settled one a
    // throttle hands out later with the time it arrived.
    let times: Vec<i64> = frames
        .iter()
        .map(|f| ts_ms(f["ts"].as_str().unwrap()))
        .collect();
    for (i, t) in times.iter().enumerate() {
        let within = times[i..].iter().filter(|u| **u < t + 1000).count();
        assert!(
            within <= 3,
            "{within} frames within 1 s from {t}: {frames:?}"
        );
    }
    for f in &frames {
        assert!(f["width"].as_u64().unwrap() > 0, "{f}");
        assert!(f["height"].as_u64().unwrap() > 0, "{f}");
        assert!(f["url"].as_str().unwrap().ends_with("/animated"), "{f}");
    }
    assert_eq!(mode(&dir.join("screen")), 0o700);
    assert_eq!(mode(&dir.join("screen").join(&first)), 0o700);
    assert_eq!(
        mode(&dir.join("screen").join(&first).join("index.jsonl")),
        0o600
    );
    for f in jpgs(&dir, &first) {
        assert_eq!(
            mode(&dir.join("screen").join(&first).join(&f)),
            0o600,
            "{f}"
        );
    }
}

#[test]
#[ignore]
fn the_settled_screen_is_the_last_frame() {
    let origin = fixture();
    let home = shared_home();
    let h = create(home, &format!("{origin}/settle"));
    let first = targets(home, &h)[0].clone();
    // The recording runs by now; the page changes five times within 300 ms and stops.
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(eval(home, &h, "start()"), "started");
    std::thread::sleep(Duration::from_secs(2));
    let settled = eval(home, &h, "window.settledAt").as_i64().unwrap();
    close(home, &h);

    let dir = recording_dir(home, &h);
    let frames = changes(&dir, &first);
    let last = frames.last().unwrap_or_else(|| {
        panic!(
            "no change frames; index {:?}; recorder: {:?}",
            index(&dir, &first),
            recorder_log(home, &first)
        )
    });
    let last_ts = ts_ms(last["ts"].as_str().unwrap());
    assert!(
        last_ts >= settled,
        "the last frame ({last_ts}) is older than the last change ({settled}): {frames:?}"
    );
    // A frame came during the changes too, so the last one is not merely the only one.
    assert!(frames.len() >= 2, "{frames:?}");
    // Never painted contentfully and never on an off page: the attach opened at once.
    let log = recorder_log(home, &first);
    assert!(log.iter().any(|l| l.contains("stream open")), "{log:?}");
}

#[test]
#[ignore]
fn human_profile_tab_is_never_recorded() {
    let origin = fixture();
    let home = screen_home(SCREEN_OFF_CONFIG);
    let human = Human::attach(&home);
    // The human's tab changes all the time: any frame of it would be written.
    human.navigate(&human.target, &format!("{origin}/animated"));

    let h = create(&home, &format!("{origin}/popup"));
    ok(&home, &h, &["click", "#pop"]);
    ok(&home, &h, &["tab", "new", &format!("{origin}/animated")]);
    std::thread::sleep(Duration::from_secs(2));
    let session_targets: BTreeSet<String> = targets(&home, &h).into_iter().collect();
    assert!(session_targets.len() >= 3, "{session_targets:?}");
    assert!(!session_targets.contains(&human.target));

    // Asked straight, the recorder refuses the human's tab.
    let paths = pegboard_core::paths::paths_in(home.path());
    let r: Value = pegboard_core::api::Client::connect(&paths)
        .unwrap()
        .call(
            "test.recordTarget",
            json!({ "sessionId": id_of(&h), "token": token_of(&h), "targetId": human.target }),
        )
        .unwrap();
    assert_eq!(r["recorded"], false, "{r}");
    // Positive control: one of the session's own tabs is (already) recorded.
    let own = session_targets.iter().next().unwrap();
    let r: Value = pegboard_core::api::Client::connect(&paths)
        .unwrap()
        .call(
            "test.recordTarget",
            json!({ "sessionId": id_of(&h), "token": token_of(&h), "targetId": own }),
        )
        .unwrap();
    assert_eq!(r["recorded"], true, "{r}");
    std::thread::sleep(Duration::from_secs(1));
    close(&home, &h);

    let dir = recording_dir(&home, &h);
    let dirs = screen_dirs(&dir);
    assert!(!dirs.is_empty(), "nothing was recorded");
    assert!(
        dirs.is_subset(&session_targets),
        "{dirs:?} not within {session_targets:?}"
    );
    assert!(!dirs.contains(&human.target));
    // The human tab's id appears in no file of the recording.
    let mut stack = vec![dir.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            assert!(
                !p.to_string_lossy().contains(&human.target),
                "{}",
                p.display()
            );
            if p.is_dir() {
                stack.push(p);
            } else {
                let bytes = std::fs::read(&p).unwrap();
                let needle = human.target.as_bytes();
                assert!(
                    !bytes.windows(needle.len()).any(|w| w == needle),
                    "{} names the human's tab",
                    p.display()
                );
            }
        }
    }
    drop(human);
}

#[test]
#[ignore]
fn screen_off_domain_frames_are_never_written() {
    let origin = fixture();
    let home = shared_home();
    let h = create(home, &format!("{origin}/animated"));
    let a = targets(home, &h)[0].clone();
    std::thread::sleep(Duration::from_millis(1500));
    // A redirect into the off domain.
    ok(home, &h, &["open", &format!("{origin}/to-off")]);
    std::thread::sleep(Duration::from_millis(1500));
    // A new tab whose popup goes straight to the off domain.
    ok(home, &h, &["tab", "new", &format!("{origin}/popup-off")]);
    let b = tabs(home, &h)
        .into_iter()
        .find(|(_, url)| url.ends_with("/popup-off"))
        .unwrap()
        .0;
    ok(home, &h, &["click", "#pop"]);
    std::thread::sleep(Duration::from_millis(1500));
    let c = tabs(home, &h)
        .into_iter()
        .find(|(id, url)| id != &a && url.contains(SCREEN_OFF_NAME))
        .unwrap_or_else(|| panic!("no popup on the off domain: {:?}", tabs(home, &h)))
        .0;
    // Back and forth between the tabs.
    ok(home, &h, &["tab", &a]);
    ok(home, &h, &["tab", &b]);
    ok(home, &h, &["tab", &c]);
    ok(home, &h, &["tab", &a]);
    std::thread::sleep(Duration::from_millis(800));
    // Out of the off domain again.
    ok(home, &h, &["open", &format!("{origin}/animated")]);
    std::thread::sleep(Duration::from_secs(2));
    close(home, &h);

    let dir = recording_dir(home, &h);
    assert_index_matches_files(&dir);
    for target in screen_dirs(&dir) {
        for line in index(&dir, &target) {
            if line["kind"] == "change" {
                let url = line["url"].as_str().unwrap_or("");
                assert_ne!(host_of(url), SCREEN_OFF_NAME, "{target}: {line}");
            }
        }
    }
    for target in [&a, &c] {
        let lines = index(&dir, target);
        assert!(
            lines.iter().any(|l| l["kind"] == "off"),
            "{target}: no off marker in {lines:?}"
        );
    }
    // Frames resume once the tab is on a normal page again.
    let lines = index(&dir, &a);
    let last_off = lines.iter().rposition(|l| l["kind"] == "off").unwrap();
    assert!(
        lines[last_off..]
            .iter()
            .any(|l| l["kind"] == "change" && l["file"].is_string()),
        "no frame after the off page: {lines:?}"
    );
    // The popup was only ever on the off domain: no frame of it at all.
    assert!(changes(&dir, &c).is_empty(), "{:?}", index(&dir, &c));
}

#[test]
#[ignore]
fn per_recording_cap_stops_only_the_screen() {
    const CAP: u64 = 2_000_000;
    let origin = fixture();
    let home = limited_home();
    let h = create(home, &format!("{origin}/animated"));
    let mut ran = 0;
    for _ in 0..20 {
        ok(home, &h, &["get", "url"]);
        ran += 1;
        std::thread::sleep(Duration::from_secs(1));
    }
    close(home, &h);

    let dir = recording_dir(home, &h);
    let m = manifest(&dir);
    assert_eq!(m["screen"], "capped", "{m}");
    assert_index_matches_files(&dir);
    let mut capped_at = None;
    let (mut bytes, mut largest) = (0u64, 0u64);
    for target in screen_dirs(&dir) {
        for line in index(&dir, &target) {
            if line["kind"] == "capped" {
                capped_at = Some(ts_ms(line["ts"].as_str().unwrap()));
            }
        }
        for f in jpgs(&dir, &target) {
            let n = std::fs::metadata(dir.join("screen").join(&target).join(f))
                .unwrap()
                .len();
            bytes += n;
            largest = largest.max(n);
        }
    }
    let capped_at = capped_at.expect("no capped marker");
    assert!(bytes <= CAP + largest, "{bytes} screen bytes");
    // The actions went on: every command is logged, also those after the cap.
    let actions: Vec<Value> = std::fs::read_to_string(dir.join("actions.jsonl"))
        .unwrap()
        .lines()
        .map(json)
        .filter(|a| a["command"] == "get")
        .collect();
    assert_eq!(actions.len(), ran, "{actions:?}");
    let after = actions
        .iter()
        .filter(|a| ts_ms(a["ts"].as_str().unwrap()) > capped_at)
        .count();
    assert!(after >= 5, "only {after} commands after the cap");
    assert!(actions.iter().all(|a| a["ok"] == true), "{actions:?}");
}

/// Brings `target` to the front of its window, as a person clicking its tab would: straight
/// on the browser (test-only knowledge of its port); agents cannot do this.
fn activate(home: &Home, target: &str) {
    let text = std::fs::read_to_string(home.path().join("chrome/DevToolsActivePort")).unwrap();
    let mut lines = text.lines();
    let (port, path) = (lines.next().unwrap(), lines.next().unwrap());
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let conn = pegboard_core::cdp::CdpConn::connect(&format!("ws://127.0.0.1:{port}{path}"))
            .await
            .unwrap();
        conn.send("Target.activateTarget", json!({ "targetId": target }), None)
            .await
            .unwrap();
        conn.close();
    });
}

#[test]
#[ignore]
fn hidden_tab_follows_the_measured_rule() {
    // F4: a tab behind another tab of the same window keeps streaming (about 4 a second in
    // the measurement): its changes are still recorded, at most 2 a second.
    let origin = fixture();
    let home = shared_home();
    let h = create(home, &format!("{origin}/animated"));
    let a = targets(home, &h)[0].clone();
    activate(home, &a);
    std::thread::sleep(Duration::from_secs(1));
    ok(home, &h, &["tab", "new", &format!("{origin}/counter")]);
    let b = tabs(home, &h)
        .into_iter()
        .find(|(_, url)| url.ends_with("/counter"))
        .unwrap()
        .0;
    activate(home, &b);
    let hidden_from = now_ms();
    std::thread::sleep(Duration::from_secs(4));
    let hidden_until = now_ms();
    close(home, &h);

    let dir = recording_dir(home, &h);
    let behind: Vec<Value> = changes(&dir, &a)
        .into_iter()
        .filter(|l| {
            let t = ts_ms(l["ts"].as_str().unwrap());
            t > hidden_from && t <= hidden_until
        })
        .collect();
    assert!(
        behind.len() >= 4,
        "{} frames of the hidden tab in 4 s: {:?}",
        behind.len(),
        index(&dir, &a)
    );
}

fn hook(home: &Home, method: &str, params: Value) -> Value {
    let paths = pegboard_core::paths::paths_in(home.path());
    pegboard_core::api::Client::connect(&paths)
        .unwrap()
        .call(method, params)
        .unwrap()
}

/// No change frame of `target` before the page's first contentful paint (`paintedAt`, read
/// from the page), at least one after it, and none on the screen-off host.
fn assert_nothing_before_the_paint(home: &Home, handle: &str, target: &str) {
    let painted = eval(home, handle, "window.paintedAt")
        .as_f64()
        .expect("no paint") as i64;
    close(home, handle);
    let dir = recording_dir(home, handle);
    assert_index_matches_files(&dir);
    let frames = changes(&dir, target);
    for f in &frames {
        assert_ne!(
            host_of(f["url"].as_str().unwrap_or("")),
            SCREEN_OFF_NAME,
            "{f}"
        );
        let t = ts_ms(f["ts"].as_str().unwrap());
        // Clock reads of the page and of the daemon may differ by a few ms.
        assert!(
            t >= painted - 50,
            "a frame {} ms before the paint: {f}; recorder: {:?}",
            painted - t,
            recorder_log(home, target)
        );
    }
    assert!(
        frames
            .iter()
            .any(|f| ts_ms(f["ts"].as_str().unwrap()) >= painted),
        "no frame after the paint: {:?}",
        index(&dir, target)
    );
}

/// The recorder loses the tab on a screen-off page; meanwhile the tab goes on to `page`
/// (on the fixture's address), which paints 2.5 s after it loads.
fn reattach_after_an_off_page(page: &str) {
    let origin = fixture();
    let home = shared_home();
    let off = format!("{}/off", origin.replace("127.0.0.1", SCREEN_OFF_NAME));
    let h = create(home, &off);
    let a = targets(home, &h)[0].clone();
    std::thread::sleep(Duration::from_secs(1));
    let r = hook(
        home,
        "test.dropRecorder",
        json!({ "sessionId": id_of(&h), "token": token_of(&h), "holdMs": 1500 }),
    );
    assert_eq!(r["dropped"], true, "{r}");
    ok(home, &h, &["open", &format!("{origin}{page}")]);
    std::thread::sleep(Duration::from_secs(5));
    assert_nothing_before_the_paint(home, &h, &a);
}

/// F5 across a reattach: until the new page's first contentful paint the screen may still
/// be the off page's, so nothing is written before it.
#[test]
#[ignore]
fn a_tab_reattached_after_leaving_an_off_page_writes_nothing_before_the_paint() {
    reattach_after_an_off_page("/late");
}

/// The paint the reattach looks for is a real paint entry: a page that only makes a
/// `performance.mark` of that name stays held until it really paints.
#[test]
#[ignore]
fn a_page_cannot_fake_its_first_paint() {
    reattach_after_an_off_page("/mark-late");
}

/// A session whose start page is screen-off and replaces itself at once (leaving no history
/// entry): the recorder followed the tab before it was navigated, so nothing of the next
/// page is written before it paints.
#[test]
#[ignore]
fn a_screen_off_start_page_is_known_to_the_recording() {
    let origin = fixture();
    let home = shared_home();
    let start = format!(
        "{}/replace-late",
        origin.replace("127.0.0.1", SCREEN_OFF_NAME)
    );
    let h = create(home, &start);
    let a = targets(home, &h)[0].clone();
    std::thread::sleep(Duration::from_secs(5));
    assert_nothing_before_the_paint(home, &h, &a);
}

/// The recorder never holds up `session create`: with its connection down (test hook), a
/// session is made about as fast as with it.
#[test]
#[ignore]
fn session_create_does_not_wait_for_the_recorder() {
    let origin = fixture();
    let home = shared_home();
    let url = format!("{origin}/counter");
    let a = create(home, &url);
    let t = Instant::now();
    let b = create(home, &url);
    let base = t.elapsed();
    close(home, &b);
    let r = hook(
        home,
        "test.dropRecorder",
        json!({ "sessionId": id_of(&a), "token": token_of(&a), "holdMs": 3000 }),
    );
    assert_eq!(r["dropped"], true, "{r}");
    let held_until = Instant::now() + Duration::from_millis(3000);
    let t = Instant::now();
    let c = create(home, &url);
    let held = t.elapsed();
    close(home, &c);
    assert!(
        held <= base + Duration::from_millis(1000),
        "session create took {held:?} without the recorder, {base:?} with it"
    );
    // The hook needs a session's token.
    let paths = pegboard_core::paths::paths_in(home.path());
    let refused: Result<Value, _> = pegboard_core::api::Client::connect(&paths).unwrap().call(
        "test.dropRecorder",
        json!({ "sessionId": id_of(&a), "token": "wrong", "holdMs": 10 }),
    );
    assert!(refused.is_err(), "{refused:?}");
    close(home, &a);
    // The next test finds the recorder connected again.
    wait_out(held_until);
}

/// Sleeps past `until` and the recorder's reconnect after a hold.
fn wait_out(until: Instant) {
    let reconnect = until + Duration::from_secs(1);
    if let Some(left) = reconnect.checked_duration_since(Instant::now()) {
        std::thread::sleep(left);
    }
}

/// A session created while the recorder's connection is down is recorded once the connection
/// is back: the connection task attaches every tab of every recording.
#[test]
#[ignore]
fn a_session_created_during_a_recorder_outage_is_recorded_once_it_ends() {
    let origin = fixture();
    let home = shared_home();
    let a = create(home, &format!("{origin}/counter"));
    let r = hook(
        home,
        "test.dropRecorder",
        json!({ "sessionId": id_of(&a), "token": token_of(&a), "holdMs": 2000 }),
    );
    assert_eq!(r["dropped"], true, "{r}");
    let held_until = Instant::now() + Duration::from_millis(2000);
    let back_ms = now_ms() + 2000;
    let c = create(home, &format!("{origin}/animated"));
    let target = targets(home, &c)[0].clone();
    wait_out(held_until);
    std::thread::sleep(Duration::from_secs(2));
    close(home, &c);
    close(home, &a);
    let dir = recording_dir(home, &c);
    let frames = changes(&dir, &target);
    assert!(
        frames
            .iter()
            .filter(|f| ts_ms(f["ts"].as_str().unwrap()) >= back_ms)
            .count()
            >= 2,
        "{frames:?}; recorder: {:?}",
        recorder_log(home, &target)
    );
    assert_eq!(manifest(&dir)["screen"], "recorded");
}

// ---- frames around commands (task 4) ----

/// The recording's action entries.
fn actions(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("actions.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(json)
        .collect()
}

/// The session's last action entry (read while the session runs).
fn last_entry(home: &Home, handle: &str) -> Value {
    actions(&recording_dir(home, handle))
        .pop()
        .expect("no action entry")
}

/// A command in the session: its exit code and its action entry.
fn command(home: &Home, handle: &str, args: &[&str]) -> (i32, Value) {
    let (code, _, _) = home.run_limited(&[("PEGBOARD_SESSION", handle)], args, 90);
    (code, last_entry(home, handle))
}

/// The index line of a frame named `<targetId>/<file>`; its file must exist.
fn frame_line(dir: &Path, name: &str) -> Value {
    let (target, file) = name.split_once('/').unwrap_or_else(|| panic!("{name}"));
    assert!(
        dir.join("screen").join(target).join(file).is_file(),
        "{name} is no file"
    );
    index(dir, target)
        .into_iter()
        .find(|l| l["file"] == file)
        .unwrap_or_else(|| panic!("{name} has no index line"))
}

/// Every JPEG of the recording, as `<targetId>/<file>`.
fn all_jpgs(dir: &Path) -> BTreeSet<String> {
    screen_dirs(dir)
        .into_iter()
        .flat_map(|t| jpgs(dir, &t).into_iter().map(move |f| format!("{t}/{f}")))
        .collect()
}

fn set_screen(home: &Home, handle: &str, on: bool) {
    let r = hook(
        home,
        "test.setScreen",
        json!({ "sessionId": id_of(handle), "token": token_of(handle), "on": on }),
    );
    assert_eq!(r["set"], true, "{r}");
}

fn has_no_frames(entry: &Value) -> bool {
    entry.get("frameBefore").is_none() && entry.get("frameAfter").is_none()
}

#[test]
#[ignore]
fn every_command_has_frames_before_and_after() {
    let origin = fixture();
    let home = shared_home();
    let form = format!("{origin}/form");
    let counter = format!("{origin}/counter");
    let h = create(home, &form);
    let first = targets(home, &h)[0].clone();
    // The stream's first frame of the start page is stored.
    std::thread::sleep(Duration::from_secs(1));
    let lines: Vec<Vec<&str>> = vec![
        vec!["open", &form],
        vec!["click", "#name"],
        vec!["fill", "#name", "hello"],
        vec!["get", "url"],
        vec!["eval", "document.title"],
        vec!["scroll", "down", "200"],
        vec!["tab", "new", &counter],
        vec!["tab", &first],
    ];
    let mut entries = Vec::new();
    for args in &lines {
        let (code, entry) = command(home, &h, args);
        assert_eq!(code, 0, "{args:?}: {entry}");
        entries.push((args.clone(), entry));
    }
    close(home, &h);
    let dir = recording_dir(home, &h);
    assert_index_matches_files(&dir);
    for (args, e) in &entries {
        let before = e["frameBefore"]
            .as_str()
            .unwrap_or_else(|| panic!("{args:?}: no frame before: {e}"));
        let after = e["frameAfter"]
            .as_str()
            .unwrap_or_else(|| panic!("{args:?}: no frame after: {e}"));
        // Before: the tab's last stored frame (stream or an earlier command's capture).
        let kind = frame_line(&dir, before)["kind"].clone();
        assert!(
            kind == "change" || kind == "after",
            "{args:?}: {before} is {kind}"
        );
        assert_eq!(
            frame_line(&dir, after)["kind"],
            "after",
            "{args:?}: {after}"
        );
        assert_ne!(before, after, "{args:?}");
    }
    let after_dir = |i: usize| {
        entries[i].1["frameAfter"]
            .as_str()
            .unwrap()
            .split('/')
            .next()
            .unwrap()
            .to_string()
    };
    // `tab new`: the picture after is of the new tab; `tab <first>` back on the first.
    let new_tab = after_dir(6);
    assert_ne!(new_tab, first, "{}", entries[6].1);
    assert_eq!(
        entries[6].1["targetId"],
        new_tab.as_str(),
        "{}",
        entries[6].1
    );
    assert_eq!(after_dir(7), first, "{}", entries[7].1);
    // The frame before `tab <first>` is the new tab's (the tab the command started on).
    assert!(
        entries[7].1["frameBefore"]
            .as_str()
            .unwrap()
            .starts_with(&new_tab),
        "{}",
        entries[7].1
    );
}

#[test]
#[ignore]
fn screen_turned_off_by_the_test_hook_writes_no_frames() {
    let origin = fixture();
    let home = shared_home();
    let h = create(home, &format!("{origin}/animated"));
    std::thread::sleep(Duration::from_secs(1));
    set_screen(home, &h, false);
    // A frame already on its way to the disk.
    std::thread::sleep(Duration::from_millis(300));
    let dir = recording_dir(home, &h);
    let files = all_jpgs(&dir);
    for args in [vec!["get", "url"], vec!["eval", "1"]] {
        let (code, e) = command(home, &h, &args);
        assert_eq!(code, 0, "{e}");
        assert!(has_no_frames(&e), "{args:?}: {e}");
    }
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(all_jpgs(&dir), files, "frames were written while off");
    set_screen(home, &h, true);
    std::thread::sleep(Duration::from_secs(1));
    let (code, e) = command(home, &h, &["get", "url"]);
    assert_eq!(code, 0, "{e}");
    assert!(
        e["frameBefore"].is_string() && e["frameAfter"].is_string(),
        "{e}"
    );
    close(home, &h);
    assert!(all_jpgs(&dir).len() > files.len() + 1);
    // The hook needs the session's token.
    let paths = pegboard_core::paths::paths_in(home.path());
    let refused: Result<Value, _> = pegboard_core::api::Client::connect(&paths).unwrap().call(
        "test.setScreen",
        json!({ "sessionId": id_of(&h), "token": "wrong", "on": false }),
    );
    assert!(refused.is_err(), "{refused:?}");
}

#[test]
#[ignore]
fn a_failed_command_still_has_both_frames() {
    let origin = fixture();
    let home = shared_home();
    let h = create(home, &format!("{origin}/counter"));
    std::thread::sleep(Duration::from_secs(1));
    let (code, e) = command(home, &h, &["click", "#missing"]);
    assert_ne!(code, 0, "{e}");
    assert_eq!(e["ok"], false, "{e}");
    assert!(
        e["frameBefore"].is_string() && e["frameAfter"].is_string(),
        "{e}"
    );
    close(home, &h);
}

#[test]
#[ignore]
fn refused_commands_have_no_frames() {
    let origin = fixture();
    let home = shared_home();
    let h = create(home, &format!("{origin}/counter"));
    std::thread::sleep(Duration::from_secs(1));
    // The session has frames: an allowed command gets them.
    let (code, e) = command(home, &h, &["get", "url"]);
    assert_eq!(code, 0, "{e}");
    assert!(e["frameBefore"].is_string(), "{e}");
    // Refused by the policy, and by the grammar.
    let (code, e) = command(home, &h, &["cookies", "set", "a", "b"]);
    assert_ne!(code, 0);
    assert!(
        e["error"]
            .as_str()
            .unwrap_or("")
            .starts_with("policy_denied"),
        "{e}"
    );
    assert!(has_no_frames(&e), "{e}");
    let (code, e) = command(home, &h, &["snapshot", "--cdp", "9222"]);
    assert_eq!(code, 2);
    assert!(
        e["error"]
            .as_str()
            .unwrap_or("")
            .starts_with("argv_rejected"),
        "{e}"
    );
    assert!(has_no_frames(&e), "{e}");
    close(home, &h);
}

#[test]
#[ignore]
fn commands_on_a_screen_off_domain_have_no_frames() {
    let origin = fixture();
    let home = shared_home();
    let off = format!("{}/off", origin.replace("127.0.0.1", SCREEN_OFF_NAME));
    let h = create(home, &format!("{origin}/counter"));
    std::thread::sleep(Duration::from_secs(1));
    let dir = recording_dir(home, &h);
    let (code, e) = command(home, &h, &["open", &off]);
    assert_eq!(code, 0, "{e}");
    assert!(e.get("frameAfter").is_none(), "{e}");
    std::thread::sleep(Duration::from_millis(300));
    let files = all_jpgs(&dir);
    for args in [vec!["get", "url"], vec!["click", "body"]] {
        let (code, e) = command(home, &h, &args);
        assert_eq!(code, 0, "{args:?}: {e}");
        assert!(has_no_frames(&e), "{args:?}: {e}");
    }
    assert_eq!(all_jpgs(&dir), files, "frames of the off page");
    // From a normal page into the off domain by a redirect: no picture after. The page
    // keeps changing, so frames of it are stored once its first paint ends the hold that
    // follows the off page (a still page would leave the tab without a frame before).
    ok(home, &h, &["open", &format!("{origin}/animated")]);
    std::thread::sleep(Duration::from_secs(1));
    let (code, e) = command(home, &h, &["open", &format!("{origin}/to-off")]);
    assert_eq!(code, 0, "{e}");
    assert!(e["frameBefore"].is_string(), "{e}");
    assert!(e.get("frameAfter").is_none(), "{e}");
    close(home, &h);
    for target in screen_dirs(&dir) {
        for line in index(&dir, &target) {
            if line["file"].is_string() {
                let url = line["url"].as_str().unwrap_or("");
                assert_ne!(host_of(url), SCREEN_OFF_NAME, "{target}: {line}");
            }
        }
    }
}

/// A click on a link that redirects into the screen-off domain (so only the commit tells the
/// recorder where the tab went): the capture after it is answered while the
/// recorder has not yet handled the navigation the browser sent before the answer (hooks: the
/// capture waits 150 ms, so the page has committed; the navigation is handled 250 ms late).
/// The capture is judged only after that navigation: nothing is kept.
#[test]
#[ignore]
fn a_capture_answered_before_its_navigation_was_handled_is_not_kept() {
    let origin = fixture();
    let home = screen_home_with(
        SCREEN_OFF_CONFIG,
        &[
            ("PEGBOARD_TEST_CAPTURE_DELAY_MS", "150"),
            ("PEGBOARD_TEST_EVENT_DELAY_MS", "250"),
        ],
    );
    let h = create(&home, &format!("{origin}/link-off"));
    let a = targets(&home, &h)[0].clone();
    std::thread::sleep(Duration::from_millis(1500));
    let (code, e) = command(&home, &h, &["click", "#go"]);
    assert_eq!(code, 0, "{e}");
    std::thread::sleep(Duration::from_secs(1));
    let (_, url) = command(&home, &h, &["get", "url"]);
    assert_eq!(
        host_of(url["url"].as_str().unwrap_or("")),
        SCREEN_OFF_NAME,
        "{url}"
    );
    close(&home, &h);
    assert!(e.get("frameAfter").is_none(), "{e}");
    let dir = recording_dir(&home, &h);
    let after: Vec<_> = index(&dir, &a)
        .into_iter()
        .filter(|l| l["kind"] == "after")
        .collect();
    assert!(after.is_empty(), "a capture was kept: {after:?}");
}

fn duration_ms(entry: &Value) -> i64 {
    entry["durationMs"]
        .as_i64()
        .unwrap_or_else(|| panic!("{entry}"))
}

#[test]
#[ignore]
fn a_slow_capture_never_delays_a_command_beyond_the_cap() {
    let origin = fixture();
    let home = screen_home_with(
        SCREEN_OFF_CONFIG,
        &[("PEGBOARD_TEST_CAPTURE_DELAY_MS", "5000")],
    );
    let h = create(&home, &format!("{origin}/counter"));
    std::thread::sleep(Duration::from_secs(1));
    // Every command: its entry and the daemon's lines while it ran (timing, recorder).
    let mut runs: Vec<(Value, String)> = Vec::new();
    let mut run = |home: &Home| {
        let from = daemon_log_len(home);
        let (code, e) = command(home, &h, &["get", "url"]);
        assert_eq!(code, 0, "{e}");
        let lines: String = daemon_log_since(home, from)
            .lines()
            .filter(|l| l.contains("pegboard: command ") || l.contains("recorder"))
            .map(|l| format!("{l}\n"))
            .collect();
        runs.push((e.clone(), lines));
        e
    };
    // Without the screen: what the command takes alone.
    set_screen(&home, &h, false);
    let base = (0..3).map(|_| duration_ms(&run(&home))).max().unwrap();
    set_screen(&home, &h, true);
    std::thread::sleep(Duration::from_millis(500));
    let first = run(&home);
    // Ten more: the page is still (no stream frame), so the tab whose capture timed out is
    // not captured again for a while.
    for _ in 0..10 {
        run(&home);
    }
    close(&home, &h);
    let report = runs
        .iter()
        .enumerate()
        .map(|(i, (e, lines))| {
            format!(
                "#{i} durationMs {} frameAfter {}\n{lines}",
                e["durationMs"], e["frameAfter"]
            )
        })
        .collect::<String>();
    let report = format!("alone {base} ms\n{report}");
    assert!(first.get("frameAfter").is_none(), "{report}");
    assert!(duration_ms(&first) < base + 1200, "{report}");
    // The first command with the screen on and the ten after it.
    let screen_on = &runs[3..];
    for (e, lines) in screen_on {
        assert!(e.get("frameAfter").is_none(), "{report}");
        // The recorder's own share of each command, from the daemon's timing line.
        let after = timing(lines, "pegboard: command ", "frame after")
            .unwrap_or_else(|| panic!("no timing line\n{report}"));
        assert!(after < 600.0, "frame after {after} ms\n{report}");
    }
    let extra: i64 = screen_on[1..]
        .iter()
        .map(|(e, _)| (duration_ms(e) - base).max(0))
        .sum();
    assert!(
        extra < 10 * 1200,
        "{extra} ms more over ten commands\n{report}"
    );
    assert!(
        extra < 2000,
        "{extra} ms more over ten commands: the timed-out tab was captured again\n{report}"
    );
}

#[test]
#[ignore]
fn commands_run_while_the_recorder_is_disconnected() {
    let origin = fixture();
    let home = shared_home();
    let h = create(home, &format!("{origin}/counter"));
    std::thread::sleep(Duration::from_secs(1));
    // What the command takes without the screen.
    set_screen(home, &h, false);
    let base = (0..3)
        .map(|_| duration_ms(&command(home, &h, &["get", "url"]).1))
        .max()
        .unwrap();
    set_screen(home, &h, true);
    let r = hook(
        home,
        "test.dropRecorder",
        json!({ "sessionId": id_of(&h), "token": token_of(&h), "holdMs": 5000 }),
    );
    assert_eq!(r["dropped"], true, "{r}");
    let held_until = Instant::now() + Duration::from_millis(5000);
    // The recorder sees its connection end.
    std::thread::sleep(Duration::from_millis(200));
    for _ in 0..3 {
        let (code, e) = command(home, &h, &["get", "url"]);
        assert_eq!(code, 0, "{e}");
        assert!(has_no_frames(&e), "{e}");
        assert!(
            duration_ms(&e) < base + 100,
            "{e}; without the screen {base} ms"
        );
    }
    wait_out(held_until);
    std::thread::sleep(Duration::from_secs(1));
    let (code, e) = command(home, &h, &["get", "url"]);
    assert_eq!(code, 0, "{e}");
    assert!(
        e["frameBefore"].is_string() && e["frameAfter"].is_string(),
        "frames did not come back: {e}"
    );
    close(home, &h);
}

fn daemon_log_len(home: &Home) -> usize {
    std::fs::metadata(home.path().join("daemon.log")).map_or(0, |m| m.len() as usize)
}

/// What daemon.log got after byte `from`.
fn daemon_log_since(home: &Home, from: usize) -> String {
    let bytes = std::fs::read(home.path().join("daemon.log")).unwrap_or_default();
    String::from_utf8_lossy(bytes.get(from..).unwrap_or_default()).into_owned()
}

/// The number before " ms" that follows `key` in the first line containing `line_has`
/// (the timing lines the daemon writes with test hooks on).
fn timing(text: &str, line_has: &str, key: &str) -> Option<f64> {
    let line = text.lines().find(|l| l.contains(line_has))?;
    let rest = &line[line.find(key)? + key.len()..];
    rest.trim_start().split(' ').next()?.parse().ok()
}

fn median_f(v: &[f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let m = s.len() / 2;
    if s.len() % 2 == 1 {
        s[m]
    } else {
        (s[m - 1] + s[m]) / 2.0
    }
}

#[test]
fn timing_reads_the_daemon_lines() {
    let text = "x\npegboard: command fill: checks 0.4 ms, exec 30.1 ms, probe 3.2 ms, frame after 41.0 ms, total 72 ms\n\
                pegboard: recorder: capture of T: ticket 0.1 ms, cdp 38.5 ms, handoff 0.2 ms\n";
    assert_eq!(timing(text, "pegboard: command ", "probe"), Some(3.2));
    assert_eq!(
        timing(text, "pegboard: command ", "frame after"),
        Some(41.0)
    );
    assert_eq!(timing(text, "capture of", "cdp"), Some(38.5));
    assert_eq!(timing(text, "capture of", "handoff"), Some(0.2));
    assert_eq!(timing(text, "nothing", "cdp"), None);
}

fn median(v: &[i64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_unstable();
    let m = s.len() / 2;
    if s.len() % 2 == 1 {
        s[m] as f64
    } else {
        (s[m - 1] + s[m]) as f64 / 2.0
    }
}

/// Design 4.6: what the recording adds to a command, median at most 60 ms and at most 1 s
/// (owner decision 2026-11-18: the after-command capture alone costs 20-40 ms in Chrome).
/// The commands of spike S2, each run in pairs with the screen on and off (test hook; the
/// order alternates), `PEGBOARD_OVERHEAD_REPS` pairs (default 20). Times are the entries'
/// `durationMs` (the daemon's own measure, frames included). Prints a table, with where the
/// time of the on-runs went (daemon.log timing lines: the focus probe and the frame after,
/// which run at the same time, and the capture's CDP round trip).
#[test]
#[ignore]
fn recording_overhead_meets_the_targets() {
    let origin = fixture();
    let home = shared_home();
    let reps: usize = std::env::var("PEGBOARD_OVERHEAD_REPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let form = format!("{origin}/form");
    let counter = format!("{origin}/counter");
    let h = create(home, &form);
    let first = targets(home, &h)[0].clone();
    std::thread::sleep(Duration::from_secs(1));
    let commands: Vec<(&str, Vec<&str>)> = vec![
        ("open", vec!["open", &form]),
        ("snapshot -i", vec!["snapshot", "-i"]),
        ("click", vec!["click", "#name"]),
        ("fill", vec!["fill", "#name", "hello"]),
        ("type", vec!["type", "#name", " world"]),
        ("press Enter", vec!["press", "Enter"]),
        ("get url", vec!["get", "url"]),
        ("get text", vec!["get", "text", "label"]),
        ("eval", vec!["eval", "document.title"]),
        ("scroll down 200", vec!["scroll", "down", "200"]),
        ("wait 100", vec!["wait", "100"]),
        ("screenshot", vec!["screenshot"]),
        ("tab new", vec!["tab", "new", &counter]),
        ("tab list", vec!["tab", "list"]),
    ];
    let mut rows = Vec::new();
    let mut missing = Vec::new();
    let mut leaked = Vec::new();
    let mut pass = true;
    for (name, args) in &commands {
        let mut added = Vec::new();
        let mut on_times = Vec::new();
        let (mut probes, mut afters, mut cdps, mut handoffs) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for rep in 0..reps {
            let order = if rep % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            };
            let mut pair = [0i64; 2];
            for on in order {
                set_screen(home, &h, on);
                if on {
                    // The stream's first frames come right after it starts (as in S2).
                    std::thread::sleep(Duration::from_millis(150));
                }
                let log_from = daemon_log_len(home);
                let (code, e) = command(home, &h, args);
                assert_eq!(code, 0, "{name}: {e}");
                if on {
                    let text = daemon_log_since(home, log_from);
                    probes.extend(timing(&text, "pegboard: command ", "probe"));
                    afters.extend(timing(&text, "pegboard: command ", "frame after"));
                    cdps.extend(timing(&text, "capture of", "cdp"));
                    handoffs.extend(timing(&text, "capture of", "handoff"));
                }
                if on && !e["frameAfter"].is_string() {
                    missing.push(format!("{name} #{rep}"));
                }
                if !on && !has_no_frames(&e) {
                    leaked.push(format!("{name} #{rep}: {e}"));
                }
                pair[usize::from(on)] = duration_ms(&e);
                if *name == "tab new" {
                    ok(home, &h, &["tab", "close"]);
                    ok(home, &h, &["tab", &first]);
                }
            }
            added.push(pair[1] - pair[0]);
            on_times.push(pair[1]);
        }
        let (m, x) = (median(&added), *added.iter().max().unwrap());
        pass &= m <= 60.0 && x <= 1000;
        rows.push(format!(
            "| {name} | {:.0} | {m:.0} | {x} | {:.1} | {:.1} | {:.1} | {:.2} |",
            median(&on_times),
            median_f(&probes),
            median_f(&afters),
            median_f(&cdps),
            median_f(&handoffs),
        ));
    }
    close(home, &h);
    println!("recording overhead, {reps} pairs per command (ms, durationMs of the entries):");
    println!(
        "| command | on median | added median | added max | on: probe median | on: frame after median | on: capture cdp median | on: handoff median |"
    );
    println!("|---|---|---|---|---|---|---|---|");
    for r in &rows {
        println!("{r}");
    }
    println!("on-runs without a frame after: {}", missing.len());
    assert!(
        leaked.is_empty(),
        "frames while the screen was off: {leaked:?}"
    );
    // A run without its capture would make the recording look cheaper than it is.
    assert!(
        missing.is_empty(),
        "on-runs without a frame after: {missing:?}"
    );
    assert!(
        pass,
        "over the targets (median <= 60 ms, max <= 1000 ms):\n{}",
        rows.join("\n")
    );
}

// ---- navigation log, human interventions, network and console (task 5) ----

/// Plays the person at the browser: a connection of the test's own, made straight to the
/// browser (test-only knowledge of its port, from the home's `DevToolsActivePort`), not
/// through a session endpoint. What it does to a session tab happens outside any command.
struct Person {
    rt: tokio::runtime::Runtime,
    conn: pegboard_core::cdp::CdpConn,
}

impl Person {
    fn connect(home: &Home) -> Person {
        let text = std::fs::read_to_string(home.path().join("chrome/DevToolsActivePort")).unwrap();
        let mut lines = text.lines();
        let (port, path) = (lines.next().unwrap(), lines.next().unwrap());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let conn = rt
            .block_on(pegboard_core::cdp::CdpConn::connect(&format!(
                "ws://127.0.0.1:{port}{path}"
            )))
            .unwrap();
        Person { rt, conn }
    }

    fn send(&self, method: &str, params: Value, session: Option<&str>) -> Value {
        self.rt
            .block_on(self.conn.send(method, params, session))
            .unwrap_or_else(|e| panic!("{method}: {e:#}"))
    }

    /// Runs `f` on a CDP session of the test's own on `target`.
    fn on<T>(&self, target: &str, f: impl FnOnce(&Person, &str) -> T) -> T {
        let session = self.rt.block_on(self.conn.attach(target)).unwrap();
        let out = f(self, &session);
        let _ = self.rt.block_on(self.conn.send(
            "Target.detachFromTarget",
            json!({ "sessionId": session }),
            None,
        ));
        out
    }

    /// Types an address into the tab (a navigation the browser starts).
    fn navigate(&self, target: &str, url: &str) {
        self.on(target, |p, s| {
            p.send("Page.navigate", json!({ "url": url }), Some(s));
        });
    }

    /// Clicks the middle of the element with the mouse.
    fn click(&self, target: &str, selector: &str) {
        self.on(target, |p, s| {
            let expr = format!(
                "JSON.stringify((r => [r.x + r.width / 2, r.y + r.height / 2])\
                 (document.querySelector({selector:?}).getBoundingClientRect()))"
            );
            let r = p.send(
                "Runtime.evaluate",
                json!({ "expression": expr, "returnByValue": true }),
                Some(s),
            );
            let xy: Vec<f64> = serde_json::from_str(r["result"]["value"].as_str().unwrap()).unwrap();
            for kind in ["mouseMoved", "mousePressed", "mouseReleased"] {
                p.send(
                    "Input.dispatchMouseEvent",
                    json!({ "type": kind, "x": xy[0], "y": xy[1], "button": "left", "clickCount": 1 }),
                    Some(s),
                );
            }
        });
    }

    /// Clicks the tab (makes it the active tab of its window).
    fn activate(&self, target: &str) {
        self.send("Target.activateTarget", json!({ "targetId": target }), None);
    }

    /// The bridge worker of the profile `target` is in: its target id.
    fn bridge_worker(&self, target: &str) -> Option<String> {
        let info = self.send("Target.getTargetInfo", json!({ "targetId": target }), None);
        let context = info["targetInfo"]["browserContextId"].clone();
        let all = self.send("Target.getTargets", json!({}), None);
        all["targetInfos"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| {
                t["type"] == "service_worker"
                    && t["browserContextId"] == context
                    && t["url"].as_str().is_some_and(|u| u.ends_with("/sw.js"))
            })
            .map(|t| t["targetId"].as_str().unwrap().to_string())
    }
}

/// Opens a second tab and makes it the front one with a command (agent-browser's `tab`
/// switches tabs in the browser, spike H6), so that `first` is behind it; waits out the
/// command's grace.
fn in_front_of(home: &Home, handle: &str, first: &str, url: &str) {
    ok(home, handle, &["tab", "new", url]);
    let second = tabs(home, handle)
        .into_iter()
        .find(|(t, _)| t != first)
        .unwrap()
        .0;
    ok(home, handle, &["tab", &second]);
}

fn nav_lines(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("navigation.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(json)
        .collect()
}

fn human_entries(dir: &Path) -> Vec<Value> {
    actions(dir)
        .into_iter()
        .filter(|a| a["kind"] == "human")
        .collect()
}

/// The handle of a session made with these options.
fn create_with(home: &Home, url: &str, options: &[&str]) -> String {
    let mut args = vec!["session", "create", "--url", url];
    args.extend_from_slice(options);
    let (code, out, err) = home.run(&args);
    assert_eq!(code, 0, "{err}");
    session_handle(&out)
}

#[test]
#[ignore]
fn navigation_log_follows_tabs_and_popups() {
    let origin = fixture();
    let home = shared_home();
    let h = create(home, &format!("{origin}/counter?start=SECRET0"));
    let a = targets(home, &h)[0].clone();
    ok(home, &h, &["open", &format!("{origin}/marker?q=SECRET1")]);
    ok(
        home,
        &h,
        &["tab", "new", &format!("{origin}/popup?q=SECRET2")],
    );
    let b = tabs(home, &h)
        .into_iter()
        .find(|(_, url)| url.contains("/popup"))
        .unwrap()
        .0;
    ok(home, &h, &["click", "#pop"]);
    std::thread::sleep(Duration::from_secs(1));
    let popup = tabs(home, &h)
        .into_iter()
        .find(|(_, url)| url.ends_with("/animated"))
        .unwrap_or_else(|| panic!("no popup: {:?}", tabs(home, &h)))
        .0;
    ok(home, &h, &["tab", &popup]);
    ok(home, &h, &["tab", "close"]);
    std::thread::sleep(Duration::from_secs(1));
    close(home, &h);

    let dir = recording_dir(home, &h);
    let lines = nav_lines(&dir);
    let text = std::fs::read_to_string(dir.join("navigation.jsonl")).unwrap();
    assert!(!text.contains('?') && !text.contains("SECRET"), "{text}");
    assert_eq!(mode(&dir.join("navigation.jsonl")), 0o600);
    let of = |t: &str| -> Vec<(String, String)> {
        lines
            .iter()
            .filter(|l| l["targetId"] == t)
            .map(|l| {
                (
                    l["event"].as_str().unwrap().to_string(),
                    l["url"].as_str().unwrap_or("-").to_string(),
                )
            })
            .collect()
    };
    let path = |u: &str| u.strip_prefix(&origin).unwrap_or(u).to_string();
    let events = |t: &str| -> Vec<(String, String)> {
        of(t).into_iter().map(|(e, u)| (e, path(&u))).collect()
    };
    // The first tab: created on its start page, then its navigation.
    let first = events(&a);
    assert_eq!(
        first[0],
        ("created".to_string(), "/counter".to_string()),
        "{lines:?}"
    );
    assert!(
        first.contains(&("navigated".to_string(), "/marker".to_string())),
        "{lines:?}"
    );
    // The second tab: created, then its page.
    let second = events(&b);
    assert_eq!(second[0].0, "created", "{lines:?}");
    assert!(
        second.contains(&("navigated".to_string(), "/popup".to_string())),
        "{lines:?}"
    );
    // The popup: created with its opener, its page, closed, in that order.
    let p = events(&popup);
    let at = |e: &str| p.iter().position(|(x, _)| x == e);
    let (created, navigated, closed) = (at("created"), at("navigated"), at("closed"));
    assert!(
        created.is_some() && created < navigated && navigated < closed,
        "{lines:?}"
    );
    assert!(
        p.contains(&("navigated".to_string(), "/animated".to_string())),
        "{lines:?}"
    );
    let created_line = lines
        .iter()
        .find(|l| l["targetId"] == popup.as_str() && l["event"] == "created")
        .unwrap();
    assert_eq!(created_line["opener"], b.as_str(), "{created_line}");
    // The person did nothing.
    assert!(human_entries(&dir).is_empty(), "{:?}", human_entries(&dir));
}

#[test]
#[ignore]
fn agent_commands_are_never_taken_for_human_ones() {
    let origin = fixture();
    let home = shared_home();
    let search = format!("{origin}/search");
    let h = create(home, &search);
    let first = targets(home, &h)[0].clone();
    let lines: Vec<Vec<&str>> = vec![
        vec!["open", &search],
        vec!["click", "#go"],
        vec!["back"],
        vec!["forward"],
        vec!["reload"],
        vec!["back"],
        vec!["click", "#go"],
        vec!["back"],
        vec!["fill", "#q", "hello"],
        vec!["press", "Enter"],
        vec!["back"],
        vec!["tab", "new", &search],
        vec!["click", "#go"],
        vec!["tab", &first],
        vec!["reload"],
        vec!["forward"],
        vec!["open", &search],
        vec!["fill", "#q", "again"],
        vec!["press", "Enter"],
        vec!["get", "url"],
    ];
    assert_eq!(lines.len(), 20);
    for args in &lines {
        ok(home, &h, args);
    }
    // Late commits and switches of the last commands come within the grace.
    std::thread::sleep(Duration::from_secs(2));
    close(home, &h);
    let dir = recording_dir(home, &h);
    assert!(human_entries(&dir).is_empty(), "{:?}", human_entries(&dir));
    // The commands did navigate (the log saw them).
    let navigated = nav_lines(&dir)
        .iter()
        .filter(|l| l["event"] == "navigated")
        .count();
    assert!(navigated >= 10, "{:?}", nav_lines(&dir));
}

#[test]
#[ignore]
fn a_human_navigation_outside_a_command_is_recorded() {
    let origin = fixture();
    let home = shared_home();
    let h = create(home, &format!("{origin}/counter"));
    let first = targets(home, &h)[0].clone();
    ok(home, &h, &["get", "url"]);
    std::thread::sleep(Duration::from_secs(2));
    let person = Person::connect(home);
    person.navigate(&first, &format!("{origin}/marker?secret=SECRET1"));
    std::thread::sleep(Duration::from_millis(1500));
    close(home, &h);

    let dir = recording_dir(home, &h);
    let marks = human_entries(&dir);
    assert_eq!(marks.len(), 1, "{marks:?}");
    let m = &marks[0];
    assert_eq!(m["targetId"], first.as_str(), "{m}");
    // Spike F6: nothing tells a person from a script's history navigation: never certain.
    assert_eq!(m["certain"], false, "{m}");
    assert_eq!(m["command"], "human", "{m}");
    assert_eq!(
        m["what"],
        format!("navigated outside a command: {origin}/marker").as_str(),
        "{m}"
    );
    assert!(!m.to_string().contains("SECRET"), "{m}");
    let rid = format!("r_{}", &id_of(&h)[2..]);
    let (code, out, err) = home.run(&["recording", "show", &rid]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("human  possible: navigated outside a command") && out.contains(&first),
        "{out}"
    );
}

#[test]
#[ignore]
fn a_human_like_click_outside_a_command_is_marked_possible() {
    let origin = fixture();
    let home = shared_home();
    let h = create(home, &format!("{origin}/search"));
    let first = targets(home, &h)[0].clone();
    ok(home, &h, &["get", "url"]);
    std::thread::sleep(Duration::from_secs(2));
    let person = Person::connect(home);
    person.click(&first, "#go");
    std::thread::sleep(Duration::from_millis(1500));
    let url = eval(home, &h, "location.pathname");
    close(home, &h);
    assert_eq!(url, "/counter", "the click did not follow the link");

    let dir = recording_dir(home, &h);
    let marks = human_entries(&dir);
    assert_eq!(marks.len(), 1, "{marks:?}");
    assert_eq!(
        marks[0]["what"],
        "followed a link or form outside a command"
    );
    assert_eq!(marks[0]["certain"], false);
    assert_eq!(marks[0]["targetId"], first.as_str());
}

#[test]
#[ignore]
fn page_script_navigation_is_not_human() {
    let origin = fixture();
    let home = shared_home();
    let h = create(home, &format!("{origin}/script-nav"));
    // The page moves on by itself 2.5 s after it loaded: no command is running then.
    std::thread::sleep(Duration::from_secs(4));
    close(home, &h);
    let dir = recording_dir(home, &h);
    assert!(
        nav_lines(&dir)
            .iter()
            .any(|l| l["event"] == "navigated" && l["url"] == format!("{origin}/counter").as_str()),
        "{:?}",
        nav_lines(&dir)
    );
    assert!(human_entries(&dir).is_empty(), "{:?}", human_entries(&dir));
}

#[test]
#[ignore]
fn a_tab_switch_outside_a_command_is_marked_possible() {
    let origin = fixture();
    let home = shared_home();
    let h = create(home, &format!("{origin}/counter"));
    let first = targets(home, &h)[0].clone();
    in_front_of(home, &h, &first, &format!("{origin}/marker"));
    std::thread::sleep(Duration::from_secs(2));
    let person = Person::connect(home);
    person.activate(&first);
    std::thread::sleep(Duration::from_millis(1500));
    close(home, &h);
    let dir = recording_dir(home, &h);
    let marks = human_entries(&dir);
    assert_eq!(marks.len(), 1, "{marks:?}");
    assert_eq!(marks[0]["what"], "switched to this tab");
    assert_eq!(marks[0]["certain"], false);
    assert_eq!(marks[0]["targetId"], first.as_str());
}

#[test]
#[ignore]
fn a_tab_switch_after_the_bridge_worker_restarted_is_marked() {
    let origin = fixture();
    let home = shared_home();
    let h = create(home, &format!("{origin}/counter"));
    let first = targets(home, &h)[0].clone();
    in_front_of(home, &h, &first, &format!("{origin}/marker"));
    let person = Person::connect(home);
    let old = person.bridge_worker(&first).expect("no bridge worker");
    // The worker starts again (as after Chrome stopped it): the binding is gone with it.
    // The switch below may be what wakes the new worker (then it keeps the switch until the
    // daemon binds it again, spike H8) or come after the daemon bound it.
    person.on(&old, |p, s| {
        // The worker goes away with the call: its answer may never come.
        let _ = p.rt.block_on(tokio::time::timeout(
            Duration::from_secs(3),
            p.conn.send(
                "Runtime.evaluate",
                json!({ "expression": "chrome.runtime.reload()" }),
                Some(s),
            ),
        ));
    });
    std::thread::sleep(Duration::from_secs(2));
    person.activate(&first);
    let deadline = Instant::now() + Duration::from_secs(10);
    let new = loop {
        match person.bridge_worker(&first) {
            Some(w) if w != old => break w,
            _ if Instant::now() > deadline => panic!("premise: the bridge worker did not restart"),
            _ => std::thread::sleep(Duration::from_millis(200)),
        }
    };
    std::thread::sleep(Duration::from_secs(2));
    // The bridge still works after its worker restarted.
    ok(home, &h, &["tab", "new", &format!("{origin}/counter")]);
    close(home, &h);
    let dir = recording_dir(home, &h);
    let marks = human_entries(&dir);
    assert_eq!(marks.len(), 1, "worker {old} -> {new}: {marks:?}");
    assert_eq!(marks[0]["what"], "switched to this tab");
    assert_eq!(marks[0]["targetId"], first.as_str());
}

#[test]
#[ignore]
fn network_and_console_are_off_by_default_and_on_with_record() {
    let origin = fixture();
    let home = shared_home();
    let talk = format!("{origin}/talk");
    let plain = create(home, &talk);
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(eval(home, &plain, "window.fetched"), r#"{"ok":true}"#);
    close(home, &plain);
    let dir = recording_dir(home, &plain);
    assert_eq!(manifest(&dir)["record"], json!([]));
    assert!(!dir.join("network.jsonl").exists());
    assert!(!dir.join("console.jsonl").exists());

    let h = create_with(home, &talk, &["--record", "network,console"]);
    let (code, out, err) = home.run(&["session", "info", &h]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("record network,console"), "{out}");
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(eval(home, &h, "window.fetched"), r#"{"ok":true}"#);
    close(home, &h);
    let dir = recording_dir(home, &h);
    assert_eq!(manifest(&dir)["record"], json!(["network", "console"]));
    let read = |f: &str| -> Vec<Value> {
        std::fs::read_to_string(dir.join(f))
            .unwrap_or_else(|e| panic!("{f}: {e}"))
            .lines()
            .map(json)
            .collect()
    };
    let api = format!("{origin}/api");
    let net = read("network.jsonl");
    assert!(
        net.iter()
            .any(|l| l["url"] == api.as_str() && l["status"] == 200 && l["method"] == "GET"),
        "{net:?}"
    );
    let console = read("console.jsonl");
    assert!(
        console
            .iter()
            .any(|l| l["level"] == "log" && l["text"] == "hello console"),
        "{console:?}"
    );
    for f in ["network.jsonl", "console.jsonl"] {
        assert_eq!(mode(&dir.join(f)), 0o600, "{f}");
    }
}

#[test]
#[ignore]
fn network_log_keeps_no_secrets() {
    let origin = fixture();
    let home = shared_home();
    let h = create_with(home, &format!("{origin}/talk"), &["--record", "network"]);
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(eval(home, &h, "window.fetched"), r#"{"ok":true}"#);
    // The request did carry them (the page's own view).
    assert!(eval(home, &h, "document.cookie")
        .as_str()
        .unwrap()
        .contains("SECRET3"));
    close(home, &h);
    let dir = recording_dir(home, &h);
    let text = std::fs::read_to_string(dir.join("network.jsonl")).unwrap();
    assert!(text.contains("/api"), "{text}");
    for secret in [
        "SECRET1",
        "SECRET2",
        "SECRET3",
        "SECRET5",
        "Authorization",
        "authorization",
        "Cookie",
        "cookie",
    ] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    assert!(!dir.join("console.jsonl").exists());
}
