use crate::provider::devin::jsonc::strip_jsonc_comments;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
fn dirs_home() -> Option<String> {
    crate::accounts::home().map(|p| p.to_string_lossy().into_owned())
}
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
#[derive(Serialize)]
pub struct OpencodeGoUsageFetch {
    pub status: String,
    pub http_status: Option<u16>,
    pub body: Option<String>,
    pub error: Option<String>,
}
const OPENCODE_GO_USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";
fn opencode_go_result(
    status: &str,
    http_status: Option<u16>,
    body: Option<String>,
    error: Option<String>,
) -> OpencodeGoUsageFetch {
    OpencodeGoUsageFetch {
        status: status.into(),
        http_status,
        body,
        error,
    }
}

pub(super) fn fetch() -> Result<OpencodeGoUsageFetch, String> {
    let Some(api_key) = read_opencode_go_api_key() else {
        return Ok(opencode_go_result(
            "unavailable",
            None,
            None,
            Some("OpenCode Go not connected".into()),
        ));
    };
    let agent = ureq::AgentBuilder::new().timeout(HTTP_TIMEOUT).build();
    let result = agent
        .get(OPENCODE_GO_USAGE_URL)
        .set("Authorization", &format!("Bearer {api_key}"))
        .call();
    match result {
        Ok(response) => {
            let http_status = response.status();
            let body = response.into_string().unwrap_or_default();
            if (200..300).contains(&http_status) {
                Ok(opencode_go_result(
                    "ok",
                    Some(http_status),
                    Some(body),
                    None,
                ))
            } else {
                Ok(opencode_go_error(http_status))
            }
        }
        Err(ureq::Error::Status(status, response)) => {
            let _ = response.into_string();
            Ok(opencode_go_error(status))
        }
        Err(error) => Ok(opencode_go_result(
            "error",
            None,
            None,
            Some(format!("OpenCode Go usage request failed: {error}")),
        )),
    }
}

fn opencode_go_error(status: u16) -> OpencodeGoUsageFetch {
    // 403 means a valid key without a Go subscription — not a failure,
    // so the footer can hide the chip instead of showing an error.
    if status == 403 {
        return opencode_go_result(
            "unavailable",
            Some(status),
            None,
            Some("No OpenCode Go subscription".into()),
        );
    }
    let message = if status == 401 {
        "OpenCode Go sign-in expired".into()
    } else {
        format!("OpenCode Go usage request failed ({status})")
    };
    opencode_go_result("error", Some(status), None, Some(message))
}

/// Resolve the OpenCode data directory the same way OpenCode does:
/// `OPENCODE_DATA_DIR`, then `$XDG_DATA_HOME/<app>`, then the default
/// `~/.local/share/<app>`, where `<app>` is `OPENCODE_APPNAME` or "opencode".
fn opencode_data_dir() -> Option<PathBuf> {
    if let Some(dir) = env_var("OPENCODE_DATA_DIR") {
        return Some(PathBuf::from(dir));
    }
    let app = env_var("OPENCODE_APPNAME").unwrap_or_else(|| "opencode".into());
    if let Some(xdg) = env_var("XDG_DATA_HOME") {
        return Some(PathBuf::from(xdg).join(app));
    }
    let home = dirs_home()?;
    Some(PathBuf::from(home).join(".local/share").join(app))
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The Go key lives at `auth.json -> "opencode-go" -> "key"` inside the
/// OpenCode data directory. Resolution mirrors OpenCode's own precedence:
/// the `OPENCODE_AUTH_CONTENT` blob, then an explicit provider key in
/// opencode config, then stored credentials on disk. OpenCode 2 keeps
/// credentials in `opencode.db` instead of `auth.json`, so both are read.
fn read_opencode_go_api_key() -> Option<String> {
    // Env-injected auth blob is authoritative when it parses: a valid blob
    // without opencode-go means "no key", not "look elsewhere".
    if let Some(blob) = env_var("OPENCODE_AUTH_CONTENT") {
        if let Ok(value) = serde_json::from_str::<Value>(&blob) {
            if value.is_object() {
                return extract_opencode_go_key(&value);
            }
        }
    }
    if let Some(key) = read_opencode_config_api_key() {
        return Some(key);
    }
    let dir = opencode_data_dir()?;
    if let Ok(raw) = std::fs::read_to_string(dir.join("auth.json")) {
        if let Some(key) = extract_opencode_go_api_key(&raw) {
            return Some(key);
        }
    }
    read_opencode_credential(&dir.join("opencode.db"))
}

/// OpenCode 2 stores sign-ins in the `credential` table, one row per
/// integration, with the key serialized as `{"type":"key","key":"..."}`.
fn read_opencode_credential(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let value: String = conn
        .query_row(
            "SELECT value FROM credential \
             WHERE integration_id = 'opencode-go' AND COALESCE(active, 1) != 0 \
             ORDER BY time_updated DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok()?;
    credential_go_key(&value)
}

fn credential_go_key(raw: &str) -> Option<String> {
    let value: Value = serde_json::from_str(raw.trim()).ok()?;
    if let Some(key) = value.get("key").and_then(Value::as_str) {
        let key = key.trim();
        if !key.is_empty() {
            return Some(key.to_string());
        }
    }
    if let Some(key) = value.get("apiKey").and_then(Value::as_str) {
        let key = key.trim();
        if !key.is_empty() {
            return Some(key.to_string());
        }
    }
    // Older rows stored the bare token.
    value
        .as_str()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string)
}

/// Explicit `provider.options.apiKey` for the Go provider in opencode
/// config: `OPENCODE_CONFIG_CONTENT`, then `OPENCODE_CONFIG`, then the
/// global `opencode.json`. Only the Go provider IDs are considered so keys
/// for unrelated providers are never picked up.
fn read_opencode_config_api_key() -> Option<String> {
    if let Some(content) = env_var("OPENCODE_CONFIG_CONTENT") {
        if let Some(value) = parse_opencode_config(&content) {
            if let Some(key) = config_go_api_key(&value) {
                return Some(key);
            }
        }
    }
    opencode_config_paths()
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|raw| parse_opencode_config(&raw))
        .find_map(|value| config_go_api_key(&value))
}

