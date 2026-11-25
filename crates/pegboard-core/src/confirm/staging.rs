//! The copies an upload that asks a person is shown and sends (design 4.3, TOCTOU).
//!
//! Before the question is asked, each file is opened without following a symbolic link and
//! checked on its descriptor (`upload::open_checked`); then, within the copy budgets
//! (`CopyStore`), it is cloned (APFS `fclonefileat` from that descriptor) or else copied from
//! that descriptor into a private directory: `<confirm root>/<random>/<n>/<name>`
//! (directories 0700, files 0600, created new). The person is shown the real path read from
//! the descriptor and the size copied; what uploads is the copy, so changing or replacing the
//! original after the question changes nothing. Each file has a directory of its own, so two
//! files of the same name keep their names; the same file given twice is copied once.
//!
//! The browser reads an input's files when the page uses them (a form submitted later), so
//! the copies of an upload that ran are kept (`CopyStore::keep`) until the session closes, or
//! until newer copies of the session need their room. The copies of a question that was not
//! approved, or of an upload that failed, go when the `Staged` is dropped. Whatever a daemon
//! left behind goes when the next one starts (`remove_leftovers`).

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use pegboard_protocol::SessionId;
use rand::RngCore;

use crate::upload::{open_checked, Allowed, Opened, MAX_UPLOAD_FILES, MAX_UPLOAD_FILE_BYTES};

/// The most bytes of copies one session holds (kept, and being asked about).
pub const SESSION_COPY_BYTES: u64 = 4 << 30;
/// The most bytes of copies the daemon holds for all sessions.
pub const DAEMON_COPY_BYTES: u64 = 8 << 30;
/// Free space a copy must leave on the disk of pegboard's home.
pub const FREE_SPACE_MARGIN: u64 = 1 << 30;

/// One file to copy: what the agent gave (the only text a refusal quotes), where it was
/// found, and where it may be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub given: String,
    pub path: PathBuf,
    pub allowed: Allowed,
}

/// One copied file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedFile {
    /// What the agent gave (a bare name, or its absolute path).
    pub given: String,
    /// The real path of the file copied, read from its descriptor: what the person sees.
    pub real: PathBuf,
    /// The copy, which uploads.
    pub staged: PathBuf,
    /// The file's name (the copy's, the page's).
    pub name: String,
    pub size: u64,
}

/// The copies of one command. Dropping it deletes them and gives their bytes back to the
/// budgets.
#[derive(Debug)]
pub struct Staged {
    pub dir: PathBuf,
    pub files: Vec<StagedFile>,
    account: Option<Account>,
}

impl Staged {
    /// The copies' real paths, each once.
    pub fn copies(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for f in &self.files {
            if !out.contains(&f.staged) {
                out.push(f.staged.clone());
            }
        }
        out
    }

    /// The bytes the copies take (each copy once).
    pub fn bytes(&self) -> u64 {
        self.account.as_ref().map_or(0, |a| a.bytes)
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Bytes of copies counted against the budgets while it lives.
#[derive(Debug)]
struct Account {
    store: Weak<CopyStore>,
    session: SessionId,
    bytes: u64,
}

impl Drop for Account {
    fn drop(&mut self) {
        if let Some(store) = self.store.upgrade() {
            let mut t = store.inner.lock().unwrap();
            if let Some(b) = t.bytes.get_mut(&self.session) {
                *b = b.saturating_sub(self.bytes);
            }
            t.total = t.total.saturating_sub(self.bytes);
        }
    }
}

#[derive(Default)]
struct StoreInner {
    /// Bytes per session (kept, and being asked about).
    bytes: HashMap<SessionId, u64>,
    total: u64,
    /// Copies of uploads that ran, oldest first.
    kept: HashMap<SessionId, VecDeque<Staged>>,
}

/// The copies the daemon holds, and their budgets (`SESSION_COPY_BYTES`,
/// `DAEMON_COPY_BYTES`).
pub struct CopyStore {
    inner: Mutex<StoreInner>,
    me: Weak<CopyStore>,
}

impl std::fmt::Debug for CopyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CopyStore")
    }
}

impl CopyStore {
    pub fn new() -> Arc<CopyStore> {
        Arc::new_cyclic(|me| CopyStore {
            inner: Mutex::new(StoreInner::default()),
            me: me.clone(),
        })
    }

