//! Signs packaged artifacts and writes the update manifest.
//!
//! The manifest is what an installed app fetches to learn about a new version,
//! so it is signed too: an unsigned manifest would let anyone with write access
//! to storage redirect updates. The signing key never leaves the environment.

use std::path::Path;

use crate::package::preview_version;

/// The platforms a native release must cover, in manifest order.
pub const TARGETS: [&str; 3] = [
    "linux-x86_64-appimage",
    "linux-x86_64-deb",
    "linux-x86_64-rpm",
];

/// Builds the signed-payload manifest.
///
/// Every check here fails closed: a missing signature, a duplicate target or a
/// non-HTTPS base would produce a manifest that silently does not update.
pub fn manifest(
    version: &str,
    base_url: &str,
    artifacts: &[serde_json::Value],
    notes: &str,
) -> Result<serde_json::Value, String> {
    preview_version(version)?;
    let base = Url::parse(base_url)?;
    let mut platforms = serde_json::Map::new();
    for artifact in artifacts {
        let filename = artifact
            .get("filename")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "An artifact has no filename".to_string())?;
        // A path here would let the manifest point outside the release prefix.
        if filename.contains('/') || filename.contains('\\') || filename == ".." {
            return Err("An artifact must be a plain filename".into());
        }
        let target = match artifact.get("target").and_then(serde_json::Value::as_str) {
            Some(target) => target,
            // Unsigned extras are published but not offered as updates.
            None => continue,
        };
        if !TARGETS.contains(&target) {
            return Err(format!("Unexpected native target: {target}"));
        }
        if platforms.contains_key(target) {
            return Err(format!("Duplicate native target: {target}"));
        }
        let signature = artifact
            .get("signature")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|signature| !signature.is_empty())
            .ok_or_else(|| format!("Missing signature for {target}"))?;
        platforms.insert(
            target.to_string(),
            serde_json::json!({
                "url": format!(
                    "{}/native-preview/{version}/{}",
                    base.trimmed(),
                    encode(filename)
                ),
                "signature": signature,
            }),
        );
    }
    for target in TARGETS {
        if !platforms.contains_key(target) {
            return Err(format!("Missing native package: {target}"));
        }
    }
    Ok(serde_json::json!({
        "app_id": "dev.usemono.gpui",
        "channel": "native-preview",
        "version": version,
        "notes": notes,
        "platforms": platforms,
    }))
}

/// Just enough URL parsing to enforce that a download base is safe.
struct Url {
    scheme: String,
    remainder: String,
}

impl Url {
    fn parse(value: &str) -> Result<Self, String> {
        let (scheme, remainder) = value
            .split_once("://")
            .ok_or_else(|| "The native download base must be an absolute HTTPS URL".to_string())?;
        if scheme != "https" {
            return Err(
                "The native download base must be an HTTPS URL without credentials, query or fragment"
                    .into(),
            );
        }
        // Credentials, a query or a fragment would be copied into the manifest
        // and could leak a secret to any client that reads it.
        if remainder.contains('@')
            || remainder.contains('?')
            || remainder.contains('#')
            || remainder.is_empty()
        {
            return Err(
                "The native download base must be an HTTPS URL without credentials, query or fragment"
                    .into(),
            );
        }
        Ok(Self {
            scheme: scheme.to_string(),
            remainder: remainder.to_string(),
        })
    }

    fn trimmed(&self) -> String {
        format!("{}://{}", self.scheme, self.remainder.trim_end_matches('/'))
    }
}

/// Percent-encodes a filename for use in a URL path.
///
/// Only the characters that are legal unescaped in a path segment are kept, so
/// a `+` or a space cannot change which object a client fetches.
fn encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The signing key keeps its historic name so existing secrets stay valid,
/// with the native name taking precedence.
fn signing_key() -> String {
    std::env::var("MONOCODE_NATIVE_SIGNING_PRIVATE_KEY")
        .or_else(|_| std::env::var("TAURI_SIGNING_PRIVATE_KEY"))
        .unwrap_or_default()
}

fn update_public_key() -> String {
    std::env::var("MONOCODE_NATIVE_UPDATER_PUBKEY")
        .or_else(|_| std::env::var("TAURI_UPDATER_PUBKEY"))
        .unwrap_or_default()
}

