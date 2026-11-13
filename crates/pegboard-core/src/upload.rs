//! Where an upload may take its files from, shared by the daemon (which turns a file name
//! into a path) and the proxy filter (which checks the path again before Chrome reads it).

use std::path::{Path, PathBuf};

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

/// Whether `path` is an absolute path to a regular file whose real path (symbolic links
/// followed) is inside the real path of one of `dirs`.
pub fn path_allowed(path: &Path, dirs: &[PathBuf]) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let Ok(real) = path.canonicalize() else {
        return false;
    };
    if !std::fs::metadata(&real).is_ok_and(|m| m.is_file()) {
        return false;
    }
    dirs.iter()
        .filter_map(|d| d.canonicalize().ok())
        .any(|d| real.starts_with(&d) && real != d)
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
