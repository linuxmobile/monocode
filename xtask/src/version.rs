//! Sets the workspace version in every file that records it.
//!
//! One version spans the Cargo workspace, and the documents that quote it must
//! agree or a release ships claiming two different numbers. The previous
//! TypeScript version also wrote `package.json` and `package-lock.json`; both
//! are gone with Node, so the Cargo manifest is the only source of truth.

use std::path::Path;

/// Rejects anything that is not a plain `MAJOR.MINOR.PATCH`, since the value is
/// written into manifests where a stray quote would corrupt the file.
pub fn validate(version: &str) -> Result<(), String> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3 {
        return Err(format!(
            "Expected a version such as 0.17.0, got {version:?}"
        ));
    }
    for part in parts {
        // No empty, signed or whitespace-bearing component: `0.-1` is not a
        // version, and a `+` would be read as build metadata by Cargo.
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(format!(
                "Expected a version such as 0.17.0, got {version:?}"
            ));
        }
    }
    Ok(())
}

/// Replaces the first `version = "..."` line inside `[workspace.package]`.
///
/// Anchoring on the section matters: the manifest declares versions for
/// dependencies too, and rewriting those would break the build.
pub fn replace_workspace_version(manifest: &str, version: &str) -> Result<String, String> {
    let mut lines: Vec<String> = Vec::new();
    let mut in_section = false;
    let mut replaced = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed == "[workspace.package]";
        } else if in_section && !replaced && trimmed.starts_with("version") {
            lines.push(format!("version = \"{version}\""));
            replaced = true;
            continue;
        }
        lines.push(line.to_string());
    }
    if !replaced {
        return Err("Cargo.toml has no version in [workspace.package]".into());
    }
    let mut text = lines.join("\n");
    // `lines()` drops the final newline, so restore it rather than silently
    // changing the file's last byte.
    if manifest.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
}

/// Bumps the workspace version, refusing to guess when a file is missing.
pub fn bump(root: &Path, version: &str) -> Result<(), String> {
    validate(version)?;
    let manifest = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("read {}: {e}", manifest.display()))?;
    let next = replace_workspace_version(&text, version)?;
    std::fs::write(&manifest, next).map_err(|e| format!("write {}: {e}", manifest.display()))?;
    println!("version {version}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_version_is_accepted() {
        for version in ["0.16.0", "1.0.0", "10.20.30"] {
            assert!(validate(version).is_ok(), "{version} was rejected");
        }
    }

    #[test]
    fn anything_that_could_corrupt_the_manifest_is_rejected() {
        for version in [
            "",
            "0.16",
            "0.16.0.1",
            "0.16.0-beta",
            "v0.16.0",
            "0.16.x",
            "-1.0.0",
            "0. 16.0",
            "0.16.0\"",
            "1.0.0+meta",
        ] {
            assert!(validate(version).is_err(), "{version:?} was accepted");
        }
    }

    #[test]
    fn only_the_workspace_version_changes() {
        // Dependency versions must survive: rewriting them breaks the build.
        let manifest = "\
[workspace.package]
version = \"0.16.0\"
edition = \"2021\"

[workspace.dependencies]
serde = { version = \"1\", features = [\"derive\"] }
";
        let next = replace_workspace_version(manifest, "0.17.0").unwrap();
        assert!(next.contains("version = \"0.17.0\""));
        assert!(
            next.contains("serde = { version = \"1\""),
            "a dependency version was rewritten: {next}"
        );
        assert_eq!(next.matches("0.17.0").count(), 1);
    }

    #[test]
    fn a_package_version_outside_the_workspace_section_is_untouched() {
        let manifest = "\
[package]
name = \"x\"
version = \"0.0.0\"

[workspace.package]
version = \"0.16.0\"
";
        let next = replace_workspace_version(manifest, "0.17.0").unwrap();
        assert!(next.contains("version = \"0.0.0\""), "{next}");
        assert!(next.contains("version = \"0.17.0\""));
    }

    #[test]
    fn the_trailing_newline_is_preserved() {
        let manifest = "[workspace.package]\nversion = \"0.16.0\"\n";
        let next = replace_workspace_version(manifest, "0.17.0").unwrap();
        assert!(
            next.ends_with('\n'),
            "the final newline was dropped: {next:?}"
        );
    }

    #[test]
    fn a_manifest_without_a_workspace_version_is_an_error() {
        let error = replace_workspace_version("[package]\nname = \"x\"\n", "0.17.0").unwrap_err();
        assert!(error.contains("[workspace.package]"), "got: {error}");
    }
}
