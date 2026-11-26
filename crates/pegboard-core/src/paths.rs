//! Filesystem layout for the pegboard home directory.

use std::ffi::{OsStr, OsString};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use pegboard_protocol::SessionId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub home: PathBuf,
    /// Chrome user data directory (never the user's everyday profile).
    pub chrome_udd: PathBuf,
    pub recordings: PathBuf,
    /// Core API unix socket.
    pub socket: PathBuf,
    /// Persisted daemon state file.
    pub state: PathBuf,
    /// agent-browser socket directory; kept short because of the unix socket path limit.
    pub ab_socket_dir: PathBuf,
    /// HOME of the agent-browser processes, so they never share the user's `~/.agent-browser`.
    pub ab_home: PathBuf,
    /// The user's settings (`recording::config`); optional.
    pub config: PathBuf,
}

impl Paths {
    /// Directory of the files a session's commands write (screenshots, PDFs); also the
    /// working directory of its agent-browser processes. See `ensure_session_files`.
    pub fn session_files(&self, id: &SessionId) -> PathBuf {
        self.home.join("sessions").join(&id.0).join("files")
    }

    /// Directory of the copies an upload that asks a person is shown and sends
    /// (`<home>/sessions/<id>/confirm`, design 4.3). See `ensure_session_confirm`.
    pub fn session_confirm_dir(&self, id: &SessionId) -> PathBuf {
        self.home.join("sessions").join(&id.0).join("confirm")
    }

    /// Where the sessions' directories are (`<home>/sessions`).
    pub fn sessions_dir(&self) -> PathBuf {
        self.home.join("sessions")
    }

    /// Directories an upload's absolute path may not point into (design 4.3, security review):
    /// - pegboard's home: every session's files, downloads, recordings and confirmation
    ///   copies, the settings, the state, the stock browser's user data;
    /// - the default home (`~/Library/Application Support/Pegboard Agent`) and its
    ///   half-moved form (`Pegboard Agent.moving`, see `migrate_legacy_home`) whatever
    ///   `PEGBOARD_HOME` says;
    /// - the fork's own user data directory (`~/Library/Application Support/Pegboard`, the
    ///   legacy home);
    /// - the browsers' HTTP caches: for a directory under `~/Library/Application Support`,
    ///   its counterpart under `~/Library/Caches` (Chrome keeps a profile's cache there);
    /// - agent-browser's directory in `/tmp` (its sockets, logs and settings name the
    ///   sessions' endpoints);
    /// - the fork's saved window state (`~/Library/Saved Application State/
    ///   app.pegboard.browser.savedState`).
    pub fn upload_forbidden(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.home.clone(), self.chrome_udd.clone()];
        if let Some(parent) = self.ab_socket_dir.parent() {
            dirs.push(parent.to_path_buf());
        }
        dirs.push(self.ab_socket_dir.clone());
        if let Some(user) = std::env::var_os("HOME").map(PathBuf::from) {
            let support = user.join(SUPPORT_DIR);
            let caches = user.join("Library/Caches");
            dirs.push(support.join(LEGACY_HOME_DIR_NAME));
            dirs.push(support.join(HOME_DIR_NAME));
            // The default home's stock browser data, for its cache counterpart.
            dirs.push(support.join(HOME_DIR_NAME).join("chrome"));
            dirs.push(support.join(MOVING_DIR_NAME));
            dirs.push(user.join("Library/Saved Application State/app.pegboard.browser.savedState"));
            let cached: Vec<PathBuf> = dirs
                .iter()
                .filter_map(|d| d.strip_prefix(&support).ok())
                .map(|rel| caches.join(rel))
                .collect();
            dirs.extend(cached);
        }
        dirs
    }
}

/// `~/<this>` holds the default home and the fork's user data directory.
const SUPPORT_DIR: &str = "Library/Application Support";