    /// Counts `bytes` more of the session's copies, when they fit once the session's kept
    /// copies made way. Nothing is evicted here: a question may still be denied, and the
    /// kept copies may still be read by a page. Only an approved upload makes the room
    /// (`make_room`).
    fn admit(&self, session: &SessionId, bytes: u64) -> Result<Account, String> {
        let mut t = self.inner.lock().unwrap();
        let mine = t.bytes.get(session).copied().unwrap_or(0);
        let kept: u64 = t
            .kept
            .get(session)
            .map_or(0, |k| k.iter().map(Staged::bytes).sum());
        let fits = (mine.saturating_sub(kept) + bytes) <= SESSION_COPY_BYTES
            && (t.total.saturating_sub(kept) + bytes) <= DAEMON_COPY_BYTES;
        if !fits {
            return Err(
                "pegboard holds as many copies for uploads as it may; close a session (or                  wait for its copies to go) and try again"
                    .to_string(),
            );
        }
        *t.bytes.entry(session.clone()).or_insert(0) += bytes;
        t.total += bytes;
        Ok(Account {
            store: self.me.clone(),
            session: session.clone(),
            bytes,
        })
    }

    /// A person approved an upload of the session: its oldest kept copies go until both
    /// budgets hold again (`admit` counted on them making way).
    pub fn make_room(&self, session: &SessionId) {
        let mut evicted = Vec::new();
        {
            let mut t = self.inner.lock().unwrap();
            loop {
                let mine = t.bytes.get(session).copied().unwrap_or(0);
                if mine <= SESSION_COPY_BYTES && t.total <= DAEMON_COPY_BYTES {
                    break;
                }
                // Dropped below, out of the lock (a copy gives its bytes back when dropped).
                let Some(old) = t.kept.get_mut(session).and_then(VecDeque::pop_front) else {
                    break;
                };
                let n = old.bytes();
                if let Some(b) = t.bytes.get_mut(session) {
                    *b = b.saturating_sub(n);
                }
                t.total = t.total.saturating_sub(n);
                evicted.push(old);
            }
        }
        // Their bytes were taken off above: dropping them must not take them off again.
        for mut old in evicted {
            if let Some(a) = old.account.as_mut() {
                a.bytes = 0;
            }
        }
    }

    /// Keeps the copies of an upload that ran until the session closes (the page may read
    /// them later), or until newer copies of the session need their room.
    pub fn keep(&self, session: &SessionId, staged: Staged) {
        self.inner
            .lock()
            .unwrap()
            .kept
            .entry(session.clone())
            .or_default()
            .push_back(staged);
    }

    /// The session closed: its kept copies go.
    pub fn forget(&self, session: &SessionId) {
        let gone = self.inner.lock().unwrap().kept.remove(session);
        drop(gone);
    }

    /// Bytes counted for the session (tests, diagnostics).
    pub fn bytes_of(&self, session: &SessionId) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .bytes
            .get(session)
            .copied()
            .unwrap_or(0)
    }
}

/// Why nothing was staged.
#[derive(Debug)]
pub enum StageError {
    /// A file pegboard does not take: `given` is the agent's own text, `why` the reason, for
    /// the person's side only.
    Refused { given: String, why: String },
    /// No room for the copies (the budgets or the disk).
    NoRoom(String),
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

/// Free bytes for an unprivileged user on the disk of `path`.
fn free_bytes(path: &Path) -> std::io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    // SAFETY: a NUL-terminated path and a zeroed out-parameter of the right type.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    #[allow(clippy::unnecessary_cast)]
    Ok((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

/// `sys/clonefile.h`: do not follow a link, do not copy the owner.
#[cfg(target_os = "macos")]
const CLONE_NOFOLLOW: u32 = 0x0001;
#[cfg(target_os = "macos")]
const CLONE_NOOWNERCOPY: u32 = 0x0002;

/// Clones the open file into `dir/name` (APFS: no bytes are written until one side
/// changes). `false` when the disk cannot (another file system), and nothing was made.
#[cfg(target_os = "macos")]
fn clone_into(opened: &Opened, dir: &Path, name: &str) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;
    let dir_fd = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir)?;
    let cname = std::ffi::CString::new(name)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    // SAFETY: two open descriptors and a NUL-terminated name.
    let r = unsafe {
        libc::fclonefileat(
            opened.file.as_raw_fd(),
            dir_fd.as_raw_fd(),
            cname.as_ptr(),
            CLONE_NOFOLLOW | CLONE_NOOWNERCOPY,
        )
    };
    Ok(r == 0)
}

#[cfg(not(target_os = "macos"))]
fn clone_into(_: &Opened, _: &Path, _: &str) -> std::io::Result<bool> {
    Ok(false)
}

/// Makes the copy of `opened` at `dir/name`: a clone, else a byte copy from the descriptor.
/// Returns its size (0600, the one link).
fn copy_into(opened: &Opened, dir: &Path, name: &str) -> std::io::Result<u64> {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    if clone_into(opened, dir, name)? {
        // The clone has the original's mode; the directory (0700) is private meanwhile.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        return Ok(std::fs::symlink_metadata(&path)?.len());
    }
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)?;
    let copied = std::io::copy(
        &mut (&opened.file).take(MAX_UPLOAD_FILE_BYTES + 1),
        &mut out,
    )?;
    out.flush()?;
    Ok(copied)
}

