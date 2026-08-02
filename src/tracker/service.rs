use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::interface;

use super::database::TrackerDatabase;
use super::{
    FEED_PATH, RestoreReport, SERVICE_NAME, TRACKER_PATH, TrackedWindow, TrackerStatus, now_ms,
};

struct State {
    windows: HashMap<String, TrackedWindow>,
    snapshot_buffer: Option<HashMap<String, TrackedWindow>>,
    generation: u64,
    history_generation: u64,
    activation_sequence: i64,
    recovery_pending: bool,
    run_id: String,
    boot_id: String,
    recovery_dirty: bool,
    recovery_due: Option<Instant>,
    current_dirty: bool,
    current_due: Option<Instant>,
    auto_enter_enabled: bool,
    attention: HashMap<String, AttentionState>,
}

struct AttentionState {
    due: Instant,
    attempts: u8,
}

struct RuntimeInner {
    state: Mutex<State>,
    database: Mutex<TrackerDatabase>,
}

#[derive(Clone)]
struct Runtime(Arc<RuntimeInner>);

impl Runtime {
    fn status(&self) -> TrackerStatus {
        let database_path = self.0.database.lock().unwrap().path().display().to_string();
        let state = self.0.state.lock().unwrap();
        TrackerStatus {
            generation: state.generation,
            history_generation: state.history_generation,
            window_count: state.windows.len(),
            recovery_pending: state.recovery_pending,
            database_path,
            run_id: state.run_id.clone(),
        }
    }

    fn windows(&self) -> Vec<TrackedWindow> {
        let state = self.0.state.lock().unwrap();
        let mut windows = state.windows.values().cloned().collect::<Vec<_>>();
        windows.sort_by_key(|window| window.activation_sequence);
        windows
    }

    fn restorable_windows(&self) -> Vec<TrackedWindow> {
        self.windows()
            .into_iter()
            .filter(is_history_worthy)
            .collect()
    }

    fn persist_current(&self) {
        let windows = self.windows();
        if let Err(err) = self.0.database.lock().unwrap().replace_current(&windows) {
            eprintln!("Tracker failed to persist current windows: {err}");
        }
    }

    fn mark_changed(state: &mut State) {
        state.generation = state.generation.wrapping_add(1);
        state.recovery_dirty = true;
        state
            .recovery_due
            .get_or_insert_with(|| Instant::now() + Duration::from_secs(30));
        state.current_dirty = true;
        state
            .current_due
            .get_or_insert_with(|| Instant::now() + Duration::from_millis(500));
    }

    fn upsert(&self, mut incoming: TrackedWindow, activated: bool) {
        let timestamp = now_ms();
        let mut state = self.0.state.lock().unwrap();
        let previous = state
            .snapshot_buffer
            .as_ref()
            .and_then(|buffer| buffer.get(&incoming.id))
            .or_else(|| state.windows.get(&incoming.id))
            .cloned();
        incoming.opened_at_ms = previous
            .as_ref()
            .map_or(timestamp, |window| window.opened_at_ms);
        incoming.updated_at_ms = timestamp;
        if activated || (incoming.active && previous.is_none()) {
            state.activation_sequence += 1;
            incoming.activation_sequence = state.activation_sequence;
            incoming.last_activated_at_ms = Some(timestamp);
        } else if let Some(previous) = &previous {
            incoming.activation_sequence = previous.activation_sequence;
            incoming.last_activated_at_ms = previous.last_activated_at_ms;
        }
        let attention_id = incoming.id.clone();
        let requires_attention = is_attention_terminal(&incoming);
        if let Some(target) = state.snapshot_buffer.as_mut() {
            target.insert(incoming.id.clone(), incoming);
        } else {
            state.windows.insert(incoming.id.clone(), incoming);
        }
        if state.auto_enter_enabled && requires_attention {
            state
                .attention
                .entry(attention_id)
                .or_insert(AttentionState {
                    due: Instant::now() + Duration::from_secs(5),
                    attempts: 0,
                });
        } else if !requires_attention {
            state.attention.remove(&attention_id);
        }
        if state.snapshot_buffer.is_none() {
            Self::mark_changed(&mut state);
        }
    }

