//! Where an upload may take its files from, shared by the daemon (which turns a file name
//! into a path) and the proxy filter (which checks the path again before Chrome reads it).

use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use pegboard_protocol::{ActionClass, SessionPolicy};

use crate::confirm::details::{shows_plainly, DETAIL_VALUE_CHARS};

/// The largest file an upload that asks a person takes (the download limit).
pub const MAX_UPLOAD_FILE_BYTES: u64 = 200 << 20;
/// The most files one upload that asks a person takes.
pub const MAX_UPLOAD_FILES: usize = 16;
/// The longest absolute path an upload takes (macOS `MAXPATHLEN`, with room for the NUL).
pub const MAX_PATH_BYTES: usize = 1023;

/// The directories the proxy lets the session's `DOM.setFileInputFiles` take files from
/// outside a command: its upload directory and its own files when uploads go unasked
/// (`--allow upload`, or a person allowed them for the rest of a session that has an upload
/// directory); none otherwise. A session that asks opens only the approved copies, and only
/// while the approved command runs (design 4.3).
pub fn standing_dirs(policy: &SessionPolicy, files: PathBuf, granted: bool) -> Vec<PathBuf> {
    let unasked =
        policy.allow.contains(&ActionClass::Upload) || (granted && policy.upload_dir.is_some());
    match (&policy.upload_dir, unasked) {
        (Some(dir), true) => vec![PathBuf::from(dir), files],
        (None, true) => vec![files],
        (_, false) => Vec::new(),
    }
}

/// What a refused upload file is told, whatever the reason: the reasons differ only on the
/// person's side (the daemon's log), so an agent cannot probe the file system through them.
pub const NOT_TAKEN: &str = "pegboard does not upload this file: it must be an existing \
     regular file (not a symbolic link, not hard-linked), at most 200 MiB, outside pegboard's \
     own data and the browser's, with a path in plain text";

/// Characters a person could misread in a path, although they show: letters of
/// right-to-left scripts (they reorder what is around them), combining marks (they change
/// the character before), slash look-alikes, and spaces other than U+0020.
fn misleading_in_path(c: char) -> bool {
    matches!(
        c as u32,
        // Right-to-left scripts: Hebrew, Arabic, Syriac, Thaana, NKo, Samaritan, Mandaic,
        // Arabic supplements and presentation forms, and the right-to-left blocks above.
        0x0590..=0x08FF
            | 0xFB1D..=0xFDFF
            | 0xFE70..=0xFEFF
            | 0x10800..=0x10FFF
            | 0x1E800..=0x1EFFF
            // Combining marks.
            | 0x0300..=0x036F
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x20D0..=0x20FF
            | 0xFE20..=0xFE2F
            // Slash look-alikes.
            | 0x2044
            | 0x2215
            | 0x29F8
            | 0xFF0F
            // Other spaces.
            | 0x00A0
            | 0x1680
            | 0x2000..=0x200A
            | 0x202F
            | 0x205F
            | 0x3000
    )
}

/// Whether `path` is an absolute path in plain form, shown to a person exactly as it is:
/// `/` and components that are neither empty (`//`, a trailing `/`) nor `.` or `..`, at most
/// `MAX_PATH_BYTES` bytes and `DETAIL_VALUE_CHARS` characters, only characters
/// `display_text` leaves alone (no control, format, bidi or invisible character, no
/// backslash), and none a person could misread (`misleading_in_path`).
pub fn check_path_form(path: &str) -> Result<(), String> {
    let Some(rest) = path.strip_prefix('/') else {
        return Err("not an absolute path".into());
    };
    if path.len() > MAX_PATH_BYTES || path.chars().count() > DETAIL_VALUE_CHARS {
        return Err(format!(
            "longer than {MAX_PATH_BYTES} bytes or {DETAIL_VALUE_CHARS} characters"
        ));
    }
    if rest
        .split('/')
        .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err("an empty, '.' or '..' component (give the path in its plain form)".into());
    }
    if !path.chars().all(shows_plainly) {
        return Err(
            "a control, format, bidirectional or invisible character, or a backslash".into(),
        );
    }
    if path.chars().any(misleading_in_path) {
        return Err(
            "a right-to-left letter, a combining mark, a slash look-alike or a special space"
                .into(),
        );
    }
    Ok(())
}

