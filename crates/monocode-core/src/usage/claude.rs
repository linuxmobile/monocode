use super::{now_ms, parse_claude, FetchError, Snapshot};
use serde_json::Value;
use std::{io::Read, path::PathBuf, time::Duration};

pub(super) fn fetch_for(profile: &crate::accounts::Profile) -> Result<Snapshot, FetchError> {
    let config = profile
        .directory(crate::provider::Kind::Claude)
        .map_err(FetchError::failed)?
        .or_else(config_dir);
    let credentials = credentials(config).ok_or_else(|| {
        FetchError::unavailable("Sign in with claude auth login, then refresh usage.")
    })?;
    let oauth = credentials.get("claudeAiOauth").unwrap_or(&credentials);
    let token = oauth["accessToken"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| FetchError::unavailable("Claude is not signed in."))?;
    let expiry = oauth["expiresAt"]
        .as_i64()
        .or_else(|| oauth["expiresAt"].as_str()?.parse::<i64>().ok());
    // The CLI owns its rotating refresh token. Never refresh or write it here.
    let expired = || expired_error(oauth);
    if expiry.is_some_and(|at| at <= now_ms()) {
        return Err(expired());
    }
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build()
        .get("https://api.anthropic.com/api/oauth/usage")
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-beta", "oauth-2025-04-20")
        .set("User-Agent", "claude-code/2.1.0")
        .call()
        .map_err(|error| match error {
            ureq::Error::Status(401, _) => expired(),
            ureq::Error::Status(403, _) => {
                FetchError::unavailable("Claude usage is unavailable for this account.")
            }
            ureq::Error::Status(status, _) => {
                FetchError::failed(format!("Claude usage request failed ({status})."))
            }
            _ => FetchError::failed("Could not reach Claude usage. Try refreshing again."),
        })?;
    let mut body = String::new();
    response
        .into_reader()
        .take(1024 * 1024)
        .read_to_string(&mut body)
        .map_err(|_| FetchError::failed("Could not read Claude usage response."))?;
    let value = serde_json::from_str(&body)
        .map_err(|_| FetchError::failed("Claude usage response was not JSON."))?;
    parse_claude(&value)
}

/// The access token lasts about 8 hours. While a refresh token is present the
/// CLI renews it on its next run, so that is not a sign-out: a failed (not
/// unavailable) result keeps the last usage and hides the sign-in panel.
fn expired_error(oauth: &Value) -> FetchError {
    if oauth["refreshToken"]
        .as_str()
        .is_some_and(|s| !s.trim().is_empty())
    {
        FetchError::failed("Claude usage updates after your next Claude message.")
    } else {
        FetchError::unavailable("Claude sign-in expired. Run claude auth login, then refresh.")
    }
}

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}
fn credentials(config: Option<PathBuf>) -> Option<Value> {
    // A blank credentials file (the CLI clears it when a refresh fails) means
    // signed out; an old file must not stand in for it, or the footer would
    // promise a renewal with a long-dead refresh token and hide Sign in.
    let dir = config
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))?;
    let raw = std::fs::read_to_string(dir.join(".credentials.json")).ok()?;
    credential_blob(&raw)
}
fn credential_blob(raw: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(raw.trim()).ok()?;
    value.get("claudeAiOauth").unwrap_or(&value)["accessToken"]
        .as_str()
        .filter(|s| !s.trim().is_empty())?;
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_expired_access_token_with_a_refresh_token_is_not_a_sign_out() {
        let renewable = serde_json::json!({"accessToken":"a","refreshToken":"r","expiresAt":1});
        assert!(!expired_error(&renewable).unavailable);
        // Claude Code blanks both tokens when a refresh fails.
        let wiped = serde_json::json!({"accessToken":"","refreshToken":"","expiresAt":0});
        assert!(expired_error(&wiped).unavailable);
        assert!(expired_error(&serde_json::json!({"accessToken":"a"})).unavailable);
    }
    #[test]
    fn accepts_nested_and_legacy_credentials_without_logging_secrets() {
        assert!(
            credential_blob(r#"{"claudeAiOauth":{"accessToken":"test","expiresAt":123}}"#)
                .is_some()
        );
        assert!(credential_blob(r#"{"accessToken":"test"}"#).is_some());
        assert!(credential_blob(r#"{"accessToken":" "}"#).is_none());
        assert!(credential_blob("invalid").is_none());
    }
}
