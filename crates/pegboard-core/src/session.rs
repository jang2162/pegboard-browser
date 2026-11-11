//! Sessions: one tab group in an agent profile's window plus one proxy endpoint.
//!
//! The manager keeps, for every session tab, a CDP session of its own on the daemon's
//! browser connection with focus emulation on, so a hidden tab stays responsive (phase 0
//! finding A1). It also follows the browser's target events on that connection, because a
//! session's proxy scope only learns about destroyed tabs and popups through a connected
//! client.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use pegboard_protocol::{
    codes, ProfileInfo, ProfileKind, RpcError, SessionCloseParams, SessionCreateParams, SessionId,
    SessionInfo, SessionState,
};
use serde_json::{json, Value};
use tokio::sync::{broadcast, OnceCell};
use tokio::task::JoinHandle;

use crate::bridge::{Bridge, GroupId};
use crate::browser::{Browser, BrowserConfig};
use crate::cdp::{page_targets, CdpConn, CdpEvent};
use crate::paths::Paths;
use crate::profiles::{profile_dir, rpc_error, valid_profile_name, Profiles, DEFAULT_PROFILE};
use crate::proxy::filter::normalize_url;
use crate::proxy::server::{Endpoint, ProxyServer};

const GROUP_COLORS: &[&str] = &[
    "grey", "blue", "red", "yellow", "green", "pink", "purple", "cyan", "orange",
];
const MAX_TITLE_CHARS: usize = 60;
const LOAD_TIMEOUT: Duration = Duration::from_secs(15);

/// Tab group title: the name, or the session id when the name is missing or blank; at most
/// 60 characters (not bytes), cut with '…'. Names are not unique and never identify a session.
pub fn group_title(name: Option<&str>, id: &SessionId) -> String {
    let name = match name {
        Some(n) if !n.trim().is_empty() => n,
        _ => return id.0.clone(),
    };
    if name.chars().count() <= MAX_TITLE_CHARS {
        return name.to_string();
    }
    let mut title: String = name.chars().take(MAX_TITLE_CHARS - 1).collect();
    title.push('…');
    title
}

/// Cycles grey, blue, red, yellow, green, pink, purple, cyan, orange.
pub fn group_color(index: usize) -> &'static str {
    GROUP_COLORS[index % GROUP_COLORS.len()]
}

/// An open agent profile: its browser context and the bridge that makes its tabs.
pub struct ProfileRuntime {
    pub context: String,
    pub bridge: Arc<Bridge>,
}

fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn browser_failed(e: anyhow::Error) -> RpcError {
    rpc_error(codes::BROWSER_FAILED, format!("{e:#}"))
}

fn not_found(id: &SessionId) -> RpcError {
    rpc_error(codes::SESSION_NOT_FOUND, format!("no session {}", id.0))
}

fn checked_url(url: &str) -> Result<String, RpcError> {
    normalize_url(url).ok_or_else(|| {
        rpc_error(
            codes::INVALID_PARAMS,
            "only http(s) URLs with a host, data: URLs and about:blank can be opened",
        )
    })
}

struct SessionRecord {
    /// False while `create` is still running: hidden from callers, but followed by the
    /// event watcher already.
    ready: bool,
    seq: u64,
    /// `targets` is not kept here; the proxy scope is the source of truth.
    info: SessionInfo,
    context: String,
    group: GroupId,
    bridge: Arc<Bridge>,
    endpoint: Endpoint,
}

#[derive(Default)]
struct Table {
    records: HashMap<SessionId, SessionRecord>,
    /// Every id handed out by this manager, so an id (and its recording id) is never reused.
    issued: HashSet<SessionId>,
    /// Sessions being created that have no record yet, with their profile. Together with
    /// `records` this is what keeps a profile in use.
    creating: HashMap<SessionId, String>,
    next_seq: u64,
}

impl Table {
    fn profile_in_use(&self, name: &str) -> bool {
        self.records.values().any(|r| r.info.profile == name)
            || self.creating.values().any(|p| p == name)
    }
}

/// A session id reserved by `create` in a profile; the profile stays in use until the
/// reservation is dropped (by then the session has a record, or never will).
struct Reservation {
    inner: Arc<Inner>,
    id: SessionId,
    seq: u64,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.inner.table.lock().unwrap().creating.remove(&self.id);
    }
}

