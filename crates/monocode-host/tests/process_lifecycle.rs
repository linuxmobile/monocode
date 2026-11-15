//! Process lifetime edge cases the turn loop must survive.
//!
//! Each test drives a real provider process, because the failures being guarded
//! against live in process handling rather than in parsing: a cancelled agent
//! that leaves its children running, a provider that dies mid-stream, and two
//! sessions whose output must not cross.

use monocode_host::turn::{run_turn, Projection, Turn};
use monocode_host::HostSessionFixture;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

fn fake_provider(body: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("claude");
    std::fs::write(
        &path,
        format!("#!/usr/bin/env python3\nimport json, sys\n{body}"),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    (dir, path)
}

fn request(binary: PathBuf, cwd: PathBuf) -> monocode_core::provider::Request {
    monocode_core::provider::Request {
        kind: monocode_core::provider::Kind::Claude,
        binary,
        cwd,
        prompt: "hello".into(),
        provider_id: None,
        model: None,
        settings: Default::default(),
    }
}

/// True while a process id still exists.
fn alive(pid: i32) -> bool {
    // Signal 0 performs the permission and existence checks without delivering.
    unsafe { kill(pid, 0) == 0 }
}

extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

/// Cancelling a turn must not leave the agent's children running.
///
/// An agent that spawns a helper is normal — a shell tool call, a language
/// server. If cancellation only kills the direct child, those helpers keep the
/// machine busy and hold the working directory, and nothing ever cleans them up.
#[test]
fn cancelling_reaps_descendant_processes() {
    let marker = tempfile::tempdir().unwrap();
    let pid_file = marker.path().join("child.pid");
    let (dir, binary) = fake_provider(&format!(
        r#"
import subprocess, time
# A long-lived grandchild, standing in for a tool the agent started.
helper = subprocess.Popen(["sleep", "120"])
open({pid_file:?}, "w").write(str(helper.pid))
sys.stdout.write(json.dumps({{"type":"stream_event","event":{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"working"}}}}}}) + "\n")
sys.stdout.flush()
time.sleep(120)
"#,
        pid_file = pid_file.to_string_lossy()
    ));

    let turn = Turn::new("reap".into());
    let started = Instant::now();
    let cancel = turn.cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        cancel.store(true, Ordering::Relaxed);
    });
    let projection = Projection::new(HostSessionFixture::new("reap").build());
    let _ = run_turn(
        &turn,
        request(binary, dir.path().to_path_buf()),
        projection,
        |_| Ok(()),
    );
    let _ = canceller.join();
    let turn_elapsed = started.elapsed();

    // The turn must end promptly after cancellation. Without this bound the
    // test also passes when cancellation does nothing and the provider simply
    // times out on its own after two minutes, which proves nothing.
    assert!(
        turn_elapsed < Duration::from_secs(15),
        "cancellation took {turn_elapsed:?}; the provider was allowed to finish by itself"
    );

    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("the provider never started its helper")
        .trim()
        .parse()
        .expect("the helper pid was not a number");

    // Reaping is asynchronous, so allow a moment before declaring a leak.
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if alive(pid) {
        // Do not leave a stray process behind for the rest of the test run.
        unsafe { kill(pid, 9) };
        panic!("cancelling the turn left the agent's child ({pid}) running");
    }
}

/// A provider killed mid-stream must end the turn, not hang it.
///
/// The real failure this guards is a loop that waits for a result event that
/// will never arrive, leaving the session stuck as running forever.
#[test]
fn a_provider_killed_mid_stream_ends_the_turn() {
    let (dir, binary) = fake_provider(
        r#"
for line in sys.stdin:
    sys.stdout.write(json.dumps({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial"}}}) + "\n")
    sys.stdout.flush()
    # Die without a result event, as an OOM kill or a segfault would.
    import os, signal
    os.kill(os.getpid(), signal.SIGKILL)
"#,
    );
    let turn = Turn::new("killed".into());
    let projection = Projection::new(HostSessionFixture::new("killed").build());
    let started = Instant::now();
    let result = run_turn(
        &turn,
        request(binary, dir.path().to_path_buf()),
        projection,
        |_| Ok(()),
    );
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "a killed provider hung the turn for {:?}",
        started.elapsed()
    );
    // The turn may end with an error or a recorded failure; what it must never
    // do is report a clean, idle session as though the reply finished.
    if let Ok(finished) = result {
        let session = finished.session();
        assert_ne!(
            session.status, "idle",
            "a provider killed mid-stream was recorded as a completed turn"
        );
    }
}

/// Two turns running at once must not mix their output.
///
/// Sessions share one host process, so a projection that keeps global state
/// would interleave two conversations. This runs them concurrently and checks
/// each session contains only its own text.
#[test]
fn concurrent_turns_keep_their_output_separate() {
    let marker = "MARK";
    let run = |label: &'static str| {
        let (directory, binary) = fake_provider(&format!(
            r#"
for line in sys.stdin:
    for chunk in ["alpha-{label}", "beta-{label}"]:
        sys.stdout.write(json.dumps({{"type":"stream_event","event":{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":chunk}}}}}}) + "\n")
        sys.stdout.flush()
        import time; time.sleep(0.05)
    sys.stdout.write(json.dumps({{"type":"result","subtype":"success","result":"done","session_id":"p-{label}"}}) + "\n")
    sys.stdout.flush()
    break
"#,
            label = label
        ));
        (directory, binary)
    };

    let handles: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|label| {
            let name = format!("{marker}-{label}");
            std::thread::spawn(move || {
                let (directory, binary) = run(label);
                let turn = Turn::new(format!("run-{label}"));
                let projection = Projection::new(HostSessionFixture::new(&name).build());
                let finished = run_turn(
                    &turn,
                    request(binary, directory.path().to_path_buf()),
                    projection,
                    |_| Ok(()),
                )
                .expect("a concurrent turn should finish");
                let blocks = finished.session().session["blocks"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let text: String = blocks
                    .iter()
                    .filter_map(|block| block["text"].as_str())
                    .collect();
                (label, text)
            })
        })
        .collect();

    for handle in handles {
        let (label, text) = handle.join().expect("a turn thread panicked");
        let other = if label == "one" { "two" } else { "one" };
        assert!(
            text.contains(&format!("alpha-{label}")),
            "{label} lost its own output: {text}"
        );
        // The decisive assertion: no text from the other session appears.
        assert!(
            !text.contains(other),
            "{label} contains text from {other}: {text}"
        );
    }
}

/// A provider that prints nothing and exits cleanly must still settle.
#[test]
fn a_silent_provider_settles_instead_of_hanging() {
    let (dir, binary) = fake_provider("sys.exit(0)\n");
    let turn = Turn::new("silent".into());
    let projection = Projection::new(HostSessionFixture::new("silent").build());
    let started = Instant::now();
    let result = run_turn(
        &turn,
        request(binary, dir.path().to_path_buf()),
        projection,
        |_| Ok(()),
    );
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "a silent provider hung the turn"
    );
    // Ending is required; whether it is an error is the loop's choice.
    let _ = result;
}
