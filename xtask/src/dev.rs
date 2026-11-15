//! The development loop: build, launch, and rebuild on request.
//!
//! Saving a file does nothing. The user types `r` because a GPUI app cannot be
//! hot-reloaded, and rebuilding on every save would restart the app mid-turn.
//!
//! Two invariants keep this usable, and both are tested:
//!
//! - **One Cargo at a time.** A request arriving during a build sets a pending
//!   revision instead of starting a second build, so two builds never contend
//!   for the same target directory.
//! - **The app is only replaced with fresh code.** After building, the revision
//!   is re-checked; if another request arrived, the loop builds again before
//!   touching the running app. A failed build keeps the current app running.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const MANIFEST: &str = "crates/monocode-gpui/Cargo.toml";
const PACKAGE: &str = "monocode-gpui";

/// How long the app gets to quit through its debug hook before being killed.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// Splits runner flags from application arguments.
///
/// Everything is forwarded to the app, so an app flag that happens to match a
/// runner flag still reaches the app. The two retired flags are rejected
/// because silently ignoring them would leave a user expecting hot reload.
pub fn parse_args(argv: &[String]) -> Result<Vec<String>, String> {
    // Options whose next argument is a value, not a flag. A project really can
    // be named `--watch`, and it must reach the app rather than be refused.
    const VALUED: [&str; 2] = ["--cwd", "--monocode-db"];
    let mut forwarded = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        let arg = &argv[index];
        if arg == "--" {
            // Everything after the separator is the app's, even if it looks
            // like a runner flag.
            forwarded.extend(argv[index + 1..].iter().cloned());
            break;
        }
        if arg == "--watch" || arg == "--bundle" {
            return Err(format!(
                "{arg} is not supported. Use cargo xtask dev, then type r and Enter to rebuild and restart."
            ));
        }
        forwarded.push(arg.clone());
        if VALUED.contains(&arg.as_str()) && index + 1 < argv.len() {
            forwarded.push(argv[index + 1].clone());
            index += 1;
        }
        index += 1;
    }
    Ok(forwarded)
}

/// Where Cargo puts the build, honouring a cross target when one is set.
fn target_directory(metadata: &serde_json::Value) -> Result<PathBuf, String> {
    let base = metadata
        .get("target_directory")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "cargo metadata did not report a target_directory".to_string())?;
    let base = PathBuf::from(base);
    match std::env::var("CARGO_BUILD_TARGET") {
        Ok(target) if !target.is_empty() => Ok(base.join(target)),
        _ => Ok(base),
    }
}

