//! Build and release orchestration for the native app.
//!
//! This exists so that no step of building, packaging or releasing needs
//! Node.js. Straightforward Linux work lives in `scripts/*.sh`; anything that
//! parses or writes structured files lives here, where a parser beats a regex.

use std::path::{Path, PathBuf};

use xtask::{dev, package, release, version};

fn root() -> Result<PathBuf, String> {
    // `xtask` is always run from the workspace, so the manifest location is the
    // only trustworthy root; guessing from argv[0] breaks under a wrapper.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "Cannot locate the workspace root".to_string())
}

fn usage() -> &'static str {
    "\
Usage: cargo xtask <command>

Commands:
  set-version <x.y.z>   Set the workspace version
  set-version-check <x.y.z>
                        Fail unless the version is a valid preview
  dev [args...]         Build, run and rebuild on request
  build [--release]     Build the app with Cargo
  package <kind> <binary> <out> [arch]
                        Package a prebuilt executable (linux, rpm)
  release <directory>   Sign artifacts and write the update manifest
  release-files <directory>
                        Print the files a release publishes, one per line
  help                  Show this help"
}

fn run(options: Vec<String>) -> Result<(), String> {
    let command = options.first().map(String::as_str).unwrap_or("help");
    match command {
        "help" | "--help" | "-h" => {
            println!("{}", usage());
            Ok(())
        }
        "set-version" => {
            let version = options
                .get(1)
                .ok_or_else(|| "Usage: cargo xtask set-version 0.17.0".to_string())?;
            version::bump(&root()?, version)
        }
        "set-version-check" => {
            // Validates without writing, so a release tag cannot silently
            // produce a version the updater rejects.
            let version = options.get(1).ok_or_else(|| {
                "Usage: cargo xtask set-version-check 0.17.0-preview.1".to_string()
            })?;
            package::preview_version(version).map(|_| ())
        }
        "dev" => dev::run(root()?, dev::parse_args(&options[1..])?),
        "build" => {
            let release = options.iter().any(|option| option == "--release");
            build(&root()?, release)
        }
        "package" => {
            let kind = options.get(1).cloned().unwrap_or_default();
            let binary = options.get(2).cloned().unwrap_or_default();
            let out = options.get(3).cloned().unwrap_or_default();
            let arch = options
                .get(4)
                .cloned()
                .unwrap_or_else(|| "x86_64".to_string());
            package::package_native(&root()?, &kind, Path::new(&binary), Path::new(&out), &arch)
        }
        "release-files" => {
            // A one-line-per-file listing is what the publish step feeds to its
            // upload loop.
            let directory = options
                .get(1)
                .ok_or_else(|| "Usage: cargo xtask release-files <directory>".to_string())?;
            let text = std::fs::read_to_string(Path::new(directory).join("release-files.json"))
                .map_err(|e| format!("read release-files.json: {e}"))?;
            let files: Vec<String> = serde_json::from_str(&text)
                .map_err(|e| format!("parse release-files.json: {e}"))?;
            for file in files {
                println!("{file}");
            }
            Ok(())
        }
        "release" => {
            let directory = options
                .get(1)
                .ok_or_else(|| "Usage: cargo xtask release <directory>".to_string())?;
            release::prepare(&root()?, Path::new(directory)).map(|_| ())
        }
        other => Err(format!("Unknown command: {other}\n\n{}", usage())),
    }
}

/// Compiles the app, reporting the executable so a caller can package it.
fn build(root: &Path, release: bool) -> Result<(), String> {
    let mut command = std::process::Command::new("cargo");
    command.current_dir(root).args([
        "build",
        "--manifest-path",
        "crates/monocode-gpui/Cargo.toml",
        "--locked",
        "-p",
        "monocode-gpui",
    ]);
    if release {
        command.arg("--release");
    }
    let status = command
        .status()
        .map_err(|e| format!("Could not run cargo build: {e}"))?;
    if !status.success() {
        return Err(format!("cargo build failed ({status})"));
    }
    let profile = if release { "release" } else { "debug" };
    println!(
        "Executable: {}",
        target_dir(root)?
            .join(profile)
            .join("monocode-gpui")
            .display()
    );
    Ok(())
}

/// Where Cargo writes, honouring a cross-compilation target.
pub fn target_dir(root: &Path) -> Result<PathBuf, String> {
    let output = std::process::Command::new("cargo")
        .current_dir(root)
        .args([
            "metadata",
            "--manifest-path",
            "crates/monocode-gpui/Cargo.toml",
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .output()
        .map_err(|e| format!("Could not run cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("cargo metadata returned invalid JSON: {e}"))?;
    let base = metadata
        .get("target_directory")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "cargo metadata did not report a target_directory".to_string())?;
    let base = PathBuf::from(base);
    Ok(match std::env::var("CARGO_BUILD_TARGET") {
        Ok(target) if !target.is_empty() => base.join(target),
        _ => base,
    })
}

fn main() {
    let options: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = run(options) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
