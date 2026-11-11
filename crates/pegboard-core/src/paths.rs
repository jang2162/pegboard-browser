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

/// Create all directories with mode 0700.
pub fn ensure(paths: &Paths) -> std::io::Result<()> {
    for dir in [&paths.home, &paths.chrome_udd, &paths.recordings, &paths.ab_socket_dir] {
        create_private(dir)?;
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
}
