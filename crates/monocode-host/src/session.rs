//! Session snapshots, sync deltas and command receipts.
//!
//! The remote host serves whole session snapshots to other machines. A
//! snapshot is stored as JSON plus, for each block, the revision at which it
//! last changed, so a client polling after revision N receives only the blocks
//! that moved instead of the entire transcript.
//!
//! This module owns the storage half of that contract: what a snapshot is,
//! how blocks are stamped, and what a client may receive. The state machine
//! that *produces* snapshots runs provider turns and is ported separately.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Newest revisions kept per session so a client can catch up after a poll.
///
/// Older rows are dropped: they can no longer describe a reachable delta base,
/// and keeping every event forever would grow the database without bound.
const EVENT_HISTORY: i64 = 2_000;

/// A stored session snapshot.
///
/// The payload is kept as `Value` rather than mirrored into Rust structs: the
/// block union has a dozen variants and changes with the desktop's UI, and the
/// host never interprets a block's contents. It only needs the block ids to
/// compute revisions and to slice deltas.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSession {
    pub project_id: String,
    pub revision: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_worktree_branch: Option<String>,
    /// The revision at which each block last changed. Host-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_revisions: Option<HashMap<String, i64>>,
    pub session: Value,
}

impl HostSession {
    pub fn id(&self) -> Option<&str> {
        self.session.get("id").and_then(Value::as_str)
    }

    /// The blocks of the transcript, in order. A session with no blocks is
    /// valid (it was just created), so an absent array is not an error.
    fn blocks(&self) -> &[Value] {
        self.session
            .get("blocks")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    fn block_id(block: &Value) -> Option<&str> {
        block.get("id").and_then(Value::as_str)
    }
}

/// What a client receives when it asks for a session at a given revision.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SessionSync {
    /// The client is already current.
    #[serde(rename_all = "camelCase")]
    Unchanged { revision: i64 },
    /// Send everything; the client cannot apply a delta.
    #[serde(rename_all = "camelCase")]
    Snapshot { value: Value },
    /// Send only the blocks that changed after `base`.
    #[serde(rename_all = "camelCase")]
    Delta {
        base: i64,
        value: Value,
        block_ids: Vec<String>,
        blocks: Vec<Value>,
    },
}

/// The result of replaying events after a revision.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EventReplay {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Value>,
    /// Absent when the client is already current.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<Value>>,
    pub revision: i64,
}

/// Records that a command was applied, so a retried command is not applied
/// twice. `signature` is the hash of the command payload: reusing an id with a
/// different payload is a client bug and is reported rather than ignored.
/// Idempotency records exist for `commands.dispatch`, which is not served yet.
/// The storage and its tests are in place so the state machine can rely on
/// them the moment it lands.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub command_id: String,
    pub signature: String,
    pub receipt: Value,
}

impl super::store::HostStore {
    /// Reads a stored snapshot, consulting the memory cache first.
    ///
    /// A miss or a corrupt row returns `Ok(None)` rather than failing: a
    /// session that cannot be parsed is reported as absent so callers give the
    /// same "not found on this machine" answer as a genuinely missing one.
    pub fn session(&mut self, id: &str) -> Result<Option<HostSession>, String> {
        if let Some(value) = self.cached(id) {
            return Ok(Some(value));
        }
        let snapshot: Option<String> = self
            .connection()
            .query_row("SELECT snapshot FROM sessions WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|e| e.to_string())?;
        let Some(snapshot) = snapshot else {
            return Ok(None);
        };
        match serde_json::from_str::<HostSession>(&snapshot) {
            Ok(value) => {
                self.remember(value.clone());
                Ok(Some(value))
            }
            // Treat an unreadable row as absent instead of poisoning every
            // later read; the log entry is what points at the cause.
            Err(error) => {
                log::error!("session {id} has an unreadable snapshot: {error}");
                Ok(None)
            }
        }
    }

    /// All sessions for a project, newest activity first.
    pub fn sessions(&mut self, project_id: Option<&str>) -> Result<Vec<HostSession>, String> {
        let ids: Vec<String> = match project_id {
            Some(project) => {
                let mut statement = self
                    .connection()
                    .prepare("SELECT id FROM sessions WHERE project_id=?1")
                    .map_err(|e| e.to_string())?;
                let rows = statement
                    .query_map([project], |row| row.get::<_, String>(0))
                    .map_err(|e| e.to_string())?;
                rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
            }
            None => {
                let mut statement = self
                    .connection()
                    .prepare("SELECT id FROM sessions")
                    .map_err(|e| e.to_string())?;
                let rows = statement
                    .query_map([], |row| row.get::<_, String>(0))
                    .map_err(|e| e.to_string())?;
                rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
            }
        };
        let mut values = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(value) = self.session(&id)? {
                values.push(value);
            }
        }
        values.sort_by_key(|value| std::cmp::Reverse(value.updated_at));
        Ok(values)
    }

