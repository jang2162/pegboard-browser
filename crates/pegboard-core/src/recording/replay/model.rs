//! What the replay page is given: one recording's manifest, actions, navigation, screen
//! indexes and (when recorded) network and console lines, where its incidents are, and for
//! every action and every tab the page can show, which screen to show before and after it.
//!
//! Every judgment is made here (`views`); the page only draws what it is given, so the rules
//! below are the ones the unit tests cover.
//!
//! Everything is read as the store reads it: only a real directory named like a recording id,
//! only regular files (a symbolic link is never followed), a line that is not one record is
//! skipped. Frame names that are not `<targetId>/<file>` as the writer makes them are left
//! out, so the page never asks for anything else.
//!
//! `seq` is the order entries were written, `ts` when they happened (a person's action may be
//! written after a later command): actions and incidents are in time order (`ts`, then `seq`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::actionlog::{recording_dir, ActionEntry, EntryKind, Manifest, ScreenStatus};
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

/// What the page shows for a tab at some moment.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Screen {
    /// The frame `<targetId>/<file>` (its file is there), taken at `ts` on `url`.
    Frame {
        name: String,
        ts: String,
        url: Option<String>,
    },
    /// "화면 기록 꺼짐": the tab was on a screen-off domain.
    Off,
    /// "화면 기록 멈춤(녹화 하나 1GB)".
    Capped,
    /// "화면 지워짐": the retention rules deleted the screen.
    Pruned,
    /// "화면 없음".
    Missing,
}

/// One tab around one action.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabView {
    /// The tab's address at the action (its last navigation then), if known.
    pub url: Option<String>,
    pub before: Screen,
    pub after: Screen,
}

/// What the page shows for one action (same position as in `actions`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionView {
    pub seq: u64,
    /// The moment "after" stands for: the next action's time, else the recording's end
    /// (None while it runs: everything recorded so far).
    pub after_ts: Option<String>,
    /// Every tab of `ReplayModel::tabs`.
    pub tabs: BTreeMap<String, TabView>,
    /// `[from, to)` in `network` / `console`: the lines after the previous action, up to
    /// `after_ts`.
    pub network: [usize; 2],
    pub console: [usize; 2],
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayModel {
    pub manifest: Manifest,
    /// In time order.
    pub actions: Vec<ActionEntry>,
    /// In time order.
    pub navigation: Vec<NavLine>,
    /// Target id -> its screen index (markers, and lines naming a well-formed frame file).
    pub screens: BTreeMap<String, Vec<IndexLine>>,
    /// In time order.
    pub network: Vec<NetworkLine>,
    /// In time order.
    pub console: Vec<ConsoleLine>,
    /// In time order.
    pub incidents: Vec<Incident>,
    /// The first incident, else None (the page starts at the beginning).
    pub start_seq: Option<u64>,
    /// The recording's tabs, in the order they first appear.
    pub tabs: Vec<String>,
    /// One per action.
    pub views: Vec<ActionView>,
}

