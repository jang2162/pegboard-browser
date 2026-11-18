//! The recordings on disk: which are running, what each holds, and the retention rules
//! carried out (`retention::plan`).
//!
//! One lock orders everything that judges "running" against what changes a recording:
//! opening and finishing one, keeping, deleting and pruning. So a recording cannot start
//! running while it is being deleted, and a live recording's manifest is only ever changed
//! through its `ManifestHandle`.
//!
//! Only real directories directly under `recordings/` whose name is a recording id are read
//! or deleted; a symbolic link is never followed, here or inside a recording.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use pegboard_protocol::{codes, RecordingList, RecordingSummary, RpcError};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::actionlog::{
    read_manifest, valid_recording_id, write_manifest_file, ActionEntry, ActionLog, EntryKind,
    Manifest, ManifestHandle, ScreenStatus, Sizes,
};
use crate::paths::Paths;
use crate::profiles::rpc_error;
use crate::recording::config::RecordingConfig;
use crate::recording::format::jsonl_lines;
use crate::recording::retention::{self, Facts, PruneAction};

pub struct RecordingStore {
    paths: Paths,
    /// Parsed once when the daemon started.
    config: RecordingConfig,
    /// The manifests of the recordings being written, by recording id.
    running: Mutex<HashMap<String, ManifestHandle>>,
}

/// The code an entry's `error` starts with (`policy_denied`, `timeout`, ...): what comes
/// before the first `:`, when that is a code word.
pub fn entry_code(e: &ActionEntry) -> Option<&str> {
    let error = e.error.as_deref()?;
    let code = error.split(':').next().unwrap_or("").trim();
    let is_code = !code.is_empty()
        && code.len() <= 40
        && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
    is_code.then_some(code)
}

/// A command the session's policy or the command grammar refused.
pub fn is_denied(e: &ActionEntry) -> bool {
    e.kind == EntryKind::Command
        && !e.ok
        && matches!(
            entry_code(e),
            Some(codes::POLICY_DENIED | codes::ARGV_REJECTED)
        )
}

/// A command that ran (or tried to) and did not succeed.
pub fn is_failed(e: &ActionEntry) -> bool {
    e.kind == EntryKind::Command && !e.ok && !is_denied(e)
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

fn parse_time(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

fn screen_word(s: ScreenStatus) -> &'static str {
    match s {
        ScreenStatus::None => "none",
        ScreenStatus::Recorded => "recorded",
        ScreenStatus::Capped => "capped",
        ScreenStatus::Pruned => "pruned",
    }
}

/// Every byte of the files in `dir` and below, and of those under `screen/`. Links are not
/// followed (nor counted).
pub fn measure(dir: &Path) -> std::io::Result<Sizes> {
    let total = bytes_below(dir)?;
    let screen = match std::fs::symlink_metadata(dir.join("screen")) {
        Ok(m) if m.is_dir() => bytes_below(&dir.join("screen"))?,
        _ => 0,
    };
    Ok(Sizes {
        screen_bytes: screen,
        total_bytes: total,
    })
}

fn bytes_below(dir: &Path) -> std::io::Result<u64> {
    let mut sum = 0u64;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?; // does not follow a link
        if meta.is_dir() {
            sum = sum.saturating_add(bytes_below(&entry.path())?);
        } else if meta.is_file() {
            sum = sum.saturating_add(meta.len());
        }
    }
    Ok(sum)
}

/// The time of the latest write to a file in `dir` or below.
fn last_write(dir: &Path) -> Option<OffsetDateTime> {
    let mut latest: Option<std::time::SystemTime> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                if let Ok(t) = meta.modified() {
                    latest = Some(latest.map_or(t, |l| l.max(t)));
                }
            }
        }
    }
    latest.map(OffsetDateTime::from)
}

/// A file of a recording, read only when it is a regular file (not a link).
fn open_regular(path: &Path) -> std::io::Result<File> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is not a regular file", path.display()),
        ));
    }
    File::open(path)
}

