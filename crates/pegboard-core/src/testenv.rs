//! Test hooks read from the environment. Every hook needs `PEGBOARD_TEST_HOOKS` as well, so
//! a stray variable in a person's environment changes nothing. A release build (built with
//! `PEGBOARD_RELEASE_VERSION`, see `crate::is_release_build`) has no test hooks at all,
//! whatever the environment says: that is decided when it is compiled, and nothing at run
//! time turns them back on (design R16).

/// Whether test hooks are on (`PEGBOARD_TEST_HOOKS` set, and not a release build). A release
/// build returns before reading the environment.
#[inline]
pub fn hooks() -> bool {
    if crate::is_release_build() {
        return false;
    }
    hooks_in(crate::is_release_build(), hooks_env_set())
}

/// The value of the test variable `name`, only when test hooks are on. A release build
/// returns before reading anything, so the call (and the hook's name) folds away.
#[inline]
pub fn var(name: &str) -> Option<String> {
    if crate::is_release_build() {
        return None;
    }
    var_in(
        crate::is_release_build(),
        hooks_env_set(),
        std::env::var(name).ok(),
    )
}

/// The name of the variable that turns the hooks on; `None` in a release build, so the name
/// is not even in its binary (a const: decided when compiling).
pub(crate) const HOOKS_ENV: Option<&str> = if crate::is_release_build() {
    None
} else {
    Some("PEGBOARD_TEST_HOOKS")
};

fn hooks_env_set() -> bool {
    HOOKS_ENV.is_some_and(|name| std::env::var_os(name).is_some())
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