    fn remove(&self, id: &str) {
        let timestamp = now_ms();
        let mut state = self.0.state.lock().unwrap();
        if let Some(buffer) = state.snapshot_buffer.as_mut() {
            buffer.remove(id);
            return;
        }
        let removed = state.windows.remove(id);
        state.attention.remove(id);
        if removed.is_some() {
            state.history_generation = state.history_generation.wrapping_add(1);
            Self::mark_changed(&mut state);
        }
        drop(state);
        if let Some(window) = removed {
            if is_history_worthy(&window)
                && let Err(err) = self
                    .0
                    .database
                    .lock()
                    .unwrap()
                    .add_history(&window, timestamp)
            {
                eprintln!("Tracker failed to append window history: {err}");
            }
        }
    }

    fn finish_snapshot(&self) {
        let timestamp = now_ms();
        let mut closed = Vec::new();
        let mut state = self.0.state.lock().unwrap();
        let Some(buffer) = state.snapshot_buffer.take() else {
            return;
        };
        for (id, window) in &state.windows {
            if !buffer.contains_key(id) {
                closed.push(window.clone());
            }
        }
        state.windows = buffer;
        if !closed.is_empty() {
            state.history_generation = state.history_generation.wrapping_add(1);
        }
        Self::mark_changed(&mut state);
        drop(state);
        let database = self.0.database.lock().unwrap();
        for window in closed {
            if is_history_worthy(&window)
                && let Err(err) = database.add_history(&window, timestamp)
            {
                eprintln!("Tracker failed to append snapshot closure: {err}");
            }
        }
        drop(database);
        self.persist_current_if_due(true);
    }

    fn write_recovery_if_due(&self, force: bool) {
        let (windows, boot_id) = {
            let mut state = self.0.state.lock().unwrap();
            if state.recovery_pending {
                return;
            }
            if !state.recovery_dirty
                || (!force && state.recovery_due.is_none_or(|due| Instant::now() < due))
            {
                return;
            }
            state.recovery_dirty = false;
            state.recovery_due = None;
            (
                state
                    .windows
                    .values()
                    .filter(|window| is_history_worthy(window))
                    .cloned()
                    .collect::<Vec<_>>(),
                state.boot_id.clone(),
            )
        };
        if let Err(err) = self.0.database.lock().unwrap().create_snapshot(
            None,
            "recovery",
            &boot_id,
            &windows,
            now_ms(),
        ) {
            eprintln!("Tracker failed to write recovery snapshot: {err}");
        }
    }

    fn persist_current_if_due(&self, force: bool) {
        {
            let mut state = self.0.state.lock().unwrap();
            if !state.current_dirty
                || (!force && state.current_due.is_none_or(|due| Instant::now() < due))
            {
                return;
            }
            state.current_dirty = false;
            state.current_due = None;
        }
        self.persist_current();
    }

    fn process_attention(&self) {
        let targets = {
            let state = self.0.state.lock().unwrap();
            if !state.auto_enter_enabled {
                return;
            }
            let now = Instant::now();
            state
                .attention
                .iter()
                .filter_map(|(id, attention)| {
                    (attention.due <= now && attention.attempts < 3)
                        .then(|| state.windows.get(id).cloned())
                        .flatten()
                })
                .collect::<Vec<_>>()
        };
        for window in targets {
            let result = send_enter_to_terminal(&window);
            let mut state = self.0.state.lock().unwrap();
            if !state
                .windows
                .get(&window.id)
                .is_some_and(is_attention_terminal)
            {
                state.attention.remove(&window.id);
                continue;
            }
            if let Some(attention) = state.attention.get_mut(&window.id) {
                attention.attempts = attention.attempts.saturating_add(1);
                attention.due = Instant::now()
                    + if result.is_ok() {
                        Duration::from_secs(2)
                    } else {
                        Duration::from_millis(750_u64 << attention.attempts.min(3))
                    };
            }
            if let Err(err) = result {
                eprintln!(
                    "Automatic terminal Enter failed for {}: {err}",
                    window.title
                );
            }
        }
    }

    fn schedule_layout_reconciliation(&self, specs: Vec<(TrackedWindow, super::RestoreSpec)>) {
        let runtime = self.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let current = runtime.windows();
                let final_attempt = Instant::now() >= deadline;
                let matched = super::restore::matching_window_count(&specs, &current);
                if matched == specs.len() || final_attempt {
                    let (_, failures) =
                        super::restore::apply_matching_layouts(&specs, &current, true);
                    for failure in failures {
                        eprintln!("Session layout restore: {failure}");
                    }
                }
                if matched == specs.len() {
                    break;
                }
                if final_attempt {
                    eprintln!(
                        "Session layout restore timed out with {matched}/{} windows matched",
                        specs.len()
                    );
                    break;
                }
            }
        });
    }
}

