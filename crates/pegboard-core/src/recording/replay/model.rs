//! What the replay page is given: one recording's manifest, actions, navigation, screen
//! indexes and (when recorded) network and console lines, plus where its incidents are.
//!
//! Everything is read as the store reads it: only a real directory named like a recording id,
//! only regular files (a symbolic link is never followed), a line that is not one record is
//! skipped. Frame names that are not `<targetId>/<file>` as the writer makes them are left
//! out, so the page never asks for anything else.
//!
//! `seq` is the order entries were written, `ts` when they happened (a person's action may be
//! written after a later command): actions and incidents are in time order (`ts`, then `seq`).

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::actionlog::{recording_dir, ActionEntry, EntryKind, Manifest};
use crate::paths::Paths;
use crate::recording::format::{
    jsonl_lines, ConsoleLine, FrameKind, IndexLine, NavEvent, NavLine, NetworkLine,
};
use crate::recording::screen::{valid_frame_name, valid_target_id};
use crate::recording::store::{is_denied, is_failed};

/// Something a person reviewing a recording looks for first.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Incident {
    pub seq: u64,
    pub ts: String,
    /// `failed`, `denied` (the policy or the command grammar refused it) or `human` (a
    /// person did, or may have done, something in a session tab).
    pub kind: &'static str,
    pub target_id: Option<String>,
    pub frame_before: Option<String>,
    pub frame_after: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayModel {
    pub manifest: Manifest,
    /// In time order.
    pub actions: Vec<ActionEntry>,
    pub navigation: Vec<NavLine>,
    /// Target id -> its screen index (markers, and lines naming a well-formed frame file).
    pub screens: BTreeMap<String, Vec<IndexLine>>,
    pub network: Vec<NetworkLine>,
    pub console: Vec<ConsoleLine>,
    /// In time order.
    pub incidents: Vec<Incident>,
    /// The first incident, else None (the page starts at the beginning).
    pub start_seq: Option<u64>,
}

/// A time as a string that sorts as the time does: an RFC 3339 UTC time
/// (`2026-11-17T10:00:00.12Z`) with its fraction filled to nine digits. Anything else is
/// left as it is. `app.js` has the same function (`tsKey`).
pub fn ts_key(ts: &str) -> String {
    let normal = (|| {
        let body = ts.strip_suffix('Z')?;
        let (secs, frac) = body.split_once('.').unwrap_or((body, ""));
        let ok = secs.len() == 19 && frac.len() <= 9 && frac.bytes().all(|b| b.is_ascii_digit());
        ok.then(|| format!("{secs}.{frac:0<9}"))
    })();
    normal.unwrap_or_else(|| ts.to_string())
}

/// What the screen of a tab was at some time.
#[derive(Debug, PartialEq)]
pub enum FrameAt<'a> {
    Frame(&'a IndexLine),
    /// The tab was on a screen-off domain ("화면 기록 꺼짐").
    Off,
    /// The recording had reached its size limit ("화면 기록 멈춤").
    Capped,
    /// No frame shows it ("화면 없음").
    Missing,
}

/// The frame to show for tab `target` at `ts`: the latest index line with a file at or
/// before `ts`, unless something came after it:
/// - a `capped` marker at or before `ts`: `Capped` (no frame is written after it);
/// - an `off` marker after the frame (or with no frame before it): `Off`, until the next frame;
/// - a navigation of that tab after the frame: `Missing` (the frame shows the page before).
///
/// Of two lines with the same time the later one in the index counts; a frame with the same
/// time as a navigation is taken to show the new page. Nothing at or before `ts`: `Missing`.
/// `app.js` follows the same rule (`frameAt`); change both.
pub fn frame_at<'a>(
    index: &'a [IndexLine],
    navigation: &[NavLine],
    target: &str,
    ts: &str,
) -> FrameAt<'a> {
    let at = ts_key(ts);
    let mut latest: Option<(String, &IndexLine)> = None;
    for line in index {
        let key = ts_key(&line.ts);
        if key > at {
            continue;
        }
        match line.kind {
            FrameKind::Capped => return FrameAt::Capped,
            FrameKind::Off => {}
            _ if line.file.is_some() => {}
            _ => continue,
        }
        if latest.as_ref().is_none_or(|(k, _)| key >= *k) {
            latest = Some((key, line));
        }
    }
    let Some((key, line)) = latest else {
        return FrameAt::Missing;
    };
    if line.kind == FrameKind::Off {
        return FrameAt::Off;
    }
    let navigated = navigation.iter().any(|n| {
        n.target_id == target && n.event == NavEvent::Navigated && {
            let k = ts_key(&n.ts);
            k > key && k <= at
        }
    });
    if navigated {
        FrameAt::Missing
    } else {
        FrameAt::Frame(line)
    }
}

