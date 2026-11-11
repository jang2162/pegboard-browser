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
    pub const PROFILE_NOT_FOUND: &str = "profile_not_found";
    pub const PROFILE_IN_USE: &str = "profile_in_use";
    pub const ARGV_REJECTED: &str = "argv_rejected";
    pub const TAB_GONE: &str = "tab_gone";
    pub const AGENT_BROWSER_MISSING: &str = "agent_browser_missing";
    pub const AGENT_BROWSER_VERSION: &str = "agent_browser_version";
    pub const BROWSER_FAILED: &str = "browser_failed";
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    Active,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCreateParams {
    pub name: Option<String>,
    pub url: Option<String>,
    pub profile: Option<String>,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecResult {
    pub exit_code: i32,
    pub output: serde_json::Value,
    pub stderr: String,
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
    fn session_info_uses_camel_case() {
        let json = serde_json::to_value(sample_session_info()).unwrap();
        assert!(json.get("tabGroupId").is_some());
        assert_eq!(json["state"], "active");
    }

    #[test]
    fn request_params_default_to_null() {
        let r: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"browser.status"}"#).unwrap();
        assert!(r.params.is_null());
    }
}