fn is_attention_terminal(window: &TrackedWindow) -> bool {
    window
        .class
        .to_lowercase()
        .replace(['-', '_'], "")
        .contains("xfce4terminal")
        && (window.demands_attention || window.title.to_lowercase().contains("action required"))
}

fn is_history_worthy(window: &TrackedWindow) -> bool {
    let class = window.class.to_lowercase();
    !window.title.trim().is_empty()
        && !window.class.trim().is_empty()
        && !matches!(
            class.as_str(),
            "plasmashell" | "org.kde.plasmashell" | "kwin_wayland" | "applicationlauncher"
        )
}

fn normalized_terminal_title(title: &str) -> String {
    title
        .chars()
        .filter(|character| !matches!(*character as u32, 0x2800..=0x28ff))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn dbus_string(values: &HashMap<String, zbus::zvariant::OwnedValue>, key: &str) -> Option<String> {
    let value = zbus::zvariant::Value::try_from(values.get(key)?).ok()?;
    value.downcast_ref::<String>().ok()
}

fn dbus_bool(values: &HashMap<String, zbus::zvariant::OwnedValue>, key: &str) -> Option<bool> {
    let value = zbus::zvariant::Value::try_from(values.get(key)?).ok()?;
    value.downcast_ref::<bool>().ok()
}

fn send_enter_to_terminal(window: &TrackedWindow) -> Result<(), String> {
    let connection = zbus::blocking::Connection::session().map_err(|err| err.to_string())?;
    let proxy = zbus::blocking::Proxy::new(
        &connection,
        "org.xfce.Terminal5",
        "/org/xfce/Terminal",
        "org.xfce.Terminal5",
    )
    .map_err(|err| err.to_string())?;
    let records: Vec<HashMap<String, zbus::zvariant::OwnedValue>> = proxy
        .call("ListTerminals", &())
        .map_err(|err| err.to_string())?;
    let wanted = normalized_terminal_title(&window.title);
    let mut matches = records
        .iter()
        .filter(|record| {
            dbus_bool(record, "active").unwrap_or(false)
                && dbus_string(record, "window_title")
                    .is_some_and(|title| normalized_terminal_title(&title) == wanted)
        })
        .filter_map(|record| dbus_string(record, "tab_uuid"));
    let tab_uuid = matches
        .next()
        .ok_or_else(|| "No active terminal tab matched this window".to_string())?;
    if matches.next().is_some() {
        return Err("Multiple terminal tabs matched this window".into());
    }
    proxy
        .call::<_, _, ()>("SendEnter", &(tab_uuid.as_str(),))
        .map_err(|err| err.to_string())
}

#[derive(Clone)]
struct WindowFeed(Runtime);

#[interface(name = "com.terrydaktal.ApplicationLauncher.WindowFeed", spawn = false)]
impl WindowFeed {
    #[zbus(name = "BeginSnapshot")]
    fn begin_snapshot(&self) {
        self.0.0.state.lock().unwrap().snapshot_buffer = Some(HashMap::new());
    }

    #[zbus(name = "ResetWindows")]
    fn reset_windows(&self) {
        self.begin_snapshot();
    }

    #[zbus(name = "UpsertWindow")]
    fn upsert_window(&self, payload: &str) {
        match serde_json::from_str(payload) {
            Ok(window) => self.0.upsert(window, false),
            Err(err) => eprintln!("Tracker rejected KWin payload: {err}"),
        }
    }

    #[zbus(name = "ReplaceSnapshot")]
    fn replace_snapshot(&self, payload: &str) {
        let windows = match serde_json::from_str::<Vec<TrackedWindow>>(payload) {
            Ok(windows) => windows,
            Err(err) => {
                eprintln!("Tracker rejected KWin snapshot: {err}");
                return;
            }
        };
        self.0.0.state.lock().unwrap().snapshot_buffer = Some(HashMap::new());
        for window in windows {
            self.0.upsert(window, false);
        }
        self.0.finish_snapshot();
    }

    #[zbus(name = "WindowActivated")]
    fn window_activated(&self, payload: &str) {
        if let Ok(window) = serde_json::from_str(payload) {
            self.0.upsert(window, true);
        }
    }

    #[zbus(name = "RemoveWindow")]
    fn remove_window(&self, id: &str) {
        self.0.remove(id);
    }

    #[zbus(name = "EndSnapshot")]
    fn end_snapshot(&self) {
        self.0.finish_snapshot();
    }
}

#[derive(Clone)]
struct TrackerApi(Runtime);

#[interface(name = "com.terrydaktal.ApplicationLauncher.Tracker1", spawn = false)]
impl TrackerApi {
    #[zbus(name = "GetStatus")]
    fn get_status(&self) -> String {
        serde_json::to_string(&self.0.status()).unwrap()
    }

    #[zbus(name = "GetWindows")]
    fn get_windows(&self) -> String {
        serde_json::to_string(&self.0.windows()).unwrap()
    }

    #[zbus(name = "GetHistory")]
    fn get_history(&self, limit: u32) -> String {
        let result = self
            .0
            .0
            .database
            .lock()
            .unwrap()
            .history(limit.clamp(1, 10_000) as usize);
        serde_json::to_string(&result).unwrap()
    }

    #[zbus(name = "GetSnapshots")]
    fn get_snapshots(&self) -> String {
        serde_json::to_string(&self.0.0.database.lock().unwrap().snapshots()).unwrap()
    }

    #[zbus(name = "CreateSnapshot")]
    fn create_snapshot(&self, name: &str) -> String {
        let windows = self.0.restorable_windows();
        let boot_id = self.0.0.state.lock().unwrap().boot_id.clone();
        let result = self.0.0.database.lock().unwrap().create_snapshot(
            Some(name.trim()),
            "named",
            &boot_id,
            &windows,
            now_ms(),
        );
        serde_json::to_string(&result).unwrap()
    }

    #[zbus(name = "DeleteSnapshot")]
    fn delete_snapshot(&self, id: i64) -> String {
        serde_json::to_string(&self.0.0.database.lock().unwrap().delete_snapshot(id)).unwrap()
    }

    #[zbus(name = "GetSnapshot")]
    fn get_snapshot(&self, id: i64) -> String {
        serde_json::to_string(&self.0.0.database.lock().unwrap().snapshot(id)).unwrap()
    }

    #[zbus(name = "RestoreSnapshot")]
    fn restore_snapshot(&self, id: i64) -> String {
        let snapshot = self.0.0.database.lock().unwrap().snapshot(id);
        let result = snapshot.and_then(|snapshot| {
            snapshot
                .map(|snapshot| {
                    let report = super::restore_snapshot(&snapshot, &self.0.windows());
                    if report.launched > 0 {
                        self.0.schedule_layout_reconciliation(snapshot.windows);
                    }
                    report
                })
                .ok_or_else(|| format!("Snapshot {id} does not exist"))
        });
        serde_json::to_string(&result).unwrap()
    }

    #[zbus(name = "RestoreRecovery")]
    fn restore_recovery(&self) -> String {
        let snapshot = {
            let database = self.0.0.database.lock().unwrap();
            database.snapshots().and_then(|snapshots| {
                let id = snapshots
                    .into_iter()
                    .find(|snapshot| snapshot.kind == "recovery")
                    .map(|snapshot| snapshot.id)
                    .ok_or_else(|| "No recovery snapshot is available".to_string())?;
                database
                    .snapshot(id)?
                    .ok_or_else(|| "Recovery snapshot disappeared".to_string())
            })
        };
        let result = snapshot.map(|snapshot| {
            let report = super::restore_snapshot(&snapshot, &self.0.windows());
            if report.launched > 0 {
                self.0.schedule_layout_reconciliation(snapshot.windows);
            }
            report
        });
        if result.is_ok() {
            self.0.0.state.lock().unwrap().recovery_pending = false;
        }
        serde_json::to_string(&result).unwrap()
    }

    #[zbus(name = "ReopenHistory")]
    fn reopen_history(&self, id: i64) -> String {
        let result = reopen_history_entry(&self.0, id);
        serde_json::to_string(&result).unwrap()
    }

    #[zbus(name = "ReopenLatestHistory")]
    fn reopen_latest_history(&self) -> String {
        let latest_id = self
            .0
            .0
            .database
            .lock()
            .unwrap()
            .history(1)
            .and_then(|history| {
                history
                    .into_iter()
                    .next()
                    .map(|entry| entry.id)
                    .ok_or_else(|| "No recently closed windows are available".into())
            });
        let result = latest_id.and_then(|id| reopen_history_entry(&self.0, id));
        serde_json::to_string(&result).unwrap()
    }

    #[zbus(name = "DismissRecovery")]
    fn dismiss_recovery(&self) {
        self.0.0.state.lock().unwrap().recovery_pending = false;
        let _ = self
            .0
            .0
            .database
            .lock()
            .unwrap()
            .set_meta("recovery_dismissed", "true");
    }

    #[zbus(name = "ClearHistory")]
    fn clear_history(&self) -> String {
        let result = self.0.0.database.lock().unwrap().clear_history();
        if result.is_ok() {
            let mut state = self.0.0.state.lock().unwrap();
            state.history_generation = state.history_generation.wrapping_add(1);
        }
        serde_json::to_string(&result).unwrap()
    }

    #[zbus(name = "SetAutoEnter")]
    fn set_auto_enter(&self, enabled: bool) -> String {
        let mut state = self.0.0.state.lock().unwrap();
        state.auto_enter_enabled = enabled;
        if !enabled {
            state.attention.clear();
        }
        drop(state);
        serde_json::to_string(
            &self
                .0
                .0
                .database
                .lock()
                .unwrap()
                .set_meta("auto_enter", if enabled { "true" } else { "false" }),
        )
        .unwrap()
    }
}

fn reopen_history_entry(runtime: &Runtime, id: i64) -> Result<RestoreReport, String> {
    let entry = runtime
        .0
        .database
        .lock()
        .unwrap()
        .history(10_000)?
        .into_iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| format!("History entry {id} does not exist"))?;
    if !is_history_worthy(&entry.window) {
        return Err(format!(
            "{} is a desktop shell surface, not a reopenable application window",
            entry.window.title
        ));
    }

    let report = super::restore::reopen_entry(&entry);
    if report.launched > 0 {
        runtime.schedule_layout_reconciliation(vec![(entry.window, entry.restore)]);
        runtime.0.database.lock().unwrap().remove_history(id)?;
        let mut state = runtime.0.state.lock().unwrap();
        state.history_generation = state.history_generation.wrapping_add(1);
    }
    Ok(report)
}

