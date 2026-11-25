//! What a confirmation shows a person, and what pins the command to what was shown
//! (design 4.4).
//!
//! Every value an agent chooses (a cookie's name, a key, a session's name) goes through
//! `display_text`, so control, format and bidirectional characters cannot make it look like
//! something else, and a long one cannot push the rest out of sight.

use pegboard_protocol::ConfirmDetail;

/// The most a cookie's or a storage entry's value shows.
pub const SHOWN_VALUE_CHARS: usize = 200;
/// The most any other shown value has.
pub const DETAIL_VALUE_CHARS: usize = 1000;
/// The most a session's or a profile's name shows.
pub const NAME_CHARS: usize = 60;

/// What a confirmed `state` command is held to when it runs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum StatePin {
    /// A cookie set without `--url` or `--domain`: it runs with `--url <this>`, the address
    /// of the tab when the person was asked.
    CookieUrl(String),
    /// A storage write: the tab must still be at this origin when it runs, else `stale`.
    Origin(String),
    /// Nothing to hold it to.
    #[default]
    None,
}

/// Format characters (Unicode category Cf) and the line and paragraph separators: they
/// change how the text around them looks (bidirectional overrides and isolates, marks,
/// invisible joiners) or break it, without being visible themselves.
fn is_format(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x2028..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

/// Control, format and bidi characters become `\u{…}` (lowercase hex); at most `max_chars`
/// characters are kept (an escape is kept whole or not at all), then '…'.
pub fn display_text(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut kept = 0;
    for c in s.chars() {
        let piece = if c.is_control() || is_format(c) {
            format!("\\u{{{:x}}}", c as u32)
        } else {
            c.to_string()
        };
        let n = piece.chars().count();
        if kept + n > max_chars {
            out.push('…');
            return out;
        }
        out.push_str(&piece);
        kept += n;
    }
    out
}

/// The origin a storage write lands in for `url`. An opaque origin (`about:blank`,
/// `data:`) is the address itself, so two of them never count as the same.
pub fn origin_of(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(u) => match u.origin() {
            o @ url::Origin::Tuple(..) => o.ascii_serialization(),
            url::Origin::Opaque(_) => url.to_string(),
        },
        Err(_) => url.to_string(),
    }
}

const NO_TAB: &str = "(탭 없음)";

fn detail(label: &str, value: String) -> ConfirmDetail {
    ConfirmDetail {
        label: label.to_string(),
        value,
    }
}

/// What a state command shows and what pins it, given the address of the tab it would act
/// on (`None`: no tab is known). `argv` is the checked command line that runs.
pub fn state_details(argv: &[String], tab_url: Option<&str>) -> (Vec<ConfirmDetail>, StatePin) {
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["cookies", "set", rest @ ..] => cookie_details(rest, tab_url),
        ["storage", kind @ ("local" | "session"), op @ ("set" | "clear"), rest @ ..] => {
            let store = if *kind == "local" {
                "localStorage"
            } else {
                "sessionStorage"
            };
            let origin = tab_url.map(origin_of);
            let mut details = vec![detail(
                "출처",
                origin
                    .as_deref()
                    .map(|o| display_text(o, DETAIL_VALUE_CHARS))
                    .unwrap_or_else(|| NO_TAB.to_string()),
            )];
            details.push(detail("종류", store.to_string()));
            if *op == "set" {
                details.push(detail(
                    "키",
                    display_text(rest.first().copied().unwrap_or(""), DETAIL_VALUE_CHARS),
                ));
                details.push(detail(
                    "값",
                    display_text(rest.get(1).copied().unwrap_or(""), SHOWN_VALUE_CHARS),
                ));
            } else {
                details.push(detail("동작", format!("이 출처의 {store} 전체 삭제")));
            }
            let pin = origin.map(StatePin::Origin).unwrap_or_default();
            (details, pin)
        }
        _ => (Vec::new(), StatePin::None),
    }
}

