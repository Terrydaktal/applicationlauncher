pub(crate) fn is_xfce4_terminal_class(class: &str) -> bool {
    normalize_app_match_key(class).contains("xfce4terminal")
}

pub(crate) fn terminal_attention_payload_requires_attention(
    payload: &KWinWindowPayload,
    use_feed_attention: bool,
) -> bool {
    is_xfce4_terminal_class(&payload.class)
        && ((use_feed_attention && payload.demands_attention)
            || payload.title.to_lowercase().contains("action required"))
}

pub(crate) fn terminal_attention_dbus_connection() -> Result<zbus::blocking::Connection, String> {
    zbus::blocking::connection::Builder::session()
        .map_err(|err| format!("Could not configure the session D-Bus connection: {err}"))?
        .method_timeout(Duration::from_secs(TERMINAL_ATTENTION_DBUS_TIMEOUT_SECS))
        .build()
        .map_err(|err| format!("Could not connect to the session D-Bus: {err}"))
}

pub(crate) fn terminal_attention_send_is_cancelled(
    cancellation: Option<&std::sync::atomic::AtomicBool>,
) -> bool {
    cancellation.is_some_and(|cancellation| cancellation.load(std::sync::atomic::Ordering::Acquire))
}

pub(crate) fn send_enter_to_terminal_identity(
    identity: &TerminalWindowIdentity,
    description: &str,
    cancellation: Option<&std::sync::atomic::AtomicBool>,
) -> Result<String, String> {
    if terminal_attention_send_is_cancelled(cancellation) {
        return Err(TERMINAL_ATTENTION_CANCELLED.to_string());
    }
    let connection = terminal_attention_dbus_connection()?;
    let proxy = zbus::blocking::Proxy::new(
        &connection,
        TERMINAL_DBUS_SERVICE,
        TERMINAL_DBUS_PATH,
        TERMINAL_DBUS_INTERFACE,
    )
    .map_err(|err| format!("Could not create the XFCE4 Terminal D-Bus client: {err}"))?;
    let raw_records: Vec<HashMap<String, zbus::zvariant::OwnedValue>> = proxy
        .call("ListTerminals", &())
        .map_err(|err| {
            format!(
                "XFCE4 Terminal's background-input API is unavailable: {err}. Restart all terminal server instances with the patched build"
            )
    })?;
    let records = parse_terminal_dbus_records(raw_records);
    let tab_uuid = select_terminal_tab(identity, &records)?;

    if terminal_attention_send_is_cancelled(cancellation) {
        return Err(TERMINAL_ATTENTION_CANCELLED.to_string());
    }

    let send_result: Result<(), zbus::Error> = proxy.call("SendEnter", &(tab_uuid.as_str(),));
    match send_result {
        Ok(()) => Ok(format!("Enter sent to {description}")),
        Err(err) => Err(format!("XFCE4 Terminal rejected SendEnter: {err}")),
    }
}

pub(crate) fn send_enter_to_terminal_window(
    win: &WindowInfo,
    cancellation: Option<&std::sync::atomic::AtomicBool>,
) -> Result<String, String> {
    if !is_xfce4_terminal_class(&win.class) {
        return Err("Background Enter is currently supported only for XFCE4 Terminal".to_string());
    }

    send_enter_to_terminal_identity(&terminal_window_identity(win), &win.title, cancellation)
}

pub(crate) fn send_enter_to_terminal_payload(
    payload: &KWinWindowPayload,
    cancellation: Option<&std::sync::atomic::AtomicBool>,
) -> Result<String, String> {
    if !is_xfce4_terminal_class(&payload.class) {
        return Err("Background Enter is currently supported only for XFCE4 Terminal".to_string());
    }

    let identity = TerminalWindowIdentity {
        normalized_title: normalize_window_sort_title(&payload.title),
        ..Default::default()
    };
    send_enter_to_terminal_identity(&identity, &payload.title, cancellation)
}

pub(crate) fn terminal_attention_retry_delay(attempt: u8) -> Duration {
    let exponent = u32::from(attempt.saturating_sub(1).min(6));
    Duration::from_millis(TERMINAL_ATTENTION_RETRY_BASE_MS.saturating_mul(1_u64 << exponent))
}