/// Where a file an asking upload takes may be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Allowed {
    /// Directly inside one of these directories (a bare name: the upload directory, the
    /// session's files).
    DirectlyIn(Vec<PathBuf>),
    /// Anywhere but inside these directories (an absolute path: not pegboard's home, not the
    /// browser's user data directory).
    Outside(Vec<PathBuf>),
}

fn real_dir(d: &Path) -> PathBuf {
    d.canonicalize().unwrap_or_else(|_| d.to_path_buf())
}

/// Whether `real` is inside one of `dirs`: by name (each directory's real path), and by
/// identity, comparing the device and inode of every directory above `real` with those of
/// `dirs` (one directory can have two names, such as `/Users/x` and
/// `/System/Volumes/Data/Users/x`; the path read from a descriptor and the one `realpath`
/// gives can differ so).
fn inside_any(real: &Path, dirs: &[PathBuf]) -> bool {
    if dirs.iter().any(|d| real.starts_with(real_dir(d))) {
        return true;
    }
    let ids: Vec<(u64, u64)> = dirs
        .iter()
        .filter_map(|d| std::fs::metadata(d).ok())
        .map(|m| (m.dev(), m.ino()))
        .collect();
    real.ancestors().skip(1).any(|a| {
        std::fs::metadata(a)
            .ok()
            .is_some_and(|m| ids.contains(&(m.dev(), m.ino())))
    })
}

/// Whether a real path (symbolic links resolved) is where `allowed` says, and shows plainly.
fn check_real(real: &Path, allowed: &Allowed) -> Result<(), String> {
    let s = real
        .to_str()
        .ok_or_else(|| "its real path is not text".to_string())?;
    check_path_form(s).map_err(|why| format!("its real path is not shown plainly: {why}"))?;
    if real.starts_with("/dev") {
        return Err("a device directory".into());
    }
    match allowed {
        Allowed::Outside(forbidden) => {
            if inside_any(real, forbidden) {
                return Err(
                    "it is inside pegboard's data directory or the browser's user data directory"
                        .into(),
                );
            }
        }
        Allowed::DirectlyIn(dirs) => {
            let parent = real.parent();
            if !dirs.iter().any(|d| Some(real_dir(d).as_path()) == parent) {
                return Err("it is not directly inside the session's directories".into());
            }
        }
    }
    Ok(())
}

/// An absolute path an asking session without an upload directory may upload: in plain form
/// (`check_path_form`), the last component not a symbolic link, a regular file whose real path
/// is outside every `forbidden` directory (pegboard's home, the browser's user data
/// directory), at most `MAX_UPLOAD_FILE_BYTES`. Returns the real path. A first look only: the
/// file is checked again on the descriptor it is copied from (`open_checked`).
pub fn check_absolute(path: &str, forbidden: &[PathBuf]) -> Result<PathBuf, String> {
    check_path_form(path)?;
    if path.starts_with("/dev/") {
        return Err("a device directory".into());
    }
    let meta = std::fs::symlink_metadata(path).map_err(|_| "no such file".to_string())?;
    if meta.file_type().is_symlink() {
        return Err("it is a symbolic link (give the file it points to)".into());
    }
    if !meta.file_type().is_file() {
        return Err("not a regular file".into());
    }
    if meta.len() > MAX_UPLOAD_FILE_BYTES {
        return Err(format!("larger than {} MiB", MAX_UPLOAD_FILE_BYTES >> 20));
    }
    let real = Path::new(path)
        .canonicalize()
        .map_err(|_| "no such file".to_string())?;
    check_real(&real, &Allowed::Outside(forbidden.to_vec()))?;
    Ok(real)
}

/// A file opened for its copy, and what was found on the descriptor.
#[derive(Debug)]
pub struct Opened {
    pub file: File,
    /// The real path of the open file, from the descriptor itself.
    pub real: PathBuf,
    pub size: u64,
}

/// The path of the file open on `file`, from the descriptor (not from a path that could
/// have changed since).
#[cfg(target_os = "macos")]
fn path_of(file: &File) -> std::io::Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut buf = vec![0u8; libc::PATH_MAX as usize + 1];
    // SAFETY: F_GETPATH writes at most MAXPATHLEN (= PATH_MAX) bytes, NUL included, into a
    // buffer of that size; the descriptor is open for the call.
    let r = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) };
    if r == -1 {
        return Err(std::io::Error::last_os_error());
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len])))
}