fn read_actions(dir: &Path) -> std::io::Result<Vec<ActionEntry>> {
    jsonl_lines(open_regular(&dir.join("actions.jsonl"))?)
}

fn manifest_of(dir: &Path) -> std::io::Result<Manifest> {
    open_regular(&dir.join("manifest.json"))?;
    read_manifest(dir)
}

fn invalid_id(id: &str) -> RpcError {
    rpc_error(
        codes::INVALID_PARAMS,
        format!("{id:?} is not a recording id"),
    )
}

fn not_found(id: &str) -> RpcError {
    rpc_error(codes::RECORDING_NOT_FOUND, format!("no recording {id}"))
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    rpc_error(codes::INTERNAL, e.to_string())
}

/// What a recording's manifest and action log say, as one summary.
fn summarize(
    m: &Manifest,
    running: bool,
    sizes: Sizes,
    actions: &[ActionEntry],
) -> RecordingSummary {
    let count = |f: &dyn Fn(&ActionEntry) -> bool| actions.iter().filter(|a| f(a)).count() as u64;
    RecordingSummary {
        recording_id: m.recording_id.clone(),
        session_id: m.session_id.clone(),
        session_name: m.session_name.clone(),
        profile: m.profile.clone(),
        started_at: m.started_at.clone(),
        ended_at: if running { None } else { m.ended_at.clone() },
        running,
        keep: m.keep,
        screen: screen_word(m.screen).to_string(),
        screen_bytes: sizes.screen_bytes,
        total_bytes: sizes.total_bytes,
        commands: count(&|a| a.kind == EntryKind::Command),
        failed: count(&is_failed),
        denied: count(&is_denied),
        human: count(&|a| a.kind == EntryKind::Human),
    }
}

/// One recording as found on disk.
struct Found {
    manifest: Manifest,
    dir: PathBuf,
    running: bool,
    sizes: Sizes,
}

impl RecordingStore {
    pub fn new(paths: Paths, config: RecordingConfig) -> Arc<RecordingStore> {
        Arc::new(RecordingStore {
            paths,
            config,
            running: Mutex::new(HashMap::new()),
        })
    }