pub(crate) fn apply_terminal_attention_feed_upsert(
    payload: KWinWindowPayload,
    windows: &mut HashMap<String, KWinWindowPayload>,
    feed_last_seen: &mut HashMap<String, Instant>,
    deadlines: &mut HashMap<String, Instant>,
    handled: &mut HashSet<String>,
    exhausted: &mut HashSet<String>,
    retry_attempts: &mut HashMap<String, u8>,
    attention_generations: &mut HashMap<String, u64>,
) -> bool {
    let id = payload.id.clone();
    let previous = windows.get(&id);
    let feed_attention_cleared =
        previous.is_some_and(|previous| previous.demands_attention && !payload.demands_attention);
    let title_attention_cleared = previous.is_some_and(|previous| {
        previous.title.to_lowercase().contains("action required")
            && !payload.title.to_lowercase().contains("action required")
    });

    // Preserve a brief clear between back-to-back prompts even when the feed queue
    // already contains the next attention update by the time it is drained. The
    // KWin flag and title are evaluated independently because either can lag.
    let attention_cleared = feed_attention_cleared || title_attention_cleared;
    if attention_cleared {
        deadlines.remove(&id);
        handled.remove(&id);
        exhausted.remove(&id);
        retry_attempts.remove(&id);
        let generation = attention_generations.entry(id.clone()).or_default();
        *generation = generation.wrapping_add(1);
    }

    feed_last_seen.insert(id.clone(), Instant::now());
    windows.insert(id, payload);
    attention_cleared
}

pub(crate) fn terminal_attention_attempt_is_current(
    id: &str,
    generation: u64,
    attention_generations: &HashMap<String, u64>,
) -> bool {
    attention_generations.get(id).copied().unwrap_or_default() == generation
}

pub(crate) fn record_terminal_attention_success(
    id: &str,
    payload: Option<&KWinWindowPayload>,
    deadlines: &mut HashMap<String, Instant>,
    handled: &mut HashSet<String>,
    now: Instant,
) {
    let still_shows_action_required =
        payload.is_some_and(|payload| payload.title.to_lowercase().contains("action required"));

    if still_shows_action_required {
        // A second prompt can replace the first without KWin or the title ever
        // reporting a clear state. Wait the full delay before checking again.
        handled.remove(id);
        deadlines.insert(
            id.to_string(),
            now + Duration::from_secs(AUTO_SEND_ENTER_DELAY_SECS),
        );
    } else {
        deadlines.remove(id);
        handled.insert(id.to_string());
    }
}

pub(crate) fn rearm_terminal_attention_automation(
    windows: &HashMap<String, KWinWindowPayload>,
    deadlines: &mut HashMap<String, Instant>,
    handled: &mut HashSet<String>,
    exhausted: &mut HashSet<String>,
    retry_attempts: &mut HashMap<String, u8>,
    in_flight: &HashMap<String, Arc<std::sync::atomic::AtomicBool>>,
    attention_generations: &mut HashMap<String, u64>,
) {
    for cancellation in in_flight.values() {
        cancellation.store(true, std::sync::atomic::Ordering::Release);
    }
    deadlines.clear();
    handled.clear();
    exhausted.clear();
    retry_attempts.clear();
    for id in windows.keys() {
        let generation = attention_generations.entry(id.clone()).or_default();
        *generation = generation.wrapping_add(1);
    }
}