struct Inner {
    browser: Arc<Browser>,
    proxy: Arc<ProxyServer>,
    /// Held while the registry changes or `create` checks a profile and reserves a session
    /// in it, so a profile cannot be deleted while a session is being created in it.
    profiles: tokio::sync::Mutex<Profiles>,
    paths: Paths,
    cfg: BrowserConfig,
    /// Profile name -> its runtime. The async lock also keeps two callers from opening
    /// the same profile's window twice.
    runtimes: tokio::sync::Mutex<HashMap<String, Arc<ProfileRuntime>>>,
    table: Mutex<Table>,
    /// Target -> the daemon's own CDP session holding focus emulation on it (`None` while
    /// it is being attached).
    focus: Mutex<HashMap<String, Option<String>>>,
    /// Target discovery on the daemon's browser connection, turned on once.
    discovery: OnceCell<()>,
}

pub struct SessionManager {
    inner: Arc<Inner>,
    watcher: JoinHandle<()>,
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        self.watcher.abort();
    }
}

impl SessionManager {
    /// Must be called inside a tokio runtime (it starts the browser event watcher).
    pub fn new(
        browser: Arc<Browser>,
        proxy: Arc<ProxyServer>,
        profiles: Profiles,
        paths: Paths,
        cfg: BrowserConfig,
    ) -> Self {
        let events = browser.conn.events();
        let inner = Arc::new(Inner {
            browser,
            proxy,
            profiles: tokio::sync::Mutex::new(profiles),
            paths,
            cfg,
            runtimes: tokio::sync::Mutex::new(HashMap::new()),
            table: Mutex::new(Table::default()),
            focus: Mutex::new(HashMap::new()),
            discovery: OnceCell::new(),
        });
        let weak = Arc::downgrade(&inner);
        inner.proxy.on_target_added(move |_, target_id| {
            if let Some(inner) = weak.upgrade() {
                let target_id = target_id.to_string();
                tokio::spawn(async move {
                    let _ = inner.ensure_focus(&target_id).await;
                });
            }
        });
        let watcher = tokio::spawn(watch(Arc::downgrade(&inner), events));
        SessionManager { inner, watcher }
    }

    pub async fn create(&self, p: SessionCreateParams) -> Result<SessionInfo, RpcError> {
        let inner = &self.inner;
        let profile = p.profile.as_deref().unwrap_or(DEFAULT_PROFILE).to_string();
        let reservation = self.reserve(&profile).await?;
        let (id, seq) = (reservation.id.clone(), reservation.seq);
        let url = p.url.as_deref().map(checked_url).transpose()?;
        inner.ensure_discovery().await?;
        let runtime = inner.runtime(&profile).await?;

        let title = group_title(p.name.as_deref(), &id);
        let color = group_color(usize::try_from(seq).unwrap_or(0));
        let (group, first) = runtime
            .bridge
            .create_group(&title, color)
            .await
            .map_err(browser_failed)?;
        // The id comes from the extension, not from the browser: it enters the scope only
        // once the browser confirms it is in the profile. If not, it is not touched; only
        // the group's members are closed (through the bridge, which refuses foreign tabs).
        let conn = &inner.browser.conn;
        if let Err(e) = require_context(conn, &first, &runtime.context).await {
            if let Ok(members) = runtime.bridge.members(&group).await {
                let _ = close_tabs(conn, &runtime.bridge, &members.into_iter().collect()).await;
            }
            return Err(e);
        }

        // From here on a failure must take back everything made so far.
        let endpoint = inner.proxy.register(
            &id,
            &runtime.context,
            group.clone(),
            runtime.bridge.clone(),
            vec![first.clone()],
        );
        let created_at = now();
        let info = SessionInfo {
            id: id.clone(),
            name: p.name.unwrap_or_default(),
            profile,
            tab_group_id: group.0,
            targets: Vec::new(),
            state: SessionState::Active,
            recording_id: format!("r_{}", &id.0[2..]),
            created_at: created_at.clone(),
            last_activity_at: created_at,
        };
        inner.table.lock().unwrap().records.insert(
            id.clone(),
            SessionRecord {
                ready: false,
                seq,
                info,
                context: runtime.context.clone(),
                group: group.clone(),
                bridge: runtime.bridge.clone(),
                endpoint,
            },
        );
        let setup = async {
            inner.ensure_focus(&first).await.map_err(browser_failed)?;
            if let Some(url) = &url {
                open_url(&inner.browser.conn, &first, url).await?;
            }
            Ok::<(), RpcError>(())
        };
        if let Err(e) = setup.await {
            inner.discard(&id, &runtime.bridge, &group, &first).await;
            return Err(e);
        }
        let mut table = inner.table.lock().unwrap();
        let record = table.records.get_mut(&id).ok_or_else(|| not_found(&id))?;
        record.ready = true;
        Ok(inner.info_of(record))
    }

