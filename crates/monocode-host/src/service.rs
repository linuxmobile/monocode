//! Keeping the host running after the terminal that started it closes.
//!
//! A remote desktop expects this machine to stay reachable. That means the host
//! must survive an SSH disconnect and start again after a reboot, without
//! needing root. Two mechanisms are used, in order of preference:
//!
//! 1. **A systemd user unit**, when systemd is the init system. This is the only
//!    option that starts the host at boot, and it lets the service manager own
//!    restarts and shutdown.
//! 2. **A detached process**, when it is not. Containers and microVMs often boot
//!    without systemd, and requiring it would make the host unusable there.
//!
//! Lingering (`loginctl enable-linger`) is required for a user unit to keep
//! running after logout. Enabling it usually needs no privileges, but where it
//! does the error says exactly what an administrator must run, rather than
//! failing with something the user cannot act on.
//!
//! Nothing here deletes the data directory. Sessions, logs and device
//! credentials survive an uninstall until the user removes them.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The unit name, shared by install, uninstall and status.
pub const UNIT: &str = "monocode-host.service";

/// What `connectionInfo` reports once the host answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub port: u16,
    pub pid: u32,
}

/// What the caller needs to know to reach a freshly started host.
#[derive(Debug, Clone)]
pub struct ServiceOptions {
    pub directory: PathBuf,
    pub port: u16,
    /// The running executable, so the unit starts this build.
    pub executable: PathBuf,
}

/// How the host was made to persist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Systemd,
    Detached,
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Systemd => "systemd",
            Self::Detached => "detached",
        })
    }
}

/// True when systemd is the init system.
///
/// `sd_booted(3)` is exactly this directory check; it also correctly reports
/// false inside a container that booted without systemd.
pub fn systemd_booted() -> bool {
    systemd_booted_at(Path::new("/"))
}

fn systemd_booted_at(root: &Path) -> bool {
    root.join("run/systemd/system").exists()
}

/// Quotes one value for a unit file.
///
/// systemd expands `%` specifiers in every setting but `$` only in `ExecStart`,
/// so both must be escaped to survive literally. A newline would end the line
/// and inject a directive, so it becomes `\n`.
pub fn unit_quote(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('\n', "\\n");
    format!("\"{escaped}\"")
}

/// The user unit that runs `serve`.
pub fn systemd_unit(options: &ServiceOptions, path: &str) -> String {
    let executable = options.executable.to_string_lossy();
    // `$` is a variable reference in ExecStart, so it is doubled to stay literal.
    let dollar = |value: &str| value.replace('$', "$$");
    let exec = [
        dollar(&executable),
        "serve".to_string(),
        "--data-dir".to_string(),
        dollar(&options.directory.to_string_lossy()),
        "--port".to_string(),
        options.port.to_string(),
    ]
    .map(|value| unit_quote(&value))
    .join(" ");
    format!(
        "[Unit]
Description=MonoCode Host
After=network.target

[Service]
ExecStart={exec}
Environment={}
Restart=on-failure
RestartSec=5
UMask=0077
KillMode=control-group
TimeoutStopSec=20

[Install]
WantedBy=default.target
",
        unit_quote(&format!("PATH={path}"))
    )
}

/// The `PATH` a service should use.
///
/// Enshrining the current `PATH` is what makes an agent CLI installed in
/// `~/.local/bin` reachable from the service, which does not inherit a login
/// shell's environment.
pub fn service_path() -> String {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut entries: Vec<String> = Vec::new();
    let mut push = |value: String| {
        if !value.is_empty() && !entries.contains(&value) {
            entries.push(value);
        }
    };
    for entry in std::env::var("PATH").unwrap_or_default().split(':') {
        push(entry.to_string());
    }
    push(home.join(".local/bin").to_string_lossy().into_owned());
    for fallback in ["/usr/local/bin", "/usr/bin", "/bin"] {
        push(fallback.to_string());
    }
    entries.join(":")
}

