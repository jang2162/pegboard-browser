//! The single-instance lock of a pegboard home: `<home>/daemon.lock`, an exclusive
//! non-blocking `flock` held by the daemon for as long as it lives. The kernel drops it when
//! the process ends, so a crashed daemon never leaves it held.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

use crate::paths::Paths;

/// Held while this value (the open file) lives. The descriptor is close-on-exec, so the
/// browser and agent-browser processes the daemon starts do not inherit the lock.
pub struct DaemonLock {
    _file: File,
}

impl DaemonLock {
    /// `Some` when the lock was free and is now ours, `None` when another process holds it.
    /// The home directory must exist.
    pub fn try_acquire(paths: &Paths) -> std::io::Result<Option<DaemonLock>> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(paths.home.join("daemon.lock"))?;
        // SAFETY: the descriptor is open for the whole call.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(Some(DaemonLock { _file: file }));
        }
        let e = std::io::Error::last_os_error();
        if e.kind() == std::io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(e)
        }
    }
}
