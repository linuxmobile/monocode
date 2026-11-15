//! End-to-end checks: the shipped host binary, driven by the real desktop
//! client from `monocode_core::remote`.
//!
//! These spawn the built executables rather than calling handlers directly, so
//! they cover argument parsing, `running.json`, the HTTP layer and the client's
//! own handshake together.

use std::{path::PathBuf, process::Command};

/// The host binary under test, built by `cargo test`.
fn host_binary() -> PathBuf {
    // `CARGO_BIN_EXE_*` is set for integration tests and points at the binary
    // cargo just built for this package.
    PathBuf::from(env!("CARGO_BIN_EXE_monocode-host"))
}

struct Host {
    _dir: tempfile::TempDir,
    directory: PathBuf,
    child: Option<std::process::Child>,
    port: u16,
}

impl Host {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        Self {
            directory: dir.path().to_path_buf(),
            _dir: dir,
            child: None,
            port: 0,
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(host_binary())
            .args(args)
            .arg("--data-dir")
            .arg(&self.directory)
            .output()
            .expect("run monocode-host")
    }

    /// Pairs a device and returns the issued token.
    fn pair(&self, name: &str) -> String {
        let output = self.run(&["pair", "--name", name, "--json"]);
        assert!(
            output.status.success(),
            "pair failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("pair prints JSON");
        value["token"].as_str().expect("a token").to_string()
    }

    /// Starts a host on a port the OS picks.
    ///
    /// A fixed port would collide when several tests run at once, so this asks
    /// for port 0 and reads back the port `serve` actually bound. `start_at`
    /// remains for the test that restarts a host on a known port.
    fn start(&mut self) {
        self.start_at(0);
    }

    fn start_at(&mut self, port: u16) {
        self.child = Some(
            Command::new(host_binary())
                .args(["serve", "--data-dir"])
                .arg(&self.directory)
                .arg("--port")
                .arg(port.to_string())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("start host"),
        );

        // `serve` writes running.json after binding, and it records the port
        // it really got, which is the only reliable readiness signal.
        let state = self.directory.join("running.json");
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(&state) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(found) = value["port"].as_u64() {
                        self.port = found as u16;
                        return;
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!(
            "host did not start; no usable running.json in {:?}",
            self.directory
        );
    }

    fn client(&self) -> monocode_core::remote::Connections {
        monocode_core::remote::Connections::new(self.directory.join("client"))
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn a_pairing_token_is_not_written_in_clear() {
    let host = Host::new();
    let token = host.pair("Laptop");
    let database = std::fs::read(host.directory.join("host.db")).expect("host.db");
    assert!(
        !String::from_utf8_lossy(&database).contains(&token),
        "the database must store only a hash of the token"
    );
}

#[test]
fn the_real_client_connects_and_lists_projects() {
    let mut host = Host::new();
    let token = host.pair("Laptop");
    host.start();

    let client = host.client();
    let machine = client
        .connect(
            "Test machine".into(),
            format!("http://127.0.0.1:{}", host.port),
            token,
        )
        .expect("the desktop client should connect to the native host");
    assert_eq!(machine.name, "Test machine");

    let projects = client
        .request(&machine.id, "projects.list", serde_json::json!({}))
        .expect("projects.list");
    assert!(projects.is_array(), "expected a list, got {projects}");
}

#[test]
fn a_revoked_device_can_no_longer_connect() {
    let mut host = Host::new();
    let token = host.pair("Laptop");
    host.start();

    // Revoking by id needs the id the pair call returned; list then revoke.
    let listed = host.run(&["devices", "--json"]);
    let devices: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("devices prints JSON");
    let id = devices[0]["id"].as_str().expect("a device id").to_string();
    assert!(host.run(&["revoke", &id]).status.success());

    let client = host.client();
    let error = client
        .connect(
            "Test".into(),
            format!("http://127.0.0.1:{}", host.port),
            token,
        )
        .unwrap_err();
    assert!(
        !error.is_empty(),
        "connecting with a revoked credential must fail"
    );
}

#[test]
fn an_unknown_credential_cannot_connect() {
    let mut host = Host::new();
    host.pair("Laptop");
    host.start();

    let client = host.client();
    let result = client.connect(
        "Test".into(),
        format!("http://127.0.0.1:{}", host.port),
        "a".repeat(43),
    );
    assert!(result.is_err(), "an unpaired credential must be refused");
}

#[test]
fn stop_shuts_the_server_down_and_clears_its_state_file() {
    let mut host = Host::new();
    host.pair("Laptop");
    host.start();

    let state_path = host.directory.join("running.json");
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).expect("running.json"))
            .expect("valid JSON");
    let secret = state["secret"].as_str().expect("admin secret");

    let response = ureq::post(&format!("http://127.0.0.1:{}/lifecycle", host.port))
        .set("Authorization", &format!("Bearer {secret}"))
        .send_string(r#"{"action":"stop"}"#);
    assert!(
        response.is_ok(),
        "the lifecycle endpoint should accept a local stop"
    );

    // The process removes its state file and exits.
    for _ in 0..100 {
        if !state_path.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("the host did not shut down after stop");
}

#[test]
fn projects_open_registers_a_folder_and_persists_it() {
    let mut host = Host::new();
    let token = host.pair("Laptop");
    let project_root = host.directory.join("work");
    std::fs::create_dir_all(&project_root).expect("project dir");
    host.start();

    let client = host.client();
    let machine = client
        .connect(
            "Test".into(),
            format!("http://127.0.0.1:{}", host.port),
            token,
        )
        .expect("connect");

    let opened = client
        .request(
            &machine.id,
            "projects.open",
            serde_json::json!({ "cwd": project_root.to_string_lossy(), "name": "work" }),
        )
        .expect("projects.open");
    assert_eq!(opened["name"], "work");

    let listed = client
        .request(&machine.id, "projects.list", serde_json::json!({}))
        .expect("projects.list");
    assert_eq!(listed.as_array().map(|list| list.len()), Some(1));
}

#[test]
fn an_unimplemented_method_says_so_instead_of_lying() {
    let mut host = Host::new();
    let token = host.pair("Laptop");
    host.start();

    let client = host.client();
    let machine = client
        .connect(
            "Test".into(),
            format!("http://127.0.0.1:{}", host.port),
            token,
        )
        .expect("connect");

    // An unported method must fail loudly rather than report a fake success.
    // This names one that is deliberately still refused, so the assertion keeps
    // testing the refusal path as more methods land.
    let error = client
        .request(
            &machine.id,
            "sessions.syncChunk",
            serde_json::json!({ "projectId": "x", "sessionId": "y" }),
        )
        .unwrap_err();
    assert!(
        error.contains("cannot serve"),
        "expected an explicit refusal, got: {error}"
    );
}