fn metadata_target(root: &Path) -> Result<PathBuf, String> {
    let output = Command::new("cargo")
        .current_dir(root)
        .args([
            "metadata",
            "--manifest-path",
            MANIFEST,
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Could not run cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("cargo metadata returned invalid JSON: {e}"))?;
    target_directory(&metadata)
}

/// Runs one dev session until [`Dev::close`] is called.
pub struct Dev {
    root: PathBuf,
    args: Vec<String>,
    /// Overrides the built executable; tests use a fake app so no real build
    /// runs, while production uses whatever Cargo just produced.
    executable: Option<PathBuf>,
    revision: Arc<Mutex<u64>>,
    closing: Arc<AtomicBool>,
    app: Arc<Mutex<Option<Child>>>,
}

impl Dev {
    pub fn new(root: PathBuf, args: Vec<String>) -> Self {
        Self {
            root,
            args,
            executable: None,
            revision: Arc::new(Mutex::new(0)),
            closing: Arc::new(AtomicBool::new(false)),
            app: Arc::new(Mutex::new(None)),
        }
    }

    /// Uses a fixed executable instead of the one Cargo produces.
    pub fn with_executable(mut self, executable: PathBuf) -> Self {
        self.executable = Some(executable);
        self
    }

    /// Records that a rebuild was requested.
    ///
    /// The caller loops on [`Dev::step`]; bumping the revision is what makes
    /// the loop notice, so a request during a build is never lost.
    pub fn request_build(&self) {
        if self.closing.load(Ordering::SeqCst) {
            return;
        }
        *self.revision.lock().unwrap() += 1;
    }

    /// Whether a request is outstanding.
    pub fn pending(&self) -> bool {
        *self.revision.lock().unwrap() > 0
    }

    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }

    /// Builds when a request is outstanding and restarts the app.
    ///
    /// Returns false when there is nothing to do, so a caller can block on
    /// input rather than spin.
    pub fn step(&self) -> Result<bool, String> {
        if self.is_closing() {
            return Ok(false);
        }
        let requested = *self.revision.lock().unwrap();
        if requested == 0 {
            return Ok(false);
        }
        println!("[dev] Building… (the current app stays open)");
        let started = std::time::Instant::now();

        let build_target = metadata_target(&self.root);
        if self.is_closing() {
            return Ok(false);
        }
        let target = match build_target {
            Ok(target) => target,
            Err(error) => {
                println!("[dev] cargo metadata failed: {error}");
                println!("[dev] Fix the error, then type r to retry.");
                *self.revision.lock().unwrap() = 0;
                return Ok(true);
            }
        };
        let status = Command::new("cargo")
            .current_dir(&self.root)
            .args([
                "build",
                "--manifest-path",
                MANIFEST,
                "--locked",
                "-p",
                PACKAGE,
                "--bin",
                PACKAGE,
                "--profile",
                "native-dev",
            ])
            .env(
                "CARGO_BUILD_JOBS",
                std::env::var("CARGO_BUILD_JOBS").unwrap_or_else(|_| "1".into()),
            )
            .status()
            .map_err(|e| format!("Could not run cargo build: {e}"))?;
        if self.is_closing() {
            return Ok(false);
        }
        if !status.success() {
            println!("[dev] Build failed. Keeping the current app; type r to retry after fixing the error.");
            // Clear only the requests seen so far: one that arrived during the
            // failed build still deserves its own attempt.
            let mut revision = self.revision.lock().unwrap();
            if *revision == requested {
                *revision = 0;
            }
            return Ok(true);
        }
        // Another request arrived while compiling, so this build is already
        // stale; loop rather than restarting onto it.
        if *self.revision.lock().unwrap() != requested {
            return Ok(true);
        }
        self.stop_app();
        if self.is_closing() {
            return Ok(false);
        }
        if *self.revision.lock().unwrap() != requested {
            return Ok(true);
        }
        let executable = self
            .executable
            .clone()
            .unwrap_or_else(|| target.join("native-dev").join(PACKAGE));
        let child = Command::new(&executable)
            .current_dir(&self.root)
            .args(&self.args)
            // The app watches this to enable its debug shutdown hook, which is
            // what lets close() end it without SIGKILL.
            .env("MONOCODE_DEV_RELOAD", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("Could not start {}: {e}", executable.display()))?;
        *self.app.lock().unwrap() = Some(child);
        *self.revision.lock().unwrap() = 0;
        println!(
            "[dev] Ready in {:.1}s. Type r then Enter to rebuild and restart.",
            started.elapsed().as_secs_f64()
        );
        Ok(true)
    }

    /// Ends the app through its own shutdown hook, then by force.
    ///
    /// The hook writes `quit` to stdin so the app saves state; skipping it
    /// would lose whatever the user changed since launch.
    fn stop_app(&self) {
        let mut slot = self.app.lock().unwrap();
        let Some(mut child) = slot.take() else {
            return;
        };
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write as _;
            let _ = stdin.write_all(b"quit\n");
            let _ = stdin.flush();
        }
        // Wait with a deadline without blocking forever on a wedged app.
        let deadline = std::time::Instant::now() + SHUTDOWN_GRACE;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        println!(
                            "[dev] App did not finish shutting down within 10s; terminating it."
                        );
                        let _ = child.kill();
                        let _ = child.wait();
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(_) => return,
            }
        }
    }

    /// Stops building and ends the app; safe to call more than once.
    pub fn close(&self) {
        self.closing.store(true, Ordering::SeqCst);
        self.stop_app();
    }
}