/// The default home: `~/Library/Application Support/<this>` (design 4.8, R17).
pub const HOME_DIR_NAME: &str = "Pegboard Agent";
/// The fork's user data directory, and pegboard's home before 3c (the legacy home).
pub const LEGACY_HOME_DIR_NAME: &str = "Pegboard";
/// The new home while `migrate_legacy_home` fills it; renamed to `HOME_DIR_NAME` at the end.
pub const MOVING_DIR_NAME: &str = "Pegboard Agent.moving";
/// Left in the legacy home once its entries have moved: where they went.
pub const MOVED_NOTE: &str = "pegboard-home-moved.txt";
/// Names pegboard owned in the legacy home, moved one by one (design 4.8). Every other
/// name there is the browser's (`Default/`, `Local State`, `PegboardAgent*/`, ...).
pub const LEGACY_ENTRIES: &[&str] = &[
    "sessions",
    "recordings",
    "chrome",
    "agent-browser-home",
    "state.json",
    "config.toml",
    "daemon.log",
];
/// Any of these in the legacy home means pegboard used it (`daemon.log` alone or `chrome/`
/// alone does not: a hosted daemon that never got far, or nothing worth moving).
const LEGACY_MARKERS: &[&str] = &["state.json", "config.toml", "recordings", "sessions"];
/// The legacy daemon's socket; removed, not moved (so is its `daemon.lock`).
const LEGACY_SOCKET: &str = "pegboardd.sock";

/// `home` is `$PEGBOARD_HOME` or `~/Library/Application Support/Pegboard Agent`.
pub fn paths() -> Paths {
    paths_in(&home_for(
        std::env::var_os("PEGBOARD_HOME"),
        std::env::var_os("HOME"),
    ))
}

/// The home for these values of `PEGBOARD_HOME` and `HOME` (see `paths`).
pub fn home_for(pegboard_home: Option<OsString>, user_home: Option<OsString>) -> PathBuf {
    match pegboard_home {
        Some(h) => PathBuf::from(h),
        None => PathBuf::from(user_home.unwrap_or_default())
            .join(SUPPORT_DIR)
            .join(HOME_DIR_NAME),
    }
}

/// The directory `migrate_legacy_home` works in (`~/Library/Application Support`), or
/// `None` when nothing is to be moved: `PEGBOARD_HOME` is set (it always wins, and the
/// legacy home is not its business) or `HOME` is unknown.
pub fn migration_support_dir(
    pegboard_home: Option<&OsStr>,
    user_home: Option<&OsStr>,
) -> Option<PathBuf> {
    if pegboard_home.is_some() {
        return None;
    }
    let user = user_home.filter(|h| !h.is_empty())?;
    Some(PathBuf::from(user).join(SUPPORT_DIR))
}

/// What `migrate_legacy_home` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Migration {
    /// The new home exists, or the legacy home holds nothing of pegboard's.
    NotNeeded,
    /// The new home is in place; `entries` are the names this call moved.
    Moved { entries: Vec<String> },
    /// A pegboard daemon of the legacy home holds its lock: nothing was changed.
    LegacyDaemonRunning,
}

/// How long `migrate_legacy_home` waits for the legacy lock: another new pegboard process
/// may be moving the home (a move is a few renames).
const LEGACY_LOCK_TRIES: u32 = 10;
const LEGACY_LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(20);

