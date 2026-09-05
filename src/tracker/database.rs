use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use super::{HistoryEntry, RestoreSpec, SnapshotDetail, SnapshotSummary, TrackedWindow};

const MAX_HISTORY_ENTRIES: i64 = 10_000;
const SCHEMA_VERSION: &str = "1";

pub struct TrackerDatabase {
    connection: Connection,
    path: PathBuf,
}

impl TrackerDatabase {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .map_err(|err| err.to_string())?;
        let connection = Connection::open(path).map_err(|err| err.to_string())?;
        connection
            .busy_timeout(Duration::from_secs(2))
            .map_err(|err| err.to_string())?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=NORMAL;
                 PRAGMA foreign_keys=ON;
                 CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS current_windows (
                    window_id TEXT PRIMARY KEY, payload_json TEXT NOT NULL,
                    restore_json TEXT NOT NULL, updated_at_ms INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS history (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, window_id TEXT NOT NULL,
                    payload_json TEXT NOT NULL, restore_json TEXT NOT NULL,
                    closed_at_ms INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS history_closed_idx ON history(closed_at_ms DESC);
                 CREATE TABLE IF NOT EXISTS snapshots (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, kind TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL, boot_id TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS snapshot_windows (
                    snapshot_id INTEGER NOT NULL REFERENCES snapshots(id) ON DELETE CASCADE,
                    ordinal INTEGER NOT NULL, payload_json TEXT NOT NULL,
                    restore_json TEXT NOT NULL, PRIMARY KEY(snapshot_id, ordinal)
                 );",
            )
            .map_err(|err| err.to_string())?;
        let schema_version = connection
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|err| err.to_string())?;
        if schema_version
            .as_deref()
            .is_some_and(|version| version != SCHEMA_VERSION)
        {
            return Err(format!(
                "unsupported applicationlauncher database schema {:?}; expected {}",
                schema_version, SCHEMA_VERSION
            ));
        }
        connection
            .execute(
                "INSERT INTO meta(key,value) VALUES('schema_version',?1) ON CONFLICT(key) DO NOTHING",
                [SCHEMA_VERSION],
            )
            .map_err(|err| err.to_string())?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|err| format!("could not secure tracker database: {err}"))?;
        Ok(Self {
            connection,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>, String> {
        self.connection
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|err| err.to_string())
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), String> {
        self.connection.execute(
            "INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        ).map(|_| ()).map_err(|err| err.to_string())
    }

    pub fn current_window_entries(&self) -> Result<Vec<(TrackedWindow, RestoreSpec)>, String> {
        let mut statement = self
            .connection
            .prepare("SELECT window_id,payload_json,restore_json FROM current_windows ORDER BY window_id")
            .map_err(|err| err.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|err| err.to_string())?;
        let rows = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())?;
        drop(statement);
        let mut entries = Vec::new();
        for (id, payload, restore) in rows {
            match (
                serde_json::from_str::<TrackedWindow>(&payload),
                serde_json::from_str::<RestoreSpec>(&restore),
            ) {
                (Ok(window), Ok(restore)) => entries.push((window, restore)),
                (window_result, restore_result) => {
                    eprintln!(
                        "Ignoring corrupt current window {id}; it remains available for recovery: {window_result:?}, {restore_result:?}"
                    );
                }
            }
        }
        Ok(entries)
    }

    pub fn replace_current_with_restore(
        &mut self,
        entries: &[(TrackedWindow, RestoreSpec)],
    ) -> Result<(), String> {
        let existing: HashMap<String, (String, String)> = {
            let mut statement = self
                .connection
                .prepare("SELECT window_id,payload_json,restore_json FROM current_windows")
                .map_err(|err| err.to_string())?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(|err| err.to_string())?
                .map(|row| row.map(|(id, payload, restore)| (id, (payload, restore))))
                .collect::<Result<HashMap<_, _>, _>>()
                .map_err(|err| err.to_string())?
        };
        let current_ids = entries
            .iter()
            .map(|(window, _)| window.id.as_str())
            .collect::<HashSet<_>>();
        let tx = self
            .connection
            .transaction()
            .map_err(|err| err.to_string())?;
        for stale_id in existing
            .keys()
            .filter(|id| !current_ids.contains(id.as_str()))
        {
            tx.execute("DELETE FROM current_windows WHERE window_id=?1", [stale_id])
                .map_err(|err| err.to_string())?;
        }
        for (window, restore) in entries {
            let payload = serde_json::to_string(window).unwrap();
            let restore = serde_json::to_string(restore).unwrap();
            match existing.get(&window.id) {
                Some((previous_payload, previous_restore)) => {
                    let previous_window =
                        serde_json::from_str::<TrackedWindow>(previous_payload).ok();
                    if previous_window.as_ref().is_some_and(|previous| {
                        persistence_equivalent(previous, window) && previous_restore == &restore
                    }) {
                        continue;
                    }
                    tx.execute(
                        "UPDATE current_windows SET payload_json=?2,restore_json=?3,updated_at_ms=?4 WHERE window_id=?1",
                        params![window.id, payload, restore, window.updated_at_ms],
                    )
                    .map_err(|err| err.to_string())?;
                }
                None => {
                    tx.execute(
                        "INSERT INTO current_windows(window_id,payload_json,restore_json,updated_at_ms) VALUES(?1,?2,?3,?4)",
                        params![window.id, payload, restore, window.updated_at_ms],
                    ).map_err(|err| err.to_string())?;
                }
            }
        }
        tx.commit().map_err(|err| err.to_string())
    }

    pub fn add_history_with_restore(
        &self,
        window: &TrackedWindow,
        restore: &RestoreSpec,
        closed_at_ms: i64,
    ) -> Result<(), String> {
        self.connection.execute(
            "INSERT INTO history(window_id,payload_json,restore_json,closed_at_ms) VALUES(?1,?2,?3,?4)",
            params![window.id, serde_json::to_string(window).unwrap(), serde_json::to_string(&restore).unwrap(), closed_at_ms],
        ).map_err(|err| err.to_string())?;
        self.connection
            .execute(
                "DELETE FROM history WHERE id NOT IN (SELECT id FROM history ORDER BY closed_at_ms DESC, id DESC LIMIT ?1)",
                [MAX_HISTORY_ENTRIES],
            )
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    pub fn history_entry(&self, id: i64) -> Result<Option<HistoryEntry>, String> {
        let row = self
            .connection
            .query_row(
                "SELECT payload_json,restore_json,closed_at_ms FROM history WHERE id=?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|err| err.to_string())?;
        let Some((payload, restore, closed_at_ms)) = row else {
            return Ok(None);
        };
        let parsed = (|| {
            let restore =
                serde_json::from_str::<RestoreSpec>(&restore).map_err(|err| err.to_string())?;
            let window = history_window_with_restore_title(
                serde_json::from_str(&payload).map_err(|err| err.to_string())?,
                &restore,
            );
            Ok::<HistoryEntry, String>(HistoryEntry {
                id,
                window,
                closed_at_ms,
                restore,
            })
        })();
        match parsed {
            Ok(entry) => Ok(Some(entry)),
            Err(err) => {
                eprintln!(
                    "Ignoring corrupt history entry {id}; it remains available for recovery: {err}"
                );
                Ok(None)
            }
        }
    }

    pub fn history(&self, limit: usize) -> Result<Vec<HistoryEntry>, String> {
        let mut statement = self.connection.prepare(
            "SELECT id,payload_json,restore_json,closed_at_ms FROM history ORDER BY closed_at_ms DESC, id DESC LIMIT ?1"
        ).map_err(|err| err.to_string())?;
        let rows = statement
            .query_map([limit as i64], |row| {
                let payload: String = row.get(1)?;
                let restore: String = row.get(2)?;
                Ok((row.get(0)?, payload, restore, row.get(3)?))
            })
            .map_err(|err| err.to_string())?;
        let rows = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())?;
        drop(statement);
        let mut history = Vec::new();
        for (id, payload, restore, closed_at_ms) in rows {
            let parsed = (|| {
                let restore = serde_json::from_str::<super::RestoreSpec>(&restore)
                    .map_err(|err| err.to_string())?;
                let window = history_window_with_restore_title(
                    serde_json::from_str(&payload).map_err(|err| err.to_string())?,
                    &restore,
                );
                Ok::<HistoryEntry, String>(HistoryEntry {
                    id,
                    window,
                    closed_at_ms,
                    restore,
                })
            })();
            match parsed {
                Ok(entry) => history.push(entry),
                Err(err) => {
                    eprintln!(
                        "Ignoring corrupt history entry {id}; it remains available for recovery: {err}"
                    );
                }
            }
        }
        Ok(history)
    }

    pub fn clear_history(&self) -> Result<(), String> {
        self.connection
            .execute("DELETE FROM history", [])
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    pub fn remove_history(&self, id: i64) -> Result<(), String> {
        self.connection
            .execute("DELETE FROM history WHERE id=?1", [id])
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    pub fn prune_shell_surface_history(&self) -> Result<usize, String> {
        self.connection
            .execute(
                "DELETE FROM history
                 WHERE lower(json_extract(payload_json, '$.class')) IN
                       ('plasmashell', 'org.kde.plasmashell', 'kwin_wayland', 'applicationlauncher')",
                [],
            )
            .map_err(|err| err.to_string())
    }

    pub fn snapshot_kind_exists(&self, kind: &str) -> Result<bool, String> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM snapshots WHERE kind=?1)",
                [kind],
                |row| row.get(0),
            )
            .map_err(|err| err.to_string())
    }

    pub fn create_snapshot(
        &mut self,
        name: Option<&str>,
        kind: &str,
        boot_id: &str,
        windows: &[TrackedWindow],
        created_at_ms: i64,
    ) -> Result<i64, String> {
        let cached_restore = {
            let mut statement = self
                .connection
                .prepare("SELECT window_id,restore_json FROM current_windows")
                .map_err(|err| err.to_string())?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|err| err.to_string())?
                .collect::<Result<HashMap<_, _>, _>>()
                .map_err(|err| err.to_string())?
        };
        let entries = windows
            .iter()
            .cloned()
            .map(|window| {
                let restore = cached_restore
                    .get(&window.id)
                    .and_then(|restore| serde_json::from_str::<super::RestoreSpec>(restore).ok())
                    .unwrap_or_else(|| super::infer_restore_spec(&window));
                (window, restore)
            })
            .collect::<Vec<_>>();
        self.create_snapshot_with_entries(name, kind, boot_id, &entries, created_at_ms)
    }

