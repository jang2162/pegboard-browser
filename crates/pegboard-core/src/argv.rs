//! Validation of the agent-browser command line an agent supplies.
//!
//! pegboard prepends its own `--session`, `--cdp` and `--json`; agent-browser 0.42.0 lets a
//! later occurrence of a flag override an earlier one, so flags that pick the browser, the
//! session, policies or the daemon's lifetime are refused wherever they appear. Words after
//! `--` are literal arguments to agent-browser (it stops reading flags there).

use pegboard_protocol::{codes, RpcError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checked {
    Run(Vec<String>),
    TabNew { url: String },
}

pub const REJECTED_FLAGS: &[&str] = &[
    "--cdp",
    "--cdp-headers",
    "--auto-connect",
    "--no-auto-connect",
    "--provider",
    "-p",
    "--profile",
    "--executable-path",
    "--args",
    "--extension",
    "--state",
    "--config",
    "--engine",
    "--session",
    "--namespace",
    "--restore",
    "--action-policy",
    "--confirm-actions",
    "--confirm-interactive",
    "--allowed-domains",
    "--headed",
    "--proxy",
    "--proxy-bypass",
    "--relaunch",
    "--idle-timeout",
    "--pin-tab",
    "--no-pin-tab",
    "--json",
];

/// `batch` runs further command lines inside one invocation, which would bypass this check.
pub const REJECTED_COMMANDS: &[&str] = &[
    "session",
    "close",
    "connect",
    "install",
    "upgrade",
    "record",
    "stream",
    "dashboard",
    "mcp",
    "chat",
    "profiles",
    "state",
    "auth",
    "plugin",
    "doctor",
    "skills",
    "confirm",
    "deny",
    "batch",
];

fn rejected(message: impl Into<String>) -> RpcError {
    RpcError {
        code: codes::ARGV_REJECTED.to_string(),
        message: message.into(),
        data: None,
    }
}

/// Validates a user-supplied agent-browser command line (without `--session`, `--cdp`, `--json`).
///
/// The command must be the first word. agent-browser takes the first word that is neither a
/// known flag nor the value of one as its command, so a leading flag that takes a value
/// (`--user-agent x close`) would hide the real command from a simpler reading; options go
/// after the command instead, where agent-browser reads them just the same.
pub fn check(argv: &[String]) -> Result<Checked, RpcError> {
    let Some(command) = argv.first() else {
        return Err(rejected("empty command line"));
    };
    if command.starts_with('-') {
        return Err(rejected(
            "the command must come first; put options after it",
        ));
    }
    if REJECTED_COMMANDS.contains(&command.as_str()) {
        return Err(rejected(format!(
            "'{command}' is not available in a pegboard session"
        )));
    }
    for word in argv.iter().take_while(|w| *w != "--") {
        let name = word.split('=').next().unwrap_or(word);
        if REJECTED_FLAGS.contains(&name) {
            return Err(rejected(format!(
                "'{name}' is not available in a pegboard session"
            )));
        }
    }
    if command == "tab" && argv.get(1).map(String::as_str) == Some("new") {
        let url = positionals(&argv[2..])
            .next()
            .cloned()
            .unwrap_or_else(|| "about:blank".to_string());
        return Ok(Checked::TabNew { url });
    }
    Ok(Checked::Run(argv.to_vec()))
}

/// Flags of `tab new` that take the next word as their value.
const TAB_NEW_VALUE_FLAGS: &[&str] = &["--label"];

/// Words that are not flags: those before `--` that neither start with '-' nor are the
/// value of a `TAB_NEW_VALUE_FLAGS` flag, and every word after it.
fn positionals(words: &[String]) -> impl Iterator<Item = &String> {
    let split = words.iter().position(|w| w == "--").unwrap_or(words.len());
    let (before, after) = words.split_at(split);
    let mut skip_next = false;
    before
        .iter()
        .filter(move |w| {
            let value =
                std::mem::replace(&mut skip_next, TAB_NEW_VALUE_FLAGS.contains(&w.as_str()));
            !value && !w.starts_with('-')
        })
        .chain(after.iter().skip(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: Vec<&str>) -> Vec<String> {
        v.into_iter().map(String::from).collect()
    }

    #[test]
    fn plain_commands_pass() {
        for a in [
            vec!["snapshot", "-i"],
            vec!["click", "@e1"],
            vec!["open", "https://a.test"],
            vec!["get", "url"],
        ] {
            assert!(matches!(check(&s(a)), Ok(Checked::Run(_))));
        }
    }

    #[test]
    fn connection_flags_are_rejected_anywhere() {
        for a in [
            vec!["--cdp", "9222", "snapshot"],
            vec!["snapshot", "--cdp=9222"],
            vec!["open", "x", "--profile", "Default"],
            vec!["-p", "browserbase", "open", "x"],
            vec!["snapshot", "--session", "other"],
            vec!["eval", "1", "--action-policy", "p.json"],
        ] {
            assert_eq!(check(&s(a)).unwrap_err().code, "argv_rejected");
        }
    }

    #[test]
    fn pegboard_owned_commands_are_rejected() {
        for c in [
            "close", "session", "record", "stream", "connect", "install", "mcp", "state", "auth",
        ] {
            assert!(check(&s(vec![c])).is_err());
        }
    }

    #[test]
    fn literal_text_after_double_dash_is_not_inspected() {
        assert!(check(&s(vec!["fill", "#q", "--", "--cdp"])).is_ok());
    }

    #[test]
    fn tab_new_is_routed_to_pegboard() {
        assert!(matches!(
            check(&s(vec!["tab", "new", "https://a.test"])),
            Ok(Checked::TabNew { .. })
        ));
    }

    #[test]
    fn empty_argv_is_rejected() {
        assert!(check(&[]).is_err());
    }

    #[test]
    fn leading_flag_is_rejected() {
        // agent-browser would run `close` here: --user-agent takes the next word as its value.
        for a in [
            vec!["--user-agent", "x", "close"],
            vec!["--annotate", "screenshot"],
        ] {
            assert_eq!(check(&s(a)).unwrap_err().code, "argv_rejected");
        }
        assert!(check(&s(vec!["screenshot", "--annotate"])).is_ok());
    }

    #[test]
    fn batch_cannot_smuggle_commands() {
        assert!(check(&s(vec!["batch", "close"])).is_err());
    }

    #[test]
    fn tab_new_takes_its_url_or_blank() {
        let url = |a: Vec<&str>| match check(&s(a)) {
            Ok(Checked::TabNew { url }) => url,
            _ => panic!("not routed to pegboard"),
        };
        assert_eq!(url(vec!["tab", "new", "https://a.test"]), "https://a.test");
        assert_eq!(url(vec!["tab", "new"]), "about:blank");
        assert_eq!(url(vec!["tab", "new", "--", "-x"]), "-x");
        assert_eq!(
            url(vec!["tab", "new", "--label", "docs", "https://a.test"]),
            "https://a.test"
        );
        assert!(matches!(
            check(&s(vec!["tab", "list"])),
            Ok(Checked::Run(_))
        ));
        assert!(check(&s(vec!["tab", "new", "--cdp", "1"])).is_err());
    }
}
