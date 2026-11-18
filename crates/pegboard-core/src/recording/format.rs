//! The files of a recording besides `manifest.json` and `actions.jsonl` (see
//! `actionlog`): `navigation.jsonl`, `screen/<targetId>/index.jsonl` with its frames, and the
//! optional `network.jsonl` and `console.jsonl`. Directories are owner-only (0700), files
//! too (0600), and a file is never opened twice for writing: the writers create theirs.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
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

/// One line of network.jsonl (`--record network`): a request's metadata. No headers, no
/// bodies; the URL as every logged URL (`url_for_log`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkLine {
    pub ts: String,
    pub target_id: String,
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    pub bytes: Option<u64>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
}

/// One line of console.jsonl (`--record console`): a console call or an uncaught
/// exception of a page, its text cut to 1000 characters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleLine {
    pub ts: String,
    pub target_id: String,
    pub level: String,
    pub text: String,
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
/// After a write that failed (and may have left part of a line), the next line starts with
/// a newline, so the cut one is skipped by readers and does not swallow it.
pub struct JsonlWriter {
    file: File,
    needs_newline: bool,
}

impl JsonlWriter {
    /// Creates the file; an existing file or link is refused.
    pub fn create(path: &Path) -> std::io::Result<JsonlWriter> {
        let file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        Ok(JsonlWriter {
            file,
            needs_newline: false,
        })
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
        if self.needs_newline {
            bytes.insert(0, b'\n');
        }
        match self.file.write_all(&bytes) {
            Ok(()) => {
                self.needs_newline = false;
                Ok(bytes.len() as u64)
            }
            Err(e) => {
                self.needs_newline = true;
                Err(e)
            }
        }
    }
}

/// The lines of a JSON-lines file. A missing file has none; a line that is not one `T`
/// (empty, cut off, not UTF-8, another shape) is skipped.
pub fn read_jsonl<T: DeserializeOwned>(path: &Path) -> std::io::Result<Vec<T>> {
    match File::open(path) {
        Ok(file) => jsonl_lines(file),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// The lines of an open JSON-lines file, as `read_jsonl` reads them. A line longer than
/// `MAX_LINE_BYTES` (no writer writes one) is skipped without being held in memory whole.
pub fn jsonl_lines<T: DeserializeOwned>(file: File) -> std::io::Result<Vec<T>> {
    let mut reader = BufReader::new(file);
    let mut lines = Vec::new();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = (&mut reader)
            .take(MAX_LINE_BYTES as u64 + 1)
            .read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Ok(lines);
        }
        if buf.len() > MAX_LINE_BYTES {
            // No newline within the limit: drop the rest of this line.
            if buf.last() != Some(&b'\n') {
                reader.skip_until(b'\n')?;
            }
            continue;
        }
        if let Ok(value) = serde_json::from_slice(&buf) {
            lines.push(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
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
    fn a_line_after_a_failed_write_starts_on_its_own_line() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("index.jsonl");
        let mut w = JsonlWriter::create(&path).unwrap();
        // What a write that failed half way leaves behind.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"ts\":\"cut")
            .unwrap();
        w.needs_newline = true;
        let line = IndexLine {
            ts: "t".into(),
            file: None,
            url: None,
            width: None,
            height: None,
            kind: FrameKind::Capped,
        };
        let n = w.append(&line).unwrap();
        assert_eq!(n as usize, serde_json::to_vec(&line).unwrap().len() + 2);
        w.append(&line).unwrap();
        assert_eq!(
            read_jsonl::<IndexLine>(&path).unwrap(),
            vec![line.clone(), line]
        );
    }

    #[test]
    fn read_jsonl_skips_lines_over_the_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("navigation.jsonl");
        let line = NavLine {
            ts: "t".into(),
            target_id: "T1".into(),
            event: NavEvent::Closed,
            url: None,
            opener: None,
        };
        let mut bytes = serde_json::to_vec(&line).unwrap();
        bytes.push(b'\n');
        // Valid JSON, but longer than any writer writes.
        let long = NavLine {
            url: Some("x".repeat(3 * MAX_LINE_BYTES)),
            ..line.clone()
        };
        bytes.extend(serde_json::to_vec(&long).unwrap());
        bytes.push(b'\n');
        bytes.extend(serde_json::to_vec(&line).unwrap());
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(
            read_jsonl::<NavLine>(&path).unwrap(),
            vec![line.clone(), line]
        );
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