    pub fn create_snapshot_with_entries(
        &mut self,
        name: Option<&str>,
        kind: &str,
        boot_id: &str,
        entries: &[(TrackedWindow, RestoreSpec)],
        created_at_ms: i64,
    ) -> Result<i64, String> {
        let tx = self
            .connection
            .transaction()
            .map_err(|err| err.to_string())?;
        if kind == "recovery" {
            tx.execute("DELETE FROM snapshots WHERE kind='recovery'", [])
                .map_err(|err| err.to_string())?;
        }
        tx.execute(
            "INSERT INTO snapshots(name,kind,created_at_ms,boot_id) VALUES(?1,?2,?3,?4)",
            params![name, kind, created_at_ms, boot_id],
        )
        .map_err(|err| err.to_string())?;
        let id = tx.last_insert_rowid();
        for (ordinal, (window, restore)) in entries.iter().enumerate() {
            tx.execute("INSERT INTO snapshot_windows(snapshot_id,ordinal,payload_json,restore_json) VALUES(?1,?2,?3,?4)", params![id, ordinal as i64, serde_json::to_string(window).unwrap(), serde_json::to_string(&restore).unwrap()]).map_err(|err| err.to_string())?;
        }
        tx.commit().map_err(|err| err.to_string())?;
        Ok(id)
    }