/// Opens a regular file for reading, never through a symbolic link (and without waiting on a
/// FIFO).
pub(crate) fn open_regular(path: &Path) -> std::io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "not a regular file"));
    }
    Ok(file)
}

/// Whether `path` is a directory itself (not a link to one).
pub(crate) fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// `<recordings>/<id>`, when `id` is a recording id and that is a real directory.
pub(crate) fn real_recording_dir(paths: &Paths, id: &str) -> std::io::Result<std::path::PathBuf> {
    let dir = recording_dir(paths, id)?;
    if !is_real_dir(&dir) {
        return Err(Error::new(
            ErrorKind::NotFound,
            format!("no recording {id}"),
        ));
    }
    Ok(dir)
}

/// The lines of an optional JSON-lines file (missing: none).
fn optional_lines<T: DeserializeOwned>(path: &Path) -> std::io::Result<Vec<T>> {
    match open_regular(path) {
        Ok(file) => jsonl_lines(file),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// A frame name kept only when it is one the writer makes.
fn checked_frame(name: Option<String>) -> Option<String> {
    name.filter(|n| valid_frame_name(n))
}

/// The screen indexes under `<dir>/screen`: real directories named like a target id.
fn read_screens(dir: &Path) -> std::io::Result<BTreeMap<String, Vec<IndexLine>>> {
    let mut screens = BTreeMap::new();
    let screen = dir.join("screen");
    if !is_real_dir(&screen) {
        return Ok(screens);
    }
    for entry in std::fs::read_dir(&screen)? {
        let entry = entry?;
        let Some(target) = entry.file_name().to_str().map(String::from) else {
            continue;
        };
        if !valid_target_id(&target) || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let lines: Vec<IndexLine> = optional_lines(&entry.path().join("index.jsonl"))?;
        let lines = lines
            .into_iter()
            .filter(|l| match (&l.kind, &l.file) {
                (FrameKind::Off | FrameKind::Capped, _) => true,
                (_, Some(file)) => valid_frame_name(&format!("{target}/{file}")),
                (_, None) => false,
            })
            .collect();
        screens.insert(target, lines);
    }
    Ok(screens)
}

fn incident_kind(e: &ActionEntry) -> Option<&'static str> {
    if e.kind == EntryKind::Human {
        Some("human")
    } else if is_denied(e) {
        Some("denied")
    } else if is_failed(e) {
        Some("failed")
    } else {
        None
    }
}

/// The replay data of recording `recording_id`. A missing or unreadable recording is an
/// error (`NotFound` when it is not there).
pub fn build(paths: &Paths, recording_id: &str) -> std::io::Result<ReplayModel> {
    let dir = real_recording_dir(paths, recording_id)?;
    let manifest: Manifest =
        serde_json::from_reader(open_regular(&dir.join("manifest.json"))?).map_err(Error::other)?;
    if manifest.recording_id != recording_id {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "the manifest names another recording",
        ));
    }
    let mut actions: Vec<ActionEntry> = jsonl_lines(open_regular(&dir.join("actions.jsonl"))?)?;
    for a in &mut actions {
        a.frame_before = checked_frame(a.frame_before.take());
        a.frame_after = checked_frame(a.frame_after.take());
    }
    actions.sort_by_cached_key(|a| (ts_key(&a.ts), a.seq));
    let incidents: Vec<Incident> = actions
        .iter()
        .filter_map(|a| {
            Some(Incident {
                seq: a.seq,
                ts: a.ts.clone(),
                kind: incident_kind(a)?,
                target_id: a.target_id.clone(),
                frame_before: a.frame_before.clone(),
                frame_after: a.frame_after.clone(),
            })
        })
        .collect();
    let mut navigation: Vec<NavLine> = optional_lines(&dir.join("navigation.jsonl"))?;
    navigation.sort_by_cached_key(|n| ts_key(&n.ts));
    Ok(ReplayModel {
        start_seq: incidents.first().map(|i| i.seq),
        incidents,
        screens: read_screens(&dir)?,
        network: optional_lines(&dir.join("network.jsonl"))?,
        console: optional_lines(&dir.join("console.jsonl"))?,
        navigation,
        actions,
        manifest,
    })
}

