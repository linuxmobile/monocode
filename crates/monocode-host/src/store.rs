//! The remote host's own database, at `<data-dir>/host.db`.
//!
//! This is deliberately separate from the desktop's database: a host stores
//! snapshots, device credentials and revisions for sessions it serves to other
//! machines, and the desktop's schema has no equivalent tables. The schema
//! below matches the TypeScript host's table names so an existing
//! `host.db` opens unchanged.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A project registered on this machine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    pub id: String,
    pub cwd: String,
    pub name: String,
}

/// A paired desktop credential. The token itself is never stored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
}

/// A newly issued credential. The `token` is returned once and cannot be
/// recovered from the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssuedDevice {
    pub id: String,
    pub name: String,
    pub token: String,
    pub environment_id: String,
}

pub struct HostStore {
    conn: Connection,
    pub environment_id: String,
    /// Recently used snapshots, so a polling client does not make the host
    /// re-parse a whole transcript on every request. Bounded by insertion
    /// order; the oldest entry is dropped when it fills.
    cache: Vec<(String, crate::session::HostSession)>,
    /// Where uploaded attachment bytes live. One file per attachment id, so a
    /// client-supplied path is never used to locate them.
    attachment_dir: PathBuf,
}

impl HostStore {
    /// Opens or creates the database, preserving an existing `environment_id`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let conn = Connection::open(path).map_err(|e| format!("open {path:?}: {e}"))?;
        let attachment_dir = path
            .parent()
            .unwrap_or(Path::new("."))
            .join("attachments");
        conn.execute_batch(
            "PRAGMA busy_timeout=5000;
             PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, cwd TEXT NOT NULL UNIQUE, name TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), snapshot TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS receipts (id TEXT PRIMARY KEY, signature TEXT NOT NULL, receipt TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS events (session_id TEXT NOT NULL REFERENCES sessions(id), revision INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(session_id, revision));
             CREATE TABLE IF NOT EXISTS devices (id TEXT PRIMARY KEY, name TEXT NOT NULL, hash TEXT NOT NULL UNIQUE);",
        )
        .map_err(|e| format!("create schema: {e}"))?;

