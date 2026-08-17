use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use zbus::interface;

use super::database::TrackerDatabase;
use super::{
    FEED_PATH, HistoryEntry, RestoreReport, RestoreSpec, SERVICE_NAME, TRACKER_PATH, TrackedWindow,
    TrackerStatus, is_compact_chromium_helper_surface, now_ms,
};

const KWIN_RECONCILIATION_INTERVAL: Duration = Duration::from_secs(15);
const KWIN_RECONCILIATION_TIMEOUT: Duration = Duration::from_secs(2);
const LAYOUT_RECONCILIATION_FAST_POLL: Duration = Duration::from_millis(40);
const LAYOUT_RECONCILIATION_SLOW_POLL: Duration = Duration::from_millis(250);
const LAYOUT_RECONCILIATION_FAST_PERIOD: Duration = Duration::from_secs(2);
const KWIN_SERVICE: &str = "org.kde.KWin";
const KWIN_PATH: &str = "/KWin";
const KWIN_INTERFACE: &str = "org.kde.KWin";
const KGLOBALACCEL_SERVICE: &str = "org.kde.kglobalaccel";
const KGLOBALACCEL_PATH: &str = "/kglobalaccel";
const KGLOBALACCEL_INTERFACE: &str = "org.kde.KGlobalAccel";
const TERMINAL_DBUS_TIMEOUT: Duration = Duration::from_secs(3);
const ATTENTION_RECHECK_DELAY: Duration = Duration::from_secs(5);
const ATTENTION_RETRY_BASE_MS: u64 = 750;
const ATTENTION_RETRY_MAX_EXPONENT: u8 = 6;
const SNAPSHOT_TRANSACTION_TIMEOUT: Duration = Duration::from_secs(5);
const REOPEN_HISTORY_SCAN_LIMIT: usize = 10_000;
const REOPEN_LAUNCH_ATTEMPT_LIMIT: usize = 4;
const HISTORY_RESTORE_RETRY_DELAY: Duration = Duration::from_secs(30);

fn reopen_shortcut_action_id() -> Vec<&'static str> {
    vec![
        "kwin",
        "applicationlauncher-reopen-latest",
        "KWin",
        "Reopen recently closed window",
    ]
}

fn set_reopen_shortcut_active(active: bool) -> Result<(), String> {
    let connection = zbus::blocking::Connection::session().map_err(|err| err.to_string())?;
    let proxy = zbus::blocking::Proxy::new(
        &connection,
        KGLOBALACCEL_SERVICE,
        KGLOBALACCEL_PATH,
        KGLOBALACCEL_INTERFACE,
    )
    .map_err(|err| err.to_string())?;
    let method = if active { "doRegister" } else { "setInactive" };
    proxy
        .call::<_, _, ()>(method, &(reopen_shortcut_action_id(),))
        .map_err(|err| err.to_string())
}

struct State {
    windows: HashMap<String, TrackedWindow>,
    snapshot_buffer: Option<HashMap<String, TrackedWindow>>,
    snapshot_deadline: Option<Instant>,
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
    recovery_write_in_flight: bool,
    current_write_in_flight: bool,
    auto_enter_enabled: bool,
    attention: HashMap<String, AttentionState>,
    restore_specs: HashMap<String, RestoreSpec>,
    restore_claims: HashSet<String>,
    history_restore_retry_after: HashMap<i64, Instant>,
}

struct AttentionState {
    due: Instant,
    consecutive_failures: u8,
    signature: String,
}

fn attention_retry_delay(consecutive_failures: u8) -> Duration {
    let exponent = u32::from(
        consecutive_failures
            .saturating_sub(1)
            .min(ATTENTION_RETRY_MAX_EXPONENT),
    );
    Duration::from_millis(ATTENTION_RETRY_BASE_MS.saturating_mul(1_u64 << exponent))
}

fn record_attention_attempt(attention: &mut AttentionState, now: Instant, succeeded: bool) {
    if succeeded {
        attention.consecutive_failures = 0;
        attention.due = now + ATTENTION_RECHECK_DELAY;
    } else {
        attention.consecutive_failures = attention.consecutive_failures.saturating_add(1);
        attention.due = now + attention_retry_delay(attention.consecutive_failures);
    }
}