#[cfg(not(target_os = "macos"))]
fn path_of(file: &File) -> std::io::Result<PathBuf> {
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

/// Opens `path` to copy it for a person's confirmation, and checks the open file rather than
/// the path: a symbolic link as the last component is not followed (`O_NOFOLLOW`), a FIFO
/// does not block the open (`O_NONBLOCK`); the descriptor must be a regular file with one
/// link (a hard link elsewhere could be a file pegboard keeps), the same file a first look at
/// the path found, at most `MAX_UPLOAD_FILE_BYTES`; its real path, read from the descriptor,
/// must be where `allowed` says and show plainly. What is copied from this descriptor is what
/// the person is shown and what uploads.
pub fn open_checked(path: &Path, allowed: &Allowed) -> Result<Opened, String> {
    let before = std::fs::symlink_metadata(path).map_err(|_| "no such file".to_string())?;
    if before.file_type().is_symlink() {
        return Err("it is a symbolic link (give the file it points to)".into());
    }
    if !before.file_type().is_file() {
        return Err("not a regular file".into());
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| {
            if e.raw_os_error() == Some(libc::ELOOP) {
                "it is a symbolic link (give the file it points to)".to_string()
            } else {
                "it cannot be opened".to_string()
            }
        })?;
    let meta = file
        .metadata()
        .map_err(|_| "it cannot be read".to_string())?;
    if !meta.file_type().is_file() {
        return Err("not a regular file".into());
    }
    if (meta.dev(), meta.ino()) != (before.dev(), before.ino()) {
        return Err("it changed while it was opened".into());
    }
    if meta.nlink() != 1 {
        return Err("it has more than one hard link".into());
    }
    if meta.len() > MAX_UPLOAD_FILE_BYTES {
        return Err(format!("larger than {} MiB", MAX_UPLOAD_FILE_BYTES >> 20));
    }
    let real = path_of(&file).map_err(|_| "its real path cannot be read".to_string())?;
    check_real(&real, allowed)?;
    Ok(Opened {
        file,
        real,
        size: meta.len(),
    })
}

/// The path of the regular file `name` in the first of `dirs` that has it. `name` is a bare
/// file name (`argv` has checked that); the file must not be a symbolic link, a directory or
/// anything else but a regular file, and must be inside its directory's real path.
pub fn resolve(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().find_map(|dir| {
        let path = dir.join(name);
        let meta = std::fs::symlink_metadata(&path).ok()?;
        (meta.file_type().is_file() && path_allowed(&path, std::slice::from_ref(dir)))
            .then_some(path)
    })
}

/// Whether `path` is an absolute path to a regular file directly inside one of `dirs`
/// (symbolic links followed on both sides).
pub fn path_allowed(path: &Path, dirs: &[PathBuf]) -> bool {
    allowed_real_path(path, dirs).is_some()
}

/// The real path (symbolic links followed) of `path` when `path_allowed` holds: the string
/// the browser should be given, so that the file judged is the file it opens. A file in a
/// subdirectory is not allowed: names are bare, so the daemon never produces one.
pub fn allowed_real_path(path: &Path, dirs: &[PathBuf]) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let real = path.canonicalize().ok()?;
    if !std::fs::metadata(&real).is_ok_and(|m| m.is_file()) {
        return None;
    }
    let parent = real.parent()?;
    dirs.iter()
        .filter_map(|d| d.canonicalize().ok())
        .any(|d| parent == d)
        .then_some(real)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Rig {
        _tmp: tempfile::TempDir,
        upload: PathBuf,
        files: PathBuf,
        outside: PathBuf,
    }

    fn rig() -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (upload, files, outside) = (
            root.join("upload"),
            root.join("files"),
            root.join("outside"),
        );
        for d in [&upload, &files, &outside] {
            std::fs::create_dir(d).unwrap();
        }
        Rig {
            _tmp: tmp,
            upload,
            files,
            outside,
        }
    }

    fn dirs(r: &Rig) -> Vec<PathBuf> {
        vec![r.upload.clone(), r.files.clone()]
    }

    #[test]
    fn absolute_path_checks() {
        let r = rig();
        let (home, udd) = (r.outside.join("home"), r.outside.join("udd"));
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        std::fs::create_dir(&udd).unwrap();
        let forbidden = [home.clone(), udd.clone()];
        let p = |p: &Path| p.display().to_string();
        std::fs::write(r.upload.join("a.txt"), "a").unwrap();
        assert_eq!(
            check_absolute(&p(&r.upload.join("a.txt")), &forbidden),
            Ok(r.upload.join("a.txt"))
        );
        // Through a directory link: allowed, and the real path comes back.
        symlink(&r.upload, r.files.join("via")).unwrap();
        assert_eq!(
            check_absolute(&p(&r.files.join("via/a.txt")), &forbidden),
            Ok(r.upload.join("a.txt"))
        );
        // The last component a link, a directory, missing.
        symlink(r.upload.join("a.txt"), r.upload.join("link")).unwrap();
        assert!(check_absolute(&p(&r.upload.join("link")), &forbidden)
            .unwrap_err()
            .contains("symbolic link"));
        assert!(check_absolute(&p(&r.upload), &forbidden)
            .unwrap_err()
            .contains("regular file"));
        assert!(check_absolute(&p(&r.upload.join("nope")), &forbidden).is_err());
        // Inside the home or the user data directory, also through a link into them.
        std::fs::write(home.join("sessions/secret"), "s").unwrap();
        std::fs::write(udd.join("Cookies"), "c").unwrap();
        symlink(&udd, r.upload.join("udd-link")).unwrap();
        for bad in [
            home.join("sessions/secret"),
            udd.join("Cookies"),
            r.upload.join("udd-link/Cookies"),
        ] {
            let e = check_absolute(&p(&bad), &forbidden).unwrap_err();
            assert!(e.contains("pegboard's data directory"), "{bad:?}: {e}");
        }
        // Over the size limit (sparse).
        let big = r.upload.join("big");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_UPLOAD_FILE_BYTES + 1).unwrap();
        assert!(check_absolute(&p(&big), &forbidden)
            .unwrap_err()
            .contains("larger than 200 MiB"));
        f.set_len(MAX_UPLOAD_FILE_BYTES).unwrap();
        assert!(check_absolute(&p(&big), &forbidden).is_ok());
        // Not in plain form.
        for bad in ["/tmp/../etc/passwd", "relative", "/tmp//a"] {
            assert!(check_absolute(bad, &forbidden).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_device_a_fifo_and_a_socket_are_not_regular_files() {
        let r = rig();
        let forbidden: [PathBuf; 0] = [];
        let fifo = r.upload.join("fifo");
        let c = std::ffi::CString::new(fifo.display().to_string()).unwrap();
        // SAFETY: a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let sock = r.upload.join("sock");
        let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        for bad in [fifo.as_path(), sock.as_path(), Path::new("/dev/null")] {
            let e = check_absolute(&bad.display().to_string(), &forbidden).unwrap_err();
            assert!(
                e.contains("regular file") || e.contains("device"),
                "{bad:?}: {e}"
            );
            // Opening one does not block (a FIFO without a writer) and is refused.
            let e = open_checked(bad, &Allowed::Outside(forbidden.to_vec())).unwrap_err();
            assert!(e.contains("regular file"), "{bad:?}: {e}");
        }
    }

    #[test]
    fn paths_a_person_could_misread_are_refused() {
        for bad in [
            "/tmp/\u{5d0}.txt",
            "/tmp/\u{627}.txt",
            "/tmp/e\u{301}.txt",
            "/tmp/a\u{2215}b",
            "/tmp/a\u{2044}b",
            "/tmp/a\u{ff0f}b",
            "/tmp/a\u{a0}b",
            "/tmp/a\u{2003}b",
            "/tmp/a\u{3000}b",
        ] {
            let e = check_path_form(bad).unwrap_err();
            assert!(
                e.contains("right-to-left") || e.contains("combining"),
                "{bad:?}: {e}"
            );
        }
        for good in [
            "/tmp/보고서 1.pdf",
            "/tmp/résumé.txt",
            "/tmp/日本.txt",
            "/tmp/a b",
        ] {
            assert!(check_path_form(good).is_ok(), "{good}");
        }
        assert!(check_absolute("/dev/null", &[])
            .unwrap_err()
            .contains("device"));
        let long = format!("/{}", "a".repeat(DETAIL_VALUE_CHARS));
        assert!(check_path_form(&long).unwrap_err().contains("characters"));
    }

    #[test]
    fn a_forbidden_directory_is_found_under_another_name() {
        // A directory with two names: by its real path, and through a link (the identity
        // comparison covers names `realpath` does not give, such as macOS firmlinks under
        // /System/Volumes/Data, which a test cannot make).
        let r = rig();
        let inner = r.outside.join("inner");
        std::fs::create_dir(&inner).unwrap();
        let alias = r.upload.join("alias");
        symlink(&r.outside, &alias).unwrap();
        for forbidden in [r.outside.clone(), alias.clone()] {
            assert!(inside_any(
                &inner.join("x"),
                std::slice::from_ref(&forbidden)
            ));
            assert!(inside_any(
                &alias.join("inner/x"),
                std::slice::from_ref(&forbidden)
            ));
            assert!(!inside_any(
                &r.files.join("x"),
                std::slice::from_ref(&forbidden)
            ));
        }
    }

    #[test]
    fn the_open_file_is_checked_not_the_path() {
        let r = rig();
        let forbidden = [r.outside.clone()];
        std::fs::write(r.upload.join("a.txt"), "a").unwrap();
        let opened = open_checked(
            &r.upload.join("a.txt"),
            &Allowed::Outside(forbidden.to_vec()),
        )
        .unwrap();
        assert_eq!(
            (opened.real.clone(), opened.size),
            (r.upload.join("a.txt"), 1)
        );
        // Through a directory link, the descriptor's own path is the real one.
        symlink(&r.upload, r.files.join("via")).unwrap();
        let opened = open_checked(
            &r.files.join("via/a.txt"),
            &Allowed::Outside(forbidden.to_vec()),
        )
        .unwrap();
        assert_eq!(opened.real, r.upload.join("a.txt"));
        // A directory link into a forbidden place: found from the descriptor.
        std::fs::write(r.outside.join("secret"), "s").unwrap();
        symlink(&r.outside, r.upload.join("out")).unwrap();
        let e = open_checked(
            &r.upload.join("out/secret"),
            &Allowed::Outside(forbidden.to_vec()),
        )
        .unwrap_err();
        assert!(e.contains("pegboard's data directory"), "{e}");
        // A file with a second hard link.
        std::fs::write(r.upload.join("h1"), "h").unwrap();
        std::fs::hard_link(r.upload.join("h1"), r.upload.join("h2")).unwrap();
        let e = open_checked(&r.upload.join("h2"), &Allowed::Outside(Vec::new())).unwrap_err();
        assert!(e.contains("hard link"), "{e}");
        // Directly inside the given directories only.
        let dirs = [r.files.clone()];
        let e =
            open_checked(&r.upload.join("a.txt"), &Allowed::DirectlyIn(dirs.to_vec())).unwrap_err();
        assert!(e.contains("not directly inside"), "{e}");
        std::fs::write(r.files.join("f.txt"), "f").unwrap();
        assert!(open_checked(&r.files.join("f.txt"), &Allowed::DirectlyIn(dirs.to_vec())).is_ok());
    }

    #[test]
    fn ask_mode_proxy_takes_no_folder_outside_a_command() {
        let files = PathBuf::from("/h/sessions/s/files");
        let asking = SessionPolicy {
            upload_dir: Some("/up".into()),
            ..SessionPolicy::default()
        };
        // Asking, with or without a directory: nothing (the approved copy only while it runs).
        assert!(standing_dirs(&asking, files.clone(), false).is_empty());
        assert!(standing_dirs(&SessionPolicy::default(), files.clone(), false).is_empty());
        // A grant without a directory opens nothing.
        assert!(standing_dirs(&SessionPolicy::default(), files.clone(), true).is_empty());
        // `--allow upload --upload-dir`, or a person's grant with a directory: as before.
        let allowed = SessionPolicy {
            allow: vec![ActionClass::Upload],
            ..asking.clone()
        };
        let both = vec![PathBuf::from("/up"), files.clone()];
        assert_eq!(standing_dirs(&allowed, files.clone(), false), both);
        assert_eq!(standing_dirs(&asking, files.clone(), true), both);
    }

    #[test]
    fn a_name_is_found_in_the_upload_dir_first_then_the_files_dir() {
        let r = rig();
        std::fs::write(r.upload.join("a.txt"), "up").unwrap();
        std::fs::write(r.files.join("a.txt"), "dl").unwrap();
        std::fs::write(r.files.join("b.txt"), "dl").unwrap();
        assert_eq!(resolve("a.txt", &dirs(&r)), Some(r.upload.join("a.txt")));
        assert_eq!(resolve("b.txt", &dirs(&r)), Some(r.files.join("b.txt")));
        assert_eq!(resolve("c.txt", &dirs(&r)), None);
    }

    #[test]
    fn a_symlink_a_directory_or_a_missing_directory_is_not_a_file() {
        let r = rig();
        std::fs::write(r.outside.join("secret"), "s").unwrap();
        symlink(r.outside.join("secret"), r.upload.join("link")).unwrap();
        symlink(&r.outside, r.upload.join("dirlink")).unwrap();
        std::fs::create_dir(r.upload.join("sub")).unwrap();
        assert_eq!(resolve("link", &dirs(&r)), None);
        assert_eq!(resolve("dirlink", &dirs(&r)), None);
        assert_eq!(resolve("sub", &dirs(&r)), None);
        // A symlink to a file in the same directory is a symlink too.
        std::fs::write(r.upload.join("real"), "r").unwrap();
        symlink(r.upload.join("real"), r.upload.join("alias")).unwrap();
        assert_eq!(resolve("alias", &dirs(&r)), None);
        // The upload directory is gone: nothing is found there, the other one still works.
        std::fs::remove_dir_all(&r.upload).unwrap();
        std::fs::write(r.files.join("d.txt"), "d").unwrap();
        assert_eq!(resolve("real", &dirs(&r)), None);
        assert_eq!(resolve("d.txt", &dirs(&r)), Some(r.files.join("d.txt")));
    }

    #[test]
    fn a_directory_that_is_itself_a_symlink_is_followed_for_the_person() {
        // The person's folder may be a link (a shortcut to Documents); what matters is that
        // the file ends up inside the folder's real path.
        let r = rig();
        let link = r.outside.join("shortcut");
        symlink(&r.upload, &link).unwrap();
        std::fs::write(r.upload.join("a.txt"), "a").unwrap();
        assert_eq!(
            resolve("a.txt", std::slice::from_ref(&link)),
            Some(link.join("a.txt"))
        );
    }

    #[test]
    fn the_filter_check_follows_links_and_wants_a_regular_file_inside() {
        let r = rig();
        let d = dirs(&r);
        std::fs::write(r.upload.join("a.txt"), "a").unwrap();
        std::fs::write(r.outside.join("o.txt"), "o").unwrap();
        symlink(r.outside.join("o.txt"), r.upload.join("link")).unwrap();
        std::fs::create_dir(r.upload.join("sub")).unwrap();
        assert!(path_allowed(&r.upload.join("a.txt"), &d));
        assert!(!path_allowed(&r.outside.join("o.txt"), &d));
        assert!(!path_allowed(&r.upload.join("link"), &d));
        assert!(!path_allowed(&r.upload.join("sub"), &d));
        assert!(!path_allowed(&r.upload.join("missing"), &d));
        // Directly inside only: a file deeper down is not allowed.
        std::fs::create_dir_all(r.upload.join("sub/deep")).unwrap();
        std::fs::write(r.upload.join("sub/deep/x"), "x").unwrap();
        std::fs::write(r.upload.join("sub/y"), "y").unwrap();
        assert!(!path_allowed(&r.upload.join("sub/deep/x"), &d));
        assert!(!path_allowed(&r.upload.join("sub/y"), &d));
        // A link to a file in the same directory passes this check (the daemon refuses
        // links itself); the real path is what comes back.
        symlink(r.upload.join("a.txt"), r.upload.join("alias")).unwrap();
        assert!(path_allowed(&r.upload.join("alias"), &d));
        assert_eq!(
            allowed_real_path(&r.upload.join("alias"), &d),
            Some(r.upload.join("a.txt"))
        );
        assert_eq!(
            allowed_real_path(&r.upload.join("../upload/a.txt"), &d),
            Some(r.upload.join("a.txt"))
        );
        assert!(!path_allowed(&r.upload.join("../outside/o.txt"), &d));
        assert!(!path_allowed(Path::new("a.txt"), &d), "relative");
        assert!(!path_allowed(Path::new("/etc/passwd"), &d));
        assert!(!path_allowed(&r.upload.join("a.txt"), &[]), "no directory");
        // A sibling whose name starts with the directory's name is outside it.
        let sibling = r.upload.with_file_name("upload2");
        std::fs::create_dir(&sibling).unwrap();
        std::fs::write(sibling.join("x"), "x").unwrap();
        assert!(!path_allowed(&sibling.join("x"), &d));
    }
}
