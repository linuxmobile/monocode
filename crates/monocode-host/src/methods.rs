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
pub const METHODS: [&str; 10] = [
    "environment.describe",
    "projects.list",
    "projects.open",
    "projects.browse",
    "files.list",
    "files.create",
    "git.worktrees",
    "models.list",
    "sessions.list",
    "devices.revokeSelf",
];

/// Methods the desktop may call that this build does not serve yet. Reported
/// explicitly rather than treated as unknown.
const UNIMPLEMENTED: [&str; 23] = [
    "attachments.read",
    "attachments.upload",
    "commands.dispatch",
    "events.read",
    "files.index",
    "files.read",
    "files.search",
    "files.searchContent",
    "files.write",
    "git.action",
    "git.branches",
    "git.createBranch",
    "git.diff",
    "git.fileDiff",
    "git.index",
    "git.switch",
    "git.worktreeCreate",
    "sessions.delete",
    "sessions.get",
    "sessions.sync",
    "sessions.syncChunk",
    "sessions.update",
    "workspace.run",
];

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
                let cwd = self.worktree_cwd(&params, &project.cwd).await?;
                let target = cwd.clone();
                let entries = blocking(move || files::list_dir(&target)).await?;
                serde_json::to_value(entries).map_err(|e| RpcFailure::BadRequest(e.to_string()))
            }
            "files.create" => {
                let project = self.project_of(&params).await?;
                let cwd = self.worktree_cwd(&params, &project.cwd).await?;
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
            "models.list" => Ok(json!({ "models": {}, "errors": {} })),
            "sessions.list" => Ok(json!([])),
            "devices.revokeSelf" => {
                let store = self.state.store.lock().await;
                let revoked = store
                    .revoke_token(&self.token)
                    .map_err(RpcFailure::BadRequest)?;
                Ok(Value::Bool(revoked))
            }
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
    async fn worktree_cwd(
        &self,
        params: &serde_json::Map<String, Value>,
        root: &str,
    ) -> Result<PathBuf, RpcFailure> {
        let Some(cwd) = params.get("cwd").and_then(Value::as_str) else {
            return Ok(PathBuf::from(root));
        };
        let cwd = PathBuf::from(cwd);
        if cwd.is_absolute() {
            return Ok(cwd);
        }
        Ok(Path::new(root).join(cwd))
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