        // Older databases predate the `summary` column.
        let has_summary = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(sessions)")
                .map_err(|e| e.to_string())?;
            let names = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            names.iter().any(|name| name == "summary")
        };
        if !has_summary {
            conn.execute_batch("ALTER TABLE sessions ADD COLUMN summary TEXT")
                .map_err(|e| format!("add summary column: {e}"))?;
        }

        conn.execute(
            "INSERT OR IGNORE INTO metadata VALUES ('environmentId', ?1)",
            params![uuid::Uuid::new_v4().to_string()],
        )
        .map_err(|e| e.to_string())?;
        let environment_id: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key='environmentId'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("read environmentId: {e}"))?;

        Ok(Self {
            conn,
            environment_id,
            cache: Vec::new(),
            attachment_dir,
        })
    }

    /// The live connection, for reads that do not need a transaction.
    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }

    pub(crate) fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Most recently used snapshots, oldest first.
    pub(crate) fn cached(&mut self, id: &str) -> Option<crate::session::HostSession> {
        let index = self.cache.iter().position(|(key, _)| key == id)?;
        let entry = self.cache.remove(index);
        let value = entry.1.clone();
        self.remember(value.clone());
        Some(value)
    }

    pub(crate) fn remember(&mut self, value: crate::session::HostSession) {
        let Some(id) = value.id().map(str::to_string) else {
            return;
        };
        self.cache.retain(|(key, _)| key != &id);
        self.cache.push((id, value));
        if self.cache.len() > 32 {
            self.cache.remove(0);
        }
    }

    pub(crate) fn forget(&mut self, id: &str) {
        self.cache.retain(|(key, _)| key != id);
    }

    pub(crate) fn clear_cache(&mut self) {
        self.cache.clear();
    }

    pub fn project(&self, id: &str) -> Result<Project, String> {
        self.conn
            .query_row(
                "SELECT id, cwd, name FROM projects WHERE id=?1",
                params![id],
                |row| {
                    Ok(Project {
                        id: row.get(0)?,
                        cwd: row.get(1)?,
                        name: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Project is not registered on this machine".to_string())
    }

    pub fn projects(&self) -> Result<Vec<Project>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, cwd, name FROM projects ORDER BY name")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Project {
                    id: row.get(0)?,
                    cwd: row.get(1)?,
                    name: row.get(2)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// Registers a folder and returns its project row.
    /// Where attachment bytes live. Callers join a validated attachment id.
    pub fn attachment_dir(&self) -> &Path {
        &self.attachment_dir
    }

    pub fn open_project(&mut self, cwd: &str, name: &str) -> Result<Project, String> {
        self.conn
            .execute(
                "INSERT INTO projects (id, cwd, name) VALUES (?1, ?2, ?3)
                 ON CONFLICT(cwd) DO UPDATE SET name=excluded.name",
                params![uuid::Uuid::new_v4().to_string(), cwd, name],
            )
            .map_err(|e| e.to_string())?;
        self.conn
            .query_row(
                "SELECT id, cwd, name FROM projects WHERE cwd=?1",
                params![cwd],
                |row| {
                    Ok(Project {
                        id: row.get(0)?,
                        cwd: row.get(1)?,
                        name: row.get(2)?,
                    })
                },
            )
            .map_err(|e| e.to_string())
    }

    /// Issues a credential. The token is shown once; only its hash is stored.
    pub fn issue_device(&mut self, name: &str) -> Result<IssuedDevice, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let token = random_token();
        self.conn
            .execute(
                "INSERT INTO devices (id, name, hash) VALUES (?1, ?2, ?3)",
                params![id, name, hash_token(&token)],
            )
            .map_err(|e| e.to_string())?;
        Ok(IssuedDevice {
            id,
            name: name.to_string(),
            token,
            environment_id: self.environment_id.clone(),
        })
    }

    pub fn devices(&self) -> Result<Vec<Device>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name FROM devices ORDER BY name")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Device {
                    id: row.get(0)?,
                    name: row.get(1)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn revoke_device(&self, id: &str) -> Result<bool, String> {
        self.conn
            .execute("DELETE FROM devices WHERE id=?1", params![id])
            .map(|changed| changed > 0)
            .map_err(|e| e.to_string())
    }

    /// Lets a desktop revoke only the credential it is currently using.
    pub fn revoke_token(&self, token: &str) -> Result<bool, String> {
        self.conn
            .execute(
                "DELETE FROM devices WHERE hash=?1",
                params![hash_token(token)],
            )
            .map(|changed| changed > 0)
            .map_err(|e| e.to_string())
    }

    pub fn authenticated(&self, token: &str) -> Result<bool, String> {
        self.conn
            .query_row(
                "SELECT id FROM devices WHERE hash=?1",
                params![hash_token(token)],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map(|found| found.is_some())
            .map_err(|e| e.to_string())
    }
}

/// A 32-byte credential in base64url without padding: 43 characters, matching
/// the length the desktop validates before it will connect.
pub fn random_token() -> String {
    use base64::Engine as _;
    use rand::TryRngCore as _;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut bytes)
        .expect("the OS random source is available");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn hash_token(token: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, HostStore) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = HostStore::open(dir.path().join("host.db")).expect("open store");
        (dir, store)
    }

    #[test]
    fn a_token_is_43_url_safe_characters() {
        let token = random_token();
        assert_eq!(token.len(), 43, "desktop rejects other lengths: {token}");
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "token must survive base64url: {token}"
        );
    }

    #[test]
    fn tokens_are_unique_and_never_stored_in_clear() {
        let (dir, mut store) = store();
        let first = store.issue_device("Laptop").unwrap();
        let second = store.issue_device("Desk").unwrap();
        assert_ne!(first.token, second.token);

        // The database file must not contain the credential itself.
        let bytes = std::fs::read(dir.path().join("host.db")).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(&first.token),
            "the issued token must not be written to the database"
        );
        // WAL pages can hold it too, so check those as well.
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let path = entry.unwrap().path();
            if let Ok(bytes) = std::fs::read(&path) {
                let text = String::from_utf8_lossy(&bytes);
                assert!(
                    !text.contains(&first.token),
                    "{} must not contain the token",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn only_issued_tokens_authenticate() {
        let (_dir, mut store) = store();
        let device = store.issue_device("Laptop").unwrap();
        assert!(store.authenticated(&device.token).unwrap());
        assert!(!store.authenticated("wrong").unwrap());
        assert!(!store.authenticated(&format!("{}a", device.token)).unwrap());
    }

    #[test]
    fn a_revoked_device_stops_authenticating() {
        let (_dir, mut store) = store();
        let device = store.issue_device("Laptop").unwrap();
        assert!(store.revoke_device(&device.id).unwrap());
        assert!(!store.authenticated(&device.token).unwrap());
        // Revoking twice reports nothing was removed.
        assert!(!store.revoke_device(&device.id).unwrap());
    }

    #[test]
    fn a_device_can_revoke_its_own_token() {
        let (_dir, mut store) = store();
        let device = store.issue_device("Laptop").unwrap();
        assert!(store.revoke_token(&device.token).unwrap());
        assert!(!store.authenticated(&device.token).unwrap());
    }

    #[test]
    fn reopening_keeps_the_environment_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host.db");
        let first = HostStore::open(&path).unwrap().environment_id;
        let second = HostStore::open(&path).unwrap().environment_id;
        assert_eq!(first, second, "a reconnect must see the same host");
        assert!(!first.is_empty());
    }

    #[test]
    fn a_failed_transaction_rolls_back() {
        let (_dir, mut store) = store();
        let error = store
            .in_transaction(|conn| {
                conn.execute(
                    "INSERT INTO projects (id, cwd, name) VALUES ('p', '/tmp/x', 'x')",
                    [],
                )
                .map_err(|e| e.to_string())?;
                Err::<(), _>("deliberate".to_string())
            })
            .unwrap_err();
        assert_eq!(error, "deliberate");
        assert!(store.projects().unwrap().is_empty(), "row must be gone");
    }

    #[test]
    fn opening_the_same_folder_twice_reuses_the_project() {
        let (_dir, mut store) = store();
        let first = store.open_project("/tmp/project", "project").unwrap();
        let second = store.open_project("/tmp/project", "renamed").unwrap();
        assert_eq!(first.id, second.id, "cwd is the unique key");
        assert_eq!(second.name, "renamed");
    }

    #[test]
    fn an_unknown_project_is_a_clear_error() {
        let (_dir, store) = store();
        let error = store.project("missing").unwrap_err();
        assert!(error.contains("not registered"), "got: {error}");
    }
}