/// The username and uid of the current user, for `loginctl` and the user bus.
fn current_user() -> Result<(String, u32), String> {
    let username = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .map_err(|_| "Cannot determine the current user".to_string())?;
    // `id -u` is the portable way to get the uid without a libc call.
    let output = Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| format!("Could not determine the user id: {e}"))?;
    let uid = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| "Could not determine the user id".to_string())?;
    Ok((username, uid))
}

/// `systemctl --user` needs bus variables that an SSH session may lack.
fn user_environment(uid: u32) -> Vec<(String, String)> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{uid}"));
    let bus = std::env::var("DBUS_SESSION_BUS_ADDRESS")
        .unwrap_or_else(|_| format!("unix:path={runtime}/bus"));
    vec![
        ("XDG_RUNTIME_DIR".to_string(), runtime),
        ("DBUS_SESSION_BUS_ADDRESS".to_string(), bus),
    ]
}

fn run(command: &str, args: &[&str], env: &[(String, String)]) -> Result<String, String> {
    let mut invocation = Command::new(command);
    invocation.args(args);
    for (key, value) in env {
        invocation.env(key, value);
    }
    let output = invocation
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Could not run {command}: {e}"))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if message.is_empty() {
            format!("{command} failed")
        } else {
            message
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Reads `running.json` and confirms the host behind it answers.
///
/// A state file alone proves nothing: the process may have died without
/// cleaning it up, so the lifecycle endpoint is asked too.
pub fn connection_info(directory: &Path) -> Result<Connection, String> {
    let text = std::fs::read_to_string(directory.join("running.json"))
        .map_err(|_| "The host is not running".to_string())?;
    let state: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| "Invalid host state".to_string())?;
    let port = state
        .get("port")
        .and_then(serde_json::Value::as_u64)
        .filter(|port| (1..=65535).contains(port))
        .ok_or_else(|| "Invalid host state".to_string())? as u16;
    let secret = state
        .get("secret")
        .and_then(serde_json::Value::as_str)
        .filter(|secret| {
            secret.len() == 43
                && secret
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .ok_or_else(|| "Invalid host state".to_string())?;
    let pid = state
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;

    // A short timeout: this runs while a user waits for a connect.
    let response = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .post(&format!("http://127.0.0.1:{port}/lifecycle"))
        .set("Authorization", &format!("Bearer {secret}"))
        .send_string(r#"{"action":"status"}"#);
    match response {
        Ok(_) => Ok(Connection { port, pid }),
        Err(_) => Err("Host is not ready".into()),
    }
}

/// Sends a lifecycle action, used for `stop`.
pub fn lifecycle(directory: &Path, action: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(directory.join("running.json"))
        .map_err(|_| "The host is not running".to_string())?;
    let state: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| "Invalid host state".to_string())?;
    let port = state
        .get("port")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "Invalid host state".to_string())?;
    let secret = state
        .get("secret")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "Invalid host state".to_string())?;
    ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .post(&format!("http://127.0.0.1:{port}/lifecycle"))
        .set("Authorization", &format!("Bearer {secret}"))
        .send_string(&format!(r#"{{"action":"{action}"}}"#))
        .map(|_| ())
        .map_err(|e| format!("Could not reach the host: {e}"))
}

/// Starts the host in its own session so it outlives this process.
///
/// Nothing supervises it in this mode, which is why every install attempt
/// starts it only when it is not already answering.
pub fn start_detached(options: &ServiceOptions, path: &str) -> Result<u32, String> {
    use std::os::unix::process::CommandExt as _;
    let log_path = options.directory.join("host.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("open {}: {e}", log_path.display()))?;
    let stderr = log.try_clone().map_err(|e| e.to_string())?;
    let mut command = Command::new(&options.executable);
    command
        .arg("serve")
        .arg("--data-dir")
        .arg(&options.directory)
        .arg("--port")
        .arg(options.port.to_string())
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr));
    // `setsid` detaches from the controlling terminal, so the host survives the
    // SSH session that started it.
    unsafe {
        command.pre_exec(|| {
            if libc_setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command
        .spawn()
        .map_err(|e| format!("Could not start the host: {e}"))?;
    Ok(child.id())
}

/// `setsid(2)`, called through `libc` so no external tool is needed.
fn libc_setsid() -> i32 {
    extern "C" {
        fn setsid() -> i32;
    }
    unsafe { setsid() }
}

/// Installs and starts the host, reusing one that already answers.
pub fn install_service(options: &ServiceOptions) -> Result<(Connection, Mode), String> {
    // Never replace a running host: connecting must not interrupt agent turns.
    if let Ok(connection) = connection_info(&options.directory) {
        let mode = if systemd_installed() {
            Mode::Systemd
        } else {
            Mode::Detached
        };
        return Ok((connection, mode));
    }
    let path = service_path();
    let mode = if !systemd_booted() {
        start_detached(options, &path)?;
        Mode::Detached
    } else {
        install_systemd(options, &path)?;
        Mode::Systemd
    };
    for _ in 0..50 {
        if let Ok(connection) = connection_info(&options.directory) {
            return Ok((connection, mode));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Err(match mode {
        Mode::Systemd => format!(
            "The host service was installed but did not start. Check {}.",
            options.directory.join("host.log").display()
        ),
        Mode::Detached => format!(
            "The host did not start. Check {}.",
            options.directory.join("host.log").display()
        ),
    })
}

fn unit_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_string())?;
    Ok(home.join(".config/systemd/user").join(UNIT))
}

fn systemd_installed() -> bool {
    unit_path().map(|path| path.exists()).unwrap_or(false)
}

fn install_systemd(options: &ServiceOptions, path: &str) -> Result<(), String> {
    let (username, uid) = current_user()?;
    let env = user_environment(uid);
    // Lingering is what keeps a user unit alive once the last session ends.
    let linger = run(
        "loginctl",
        &["enable-linger", &username, "--no-ask-password"],
        &env,
    )
    .and_then(|_| {
        run(
            "loginctl",
            &["show-user", &username, "--property=Linger", "--value"],
            &env,
        )
    });
    match linger {
        Ok(value) if value.trim() == "yes" => {}
        _ => {
            return Err(format!(
                "This host needs systemd user services and lingering to keep sessions running after SSH disconnects. An administrator can enable it with: sudo loginctl enable-linger {username}"
            ))
        }
    }
    let file = unit_path()?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    // The unit names a directory and port, so a unit left from an earlier
    // install may point somewhere else entirely. Rewriting keeps the service
    // matching what was just asked for; a user who edits the unit is expected
    // to stop editing it and pass the flags instead.
    let unit = systemd_unit(options, path);
    let unchanged = std::fs::read_to_string(&file)
        .map(|old| old == unit)
        .unwrap_or(false);
    if !unchanged {
        std::fs::write(&file, &unit).map_err(|e| format!("write {}: {e}", file.display()))?;
        // The unit may name a credential path, so keep it private.
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
    }
    run("systemctl", &["--user", "daemon-reload"], &env)?;
    run("systemctl", &["--user", "enable", UNIT], &env)?;
    // `enable --now` leaves an already-active unit running with its old
    // arguments, so a changed unit is restarted explicitly.
    if unchanged {
        run("systemctl", &["--user", "start", UNIT], &env)?;
    } else {
        run("systemctl", &["--user", "restart", UNIT], &env)?;
    }
    Ok(())
}

/// Removes the unit so the host no longer starts, and stops it.
///
/// The data directory is never touched: sessions, logs and credentials are the
/// user's, and deleting them on uninstall would be data loss.
pub fn uninstall_service() -> Result<Vec<String>, String> {
    let mut notes = Vec::new();
    if !systemd_booted() {
        // Nothing was registered; the caller stops any running host itself.
        return Ok(notes);
    }
    let (username, uid) = current_user()?;
    let env = user_environment(uid);
    // Both may fail when nothing was ever installed; that is not an error.
    let _ = run("systemctl", &["--user", "disable", "--now", UNIT], &env);
    if let Ok(file) = unit_path() {
        let _ = std::fs::remove_file(file);
    }
    let _ = run("systemctl", &["--user", "daemon-reload"], &env);
    notes.push(format!(
        "Lingering is still enabled for {username}; other user services may rely on it. To turn it off: loginctl disable-linger {username}"
    ));
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> ServiceOptions {
        ServiceOptions {
            directory: PathBuf::from("/home/user/.monocode-host"),
            port: 3774,
            executable: PathBuf::from("/usr/bin/monocode-host"),
        }
    }

    #[test]
    fn a_detached_directory_without_marker_is_not_systemd() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!systemd_booted_at(dir.path()));
        std::fs::create_dir_all(dir.path().join("run/systemd/system")).unwrap();
        assert!(systemd_booted_at(dir.path()));
    }

    #[test]
    fn the_unit_runs_serve_with_this_directory_and_port() {
        let unit = systemd_unit(&options(), "/usr/bin:/bin");
        assert!(unit.contains("ExecStart=\"/usr/bin/monocode-host\" \"serve\""));
        assert!(unit.contains("\"/home/user/.monocode-host\""));
        assert!(unit.contains("\"3774\""));
        assert!(unit.contains("Restart=on-failure"));
        // A world-readable unit could expose the PATH but not secrets; still,
        // the file is written 0600 and the umask keeps logs private.
        assert!(unit.contains("UMask=0077"));
    }

    #[test]
    fn a_quote_or_percent_in_a_path_cannot_break_the_unit() {
        // `%` is a specifier and `"` ends the value, so both must be escaped.
        let quoted = unit_quote("a\"b%c\\d\ne");
        assert_eq!(quoted, "\"a\\\"b%%c\\\\d\\ne\"");
        assert!(!quoted.contains('\n'));
    }

    #[test]
    fn a_dollar_in_execstart_is_literal() {
        let mut options = options();
        options.directory = PathBuf::from("/home/user/$weird");
        let unit = systemd_unit(&options, "/bin");
        // `$` would be expanded as a variable, so it is doubled.
        assert!(unit.contains("$$weird"), "got: {unit}");
    }

    #[test]
    fn the_service_path_always_contains_the_essentials() {
        let path = service_path();
        for expected in ["/usr/bin", "/bin"] {
            assert!(path.contains(expected), "{path} is missing {expected}");
        }
        // No entry appears twice, which would be harmless but untidy.
        let entries: Vec<&str> = path.split(':').collect();
        let mut unique = entries.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(entries.len(), unique.len(), "duplicate entries in {path}");
    }

    #[test]
    fn a_missing_state_file_reports_the_host_is_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let error = connection_info(dir.path()).unwrap_err();
        assert!(error.contains("not running"), "got: {error}");
    }

    #[test]
    fn a_malformed_state_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("running.json"), "{not json").unwrap();
        assert_eq!(
            connection_info(dir.path()).unwrap_err(),
            "Invalid host state"
        );
    }

    #[test]
    fn a_state_file_with_a_bad_secret_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        for secret in ["short", "", &"x".repeat(44)] {
            std::fs::write(
                dir.path().join("running.json"),
                serde_json::json!({ "port": 3774, "pid": 1, "secret": secret }).to_string(),
            )
            .unwrap();
            assert_eq!(
                connection_info(dir.path()).unwrap_err(),
                "Invalid host state"
            );
        }
    }

    #[test]
    fn a_state_file_pointing_at_nothing_reports_not_ready() {
        let dir = tempfile::tempdir().unwrap();
        // Port 1 will not answer; the state file alone must not be trusted.
        std::fs::write(
            dir.path().join("running.json"),
            serde_json::json!({ "port": 1, "pid": 1, "secret": "x".repeat(43) }).to_string(),
        )
        .unwrap();
        let error = connection_info(dir.path()).unwrap_err();
        assert!(error.contains("not ready"), "got: {error}");
    }
}
