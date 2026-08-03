use crate::models::{
    KWinWindowPayload, ProcessChainEntry, TerminalDbusRecord, TerminalWindowIdentity,
    WindowFeedEvent, WindowInfo,
};
use crate::{
    AUTO_SEND_ENTER_DELAY_SECS, KWIN_DBUS_INTERFACE, KWIN_DBUS_PATH, KWIN_DBUS_SERVICE,
    TERMINAL_ATTENTION_CANCELLED, TERMINAL_ATTENTION_DBUS_TIMEOUT_SECS,
    TERMINAL_ATTENTION_FEED_STATE_TTL_SECS, TERMINAL_ATTENTION_MAX_RETRIES,
    TERMINAL_ATTENTION_RECONCILIATION_SECS, TERMINAL_ATTENTION_RETRY_BASE_MS,
    TERMINAL_ATTENTION_WORKER_MAX_WAIT_MS, TERMINAL_DBUS_INTERFACE, TERMINAL_DBUS_PATH,
    TERMINAL_DBUS_SERVICE, is_braille_spinner_char, normalize_app_match_key,
    normalize_window_sort_title, process_pty_path, read_process_stat,
    update_terminal_attention_schedule,
};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{
    Arc,
    mpsc::{Receiver, Sender},
};
use std::time::{Duration, Instant};

pub(crate) fn is_generic_terminal_process(proc_name: &str) -> bool {
    matches!(
        normalize_app_match_key(proc_name).as_str(),
        "bash" | "fish" | "sh" | "zsh" | "python" | "python3" | "node" | "ruby" | "perl"
    )
}

pub(crate) fn terminal_process_display_name(proc_name: &str) -> &str {
    if is_codex_process(proc_name) {
        "codex"
    } else {
        proc_name.trim()
    }
}

pub(crate) fn is_codex_process(proc_name: &str) -> bool {
    let normalized = normalize_app_match_key(proc_name);
    normalized == "codex" || normalized.starts_with("codexcodemode")
}

pub(crate) fn terminal_parent_program<'a>(
    proc_name: &str,
    process_chain: &'a [ProcessChainEntry],
) -> Option<&'a str> {
    if is_codex_process(proc_name) {
        return None;
    }

    if process_chain
        .iter()
        .skip(1)
        .any(|entry| is_codex_process(&entry.name))
    {
        return Some("codex");
    }

    if normalize_app_match_key(proc_name) == "ssh" {
        return process_chain
            .iter()
            .skip(1)
            .find(|entry| is_shell_process(&entry.name))
            .map(|entry| terminal_process_display_name(&entry.name));
    }

    None
}

fn is_shell_process(proc_name: &str) -> bool {
    matches!(
        normalize_app_match_key(proc_name).as_str(),
        "bash" | "fish" | "sh" | "zsh"
    )
}

pub(crate) fn terminal_primary_title(proc_name: &str, command_summary: Option<&str>) -> String {
    let proc_name = proc_name.trim();
    let Some(command_summary) = command_summary
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return terminal_process_display_name(proc_name).to_string();
    };

    if is_generic_terminal_process(proc_name)
        && normalize_app_match_key(command_summary) != normalize_app_match_key(proc_name)
    {
        command_summary.to_string()
    } else {
        terminal_process_display_name(proc_name).to_string()
    }
}

pub(crate) fn terminal_context_looks_path_like(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && (trimmed.starts_with('~')
            || trimmed.starts_with('/')
            || trimmed == "."
            || trimmed == ".."
            || trimmed.contains('/'))
}

pub(crate) fn strip_leading_braille_spinner(value: &str) -> &str {
    value.trim_start_matches(|ch: char| is_braille_spinner_char(ch) || ch.is_whitespace())
}

pub(crate) fn terminal_segment_leading_spinner(value: &str) -> Option<char> {
    value
        .trim_start()
        .chars()
        .next()
        .filter(|ch| is_braille_spinner_char(*ch))
}

pub(crate) fn terminal_path_basename(value: &str) -> Option<&str> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    trimmed.rsplit('/').next().filter(|part| !part.is_empty())
}

