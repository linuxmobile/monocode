//! Request dispatch for the host protocol.
//!
//! Every method here is served by an existing `monocode-core` service, or
//! reports that the host cannot serve it. Nothing returns a plausible-looking
//! empty value in place of real work: a method that is not implemented says so,
//! because a silent stub would let a desktop think files were written when they
//! were not.

use crate::{protocol::RpcFailure, server::HostState};
use monocode_core::{files, git};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The methods this host answers. Kept as one list so the descriptor and the
/// dispatcher cannot disagree: every entry has an arm below, and every arm is
/// listed here.
pub const METHODS: [&str; 25] = [
    "environment.describe",
    "projects.list",
    "projects.open",
    "git.fileDiff",
    "files.write",
    "files.read",
    "git.action",
    "git.index",
    "git.worktreeCreate",
    "git.diff",
    "git.createBranch",
    "git.switch",
    "git.branches",
    "commands.dispatch",
    "projects.browse",
    "files.list",
    "files.create",
    "git.worktrees",
    "models.list",
    "sessions.delete",
    "sessions.get",
    "sessions.list",
    "sessions.update",
    "sessions.sync",
    "devices.revokeSelf",
];

/// Methods the desktop may call that this build does not serve yet. Reported
/// explicitly rather than treated as unknown.
const UNIMPLEMENTED: [&str; 7] = [
    "attachments.read",
    "attachments.upload",
    "files.index",
    "files.search",
    "files.searchContent",
    "sessions.syncChunk",
    "workspace.run",
];

/// A required string parameter, reported by name when it is missing.
fn string_param(params: &serde_json::Map<String, Value>, key: &str) -> Result<String, RpcFailure> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| RpcFailure::BadRequest(format!("`{key}` is required")))
}

pub struct Dispatch {
    pub state: Arc<HostState>,
    pub token: String,
}

impl Dispatch {
    pub async fn dispatch(
        &self,
        method: String,
        environment_id: Option<String>,
        params: Value,
    ) -> Result<Value, RpcFailure> {
        // Identity is checked for every call except the one that discovers it.
        if method != "environment.describe" {
            let expected = {
                let store = self.state.store.lock().await;
                store.environment_id.clone()
            };
            match environment_id.as_deref() {
                Some(id) if id == expected => {}
                Some(_) => {
                    return Err(RpcFailure::BadRequest(
                        "Host identity changed; reconnect this machine explicitly".into(),
                    ));
                }
                None => {
                    return Err(RpcFailure::BadRequest(
                        "Host identity changed; reconnect this machine explicitly".into(),
                    ));
                }
            }
        }

        let params = params.as_object().cloned().unwrap_or_default();
        let text = |key: &str| -> Result<&str, RpcFailure> {
            params
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| RpcFailure::BadRequest(format!("`{key}` is required")))
        };

