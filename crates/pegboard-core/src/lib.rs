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
pub fn is_release_build() -> bool {
    RELEASE_VERSION.is_some()
}

#[cfg(test)]
mod version_tests {
    #[test]
    fn dev_version_says_dev() {
        assert!(super::version().ends_with("-dev"), "{}", super::version());
        assert!(super::version().starts_with(env!("CARGO_PKG_VERSION")));
    }
}
