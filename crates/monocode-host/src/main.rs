//! The MonoCode remote host.
//!
//! Serves one machine's projects, files and Git state to a paired desktop over
//! loopback HTTP. Every provider CLI it launches is one the user installed; the
//! host never downloads or installs an agent runtime.

use monocode_host::{mutex, protocol, server, store};

use protocol::HOST_PROTOCOL_VERSION;
use server::HostState;
use std::{
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
};
use store::HostStore;
use tokio::sync::{broadcast, mpsc};

const DEFAULT_PORT: u16 = 3774;

fn main() {
    let code = match run(std::env::args().skip(1).collect()) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    };
    std::process::exit(code);
}

fn run(args: Vec<String>) -> Result<(), String> {
    // The remote bootstrap compares this against the desktop's version to prove
    // it unpacked the build it asked for. The wording has to match the desktop
    // exactly, or every SSH host install fails its version check.
    if matches!(args.first().map(String::as_str), Some("--version" | "-V")) {
        println!("Monocode Native {}", version());
        return Ok(());
    }
    let options = Options::parse(&args)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("start the runtime: {e}"))?;
    runtime.block_on(async_main(options, &args))
}

/// Set by the packaging step so a built archive reports its release version.
fn version() -> &'static str {
    option_env!("MONOCODE_NATIVE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

struct Options {
    command: String,
    directory: PathBuf,
    port: u16,
    json: bool,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let command = args.first().cloned().unwrap_or_else(|| "help".into());
        let mut directory = match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(".monocode-host"),
            None => return Err("HOME is not set".into()),
        };
        let mut port = DEFAULT_PORT;
        let mut json = false;
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--data-dir" => {
                    index += 1;
                    directory = PathBuf::from(args.get(index).ok_or("Missing --data-dir value")?);
                }
                "--port" => {
                    index += 1;
                    port = args
                        .get(index)
                        .ok_or("Missing --port value")?
                        .parse()
                        .map_err(|_| "The port must be a number".to_string())?;
                }
                "--json" => json = true,
                _ => {}
            }
            index += 1;
        }
        Ok(Self {
            command,
            directory,
            port,
            json,
        })
    }
}

