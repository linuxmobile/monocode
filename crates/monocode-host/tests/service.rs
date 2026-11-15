//! Phase 6 end-to-end: the host must keep running after the process that
//! started it goes away, which is what makes a remote machine reachable.
//!
//! Only the detached path is exercised here. The systemd path installs a real
//! user unit, so testing it would mutate the machine running the tests.

use monocode_host::service::{connection_info, lifecycle, start_detached, service_path, ServiceOptions};

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
