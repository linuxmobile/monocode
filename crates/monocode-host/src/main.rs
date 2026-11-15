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
    let options = Options::parse(&args)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("start the runtime: {e}"))?;
    runtime.block_on(async_main(options, &args))
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
  serve             Serve this machine to a paired desktop
  pair              Issue a device credential for a desktop
  devices           List paired devices
  revoke <id>       Revoke a paired device
  version           Print the version and protocol
  help              Show this help

Options:
  --data-dir <dir>  Data directory (default: ~/.monocode-host)
  --port <port>     Port to listen on (default: 3774)";

async fn serve(store: HostStore, options: Options) -> Result<(), String> {
    let environment_id = store.environment_id.clone();
    let providers = discover_providers();
    let (shutdown, _) = broadcast::channel(4);
    let (stop, mut stopped) = mpsc::channel::<()>(1);

    let admin_secret = store::random_token();
    let state = Arc::new(HostState {
        store: Arc::new(mutex::Mutex::new(store)),
        admin_secret: admin_secret.clone(),
        providers: providers.clone(),
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
    // restart on the same port does not race the old process.
    let _ = std::fs::remove_file(&state_path);
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
