//! The files of a recording besides `manifest.json` and `actions.jsonl` (see
//! `actionlog`): `navigation.jsonl`, `screen/<targetId>/index.jsonl` with its frames, and the
//! optional `network.jsonl` and `console.jsonl`. Directories are owner-only (0700), files
//! too (0600), and a file is never opened twice for writing: the writers create theirs.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::actionlog::MAX_LINE_BYTES;

/// Why a frame was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameKind {
    /// The screen changed.
    Change,
    /// Taken just before a command.
    Before,
    /// Taken just after a command.
    After,
    /// From here the tab is on a screen-off domain: no frames until the next frame.
    Off,
    /// From here the recording is over its size limit: no more frames.
    Capped,
}

/// One line of screen/<targetId>/index.jsonl. `Off` and `Capped` are markers without a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexLine {
    pub ts: String,
    /// The frame's file name in the same directory.
    pub file: Option<String>,
    pub url: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub kind: FrameKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NavEvent {
    Created,
    Navigated,
    Closed,
}

/// One line of navigation.jsonl: a tab of the session was opened, went to a page (main
/// frame) or was closed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavLine {
    pub ts: String,
    pub target_id: String,
    pub event: NavEvent,
    pub url: Option<String>,
    /// The tab that opened this one.
    pub opener: Option<String>,
}

/// Creates the directory with mode 0700 (its parent must exist). An existing directory is
/// made 0700; an existing symlink (even to a directory) or anything else is refused.
pub fn create_private_dir(path: &Path) -> std::io::Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let meta = std::fs::symlink_metadata(path)?;
            if !meta.file_type().is_dir() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("{} exists and is not a directory", path.display()),
                ));
            }
        }
        Err(e) => return Err(e),
    }
    // The umask may have taken bits away, never added them; an existing one may be looser.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

fn create_new_private(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// Writes a new file (mode 0600) with these bytes. An existing file or link is refused.
pub fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = create_new_private(path)?;
    file.write_all(bytes)?;
    file.flush()
}

/// Appends JSON lines to a file it created (mode 0600). Lines are written unbuffered, each
/// in one write, so a reader sees whole lines (and at most a cut last one after a crash).
pub struct JsonlWriter {
    file: File,
}

impl JsonlWriter {
    /// Creates the file; an existing file or link is refused.
    pub fn create(path: &Path) -> std::io::Result<JsonlWriter> {
        let file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        Ok(JsonlWriter { file })
    }

    /// Writes the value as one line and returns the bytes written. A line longer than
    /// `MAX_LINE_BYTES` (with its newline) is refused and nothing is written.
    pub fn append<T: Serialize>(&mut self, line: &T) -> std::io::Result<u64> {
        let mut bytes = serde_json::to_vec(line).map_err(std::io::Error::other)?;
        bytes.push(b'\n');
        if bytes.len() > MAX_LINE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("a line of {} bytes is over the limit", bytes.len()),
            ));
        }
        self.file.write_all(&bytes)?;
        Ok(bytes.len() as u64)
    }
}