fn read_boot_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn run_id() -> String {
    format!("{}-{}", std::process::id(), now_ms())
}

pub fn run_tracker_daemon() -> Result<(), String> {
    let database_path = super::state_dir().join("history.sqlite3");
    let database = TrackerDatabase::open(&database_path)?;
    database.prune_shell_surface_history()?;
    let boot_id = read_boot_id();
    let previous_boot = database.meta("boot_id")?.unwrap_or_default();
    let previous_clean = database.meta("clean_shutdown")?.as_deref() == Some("true");
    let recovery_pending = !previous_boot.is_empty() && previous_boot != boot_id && !previous_clean;
    database.set_meta("boot_id", &boot_id)?;
    database.set_meta("clean_shutdown", "false")?;
    database.set_meta("recovery_dismissed", "false")?;
    let run_id = run_id();
    database.set_meta("run_id", &run_id)?;
    let auto_enter_enabled = database.meta("auto_enter")?.as_deref() == Some("true");

    let runtime = Runtime(Arc::new(RuntimeInner {
        state: Mutex::new(State {
            windows: HashMap::new(),
            snapshot_buffer: None,
            generation: 0,
            history_generation: 0,
            activation_sequence: 0,
            recovery_pending,
            run_id,
            boot_id,
            recovery_dirty: false,
            recovery_due: None,
            current_dirty: false,
            current_due: None,
            auto_enter_enabled,
            attention: HashMap::new(),
        }),
        database: Mutex::new(database),
    }));

    let recovery_runtime = runtime.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(250));
            recovery_runtime.write_recovery_if_due(false);
            recovery_runtime.persist_current_if_due(false);
            recovery_runtime.process_attention();
        }
    });

    let shutdown_runtime = runtime.clone();
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
    ])
    .map_err(|err| err.to_string())?;
    std::thread::spawn(move || {
        if signals.forever().next().is_some() {
            shutdown_runtime.write_recovery_if_due(true);
            shutdown_runtime.persist_current_if_due(true);
            let _ = shutdown_runtime
                .0
                .database
                .lock()
                .unwrap()
                .set_meta("clean_shutdown", "true");
            std::process::exit(0);
        }
    });

    pollster::block_on(async move {
        let _connection = zbus::connection::Builder::session()
            .map_err(|err| err.to_string())?
            .name(SERVICE_NAME)
            .map_err(|err| err.to_string())?
            .serve_at(FEED_PATH, WindowFeed(runtime.clone()))
            .map_err(|err| err.to_string())?
            .serve_at(TRACKER_PATH, TrackerApi(runtime))
            .map_err(|err| err.to_string())?
            .build()
            .await
            .map_err(|err| err.to_string())?;
        if let Err(err) = super::install::ensure_kwin_feed_installed() {
            eprintln!("Tracker could not install the KWin feed: {err}");
        }
        super::install::start_kwin_feed_watchdog();
        std::future::pending::<()>().await;
        #[allow(unreachable_code)]
        Ok(())
    })
}