pub(crate) fn terminal_segment_matches_cwd_basename(part: &str, cwd: &str) -> bool {
    let part = strip_leading_braille_spinner(part).trim();
    let Some(cwd_basename) = terminal_path_basename(cwd) else {
        return false;
    };

    !part.is_empty() && normalize_app_match_key(part) == normalize_app_match_key(cwd_basename)
}

pub(crate) fn is_terminal_title_marker(value: &str) -> bool {
    let key = normalize_app_match_key(value);
    key == "terminal"
        || key == "xfce4terminal"
        || key == "konsole"
        || key == "kitty"
        || key == "alacritty"
        || key == "wezterm"
        || key == "foot"
        || key.ends_with("terminal")
}

pub(crate) fn terminal_title_segments(
    dynamic_title: &str,
    proc_name: &str,
    command_summary: Option<&str>,
    cwd: Option<&str>,
    parent_program: Option<&str>,
) -> Vec<String> {
    let dynamic_title = dynamic_title.trim();
    let mut segments = Vec::new();
    let primary_title = terminal_primary_title(proc_name, command_summary);
    if let Some(parent_program) = parent_program {
        push_unique_terminal_segment(&mut segments, parent_program);
        push_unique_terminal_segment(&mut segments, terminal_process_display_name(proc_name));
    } else {
        push_unique_terminal_segment(&mut segments, primary_title.clone());
    }

    let primary_key = normalize_app_match_key(&primary_title);
    let proc_key = normalize_app_match_key(proc_name);
    let parent_key = parent_program.map(normalize_app_match_key);
    let cwd = cwd.map(str::trim).filter(|value| !value.is_empty());
    let cwd_key = cwd.map(normalize_app_match_key);
    let separators = [" - ", " — ", " – ", " : ", " | "];
    let mut has_cwd_segment = false;
    let prefer_cwd_over_dynamic_path =
        cwd.is_some() && terminal_context_looks_path_like(dynamic_title);

    let title_parts: Vec<&str> = separators
        .iter()
        .find_map(|sep| {
            dynamic_title
                .contains(sep)
                .then(|| dynamic_title.split(sep).map(str::trim).collect())
        })
        .unwrap_or_else(|| {
            if dynamic_title.is_empty() {
                Vec::new()
            } else {
                vec![dynamic_title]
            }
        });

    for part in title_parts {
        let part = part.trim();
        if part.is_empty() || is_terminal_title_marker(part) {
            continue;
        }

        if prefer_cwd_over_dynamic_path && terminal_context_looks_path_like(part) {
            continue;
        }

        if let Some(cwd) = cwd.filter(|cwd| terminal_segment_matches_cwd_basename(part, cwd)) {
            let spinner = terminal_segment_leading_spinner(part).filter(|_| {
                !segments
                    .iter()
                    .any(|segment| segment.chars().any(is_braille_spinner_char))
            });
            let cwd_segment = spinner
                .map(|spinner| format!("{spinner} {cwd}"))
                .unwrap_or_else(|| cwd.to_string());
            push_unique_terminal_segment(&mut segments, cwd_segment);
            has_cwd_segment = true;
            continue;
        }

        let part_key = normalize_app_match_key(part);
        if part_key.is_empty()
            || part_key == primary_key
            || part_key == proc_key
            || parent_key.as_ref().is_some_and(|key| *key == part_key)
        {
            continue;
        }

        if cwd_key.as_ref().is_some_and(|cwd_key| *cwd_key == part_key) {
            has_cwd_segment = true;
        }

        push_unique_terminal_segment(&mut segments, part.to_string());
    }

    if let Some(cwd) = cwd {
        if !has_cwd_segment {
            let cwd_context = if dynamic_title.is_empty()
                || parent_program.is_some()
                || !terminal_context_looks_path_like(dynamic_title)
            {
                cwd.to_string()
            } else {
                replace_terminal_suffix_path(dynamic_title, cwd)
            };
            push_unique_terminal_segment(&mut segments, cwd_context);
        }
    }

    segments.push("Terminal".to_string());
    segments
}

