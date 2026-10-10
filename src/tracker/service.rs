use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use zbus::interface;

use super::database::{PERIODIC_SNAPSHOT_INTERVAL_MS, TrackerDatabase, periodic_snapshot_delay};
use super::{
    FEED_PATH, HistoryEntry, RestoreReport, RestoreSpec, SERVICE_NAME, TRACKER_PATH, TrackedWindow,
    TrackerStatus, is_compact_chromium_helper_surface, now_ms,
};

const KWIN_RECONCILIATION_INTERVAL: Duration = Duration::from_secs(15);
const KWIN_RECONCILIATION_TIMEOUT: Duration = Duration::from_secs(2);
const LAYOUT_RECONCILIATION_FAST_POLL: Duration = Duration::from_millis(40);
const LAYOUT_RECONCILIATION_SLOW_POLL: Duration = Duration::from_millis(250);
const LAYOUT_RECONCILIATION_FAST_PERIOD: Duration = Duration::from_secs(2);
const LAYOUT_VERIFY_INITIAL_DELAY: Duration = Duration::from_millis(180);
const LAYOUT_VERIFY_CONFIRM_DELAY: Duration = Duration::from_millis(350);
const LAYOUT_VERIFY_FINAL_DELAY: Duration = Duration::from_millis(650);
const LAYOUT_APPLY_ATTEMPTS: u8 = 3;
const MAX_RESTORE_REPORTS: usize = 32;
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
const RECOVERY_KIND: &str = "recovery";
const RECOVERY_PENDING_META: &str = "recovery_pending";
const RECOVERY_CHECK_PENDING_META: &str = "recovery_check_pending";
const MASS_DISAPPEARANCE_MIN_WINDOWS: usize = 3;
const MASS_DISAPPEARANCE_CONFIRM_DELAY: Duration = Duration::from_secs(2);
const MASS_DISAPPEARANCE_MAX_CONFIRM_ATTEMPTS: u8 = 5;
const MASS_REMOVAL_WINDOW: Duration = Duration::from_secs(3);
const PERIODIC_SNAPSHOT_RETRY_DELAY: Duration = Duration::from_secs(30);
const RESTORE_SPEC_CACHE_MAX_AGE: Duration = Duration::from_millis(500);

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

#[derive(Default)]
struct State {
    windows: HashMap<String, TrackedWindow>,
    snapshot_buffer: Option<HashMap<String, TrackedWindow>>,
    snapshot_deadline: Option<Instant>,
    snapshot_durable_changed: bool,
    generation: u64,
    window_changes: HashMap<String, u64>,
    changes_floor: u64,
    durable_generation: u64,
    dirty_window_ids: HashMap<String, u64>,
    full_persist_generation: Option<u64>,
    history_generation: u64,
    activation_sequence: i64,
    recovery_pending: bool,
    checkpoint_guarded: bool,
    mass_disappearance: Option<MassDisappearanceCheck>,
    removal_burst: Option<RemovalBurst>,
    recovery_check_sequence: u64,
    run_id: String,
    boot_id: String,
    recovery_dirty: bool,
    recovery_due: Option<Instant>,
    current_dirty: bool,
    current_due: Option<Instant>,
    recovery_write_in_flight: bool,
    current_write_in_flight: bool,
    initial_snapshot_received: bool,
    periodic_due: Option<Instant>,
    periodic_write_in_flight: bool,
    auto_enter_enabled: bool,
    attention: HashMap<String, AttentionState>,
    restore_specs: HashMap<String, RestoreSpec>,
    restore_spec_checked_at: HashMap<String, Instant>,
    restore_claims: HashSet<String>,
    history_restore_retry_after: HashMap<i64, Instant>,
    restore_sequence: u64,
    restore_reports: HashMap<String, RestoreReport>,
    restore_report_order: VecDeque<String>,
    recovery_restore_operations: HashSet<String>,
    terminal_upgrade_windows: HashSet<String>,
    terminal_upgrade_closed: HashSet<String>,
}

fn window_changes_since(state: &State, since: u64, run_id: &str) -> super::WindowChanges {
    let reset = run_id != state.run_id || since < state.changes_floor || since > state.generation;
    let mut changes = super::WindowChanges {
        run_id: state.run_id.clone(),
        generation: state.generation,
        reset,
        ..Default::default()
    };
    if reset {
        changes.windows.extend(state.windows.values().cloned());
    } else {
        for (id, revision) in &state.window_changes {
            if *revision <= since {
                continue;
            }
            match state.windows.get(id) {
                Some(window) => changes.windows.push(window.clone()),
                None => changes.removed.push(id.clone()),
            }
        }
    }
    changes.windows.sort_by(|a, b| {
        a.activation_sequence
            .cmp(&b.activation_sequence)
            .then_with(|| a.id.cmp(&b.id))
    });
    changes.removed.sort();
    changes
}

fn reusable_restore_spec(
    state: &State,
    incoming: &TrackedWindow,
    now: Instant,
) -> Option<RestoreSpec> {
    let checked_at = state.restore_spec_checked_at.get(&incoming.id)?;
    if now.saturating_duration_since(*checked_at) >= RESTORE_SPEC_CACHE_MAX_AGE {
        return None;
    }
    state
        .snapshot_buffer
        .as_ref()
        .and_then(|windows| windows.get(&incoming.id))
        .or_else(|| state.windows.get(&incoming.id))
        .filter(|previous| {
            previous.pid == incoming.pid
                && previous.class == incoming.class
                && previous.desktop_file_name == incoming.desktop_file_name
                && previous.title != incoming.title
                && previous
                    .title
                    .chars()
                    .any(|ch| matches!(ch as u32, 0x2800..=0x28ff))
                    == incoming
                        .title
                        .chars()
                        .any(|ch| matches!(ch as u32, 0x2800..=0x28ff))
                && super::database::stable_title(&previous.title)
                    == super::database::stable_title(&incoming.title)
        })
        .and_then(|_| state.restore_specs.get(&incoming.id))
        .cloned()
}

struct AttentionState {
    due: Instant,
    consecutive_failures: u8,
    signature: String,
}

struct MassDisappearanceCheck {
    token: u64,
    baseline_entries: Vec<(TrackedWindow, RestoreSpec)>,
    baseline_count: usize,
    attempts: u8,
}

struct RemovalBurst {
    started: Instant,
    baseline_entries: Vec<(TrackedWindow, RestoreSpec)>,
}

#[derive(Clone)]
struct MassDisappearanceTrigger {
    token: u64,
    baseline_entries: Vec<(TrackedWindow, RestoreSpec)>,
}

struct LayoutProgress {
    current_window_id: String,
    target: super::restore::LayoutTarget,
    attempts: u8,
    exact_observations: u8,
    verify_after: Instant,
    ambiguous_match: bool,
    last_mismatch: Option<String>,
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

fn is_mass_disappearance(previous_count: usize, current_count: usize) -> bool {
    previous_count >= MASS_DISAPPEARANCE_MIN_WINDOWS
        && previous_count.saturating_sub(current_count) >= MASS_DISAPPEARANCE_MIN_WINDOWS
        && current_count <= previous_count / 2
}

fn should_offer_recovery(
    previous_boot: &str,
    current_boot: &str,
    previous_clean: bool,
    persisted_pending: bool,
    persisted_check: bool,
    snapshot_exists: bool,
) -> bool {
    snapshot_exists
        && ((!previous_boot.is_empty() && previous_boot != current_boot)
            || (!previous_boot.is_empty() && previous_boot == current_boot && !previous_clean)
            || persisted_pending
            || persisted_check)
}

fn state_snapshot_entries(state: &State) -> Vec<(TrackedWindow, RestoreSpec)> {
    state
        .windows
        .values()
        .filter(|window| is_history_worthy(window))
        .cloned()
        .map(|window| {
            let restore = state
                .restore_specs
                .get(&window.id)
                .cloned()
                .unwrap_or_else(|| super::infer_restore_spec(&window));
            (window, restore)
        })
        .collect()
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
    terminal_upgrades: Mutex<super::terminal_upgrade::UpgradeRegistry>,
}

#[derive(Clone)]
struct Runtime(Arc<RuntimeInner>);

impl Runtime {
    fn history_generation(&self) -> u64 {
        self.0.state.lock().unwrap().history_generation
    }

    fn claim_restore(&self, key: String) -> Result<(), String> {
        let mut state = self.0.state.lock().unwrap();
        if state.restore_claims.contains("terminal-upgrade") {
            return Err(
                "Terminal maintenance is running; wait before restoring another session".into(),
            );
        }
        if !state.restore_claims.insert(key.clone()) {
            return Err(format!("restore operation {key} is already in progress"));
        }
        Ok(())
    }

    fn release_restore(&self, key: &str) {
        self.0.state.lock().unwrap().restore_claims.remove(key);
    }

    fn register_restore_report(&self, mut report: RestoreReport) -> RestoreReport {
        let mut state = self.0.state.lock().unwrap();
        state.restore_sequence = state.restore_sequence.wrapping_add(1);
        let operation_id = format!("{}-restore-{}", state.run_id, state.restore_sequence);
        report.operation_id = Some(operation_id.clone());
        while state.restore_report_order.len() >= MAX_RESTORE_REPORTS {
            if let Some(expired) = state.restore_report_order.pop_front() {
                state.restore_reports.remove(&expired);
            }
        }
        state.restore_report_order.push_back(operation_id.clone());
        state.restore_reports.insert(operation_id, report.clone());
        report
    }

    fn restore_report(&self, operation_id: &str) -> Option<RestoreReport> {
        self.0
            .state
            .lock()
            .unwrap()
            .restore_reports
            .get(operation_id)
            .cloned()
    }

    fn update_restore_outcome(
        &self,
        operation_id: &str,
        outcome_index: usize,
        restored_window_id: Option<String>,
        status: super::RestoreOutcomeStatus,
        detail: String,
    ) {
        let mut state = self.0.state.lock().unwrap();
        let Some(report) = state.restore_reports.get_mut(operation_id) else {
            return;
        };
        let Some(outcome) = report.outcomes.get_mut(outcome_index) else {
            return;
        };
        if restored_window_id.is_some() {
            outcome.restored_window_id = restored_window_id;
        }
        outcome.status = status;
        outcome.detail = detail;
    }

