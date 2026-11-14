//! Test hooks read from the environment. Every hook needs `PEGBOARD_TEST_HOOKS` as well, so
//! a stray variable in a person's environment changes nothing.

/// Whether test hooks are on (`PEGBOARD_TEST_HOOKS` set).
pub fn hooks() -> bool {
    std::env::var_os("PEGBOARD_TEST_HOOKS").is_some()
}

/// The value of the test variable `name`, only when test hooks are on.
pub fn var(name: &str) -> Option<String> {
    gated(hooks(), std::env::var(name).ok())
}

fn gated(hooks: bool, value: Option<String>) -> Option<String> {
    value.filter(|_| hooks)
}

#[cfg(test)]
mod tests {
    use super::gated;

    #[test]
    fn a_test_variable_counts_only_with_hooks_on() {
        assert_eq!(gated(false, Some("3".into())), None);
        assert_eq!(gated(true, Some("3".into())), Some("3".into()));
        assert_eq!(gated(true, None), None);
    }
}