    /// Checks the profile and reserves a new session id in it, in the critical section
    /// `delete_profile` uses.
    async fn reserve(&self, profile: &str) -> Result<Reservation, RpcError> {
        let profiles = self.inner.profiles.lock().await;
        check_profile(&profiles, profile)?;
        let mut table = self.inner.table.lock().unwrap();
        let id = loop {
            let id = SessionId::generate();
            if !table.issued.contains(&id) {
                break id;
            }
        };
        table.issued.insert(id.clone());
        table.creating.insert(id.clone(), profile.to_string());
        let seq = table.next_seq;
        table.next_seq += 1;
        Ok(Reservation {
            inner: self.inner.clone(),
            id,
            seq,
        })
    }

    /// Open sessions, oldest first.
    pub fn list(&self) -> Vec<SessionInfo> {
        let table = self.inner.table.lock().unwrap();
        let mut records: Vec<&SessionRecord> = table.records.values().filter(|r| r.ready).collect();
        records.sort_by_key(|r| r.seq);
        records.into_iter().map(|r| self.inner.info_of(r)).collect()
    }

    pub fn info(&self, id: &SessionId) -> Result<SessionInfo, RpcError> {
        let table = self.inner.table.lock().unwrap();
        let record = ready(&table, id)?;
        Ok(self.inner.info_of(record))
    }

    /// Closes the session's tabs (also tabs in its group the proxy never saw), or with
    /// `keep_tabs` only ungroups them, and drops its endpoint.
    pub async fn close(&self, p: SessionCloseParams) -> Result<SessionInfo, RpcError> {
        let inner = &self.inner;
        let id = &p.session_id;
        // Taken out first, so nothing else can use the session while it closes.
        let record = {
            let mut table = inner.table.lock().unwrap();
            ready(&table, id)?;
            table.records.remove(id).ok_or_else(|| not_found(id))?
        };
        let mut targets: BTreeSet<String> = inner.proxy.targets(id).into_iter().collect();
        // Clients go first, so an agent cannot make new tabs while the old ones close.
        inner.proxy.unregister(id);
        if let Ok(members) = record.bridge.members(&record.group).await {
            targets.extend(members);
        }
        let result = if p.keep_tabs {
            record.bridge.ungroup(&record.group).await
        } else {
            close_tabs(&inner.browser.conn, &record.bridge, &targets).await
        };
        // Tabs that stay open (kept, or not closed after all) leave focus emulation.
        for t in &targets {
            inner.release_focus(t).await;
        }
        result.map_err(|e| {
            rpc_error(
                codes::BROWSER_FAILED,
                format!("session {} closed, but its tabs may remain: {e:#}", id.0),
            )
        })?;
        let mut info = record.info;
        info.targets = targets.into_iter().collect();
        info.state = SessionState::Closed;
        Ok(info)
    }

    pub fn endpoint(&self, id: &SessionId) -> Result<Endpoint, RpcError> {
        let table = self.inner.table.lock().unwrap();
        Ok(ready(&table, id)?.endpoint.clone())
    }

