//! Drives a real provider process through the turn loop.
//!
//! The provider here is a script that speaks just enough of Claude Code's
//! stream-json protocol to exercise the loop: a text delta, a tool call and a
//! final result. That proves the wiring — spawn, read, project, persist,
//! settle — without needing a real agent installed.

use monocode_host::turn::{run_turn, Projection, Turn};
use monocode_host::HostSessionFixture;
use std::os::unix::fs::PermissionsExt;

/// Writes a fake `claude` that emits one text block and finishes.
fn fake_provider(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("claude");
    let script = format!("#!/usr/bin/env python3\nimport json, sys\n{body}");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    (dir, path)
}

fn request(
    binary: std::path::PathBuf,
    cwd: std::path::PathBuf,
) -> monocode_core::provider::Request {
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

#[test]
fn a_turn_streams_text_into_the_session_and_settles() {
    let (dir, binary) = fake_provider(
        r#"
for line in sys.stdin:
    value = json.loads(line)
    if value.get("type") != "user":
        continue
    print(json.dumps({"type":"stream_event","event":{"type":"message_start","message":{"id":"m"}},"session_id":"prov-1"}), flush=True)
    print(json.dumps({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}), flush=True)
    print(json.dumps({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi there"}}}), flush=True)
    print(json.dumps({"type":"stream_event","event":{"type":"content_block_stop","index":0}}), flush=True)
    print(json.dumps({"type":"result","subtype":"success","result":"Hi there","session_id":"prov-1"}), flush=True)
    break
"#,
    );
    let turn = Turn::new("run-1".into());
    let projection = Projection::new(HostSessionFixture::new("s1").build());
    let mut writes = 0;
    let finished = run_turn(
        &turn,
        request(binary, dir.path().to_path_buf()),
        projection,
        |projection| {
            // Stand-in for the store write: prove the loop persists progress.
            writes += 1;
            assert!(
                projection.session().revision > 0,
                "a write carried no revision"
            );
            Ok(())
        },
    )
    .expect("the turn should finish");

    let session = finished.session();
    assert_eq!(
        session.status, "idle",
        "a finished turn must not stay running"
    );
    let blocks = session.session["blocks"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let text: String = blocks
        .iter()
        .filter(|block| block["role"] == "assistant")
        .filter_map(|block| block["text"].as_str())
        .collect();
    assert!(
        text.contains("Hi there"),
        "streamed text was lost: {blocks:?}"
    );
    assert!(
        blocks
            .iter()
            .all(|block| block["streaming"].as_bool() != Some(true)),
        "a settled turn left a block streaming"
    );
    assert!(writes > 0, "the loop never persisted anything");
}

#[test]
fn a_provider_that_exits_early_is_recorded_as_an_error() {
    let (dir, binary) = fake_provider(
        r#"
for line in sys.stdin:
    print(json.dumps({"type":"result","is_error":True,"session_id":"p"}), flush=True)
    sys.exit(3)
"#,
    );
    let turn = Turn::new("run-2".into());
    let projection = Projection::new(HostSessionFixture::new("s2").build());
    let result = run_turn(
        &turn,
        request(binary, dir.path().to_path_buf()),
        projection,
        |_| Ok(()),
    );
    // Either the provider reports failure or the loop does; what must not
    // happen is a session that looks finished and successful.
    if let Ok(finished) = result {
        let blocks = finished.session().session["blocks"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            blocks.iter().any(|block| block["role"] == "system"),
            "a failed turn left no explanation: {blocks:?}"
        );
    }
}

#[test]
fn cancelling_a_turn_marks_it_interrupted_rather_than_failed() {
    let (dir, binary) = fake_provider(
        r#"
for line in sys.stdin:
    print(json.dumps({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"working"}}}), flush=True)
    import time
    time.sleep(5)
    break
"#,
    );
    let turn = Turn::new("run-3".into());
    let handle = {
        let cancel = turn.cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        })
    };
    let projection = Projection::new(HostSessionFixture::new("s3").build());
    let finished = run_turn(
        &turn,
        request(binary, dir.path().to_path_buf()),
        projection,
        |_| Ok(()),
    )
    .expect("a cancelled turn is a normal end");
    let _ = handle.join();
    assert_eq!(
        finished.session().status,
        "interrupted",
        "cancellation must not look like a crash"
    );
}

/// Plan mode must change how approvals are answered.
///
/// A planning turn is supposed to inspect and propose without changing files.
/// The guard is that a permission request is denied while planning: if the flag
/// were dropped, the agent would start editing files while the user believed it
/// was only drafting a plan.
#[test]
fn plan_mode_denies_permission_requests() {
    let (dir, binary) = fake_provider(
        r#"
for line in sys.stdin:
    value = json.loads(line)
    kind = value.get("type", "")
    if kind == "control_request":
        # Ask for permission to write a file.
        print(json.dumps({"type":"control_request","request_id":"perm-1","request":{"subtype":"can_use_tool","tool_name":"Write","input":{"file_path":"x"},"permission_suggestions":[]}}), flush=True)
    elif kind == "control_response":
        # Record the answer the host gave, then finish.
        with open("answer", "a") as handle:
            handle.write(json.dumps(value) + "\n")
        print(json.dumps({"type":"result","subtype":"success","result":"ok","session_id":"p"}), flush=True)
        break
    else:
        print(json.dumps({"type":"result","subtype":"success","result":"ok","session_id":"p"}), flush=True)
        break
"#,
    );
    let mut request = request(binary, dir.path().to_path_buf());
    // Full access would normally approve a file change, so denying here can
    // only come from plan mode. Without this the test passes either way, since
    // the default access mode denies regardless.
    request.settings.access = monocode_core::agent::AccessMode::FullAccess;
    request.settings.plan = true;
    let turn = Turn::new("plan".into());
    let projection = Projection::new(HostSessionFixture::new("plan").build());
    let _ = run_turn(&turn, request, projection, |_| Ok(()));

    let answer = std::fs::read_to_string(dir.path().join("answer"))
        .expect("the host never answered the permission request");
    assert!(
        answer.contains("deny"),
        "planning mode allowed a file change: {answer}"
    );
}