pub(crate) fn reconcile_terminal_attention_windows_from_kwin(
    windows: &mut HashMap<String, KWinWindowPayload>,
    enriched_windows: &Arc<std::sync::Mutex<HashMap<String, WindowInfo>>>,
) -> Result<(), String> {
    let enriched_windows = enriched_windows
        .lock()
        .map(|windows| windows.clone())
        .unwrap_or_default();
    let ids = windows
        .values()
        .filter(|payload| is_xfce4_terminal_class(&payload.class))
        .map(|payload| payload.id.clone())
        .chain(
            enriched_windows
                .values()
                .filter(|window| is_xfce4_terminal_class(&window.class))
                .map(|window| window.id.clone()),
        )
        .collect::<HashSet<_>>();
    if ids.is_empty() {
        return Ok(());
    }

    let connection = terminal_attention_dbus_connection()
        .map_err(|err| format!("Could not connect to KWin over D-Bus: {err}"))?;
    let proxy = zbus::blocking::Proxy::new(
        &connection,
        KWIN_DBUS_SERVICE,
        KWIN_DBUS_PATH,
        KWIN_DBUS_INTERFACE,
    )
    .map_err(|err| format!("Could not create the KWin D-Bus client: {err}"))?;

    for id in ids {
        let values: HashMap<String, zbus::zvariant::OwnedValue> = proxy
            .call("getWindowInfo", &(id.as_str(),))
            .map_err(|err| format!("KWin window reconciliation failed: {err}"))?;
        if values.is_empty() {
            windows.remove(&id);
            continue;
        }

        let enriched = enriched_windows.get(&id);
        let class = terminal_dbus_string(&values, "resourceClass")
            .filter(|value| !value.trim().is_empty())
            .or_else(|| enriched.map(|window| window.class.clone()))
            .unwrap_or_default();
        if !is_xfce4_terminal_class(&class) {
            windows.remove(&id);
            continue;
        }

        let title = terminal_dbus_string(&values, "caption")
            .or_else(|| enriched.map(|window| window.raw_title.clone()))
            .unwrap_or_default();
        let existing_attention = windows
            .get(&id)
            .is_some_and(|payload| payload.demands_attention);
        let minimized = terminal_dbus_bool(&values, "minimized").unwrap_or_else(|| {
            enriched
                .and_then(|window| window.minimized)
                .unwrap_or(false)
        });
        windows.insert(
            id.clone(),
            KWinWindowPayload {
                id,
                title,
                class,
                pid: enriched.and_then(|window| window.pid).unwrap_or_default(),
                desktop_file_name: enriched
                    .and_then(|window| window.desktop_file_name.clone())
                    .unwrap_or_default(),
                x: 0,
                y: 0,
                width: 0,
                height: 0,
                minimized,
                demands_attention: existing_attention,
                last_activated_at_ms: enriched.and_then(|window| window.last_activated_at_ms),
                activation_sequence: enriched.map_or(0, |window| window.activation_sequence),
            },
        );
    }

    Ok(())
}