    pub fn snapshots(&self) -> Result<Vec<SnapshotSummary>, String> {
        let mut statement = self.connection.prepare("SELECT s.id,s.name,s.kind,s.created_at_ms,COUNT(w.ordinal) FROM snapshots s LEFT JOIN snapshot_windows w ON w.snapshot_id=s.id GROUP BY s.id ORDER BY s.created_at_ms DESC").map_err(|err| err.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok(SnapshotSummary {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    created_at_ms: row.get(3)?,
                    window_count: row.get::<_, i64>(4)? as usize,
                })
            })
            .map_err(|err| err.to_string())?;
        rows.map(|row| row.map_err(|err| err.to_string())).collect()
    }

    pub fn snapshot(&self, id: i64) -> Result<Option<SnapshotDetail>, String> {
        let summary = self.snapshots()?.into_iter().find(|item| item.id == id);
        let Some(summary) = summary else {
            return Ok(None);
        };
        let mut statement = self.connection.prepare("SELECT ordinal,payload_json,restore_json FROM snapshot_windows WHERE snapshot_id=?1 ORDER BY ordinal").map_err(|err| err.to_string())?;
        let rows = statement
            .query_map([id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|err| err.to_string())?;
        let rows = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())?;
        drop(statement);
        let mut windows = Vec::new();
        for (ordinal, payload, restore) in rows {
            match (
                serde_json::from_str::<TrackedWindow>(&payload),
                serde_json::from_str::<RestoreSpec>(&restore),
            ) {
                (Ok(window), Ok(restore)) => windows.push((window, restore)),
                (window_result, restore_result) => {
                    eprintln!(
                        "Ignoring corrupt snapshot row {id}/{ordinal}; it remains available for recovery: {window_result:?}, {restore_result:?}"
                    );
                }
            }
        }
        Ok(Some(SnapshotDetail { summary, windows }))
    }

    pub fn delete_snapshot(&self, id: i64) -> Result<(), String> {
        self.connection
            .execute("DELETE FROM snapshots WHERE id=?1", [id])
            .map(|_| ())
            .map_err(|err| err.to_string())
    }
}

