//! `<PEGBOARD_HOME>/config.toml`: the `[recording]` settings (retention, size limits and the
//! domains whose screen is not written). A missing file means the defaults.

use serde::Deserialize;

use crate::address::{host_listed, target_of};
use crate::paths::Paths;
use crate::policy::{is_host, normalized_domains};

const MAX_AGE_DAYS: std::ops::RangeInclusive<u32> = 1..=3650;

/// A gigabyte, as the settings count it.
const GB: f64 = 1e9;

/// How many `blob:`/`view-source:`/`filesystem:` wrappers `screen_off` unwraps; a URL
/// nested deeper counts as off.
pub const SCREEN_OFF_DEPTH: usize = 8;

/// Smallest size limit, in GB (1 MB): below it a limit is surely a typing error.
const MIN_GB: f64 = 0.001;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RecordingConfig {
    /// Finished recordings older than this are deleted. 1 to 3650.
    pub max_age_days: u32,
    /// Above this, the oldest finished recordings lose their screen first. Finite, above 0.
    pub max_total_gb: f64,
    /// Above this, a recording stops recording its screen (its actions go on). Finite,
    /// above 0.
    pub max_per_recording_gb: f64,
    /// The screen of a tab on one of these (or a subdomain) is not written: host names or
    /// IP addresses, kept as the policy keeps its allowed domains (lower case, sorted).
    pub screen_off_domains: Vec<String>,
}

impl Default for RecordingConfig {
    fn default() -> RecordingConfig {
        RecordingConfig {
            max_age_days: 14,
            max_total_gb: 5.0,
            max_per_recording_gb: 1.0,
            screen_off_domains: Vec::new(),
        }
    }
}

/// The whole file: only a `[recording]` table.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default)]
    recording: RecordingConfig,
}