pub(crate) fn terminal_display_title(
    raw_title: &str,
    proc_name: &str,
    command_summary: Option<&str>,
    cwd: Option<&str>,
    parent_program: Option<&str>,
) -> String {
    let separators = [" - ", " — ", " – ", " : ", " | "];
    let raw_title = raw_title
        .trim()
        .strip_prefix("- ")
        .unwrap_or(raw_title.trim());

    if normalize_app_match_key(proc_name) == "ssh"
        && let Some(title) = ssh_terminal_display_title(raw_title, parent_program)
    {
        return title;
    }

    for sep in separators {
        let parts: Vec<&str> = raw_title.split(sep).map(str::trim).collect();
        if parts.len() < 2 {
            continue;
        }

        if parts
            .first()
            .is_some_and(|part| is_terminal_title_marker(part))
        {
            let suffix = parts[1..].join(sep);
            return terminal_title_segments(
                &suffix,
                proc_name,
                command_summary,
                cwd,
                parent_program,
            )
            .join(sep);
        }

        if parts
            .last()
            .is_some_and(|part| is_terminal_title_marker(part))
        {
            let suffix = parts[..parts.len() - 1].join(sep);
            return terminal_title_segments(
                &suffix,
                proc_name,
                command_summary,
                cwd,
                parent_program,
            )
            .join(sep);
        }
    }

    terminal_title_segments(raw_title, proc_name, command_summary, cwd, parent_program).join(" - ")
}

fn ssh_terminal_display_title(raw_title: &str, parent_program: Option<&str>) -> Option<String> {
    let dynamic_title = strip_terminal_title_marker(raw_title);
    let closing_bracket = dynamic_title.find(']')?;
    if !dynamic_title.starts_with('[') {
        return None;
    }

    let remote_user = dynamic_title[..=closing_bracket].trim();
    let remote_context = dynamic_title[closing_bracket + 1..].trim();
    if remote_user.len() <= 2 || remote_context.is_empty() {
        return None;
    }

    let remote_shell = parent_program
        .filter(|program| is_shell_process(program))
        .map(terminal_process_display_name);
    let remote_identity = remote_shell
        .map(|shell| format!("{remote_user} {shell}"))
        .unwrap_or_else(|| remote_user.to_string());

    Some(format!(
        "ssh - {remote_identity} - {remote_context} - Terminal"
    ))
}

fn strip_terminal_title_marker(raw_title: &str) -> &str {
    for separator in [" - ", " — ", " – ", " : ", " | "] {
        if let Some(dynamic_title) = raw_title
            .strip_suffix("Terminal")
            .and_then(|title| title.strip_suffix(separator))
        {
            return dynamic_title.trim();
        }
    }

    raw_title.trim()
}

pub(crate) fn terminal_dbus_string(
    values: &HashMap<String, zbus::zvariant::OwnedValue>,
    key: &str,
) -> Option<String> {
    let value = zbus::zvariant::Value::try_from(values.get(key)?).ok()?;
    value.downcast_ref::<String>().ok()
}

pub(crate) fn terminal_dbus_bool(
    values: &HashMap<String, zbus::zvariant::OwnedValue>,
    key: &str,
) -> Option<bool> {
    let value = zbus::zvariant::Value::try_from(values.get(key)?).ok()?;
    value.downcast_ref::<bool>().ok()
}

pub(crate) fn terminal_dbus_u32(
    values: &HashMap<String, zbus::zvariant::OwnedValue>,
    key: &str,
) -> Option<u32> {
    let value = zbus::zvariant::Value::try_from(values.get(key)?).ok()?;
    value.downcast_ref::<u32>().ok()
}

