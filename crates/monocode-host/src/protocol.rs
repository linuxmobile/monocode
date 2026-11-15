//! The host wire protocol.
//!
//! Version 1 is already spoken by `monocode_core::remote`, so these types are
//! shaped by that client rather than invented here. A request is
//! `{version, environmentId, method, params}` and a reply is `{result}` or
//! `{error}`.

use serde::{Deserialize, Serialize};

pub const HOST_PROTOCOL_VERSION: u32 = 1;

/// Largest request accepted. A 1 MiB text file can expand to 6 MiB once JSON
/// escapes control characters, and `files.write` sends two copies.
pub const MAX_BODY: usize = 16 * 1024 * 1024;

pub const REMOTE_PROVIDERS: [&str; 11] = [
    "codex",
    "claude",
    "cursor",
    "grok",
    "opencode",
    "pi",
    "omp",
    "fx",
    "hermes",
    "antigravity",
    "devin",
];

/// An inbound RPC call. `environment_id` is absent for
/// `environment.describe`, which is how a client discovers the host it reached.
#[derive(Debug, Clone, Deserialize)]
pub struct RpcRequest {
    pub version: u32,
    #[serde(default, rename = "environmentId")]
    pub environment_id: Option<String>,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct RpcSuccess {
    pub result: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub error: String,
}

/// What the desktop receives first after connecting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostDescriptor {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: u32,
    #[serde(rename = "environmentId")]
    pub environment_id: String,
    pub name: String,
    pub providers: Vec<String>,
    pub capabilities: Vec<String>,
}

/// Body of a `/lifecycle` call: `{"action": "status" | "stop"}`.
#[derive(Debug, Deserialize)]
pub struct LifecycleRequest {
    pub action: String,
}

/// Errors that map onto a specific HTTP status. Everything else is a 400, as
/// the TypeScript host did.
#[derive(Debug)]
pub enum RpcFailure {
    /// 403 — wrong route, present `Origin`, or not a POST.
    Forbidden(String),
    /// 401 — unknown or revoked credential.
    Unauthorized(String),
    /// 400 — anything the caller can fix.
    BadRequest(String),
}

impl RpcFailure {
    pub fn message(&self) -> &str {
        match self {
            Self::Forbidden(m) | Self::Unauthorized(m) | Self::BadRequest(m) => m,
        }
    }

    /// The token in `Authorization: Bearer …`, validated by shape.
    pub fn bearer(header: Option<&str>) -> Option<String> {
        let value = header?.strip_prefix("Bearer ")?;
        let valid = value.len() == 43
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        valid.then(|| value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bearer_token_must_be_43_url_safe_characters() {
        let token = "a".repeat(43);
        assert_eq!(
            RpcFailure::bearer(Some(&format!("Bearer {token}"))),
            Some(token.clone())
        );
        assert_eq!(RpcFailure::bearer(Some("Bearer short")), None);
        assert_eq!(RpcFailure::bearer(Some(&format!("Basic {token}"))), None);
        assert_eq!(RpcFailure::bearer(None), None);
        let pinned = format!("Bearer {}", "a".repeat(42));
        assert_eq!(RpcFailure::bearer(Some(&pinned)), None);
        let bad = format!("Bearer {}!", "a".repeat(42));
        assert_eq!(RpcFailure::bearer(Some(&bad)), None);
    }

    #[test]
    fn a_request_without_environment_is_accepted_for_describe() {
        let request: RpcRequest =
            serde_json::from_str(r#"{"version":1,"method":"environment.describe","params":{}}"#)
                .unwrap();
        assert_eq!(request.version, 1);
        assert!(request.environment_id.is_none());
        assert_eq!(request.method, "environment.describe");
    }

    #[test]
    fn params_default_to_null() {
        let request: RpcRequest =
            serde_json::from_str(r#"{"version":1,"method":"projects.list"}"#).unwrap();
        assert!(request.params.is_null());
    }
}