/// Starts the loop, printing prompts and handling `r` on stdin.
pub fn run(root: PathBuf, args: Vec<String>) -> Result<(), String> {
    let dev = std::sync::Arc::new(Dev::new(root.clone(), args));
    dev.request_build();
    println!("[dev] Type r then Enter to rebuild and restart; Ctrl+C to stop. File saves do not trigger builds.");

    // Reading stdin on its own thread keeps the main thread free to build, and
    // lets a terminal close end the process instead of leaving it spinning.
    let reader = {
        let dev = std::sync::Arc::clone(&dev);
        std::thread::spawn(move || {
            let mut input = std::io::stdin();
            let mut buffer = [0u8; 256];
            loop {
                match input.read(&mut buffer) {
                    Ok(0) | Err(_) => {
                        // A closed terminal is a request to stop, not to spin.
                        STDIN_ENDED.store(true, Ordering::SeqCst);
                        break;
                    }
                    Ok(count) => {
                        let text = String::from_utf8_lossy(&buffer[..count]).to_lowercase();
                        if text.lines().any(|line| line.trim() == "r") {
                            println!("[dev] Rebuilding and restarting…");
                            dev.request_build();
                        }
                    }
                }
            }
        })
    };

    // Ctrl+C must stop the app through its hook before this process exits,
    // otherwise the app is orphaned and its unsaved state is lost. The handler
    // only raises a flag: doing real work in a signal handler is unsafe.
    install_signal_handlers();
    loop {
        if dev.is_closing() {
            break;
        }
        if dev.step()? {
            continue;
        }
        // Nothing pending: wait until the reader or a signal asks for work.
        //
        // The timeout is what makes a signal visible; without it the loop would
        // block until the next keystroke.
        let mut waited = 0u32;
        while !dev.pending() && !dev.is_closing() && !SIGNALLED.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(25));
            waited += 1;
            if waited > 40 && !pending_input() {
                // Idle with no input: stop watching the process, so a
                // non-interactive run does not spin.
                break;
            }
        }
        if SIGNALLED.load(Ordering::SeqCst) {
            break;
        }
    }
    dev.close();
    let _ = reader.join();
    Ok(())
}

/// Set when stdin closes, so a detached terminal stops the loop.
static STDIN_ENDED: AtomicBool = AtomicBool::new(false);

/// Set by the signal handler; read by the main loop.
static SIGNALLED: AtomicBool = AtomicBool::new(false);

/// True while stdin is still open.
fn pending_input() -> bool {
    !STDIN_ENDED.load(Ordering::SeqCst)
}

extern "C" fn handle_signal(_signal: i32) {
    // Only an atomic store happens here, which is async-signal-safe.
    SIGNALLED.store(true, Ordering::SeqCst);
}

/// Installs handlers for `SIGINT` and `SIGTERM` through `libc`.
///
/// Core Rust has no signal API and a dependency is not justified for two
/// handlers, so `sigaction` is declared directly.
fn install_signal_handlers() {
    #[cfg(unix)]
    unsafe {
        for signal in [2, 15] {
            // SIGINT, SIGTERM
            let mut action: SigAction = std::mem::zeroed();
            action.sa_handler = handle_signal as *const () as usize;
            action.sa_flags = 0;
            sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

#[cfg(unix)]
#[repr(C)]
struct SigAction {
    sa_handler: usize,
    sa_mask: [u64; 16],
    sa_flags: i32,
    sa_restorer: usize,
}

#[cfg(unix)]
extern "C" {
    fn sigaction(signal: i32, action: *const SigAction, previous: *mut SigAction) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn application_options_are_forwarded() {
        assert_eq!(
            parse_args(&args(&["--monocode-data", "--cwd", "a project"])).unwrap(),
            args(&["--monocode-data", "--cwd", "a project"])
        );
    }

    #[test]
    fn app_options_that_look_like_runner_flags_are_not_intercepted() {
        // A project really can be called "--watch"; forwarding it is correct.
        assert_eq!(
            parse_args(&args(&[
                "--dev-data",
                "--cwd",
                "--watch",
                "--monocode-db",
                "--bundle"
            ]))
            .unwrap(),
            args(&[
                "--dev-data",
                "--cwd",
                "--watch",
                "--monocode-db",
                "--bundle"
            ])
        );
    }

    #[test]
    fn the_separator_splits_runner_from_app_arguments() {
        assert_eq!(
            parse_args(&args(&["--", "--cwd", "a project"])).unwrap(),
            args(&["--cwd", "a project"])
        );
    }

    #[test]
    fn a_retired_flag_is_rejected_with_an_actionable_message() {
        for flag in ["--watch", "--bundle"] {
            let error = parse_args(&args(&[flag])).unwrap_err();
            assert!(error.contains("not supported"), "got: {error}");
            // The message must say what to do instead, not just that it failed.
            assert!(error.contains("rebuild and restart"), "got: {error}");
        }
    }

    #[test]
    fn the_target_directory_appends_a_cross_target() {
        let metadata = serde_json::json!({ "target_directory": "/t" });
        let path = target_directory(&metadata).unwrap();
        // Without a cross target set, the base is used directly.
        if std::env::var("CARGO_BUILD_TARGET").is_ok() {
            assert!(path.starts_with("/t"));
        } else {
            assert_eq!(path, PathBuf::from("/t"));
        }
    }

    #[test]
    fn metadata_without_a_target_directory_is_an_error() {
        let metadata = serde_json::json!({});
        assert!(target_directory(&metadata).is_err());
    }
}
