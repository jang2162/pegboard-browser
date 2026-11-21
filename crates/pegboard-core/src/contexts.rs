//! Which browser contexts are agent profiles.
//!
//! An agent profile is a profile directory named exactly `PegboardAgent` or starting with
//! `PegboardAgent-` (`profiles::profile_dir`). Every other context is the person's: the
//! person may have several profiles, and a context this set does not know (or an empty
//! context id) counts as theirs. Nothing decides "human" by comparing to one id.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Whether `dir` is an agent profile's directory name.
pub fn is_agent_profile_dir(dir: &str) -> bool {
    dir == AGENT_PROFILE_DIR || dir.starts_with(AGENT_PROFILE_PREFIX)
}

const AGENT_PROFILE_DIR: &str = "PegboardAgent";
const AGENT_PROFILE_PREFIX: &str = "PegboardAgent-";

#[derive(Default)]
struct Maps {
    by_profile: HashMap<String, String>,
    by_context: HashMap<String, String>,
}

impl Maps {
    fn remove_context(&mut self, context: &str) {
        if let Some(profile) = self.by_context.remove(context) {
            self.by_profile.remove(&profile);
        }
    }
}

/// The browser contexts of the agent profiles that are loaded now, by profile directory.
#[derive(Clone, Default)]
pub struct AgentContexts {
    inner: Arc<RwLock<Maps>>,
}

impl AgentContexts {
    /// The agent profile `profile_dir` has the browser context `context` now. Its earlier
    /// context, if any, is forgotten. A directory that is not an agent profile's, or an
    /// empty context id, is not taken.
    pub fn set(&self, profile_dir: &str, context: &str) {
        if context.is_empty() || !is_agent_profile_dir(profile_dir) {
            return;
        }
        let mut m = self.inner.write().unwrap();
        if let Some(old) = m.by_profile.remove(profile_dir) {
            m.by_context.remove(&old);
        }
        m.remove_context(context);
        m.by_profile
            .insert(profile_dir.to_string(), context.to_string());
        m.by_context
            .insert(context.to_string(), profile_dir.to_string());
    }

    /// The context is gone (Chrome unloaded the profile): it counts as the person's again.
    pub fn forget_context(&self, context: &str) {
        self.inner.write().unwrap().remove_context(context);
    }

    /// Whether `context` is a loaded agent profile's. Empty and unknown ids are not.
    pub fn is_agent(&self, context: &str) -> bool {
        !context.is_empty() && self.inner.read().unwrap().by_context.contains_key(context)
    }

    /// The context of the agent profile `profile_dir`, when it is loaded.
    pub fn context_of(&self, profile_dir: &str) -> Option<String> {
        self.inner
            .read()
            .unwrap()
            .by_profile
            .get(profile_dir)
            .cloned()
    }

    /// The agent profile directory of `context`, when it is an agent context.
    pub fn profile_of(&self, context: &str) -> Option<String> {
        self.inner.read().unwrap().by_context.get(context).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_context_counts_as_human() {
        let a = AgentContexts::default();
        a.set("PegboardAgent", "A1");
        assert!(a.is_agent("A1"));
        assert!(!a.is_agent("X"));
        assert!(!a.is_agent(""));
    }

    #[test]
    fn a_reloaded_agent_profile_gets_its_new_context() {
        let a = AgentContexts::default();
        a.set("PegboardAgent", "C1");
        a.set("PegboardAgent", "C2");
        assert!(!a.is_agent("C1"));
        assert!(a.is_agent("C2"));
        assert_eq!(a.context_of("PegboardAgent").as_deref(), Some("C2"));
        assert_eq!(a.profile_of("C2").as_deref(), Some("PegboardAgent"));
        assert_eq!(a.profile_of("C1"), None);
    }

    #[test]
    fn agent_profile_dirs_are_recognized() {
        for yes in ["PegboardAgent", "PegboardAgent-work"] {
            assert!(is_agent_profile_dir(yes), "{yes}");
        }
        for no in [
            "Default",
            "Profile 1",
            "PegboardAgentX",
            "pegboardagent",
            "",
        ] {
            assert!(!is_agent_profile_dir(no), "{no}");
        }
    }

    #[test]
    fn only_agent_profiles_and_real_ids_are_taken() {
        let a = AgentContexts::default();
        a.set("Default", "H1");
        a.set("Profile 1", "H2");
        a.set("PegboardAgent-work", "");
        assert!(!a.is_agent("H1") && !a.is_agent("H2") && !a.is_agent(""));
        assert_eq!(a.context_of("PegboardAgent-work"), None);
    }

    #[test]
    fn a_forgotten_context_is_human_again() {
        let a = AgentContexts::default();
        a.set("PegboardAgent", "C1");
        a.set("PegboardAgent-work", "W1");
        a.forget_context("C1");
        assert!(!a.is_agent("C1"));
        assert_eq!(a.context_of("PegboardAgent"), None);
        assert!(a.is_agent("W1"));
    }

    #[test]
    fn a_context_moving_to_another_profile_leaves_the_first() {
        let a = AgentContexts::default();
        a.set("PegboardAgent", "C1");
        a.set("PegboardAgent-work", "C1");
        assert_eq!(a.context_of("PegboardAgent"), None);
        assert_eq!(a.profile_of("C1").as_deref(), Some("PegboardAgent-work"));
    }
}