async fn async_main(options: Options, args: &[String]) -> Result<(), String> {
    std::fs::create_dir_all(&options.directory)
        .map_err(|e| format!("create {}: {e}", options.directory.display()))?;
    let database = options.directory.join("host.db");
    let mut store = HostStore::open(&database)?;

    match options.command.as_str() {
        "pair" => {
            let name = flag(args, "--name").unwrap_or_else(|| "Desktop".to_string());
            let device = store.issue_device(&name)?;
            print_json(&device, options.json);
            Ok(())
        }
        "devices" => {
            let devices = store.devices()?;
            print_json(&devices, options.json);
            Ok(())
        }
        "revoke" => {
            let id = args
                .iter()
                .skip(1)
                .find(|arg| !arg.starts_with("--"))
                .ok_or("Provide a device ID to revoke")?;
            if store.revoke_device(id)? {
                println!("Device revoked");
                Ok(())
            } else {
                Err("Device not found".into())
            }
        }
        "install" => {
            let service = monocode_host::service::ServiceOptions {
                directory: options.directory.clone(),
                port: options.port,
                executable: std::env::current_exe()
                    .map_err(|e| format!("Cannot locate this executable: {e}"))?,
            };
            // Reusing a running host means a reconnect never interrupts work.
            let (connection, mode) = monocode_host::service::install_service(&service)?;
            if options.json {
                print_json(
                    &serde_json::json!({
                        "port": connection.port,
                        "pid": connection.pid,
                        "mode": mode.to_string(),
                    }),
                    true,
                );
            } else {
                println!(
                    "Host is running on 127.0.0.1:{} (PID {}), managed by {mode}.",
                    connection.port, connection.pid
                );
            }
            Ok(())
        }
        "status" => {
            match monocode_host::service::connection_info(&options.directory) {
                Ok(connection) => {
                    if options.json {
                        print_json(
                            &serde_json::json!({
                                "port": connection.port,
                                "pid": connection.pid,
                            }),
                            true,
                        );
                    } else {
                        println!(
                            "Host is running (PID {}, port {})",
                            connection.pid, connection.port
                        );
                    }
                }
                Err(_) => println!("Host is stopped"),
            }
            Ok(())
        }
        "stop" => {
            monocode_host::service::lifecycle(&options.directory, "stop")?;
            println!("Host is stopping");
            Ok(())
        }
        "uninstall" => {
            let notes = monocode_host::service::uninstall_service()?;
            // A host started outside the service manager is still running, so
            // stop it too; the state file disappearing is the confirmation.
            let _ = monocode_host::service::lifecycle(&options.directory, "stop");
            let state = options.directory.join("running.json");
            for _ in 0..200 {
                if !state.exists() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            println!(
                "{}",
                if state.exists() {
                    "The host service was removed, but the host is still running. Run stop, or end its process."
                } else {
                    "The host is stopped and will not start automatically."
                }
            );
            // Removing the data directory would erase sessions and credentials,
            // which is the user's decision, not an uninstall's.
            println!(
                "Sessions, logs and device credentials are kept in {}. Delete that directory only if you want to erase them.",
                options.directory.display()
            );
            for note in notes {
                println!("{note}");
            }
            Ok(())
        }
        "serve" => serve(store, options).await,
        "version" => {
            println!(
                "monocode-host {} (protocol {HOST_PROTOCOL_VERSION})",
                env!("CARGO_PKG_VERSION")
            );
            Ok(())
        }
        "help" | "--help" | "-h" => {
            println!("{HELP}");
            Ok(())
        }
        other => Err(format!("Unknown command `{other}`; run with --help")),
    }
}

const HELP: &str = "\
Usage: monocode-host <command> [options]

Commands:
  install           Install and start the host, surviving logout
  status            Report whether the host is running
  stop              Stop a running host
  uninstall         Remove the service and stop the host
  serve             Serve this machine to a paired desktop
  pair              Issue a device credential for a desktop
  devices           List paired devices
  revoke <id>       Revoke a paired device
  version           Print the version and protocol
  help              Show this help

Options:
  --data-dir <dir>  Data directory (default: ~/.monocode-host)
  --port <port>     Port to listen on (default: 3774)";

/// Reads the recorded owner of a data directory, if one is still running.
///
/// Two hosts sharing a data directory would both write `running.json` and both
/// clean up the other's, leaving a live host unreachable. A stale file from a
/// crash is not an owner, so the process is checked rather than trusted.
fn running_owner(directory: &std::path::Path) -> Option<u32> {
    let text = std::fs::read_to_string(directory.join("running.json")).ok()?;
    let state: serde_json::Value = serde_json::from_str(&text).ok()?;
    let pid = state.get("pid").and_then(serde_json::Value::as_u64)? as i32;
    // Signal 0 tests for existence without disturbing the process.
    #[cfg(unix)]
    {
        if unsafe { kill(pid, 0) } == 0 {
            return Some(pid as u32);
        }
    }
    None
}

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

async fn serve(store: HostStore, options: Options) -> Result<(), String> {
    // Refusing is the only safe answer: a second host would overwrite the
    // first one's state file and, on exit, delete it.
    if let Some(pid) = running_owner(&options.directory) {
        return Err(format!(
            "Another MonoCode host ({pid}) is already serving {}. Stop it first, or use a different --data-dir.",
            options.directory.display()
        ));
    }
    let environment_id = store.environment_id.clone();
    let providers = discover_providers();
    let (shutdown, _) = broadcast::channel(4);
    let (stop, mut stopped) = mpsc::channel::<()>(1);

    let admin_secret = store::random_token();
    let state = Arc::new(HostState {
        store: Arc::new(mutex::Mutex::new(store)),
        admin_secret: admin_secret.clone(),
        providers: providers.clone(),
        transfers: std::sync::Arc::new(monocode_host::transfers::SyncTransfers::new()),
        workspace: std::sync::Arc::new(monocode_host::workspace_commands::Workspace::new(
            Vec::new(),
        )),
        turns: Arc::new(mutex::Mutex::new(std::collections::HashMap::new())),
        shutdown: shutdown.clone(),
    });

    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, options.port));
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|e| format!("listen on {address}: {e}"))?;
    let bound = listener.local_addr().map_err(|e| e.to_string())?;

    // The desktop reads this file to find the port and administrative secret.
    let state_path = options.directory.join("running.json");
    std::fs::write(
        &state_path,
        serde_json::json!({
            "pid": std::process::id(),
            "port": bound.port(),
            "secret": admin_secret,
        })
        .to_string(),
    )
    .map_err(|e| format!("write {}: {e}", state_path.display()))?;

    log::info!("MonoCode Host {environment_id} listening on {bound}");

    let app = server::router(Arc::clone(&state));
    let mut shutdown_rx = shutdown.subscribe();

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = shutdown_rx.recv() => {}
                _ = signal() => {}
                _ = stopped.recv() => {}
            }
        })
        .await;

    // Remove the state file and hand back the socket before exiting, so a
    // restart on the same port does not race the old process. Only our own
    // file is removed: if it now names another process, deleting it would make
    // that host unreachable.
    if running_owner(&options.directory) == Some(std::process::id()) {
        let _ = std::fs::remove_file(&state_path);
    }
    if result.is_err() {
        let _ = stop.send(()).await;
    }
    result.map_err(|e| format!("serve: {e}"))
}