fn opencode_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(custom) = env_var("OPENCODE_CONFIG") {
        paths.push(PathBuf::from(custom));
    }
    if let Some(xdg) = env_var("XDG_CONFIG_HOME") {
        let root = PathBuf::from(xdg).join("opencode");
        paths.push(root.join("opencode.jsonc"));
        paths.push(root.join("opencode.json"));
    }
    if let Some(home) = dirs_home() {
        let root = PathBuf::from(home).join(".config/opencode");
        paths.push(root.join("opencode.jsonc"));
        paths.push(root.join("opencode.json"));
    }
    paths
}

/// Parse OpenCode configuration using JSONC semantics: comments and trailing
/// commas are accepted, while ordinary JSON stays on serde_json's fast path.
fn parse_opencode_config(raw: &str) -> Option<Value> {
    serde_json::from_str(raw.trim()).ok().or_else(|| {
        let without_comments = strip_jsonc_comments(raw)?;
        let normalized = strip_jsonc_trailing_commas(&without_comments);
        serde_json::from_str(&normalized).ok()
    })
}

fn strip_jsonc_trailing_commas(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            ',' if matches!(
                chars.clone().find(|next| !next.is_whitespace()),
                Some('}' | ']')
            ) => {}
            _ => out.push(ch),
        }
    }
    out
}

fn config_go_api_key(value: &Value) -> Option<String> {
    let providers = value.get("provider")?.as_object()?;
    for id in ["opencode-go", "opencode"] {
        let Some(api_key) = providers
            .get(id)
            .and_then(|entry| entry.get("options"))
            .and_then(|options| options.get("apiKey"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|key| !key.is_empty())
        else {
            continue;
        };
        if let Some(var) = api_key
            .strip_prefix("{env:")
            .and_then(|rest| rest.strip_suffix('}'))
        {
            if let Some(resolved) = env_var(var) {
                return Some(resolved);
            }
            continue;
        }
        return Some(api_key.to_string());
    }
    None
}

fn extract_opencode_go_key(value: &Value) -> Option<String> {
    let key = value.get("opencode-go")?.get("key")?.as_str()?.trim();
    if key.is_empty() {
        None
    } else {
        Some(key.to_string())
    }
}

pub(crate) fn extract_opencode_go_api_key(raw: &str) -> Option<String> {
    let value: Value = serde_json::from_str(raw.trim()).ok()?;
    let key = value.get("opencode-go")?.get("key")?.as_str()?.trim();
    if key.is_empty() {
        None
    } else {
        Some(key.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential_db(path: &Path, rows: &[(&str, bool, i64)]) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE credential (
                id text PRIMARY KEY,
                integration_id text,
                label text NOT NULL,
                value text NOT NULL,
                connector_id text,
                method_id text,
                active integer,
                time_created integer NOT NULL,
                time_updated integer NOT NULL
            );",
        )
        .unwrap();
        for (ix, (key, active, updated)) in rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO credential \
                 (id, integration_id, label, value, active, time_created, time_updated) \
                 VALUES (?1, 'opencode-go', 'OpenCode Go', ?2, ?3, ?4, ?4)",
                (
                    format!("cred_{ix}"),
                    format!(r#"{{"type":"key","key":"{key}"}}"#),
                    *active as i64,
                    updated,
                ),
            )
            .unwrap();
        }
    }

    #[test]
    fn reads_the_go_key_from_the_opencode_credential_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        credential_db(
            &path,
            &[("oc_sk_first", true, 1), ("oc_sk_newest", true, 2)],
        );
        assert_eq!(
            read_opencode_credential(&path).as_deref(),
            Some("oc_sk_newest")
        );
    }

    #[test]
    fn skips_inactive_credentials_and_missing_databases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        credential_db(&path, &[("oc_sk_old", false, 2)]);
        assert_eq!(read_opencode_credential(&path), None);
        assert_eq!(
            read_opencode_credential(&dir.path().join("absent.db")),
            None
        );
    }

    #[test]
    fn accepts_every_credential_payload_shape() {
        assert_eq!(
            credential_go_key(r#"{"type":"key","key":" oc_sk_plain "}"#).as_deref(),
            Some("oc_sk_plain")
        );
        assert_eq!(
            credential_go_key(r#"{"apiKey":"oc_sk_camel"}"#).as_deref(),
            Some("oc_sk_camel")
        );
        assert_eq!(
            credential_go_key(r#""oc_sk_bare""#).as_deref(),
            Some("oc_sk_bare")
        );
        assert_eq!(credential_go_key(r#"{"key":"  "}"#), None);
        assert_eq!(credential_go_key("not json"), None);
    }
}
