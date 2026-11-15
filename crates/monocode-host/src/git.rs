//! Git operations the remote host exposes.
//!
//! These run the user's installed `git` directly in the project directory, so
//! their configuration, hooks, credential helpers and environment all apply
//! exactly as they would in a local terminal. Nothing here reimplements Git.
//!
//! Two rules shape the destructive operations:
//!
//! - A branch is never switched under uncommitted changes. Git would carry them
//!   across or refuse, and either outcome is confusing from a remote client, so
//!   this refuses first with a message that says why.
//! - A branch name is validated with `git check-ref-format` and rejected if it
//!   starts with `-` or `@`, which would otherwise be read as an option.

use crate::workspace;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// How long a single Git invocation may take.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Diff output is truncated past this, which is far more than a client renders.
const MAX_DIFF: usize = 2 * 1024 * 1024;

/// Runs Git in `cwd` and returns stdout, or the stderr on failure.
pub fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    git_with_input(cwd, args, None)
}

fn git_with_input(cwd: &Path, args: &[&str], input: Option<&str>) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .current_dir(cwd)
        // A pager would block forever with no terminal attached.
        .args(["-c", "core.pager=cat"])
        .args(args)
        // Never prompt: a remote request has no way to answer.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = command.spawn().map_err(|e| format!("Could not run Git: {e}"))?;
    if let Some(input) = input {
        use std::io::Write as _;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(input.as_bytes())
                .map_err(|e| format!("Could not write to Git: {e}"))?;
        }
    }
    // A hung Git (a remote fetch, say) must not hold the request forever.
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Git took too long".into());
            }
            Err(error) => return Err(format!("Could not run Git: {error}")),
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("Could not read Git output: {e}"))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if message.is_empty() {
            "Git failed".into()
        } else {
            message
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Local and remote branches, plus which one is checked out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Branches {
    pub current: Option<String>,
    pub local: Vec<String>,
    pub remotes: Vec<(String, String)>,
}

impl Branches {
    pub fn to_json(&self) -> Value {
        json!({
            "current": self.current,
            "branches": self.local,
            "remotes": self
                .remotes
                .iter()
                .map(|(remote, name)| json!({ "remote": remote, "name": name }))
                .collect::<Vec<_>>(),
        })
    }
}

pub fn branches(cwd: &Path) -> Result<Branches, String> {
    let local: Vec<String> = git(cwd, &["for-each-ref", "--format=%(refname:short)", "refs/heads"])?
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    // Symbolic refs like `origin/HEAD` are aliases, not branches a user picks.
    let remotes = git(
        cwd,
        &[
            "for-each-ref",
            "--format=%(refname:short)%00%(symref)",
            "refs/remotes",
        ],
    )?
    .lines()
    .filter(|line| !line.is_empty())
    .filter_map(|line| {
        let (reference, symref) = line.split_once('\0').unwrap_or((line, ""));
        if !symref.is_empty() {
            return None;
        }
        let (remote, name) = reference.split_once('/')?;
        Some((remote.to_string(), name.to_string()))
    })
    .collect();
    let current = git(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    Ok(Branches {
        current,
        local,
        remotes,
    })
}

/// True when the working copy has anything uncommitted, including untracked.
pub fn dirty(cwd: &Path) -> Result<bool, String> {
    Ok(!git(cwd, &["status", "--porcelain", "--untracked-files=all"])?
        .trim()
        .is_empty())
}

/// Rejects a branch name Git could not accept, or one that looks like a flag.
pub fn valid_name(cwd: &Path, name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 255 || name.starts_with(['-', '@']) {
        return Err("Enter a valid branch name".into());
    }
    git(cwd, &["check-ref-format", "--branch", name]).map(|_| ())
}

pub fn switch(cwd: &Path, branch: &str, remote: Option<&str>) -> Result<Branches, String> {
    valid_name(cwd, branch)?;
    let state = branches(cwd)?;
    let local = state.local.iter().any(|name| name == branch);
    let remote_ref = remote
        .filter(|remote| {
            state
                .remotes
                .iter()
                .any(|(r, name)| r == remote && name == branch)
        })
        .map(|remote| format!("refs/remotes/{remote}/{branch}"));
    if remote.is_some() && remote_ref.is_none() {
        return Err("Choose an available remote branch".into());
    }
    if !local && remote_ref.is_none() {
        return Err("Choose an existing local branch".into());
    }
    if state.current.as_deref() == Some(branch) {
        return Ok(state);
    }
    // Switching under a dirty tree either carries changes across or fails
    // midway; refusing up front is the only predictable outcome.
    if dirty(cwd)? {
        return Err("Commit or stash changes on the host before switching branches".into());
    }
    match (local, remote_ref) {
        (false, Some(reference)) => {
            git(cwd, &["switch", "--track", "-c", branch, &reference])?;
        }
        _ => {
            git(cwd, &["switch", branch])?;
        }
    }
    branches(cwd)
}

pub fn create_branch(cwd: &Path, branch: &str) -> Result<Branches, String> {
    valid_name(cwd, branch)?;
    if branches(cwd)?.local.iter().any(|name| name == branch) {
        return Err("Branch already exists".into());
    }
    if dirty(cwd)? {
        return Err("Commit or stash changes on the host before switching branches".into());
    }
    git(cwd, &["switch", "-c", branch])?;
    branches(cwd)
}

/// Creates a worktree for `branch`, optionally from `base`.
///
/// `existing` means the branch already exists and should be checked out rather
/// than created, which Git needs a different invocation for.
pub fn create_worktree(
    project_cwd: &Path,
    branch: &str,
    base: Option<&str>,
    existing: bool,
) -> Result<PathBuf, String> {
    valid_name(project_cwd, branch)?;
    let parent = project_cwd.parent().unwrap_or(project_cwd);
    let name = project_cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".into());
    let suffix = branch.replace(['/', '\\'], "-");
    let path = parent.join(format!("{name}-{suffix}"));
    if path.exists() {
        return Err(format!("{} already exists on the host", path.display()));
    }
    let mut args = vec!["worktree", "add"];
    let target = path.to_string_lossy().into_owned();
    args.push(&target);
    if existing {
        args.push(branch);
    } else {
        args.push("-b");
        args.push(branch);
        if let Some(base) = base {
            args.push(base);
        }
    }
    git(project_cwd, &args)?;
    Ok(path)
}

/// The repository's uncommitted diff against HEAD.
pub fn diff(cwd: &Path) -> Result<String, String> {
    let output = git(
        cwd,
        &["diff", "--no-ext-diff", "--no-textconv", "HEAD", "--"],
    )?;
    Ok(output.chars().take(MAX_DIFF).collect())
}

/// One changed file in the working copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub relative: String,
    pub status: String,
    pub additions: u64,
    pub deletions: u64,
    pub staged: bool,
    pub unstaged: bool,
}

/// The Changes panel: branch state, upstream distance and every changed file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Index {
    pub branch: Option<String>,
    pub head: Option<String>,
    pub files: Vec<ChangedFile>,
    pub additions: u64,
    pub deletions: u64,
    pub remote: Option<String>,
    pub upstream: Option<String>,
    pub default_branch: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    pub ahead_of_default: u64,
    pub head_pushed: bool,
}

