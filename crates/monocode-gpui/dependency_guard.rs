//! Std-only so scripts/native-dependencies.test.mjs can build it in an
//! offline fixture crate.
use std::{fs, path::Path};

pub fn check(manifest_dir: &Path) {
    let lockfile = manifest_dir.join("Cargo.lock");
    println!("cargo:rerun-if-changed={}", lockfile.display());
    let lock = fs::read_to_string(&lockfile).expect("The native workspace requires Cargo.lock");
    assert!(
        !has_registry_scap(&lock),
        "zed-scap is prohibited in the native dependency graph, including optional dependencies. \
         Only the local stub (a path patch with no implementation, so screen capture cannot \
         compile in) is allowed. Do not suppress this check based on its MIT metadata."
    );
}

/// Report whether the lockfile resolves zed-scap from a registry or other
/// remote source. Cargo writes canonical TOML: every `[[package]]` block
/// carries `name`, an optional `source`, then its dependencies. A path patch
/// has no `source` line, so the stub is invisible to this check.
fn has_registry_scap(lock: &str) -> bool {
    let mut in_scap = false;
    for line in lock.lines() {
        let line = line.trim();
        if line == "name = \"zed-scap\"" {
            in_scap = true;
            continue;
        }
        if line == "[[package]]" {
            if in_scap {
                return false;
            }
            continue;
        }
        if in_scap && line.starts_with("source = ") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::has_registry_scap;

    const PATH_SCAP: &str = r#"
[[package]]
name = "zed-scap"
version = "0.0.8-zed"
dependencies = [
 "gpui-pre",
]
"#;

    const REGISTRY_SCAP: &str = r#"
[[package]]
name = "zed-scap"
version = "0.0.8-zed"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "0000000000000000000000000000000000000000000000000000000000000000"
dependencies = [
 "gpui-pre",
]
"#;

    #[test]
    fn path_stub_is_allowed() {
        assert!(!has_registry_scap(PATH_SCAP));
        assert!(!has_registry_scap(""));
    }

    #[test]
    fn registry_copy_is_rejected() {
        assert!(has_registry_scap(REGISTRY_SCAP));
    }
}
