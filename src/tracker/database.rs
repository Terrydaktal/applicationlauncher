use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use super::{HistoryEntry, RestoreSpec, SnapshotDetail, SnapshotSummary, TrackedWindow};

const MAX_HISTORY_ENTRIES: i64 = 10_000;
const MAX_RECENT_SNAPSHOTS: i64 = 120;
const MAX_HOURLY_SNAPSHOTS: i64 = 120;
const HOURLY_PROMOTION_STRIDE: u8 = 12;
const HOURLY_PROMOTION_PHASE_META: &str = "hourly_promotion_phase";
const LAST_PERIODIC_SNAPSHOT_META: &str = "last_periodic_snapshot_at_ms";
pub(super) const PERIODIC_SNAPSHOT_INTERVAL_MS: i64 = 300_000;
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
                 CREATE INDEX IF NOT EXISTS snapshots_kind_id_idx ON snapshots(kind, id DESC);
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

    pub fn last_periodic_snapshot_at_ms(&self) -> Result<Option<i64>, String> {
        last_periodic_snapshot_at_ms(&self.connection)
    }

    pub fn create_periodic_snapshot_if_due(
        &mut self,
        boot_id: &str,
        entries: &[(TrackedWindow, RestoreSpec)],
        created_at_ms: i64,
    ) -> Result<Option<i64>, String> {
        if entries.is_empty() {
            return Ok(None);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|err| err.to_string())?;
        if !periodic_snapshot_delay(last_periodic_snapshot_at_ms(&tx)?, created_at_ms).is_zero() {
            return Ok(None);
        }
        let id = insert_snapshot(&tx, None, "recent", boot_id, entries, created_at_ms)?;
        // Promote one in twelve outgoing recent snapshots, oldest first. Updating
        // the kind preserves the original payload, ID and capture time, without
        // overlapping tiers. The phase survives restarts and transaction failures.
        let mut phase = hourly_promotion_phase(&tx)?;
        let outgoing = {
            let mut statement = tx
                .prepare(
                    "SELECT id FROM (
                         SELECT id FROM snapshots WHERE kind='recent'
                         ORDER BY id DESC LIMIT -1 OFFSET ?1
                     ) ORDER BY id ASC",
                )
                .map_err(|err| err.to_string())?;
            statement
                .query_map([MAX_RECENT_SNAPSHOTS], |row| row.get::<_, i64>(0))
                .map_err(|err| err.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| err.to_string())?
        };
        for outgoing_id in outgoing {
            if phase == 0 {
                tx.execute(
                    "UPDATE snapshots SET kind='hourly' WHERE id=?1",
                    [outgoing_id],
                )
            } else {
                tx.execute("DELETE FROM snapshots WHERE id=?1", [outgoing_id])
            }
            .map_err(|err| err.to_string())?;
            phase = (phase + 1) % HOURLY_PROMOTION_STRIDE;
        }
        // Capture, both retention tiers and schedule commit together. ID order
        // keeps the newest captures even when the system clock moves backwards.
        tx.execute(
            "DELETE FROM snapshots WHERE id IN (
                 SELECT id FROM snapshots WHERE kind='hourly'
                 ORDER BY id DESC LIMIT -1 OFFSET ?1
             )",
            [MAX_HOURLY_SNAPSHOTS],
        )
        .map_err(|err| err.to_string())?;
        tx.execute(
            "INSERT INTO meta(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![HOURLY_PROMOTION_PHASE_META, phase.to_string()],
        )
        .map_err(|err| err.to_string())?;
        tx.execute(
            "INSERT INTO meta(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![LAST_PERIODIC_SNAPSHOT_META, created_at_ms.to_string()],
        )
        .map_err(|err| err.to_string())?;
        tx.commit().map_err(|err| err.to_string())?;
        Ok(Some(id))
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
                let stored = cached_restore
                    .get(&window.id)
                    .and_then(|restore| serde_json::from_str::<super::RestoreSpec>(restore).ok())
                    .unwrap_or_else(|| super::infer_restore_spec(&window));
                let restore = super::refresh_live_restore_spec(&window, stored);
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
        let id = insert_snapshot(&tx, name, kind, boot_id, entries, created_at_ms)?;
        tx.commit().map_err(|err| err.to_string())?;
        Ok(id)
    }

    pub fn snapshots(&self) -> Result<Vec<SnapshotSummary>, String> {
        let mut statement = self.connection.prepare("SELECT s.id,s.name,s.kind,s.created_at_ms,COUNT(w.ordinal) FROM snapshots s LEFT JOIN snapshot_windows w ON w.snapshot_id=s.id GROUP BY s.id ORDER BY s.created_at_ms DESC,s.id DESC").map_err(|err| err.to_string())?;
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

pub(super) fn periodic_snapshot_delay(last_saved_ms: Option<i64>, now_ms: i64) -> Duration {
    let Some(last_saved_ms) = last_saved_ms.filter(|last| *last <= now_ms) else {
        // Start a new schedule after a clock correction, not an indefinitely
        // deferred archive. Subsequent saves still wait a full interval.
        return Duration::ZERO;
    };
    Duration::from_millis(
        PERIODIC_SNAPSHOT_INTERVAL_MS
            .saturating_sub(now_ms.saturating_sub(last_saved_ms))
            .max(0) as u64,
    )
}

fn last_periodic_snapshot_at_ms(connection: &Connection) -> Result<Option<i64>, String> {
    let stored: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key=?1",
            [LAST_PERIODIC_SNAPSHOT_META],
            |row| row.get(0),
        )
        .optional()
        .map_err(|err| err.to_string())?;
    match stored {
        Some(value) => value
            .parse::<i64>()
            .map(Some)
            .map_err(|err| err.to_string()),
        None => connection
            .query_row(
                "SELECT created_at_ms FROM snapshots WHERE kind IN ('recent','hourly') ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|err| err.to_string()),
    }
}

fn hourly_promotion_phase(connection: &Connection) -> Result<u8, String> {
    let stored: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key=?1",
            [HOURLY_PROMOTION_PHASE_META],
            |row| row.get(0),
        )
        .optional()
        .map_err(|err| err.to_string())?;
    let phase = stored
        .map(|value| value.parse::<u8>())
        .transpose()
        .map_err(|err| err.to_string())?
        .unwrap_or(0);
    if phase >= HOURLY_PROMOTION_STRIDE {
        return Err(format!("Invalid hourly snapshot promotion phase: {phase}"));
    }
    Ok(phase)
}

