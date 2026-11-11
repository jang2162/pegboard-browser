//! Registry of agent profiles.
//!
//! Every agent profile is a Chrome profile directory of its own inside the pegboard user
//! data directory, never the human one. "default" always exists; named profiles are kept
//! in `<home>/state.json`.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use anyhow::{Context, Result};
use pegboard_protocol::{codes, RpcError};
use serde::{Deserialize, Serialize};

use crate::paths::Paths;

pub const DEFAULT_PROFILE: &str = "default";
const MAX_NAME_LEN: usize = 32;

/// Chrome profile directory of an agent profile: "default" -> "PegboardAgent",
/// "work" -> "PegboardAgent-work". Only call it with a name that passed `valid_profile_name`.
pub fn profile_dir(name: &str) -> String {
    if name == DEFAULT_PROFILE {
        "PegboardAgent".to_string()
    } else {
        format!("PegboardAgent-{name}")
    }
}

/// 1..=32 characters of `[a-z0-9-]`, not starting with '-'. Names come from callers and
/// end up in a directory name and a process argument, so nothing else is accepted.
pub fn valid_profile_name(name: &str) -> bool {
    (1..=MAX_NAME_LEN).contains(&name.len())
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub(crate) fn rpc_error(code: &str, message: impl Into<String>) -> RpcError {
    RpcError {
        code: code.to_string(),
        message: message.into(),
        data: None,
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// Named profiles, in creation order ("default" is implied).
    #[serde(default)]
    profiles: Vec<String>,
}

pub struct Profiles {
    file: PathBuf,
    home: PathBuf,
    named: Vec<String>,
}

impl Profiles {
    /// Reads `<home>/state.json`; a missing file means no named profiles.
    pub fn load(paths: &Paths) -> Result<Profiles> {
        let state = match std::fs::read(&paths.state) {
            Ok(bytes) => serde_json::from_slice::<State>(&bytes)
                .with_context(|| format!("parsing {}", paths.state.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", paths.state.display())),
        };
        let mut named: Vec<String> = Vec::new();
        for name in state.profiles {
            // A name that is not valid never reaches the filesystem; drop it.
            if valid_profile_name(&name) && name != DEFAULT_PROFILE && !named.contains(&name) {
                named.push(name);
            }
        }
        Ok(Profiles {
            file: paths.state.clone(),
            home: paths.home.clone(),
            named,
        })
    }

    /// "default" first, then the named profiles in creation order.
    pub fn list(&self) -> Vec<String> {
        std::iter::once(DEFAULT_PROFILE.to_string())
            .chain(self.named.iter().cloned())
            .collect()
    }

    pub fn exists(&self, name: &str) -> bool {
        name == DEFAULT_PROFILE || self.named.iter().any(|n| n == name)
    }

    pub fn create(&mut self, name: &str) -> Result<(), RpcError> {
        if !valid_profile_name(name) {
            return Err(rpc_error(
                codes::INVALID_PARAMS,
                "profile names are 1 to 32 characters of a-z, 0-9 and '-', not starting with '-'",
            ));
        }
        if self.exists(name) {
            return Err(rpc_error(
                codes::INVALID_PARAMS,
                format!("profile {name} already exists"),
            ));
        }
        self.named.push(name.to_string());
        if let Err(e) = self.save() {
            self.named.pop();
            return Err(rpc_error(
                codes::INTERNAL,
                format!("saving profiles: {e:#}"),
            ));
        }
        Ok(())
    }

    /// Removes a named profile from the registry. Its Chrome profile directory is left alone.
    pub fn delete(&mut self, name: &str, in_use: bool) -> Result<(), RpcError> {
        if name == DEFAULT_PROFILE {
            return Err(rpc_error(
                codes::INVALID_PARAMS,
                "the default profile cannot be deleted",
            ));
        }
        let Some(pos) = self.named.iter().position(|n| n == name) else {
            return Err(rpc_error(
                codes::PROFILE_NOT_FOUND,
                format!("no profile named {name}"),
            ));
        };
        if in_use {
            return Err(rpc_error(
                codes::PROFILE_IN_USE,
                format!("profile {name} has open sessions"),
            ));
        }
        let removed = self.named.remove(pos);
        if let Err(e) = self.save() {
            self.named.insert(pos, removed);
            return Err(rpc_error(
                codes::INTERNAL,
                format!("saving profiles: {e:#}"),
            ));
        }
        Ok(())
    }

    /// Writes the state atomically: a private temporary file, then a rename over the old one.
    fn save(&self) -> Result<()> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.home)
            .with_context(|| format!("creating {}", self.home.display()))?;
        let state = State {
            profiles: self.named.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&state)?;
        let tmp = self.file.with_extension("json.tmp");
        let _ = std::fs::remove_file(&tmp);
        let write = || -> Result<()> {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)
                .with_context(|| format!("creating {}", tmp.display()))?;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            std::fs::rename(&tmp, &self.file)
                .with_context(|| format!("replacing {}", self.file.display()))?;
            Ok(())
        };
        let r = write();
        if r.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pegboard_protocol::codes;

    #[test]
    fn profile_names_are_restricted() {
        for ok in ["default", "work", "a-1"] {
            assert!(valid_profile_name(ok), "{ok}");
        }
        for bad in ["", "Work", "a b", "../x", "-a", &"a".repeat(33)] {
            assert!(!valid_profile_name(bad), "{bad}");
        }
    }

    #[test]
    fn profile_dirs_never_collide_with_the_human_profile() {
        assert_eq!(profile_dir("default"), "PegboardAgent");
        assert_eq!(profile_dir("work"), "PegboardAgent-work");
        assert_ne!(profile_dir("default"), crate::browser::HUMAN_PROFILE_DIR);
    }

    #[test]
    fn default_profile_cannot_be_deleted_and_duplicates_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(tmp.path());
        let mut p = Profiles::load(&paths).unwrap();
        p.create("work").unwrap();
        assert_eq!(p.create("work").unwrap_err().code, codes::INVALID_PARAMS);
        assert_eq!(p.create("default").unwrap_err().code, codes::INVALID_PARAMS);
        assert_eq!(
            p.delete("default", false).unwrap_err().code,
            codes::INVALID_PARAMS
        );
        assert_eq!(
            p.delete("work", true).unwrap_err().code,
            codes::PROFILE_IN_USE
        );
        assert_eq!(
            p.delete("nope", false).unwrap_err().code,
            codes::PROFILE_NOT_FOUND
        );
        assert_eq!(p.list(), vec!["default", "work"]);
    }

    #[test]
    fn named_profiles_persist_in_a_private_state_file() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::paths::paths_in(tmp.path());
        let mut p = Profiles::load(&paths).unwrap();
        assert_eq!(p.list(), vec!["default"]);
        p.create("work").unwrap();
        p.create("b-2").unwrap();
        let mode = std::fs::metadata(&paths.state)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            Profiles::load(&paths).unwrap().list(),
            vec!["default", "work", "b-2"]
        );
        p.delete("work", false).unwrap();
        assert_eq!(
            Profiles::load(&paths).unwrap().list(),
            vec!["default", "b-2"]
        );
        assert!(!paths.state.with_extension("json.tmp").exists());
        // Names that are not valid never come back from the file.
        std::fs::write(&paths.state, r#"{"profiles":["../x","ok","Default","ok"]}"#).unwrap();
        assert_eq!(
            Profiles::load(&paths).unwrap().list(),
            vec!["default", "ok"]
        );
        assert_eq!(p.create("../evil").unwrap_err().code, codes::INVALID_PARAMS);
    }
}
