//! Tab groups and tabs in an agent profile. Stock Chrome does this through the bridge
//! extension's service worker (`ExtensionBridge`); the Pegboard fork has it built in as the
//! CDP domain `Pegboard.*` (`DomainBridge`).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use async_trait::async_trait;

pub mod domain;
pub mod extension;

pub use domain::DomainBridge;
pub use extension::ExtensionBridge;

/// Tab switches until this long after the bridge closed tabs are Chrome's doing (it
/// activates a neighbour of a closed tab), not a person's.
const CLOSE_QUIET: Duration = Duration::from_secs(2);

/// Told of every tab switch in the profile: (the profile's browser context, the target that
/// became active, when).
pub type ActivityFn = Arc<dyn Fn(&str, &str, SystemTime) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupId(pub i64);

/// The tab work of one agent profile. Every call refuses a tab outside the profile, and a
/// new tab's URL may only be `about:blank` or an absolute `http:`, `https:` or `data:` URL.
#[async_trait]
pub trait TabBridge: Send + Sync {
    /// A new group with one blank tab in it: (the group, that tab's target).
    async fn create_group(&self, title: &str, color: &str) -> Result<(GroupId, String)>;

    /// Makes a tab in `group`. When Chrome already deleted the group (it goes with its last
    /// tab) a new one is started with `title` and `color` and its id is returned with the
    /// target.
    async fn create_tab(
        &self,
        group: &GroupId,
        url: &str,
        title: &str,
        color: &str,
    ) -> Result<(String, Option<GroupId>)>;

    async fn add_to_group(&self, group: &GroupId, target_id: &str) -> Result<()>;

    async fn members(&self, group: &GroupId) -> Result<Vec<String>>;

    /// Closes the tabs. Tab switches within `CLOSE_QUIET` of the call are not reported to
    /// `follow_activity` (Chrome activates a neighbour of a closed tab).
    async fn close_targets(&self, target_ids: &[String]) -> Result<()>;

    /// Takes the tabs out of their groups; no other tab is touched.
    async fn ungroup_targets(&self, target_ids: &[String]) -> Result<()>;

    async fn ungroup(&self, group: &GroupId) -> Result<()>;

    /// Reports every tab switch in the profile to `on` from now on, while the bridge lives.
    async fn follow_activity(self: Arc<Self>, on: ActivityFn);

    /// Test hook (`test.detachBridge`): lets go of what keeps the bridge's other end running,
    /// if anything does. Only the extension bridge has something to let go of.
    async fn detach_for_test(&self) -> bool {
        false
    }
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
