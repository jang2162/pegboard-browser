//! Wire types shared by the pegboard daemon, CLI and clients (JSON-RPC 2.0 over a unix socket).

use rand::Rng;
use serde::{Deserialize, Serialize};

pub const JSONRPC: &str = "2.0";

/// Error code values carried in `RpcError::code`.
pub mod codes {
    pub const NOT_RUNNING: &str = "not_running";
    pub const INVALID_PARAMS: &str = "invalid_params";
    pub const UNKNOWN_METHOD: &str = "unknown_method";
    pub const SESSION_NOT_FOUND: &str = "session_not_found";
    pub const RECORDING_NOT_FOUND: &str = "recording_not_found";
    pub const PROFILE_NOT_FOUND: &str = "profile_not_found";
    pub const PROFILE_IN_USE: &str = "profile_in_use";
    pub const ARGV_REJECTED: &str = "argv_rejected";
    pub const TAB_GONE: &str = "tab_gone";
    pub const AGENT_BROWSER_MISSING: &str = "agent_browser_missing";
    pub const AGENT_BROWSER_VERSION: &str = "agent_browser_version";
    pub const BROWSER_FAILED: &str = "browser_failed";
    /// A command did not finish within its time limit (it was stopped).
    pub const TIMEOUT: &str = "timeout";
    /// The session's policy does not allow the command's action class.
    pub const POLICY_DENIED: &str = "policy_denied";
    /// A session asks for a profile-wide setting the profile is pinned to differently.
    pub const PROFILE_POLICY_CONFLICT: &str = "profile_policy_conflict";
    pub const LIMIT_EXCEEDED: &str = "limit_exceeded";
    pub const INTERNAL: &str = "internal";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: u64,
    pub result: Option<serde_json::Value>,
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

const ID_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
const ID_BODY_LEN: usize = 8;

/// "s_" followed by 8 lowercase base32 characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn generate() -> Self {
        let mut rng = rand::thread_rng();
        let body: String = (0..ID_BODY_LEN)
            .map(|_| ID_ALPHABET[rng.gen_range(0..ID_ALPHABET.len())] as char)
            .collect();
        SessionId(format!("s_{body}"))
    }

    pub fn parse(s: &str) -> Option<Self> {
        let body = s.strip_prefix("s_")?;
        let valid = body.len() == ID_BODY_LEN && body.bytes().all(|b| ID_ALPHABET.contains(&b));
        valid.then(|| SessionId(s.to_string()))
    }
}

/// The kind of action a command is, which a session policy allows or denies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionClass {
    Navigate,
    Click,
    Fill,
    Snapshot,
    Scroll,
    Wait,
    Read,
    Get,
    Interact,
    Eval,
    Download,
    Upload,
    State,
    Clipboard,
}

impl ActionClass {
    pub const ALL: [ActionClass; 14] = [
        ActionClass::Navigate,
        ActionClass::Click,
        ActionClass::Fill,
        ActionClass::Snapshot,
        ActionClass::Scroll,
        ActionClass::Wait,
        ActionClass::Read,
        ActionClass::Get,
        ActionClass::Interact,
        ActionClass::Eval,
        ActionClass::Download,
        ActionClass::Upload,
        ActionClass::State,
        ActionClass::Clipboard,
    ];

    /// The lower-case name used on the wire and in options.
    pub fn as_str(self) -> &'static str {
        match self {
            ActionClass::Navigate => "navigate",
            ActionClass::Click => "click",
            ActionClass::Fill => "fill",
            ActionClass::Snapshot => "snapshot",
            ActionClass::Scroll => "scroll",
            ActionClass::Wait => "wait",
            ActionClass::Read => "read",
            ActionClass::Get => "get",
            ActionClass::Interact => "interact",
            ActionClass::Eval => "eval",
            ActionClass::Download => "download",
            ActionClass::Upload => "upload",
            ActionClass::State => "state",
            ActionClass::Clipboard => "clipboard",
        }
    }
}

impl std::str::FromStr for ActionClass {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        ActionClass::ALL
            .into_iter()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| {
                let all: Vec<&str> = ActionClass::ALL.iter().map(|c| c.as_str()).collect();
                format!("{s:?} is not an action class (one of {})", all.join(", "))
            })
    }
}

/// What a session may do. Fixed when the session is created.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct SessionPolicy {
    /// Opens a class that is denied by default.
    pub allow: Vec<ActionClass>,
    /// Closes a class that is allowed by default.
    pub deny: Vec<ActionClass>,
    /// Empty means no allow-list.
    pub allow_domains: Vec<String>,
    pub allow_private_network: bool,
    pub no_localhost: bool,
    /// Absolute path, checked at `session.create`.
    pub upload_dir: Option<String>,
    pub no_expire: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: SessionId,
    pub name: String,
    pub profile: String,
    pub tab_group_id: i64,
    pub targets: Vec<String>,
    pub state: SessionState,
    pub recording_id: String,
    /// RFC 3339, UTC.
    pub created_at: String,
    /// RFC 3339, UTC.
    pub last_activity_at: String,
    /// Secret that authorizes commands in the session. Only the result of `session.create`
    /// carries it, once; it is never in `session.list` or `session.info`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default)]
    pub policy: SessionPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    Active,
    Closed,
}