/// Moves pegboard's entries from the legacy home (`<support>/Pegboard`, which is also the
/// fork's user data directory) to the new one (`<support>/Pegboard Agent`), once (design
/// 4.8). Callers run it before anything creates the home, and only when `PEGBOARD_HOME` is
/// unset (see `migration_support_dir`).
///
/// - Nothing to do when the new home exists, or when the legacy home has none of
///   `LEGACY_MARKERS` and no move was left half done.
/// - The legacy daemon's lock (`<legacy>/daemon.lock`) is taken first and held throughout;
///   while another process holds it nothing changes (`LegacyDaemonRunning`).
/// - The entries (`LEGACY_ENTRIES`, nothing else) are renamed one at a time into
///   `<support>/Pegboard Agent.moving` (0700, a real directory of this user), which is
///   renamed to the new home at the end. A process that dies half way leaves the
///   `.moving` directory, and the next call carries on from it.
/// - Nothing is overwritten or deleted: an entry whose name is already taken in `.moving`
///   stops the move with an error, both left as they are. Only the legacy daemon's socket
///   and lock file are removed, and a note (`MOVED_NOTE`) is left in the legacy home.
/// - The legacy home must be a real directory of this user; anything else is refused.
pub fn migrate_legacy_home(support: &Path) -> std::io::Result<Migration> {
    use std::io::ErrorKind;

    let legacy = support.join(LEGACY_HOME_DIR_NAME);
    let home = support.join(HOME_DIR_NAME);
    let moving = support.join(MOVING_DIR_NAME);
    let needed = |legacy: &Path| -> std::io::Result<bool> {
        if exists_no_follow(&home)? {
            return Ok(false);
        }
        if exists_no_follow(&moving)? {
            return Ok(true);
        }
        for name in LEGACY_MARKERS {
            if exists_no_follow(&legacy.join(name))? {
                return Ok(true);
            }
        }
        Ok(false)
    };
    match std::fs::symlink_metadata(&legacy) {
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Ok(if exists_no_follow(&moving)? && !exists_no_follow(&home)? {
                // The legacy home went away after a move began; finish what was moved.
                finish_move(&legacy, &moving, &home, Vec::new())?
            } else {
                Migration::NotNeeded
            });
        }
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    if !needed(&legacy)? {
        return Ok(Migration::NotNeeded);
    }
    require_own_dir(&legacy)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(legacy.join("daemon.lock"))?;
    let mut tries = 0;
    loop {
        // SAFETY: the descriptor is open for the whole call.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break;
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != ErrorKind::WouldBlock {
            return Err(e);
        }
        tries += 1;
        if tries >= LEGACY_LOCK_TRIES {
            // Another new pegboard process may have just finished the move.
            return Ok(if exists_no_follow(&home)? {
                Migration::NotNeeded
            } else {
                Migration::LegacyDaemonRunning
            });
        }
        std::thread::sleep(LEGACY_LOCK_WAIT);
    }
    // Ours now: look again, another process may have moved the home before we got it.
    if !needed(&legacy)? {
        return Ok(Migration::NotNeeded);
    }
    ensure_owned_private_dir(&moving)?;
    let mut moved = Vec::new();
    for name in LEGACY_ENTRIES {
        let from = legacy.join(name);
        if !exists_no_follow(&from)? {
            continue;
        }
        rename_no_replace(&from, &moving.join(name)).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!(
                    "moving {} to {}: {e}; nothing was deleted",
                    from.display(),
                    moving.display()
                ),
            )
        })?;
        moved.push((*name).to_string());
    }
    let result = finish_move(&legacy, &moving, &home, moved)?;
    // Held until here: the lock file goes last, with the move done.
    let _ = std::fs::remove_file(legacy.join("daemon.lock"));
    drop(lock);
    Ok(result)
}

/// The end of a move: the legacy daemon's socket goes, the note is left, and `.moving`
/// becomes the new home.
fn finish_move(
    legacy: &Path,
    moving: &Path,
    home: &Path,
    moved: Vec<String>,
) -> std::io::Result<Migration> {
    use std::io::ErrorKind;
    ensure_owned_private_dir(moving)?;
    if exists_no_follow(legacy)? {
        let socket = legacy.join(LEGACY_SOCKET);
        match std::fs::symlink_metadata(&socket) {
            Ok(m) if !m.is_dir() => std::fs::remove_file(&socket)?,
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let note = format!(
            "Pegboard keeps its sessions, recordings and settings in {} since 3c; \
             this directory is the Pegboard browser's own data.\n",
            home.display()
        );
        write_no_follow(&legacy.join(MOVED_NOTE), note.as_bytes())?;
    }
    rename_no_replace(moving, home)?;
    Ok(Migration::Moved { entries: moved })
}

/// Whether `path` names anything, a symbolic link included (it is not followed).
fn exists_no_follow(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// `dir` is a real directory (not a symbolic link) owned by this user.
fn require_own_dir(dir: &Path) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(dir)?;
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let why = if meta.file_type().is_symlink() {
        "it is a symbolic link"
    } else if !meta.is_dir() {
        "it is not a directory"
    } else if meta.uid() != uid {
        "it is owned by another user"
    } else {
        return Ok(());
    };
    Err(Error::new(
        ErrorKind::PermissionDenied,
        format!(
            "refusing to move pegboard's files out of {}: {why}; set PEGBOARD_HOME to keep using it",
            dir.display()
        ),
    ))
}

/// Writes `bytes` to `path` (0600), never through a symbolic link.
fn write_no_follow(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

/// `rename` that fails (`AlreadyExists`) instead of replacing what `to` names. Atomic on
/// macOS (`renamex_np` with `RENAME_EXCL`).
fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = |p: &Path| {
            std::ffi::CString::new(p.as_os_str().as_bytes())
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))
        };
        let (from_c, to_c) = (c(from)?, c(to)?);
        // SAFETY: both are NUL-terminated paths that live for the call.
        if unsafe { libc::renamex_np(from_c.as_ptr(), to_c.as_ptr(), libc::RENAME_EXCL) } == 0 {
            return Ok(());
        }
        Err(std::io::Error::last_os_error())
    }
    #[cfg(not(target_os = "macos"))]
    {
        if exists_no_follow(to)? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} exists", to.display()),
            ));
        }
        std::fs::rename(from, to)
    }
}

