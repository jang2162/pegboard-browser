//! Filesystem layout for the pegboard home directory.

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

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
}

/// `home` is `$PEGBOARD_HOME` or `~/Library/Application Support/Pegboard`.
pub fn paths() -> Paths {
    let home = match std::env::var_os("PEGBOARD_HOME") {
        Some(h) => PathBuf::from(h),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
            .join("Library/Application Support/Pegboard"),
    };
    paths_in(&home)
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

/// Create all directories with mode 0700. The agent-browser socket directory lives in the
/// shared `/tmp`, so it is checked component by component (see `ensure_socket_dir`).
pub fn ensure(paths: &Paths) -> std::io::Result<()> {
    for dir in [&paths.home, &paths.chrome_udd, &paths.recordings] {
        create_private(dir)?;
    }
    ensure_socket_dir(&paths.ab_socket_dir)
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

fn ensure_owned_private_dir(dir: &Path) -> std::io::Result<()> {
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
        std::fs::DirBuilder::new().mode(0o700).create(&elsewhere).unwrap();
        // The parent is a symlink.
        let link = tmp.path().join("pegboard-a");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        let e = ensure_socket_dir(&link.join("ab")).unwrap_err();
        assert!(e.to_string().contains("symbolic link"), "{e}");
        // The leaf is a symlink.
        let parent = tmp.path().join("pegboard-b");
        std::fs::DirBuilder::new().mode(0o700).create(&parent).unwrap();
        std::os::unix::fs::symlink(&elsewhere, parent.join("ab")).unwrap();
        let e = ensure_socket_dir(&parent.join("ab")).unwrap_err();
        assert!(e.to_string().contains("symbolic link"), "{e}");
    }

    #[test]
    fn loose_or_non_directory_socket_dir_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("pegboard-c");
        std::fs::DirBuilder::new().mode(0o700).create(&parent).unwrap();
        let leaf = parent.join("ab");
        std::fs::DirBuilder::new().create(&leaf).unwrap();
        std::fs::set_permissions(&leaf, std::fs::Permissions::from_mode(0o755)).unwrap();
        let e = ensure_socket_dir(&leaf).unwrap_err();
        assert!(e.to_string().contains("0755"), "{e}");
        assert_eq!(mode_of(&leaf), 0o755, "a refused directory is left as it was");

        let file_parent = tmp.path().join("pegboard-d");
        std::fs::write(&file_parent, b"").unwrap();
        let e = ensure_socket_dir(&file_parent.join("ab")).unwrap_err();
        assert!(e.to_string().contains("not a directory"), "{e}");
    }
}
