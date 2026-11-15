//! The MonoCode remote host, as a library.
//!
//! The binary in `main.rs` drives this; the library target exists so
//! integration tests can exercise the session store and the turn loop against
//! a real (fake) provider process instead of only through HTTP.

pub mod git;
pub mod methods;
pub mod mutex;
pub mod protocol;
pub mod server;
pub mod session;
pub mod store;
pub mod turn;
pub mod workspace;

use serde_json::json;

/// A minimal session snapshot for tests.
///
/// Building one by hand means every field has to be kept in step with the wire
/// format; this keeps the fixtures honest and short.
pub struct HostSessionFixture {
    id: String,
    blocks: Vec<serde_json::Value>,
}

impl HostSessionFixture {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            blocks: Vec::new(),
        }
    }

    pub fn with_blocks(mut self, blocks: Vec<serde_json::Value>) -> Self {
        self.blocks = blocks;
        self
    }

    pub fn build(self) -> session::HostSession {
        session::HostSession {
            project_id: "project".into(),
            revision: 0,
            run_id: Some("run".into()),
            status: "running".into(),
            created_at: Some(1),
            updated_at: 1,
            archived: None,
            pinned: None,
            auto_worktree_branch: None,
            block_revisions: None,
            session: json!({
                "id": self.id,
                "cwd": "/tmp",
                "title": "Test session",
                "harness": "claude",
                "blocks": self.blocks,
            }),
        }
    }
}
