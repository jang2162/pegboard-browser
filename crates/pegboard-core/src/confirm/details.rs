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
    /// A storage write: when it runs, the frame agent-browser acts in must still be at
    /// `acting` and its tab at `page` (origins), else `stale`.
    Origin { acting: String, page: String },
    /// Nothing to hold it to (the command names where it writes).
    #[default]
    None,
}

/// Where a state command would act, as read when it is asked about.
#[derive(Debug, Clone, Copy, Default)]
pub struct Where<'a> {
    /// The address of the tab (its top frame).
    pub page_url: Option<&'a str>,
    /// The address of the frame agent-browser acts in (after `frame <sel>`, that frame;
    /// else the tab's top frame). Read for storage commands only.
    pub acting_url: Option<&'a str>,
}

/// Where the command would write cannot be told: it is neither asked about nor run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownOrigin;

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

fn detail(label: &str, value: String) -> ConfirmDetail {
    ConfirmDetail {
        label: label.to_string(),
        value,
    }
}

/// The path a cookie set for `url` without `--path` gets (RFC 6265 5.1.4, default-path).
pub fn default_cookie_path(url: &str) -> String {
    let path = url::Url::parse(url)
        .map(|u| u.path().to_string())
        .unwrap_or_default();
    match path.rfind('/') {
        Some(i) if path.starts_with('/') && i > 0 => path[..i].to_string(),
        _ => "/".to_string(),
    }
}

/// What a state command shows and what pins it, given where it would act. A storage write
/// needs the frame it acts in and its tab; a cookie without `--url`/`--domain` needs the
/// tab. Without them: `UnknownOrigin` (fail closed: a person is never shown a guess).
/// `argv` is the checked command line that runs.
pub fn state_details(
    argv: &[String],
    at: Where<'_>,
) -> Result<(Vec<ConfirmDetail>, StatePin), UnknownOrigin> {
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["cookies", "set", rest @ ..] => cookie_details(rest, at.page_url),
        ["storage", kind @ ("local" | "session"), op @ ("set" | "clear"), rest @ ..] => {
            let store = if *kind == "local" {
                "localStorage"
            } else {
                "sessionStorage"
            };
            let (Some(acting), Some(page)) = (at.acting_url, at.page_url) else {
                return Err(UnknownOrigin);
            };
            let (acting, page) = (origin_of(acting), origin_of(page));
            let mut details = vec![detail("출처", display_text(&acting, DETAIL_VALUE_CHARS))];
            // Inside a frame of another origin: the tab's origin too, so the person sees
            // the write does not go to the page they see.
            if acting != page {
                details.push(detail(
                    "탭 출처 (쓰기는 그 안의 프레임)",
                    display_text(&page, DETAIL_VALUE_CHARS),
                ));
            }
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
            Ok((details, StatePin::Origin { acting, page }))
        }
        _ => Ok((Vec::new(), StatePin::None)),
    }
}