pub(crate) fn parse_terminal_dbus_records(
    records: Vec<HashMap<String, zbus::zvariant::OwnedValue>>,
) -> Vec<TerminalDbusRecord> {
    records
        .into_iter()
        .filter_map(|values| {
            let tab_uuid = terminal_dbus_string(&values, "tab_uuid")?;
            if tab_uuid.is_empty() {
                return None;
            }

            Some(TerminalDbusRecord {
                window_uuid: terminal_dbus_string(&values, "window_uuid").unwrap_or_default(),
                tab_uuid,
                active: terminal_dbus_bool(&values, "active").unwrap_or(false),
                window_title: terminal_dbus_string(&values, "window_title").unwrap_or_default(),
                working_directory: terminal_dbus_string(&values, "working_directory")
                    .unwrap_or_default(),
                child_pid: terminal_dbus_u32(&values, "child_pid").unwrap_or(0),
                foreground_pid: terminal_dbus_u32(&values, "foreground_pid").unwrap_or(0),
                foreground_pgid: terminal_dbus_u32(&values, "foreground_pgid").unwrap_or(0),
                pty: terminal_dbus_string(&values, "pty").unwrap_or_default(),
            })
        })
        .collect()
}

pub(crate) fn fetch_terminal_dbus_records() -> Result<Vec<TerminalDbusRecord>, String> {
    let connection = zbus::blocking::Connection::session()
        .map_err(|err| format!("Could not connect to the session D-Bus: {err}"))?;
    let proxy = zbus::blocking::Proxy::new(
        &connection,
        TERMINAL_DBUS_SERVICE,
        TERMINAL_DBUS_PATH,
        TERMINAL_DBUS_INTERFACE,
    )
    .map_err(|err| format!("Could not create the XFCE4 Terminal D-Bus client: {err}"))?;
    let raw_records: Vec<HashMap<String, zbus::zvariant::OwnedValue>> = proxy
        .call("ListTerminals", &())
        .map_err(|err| format!("XFCE4 Terminal's metadata API is unavailable: {err}"))?;
    Ok(parse_terminal_dbus_records(raw_records))
}

pub(crate) fn terminal_record_for_window_title<'a>(
    raw_title: &str,
    records: &'a [TerminalDbusRecord],
) -> Option<&'a TerminalDbusRecord> {
    let normalized_title = normalize_window_sort_title(raw_title);
    if normalized_title.is_empty() {
        return None;
    }

    let mut matches = records.iter().filter(|record| {
        record.active
            && normalize_window_sort_title(&record.window_title) == normalized_title
            && (record.child_pid > 0 || record.foreground_pid > 0)
    });
    let matched = matches.next()?;
    matches.next().is_none().then_some(matched)
}

pub(crate) fn terminal_server_has_dbus_records(
    terminal_pid: i32,
    records: &[TerminalDbusRecord],
    pid_to_ppid: &HashMap<i32, i32>,
) -> bool {
    records.iter().any(|record| {
        [record.child_pid, record.foreground_pid]
            .into_iter()
            .filter_map(|pid| i32::try_from(pid).ok())
            .any(|mut pid| {
                let mut visited = HashSet::new();
                while pid > 0 && visited.insert(pid) {
                    if pid == terminal_pid {
                        return true;
                    }
                    let Some(parent) = pid_to_ppid.get(&pid).copied() else {
                        break;
                    };
                    pid = parent;
                }
                false
            })
    })
}

pub(crate) fn terminal_window_identity(win: &WindowInfo) -> TerminalWindowIdentity {
    let mut identity = TerminalWindowIdentity {
        normalized_title: normalize_window_sort_title(&win.raw_title),
        cwd: win.cwd_path.clone(),
        ..Default::default()
    };

    if let Some(pid) = win.pid.and_then(|pid| u32::try_from(pid).ok()) {
        identity.process_pids.insert(pid);
    }

    for entry in &win.process_chain {
        if let Ok(pid) = u32::try_from(entry.pid) {
            identity.process_pids.insert(pid);
        }
        if let Some(stat) = read_process_stat(entry.pid) {
            if let Ok(process_group) = u32::try_from(stat.process_group) {
                identity.process_groups.insert(process_group);
            }
        }
        if let Some(pty) = process_pty_path(entry.pid) {
            identity.ptys.insert(pty);
        }
    }

    identity
}