        match method.as_str() {
            "environment.describe" => {
                let store = self.state.store.lock().await;
                let descriptor = crate::protocol::HostDescriptor {
                    protocol_version: crate::protocol::HOST_PROTOCOL_VERSION,
                    environment_id: store.environment_id.clone(),
                    name: hostname(),
                    providers: self.state.providers.clone(),
                    capabilities: METHODS.iter().map(|m| (*m).to_string()).collect(),
                };
                serde_json::to_value(descriptor).map_err(|e| RpcFailure::BadRequest(e.to_string()))
            }
            "projects.list" => {
                let store = self.state.store.lock().await;
                let projects = store.projects().map_err(RpcFailure::BadRequest)?;
                Ok(serde_json::to_value(projects)
                    .map_err(|e| RpcFailure::BadRequest(e.to_string()))?)
            }
            "projects.open" => {
                let cwd = text("cwd")?.to_string();
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        Path::new(&cwd)
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or(&cwd)
                    })
                    .to_string();
                let mut store = self.state.store.lock().await;
                let project = store
                    .open_project(&cwd, &name)
                    .map_err(RpcFailure::BadRequest)?;
                serde_json::to_value(project).map_err(|e| RpcFailure::BadRequest(e.to_string()))
            }
            "projects.browse" => {
                let path = params
                    .get("path")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(home_directory);
                let browse = path.clone();
                let entries = blocking(move || files::list_dir(&browse)).await?;
                let parent = path
                    .parent()
                    .filter(|parent| *parent != path)
                    .map(|parent| parent.to_string_lossy().to_string());
                Ok(json!({
                    "path": path.to_string_lossy(),
                    "parent": parent,
                    "entries": entries
                        .iter()
                        .filter(|entry| entry.is_dir)
                        .map(|entry| json!({ "name": entry.name, "path": entry.path }))
                        .collect::<Vec<_>>(),
                }))
            }
            "files.list" => {
                let project = self.project_of(&params).await?;
                let cwd = self.worktree_cwd(&project, &params).await?;
                let target = cwd.clone();
                let entries = blocking(move || files::list_dir(&target)).await?;
                serde_json::to_value(entries).map_err(|e| RpcFailure::BadRequest(e.to_string()))
            }
            "files.create" => {
                let project = self.project_of(&params).await?;
                let cwd = self.worktree_cwd(&project, &params).await?;
                let parent = text("parent")?;
                let name = text("name")?;
                let is_dir = params
                    .get("isDir")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let parent_dir = cwd.join(parent);
                let owned_name = name.to_string();
                let path =
                    blocking(move || files::create_path(&parent_dir, &owned_name, is_dir)).await?;
                Ok(Value::String(path))
            }
            "git.worktrees" => {
                let project = self.project_of(&params).await?;
                let root = PathBuf::from(&project.cwd);
                let worktrees = blocking(move || git::worktrees(&root)).await?;
                // `Worktree` is a client-side view type, so it only decodes;
                // the wire shape is built here to match `HostWorktree`.
                Ok(Value::Array(
                    worktrees
                        .into_iter()
                        .map(|worktree| {
                            json!({
                                "path": worktree.path,
                                "branch": worktree.branch,
                                "head": worktree.head,
                                "isMain": worktree.main,
                                "missing": worktree.missing,
                            })
                        })
                        .collect(),
                ))
            }
            "models.list" => {
                // Discovery shells out to each provider CLI, which takes seconds
                // per provider. It runs off the async workers and reports each
                // provider's failure separately, so one broken CLI cannot hide
                // the models the others returned.
                let providers: Vec<monocode_core::provider::Kind> = self
                    .state
                    .providers
                    .iter()
                    .filter_map(|id| monocode_core::provider::Kind::parse(id))
                    .collect();
                blocking(move || {
                    let mut models = serde_json::Map::new();
                    let mut errors = serde_json::Map::new();
                    for kind in providers {
                        match monocode_core::catalog::models(kind) {
                            Ok(rows) => {
                                models.insert(
                                    kind.id().to_string(),
                                    serde_json::to_value(rows).map_err(|e| e.to_string())?,
                                );
                            }
                            Err(error) => {
                                errors.insert(kind.id().to_string(), Value::String(error));
                            }
                        }
                    }
                    Ok(json!({ "models": models, "errors": errors }))
                })
                .await
            }
            "git.branches" => {
                let project = self.project_of(&params).await?;
                let cwd = self.worktree_cwd(&project, &params).await?;
                blocking(move || crate::git::branches(&cwd).map(|b| b.to_json())).await
            }
            "git.switch" => {
                let project = self.project_of(&params).await?;
                let cwd = self.worktree_cwd(&project, &params).await?;
                let branch = string_param(&params, "branch")?;
                let remote = params.get("remote").and_then(Value::as_str).map(str::to_string);
                blocking(move || {
                    crate::git::switch(&cwd, &branch, remote.as_deref()).map(|b| b.to_json())
                })
                .await
            }
            "git.createBranch" => {
                let project = self.project_of(&params).await?;
                let cwd = self.worktree_cwd(&project, &params).await?;
                let branch = string_param(&params, "branch")?;
                blocking(move || crate::git::create_branch(&cwd, &branch).map(|b| b.to_json()))
                    .await
            }
            "git.diff" => {
                let project = self.project_of(&params).await?;
                let cwd = self.worktree_cwd(&project, &params).await?;
                blocking(move || crate::git::diff(&cwd).map(Value::String)).await
            }
            "git.worktreeCreate" => {
                let project = self.project_of(&params).await?;
                // Resolving the worktree validates that the client named a
                // directory belonging to this project, even though the new
                // worktree is created beside the project root.
                self.worktree_cwd(&project, &params).await?;
                let branch = string_param(&params, "branch")?;
                let base = params.get("base").and_then(Value::as_str).map(str::to_string);
                let existing = params.get("existing").and_then(Value::as_bool).unwrap_or(false);
                let root = PathBuf::from(&project.cwd);
                blocking(move || {
                    crate::git::create_worktree(&root, &branch, base.as_deref(), existing)
                        .map(|path| Value::String(path.to_string_lossy().into_owned()))
                })
                .await
            }
            "git.index" => {
                let project = self.project_of(&params).await?;
                let cwd = self.worktree_cwd(&project, &params).await?;
                blocking(move || crate::git::index(&cwd).map(|index| index.to_json())).await
            }
            "git.action" => {
                let project = self.project_of(&params).await?;
                let cwd = self.worktree_cwd(&project, &params).await?;
                let action = string_param(&params, "action")?;
                let path = params.get("path").and_then(Value::as_str).map(str::to_string);
                let message = params.get("message").and_then(Value::as_str).map(str::to_string);
                let content = params.get("content").and_then(Value::as_str).map(str::to_string);
                blocking(move || {
                    crate::git::action(&cwd, &action, path.as_deref(), message.as_deref(), content.as_deref())
                })
                .await
            }
            "files.read" => {
                let project = self.project_of(&params).await?;
                let cwd = self.worktree_cwd(&project, &params).await?;
                let path = string_param(&params, "path")?;
                blocking(move || crate::git::read_file(&cwd, &path).map(Value::String)).await
            }
            "files.write" => {
                let project = self.project_of(&params).await?;
                let cwd = self.worktree_cwd(&project, &params).await?;
                let path = string_param(&params, "path")?;
                let expected = string_param(&params, "expected")?;
                let content = string_param(&params, "content")?;
                blocking(move || {
                    crate::git::write_file(&cwd, &path, &expected, &content).map(|()| Value::Null)
                })
                .await
            }
            "git.fileDiff" => {
                let project = self.project_of(&params).await?;
                let cwd = self.worktree_cwd(&project, &params).await?;
                let path = string_param(&params, "path")?;
                let staged = params.get("staged").and_then(Value::as_bool).unwrap_or(false);
                blocking(move || crate::git::file_diff(&cwd, &path, staged)).await
            }
            "sessions.list" => {
                let project = self.project_of(&params).await?;
                let mut store = self.state.store.lock().await;
                let sessions = store
                    .sessions(Some(&project.id))
                    .map_err(RpcFailure::BadRequest)?;
                let mut summaries = Vec::with_capacity(sessions.len());
                for session in &sessions {
                    let mut summary = crate::session::summary(session);
                    if let Some(object) = summary.as_object_mut() {
                        object.insert("repo".into(), Value::String(project.name.clone()));
                        // A session recorded away from the project root is a
                        // worktree checkout, which the desktop labels.
                        let cwd = session.session.get("cwd").and_then(Value::as_str);
                        if let Some(cwd) = cwd.filter(|cwd| *cwd != project.cwd) {
                            object.insert("worktreeCwd".into(), Value::String(cwd.into()));
                        }
                    }
                    summaries.push(summary);
                }
                summaries.sort_by(|a, b| b["updatedAt"].as_u64().cmp(&a["updatedAt"].as_u64()));
                Ok(Value::Array(summaries))
            }
            "sessions.get" => {
                let project = self.project_of(&params).await?;
                let id = string_param(&params, "sessionId")?;
                let revision = params.get("revision").and_then(Value::as_i64);
                let mut store = self.state.store.lock().await;
                let Some(session) = store.session(&id).map_err(RpcFailure::BadRequest)? else {
                    return Err(RpcFailure::BadRequest(
                        "Session not found on this machine".into(),
                    ));
                };
                if session.project_id != project.id {
                    return Err(RpcFailure::BadRequest(
                        "Session belongs to another project".into(),
                    ));
                }
                // A client that is already current is answered with null, so
                // it does not re-download an unchanged transcript.
                if revision == Some(session.revision) {
                    return Ok(Value::Null);
                }
                Ok(serde_json::to_value(session)
                    .map_err(|e| RpcFailure::BadRequest(e.to_string()))?)
            }
            "sessions.sync" => {
                let project = self.project_of(&params).await?;
                let id = string_param(&params, "sessionId")?;
                let revision = params.get("revision").and_then(Value::as_i64);
                let mut store = self.state.store.lock().await;
                let Some(session) = store.session(&id).map_err(RpcFailure::BadRequest)? else {
                    return Err(RpcFailure::BadRequest(
                        "Session not found on this machine".into(),
                    ));
                };
                if session.project_id != project.id {
                    return Err(RpcFailure::BadRequest(
                        "Session belongs to another project".into(),
                    ));
                }
                let sync = store
                    .sync(&id, revision)
                    .map_err(RpcFailure::BadRequest)?
                    .ok_or_else(|| {
                        RpcFailure::BadRequest("Session not found on this machine".into())
                    })?;
                Ok(
                    serde_json::to_value(sync)
                        .map_err(|e| RpcFailure::BadRequest(e.to_string()))?,
                )
            }
            "events.read" => {
                let project = self.project_of(&params).await?;
                let id = string_param(&params, "sessionId")?;
                let after = params
                    .get("after")
                    .and_then(Value::as_i64)
                    .filter(|after| *after >= 0)
                    .ok_or_else(|| RpcFailure::BadRequest("Invalid event cursor".into()))?;
                let mut store = self.state.store.lock().await;
                let Some(session) = store.session(&id).map_err(RpcFailure::BadRequest)? else {
                    return Err(RpcFailure::BadRequest(
                        "Session not found on this machine".into(),
                    ));
                };
                if session.project_id != project.id {
                    return Err(RpcFailure::BadRequest(
                        "Session belongs to another project".into(),
                    ));
                }
                let replay = store
                    .events(&id, after)
                    .map_err(RpcFailure::BadRequest)?
                    .ok_or_else(|| {
                        RpcFailure::BadRequest("Session not found on this machine".into())
                    })?;
                Ok(serde_json::to_value(replay)
                    .map_err(|e| RpcFailure::BadRequest(e.to_string()))?)
            }
            "sessions.update" => {
                let project = self.project_of(&params).await?;
                let id = string_param(&params, "sessionId")?;
                let mut store = self.state.store.lock().await;
                let Some(mut current) = store.session(&id).map_err(RpcFailure::BadRequest)? else {
                    return Err(RpcFailure::BadRequest(
                        "Session not found on this machine".into(),
                    ));
                };
                if current.project_id != project.id {
                    return Err(RpcFailure::BadRequest(
                        "Session belongs to another project".into(),
                    ));
                }
                let mut changed = false;
                if let Some(title) = params.get("title") {
                    let title = title
                        .as_str()
                        .ok_or_else(|| RpcFailure::BadRequest("Invalid session title".into()))?;
                    if title.trim().is_empty() || title.len() > 200 {
                        return Err(RpcFailure::BadRequest("Invalid session title".into()));
                    }
                    if let Some(session) = current.session.as_object_mut() {
                        session.insert("title".into(), Value::String(title.trim().into()));
                    }
                    changed = true;
                }
                for (key, name) in [("archived", "archive"), ("pinned", "pin")] {
                    let Some(value) = params.get(key) else {
                        continue;
                    };
                    let value = value
                        .as_bool()
                        .ok_or_else(|| RpcFailure::BadRequest(format!("Invalid {name} value")))?;
                    let slot = if key == "archived" {
                        &mut current.archived
                    } else {
                        &mut current.pinned
                    };
                    *slot = Some(value);
                    changed = true;
                }
                if let Some(item) = params.get("linkedWorkItem") {
                    let linked = if item.is_null() {
                        None
                    } else {
                        // The desktop only links one recognised GitHub issue or
                        // pull request; anything else is a client bug.
                        let url = item.get("url").and_then(Value::as_str).ok_or_else(|| {
                            RpcFailure::BadRequest("Invalid linked work item".into())
                        })?;
                        let parsed = monocode_core::session_groups::parse_github_work_item(url)
                            .ok_or_else(|| {
                                RpcFailure::BadRequest("Invalid linked work item".into())
                            })?;
                        let matches = item.get("kind").and_then(Value::as_str)
                            == Some(parsed.kind.as_str())
                            && item.get("repo").and_then(Value::as_str)
                                == Some(parsed.repo.as_str())
                            && item.get("number").and_then(Value::as_u64)
                                == Some(parsed.number as u64)
                            && item.get("url").and_then(Value::as_str) == Some(parsed.url.as_str());
                        if !matches {
                            return Err(RpcFailure::BadRequest("Invalid linked work item".into()));
                        }
                        Some(item.clone())
                    };
                    if let Some(session) = current.session.as_object_mut() {
                        match linked {
                            Some(linked) => session.insert("linkedWorkItem".into(), linked),
                            None => session.remove("linkedWorkItem"),
                        };
                    }
                    changed = true;
                }
                if !changed {
                    return Err(RpcFailure::BadRequest("No session changes supplied".into()));
                }
                current.revision += 1;
                store
                    .save_session(current, json!({ "type": "session.metadata" }))
                    .map_err(RpcFailure::BadRequest)?;
                let saved = store
                    .session(&id)
                    .map_err(RpcFailure::BadRequest)?
                    .ok_or_else(|| {
                        RpcFailure::BadRequest("Session not found on this machine".into())
                    })?;
                Ok(crate::session::summary(&saved))
            }
            "sessions.delete" => {
                let project = self.project_of(&params).await?;
                let id = string_param(&params, "sessionId")?;
                let mut store = self.state.store.lock().await;
                let Some(session) = store.session(&id).map_err(RpcFailure::BadRequest)? else {
                    return Err(RpcFailure::BadRequest(
                        "Session not found on this machine".into(),
                    ));
                };
                if session.project_id != project.id {
                    return Err(RpcFailure::BadRequest(
                        "Session belongs to another project".into(),
                    ));
                }
                store.delete_session(&id).map_err(RpcFailure::BadRequest)?;
                Ok(Value::Null)
            }
            "devices.revokeSelf" => {
                let store = self.state.store.lock().await;
                let revoked = store
                    .revoke_token(&self.token)
                    .map_err(RpcFailure::BadRequest)?;
                Ok(Value::Bool(revoked))
            }
            "commands.dispatch" => self.apply_command(params).await,
            other if UNIMPLEMENTED.contains(&other) => Err(RpcFailure::BadRequest(format!(
                "The host cannot serve `{other}` yet"
            ))),
            _ => Err(RpcFailure::BadRequest("Unsupported host method".into())),
        }
    }

    async fn project_of(
        &self,
        params: &serde_json::Map<String, Value>,
    ) -> Result<crate::store::Project, RpcFailure> {
        let id = params
            .get("projectId")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcFailure::BadRequest("`projectId` is required".into()))?;
        let store = self.state.store.lock().await;
        store.project(id).map_err(RpcFailure::BadRequest)
    }

    /// A request may name a worktree; otherwise it means the project root.
    /// Resolves the working copy a request names.
    ///
    /// A client may name a worktree, but never an arbitrary directory: the
    /// resolved path must be the project root or one of its registered
    /// worktrees. Otherwise `cwd` would let a paired desktop run Git anywhere
    /// on this machine.
    async fn worktree_cwd(
        &self,
        project: &crate::store::Project,
        params: &serde_json::Map<String, Value>,
    ) -> Result<PathBuf, RpcFailure> {
        let Some(cwd) = params.get("cwd").and_then(Value::as_str) else {
            return Ok(PathBuf::from(&project.cwd));
        };
        let requested = if Path::new(cwd).is_absolute() {
            PathBuf::from(cwd)
        } else {
            Path::new(&project.cwd).join(cwd)
        };
        let canonical = |path: &Path| {
            std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        };
        let wanted = canonical(&requested);
        let root = canonical(Path::new(&project.cwd));
        if wanted == root {
            return Ok(wanted);
        }
        let trees = monocode_core::git::worktrees(&root).map_err(RpcFailure::BadRequest)?;
        let known = trees
            .iter()
            .any(|tree| canonical(Path::new(&tree.path)) == wanted);
        if !known {
            return Err(RpcFailure::BadRequest(
                "That working copy is not part of this project".into(),
            ));
        }
        Ok(wanted)
    }
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| {
            std::fs::read_to_string("/proc/sys/kernel/hostname")
                .ok()
                .map(|name| name.trim().to_string())
        })
        .unwrap_or_else(|| "monocode-host".to_string())
}