fn reconcile_attention_states(
    windows: &HashMap<String, TrackedWindow>,
    attention: &mut HashMap<String, AttentionState>,
    now: Instant,
) {
    attention.retain(|id, _| windows.get(id).is_some_and(is_attention_terminal));
    for (id, window) in windows {
        if is_attention_terminal(window) {
            let signature = attention_signature(window);
            match attention.entry(id.clone()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(AttentionState {
                        due: now + ATTENTION_RECHECK_DELAY,
                        consecutive_failures: 0,
                        signature,
                    });
                }
                std::collections::hash_map::Entry::Occupied(mut entry)
                    if entry.get().signature != signature =>
                {
                    entry.get_mut().due = now + ATTENTION_RECHECK_DELAY;
                    entry.get_mut().consecutive_failures = 0;
                    entry.get_mut().signature = signature;
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
        }
    }
}

struct RuntimeInner {
    state: Mutex<State>,
    database: Mutex<TrackerDatabase>,
}

#[derive(Clone)]
struct Runtime(Arc<RuntimeInner>);

impl Runtime {
    fn claim_restore(&self, key: String) -> Result<(), String> {
        let mut state = self.0.state.lock().unwrap();
        if !state.restore_claims.insert(key.clone()) {
            return Err(format!("restore operation {key} is already in progress"));
        }
        Ok(())
    }

    fn release_restore(&self, key: &str) {
        self.0.state.lock().unwrap().restore_claims.remove(key);
    }

    fn excluded_history_restores(&self) -> HashSet<i64> {
        let now = Instant::now();
        let mut state = self.0.state.lock().unwrap();
        state
            .history_restore_retry_after
            .retain(|_, retry_after| *retry_after > now);
        let mut excluded = state
            .history_restore_retry_after
            .keys()
            .copied()
            .collect::<HashSet<_>>();
        excluded.extend(
            state
                .restore_claims
                .iter()
                .filter_map(|claim| claim.strip_prefix("history:"))
                .filter_map(|id| id.parse::<i64>().ok()),
        );
        excluded
    }

    fn defer_history_restore(&self, id: i64, reason: &'static str) {
        self.0
            .state
            .lock()
            .unwrap()
            .history_restore_retry_after
            .insert(id, Instant::now() + HISTORY_RESTORE_RETRY_DELAY);
        eprintln!(
            "tracker_restore event=history_deferred history_id={id} reason={reason} retry_ms={}",
            HISTORY_RESTORE_RETRY_DELAY.as_millis()
        );
    }

    fn clear_history_restore_deferment(&self, id: i64) {
        self.0
            .state
            .lock()
            .unwrap()
            .history_restore_retry_after
            .remove(&id);
    }

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
            build_id: crate::BUILD_ID.to_string(),
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

    fn persist_current(&self) -> Result<(), String> {
        let entries = {
            let state = self.0.state.lock().unwrap();
            state
                .windows
                .values()
                .cloned()
                .map(|window| {
                    let restore = state
                        .restore_specs
                        .get(&window.id)
                        .cloned()
                        .unwrap_or_else(|| super::infer_restore_spec(&window));
                    (window, restore)
                })
                .collect::<Vec<_>>()
        };
        self.0
            .database
            .lock()
            .unwrap()
            .replace_current_with_restore(&entries)
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

    fn expire_snapshot_if_needed(state: &mut State) {
        if state
            .snapshot_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            state.snapshot_buffer = None;
            state.snapshot_deadline = None;
        }
    }

    fn upsert(&self, mut incoming: TrackedWindow, activated: bool) {
        if !is_history_worthy(&incoming) {
            return;
        }
        let timestamp = now_ms();
        let restore = super::infer_restore_spec(&incoming);
        let mut state = self.0.state.lock().unwrap();
        Self::expire_snapshot_if_needed(&mut state);
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
        if activated
            || (incoming.active
                && previous
                    .as_ref()
                    .is_none_or(|window| window.last_activated_at_ms.is_none()))
        {
            state.activation_sequence += 1;
            incoming.activation_sequence = state.activation_sequence;
            incoming.last_activated_at_ms = Some(timestamp);
        } else if let Some(previous) = &previous {
            incoming.activation_sequence = previous.activation_sequence;
            incoming.last_activated_at_ms = previous.last_activated_at_ms;
        }
        let attention_id = incoming.id.clone();
        let requires_attention = is_attention_terminal(&incoming);
        let signature = attention_signature(&incoming);
        let changed = previous
            .as_ref()
            .is_none_or(|previous| tracked_window_state_changed(previous, &incoming));
        state.restore_specs.insert(attention_id.clone(), restore);
        if let Some(target) = state.snapshot_buffer.as_mut() {
            target.insert(incoming.id.clone(), incoming);
        } else {
            state.windows.insert(incoming.id.clone(), incoming);
        }
        if state.auto_enter_enabled && requires_attention {
            let now = Instant::now();
            match state.attention.entry(attention_id) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(AttentionState {
                        due: now + ATTENTION_RECHECK_DELAY,
                        consecutive_failures: 0,
                        signature,
                    });
                }
                std::collections::hash_map::Entry::Occupied(mut entry)
                    if entry.get().signature != signature =>
                {
                    entry.get_mut().due = now + ATTENTION_RECHECK_DELAY;
                    entry.get_mut().consecutive_failures = 0;
                    entry.get_mut().signature = signature;
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
        } else if !requires_attention {
            state.attention.remove(&attention_id);
        }
        if state.snapshot_buffer.is_none() && changed {
            Self::mark_changed(&mut state);
        }
        crate::observability::set_gauge(
            crate::observability::Gauge::TrackedWindows,
            state.windows.len(),
        );
        crate::observability::set_gauge(
            crate::observability::Gauge::AttentionPending,
            state.attention.len(),
        );
    }

    fn remove(&self, id: &str) {
        let timestamp = now_ms();
        let mut state = self.0.state.lock().unwrap();
        Self::expire_snapshot_if_needed(&mut state);
        if let Some(buffer) = state.snapshot_buffer.as_mut() {
            buffer.remove(id);
            return;
        }
        let removed = state.windows.remove(id);
        state.attention.remove(id);
        let restore = state.restore_specs.remove(id);
        if removed.is_some() {
            state.history_generation = state.history_generation.wrapping_add(1);
            Self::mark_changed(&mut state);
        }
        crate::observability::set_gauge(
            crate::observability::Gauge::TrackedWindows,
            state.windows.len(),
        );
        crate::observability::set_gauge(
            crate::observability::Gauge::AttentionPending,
            state.attention.len(),
        );
        drop(state);
        if let Some(window) = removed
            && is_history_worthy(&window)
            && let Err(err) = self.0.database.lock().unwrap().add_history_with_restore(
                &window,
                restore
                    .as_ref()
                    .unwrap_or(&super::infer_restore_spec(&window)),
                timestamp,
            )
        {
            eprintln!("Tracker failed to append window history: {err}");
        }
    }

    fn finish_snapshot(&self) {
        let timestamp = now_ms();
        let mut closed = Vec::new();
        let mut state = self.0.state.lock().unwrap();
        Self::expire_snapshot_if_needed(&mut state);
        let Some(buffer) = state.snapshot_buffer.take() else {
            return;
        };
        state.snapshot_deadline = None;
        for (id, window) in &state.windows {
            if !buffer.contains_key(id) {
                closed.push(window.clone());
            }
        }
        state.windows = buffer;
        let closed_restores = closed
            .iter()
            .map(|window| {
                (
                    window.id.clone(),
                    state
                        .restore_specs
                        .get(&window.id)
                        .cloned()
                        .unwrap_or_else(|| super::infer_restore_spec(window)),
                )
            })
            .collect::<HashMap<_, _>>();
        for window in &closed {
            state.restore_specs.remove(&window.id);
        }
        if !closed.is_empty() {
            state.history_generation = state.history_generation.wrapping_add(1);
        }
        Self::mark_changed(&mut state);
        crate::observability::set_gauge(
            crate::observability::Gauge::TrackedWindows,
            state.windows.len(),
        );
        crate::observability::set_gauge(
            crate::observability::Gauge::AttentionPending,
            state.attention.len(),
        );
        drop(state);
        let database = self.0.database.lock().unwrap();
        for window in closed {
            if is_history_worthy(&window)
                && let Err(err) = database.add_history_with_restore(
                    &window,
                    closed_restores
                        .get(&window.id)
                        .unwrap_or(&super::infer_restore_spec(&window)),
                    timestamp,
                )
            {
                eprintln!("Tracker failed to append snapshot closure: {err}");
            }
        }
        drop(database);
        if let Err(err) = self.persist_current_if_due(true) {
            eprintln!("Tracker failed to persist current windows: {err}");
        }
    }

    fn write_recovery_if_due(&self, force: bool) -> Result<(), String> {
        let (windows, boot_id, generation) = {
            let mut state = self.0.state.lock().unwrap();
            if state.recovery_pending {
                return Ok(());
            }
            if state.recovery_write_in_flight
                || !state.recovery_dirty
                || (!force && state.recovery_due.is_none_or(|due| Instant::now() < due))
            {
                return Ok(());
            }
            state.recovery_write_in_flight = true;
            (
                state
                    .windows
                    .values()
                    .filter(|window| is_history_worthy(window))
                    .cloned()
                    .collect::<Vec<_>>(),
                state.boot_id.clone(),
                state.generation,
            )
        };
        let result = self.0.database.lock().unwrap().create_snapshot(
            None,
            "recovery",
            &boot_id,
            &windows,
            now_ms(),
        );
        let mut state = self.0.state.lock().unwrap();
        state.recovery_write_in_flight = false;
        match result {
            Ok(_) => {
                crate::observability::increment(crate::observability::Counter::PersistenceWrites);
                if state.generation == generation {
                    state.recovery_dirty = false;
                    state.recovery_due = None;
                }
                Ok(())
            }
            Err(err) => {
                state.recovery_due = Some(Instant::now() + Duration::from_secs(1));
                Err(err)
            }
        }
    }

    fn persist_current_if_due(&self, force: bool) -> Result<(), String> {
        let generation = {
            let mut state = self.0.state.lock().unwrap();
            if state.current_write_in_flight
                || !state.current_dirty
                || (!force && state.current_due.is_none_or(|due| Instant::now() < due))
            {
                return Ok(());
            }
            state.current_write_in_flight = true;
            state.generation
        };
        let result = self.persist_current();
        let mut state = self.0.state.lock().unwrap();
        state.current_write_in_flight = false;
        match result {
            Ok(()) => {
                crate::observability::increment(crate::observability::Counter::PersistenceWrites);
                if state.generation == generation {
                    state.current_dirty = false;
                    state.current_due = None;
                }
                Ok(())
            }
            Err(err) => {
                state.current_due = Some(Instant::now() + Duration::from_secs(1));
                Err(err)
            }
        }
    }

    fn process_attention(&self) {
        let targets = {
            let mut state = self.0.state.lock().unwrap();
            if !state.auto_enter_enabled {
                return;
            }
            let now = Instant::now();
            let State {
                windows, attention, ..
            } = &mut *state;
            reconcile_attention_states(windows, attention, now);
            crate::observability::set_gauge(
                crate::observability::Gauge::AttentionPending,
                attention.len(),
            );
            attention
                .iter()
                .filter_map(|(id, attention)| {
                    let window = windows.get(id)?;
                    (attention.due <= now).then_some(window.clone())
                })
                .collect::<Vec<_>>()
        };
        for window in targets {
            crate::observability::increment(crate::observability::Counter::AttentionAttempts);
            let result = send_enter_to_terminal(&window);
            if result.is_err() {
                crate::observability::increment(crate::observability::Counter::AttentionFailures);
            }
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
                let signature = attention_signature(&window);
                if attention.signature == signature {
                    record_attention_attempt(attention, Instant::now(), result.is_ok());
                }
            }
            if let Err(err) = result {
                eprintln!(
                    "Automatic terminal Enter failed for {}: {err}",
                    window.title
                );
            }
        }
    }

    fn reconcile_stale_kwin_windows(&self) -> Result<usize, String> {
        let ids = {
            let state = self.0.state.lock().unwrap();
            state.windows.keys().cloned().collect::<Vec<_>>()
        };
        if ids.is_empty() {
            return Ok(0);
        }

        let connection = zbus::blocking::connection::Builder::session()
            .map_err(|err| err.to_string())?
            .method_timeout(KWIN_RECONCILIATION_TIMEOUT)
            .build()
            .map_err(|err| err.to_string())?;
        let proxy =
            zbus::blocking::Proxy::new(&connection, KWIN_SERVICE, KWIN_PATH, KWIN_INTERFACE)
                .map_err(|err| err.to_string())?;

        let mut stale_ids = Vec::new();
        for id in ids {
            let details: HashMap<String, zbus::zvariant::OwnedValue> = proxy
                .call("getWindowInfo", &(id.as_str(),))
                .map_err(|err| format!("KWin rejected window reconciliation for {id}: {err}"))?;
            if details.is_empty() {
                stale_ids.push(id);
            }
        }

        for id in &stale_ids {
            self.remove(id);
        }
        Ok(stale_ids.len())
    }

    fn schedule_layout_reconciliation(
        &self,
        specs: Vec<(TrackedWindow, super::RestoreSpec)>,
        history_id: Option<i64>,
        baseline_ids: HashSet<String>,
        claim: Option<String>,
    ) {
        let runtime = self.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let current = runtime.windows();
                let final_attempt = Instant::now() >= deadline;
                let candidates = if history_id.is_some() {
                    current
                        .iter()
                        .filter(|window| !baseline_ids.contains(&window.id))
                        .cloned()
                        .collect::<Vec<_>>()
                } else {
                    current.clone()
                };
                let matched = super::restore::matching_window_count(&specs, &candidates);
                if matched == specs.len() || final_attempt {
                    let (_, failures) =
                        super::restore::apply_matching_layouts(&specs, &candidates, true);
                    for failure in failures {
                        eprintln!("Session layout restore: {failure}");
                    }
                }
                if matched == specs.len() {
                    if let Some(history_id) = history_id {
                        match runtime
                            .0
                            .database
                            .lock()
                            .unwrap()
                            .remove_history(history_id)
                        {
                            Ok(()) => {
                                let mut state = runtime.0.state.lock().unwrap();
                                state.history_generation = state.history_generation.wrapping_add(1);
                                state.history_restore_retry_after.remove(&history_id);
                            }
                            Err(err) => eprintln!(
                                "Session restore matched, but history entry {history_id} could not be removed: {err}"
                            ),
                        }
                    }
                    if let Some(claim) = claim.as_deref() {
                        runtime.release_restore(claim);
                    }
                    break;
                }
                if final_attempt {
                    eprintln!(
                        "Session layout restore timed out with {matched}/{} windows matched",
                        specs.len()
                    );
                    if let Some(history_id) = history_id {
                        runtime.defer_history_restore(history_id, "window_match_timeout");
                    }
                    if let Some(claim) = claim.as_deref() {
                        runtime.release_restore(claim);
                    }
                    break;
                }
                std::thread::sleep(layout_reconciliation_poll_interval(started.elapsed()));
            }
        });
    }
}

