//! Phase 5 end-to-end: the Git methods over real HTTP, against a repository the
//! test creates.
//!
//! These spawn the shipped binary and drive it with the real desktop client,
//! because the interesting failures in this phase are wiring mistakes — a path
//! resolved against the wrong root, a method registered but not routed — which
//! unit tests of the individual functions cannot see.

use monocode_core::remote::Connections;
use std::process::Command;

struct Fixture {
    _dir: tempfile::TempDir,
    directory: std::path::PathBuf,
    repo: std::path::PathBuf,
    port: u16,
    machine: String,
    client: Connections,
    child: Option<std::process::Child>,
}

impl Fixture {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "T"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            assert!(Command::new("git")
                .current_dir(&repo)
                .args(&args)
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-qm", "first"]] {
            assert!(Command::new("git")
                .current_dir(&repo)
                .args(&args)
                .status()
                .unwrap()
                .success());
        }
        let directory = dir.path().to_path_buf();
        let output = Command::new(env!("CARGO_BIN_EXE_monocode-host"))
            .args(["pair", "--name", "test", "--json", "--data-dir"])
            .arg(&directory)
            .output()
            .unwrap();
        assert!(output.status.success(), "pair failed");
        let token: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let token = token["token"].as_str().unwrap().to_string();

        // `serve` writes `running.json` after binding the socket, so it
        // reports the port it actually got. Waiting for that file is exact;
        // probing a chosen port could succeed against a host that then dies.
        let mut child = Command::new(env!("CARGO_BIN_EXE_monocode-host"))
            .args(["serve", "--data-dir"])
            .arg(&directory)
            .arg("--port")
            .arg("0")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let state = directory.join("running.json");
        let mut port = 0;
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(&state) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(found) = value["port"].as_u64() {
                        port = found as u16;
                        break;
                    }
                }
            }
            if let Ok(Some(status)) = child.try_wait() {
                panic!("host exited before serving: {status}");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_ne!(port, 0, "the host never reported a port");
        let client = Connections::new(directory.join("client"));
        let machine = client
            .connect("test".into(), format!("http://127.0.0.1:{port}"), token)
            .expect("connect to the host");
        Self {
            repo,
            directory,
            port,
            machine: machine.id,
            client,
            child: Some(child),
            _dir: dir,
        }
    }

    fn call(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let _ = self.port;
        self.client
            .request(&self.machine, method, params)
            .unwrap_or_else(|e| panic!("{method} failed: {e}"))
    }

    fn attempt(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let _ = &self.directory;
        self.client.request(&self.machine, method, params)
    }

    fn project(&self) -> String {
        self.call("projects.open", serde_json::json!({ "cwd": self.repo }))
            .get("id")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn branches_are_listed_and_created() {
    let fixture = Fixture::start();
    let project = fixture.project();

    let listed = fixture.call("git.branches", serde_json::json!({ "projectId": project }));
    assert_eq!(listed["current"], "main");
    assert_eq!(listed["branches"], serde_json::json!(["main"]));

    let created = fixture.call(
        "git.createBranch",
        serde_json::json!({ "projectId": project, "branch": "feature/x" }),
    );
    assert_eq!(created["current"], "feature/x");
    assert!(created["branches"]
        .as_array()
        .unwrap()
        .iter()
        .any(|name| name == "feature/x"));
}

#[test]
fn the_index_reports_changed_files_with_counts() {
    let fixture = Fixture::start();
    let project = fixture.project();
    std::fs::write(fixture.repo.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(fixture.repo.join("new.txt"), "a\nb\nc\n").unwrap();

    let index = fixture.call("git.index", serde_json::json!({ "projectId": project }));
    assert_eq!(index["branch"], "main");
    let files = index["files"].as_array().unwrap();
    assert_eq!(files.len(), 2, "expected two changed files: {files:?}");
    let modified = files.iter().find(|f| f["relative"] == "a.txt").unwrap();
    assert_eq!(modified["status"], "modified");
    assert_eq!(modified["additions"], 1);
    assert_eq!(modified["unstaged"], true);
    let untracked = files.iter().find(|f| f["relative"] == "new.txt").unwrap();
    assert_eq!(untracked["status"], "untracked");
    // Untracked files have no diff, so their lines are counted directly.
    assert_eq!(untracked["additions"], 3);
    assert_eq!(index["additions"], 4);
}

#[test]
fn staging_moves_a_file_between_the_staged_and_unstaged_sets() {
    let fixture = Fixture::start();
    let project = fixture.project();
    std::fs::write(fixture.repo.join("a.txt"), "changed\n").unwrap();

    fixture.call(
        "git.action",
        serde_json::json!({ "projectId": project, "action": "stage", "path": "a.txt" }),
    );
    let index = fixture.call("git.index", serde_json::json!({ "projectId": project }));
    let file = &index["files"][0];
    assert_eq!(file["staged"], true);
    assert_eq!(file["unstaged"], false);
}

#[test]
fn a_commit_clears_the_change_list() {
    let fixture = Fixture::start();
    let project = fixture.project();
    std::fs::write(fixture.repo.join("a.txt"), "changed\n").unwrap();
    fixture.call(
        "git.action",
        serde_json::json!({ "projectId": project, "action": "stageAll" }),
    );
    fixture.call(
        "git.action",
        serde_json::json!({ "projectId": project, "action": "commit", "message": "second" }),
    );

    let index = fixture.call("git.index", serde_json::json!({ "projectId": project }));
    assert_eq!(index["files"].as_array().unwrap().len(), 0);
    assert_eq!(index["headPushed"], false);
}

#[test]
fn discarding_restores_tracked_files_and_removes_untracked_ones() {
    let fixture = Fixture::start();
    let project = fixture.project();
    std::fs::write(fixture.repo.join("a.txt"), "changed\n").unwrap();
    std::fs::write(fixture.repo.join("loose.txt"), "bye\n").unwrap();

    fixture.call(
        "git.action",
        serde_json::json!({ "projectId": project, "action": "discardAll" }),
    );
    assert_eq!(std::fs::read_to_string(fixture.repo.join("a.txt")).unwrap(), "one\n");
    assert!(!fixture.repo.join("loose.txt").exists());
}

#[test]
fn switching_branches_is_refused_while_the_tree_is_dirty() {
    let fixture = Fixture::start();
    let project = fixture.project();
    fixture.call(
        "git.createBranch",
        serde_json::json!({ "projectId": project, "branch": "other" }),
    );
    fixture.call("git.switch", serde_json::json!({ "projectId": project, "branch": "main" }));
    std::fs::write(fixture.repo.join("a.txt"), "changed\n").unwrap();

    let error = fixture
        .attempt(
            "git.switch",
            serde_json::json!({ "projectId": project, "branch": "other" }),
        )
        .unwrap_err();
    assert!(error.contains("stash"), "got: {error}");
    // The branch must not have moved.
    let listed = fixture.call("git.branches", serde_json::json!({ "projectId": project }));
    assert_eq!(listed["current"], "main");
}

/// The path checks are the security boundary of this phase, so they are tested
/// through the RPC surface rather than only on the resolver.
#[test]
fn paths_outside_the_project_are_refused() {
    let fixture = Fixture::start();
    let project = fixture.project();

    for path in ["../../etc/passwd", "/etc/passwd", ".git/config", "src/../../escape"] {
        let error = fixture
            .attempt(
                "git.action",
                serde_json::json!({ "projectId": project, "action": "stage", "path": path }),
            )
            .unwrap_err();
        assert!(
            error.contains("outside the workspace"),
            "{path} was not refused: {error}"
        );
    }
}

/// A client may name a worktree, but not an arbitrary directory to run Git in.
#[test]
fn a_working_copy_outside_the_project_is_refused() {
    let fixture = Fixture::start();
    let project = fixture.project();
    let error = fixture
        .attempt(
            "git.branches",
            serde_json::json!({ "projectId": project, "cwd": "/etc" }),
        )
        .unwrap_err();
    assert!(error.contains("not part of this project"), "got: {error}");
}

#[test]
fn an_unknown_action_is_refused_by_name() {
    let fixture = Fixture::start();
    let project = fixture.project();
    let error = fixture
        .attempt(
            "git.action",
            serde_json::json!({ "projectId": project, "action": "teleport" }),
        )
        .unwrap_err();
    assert!(error.contains("Unsupported Git action: teleport"), "got: {error}");
}

#[test]
fn a_worktree_is_created_and_then_accepted_as_a_working_copy() {
    let fixture = Fixture::start();
    let project = fixture.project();
    let created = fixture.call(
        "git.worktreeCreate",
        serde_json::json!({ "projectId": project, "branch": "wt", "base": "main" }),
    );
    let path = created.as_str().unwrap();
    assert!(std::path::Path::new(path).is_dir(), "worktree not created");

    // The new worktree is now a valid `cwd` for this project.
    let listed = fixture.call(
        "git.branches",
        serde_json::json!({ "projectId": project, "cwd": path }),
    );
    assert_eq!(listed["current"], "wt");
}

#[test]
fn a_file_is_read_and_written_through_the_workspace_guard() {
    let fixture = Fixture::start();
    let project = fixture.project();

    let original = fixture.call(
        "files.read",
        serde_json::json!({ "projectId": project, "path": "a.txt" }),
    );
    assert_eq!(original, "one\n");

    fixture.call(
        "files.write",
        serde_json::json!({
            "projectId": project,
            "path": "a.txt",
            "expected": "one\n",
            "content": "two\n",
        }),
    );
    assert_eq!(std::fs::read_to_string(fixture.repo.join("a.txt")).unwrap(), "two\n");
}

#[test]
fn a_write_based_on_a_stale_read_is_refused_and_leaves_the_file_alone() {
    let fixture = Fixture::start();
    let project = fixture.project();
    fixture.call(
        "files.write",
        serde_json::json!({
            "projectId": project, "path": "a.txt", "expected": "one\n", "content": "two\n",
        }),
    );

    // The client still believes the file holds "one\n".
    let error = fixture
        .attempt(
            "files.write",
            serde_json::json!({
                "projectId": project, "path": "a.txt", "expected": "one\n", "content": "clobber\n",
            }),
        )
        .unwrap_err();
    assert!(error.contains("changed on the host"), "got: {error}");
    assert_eq!(std::fs::read_to_string(fixture.repo.join("a.txt")).unwrap(), "two\n");
}

#[test]
fn a_file_diff_shows_the_original_and_the_working_copy() {
    let fixture = Fixture::start();
    let project = fixture.project();
    std::fs::write(fixture.repo.join("a.txt"), "changed\n").unwrap();

    let diff = fixture.call(
        "git.fileDiff",
        serde_json::json!({ "projectId": project, "path": "a.txt" }),
    );
    assert_eq!(diff["original"], "one\n");
    assert_eq!(diff["modified"], "changed\n");
    assert_eq!(diff["status"], "modified");
}

#[test]
fn reading_outside_the_project_or_its_git_directory_is_refused() {
    let fixture = Fixture::start();
    let project = fixture.project();
    for path in ["../../etc/passwd", ".git/config"] {
        let error = fixture
            .attempt(
                "files.read",
                serde_json::json!({ "projectId": project, "path": path }),
            )
            .unwrap_err();
        assert!(error.contains("outside the workspace"), "{path}: {error}");
    }
}

/// `workspace.run` takes absolute paths, so it resolves which project contains
/// them rather than being handed a project id. That makes its containment check
/// the security boundary, and it is tested through the RPC surface.
#[test]
fn the_workspace_dispatcher_serves_files_by_absolute_path() {
    let fixture = Fixture::start();
    fixture.project();
    let repo = fixture.repo.to_string_lossy().into_owned();
    let run = |command: &str, args: serde_json::Value| {
        fixture.call(
            "workspace.run",
            serde_json::json!({ "command": command, "args": args }),
        )
    };

    let listed = run("list_dir", serde_json::json!({ "path": repo }));
    assert!(listed.as_array().unwrap().iter().any(|e| e["name"] == "a.txt"));

    assert_eq!(
        run("read_text_file", serde_json::json!({ "path": format!("{repo}/a.txt") })),
        "one\n"
    );
    assert_eq!(
        run("list_project_files", serde_json::json!({ "cwd": repo }))[0]["relative"],
        "a.txt"
    );

    run(
        "write_text_file",
        serde_json::json!({ "path": format!("{repo}/a.txt"), "content": "written\n" }),
    );
    assert_eq!(std::fs::read_to_string(fixture.repo.join("a.txt")).unwrap(), "written\n");
}

#[test]
fn the_workspace_dispatcher_runs_git_commands() {
    let fixture = Fixture::start();
    fixture.project();
    let repo = fixture.repo.to_string_lossy().into_owned();
    let run = |command: &str, args: serde_json::Value| {
        fixture.call(
            "workspace.run",
            serde_json::json!({ "command": command, "args": args }),
        )
    };

    let branches = run("git_branches", serde_json::json!({ "cwd": repo }));
    assert_eq!(branches["current"], "main");

    let history = run("git_history", serde_json::json!({ "cwd": repo, "limit": 5 }));
    assert_eq!(history[0]["subject"], "first");

    std::fs::write(fixture.repo.join("a.txt"), "changed\n").unwrap();
    let stats = run("git_diff_stats", serde_json::json!({ "cwd": repo }));
    assert_eq!(stats["files"], 1);

    run("git_stage_all", serde_json::json!({ "cwd": repo }));
    run("git_commit", serde_json::json!({ "cwd": repo, "message": "second" }));
    let history = run("git_history", serde_json::json!({ "cwd": repo, "limit": 1 }));
    assert_eq!(history[0]["subject"], "second");
}

#[test]
fn the_workspace_dispatcher_searches_the_project() {
    let fixture = Fixture::start();
    fixture.project();
    let repo = fixture.repo.to_string_lossy().into_owned();
    let found = fixture.call(
        "workspace.run",
        serde_json::json!({
            "command": "search_project",
            "args": { "options": { "cwd": repo, "query": "one" } },
        }),
    );
    let matches = found["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1, "expected one hit: {found}");
    assert_eq!(matches[0]["relative"], "a.txt");
}

/// Every command that takes a path must refuse one outside the projects, not
/// just the handful that happen to be checked by hand.
#[test]
fn every_workspace_path_is_confined_to_the_project() {
    let fixture = Fixture::start();
    fixture.project();
    let outside = "/etc/passwd";

    let attempts: &[(&str, serde_json::Value)] = &[
        ("list_dir", serde_json::json!({ "path": outside })),
        ("read_text_file", serde_json::json!({ "path": outside })),
        ("read_binary_file", serde_json::json!({ "path": outside })),
        ("read_file_preview", serde_json::json!({ "path": outside })),
        ("write_text_file", serde_json::json!({ "path": outside, "content": "x" })),
        ("delete_path", serde_json::json!({ "path": outside })),
        ("git_branches", serde_json::json!({ "cwd": "/etc" })),
        ("git_diff_index", serde_json::json!({ "cwd": "/etc" })),
        ("list_project_files", serde_json::json!({ "cwd": "/etc" })),
        ("search_project", serde_json::json!({ "options": { "cwd": "/etc", "query": "x" } })),
    ];
    for (command, args) in attempts {
        let error = fixture
            .attempt(
                "workspace.run",
                serde_json::json!({ "command": command, "args": args }),
            )
            .unwrap_err();
        assert!(
            error.contains("outside"),
            "`{command}` was not confined: {error}"
        );
    }

    // `.git` stays unreachable even though it is inside the project.
    let error = fixture
        .attempt(
            "workspace.run",
            serde_json::json!({
                "command": "read_text_file",
                "args": { "path": fixture.repo.join(".git/config").to_string_lossy() },
            }),
        )
        .unwrap_err();
    assert!(error.contains("outside the workspace"), "got: {error}");
}

#[test]
fn a_named_workspace_command_that_is_unknown_is_refused() {
    let fixture = Fixture::start();
    let error = fixture
        .attempt(
            "workspace.run",
            serde_json::json!({ "command": "delete_everything", "args": {} }),
        )
        .unwrap_err();
    assert!(error.contains("Unsupported workspace command"), "got: {error}");
}
