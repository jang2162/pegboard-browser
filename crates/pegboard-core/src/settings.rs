//! `<PEGBOARD_HOME>/config.toml`, `[browser]`: which browser `pegboard browser start` and
//! `stop` drive. `kind = "fork"` is the Pegboard app (it starts the daemon itself), `"stock"`
//! is Chrome that the daemon launches. Without the key: the fork when `Pegboard.app` is in
//! `/Applications` or `~/Applications`, else stock.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::paths::Paths;

/// The fork's app bundle, as it is installed.
pub const APP_NAME: &str = "Pegboard.app";
/// The fork's bundle id (`open -b`).
pub const BUNDLE_ID: &str = "app.pegboard.browser";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserKind {
    Fork,
    Stock,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserSettings {
    pub kind: BrowserKind,
}

/// The parts of the file this module reads. The `[recording]` table is
/// `recording::config`'s, which also refuses unknown tables.
#[derive(Deserialize)]
struct ConfigFile {
    #[serde(default)]
    browser: Option<BrowserTable>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserTable {
    #[serde(default)]
    kind: Option<String>,
}

impl BrowserSettings {
    /// The settings of `paths.config`. A missing file or key means the default (see the
    /// module); a file that cannot be read or a wrong value is an error naming the file.
    pub fn load(paths: &Paths) -> Result<BrowserSettings, String> {
        let file = paths.config.display();
        let text = match std::fs::read_to_string(&paths.config) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("{file}: {e}")),
        };
        BrowserSettings::from_text(text.as_deref(), &application_dirs())
            .map_err(|e| format!("{file}: {e}"))
    }

    /// The settings of a config file's text (`None`: no file), with `applications` the
    /// directories an installed app is looked for in.
    pub fn from_text(
        text: Option<&str>,
        applications: &[PathBuf],
    ) -> Result<BrowserSettings, String> {
        let kind = match text {
            Some(text) => {
                let file: ConfigFile = toml::from_str(text).map_err(|e| e.message().to_string())?;
                file.browser.and_then(|b| b.kind)
            }
            None => None,
        };
        let kind = match kind.as_deref() {
            Some("fork") => BrowserKind::Fork,
            Some("stock") => BrowserKind::Stock,
            Some(other) => {
                return Err(format!(
                    "[browser] kind must be \"fork\" or \"stock\" (is {other:?})"
                ))
            }
            None if installed_app_in(applications).is_some() => BrowserKind::Fork,
            None => BrowserKind::Stock,
        };
        Ok(BrowserSettings { kind })
    }
}

/// Where an installed app is looked for: `/Applications` and `~/Applications`, or only the
/// directory the test variable `PEGBOARD_TEST_APPLICATIONS` names.
pub fn application_dirs() -> Vec<PathBuf> {
    application_dirs_for(
        crate::testenv::var("PEGBOARD_TEST_APPLICATIONS"),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn application_dirs_for(test_dir: Option<String>, home: Option<PathBuf>) -> Vec<PathBuf> {
    if let Some(dir) = test_dir.filter(|d| !d.is_empty()) {
        return vec![PathBuf::from(dir)];
    }
    let mut dirs = vec![PathBuf::from("/Applications")];
    if let Some(home) = home {
        dirs.push(home.join("Applications"));
    }
    dirs
}

/// The installed `Pegboard.app`, if there is one.
pub fn installed_app() -> Option<PathBuf> {
    installed_app_in(&application_dirs())
}

fn installed_app_in(dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|d| d.join(APP_NAME))
        .find(|app| Path::new(app).is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_kind_defaults_to_fork_when_the_app_is_installed() {
        let apps = tempfile::tempdir().unwrap();
        // The test variable stands for both application directories.
        let dirs = application_dirs_for(
            Some(apps.path().display().to_string()),
            Some(PathBuf::from("/nonexistent-home")),
        );
        assert_eq!(dirs, vec![apps.path().to_path_buf()]);
        let kind = |text: Option<&str>| BrowserSettings::from_text(text, &dirs).unwrap().kind;
        assert_eq!(kind(None), BrowserKind::Stock);
        assert_eq!(
            kind(Some("[recording]\nmax_age_days = 3\n")),
            BrowserKind::Stock
        );
        std::fs::create_dir(apps.path().join(APP_NAME)).unwrap();
        assert_eq!(kind(None), BrowserKind::Fork);
        assert_eq!(kind(Some("[browser]\n")), BrowserKind::Fork);
        // Said in the file: that, whatever is installed.
        assert_eq!(
            kind(Some("[browser]\nkind = \"stock\"\n")),
            BrowserKind::Stock
        );
        // Without the variable: the two usual places.
        assert_eq!(
            application_dirs_for(None, Some(PathBuf::from("/h"))),
            vec![
                PathBuf::from("/Applications"),
                PathBuf::from("/h/Applications")
            ]
        );
    }

    #[test]
    fn browser_kind_reads_config() {
        let none: Vec<PathBuf> = Vec::new();
        let read = |text: &str| BrowserSettings::from_text(Some(text), &none);
        assert_eq!(
            read("[browser]\nkind = \"stock\"\n").unwrap().kind,
            BrowserKind::Stock
        );
        assert_eq!(
            read("[browser]\nkind = \"fork\"\n").unwrap().kind,
            BrowserKind::Fork
        );
        let e = read("[browser]\nkind = \"x\"\n").unwrap_err();
        assert!(e.contains("kind") && e.contains("\"x\""), "{e}");
        assert!(read("[browser]\nkind = 1\n").is_err());
        assert!(read("[browser]\nknd = \"fork\"\n").is_err());
        assert!(read("[browser\n").is_err());
    }

    #[test]
    fn load_names_the_file() {
        let home = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(home.path());
        std::fs::write(&paths.config, "[browser]\nkind = \"x\"\n").unwrap();
        let e = BrowserSettings::load(&paths).unwrap_err();
        assert!(e.contains("config.toml"), "{e}");
        std::fs::write(&paths.config, "[browser]\nkind = \"stock\"\n").unwrap();
        assert_eq!(
            BrowserSettings::load(&paths).unwrap().kind,
            BrowserKind::Stock
        );
    }
}