/// Copies each of `sources` (in order) into a new directory under `root` (a private
/// directory: `paths::ensure_session_confirm`), each checked where its `Allowed` says, within
/// the session's room in `store` and the disk's free space. One file refused or failing
/// stages nothing (what was copied is deleted).
pub fn stage(
    sources: &[Source],
    root: &Path,
    store: &CopyStore,
    session: &SessionId,
) -> Result<Staged, StageError> {
    if sources.is_empty() || sources.len() > MAX_UPLOAD_FILES {
        return Err(StageError::Refused {
            given: String::new(),
            why: format!("an upload that asks a person takes 1 to {MAX_UPLOAD_FILES} files"),
        });
    }
    // Every file is opened and checked first; the same file (device and inode) twice is
    // copied once.
    let mut opened: Vec<(Opened, String)> = Vec::new();
    let mut which = Vec::new();
    for src in sources {
        let o = open_checked(&src.path, &src.allowed).map_err(|why| StageError::Refused {
            given: src.given.clone(),
            why,
        })?;
        let meta = o.file.metadata().map_err(StageError::Io)?;
        let id = (meta.dev(), meta.ino());
        let found = opened
            .iter()
            .position(|(p, _)| p.file.metadata().is_ok_and(|m| (m.dev(), m.ino()) == id));
        match found {
            Some(i) => which.push(i),
            None => {
                which.push(opened.len());
                opened.push((o, src.given.clone()));
            }
        }
    }
    let bytes: u64 = opened.iter().map(|(o, _)| o.size).sum();
    let root = root.canonicalize().map_err(StageError::Io)?;
    // The disk first: a refusal there leaves the budgets untouched.
    let free = free_bytes(&root).map_err(StageError::Io)?;
    if free < bytes.saturating_add(FREE_SPACE_MARGIN) {
        return Err(StageError::NoRoom(
            "the disk of pegboard's data has too little free space for a copy of these files"
                .to_string(),
        ));
    }
    let account = store.admit(session, bytes).map_err(StageError::NoRoom)?;
    let dir = root.join(random_name());
    private_dir(&dir).map_err(StageError::Io)?;
    // From here on a failure deletes what was made.
    let mut staged = Staged {
        dir,
        files: Vec::new(),
        account: Some(account),
    };
    let mut copies = Vec::new();
    for (i, (o, given)) in opened.iter().enumerate() {
        let name = o
            .real
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty() && *n != "." && *n != "..")
            .ok_or_else(|| StageError::Refused {
                given: given.clone(),
                why: "it has no file name".into(),
            })?
            .to_string();
        let sub = staged.dir.join(i.to_string());
        private_dir(&sub).map_err(StageError::Io)?;
        // Exactly the size it had when opened (a file still being written is refused).
        let size = copy_into(o, &sub, &name).map_err(StageError::Io)?;
        if size != o.size {
            return Err(StageError::Refused {
                given: given.clone(),
                why: "it changed while it was copied".into(),
            });
        }
        copies.push((sub.join(&name), name));
    }
    for (src, i) in sources.iter().zip(which) {
        let (o, _) = &opened[i];
        let (path, name) = &copies[i];
        staged.files.push(StagedFile {
            given: src.given.clone(),
            real: o.real.clone(),
            staged: path.clone(),
            name: name.clone(),
            size: o.size,
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

    fn session() -> SessionId {
        SessionId("s_test".into())
    }

    impl Rig {
        fn sources(&self, paths: &[PathBuf]) -> Vec<Source> {
            paths
                .iter()
                .map(|p| Source {
                    given: p.display().to_string(),
                    path: p.clone(),
                    allowed: Allowed::Outside(self.forbidden.clone()),
                })
                .collect()
        }

        fn stage(&self, paths: &[PathBuf]) -> Result<Staged, StageError> {
            stage(
                &self.sources(paths),
                &self.root,
                &CopyStore::new(),
                &session(),
            )
        }
    }

    /// The reason, prefixed with the given text.
    fn refused(r: Result<Staged, StageError>) -> String {
        match r {
            Err(StageError::Refused { given, why }) => format!("{given}: {why}"),
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
        // The proxy's window takes exactly the copies.
        assert_eq!(staged.copies(), vec![f.staged.clone()]);
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
        assert_eq!(staged.copies().len(), 2);
    }

    #[test]
    fn the_same_file_twice_is_copied_once() {
        let r = rig();
        std::fs::write(r.src.join("a"), "aaa").unwrap();
        let staged = r.stage(&[r.src.join("a"), r.src.join("a")]).unwrap();
        assert_eq!(staged.files.len(), 2);
        assert_eq!(staged.files[0].staged, staged.files[1].staged);
        assert_eq!(staged.copies().len(), 1);
        assert_eq!(staged.bytes(), 3);
    }

    #[test]
    fn copies_count_against_the_budgets_and_the_oldest_kept_make_way() {
        let r = rig();
        let store = CopyStore::new();
        let s1 = session();
        std::fs::write(r.src.join("a"), "12345").unwrap();
        let a = stage(&r.sources(&[r.src.join("a")]), &r.root, &store, &s1).unwrap();
        assert_eq!(store.bytes_of(&s1), 5);
        let dir = a.dir.clone();
        // Kept: still counted, still there.
        store.keep(&s1, a);
        assert_eq!(store.bytes_of(&s1), 5);
        assert!(dir.is_dir());
        // A denied one goes, and gives its bytes back.
        let b = stage(&r.sources(&[r.src.join("a")]), &r.root, &store, &s1).unwrap();
        assert_eq!(store.bytes_of(&s1), 10);
        drop(b);
        assert_eq!(store.bytes_of(&s1), 5);
        // Room for more is made from the session's oldest kept copies, but only once a
        // person approved: asking alone (which may be denied) evicts nothing.
        let account = store.admit(&s1, SESSION_COPY_BYTES - 2).unwrap();
        assert!(
            dir.is_dir(),
            "a kept copy went before the upload was approved"
        );
        store.make_room(&s1);
        assert!(!dir.exists(), "the oldest kept copy made way");
        assert_eq!(store.bytes_of(&s1), SESSION_COPY_BYTES - 2);
        // Nothing left to make way: refused.
        assert!(store.admit(&s1, 3).is_err());
        // Another session: the daemon's budget holds across sessions.
        let (s2, s3) = (SessionId("s_2".into()), SessionId("s_3".into()));
        assert!(
            store.admit(&s2, SESSION_COPY_BYTES + 1).is_err(),
            "per session"
        );
        let held = store.admit(&s2, SESSION_COPY_BYTES).unwrap();
        // Now DAEMON_COPY_BYTES - 2 in all.
        assert!(store.admit(&s3, 3).is_err(), "daemon-wide");
        let last = store.admit(&s3, 2).unwrap();
        drop((account, held, last));
        assert_eq!(
            (
                store.bytes_of(&s1),
                store.bytes_of(&s2),
                store.bytes_of(&s3)
            ),
            (0, 0, 0)
        );
        // A session's close drops what it kept.
        let c = stage(&r.sources(&[r.src.join("a")]), &r.root, &store, &s1).unwrap();
        let dir = c.dir.clone();
        store.keep(&s1, c);
        store.forget(&s1);
        assert!(!dir.exists());
        assert_eq!(store.bytes_of(&s1), 0);
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