/// `cookies set <name> <value>` and its flags (`argv::COOKIE_FLAGS`: `--url`, `--domain`,
/// `--path`, `--same-site`, `--expires` take a value; `--http-only`, `--secure` do not).
fn cookie_details(
    rest: &[&str],
    tab_url: Option<&str>,
) -> Result<(Vec<ConfirmDetail>, StatePin), UnknownOrigin> {
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
    let shown = |v: &str| display_text(v, DETAIL_VALUE_CHARS);
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
    let mut pin = StatePin::None;
    // The address the cookie is set for: the one given, else the tab's (pinned, so a
    // navigation meanwhile changes nothing). No address and no tab: unknown.
    let effective_url = match (url, domain) {
        (Some(u), _) => Some(u.to_string()),
        (None, Some(_)) => None,
        (None, None) => {
            let u = tab_url.ok_or(UnknownOrigin)?;
            pin = StatePin::CookieUrl(u.to_string());
            Some(u.to_string())
        }
    };
    if let Some(u) = &effective_url {
        details.push(detail("URL", shown(u)));
    }
    if let Some(d) = domain {
        details.push(detail("도메인", shown(d)));
    }
    // The path it gets: the one given, else the default for its address ("/" for a
    // domain alone).
    let effective_path = match (path, &effective_url) {
        (Some(p), _) => p.to_string(),
        (None, Some(u)) => default_cookie_path(u),
        (None, None) => "/".to_string(),
    };
    details.push(detail("경로", shown(&effective_path)));
    if !flags.is_empty() {
        details.push(detail("플래그", shown(&flags.join(", "))));
    }
    Ok((details, pin))
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

    fn tab(url: &str) -> Where<'_> {
        Where {
            page_url: Some(url),
            acting_url: Some(url),
        }
    }

    #[test]
    fn details_cap_values_at_200_chars() {
        let long = "v".repeat(500);
        let argv = s(&["storage", "local", "set", "k", &long]);
        let (details, _) = state_details(&argv, tab("https://a.test/x")).unwrap();
        let v = value_of(&details, "값");
        assert_eq!(v, format!("{}…", "v".repeat(SHOWN_VALUE_CHARS)));
        let (details, _) =
            state_details(&s(&["cookies", "set", "n", &long]), tab("https://a.test/")).unwrap();
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
            tab("https://a.test/x"),
        )
        .unwrap();
        assert_eq!(pin, StatePin::CookieUrl("https://a.test/x".into()));
        assert_eq!(value_of(&details, "이름"), "n");
        assert_eq!(value_of(&details, "값"), "v");
        assert_eq!(value_of(&details, "URL"), "https://a.test/x");
        assert_eq!(value_of(&details, "경로"), "/p");
        assert_eq!(value_of(&details, "플래그"), "Secure");
    }

    #[test]
    fn the_cookie_path_shown_is_the_one_it_gets() {
        let path = |argv: &[&str], url: &str| {
            let (details, _) = state_details(&s(argv), tab(url)).unwrap();
            value_of(&details, "경로").to_string()
        };
        // No --path: the default path of the address it is set for (RFC 6265 5.1.4).
        assert_eq!(
            path(&["cookies", "set", "n", "v"], "https://a.test/a/b/c?q"),
            "/a/b"
        );
        assert_eq!(path(&["cookies", "set", "n", "v"], "https://a.test/x"), "/");
        assert_eq!(path(&["cookies", "set", "n", "v"], "https://a.test/"), "/");
        assert_eq!(
            path(
                &["cookies", "set", "n", "v", "--url", "https://c.test/d/e"],
                "https://a.test/a/b"
            ),
            "/d"
        );
        assert_eq!(
            path(
                &["cookies", "set", "n", "v", "--domain", "b.test"],
                "https://a.test/a/b"
            ),
            "/"
        );
        assert_eq!(
            path(
                &["cookies", "set", "n", "v", "--path", "/z"],
                "https://a.test/a/b"
            ),
            "/z"
        );
    }

    #[test]
    fn cookie_with_domain_is_not_repinned() {
        let (details, pin) = state_details(
            &s(&["cookies", "set", "n", "v", "--domain", "b.test"]),
            tab("https://a.test/x"),
        )
        .unwrap();
        assert_eq!(pin, StatePin::None);
        assert_eq!(value_of(&details, "도메인"), "b.test");
        assert!(details.iter().all(|d| d.label != "URL"), "{details:?}");
        // Given an address or a domain, no tab is needed.
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
            Where::default(),
        )
        .unwrap();
        assert_eq!(pin, StatePin::None);
        assert_eq!(value_of(&details, "URL"), "https://c.test/");
        assert_eq!(value_of(&details, "이름"), "n");
        assert_eq!(value_of(&details, "플래그"), "SameSite=Lax, HttpOnly");
        assert!(state_details(
            &s(&["cookies", "set", "n", "v", "--domain", "b.test"]),
            Where::default()
        )
        .is_ok());
    }

    #[test]
    fn storage_set_pins_the_origin() {
        let (details, pin) = state_details(
            &s(&["storage", "local", "set", "k\u{202E}", "v"]),
            tab("https://a.test:8443/x?q=1"),
        )
        .unwrap();
        assert_eq!(
            pin,
            StatePin::Origin {
                acting: "https://a.test:8443".into(),
                page: "https://a.test:8443".into()
            }
        );
        assert_eq!(value_of(&details, "출처"), "https://a.test:8443");
        assert!(details.iter().all(|d| !d.label.starts_with("탭 출처")));
        assert_eq!(value_of(&details, "종류"), "localStorage");
        assert_eq!(value_of(&details, "키"), "k\\u{202e}");
        let (details, _) = state_details(
            &s(&["storage", "session", "clear"]),
            tab("https://a.test/x"),
        )
        .unwrap();
        assert_eq!(
            value_of(&details, "동작"),
            "이 출처의 sessionStorage 전체 삭제"
        );
        // Opaque origins never match each other.
        assert_eq!(origin_of("about:blank"), "about:blank");
        assert_ne!(origin_of("data:text/html,a"), origin_of("data:text/html,b"));
    }

    #[test]
    fn a_write_inside_a_frame_shows_and_pins_the_frames_origin() {
        let (details, pin) = state_details(
            &s(&["storage", "local", "set", "k", "v"]),
            Where {
                page_url: Some("https://page.test/p"),
                acting_url: Some("https://frame.test/inner"),
            },
        )
        .unwrap();
        assert_eq!(value_of(&details, "출처"), "https://frame.test");
        assert_eq!(
            value_of(&details, "탭 출처 (쓰기는 그 안의 프레임)"),
            "https://page.test"
        );
        assert_eq!(
            pin,
            StatePin::Origin {
                acting: "https://frame.test".into(),
                page: "https://page.test".into()
            }
        );
    }

    #[test]
    fn where_it_writes_unknown_is_refused_not_asked() {
        let storage = s(&["storage", "local", "set", "k", "v"]);
        for at in [
            Where::default(),
            Where {
                page_url: Some("https://a.test/"),
                acting_url: None,
            },
            Where {
                page_url: None,
                acting_url: Some("https://a.test/"),
            },
        ] {
            assert_eq!(state_details(&storage, at), Err(UnknownOrigin), "{at:?}");
            assert_eq!(
                state_details(&s(&["storage", "session", "clear"]), at),
                Err(UnknownOrigin)
            );
        }
        // A cookie without an address needs the tab's.
        assert_eq!(
            state_details(&s(&["cookies", "set", "n", "v"]), Where::default()),
            Err(UnknownOrigin)
        );
    }
}