/// Recordings on disk for the replay tests.
#[cfg(test)]
pub(crate) mod testkit {
    use std::path::PathBuf;

    use crate::actionlog::{write_manifest_file, ActionEntry, Manifest, ScreenStatus};
    use crate::paths::Paths;
    use crate::recording::format::{FrameKind, IndexLine};

    pub const T1: &str = "0123456789ABCDEF0123456789ABCDEF";
    pub const T2: &str = "FEDCBA9876543210FEDCBA9876543210";

    pub fn ts(sec: u32) -> String {
        format!("2026-11-17T10:00:{sec:02}Z")
    }

    pub fn command(
        seq: u64,
        sec: u32,
        command: &str,
        ok: bool,
        error: Option<&str>,
    ) -> ActionEntry {
        ActionEntry {
            seq,
            ts: ts(sec),
            session_id: "s_abcdefgh".into(),
            target_id: Some(T1.into()),
            command: command.into(),
            ok,
            error: error.map(String::from),
            frame_before: Some(format!("{T1}/{:06}.jpg", seq * 2 - 1)),
            frame_after: Some(format!("{T1}/{:06}.jpg", seq * 2)),
            ..ActionEntry::default()
        }
    }

    pub fn frame(sec: u32, n: u32) -> IndexLine {
        IndexLine {
            ts: ts(sec),
            file: Some(format!("{n:06}.jpg")),
            url: Some("https://example.test/".into()),
            width: Some(10),
            height: Some(10),
            kind: FrameKind::Change,
        }
    }

    pub fn marker(sec: u32, kind: FrameKind) -> IndexLine {
        IndexLine {
            ts: ts(sec),
            file: None,
            url: None,
            width: None,
            height: None,
            kind,
        }
    }

    /// A finished recording `id` with these actions and, for tab `T1`, these index lines
    /// (each frame file is written with a few bytes naming it).
    pub fn plant(paths: &Paths, id: &str, actions: &[ActionEntry], index: &[IndexLine]) -> PathBuf {
        let dir = paths.recordings.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let mut m = Manifest::new(id, "s_abcdefgh", "조사", "PegboardAgent", &ts(0), "c", "a");
        m.ended_at = Some(ts(59));
        m.screen = ScreenStatus::Recorded;
        write_manifest_file(&dir, &m).unwrap();
        let lines: String = actions
            .iter()
            .map(|a| serde_json::to_string(a).unwrap() + "\n")
            .collect();
        std::fs::write(dir.join("actions.jsonl"), lines).unwrap();
        let t = dir.join("screen").join(T1);
        std::fs::create_dir_all(&t).unwrap();
        let lines: String = index
            .iter()
            .map(|l| serde_json::to_string(l).unwrap() + "\n")
            .collect();
        std::fs::write(t.join("index.jsonl"), lines).unwrap();
        for l in index {
            if let Some(f) = l.file.as_ref().filter(|f| !f.contains('/')) {
                std::fs::write(t.join(f), format!("jpeg {id} {f}")).unwrap();
            }
        }
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;
    use crate::actionlog::REDACTED;
    use crate::paths::paths_in;

    fn nav(sec: u32, target: &str) -> NavLine {
        NavLine {
            ts: ts(sec),
            target_id: target.into(),
            event: NavEvent::Navigated,
            url: Some("https://example.test/next".into()),
            opener: None,
        }
    }

    #[test]
    fn model_finds_the_first_incident() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        let human = ActionEntry {
            seq: 7,
            ..ActionEntry::human("s_abcdefgh", &ts(7), T1, None, "clicked", false)
        };
        let actions = vec![
            command(1, 1, "open", true, None),
            command(2, 2, "click", true, None),
            command(3, 3, "click", false, Some("timeout: no such element")),
            command(4, 4, "get", true, None),
            command(5, 5, "open", false, Some("policy_denied: not allowed")),
            command(6, 6, "eval", false, Some("argv_rejected: bad")),
            human,
        ];
        plant(&paths, "r_aaaaaaaa", &actions, &[]);
        let m = build(&paths, "r_aaaaaaaa").unwrap();
        let got: Vec<(u64, &str)> = m.incidents.iter().map(|i| (i.seq, i.kind)).collect();
        assert_eq!(
            got,
            vec![(3, "failed"), (5, "denied"), (6, "denied"), (7, "human")]
        );
        assert_eq!(m.start_seq, Some(3));
        assert_eq!(
            m.incidents[0].frame_before.as_deref(),
            Some(format!("{T1}/000005.jpg").as_str())
        );

        let fine = vec![
            command(1, 1, "open", true, None),
            command(2, 2, "click", true, None),
        ];
        plant(&paths, "r_bbbbbbbb", &fine, &[]);
        let m = build(&paths, "r_bbbbbbbb").unwrap();
        assert!(m.incidents.is_empty());
        assert_eq!(m.start_seq, None);
    }