/// Layout for an explicit home directory (no environment lookup).
pub fn paths_in(home: &Path) -> Paths {
    let home = home.to_path_buf();
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    Paths {
        chrome_udd: home.join("chrome"),
        recordings: home.join("recordings"),
        socket: home.join("pegboardd.sock"),
        state: home.join("state.json"),
        ab_socket_dir: PathBuf::from(format!("/tmp/pegboard-{uid}/ab")),
        ab_home: home.join("agent-browser-home"),
        config: home.join("config.toml"),
        home,
    }
}

fn create_private(dir: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    // The directory may already exist with looser permissions.
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Longest unix socket path the platform accepts (`sun_path` is 104 bytes on macOS, with the NUL).
const MAX_SOCKET_PATH: usize = 103;

/// Whether the API socket path fits in a unix socket address.
pub fn socket_path_fits(paths: &Paths) -> bool {
    use std::os::unix::ffi::OsStrExt;
    paths.socket.as_os_str().as_bytes().len() <= MAX_SOCKET_PATH
}

/// Creates the home directory (0700) and nothing else.
pub fn ensure_home(paths: &Paths) -> std::io::Result<()> {
    create_private(&paths.home)
}

/// Create all directories with mode 0700. The agent-browser socket directory lives in the
/// shared `/tmp`, so it is checked component by component (see `ensure_socket_dir`).
pub fn ensure(paths: &Paths) -> std::io::Result<()> {
    for dir in [&paths.home, &paths.chrome_udd, &paths.recordings] {
        create_private(dir)?;
    }
    ensure_socket_dir(&paths.ab_socket_dir)
}

/// Creates the session's files directory (`<home>/sessions/<id>/files`), each component a
/// real directory owned by the current user with mode 0700, like `ensure_socket_dir`.
pub fn ensure_session_files(paths: &Paths, id: &SessionId) -> std::io::Result<PathBuf> {
    let files = paths.session_files(id);
    let session = files.parent().unwrap_or(&paths.home);
    let sessions = session.parent().unwrap_or(&paths.home);
    for dir in [&paths.home, sessions, session, files.as_path()] {
        ensure_owned_private_dir(dir)?;
    }
    Ok(files)
}

/// Creates the session's confirmation copies directory (`<home>/sessions/<id>/confirm`),
/// each component a real directory owned by the current user with mode 0700.
pub fn ensure_session_confirm(paths: &Paths, id: &SessionId) -> std::io::Result<PathBuf> {
    let confirm = paths.session_confirm_dir(id);
    let session = confirm.parent().unwrap_or(&paths.home);
    let sessions = session.parent().unwrap_or(&paths.home);
    for dir in [&paths.home, sessions, session, confirm.as_path()] {
        ensure_owned_private_dir(dir)?;
    }
    Ok(confirm)
}

/// Creates `dir` and its parent (`/tmp/pegboard-<uid>` and its `ab`) when missing, and
/// requires each of the two to be a real directory (never a symlink), owned by the current
/// user, with mode 0700. Anything else is refused and left untouched: another local user
/// could have planted it to read or replace the agent-browser sockets.
pub fn ensure_socket_dir(dir: &Path) -> std::io::Result<()> {
    if let Some(parent) = dir.parent() {
        ensure_owned_private_dir(parent)?;
    }
    ensure_owned_private_dir(dir)
}

/// Creates `dir` (not its parents) when missing and requires it to be a real directory
/// owned by the current user with mode 0700; anything else is refused and left untouched.
pub fn ensure_owned_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    use std::os::unix::fs::MetadataExt;

    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let refuse = |why: String| {
        Err(Error::new(
            ErrorKind::PermissionDenied,
            format!("refusing to use {}: {why}", dir.display()),
        ))
    };
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() {
        return refuse("it is a symbolic link".into());
    }
    if !meta.is_dir() {
        return refuse("it is not a directory".into());
    }
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    if meta.uid() != uid {
        return refuse(format!("it is owned by uid {}, not {uid}", meta.uid()));
    }
    let mode = meta.mode() & 0o777;
    if mode != 0o700 {
        return refuse(format!("its mode is {mode:04o}, not 0700"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uploads_may_not_reach_pegboards_or_the_browsers_data() {
        let user = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        let p = paths_in(&user.join("Library/Application Support/Pegboard"));
        let f = p.upload_forbidden();
        for d in [
            user.join("Library/Application Support/Pegboard"),
            user.join("Library/Caches/Pegboard"),
            user.join("Library/Caches/Pegboard/chrome"),
            user.join("Library/Saved Application State/app.pegboard.browser.savedState"),
            p.ab_socket_dir.parent().unwrap().to_path_buf(),
            p.chrome_udd.clone(),
        ] {
            assert!(f.contains(&d), "{d:?} not in {f:?}");
        }
        // Another home: its own directories, and the fork's default ones all the same.
        let q = paths_in(Path::new("/tmp/elsewhere"));
        let f = q.upload_forbidden();
        assert!(f.contains(&PathBuf::from("/tmp/elsewhere")));
        assert!(f.contains(&user.join("Library/Caches/Pegboard")));
    }

    #[test]
    fn default_home_is_pegboard_agent() {
        let home = home_for(None, Some("/u".into()));
        assert_eq!(
            home,
            PathBuf::from("/u/Library/Application Support/Pegboard Agent")
        );
        assert_eq!(paths_in(&home).socket, home.join("pegboardd.sock"));
        // PEGBOARD_HOME always wins.
        assert_eq!(
            home_for(Some("/elsewhere".into()), Some("/u".into())),
            PathBuf::from("/elsewhere")
        );
    }

    #[test]
    fn pegboard_home_env_skips_migration() {
        assert_eq!(
            migration_support_dir(None, Some(OsStr::new("/u"))),
            Some(PathBuf::from("/u/Library/Application Support"))
        );
        assert_eq!(
            migration_support_dir(Some(OsStr::new("/x")), Some(OsStr::new("/u"))),
            None
        );
        assert_eq!(migration_support_dir(None, None), None);
        assert_eq!(migration_support_dir(None, Some(OsStr::new(""))), None);
    }

    /// `<tmp>/Library/Application Support` with a legacy home (0700) in it.
    fn support_with_legacy() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let support = tmp.path().join("Application Support");
        std::fs::create_dir(&support).unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(support.join(LEGACY_HOME_DIR_NAME))
            .unwrap();
        (tmp, support)
    }

    /// Plants every legacy entry (each with content naming it) and the browser's own.
    fn plant_everything(legacy: &Path) {
        for name in LEGACY_ENTRIES {
            if name.contains('.') {
                std::fs::write(legacy.join(name), name.as_bytes()).unwrap();
            } else {
                std::fs::create_dir(legacy.join(name)).unwrap();
                std::fs::write(legacy.join(name).join("inside"), name.as_bytes()).unwrap();
            }
        }
        for dir in ["Default", "PegboardAgent-x", "Crashpad"] {
            std::fs::create_dir(legacy.join(dir)).unwrap();
            std::fs::write(legacy.join(dir).join("inside"), dir.as_bytes()).unwrap();
        }
        std::fs::write(legacy.join("Local State"), b"{}").unwrap();
    }

    fn assert_entry_is(path: &Path, name: &str) {
        let file = if path.is_dir() {
            path.join("inside")
        } else {
            path.to_path_buf()
        };
        assert_eq!(std::fs::read(&file).unwrap(), name.as_bytes(), "{path:?}");
    }

    fn assert_browsers_entries_stay(legacy: &Path) {
        for dir in ["Default", "PegboardAgent-x", "Crashpad"] {
            assert_entry_is(&legacy.join(dir), dir);
        }
        assert_eq!(std::fs::read(legacy.join("Local State")).unwrap(), b"{}");
    }

    #[test]
    fn migration_moves_only_pegboards_entries() {
        let (_tmp, support) = support_with_legacy();
        let legacy = support.join(LEGACY_HOME_DIR_NAME);
        plant_everything(&legacy);
        let m = migrate_legacy_home(&support).unwrap();
        let all: Vec<String> = LEGACY_ENTRIES.iter().map(|n| n.to_string()).collect();
        assert_eq!(m, Migration::Moved { entries: all });
        let home = support.join(HOME_DIR_NAME);
        for name in LEGACY_ENTRIES {
            assert_entry_is(&home.join(name), name);
            assert!(!exists_no_follow(&legacy.join(name)).unwrap(), "{name}");
        }
        assert_eq!(mode_of(&home), 0o700);
        assert!(!support.join(MOVING_DIR_NAME).exists());
        assert_browsers_entries_stay(&legacy);
        // Once: the next call has nothing to do and changes nothing.
        assert_eq!(migrate_legacy_home(&support).unwrap(), Migration::NotNeeded);
        assert_browsers_entries_stay(&legacy);
    }

    #[test]
    fn migration_is_not_needed_without_pegboard_files() {
        let (_tmp, support) = support_with_legacy();
        let legacy = support.join(LEGACY_HOME_DIR_NAME);
        std::fs::create_dir(legacy.join("Default")).unwrap();
        std::fs::write(legacy.join("Local State"), b"{}").unwrap();
        // A log or a stock browser's data alone is not a home worth moving.
        std::fs::write(legacy.join("daemon.log"), b"x").unwrap();
        assert_eq!(migrate_legacy_home(&support).unwrap(), Migration::NotNeeded);
        assert!(!support.join(HOME_DIR_NAME).exists());
        assert!(!support.join(MOVING_DIR_NAME).exists());
        assert!(legacy.join("daemon.log").exists());
        assert!(!legacy.join("daemon.lock").exists(), "nothing was created");
        // No legacy home at all.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            migrate_legacy_home(empty.path()).unwrap(),
            Migration::NotNeeded
        );
        assert!(!empty.path().join(HOME_DIR_NAME).exists());
    }

    #[test]
    fn migration_leaves_an_existing_new_home_alone() {
        let (_tmp, support) = support_with_legacy();
        let legacy = support.join(LEGACY_HOME_DIR_NAME);
        plant_everything(&legacy);
        let home = support.join(HOME_DIR_NAME);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        std::fs::write(home.join("state.json"), b"new").unwrap();
        assert_eq!(migrate_legacy_home(&support).unwrap(), Migration::NotNeeded);
        assert_eq!(std::fs::read(home.join("state.json")).unwrap(), b"new");
        for name in LEGACY_ENTRIES {
            assert_entry_is(&legacy.join(name), name);
        }
    }

    #[test]
    fn migration_waits_for_a_running_legacy_daemon() {
        let (_tmp, support) = support_with_legacy();
        let legacy = support.join(LEGACY_HOME_DIR_NAME);
        plant_everything(&legacy);
        std::fs::write(legacy.join("pegboardd.sock"), b"").unwrap();
        let held = crate::lock::DaemonLock::try_acquire(&paths_in(&legacy))
            .unwrap()
            .expect("free");
        assert_eq!(
            migrate_legacy_home(&support).unwrap(),
            Migration::LegacyDaemonRunning
        );
        for name in LEGACY_ENTRIES {
            assert_entry_is(&legacy.join(name), name);
        }
        assert!(legacy.join("pegboardd.sock").exists());
        assert!(legacy.join("daemon.lock").exists());
        assert!(!legacy.join(MOVED_NOTE).exists());
        assert!(!support.join(HOME_DIR_NAME).exists());
        assert!(!support.join(MOVING_DIR_NAME).exists());
        // Once that daemon has gone, the move happens.
        drop(held);
        assert!(matches!(
            migrate_legacy_home(&support).unwrap(),
            Migration::Moved { .. }
        ));
    }

    #[test]
    fn migration_resumes_after_an_interrupted_move() {
        let (_tmp, support) = support_with_legacy();
        let legacy = support.join(LEGACY_HOME_DIR_NAME);
        plant_everything(&legacy);
        let moving = support.join(MOVING_DIR_NAME);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&moving)
            .unwrap();
        std::fs::rename(legacy.join("sessions"), moving.join("sessions")).unwrap();
        let m = migrate_legacy_home(&support).unwrap();
        let rest: Vec<String> = LEGACY_ENTRIES
            .iter()
            .filter(|n| **n != "sessions")
            .map(|n| n.to_string())
            .collect();
        assert_eq!(m, Migration::Moved { entries: rest });
        let home = support.join(HOME_DIR_NAME);
        for name in LEGACY_ENTRIES {
            assert_entry_is(&home.join(name), name);
        }
        assert!(!moving.exists());
        assert_browsers_entries_stay(&legacy);

        // A process that died after every entry had moved: no marker is left in the
        // legacy home, and the next call still finishes.
        let (_tmp, support) = support_with_legacy();
        let moving = support.join(MOVING_DIR_NAME);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&moving)
            .unwrap();
        std::fs::write(moving.join("state.json"), b"state.json").unwrap();
        assert_eq!(
            migrate_legacy_home(&support).unwrap(),
            Migration::Moved { entries: vec![] }
        );
        assert_entry_is(
            &support.join(HOME_DIR_NAME).join("state.json"),
            "state.json",
        );
    }

    #[test]
    fn migration_never_overwrites() {
        let (_tmp, support) = support_with_legacy();
        let legacy = support.join(LEGACY_HOME_DIR_NAME);
        std::fs::write(legacy.join("state.json"), b"old").unwrap();
        std::fs::create_dir(legacy.join("sessions")).unwrap();
        std::fs::write(legacy.join("sessions/inside"), b"old").unwrap();
        let moving = support.join(MOVING_DIR_NAME);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&moving)
            .unwrap();
        std::fs::write(moving.join("state.json"), b"half-moved").unwrap();
        // An empty directory too: plain rename(2) would replace it.
        std::fs::create_dir(moving.join("sessions")).unwrap();
        let e = migrate_legacy_home(&support).unwrap_err();
        assert!(e.to_string().contains("nothing was deleted"), "{e}");
        assert_eq!(std::fs::read(legacy.join("state.json")).unwrap(), b"old");
        assert_eq!(
            std::fs::read(moving.join("state.json")).unwrap(),
            b"half-moved"
        );
        assert_eq!(
            std::fs::read(legacy.join("sessions/inside")).unwrap(),
            b"old"
        );
        assert!(moving.join("sessions").is_dir());
        assert!(!support.join(HOME_DIR_NAME).exists());
        // And again: the same error, still nothing lost.
        assert!(migrate_legacy_home(&support).is_err());
        assert_eq!(std::fs::read(legacy.join("state.json")).unwrap(), b"old");
    }

    #[test]
    fn migration_leaves_a_note_and_drops_socket_and_lock() {
        let (_tmp, support) = support_with_legacy();
        let legacy = support.join(LEGACY_HOME_DIR_NAME);
        std::fs::write(legacy.join("config.toml"), b"").unwrap();
        std::fs::write(legacy.join("pegboardd.sock"), b"").unwrap();
        std::fs::write(legacy.join("daemon.lock"), b"").unwrap();
        assert!(matches!(
            migrate_legacy_home(&support).unwrap(),
            Migration::Moved { .. }
        ));
        assert!(!legacy.join("pegboardd.sock").exists());
        assert!(!legacy.join("daemon.lock").exists());
        assert!(!support.join(HOME_DIR_NAME).join("daemon.lock").exists());
        let note = std::fs::read_to_string(legacy.join(MOVED_NOTE)).unwrap();
        assert!(
            note.contains(&support.join(HOME_DIR_NAME).display().to_string()),
            "{note}"
        );
    }

    #[test]
    fn migration_refuses_a_symlinked_legacy_home() {
        let tmp = tempfile::tempdir().unwrap();
        let support = tmp.path().join("support");
        let real = tmp.path().join("real");
        std::fs::create_dir(&support).unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&real)
            .unwrap();
        std::fs::write(real.join("state.json"), b"s").unwrap();
        std::os::unix::fs::symlink(&real, support.join(LEGACY_HOME_DIR_NAME)).unwrap();
        let e = migrate_legacy_home(&support).unwrap_err();
        assert!(e.to_string().contains("symbolic link"), "{e}");
        assert!(real.join("state.json").exists());
        assert!(!support.join(HOME_DIR_NAME).exists());
        assert!(!support.join(MOVING_DIR_NAME).exists());
    }

    #[test]
    fn upload_forbidden_covers_both_dirs_and_their_caches() {
        let user = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        let support = user.join("Library/Application Support");
        let caches = user.join("Library/Caches");
        for home in [
            support.join(HOME_DIR_NAME),
            PathBuf::from("/tmp/elsewhere-home"),
        ] {
            let f = paths_in(&home).upload_forbidden();
            for d in [
                home.clone(),
                home.join("chrome"),
                support.join(HOME_DIR_NAME),
                support.join(MOVING_DIR_NAME),
                support.join(LEGACY_HOME_DIR_NAME),
                caches.join(HOME_DIR_NAME),
                caches.join(HOME_DIR_NAME).join("chrome"),
                caches.join(LEGACY_HOME_DIR_NAME),
            ] {
                assert!(f.contains(&d), "{d:?} not in {f:?}");
            }
        }
    }

    #[test]
    fn home_follows_env_and_dirs_are_private() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("PEGBOARD_HOME", tmp.path());
        let p = paths();
        ensure(&p).unwrap();
        assert_eq!(p.socket, tmp.path().join("pegboardd.sock"));
        let mode = std::fs::metadata(&p.home).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert!(p.ab_socket_dir.to_str().unwrap().len() < 40);
    }

    fn mode_of(p: &Path) -> u32 {
        std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn socket_dir_chain_is_created_private() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pegboard-x/ab");
        ensure_socket_dir(&dir).unwrap();
        assert_eq!(mode_of(&dir), 0o700);
        assert_eq!(mode_of(dir.parent().unwrap()), 0o700);
        // Reusing a good chain is fine.
        ensure_socket_dir(&dir).unwrap();
    }

    #[test]
    fn symlinked_socket_dir_components_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&elsewhere)
            .unwrap();
        // The parent is a symlink.
        let link = tmp.path().join("pegboard-a");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        let e = ensure_socket_dir(&link.join("ab")).unwrap_err();
        assert!(e.to_string().contains("symbolic link"), "{e}");
        // The leaf is a symlink.
        let parent = tmp.path().join("pegboard-b");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&parent)
            .unwrap();
        std::os::unix::fs::symlink(&elsewhere, parent.join("ab")).unwrap();
        let e = ensure_socket_dir(&parent.join("ab")).unwrap_err();
        assert!(e.to_string().contains("symbolic link"), "{e}");
    }

    #[test]
    fn loose_or_non_directory_socket_dir_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("pegboard-c");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&parent)
            .unwrap();
        let leaf = parent.join("ab");
        std::fs::DirBuilder::new().create(&leaf).unwrap();
        std::fs::set_permissions(&leaf, std::fs::Permissions::from_mode(0o755)).unwrap();
        let e = ensure_socket_dir(&leaf).unwrap_err();
        assert!(e.to_string().contains("0755"), "{e}");
        assert_eq!(
            mode_of(&leaf),
            0o755,
            "a refused directory is left as it was"
        );

        let file_parent = tmp.path().join("pegboard-d");
        std::fs::write(&file_parent, b"").unwrap();
        let e = ensure_socket_dir(&file_parent.join("ab")).unwrap_err();
        assert!(e.to_string().contains("not a directory"), "{e}");
    }

    #[test]
    fn session_files_dir_is_private_and_not_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths_in(&tmp.path().join("home"));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&p.home)
            .unwrap();
        let id = SessionId("s_abcdefgh".into());
        let files = ensure_session_files(&p, &id).unwrap();
        assert_eq!(files, p.home.join("sessions/s_abcdefgh/files"));
        for dir in [
            &files,
            &p.home.join("sessions/s_abcdefgh"),
            &p.home.join("sessions"),
        ] {
            assert_eq!(mode_of(dir), 0o700, "{dir:?}");
        }
        let other = SessionId("s_bbbbbbbb".into());
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(p.home.join("sessions/s_bbbbbbbb"))
            .unwrap();
        std::os::unix::fs::symlink(tmp.path(), p.home.join("sessions/s_bbbbbbbb/files")).unwrap();
        let e = ensure_session_files(&p, &other).unwrap_err();
        assert!(e.to_string().contains("symbolic link"), "{e}");
    }
}