    /// Opens a new tab in the session group. Used to recover after the last tab was closed by hand.
    pub async fn new_tab(&self, id: &SessionId, url: &str) -> Result<String, RpcError> {
        let inner = &self.inner;
        let (bridge, group, context) = {
            let table = inner.table.lock().unwrap();
            let r = ready(&table, id)?;
            (r.bridge.clone(), r.group.clone(), r.context.clone())
        };
        let url = checked_url(url)?;
        let (target, navigate) = match bridge.create_tab(&group, &url).await {
            Ok(target) => (target, false),
            // Chrome deleted the group together with its last tab: start a new one.
            Err(_) if bridge.members(&group).await.is_ok_and(|m| m.is_empty()) => {
                (inner.regroup(id, &bridge).await?, url != "about:blank")
            }
            Err(e) => return Err(browser_failed(e)),
        };
        let added = async {
            if navigate {
                open_url(&inner.browser.conn, &target, &url).await?;
            }
            require_context(&inner.browser.conn, &target, &context).await?;
            inner.ensure_focus(&target).await.map_err(browser_failed)?;
            if !inner.proxy.add_target(id, &target) {
                return Err(not_found(id));
            }
            Ok(())
        };
        if let Err(e) = added.await {
            inner.release_focus(&target).await;
            // Through the bridge, which refuses a tab outside its profile.
            let tabs = BTreeSet::from([target]);
            let _ = close_tabs(&inner.browser.conn, &bridge, &tabs).await;
            return Err(e);
        }
        Ok(target)
    }

    /// Opens a window of the profile (and its bridge) if it has none.
    pub async fn open_profile(&self, name: &str) -> Result<(), RpcError> {
        check_profile(&*self.inner.profiles.lock().await, name)?;
        self.inner.runtime(name).await.map(drop)
    }

    pub fn touch(&self, id: &SessionId) {
        let mut table = self.inner.table.lock().unwrap();
        if let Some(r) = table.records.get_mut(id).filter(|r| r.ready) {
            r.info.last_activity_at = now();
        }
    }

    /// Every profile with the ids of its open sessions (oldest first).
    pub async fn list_profiles(&self) -> Vec<ProfileInfo> {
        let names = self.inner.profiles.lock().await.list();
        let table = self.inner.table.lock().unwrap();
        let mut records: Vec<&SessionRecord> = table.records.values().filter(|r| r.ready).collect();
        records.sort_by_key(|r| r.seq);
        names
            .into_iter()
            .map(|name| ProfileInfo {
                sessions: records
                    .iter()
                    .filter(|r| r.info.profile == name)
                    .map(|r| r.info.id.clone())
                    .collect(),
                kind: profile_kind(&name),
                name,
            })
            .collect()
    }

    pub async fn create_profile(&self, name: &str) -> Result<ProfileInfo, RpcError> {
        self.inner.profiles.lock().await.create(name)?;
        Ok(ProfileInfo {
            name: name.to_string(),
            kind: profile_kind(name),
            sessions: Vec::new(),
        })
    }

    /// Refused with profile_in_use while a session uses the profile or is being created in it.
    pub async fn delete_profile(&self, name: &str) -> Result<(), RpcError> {
        let mut profiles = self.inner.profiles.lock().await;
        let in_use = self.inner.table.lock().unwrap().profile_in_use(name);
        profiles.delete(name, in_use)
    }

    /// Targets the daemon holds (or is attaching) a focus-emulation session on, sorted.
    #[doc(hidden)]
    pub fn focused_targets(&self) -> Vec<String> {
        let mut targets: Vec<String> = self.inner.focus.lock().unwrap().keys().cloned().collect();
        targets.sort();
        targets
    }
}

fn profile_kind(name: &str) -> ProfileKind {
    if name == DEFAULT_PROFILE {
        ProfileKind::Default
    } else {
        ProfileKind::Named
    }
}

/// Profile names come from callers: they are checked before any use.
fn check_profile(profiles: &Profiles, name: &str) -> Result<(), RpcError> {
    if valid_profile_name(name) && profiles.exists(name) {
        Ok(())
    } else {
        Err(rpc_error(
            codes::PROFILE_NOT_FOUND,
            format!("no profile named {name:?}"),
        ))
    }
}

/// The browser's word on which profile a tab is in; ids reported by the bridge are only
/// trusted after this.
async fn require_context(conn: &CdpConn, target: &str, context: &str) -> Result<(), RpcError> {
    let r = conn
        .send("Target.getTargetInfo", json!({ "targetId": target }), None)
        .await
        .map_err(browser_failed)?;
    if r.pointer("/targetInfo/browserContextId")
        .and_then(Value::as_str)
        == Some(context)
    {
        Ok(())
    } else {
        Err(rpc_error(
            codes::BROWSER_FAILED,
            "the bridge reported a tab that is not in the session's profile",
        ))
    }
}

