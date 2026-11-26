pub mod actionlog;
pub mod address;
pub mod api;
pub mod argv;
pub mod bridge;
pub mod browser;
pub mod cdp;
pub mod confirm;
pub mod contexts;
pub mod daemon;
pub mod downloads;
pub mod guard;
pub mod link;
pub mod lock;
pub mod paths;
pub mod policy;
pub mod profiles;
pub mod proxy;
pub mod recording;
pub mod runner;
pub mod service;
pub mod session;
pub mod settings;
pub mod testenv;
pub mod upload;

/// The release version `release.sh` builds with (`PEGBOARD_RELEASE_VERSION`, e.g.
/// `156.0.8078.109-pb.3`); `None` in every other build.
const RELEASE_VERSION: Option<&str> = option_env!("PEGBOARD_RELEASE_VERSION");

// A release version that is not `<a>.<b>.<c>.<d>-pb.<N>` (an empty one included) does not
// compile: every value of the variable makes a release build, so a typo must not slip by.
const _: () = if let Some(v) = RELEASE_VERSION {
    assert!(
        is_release_version(v),
        "PEGBOARD_RELEASE_VERSION must be <Chromium version>-pb.<N>, e.g. 156.0.8078.109-pb.3"
    )
};

/// `v` is `<a>.<b>.<c>.<d>-pb.<N>`: four dot-separated numbers, then `-pb.` and a build
/// number from 1, all without leading zeros except a lone `0`.
const fn is_release_version(v: &str) -> bool {
    let b = v.as_bytes();
    let mut i = 0;
    let mut part = 0;
    // The four Chromium numbers.
    while part < 4 {
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start || (b[start] == b'0' && i - start > 1) {
            return false;
        }
        part += 1;
        if part < 4 {
            if i >= b.len() || b[i] != b'.' {
                return false;
            }
            i += 1;
        }
    }
    // "-pb." and N.
    if i + 4 > b.len() || b[i] != b'-' || b[i + 1] != b'p' || b[i + 2] != b'b' || b[i + 3] != b'.' {
        return false;
    }
    i += 4;
    if i >= b.len() || b[i] == b'0' {
        return false;
    }
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            return false;
        }
        i += 1;
    }
    true
}

/// The repository this crate was built from, where a development build finds
/// `vendor/agent-browser` and `extension/bridge`; `None` in a release build, which looks only
/// inside its app bundle. A const, so a release binary does not hold the build machine's path.
pub(crate) const BUILD_REPO: Option<&str> = match RELEASE_VERSION {
    Some(_) => None,
    None => Some(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
};

/// This build's version: `<Chromium>-pb.<N>` in a release build (design R13), else
/// `<CARGO_PKG_VERSION>-dev`.
pub fn version() -> &'static str {
    match RELEASE_VERSION {
        Some(v) => v,
        None => concat!(env!("CARGO_PKG_VERSION"), "-dev"),
    }
}

/// Whether this is a release build (built with `PEGBOARD_RELEASE_VERSION`): its test hooks
/// are compiled out (`testenv`). Decided when compiling; nothing changes it at run time.
#[inline]
pub const fn is_release_build() -> bool {
    RELEASE_VERSION.is_some()
}

#[cfg(test)]
mod version_tests {
    #[test]
    fn dev_version_says_dev() {
        assert!(super::version().ends_with("-dev"), "{}", super::version());
        assert!(super::version().starts_with(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn release_version_format() {
        use super::is_release_version as ok;
        assert!(ok("156.0.8078.109-pb.3"));
        assert!(ok("156.0.8078.109-pb.120"));
        for bad in [
            "",
            "156.0.8078.109",
            "156.0.8078.109-pb.",
            "156.0.8078.109-pb.0",
            "156.0.8078.109-pb.03",
            "156.0.8078-pb.3",
            "156.0.8078.109.1-pb.3",
            "156.00.8078.109-pb.3",
            "156.0.8078.109-pb.3a",
            "156.0.8078.109-PB.3",
            " 156.0.8078.109-pb.3",
        ] {
            assert!(!ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_dev_build_knows_its_repository() {
        let repo = super::BUILD_REPO.expect("a dev build has BUILD_REPO");
        assert!(std::path::Path::new(repo).join("Cargo.toml").is_file());
    }
}
