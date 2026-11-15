//! The `workspace.run` dispatcher: the file and Git commands a remote desktop
//! sends by their Tauri names, so the same UI works against a host.
//!
//! This layer differs from [`crate::git`] in how it receives paths: a remote
//! client here sends **absolute** host paths, because it is showing a tree it
//! discovered from the host itself. So instead of resolving a relative path
//! against a known project, it must find which project (or worktree) contains
//! the path it was given. That is the check that keeps this from becoming
//! arbitrary filesystem access, and it happens in [`Workspace::locate`] before
//! any command runs.
//!
//! Symlinks are resolved on the nearest existing ancestor and the missing tail
//! is re-appended, so a path to a file that is about to be created is still
//! checked against its real parent.

use crate::git;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Every command this layer answers. Anything else is refused by name.
pub const COMMANDS: &[&str] = &[
    "list_dir",
    "list_project_files",
    "read_text_file",
    "read_binary_file",
    "read_file_preview",
    "write_text_file",
    "stat_files",
    "create_path",
    "rename_path",
    "delete_path",
    "copy_path",
    "move_path",
    "git_diff_index",
    "git_diff_files",
    "git_diff_stats",
    "git_file_diff",
    "git_stage_contents",
    "git_stage_file",
    "git_unstage_file",
    "git_discard_file",
    "git_discard_all",
    "git_stage_all",
    "git_unstage_all",
    "git_commit",
    "git_locate_files",
    "git_head_message",
    "git_push",
    "git_pull",
    "git_sync",
    "git_pr_status",
    "git_pr_create",
    "git_history",
    "git_commit_files",
    "git_commit_file_diff",
    "git_staged_context",
    "git_range_context",
    "git_branches",
    "git_checkout",
    "git_create_branch",
    "git_stash",
    "git_worktrees",
    "search_project",
];

/// Text files are capped so a JSON response stays a sensible size.
const MAX_TEXT: u64 = 1024 * 1024;
/// Previews allow more, because the client only renders part of them.
const MAX_PREVIEW: u64 = 10 * 1024 * 1024;
/// `stat_files` is called with a selection, not a whole tree.
const MAX_STAT: usize = 64;
const MAX_PATH: usize = 4096;

/// Thrown when a path is not inside any project on this machine.
const OUTSIDE: &str = "Path is outside this machine’s projects";

/// A path located inside one of the allowed project roots.
pub struct Located {
    pub root: PathBuf,
    pub relative: String,
}

/// Issues `workspace.run` commands against the projects this host serves.
pub struct Workspace {
    /// Every project root, refreshed when a project is opened or a worktree
    /// created. Cheap enough to keep as a plain list.
    roots: Arc<Mutex<Vec<PathBuf>>>,
}