fn ready<'a>(table: &'a Table, id: &SessionId) -> Result<&'a SessionRecord, RpcError> {
    table
        .records
        .get(id)
        .filter(|r| r.ready)
        .ok_or_else(|| not_found(id))
}

impl Inner {
    fn info_of(&self, record: &SessionRecord) -> SessionInfo {
        let mut info = record.info.clone();
        info.targets = self.proxy.targets(&info.id);
        info
    }

    async fn ensure_discovery(&self) -> Result<(), RpcError> {
        self.discovery
            .get_or_try_init(|| async {
                self.browser
                    .conn
                    .send(
                        "Target.setDiscoverTargets",
                        json!({ "discover": true }),
                        None,
                    )
                    .await
                    .map(drop)
            })
            .await
            .map(drop)
            .map_err(browser_failed)
    }

    /// The profile's runtime, opening its window and attaching its bridge on first use.
    /// A runtime whose context is gone (e.g. every window of the profile was closed and
    /// Chrome unloaded it) is replaced.
    async fn runtime(&self, name: &str) -> Result<Arc<ProfileRuntime>, RpcError> {
        let mut runtimes = self.runtimes.lock().await;
        let context = self
            .browser
            .open_profile_window(&self.paths, &self.cfg, &profile_dir(name))
            .await
            .map_err(browser_failed)?;
        if let Some(rt) = runtimes.get(name) {
            if rt.context == context {
                return Ok(rt.clone());
            }
        }
        let bridge = Bridge::attach(&self.browser.conn, &context)
            .await
            .map_err(browser_failed)?;
        let rt = Arc::new(ProfileRuntime {
            context,
            bridge: Arc::new(bridge),
        });
        runtimes.insert(name.to_string(), rt.clone());
        Ok(rt)
    }

    /// Attaches the daemon's own CDP session to the target with focus emulation on and
    /// keeps it attached for the life of the target. Without it a hidden tab takes about
    /// 5 s per click and produces no screencast frames.
    async fn ensure_focus(&self, target: &str) -> anyhow::Result<()> {
        {
            let mut focus = self.focus.lock().unwrap();
            if focus.contains_key(target) {
                return Ok(());
            }
            focus.insert(target.to_string(), None);
        }
        let conn = &self.browser.conn;
        let attached = async {
            let session = conn.attach(target).await?;
            let emulate = async {
                conn.send(
                    "Emulation.setFocusEmulationEnabled",
                    json!({ "enabled": true }),
                    Some(&session),
                )
                .await?;
                conn.send(
                    "Page.setWebLifecycleState",
                    json!({ "state": "active" }),
                    Some(&session),
                )
                .await
            };
            if let Err(e) = emulate.await {
                detach(conn, &session).await;
                return Err(e);
            }
            anyhow::Ok(session)
        };
        match attached.await {
            Ok(session) => {
                let kept = {
                    let mut focus = self.focus.lock().unwrap();
                    match focus.get_mut(target) {
                        Some(slot) => {
                            *slot = Some(session.clone());
                            true
                        }
                        // The target left meanwhile.
                        None => false,
                    }
                };
                if !kept {
                    detach(conn, &session).await;
                }
                Ok(())
            }
            Err(e) => {
                self.focus.lock().unwrap().remove(target);
                Err(e)
            }
        }
    }

    /// Gives the session a new tab group (with one blank tab) in place of a deleted one.
    async fn regroup(&self, id: &SessionId, bridge: &Bridge) -> Result<String, RpcError> {
        let (title, color) = {
            let table = self.table.lock().unwrap();
            let r = ready(&table, id)?;
            let color = group_color(usize::try_from(r.seq).unwrap_or(0));
            (group_title(Some(&r.info.name), id), color)
        };
        let (group, target) = bridge
            .create_group(&title, color)
            .await
            .map_err(browser_failed)?;
        let moved = {
            let mut table = self.table.lock().unwrap();
            match table.records.get_mut(id).filter(|r| r.ready) {
                Some(r) => {
                    r.group = group.clone();
                    r.info.tab_group_id = group.0;
                    true
                }
                None => false,
            }
        };
        if !moved {
            let _ = bridge.close_targets(&[target]).await;
            return Err(not_found(id));
        }
        self.proxy.set_group(id, group);
        Ok(target)
    }