/// The lines of a JSON-lines file. A missing file has none; a line that is not one `T`
/// (empty, cut off, not UTF-8, another shape) is skipped.
pub fn read_jsonl<T: DeserializeOwned>(path: &Path) -> std::io::Result<Vec<T>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut lines = Vec::new();
    for line in BufReader::new(file).split(b'\n') {
        if let Ok(value) = serde_json::from_slice(&line?) {
            lines.push(value);
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn private_files_and_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("screen");
        create_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        // Again: an existing directory is fine, and is made owner-only.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        create_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        // A symlink (even to a directory) and a file are refused.
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        assert!(create_private_dir(&link).is_err());
        let file = tmp.path().join("file");
        std::fs::write(&file, "x").unwrap();
        assert!(create_private_dir(&file).is_err());
        // A missing parent is not created.
        assert!(create_private_dir(&tmp.path().join("no/such")).is_err());

        let frame = dir.join("000001.jpg");
        write_private_file(&frame, b"\xff\xd8jpeg").unwrap();
        assert_eq!(mode(&frame), 0o600);
        assert_eq!(std::fs::read(&frame).unwrap(), b"\xff\xd8jpeg");
        // Never overwritten, never written through a link.
        assert!(write_private_file(&frame, b"other").is_err());
        assert_eq!(std::fs::read(&frame).unwrap(), b"\xff\xd8jpeg");
        let target = tmp.path().join("target");
        std::os::unix::fs::symlink(&target, dir.join("000002.jpg")).unwrap();
        assert!(write_private_file(&dir.join("000002.jpg"), b"x").is_err());
        assert!(!target.exists());

        let index = dir.join("index.jsonl");
        let mut w = JsonlWriter::create(&index).unwrap();
        assert_eq!(mode(&index), 0o600);
        let line = IndexLine {
            ts: "2026-01-01T00:00:00Z".into(),
            file: Some("000001.jpg".into()),
            url: Some("https://a.test/".into()),
            width: Some(1280),
            height: Some(800),
            kind: FrameKind::Change,
        };
        let n = w.append(&line).unwrap();
        assert_eq!(n as usize, serde_json::to_vec(&line).unwrap().len() + 1);
        let off = IndexLine {
            ts: "2026-01-01T00:00:01Z".into(),
            file: None,
            url: None,
            width: None,
            height: None,
            kind: FrameKind::Off,
        };
        w.append(&off).unwrap();
        // A line over the limit is refused, and nothing of it is written.
        let huge = IndexLine {
            url: Some("x".repeat(MAX_LINE_BYTES)),
            ..line.clone()
        };
        assert!(w.append(&huge).is_err());
        let text = std::fs::read_to_string(&index).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains(r#""kind":"change""#), "{text}");
        assert!(text.contains(r#""kind":"off""#), "{text}");
        assert_eq!(read_jsonl::<IndexLine>(&index).unwrap(), vec![line, off]);
        // An existing file (a second writer, or a link planted there) is refused.
        assert!(JsonlWriter::create(&index).is_err());
        std::os::unix::fs::symlink(&target, dir.join("nav.jsonl")).unwrap();
        assert!(JsonlWriter::create(&dir.join("nav.jsonl")).is_err());
        assert!(!target.exists());
    }

    #[test]
    fn read_jsonl_skips_bad_lines_and_missing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("navigation.jsonl");
        assert!(read_jsonl::<NavLine>(&path).unwrap().is_empty());
        let created = NavLine {
            ts: "2026-01-01T00:00:00Z".into(),
            target_id: "T1".into(),
            event: NavEvent::Created,
            url: Some("about:blank".into()),
            opener: None,
        };
        let navigated = NavLine {
            ts: "2026-01-01T00:00:01Z".into(),
            target_id: "T1".into(),
            event: NavEvent::Navigated,
            url: Some("https://a.test/".into()),
            opener: Some("T0".into()),
        };
        let mut bytes = Vec::new();
        bytes.extend(serde_json::to_vec(&created).unwrap());
        bytes.extend(b"\n\nnot json\n{\"ts\":1}\n\xff\xfe\n");
        bytes.extend(serde_json::to_vec(&navigated).unwrap());
        // A last line cut off by a crash.
        bytes.extend(b"\n{\"ts\":\"2026-01-01T00:00:02Z\",\"targ");
        std::fs::write(&path, bytes).unwrap();
        let got = read_jsonl::<NavLine>(&path).unwrap();
        assert_eq!(got, vec![created, navigated]);
        let v = serde_json::to_value(&got[1]).unwrap();
        assert_eq!(v["targetId"], "T1");
        assert_eq!(v["event"], "navigated");
        // A directory is not a missing file.
        assert!(read_jsonl::<NavLine>(tmp.path()).is_err());
    }
}