impl RecordingConfig {
    /// The settings of `paths.config`. A missing file means the defaults; a file that cannot
    /// be read or is wrong is an error that names the file.
    pub fn load(paths: &Paths) -> Result<RecordingConfig, String> {
        let file = paths.config.display();
        match std::fs::read_to_string(&paths.config) {
            Ok(text) => RecordingConfig::parse(&text).map_err(|e| format!("{file}: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RecordingConfig::default()),
            Err(e) => Err(format!("{file}: {e}")),
        }
    }

    /// Reads and checks the text of a config file. The error is one line naming the key.
    pub fn parse(text: &str) -> Result<RecordingConfig, String> {
        let file: ConfigFile = toml::from_str(text).map_err(|e| toml_error(text, &e))?;
        file.recording.checked()
    }

    fn checked(mut self) -> Result<RecordingConfig, String> {
        if !MAX_AGE_DAYS.contains(&self.max_age_days) {
            return Err(format!(
                "max_age_days must be from {} to {} (is {})",
                MAX_AGE_DAYS.start(),
                MAX_AGE_DAYS.end(),
                self.max_age_days
            ));
        }
        for (key, value) in [
            ("max_total_gb", self.max_total_gb),
            ("max_per_recording_gb", self.max_per_recording_gb),
        ] {
            if !(value.is_finite() && value >= MIN_GB) {
                return Err(format!(
                    "{key} must be a number of GB, at least {MIN_GB} (1 MB) (is {value})"
                ));
            }
        }
        if let Some(bad) = self.screen_off_domains.iter().find(|d| !is_host(d)) {
            return Err(format!(
                "screen_off_domains: {bad:?} is not a host name or IP address \
                 (no scheme, path, port or wildcard; subdomains are included; write a name \
                 that is not ASCII as punycode, xn--...)"
            ));
        }
        self.screen_off_domains = normalized_domains(&self.screen_off_domains);
        Ok(self)
    }

    pub fn max_total_bytes(&self) -> u64 {
        (self.max_total_gb * GB) as u64
    }

    pub fn max_per_recording_bytes(&self) -> u64 {
        (self.max_per_recording_gb * GB) as u64
    }

    pub fn max_age(&self) -> time::Duration {
        time::Duration::days(i64::from(self.max_age_days))
    }

    /// Whether frames of a main frame at `url` must not be written. A URL that does not
    /// parse counts as off when the list is not empty; data: and about:blank are never off.
    /// `blob:`, `view-source:` and `filesystem:` URLs are judged by the URL inside them; any
    /// other URL without a host (`file:`, `about:srcdoc`, ...) cannot be told apart from a
    /// listed page and counts as off too, as does one nested more than `SCREEN_OFF_DEPTH`
    /// deep.
    pub fn screen_off(&self, url: &str) -> bool {
        if self.screen_off_domains.is_empty() {
            return false;
        }
        self.screen_off_within(url, SCREEN_OFF_DEPTH)
    }

    fn screen_off_within(&self, url: &str, depth: usize) -> bool {
        let url = url.trim();
        let Ok(parsed) = url::Url::parse(url) else {
            return true;
        };
        let scheme = parsed.scheme();
        match scheme {
            "data" => false,
            "about" if parsed.path() == "blank" => false,
            // The inner URL, from the parsed form (tabs and newlines taken out, as the browser
            // does), so the cut after `<scheme>:` is exact.
            "blob" | "view-source" | "filesystem" => match depth.checked_sub(1) {
                Some(depth) => self.screen_off_within(&parsed.as_str()[scheme.len() + 1..], depth),
                None => true,
            },
            _ if target_of(&parsed).is_none() => true,
            _ => host_listed(&parsed, &self.screen_off_domains),
        }
    }
}

/// A TOML error on one line: where it is, what is wrong, and the line itself (which names
/// the key when the message does not).
fn toml_error(text: &str, e: &toml::de::Error) -> String {
    let Some(span) = e.span() else {
        return e.message().to_string();
    };
    let start = span.start.min(text.len());
    let number = text.get(..start).unwrap_or(text).matches('\n').count() + 1;
    let line = text.lines().nth(number - 1).unwrap_or("").trim();
    format!("line {number}: {} ({line})", e.message())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_without_a_file() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(tmp.path());
        assert_eq!(paths.config, tmp.path().join("config.toml"));
        let c = RecordingConfig::load(&paths).unwrap();
        assert_eq!(c, RecordingConfig::default());
        assert_eq!(c.max_age_days, 14);
        assert_eq!(c.max_total_gb, 5.0);
        assert_eq!(c.max_per_recording_gb, 1.0);
        assert!(c.screen_off_domains.is_empty());
        assert_eq!(c.max_total_bytes(), 5_000_000_000);
        assert_eq!(c.max_per_recording_bytes(), 1_000_000_000);
        assert_eq!(c.max_age(), time::Duration::days(14));
        // An empty file, or one without a [recording] table, is the defaults too.
        assert_eq!(RecordingConfig::parse("").unwrap(), c);
        assert_eq!(RecordingConfig::parse("[recording]\n").unwrap(), c);
    }

    #[test]
    fn config_is_parsed_and_checked() {
        let c = RecordingConfig::parse(
            "[recording]\nmax_age_days = 3\nscreen_off_domains = [\"Bank.Example\"]",
        )
        .unwrap();
        assert_eq!(c.max_age_days, 3);
        assert_eq!(c.screen_off_domains, vec!["bank.example"]);
        assert_eq!(c.max_total_gb, 5.0);
        let c = RecordingConfig::parse(
            "[recording]\nmax_total_gb = 0.5\nmax_per_recording_gb = 2\nmax_age_days = 3650\n\
             screen_off_domains = [\"b.test\", \"a.test\", \"A.test\", \"::1\", \"10.0.0.1\"]",
        )
        .unwrap();
        assert_eq!(c.max_total_bytes(), 500_000_000);
        assert_eq!(c.max_per_recording_bytes(), 2_000_000_000);
        assert_eq!(c.max_age(), time::Duration::days(3650));
        assert_eq!(
            c.screen_off_domains,
            vec!["10.0.0.1", "::1", "a.test", "b.test"]
        );
        for (text, key) in [
            ("max_age_days = 0", "max_age_days"),
            ("max_age_days = 3651", "max_age_days"),
            ("max_age_days = -1", "max_age_days"),
            ("max_total_gb = 0", "max_total_gb"),
            ("max_total_gb = -1", "max_total_gb"),
            ("max_total_gb = inf", "max_total_gb"),
            ("max_per_recording_gb = nan", "max_per_recording_gb"),
            ("max_total_gb = 1e-12", "max_total_gb"),
            ("max_per_recording_gb = 0.0009", "max_per_recording_gb"),
            ("max_per_recording_gb = \"1\"", "max_per_recording_gb"),
            (
                "screen_off_domains = [\"https://bank.example\"]",
                "screen_off_domains",
            ),
            (
                "screen_off_domains = [\"*.bank.example\"]",
                "screen_off_domains",
            ),
            (
                "screen_off_domains = [\"bank.example/x\"]",
                "screen_off_domains",
            ),
            ("screen_off_domains = [\"\"]", "screen_off_domains"),
            (
                "screen_off_domains = \"bank.example\"",
                "screen_off_domains",
            ),
            (
                "screen_off_domain = [\"bank.example\"]",
                "screen_off_domain",
            ),
        ] {
            let full = format!("[recording]\n{text}\n");
            let err = RecordingConfig::parse(&full).unwrap_err();
            assert!(err.contains(key), "{text}: {err}");
        }
        // A name that is not ASCII is written as punycode, and the message says so.
        let err = RecordingConfig::parse("[recording]\nscreen_off_domains = [\"bänk.example\"]")
            .unwrap_err();
        assert!(err.contains("xn--"), "{err}");
        let c = RecordingConfig::parse(
            "[recording]\nmax_total_gb = 0.001\nscreen_off_domains = [\"xn--bnk-qla.example\"]",
        )
        .unwrap();
        assert_eq!(c.max_total_bytes(), 1_000_000);
        assert!(c.screen_off("https://bänk.example/"));
        let err = RecordingConfig::parse("[other]\nx = 1\n").unwrap_err();
        assert!(err.contains("other"), "{err}");
        let err = RecordingConfig::parse("max_age_days = 3\n").unwrap_err();
        assert!(err.contains("max_age_days"), "{err}");
        assert!(RecordingConfig::parse("this is not toml").is_err());
        assert!(RecordingConfig::parse("[recording\nmax_age_days = 3").is_err());
    }

    #[test]
    fn a_config_file_that_cannot_be_used_is_an_error_naming_it() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(tmp.path());
        std::fs::write(&paths.config, "[recording]\nmax_age_days = 7\n").unwrap();
        assert_eq!(RecordingConfig::load(&paths).unwrap().max_age_days, 7);
        std::fs::write(&paths.config, "[recording]\nmax_age_days = 0\n").unwrap();
        let err = RecordingConfig::load(&paths).unwrap_err();
        assert!(
            err.contains("config.toml") && err.contains("max_age_days"),
            "{err}"
        );
        std::fs::write(&paths.config, b"\xff\xfe").unwrap();
        assert!(RecordingConfig::load(&paths)
            .unwrap_err()
            .contains("config.toml"));
        std::fs::remove_file(&paths.config).unwrap();
        std::fs::create_dir(&paths.config).unwrap();
        assert!(RecordingConfig::load(&paths)
            .unwrap_err()
            .contains("config.toml"));
    }