/// Signs one file, returning the detached signature.
fn sign(path: &Path) -> Result<String, String> {
    let key = signing_key();
    let password = std::env::var("MONOCODE_NATIVE_SIGNING_KEY_PASSWORD")
        .or_else(|_| std::env::var("TAURI_SIGNING_PRIVATE_KEY_PASSWORD"))
        .unwrap_or_default();
    let output = std::process::Command::new("cargo")
        .args([
            "run",
            "--quiet",
            "--release",
            "--package",
            "native-sign",
            "--",
        ])
        .arg(path)
        .env("CI", "true")
        .env("MONOCODE_NATIVE_SIGNING_PRIVATE_KEY", key)
        .env("MONOCODE_NATIVE_SIGNING_KEY_PASSWORD", password)
        .env("MONOCODE_NATIVE_UPDATER_PUBKEY", update_public_key())
        .output()
        .map_err(|e| format!("Could not run the signer: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Could not sign {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let signature = path.with_extension(format!(
        "{}sig",
        path.extension()
            .map(|extension| format!("{}.", extension.to_string_lossy()))
            .unwrap_or_default()
    ));
    std::fs::read_to_string(&signature)
        .map(|text| text.trim().to_string())
        .map_err(|e| format!("read {}: {e}", signature.display()))
}

/// Signs every artifact and writes the manifest, returning the published files.
pub fn prepare(root: &Path, directory: &Path) -> Result<Vec<String>, String> {
    let version = std::env::var("MONOCODE_NATIVE_VERSION")
        .map_err(|_| "MONOCODE_NATIVE_VERSION is required".to_string())?;
    preview_version(&version)?;
    if signing_key().is_empty() || update_public_key().is_empty() {
        return Err("The native signing key and update public key are required".into());
    }
    let mut records = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(directory)
        .map_err(|e| format!("read {}: {e}", directory.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .map(|name| {
                    let name = name.to_string_lossy();
                    name.starts_with("artifacts-") && name.ends_with(".json")
                })
                .unwrap_or(false)
        })
        .collect();
    // Directory order is arbitrary; sorting keeps the manifest deterministic so
    // an unchanged release produces an identical payload.
    entries.sort();
    for entry in entries {
        let text = std::fs::read_to_string(&entry)
            .map_err(|e| format!("read {}: {e}", entry.display()))?;
        let records_in_file: Vec<serde_json::Value> =
            serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", entry.display()))?;
        for mut record in records_in_file {
            let filename = record
                .get("filename")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "An artifact has no filename".to_string())?
                .to_string();
            if filename.contains('/') || filename.contains('\\') {
                return Err("Invalid artifact filename".into());
            }
            if files.contains(&filename) {
                return Err(format!("Duplicate native artifact: {filename}"));
            }
            let path = directory.join(&filename);
            files.push(filename.clone());
            if record
                .get("target")
                .and_then(serde_json::Value::as_str)
                .is_some()
            {
                let signature = sign(&path)?;
                record["signature"] = serde_json::Value::String(signature);
                files.push(format!("{filename}.sig"));
            }
            records.push(record);
        }
    }
    let notes = match std::env::var("NATIVE_RELEASE_NOTES") {
        Ok(notes) => notes,
        Err(_) => std::fs::read_to_string(root.join("docs/native/preview-release-notes.md"))
            .map_err(|e| format!("read the release notes: {e}"))?,
    };
    let base_url = std::env::var("R2_PUBLIC_BASE_URL")
        .map_err(|_| "R2_PUBLIC_BASE_URL is required".to_string())?;
    let manifest = manifest(&version, &base_url, &records, &notes)?;

    // The payload is signed in base64 so the updater verifies exactly the bytes
    // it parses, not a re-serialised copy.
    let payload = base64(manifest.to_string().as_bytes());
    let payload_path = directory.join("native-manifest.payload");
    std::fs::write(&payload_path, payload.as_bytes())
        .map_err(|e| format!("write {}: {e}", payload_path.display()))?;
    let signature = sign(&payload_path)?;
    std::fs::write(
        directory.join("latest.json"),
        serde_json::json!({ "payload": payload, "signature": signature }).to_string(),
    )
    .map_err(|e| format!("write latest.json: {e}"))?;
    std::fs::write(
        directory.join("release-files.json"),
        serde_json::to_string(&files).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write release-files.json: {e}"))?;
    Ok(files)
}

