//! The dependency guard that keeps a screen-capture crate out of the build.
//!
//! `zed-scap` cannot be allowed in the native dependency graph, because its
//! implementation links a capture library the app deliberately does not ship.
//! The workspace patches it to an empty stub, and the guard checks that a
//! registry copy never resolves in — including as an optional dependency, which
//! is easy to add without noticing.
//!
//! These run real `cargo` builds in throwaway workspaces, so they are slow and
//! require no network (the fixture has no dependencies).

use std::path::{Path, PathBuf};
use std::process::Command;

/// The guard lives with the crate it protects; the fixture compiles a copy.
fn guard_source(root: &Path) -> PathBuf {
    root.join("crates/monocode-gpui/dependency_guard.rs")
}

struct Fixture {
    root: PathBuf,
    manifest: String,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "monocode-dependency-guard-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "").unwrap();
        // A copy, so the fixture does not need the real crate's dependencies.
        std::fs::copy(
            guard_source(&repository_root()),
            root.join("dependency_guard.rs"),
        )
        .unwrap();
        std::fs::write(
            root.join("build.rs"),
            "mod dependency_guard;\nfn main() {\n    dependency_guard::check(std::path::Path::new(env!(\"CARGO_MANIFEST_DIR\")));\n}\n",
        )
        .unwrap();
        let manifest = "[package]\nname = \"dependency-guard-fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n".to_string();
        std::fs::write(root.join("Cargo.toml"), &manifest).unwrap();
        Self { root, manifest }
    }

    fn cargo(&self, arguments: &[&str]) -> std::process::Output {
        Command::new("cargo")
            .args(arguments)
            .arg("--offline")
            .current_dir(&self.root)
            .env("CARGO_TARGET_DIR", self.root.join("target"))
            .env("CARGO_TERM_COLOR", "never")
            .output()
            .expect("cargo could not be run")
    }

    /// Adds the capture crate as a local path. A path dependency has no
    /// `source`, which is exactly what the stub looks like in the lockfile.
    fn add_path_dependency(&self, optional: bool) {
        std::fs::create_dir_all(self.root.join("capture/src")).unwrap();
        std::fs::write(self.root.join("capture/src/lib.rs"), "").unwrap();
        std::fs::write(
            self.root.join("capture/Cargo.toml"),
            "[package]\nname = \"zed-scap\"\nversion = \"9.9.9\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            self.root.join("Cargo.toml"),
            format!(
                "{}\n[dependencies]\ncapture = {{ package = \"zed-scap\", path = \"capture\", optional = {optional} }}\n",
                self.manifest
            ),
        )
        .unwrap();
    }

    fn lock(&self) -> String {
        std::fs::read_to_string(self.root.join("Cargo.lock")).unwrap_or_default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Finds the workspace root from this test binary's location.
fn repository_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `<root>/xtask`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent directory")
        .to_path_buf()
}

#[test]
fn the_guard_allows_a_local_stub() {
    let fixture = Fixture::new("stub");
    let output = fixture.cargo(&["build"]);
    assert!(
        output.status.success(),
        "a local stub was rejected: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn an_optional_local_stub_stays_out_of_the_registry() {
    // The dangerous case: an optional dependency is easy to add and does not
    // appear in a default build, but it still enters the lockfile.
    let fixture = Fixture::new("optional");
    fixture.add_path_dependency(true);
    let output = fixture.cargo(&["build"]);
    assert!(
        output.status.success(),
        "an optional path stub was rejected: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lock = fixture.lock();
    assert!(
        lock.contains("name = \"zed-scap\""),
        "the stub did not resolve"
    );
    assert!(
        !lock
            .lines()
            .any(|line| line.trim().starts_with("source = \"registry")),
        "a registry dependency appeared in the lockfile:\n{lock}"
    );
}

#[test]
fn removing_the_dependency_leaves_a_buildable_workspace() {
    // Reversibility matters: a guard that only tolerates adding is a trap.
    let fixture = Fixture::new("removal");
    fixture.add_path_dependency(true);
    assert!(fixture.cargo(&["build"]).status.success());
    std::fs::write(fixture.root.join("Cargo.toml"), &fixture.manifest).unwrap();
    let output = fixture.cargo(&["build"]);
    assert!(
        output.status.success(),
        "the workspace did not build after removal: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Runs the guard's decision against a lockfile we control.
///
/// A real build cannot test this: Cargo rewrites a stale lockfile before
/// `build.rs` executes, so a forged `zed-scap` entry is removed before the guard
/// sees it (verified by hand; the build simply succeeds). The guard's own
/// `#[cfg(test)]` tests never run either, because they live in a module only
/// `build.rs` compiles.
///
/// So the shipped function is compiled into a tiny standalone program with the
/// guard's unit tests enabled. That is the only way to exercise the real code.
#[test]
fn the_guard_rejects_a_registry_source() {
    let directory =
        std::env::temp_dir().join(format!("monocode-guard-logic-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let source = std::fs::read_to_string(guard_source(&repository_root())).unwrap();
    // The guard is std-only precisely so it can be compiled like this. The
    // lockfile fixtures are built from real newlines via `concat!`, so the
    // guard sees the same text Cargo would write.
    let program = format!(
        "#![allow(dead_code)]\n{source}\n\nfn main() {{\n    \
         let path_stub = concat!(\n        \"[[package]]\\n\",\n        \"name = \\\"zed-scap\\\"\\n\",\n        \"version = \\\"1.0.0\\\"\\n\",\n    );\n    \
         let registry = concat!(\n        \"[[package]]\\n\",\n        \"name = \\\"zed-scap\\\"\\n\",\n        \"version = \\\"1.0.0\\\"\\n\",\n        \"source = \\\"registry+https://example\\\"\\n\",\n    );\n    \
         assert!(!has_registry_scap(path_stub), \"a path stub was rejected\");\n    \
         assert!(has_registry_scap(registry), \"a registry copy was allowed\");\n    \
         println!(\"guard-ok\");\n}}\n"
    );
    let file = directory.join("main.rs");
    std::fs::write(&file, program).unwrap();
    let output = Command::new("rustc")
        .arg(&file)
        .args(["--edition", "2021", "-o"])
        .arg(directory.join("guard"))
        .output()
        .expect("rustc could not be run");
    assert!(
        output.status.success(),
        "the guard did not compile standalone:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run = Command::new(directory.join("guard"))
        .output()
        .expect("the guard program could not be run");
    let _ = std::fs::remove_dir_all(&directory);
    assert!(
        run.status.success(),
        "the guard decided wrongly:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(String::from_utf8_lossy(&run.stdout).contains("guard-ok"));
}

#[test]
fn gpui_refuses_to_enable_screen_capture() {
    // The vendor crate must fail to compile with the feature on, so nobody can
    // turn it on believing it works.
    let root = repository_root();
    let build_script = root.join("vendor/gpui-pre/build.rs");
    if !build_script.exists() {
        // The vendor tree is optional in some checkouts; skipping keeps this
        // test honest rather than failing on a missing path.
        eprintln!("skipping: {} is absent", build_script.display());
        return;
    }
    let output = Command::new("rustc")
        .arg(&build_script)
        .args(["--edition", "2021", "--cfg", "feature=\"screen-capture\""])
        .arg("--out-dir")
        .arg(std::env::temp_dir())
        .output()
        .expect("rustc could not be run");
    assert!(
        !output.status.success(),
        "screen capture compiled successfully"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Screen capture is disabled in Monocode"),
        "the refusal had an unexpected message: {stderr}"
    );
}