pub(crate) fn run_terminal_attention_worker(
    receiver: &Receiver<WindowFeedEvent>,
    enabled: &Arc<std::sync::atomic::AtomicBool>,
    enriched_windows: &Arc<std::sync::Mutex<HashMap<String, WindowInfo>>>,
    result_sender: &Sender<Result<String, String>>,
    repaint_ctx: &egui::Context,
) {
    let mut windows = HashMap::<String, KWinWindowPayload>::new();
    let mut deadlines = HashMap::<String, Instant>::new();
    let mut handled = HashSet::<String>::new();
    let mut exhausted = HashSet::<String>::new();
    let mut retry_attempts = HashMap::<String, u8>::new();
    let mut in_flight = HashMap::<String, Arc<std::sync::atomic::AtomicBool>>::new();
    let mut feed_last_seen = HashMap::<String, Instant>::new();
    let mut attention_generations = HashMap::<String, u64>::new();
    let (attempt_sender, attempt_receiver) =
        std::sync::mpsc::channel::<(String, u64, Result<String, String>)>();
    let maximum_wait = Duration::from_millis(TERMINAL_ATTENTION_WORKER_MAX_WAIT_MS);
    let reconciliation_interval = Duration::from_secs(TERMINAL_ATTENTION_RECONCILIATION_SECS);
    let feed_state_ttl = Duration::from_secs(TERMINAL_ATTENTION_FEED_STATE_TTL_SECS);
    let mut next_reconciliation = Instant::now();
    let mut feed_connected = true;
    let mut last_reconciliation_error: Option<(String, Instant)> = None;
    let mut wait = Duration::ZERO;

    loop {
        if feed_connected {
            match receiver.recv_timeout(wait) {
                Ok(WindowFeedEvent::Reset) => {
                    for cancellation in in_flight.values() {
                        cancellation.store(true, std::sync::atomic::Ordering::Release);
                    }
                    windows.clear();
                    deadlines.clear();
                    handled.clear();
                    exhausted.clear();
                    retry_attempts.clear();
                    feed_last_seen.clear();
                    attention_generations.clear();
                }
                Ok(WindowFeedEvent::Snapshot(_)) => {}
                Ok(WindowFeedEvent::Upsert(payload)) => {
                    let id = payload.id.clone();
                    let attention_cleared = apply_terminal_attention_feed_upsert(
                        payload,
                        &mut windows,
                        &mut feed_last_seen,
                        &mut deadlines,
                        &mut handled,
                        &mut exhausted,
                        &mut retry_attempts,
                        &mut attention_generations,
                    );
                    if attention_cleared && let Some(cancellation) = in_flight.get(&id) {
                        cancellation.store(true, std::sync::atomic::Ordering::Release);
                    }
                }
                Ok(WindowFeedEvent::Remove(id)) => {
                    if let Some(cancellation) = in_flight.get(&id) {
                        cancellation.store(true, std::sync::atomic::Ordering::Release);
                    }
                    windows.remove(&id);
                    deadlines.remove(&id);
                    handled.remove(&id);
                    exhausted.remove(&id);
                    retry_attempts.remove(&id);
                    feed_last_seen.remove(&id);
                    attention_generations.remove(&id);
                }
                Ok(WindowFeedEvent::RearmAttentionAutomation) => {
                    rearm_terminal_attention_automation(
                        &windows,
                        &mut deadlines,
                        &mut handled,
                        &mut exhausted,
                        &mut retry_attempts,
                        &in_flight,
                        &mut attention_generations,
                    );
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    feed_connected = false;
                }
            }
        } else if !wait.is_zero() {
            std::thread::sleep(wait);
        }

        if feed_connected {
            while let Ok(event) = receiver.try_recv() {
                match event {
                    WindowFeedEvent::Reset => {
                        for cancellation in in_flight.values() {
                            cancellation.store(true, std::sync::atomic::Ordering::Release);
                        }
                        windows.clear();
                        deadlines.clear();
                        handled.clear();
                        exhausted.clear();
                        retry_attempts.clear();
                        feed_last_seen.clear();
                        attention_generations.clear();
                    }
                    WindowFeedEvent::Snapshot(_) => {}
                    WindowFeedEvent::Upsert(payload) => {
                        let id = payload.id.clone();
                        let attention_cleared = apply_terminal_attention_feed_upsert(
                            payload,
                            &mut windows,
                            &mut feed_last_seen,
                            &mut deadlines,
                            &mut handled,
                            &mut exhausted,
                            &mut retry_attempts,
                            &mut attention_generations,
                        );
                        if attention_cleared && let Some(cancellation) = in_flight.get(&id) {
                            cancellation.store(true, std::sync::atomic::Ordering::Release);
                        }
                    }
                    WindowFeedEvent::Remove(id) => {
                        if let Some(cancellation) = in_flight.get(&id) {
                            cancellation.store(true, std::sync::atomic::Ordering::Release);
                        }
                        windows.remove(&id);
                        deadlines.remove(&id);
                        handled.remove(&id);
                        exhausted.remove(&id);
                        retry_attempts.remove(&id);
                        feed_last_seen.remove(&id);
                        attention_generations.remove(&id);
                    }
                    WindowFeedEvent::RearmAttentionAutomation => {
                        rearm_terminal_attention_automation(
                            &windows,
                            &mut deadlines,
                            &mut handled,
                            &mut exhausted,
                            &mut retry_attempts,
                            &in_flight,
                            &mut attention_generations,
                        );
                    }
                }
            }
        }

        let completed_attempts = attempt_receiver.try_iter().collect::<Vec<_>>();
        for (id, _, _) in &completed_attempts {
            in_flight.remove(id);
        }

        let enabled_now = enabled.load(std::sync::atomic::Ordering::Acquire);
        let now = Instant::now();
        if !enabled_now {
            for cancellation in in_flight.values() {
                cancellation.store(true, std::sync::atomic::Ordering::Release);
            }
            let empty = HashSet::new();
            update_terminal_attention_schedule(false, &empty, &mut deadlines, &mut handled, now);
            exhausted.clear();
            retry_attempts.clear();
            next_reconciliation = now;
            wait = maximum_wait;
            continue;
        }

        if now >= next_reconciliation {
            match reconcile_terminal_attention_windows_from_kwin(&mut windows, &enriched_windows) {
                Ok(()) => last_reconciliation_error = None,
                Err(err) => {
                    let should_log =
                        last_reconciliation_error
                            .as_ref()
                            .is_none_or(|(previous, logged_at)| {
                                previous != &err || logged_at.elapsed() >= Duration::from_secs(60)
                            });
                    if should_log {
                        eprintln!("Terminal attention reconciliation failed: {err}");
                        last_reconciliation_error = Some((err, Instant::now()));
                    }
                }
            }
            next_reconciliation = Instant::now() + reconciliation_interval;
        }

        let now = Instant::now();
        let eligible_ids = windows
            .values()
            .filter(|payload| {
                let has_recent_feed_state = feed_connected
                    && feed_last_seen
                        .get(&payload.id)
                        .is_some_and(|seen| now.duration_since(*seen) <= feed_state_ttl);
                terminal_attention_payload_requires_attention(payload, has_recent_feed_state)
            })
            .map(|payload| payload.id.clone())
            .collect::<HashSet<_>>();

        for (id, cancellation) in &in_flight {
            if !eligible_ids.contains(id) {
                cancellation.store(true, std::sync::atomic::Ordering::Release);
            }
        }

        for (id, attempt_generation, result) in completed_attempts {
            let current_episode = terminal_attention_attempt_is_current(
                &id,
                attempt_generation,
                &attention_generations,
            );
            match result {
                Ok(message) => {
                    if current_episode && eligible_ids.contains(&id) {
                        record_terminal_attention_success(
                            &id,
                            windows.get(&id),
                            &mut deadlines,
                            &mut handled,
                            Instant::now(),
                        );
                    }
                    if current_episode {
                        retry_attempts.remove(&id);
                    }
                    let _ = result_sender.send(Ok(message));
                    repaint_ctx.request_repaint();
                }
                Err(err) if err == TERMINAL_ATTENTION_CANCELLED => {
                    retry_attempts.remove(&id);
                }
                Err(err) if current_episode && eligible_ids.contains(&id) => {
                    let attempt = retry_attempts.entry(id.clone()).or_insert(0);
                    *attempt = attempt.saturating_add(1);
                    if *attempt < TERMINAL_ATTENTION_MAX_RETRIES {
                        deadlines.insert(
                            id,
                            Instant::now() + terminal_attention_retry_delay(*attempt),
                        );
                    } else {
                        retry_attempts.remove(&id);
                        exhausted.insert(id);
                        let _ = result_sender.send(Err(format!(
                            "Auto-send Enter failed after {TERMINAL_ATTENTION_MAX_RETRIES} attempts: {err}"
                        )));
                        repaint_ctx.request_repaint();
                    }
                }
                Err(_) => {
                    retry_attempts.remove(&id);
                }
            }
        }

        exhausted.retain(|id| eligible_ids.contains(id));
        retry_attempts.retain(|id, _| eligible_ids.contains(id));
        let schedulable_ids = eligible_ids
            .iter()
            .filter(|id| !exhausted.contains(*id) && !in_flight.contains_key(*id))
            .cloned()
            .collect::<HashSet<_>>();
        let (due_ids, next_deadline) = update_terminal_attention_schedule(
            true,
            &schedulable_ids,
            &mut deadlines,
            &mut handled,
            now,
        );

        for id in due_ids {
            let payload = windows.get(&id).cloned();
            let enriched_window = enriched_windows
                .lock()
                .ok()
                .and_then(|windows| windows.get(&id).cloned());
            let sender = attempt_sender.clone();
            let attempt_generation = attention_generations.get(&id).copied().unwrap_or_default();
            let cancellation = Arc::new(std::sync::atomic::AtomicBool::new(false));
            in_flight.insert(id.clone(), cancellation.clone());
            std::thread::spawn(move || {
                let result = if let Some(window) = enriched_window.as_ref() {
                    send_enter_to_terminal_window(window, Some(cancellation.as_ref()))
                } else if let Some(payload) = payload.as_ref() {
                    send_enter_to_terminal_payload(payload, Some(cancellation.as_ref()))
                } else {
                    Err("The attention window disappeared before Enter was sent".to_string())
                };
                let _ = sender.send((id, attempt_generation, result));
            });
        }

        let now = Instant::now();
        wait = [
            next_deadline,
            deadlines.values().copied().min(),
            Some(next_reconciliation),
        ]
        .into_iter()
        .flatten()
        .map(|deadline| deadline.saturating_duration_since(now))
        .min()
        .unwrap_or(maximum_wait)
        .min(maximum_wait);
    }
}

pub(crate) fn start_terminal_attention_worker(
    receiver: Receiver<WindowFeedEvent>,
    enabled: Arc<std::sync::atomic::AtomicBool>,
    enriched_windows: Arc<std::sync::Mutex<HashMap<String, WindowInfo>>>,
    result_sender: Sender<Result<String, String>>,
    repaint_ctx: egui::Context,
) {
    std::thread::spawn(move || {
        loop {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_terminal_attention_worker(
                    &receiver,
                    &enabled,
                    &enriched_windows,
                    &result_sender,
                    &repaint_ctx,
                );
            }));
            if result.is_ok() {
                break;
            }

            let _ = result_sender.send(Err(
                "Terminal attention worker panicked and is restarting".to_string()
            ));
            repaint_ctx.request_repaint();
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}
use super::*;
use crate::*;
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc,
    mpsc::{Receiver, Sender},
};
use std::time::{Duration, Instant};