    fn add_restore_failure(&self, operation_id: &str, failure: String) {
        if let Some(report) = self
            .0
            .state
            .lock()
            .unwrap()
            .restore_reports
            .get_mut(operation_id)
        {
            report.failures.push(failure);
        }
    }

    fn finish_restore_report(&self, operation_id: &str) {
        let recovery_restore_succeeded = {
            let mut state = self.0.state.lock().unwrap();
            let succeeded = if let Some(report) = state.restore_reports.get_mut(operation_id) {
                report.in_progress = false;
                report.finished_at_ms = Some(now_ms());
                restore_report_succeeded(report)
            } else {
                false
            };
            state.recovery_restore_operations.remove(operation_id) && succeeded
        };
        if recovery_restore_succeeded {
            self.complete_recovery_decision();
        }
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

    fn window_changes(&self, since: u64, run_id: &str) -> super::WindowChanges {
        let state = self.0.state.lock().unwrap();
        window_changes_since(&state, since, run_id)
    }

    fn restorable_windows(&self) -> Vec<TrackedWindow> {
        self.windows()
            .into_iter()
            .filter(is_history_worthy)
            .collect()
    }

    fn publish_change(state: &mut State, id: Option<&str>) {
        state.generation = state.generation.wrapping_add(1);
        if let Some(id) = id {
            state
                .window_changes
                .insert(id.to_string(), state.generation);
        }
        if id.is_none() || state.window_changes.len() > 4096 {
            state.window_changes.clear();
            state.changes_floor = state.generation;
        }
    }

    fn mark_durable(state: &mut State, id: Option<&str>) {
        state.durable_generation = state.durable_generation.wrapping_add(1);
        if let Some(id) = id {
            state
                .dirty_window_ids
                .insert(id.to_string(), state.durable_generation);
        } else {
            state.full_persist_generation = Some(state.durable_generation);
        }
        if state.dirty_window_ids.len() > 4096 {
            state.dirty_window_ids.clear();
            state.full_persist_generation = Some(state.durable_generation);
        }
        state.recovery_dirty = true;
        state
            .recovery_due
            .get_or_insert_with(|| Instant::now() + Duration::from_secs(30));
        state.current_dirty = true;
        state
            .current_due
            .get_or_insert_with(|| Instant::now() + Duration::from_millis(500));
    }

    fn begin_mass_disappearance_check(
        state: &mut State,
        baseline_entries: Vec<(TrackedWindow, RestoreSpec)>,
        current_count: usize,
    ) -> Option<MassDisappearanceTrigger> {
        if state.recovery_pending
            || state.checkpoint_guarded
            || state.mass_disappearance.is_some()
            || !is_mass_disappearance(baseline_entries.len(), current_count)
        {
            return None;
        }

        state.recovery_check_sequence = state.recovery_check_sequence.wrapping_add(1);
        let token = state.recovery_check_sequence;
        state.checkpoint_guarded = true;
        state.mass_disappearance = Some(MassDisappearanceCheck {
            token,
            baseline_count: baseline_entries.len(),
            baseline_entries: baseline_entries.clone(),
            attempts: 0,
        });
        Some(MassDisappearanceTrigger {
            token,
            baseline_entries,
        })
    }

    fn set_recovery_metadata(&self, pending: bool, checking: bool) -> Result<(), String> {
        let database = self.0.database.lock().unwrap();
        database.set_meta(
            RECOVERY_PENDING_META,
            if pending { "true" } else { "false" },
        )?;
        database.set_meta(
            RECOVERY_CHECK_PENDING_META,
            if checking { "true" } else { "false" },
        )
    }

    fn complete_recovery_decision(&self) {
        {
            let mut state = self.0.state.lock().unwrap();
            state.recovery_pending = false;
            state.checkpoint_guarded = false;
            state.mass_disappearance = None;
            state.removal_burst = None;
            state.recovery_dirty = true;
            state
                .recovery_due
                .get_or_insert_with(|| Instant::now() + Duration::from_secs(30));
        }
        if let Err(err) = self.set_recovery_metadata(false, false) {
            eprintln!("Tracker failed to persist recovery decision: {err}");
        }
    }

    fn persist_mass_disappearance_candidate(&self, trigger: MassDisappearanceTrigger) {
        let is_current = self
            .0
            .state
            .lock()
            .unwrap()
            .mass_disappearance
            .as_ref()
            .is_some_and(|check| check.token == trigger.token);
        if !is_current {
            return;
        }

        if let Err(err) = self.set_recovery_metadata(false, true) {
            eprintln!("Tracker failed to mark recovery check in progress: {err}");
        }
        let boot_id = self.0.state.lock().unwrap().boot_id.clone();
        let result = self
            .0
            .database
            .lock()
            .unwrap()
            .create_snapshot_with_entries(
                None,
                RECOVERY_KIND,
                &boot_id,
                &trigger.baseline_entries,
                now_ms(),
            );
        let candidate_written = match result {
            Ok(_) => {
                crate::observability::increment(crate::observability::Counter::PersistenceWrites);
                crate::observability::record_boundary(
                    crate::replay::BoundaryKind::TrackerMutation,
                    "recovery-candidate-written",
                    &serde_json::json!({
                        "token": trigger.token,
                        "window_count": trigger.baseline_entries.len(),
                    })
                    .to_string(),
                );
                true
            }
            Err(err) => {
                eprintln!("Tracker failed to persist recovery candidate: {err}");
                false
            }
        };
        if candidate_written {
            self.schedule_mass_disappearance_confirmation(trigger.token);
        } else {
            let runtime = self.clone();
            crate::observability::spawn_named("tracker-recovery-retry", move |worker| {
                worker.set_state("waiting");
                std::thread::sleep(Duration::from_secs(1));
                worker.set_state("retrying");
                runtime.persist_mass_disappearance_candidate(trigger);
            });
        }
    }

    fn schedule_mass_disappearance_confirmation(&self, token: u64) {
        let runtime = self.clone();
        crate::observability::spawn_named("tracker-mass-confirm", move |worker| {
            worker.set_state("waiting");
            std::thread::sleep(MASS_DISAPPEARANCE_CONFIRM_DELAY);
            worker.set_state("checking");
            runtime.confirm_mass_disappearance(token);
        });
    }

    fn kwin_baseline_existing_count(&self, ids: &[String]) -> Result<usize, String> {
        let connection = zbus::blocking::connection::Builder::session()
            .map_err(|err| err.to_string())?
            .method_timeout(KWIN_RECONCILIATION_TIMEOUT)
            .build()
            .map_err(|err| err.to_string())?;
        let proxy =
            zbus::blocking::Proxy::new(&connection, KWIN_SERVICE, KWIN_PATH, KWIN_INTERFACE)
                .map_err(|err| err.to_string())?;
        let mut existing = 0;
        for id in ids {
            let details: HashMap<String, zbus::zvariant::OwnedValue> = proxy
                .call("getWindowInfo", &(id.as_str(),))
                .map_err(|err| format!("KWin rejected recovery verification for {id}: {err}"))?;
            if !details.is_empty() {
                existing += 1;
            }
        }
        Ok(existing)
    }

    fn confirm_mass_disappearance(&self, token: u64) {
        let (baseline_count, baseline_ids, current_count, attempts) = {
            let state = self.0.state.lock().unwrap();
            let Some(check) = state
                .mass_disappearance
                .as_ref()
                .filter(|check| check.token == token)
            else {
                return;
            };
            (
                check.baseline_count,
                check
                    .baseline_entries
                    .iter()
                    .map(|(window, _)| window.id.clone())
                    .collect::<Vec<_>>(),
                state.windows.len(),
                check.attempts,
            )
        };

        if !is_mass_disappearance(baseline_count, current_count) {
            let mut state = self.0.state.lock().unwrap();
            if state
                .mass_disappearance
                .as_ref()
                .is_some_and(|check| check.token == token)
            {
                state.mass_disappearance = None;
                state.checkpoint_guarded = false;
                state.recovery_dirty = true;
                state
                    .recovery_due
                    .get_or_insert_with(|| Instant::now() + Duration::from_secs(30));
            }
            drop(state);
            if let Err(err) = self.set_recovery_metadata(false, false) {
                eprintln!("Tracker failed to clear recovery check: {err}");
            }
            return;
        }

        let kwin_result = self.kwin_baseline_existing_count(&baseline_ids);
        let confirm = match kwin_result {
            Ok(existing) => {
                existing <= baseline_count / 2
                    || attempts >= MASS_DISAPPEARANCE_MAX_CONFIRM_ATTEMPTS
            }
            Err(err) => {
                eprintln!("Tracker recovery verification unavailable: {err}");
                attempts >= MASS_DISAPPEARANCE_MAX_CONFIRM_ATTEMPTS
            }
        };
        if !confirm {
            let mut state = self.0.state.lock().unwrap();
            if let Some(check) = state
                .mass_disappearance
                .as_mut()
                .filter(|check| check.token == token)
            {
                check.attempts = check.attempts.saturating_add(1);
            } else {
                return;
            }
            drop(state);
            self.schedule_mass_disappearance_confirmation(token);
            return;
        }

        let mut state = self.0.state.lock().unwrap();
        if state
            .mass_disappearance
            .as_ref()
            .is_some_and(|check| check.token == token)
        {
            state.mass_disappearance = None;
            state.recovery_pending = true;
            state.checkpoint_guarded = true;
        }
        drop(state);
        if let Err(err) = self.set_recovery_metadata(true, false) {
            eprintln!("Tracker failed to persist recovery prompt state: {err}");
        }
        crate::observability::record_boundary(
            crate::replay::BoundaryKind::TrackerMutation,
            "recovery-candidate-confirmed",
            &serde_json::json!({
                "token": token,
                "window_count": baseline_count,
            })
            .to_string(),
        );
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
        let cached_restore = {
            let state = self.0.state.lock().unwrap();
            reusable_restore_spec(&state, &incoming, Instant::now())
        };
        let restore_refreshed = cached_restore.is_none();
        let restore = cached_restore.unwrap_or_else(|| super::infer_restore_spec(&incoming));
        let mut state = self.0.state.lock().unwrap();
        Self::expire_snapshot_if_needed(&mut state);
        if state.snapshot_buffer.is_none() && state.mass_disappearance.is_none() {
            state.removal_burst = None;
        }
        let previous = state
            .snapshot_buffer
            .as_ref()
            .and_then(|buffer| buffer.get(&incoming.id))
            .or_else(|| state.windows.get(&incoming.id))
            .cloned();
        retain_normal_geometry(&mut incoming, previous.as_ref());
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
        let durable_changed = previous
            .as_ref()
            .is_none_or(|previous| !super::database::persistence_equivalent(previous, &incoming))
            || state.restore_specs.get(&incoming.id) != Some(&restore);
        let changed_id = incoming.id.clone();
        if restore_refreshed {
            state
                .restore_spec_checked_at
                .insert(changed_id.clone(), Instant::now());
        }
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
            Self::publish_change(&mut state, Some(&changed_id));
        }
        if state.snapshot_buffer.is_none() && durable_changed {
            Self::mark_durable(&mut state, Some(&changed_id));
        } else if state.snapshot_buffer.is_some() {
            state.snapshot_durable_changed |= durable_changed;
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
        let now = Instant::now();
        let intentional_restart = state.terminal_upgrade_windows.contains(id);
        let removal_burst = state
            .removal_burst
            .take()
            .filter(|burst| now.duration_since(burst.started) <= MASS_REMOVAL_WINDOW)
            .unwrap_or_else(|| RemovalBurst {
                started: now,
                baseline_entries: state_snapshot_entries(&state),
            });
        let removed = state.windows.remove(id);
        state.terminal_upgrade_windows.remove(id);
        state.terminal_upgrade_closed.remove(id);
        state.attention.remove(id);
        let restore = state.restore_specs.remove(id);
        state.restore_spec_checked_at.remove(id);
        let mut mass_trigger = None;
        if removed.is_some() {
            state.history_generation = state.history_generation.wrapping_add(1);
            Self::publish_change(&mut state, Some(id));
            Self::mark_durable(&mut state, Some(id));
            if !intentional_restart
                && is_mass_disappearance(removal_burst.baseline_entries.len(), state.windows.len())
            {
                let current_count = state.windows.len();
                mass_trigger = Self::begin_mass_disappearance_check(
                    &mut state,
                    removal_burst.baseline_entries.clone(),
                    current_count,
                );
            }
            if !intentional_restart && mass_trigger.is_none() {
                state.removal_burst = Some(removal_burst);
            }
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
            && !intentional_restart
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
        if let Some(trigger) = mass_trigger {
            self.persist_mass_disappearance_candidate(trigger);
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
        let display_changed = state.windows.len() != buffer.len()
            || buffer.iter().any(|(id, window)| {
                state
                    .windows
                    .get(id)
                    .is_none_or(|old| tracked_window_state_changed(old, window))
            });
        let durable_changed = state.snapshot_durable_changed
            || state.windows.len() != buffer.len()
            || buffer.iter().any(|(id, window)| {
                state
                    .windows
                    .get(id)
                    .is_none_or(|old| !super::database::persistence_equivalent(old, window))
            });
        state.snapshot_durable_changed = false;
        let baseline_entries = state_snapshot_entries(&state);
        let expected_removals = baseline_entries
            .iter()
            .filter(|(window, _)| {
                state.terminal_upgrade_windows.contains(&window.id)
                    && !buffer.contains_key(&window.id)
            })
            .count();
        let mass_trigger = Self::begin_mass_disappearance_check(
            &mut state,
            baseline_entries,
            buffer.len() + expected_removals,
        );
        state.removal_burst = None;
        for (id, window) in &state.windows {
            if !buffer.contains_key(id) && !state.terminal_upgrade_windows.contains(id) {
                closed.push(window.clone());
            }
        }
        state.windows = buffer;
        let live_ids = state.windows.keys().cloned().collect::<HashSet<_>>();
        state
            .terminal_upgrade_windows
            .retain(|id| live_ids.contains(id));
        state
            .terminal_upgrade_closed
            .retain(|id| live_ids.contains(id));
        state.initial_snapshot_received = true;
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
        state.restore_specs.retain(|id, _| live_ids.contains(id));
        state
            .restore_spec_checked_at
            .retain(|id, _| live_ids.contains(id));
        if !closed.is_empty() {
            state.history_generation = state.history_generation.wrapping_add(1);
        }
        if display_changed {
            Self::publish_change(&mut state, None);
        }
        if durable_changed {
            Self::mark_durable(&mut state, None);
        }
        crate::observability::set_gauge(
            crate::observability::Gauge::TrackedWindows,
            state.windows.len(),
        );
        crate::observability::set_gauge(
            crate::observability::Gauge::AttentionPending,
            state.attention.len(),
        );
        let snapshot_generation = state.history_generation;
        let window_count = state.windows.len();
        drop(state);
        crate::observability::record_boundary(
            crate::replay::BoundaryKind::WindowFeed,
            "snapshot-applied",
            &serde_json::json!({
                "window_count": window_count,
                "generation": snapshot_generation,
                "closed_count": closed.len(),
            })
            .to_string(),
        );
        crate::observability::record_boundary(
            crate::replay::BoundaryKind::TrackerMutation,
            "history-write",
            &serde_json::json!({
                "closed_count": closed.len(),
                "generation": snapshot_generation,
            })
            .to_string(),
        );
        let database = self.0.database.lock().unwrap();
        for window in closed {
            if is_history_worthy(&window) {
                let operation_id = crate::observability::next_operation_id();
                let result = if crate::replay::environment_should_inject(
                    crate::replay::FaultPoint::TrackerWriteBusy,
                    operation_id,
                ) {
                    crate::observability::record_boundary(
                        crate::replay::BoundaryKind::FaultInjection,
                        "tracker-write-busy",
                        &serde_json::json!({ "operation_id": operation_id }).to_string(),
                    );
                    Err("fault injection: tracker write busy".to_string())
                } else {
                    database.add_history_with_restore(
                        &window,
                        closed_restores
                            .get(&window.id)
                            .unwrap_or(&super::infer_restore_spec(&window)),
                        timestamp,
                    )
                };
                if let Err(err) = result {
                    eprintln!("Tracker failed to append snapshot closure: {err}");
                }
            }
        }
        drop(database);
        if let Some(trigger) = mass_trigger {
            self.persist_mass_disappearance_candidate(trigger);
        }
        if let Err(err) = self.persist_current_if_due(true) {
            eprintln!("Tracker failed to persist current windows: {err}");
        }
    }

    fn write_recovery_if_due(&self, force: bool) -> Result<(), String> {
        let (mut windows, boot_id, generation) = {
            let mut state = self.0.state.lock().unwrap();
            if state.recovery_pending || state.checkpoint_guarded {
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
                state_snapshot_entries(&state),
                state.boot_id.clone(),
                state.durable_generation,
            )
        };
        super::tmux::refresh_snapshot_panes(&mut windows);
        let result = self
            .0
            .database
            .lock()
            .unwrap()
            .create_snapshot_with_entries(None, RECOVERY_KIND, &boot_id, &windows, now_ms());
        let mut state = self.0.state.lock().unwrap();
        state.recovery_write_in_flight = false;
        match result {
            Ok(_) => {
                crate::observability::increment(crate::observability::Counter::PersistenceWrites);
                if state.durable_generation == generation {
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

    fn write_periodic_if_due(&self, now: Instant, timestamp_ms: i64) -> Result<(), String> {
        let (mut windows, boot_id) = {
            let mut state = self.0.state.lock().unwrap();
            if state.periodic_write_in_flight || state.periodic_due.is_some_and(|due| now < due) {
                return Ok(());
            }
            // One archive per five minutes; incomplete/empty feeds, active restores and
            // write failures retry at most every 30 seconds without catch-up writes.
            state.periodic_due = Some(now + PERIODIC_SNAPSHOT_RETRY_DELAY);
            if !state.initial_snapshot_received
                || state.snapshot_buffer.is_some()
                || !state.restore_claims.is_empty()
            {
                return Ok(());
            }
            let windows = state_snapshot_entries(&state);
            if windows.is_empty() {
                return Ok(());
            }
            state.periodic_write_in_flight = true;
            (windows, state.boot_id.clone())
        };
        super::tmux::refresh_snapshot_panes(&mut windows);
        let result = self
            .0
            .database
            .lock()
            .unwrap()
            .create_periodic_snapshot_if_due(&boot_id, &windows, timestamp_ms);
        let mut state = self.0.state.lock().unwrap();
        state.periodic_write_in_flight = false;
        match result {
            Ok(Some(id)) => {
                state.periodic_due =
                    Some(now + Duration::from_millis(PERIODIC_SNAPSHOT_INTERVAL_MS as u64));
                state.history_generation = state.history_generation.wrapping_add(1);
                crate::observability::increment(crate::observability::Counter::PersistenceWrites);
                crate::observability::record(
                    crate::observability::Event::new("tracker", "periodic-snapshot-saved")
                        .object(&id.to_string())
                        .reason("retention-120-five-minute-plus-120-hourly"),
                );
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(err) => Err(err),
        }
    }

    fn persist_current_if_due(&self, force: bool) -> Result<(), String> {
        let (generation, full, entries, removed) = {
            let mut state = self.0.state.lock().unwrap();
            if state.current_write_in_flight
                || !state.current_dirty
                || (!force && state.current_due.is_none_or(|due| Instant::now() < due))
            {
                return Ok(());
            }
            state.current_write_in_flight = true;
            let full = state.full_persist_generation.is_some();
            let entries = state
                .windows
                .values()
                .filter(|window| full || state.dirty_window_ids.contains_key(&window.id))
                .map(|window| {
                    (
                        window.clone(),
                        state
                            .restore_specs
                            .get(&window.id)
                            .cloned()
                            .unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>();
            let removed = state
                .dirty_window_ids
                .keys()
                .filter(|id| !state.windows.contains_key(*id))
                .cloned()
                .collect::<Vec<_>>();
            (state.durable_generation, full, entries, removed)
        };
        let result = {
            let mut database = self.0.database.lock().unwrap();
            if full {
                database.replace_current_with_restore(&entries)
            } else {
                database.patch_current_with_restore(&entries, &removed)
            }
        };
        let mut state = self.0.state.lock().unwrap();
        state.current_write_in_flight = false;
        match result {
            Ok(()) => {
                crate::observability::increment(crate::observability::Counter::PersistenceWrites);
                state
                    .dirty_window_ids
                    .retain(|_, revision| *revision > generation);
                if state
                    .full_persist_generation
                    .is_some_and(|revision| revision <= generation)
                {
                    state.full_persist_generation = None;
                }
                if state.durable_generation == generation {
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
                windows,
                attention,
                terminal_upgrade_windows,
                ..
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
                    (attention.due <= now && !terminal_upgrade_windows.contains(id))
                        .then_some(window.clone())
                })
                .collect::<Vec<_>>()
        };
        for window in targets {
            crate::observability::increment(crate::observability::Counter::AttentionAttempts);
            let result = send_enter_to_terminal(&window);
            crate::observability::record_boundary(
                crate::replay::BoundaryKind::Attention,
                "send-enter",
                &serde_json::json!({
                    "window_id": window.id,
                    "succeeded": result.is_ok(),
                    "error": result.as_ref().err().map(ToString::to_string),
                })
                .to_string(),
            );
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
        pending: Vec<super::restore::PendingRestore>,
        history_id: Option<i64>,
        baseline_ids: HashSet<String>,
        claim: Option<String>,
        operation_id: String,
    ) {
        let runtime = self.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let deadline = Instant::now() + Duration::from_secs(20);
            let outputs = super::restore::current_output_geometries();
            let mut progress = HashMap::<usize, LayoutProgress>::new();
            let mut matched_ids = HashMap::<usize, String>::new();
            let mut completed = HashSet::<usize>::new();
            loop {
                let current = runtime.windows();
                let now = Instant::now();
                let used_ids = matched_ids.values().cloned().collect::<HashSet<_>>();
                let waiting_indices = (0..pending.len())
                    .filter(|index| !matched_ids.contains_key(index))
                    .collect::<Vec<_>>();
                let waiting = waiting_indices
                    .iter()
                    .map(|index| pending[*index].clone())
                    .collect::<Vec<_>>();
                for assignment in super::restore::assign_pending_windows(
                    &waiting,
                    &current,
                    &baseline_ids,
                    &used_ids,
                ) {
                    let pending_index = waiting_indices[assignment.pending_index];
                    let item = &pending[pending_index];
                    let target = super::restore::layout_target(&item.wanted, &outputs);
                    let apply_result = super::restore::apply_layout_once(
                        &assignment.window_id,
                        &item.wanted,
                        &target,
                    );
                    let last_mismatch = apply_result.err();
                    runtime.update_restore_outcome(
                        &operation_id,
                        item.outcome_index,
                        Some(assignment.window_id.clone()),
                        super::RestoreOutcomeStatus::Pending,
                        "Matched a KWin window and applied its saved layout; verification is pending"
                            .into(),
                    );
                    matched_ids.insert(pending_index, assignment.window_id.clone());
                    progress.insert(
                        pending_index,
                        LayoutProgress {
                            current_window_id: assignment.window_id,
                            target,
                            attempts: 1,
                            exact_observations: 0,
                            verify_after: now + LAYOUT_VERIFY_INITIAL_DELAY,
                            ambiguous_match: assignment.ambiguous,
                            last_mismatch,
                        },
                    );
                }

                let due = progress
                    .iter()
                    .filter(|(index, state)| {
                        !completed.contains(index) && now >= state.verify_after
                    })
                    .map(|(index, _)| *index)
                    .collect::<Vec<_>>();
                for pending_index in due {
                    let item = &pending[pending_index];
                    let Some(state) = progress.get_mut(&pending_index) else {
                        continue;
                    };
                    let Some(actual) = current
                        .iter()
                        .find(|window| window.id == state.current_window_id)
                    else {
                        state.last_mismatch = Some("The matched KWin window disappeared".into());
                        if now < deadline {
                            matched_ids.remove(&pending_index);
                            progress.remove(&pending_index);
                        }
                        continue;
                    };
                    match super::restore::verify_layout(actual, &item.wanted, &state.target) {
                        Ok(()) if state.exact_observations == 0 => {
                            state.exact_observations = 1;
                            state.verify_after = now + LAYOUT_VERIFY_CONFIRM_DELAY;
                        }
                        Ok(()) => {
                            let mut qualifications = Vec::new();
                            if let Some(note) = &item.partial_note {
                                qualifications.push(note.clone());
                            }
                            if let Some(adjustment) = state.target.adjustment.clone() {
                                qualifications.push(adjustment);
                            }
                            if state.ambiguous_match {
                                qualifications.push(
                                    "multiple windows had equally strong identity metadata".into(),
                                );
                            }
                            if !item.wanted.activities.is_empty() {
                                qualifications.push(
                                    "Plasma activity membership was recorded but cannot be applied by kdotool"
                                        .into(),
                                );
                            }
                            if item.wanted.skip_switcher {
                                qualifications.push(
                                    "skip-switcher state was recorded but cannot be applied by kdotool"
                                        .into(),
                                );
                            }
                            let status = if qualifications.is_empty() {
                                super::RestoreOutcomeStatus::Exact
                            } else {
                                super::RestoreOutcomeStatus::Adjusted
                            };
                            let mut detail = format!(
                                "Verified geometry and supported window state after {} layout attempt{}",
                                state.attempts,
                                if state.attempts == 1 { "" } else { "s" }
                            );
                            if !qualifications.is_empty() {
                                detail.push_str(": ");
                                detail.push_str(&qualifications.join("; "));
                            }
                            runtime.update_restore_outcome(
                                &operation_id,
                                item.outcome_index,
                                Some(state.current_window_id.clone()),
                                status,
                                detail,
                            );
                            completed.insert(pending_index);
                        }
                        Err(mismatch) if state.attempts < LAYOUT_APPLY_ATTEMPTS => {
                            state.last_mismatch = Some(mismatch);
                            state.attempts += 1;
                            if let Err(err) = super::restore::apply_layout_once(
                                &state.current_window_id,
                                &item.wanted,
                                &state.target,
                            ) {
                                state.last_mismatch = Some(err);
                            }
                            state.exact_observations = 0;
                            state.verify_after = now
                                + if state.attempts == LAYOUT_APPLY_ATTEMPTS {
                                    LAYOUT_VERIFY_FINAL_DELAY
                                } else {
                                    LAYOUT_VERIFY_CONFIRM_DELAY
                                };
                        }
                        Err(mismatch) => {
                            state.last_mismatch = Some(mismatch);
                            let detail = format!(
                                "Window appeared, but its application or KWin retained a different layout after {} attempts: {}",
                                state.attempts,
                                state.last_mismatch.as_deref().unwrap_or("unknown mismatch")
                            );
                            runtime.add_restore_failure(
                                &operation_id,
                                format!("{}: {detail}", item.wanted.title),
                            );
                            runtime.update_restore_outcome(
                                &operation_id,
                                item.outcome_index,
                                Some(state.current_window_id.clone()),
                                super::RestoreOutcomeStatus::Failed,
                                detail,
                            );
                            completed.insert(pending_index);
                        }
                    }
                }

                if completed.len() == pending.len() {
                    break;
                }
                if now >= deadline {
                    for (pending_index, item) in pending.iter().enumerate() {
                        if completed.contains(&pending_index) {
                            continue;
                        }
                        let detail = progress
                            .get(&pending_index)
                            .and_then(|state| state.last_mismatch.clone())
                            .map(|mismatch| format!("Layout verification timed out: {mismatch}"))
                            .unwrap_or_else(|| {
                                if item.launched {
                                    "No new matching KWin window appeared within 20 seconds".into()
                                } else {
                                    "The matched KWin window was unavailable during reconciliation"
                                        .into()
                                }
                            });
                        runtime.add_restore_failure(
                            &operation_id,
                            format!("{}: {detail}", item.wanted.title),
                        );
                        runtime.update_restore_outcome(
                            &operation_id,
                            item.outcome_index,
                            matched_ids.get(&pending_index).cloned(),
                            super::RestoreOutcomeStatus::Failed,
                            detail,
                        );
                    }
                    break;
                }
                std::thread::sleep(layout_reconciliation_poll_interval(started.elapsed()));
            }

            let stacked = matched_ids
                .iter()
                .filter(|(index, _)| completed.contains(index))
                .map(|(index, id)| (pending[*index].wanted.clone(), id.clone()))
                .collect::<Vec<_>>();
            if let Err(err) = super::restore::restore_stacking(&stacked) {
                runtime.add_restore_failure(
                    &operation_id,
                    format!("Could not restore window stacking order: {err}"),
                );
            }

            let current_window_ids = runtime
                .0
                .state
                .lock()
                .unwrap()
                .windows
                .keys()
                .cloned()
                .collect::<HashSet<_>>();
            let all_windows_appeared = matched_ids.len() == pending.len()
                && matched_ids
                    .values()
                    .all(|window_id| current_window_ids.contains(window_id));
            if all_windows_appeared {
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
                        Err(err) => runtime.add_restore_failure(
                            &operation_id,
                            format!("Restored the window but could not remove history entry {history_id}: {err}"),
                        ),
                    }
                }
            } else if let Some(history_id) = history_id {
                runtime.defer_history_restore(history_id, "window_match_timeout");
            }
            if let Some(claim) = claim.as_deref() {
                runtime.release_restore(claim);
            }
            runtime.finish_restore_report(&operation_id);
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

fn restore_report_succeeded(report: &RestoreReport) -> bool {
    report.failures.is_empty()
        && report.outcomes.iter().all(|outcome| {
            matches!(
                outcome.status,
                super::RestoreOutcomeStatus::Exact | super::RestoreOutcomeStatus::Adjusted
            )
        })
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

fn retain_normal_geometry(incoming: &mut TrackedWindow, previous: Option<&TrackedWindow>) {
    if !incoming.maximized
        && !incoming.maximized_horizontally
        && !incoming.maximized_vertically
        && !incoming.fullscreen
        && incoming.width > 0
        && incoming.height > 0
    {
        incoming.normal_geometry = Some(super::WindowGeometry {
            x: incoming.x,
            y: incoming.y,
            width: incoming.width,
            height: incoming.height,
        });
    } else if incoming.normal_geometry.is_none() {
        incoming.normal_geometry = previous.and_then(|window| window.normal_geometry);
    }
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
    let operation_id = crate::observability::next_operation_id();
    if crate::replay::environment_should_inject(
        crate::replay::FaultPoint::TerminalSendFailure,
        operation_id,
    ) {
        let error = "fault injection: terminal SendEnter failure".to_string();
        crate::observability::record_boundary(
            crate::replay::BoundaryKind::FaultInjection,
            "terminal-send-failure",
            &serde_json::json!({ "operation_id": operation_id }).to_string(),
        );
        return Err(error);
    }
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
    let result = proxy
        .call::<_, _, ()>("SendEnter", &(tab_uuid.as_str(),))
        .map_err(|err| err.to_string());
    crate::observability::record_boundary(
        crate::replay::BoundaryKind::TerminalAction,
        "send-enter-dbus",
        &serde_json::json!({
            "window_id": window.id,
            "service": service,
            "succeeded": result.is_ok(),
            "error": result.as_ref().err(),
        })
        .to_string(),
    );
    result
}

#[derive(Clone)]
struct WindowFeed(Runtime);

#[interface(name = "com.terrydaktal.ApplicationLauncher.WindowFeed", spawn = false)]
impl WindowFeed {
    #[zbus(name = "BeginSnapshot")]
    fn begin_snapshot(&self) {
        let mut state = self.0.0.state.lock().unwrap();
        state.snapshot_buffer = Some(HashMap::new());
        state.snapshot_durable_changed = false;
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
        crate::observability::record_boundary(
            crate::replay::BoundaryKind::WindowFeed,
            "replace-snapshot",
            &serde_json::json!({
                "window_count": windows.len(),
                "generation": self.0.history_generation(),
            })
            .to_string(),
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

struct TerminalUpgradeProtection(Runtime);

impl Drop for TerminalUpgradeProtection {
    fn drop(&mut self) {
        let mut state = self.0.0.state.lock().unwrap();
        let retired = std::mem::take(&mut state.terminal_upgrade_closed);
        let live_ids = state.windows.keys().cloned().collect::<HashSet<_>>();
        state
            .terminal_upgrade_windows
            .retain(|id| retired.contains(id) && live_ids.contains(id));
        state.restore_claims.remove("terminal-upgrade");
        state.removal_burst = None;
    }
}

impl super::terminal_upgrade::UpgradeBackend for Runtime {
    fn windows(&self) -> Vec<TrackedWindow> {
        self.windows()
    }

    fn backup(
        &self,
        all_windows: &[TrackedWindow],
        targets: &[super::terminal_upgrade::UpgradeTarget],
    ) -> Result<i64, String> {
        let (boot, stored) = {
            let state = self.0.state.lock().unwrap();
            (state.boot_id.clone(), state.restore_specs.clone())
        };
        let mut entries = all_windows
            .iter()
            .map(|window| {
                let mut restore = stored
                    .get(&window.id)
                    .cloned()
                    .unwrap_or_else(|| super::infer_restore_spec(window));
                if let Some(target) = targets.iter().find(|target| target.window.id == window.id) {
                    restore.terminal_tabs = target.restore_tabs();
                }
                (window.clone(), restore)
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|(window, _)| window.stacking_order);
        self.0
            .database
            .lock()
            .unwrap()
            .create_terminal_upgrade_backup(&boot, &entries)
    }

    fn preflight(&self, target: &super::terminal_upgrade::UpgradeTarget) -> Result<(), String> {
        super::terminal_upgrade::preflight_target(target, &self.windows())
    }

    fn launch_and_verify(
        &self,
        target: &super::terminal_upgrade::UpgradeTarget,
    ) -> Result<String, String> {
        super::terminal_upgrade::launch_replacement(target, || self.windows())
    }

    fn close_and_wait(
        &self,
        target: &super::terminal_upgrade::UpgradeTarget,
    ) -> Result<(), String> {
        let result = super::terminal_upgrade::close_original(target, || self.windows());
        if result.is_ok() {
            self.0
                .state
                .lock()
                .unwrap()
                .terminal_upgrade_closed
                .insert(target.window.id.clone());
        }
        result
    }

    fn finish_layout(&self, windows: &[(TrackedWindow, String)]) -> Result<(), String> {
        super::restore::restore_stacking(windows)
    }
}

#[interface(name = "com.terrydaktal.ApplicationLauncher.Tracker1", spawn = false)]
impl TrackerApi {
    #[zbus(name = "BeginTerminalUpgradePreview")]
    fn begin_terminal_upgrade_preview(&self) -> String {
        let (id, preparation) = self.0.0.terminal_upgrades.lock().unwrap().begin();
        if let Some(preparation) = preparation {
            let runtime = self.0.clone();
            let _ = std::thread::Builder::new()
                .name("terminal-upgrade-preview".into())
                .spawn(move || {
                    let worker = crate::observability::register_worker("terminal-upgrade-preview");
                    worker.set_state("inspecting-terminals");
                    preparation.prepare(runtime.windows());
                });
        }
        serde_json::to_string(&Ok::<_, String>(id)).unwrap()
    }

    #[zbus(name = "GetTerminalUpgradeReport")]
    fn get_terminal_upgrade_report(&self, id: &str) -> String {
        serde_json::to_string(&self.0.0.terminal_upgrades.lock().unwrap().report(id)).unwrap()
    }

    #[zbus(name = "StartTerminalUpgrade")]
    fn start_terminal_upgrade(&self, id: &str, selected: Vec<String>, confirmed: bool) -> String {
        let result = (|| {
            let work = {
                let registry = self.0.0.terminal_upgrades.lock().unwrap();
                let mut state = self.0.0.state.lock().unwrap();
                if state
                    .restore_claims
                    .iter()
                    .any(|claim| claim != "terminal-upgrade")
                {
                    return Err(
                        "Another session restore is in progress; wait for it to finish".to_string(),
                    );
                }
                let work = registry.start(id, &selected, confirmed)?;
                if let Some(work) = &work {
                    state.terminal_upgrade_windows.extend(work.window_ids());
                    state.restore_claims.insert("terminal-upgrade".into());
                }
                work
            };
            if let Some(work) = work {
                let runtime = self.0.clone();
                let protection = TerminalUpgradeProtection(runtime.clone());
                std::thread::Builder::new()
                    .name("terminal-upgrade".into())
                    .spawn(move || {
                        let _protection = protection;
                        let worker = crate::observability::register_worker("terminal-upgrade");
                        worker.set_state("backing-up-and-restarting-terminals");
                        work.run(&runtime);
                    })
                    .map_err(|err| err.to_string())?;
            }
            self.0.0.terminal_upgrades.lock().unwrap().report(id)
        })();
        serde_json::to_string(&result).unwrap()
    }

    #[zbus(name = "GetStatus")]
    fn get_status(&self) -> String {
        serde_json::to_string(&self.0.status()).unwrap()
    }

    #[zbus(name = "GetWindows")]
    fn get_windows(&self) -> String {
        serde_json::to_string(&self.0.windows()).unwrap()
    }

    #[zbus(name = "GetWindowChanges")]
    fn get_window_changes(&self, since: u64, run_id: &str) -> String {
        serde_json::to_string(&self.0.window_changes(since, run_id)).unwrap()
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

    #[zbus(name = "GetRestoreReport")]
    fn get_restore_report(&self, operation_id: &str) -> String {
        serde_json::to_string(&self.0.restore_report(operation_id)).unwrap()
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
        if result.is_ok() {
            let mut state = self.0.0.state.lock().unwrap();
            state.history_generation = state.history_generation.wrapping_add(1);
        }
        serde_json::to_string(&result).unwrap()
    }

    #[zbus(name = "DeleteSnapshot")]
    fn delete_snapshot(&self, id: i64) -> String {
        let result = self.0.0.database.lock().unwrap().delete_snapshot(id);
        if result.is_ok() {
            let mut state = self.0.0.state.lock().unwrap();
            state.history_generation = state.history_generation.wrapping_add(1);
        }
        serde_json::to_string(&result).unwrap()
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
            let prepared =
                super::restore::prepare_restore_specs(&snapshot.windows, &self.0.windows());
            let report = self.0.register_restore_report(prepared.report);
            if !prepared.pending.is_empty() {
                self.0.schedule_layout_reconciliation(
                    prepared.pending,
                    None,
                    prepared.baseline_ids,
                    Some(claim),
                    report.operation_id.clone().unwrap(),
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
            let prepared =
                super::restore::prepare_restore_specs(&snapshot.windows, &self.0.windows());
            let report = self.0.register_restore_report(prepared.report);
            if !prepared.pending.is_empty() {
                self.0
                    .0
                    .state
                    .lock()
                    .unwrap()
                    .recovery_restore_operations
                    .insert(report.operation_id.clone().unwrap());
                self.0.schedule_layout_reconciliation(
                    prepared.pending,
                    None,
                    prepared.baseline_ids,
                    Some(claim),
                    report.operation_id.clone().unwrap(),
                );
            } else {
                self.0.release_restore(&claim);
                if restore_report_succeeded(&report) {
                    self.0.complete_recovery_decision();
                }
            }
            Ok(report)
        });
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
        self.0.complete_recovery_decision();
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
    let prepared = super::restore::prepare_restore_specs(
        &[(entry.window.clone(), entry.restore.clone())],
        &[],
    );
    let report = runtime.register_restore_report(prepared.report);
    if !prepared.pending.is_empty() {
        runtime.clear_history_restore_deferment(id);
        runtime.schedule_layout_reconciliation(
            prepared.pending,
            Some(id),
            baseline_ids,
            Some(claim),
            report.operation_id.clone().unwrap(),
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
            "tracker_restore event=reopen_latest_selected history_id={id} scanned={} excluded={} non_window={} unsupported_terminal={} missing_ssh_arguments={} missing_tmux_session={} missing_executable={} missing_desktop={} non_launchable_desktop={} prior_launch_failures={launch_failures}",
            stats.scanned,
            stats.excluded,
            stats.non_window,
            stats.unsupported_terminal,
            stats.missing_ssh_arguments,
            stats.missing_tmux_session,
            stats.missing_executable,
            stats.missing_desktop,
            stats.non_launchable_desktop,
        );
        return Ok(report);
    }

    eprintln!(
        "tracker_restore event=reopen_latest_unavailable scanned={} excluded={} non_window={} unsupported_terminal={} missing_ssh_arguments={} missing_tmux_session={} missing_executable={} missing_desktop={} non_launchable_desktop={} launch_failures={launch_failures}",
        stats.scanned,
        stats.excluded,
        stats.non_window,
        stats.unsupported_terminal,
        stats.missing_ssh_arguments,
        stats.missing_tmux_session,
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
    missing_ssh_arguments: usize,
    missing_tmux_session: usize,
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
                    super::restore::LaunchUnavailableCode::MissingSshArguments => {
                        stats.missing_ssh_arguments += 1;
                    }
                    super::restore::LaunchUnavailableCode::MissingTmuxSession => {
                        stats.missing_tmux_session += 1;
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
    let new_boot = !previous_boot.is_empty() && previous_boot != boot_id;
    let previous_clean = database.meta("clean_shutdown")?.as_deref() == Some("true");
    let recovery_snapshot_exists = database.snapshot_kind_exists(RECOVERY_KIND)?;
    let persisted_recovery_pending =
        database.meta(RECOVERY_PENDING_META)?.as_deref() == Some("true");
    let persisted_recovery_check =
        database.meta(RECOVERY_CHECK_PENDING_META)?.as_deref() == Some("true");
    let recovery_pending = should_offer_recovery(
        &previous_boot,
        &boot_id,
        previous_clean,
        persisted_recovery_pending,
        persisted_recovery_check,
        recovery_snapshot_exists,
    );
    let checkpoint_guarded = recovery_pending;
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
        .map(|(window, restore)| {
            (
                window.id.clone(),
                super::refresh_live_restore_spec(window, restore.clone()),
            )
        })
        .collect::<HashMap<_, _>>();
    let persisted_windows = persisted_entries
        .into_iter()
        .map(|(window, _)| (window.id.clone(), window))
        .collect::<HashMap<_, _>>();
    database.set_meta("boot_id", &boot_id)?;
    database.set_meta("clean_shutdown", "false")?;
    database.set_meta(
        RECOVERY_PENDING_META,
        if recovery_pending { "true" } else { "false" },
    )?;
    database.set_meta(RECOVERY_CHECK_PENDING_META, "false")?;
    database.set_meta("recovery_dismissed", "false")?;
    let run_id = run_id();
    database.set_meta("run_id", &run_id)?;
    let auto_enter_enabled = database.meta("auto_enter")?.as_deref() == Some("true");
    let periodic_due = Instant::now()
        + periodic_snapshot_delay(database.last_periodic_snapshot_at_ms()?, now_ms());

    let runtime = Runtime(Arc::new(RuntimeInner {
        state: Mutex::new(State {
            windows: persisted_windows,
            snapshot_buffer: None,
            snapshot_deadline: None,
            snapshot_durable_changed: false,
            generation: 0,
            window_changes: HashMap::new(),
            changes_floor: 0,
            durable_generation: 0,
            dirty_window_ids: HashMap::new(),
            full_persist_generation: None,
            history_generation: 0,
            activation_sequence,
            recovery_pending,
            checkpoint_guarded,
            mass_disappearance: None,
            removal_burst: None,
            recovery_check_sequence: 0,
            run_id,
            boot_id,
            recovery_dirty: false,
            recovery_due: None,
            current_dirty: false,
            current_due: None,
            recovery_write_in_flight: false,
            current_write_in_flight: false,
            initial_snapshot_received: false,
            periodic_due: Some(periodic_due),
            periodic_write_in_flight: false,
            auto_enter_enabled,
            attention: HashMap::new(),
            restore_specs,
            restore_spec_checked_at: HashMap::new(),
            restore_claims: HashSet::new(),
            history_restore_retry_after: HashMap::new(),
            restore_sequence: 0,
            restore_reports: HashMap::new(),
            restore_report_order: VecDeque::new(),
            recovery_restore_operations: HashSet::new(),
            terminal_upgrade_windows: HashSet::new(),
            terminal_upgrade_closed: HashSet::new(),
        }),
        database: Mutex::new(database),
        terminal_upgrades: Mutex::new(super::terminal_upgrade::UpgradeRegistry::default()),
    }));
    crate::observability::set_gauge(
        crate::observability::Gauge::TrackedWindows,
        runtime.0.state.lock().unwrap().windows.len(),
    );
    crate::observability::record(
        crate::observability::Event::new("tracker", "state-loaded")
            .reason(if new_boot { "new-boot" } else { "same-boot" })
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
                if let Err(err) = recovery_runtime.write_periodic_if_due(Instant::now(), now_ms()) {
                    eprintln!("Tracker failed to write periodic snapshot: {err}");
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
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::{
        ATTENTION_RECHECK_DELAY, AttentionState, PERIODIC_SNAPSHOT_RETRY_DELAY, Runtime,
        RuntimeInner, State, attention_retry_delay, first_launched_history_report,
        is_history_worthy, is_mass_disappearance, layout_reconciliation_poll_interval,
        normalized_terminal_title, reconcile_attention_states, record_attention_attempt,
        reopen_shortcut_action_id, reopenable_history_ids_with, restore_report_succeeded,
        retain_normal_geometry, should_offer_recovery, terminal_dbus_service_names,
        tracked_window_state_changed,
    };
    use crate::tracker::restore::LaunchUnavailableCode;
    use crate::tracker::{HistoryEntry, RestoreReport, RestoreSpec, TrackedWindow};

    #[test]
    fn restore_spec_cache_expires_without_postponement_and_rechecks_identity() {
        let now = Instant::now();
        let window = TrackedWindow {
            id: "terminal".into(),
            pid: 100,
            class: "xfce4-terminal".into(),
            title: "codex - \u{280b} project - Terminal".into(),
            ..Default::default()
        };
        let saved = RestoreSpec {
            terminal_kind: Some("codex".into()),
            cwd: Some("/project".into()),
            ..Default::default()
        };
        let state = State {
            windows: HashMap::from([(window.id.clone(), window.clone())]),
            restore_specs: HashMap::from([(window.id.clone(), saved.clone())]),
            restore_spec_checked_at: HashMap::from([(window.id.clone(), now)]),
            ..Default::default()
        };
        let mut changed = window.clone();
        changed.title = "codex - \u{2819} project - Terminal".into();
        for millis in [1, 100, 499] {
            assert_eq!(
                super::reusable_restore_spec(&state, &changed, now + Duration::from_millis(millis)),
                Some(saved.clone())
            );
        }
        assert!(
            super::reusable_restore_spec(&state, &changed, now + Duration::from_millis(500))
                .is_none()
        );
        changed.pid += 1;
        assert!(super::reusable_restore_spec(&state, &changed, now).is_none());
        changed = window.clone();
        changed.title = "codex - project - Terminal".into();
        assert!(super::reusable_restore_spec(&state, &changed, now).is_none());
        changed.title = "codex - \u{2819} other - Terminal".into();
        assert!(super::reusable_restore_spec(&state, &changed, now).is_none());
    }

    #[test]
    fn incremental_tracker_feed_coalesces_latest_state_and_removed_ids() {
        let mut state = State {
            run_id: "run-one".into(),
            ..Default::default()
        };
        state.windows.insert(
            "a".into(),
            TrackedWindow {
                id: "a".into(),
                title: "old".into(),
                ..Default::default()
            },
        );
        state.windows.insert(
            "b".into(),
            TrackedWindow {
                id: "b".into(),
                ..Default::default()
            },
        );
        assert!(super::window_changes_since(&state, u64::MAX, "").reset);
        for title in ["middle", "latest"] {
            state.windows.get_mut("a").unwrap().title = title.into();
            Runtime::publish_change(&mut state, Some("a"));
        }
        state.windows.remove("b");
        Runtime::publish_change(&mut state, Some("b"));
        let delta = super::window_changes_since(&state, 0, "run-one");
        assert!(!delta.reset);
        assert_eq!(delta.generation, 3);
        assert_eq!(delta.windows.len(), 1);
        assert_eq!(delta.windows[0].title, "latest");
        assert_eq!(delta.removed, ["b"]);
        let idle = super::window_changes_since(&state, 3, "run-one");
        assert!(idle.windows.is_empty());
        assert!(idle.removed.is_empty());
        assert!(!idle.reset);
        assert!(super::window_changes_since(&state, 3, "previous-run").reset);
        assert!(super::window_changes_since(&state, 4, "run-one").reset);
    }

    #[test]
    fn bounded_tracker_journal_and_dirty_set_fall_back_to_full_resynchronization() {
        let mut state = State {
            run_id: "run".into(),
            ..Default::default()
        };
        for index in 0..4097 {
            let id = index.to_string();
            Runtime::publish_change(&mut state, Some(&id));
            Runtime::mark_durable(&mut state, Some(&id));
        }
        assert!(state.window_changes.len() <= 4096);
        assert!(state.dirty_window_ids.len() <= 4096);
        assert!(state.full_persist_generation.is_some());
        let reset = super::window_changes_since(&state, 0, "run");
        assert!(reset.reset);
        assert!(reset.windows.is_empty());
        let baseline = state.generation;
        state.windows.insert(
            "new".into(),
            TrackedWindow {
                id: "new".into(),
                ..Default::default()
            },
        );
        Runtime::publish_change(&mut state, Some("new"));
        let delta = super::window_changes_since(&state, baseline, "run");
        assert!(!delta.reset);
        assert_eq!(delta.windows[0].id, "new");
    }

    #[test]
    fn delayed_incremental_consumers_receive_exact_end_state() {
        let mut state = State {
            run_id: "run".into(),
            ..Default::default()
        };
        let mut cursor = 0;
        let mut visible = HashMap::new();
        for sequence in 0..1000 {
            let id = (sequence % 17).to_string();
            if sequence % 3 == 0 {
                state.windows.remove(&id);
            } else {
                state.windows.insert(
                    id.clone(),
                    TrackedWindow {
                        id: id.clone(),
                        title: sequence.to_string(),
                        activation_sequence: sequence,
                        ..Default::default()
                    },
                );
            }
            Runtime::publish_change(&mut state, Some(&id));
            if sequence % 37 == 0 || sequence == 999 {
                let changes = super::window_changes_since(&state, cursor, "run");
                if changes.reset {
                    visible.clear();
                }
                for window in changes.windows {
                    visible.insert(window.id.clone(), window);
                }
                for id in changes.removed {
                    visible.remove(&id);
                }
                cursor = changes.generation;
                assert_eq!(visible, state.windows);
            }
        }
    }

    fn optimization_runtime() -> (Runtime, super::super::database::TestDatabaseDirectory) {
        let (database, directory) = super::super::database::test_database();
        (
            Runtime(std::sync::Arc::new(RuntimeInner {
                database: std::sync::Mutex::new(database),
                terminal_upgrades: std::sync::Mutex::new(
                    super::super::terminal_upgrade::UpgradeRegistry::default(),
                ),
                state: std::sync::Mutex::new(State {
                    run_id: "optimization-test".into(),
                    checkpoint_guarded: true,
                    ..Default::default()
                }),
            })),
            directory,
        )
    }

    #[test]
    fn transient_tracker_frames_publish_without_scheduling_database_writes() {
        let (runtime, _directory) = optimization_runtime();
        let mut window = TrackedWindow {
            id: "spinner".into(),
            pid: -1,
            class: "xfce4-terminal".into(),
            title: "codex - \u{280b} /project - Terminal".into(),
            width: 800,
            height: 600,
            ..Default::default()
        };
        runtime.upsert(window.clone(), false);
        runtime.persist_current_if_due(true).unwrap();
        let before = {
            let state = runtime.0.state.lock().unwrap();
            assert!(!state.current_dirty);
            (state.generation, state.durable_generation)
        };
        let stored_title = window.title.clone();
        for frame in 0..100 {
            window.title = format!(
                "codex - {} /project - Terminal",
                if frame % 2 == 0 {
                    '\u{2819}'
                } else {
                    '\u{2807}'
                }
            );
            runtime.upsert(window.clone(), false);
        }
        {
            let state = runtime.0.state.lock().unwrap();
            assert_eq!(state.generation, before.0 + 100);
            assert_eq!(state.durable_generation, before.1);
            assert!(!state.current_dirty);
            assert!(state.current_due.is_none());
            assert!(state.dirty_window_ids.is_empty());
        }
        runtime.persist_current_if_due(true).unwrap();
        assert_eq!(runtime.windows()[0].title, window.title);
        assert_eq!(
            runtime
                .0
                .database
                .lock()
                .unwrap()
                .current_window_entries()
                .unwrap()[0]
                .0
                .title,
            stored_title
        );
    }

    struct LegacyTrackerFixture {
        runtime: Runtime,
        reads: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[zbus::interface(name = "com.terrydaktal.ApplicationLauncher.Tracker1")]
    impl LegacyTrackerFixture {
        fn get_status(&self) -> String {
            serde_json::to_string(&self.runtime.status()).unwrap()
        }

        fn get_windows(&self) -> String {
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            serde_json::to_string(&self.runtime.windows()).unwrap()
        }
    }

    #[test]
    #[ignore = "run only on a private dbus-run-session with APPLICATIONLAUNCHER_TEST_PRIVATE_BUS=1"]
    fn private_bus_incremental_feed_and_legacy_fallback_preserve_end_state() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        assert_eq!(
            std::env::var("APPLICATIONLAUNCHER_TEST_PRIVATE_BUS").as_deref(),
            Ok("1")
        );
        let (runtime, _directory) = optimization_runtime();
        {
            let mut state = runtime.0.state.lock().unwrap();
            for id in ["a", "b"] {
                state.windows.insert(
                    id.into(),
                    TrackedWindow {
                        id: id.into(),
                        title: "initial".into(),
                        ..Default::default()
                    },
                );
                Runtime::publish_change(&mut state, Some(id));
            }
        }
        let server = zbus::blocking::connection::Builder::session()
            .unwrap()
            .name(crate::tracker::SERVICE_NAME)
            .unwrap()
            .serve_at(
                crate::tracker::TRACKER_PATH,
                super::TrackerApi(runtime.clone()),
            )
            .unwrap()
            .build()
            .unwrap();
        let client = crate::tracker::TrackerClient::connect().unwrap();
        let initial = client.window_changes(u64::MAX, "").unwrap();
        assert!(initial.reset);
        assert_eq!(initial.windows.len(), 2);
        {
            let mut state = runtime.0.state.lock().unwrap();
            state.windows.get_mut("a").unwrap().title = "latest".into();
            Runtime::publish_change(&mut state, Some("a"));
            state.windows.remove("b");
            Runtime::publish_change(&mut state, Some("b"));
        }
        let delta = client
            .window_changes(initial.generation, &initial.run_id)
            .unwrap();
        assert!(!delta.reset);
        assert_eq!(delta.windows.len(), 1);
        assert_eq!(delta.windows[0].title, "latest");
        assert_eq!(delta.removed, ["b"]);
        let idle = client
            .window_changes(delta.generation, &delta.run_id)
            .unwrap();
        assert!(!idle.reset);
        assert!(idle.windows.is_empty());
        assert!(idle.removed.is_empty());
        drop(client);
        server.release_name(crate::tracker::SERVICE_NAME).unwrap();
        drop(server);

        let reads = Arc::new(AtomicUsize::new(0));
        let _legacy_server = zbus::blocking::connection::Builder::session()
            .unwrap()
            .name(crate::tracker::SERVICE_NAME)
            .unwrap()
            .serve_at(
                crate::tracker::TRACKER_PATH,
                LegacyTrackerFixture {
                    runtime: runtime.clone(),
                    reads: Arc::clone(&reads),
                },
            )
            .unwrap()
            .build()
            .unwrap();
        let client = crate::tracker::TrackerClient::connect().unwrap();
        let full = client.window_changes(u64::MAX, "").unwrap();
        assert!(full.reset);
        assert_eq!(full.windows, runtime.windows());
        assert_eq!(reads.load(Ordering::Relaxed), 1);
        let idle = client
            .window_changes(full.generation, &full.run_id)
            .unwrap();
        assert!(!idle.reset);
        assert!(idle.windows.is_empty());
        assert_eq!(reads.load(Ordering::Relaxed), 1);
        {
            let mut state = runtime.0.state.lock().unwrap();
            state.windows.get_mut("a").unwrap().title = "legacy latest".into();
            Runtime::publish_change(&mut state, Some("a"));
        }
        let full = client
            .window_changes(idle.generation, &idle.run_id)
            .unwrap();
        assert!(full.reset);
        assert_eq!(full.windows, runtime.windows());
        assert_eq!(reads.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn persistence_in_flight_retains_newer_dirty_revisions() {
        let (runtime, _directory) = optimization_runtime();
        let window = TrackedWindow {
            id: "window".into(),
            title: "Editor".into(),
            x: 10,
            ..Default::default()
        };
        {
            let mut state = runtime.0.state.lock().unwrap();
            state.windows.insert(window.id.clone(), window);
            Runtime::mark_durable(&mut state, Some("window"));
        }
        let database = runtime.0.database.lock().unwrap();
        let worker_runtime = runtime.clone();
        let worker =
            std::thread::spawn(move || worker_runtime.persist_current_if_due(true).unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if runtime.0.state.lock().unwrap().current_write_in_flight {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        {
            let mut state = runtime.0.state.lock().unwrap();
            let window = state.windows.get_mut("window").unwrap();
            window.x = 500;
            window.last_activated_at_ms = Some(123456);
            Runtime::mark_durable(&mut state, Some("window"));
        }
        drop(database);
        worker.join().unwrap();
        assert!(
            runtime
                .0
                .state
                .lock()
                .unwrap()
                .dirty_window_ids
                .contains_key("window")
        );
        runtime.persist_current_if_due(true).unwrap();
        let saved = runtime
            .0
            .database
            .lock()
            .unwrap()
            .current_window_entries()
            .unwrap();
        assert_eq!(saved[0].0.x, 500);
        assert_eq!(saved[0].0.last_activated_at_ms, Some(123456));
        assert!(!runtime.0.state.lock().unwrap().current_dirty);
    }

    #[test]
    fn history_generation_tracks_manual_snapshot_creation_and_deletion() {
        let (runtime, _directory) = optimization_runtime();
        let api = super::TrackerApi(runtime.clone());
        let before = runtime.history_generation();
        let id: Result<i64, String> =
            serde_json::from_str(&api.create_snapshot("Test snapshot")).unwrap();
        let id = id.unwrap();
        assert_eq!(runtime.history_generation(), before + 1);
        let deleted: Result<(), String> = serde_json::from_str(&api.delete_snapshot(id)).unwrap();
        deleted.unwrap();
        assert_eq!(runtime.history_generation(), before + 2);
    }

    #[test]
    fn terminal_maintenance_closures_do_not_pollute_history_or_leave_stale_specs() {
        for use_snapshot in [false, true] {
            let (database, _directory) = super::super::database::test_database();
            let terminal = TrackedWindow {
                id: "terminal-being-restarted".into(),
                title: "fish - Terminal".into(),
                class: "xfce4-terminal".into(),
                ..Default::default()
            };
            let unrelated = TrackedWindow {
                id: "user-closed-editor".into(),
                title: "Editor".into(),
                class: "org.example.Editor".into(),
                ..Default::default()
            };
            let windows = [terminal.clone(), unrelated.clone()]
                .into_iter()
                .map(|window| (window.id.clone(), window))
                .collect::<HashMap<_, _>>();
            let runtime = Runtime(std::sync::Arc::new(RuntimeInner {
                database: std::sync::Mutex::new(database),
                terminal_upgrades: std::sync::Mutex::new(
                    crate::tracker::terminal_upgrade::UpgradeRegistry::default(),
                ),
                state: std::sync::Mutex::new(State {
                    restore_specs: windows
                        .keys()
                        .map(|id| (id.clone(), RestoreSpec::default()))
                        .collect(),
                    windows,
                    terminal_upgrade_windows: HashSet::from([terminal.id.clone()]),
                    terminal_upgrade_closed: HashSet::from([terminal.id.clone()]),
                    // This test must never request recovery from the user's desktop.
                    checkpoint_guarded: true,
                    ..Default::default()
                }),
            }));
            if use_snapshot {
                runtime.0.state.lock().unwrap().snapshot_buffer = Some(HashMap::new());
                runtime.finish_snapshot();
            } else {
                runtime.remove(&terminal.id);
                runtime.remove(&unrelated.id);
            }
            let state = runtime.0.state.lock().unwrap();
            assert!(state.windows.is_empty());
            assert!(state.terminal_upgrade_windows.is_empty());
            assert!(state.terminal_upgrade_closed.is_empty());
            assert!(state.restore_specs.is_empty());
            assert!(state.mass_disappearance.is_none());
            drop(state);
            let history = runtime.0.database.lock().unwrap().history(10).unwrap();
            assert_eq!(history.len(), 1);
            assert_eq!(history[0].window.id, unrelated.id);
        }
    }

    #[test]
    fn terminal_maintenance_releases_the_claim_but_protects_a_late_removed_event() {
        let (database, _directory) = super::super::database::test_database();
        let retired = TrackedWindow {
            id: "retired-terminal".into(),
            class: "xfce4-terminal".into(),
            ..Default::default()
        };
        let runtime = Runtime(std::sync::Arc::new(RuntimeInner {
            database: std::sync::Mutex::new(database),
            terminal_upgrades: std::sync::Mutex::new(
                crate::tracker::terminal_upgrade::UpgradeRegistry::default(),
            ),
            state: std::sync::Mutex::new(State {
                windows: HashMap::from([(retired.id.clone(), retired.clone())]),
                terminal_upgrade_windows: HashSet::from([
                    retired.id.clone(),
                    "failed-and-still-open".into(),
                ]),
                terminal_upgrade_closed: HashSet::from([retired.id.clone()]),
                restore_claims: HashSet::from(["terminal-upgrade".into()]),
                checkpoint_guarded: true,
                ..Default::default()
            }),
        }));
        assert!(runtime.claim_restore("ordinary".into()).is_err());
        drop(super::TerminalUpgradeProtection(runtime.clone()));
        {
            let state = runtime.0.state.lock().unwrap();
            assert!(state.restore_claims.is_empty());
            assert_eq!(
                state.terminal_upgrade_windows,
                HashSet::from([retired.id.clone()])
            );
        }
        runtime.remove(&retired.id);
        assert!(
            runtime
                .0
                .state
                .lock()
                .unwrap()
                .terminal_upgrade_windows
                .is_empty()
        );
        assert!(
            runtime
                .0
                .database
                .lock()
                .unwrap()
                .history(10)
                .unwrap()
                .is_empty()
        );
        assert!(runtime.claim_restore("ordinary".into()).is_ok());
    }

    #[test]
    fn periodic_archives_wait_for_complete_idle_sessions_without_replacing_recovery() {
        let (mut database, _directory) = super::super::database::test_database();
        let window = TrackedWindow {
            id: "editor".into(),
            title: "Editor".into(),
            class: "org.example.Editor".into(),
            x: 400,
            y: 200,
            width: 800,
            height: 600,
            ..Default::default()
        };
        let entries = vec![(window.clone(), RestoreSpec::default())];
        let recovery = database
            .create_snapshot_with_entries(None, "recovery", "old-boot", &entries, 1)
            .unwrap();
        let runtime = Runtime(std::sync::Arc::new(RuntimeInner {
            database: std::sync::Mutex::new(database),
            terminal_upgrades: std::sync::Mutex::new(
                crate::tracker::terminal_upgrade::UpgradeRegistry::default(),
            ),
            state: std::sync::Mutex::new(State {
                windows: HashMap::from([(window.id.clone(), window.clone())]),
                restore_specs: HashMap::from([(window.id.clone(), RestoreSpec::default())]),
                boot_id: "boot".into(),
                recovery_pending: true,
                checkpoint_guarded: true,
                ..Default::default()
            }),
        }));
        let mut now = Instant::now();
        let mut timestamp = 10_000;
        let retry_ms = PERIODIC_SNAPSHOT_RETRY_DELAY.as_millis() as i64;
        runtime.write_periodic_if_due(now, timestamp).unwrap();
        assert_eq!(
            runtime
                .0
                .database
                .lock()
                .unwrap()
                .snapshots()
                .unwrap()
                .len(),
            1
        );
        {
            let mut state = runtime.0.state.lock().unwrap();
            state.initial_snapshot_received = true;
            state.snapshot_buffer = Some(state.windows.clone());
        }
        now += PERIODIC_SNAPSHOT_RETRY_DELAY;
        timestamp += retry_ms;
        runtime.write_periodic_if_due(now, timestamp).unwrap();
        assert_eq!(
            runtime
                .0
                .database
                .lock()
                .unwrap()
                .snapshots()
                .unwrap()
                .len(),
            1
        );
        {
            let mut state = runtime.0.state.lock().unwrap();
            state.snapshot_buffer = None;
            state.restore_claims.insert("restore-in-progress".into());
        }
        now += PERIODIC_SNAPSHOT_RETRY_DELAY;
        timestamp += retry_ms;
        runtime.write_periodic_if_due(now, timestamp).unwrap();
        assert_eq!(
            runtime
                .0
                .database
                .lock()
                .unwrap()
                .snapshots()
                .unwrap()
                .len(),
            1
        );
        runtime.0.state.lock().unwrap().restore_claims.clear();
        now += PERIODIC_SNAPSHOT_RETRY_DELAY;
        timestamp += retry_ms;
        runtime.write_periodic_if_due(now, timestamp).unwrap();
        {
            let database = runtime.0.database.lock().unwrap();
            assert_eq!(database.snapshots().unwrap().len(), 2);
            assert_eq!(
                database.snapshot(recovery).unwrap().unwrap().windows,
                entries
            );
        }
        let interval = Duration::from_secs(300);
        runtime
            .write_periodic_if_due(
                now + interval - Duration::from_millis(1),
                timestamp + 300_000 - 1,
            )
            .unwrap();
        assert_eq!(
            runtime
                .0
                .database
                .lock()
                .unwrap()
                .snapshots()
                .unwrap()
                .len(),
            2
        );
        runtime
            .write_periodic_if_due(now + interval, timestamp + 300_000)
            .unwrap();
        assert_eq!(
            runtime
                .0
                .database
                .lock()
                .unwrap()
                .snapshots()
                .unwrap()
                .len(),
            3
        );
        runtime.0.state.lock().unwrap().windows.clear();
        runtime
            .write_periodic_if_due(now + interval * 2, timestamp + 600_000)
            .unwrap();
        assert_eq!(
            runtime
                .0
                .database
                .lock()
                .unwrap()
                .snapshots()
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn mass_disappearance_requires_a_real_burst() {
        assert!(is_mass_disappearance(50, 0));
        assert!(is_mass_disappearance(50, 24));
        assert!(is_mass_disappearance(3, 0));
        assert!(!is_mass_disappearance(2, 0));
        assert!(!is_mass_disappearance(10, 6));
        assert!(!is_mass_disappearance(5, 3));
    }

    #[test]
    fn recovery_is_offered_for_new_boots_and_interrupted_decisions() {
        assert!(should_offer_recovery(
            "boot-a", "boot-b", true, false, false, true
        ));
        assert!(should_offer_recovery(
            "boot-a", "boot-a", false, true, false, true
        ));
        assert!(should_offer_recovery(
            "boot-a", "boot-a", true, false, true, true
        ));
        assert!(!should_offer_recovery(
            "boot-a", "boot-a", true, false, false, true
        ));
        assert!(!should_offer_recovery(
            "boot-a", "boot-b", true, false, false, false
        ));
    }

    #[test]
    fn partial_recovery_remains_available_for_retry() {
        let successful = RestoreReport {
            outcomes: vec![crate::tracker::WindowRestoreOutcome {
                status: crate::tracker::RestoreOutcomeStatus::Exact,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(restore_report_succeeded(&successful));

        let partial = RestoreReport {
            failures: vec!["terminal did not appear".into()],
            outcomes: vec![crate::tracker::WindowRestoreOutcome {
                status: crate::tracker::RestoreOutcomeStatus::Failed,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(!restore_report_succeeded(&partial));
    }

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
    fn maximized_updates_preserve_the_last_normal_geometry() {
        let previous = TrackedWindow {
            normal_geometry: Some(crate::tracker::WindowGeometry {
                x: 40,
                y: 50,
                width: 900,
                height: 700,
            }),
            ..TrackedWindow::default()
        };
        let mut maximized = TrackedWindow {
            x: 0,
            y: 0,
            width: 2_000,
            height: 1_200,
            maximized: true,
            ..TrackedWindow::default()
        };
        retain_normal_geometry(&mut maximized, Some(&previous));
        assert_eq!(maximized.normal_geometry, previous.normal_geometry);

        maximized.maximized = false;
        maximized.x = 100;
        maximized.y = 110;
        maximized.width = 1_000;
        maximized.height = 800;
        retain_normal_geometry(&mut maximized, Some(&previous));
        assert_eq!(
            maximized.normal_geometry,
            Some(crate::tracker::WindowGeometry {
                x: 100,
                y: 110,
                width: 1_000,
                height: 800,
            })
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
