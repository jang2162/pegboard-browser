//! Test hooks read from the environment. Every hook needs `PEGBOARD_TEST_HOOKS` as well, so
//! a stray variable in a person's environment changes nothing. A release build (built with
//! `PEGBOARD_RELEASE_VERSION`, see `crate::is_release_build`) has no test hooks at all,
//! whatever the environment says: that is decided when it is compiled, and nothing at run
//! time turns them back on (design R16).

/// Whether test hooks are on (`PEGBOARD_TEST_HOOKS` set, and not a release build).
pub fn hooks() -> bool {
    hooks_in(
        crate::is_release_build(),
        std::env::var_os("PEGBOARD_TEST_HOOKS").is_some(),
    )
}

/// The value of the test variable `name`, only when test hooks are on.
pub fn var(name: &str) -> Option<String> {
    var_in(
        crate::is_release_build(),
        std::env::var_os("PEGBOARD_TEST_HOOKS").is_some(),
        std::env::var(name).ok(),
    )
}

fn hooks_in(release: bool, hooks_on: bool) -> bool {
    hooks_on && !release
}

/// A test variable's `value` as the hooks see it: `None` in a release build, or with test
/// hooks off.
pub(crate) fn var_in(release: bool, hooks_on: bool, value: Option<String>) -> Option<String> {
    value.filter(|_| hooks_in(release, hooks_on))
}

#[cfg(test)]
mod tests {
    use super::{hooks_in, var_in};

    #[test]
    fn a_test_variable_counts_only_with_hooks_on() {
        assert_eq!(var_in(false, false, Some("3".into())), None);
        assert_eq!(var_in(false, true, Some("3".into())), Some("3".into()));
        assert_eq!(var_in(false, true, None), None);
    }

    #[test]
    fn release_build_ignores_test_hooks() {
        assert_eq!(var_in(true, true, Some("1".into())), None);
        assert_eq!(var_in(false, true, Some("1".into())), Some("1".into()));
        assert_eq!(var_in(false, false, Some("1".into())), None);
        assert!(!hooks_in(true, true));
        assert!(hooks_in(false, true));
        assert!(!hooks_in(false, false));
    }

    /// The tests are never a release build, so the hooks they use are on.
    #[test]
    fn tests_are_not_a_release_build() {
        assert!(!crate::is_release_build());
    }
}