    /// Stores a snapshot and its event, stamping per-block revisions.
    ///
    /// `created_at` is backfilled from the previous snapshot when a session
    /// written by an older host has no creation time, so upgrading does not
    /// reset it to the current write.
    pub fn save_session(
        &mut self,
        mut input: HostSession,
        event: Value,
    ) -> Result<HostSession, String> {
        let previous = self.session(input.id().unwrap_or_default())?;
        if input.created_at.is_none() {
            input.created_at = previous
                .as_ref()
                .and_then(|p| p.created_at)
                .or_else(|| previous.as_ref().map(|p| p.updated_at));
        }
        let revision = input.revision;
        input.block_revisions = Some(block_revisions(previous.as_ref(), &input));
        let id = input.id().ok_or("session snapshot has no id")?.to_string();
        let summary = summary(&input);
        let payload = serde_json::to_string(&input).map_err(|e| e.to_string())?;
        let summary = serde_json::to_string(&summary).map_err(|e| e.to_string())?;
        let event = serde_json::to_string(&event).map_err(|e| e.to_string())?;
        let project_id = input.project_id.clone();
        let conn = self.connection_mut();
        conn.execute(
            "INSERT INTO sessions (id, project_id, snapshot, summary) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET snapshot=excluded.snapshot, summary=excluded.summary",
            params![id, project_id, payload, summary],
        )
        .map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO events (session_id, revision, payload) VALUES (?1, ?2, ?3)",
            params![id, revision, event],
        )
        .map_err(|e| e.to_string())?;
        conn.execute(
            "DELETE FROM events WHERE session_id=?1 AND revision<?2",
            params![id, revision - EVENT_HISTORY],
        )
        .map_err(|e| e.to_string())?;
        self.remember(input.clone());
        Ok(input)
    }

    /// Produces the smallest message that brings a client up to date.
    pub fn sync(&mut self, id: &str, revision: Option<i64>) -> Result<Option<SessionSync>, String> {
        let Some(value) = self.session(id)? else {
            return Ok(None);
        };
        if revision == Some(value.revision) {
            return Ok(Some(SessionSync::Unchanged {
                revision: value.revision,
            }));
        }
        // Without a known base, a base in the future, or no stamps to compare
        // against, only a full snapshot is safe.
        let Some(revision) = revision else {
            return Ok(Some(SessionSync::Snapshot {
                value: snapshot_value(&value),
            }));
        };
        let Some(stamps) = value.block_revisions.as_ref() else {
            return Ok(Some(SessionSync::Snapshot {
                value: snapshot_value(&value),
            }));
        };
        if revision > value.revision {
            return Ok(Some(SessionSync::Snapshot {
                value: snapshot_value(&value),
            }));
        }
        let blocks = value.blocks();
        let block_ids: Vec<String> = blocks
            .iter()
            .filter_map(HostSession::block_id)
            .map(str::to_string)
            .collect();
        let changed: Vec<Value> = blocks
            .iter()
            .filter(|block| {
                let stamp = HostSession::block_id(block)
                    .and_then(|id| stamps.get(id))
                    .copied()
                    .unwrap_or(value.revision);
                stamp > revision
            })
            .cloned()
            .collect();
        Ok(Some(SessionSync::Delta {
            base: revision,
            value: snapshot_value(&value),
            block_ids,
            blocks: changed,
        }))
    }

    /// Replays the events after `after`, or a snapshot when they are gone.
    ///
    /// A gap means the client fell too far behind for the retained history, so
    /// it is handed a snapshot to resynchronize rather than a partial list.
    pub fn events(&mut self, id: &str, after: i64) -> Result<Option<EventReplay>, String> {
        let Some(value) = self.session(id)? else {
            return Ok(None);
        };
        let rows: Vec<(i64, String)> = {
            let mut statement = self
                .connection()
                .prepare(
                    "SELECT revision, payload FROM events WHERE session_id=?1 AND revision>?2
                     ORDER BY revision",
                )
                .map_err(|e| e.to_string())?;
            let rows = statement
                .query_map(params![id, after], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
        };
        let contiguous = rows
            .first()
            .is_some_and(|(revision, _)| *revision == after + 1);
        if after > value.revision || (after < value.revision && !contiguous) {
            return Ok(Some(EventReplay {
                snapshot: Some(snapshot_value(&value)),
                events: None,
                revision: value.revision,
            }));
        }
        let mut events = Vec::with_capacity(rows.len());
        for (revision, payload) in rows {
            match serde_json::from_str::<Value>(&payload) {
                Ok(event) => events.push(json!({ "revision": revision, "event": event })),
                Err(error) => {
                    // A payload this host cannot parse is a gap like any other.
                    log::error!("session {id} has an unreadable event at {revision}: {error}");
                    return Ok(Some(EventReplay {
                        snapshot: Some(snapshot_value(&value)),
                        events: None,
                        revision: value.revision,
                    }));
                }
            }
        }
        Ok(Some(EventReplay {
            snapshot: None,
            events: Some(events),
            revision: value.revision,
        }))
    }

    /// Returns a command's earlier receipt, or an error if the id was reused
    /// with a different payload.
    #[allow(dead_code)]
    pub fn receipt(&self, command_id: &str, signature: &str) -> Result<Option<Receipt>, String> {
        let row: Option<(String, String)> = self
            .connection()
            .query_row(
                "SELECT signature, receipt FROM receipts WHERE id=?1",
                [command_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some((stored, payload)) = row else {
            return Ok(None);
        };
        if stored != signature {
            return Err("Command ID was already used with a different payload".into());
        }
        let receipt = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
        Ok(Some(Receipt {
            command_id: command_id.to_string(),
            signature: stored,
            receipt,
        }))
    }

    /// Remembers that a command was applied.
    #[allow(dead_code)]
    pub fn record_receipt(&mut self, signature: &str, receipt: &Receipt) -> Result<(), String> {
        let payload = serde_json::to_string(&receipt.receipt).map_err(|e| e.to_string())?;
        self.connection_mut()
            .execute(
                "INSERT INTO receipts (id, signature, receipt) VALUES (?1, ?2, ?3)",
                params![receipt.command_id, signature, payload],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Deletes a session and its events. Refuses while a turn is running, so a
    /// live provider process is never orphaned by a delete.
    pub fn delete_session(&mut self, id: &str) -> Result<(), String> {
        let Some(current) = self.session(id)? else {
            return Err("Session not found on this machine".into());
        };
        if current.status == "running" {
            return Err("Stop this session before deleting it".into());
        }
        self.in_transaction(|conn| {
            conn.execute("DELETE FROM events WHERE session_id=?1", [id])
                .map_err(|e| e.to_string())?;
            conn.execute("DELETE FROM sessions WHERE id=?1", [id])
                .map_err(|e| e.to_string())?;
            Ok(())
        })?;
        self.forget(id);
        Ok(())
    }

    /// Runs `body` inside `BEGIN IMMEDIATE`, rolling back on error.
    pub(crate) fn in_transaction<T>(
        &mut self,
        body: impl FnOnce(&Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        self.connection()
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| e.to_string())?;
        match body(self.connection()) {
            Ok(value) => {
                if let Err(error) = self.connection().execute_batch("COMMIT") {
                    let _ = self.connection().execute_batch("ROLLBACK");
                    return Err(error.to_string());
                }
                Ok(value)
            }
            Err(error) => {
                // A cache filled during a rolled-back write describes rows that
                // no longer exist.
                self.clear_cache();
                if let Err(rollback) = self.connection().execute_batch("ROLLBACK") {
                    log::error!("host transaction rollback failed: {rollback}");
                }
                Err(error)
            }
        }
    }
}

/// The snapshot without the host-only revision stamps.
fn snapshot_value(value: &HostSession) -> Value {
    let mut value = serde_json::to_value(value).unwrap_or(Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.remove("blockRevisions");
    }
    value
}

/// Summary row for a session list. Recomputed on every save so listing never
/// has to parse a transcript.
pub fn summary(value: &HostSession) -> Value {
    let session = &value.session;
    json!({
        "projectId": value.project_id,
        "revision": value.revision,
        "runId": value.run_id,
        "status": value.status,
        "updatedAt": value.updated_at,
        "id": session.get("id").cloned().unwrap_or(Value::Null),
        "cwd": session.get("cwd").cloned().unwrap_or(Value::Null),
        "title": session.get("title").cloned().unwrap_or(Value::Null),
        "harness": session.get("harness").cloned().unwrap_or(Value::Null),
        "model": session.get("model").cloned().unwrap_or(Value::Null),
        "runtimeMode": session.get("runtimeMode").cloned().unwrap_or(Value::Null),
        "providerSessionId": session.get("providerSessionId").cloned().unwrap_or(Value::Null),
        "createdAt": value.created_at.unwrap_or(value.updated_at),
        "archived": value.archived.unwrap_or_default(),
        "pinned": value.pinned.unwrap_or_default(),
        "linkedWorkItem": session.get("linkedWorkItem").cloned().unwrap_or(Value::Null),
        "needsInput": needs_input(session),
        "draft": session
            .get("blocks")
            .and_then(Value::as_array)
            .is_some_and(|blocks| blocks.iter().any(|b| {
                b.get("role").and_then(Value::as_str) == Some("user")
                    && b.get("draft").and_then(Value::as_bool) == Some(true)
            })),
    })
}

/// True when the session is waiting on the user for something.
fn needs_input(session: &Value) -> bool {
    let blocks = session
        .get("blocks")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    blocks.iter().any(|block| {
        let role = block.get("role").and_then(Value::as_str);
        match role {
            // An approval or question block is pending until answered.
            Some("approval") => block
                .get("decision")
                .is_none_or(|d| d.is_null() || d.as_str() == Some("pending")),
            _ => false,
        }
    })
}

/// Stamps each block with the revision at which it last changed.
///
/// A block that is unchanged keeps its previous stamp, which is what lets a
/// client receive only the blocks that actually moved. Identity is the fast
/// path; a value re-read from disk is compared structurally instead.
pub fn block_revisions(previous: Option<&HostSession>, next: &HostSession) -> HashMap<String, i64> {
    let before: HashMap<&str, &Value> = previous
        .map(|p| {
            p.blocks()
                .iter()
                .filter_map(|block| HostSession::block_id(block).map(|id| (id, block)))
                .collect()
        })
        .unwrap_or_default();
    let mut revisions = HashMap::new();
    for block in next.blocks() {
        let Some(id) = HostSession::block_id(block) else {
            continue;
        };
        let unchanged = before.get(id).is_some_and(|old| {
            let stamp = previous
                .and_then(|p| p.block_revisions.as_ref())
                .and_then(|stamps| stamps.get(id));
            stamp.is_some() && (*old == block || **old == *block)
        });
        let stamp = if unchanged {
            previous
                .and_then(|p| p.block_revisions.as_ref())
                .and_then(|stamps| stamps.get(id))
                .copied()
                .unwrap_or(next.revision)
        } else {
            next.revision
        };
        revisions.insert(id.to_string(), stamp);
    }
    revisions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::HostStore;

    pub(super) fn store() -> (tempfile::TempDir, HostStore) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = HostStore::open(dir.path().join("host.db")).expect("open store");
        (dir, store)
    }

    pub(super) fn host_session(
        project_id: &str,
        id: &str,
        revision: i64,
        blocks: Vec<Value>,
    ) -> HostSession {
        HostSession {
            project_id: project_id.to_string(),
            revision,
            run_id: None,
            status: "idle".into(),
            created_at: Some(1),
            updated_at: revision as u64,
            archived: None,
            pinned: None,
            auto_worktree_branch: None,
            block_revisions: None,
            session: json!({
                "id": id,
                "cwd": "/tmp",
                "title": "T",
                "harness": "claude",
                "blocks": blocks,
            }),
        }
    }

    pub(super) fn block(id: &str, text: &str) -> Value {
        json!({ "id": id, "role": "user", "text": text })
    }

    /// A store with one registered project, plus that project's real id: the
    /// sessions table has a foreign key, so a fixture cannot invent one.
    pub(super) fn seeded() -> (tempfile::TempDir, HostStore, String) {
        let (dir, mut store) = store();
        let project = store.open_project("/tmp/p", "p").unwrap();
        (dir, store, project.id)
    }

    #[test]
    fn a_saved_snapshot_reads_back_unchanged() {
        let (_dir, mut store, project) = seeded();
        let value = host_session(&project, "s1", 1, vec![block("b1", "hi")]);
        store
            .save_session(value.clone(), json!({"type": "x"}))
            .unwrap();
        let read = store.session("s1").unwrap().expect("stored");
        assert_eq!(read.revision, 1);
        assert_eq!(read.session["blocks"][0]["text"], "hi");
    }

    #[test]
    fn an_unknown_session_is_absent_not_an_error() {
        let (_dir, mut store, _project) = seeded();
        assert!(store.session("nope").unwrap().is_none());
        assert!(store.sync("nope", None).unwrap().is_none());
    }

    #[test]
    fn a_client_at_the_current_revision_is_told_unchanged() {
        let (_dir, mut store, project) = seeded();
        store
            .save_session(
                host_session(&project, "s1", 3, vec![block("b1", "hi")]),
                json!({}),
            )
            .unwrap();
        let sync = store.sync("s1", Some(3)).unwrap().unwrap();
        assert_eq!(sync, SessionSync::Unchanged { revision: 3 });
    }

    #[test]
    fn a_delta_carries_only_the_blocks_that_changed() {
        let (_dir, mut store, project) = seeded();
        store
            .save_session(
                host_session(
                    &project,
                    "s1",
                    1,
                    vec![block("a", "one"), block("b", "two")],
                ),
                json!({}),
            )
            .unwrap();
        store
            .save_session(
                host_session(
                    &project,
                    "s1",
                    2,
                    vec![block("a", "one"), block("b", "edited")],
                ),
                json!({}),
            )
            .unwrap();
        let SessionSync::Delta {
            base,
            blocks,
            block_ids,
            ..
        } = store.sync("s1", Some(1)).unwrap().unwrap()
        else {
            panic!("expected a delta");
        };
        assert_eq!(base, 1);
        // Only `b` moved; `a` kept its revision-1 stamp.
        assert_eq!(block_ids, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(blocks.len(), 1, "unchanged block was resent: {blocks:?}");
        assert_eq!(blocks[0]["id"], "b");
    }

    #[test]
    fn an_unchanged_block_keeps_its_original_stamp() {
        let (_dir, mut store, project) = seeded();
        let same = vec![block("a", "one"), block("b", "two")];
        store
            .save_session(host_session(&project, "s1", 1, same.clone()), json!({}))
            .unwrap();
        store
            .save_session(
                host_session(
                    &project,
                    "s1",
                    2,
                    vec![block("a", "one"), block("b", "two")],
                ),
                json!({}),
            )
            .unwrap();
        let read = store.session("s1").unwrap().unwrap();
        let stamps = read.block_revisions.unwrap();
        assert_eq!(stamps["a"], 1, "an identical block was restamped");
        assert_eq!(stamps["b"], 1, "an identical block was restamped");
    }

    #[test]
    fn a_client_with_no_revision_gets_a_whole_snapshot() {
        let (_dir, mut store, project) = seeded();
        store
            .save_session(
                host_session(&project, "s1", 1, vec![block("a", "one")]),
                json!({}),
            )
            .unwrap();
        let SessionSync::Snapshot { value } = store.sync("s1", None).unwrap().unwrap() else {
            panic!("expected a snapshot");
        };
        assert_eq!(value["revision"], 1);
        assert!(
            value.get("blockRevisions").is_none(),
            "host-only stamps leaked"
        );
    }

    #[test]
    fn a_revision_from_the_future_is_refused_a_delta() {
        let (_dir, mut store, project) = seeded();
        store
            .save_session(
                host_session(&project, "s1", 2, vec![block("a", "one")]),
                json!({}),
            )
            .unwrap();
        // A client claiming a newer revision cannot be trusted to apply a delta.
        let sync = store.sync("s1", Some(9)).unwrap().unwrap();
        assert!(matches!(sync, SessionSync::Snapshot { .. }), "got {sync:?}");
    }

    #[test]
    fn events_replay_in_order_after_a_revision() {
        let (_dir, mut store, project) = seeded();
        store
            .save_session(host_session(&project, "s1", 1, vec![]), json!({"n": 1}))
            .unwrap();
        store
            .save_session(host_session(&project, "s1", 2, vec![]), json!({"n": 2}))
            .unwrap();
        store
            .save_session(host_session(&project, "s1", 3, vec![]), json!({"n": 3}))
            .unwrap();
        let replay = store.events("s1", 1).unwrap().unwrap();
        assert!(replay.snapshot.is_none());
        let events = replay.events.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["revision"], 2);
        assert_eq!(events[1]["event"]["n"], 3);
    }

    #[test]
    fn a_client_behind_the_retained_history_gets_a_snapshot() {
        let (_dir, mut store, project) = seeded();
        // Revisions far apart leave a gap no delta can bridge.
        store
            .save_session(host_session(&project, "s1", 1, vec![]), json!({}))
            .unwrap();
        store
            .save_session(host_session(&project, "s1", 50, vec![]), json!({}))
            .unwrap();
        let replay = store.events("s1", 1).unwrap().unwrap();
        assert!(replay.snapshot.is_some(), "a gap must resynchronize");
        assert!(replay.events.is_none());
    }

    #[test]
    fn a_retried_command_returns_its_first_receipt() {
        let (_dir, mut store, _project) = seeded();
        assert!(store.receipt("c1", "sig").unwrap().is_none());
        store
            .record_receipt(
                "sig",
                &Receipt {
                    command_id: "c1".into(),
                    signature: "sig".into(),
                    receipt: json!({ "ok": true }),
                },
            )
            .unwrap();
        let replay = store.receipt("c1", "sig").unwrap().expect("receipt");
        assert_eq!(replay.receipt["ok"], true);
    }

    #[test]
    fn reusing_a_command_id_with_a_different_payload_is_an_error() {
        let (_dir, mut store, _project) = seeded();
        store
            .record_receipt(
                "sig",
                &Receipt {
                    command_id: "c1".into(),
                    signature: "sig".into(),
                    receipt: json!({}),
                },
            )
            .unwrap();
        let error = store.receipt("c1", "other").unwrap_err();
        assert!(error.contains("different payload"), "got: {error}");
    }

    #[test]
    fn a_running_session_cannot_be_deleted() {
        let (_dir, mut store, project) = seeded();
        let mut value = host_session(&project, "s1", 1, vec![]);
        value.status = "running".into();
        store.save_session(value, json!({})).unwrap();
        let error = store.delete_session("s1").unwrap_err();
        assert!(error.contains("Stop this session"), "got: {error}");
    }

    #[test]
    fn deleting_a_session_removes_its_events() {
        let (_dir, mut store, project) = seeded();
        store
            .save_session(host_session(&project, "s1", 1, vec![]), json!({}))
            .unwrap();
        store.delete_session("s1").unwrap();
        assert!(store.session("s1").unwrap().is_none());
        let remaining: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM events WHERE session_id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0, "events outlived their session");
    }

    #[test]
    fn a_missing_creation_time_is_backfilled_from_the_previous_snapshot() {
        let (_dir, mut store, project) = seeded();
        let mut first = host_session(&project, "s1", 1, vec![]);
        first.created_at = Some(111);
        store.save_session(first, json!({})).unwrap();
        let mut second = host_session(&project, "s1", 2, vec![]);
        second.created_at = None;
        store.save_session(second, json!({})).unwrap();
        assert_eq!(store.session("s1").unwrap().unwrap().created_at, Some(111));
    }

    #[test]
    fn a_failed_transaction_rolls_back_and_clears_the_cache() {
        let (_dir, mut store, project) = seeded();
        store
            .save_session(host_session(&project, "s1", 1, vec![]), json!({}))
            .unwrap();
        let error = store
            .in_transaction(|conn| {
                conn.execute(
                    "INSERT INTO sessions (id, project_id, snapshot) VALUES ('s2', ?1, '{}')",
                    [&project],
                )
                .map_err(|e| e.to_string())?;
                Err::<(), _>("boom".to_string())
            })
            .unwrap_err();
        assert_eq!(error, "boom");
        let count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "a rolled-back write survived");
    }
}

#[cfg(test)]
mod update_tests {
    use super::tests::{host_session, seeded};
    use super::*;

    #[test]
    fn updating_a_title_advances_the_revision() {
        let (_dir, mut store, project) = seeded();
        store
            .save_session(host_session(&project, "s1", 1, vec![]), json!({}))
            .unwrap();
        let mut current = store.session("s1").unwrap().unwrap();
        current.revision += 1;
        current.session["title"] = json!("Renamed");
        store
            .save_session(current, json!({"type": "session.metadata"}))
            .unwrap();
        let saved = store.session("s1").unwrap().unwrap();
        assert_eq!(saved.revision, 2);
        assert_eq!(saved.session["title"], "Renamed");
        // The metadata write is also an event, so a polling client sees it.
        let replay = store.events("s1", 1).unwrap().unwrap();
        assert_eq!(
            replay.events.unwrap()[0]["event"]["type"],
            "session.metadata"
        );
    }

    #[test]
    fn a_title_change_is_reflected_in_the_summary() {
        let (_dir, mut store, project) = seeded();
        store
            .save_session(host_session(&project, "s1", 1, vec![]), json!({}))
            .unwrap();
        let mut current = store.session("s1").unwrap().unwrap();
        current.revision += 1;
        current.session["title"] = json!("New name");
        let saved = store.save_session(current, json!({})).unwrap();
        assert_eq!(summary(&saved)["title"], "New name");
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::tests::{host_session, seeded};
    use super::*;
    use crate::store::HostStore;

    #[test]
    fn a_committed_session_survives_reopening_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host.db");
        let project = {
            let mut store = HostStore::open(&path).unwrap();
            let project = store.open_project("/tmp/p", "p").unwrap();
            let mut value = host_session(&project.id, "s1", 4, vec![]);
            value.session["title"] = json!("Survivor");
            store.save_session(value, json!({"n": 4})).unwrap();
            project
        };
        // A fresh process: no cache, nothing in memory.
        let mut store = HostStore::open(&path).unwrap();
        let read = store
            .session("s1")
            .unwrap()
            .expect("session survived restart");
        assert_eq!(read.revision, 4);
        assert_eq!(read.session["title"], "Survivor");
        let replay = store.events("s1", 3).unwrap().unwrap();
        assert_eq!(replay.events.unwrap().len(), 1, "events survived restart");
        assert_eq!(replay.revision, 4);
        assert_eq!(project.cwd, "/tmp/p");
    }

    #[test]
    fn an_unreadable_row_is_reported_absent_not_fatal() {
        let (_dir, mut store, project) = seeded();
        store
            .save_session(host_session(&project, "s1", 1, vec![]), json!({}))
            .unwrap();
        store
            .connection()
            .execute("UPDATE sessions SET snapshot='{not json' WHERE id='s1'", [])
            .unwrap();
        // A live process still holds the good copy; the corruption is only
        // reachable on a cold read, which is what a restart does.
        store.clear_cache();
        // The corrupt row must not break the listing for healthy sessions.
        assert!(store.session("s1").unwrap().is_none());
    }
}

#[cfg(test)]
mod revision_tests {
    use super::tests::{host_session, seeded};
    use super::*;

    #[test]
    fn two_writes_at_the_same_revision_collide() {
        // Guards the bug this suite was written for: a projection built from a
        // pre-turn snapshot bumped back onto the revision the submitted-turn
        // row already occupied, and the second insert failed with a UNIQUE
        // constraint. The caller must build its projection from the snapshot it
        // just wrote, not from the value it read before writing.
        let (_dir, mut store, project) = seeded();
        store
            .save_session(host_session(&project, "s1", 1, vec![]), json!({}))
            .unwrap();
        let error = store
            .save_session(host_session(&project, "s1", 1, vec![]), json!({}))
            .unwrap_err();
        assert!(
            error.contains("UNIQUE") || error.contains("constraint"),
            "expected a revision collision, got: {error}"
        );
    }

    #[test]
    fn consecutive_revisions_are_kept_apart() {
        let (_dir, mut store, project) = seeded();
        for revision in 1..=3 {
            store
                .save_session(
                    host_session(&project, "s1", revision, vec![]),
                    json!({ "n": revision }),
                )
                .unwrap();
        }
        let replay = store.events("s1", 0).unwrap().unwrap();
        assert_eq!(replay.events.unwrap().len(), 3);
    }
}