    pub fn config(&self) -> &RecordingConfig {
        &self.config
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, ManifestHandle>> {
        self.running.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Creates the recording (see `ActionLog::open`) and registers it as running.
    pub fn open(&self, manifest: Manifest) -> std::io::Result<ActionLog> {
        let mut running = self.lock();
        let log = ActionLog::open(&self.paths, manifest)?;
        running.insert(log.manifest().recording_id, log.manifest_handle());
        Ok(log)
    }

    /// Writes the end time and the sizes into the manifest (through its handle) and
    /// unregisters the recording, also when the manifest could not be written.
    pub fn finish(&self, log: &mut ActionLog, ended_at: &str) -> std::io::Result<()> {
        let mut running = self.lock();
        let sizes = measure(log.dir()).ok();
        let written = log.manifest_handle().update(|m| {
            m.ended_at = Some(ended_at.to_string());
            if sizes.is_some() {
                m.sizes = sizes;
            }
        });
        running.remove(&log.manifest().recording_id);
        written
    }

    /// `recordings/<id>` when it is a real directory (not a link).
    fn dir_of(&self, id: &str) -> Result<PathBuf, RpcError> {
        if !valid_recording_id(id) {
            return Err(invalid_id(id));
        }
        let dir = self.paths.recordings.join(id);
        match std::fs::symlink_metadata(&dir) {
            Ok(m) if m.is_dir() => Ok(dir),
            Ok(_) => Err(not_found(id)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(not_found(id)),
            Err(e) => Err(internal(e)),
        }
    }

    /// The recording directories: real directories directly under `recordings/` named like
    /// a recording id.
    fn dirs(&self) -> std::io::Result<Vec<(String, PathBuf)>> {
        let entries = match std::fs::read_dir(&self.paths.recordings) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut dirs = Vec::new();
        for entry in entries {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(String::from) else {
                continue;
            };
            if !valid_recording_id(&name) {
                continue;
            }
            // `DirEntry::file_type` does not follow a link.
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                dirs.push((name, entry.path()));
            }
        }
        dirs.sort();
        Ok(dirs)
    }

    /// The recording `id`, with its manifest (a running one's as its writers hold it).
    fn found(
        &self,
        running: &HashMap<String, ManifestHandle>,
        id: &str,
        dir: PathBuf,
    ) -> std::io::Result<Found> {
        let (manifest, live) = match running.get(id) {
            Some(handle) => (handle.get(), true),
            None => (manifest_of(&dir)?, false),
        };
        if manifest.recording_id != id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("its manifest names {:?}", manifest.recording_id),
            ));
        }
        let sizes = match manifest.sizes {
            Some(sizes) if !live => sizes,
            _ => measure(&dir)?,
        };
        Ok(Found {
            manifest,
            dir,
            running: live,
            sizes,
        })
    }

    /// Every recording whose manifest can be read; the others are told in daemon.log when
    /// `tell` and left out.
    fn all(
        &self,
        running: &HashMap<String, ManifestHandle>,
        tell: bool,
    ) -> std::io::Result<Vec<Found>> {
        let mut found = Vec::new();
        for (id, dir) in self.dirs()? {
            match self.found(running, &id, dir) {
                Ok(f) => found.push(f),
                Err(e) if tell => {
                    eprintln!(
                        "pegboard: recording {id} is left alone: its manifest cannot be read: {e}"
                    )
                }
                Err(_) => {}
            }
        }
        Ok(found)
    }

    fn summary_of(f: &Found) -> RecordingSummary {
        let actions = read_actions(&f.dir).unwrap_or_default();
        summarize(&f.manifest, f.running, f.sizes, &actions)
    }

    /// Every recording, oldest first, and their bytes.
    pub fn list(&self) -> std::io::Result<RecordingList> {
        let found = {
            let running = self.lock();
            self.all(&running, false)?
        };
        let mut recordings: Vec<RecordingSummary> =
            found.iter().map(RecordingStore::summary_of).collect();
        recordings.sort_by(|a, b| {
            (&a.started_at, &a.recording_id).cmp(&(&b.started_at, &b.recording_id))
        });
        let total_bytes = recordings.iter().map(|r| r.total_bytes).sum();
        let kept_bytes = recordings
            .iter()
            .filter(|r| r.keep)
            .map(|r| r.total_bytes)
            .sum();
        Ok(RecordingList {
            recordings,
            total_bytes,
            kept_bytes,
        })
    }

    /// The manifest and actions of a recording (`recording.get`).
    pub fn get(&self, id: &str) -> Result<(Manifest, Vec<ActionEntry>), RpcError> {
        let dir = self.dir_of(id)?;
        let manifest = match self.lock().get(id) {
            Some(handle) => handle.get(),
            None => manifest_of(&dir).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => not_found(id),
                _ => internal(e),
            })?,
        };
        let actions = read_actions(&dir).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => not_found(id),
            _ => internal(e),
        })?;
        Ok((manifest, actions))
    }

    /// Marks a recording kept (or not). A running one is changed through its handle, so its
    /// writers keep the mark.
    pub fn set_keep(&self, id: &str, keep: bool) -> Result<RecordingSummary, RpcError> {
        let dir = self.dir_of(id)?;
        let running = self.lock();
        if let Some(handle) = running.get(id) {
            handle.update(|m| m.keep = keep).map_err(internal)?;
        } else {
            let mut m = manifest_of(&dir).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => not_found(id),
                _ => internal(e),
            })?;
            m.keep = keep;
            write_manifest_file(&dir, &m).map_err(internal)?;
        }
        let found = self.found(&running, id, dir).map_err(internal)?;
        Ok(RecordingStore::summary_of(&found))
    }

    /// Deletes a recording that is not running (`recording_running` for one that is).
    pub fn delete(&self, id: &str) -> Result<(), RpcError> {
        let dir = self.dir_of(id)?;
        let running = self.lock();
        if running.contains_key(id) {
            return Err(rpc_error(
                codes::RECORDING_RUNNING,
                format!("recording {id} is still being written; close its session first"),
            ));
        }
        std::fs::remove_dir_all(&dir).map_err(internal)
    }

    /// Startup: a recording that is not running and has no ended_at gets the time of its last write.
    pub fn close_orphans(&self) -> std::io::Result<usize> {
        let running = self.lock();
        let mut closed = 0;
        for (id, dir) in self.dirs()? {
            if running.contains_key(&id) {
                continue;
            }
            let Ok(mut m) = manifest_of(&dir) else {
                continue;
            };
            if m.ended_at.is_some() || m.recording_id != id {
                continue;
            }
            let ended = last_write(&dir)
                .or_else(|| parse_time(&m.started_at))
                .unwrap_or_else(OffsetDateTime::now_utc);
            m.ended_at = Some(rfc3339(ended));
            if let Ok(sizes) = measure(&dir) {
                m.sizes = Some(sizes);
            }
            match write_manifest_file(&dir, &m) {
                Ok(()) => closed += 1,
                Err(e) => eprintln!("pegboard: recording {id}: its end could not be written: {e}"),
            }
        }
        Ok(closed)
    }

    /// Applies the retention rules: what `retention::plan` says, judged and carried out
    /// under the store's lock. A dry run only answers the plan. Otherwise the answer is
    /// what was done; what could not be done is told in daemon.log.
    pub fn prune(&self, now: OffsetDateTime, dry_run: bool) -> std::io::Result<Vec<PruneAction>> {
        let running = self.lock();
        let found = self.all(&running, true)?;
        let facts: Vec<Facts> = found
            .iter()
            .map(|f| {
                let last = || last_write(&f.dir);
                let started_at = parse_time(&f.manifest.started_at)
                    .or_else(last)
                    .unwrap_or(now);
                // A recording cut off without an end counts from its last write.
                let ended_at = if f.running {
                    None
                } else {
                    f.manifest
                        .ended_at
                        .as_deref()
                        .and_then(parse_time)
                        .or_else(last)
                        .or(Some(started_at))
                };
                Facts {
                    id: f.manifest.recording_id.clone(),
                    started_at,
                    ended_at,
                    running: f.running,
                    keep: f.manifest.keep,
                    screen_bytes: f.sizes.screen_bytes,
                    other_bytes: f.sizes.total_bytes.saturating_sub(f.sizes.screen_bytes),
                }
            })
            .collect();
        let plan = retention::plan(&facts, now, &self.config);
        if dry_run {
            return Ok(plan);
        }
        let mut done = Vec::new();
        for action in plan {
            // Judged once more under the same lock: nothing running is touched.
            if running.contains_key(action.id()) {
                continue;
            }
            match self.carry_out(&action) {
                Ok(()) => done.push(action),
                Err(e) => eprintln!("pegboard: recording {}: not pruned: {e}", action.id()),
            }
        }
        Ok(done)
    }

    fn carry_out(&self, action: &PruneAction) -> std::io::Result<()> {
        let dir = self
            .dir_of(action.id())
            .map_err(|e| std::io::Error::other(e.message))?;
        match action {
            PruneAction::Delete { .. } => std::fs::remove_dir_all(&dir),
            PruneAction::DeleteScreen { .. } => {
                let screen = dir.join("screen");
                match std::fs::symlink_metadata(&screen) {
                    Ok(m) if m.is_dir() => std::fs::remove_dir_all(&screen)?,
                    Ok(_) => std::fs::remove_file(&screen)?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
                let mut m = manifest_of(&dir)?;
                m.screen = ScreenStatus::Pruned;
                m.sizes = Some(measure(&dir)?);
                write_manifest_file(&dir, &m)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::paths_in;
    use std::os::unix::fs::symlink;
    use std::time::SystemTime;
    use time::Duration;

    fn manifest(id: &str, started: &str, ended: Option<&str>) -> Manifest {
        let mut m = Manifest::new(
            id,
            &format!("s_{}", &id[2..]),
            "demo",
            "default",
            started,
            "157",
            "0.42.0",
        );
        m.ended_at = ended.map(String::from);
        m
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        paths: Paths,
    }

    impl Fixture {
        fn new() -> Fixture {
            let tmp = tempfile::tempdir().unwrap();
            let paths = paths_in(tmp.path());
            std::fs::create_dir_all(&paths.recordings).unwrap();
            Fixture { _tmp: tmp, paths }
        }

        fn store(&self, config: RecordingConfig) -> Arc<RecordingStore> {
            RecordingStore::new(self.paths.clone(), config)
        }

        fn dir(&self, id: &str) -> PathBuf {
            self.paths.recordings.join(id)
        }

        /// A finished recording on disk, with `screen` bytes of frames and these actions.
        fn plant(&self, m: &Manifest, screen: usize, actions: &[ActionEntry]) -> PathBuf {
            let dir = self.dir(&m.recording_id);
            std::fs::create_dir_all(&dir).unwrap();
            write_manifest_file(&dir, m).unwrap();
            let lines: String = actions
                .iter()
                .map(|a| serde_json::to_string(a).unwrap() + "\n")
                .collect();
            std::fs::write(dir.join("actions.jsonl"), lines).unwrap();
            if screen > 0 {
                let t = dir.join("screen").join("A".repeat(32));
                std::fs::create_dir_all(&t).unwrap();
                std::fs::write(t.join("000001.jpg"), vec![0u8; screen]).unwrap();
            }
            dir
        }
    }

    fn set_mtime_below(dir: &Path, t: SystemTime) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                set_mtime_below(&path, t);
            } else {
                File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(t)
                    .unwrap();
            }
        }
    }

    fn ago(d: Duration) -> String {
        rfc3339(OffsetDateTime::now_utc() - d)
    }

    fn command(ok: bool, error: Option<&str>) -> ActionEntry {
        ActionEntry {
            command: "click".into(),
            ok,
            error: error.map(String::from),
            ..ActionEntry::default()
        }
    }

    /// Sizes counted before the manifest was written with them: only the manifest's own
    /// growth differs.
    fn assert_close(counted: Sizes, now: Sizes) {
        assert_eq!(counted.screen_bytes, now.screen_bytes);
        assert!(
            now.total_bytes >= counted.total_bytes && now.total_bytes - counted.total_bytes < 200,
            "{counted:?} {now:?}"
        );
    }

    fn tight() -> RecordingConfig {
        RecordingConfig {
            max_total_gb: 0.001,
            ..RecordingConfig::default()
        }
    }

    #[test]
    fn an_orphan_counts_from_its_last_write() {
        let fx = Fixture::new();
        let m = manifest("r_orphanaa", &ago(Duration::days(21)), None);
        let dir = fx.plant(&m, 10, &[command(true, None)]);
        let twenty_days = std::time::Duration::from_secs(20 * 86_400);
        let then = SystemTime::now() - twenty_days;
        set_mtime_below(&dir, then);
        let store = fx.store(RecordingConfig::default());
        assert_eq!(store.close_orphans().unwrap(), 1);
        let m = read_manifest(&dir).unwrap();
        let ended = parse_time(m.ended_at.as_deref().unwrap()).unwrap();
        let expected = OffsetDateTime::from(then);
        assert!(
            (ended - expected).abs() < Duration::seconds(2),
            "{ended} {expected}"
        );
        assert!(m
            .sizes
            .is_some_and(|s| s.total_bytes > 10 && s.screen_bytes == 10));
        // Closed once.
        assert_eq!(store.close_orphans().unwrap(), 0);
        let done = store.prune(OffsetDateTime::now_utc(), false).unwrap();
        assert_eq!(
            done,
            vec![PruneAction::Delete {
                id: "r_orphanaa".into()
            }]
        );
        assert!(!dir.exists());
    }

    #[test]
    fn an_orphan_without_an_end_is_pruned_by_its_last_write_too() {
        // Not closed at start (say the manifest could not be written then): prune still
        // judges it by its last write, not as running.
        let fx = Fixture::new();
        let m = manifest("r_orphanbb", &ago(Duration::days(21)), None);
        let dir = fx.plant(&m, 0, &[]);
        set_mtime_below(
            &dir,
            SystemTime::now() - std::time::Duration::from_secs(15 * 86_400),
        );
        let store = fx.store(RecordingConfig::default());
        let plan = store.prune(OffsetDateTime::now_utc(), true).unwrap();
        assert_eq!(
            plan,
            vec![PruneAction::Delete {
                id: "r_orphanbb".into()
            }]
        );
    }

    #[test]
    fn an_unreadable_manifest_is_left_alone() {
        let fx = Fixture::new();
        let dir = fx.dir("r_garbageg");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), "{not json").unwrap();
        std::fs::write(dir.join("actions.jsonl"), "").unwrap();
        set_mtime_below(
            &dir,
            SystemTime::now() - std::time::Duration::from_secs(60 * 86_400),
        );
        // A manifest naming another recording is not this one's either.
        let other = manifest(
            "r_otherrrr",
            &ago(Duration::days(40)),
            Some(&ago(Duration::days(40))),
        );
        let wrong = fx.dir("r_wrongnam");
        std::fs::create_dir_all(&wrong).unwrap();
        write_manifest_file(&wrong, &other).unwrap();
        let store = fx.store(tight());
        assert_eq!(store.close_orphans().unwrap(), 0);
        assert_eq!(
            store.prune(OffsetDateTime::now_utc(), false).unwrap(),
            vec![]
        );
        assert!(dir.join("manifest.json").exists());
        assert!(wrong.join("manifest.json").exists());
        assert!(store.list().unwrap().recordings.is_empty());
    }

    #[test]
    fn only_real_recording_dirs_are_deleted() {
        let fx = Fixture::new();
        let outside = tempfile::tempdir().unwrap();
        let old = ago(Duration::days(30));
        // A recording-looking directory outside, reached by a link named like a recording.
        let target = outside.path().join("r_linklink");
        std::fs::create_dir_all(&target).unwrap();
        write_manifest_file(&target, &manifest("r_linklink", &old, Some(&old))).unwrap();
        std::fs::write(target.join("actions.jsonl"), "").unwrap();
        symlink(&target, fx.dir("r_linklink")).unwrap();
        // A directory that is not named like a recording, with an old recording inside.
        let notes = fx.dir("notes");
        std::fs::create_dir_all(&notes).unwrap();
        write_manifest_file(&notes, &manifest("r_notesnot", &old, Some(&old))).unwrap();
        // A file named like a recording.
        std::fs::write(fx.dir("r_abcdefgh"), "x").unwrap();
        // Inside a real recording, a link to the outside as its screen.
        let real = fx.plant(&manifest("r_realreal", &old, Some(&old)), 0, &[]);
        let outside_screen = outside.path().join("screen");
        std::fs::create_dir_all(&outside_screen).unwrap();
        std::fs::write(outside_screen.join("keep.jpg"), vec![0u8; 5000]).unwrap();
        symlink(&outside_screen, real.join("screen")).unwrap();

        let store = fx.store(tight());
        let done = store.prune(OffsetDateTime::now_utc(), false).unwrap();
        assert_eq!(
            done,
            vec![PruneAction::Delete {
                id: "r_realreal".into()
            }]
        );
        assert!(!real.exists());
        assert!(target.join("manifest.json").exists());
        assert!(std::fs::symlink_metadata(fx.dir("r_linklink")).is_ok());
        assert!(notes.join("manifest.json").exists());
        assert!(fx.dir("r_abcdefgh").is_file());
        assert!(outside_screen.join("keep.jpg").exists());
        // Nor through the other paths.
        for id in ["r_linklink", "r_abcdefgh"] {
            assert_eq!(
                store.delete(id).unwrap_err().code,
                codes::RECORDING_NOT_FOUND
            );
            assert_eq!(
                store.set_keep(id, true).unwrap_err().code,
                codes::RECORDING_NOT_FOUND
            );
            assert_eq!(store.get(id).unwrap_err().code, codes::RECORDING_NOT_FOUND);
        }
        for bad in ["../r_linklink", "/etc", "r_", "notes", "r_abcdefgh/.."] {
            assert_eq!(store.delete(bad).unwrap_err().code, codes::INVALID_PARAMS);
            assert_eq!(store.get(bad).unwrap_err().code, codes::INVALID_PARAMS);
        }
        assert!(target.join("manifest.json").exists());
        assert!(store.list().unwrap().recordings.is_empty());
    }

    #[test]
    fn delete_refuses_a_running_recording() {
        let fx = Fixture::new();
        let store = fx.store(RecordingConfig::default());
        let mut log = store
            .open(manifest("r_runnnnnn", &ago(Duration::days(1)), None))
            .unwrap();
        let e = store.delete("r_runnnnnn").unwrap_err();
        assert_eq!(e.code, codes::RECORDING_RUNNING);
        assert!(fx.dir("r_runnnnnn").join("manifest.json").exists());
        let list = store.list().unwrap();
        assert!(list.recordings[0].running);
        assert_eq!(list.recordings[0].ended_at, None);
        store.finish(&mut log, &ago(Duration::ZERO)).unwrap();
        store.delete("r_runnnnnn").unwrap();
        assert!(!fx.dir("r_runnnnnn").exists());
        assert_eq!(
            store.delete("r_runnnnnn").unwrap_err().code,
            codes::RECORDING_NOT_FOUND
        );
    }

    #[test]
    fn keep_on_a_running_recording_survives_its_finish() {
        let fx = Fixture::new();
        let store = fx.store(tight());
        let mut log = store
            .open(manifest("r_keeprunn", &ago(Duration::days(30)), None))
            .unwrap();
        let s = store.set_keep("r_keeprunn", true).unwrap();
        assert!(s.keep && s.running, "{s:?}");
        // A writer of the recording changes the manifest after: the mark stays.
        log.manifest_handle()
            .update(|m| m.screen = ScreenStatus::Recorded)
            .unwrap();
        // Every file of the recording counts: line logs and frames too.
        let dir = fx.dir("r_keeprunn");
        for (name, n) in [
            ("navigation.jsonl", 100),
            ("network.jsonl", 200),
            ("console.jsonl", 300),
        ] {
            std::fs::write(dir.join(name), vec![b'x'; n]).unwrap();
        }
        let t = dir.join("screen").join("B".repeat(32));
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(t.join("000001.jpg"), vec![0u8; 2_000_000]).unwrap();
        store.finish(&mut log, &ago(Duration::days(20))).unwrap();
        let m = read_manifest(&dir).unwrap();
        assert!(m.keep);
        assert_eq!(m.screen, ScreenStatus::Recorded);
        let sizes = m.sizes.unwrap();
        assert_eq!(sizes.screen_bytes, 2_000_000);
        assert_close(sizes, measure(&dir).unwrap());
        assert!(sizes.total_bytes >= 2_000_600, "{sizes:?}");
        // Old, over the limit, and kept: nothing happens to it.
        assert_eq!(
            store.prune(OffsetDateTime::now_utc(), false).unwrap(),
            vec![]
        );
        let list = store.list().unwrap();
        assert_eq!(list.kept_bytes, sizes.total_bytes);
        assert_eq!(list.total_bytes, sizes.total_bytes);
        // Unkept, it goes.
        assert!(!store.set_keep("r_keeprunn", false).unwrap().keep);
        assert_eq!(
            store.prune(OffsetDateTime::now_utc(), false).unwrap(),
            vec![PruneAction::Delete {
                id: "r_keeprunn".into()
            }]
        );
    }

    #[test]
    fn a_running_recording_is_never_pruned() {
        let fx = Fixture::new();
        let store = fx.store(tight());
        let _log = store
            .open(manifest("r_runnnnnn", &ago(Duration::days(30)), None))
            .unwrap();
        let t = fx.dir("r_runnnnnn").join("screen").join("C".repeat(32));
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(t.join("000001.jpg"), vec![0u8; 2_000_000]).unwrap();
        let later = OffsetDateTime::now_utc() + Duration::days(60);
        assert_eq!(store.prune(later, false).unwrap(), vec![]);
        assert!(t.join("000001.jpg").exists());
        // Nor taken for an orphan.
        assert_eq!(store.close_orphans().unwrap(), 0);
        assert_eq!(read_manifest(&fx.dir("r_runnnnnn")).unwrap().ended_at, None);
    }

    #[test]
    fn pruning_the_screen_marks_the_manifest() {
        let fx = Fixture::new();
        let mut m = manifest(
            "r_screenss",
            &ago(Duration::hours(2)),
            Some(&ago(Duration::hours(1))),
        );
        m.screen = ScreenStatus::Capped;
        let dir = fx.plant(&m, 1_500_000, &[command(true, None)]);
        std::fs::write(dir.join("navigation.jsonl"), "{}\n").unwrap();
        let store = fx.store(tight());
        let plan = store.prune(OffsetDateTime::now_utc(), true).unwrap();
        assert_eq!(
            plan,
            vec![PruneAction::DeleteScreen {
                id: "r_screenss".into()
            }]
        );
        assert!(dir.join("screen").exists(), "a dry run changes nothing");
        assert_eq!(read_manifest(&dir).unwrap().screen, ScreenStatus::Capped);
        assert_eq!(store.prune(OffsetDateTime::now_utc(), false).unwrap(), plan);
        assert!(!dir.join("screen").exists());
        assert!(dir.join("actions.jsonl").exists());
        assert!(dir.join("navigation.jsonl").exists());
        let m = read_manifest(&dir).unwrap();
        assert_eq!(m.screen, ScreenStatus::Pruned);
        assert_close(m.sizes.unwrap(), measure(&dir).unwrap());
        assert_eq!(m.sizes.unwrap().screen_bytes, 0);
        let s = &store.list().unwrap().recordings[0];
        assert_eq!(s.screen, "pruned");
        assert_eq!(s.screen_bytes, 0);
        // Nothing more to do.
        assert_eq!(
            store.prune(OffsetDateTime::now_utc(), false).unwrap(),
            vec![]
        );
    }

    #[test]
    fn summary_counts() {
        let fx = Fixture::new();
        let human =
            ActionEntry::human("s_countsss", "t", "T1", None, "switched to this tab", false);
        let actions = [
            command(true, None),
            command(true, None),
            command(false, Some("Element not found: #x")),
            command(false, Some("timeout")),
            command(false, Some(codes::POLICY_DENIED)),
            command(
                false,
                Some("policy_denied: details withheld (entry redacted)"),
            ),
            command(false, Some(codes::ARGV_REJECTED)),
            command(false, None),
            human.clone(),
            human,
        ];
        let m = manifest(
            "r_countsss",
            "2026-01-01T00:00:00Z",
            Some("2026-01-01T00:10:00Z"),
        );
        fx.plant(&m, 0, &actions);
        let mut kept = manifest(
            "r_keptkept",
            "2026-01-02T00:00:00Z",
            Some("2026-01-02T00:10:00Z"),
        );
        kept.keep = true;
        fx.plant(&kept, 300, &[]);
        let list = fx.store(RecordingConfig::default()).list().unwrap();
        let s = &list.recordings[0];
        assert_eq!(s.recording_id, "r_countsss");
        assert_eq!(
            (s.commands, s.failed, s.denied, s.human),
            (8, 3, 3, 2),
            "{s:?}"
        );
        assert!(!s.running && !s.keep);
        assert_eq!(s.screen, "none");
        let k = &list.recordings[1];
        assert!(k.keep);
        assert_eq!(k.screen_bytes, 300);
        assert_eq!(list.kept_bytes, k.total_bytes);
        assert_eq!(list.total_bytes, s.total_bytes + k.total_bytes);
    }
}