/// `cookies set <name> <value>` and its flags (`argv::COOKIE_FLAGS`: `--url`, `--domain`,
/// `--path`, `--same-site`, `--expires` take a value; `--http-only`, `--secure` do not).
fn cookie_details(rest: &[&str], tab_url: Option<&str>) -> (Vec<ConfirmDetail>, StatePin) {
    let mut positional = Vec::new();
    let (mut url, mut domain, mut path) = (None, None, None);
    let mut flags = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let value = rest.get(i + 1).copied();
        match rest[i] {
            "--url" => url = value,
            "--domain" => domain = value,
            "--path" => path = value,
            "--same-site" => flags.push(format!("SameSite={}", value.unwrap_or(""))),
            "--expires" => flags.push(format!("expires={}", value.unwrap_or(""))),
            "--http-only" => {
                flags.push("HttpOnly".to_string());
                i += 1;
                continue;
            }
            "--secure" => {
                flags.push("Secure".to_string());
                i += 1;
                continue;
            }
            word => {
                positional.push(word);
                i += 1;
                continue;
            }
        }
        i += 2;
    }
    let mut details = vec![
        detail(
            "이름",
            display_text(
                positional.first().copied().unwrap_or(""),
                DETAIL_VALUE_CHARS,
            ),
        ),
        detail(
            "값",
            display_text(positional.get(1).copied().unwrap_or(""), SHOWN_VALUE_CHARS),
        ),
    ];
    let shown = |v: &str| display_text(v, DETAIL_VALUE_CHARS);
    let mut pin = StatePin::None;
    if let Some(u) = url {
        details.push(detail("URL", shown(u)));
    }
    if let Some(d) = domain {
        details.push(detail("도메인", shown(d)));
    }
    if url.is_none() && domain.is_none() {
        // Pinned to the tab the person was shown, so a navigation meanwhile changes nothing.
        details.push(detail(
            "URL",
            tab_url.map(shown).unwrap_or_else(|| NO_TAB.to_string()),
        ));
        if let Some(u) = tab_url {
            pin = StatePin::CookieUrl(u.to_string());
        }
    }
    if let Some(p) = path {
        details.push(detail("경로", shown(p)));
    }
    if !flags.is_empty() {
        details.push(detail("플래그", shown(&flags.join(", "))));
    }
    (details, pin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|w| w.to_string()).collect()
    }

    fn value_of<'a>(details: &'a [ConfirmDetail], label: &str) -> &'a str {
        &details
            .iter()
            .find(|d| d.label == label)
            .unwrap_or_else(|| panic!("no {label}: {details:?}"))
            .value
    }

    #[test]
    fn details_escape_bidi_and_control_characters() {
        assert_eq!(
            display_text("a\u{202E}fdp.exe\n", 100),
            "a\\u{202e}fdp.exe\\u{a}"
        );
        for c in [
            '\u{2066}',
            '\u{2067}',
            '\u{2068}',
            '\u{2069}',
            '\u{200E}',
            '\u{200F}',
            '\u{061C}',
            '\u{FEFF}',
            '\u{202A}',
            '\u{200B}',
            '\u{2028}',
            '\u{0}',
            '\u{7F}',
            '\u{85}',
            '\u{E0041}',
        ] {
            let shown = display_text(&format!("x{c}y"), 100);
            assert_eq!(shown, format!("x\\u{{{:x}}}y", c as u32), "{:x}", c as u32);
        }
        // Ordinary text of any script stays as it is.
        assert_eq!(display_text("세션 one é 日本", 100), "세션 one é 日本");
    }

    #[test]
    fn details_cap_values_at_200_chars() {
        let long = "v".repeat(500);
        let argv = s(&["storage", "local", "set", "k", &long]);
        let (details, _) = state_details(&argv, Some("https://a.test/x"));
        let v = value_of(&details, "값");
        assert_eq!(v, format!("{}…", "v".repeat(SHOWN_VALUE_CHARS)));
        let (details, _) = state_details(&s(&["cookies", "set", "n", &long]), None);
        assert_eq!(
            value_of(&details, "값").chars().count(),
            SHOWN_VALUE_CHARS + 1
        );
        // An escape is kept whole or not at all.
        let cut = display_text(&format!("{}\u{202E}", "a".repeat(9)), 12);
        assert_eq!(cut, format!("{}…", "a".repeat(9)));
        assert_eq!(display_text("abc", 3), "abc");
    }

    #[test]
    fn cookie_without_url_is_pinned_to_the_asked_tab() {
        let (details, pin) = state_details(
            &s(&["cookies", "set", "n", "v", "--path", "/p", "--secure"]),
            Some("https://a.test/x"),
        );
        assert_eq!(pin, StatePin::CookieUrl("https://a.test/x".into()));
        assert_eq!(value_of(&details, "이름"), "n");
        assert_eq!(value_of(&details, "값"), "v");
        assert_eq!(value_of(&details, "URL"), "https://a.test/x");
        assert_eq!(value_of(&details, "경로"), "/p");
        assert_eq!(value_of(&details, "플래그"), "Secure");
        // No tab known: nothing to pin it to, and the window says so.
        let (details, pin) = state_details(&s(&["cookies", "set", "n", "v"]), None);
        assert_eq!(pin, StatePin::None);
        assert_eq!(value_of(&details, "URL"), "(탭 없음)");
    }

    #[test]
    fn cookie_with_domain_is_not_repinned() {
        let (details, pin) = state_details(
            &s(&["cookies", "set", "n", "v", "--domain", "b.test"]),
            Some("https://a.test/x"),
        );
        assert_eq!(pin, StatePin::None);
        assert_eq!(value_of(&details, "도메인"), "b.test");
        assert!(details.iter().all(|d| d.label != "URL"), "{details:?}");
        let (details, pin) = state_details(
            &s(&[
                "cookies",
                "set",
                "--url",
                "https://c.test/",
                "n",
                "v",
                "--same-site",
                "Lax",
                "--http-only",
            ]),
            Some("https://a.test/x"),
        );
        assert_eq!(pin, StatePin::None);
        assert_eq!(value_of(&details, "URL"), "https://c.test/");
        assert_eq!(value_of(&details, "이름"), "n");
        assert_eq!(value_of(&details, "플래그"), "SameSite=Lax, HttpOnly");
    }

    #[test]
    fn storage_set_pins_the_origin() {
        let (details, pin) = state_details(
            &s(&["storage", "local", "set", "k\u{202E}", "v"]),
            Some("https://a.test:8443/x?q=1"),
        );
        assert_eq!(pin, StatePin::Origin("https://a.test:8443".into()));
        assert_eq!(value_of(&details, "출처"), "https://a.test:8443");
        assert_eq!(value_of(&details, "종류"), "localStorage");
        assert_eq!(value_of(&details, "키"), "k\\u{202e}");
        let (details, pin) = state_details(
            &s(&["storage", "session", "clear"]),
            Some("https://a.test/x"),
        );
        assert_eq!(pin, StatePin::Origin("https://a.test".into()));
        assert_eq!(
            value_of(&details, "동작"),
            "이 출처의 sessionStorage 전체 삭제"
        );
        // Opaque origins never match each other.
        assert_eq!(origin_of("about:blank"), "about:blank");
        assert_ne!(origin_of("data:text/html,a"), origin_of("data:text/html,b"));
    }

    #[test]
    fn a_tabless_state_command_shows_no_origin_and_pins_none() {
        let (details, pin) = state_details(&s(&["storage", "local", "set", "k", "v"]), None);
        assert_eq!(pin, StatePin::None);
        assert_eq!(value_of(&details, "출처"), "(탭 없음)");
        let (_, pin) = state_details(&s(&["storage", "local", "clear"]), None);
        assert_eq!(pin, StatePin::None);
    }
}