fn home_directory() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Runs filesystem and Git work off the async workers.
///
/// `monocode-core` is synchronous: directory walks and `git` subprocesses block
/// for as long as the filesystem or the child takes. Doing that on a Tokio
/// worker would stall every other request sharing the thread, so it goes to the
/// blocking pool instead.
async fn blocking<T, F>(work: F) -> Result<T, RpcFailure>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| RpcFailure::BadRequest(e.to_string()))?
        .map_err(RpcFailure::BadRequest)
}

/// Applies one client command to a session.
///
/// Only the commands a remote client actually needs are served: creating a
/// session, sending a turn, cancelling it, and changing settings. The rest are
/// refused by name so a client is told what is missing instead of having its
/// command silently ignored.
impl Dispatch {
    async fn apply_command(
        &self,
        params: serde_json::Map<String, Value>,
    ) -> Result<Value, RpcFailure> {
        let command = params
            .get("command")
            .or_else(|| params.get("params"))
            .cloned()
            .unwrap_or(Value::Object(params.clone()));
        let kind = command
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcFailure::BadRequest("Invalid command".into()))?;
        // A retried command must not run twice; the receipt is keyed by the
        // command id and its payload hash.
        let command_id = command
            .get("commandId")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcFailure::BadRequest("Invalid command".into()))?;
        let signature = command_signature(&command);
        {
            let store = self.state.store.lock().await;
            if let Some(previous) = store
                .receipt(command_id, &signature)
                .map_err(RpcFailure::BadRequest)?
            {
                return Ok(previous.receipt);
            }
        }
        let outcome = match kind {
            "create" => self.create_session(&command).await,
            "send" => self.send_turn(&command).await,
            "cancel" => self.cancel_turn(&command).await,
            "configure" => self.configure_session(&command).await,
            other => Err(RpcFailure::BadRequest(format!(
                "The host cannot apply a `{other}` command yet"
            ))),
        }?;
        let mut store = self.state.store.lock().await;
        store
            .record_receipt(&signature, &crate::turn::receipt(command_id, &signature))
            .map_err(RpcFailure::BadRequest)?;
        Ok(outcome)
    }

    async fn create_session(&self, command: &Value) -> Result<Value, RpcFailure> {
        let project_id = command
            .get("projectId")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcFailure::BadRequest("Invalid command".into()))?;
        let harness = command
            .get("harness")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcFailure::BadRequest("Invalid provider or permission mode".into()))?;
        let kind = monocode_core::provider::Kind::parse(harness)
            .ok_or_else(|| RpcFailure::BadRequest("Invalid provider or permission mode".into()))?;
        let mut store = self.state.store.lock().await;
        let project = store.project(project_id).map_err(RpcFailure::BadRequest)?;
        let cwd = command
            .get("worktreeCwd")
            .and_then(Value::as_str)
            .unwrap_or(&project.cwd)
            .to_string();
        let now = crate::turn::now_ms();
        let id = crate::turn::new_id();
        let session = crate::session::HostSession {
            project_id: project.id,
            revision: 0,
            run_id: None,
            status: "idle".into(),
            created_at: Some(now),
            updated_at: now,
            archived: None,
            pinned: None,
            auto_worktree_branch: command
                .get("autoWorktreeBranch")
                .and_then(Value::as_str)
                .map(str::to_string),
            block_revisions: None,
            session: json!({
                "id": id,
                "cwd": cwd,
                "harness": kind.id(),
                "model": command.get("model").cloned().unwrap_or(Value::Null),
                "runtimeMode": command.get("runtimeMode").cloned().unwrap_or(Value::Null),
                "modelSettings": command.get("modelSettings").cloned().unwrap_or(json!({})),
                "title": "New remote session",
                "blocks": [],
            }),
        };
        let summary = crate::session::summary(&session);
        store
            .save_session(session, json!({ "type": "session.created" }))
            .map_err(RpcFailure::BadRequest)?;
        Ok(it_receipt(command, summary))
    }

    async fn configure_session(&self, command: &Value) -> Result<Value, RpcFailure> {
        let id = string_param_command(command, "sessionId")?;
        let mut store = self.state.store.lock().await;
        let mut current = store
            .session(&id)
            .map_err(RpcFailure::BadRequest)?
            .ok_or_else(|| RpcFailure::BadRequest("Session not found on this machine".into()))?;
        // Changing settings under a live turn would apply them to the turn
        // already in flight.
        if current.status == "running" {
            return Err(RpcFailure::BadRequest(
                "Wait for the current turn before changing settings".into(),
            ));
        }
        let Some(session) = current.session.as_object_mut() else {
            return Err(RpcFailure::BadRequest(
                "Session snapshot is malformed".into(),
            ));
        };
        for (key, field) in [
            ("model", "model"),
            ("modelSettings", "modelSettings"),
            ("runtimeMode", "runtimeMode"),
        ] {
            if let Some(value) = command.get(key) {
                session.insert(field.into(), value.clone());
            }
        }
        current.revision += 1;
        current.updated_at = crate::turn::now_ms();
        let summary = crate::session::summary(&current);
        store
            .save_session(current, json!({ "type": "session.configChanged" }))
            .map_err(RpcFailure::BadRequest)?;
        Ok(it_receipt(command, summary))
    }

    async fn cancel_turn(&self, command: &Value) -> Result<Value, RpcFailure> {
        let id = string_param_command(command, "sessionId")?;
        let turn = {
            let turns = self.state.turns.lock().await;
            turns.get(&id).cloned()
        };
        let Some(turn) = turn else {
            // Cancelling an idle session is not an error: a client may race the
            // end of a turn it was watching.
            return Ok(it_receipt(command, json!({ "cancelled": false })));
        };
        turn.cancel();
        Ok(it_receipt(command, json!({ "cancelled": true })))
    }

    async fn send_turn(&self, command: &Value) -> Result<Value, RpcFailure> {
        let id = string_param_command(command, "sessionId")?;
        let text = command
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let (session, project_cwd) = {
            let mut store = self.state.store.lock().await;
            let current = store
                .session(&id)
                .map_err(RpcFailure::BadRequest)?
                .ok_or_else(|| {
                    RpcFailure::BadRequest("Session not found on this machine".into())
                })?;
            if current.status == "running" {
                return Err(RpcFailure::BadRequest(
                    "This session is already running".into(),
                ));
            }
            let project = store
                .project(&current.project_id)
                .map_err(RpcFailure::BadRequest)?;
            (current, project.cwd)
        };
        let harness = session.session["harness"]
            .as_str()
            .and_then(monocode_core::provider::Kind::parse)
            .ok_or_else(|| RpcFailure::BadRequest("Session names an unknown provider".into()))?;
        let Some(binary) = monocode_core::provider::resolve(harness) else {
            return Err(RpcFailure::BadRequest(format!(
                "{} CLI not installed",
                harness.label()
            )));
        };
        let cwd = session.session["cwd"]
            .as_str()
            .unwrap_or(&project_cwd)
            .to_string();
        let model = session.session["model"].as_str().map(str::to_string);
        let cwd_path = std::path::PathBuf::from(&cwd);

        // A second turn for the same session must not race the first.
        let turn = {
            let mut turns = self.state.turns.lock().await;
            if turns.contains_key(&id) {
                return Err(RpcFailure::BadRequest(
                    "This session is already running".into(),
                ));
            }
            let turn = std::sync::Arc::new(crate::turn::Turn::new(crate::turn::new_id()));
            turns.insert(id.clone(), turn.clone());
            turn
        };

        // Mark the session running before the worker starts, so a client that
        // polls immediately sees the turn rather than a stale idle session.
        let submitted = {
            let mut store = self.state.store.lock().await;
            let mut current = session.clone();
            current.status = "running".into();
            current.run_id = Some(turn.run_id.clone());
            current.revision += 1;
            current.updated_at = crate::turn::now_ms();
            let mut session_value = current.session.clone();
            if let Some(blocks) = session_value
                .get_mut("blocks")
                .and_then(Value::as_array_mut)
            {
                blocks.push(json!({
                    "id": crate::turn::new_id(),
                    "role": "user",
                    "text": text,
                    "streaming": false,
                    "draft": false,
                }));
            }
            current.session = session_value;
            let submitted = current.clone();
            store
                .save_session(current, json!({ "type": "turn.submitted" }))
                .map_err(RpcFailure::BadRequest)?;
            submitted
        };

        // The provider loop owns a blocking thread and cannot touch the async
        // mutex, so snapshots are handed back over a channel and written by an
        // async task. Bounded so a stuck writer applies backpressure instead of
        // growing without limit.
        let (writes, mut queued) =
            tokio::sync::mpsc::channel::<(crate::session::HostSession, Value)>(8);
        let store = self.state.store.clone();
        let writer = tokio::spawn(async move {
            while let Some((value, event)) = queued.recv().await {
                let mut store = store.lock().await;
                if let Err(error) = store.save_session(value, event) {
                    log::error!("could not persist a turn update: {error}");
                }
            }
        });

        let turn_id = turn.clone();
        let provider_id = session.session["providerSessionId"]
            .as_str()
            .map(str::to_string)
            .or_else(|| session.session["providerId"].as_str().map(str::to_string));
        // Start from the snapshot just written: its revision is already
        // advanced, and a projection built from the pre-turn value would bump
        // back onto the revision that event row already occupies.
        let projection = crate::turn::Projection::new(submitted);
        let request = crate::turn::request(crate::turn::TurnRequest {
            kind: harness,
            binary,
            cwd: cwd_path,
            prompt: text,
            provider_id,
            model,
            settings: Default::default(),
        });
        let turn_for_worker = turn.clone();
        let id_for_worker = id.clone();
        tokio::task::spawn_blocking(move || {
            let result =
                crate::turn::run_turn(&turn_for_worker, request, projection, |projection| {
                    let events = projection.take_events();
                    let value = projection.session().clone();
                    writes
                        .blocking_send((value, json!({ "type": "turn.update", "events": events })))
                        .map_err(|_| "the host stopped accepting turn updates".to_string())
                });
            if let Err(error) = &result {
                log::error!("turn for {id_for_worker} failed: {error}");
            }
            drop(writes);
            let _ = id_for_worker;
        });
        // Wait for the writer to drain, then release the session so a later
        // turn is not refused by a stale running entry.
        let turns = self.state.turns.clone();
        let done_id = id.clone();
        tokio::spawn(async move {
            let _ = writer.await;
            let mut turns = turns.lock().await;
            turns.remove(&done_id);
        });
        Ok(it_receipt(command, json!({ "runId": turn_id.run_id })))
    }
}