impl Workspace {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots: Arc::new(Mutex::new(roots)),
        }
    }

    /// Replaces the allowed roots. Called whenever the project set changes.
    pub fn set_roots(&self, roots: Vec<PathBuf>) {
        if let Ok(mut current) = self.roots.lock() {
            *current = roots;
        }
    }

    fn roots(&self) -> Vec<PathBuf> {
        let base = self
            .roots
            .lock()
            .map(|roots| roots.clone())
            .unwrap_or_default();
        let mut all = Vec::new();
        for root in base {
            // A project's worktrees are equally valid places to edit files.
            if let Ok(trees) = monocode_core::git::worktrees(&root) {
                for tree in trees {
                    if !tree.missing {
                        all.push(PathBuf::from(&tree.path));
                    }
                }
            }
            all.push(root);
        }
        all
    }

    /// Finds the project root that contains `input`, which need not exist.
    pub fn locate(&self, input: &str) -> Result<Located, String> {
        if input.len() > MAX_PATH || input.contains('\0') {
            return Err("Invalid workspace path".into());
        }
        if !Path::new(input).is_absolute() {
            return Err("Invalid workspace path".into());
        }
        let actual = resolve_existing_ancestor(Path::new(input))?;
        for root in self.roots() {
            let root = std::fs::canonicalize(&root).unwrap_or(root);
            if let Ok(relative) = actual.strip_prefix(&root) {
                return Ok(Located {
                    root,
                    relative: relative.to_string_lossy().replace('\\', "/"),
                });
            }
        }
        Err(OUTSIDE.into())
    }

    /// Locates, then applies the same containment rules as the RPC layer: no
    /// escape, and no `.git` at any depth.
    ///
    /// The project root itself is allowed, because these commands address
    /// directories (list, search, run Git) as often as individual files.
    pub fn existing(&self, input: &str) -> Result<(PathBuf, PathBuf), String> {
        let located = self.locate(input)?;
        let path = crate::workspace::resolve(&located.root, &located.relative, true)
            .map_err(|e| e.to_string())?;
        Ok((located.root, path))
    }

    /// Runs one command by its Tauri name.
    pub fn run(&self, command: &str, args: &Value) -> Result<Value, String> {
        if !COMMANDS.contains(&command) {
            return Err("Unsupported workspace command".into());
        }
        let arg = |key: &str| -> Option<&str> { args.get(key).and_then(Value::as_str) };
        let required = |key: &str| -> Result<&str, String> {
            arg(key).ok_or_else(|| format!("`{key}` is required"))
        };
        match command {
            "list_dir" => self.list_dir(required("path")?),
            "list_project_files" => self.list_project_files(required("cwd")?),
            "read_text_file" => self.read_text(required("path")?),
            "read_binary_file" => self.read_binary(required("path")?),
            "read_file_preview" => {
                let start = args.get("startLine").and_then(Value::as_u64).unwrap_or(0);
                let max = args.get("maxLines").and_then(Value::as_u64).unwrap_or(400);
                self.preview(required("path")?, max, start)
            }
            "write_text_file" => {
                self.write_text(required("path")?, required("content")?)
            }
            "stat_files" => match args.get("paths").and_then(Value::as_array) {
                Some(paths) => {
                    let paths: Vec<String> = paths
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect();
                    self.stat_files(&paths)
                }
                None => Err("`paths` is required".into()),
            },
            "create_path" => self.create(
                required("parent")?,
                required("name")?,
                args.get("isDir").and_then(Value::as_bool).unwrap_or(false),
            ),
            "rename_path" => self.rename(required("path")?, required("name")?),
            "delete_path" => self.delete(required("path")?),
            "copy_path" => self.copy(required("from")?, required("destParent")?),
            "move_path" => self.move_to(required("from")?, required("destParent")?),
            // The Git commands take a working directory and reuse the module
            // already serving the `git.*` methods.
            "git_diff_index" | "git_diff_files" => {
                let cwd = self.cwd(required("cwd")?)?;
                git::index(&cwd).map(|index| index.to_json())
            }
            "git_diff_stats" => {
                let cwd = self.cwd(required("cwd")?)?;
                let index = git::index(&cwd)?;
                Ok(json!({
                    "files": index.files.len(),
                    "additions": index.additions,
                    "deletions": index.deletions,
                }))
            }
            "git_file_diff" => {
                let cwd = self.cwd(required("cwd")?)?;
                git::file_diff(
                    &cwd,
                    required("relative")?,
                    args.get("staged").and_then(Value::as_bool).unwrap_or(false),
                )
            }
            "git_stage_contents" => {
                let cwd = self.cwd(required("cwd")?)?;
                git::action(
                    &cwd,
                    "stageContents",
                    Some(required("relative")?),
                    None,
                    Some(required("contents")?),
                )
            }
            "git_stage_file" | "git_unstage_file" | "git_discard_file" => {
                let cwd = self.cwd(required("cwd")?)?;
                let action = match command {
                    "git_stage_file" => "stage",
                    "git_unstage_file" => "unstage",
                    _ => "discard",
                };
                git::action(&cwd, action, Some(required("relative")?), None, None)
            }
            "git_stage_all" | "git_unstage_all" | "git_discard_all" | "git_push" => {
                let cwd = self.cwd(required("cwd")?)?;
                let action = match command {
                    "git_stage_all" => "stageAll",
                    "git_unstage_all" => "unstageAll",
                    "git_discard_all" => "discardAll",
                    _ => "push",
                };
                git::action(&cwd, action, None, None, None)
            }
            "git_commit" => {
                let cwd = self.cwd(required("cwd")?)?;
                let message = required("message")?;
                let amend = args.get("amend").and_then(Value::as_bool).unwrap_or(false);
                let paths: Vec<String> = args
                    .get("paths")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                self.git_commit(&cwd, message, amend, &paths)
            }
            "git_head_message" => {
                let cwd = self.cwd(required("cwd")?)?;
                git::git(&cwd, &["log", "-1", "--format=%B"]).map(Value::String)
            }
            "git_pull" => {
                let cwd = self.cwd(required("cwd")?)?;
                // A remote request cannot resolve a merge conflict, so this
                // only ever fast-forwards.
                git::git(&cwd, &["pull", "--ff-only"]).map(|_| Value::Null)
            }
            "git_sync" => {
                let cwd = self.cwd(required("cwd")?)?;
                git::git(&cwd, &["pull", "--ff-only"])?;
                git::action(&cwd, "push", None, None, None)
            }
            "git_history" => {
                let cwd = self.cwd(required("cwd")?)?;
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50);
                self.git_history(&cwd, limit)
            }
            "git_commit_files" => {
                let cwd = self.cwd(required("cwd")?)?;
                let sha = required("sha")?;
                monocode_core::git::commit_files(&cwd, sha).map(|files| {
                    Value::Array(
                        files
                            .into_iter()
                            .map(|file| json!({
                                "path": file.path,
                                "additions": file.additions,
                                "deletions": file.deletions,
                            }))
                            .collect(),
                    )
                })
            }
            "git_commit_file_diff" => {
                let cwd = self.cwd(required("cwd")?)?;
                let sha = required("sha")?;
                let relative = required("relative")?;
                git::git(
                    &cwd,
                    &["show", "--no-ext-diff", "--no-textconv", &format!("{sha}:{relative}")],
                )
                .map(Value::String)
            }
            "git_branches" => {
                let cwd = self.cwd(required("cwd")?)?;
                git::branches(&cwd).map(|b| b.to_json())
            }
            "git_checkout" => {
                let cwd = self.cwd(required("cwd")?)?;
                git::switch(&cwd, required("name")?, arg("remote")).map(|b| b.to_json())
            }
            "git_create_branch" => {
                let cwd = self.cwd(required("cwd")?)?;
                git::create_branch(&cwd, required("name")?).map(|b| b.to_json())
            }
            "git_worktrees" => {
                let cwd = self.cwd(required("cwd")?)?;
                monocode_core::git::worktrees(&cwd).map(|trees| {
                    Value::Array(
                        trees
                            .into_iter()
                            .map(|tree| {
                                json!({
                                    "path": tree.path,
                                    "branch": tree.branch,
                                    "head": tree.head,
                                    "main": tree.main,
                                    "missing": tree.missing,
                                })
                            })
                            .collect(),
                    )
                })
            }
            "git_stash" => {
                let cwd = self.cwd(required("cwd")?)?;
                match arg("message") {
                    Some(message) => git::git(&cwd, &["stash", "push", "-u", "-m", message]),
                    None => git::git(&cwd, &["stash", "push", "-u"]),
                }
                .map(|_| Value::Null)
            }
            "git_pr_status" | "git_pr_create" => self.git_pr(command, args),
            "git_locate_files" | "git_staged_context" | "git_range_context" => {
                Err(format!("`{command}` is not available from a remote host yet"))
            }
            "search_project" => self.search_project(args),
            other => Err(format!("Unsupported workspace command: {other}")),
        }
    }

    /// Resolves a working directory a client named, refusing anything that is
    /// not part of a project this host serves.
    fn cwd(&self, input: &str) -> Result<PathBuf, String> {
        self.existing(input).map(|(_, path)| path)
    }

    fn list_dir(&self, input: &str) -> Result<Value, String> {
        let (_, path) = self.existing(input)?;
        let entries = monocode_core::files::list_dir(&path)?;
        let base = input.trim_end_matches('/');
        Ok(Value::Array(
            entries
                .into_iter()
                .map(|entry| {
                    let mut value = serde_json::to_value(&entry).unwrap_or(Value::Null);
                    // The client expects each entry to carry its full path.
                    if let Value::Object(ref mut map) = value {
                        map.insert(
                            "path".into(),
                            Value::String(format!("{base}/{}", entry.name)),
                        );
                    }
                    value
                })
                .collect(),
        ))
    }

    fn list_project_files(&self, input: &str) -> Result<Value, String> {
        let (_, path) = self.existing(input)?;
        let files = monocode_core::files::list_project_files(&path.to_string_lossy())?;
        let base = input.trim_end_matches('/');
        Ok(Value::Array(
            files
                .into_iter()
                .map(|file| {
                    json!({
                        "name": file.name,
                        "path": format!("{base}/{}", file.relative),
                        "relative": file.relative,
                    })
                })
                .collect(),
        ))
    }

    fn read_text(&self, input: &str) -> Result<Value, String> {
        let (_, path) = self.existing(input)?;
        let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err("Not a file".into());
        }
        if meta.len() > MAX_TEXT {
            return Err(format!(
                "File is too large to open (maximum {} MB).",
                MAX_TEXT / 1024 / 1024
            ));
        }
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        if bytes.contains(&0) {
            return Err("Binary file cannot be previewed".into());
        }
        String::from_utf8(bytes)
            .map(Value::String)
            .map_err(|_| "File is not valid UTF-8".to_string())
    }

    fn read_binary(&self, input: &str) -> Result<Value, String> {
        let (_, path) = self.existing(input)?;
        let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err("Not a file".into());
        }
        // Base64 keeps arbitrary bytes intact through JSON.
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        Ok(json!({ "base64": base64_encode(&bytes), "size": bytes.len() }))
    }

    fn preview(&self, input: &str, max_lines: u64, start: u64) -> Result<Value, String> {
        let (_, path) = self.existing(input)?;
        let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err("Not a file".into());
        }
        if meta.len() > MAX_PREVIEW {
            return Err(format!(
                "File is too large to preview (maximum {} MB).",
                MAX_PREVIEW / 1024 / 1024
            ));
        }
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        if bytes.contains(&0) {
            return Err("Binary file cannot be previewed".into());
        }
        let text = String::from_utf8_lossy(&bytes);
        let all: Vec<&str> = text.lines().collect();
        let total = all.len() as u64;
        let start = start.min(total);
        let end = start.saturating_add(max_lines).min(total);
        Ok(json!({
            "lines": all[start as usize..end as usize],
            "startLine": start,
            "totalLines": total,
            "truncated": end < total,
        }))
    }

    fn write_text(&self, input: &str, content: &str) -> Result<Value, String> {
        if content.contains('\0') || content.len() as u64 > MAX_TEXT {
            return Err("Invalid file content".into());
        }
        let (root, path) = self.existing(input)?;
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| OUTSIDE.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        // Reuse the guarded write, which also checks the file has not changed.
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        git::write_file(&root, &relative, &current, content).map(|()| Value::Null)
    }

    fn stat_files(&self, paths: &[String]) -> Result<Value, String> {
        if paths.len() > MAX_STAT {
            return Err(format!("At most {MAX_STAT} paths may be checked at once"));
        }
        let mut out = Vec::new();
        for input in paths {
            // A path that is refused or missing is reported as absent rather
            // than failing the whole request, which is what the UI expects.
            let Ok((_, path)) = self.existing(input) else {
                out.push(json!({ "path": input, "exists": false }));
                continue;
            };
            match std::fs::metadata(&path) {
                Ok(meta) => {
                    let modified = meta
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    out.push(json!({
                        "path": input,
                        "exists": true,
                        "isDir": meta.is_dir(),
                        "size": meta.len(),
                        "modified": modified,
                    }));
                }
                Err(_) => out.push(json!({ "path": input, "exists": false })),
            }
        }
        Ok(Value::Array(out))
    }

    fn create(&self, parent: &str, name: &str, is_dir: bool) -> Result<Value, String> {
        let (_, parent) = self.existing(parent)?;
        monocode_core::files::create_path(&parent, name, is_dir).map(Value::String)
    }

    fn rename(&self, input: &str, name: &str) -> Result<Value, String> {
        let (_, path) = self.existing(input)?;
        monocode_core::files::rename_path(&path, name).map(Value::String)
    }

    fn delete(&self, input: &str) -> Result<Value, String> {
        let (_, path) = self.existing(input)?;
        monocode_core::files::delete_path(&path).map(|()| Value::Null)
    }

    fn copy(&self, from: &str, dest_parent: &str) -> Result<Value, String> {
        let (_, source) = self.existing(from)?;
        let (_, dest) = self.existing(dest_parent)?;
        monocode_core::files::copy_path(&source, &dest).map(Value::String)
    }

    fn move_to(&self, from: &str, dest_parent: &str) -> Result<Value, String> {
        let (_, source) = self.existing(from)?;
        let (_, dest) = self.existing(dest_parent)?;
        monocode_core::files::move_path(&source, &dest).map(Value::String)
    }

    fn git_commit(
        &self,
        cwd: &Path,
        message: &str,
        amend: bool,
        paths: &[String],
    ) -> Result<Value, String> {
        if message.trim().is_empty() || message.len() > 100_000 {
            return Err("Enter a commit message".into());
        }
        // Committing a selection stages only those paths first, so a partial
        // commit does not silently include everything else.
        if !paths.is_empty() {
            let mut args = vec!["--literal-pathspecs", "add", "--"];
            for path in paths {
                args.push(path);
            }
            git::git(cwd, &args)?;
        }
        if amend {
            git::git(cwd, &["commit", "--amend", "-m", message])?;
        } else {
            git::git(cwd, &["commit", "-m", message])?;
        }
        Ok(Value::Null)
    }

    fn git_history(&self, cwd: &Path, limit: u64) -> Result<Value, String> {
        let limit = limit.min(500).to_string();
        // NUL separates fields and RS separates records, so a commit message
        // containing newlines does not break parsing.
        let text = git::git(
            cwd,
            &[
                "log",
                "-n",
                &limit,
                "--format=%H%x00%an%x00%at%x00%s%x1e",
            ],
        )?;
        let mut out = Vec::new();
        for record in text.split('\u{1e}') {
            let mut fields = record.trim_start_matches('\n').split('\0');
            let (Some(sha), Some(author), Some(time), Some(subject)) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if sha.is_empty() {
                continue;
            }
            out.push(json!({
                "sha": sha,
                "author": author,
                "time": time.parse::<u64>().unwrap_or(0),
                "subject": subject,
            }));
        }
        Ok(Value::Array(out))
    }

    fn git_pr(&self, command: &str, args: &Value) -> Result<Value, String> {
        let cwd = self.cwd(
            args.get("cwd")
                .and_then(Value::as_str)
                .ok_or_else(|| "`cwd` is required".to_string())?,
        )?;
        // `gh` is optional; report its absence rather than pretending there is
        // no pull request.
        let mut invocation = std::process::Command::new("gh");
        invocation
            .current_dir(&cwd)
            .env("GH_PROMPT_DISABLED", "1")
            .env("GIT_TERMINAL_PROMPT", "0");
        if command == "git_pr_status" {
            invocation.args(["pr", "view", "--json", "number,url,state,title"]);
        } else {
            invocation.args(["pr", "create", "--fill"]);
        }
        let output = invocation
            .output()
            .map_err(|_| "The GitHub CLI is not installed on this machine".to_string())?;
        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
            // "no pull requests found" is an answer, not a failure.
            if command == "git_pr_status" && message.contains("no pull requests") {
                return Ok(Value::Null);
            }
            return Err(if message.is_empty() {
                "GitHub CLI failed".into()
            } else {
                message
            });
        }
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if command == "git_pr_status" {
            serde_json::from_str(&text).map_err(|e| e.to_string())
        } else {
            Ok(Value::String(text))
        }
    }

    fn search_project(&self, args: &Value) -> Result<Value, String> {
        let options = args
            .get("options")
            .cloned()
            .ok_or_else(|| "`options` is required".to_string())?;
        let cwd = options
            .get("cwd")
            .and_then(Value::as_str)
            .ok_or_else(|| "`options.cwd` is required".to_string())?;
        let root = self.cwd(cwd)?;
        let options: monocode_core::search::project::SearchOptions =
            serde_json::from_value(options).map_err(|e| e.to_string())?;
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let result = monocode_core::search::project::search_project(&root, &options, &cancel)?;
        serde_json::to_value(result).map_err(|e| e.to_string())
    }
}

/// Resolves symlinks on the nearest existing ancestor and re-appends the part
/// that does not exist yet.
fn resolve_existing_ancestor(input: &Path) -> Result<PathBuf, String> {
    let mut actual = input.to_path_buf();
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(&actual) {
            let mut out = real;
            for part in missing.iter().rev() {
                out.push(part);
            }
            return Ok(normalise(&out));
        }
        let Some(parent) = actual.parent() else {
            break;
        };
        if parent == actual {
            break;
        }
        if let Some(name) = actual.file_name() {
            missing.push(name.to_os_string());
        }
        actual = parent.to_path_buf();
    }
    Ok(normalise(input))
}

/// Removes `.` and resolves `..` lexically, without touching the filesystem.
fn normalise(path: &Path) -> PathBuf {
    let mut parts: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(parts.last(), Some(Component::Normal(_))) {
                    parts.pop();
                } else if !matches!(parts.last(), Some(Component::RootDir)) {
                    parts.push(component);
                }
            }
            other => parts.push(other),
        }
    }
    parts.iter().collect()
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Kept for the dispatcher's error paths that need a project-scoped map.
pub type CommandResults = HashMap<String, Value>;
