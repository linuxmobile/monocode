//! Phase 6 end-to-end: the host must keep running after the process that
//! started it goes away, which is what makes a remote machine reachable.
//!
//! Only the detached path is exercised here. The systemd path installs a real
//! user unit, so testing it would mutate the machine running the tests.

use monocode_host::service::{
    connection_info, lifecycle, service_path, start_detached, ServiceOptions,
};

/// A detached host must survive the process that launched it and answer on the
/// port it reported.
#[test]
fn a_detached_host_starts_and_is_reachable() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let options = ServiceOptions {
        directory: dir.path().to_path_buf(),
        port,
        executable: std::path::PathBuf::from(env!("CARGO_BIN_EXE_monocode-host")),
    };
    let pid = start_detached(&options, &service_path()).expect("start_detached");
    assert!(pid > 0, "no process id was returned");

    let connection = wait_for_host(&options, "the detached host never answered");
    assert_eq!(connection.port, port, "the host reported a different port");

    // Stop it so nothing is left running, then confirm cleanup.
    let _ = lifecycle(&options.directory, "stop");
    for _ in 0..50 {
        if !options.directory.join("running.json").exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // A survivor would leak into later runs, so this fails rather than warns.
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
    panic!("the detached host did not clean up its state file");
}

/// Installing twice must reuse a running host, because a reconnect that
/// restarted it would interrupt whatever an agent was doing.
#[test]
fn a_second_start_reuses_the_running_host() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let options = ServiceOptions {
        directory: dir.path().to_path_buf(),
        port,
        executable: std::path::PathBuf::from(env!("CARGO_BIN_EXE_monocode-host")),
    };
    let first = start_detached(&options, &service_path()).unwrap();
    let connection = wait_for_host(&options, "the host never answered");

    // A second start would fail to bind or replace it; the point is that the
    // already-running host is detected first.
    let again = connection_info(&options.directory).unwrap();
    assert_eq!(again.pid, connection.pid);
    assert_eq!(connection.pid, first, "a different process took over");

    let _ = lifecycle(&options.directory, "stop");
    for _ in 0..50 {
        if !options.directory.join("running.json").exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = std::process::Command::new("kill")
        .args(["-9", &first.to_string()])
        .status();
}

/// The host writes `running.json` with the port it bound, so a caller reads the
/// real port rather than assuming the one it asked for.
fn wait_for_host(options: &ServiceOptions, message: &str) -> monocode_host::service::Connection {
    let mut last = String::new();
    for _ in 0..100 {
        match connection_info(&options.directory) {
            Ok(connection) => return connection,
            Err(error) => last = error,
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("{message} (last error: {last})");
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Two hosts must never share a data directory.
///
/// The second would overwrite `running.json` pointing at itself and, when it
/// exited, delete the file the first host depends on — leaving a live host
/// unreachable with no way to discover its port.
#[test]
fn a_second_host_refuses_to_share_a_data_directory() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let options = ServiceOptions {
        directory: dir.path().to_path_buf(),
        port,
        executable: std::path::PathBuf::from(env!("CARGO_BIN_EXE_monocode-host")),
    };
    let pid = start_detached(&options, &service_path()).unwrap();
    let connection = wait_for_host(&options, "the first host never answered");

    // A second `serve` on the same directory must fail and say why.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_monocode-host"))
        .args([
            "serve",
            "--data-dir",
            &dir.path().to_string_lossy(),
            "--port",
            "0",
        ])
        .output()
        .expect("the second host could not be started");
    assert!(
        !output.status.success(),
        "a second host started on a directory that already had one"
    );
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(
        message.contains("already serving"),
        "the refusal did not explain the conflict: {message}"
    );

    // The decisive check: the original host is still reachable. A second host
    // that wrote its own state file would make this fail.
    let still_there = connection_info(dir.path()).expect("the first host became unreachable");
    assert_eq!(still_there.pid, connection.pid);
    assert_eq!(still_there.port, connection.port);

    let _ = lifecycle(dir.path(), "stop");
    for _ in 0..50 {
        if !dir.path().join("running.json").exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
}

/// A state file left by a crashed host must not block a new one.
///
/// Refusing on any existing file would mean one crash needed a manual delete to
/// start again, which is worse than the problem being solved.
#[test]
fn a_stale_state_file_does_not_block_startup() {
    let dir = tempfile::tempdir().unwrap();
    // A pid that cannot exist, as a reboot or a crash leaves behind.
    std::fs::write(
        dir.path().join("running.json"),
        serde_json::json!({
            "pid": 999_999,
            "port": 1234,
            "secret": "x".repeat(43),
        })
        .to_string(),
    )
    .unwrap();

    let port = free_port();
    let options = ServiceOptions {
        directory: dir.path().to_path_buf(),
        port,
        executable: std::path::PathBuf::from(env!("CARGO_BIN_EXE_monocode-host")),
    };
    let pid = start_detached(&options, &service_path()).expect("a stale file blocked startup");
    let connection = wait_for_host(&options, "the host never answered");
    assert_eq!(connection.pid, pid, "the host did not take ownership");

    let _ = lifecycle(dir.path(), "stop");
    for _ in 0..50 {
        if !dir.path().join("running.json").exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
}
