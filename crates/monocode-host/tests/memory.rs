//! Whether repeated turns leak memory.
//!
//! A remote host runs for weeks, so a leak of even a few kilobytes per turn
//! becomes gigabytes. This measures resident memory across many turns and fails
//! if it keeps climbing.
//!
//! The threshold is deliberately loose. Allocators do not return freed memory to
//! the operating system immediately, and a test that fails on noise gets
//! disabled — which is worse than one that catches only real growth.

use monocode_host::turn::{run_turn, Projection, Turn};
use monocode_host::HostSessionFixture;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// Resident set size in bytes, read from the kernel rather than estimated.
fn resident_bytes() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kilobytes: u64 = rest
                .trim()
                .trim_end_matches(" kB")
                .trim()
                .parse()
                .unwrap_or(0);
            return kilobytes * 1024;
        }
    }
    0
}

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

/// Many turns in a row must not grow resident memory without bound.
///
/// Skipped when `/proc` is unavailable, because without a real measurement the
/// test would pass regardless of the outcome.
#[test]
fn repeated_turns_do_not_leak() {
    if resident_bytes() == 0 {
        eprintln!("skipping: /proc/self/status is unavailable");
        return;
    }
    let (dir, binary) = fake_provider(
        r#"
for line in sys.stdin:
    text = "a" * 4000
    sys.stdout.write(json.dumps({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}}) + "\n")
    sys.stdout.write(json.dumps({"type":"result","subtype":"success","result":"done","session_id":"p"}) + "\n")
    sys.stdout.flush()
    break
"#,
    );

    let run = |session: &str| {
        let turn = Turn::new(format!("run-{session}"));
        // A fresh projection each time, as a new session would have.
        let projection = Projection::new(HostSessionFixture::new(session).build());
        let _ = run_turn(
            &turn,
            request(binary.clone(), dir.path().to_path_buf()),
            projection,
            |_| Ok(()),
        );
    };

    // Warm up first: the first turns allocate thread stacks, page tables and
    // lookup tables that are not leaks and would otherwise be counted.
    for index in 0..10 {
        run(&format!("warm-{index}"));
    }
    let baseline = resident_bytes();
    for index in 0..60 {
        run(&format!("measure-{index}"));
    }
    let after = resident_bytes();

    // A growth of over 64 MiB across 60 turns is far beyond allocator slack and
    // would mean several megabytes leaking per turn.
    let growth = after.saturating_sub(baseline);
    // Printing the measurement makes a silent regression visible in CI logs,
    // where a bare "ok" would hide that nothing was observed.
    println!(
        "memory: {:.1} MiB baseline, {:.1} MiB after 60 turns (+{:.1} MiB)",
        baseline as f64 / 1024.0 / 1024.0,
        after as f64 / 1024.0 / 1024.0,
        growth as f64 / 1024.0 / 1024.0
    );
    assert!(
        growth < 64 * 1024 * 1024,
        "memory grew by {:.1} MiB over 60 turns ({:.1} MiB to {:.1} MiB)",
        growth as f64 / 1024.0 / 1024.0,
        baseline as f64 / 1024.0 / 1024.0,
        after as f64 / 1024.0 / 1024.0
    );
}