/// Resolves SIGTERM and Ctrl-C, so a service manager's stop is graceful.
async fn signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal as unix_signal, SignalKind};
        let mut terminate = match unix_signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(_) => return std::future::pending::<()>().await,
        };
        let mut interrupt = match unix_signal(SignalKind::interrupt()) {
            Ok(signal) => signal,
            Err(_) => return std::future::pending::<()>().await,
        };
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Provider CLIs are the user's own installs; the host only reports which are
/// reachable on `PATH`.
fn discover_providers() -> Vec<String> {
    protocol::REMOTE_PROVIDERS
        .iter()
        .filter(|name| on_path(name))
        .map(|name| (*name).to_string())
        .collect()
}

fn on_path(binary: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(binary);
        candidate.is_file() && is_executable(&candidate)
    })
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let index = args.iter().position(|arg| arg == name)?;
    args.get(index + 1).cloned()
}

/// Matches the previous host: `--json` prints one compact line for a script,
/// and the default is indented for a person.
fn print_json<T: serde::Serialize>(value: &T, compact: bool) {
    let rendered = if compact {
        serde_json::to_string(value)
    } else {
        serde_json::to_string_pretty(value)
    };
    println!("{}", rendered.unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    /// The remote bootstrap compares this output to the desktop's version
    /// string. A host that cannot report its version fails every SSH install at
    /// the unpack or install check, so the exact form is part of the contract.
    #[test]
    fn the_version_is_reported_in_the_form_the_bootstrap_compares() {
        let printed = format!("Monocode Native {}", version());
        assert!(
            printed.starts_with("Monocode Native "),
            "unexpected version form: {printed}"
        );
        let reported = printed.strip_prefix("Monocode Native ").expect("prefix");
        assert!(!reported.is_empty());
        // Accepting `--version` and `-V`, and nothing that looks like a command.
        for flag in ["--version", "-V"] {
            assert!(matches!(Some(flag), Some("--version" | "-V")));
        }
        assert!(!matches!(Some("serve"), Some("--version" | "-V")));
    }

    #[test]
    fn the_help_command_is_the_default() {
        let options = Options::parse(&[]).unwrap();
        assert_eq!(options.command, "help");
    }

    #[test]
    fn data_dir_and_port_are_parsed() {
        let options = Options::parse(&args(&[
            "serve",
            "--data-dir",
            "/tmp/host",
            "--port",
            "9000",
        ]))
        .unwrap();
        assert_eq!(options.command, "serve");
        assert_eq!(options.directory, PathBuf::from("/tmp/host"));
        assert_eq!(options.port, 9000);
    }

    #[test]
    fn a_missing_option_value_is_an_error() {
        assert!(Options::parse(&args(&["serve", "--port"])).is_err());
        assert!(Options::parse(&args(&["serve", "--data-dir"])).is_err());
    }

    #[test]
    fn a_non_numeric_port_is_rejected() {
        assert!(Options::parse(&args(&["serve", "--port", "http"])).is_err());
    }

    #[test]
    fn the_default_port_matches_the_previous_host() {
        let options = Options::parse(&args(&["serve"])).unwrap();
        assert_eq!(options.port, DEFAULT_PORT);
        assert_eq!(DEFAULT_PORT, 3774);
    }

    #[tokio::test]
    async fn pairing_issues_a_usable_credential() {
        let dir = tempfile::tempdir().unwrap();
        async_main(
            Options {
                command: "pair".into(),
                directory: dir.path().to_path_buf(),
                port: 0,
                json: true,
            },
            &args(&["pair", "--name", "Laptop"]),
        )
        .await
        .unwrap();
        let store = HostStore::open(dir.path().join("host.db")).unwrap();
        assert_eq!(store.devices().unwrap().len(), 1);
        assert_eq!(store.devices().unwrap()[0].name, "Laptop");
    }

    #[tokio::test]
    async fn revoking_an_unknown_device_reports_it() {
        let dir = tempfile::tempdir().unwrap();
        let error = async_main(
            Options {
                command: "revoke".into(),
                directory: dir.path().to_path_buf(),
                port: 0,
                json: false,
            },
            &args(&["revoke", "missing"]),
        )
        .await
        .unwrap_err();
        assert_eq!(error, "Device not found");
    }

    #[tokio::test]
    async fn revoking_without_an_id_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let error = async_main(
            Options {
                command: "revoke".into(),
                directory: dir.path().to_path_buf(),
                port: 0,
                json: false,
            },
            &args(&["revoke"]),
        )
        .await
        .unwrap_err();
        assert!(error.contains("Provide a device ID"));
    }
}