    #[test]
    fn incidents_follow_the_time_not_the_write_order() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        // A person's switch, written (seq 3) after a later failed command (seq 2).
        let human = ActionEntry {
            seq: 3,
            ..ActionEntry::human("s_abcdefgh", "2026-11-17T10:00:01.5Z", T1, None, "x", false)
        };
        let actions = vec![
            command(1, 1, "open", true, None),
            command(2, 2, "click", false, Some("timeout: x")),
            human,
        ];
        plant(&paths, "r_aaaaaaaa", &actions, &[]);
        let m = build(&paths, "r_aaaaaaaa").unwrap();
        let seqs: Vec<u64> = m.actions.iter().map(|a| a.seq).collect();
        assert_eq!(seqs, vec![1, 3, 2]);
        assert_eq!(m.start_seq, Some(3));
    }

    #[test]
    fn ts_keys_sort_as_times() {
        assert!(ts_key("2026-11-17T10:00:01.5Z") > ts_key("2026-11-17T10:00:01.123456Z"));
        assert!(ts_key("2026-11-17T10:00:01Z") < ts_key("2026-11-17T10:00:01.000001Z"));
        assert_eq!(
            ts_key("2026-11-17T10:00:01Z"),
            ts_key("2026-11-17T10:00:01.000Z")
        );
        assert_eq!(ts_key("garbage"), "garbage");
    }

    #[test]
    fn frame_at_rules() {
        let index = vec![
            frame(1, 1),
            frame(3, 2),
            marker(5, FrameKind::Off),
            frame(7, 3),
        ];
        let none: Vec<NavLine> = Vec::new();
        // Nothing at or before the time.
        assert_eq!(frame_at(&index, &none, T1, &ts(0)), FrameAt::Missing);
        assert_eq!(frame_at(&[], &none, T1, &ts(9)), FrameAt::Missing);
        // The latest frame at or before it.
        assert_eq!(
            frame_at(&index, &none, T1, &ts(1)),
            FrameAt::Frame(&index[0])
        );
        assert_eq!(
            frame_at(&index, &none, T1, &ts(4)),
            FrameAt::Frame(&index[1])
        );
        // An off marker after it, until the next frame.
        assert_eq!(frame_at(&index, &none, T1, &ts(5)), FrameAt::Off);
        assert_eq!(frame_at(&index, &none, T1, &ts(6)), FrameAt::Off);
        assert_eq!(
            frame_at(&index, &none, T1, &ts(8)),
            FrameAt::Frame(&index[3])
        );
        // A navigation of that tab after the frame: the frame shows the page before.
        let navs = vec![nav(2, T1)];
        assert_eq!(frame_at(&index, &navs, T1, &ts(2)), FrameAt::Missing);
        assert_eq!(
            frame_at(&index, &navs, T1, &ts(3)),
            FrameAt::Frame(&index[1])
        );
        // Another tab's navigation does not count; neither does one after the time.
        let other = vec![nav(2, T2)];
        assert_eq!(
            frame_at(&index, &other, T1, &ts(2)),
            FrameAt::Frame(&index[0])
        );
        let later = vec![nav(9, T1)];
        assert_eq!(
            frame_at(&index, &later, T1, &ts(2)),
            FrameAt::Frame(&index[0])
        );
        // A frame at the navigation's time shows the new page.
        let same = vec![nav(3, T1)];
        assert_eq!(
            frame_at(&index, &same, T1, &ts(3)),
            FrameAt::Frame(&index[1])
        );
        // Capped: nothing after it.
        let capped = vec![frame(1, 1), marker(2, FrameKind::Capped)];
        assert_eq!(
            frame_at(&capped, &none, T1, &ts(1)),
            FrameAt::Frame(&capped[0])
        );
        assert_eq!(frame_at(&capped, &none, T1, &ts(3)), FrameAt::Capped);
        // Times with and without a fraction compare as times.
        let fine = vec![IndexLine {
            ts: "2026-11-17T10:00:01.5Z".into(),
            ..frame(0, 9)
        }];
        assert_eq!(frame_at(&fine, &none, T1, &ts(1)), FrameAt::Missing);
        assert_eq!(frame_at(&fine, &none, T1, &ts(2)), FrameAt::Frame(&fine[0]));
    }

    #[test]
    fn data_keeps_redacted_values_redacted() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        let fill = ActionEntry {
            command: "fill".into(),
            args: vec!["@e1".into(), REDACTED.into()],
            redacted: true,
            ..command(1, 1, "fill", true, None)
        };
        plant(&paths, "r_aaaaaaaa", &[fill], &[]);
        let json = serde_json::to_value(build(&paths, "r_aaaaaaaa").unwrap()).unwrap();
        assert_eq!(
            json["actions"][0]["args"],
            serde_json::json!(["@e1", REDACTED])
        );
        assert_eq!(json["actions"][0]["redacted"], true);
    }

    #[test]
    fn malformed_frame_names_and_links_are_left_out() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        let bad = ActionEntry {
            frame_before: Some("../manifest.json".into()),
            frame_after: Some(format!("{T1}/../../x.jpg")),
            ..command(1, 1, "open", true, None)
        };
        let mut odd = frame(2, 2);
        odd.file = Some("../../manifest.json".into());
        let dir = plant(&paths, "r_aaaaaaaa", &[bad], &[frame(1, 1), odd]);
        // A target directory that is a link, and one not named like a target.
        std::os::unix::fs::symlink(dir.join("screen").join(T1), dir.join("screen").join(T2))
            .unwrap();
        std::fs::create_dir(dir.join("screen").join("notes")).unwrap();
        let m = build(&paths, "r_aaaaaaaa").unwrap();
        assert_eq!(m.actions[0].frame_before, None);
        assert_eq!(m.actions[0].frame_after, None);
        assert_eq!(m.screens.keys().collect::<Vec<_>>(), vec![T1]);
        assert_eq!(m.screens[T1].len(), 1);
    }

    #[test]
    fn a_recording_that_is_a_link_or_names_another_is_not_read() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        let dir = plant(
            &paths,
            "r_aaaaaaaa",
            &[command(1, 1, "open", true, None)],
            &[],
        );
        std::os::unix::fs::symlink(&dir, paths.recordings.join("r_bbbbbbbb")).unwrap();
        assert_eq!(
            build(&paths, "r_bbbbbbbb").unwrap_err().kind(),
            ErrorKind::NotFound
        );
        assert!(build(&paths, "r_cccccccc").is_err());
        assert!(build(&paths, "../r_aaaaaaaa").is_err());
        // A copy whose manifest names the first recording.
        let copy = paths.recordings.join("r_dddddddd");
        std::fs::create_dir(&copy).unwrap();
        std::fs::copy(dir.join("manifest.json"), copy.join("manifest.json")).unwrap();
        std::fs::copy(dir.join("actions.jsonl"), copy.join("actions.jsonl")).unwrap();
        assert!(build(&paths, "r_dddddddd").is_err());
        // actions.jsonl that is a link.
        std::fs::remove_file(dir.join("actions.jsonl")).unwrap();
        std::os::unix::fs::symlink(copy.join("actions.jsonl"), dir.join("actions.jsonl")).unwrap();
        assert!(build(&paths, "r_aaaaaaaa").is_err());
    }
}
