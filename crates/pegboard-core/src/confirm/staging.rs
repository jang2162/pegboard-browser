//! The copies an upload that asks a person is shown and sends (design 4.3, TOCTOU).
//!
//! Before the question is asked, each file is opened without following a symbolic link,
//! checked on its descriptor (`upload::open_checked`) and copied from that descriptor into a
//! private directory: `<confirm root>/<random>/<n>/<name>` (directories 0700, files 0600,
//! created new). The person is shown the real path read from the descriptor and the size
//! copied; what uploads is the copy, so changing or replacing the original after the question
//! changes nothing. Each file has a directory of its own, so two files of the same name keep
//! their names, and the proxy can be opened to exactly these directories while the approved
//! command runs. The copies go when the `Staged` is dropped (the command ended, however), when
//! the session closes, and when the daemon starts (`remove_leftovers`).

use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use rand::RngCore;

use crate::upload::{open_checked, Allowed, MAX_UPLOAD_FILES, MAX_UPLOAD_FILE_BYTES};

/// One copied file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedFile {
    /// The path given (a bare name's resolved path, or the agent's absolute path).
    pub given: PathBuf,
    /// The real path of the file copied, read from its descriptor: what the person sees.
    pub real: PathBuf,
    /// The copy, which uploads.
    pub staged: PathBuf,
    /// The file's name (the copy's, the page's).
    pub name: String,
    pub size: u64,
}

/// The copies of one command. Dropping it deletes them.
#[derive(Debug)]
pub struct Staged {
    pub dir: PathBuf,
    pub files: Vec<StagedFile>,
}

impl Staged {
    /// The directory of each copy: what the proxy may take files from while the approved
    /// command runs.
    pub fn dirs(&self) -> Vec<PathBuf> {
        self.files
            .iter()
            .filter_map(|f| f.staged.parent().map(Path::to_path_buf))
            .collect()
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Why nothing was staged.
#[derive(Debug)]
pub enum StageError {
    /// A file pegboard does not take (the reason names the given path, nothing else).
    Refused(String),
    /// The copy could not be made.
    Io(std::io::Error),
}

fn private_dir(dir: &Path) -> std::io::Result<()> {
    // Created new: a directory that already exists (planted, or a clash) is not used.
    std::fs::DirBuilder::new().mode(0o700).create(dir)
}

fn random_name() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    data_encoding::HEXLOWER.encode(&bytes)
}

/// Copies each of `sources` (in order) into a new directory under `root` (a private
/// directory: `paths::ensure_session_confirm`), each checked where its `Allowed` says. One
/// file refused or failing stages nothing (what was copied is deleted).
pub fn stage(sources: &[(PathBuf, Allowed)], root: &Path) -> Result<Staged, StageError> {
    if sources.is_empty() || sources.len() > MAX_UPLOAD_FILES {
        return Err(StageError::Refused(format!(
            "an upload that asks a person takes 1 to {MAX_UPLOAD_FILES} files"
        )));
    }
    let root = root.canonicalize().map_err(StageError::Io)?;
    let dir = root.join(random_name());
    private_dir(&dir).map_err(StageError::Io)?;
    // From here on a failure deletes what was made.
    let mut staged = Staged {
        dir,
        files: Vec::new(),
    };
    for (i, (given, allowed)) in sources.iter().enumerate() {
        let opened = open_checked(given, allowed)
            .map_err(|why| StageError::Refused(format!("{}: {why}", given.display())))?;
        let name = opened
            .real
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty() && *n != "." && *n != "..")
            .ok_or_else(|| {
                StageError::Refused(format!("{}: it has no file name", given.display()))
            })?
            .to_string();
        let sub = staged.dir.join(i.to_string());
        private_dir(&sub).map_err(StageError::Io)?;
        let path = sub.join(&name);
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .map_err(StageError::Io)?;
        // From the descriptor that was checked; never more than the limit, and exactly the
        // size it had when opened (a file still being written is refused).
        let copied = std::io::copy(
            &mut (&opened.file).take(MAX_UPLOAD_FILE_BYTES + 1),
            &mut out,
        )
        .map_err(StageError::Io)?;
        out.flush().map_err(StageError::Io)?;
        if copied != opened.size {
            return Err(StageError::Refused(format!(
                "{}: it changed while it was copied",
                given.display()
            )));
        }
        staged.files.push(StagedFile {
            given: given.clone(),
            real: opened.real,
            staged: path,
            name,
            size: copied,
        });
    }
    Ok(staged)
}