/// Standard base64, written here because the only use is one encoding and a
/// dependency for it would outlast the reason it was added.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let value = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(value >> 18) as usize & 0x3f] as char);
        out.push(TABLE[(value >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(value >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[value as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(
        filename: &str,
        target: Option<&str>,
        signature: Option<&str>,
    ) -> serde_json::Value {
        let mut value = serde_json::json!({ "filename": filename });
        if let Some(target) = target {
            value["target"] = serde_json::Value::String(target.into());
        }
        if let Some(signature) = signature {
            value["signature"] = serde_json::Value::String(signature.into());
        }
        value
    }

    fn complete() -> Vec<serde_json::Value> {
        TARGETS
            .iter()
            .map(|target| artifact("a.bin", Some(target), Some("sig")))
            .collect()
    }

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn filenames_are_escaped_so_a_client_fetches_the_named_object() {
        // A `+` would decode to a space in some clients.
        assert_eq!(encode("a b+c"), "a%20b%2Bc");
        assert_eq!(encode("plain-1.0_x.deb"), "plain-1.0_x.deb");
    }

    #[test]
    fn the_update_base_must_be_https() {
        assert!(Url::parse("http://example.com").is_err());
        assert!(Url::parse("ftp://example.com").is_err());
        assert!(Url::parse("example.com").is_err());
    }

    #[test]
    fn a_base_with_credentials_or_a_query_is_rejected() {
        // These would be copied into the manifest, leaking a secret.
        for base in [
            "https://user:pass@example.com",
            "https://example.com?a=b",
            "https://example.com#x",
            "https://",
        ] {
            assert!(Url::parse(base).is_err(), "{base} was accepted");
        }
    }

    #[test]
    fn a_trailing_slash_is_removed_from_the_base() {
        assert_eq!(
            Url::parse("https://example.com/dist/").unwrap().trimmed(),
            "https://example.com/dist"
        );
    }

    #[test]
    fn a_complete_set_of_artifacts_becomes_a_manifest() {
        let manifest = manifest(
            "1.2.3-preview.4",
            "https://example.com",
            &complete(),
            "notes",
        )
        .unwrap();
        assert_eq!(manifest["version"], "1.2.3-preview.4");
        assert_eq!(manifest["channel"], "native-preview");
        for target in TARGETS {
            assert_eq!(
                manifest["platforms"][target]["url"],
                "https://example.com/native-preview/1.2.3-preview.4/a.bin"
            );
        }
    }

    #[test]
    fn a_missing_target_fails_the_release() {
        let mut artifacts = complete();
        artifacts.pop();
        let error = manifest("1.2.3-preview.4", "https://example.com", &artifacts, "").unwrap_err();
        assert!(error.contains("Missing native package"), "got: {error}");
    }

    #[test]
    fn a_missing_signature_fails_the_release() {
        let mut artifacts = complete();
        artifacts[0]["signature"] = serde_json::Value::String("  ".into());
        let error = manifest("1.2.3-preview.4", "https://example.com", &artifacts, "").unwrap_err();
        assert!(error.contains("Missing signature"), "got: {error}");
    }

    #[test]
    fn a_duplicate_target_is_rejected() {
        let mut artifacts = complete();
        artifacts.push(artifact("b.bin", Some(TARGETS[0]), Some("sig")));
        let error = manifest("1.2.3-preview.4", "https://example.com", &artifacts, "").unwrap_err();
        assert!(error.contains("Duplicate"), "got: {error}");
    }

    #[test]
    fn an_unexpected_target_is_rejected() {
        let mut artifacts = complete();
        artifacts.push(artifact("b.bin", Some("linux-x86_64-zip"), Some("sig")));
        let error = manifest("1.2.3-preview.4", "https://example.com", &artifacts, "").unwrap_err();
        assert!(
            error.contains("Unexpected") || error.contains("Duplicate"),
            "got: {error}"
        );
    }

    #[test]
    fn an_artifact_without_a_target_is_published_but_not_offered() {
        let mut artifacts = complete();
        artifacts.push(artifact("notes.txt", None, None));
        let manifest = manifest("1.2.3-preview.4", "https://example.com", &artifacts, "").unwrap();
        assert_eq!(
            manifest["platforms"].as_object().unwrap().len(),
            TARGETS.len()
        );
    }

    #[test]
    fn a_path_in_a_filename_is_rejected() {
        let mut artifacts = complete();
        artifacts[0]["filename"] = serde_json::Value::String("../escape".into());
        let error = manifest("1.2.3-preview.4", "https://example.com", &artifacts, "").unwrap_err();
        assert!(error.contains("plain filename"), "got: {error}");
    }

    #[test]
    fn a_non_preview_version_is_rejected() {
        let error = manifest("1.2.3", "https://example.com", &complete(), "").unwrap_err();
        assert!(error.contains("preview"), "got: {error}");
    }
}