fn history_window_with_restore_title(
    mut window: TrackedWindow,
    restore: &super::RestoreSpec,
) -> TrackedWindow {
    let Some(kind) = restore.terminal_kind.as_deref() else {
        return window;
    };
    if kind == "shell" || window.title.to_lowercase().contains(kind) {
        return window;
    }

    let title = window.title.trim();
    window.title = if title.is_empty() {
        kind.to_string()
    } else {
        format!("{kind} - {title}")
    };
    window
}

fn stable_title(title: &str) -> String {
    title
        .chars()
        .filter(|character| !matches!(*character as u32, 0x2800..=0x28ff))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn persistence_equivalent(previous: &TrackedWindow, current: &TrackedWindow) -> bool {
    stable_title(&previous.title) == stable_title(&current.title)
        && previous.class == current.class
        && previous.pid == current.pid
        && previous.desktop_file_name == current.desktop_file_name
        && previous.x == current.x
        && previous.y == current.y
        && previous.width == current.width
        && previous.height == current.height
        && previous.normal_geometry == current.normal_geometry
        && previous.minimized == current.minimized
        && previous.maximized == current.maximized
        && previous.maximized_horizontally == current.maximized_horizontally
        && previous.maximized_vertically == current.maximized_vertically
        && previous.fullscreen == current.fullscreen
        && previous.demands_attention == current.demands_attention
        && previous.active == current.active
        && previous.desktop == current.desktop
        && previous.on_all_desktops == current.on_all_desktops
        && previous.output == current.output
        && previous.output_geometry == current.output_geometry
        && previous.activities == current.activities
        && previous.stacking_order == current.stacking_order
        && previous.keep_above == current.keep_above
        && previous.keep_below == current.keep_below
        && previous.shaded == current.shaded
        && previous.skip_pager == current.skip_pager
        && previous.no_border == current.no_border
        && previous.opened_at_ms == current.opened_at_ms
        && previous.last_activated_at_ms == current.last_activated_at_ms
        && previous.activation_sequence == current.activation_sequence
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::{TrackedWindow, now_ms};

    #[test]
    fn stores_history_and_snapshots() {
        let path = std::env::temp_dir().join(format!(
            "applicationlauncher-tracker-{}.sqlite3",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut db = TrackerDatabase::open(&path).unwrap();
        let window = TrackedWindow {
            id: "one".into(),
            title: "fish - ~ - Terminal".into(),
            class: "xfce4-terminal".into(),
            updated_at_ms: now_ms(),
            ..Default::default()
        };
        let restore = crate::tracker::infer_restore_spec(&window);
        db.replace_current_with_restore(&[(window.clone(), restore.clone())])
            .unwrap();
        assert_eq!(
            db.current_window_entries().unwrap(),
            vec![(window.clone(), restore.clone())]
        );
        db.add_history_with_restore(&window, &restore, now_ms())
            .unwrap();
        assert_eq!(db.history(10).unwrap().len(), 1);

        let plasma = TrackedWindow {
            id: "plasma-popup".into(),
            title: "plasmashell".into(),
            class: "org.kde.plasmashell".into(),
            updated_at_ms: now_ms(),
            ..Default::default()
        };
        let plasma_restore = crate::tracker::infer_restore_spec(&plasma);
        db.add_history_with_restore(&plasma, &plasma_restore, now_ms())
            .unwrap();
        assert_eq!(db.prune_shell_surface_history().unwrap(), 1);
        assert_eq!(db.history(10).unwrap().len(), 1);
        let id = db
            .create_snapshot(Some("test"), "named", "boot", &[window.clone()], now_ms())
            .unwrap();
        assert_eq!(db.snapshot(id).unwrap().unwrap().summary.window_count, 1);
        let recovery_window = TrackedWindow {
            id: "recovery-window".into(),
            title: "htop - Terminal".into(),
            class: "xfce4-terminal".into(),
            updated_at_ms: now_ms(),
            ..Default::default()
        };
        let recovery_restore = RestoreSpec {
            terminal_kind: Some("htop".into()),
            ..Default::default()
        };
        db.create_snapshot_with_entries(
            None,
            "recovery",
            "boot-a",
            &[(recovery_window.clone(), recovery_restore.clone())],
            now_ms(),
        )
        .unwrap();
        db.create_snapshot_with_entries(
            None,
            "recovery",
            "boot-b",
            &[(window.clone(), restore.clone())],
            now_ms(),
        )
        .unwrap();
        let recovery_snapshots = db
            .snapshots()
            .unwrap()
            .into_iter()
            .filter(|snapshot| snapshot.kind == "recovery")
            .collect::<Vec<_>>();
        assert_eq!(recovery_snapshots.len(), 1);
        assert!(db.snapshot_kind_exists("recovery").unwrap());
        assert_eq!(
            db.snapshot(recovery_snapshots[0].id)
                .unwrap()
                .unwrap()
                .windows[0]
                .1,
            restore
        );
        let history_id = db.history(10).unwrap().first().unwrap().id;
        db.remove_history(history_id).unwrap();
        assert!(db.history(10).unwrap().is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn history_title_uses_the_saved_terminal_program() {
        let window = TrackedWindow {
            title: "Terminal".into(),
            ..Default::default()
        };
        let restore = super::super::RestoreSpec {
            terminal_kind: Some("htop".into()),
            ..Default::default()
        };

        assert_eq!(
            history_window_with_restore_title(window, &restore).title,
            "htop - Terminal"
        );
    }
}