/// Unknown fields are refused: a misspelt policy field must not become a wider session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCreateParams {
    pub name: Option<String>,
    pub url: Option<String>,
    pub profile: Option<String>,
    #[serde(default)]
    pub policy: SessionPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCloseParams {
    pub session_id: SessionId,
    pub keep_tabs: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecParams {
    pub session_id: SessionId,
    pub argv: Vec<String>,
    pub timeout_ms: Option<u64>,
    /// The session's token (see `SessionInfo::token`).
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecResult {
    pub exit_code: i32,
    pub output: serde_json::Value,
    pub stderr: String,
}

/// A file in a session's files directory (`files.list`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub name: String,
    pub bytes: u64,
    /// RFC 3339, UTC.
    pub modified_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileInfo {
    pub name: String,
    pub kind: ProfileKind,
    pub sessions: Vec<SessionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileKind {
    Default,
    Named,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub chrome_version: Option<String>,
    pub sessions: usize,
    /// Shutdown has begun: the daemon still answers but is about to go away.
    #[serde(default)]
    pub stopping: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_session_info() -> SessionInfo {
        SessionInfo {
            id: SessionId::generate(),
            name: "demo".into(),
            profile: "default".into(),
            tab_group_id: 7,
            targets: vec!["T1".into()],
            state: SessionState::Active,
            recording_id: "r_1".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            last_activity_at: "2026-01-01T00:00:00Z".into(),
            token: None,
            policy: SessionPolicy::default(),
        }
    }

    #[test]
    fn session_id_has_prefix_and_length() {
        let id = SessionId::generate();
        assert!(id.0.starts_with("s_"));
        assert_eq!(id.0.len(), 10);
        assert!(SessionId::parse(&id.0).is_some());
        assert!(SessionId::parse("../x").is_none());
        assert!(SessionId::parse("s_UPPER123").is_none());
    }

    #[test]
    fn token_is_left_out_unless_set() {
        let mut info = sample_session_info();
        assert!(serde_json::to_value(&info).unwrap().get("token").is_none());
        info.token = Some("t".into());
        assert_eq!(serde_json::to_value(&info).unwrap()["token"], "t");
    }

    #[test]
    fn session_info_uses_camel_case() {
        let json = serde_json::to_value(sample_session_info()).unwrap();
        assert!(json.get("tabGroupId").is_some());
        assert_eq!(json["state"], "active");
    }

    #[test]
    fn policy_defaults_and_names() {
        let p: SessionPolicy = serde_json::from_str("{}").unwrap();
        assert_eq!(p, SessionPolicy::default());
        let p: SessionPolicy =
            serde_json::from_str(r#"{"allow":["state"],"allowDomains":["a.test"]}"#).unwrap();
        assert_eq!(p.allow, vec![ActionClass::State]);
        for c in ActionClass::ALL {
            assert_eq!(c.as_str().parse::<ActionClass>(), Ok(c));
            assert_eq!(serde_json::to_value(c).unwrap(), c.as_str());
        }
        assert!("Eval".parse::<ActionClass>().is_err());
    }

    #[test]
    fn unknown_policy_fields_are_refused() {
        for raw in [
            r#"{"denny":["eval"]}"#,
            r#"{"allow_domains":["a.test"]}"#,
            r#"{"allowDomain":["a.test"]}"#,
        ] {
            assert!(serde_json::from_str::<SessionPolicy>(raw).is_err(), "{raw}");
        }
        for raw in [
            r#"{"policy":{"denny":["eval"]}}"#,
            r#"{"policy":{}, "polcy":{}}"#,
            r#"{"name":"x","profil":"p"}"#,
        ] {
            assert!(
                serde_json::from_str::<SessionCreateParams>(raw).is_err(),
                "{raw}"
            );
        }
        let ok: SessionCreateParams =
            serde_json::from_str(r#"{"policy":{"deny":["eval"]},"url":"http://a.test/"}"#).unwrap();
        assert_eq!(ok.policy.deny, vec![ActionClass::Eval]);
        assert!(serde_json::from_str::<SessionCreateParams>("{}").is_ok());
    }

    #[test]
    fn request_params_default_to_null() {
        let r: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"browser.status"}"#).unwrap();
        assert!(r.params.is_null());
    }
}