/// A time as a string that sorts as the time does: an RFC 3339 UTC time
/// (`2026-11-17T10:00:00.12Z`, a fraction of 1 to 9 digits or none) with its fraction filled
/// to nine digits. Anything else is left as it is.
pub fn ts_key(ts: &str) -> String {
    let normal = (|| {
        let body = ts.strip_suffix('Z')?;
        let (secs, frac) = match body.split_once('.') {
            Some((secs, frac)) if !frac.is_empty() => (secs, frac),
            Some(_) => return None,
            None => (body, ""),
        };
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
pub fn frame_at<'a>(
    index: &'a [IndexLine],
    navigation: &[NavLine],
    target: &str,
    ts: &str,
) -> FrameAt<'a> {
    let tab = Tab::new(index, navigation, target, HashSet::new());
    tab.frame_at(&ts_key(ts))
}

/// One tab's index and navigation with their time keys, for many `frame_at` questions.
struct Tab<'a> {
    /// Index lines that count (markers and frames), in index order.
    lines: Vec<(String, &'a IndexLine)>,
    /// Keys of the tab's navigations, sorted.
    navigated: Vec<String>,
    /// (key, url) of the tab's navigations with an address, sorted.
    urls: Vec<(String, String)>,
    /// The frame files present in its directory.
    files: HashSet<String>,
}

impl<'a> Tab<'a> {
    fn new(
        index: &'a [IndexLine],
        navigation: &[NavLine],
        target: &str,
        files: HashSet<String>,
    ) -> Tab<'a> {
        let lines = index
            .iter()
            .filter(|l| matches!(l.kind, FrameKind::Off | FrameKind::Capped) || l.file.is_some())
            .map(|l| (ts_key(&l.ts), l))
            .collect();
        let mine = || {
            navigation
                .iter()
                .filter(move |n| n.target_id == target && n.event == NavEvent::Navigated)
        };
        let mut navigated: Vec<String> = mine().map(|n| ts_key(&n.ts)).collect();
        navigated.sort();
        let mut urls: Vec<(String, String)> = mine()
            .filter_map(|n| Some((ts_key(&n.ts), n.url.clone()?)))
            .collect();
        urls.sort_by(|a, b| a.0.cmp(&b.0));
        Tab {
            lines,
            navigated,
            urls,
            files,
        }
    }

    fn frame_at(&self, at: &str) -> FrameAt<'a> {
        let mut latest: Option<(&str, &'a IndexLine)> = None;
        for (key, line) in &self.lines {
            if key.as_str() > at {
                continue;
            }
            if line.kind == FrameKind::Capped {
                return FrameAt::Capped;
            }
            if latest.is_none_or(|(k, _)| key.as_str() >= k) {
                latest = Some((key, line));
            }
        }
        let Some((key, line)) = latest else {
            return FrameAt::Missing;
        };
        if line.kind == FrameKind::Off {
            return FrameAt::Off;
        }
        // The first navigation strictly after the frame, if it is not after `at`.
        let next = self.navigated.partition_point(|k| k.as_str() <= key);
        match self.navigated.get(next) {
            Some(k) if k.as_str() <= at => FrameAt::Missing,
            _ => FrameAt::Frame(line),
        }
    }

    /// The tab's address at `at`: its last navigation then.
    fn url_at(&self, at: &str) -> Option<String> {
        let n = self.urls.partition_point(|(k, _)| k.as_str() <= at);
        n.checked_sub(1).map(|i| self.urls[i].1.clone())
    }

    /// The screen at `at` as the page shows it.
    fn screen_at(&self, target: &str, at: &str) -> Screen {
        match self.frame_at(at) {
            FrameAt::Frame(line) => {
                let file = line.file.clone().unwrap_or_default();
                if self.files.contains(&file) {
                    Screen::Frame {
                        name: format!("{target}/{file}"),
                        ts: line.ts.clone(),
                        url: line.url.clone(),
                    }
                } else {
                    Screen::Missing
                }
            }
            FrameAt::Off => Screen::Off,
            FrameAt::Capped => Screen::Capped,
            FrameAt::Missing => Screen::Missing,
        }
    }

    /// A frame an action names (`<target>/<file>`), when its file is there.
    fn named(&self, name: &str, ts: &str, url: &Option<String>) -> Screen {
        let present = name
            .split_once('/')
            .is_some_and(|(_, file)| self.files.contains(file));
        if present {
            Screen::Frame {
                name: name.to_string(),
                ts: ts.to_string(),
                url: url.clone(),
            }
        } else {
            Screen::Missing
        }
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

/// The screen indexes under `<dir>/screen` (real directories named like a target id), and
/// the frame files present in each (regular files with a frame's name).
#[allow(clippy::type_complexity)]
fn read_screens(
    dir: &Path,
) -> std::io::Result<(
    BTreeMap<String, Vec<IndexLine>>,
    HashMap<String, HashSet<String>>,
)> {
    let mut screens = BTreeMap::new();
    let mut files = HashMap::new();
    let screen = dir.join("screen");
    if !is_real_dir(&screen) {
        return Ok((screens, files));
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
        let mut present = HashSet::new();
        for f in std::fs::read_dir(entry.path())? {
            let f = f?;
            let Some(name) = f.file_name().to_str().map(String::from) else {
                continue;
            };
            if f.file_type().is_ok_and(|t| t.is_file())
                && valid_frame_name(&format!("{target}/{name}"))
            {
                present.insert(name);
            }
        }
        files.insert(target.clone(), present);
        screens.insert(target, lines);
    }
    Ok((screens, files))
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

/// The recording's tabs (target ids of actions, navigation and screens) in the order they
/// first appear.
fn tabs_of(
    actions: &[ActionEntry],
    navigation: &[NavLine],
    screens: &BTreeMap<String, Vec<IndexLine>>,
) -> Vec<String> {
    let mut first: BTreeMap<String, String> = BTreeMap::new();
    let mut see = |target: &str, key: String| {
        if !valid_target_id(target) {
            return;
        }
        let e = first
            .entry(target.to_string())
            .or_insert_with(|| key.clone());
        if key < *e {
            *e = key;
        }
    };
    for a in actions {
        if let Some(t) = &a.target_id {
            see(t, ts_key(&a.ts));
        }
    }
    for n in navigation {
        see(&n.target_id, ts_key(&n.ts));
    }
    for (t, index) in screens {
        if let Some(key) = index.iter().map(|l| ts_key(&l.ts)).min() {
            see(t, key);
        }
    }
    let mut tabs: Vec<(String, String)> = first.into_iter().collect();
    tabs.sort_by(|a, b| (&a.1, &a.0).cmp(&(&b.1, &b.0)));
    tabs.into_iter().map(|(t, _)| t).collect()
}

/// After every key, so "after" the last action of a running recording is everything so far.
const END: &str = "9999-12-31T23:59:59.999999999";

/// What the page shows around each action, for every tab.
fn views(
    manifest: &Manifest,
    actions: &[ActionEntry],
    tabs: &[String],
    tab: &HashMap<&str, Tab<'_>>,
    network: &[NetworkLine],
    console: &[ConsoleLine],
) -> Vec<ActionView> {
    let pruned = manifest.screen == ScreenStatus::Pruned;
    let net_keys: Vec<String> = network.iter().map(|l| ts_key(&l.ts)).collect();
    let con_keys: Vec<String> = console.iter().map(|l| ts_key(&l.ts)).collect();
    let range = |keys: &[String], from: Option<&str>, to: &str| {
        let start = from.map_or(0, |f| keys.partition_point(|k| k.as_str() <= f));
        let end = keys.partition_point(|k| k.as_str() <= to).max(start);
        [start, end]
    };
    let mut out = Vec::with_capacity(actions.len());
    for (i, a) in actions.iter().enumerate() {
        let after_ts = match actions.get(i + 1) {
            Some(next) => Some(next.ts.clone()),
            None => manifest.ended_at.clone(),
        };
        let before_key = ts_key(&a.ts);
        let after_key = after_ts.as_deref().map_or(END.to_string(), ts_key);
        let mut by_tab = BTreeMap::new();
        for t in tabs {
            let Some(view) = tab.get(t.as_str()) else {
                continue;
            };
            let own = a.target_id.as_deref() == Some(t.as_str());
            let (before, after) = if pruned {
                (Screen::Pruned, Screen::Pruned)
            } else {
                let before = match (&a.frame_before, own) {
                    (Some(name), true) => view.named(name, &a.ts, &a.url),
                    _ => view.screen_at(t, &before_key),
                };
                let after = match (&a.frame_after, own) {
                    (Some(name), true) => view.named(name, &a.ts, &a.url),
                    _ => view.screen_at(t, &after_key),
                };
                (before, after)
            };
            by_tab.insert(
                t.clone(),
                TabView {
                    url: view.url_at(&before_key),
                    before,
                    after,
                },
            );
        }
        let from = i.checked_sub(1).map(|p| ts_key(&actions[p].ts));
        out.push(ActionView {
            seq: a.seq,
            after_ts,
            tabs: by_tab,
            network: range(&net_keys, from.as_deref(), &after_key),
            console: range(&con_keys, from.as_deref(), &after_key),
        });
    }
    out
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
    let mut network: Vec<NetworkLine> = optional_lines(&dir.join("network.jsonl"))?;
    network.sort_by_cached_key(|n| ts_key(&n.ts));
    let mut console: Vec<ConsoleLine> = optional_lines(&dir.join("console.jsonl"))?;
    console.sort_by_cached_key(|n| ts_key(&n.ts));
    let (screens, mut files) = read_screens(&dir)?;
    let tabs = tabs_of(&actions, &navigation, &screens);
    let empty: Vec<IndexLine> = Vec::new();
    let per_tab: HashMap<&str, Tab<'_>> = tabs
        .iter()
        .map(|t| {
            let index = screens.get(t).unwrap_or(&empty);
            let present = files.remove(t).unwrap_or_default();
            (t.as_str(), Tab::new(index, &navigation, t, present))
        })
        .collect();
    let views = views(&manifest, &actions, &tabs, &per_tab, &network, &console);
    drop(per_tab);
    Ok(ReplayModel {
        start_seq: incidents.first().map(|i| i.seq),
        incidents,
        tabs,
        views,
        network,
        console,
        navigation,
        actions,
        manifest,
        screens,
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
    use crate::actionlog::ScreenStatus;
    use crate::actionlog::REDACTED;
    use crate::paths::paths_in;

    const RA_ID: &str = "r_aaaaaaaa";

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

    fn at(t: &str) -> String {
        format!("2026-11-17T10:00:{t}Z")
    }

    fn lines<T: Serialize>(v: &[T]) -> String {
        v.iter()
            .map(|l| serde_json::to_string(l).unwrap() + "\n")
            .collect()
    }

    /// `frame:<name>`, `off`, `capped`, `pruned` or `missing`.
    fn word(s: &Screen) -> String {
        match s {
            Screen::Frame { name, .. } => format!("frame:{name}"),
            Screen::Off => "off".into(),
            Screen::Capped => "capped".into(),
            Screen::Pruned => "pruned".into(),
            Screen::Missing => "missing".into(),
        }
    }

    #[test]
    fn views_judge_every_tab_before_and_after() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        let human = |seq: u64, ts: &str, target: &str| ActionEntry {
            seq,
            ..ActionEntry::human("s_abcdefgh", ts, target, None, "x", false)
        };
        let actions = vec![
            // Names its frames; 000002 never reached the disk.
            command(1, 2, "click", true, None),
            human(2, &at("03"), T1),
            human(3, &at("04"), T1),
            // Written last, happened first.
            human(4, &at("01"), T2),
        ];
        let mut f3 = frame(0, 3);
        f3.ts = at("03.5");
        let dir = plant(&paths, RA_ID, &actions, &[frame(1, 1), f3]);
        let t2 = dir.join("screen").join(T2);
        std::fs::create_dir_all(&t2).unwrap();
        let mut early = frame(0, 1);
        early.ts = at("01.5");
        std::fs::write(t2.join("index.jsonl"), lines(&[early, frame(3, 2)])).unwrap();
        std::fs::write(t2.join("000001.jpg"), "a").unwrap();
        std::fs::write(t2.join("000002.jpg"), "b").unwrap();
        let nav = |ts: String, target: &str, url: &str| NavLine {
            ts,
            target_id: target.into(),
            event: NavEvent::Navigated,
            url: Some(url.into()),
            opener: None,
        };
        // Out of time order in the file.
        std::fs::write(
            dir.join("navigation.jsonl"),
            lines(&[
                nav(at("03"), T1, "https://one.test/next"),
                nav(at("03"), T2, "https://two.test/"),
                nav(at("00.5"), T1, "https://one.test/"),
            ]),
        )
        .unwrap();
        let net = |ts: String| NetworkLine {
            ts,
            target_id: T1.into(),
            method: "GET".into(),
            url: "https://one.test/x".into(),
            status: Some(200),
            bytes: None,
            duration_ms: None,
            error: None,
        };
        std::fs::write(
            dir.join("network.jsonl"),
            lines(&[net(at("03.5")), net(at("01")), net(at("03"))]),
        )
        .unwrap();

        let m = build(&paths, RA_ID).unwrap();
        let nav_ts: Vec<&str> = m.navigation.iter().map(|n| n.ts.as_str()).collect();
        assert_eq!(nav_ts, vec![at("00.5"), at("03"), at("03")]);
        let net_ts: Vec<&str> = m.network.iter().map(|n| n.ts.as_str()).collect();
        assert_eq!(net_ts, vec![at("01"), at("03"), at("03.5")]);
        // T1 appears first (its navigation at 00.5).
        assert_eq!(m.tabs, vec![T1.to_string(), T2.to_string()]);
        let seqs: Vec<u64> = m.views.iter().map(|v| v.seq).collect();
        assert_eq!(seqs, vec![4, 1, 2, 3]);
        let judged = |i: usize, t: &str| {
            let v = &m.views[i].tabs[t];
            (word(&v.before), word(&v.after))
        };
        let f = |t: &str, n: u32| format!("frame:{t}/{n:06}.jpg");
        // seq 4 at :01, tab T2: its first frame is at :01.5 (a fraction after a whole
        // second): nothing before; after (at :02, the next action) that frame.
        assert_eq!(judged(0, T2), ("missing".into(), f(T2, 1)));
        assert_eq!(judged(0, T1), (f(T1, 1), f(T1, 1)));
        assert_eq!(m.views[0].after_ts.as_deref(), Some(ts(2).as_str()));
        // seq 1 names its frames: the one on disk is shown, the one that never came is missing.
        assert_eq!(judged(1, T1), (f(T1, 1), "missing".into()));
        // seq 2 at :03, a person's action in T1: T1 navigated at :03 after its :01 frame, so
        // nothing shows it; after (:04) the :03.5 frame. T2: a frame and a navigation both at
        // :03: the frame counts as the new page.
        assert_eq!(judged(2, T1), ("missing".into(), f(T1, 3)));
        assert_eq!(judged(2, T2), (f(T2, 2), f(T2, 2)));
        assert_eq!(
            m.views[2].tabs[T1].url.as_deref(),
            Some("https://one.test/next")
        );
        assert_eq!(
            m.views[0].tabs[T1].url.as_deref(),
            Some("https://one.test/")
        );
        assert_eq!(m.views[0].tabs[T2].url, None);
        // The last action: "after" is the recording's end.
        assert_eq!(m.views[3].after_ts.as_deref(), Some(ts(59).as_str()));
        assert_eq!(judged(3, T1), (f(T1, 3), f(T1, 3)));
        // Network lines after the previous action, up to "after".
        let ranges: Vec<[usize; 2]> = m.views.iter().map(|v| v.network).collect();
        assert_eq!(ranges, vec![[0, 1], [1, 2], [1, 3], [2, 3]]);
        assert!(m.views.iter().all(|v| v.console == [0, 0]));
        // As the page gets it.
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(
            json["views"][2]["tabs"][T1]["after"],
            serde_json::json!({ "kind": "frame", "name": format!("{T1}/000003.jpg"), "ts": at("03.5"), "url": "https://example.test/" })
        );
        assert_eq!(
            json["views"][2]["tabs"][T1]["before"],
            serde_json::json!({ "kind": "missing" })
        );
        assert_eq!(json["views"][1]["afterTs"], ts(3).to_string());
    }

    #[test]
    fn views_tell_off_capped_and_pruned() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        let actions = vec![
            command(1, 3, "get", true, None),
            command(2, 11, "get", true, None),
        ];
        // Commands that name no frames.
        let actions: Vec<ActionEntry> = actions
            .into_iter()
            .map(|a| ActionEntry {
                frame_before: None,
                frame_after: None,
                ..a
            })
            .collect();
        plant(
            &paths,
            RA_ID,
            &actions,
            &[
                frame(1, 1),
                marker(2, FrameKind::Off),
                marker(10, FrameKind::Capped),
            ],
        );
        let m = build(&paths, RA_ID).unwrap();
        assert_eq!(m.views[0].tabs[T1].before, Screen::Off);
        assert_eq!(m.views[0].tabs[T1].after, Screen::Capped);
        assert_eq!(m.views[1].tabs[T1].before, Screen::Capped);
        // An index line whose file is not on disk (not written, or removed): no frame.
        let dir = plant(
            &paths,
            "r_cccccccc",
            &actions[..1],
            &[frame(1, 1), frame(2, 2)],
        );
        std::fs::remove_file(dir.join("screen").join(T1).join("000002.jpg")).unwrap();
        let m = build(&paths, "r_cccccccc").unwrap();
        assert_eq!(m.views[0].tabs[T1].before, Screen::Missing);
        // Pruned: nothing is shown, whatever the actions name.
        let dir = plant(
            &paths,
            "r_bbbbbbbb",
            &[command(1, 1, "get", true, None)],
            &[frame(1, 1)],
        );
        let mut manifest = crate::actionlog::read_manifest(&dir).unwrap();
        manifest.screen = ScreenStatus::Pruned;
        crate::actionlog::write_manifest_file(&dir, &manifest).unwrap();
        let m = build(&paths, "r_bbbbbbbb").unwrap();
        assert_eq!(m.views[0].tabs[T1].before, Screen::Pruned);
        assert_eq!(m.views[0].tabs[T1].after, Screen::Pruned);
    }

    #[test]
    fn a_running_recording_shows_everything_so_far_after_its_last_action() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        let mut last = command(1, 1, "get", true, None);
        last.frame_after = None;
        let dir = plant(&paths, RA_ID, &[last], &[frame(1, 1), frame(30, 3)]);
        let mut manifest = crate::actionlog::read_manifest(&dir).unwrap();
        manifest.ended_at = None;
        crate::actionlog::write_manifest_file(&dir, &manifest).unwrap();
        let m = build(&paths, RA_ID).unwrap();
        assert_eq!(m.views[0].after_ts, None);
        assert_eq!(
            word(&m.views[0].tabs[T1].after),
            format!("frame:{T1}/000003.jpg")
        );
    }

    #[test]
    fn a_fraction_that_is_not_one_is_left_as_written() {
        assert_eq!(ts_key("2026-11-17T10:00:01.Z"), "2026-11-17T10:00:01.Z");
        assert_eq!(
            ts_key("2026-11-17T10:00:01.1234567890Z"),
            "2026-11-17T10:00:01.1234567890Z"
        );
        assert_eq!(
            ts_key("2026-11-17T10:00:01.1x3Z"),
            "2026-11-17T10:00:01.1x3Z"
        );
        assert_eq!(
            ts_key("2026-11-17T10:00:01.5Z"),
            "2026-11-17T10:00:01.500000000"
        );
    }
}