fn insert_snapshot(
    tx: &Transaction<'_>,
    name: Option<&str>,
    kind: &str,
    boot_id: &str,
    entries: &[(TrackedWindow, RestoreSpec)],
    created_at_ms: i64,
) -> Result<i64, String> {
    tx.execute(
        "INSERT INTO snapshots(name,kind,created_at_ms,boot_id) VALUES(?1,?2,?3,?4)",
        params![name, kind, created_at_ms, boot_id],
    )
    .map_err(|err| err.to_string())?;
    let id = tx.last_insert_rowid();
    let mut insert = tx
        .prepare("INSERT INTO snapshot_windows(snapshot_id,ordinal,payload_json,restore_json) VALUES(?1,?2,?3,?4)")
        .map_err(|err| err.to_string())?;
    for (ordinal, (window, restore)) in entries.iter().enumerate() {
        insert
            .execute(params![
                id,
                ordinal as i64,
                serde_json::to_string(window).map_err(|err| err.to_string())?,
                serde_json::to_string(restore).map_err(|err| err.to_string())?,
            ])
            .map_err(|err| err.to_string())?;
    }
    Ok(id)
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
pub(super) struct TestDatabaseDirectory(PathBuf);

#[cfg(test)]
impl Drop for TestDatabaseDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
pub(super) fn test_database() -> (TrackerDatabase, TestDatabaseDirectory) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = TestDatabaseDirectory(std::env::temp_dir().join(format!(
        "applicationlauncher-periodic-{}-{nonce}",
        std::process::id()
    )));
    let database = TrackerDatabase::open(&directory.0.join("history.sqlite3")).unwrap();
    (database, directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::{TrackedWindow, now_ms};

    fn periodic_entries() -> Vec<(TrackedWindow, RestoreSpec)> {
        vec![(
            TrackedWindow {
                id: "window".into(),
                title: "Editor".into(),
                class: "org.example.Editor".into(),
                x: 120,
                y: 340,
                width: 800,
                height: 600,
                stacking_order: 7,
                ..Default::default()
            },
            RestoreSpec {
                executable: Some("/usr/bin/editor".into()),
                cwd: Some("/project".into()),
                safe_arguments: vec!["/project/document.txt".into()],
                ..Default::default()
            },
        )]
    }

    #[test]
    fn periodic_schedule_survives_restart_and_does_not_backfill() {
        let (mut db, _directory) = test_database();
        let entries = periodic_entries();
        let start = 10 * PERIODIC_SNAPSHOT_INTERVAL_MS;
        let first = db
            .create_periodic_snapshot_if_due("boot-a", &entries, start)
            .unwrap()
            .unwrap();
        assert_eq!(db.snapshot(first).unwrap().unwrap().windows, entries);
        let path = db.path().to_owned();
        drop(db);
        let mut db = TrackerDatabase::open(&path).unwrap();
        assert_eq!(db.last_periodic_snapshot_at_ms().unwrap(), Some(start));
        assert_eq!(
            db.create_periodic_snapshot_if_due(
                "boot-b",
                &entries,
                start + PERIODIC_SNAPSHOT_INTERVAL_MS - 1
            )
            .unwrap(),
            None
        );
        // An unchanged session still gets an archive after five minutes.
        assert!(
            db.create_periodic_snapshot_if_due(
                "boot-b",
                &entries,
                start + PERIODIC_SNAPSHOT_INTERVAL_MS
            )
            .unwrap()
            .is_some()
        );
        assert!(
            db.create_periodic_snapshot_if_due(
                "boot-c",
                &entries,
                start + 50 * PERIODIC_SNAPSHOT_INTERVAL_MS
            )
            .unwrap()
            .is_some()
        );
        assert_eq!(db.snapshots().unwrap().len(), 3);
    }

    #[test]
    fn concurrent_periodic_writers_create_only_one_archive() {
        let (db, _directory) = test_database();
        let writers = [
            TrackerDatabase::open(db.path()).unwrap(),
            TrackerDatabase::open(db.path()).unwrap(),
        ];
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles = writers.map(|mut writer| {
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                writer
                    .create_periodic_snapshot_if_due("boot", &periodic_entries(), 10_000)
                    .unwrap()
            })
        });
        let results = handles.map(|handle| handle.join().unwrap());
        assert_eq!(results.iter().filter(|id| id.is_some()).count(), 1);
        assert_eq!(db.snapshots().unwrap().len(), 1);
    }

    #[test]
    fn tiered_retention_preserves_named_recovery_and_closed_history() {
        let (mut db, _directory) = test_database();
        let entries = periodic_entries();
        let named = db
            .create_snapshot_with_entries(Some("Keep me"), "named", "boot", &entries, 1)
            .unwrap();
        let recovery = db
            .create_snapshot_with_entries(None, "recovery", "boot", &entries, 2)
            .unwrap();
        db.add_history_with_restore(&entries[0].0, &entries[0].1, 3)
            .unwrap();
        let mut ids = Vec::new();
        let captures = MAX_RECENT_SNAPSHOTS + 125 * i64::from(HOURLY_PROMOTION_STRIDE);
        for capture in 1..=captures {
            ids.push(
                db.create_periodic_snapshot_if_due(
                    "boot",
                    &entries,
                    capture * PERIODIC_SNAPSHOT_INTERVAL_MS,
                )
                .unwrap()
                .unwrap(),
            );
        }
        let snapshots = db.snapshots().unwrap();
        assert_eq!(snapshots.iter().filter(|s| s.kind == "recent").count(), 120);
        assert_eq!(snapshots.iter().filter(|s| s.kind == "hourly").count(), 120);
        assert_eq!(snapshots.len(), 242);
        assert!(
            ids[..60]
                .iter()
                .all(|id| db.snapshot(*id).unwrap().is_none())
        );
        let oldest_hourly = db.snapshot(ids[60]).unwrap().unwrap();
        assert_eq!(oldest_hourly.summary.kind, "hourly");
        assert_eq!(
            oldest_hourly.summary.created_at_ms,
            61 * PERIODIC_SNAPSHOT_INTERVAL_MS
        );
        assert_eq!(oldest_hourly.windows, entries);
        let mut hourly_times = snapshots
            .iter()
            .filter(|s| s.kind == "hourly")
            .map(|s| s.created_at_ms)
            .collect::<Vec<_>>();
        hourly_times.sort_unstable();
        assert!(
            hourly_times
                .windows(2)
                .all(|times| times[1] - times[0] == 3_600_000)
        );
        let recent_times = snapshots
            .iter()
            .filter(|s| s.kind == "recent")
            .map(|s| s.created_at_ms)
            .collect::<Vec<_>>();
        assert!(
            recent_times
                .iter()
                .all(|time| *time > *hourly_times.last().unwrap())
        );
        assert_eq!(
            db.snapshot(*ids.last().unwrap()).unwrap().unwrap().windows,
            entries
        );
        assert_eq!(db.snapshot(named).unwrap().unwrap().windows, entries);
        assert_eq!(db.snapshot(recovery).unwrap().unwrap().windows, entries);
        assert_eq!(db.history(10).unwrap().len(), 1);
        let orphaned: i64 = db.connection.query_row(
            "SELECT COUNT(*) FROM snapshot_windows WHERE snapshot_id NOT IN (SELECT id FROM snapshots)",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(orphaned, 0);
        // A corrected clock must not cause the freshly written archive to prune itself.
        let corrected = db
            .create_periodic_snapshot_if_due("boot", &entries, 1)
            .unwrap()
            .unwrap();
        assert!(db.snapshot(corrected).unwrap().is_some());
        assert!(db.snapshot(ids[60]).unwrap().is_none());
        assert_eq!(
            db.snapshots()
                .unwrap()
                .iter()
                .filter(|s| s.kind == "hourly")
                .count(),
            120
        );
        assert_eq!(db.snapshots().unwrap().len(), 242);
    }

    #[test]
    fn recent_snapshots_age_into_hourly_history_without_overlap_across_restarts() {
        let (mut db, _directory) = test_database();
        let entries = periodic_entries();
        let mut ids = Vec::new();
        for capture in 1..=MAX_RECENT_SNAPSHOTS {
            ids.push(
                db.create_periodic_snapshot_if_due(
                    "original-boot",
                    &entries,
                    capture * PERIODIC_SNAPSHOT_INTERVAL_MS,
                )
                .unwrap()
                .unwrap(),
            );
        }
        assert_eq!(db.snapshots().unwrap().len(), 120);
        assert!(db.snapshots().unwrap().iter().all(|s| s.kind == "recent"));
        assert_eq!(hourly_promotion_phase(&db.connection).unwrap(), 0);

        let mut changed = entries.clone();
        changed[0].0.x += 500;
        changed[0].1.cwd = Some("/changed".into());
        changed[0].1.safe_arguments = vec!["/changed/document.txt".into()];
        for capture in 121..=126 {
            db.create_periodic_snapshot_if_due(
                "new-boot",
                &changed,
                capture * PERIODIC_SNAPSHOT_INTERVAL_MS,
            )
            .unwrap();
        }
        let promoted = db.snapshot(ids[0]).unwrap().unwrap();
        assert_eq!(promoted.summary.kind, "hourly");
        assert_eq!(
            promoted.summary.created_at_ms,
            PERIODIC_SNAPSHOT_INTERVAL_MS
        );
        assert_eq!(promoted.windows, entries);
        let boot: String = db
            .connection
            .query_row(
                "SELECT boot_id FROM snapshots WHERE id=?1",
                [ids[0]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(boot, "original-boot");
        assert_eq!(hourly_promotion_phase(&db.connection).unwrap(), 6);
        assert!(
            ids[1..6]
                .iter()
                .all(|id| db.snapshot(*id).unwrap().is_none())
        );

        let path = db.path().to_owned();
        drop(db);
        let mut db = TrackerDatabase::open(&path).unwrap();
        assert_eq!(hourly_promotion_phase(&db.connection).unwrap(), 6);
        for capture in 127..=133 {
            db.create_periodic_snapshot_if_due(
                "new-boot",
                &changed,
                capture * PERIODIC_SNAPSHOT_INTERVAL_MS,
            )
            .unwrap();
        }
        let snapshots = db.snapshots().unwrap();
        assert_eq!(snapshots.iter().filter(|s| s.kind == "recent").count(), 120);
        let hourly_ids = snapshots
            .iter()
            .filter(|s| s.kind == "hourly")
            .map(|s| s.id)
            .collect::<Vec<_>>();
        assert_eq!(hourly_ids, vec![ids[12], ids[0]]);
        assert_eq!(hourly_promotion_phase(&db.connection).unwrap(), 1);
        assert!(
            ids[1..12]
                .iter()
                .all(|id| db.snapshot(*id).unwrap().is_none())
        );
        assert_eq!(db.snapshot(ids[12]).unwrap().unwrap().windows, entries);
        assert_eq!(
            db.snapshot(snapshots[0].id).unwrap().unwrap().windows,
            changed
        );
    }

    #[test]
    fn failed_periodic_write_rolls_back_archive_retention_and_schedule() {
        let (mut db, _directory) = test_database();
        let entries = periodic_entries();
        let captures =
            MAX_RECENT_SNAPSHOTS + MAX_HOURLY_SNAPSHOTS * i64::from(HOURLY_PROMOTION_STRIDE);
        for capture in 1..=captures {
            db.create_periodic_snapshot_if_due(
                "boot",
                &entries,
                capture * PERIODIC_SNAPSHOT_INTERVAL_MS,
            )
            .unwrap();
        }
        assert_eq!(hourly_promotion_phase(&db.connection).unwrap(), 0);
        let before = db.snapshots().unwrap();
        let last_saved = db.last_periodic_snapshot_at_ms().unwrap();
        db.connection
            .execute_batch(
                "CREATE TRIGGER fail_periodic_schedule BEFORE UPDATE ON meta
             WHEN NEW.key='last_periodic_snapshot_at_ms'
             BEGIN SELECT RAISE(ABORT, 'injected schedule write failure'); END;",
            )
            .unwrap();
        let next = (captures + 1) * PERIODIC_SNAPSHOT_INTERVAL_MS;
        assert!(
            db.create_periodic_snapshot_if_due("boot", &entries, next)
                .is_err()
        );
        assert_eq!(db.snapshots().unwrap(), before);
        assert_eq!(db.last_periodic_snapshot_at_ms().unwrap(), last_saved);
        assert_eq!(hourly_promotion_phase(&db.connection).unwrap(), 0);
        assert!(db.snapshot(before.last().unwrap().id).unwrap().is_some());
        let children: i64 = db
            .connection
            .query_row("SELECT COUNT(*) FROM snapshot_windows", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(children, 240);
        db.connection
            .execute_batch("DROP TRIGGER fail_periodic_schedule;")
            .unwrap();
        assert!(
            db.create_periodic_snapshot_if_due("boot", &entries, next)
                .unwrap()
                .is_some()
        );
        assert_eq!(db.snapshots().unwrap().len(), 240);
        assert_eq!(hourly_promotion_phase(&db.connection).unwrap(), 1);
        assert!(db.snapshot(before.last().unwrap().id).unwrap().is_none());
    }

    #[test]
    fn periodic_schedule_handles_empty_sessions_deletion_and_clock_corrections() {
        let (mut db, _directory) = test_database();
        let entries = periodic_entries();
        assert_eq!(
            db.create_periodic_snapshot_if_due("boot", &[], 1).unwrap(),
            None
        );
        assert_eq!(db.last_periodic_snapshot_at_ms().unwrap(), None);
        let first = db
            .create_periodic_snapshot_if_due("boot", &entries, 10_000)
            .unwrap()
            .unwrap();
        db.delete_snapshot(first).unwrap();
        assert_eq!(
            db.create_periodic_snapshot_if_due("boot", &entries, 10_001)
                .unwrap(),
            None
        );
        assert!(
            db.create_periodic_snapshot_if_due("boot", &entries, 5_000)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            db.create_periodic_snapshot_if_due("boot", &entries, 5_001)
                .unwrap(),
            None
        );
        assert_eq!(
            periodic_snapshot_delay(Some(5_000), 5_001),
            Duration::from_millis(PERIODIC_SNAPSHOT_INTERVAL_MS as u64 - 1)
        );
        assert_eq!(
            periodic_snapshot_delay(Some(5_000), i64::MAX),
            Duration::ZERO
        );
    }

    #[test]
    fn existing_hourly_history_is_preserved_when_recent_history_starts() {
        let (mut db, _directory) = test_database();
        let entries = periodic_entries();
        let hourly = db
            .create_snapshot_with_entries(None, "hourly", "old-boot", &entries, 10_000)
            .unwrap();
        assert_eq!(db.last_periodic_snapshot_at_ms().unwrap(), Some(10_000));
        assert_eq!(
            db.create_periodic_snapshot_if_due("boot", &entries, 10_001)
                .unwrap(),
            None
        );
        let recent = db
            .create_periodic_snapshot_if_due("boot", &entries, 310_000)
            .unwrap()
            .unwrap();
        assert_eq!(db.snapshot(hourly).unwrap().unwrap().windows, entries);
        assert_eq!(db.snapshot(hourly).unwrap().unwrap().summary.kind, "hourly");
        assert_eq!(db.snapshot(recent).unwrap().unwrap().summary.kind, "recent");
        assert_eq!(hourly_promotion_phase(&db.connection).unwrap(), 0);
    }

    #[test]
    fn codex_bypass_options_survive_current_history_and_snapshot_storage() {
        let (mut db, _directory) = test_database();
        let window = TrackedWindow {
            id: "codex-window".into(),
            title: "codex - /project - Terminal".into(),
            class: "xfce4-terminal".into(),
            ..Default::default()
        };
        let restore = RestoreSpec {
            terminal_kind: Some("codex".into()),
            safe_arguments: super::super::codex_restore_arguments(&[
                "codex".into(),
                "--yolo".into(),
            ]),
            ..Default::default()
        };
        let entries = vec![(window.clone(), restore.clone())];
        db.replace_current_with_restore(&entries).unwrap();
        db.add_history_with_restore(&window, &restore, 1).unwrap();
        let snapshot = db
            .create_periodic_snapshot_if_due("boot", &entries, 2)
            .unwrap()
            .unwrap();
        let path = db.path().to_owned();
        drop(db);
        let db = TrackerDatabase::open(&path).unwrap();
        assert_eq!(db.current_window_entries().unwrap(), entries);
        assert_eq!(db.history(1).unwrap()[0].restore, restore);
        assert_eq!(db.snapshot(snapshot).unwrap().unwrap().windows, entries);
        assert_eq!(
            super::super::codex_resume_arguments(&db.history(1).unwrap()[0].restore.safe_arguments),
            [
                "codex",
                "resume",
                "--last",
                "--dangerously-bypass-approvals-and-sandbox",
            ]
        );
    }

    #[test]
    fn tmux_pane_restore_survives_current_history_snapshots_and_database_reopen() {
        let (mut db, _directory) = test_database();
        let window = TrackedWindow {
            id: "tmux-window".into(),
            class: "xfce4-terminal".into(),
            title: "tmux - qwen - Terminal".into(),
            ..Default::default()
        };
        let restore = RestoreSpec {
            terminal_kind: Some("tmux".into()),
            tmux_session: Some(super::super::TmuxSession {
                socket_path: "/tmp/tmux-test/default".into(),
                server_pid: 800,
                session_id: "$7".into(),
                session_name: "qwen".into(),
                created_at: 123,
            }),
            tmux_pane: Some(super::super::TmuxPaneRestore {
                cwd: "/project".into(),
                kind: "codex".into(),
                safe_arguments: vec!["--dangerously-bypass-approvals-and-sandbox".into()],
            }),
            ..Default::default()
        };
        let entries = vec![(window.clone(), restore.clone())];
        db.replace_current_with_restore(&entries).unwrap();
        db.add_history_with_restore(&window, &restore, 1).unwrap();
        let snapshot = db
            .create_periodic_snapshot_if_due("boot", &entries, 2)
            .unwrap()
            .unwrap();
        let path = db.path().to_owned();
        drop(db);
        let db = TrackerDatabase::open(&path).unwrap();
        assert_eq!(db.current_window_entries().unwrap(), entries);
        assert_eq!(db.history(1).unwrap()[0].restore, restore);
        assert_eq!(db.snapshot(snapshot).unwrap().unwrap().windows, entries);
    }

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