    fn off(list: &[&str]) -> RecordingConfig {
        RecordingConfig {
            screen_off_domains: list.iter().map(|d| d.to_string()).collect(),
            ..RecordingConfig::default()
        }
    }

    #[test]
    fn screen_off_matches_domain_and_subdomains_only() {
        let c = off(&["bank.example"]);
        for url in [
            "https://bank.example/",
            "https://www.bank.example/x",
            "https://BANK.example./",
            "http://user:pw@bank.example:8443/login?x=1",
            "not a url",
        ] {
            assert!(c.screen_off(url), "{url}");
        }
        for url in [
            "https://bank.example.evil.test/",
            "https://notbank.example/",
            "data:text/html,x",
            "about:blank",
            "about:blank#x",
            "https://example/",
        ] {
            assert!(!c.screen_off(url), "{url}");
        }
        // A URL whose page is another URL's: blob: by its origin, view-source: and
        // filesystem: by the URL inside.
        for url in [
            "blob:https://bank.example/0b5c7e6a-1f1e-4f6e-9a54-3c1a0d1f2b3c",
            "blob:https://www.bank.example:8443/x",
            "view-source:https://bank.example/login?x=1",
            "view-source:view-source:https://bank.example/",
            "bl\tob:https://bank.example/x",
            "filesystem:https://bank.example/temporary/a.txt",
        ] {
            assert!(c.screen_off(url), "{url}");
        }
        for url in [
            "blob:https://other.test/x",
            "view-source:https://other.test/",
            "filesystem:https://other.test/temporary/a.txt",
            "view-source:data:text/html,x",
        ] {
            assert!(!c.screen_off(url), "{url}");
        }
        // Any other URL without a host cannot be told apart: off while the list is not empty.
        for url in [
            "file:///Users/x/bank.html",
            "about:srcdoc",
            "blob:null/x",
            "javascript:1",
        ] {
            assert!(c.screen_off(url), "{url}");
        }
        // An address matches only the same address.
        let c = off(&["10.0.0.1", "::1"]);
        assert!(c.screen_off("http://10.0.0.1/"));
        assert!(c.screen_off("http://[::1]:8080/"));
        assert!(!c.screen_off("http://10.0.0.10/"));
        assert!(!c.screen_off("http://1.10.0.0.1.test/"));
        // An IPv6 form of a listed IPv4 address is that address, both ways round.
        assert!(c.screen_off("http://[::ffff:10.0.0.1]/"));
        let c = RecordingConfig::parse("[recording]\nscreen_off_domains = [\"::ffff:10.0.0.1\"]")
            .unwrap();
        assert!(c.screen_off("http://10.0.0.1/"));
        assert!(!c.screen_off("http://10.0.0.2/"));
        // Nesting deeper than SCREEN_OFF_DEPTH is not unwrapped: it counts as off.
        let c = off(&["bank.example"]);
        let nested = |n: usize| format!("{}https://other.test/", "view-source:".repeat(n));
        assert!(!c.screen_off(&nested(SCREEN_OFF_DEPTH)));
        assert!(c.screen_off(&nested(SCREEN_OFF_DEPTH + 1)));
        assert!(c.screen_off(&nested(10_000)));
        // An empty list never turns the screen off.
        let c = off(&[]);
        for url in ["https://bank.example/", "not a url", "about:blank"] {
            assert!(!c.screen_off(url), "{url}");
        }
    }
}
