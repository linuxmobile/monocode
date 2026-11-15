//! Read the desktop webview localStorage without writing WebKit files.
use super::DesktopPreferences;
use rusqlite::{types::ValueRef, Connection, OpenFlags};
use std::{collections::BTreeMap, path::Path};

impl DesktopPreferences {
    pub(super) fn load_installed(data_dir: &Path) -> Self {
        Self::load_webkitgtk(&data_dir.join("localstorage"))
    }

    /// The desktop app's localStorage lives in a WebKitGTK profile. Keep the
    /// packaged origin first and drop the development ones.
    pub(super) fn load_webkitgtk(directory: &Path) -> Self {
        let mut ranked = Vec::new();
        if let Ok(entries) = std::fs::read_dir(directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if let Some(rank) = webkitgtk_filename_rank(name) {
                    ranked.push((rank, path));
                }
            }
        }
        ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        ranked
            .into_iter()
            .find_map(|(_, path)| {
                Self::read_database(&path)
                    .ok()
                    .filter(|prefs| prefs.available)
            })
            .unwrap_or_default()
    }

    fn read_database(path: &Path) -> Result<Self, String> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
        conn.busy_timeout(std::time::Duration::from_millis(150))
            .map_err(|e| e.to_string())?;
        let mut query = conn
            .prepare("SELECT key,value FROM ItemTable WHERE key LIKE 'monocode%' ")
            .map_err(|e| e.to_string())?;
        let rows = query
            .query_map([], |row| {
                let key: String = row.get(0)?;
                let value = match row.get_ref(1)? {
                    ValueRef::Text(bytes) => std::str::from_utf8(bytes).ok().map(str::to_owned),
                    ValueRef::Blob(bytes) => decode_webkit_value(bytes),
                    _ => None,
                };
                Ok((key, value))
            })
            .map_err(|e| e.to_string())?;
        let mut values = BTreeMap::new();
        for row in rows {
            let (key, value) = row.map_err(|e| e.to_string())?;
            if let Some(value) = value {
                values.insert(key, value);
            }
        }
        Ok(Self {
            available: !values.is_empty(),
            values,
        })
    }
}

fn origin_rank(scheme: &str, host: &str) -> Option<u8> {
    match (scheme, host) {
        ("tauri", "localhost") => Some(0),
        ("https", "tauri.localhost") => Some(1),
        ("http", "tauri.localhost") => Some(2),
        _ => None,
    }
}

pub(super) fn webkitgtk_filename_rank(name: &str) -> Option<u8> {
    let stem = name.strip_suffix(".localstorage")?;
    let (rest, port) = stem.rsplit_once('_')?;
    let port: u16 = port.parse().ok()?;
    let (scheme, host) = rest.split_once('_')?;
    if port != 0 && !matches!((scheme, port), ("http", 80) | ("https", 443)) {
        return None;
    }
    origin_rank(scheme, host)
}

pub(super) fn decode_webkit_value(bytes: &[u8]) -> Option<String> {
    let (pairs, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        return None;
    }
    String::from_utf16(
        &pairs
            .iter()
            .copied()
            .map(u16::from_le_bytes)
            .collect::<Vec<_>>(),
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn utf16(value: &str) -> Vec<u8> {
        value.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    fn write_localstorage(path: &Path, key: &str, value: &str) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("CREATE TABLE ItemTable(key TEXT,value BLOB)")
            .unwrap();
        conn.execute(
            "INSERT INTO ItemTable VALUES(?1,?2)",
            rusqlite::params![key, utf16(value)],
        )
        .unwrap();
    }

    #[test]
    fn webkitgtk_filenames_keep_packaged_origins_and_drop_vite() {
        assert_eq!(
            webkitgtk_filename_rank("tauri_localhost_0.localstorage"),
            Some(0)
        );
        assert_eq!(
            webkitgtk_filename_rank("https_tauri.localhost_0.localstorage"),
            Some(1)
        );
        assert_eq!(
            webkitgtk_filename_rank("http_tauri.localhost_0.localstorage"),
            Some(2)
        );
        assert_eq!(
            webkitgtk_filename_rank("http_localhost_1420.localstorage"),
            None
        );
        assert_eq!(
            webkitgtk_filename_rank("http_localhost_0.localstorage"),
            None
        );
        assert_eq!(webkitgtk_filename_rank("localstorage.sqlite3"), None);
    }

    #[test]
    fn linux_localstorage_prefers_the_packaged_origin() {
        let root = tempfile::tempdir().unwrap();
        write_localstorage(
            &root.path().join("http_localhost_1420.localstorage"),
            "monocode.workspaceTheme",
            "ember",
        );
        write_localstorage(
            &root.path().join("tauri_localhost_0.localstorage"),
            "monocode.workspaceTheme",
            "tidepool",
        );
        let preferences = DesktopPreferences::load_webkitgtk(root.path());
        assert_eq!(preferences.get("monocode.workspaceTheme"), Some("tidepool"));
        assert!(preferences.available);
        let mut workspace = serde_json::json!({});
        preferences.apply_workspace(&mut workspace);
        assert_eq!(workspace["preferences"]["workspace_theme"], "tidepool");
    }

    #[test]
    fn webkitgtk_empty_origin_does_not_hide_saved_projects() {
        let root = tempfile::tempdir().unwrap();
        let empty = root.path().join("tauri_localhost_0.localstorage");
        write_localstorage(&empty, "unrelated", "value");
        let unavailable = DesktopPreferences::read_database(&empty).unwrap();
        assert!(!unavailable.is_available());
        let mut workspace = serde_json::json!({"preferences": {"workspace_theme": "ember"}});
        let original = workspace.clone();
        unavailable.apply_workspace(&mut workspace);
        assert_eq!(workspace, original);

        let projects: Vec<_> = (0..8)
            .map(|i| serde_json::json!({"path": format!("/code/project-{i}")}))
            .collect();
        write_localstorage(
            &root.path().join("https_tauri.localhost_0.localstorage"),
            "monocode.recentProjects",
            &serde_json::to_string(&projects).unwrap(),
        );
        let loaded = DesktopPreferences::load_webkitgtk(root.path());
        assert_eq!(
            loaded.json("monocode.recentProjects"),
            Some(serde_json::json!(projects))
        );
        loaded.apply_workspace(&mut workspace);
        assert_eq!(workspace["projects"].as_array().unwrap().len(), 8);
    }
}