    /// Ends focus emulation on a tab that stays open but leaves the session.
    async fn release_focus(&self, target: &str) {
        let session = self.focus.lock().unwrap().remove(target).flatten();
        if let Some(session) = session {
            detach(&self.browser.conn, &session).await;
        }
    }

    /// Takes back what a failed `create` made.
    async fn discard(&self, id: &SessionId, bridge: &Bridge, group: &GroupId, first: &str) {
        let mut targets: BTreeSet<String> = self.proxy.targets(id).into_iter().collect();
        targets.insert(first.to_string());
        self.proxy.unregister(id);
        self.table.lock().unwrap().records.remove(id);
        if let Ok(members) = bridge.members(group).await {
            targets.extend(members);
        }
        let _ = close_tabs(&self.browser.conn, bridge, &targets).await;
        let mut focus = self.focus.lock().unwrap();
        for t in &targets {
            focus.remove(t);
        }
    }

    /// Sessions and their browser contexts, including ones still being created.
    fn scopes(&self) -> Vec<(SessionId, String)> {
        let table = self.table.lock().unwrap();
        table
            .records
            .iter()
            .map(|(id, r)| (id.clone(), r.context.clone()))
            .collect()
    }

    fn on_event(self: &Arc<Self>, e: &CdpEvent) {
        if e.session_id.is_some() {
            return;
        }
        let str_at = |p: &str| e.params.pointer(p).and_then(Value::as_str);
        match e.method.as_str() {
            "Target.targetDestroyed" => {
                let Some(target) = str_at("/targetId") else {
                    return;
                };
                for (id, _) in self.scopes() {
                    self.proxy.remove_target(&id, target);
                }
                self.focus.lock().unwrap().remove(target);
            }
            "Target.targetCreated" | "Target.targetInfoChanged" => {
                if str_at("/targetInfo/type") != Some("page") {
                    return;
                }
                let (Some(target), Some(opener), Some(context)) = (
                    str_at("/targetInfo/targetId"),
                    str_at("/targetInfo/openerId"),
                    str_at("/targetInfo/browserContextId"),
                ) else {
                    return;
                };
                self.adopt(target, opener, context);
            }
            "Target.detachedFromTarget" => {
                // One of the daemon's focus sessions went away while its target may live on.
                let Some(session) = str_at("/sessionId") else {
                    return;
                };
                let target = {
                    let mut focus = self.focus.lock().unwrap();
                    let target = focus
                        .iter()
                        .find(|(_, s)| s.as_deref() == Some(session))
                        .map(|(t, _)| t.clone());
                    if let Some(t) = &target {
                        focus.remove(t);
                    }
                    target
                };
                if let Some(target) = target {
                    let in_scope = self
                        .scopes()
                        .iter()
                        .any(|(id, _)| self.proxy.targets(id).contains(&target));
                    if in_scope {
                        let inner = self.clone();
                        tokio::spawn(async move {
                            let _ = inner.ensure_focus(&target).await;
                        });
                    }
                }
            }
            _ => {}
        }
    }

    /// A popup of a session tab in the session's context joins that session (the same
    /// rule the proxy applies for a connected client).
    fn adopt(&self, target: &str, opener: &str, context: &str) {
        for (id, scope_context) in self.scopes() {
            if scope_context == context && self.proxy.targets(&id).iter().any(|t| t == opener) {
                // Calls `on_target_added`, which starts focus emulation.
                self.proxy.add_target(&id, target);
            }
        }
    }

    /// Re-reads the browser's targets after events were missed.
    async fn reconcile(self: &Arc<Self>) {
        let Ok(pages) = page_targets(&self.browser.conn).await else {
            return;
        };
        let live: HashSet<&str> = pages.iter().map(|t| t.target_id.as_str()).collect();
        for (id, _) in self.scopes() {
            for t in self.proxy.targets(&id) {
                if !live.contains(t.as_str()) {
                    self.proxy.remove_target(&id, &t);
                    self.focus.lock().unwrap().remove(&t);
                }
            }
        }
        // Popups of popups are adopted once their opener is.
        loop {
            let before: usize = self
                .scopes()
                .iter()
                .map(|(id, _)| self.proxy.targets(id).len())
                .sum();
            for t in &pages {
                if let (Some(opener), Some(context)) = (&t.opener_id, &t.browser_context_id) {
                    self.adopt(&t.target_id, opener, context);
                }
            }
            let after: usize = self
                .scopes()
                .iter()
                .map(|(id, _)| self.proxy.targets(id).len())
                .sum();
            if after == before {
                break;
            }
        }
        // A session tab whose focus emulation is missing (e.g. its attach failed) gets it.
        for (id, _) in self.scopes() {
            for t in self.proxy.targets(&id) {
                if !self.focus.lock().unwrap().contains_key(&t) {
                    let inner = self.clone();
                    tokio::spawn(async move {
                        let _ = inner.ensure_focus(&t).await;
                    });
                }
            }
        }
    }
}