/// Deletes the confirmation copies a daemon left behind (it stopped while a command
/// waited): each `<sessions>/<id>/confirm`. A link there is removed, not followed.
pub fn remove_leftovers(sessions: &Path) {
    let Ok(entries) = std::fs::read_dir(sessions) else {
        return;
    };
    for entry in entries.flatten() {
        let confirm = entry.path().join("confirm");
        match std::fs::symlink_metadata(&confirm) {
            Ok(m) if m.file_type().is_dir() => {
                let _ = std::fs::remove_dir_all(&confirm);
            }
            Ok(_) => {
                let _ = std::fs::remove_file(&confirm);
            }
            Err(_) => continue,
        }
        // The session's directory, when nothing else is left in it.
        let _ = std::fs::remove_dir(entry.path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

    struct Rig {
        _tmp: tempfile::TempDir,
        src: PathBuf,
        root: PathBuf,
        forbidden: Vec<PathBuf>,
    }

    fn rig() -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let (src, root, home) = (base.join("src"), base.join("confirm"), base.join("home"));
        std::fs::create_dir(&src).unwrap();
        std::fs::create_dir(&home).unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        Rig {
            _tmp: tmp,
            src,
            root,
            forbidden: vec![home],
        }
    }

    impl Rig {
        fn stage(&self, sources: &[PathBuf]) -> Result<Staged, StageError> {
            let sources: Vec<(PathBuf, Allowed)> = sources
                .iter()
                .map(|p| (p.clone(), Allowed::Outside(self.forbidden.clone())))
                .collect();
            stage(&sources, &self.root)
        }
    }

    fn refused(r: Result<Staged, StageError>) -> String {
        match r {
            Err(StageError::Refused(why)) => why,
            other => panic!("not refused: {other:?}"),
        }
    }

    #[test]
    fn staging_copies_the_opened_file() {
        let r = rig();
        let a = r.src.join("a.txt");
        std::fs::write(&a, "first").unwrap();
        let staged = r.stage(std::slice::from_ref(&a)).unwrap();
        let f = &staged.files[0];
        assert_eq!(
            (f.real.as_path(), f.name.as_str(), f.size),
            (a.as_path(), "a.txt", 5)
        );
        assert_eq!(f.staged.file_name().unwrap(), "a.txt");
        // The original changes, then becomes a link to something else: the copy does not.
        std::fs::write(&a, "second, longer").unwrap();
        assert_eq!(std::fs::read_to_string(&f.staged).unwrap(), "first");
        std::fs::write(r.src.join("other"), "other").unwrap();
        std::fs::remove_file(&a).unwrap();
        symlink(r.src.join("other"), &a).unwrap();
        assert_eq!(std::fs::read_to_string(&f.staged).unwrap(), "first");
        // Private: 0700 directories, 0600 files.
        let mode = |p: &Path| std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&staged.dir), 0o700);
        assert_eq!(mode(f.staged.parent().unwrap()), 0o700);
        assert_eq!(mode(&f.staged), 0o600);
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        assert_eq!(std::fs::metadata(&f.staged).unwrap().uid(), uid);
        // The proxy opens each copy's own directory.
        assert_eq!(
            staged.dirs(),
            vec![f.staged.parent().unwrap().to_path_buf()]
        );
    }

    #[test]
    fn staging_refuses_a_file_swapped_for_a_link_before_open() {
        let r = rig();
        std::fs::write(r.src.join("real"), "real").unwrap();
        let a = r.src.join("a.txt");
        // Checked as a file earlier (`check_absolute`), a link by the time it is opened.
        symlink(r.src.join("real"), &a).unwrap();
        let why = refused(r.stage(std::slice::from_ref(&a)));
        assert!(why.contains("symbolic link"), "{why}");
        // A directory on the way swapped for a link into a forbidden place: the path read
        // from the descriptor gives it away.
        let home = &r.forbidden[0];
        std::fs::write(home.join("secret"), "s").unwrap();
        symlink(home, r.src.join("dir")).unwrap();
        let why = refused(r.stage(&[r.src.join("dir/secret")]));
        assert!(why.contains("pegboard's data directory"), "{why}");
        // The refusal names the given path only.
        assert!(
            why.starts_with(&r.src.join("dir/secret").display().to_string()),
            "{why}"
        );
        // Nothing is left of a refused staging.
        assert_eq!(std::fs::read_dir(&r.root).unwrap().count(), 0);
    }

    #[test]
    fn one_bad_file_stages_nothing() {
        let r = rig();
        std::fs::write(r.src.join("a"), "a").unwrap();
        let fifo = r.src.join("fifo");
        let c = std::ffi::CString::new(fifo.display().to_string()).unwrap();
        // SAFETY: a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let why = refused(r.stage(&[r.src.join("a"), fifo]));
        assert!(why.contains("regular file"), "{why}");
        assert_eq!(std::fs::read_dir(&r.root).unwrap().count(), 0);
    }

    #[test]
    fn two_files_of_one_name_keep_it() {
        let r = rig();
        std::fs::create_dir(r.src.join("x")).unwrap();
        std::fs::create_dir(r.src.join("y")).unwrap();
        std::fs::write(r.src.join("x/a.txt"), "x").unwrap();
        std::fs::write(r.src.join("y/a.txt"), "yy").unwrap();
        let staged = r
            .stage(&[r.src.join("x/a.txt"), r.src.join("y/a.txt")])
            .unwrap();
        let names: Vec<&str> = staged.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["a.txt", "a.txt"]);
        assert_ne!(staged.files[0].staged, staged.files[1].staged);
        assert_eq!(
            std::fs::read_to_string(&staged.files[1].staged).unwrap(),
            "yy"
        );
        assert_eq!(staged.dirs().len(), 2);
    }

    #[test]
    fn seventeen_files_are_refused() {
        let r = rig();
        let files: Vec<PathBuf> = (0..=MAX_UPLOAD_FILES)
            .map(|i| {
                let p = r.src.join(format!("f{i}"));
                std::fs::write(&p, "x").unwrap();
                p
            })
            .collect();
        let why = refused(r.stage(&files));
        assert!(why.contains("1 to 16 files"), "{why}");
        assert!(r.stage(&files[..MAX_UPLOAD_FILES]).is_ok());
    }

    #[test]
    fn staged_dir_goes_when_dropped() {
        let r = rig();
        std::fs::write(r.src.join("a"), "a").unwrap();
        let staged = r.stage(&[r.src.join("a")]).unwrap();
        let dir = staged.dir.clone();
        assert!(dir.is_dir());
        drop(staged);
        assert!(!dir.exists());
    }

    #[test]
    fn leftover_staging_is_removed_at_start() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let left = sessions.join("s_1/confirm/abc/0");
        std::fs::create_dir_all(&left).unwrap();
        std::fs::write(left.join("a.txt"), "a").unwrap();
        // Another session's files stay; so does its directory.
        std::fs::create_dir_all(sessions.join("s_2/files")).unwrap();
        std::fs::write(sessions.join("s_2/files/keep"), "k").unwrap();
        // A link named `confirm` is removed, its target is not touched.
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), "k").unwrap();
        std::fs::create_dir_all(sessions.join("s_3")).unwrap();
        symlink(&outside, sessions.join("s_3/confirm")).unwrap();
        remove_leftovers(&sessions);
        assert!(!sessions.join("s_1").exists());
        assert!(sessions.join("s_2/files/keep").exists());
        assert!(outside.join("keep").exists());
        assert!(!sessions.join("s_3/confirm").exists());
        // No sessions directory at all: nothing happens.
        remove_leftovers(&tmp.path().join("none"));
    }
}