fn layout_reconciliation_poll_interval(elapsed: Duration) -> Duration {
    if elapsed < LAYOUT_RECONCILIATION_FAST_PERIOD {
        LAYOUT_RECONCILIATION_FAST_POLL
    } else {
        LAYOUT_RECONCILIATION_SLOW_POLL
    }
}

fn is_attention_terminal(window: &TrackedWindow) -> bool {
    window
        .class
        .to_lowercase()
        .replace(['-', '_'], "")
        .contains("xfce4terminal")
        && window.title.to_lowercase().contains("action required")
}

fn attention_signature(window: &TrackedWindow) -> String {
    let title = window.title.to_lowercase();
    let Some(action_start) = title.find("action required") else {
        return String::new();
    };

    title[action_start..]
        .chars()
        .filter(|character| !('\u{2800}'..='\u{28ff}').contains(character))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_history_worthy(window: &TrackedWindow) -> bool {
    let class = window.class.to_lowercase();
    !window.title.trim().is_empty()
        && !window.class.trim().is_empty()
        && !window.skip_taskbar
        && !window.skip_switcher
        && !is_compact_chromium_helper_surface(
            &window.class,
            Some(&window.desktop_file_name),
            window.width,
        )
        && !matches!(
            class.as_str(),
            "plasmashell" | "org.kde.plasmashell" | "kwin_wayland" | "applicationlauncher"
        )
}

fn tracked_window_state_changed(previous: &TrackedWindow, incoming: &TrackedWindow) -> bool {
    let mut comparable = previous.clone();
    comparable.updated_at_ms = incoming.updated_at_ms;
    comparable.opened_at_ms = incoming.opened_at_ms;
    comparable != *incoming
}

fn normalized_terminal_title(title: &str) -> String {
    title
        .chars()
        .filter(|character| !matches!(*character as u32, 0x2800..=0x28ff))
        .collect::<String>()
        .replace("[ ! ]", "")
        .replace("[ . ]", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn terminal_dbus_service_names(names: Vec<String>) -> Vec<String> {
    let mut services = names
        .into_iter()
        .filter(|name| {
            name == "org.xfce.Terminal5" || name.starts_with("org.xfce.Terminal5.Instance.")
        })
        .collect::<Vec<_>>();
    services.sort();
    services.dedup();
    services
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
    let connection = zbus::blocking::connection::Builder::session()
        .map_err(|err| err.to_string())?
        .method_timeout(TERMINAL_DBUS_TIMEOUT)
        .build()
        .map_err(|err| err.to_string())?;
    let dbus_proxy = zbus::blocking::Proxy::new(
        &connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .map_err(|err| err.to_string())?;
    let names: Vec<String> = dbus_proxy
        .call("ListNames", &())
        .map_err(|err| err.to_string())?;
    let services = terminal_dbus_service_names(names);
    if services.is_empty() {
        return Err("No XFCE4 Terminal D-Bus services are available".to_string());
    }

    let wanted = normalized_terminal_title(&window.title);
    let mut matches = Vec::new();
    for service in services {
        let Ok(proxy) = zbus::blocking::Proxy::new(
            &connection,
            service.as_str(),
            "/org/xfce/Terminal",
            "org.xfce.Terminal5",
        ) else {
            continue;
        };
        let Ok(records) = proxy
            .call::<_, _, Vec<HashMap<String, zbus::zvariant::OwnedValue>>>("ListTerminals", &())
        else {
            continue;
        };
        let owner_pid = dbus_proxy
            .call::<_, _, u32>("GetConnectionUnixProcessID", &(service.as_str(),))
            .ok();
        let active_records = records
            .iter()
            .filter(|record| dbus_bool(record, "active").unwrap_or(false))
            .collect::<Vec<_>>();
        let process_identifies_tab =
            owner_pid == u32::try_from(window.pid).ok() && active_records.len() == 1;

        for record in active_records {
            let title_matches = dbus_string(record, "window_title")
                .is_some_and(|title| normalized_terminal_title(&title) == wanted);
            if (process_identifies_tab || title_matches)
                && let Some(tab_uuid) = dbus_string(record, "tab_uuid")
            {
                matches.push((service.clone(), tab_uuid));
            }
        }
    }

    matches.sort();
    matches.dedup();
    let [(service, tab_uuid)] = matches.as_slice() else {
        if matches.is_empty() {
            return Err("No active terminal tab matched this window".to_string());
        }
        return Err("Multiple terminal tabs matched this window".into());
    };
    let proxy = zbus::blocking::Proxy::new(
        &connection,
        service.as_str(),
        "/org/xfce/Terminal",
        "org.xfce.Terminal5",
    )
    .map_err(|err| err.to_string())?;
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
        let mut state = self.0.0.state.lock().unwrap();
        state.snapshot_buffer = Some(HashMap::new());
        state.snapshot_deadline = Some(Instant::now() + SNAPSHOT_TRANSACTION_TIMEOUT);
    }

    #[zbus(name = "ResetWindows")]
    fn reset_windows(&self) {
        self.begin_snapshot();
    }

    #[zbus(name = "UpsertWindow")]
    fn upsert_window(&self, payload: &str) {
        crate::observability::increment(crate::observability::Counter::KwinUpserts);
        match serde_json::from_str(payload) {
            Ok(window) => self.0.upsert(window, false),
            Err(err) => eprintln!("Tracker rejected KWin payload: {err}"),
        }
    }

    #[zbus(name = "ReplaceSnapshot")]
    fn replace_snapshot(&self, payload: &str) {
        crate::observability::increment(crate::observability::Counter::KwinSnapshots);
        let windows = match serde_json::from_str::<Vec<TrackedWindow>>(payload) {
            Ok(windows) => windows,
            Err(err) => {
                eprintln!("Tracker rejected KWin snapshot: {err}");
                return;
            }
        };
        crate::observability::record(
            crate::observability::Event::new("kwin-feed", "replace-snapshot")
                .object(&windows.len().to_string())
                .reason("authoritative-resync"),
        );
        self.begin_snapshot();
        for window in windows {
            self.0.upsert(window, false);
        }
        self.0.finish_snapshot();
    }

    #[zbus(name = "WindowActivated")]
    fn window_activated(&self, payload: &str) {
        crate::observability::increment(crate::observability::Counter::KwinActivations);
        if let Ok(window) = serde_json::from_str(payload) {
            self.0.upsert(window, true);
        }
    }

    #[zbus(name = "RemoveWindow")]
    fn remove_window(&self, id: &str) {
        crate::observability::increment(crate::observability::Counter::KwinRemovals);
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
            let snapshot = snapshot.ok_or_else(|| format!("Snapshot {id} does not exist"))?;
            let claim = format!("snapshot:{id}");
            self.0.claim_restore(claim.clone())?;
            let report = super::restore_snapshot(&snapshot, &self.0.windows());
            if report.launched > 0 {
                self.0.schedule_layout_reconciliation(
                    snapshot.windows,
                    None,
                    HashSet::new(),
                    Some(claim),
                );
            } else {
                self.0.release_restore(&claim);
            }
            Ok(report)
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
        let result = snapshot.and_then(|snapshot| {
            let claim = "recovery".to_string();
            self.0.claim_restore(claim.clone())?;
            let report = super::restore_snapshot(&snapshot, &self.0.windows());
            if report.launched > 0 {
                self.0.schedule_layout_reconciliation(
                    snapshot.windows,
                    None,
                    HashSet::new(),
                    Some(claim),
                );
            } else {
                self.0.release_restore(&claim);
            }
            Ok(report)
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
        let history = self
            .0
            .0
            .database
            .lock()
            .unwrap()
            .history(REOPEN_HISTORY_SCAN_LIMIT);
        let result = history.and_then(|history| reopen_latest_history_entry(&self.0, &history));
        serde_json::to_string(&result).unwrap()
    }

    #[zbus(name = "SetReopenShortcutActive")]
    fn set_reopen_shortcut_active(&self, active: bool) -> bool {
        match set_reopen_shortcut_active(active) {
            Ok(()) => true,
            Err(err) => {
                eprintln!(
                    "tracker_shortcut event=set_reopen_active_failed active={active} error={err}"
                );
                false
            }
        }
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
            state.history_restore_retry_after.clear();
        }
        serde_json::to_string(&result).unwrap()
    }

    #[zbus(name = "SetAutoEnter")]
    fn set_auto_enter(&self, enabled: bool) -> String {
        let mut state = self.0.0.state.lock().unwrap();
        state.auto_enter_enabled = enabled;
        if enabled {
            let due = Instant::now() + Duration::from_secs(5);
            let attention_ids = state
                .windows
                .values()
                .filter(|window| is_attention_terminal(window))
                .map(|window| (window.id.clone(), attention_signature(window)))
                .collect::<Vec<_>>();
            for (id, signature) in attention_ids {
                state.attention.entry(id).or_insert(AttentionState {
                    due,
                    consecutive_failures: 0,
                    signature,
                });
            }
        } else {
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
        .history_entry(id)?
        .ok_or_else(|| format!("History entry {id} does not exist"))?;
    if !is_history_worthy(&entry.window) {
        return Err(format!(
            "{} is a desktop shell surface, not a reopenable application window",
            entry.window.title
        ));
    }

    let baseline_ids = runtime
        .windows()
        .into_iter()
        .map(|window| window.id)
        .collect::<HashSet<_>>();
    let claim = format!("history:{id}");
    runtime.claim_restore(claim.clone())?;
    let report = super::restore::reopen_entry(&entry);
    if report.launched > 0 {
        runtime.clear_history_restore_deferment(id);
        runtime.schedule_layout_reconciliation(
            vec![(entry.window, entry.restore)],
            Some(id),
            baseline_ids,
            Some(claim),
        );
    } else {
        runtime.release_restore(&claim);
        runtime.defer_history_restore(id, "launch_failed");
    }
    Ok(report)
}

fn reopen_latest_history_entry(
    runtime: &Runtime,
    history: &[HistoryEntry],
) -> Result<RestoreReport, String> {
    let excluded = runtime.excluded_history_restores();
    let (candidate_ids, stats) = reopenable_history_ids(history, &excluded);
    let (selected, launch_failures) =
        first_launched_history_report(candidate_ids, |id| reopen_history_entry(runtime, id));
    if let Some((id, report)) = selected {
        eprintln!(
            "tracker_restore event=reopen_latest_selected history_id={id} scanned={} excluded={} non_window={} unsupported_terminal={} missing_executable={} missing_desktop={} non_launchable_desktop={} prior_launch_failures={launch_failures}",
            stats.scanned,
            stats.excluded,
            stats.non_window,
            stats.unsupported_terminal,
            stats.missing_executable,
            stats.missing_desktop,
            stats.non_launchable_desktop,
        );
        return Ok(report);
    }

    eprintln!(
        "tracker_restore event=reopen_latest_unavailable scanned={} excluded={} non_window={} unsupported_terminal={} missing_executable={} missing_desktop={} non_launchable_desktop={} launch_failures={launch_failures}",
        stats.scanned,
        stats.excluded,
        stats.non_window,
        stats.unsupported_terminal,
        stats.missing_executable,
        stats.missing_desktop,
        stats.non_launchable_desktop,
    );
    Err("No reopenable recently closed windows are currently available".into())
}

fn first_launched_history_report(
    candidate_ids: impl IntoIterator<Item = i64>,
    mut reopen: impl FnMut(i64) -> Result<RestoreReport, String>,
) -> (Option<(i64, RestoreReport)>, usize) {
    let mut failures = 0;
    for id in candidate_ids {
        match reopen(id) {
            Ok(report) if report.launched > 0 => return (Some((id, report)), failures),
            Ok(_) | Err(_) => failures += 1,
        }
    }
    (None, failures)
}

#[derive(Default)]
struct ReopenSelectionStats {
    scanned: usize,
    excluded: usize,
    non_window: usize,
    unsupported_terminal: usize,
    missing_executable: usize,
    missing_desktop: usize,
    non_launchable_desktop: usize,
}

fn reopenable_history_ids(
    history: &[HistoryEntry],
    excluded: &HashSet<i64>,
) -> (Vec<i64>, ReopenSelectionStats) {
    reopenable_history_ids_with(
        history,
        excluded,
        REOPEN_LAUNCH_ATTEMPT_LIMIT,
        super::restore::unavailable_reason,
    )
}

fn reopenable_history_ids_with(
    history: &[HistoryEntry],
    excluded: &HashSet<i64>,
    limit: usize,
    unavailable_reason: impl Fn(&RestoreSpec) -> Option<super::restore::LaunchUnavailableCode>,
) -> (Vec<i64>, ReopenSelectionStats) {
    let mut stats = ReopenSelectionStats::default();
    let candidates = history
        .iter()
        .filter_map(|entry| {
            stats.scanned += 1;
            if excluded.contains(&entry.id) {
                stats.excluded += 1;
                return None;
            }
            if !is_history_worthy(&entry.window) {
                stats.non_window += 1;
                return None;
            }
            if let Some(reason) = unavailable_reason(&entry.restore) {
                match reason {
                    super::restore::LaunchUnavailableCode::UnsupportedTerminalKind => {
                        stats.unsupported_terminal += 1;
                    }
                    super::restore::LaunchUnavailableCode::MissingExecutable => {
                        stats.missing_executable += 1;
                    }
                    super::restore::LaunchUnavailableCode::MissingDesktopEntry => {
                        stats.missing_desktop += 1;
                    }
                    super::restore::LaunchUnavailableCode::NonLaunchableDesktopEntry => {
                        stats.non_launchable_desktop += 1;
                    }
                }
                return None;
            }
            Some(entry.id)
        })
        .take(limit)
        .collect();
    (candidates, stats)
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
    let state_dir = super::state_dir();
    std::fs::create_dir_all(&state_dir)
        .map_err(|err| format!("could not create tracker state directory: {err}"))?;
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|err| format!("could not secure tracker state directory: {err}"))?;
    let database_path = state_dir.join("history.sqlite3");
    let database = TrackerDatabase::open(&database_path)?;
    database.prune_shell_surface_history()?;
    let boot_id = read_boot_id();
    let previous_boot = database.meta("boot_id")?.unwrap_or_default();
    let same_boot = previous_boot == boot_id;
    let previous_clean = database.meta("clean_shutdown")?.as_deref() == Some("true");
    let recovery_pending = !previous_boot.is_empty() && previous_boot != boot_id && !previous_clean;
    let persisted_entries = if same_boot {
        database.current_window_entries()?
    } else {
        Vec::new()
    };
    let activation_sequence = persisted_entries
        .iter()
        .map(|(window, _)| window.activation_sequence)
        .max()
        .unwrap_or_default();
    let restore_specs = persisted_entries
        .iter()
        .map(|(window, restore)| (window.id.clone(), restore.clone()))
        .collect::<HashMap<_, _>>();
    let persisted_windows = persisted_entries
        .into_iter()
        .map(|(window, _)| (window.id.clone(), window))
        .collect::<HashMap<_, _>>();
    database.set_meta("boot_id", &boot_id)?;
    database.set_meta("clean_shutdown", "false")?;
    database.set_meta("recovery_dismissed", "false")?;
    let run_id = run_id();
    database.set_meta("run_id", &run_id)?;
    let auto_enter_enabled = database.meta("auto_enter")?.as_deref() == Some("true");

    let runtime = Runtime(Arc::new(RuntimeInner {
        state: Mutex::new(State {
            windows: persisted_windows,
            snapshot_buffer: None,
            snapshot_deadline: None,
            generation: 0,
            history_generation: 0,
            activation_sequence,
            recovery_pending,
            run_id,
            boot_id,
            recovery_dirty: false,
            recovery_due: None,
            current_dirty: false,
            current_due: None,
            recovery_write_in_flight: false,
            current_write_in_flight: false,
            auto_enter_enabled,
            attention: HashMap::new(),
            restore_specs,
            restore_claims: HashSet::new(),
            history_restore_retry_after: HashMap::new(),
        }),
        database: Mutex::new(database),
    }));
    crate::observability::set_gauge(
        crate::observability::Gauge::TrackedWindows,
        runtime.0.state.lock().unwrap().windows.len(),
    );
    crate::observability::record(
        crate::observability::Event::new("tracker", "state-loaded")
            .reason(if same_boot { "same-boot" } else { "new-boot" })
            .transition("database", "runtime"),
    );

    if auto_enter_enabled {
        let due = Instant::now() + Duration::from_secs(5);
        let mut state = runtime.0.state.lock().unwrap();
        let attention_ids = state
            .windows
            .values()
            .filter(|window| is_attention_terminal(window))
            .map(|window| (window.id.clone(), attention_signature(window)))
            .collect::<Vec<_>>();
        for (id, signature) in attention_ids {
            state.attention.insert(
                id,
                AttentionState {
                    due,
                    consecutive_failures: 0,
                    signature,
                },
            );
        }
    }

    let recovery_runtime = runtime.clone();
    crate::observability::spawn_named("tracker-persistence", move |worker| {
        worker.set_state("waiting");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            loop {
                std::thread::sleep(Duration::from_millis(250));
                if let Err(err) = recovery_runtime.write_recovery_if_due(false) {
                    eprintln!("Tracker failed to write recovery snapshot: {err}");
                }
                if let Err(err) = recovery_runtime.persist_current_if_due(false) {
                    eprintln!("Tracker failed to persist current windows: {err}");
                }
            }
        }));
        if result.is_err() {
            crate::observability::increment(crate::observability::Counter::WorkerPanics);
            eprintln!("Tracker persistence worker panicked; restarting daemon");
            std::process::exit(1);
        }
    });

    let attention_runtime = runtime.clone();
    crate::observability::spawn_named("tracker-attention", move |worker| {
        worker.set_state("waiting");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            loop {
                std::thread::sleep(Duration::from_millis(250));
                attention_runtime.process_attention();
            }
        }));
        if result.is_err() {
            crate::observability::increment(crate::observability::Counter::WorkerPanics);
            eprintln!("Tracker attention worker panicked; restarting daemon");
            std::process::exit(1);
        }
    });

    let reconciliation_runtime = runtime.clone();
    crate::observability::spawn_named("tracker-reconcile", move |worker| {
        worker.set_state("waiting");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            loop {
                if let Err(err) = reconciliation_runtime.reconcile_stale_kwin_windows() {
                    eprintln!("Tracker KWin window reconciliation failed: {err}");
                }
                std::thread::sleep(KWIN_RECONCILIATION_INTERVAL);
            }
        }));
        if result.is_err() {
            crate::observability::increment(crate::observability::Counter::WorkerPanics);
            eprintln!("Tracker KWin reconciliation worker panicked; restarting daemon");
            std::process::exit(1);
        }
    });

    let shutdown_runtime = runtime.clone();
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
    ])
    .map_err(|err| err.to_string())?;
    crate::observability::spawn_named("tracker-signals", move |worker| {
        worker.set_state("waiting");
        if signals.forever().next().is_some() {
            let recovery_ok = shutdown_runtime.write_recovery_if_due(true).is_ok();
            let current_ok = shutdown_runtime.persist_current_if_due(true).is_ok();
            if recovery_ok && current_ok {
                let _ = shutdown_runtime
                    .0
                    .database
                    .lock()
                    .unwrap()
                    .set_meta("clean_shutdown", "true");
            }
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

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::time::{Duration, Instant};

    use super::{
        ATTENTION_RECHECK_DELAY, AttentionState, attention_retry_delay,
        first_launched_history_report, is_history_worthy, layout_reconciliation_poll_interval,
        normalized_terminal_title, reconcile_attention_states, record_attention_attempt,
        reopen_shortcut_action_id, reopenable_history_ids_with, terminal_dbus_service_names,
        tracked_window_state_changed,
    };
    use crate::tracker::restore::LaunchUnavailableCode;
    use crate::tracker::{HistoryEntry, RestoreReport, RestoreSpec, TrackedWindow};

    #[test]
    fn reopened_windows_use_fast_initial_reconciliation() {
        assert_eq!(
            layout_reconciliation_poll_interval(Duration::ZERO),
            Duration::from_millis(40)
        );
        assert_eq!(
            layout_reconciliation_poll_interval(Duration::from_secs(2)),
            Duration::from_millis(250)
        );
    }

    #[test]
    fn browser_passthrough_targets_only_the_launcher_reopen_action() {
        assert_eq!(
            reopen_shortcut_action_id(),
            [
                "kwin",
                "applicationlauncher-reopen-latest",
                "KWin",
                "Reopen recently closed window",
            ]
        );
    }

    #[test]
    fn global_reopen_skips_unlaunchable_history_entries() {
        let unlaunchable = HistoryEntry {
            id: 2,
            window: TrackedWindow {
                title: "chatgpt".into(),
                class: "electron".into(),
                ..TrackedWindow::default()
            },
            closed_at_ms: 2,
            restore: RestoreSpec {
                app_key: "missing-electron-desktop-entry".into(),
                desktop_file: Some("missing-electron-desktop-entry".into()),
                ..RestoreSpec::default()
            },
        };
        let terminal = HistoryEntry {
            id: 1,
            window: TrackedWindow {
                title: "~ - Terminal".into(),
                class: "xfce4-terminal".into(),
                ..TrackedWindow::default()
            },
            closed_at_ms: 1,
            restore: RestoreSpec {
                app_key: "xfce4-terminal".into(),
                terminal_kind: Some("shell".into()),
                ..RestoreSpec::default()
            },
        };

        let history = [unlaunchable, terminal];
        let (candidates, stats) =
            reopenable_history_ids_with(&history, &HashSet::new(), 4, |restore| {
                (restore.app_key == "missing-electron-desktop-entry")
                    .then_some(LaunchUnavailableCode::MissingDesktopEntry)
            });

        assert_eq!(candidates, [1]);
        assert_eq!(stats.scanned, 2);
        assert_eq!(stats.missing_desktop, 1);
        assert_eq!(stats.excluded, 0);
    }

    #[test]
    fn global_reopen_skips_in_flight_entries_and_bounds_candidates() {
        let history = (1..=8)
            .rev()
            .map(|id| HistoryEntry {
                id,
                window: TrackedWindow {
                    title: format!("Window {id}"),
                    class: "test-app".into(),
                    ..TrackedWindow::default()
                },
                closed_at_ms: id,
                restore: RestoreSpec {
                    app_key: "test-app".into(),
                    ..RestoreSpec::default()
                },
            })
            .collect::<Vec<_>>();
        let excluded = HashSet::from([8, 7]);

        let (candidates, stats) = reopenable_history_ids_with(&history, &excluded, 3, |_| None);

        assert_eq!(candidates, [6, 5, 4]);
        assert_eq!(stats.scanned, 5);
        assert_eq!(stats.excluded, 2);
    }

    #[test]
    fn global_reopen_continues_after_immediate_launch_failures() {
        let mut attempts = Vec::new();

        let (selected, failures) = first_launched_history_report([3, 2, 1], |id| {
            attempts.push(id);
            Ok(RestoreReport {
                launched: usize::from(id == 1),
                ..RestoreReport::default()
            })
        });

        assert_eq!(attempts, [3, 2, 1]);
        assert_eq!(selected.map(|(id, _)| id), Some(1));
        assert_eq!(failures, 2);
    }

    #[test]
    fn successful_attention_send_rearms_identical_back_to_back_prompts() {
        let now = Instant::now();
        let mut attention = AttentionState {
            due: now,
            consecutive_failures: u8::MAX,
            signature: "action required | tree".to_string(),
        };

        record_attention_attempt(&mut attention, now, true);

        assert_eq!(attention.consecutive_failures, 0);
        assert_eq!(attention.due, now + ATTENTION_RECHECK_DELAY);
    }

    #[test]
    fn failed_attention_sends_keep_retrying_with_capped_backoff() {
        let now = Instant::now();
        let mut attention = AttentionState {
            due: now,
            consecutive_failures: 6,
            signature: "action required | tree".to_string(),
        };

        record_attention_attempt(&mut attention, now, false);
        assert_eq!(attention.consecutive_failures, 7);
        assert_eq!(attention.due, now + Duration::from_secs(48));

        let later = now + Duration::from_secs(100);
        record_attention_attempt(&mut attention, later, false);
        assert_eq!(attention.consecutive_failures, 8);
        assert_eq!(attention.due, later + attention_retry_delay(8));
        assert_eq!(attention_retry_delay(8), Duration::from_secs(48));
    }

    #[test]
    fn attention_queue_reconciles_from_authoritative_window_state() {
        let now = Instant::now();
        let mut windows = HashMap::new();
        windows.insert(
            "terminal".to_string(),
            TrackedWindow {
                id: "terminal".to_string(),
                title: "[ ! ] Action Required | tree - Terminal".to_string(),
                class: "xfce4-terminal".to_string(),
                ..TrackedWindow::default()
            },
        );
        let mut attention = HashMap::new();

        reconcile_attention_states(&windows, &mut attention, now);

        let pending = attention.get("terminal").unwrap();
        assert_eq!(pending.consecutive_failures, 0);
        assert_eq!(pending.due, now + ATTENTION_RECHECK_DELAY);

        windows.get_mut("terminal").unwrap().title = "tree - Terminal".to_string();
        reconcile_attention_states(&windows, &mut attention, now);
        assert!(attention.is_empty());
    }

    #[test]
    fn attention_animation_does_not_change_terminal_title_identity() {
        assert_eq!(
            normalized_terminal_title("[ ! ] Action Required | wordelites - Terminal"),
            normalized_terminal_title("[ . ] Action Required | wordelites - Terminal")
        );
    }

    #[test]
    fn discovers_shared_and_per_process_terminal_services() {
        assert_eq!(
            terminal_dbus_service_names(vec![
                ":1.2".to_string(),
                "org.xfce.Terminal5.Instance.iabc".to_string(),
                "org.xfce.Terminal5".to_string(),
                "org.example.Other".to_string(),
            ]),
            vec![
                "org.xfce.Terminal5".to_string(),
                "org.xfce.Terminal5.Instance.iabc".to_string(),
            ]
        );
    }

    #[test]
    fn tracker_generation_ignores_storage_timestamps_but_detects_window_changes() {
        let previous = TrackedWindow {
            id: "window".into(),
            title: "first".into(),
            class: "example".into(),
            opened_at_ms: 10,
            updated_at_ms: 20,
            ..TrackedWindow::default()
        };
        let mut incoming = previous.clone();
        incoming.updated_at_ms = 30;

        assert!(!tracked_window_state_changed(&previous, &incoming));
        incoming.title = "second".into();
        assert!(tracked_window_state_changed(&previous, &incoming));
    }

    #[test]
    fn compact_chromium_extension_helpers_are_not_session_windows() {
        let window = TrackedWindow {
            id: "extension-popup".into(),
            title: "transparency".into(),
            class: "chrome-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-Default".into(),
            desktop_file_name: "chrome-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-Default".into(),
            width: 150,
            height: 478,
            ..TrackedWindow::default()
        };

        assert!(!is_history_worthy(&window));
    }
}