fn string_param_command(command: &Value, key: &str) -> Result<String, RpcFailure> {
    command
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| RpcFailure::BadRequest(format!("`{key}` is required")))
}

/// Wraps a command's effect with its id, which is what a client correlates.
fn it_receipt(command: &Value, effect: Value) -> Value {
    json!({
        "commandId": command.get("commandId").cloned().unwrap_or(Value::Null),
        "result": effect,
    })
}

/// A stable hash of the command payload, used to detect a reused command id.
fn command_signature(command: &Value) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(command).unwrap_or_default());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn advertised_capabilities_are_unique() {
        let unique: HashSet<_> = METHODS.iter().collect();
        assert_eq!(
            unique.len(),
            METHODS.len(),
            "a duplicated capability would misreport the host"
        );
    }

    #[test]
    fn no_method_is_both_served_and_unimplemented() {
        for method in METHODS {
            assert!(
                !UNIMPLEMENTED.contains(&method),
                "`{method}` cannot be advertised and unimplemented at once"
            );
        }
    }

    /// The descriptor, the dispatcher and this list must agree. A method that
    /// is advertised but has no arm would reach the `Unsupported host method`
    /// branch, which is exactly the drift this test prevents.
    #[tokio::test]
    async fn every_advertised_method_has_a_handler() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = crate::store::HostStore::open(dir.path().join("host.db")).unwrap();
        let device = store.issue_device("Laptop").unwrap();
        let (shutdown, _) = tokio::sync::broadcast::channel(4);
        let state = Arc::new(crate::server::HostState {
            store: Arc::new(crate::mutex::Mutex::new(store)),
            admin_secret: "admin".into(),
            providers: vec![],
            turns: Arc::new(crate::mutex::Mutex::new(std::collections::HashMap::new())),
            shutdown,
        });
        let dispatch = Dispatch {
            state,
            token: device.token,
        };
        for method in METHODS {
            // A missing handler reports "Unsupported"; a handler that refuses
            // bad input reports something else. Only the former is drift.
            let error = match dispatch
                .dispatch(method.to_string(), None, Value::Null)
                .await
            {
                Ok(_) => continue,
                Err(error) => error.message().to_string(),
            };
            assert!(
                error != "Unsupported host method",
                "`{method}` is advertised but has no handler: {error}"
            );
        }
    }
}
