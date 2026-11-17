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

/// F5 across a reattach: the recorder lost the tab while it was on a screen-off page, and
/// the tab went on to a page that paints late. Until that page's first contentful paint the
/// screen may still be the off page's, so nothing is written before it.
#[test]
#[ignore]
fn a_tab_reattached_after_leaving_an_off_page_writes_nothing_before_the_paint() {
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
    // While the recorder is away: to a page that paints 2.5 s after it loads.
    ok(home, &h, &["open", &format!("{origin}/late")]);
    std::thread::sleep(Duration::from_secs(5));
    let painted = eval(home, &h, "window.paintedAt")
        .as_f64()
        .expect("no paint") as i64;
    close(home, &h);

    let dir = recording_dir(home, &h);
    assert_index_matches_files(&dir);
    let frames = changes(&dir, &a);
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
            "a frame {} ms before the paint: {f}",
            painted - t
        );
    }
    assert!(
        frames
            .iter()
            .any(|f| ts_ms(f["ts"].as_str().unwrap()) >= painted),
        "no frame after the paint: {:?}",
        index(&dir, &a)
    );
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