impl Index {
    pub fn to_json(&self) -> Value {
        json!({
            "branch": self.branch,
            "head": self.head,
            "files": self
                .files
                .iter()
                .map(|file| json!({
                    "path": file.path,
                    "relative": file.relative,
                    "status": file.status,
                    "additions": file.additions,
                    "deletions": file.deletions,
                    "staged": file.staged,
                    "unstaged": file.unstaged,
                }))
                .collect::<Vec<_>>(),
            "additions": self.additions,
            "deletions": self.deletions,
            "remote": self.remote,
            "upstream": self.upstream,
            "defaultBranch": self.default_branch,
            "ahead": self.ahead,
            "behind": self.behind,
            "aheadOfDefault": self.ahead_of_default,
            "headPushed": self.head_pushed,
        })
    }
}

/// Maps a two-character porcelain code to a readable status.
fn status_name(code: &str) -> String {
    match code {
        "??" => "untracked",
        _ if code.contains('D') => "deleted",
        _ if code.contains('A') => "added",
        _ if code.contains('R') => "renamed",
        _ if code.contains('C') => "copied",
        _ => "modified",
    }
    .to_string()
}

pub fn index(cwd: &Path) -> Result<Index, String> {
    // An ordinary folder has no changes; that is not an error.
    let inside = git(cwd, &["rev-parse", "--is-inside-work-tree"])
        .map(|text| text.trim() == "true")
        .unwrap_or(false);
    if !inside {
        return Ok(Index::default());
    }
    let branch = git(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let head = git(cwd, &["rev-parse", "--verify", "HEAD"])
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let upstream = git(
        cwd,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"],
    )
    .ok()
    .map(|t| t.trim().to_string())
    .filter(|t| !t.is_empty());
    let remote = git(cwd, &["remote", "get-url", "origin"])
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .map(|_| "origin".to_string());
    let (mut ahead, mut behind) = (0, 0);
    if upstream.is_some() {
        if let Ok(counts) = git(cwd, &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"])
        {
            let mut fields = counts.split_whitespace();
            behind = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            ahead = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        }
    }
    let default_branch = git(
        cwd,
        &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"],
    )
    .ok()
    .map(|t| t.trim().to_string())
    .filter(|t| !t.is_empty());
    let ahead_of_default = default_branch
        .as_deref()
        .and_then(|base| {
            git(cwd, &["rev-list", "--count", &format!("{base}..HEAD")])
                .ok()
                .and_then(|t| t.trim().parse().ok())
        })
        .unwrap_or(0);
    let head_pushed = upstream.is_some() && ahead == 0;
    // `--numstat` gives per-file line counts in one call.
    let numstat = git(
        cwd,
        &["diff", "--no-ext-diff", "--no-textconv", "--numstat", "HEAD", "--"],
    )
    .unwrap_or_default();
    let mut counts: std::collections::HashMap<&str, (u64, u64)> = std::collections::HashMap::new();
    for line in numstat.lines() {
        let mut fields = line.split('\t');
        let (Some(add), Some(del), Some(path)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        counts.insert(path, (add.parse().unwrap_or(0), del.parse().unwrap_or(0)));
    }
    let status_text = git(cwd, &["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
    let mut files = Vec::new();
    let mut parts = status_text.split('\0').peekable();
    while let Some(item) = parts.next() {
        if item.len() < 4 {
            continue;
        }
        let (code, path) = item.split_at(2);
        let path = path.trim_start();
        // -z writes a rename as `XY new\0old`; the old path is the next field.
        if code.contains('R') || code.contains('C') {
            parts.next();
        }
        let (mut additions, deletions) = counts.get(path).copied().unwrap_or((0, 0));
        // Untracked files have no diff, so count their lines directly.
        if code == "??" {
            if let Ok(meta) = std::fs::symlink_metadata(cwd.join(path)) {
                if meta.is_file() && meta.len() <= monocode_core::files::document::MAX_TEXT_BYTES as u64 {
                    if let Ok(content) = std::fs::read_to_string(cwd.join(path)) {
                        additions = content.lines().count() as u64;
                    }
                }
            }
        }
        files.push(ChangedFile {
            path: path.to_string(),
            relative: path.to_string(),
            status: status_name(code),
            additions,
            deletions,
            staged: code != "??" && !code.starts_with(' '),
            unstaged: code == "??" || !code.ends_with(' '),
        });
    }
    files.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(Index {
        branch,
        head,
        additions: files.iter().map(|f| f.additions).sum(),
        deletions: files.iter().map(|f| f.deletions).sum(),
        files,
        remote,
        upstream,
        default_branch,
        ahead,
        behind,
        ahead_of_default,
        head_pushed,
    })
}

/// Runs one of the Changes-panel actions.
///
/// Every path is resolved through [`project_path`] first, so an action cannot
/// reach outside the project even though it names files.
pub fn action(
    cwd: &Path,
    action: &str,
    path: Option<&str>,
    message: Option<&str>,
    content: Option<&str>,
) -> Result<Value, String> {
    let relative = |input: Option<&str>| -> Result<String, String> {
        let input = input.ok_or_else(|| "A path is required".to_string())?;
        let resolved = project_path(cwd, input)?;
        let relative = resolved
            .strip_prefix(cwd)
            .map_err(|_| "Path is outside the workspace".to_string())?;
        Ok(relative.to_string_lossy().replace('\\', "/"))
    };
    match action {
        "stage" | "unstage" | "stageContents" => {
            let file = relative(path)?;
            if action == "stageContents" {
                // Writing the exact bytes the client sent, rather than staging
                // whatever is on disk, is what makes this safe to use for a
                // partially-edited file.
                let content = content.ok_or_else(|| "File too large or invalid".to_string())?;
                if content.len() > monocode_core::files::document::MAX_TEXT_BYTES {
                    return Err("File too large or invalid".into());
                }
                let hash = git_with_input(
                    cwd,
                    &["hash-object", "-w", "--path", &file, "--stdin"],
                    Some(content),
                )?
                .trim()
                .to_string();
                let staged = git(cwd, &["ls-files", "--stage", "--", &file]).unwrap_or_default();
                let mode = staged
                    .split_whitespace()
                    .next()
                    .filter(|mode| mode.len() == 6 && mode.chars().all(|c| c.is_ascii_digit()))
                    .unwrap_or("100644")
                    .to_string();
                git(
                    cwd,
                    &["update-index", "--add", "--cacheinfo", &mode, &hash, &file],
                )?;
            } else if action == "stage" {
                git(cwd, &["--literal-pathspecs", "add", "--", &file])?;
            } else {
                git(cwd, &["--literal-pathspecs", "restore", "--staged", "--", &file])?;
            }
            Ok(Value::Null)
        }
        "stageAll" => {
            git(cwd, &["add", "-A", "--", "."])?;
            Ok(Value::Null)
        }
        "unstageAll" => {
            git(cwd, &["reset", "-q", "--", "."])?;
            Ok(Value::Null)
        }
        "discard" | "discardAll" => {
            let targets = if action == "discardAll" {
                index(cwd)?
                    .files
                    .into_iter()
                    .filter(|file| file.unstaged)
                    .map(|file| file.relative)
                    .collect::<Vec<_>>()
            } else {
                let file = relative(path)?;
                let found = index(cwd)?
                    .files
                    .into_iter()
                    .any(|entry| entry.relative == file && entry.unstaged);
                if !found {
                    return Err("File has no changes to discard".into());
                }
                vec![file]
            };
            for file in targets {
                let known = index(cwd)?
                    .files
                    .into_iter()
                    .find(|entry| entry.relative == file);
                match known.map(|entry| entry.status) {
                    // An untracked file has no committed version to restore.
                    Some(status) if status == "untracked" => {
                        let target = project_path(cwd, &file)?;
                        if target.is_dir() {
                            std::fs::remove_dir_all(&target).map_err(|e| e.to_string())?;
                        } else {
                            std::fs::remove_file(&target).map_err(|e| e.to_string())?;
                        }
                    }
                    _ => {
                        git(cwd, &["restore", "--worktree", "--", &file])?;
                    }
                }
            }
            Ok(Value::Null)
        }
        "commit" => {
            let message = message.unwrap_or_default();
            if message.trim().is_empty() || message.len() > 100_000 {
                return Err("Enter a commit message".into());
            }
            git(cwd, &["commit", "-m", message])?;
            Ok(Value::Null)
        }
        "push" => {
            git(cwd, &["push", "-u", "origin", "HEAD"])?;
            Ok(Value::Null)
        }
        "createPr" => {
            // `gh` is optional; a missing one is reported, not silently ignored.
            let output = Command::new("gh")
                .current_dir(cwd)
                .args(["pr", "create", "--fill"])
                .env("GH_PROMPT_DISABLED", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .map_err(|_| "The GitHub CLI is not installed on this machine".to_string())?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
            }
            Ok(Value::String(
                String::from_utf8_lossy(&output.stdout).trim().to_string(),
            ))
        }
        other => Err(format!("Unsupported Git action: {other}")),
    }
}

/// One file's before/after pair for the diff viewer.
///
/// `staged` compares HEAD against the index; otherwise HEAD against the working
/// copy. A deleted file yields an empty "after" rather than an error, because
/// the viewer still needs to show what was removed.
pub fn file_diff(cwd: &Path, input: &str, staged: bool) -> Result<Value, String> {
    let path = project_path(cwd, input)?;
    let relative = path
        .strip_prefix(cwd)
        .map_err(|_| "Path is outside the workspace".to_string())?
        .to_string_lossy()
        .replace('\\', "/");
    let entry = index(cwd)?
        .files
        .into_iter()
        .find(|file| file.relative == relative)
        .ok_or_else(|| "File has no uncommitted changes".to_string())?;
    let original = git(cwd, &["show", &format!("HEAD:{relative}")]).unwrap_or_default();
    let current = if staged {
        git(cwd, &["show", &format!(":{relative}")]).unwrap_or_default()
    } else {
        match read_file(cwd, &relative) {
            Ok(text) => text,
            // A deleted file has no working copy to read.
            Err(_) if entry.status == "deleted" => String::new(),
            Err(_) => String::new(),
        }
    };
    Ok(json!({
        "path": relative,
        "relative": relative,
        "original": original,
        "modified": current,
        "staged": staged,
        "status": entry.status,
        "binary": original.contains('\0') || current.contains('\0'),
    }))
}

/// Reads a text file the client named.
///
/// Binary and oversized files are refused rather than truncated, so a client
/// never silently saves a partial copy back over the original.
pub fn read_file(root: &Path, input: &str) -> Result<String, String> {
    let path = project_path(root, input)?;
    let meta = std::fs::metadata(&path).map_err(|e| format!("Could not read the file: {e}"))?;
    if !meta.is_file() {
        return Err("Path is not a file".into());
    }
    if meta.len() > monocode_core::files::document::MAX_TEXT_BYTES as u64 {
        return Err("File is too large to preview".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("Could not read the file: {e}"))?;
    if bytes.contains(&0) {
        return Err("Binary file cannot be previewed".into());
    }
    String::from_utf8(bytes).map_err(|_| "File is not valid UTF-8".to_string())
}

/// Writes a file the client named, but only if it still holds what the client
/// last read.
///
/// `expected` is the optimistic-concurrency guard: without it a remote save
/// would overwrite whatever changed on the machine in between. The write goes
/// to a sibling temporary file and is renamed into place, so a crash mid-write
/// cannot leave a half-written source file.
pub fn write_file(root: &Path, input: &str, expected: &str, content: &str) -> Result<(), String> {
    if content.contains('\0') || content.len() > monocode_core::files::document::MAX_TEXT_BYTES {
        return Err("Invalid file content".into());
    }
    let path = project_path(root, input)?;
    let current = read_file(root, input)?;
    if current != expected {
        return Err("File changed on the host; reload before saving".into());
    }
    let temporary = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    std::fs::write(&temporary, content).map_err(|e| format!("Could not save the file: {e}"))?;
    std::fs::rename(&temporary, &path).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        format!("Could not save the file: {e}")
    })
}

/// Resolves a client path inside the project, refusing escapes and `.git`.
pub fn project_path(root: &Path, input: &str) -> Result<PathBuf, String> {
    workspace::resolve(root, input, false).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A repository with one commit on `main`.
    fn repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "T"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            let status = Command::new("git")
                .current_dir(&path)
                .args(&args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        }
        std::fs::write(path.join("a.txt"), "one\n").unwrap();
        for args in [vec!["add", "a.txt"], vec!["commit", "-qm", "first"]] {
            assert!(Command::new("git")
                .current_dir(&path)
                .args(&args)
                .status()
                .unwrap()
                .success());
        }
        (dir, path)
    }

    #[test]
    fn git_availability() {
        // These tests need a real git; the host runs the user's anyway.
        assert!(Command::new("git").arg("--version").output().is_ok());
    }

    #[test]
    fn listing_reports_the_current_branch() {
        let (_dir, path) = repo();
        let state = branches(&path).unwrap();
        assert_eq!(state.current.as_deref(), Some("main"));
        assert_eq!(state.local, vec!["main".to_string()]);
    }

    #[test]
    fn a_clean_tree_is_not_dirty() {
        let (_dir, path) = repo();
        assert!(!dirty(&path).unwrap());
    }

    #[test]
    fn an_untracked_file_makes_the_tree_dirty() {
        let (_dir, path) = repo();
        std::fs::write(path.join("new.txt"), "x").unwrap();
        assert!(dirty(&path).unwrap());
    }

    #[test]
    fn switching_is_refused_while_the_tree_is_dirty() {
        let (_dir, path) = repo();
        git(&path, &["switch", "-c", "other"]).unwrap();
        git(&path, &["switch", "main"]).unwrap();
        std::fs::write(path.join("a.txt"), "changed\n").unwrap();
        let error = switch(&path, "other", None).unwrap_err();
        assert!(error.contains("stash"), "got: {error}");
        // The branch must not have moved.
        assert_eq!(branches(&path).unwrap().current.as_deref(), Some("main"));
    }

    #[test]
    fn switching_a_clean_tree_works() {
        let (_dir, path) = repo();
        git(&path, &["switch", "-c", "other"]).unwrap();
        git(&path, &["switch", "main"]).unwrap();
        let state = switch(&path, "other", None).unwrap();
        assert_eq!(state.current.as_deref(), Some("other"));
    }

    #[test]
    fn an_unknown_branch_is_refused() {
        let (_dir, path) = repo();
        let error = switch(&path, "nope", None).unwrap_err();
        assert!(error.contains("existing local branch"), "got: {error}");
    }

    #[test]
    fn a_branch_name_that_looks_like_a_flag_is_refused() {
        let (_dir, path) = repo();
        let error = create_branch(&path, "--force").unwrap_err();
        assert!(error.contains("valid branch name"), "got: {error}");
    }

    #[test]
    fn creating_an_existing_branch_is_refused() {
        let (_dir, path) = repo();
        let error = create_branch(&path, "main").unwrap_err();
        assert!(error.contains("already exists"), "got: {error}");
    }

    #[test]
    fn creating_a_branch_checks_it_out() {
        let (_dir, path) = repo();
        let state = create_branch(&path, "feature/x").unwrap();
        assert_eq!(state.current.as_deref(), Some("feature/x"));
    }

    #[test]
    fn a_worktree_is_created_beside_the_project() {
        let (_dir, path) = repo();
        let created = create_worktree(&path, "wt-branch", Some("main"), false).unwrap();
        assert!(created.is_dir(), "worktree was not created");
        assert!(created.file_name().unwrap().to_string_lossy().contains("wt-branch"));
    }

    #[test]
    fn diff_shows_an_uncommitted_change() {
        let (_dir, path) = repo();
        std::fs::write(path.join("a.txt"), "one\ntwo\n").unwrap();
        let text = diff(&path).unwrap();
        assert!(text.contains("+two"), "diff missed the change: {text}");
    }

    #[test]
    fn a_git_failure_reports_its_message() {
        let (_dir, path) = repo();
        let error = git(&path, &["rev-parse", "no-such-ref"]).unwrap_err();
        assert!(!error.is_empty());
    }
}
