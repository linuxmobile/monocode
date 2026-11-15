//! The dev loop's lifecycle rules, driven against a fake app.
//!
//! Each behaviour here was a bug once and is easy to reintroduce: the app must
//! be ended through its own `quit` hook rather than a signal, so unsaved state
//! survives; and closing must not leave the app running after the runner exits.

use std::path::PathBuf;

use xtask::dev::Dev;

struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("monocode-dev-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // Cargo metadata must succeed without a real crate: a stub manifest and
        // a fake `cargo` on PATH keep the test offline and fast.
        std::fs::create_dir_all(root.join("crates/monocode-gpui/src")).unwrap();
        // A stub target directory so no real Cargo build is attempted.
        std::fs::create_dir_all(root.join("target/native-dev")).unwrap();
        std::fs::write(
            root.join("crates/monocode-gpui/Cargo.toml"),
            "[package]\nname = \"monocode-gpui\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        // The fake app reads stdin and records both events, which is the same
        // contract the real app's debug hook has.
        std::fs::write(
            root.join("crates/monocode-gpui/src/main.rs"),
            r#"use std::io::BufRead as _;
fn main() {
    let dir = std::env::var("MONOCODE_FIXTURE_DIR").unwrap();
    std::fs::write(format!("{dir}/events"), "started\n").unwrap();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    std::fs::write(format!("{dir}/events"), format!("started\nquit:{}\n", line.trim())).unwrap();
}
"#,
        )
        .unwrap();
        // The dev build uses this profile, so the fixture must define it.
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"2\"\nmembers = [\"crates/monocode-gpui\"]\n\n[profile.native-dev]\ninherits = \"dev\"\n",
        )
        .unwrap();
        // `cargo metadata --locked` refuses to create a lockfile, so the
        // fixture provides one and never touches the network.
        std::fs::write(
            root.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"monocode-gpui\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        Self { root }
    }

    fn events(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.join("events"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Closing ends a running app through its `quit` hook, not a signal.
#[test]
fn closing_ends_the_app_through_its_quit_hook() {
    let workspace = Workspace::new("quit");
    // The fixture crate is real, so this compiles an actual app and launches
    // the binary Cargo produced rather than a stand-in.
    std::env::set_var("MONOCODE_FIXTURE_DIR", &workspace.root);
    let dev = Dev::new(workspace.root.clone(), Vec::new());
    dev.request_build();
    dev.step().expect("the dev loop failed");

    // Wait for the app to record its start before closing it.
    for _ in 0..100 {
        if workspace.events().iter().any(|event| event == "started") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        workspace.events().iter().any(|event| event == "started"),
        "the app never started: {:?}",
        workspace.events()
    );

    dev.close();
    let events = workspace.events();
    assert!(
        events.iter().any(|event| event == "quit:quit"),
        "the app was killed instead of asked to quit: {events:?}"
    );
}

/// Closing before any app started must not fail or hang.
#[test]
fn closing_without_a_running_app_is_harmless() {
    let workspace = Workspace::new("idle");
    let dev = Dev::new(workspace.root.clone(), Vec::new());
    dev.close();
    dev.close();
}

/// After closing, a queued request must not start another build.
#[test]
fn a_request_after_closing_is_ignored() {
    let workspace = Workspace::new("closed");
    let dev = Dev::new(workspace.root.clone(), Vec::new());
    assert!(!dev.pending(), "a fresh runner should have nothing to do");
    dev.close();
    dev.request_build();
    assert!(!dev.pending(), "a closed runner accepted a new build");
    assert!(!dev.step().unwrap(), "a closed runner stepped again");
}