pub(crate) fn select_terminal_tab(
    identity: &TerminalWindowIdentity,
    records: &[TerminalDbusRecord],
) -> Result<String, String> {
    let active_records: Vec<&TerminalDbusRecord> =
        records.iter().filter(|record| record.active).collect();
    if active_records.is_empty() {
        return Err("XFCE4 Terminal reported no active terminal tabs".to_string());
    }

    let matching_title_count = active_records
        .iter()
        .filter(|record| {
            !identity.normalized_title.is_empty()
                && normalize_window_sort_title(&record.window_title) == identity.normalized_title
        })
        .count();
    let mut candidates = Vec::new();

    for record in active_records {
        let pty_match = !record.pty.is_empty() && identity.ptys.contains(&record.pty);
        let child_match = record.child_pid > 0 && identity.process_pids.contains(&record.child_pid);
        let process_group_match =
            record.foreground_pgid > 0 && identity.process_groups.contains(&record.foreground_pgid);
        let title_match = !identity.normalized_title.is_empty()
            && normalize_window_sort_title(&record.window_title) == identity.normalized_title;
        let cwd_match = identity.cwd.as_ref().is_some_and(|cwd| {
            !record.working_directory.is_empty()
                && Path::new(&record.working_directory) == cwd.as_path()
        });
        let has_process_identity = pty_match || child_match || process_group_match;

        if !has_process_identity && !(title_match && matching_title_count == 1) {
            continue;
        }

        let score = u32::from(pty_match) * 1000
            + u32::from(child_match) * 700
            + u32::from(process_group_match) * 600
            + u32::from(title_match) * 200
            + u32::from(cwd_match) * 100;
        candidates.push((score, record));
    }

    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    let Some((best_score, best)) = candidates.first() else {
        return Err(
            "Could not safely match this window to an active XFCE4 Terminal tab".to_string(),
        );
    };
    if candidates
        .iter()
        .skip(1)
        .any(|(score, _)| score == best_score)
    {
        return Err(format!(
            "Multiple XFCE4 Terminal tabs match this window (window UUID {})",
            best.window_uuid
        ));
    }

    Ok(best.tab_uuid.clone())
}

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
pub(crate) fn replace_terminal_suffix_path(original_suffix: &str, cwd: &str) -> String {
    let trimmed = original_suffix.trim();
    if trimmed.is_empty() {
        return cwd.to_string();
    }

    match trimmed.rfind(char::is_whitespace) {
        Some(split_at) => format!("{} {}", trimmed[..split_at].trim_end(), cwd),
        None => cwd.to_string(),
    }
}

pub(crate) fn push_unique_terminal_segment(segments: &mut Vec<String>, value: impl Into<String>) {
    let value = value.into();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }

    let key = normalize_app_match_key(trimmed);
    let already_present = if key.is_empty() {
        segments
            .iter()
            .any(|existing| existing.trim().eq_ignore_ascii_case(trimmed))
    } else {
        segments
            .iter()
            .any(|existing| normalize_app_match_key(existing) == key)
    };
    if already_present {
        return;
    }

    segments.push(trimmed.to_string());
}

pub(crate) fn normalize_terminal_title_marker_position(raw_title: &str) -> String {
    let separators = [" - ", " — ", " – ", " : ", " | "];

    for sep in separators {
        let parts: Vec<&str> = raw_title.split(sep).map(str::trim).collect();
        if parts.len() < 2 {
            continue;
        }

        let first_is_marker = parts
            .first()
            .is_some_and(|part| is_terminal_title_marker(part));
        let last_is_marker = parts
            .last()
            .is_some_and(|part| is_terminal_title_marker(part));

        if first_is_marker && last_is_marker && parts.len() >= 3 {
            return parts[1..].join(sep);
        }

        if first_is_marker {
            let body = parts[1..]
                .iter()
                .copied()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            if !body.is_empty() {
                let mut rebuilt = body;
                rebuilt.push("Terminal");
                return rebuilt.join(sep);
            }
        }
    }

    raw_title.trim().to_string()
}