/// Follows the daemon's browser connection for as long as the manager lives.
async fn watch(inner: Weak<Inner>, mut events: broadcast::Receiver<CdpEvent>) {
    loop {
        let event = events.recv().await;
        let Some(inner) = inner.upgrade() else {
            return;
        };
        match event {
            Ok(e) => inner.on_event(&e),
            Err(broadcast::error::RecvError::Lagged(_)) => inner.reconcile().await,
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

async fn detach(conn: &CdpConn, session: &str) {
    let _ = conn
        .send(
            "Target.detachFromTarget",
            json!({ "sessionId": session }),
            None,
        )
        .await;
}

/// Navigates the tab on a CDP session of its own (so the focus session keeps its state)
/// and waits for the load event. A page that loads slowly is not an error; a navigation
/// that fails is.
async fn open_url(conn: &CdpConn, target: &str, url: &str) -> Result<(), RpcError> {
    let session = conn.attach(target).await.map_err(browser_failed)?;
    let navigate = async {
        let mut events = conn.events();
        conn.send("Page.enable", json!({}), Some(&session))
            .await
            .map_err(browser_failed)?;
        let r = conn
            .send("Page.navigate", json!({ "url": url }), Some(&session))
            .await
            .map_err(browser_failed)?;
        if let Some(err) = r.get("errorText").and_then(Value::as_str) {
            return Err(rpc_error(
                codes::INVALID_PARAMS,
                format!("opening {url} failed: {err}"),
            ));
        }
        let loaded = async {
            loop {
                match events.recv().await {
                    Ok(e)
                        if e.method == "Page.loadEventFired"
                            && e.session_id.as_deref() == Some(session.as_str()) =>
                    {
                        return;
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                    _ => {}
                }
            }
        };
        let _ = tokio::time::timeout(LOAD_TIMEOUT, loaded).await;
        Ok(())
    };
    let r = navigate.await;
    detach(conn, &session).await;
    r
}

/// Closes the tabs that still exist. The bridge refuses a whole call when one id is
/// unknown, and a tab can disappear at any moment, so the live set is read right before
/// and the call is retried once.
async fn close_tabs(
    conn: &CdpConn,
    bridge: &Bridge,
    targets: &BTreeSet<String>,
) -> anyhow::Result<()> {
    let mut last = Ok(());
    for _ in 0..2 {
        let live: HashSet<String> = page_targets(conn)
            .await?
            .into_iter()
            .map(|t| t.target_id)
            .collect();
        let open: Vec<String> = targets
            .iter()
            .filter(|t| live.contains(*t))
            .cloned()
            .collect();
        if open.is_empty() {
            return Ok(());
        }
        last = bridge.close_targets(&open).await;
        if last.is_ok() {
            return Ok(());
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;
    use pegboard_protocol::SessionId;

    #[test]
    fn group_title_handles_odd_names() {
        let id = SessionId("s_abcdefgh".into());
        assert_eq!(group_title(None, &id), "s_abcdefgh");
        assert_eq!(group_title(Some("   "), &id), "s_abcdefgh");
        assert_eq!(group_title(Some("가격 조사 🔎"), &id), "가격 조사 🔎");
        let long = group_title(Some(&"가".repeat(200)), &id);
        assert_eq!(long.chars().count(), 60);
        assert!(long.ends_with('…'));
        assert_eq!(group_title(Some(&"a".repeat(60)), &id), "a".repeat(60));
    }

    #[test]
    fn group_colors_cycle() {
        assert_eq!(group_color(0), "grey");
        assert_eq!(group_color(8), "orange");
        assert_eq!(group_color(9), "grey");
        assert_eq!(group_color(10), "blue");
    }
}
