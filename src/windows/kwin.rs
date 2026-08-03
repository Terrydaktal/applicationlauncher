use eframe::egui;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::models::{
    KWinWindowPayload, SnapshotWindowDetails, TerminalDbusRecord, WindowFeedEvent, WindowInfo,
};
use crate::*;

const TRACKER_WINDOW_FEED_POLL_INTERVAL: Duration = Duration::from_millis(50);
const TRACKER_WINDOW_FEED_RECONNECT_DELAY: Duration = Duration::from_millis(500);

pub(crate) fn get_kdotool_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(format!("{home}/.cargo/bin/kdotool"));
        if path.exists() {
            return path;
        }
    }
    PathBuf::from("kdotool")
}

pub(crate) fn build_window_info(
    id: String,
    title: String,
    class: String,
    desktop_file_name: Option<String>,
    pid: Option<i32>,
    geometry: Option<(i32, i32, i32, i32)>,
    minimized: Option<bool>,
    theme: &str,
    icon_cache: &mut HashMap<WindowIconCacheKey, Option<PathBuf>>,
    ppid_to_children: &HashMap<i32, Vec<i32>>,
    pid_to_name: &HashMap<i32, String>,
    pid_to_ppid: &HashMap<i32, i32>,
    terminal_records: &[TerminalDbusRecord],
) -> Option<WindowInfo> {
    let class_lower = class.to_lowercase();
    let my_pid = std::process::id() as i32;

    if class_lower.contains("plasmashell")
        || class_lower == "kwin_wayland"
        || class_lower.is_empty()
        || title.trim().is_empty()
        || class_lower == "applicationlauncher"
        || title == "Open Application Windows"
        || pid == Some(my_pid)
    {
        return None;
    }

    if let Some(pid) = pid {
        if !process_exists(pid) {
            return None;
        }
    }

    let raw_title = title.clone();
    let display_title = title;

    let mut active_process = None;
    let mut exe_path = None;
    let mut cwd_path = None;
    let mut command_line = None;
    let mut command_summary = None;
    let mut process_chain = Vec::new();
    if let Some(pid) = pid {
        let mut target_pid = pid;
        if is_terminal_class(&class_lower) {
            if let Some(record) = terminal_record_for_window_title(&raw_title, terminal_records) {
                let record_target = [record.foreground_pid, record.child_pid]
                    .into_iter()
                    .filter_map(|candidate| i32::try_from(candidate).ok())
                    .find(|candidate| *candidate > 0 && process_exists(*candidate));
                if let Some(record_target) = record_target {
                    target_pid = record_target;
                    active_process = pid_to_name
                        .get(&record_target)
                        .cloned()
                        .or_else(|| read_process_stat(record_target).map(|stat| stat.name));
                }
                if !record.working_directory.is_empty() {
                    cwd_path = Some(PathBuf::from(&record.working_directory));
                }
            } else if !terminal_server_has_dbus_records(pid, terminal_records, pid_to_ppid)
                && let Some((leaf_pid, leaf_name)) =
                    find_terminal_leaf(pid, ppid_to_children, pid_to_name)
            {
                active_process = Some(leaf_name);
                target_pid = leaf_pid;
            }
        }

        if let Ok(path) = std::fs::read_link(format!("/proc/{}/exe", pid)) {
            exe_path = Some(path);
        }

        if cwd_path.is_none()
            && let Ok(path) = std::fs::read_link(format!("/proc/{}/cwd", target_pid))
        {
            cwd_path = Some(path);
        }

        if let Some(args) = read_proc_cmdline(target_pid) {
            command_summary = summarize_command_line(&args);
            command_line = Some(args.join(" "));
        }

        process_chain = build_process_chain(target_pid, pid_to_name, pid_to_ppid);
    }

    let mut final_title = display_title;
    if let Some(ref proc_name) = active_process {
        if is_terminal_class(&class_lower) {
            let terminal_suffix = cwd_path.as_ref().map(|path| display_path(path));
            let parent_program = terminal_parent_program(proc_name, &process_chain);
            final_title = terminal_display_title(
                &final_title,
                proc_name,
                command_summary.as_deref(),
                terminal_suffix.as_deref(),
                parent_program,
            );
        } else {
            let separators = [" - ", " — ", " – ", " : ", " | "];
            let mut split_found = false;
            for sep in separators {
                if let Some(pos) = final_title.find(sep) {
                    let (left, right) = final_title.split_at(pos);
                    let original_suffix = &right[sep.len()..];
                    final_title = format!(
                        "{}{}{}{}{}",
                        left.trim(),
                        sep,
                        proc_name,
                        sep,
                        original_suffix.trim()
                    );
                    split_found = true;
                    break;
                }
            }
            if !split_found {
                final_title = format!("{} - {}", final_title, proc_name);
            }
        }
    } else if is_terminal_class(&class_lower) {
        final_title = normalize_terminal_title_marker_position(&final_title);
    }
    if is_pcmanfm_class(&class_lower) {
        let title_key = normalize_app_match_key(&final_title);
        if !title_key.ends_with("pcmanfm") {
            final_title = format!("{} — PCManFM", final_title.trim());
        }
    }

    let icon_key = window_icon_cache_key(
        &class,
        desktop_file_name.as_deref(),
        active_process.as_deref(),
        exe_path.as_deref(),
    );
    let icon_path = icon_cache
        .entry(icon_key)
        .or_insert_with(|| {
            resolve_window_icon(
                theme,
                &class,
                desktop_file_name.as_deref(),
                active_process.as_deref(),
                exe_path.as_deref(),
            )
        })
        .clone();

    Some(WindowInfo {
        id,
        title: final_title,
        raw_title,
        class,
        desktop_file_name,
        minimized,
        demands_attention: false,
        icon_path,
        active_process,
        exe_path,
        cwd_path,
        command_line,
        command_summary,
        geometry,
        process_chain,
        pid,
        last_activated_at_ms: None,
        activation_sequence: 0,
    })
}

pub(crate) fn setup_kwin_window_feed(
    tx: Sender<WindowFeedEvent>,
    repaint_ctx: egui::Context,
) -> Result<(), String> {
    applicationlauncher::tracker::ensure_tracker_installed()?;
    let mut tracker_ready = false;
    for _ in 0..30 {
        if applicationlauncher::tracker::TrackerClient::connect()
            .and_then(|client| client.status())
            .is_ok()
        {
            tracker_ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !tracker_ready {
        return Err("The tracker service did not become ready within three seconds".into());
    }

    std::thread::spawn(move || {
        let mut last_generation = u64::MAX;
        let mut client = None;
        let mut previous_payloads = None;
        loop {
            if client.is_none() {
                match applicationlauncher::tracker::TrackerClient::connect() {
                    Ok(connected) => client = Some(connected),
                    Err(err) => {
                        eprintln!("Application Launcher tracker connection lost: {err}");
                        std::thread::sleep(TRACKER_WINDOW_FEED_RECONNECT_DELAY);
                        continue;
                    }
                }
            }

            let result = client.as_ref().unwrap().status().and_then(|status| {
                if status.generation == last_generation {
                    return Ok(None);
                }
                client
                    .as_ref()
                    .unwrap()
                    .windows()
                    .map(|windows| Some((status.generation, windows)))
            });

            match result {
                Ok(Some((generation, windows))) => {
                    let payloads = windows
                        .into_iter()
                        .map(|window| KWinWindowPayload {
                            id: window.id,
                            title: window.title,
                            class: window.class,
                            pid: window.pid,
                            desktop_file_name: window.desktop_file_name,
                            x: window.x,
                            y: window.y,
                            width: window.width,
                            height: window.height,
                            minimized: window.minimized,
                            demands_attention: window.demands_attention,
                            last_activated_at_ms: window.last_activated_at_ms,
                            activation_sequence: window.activation_sequence,
                        })
                        .collect::<Vec<_>>();
                    let (events, current_payloads) =
                        window_feed_events_from_snapshot(previous_payloads.as_ref(), payloads);
                    let has_events = !events.is_empty();
                    for event in events {
                        let _ = tx.send(event);
                    }
                    previous_payloads = Some(current_payloads);
                    last_generation = generation;
                    if has_events {
                        repaint_ctx.request_repaint();
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    eprintln!("Application Launcher tracker connection lost: {err}");
                    last_generation = u64::MAX;
                    client = None;
                    previous_payloads = None;
                    std::thread::sleep(TRACKER_WINDOW_FEED_RECONNECT_DELAY);
                    continue;
                }
            }
            std::thread::sleep(TRACKER_WINDOW_FEED_POLL_INTERVAL);
        }
    });
    Ok(())
}

pub(crate) fn window_feed_events_from_snapshot(
    previous: Option<&HashMap<String, KWinWindowPayload>>,
    payloads: Vec<KWinWindowPayload>,
) -> (Vec<WindowFeedEvent>, HashMap<String, KWinWindowPayload>) {
    let current = payloads
        .iter()
        .cloned()
        .map(|payload| (payload.id.clone(), payload))
        .collect::<HashMap<_, _>>();
    let Some(previous) = previous else {
        return (vec![WindowFeedEvent::Snapshot(payloads)], current);
    };

    let mut events = payloads
        .into_iter()
        .filter(|payload| previous.get(&payload.id) != Some(payload))
        .map(WindowFeedEvent::Upsert)
        .collect::<Vec<_>>();
    events.extend(
        previous
            .keys()
            .filter(|id| !current.contains_key(*id))
            .cloned()
            .map(WindowFeedEvent::Remove),
    );
    (events, current)
}

pub(crate) fn window_info_from_kwin_payload(
    payload: KWinWindowPayload,
    theme: &str,
    icon_cache: &mut HashMap<WindowIconCacheKey, Option<PathBuf>>,
    ppid_to_children: &HashMap<i32, Vec<i32>>,
    pid_to_name: &HashMap<i32, String>,
    pid_to_ppid: &HashMap<i32, i32>,
    terminal_records: &[TerminalDbusRecord],
) -> Option<WindowInfo> {
    let desktop_file_name_value = payload.desktop_file_name.trim().to_string();
    let class = if payload.class.trim().is_empty() {
        desktop_file_name_value.clone()
    } else {
        payload.class
    };
    let pid = (payload.pid > 0).then_some(payload.pid);
    let desktop_file_name =
        (!desktop_file_name_value.is_empty()).then_some(desktop_file_name_value);
    let geometry = (payload.width > 0 && payload.height > 0).then_some((
        payload.x,
        payload.y,
        payload.width,
        payload.height,
    ));
    let minimized = Some(payload.minimized);
    let mut window = build_window_info(
        payload.id,
        payload.title,
        class,
        desktop_file_name,
        pid,
        geometry,
        minimized,
        theme,
        icon_cache,
        ppid_to_children,
        pid_to_name,
        pid_to_ppid,
        terminal_records,
    )?;
    window.demands_attention = payload.demands_attention;
    window.last_activated_at_ms = payload.last_activated_at_ms;
    window.activation_sequence = payload.activation_sequence;
    Some(window)
}

pub(crate) fn coalesce_window_feed_events(events: Vec<WindowFeedEvent>) -> Vec<WindowFeedEvent> {
    let mut expanded = Vec::new();
    for event in events {
        match event {
            WindowFeedEvent::Snapshot(payloads) => {
                expanded.push(WindowFeedEvent::Reset);
                expanded.extend(payloads.into_iter().map(WindowFeedEvent::Upsert));
            }
            event => expanded.push(event),
        }
    }

    let mut latest_by_id: HashMap<String, WindowFeedEvent> = HashMap::new();
    let mut order = Vec::new();
    let reset_at = expanded
        .iter()
        .rposition(|event| matches!(event, WindowFeedEvent::Reset));
    let mut coalesced = reset_at
        .map(|_| vec![WindowFeedEvent::Reset])
        .unwrap_or_default();

    for event in expanded
        .into_iter()
        .skip(reset_at.map_or(0, |index| index + 1))
    {
        let Some(id) = (match &event {
            WindowFeedEvent::Upsert(payload) => Some(payload.id.clone()),
            WindowFeedEvent::Remove(id) => Some(id.clone()),
            WindowFeedEvent::Reset
            | WindowFeedEvent::Snapshot(_)
            | WindowFeedEvent::RearmAttentionAutomation => None,
        }) else {
            continue;
        };
        if !latest_by_id.contains_key(&id) {
            order.push(id.clone());
        }
        latest_by_id.insert(id, event);
    }

    coalesced.extend(order.into_iter().filter_map(|id| latest_by_id.remove(&id)));
    coalesced
}

pub(crate) fn get_open_windows_with_snapshot_mode(
    kdotool_path: &Path,
    theme: &str,
    include_snapshot_details: bool,
) -> Option<Vec<WindowInfo>> {
    // 1. Fetch all window IDs using kdotool search
    let output = match Command::new(kdotool_path)
        .arg("search")
        .arg("--title")
        .arg("")
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Failed to execute kdotool search: {:?}", e);
            return None;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        eprintln!(
            "kdotool window search failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return None;
    }

    let mut ids = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if !line.is_empty() {
            ids.push(line.to_string());
        }
    }

    if ids.is_empty() {
        return None;
    }

    // 2. Query all window metadata in a single chained kdotool invocation!
    // This reduces process spawning from N*3 down to exactly 1, eliminating startup lag.
    let mut cmd = Command::new(kdotool_path);
    for id in &ids {
        cmd.arg("getwindowid")
            .arg(id)
            .arg("getwindowname")
            .arg(id)
            .arg("getwindowclassname")
            .arg(id)
            .arg("getwindowpid")
            .arg(id);
    }

    let meta_output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!(
                "Failed to execute chained kdotool metadata command: {:?}",
                e
            );
            return None;
        }
    };

    if !meta_output.status.success() {
        eprintln!(
            "kdotool window metadata query failed with {}: {}",
            meta_output.status,
            String::from_utf8_lossy(&meta_output.stderr).trim()
        );
        return None;
    }

    let meta_stdout = String::from_utf8_lossy(&meta_output.stdout);
    let lines: Vec<&str> = meta_stdout.lines().collect();

    // 3. Scan /proc once to build process tree before querying PIDs
    let (ppid_to_children, pid_to_name, pid_to_ppid) = get_process_tree();
    let terminal_records = fetch_terminal_dbus_records().unwrap_or_default();

    let mut windows = Vec::new();
    let theme_str = theme.to_string();
    let mut icon_cache = HashMap::new();

    // Parse blocks of metadata. Since invalid windows get skipped, we search for UUID patterns
    // to identify the start of each valid window's metadata block.
    let mut window_blocks = Vec::new();
    let mut current_block = Vec::new();

    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with('{') && trimmed.ends_with('}') {
            if !current_block.is_empty() {
                window_blocks.push(current_block);
            }
            current_block = vec![trimmed.to_string()];
        } else {
            current_block.push(line.to_string());
        }
    }
    if !current_block.is_empty() {
        window_blocks.push(current_block);
    }

    for block in window_blocks {
        if block.is_empty() {
            continue;
        }
        let id = block[0].clone();

        let mut title = String::new();
        let mut class = String::new();
        let mut pid = None;

        if block.len() >= 2 {
            let last_line = block.last().unwrap().trim();
            if let Ok(p) = last_line.parse::<i32>() {
                pid = Some(p);
                if block.len() >= 3 {
                    class = block[block.len() - 2].trim().to_string();
                    if block.len() > 3 {
                        title = block[1..block.len() - 2].join(" ").trim().to_string();
                    }
                }
            } else {
                class = block.get(2).cloned().unwrap_or_default();
                title = block.get(1).cloned().unwrap_or_default();
            }
        }

        let snapshot_details = if include_snapshot_details {
            get_snapshot_window_details(&id)
        } else {
            SnapshotWindowDetails {
                desktop_file_name: None,
                geometry: None,
                minimized: None,
            }
        };

        if let Some(window) = build_window_info(
            id,
            title,
            class,
            snapshot_details.desktop_file_name,
            pid,
            snapshot_details.geometry,
            snapshot_details.minimized,
            &theme_str,
            &mut icon_cache,
            &ppid_to_children,
            &pid_to_name,
            &pid_to_ppid,
            &terminal_records,
        ) {
            windows.push(window);
        }
    }

    Some(windows)
}

pub(crate) fn get_open_windows(kdotool_path: &Path, theme: &str) -> Option<Vec<WindowInfo>> {
    get_open_windows_with_snapshot_mode(kdotool_path, theme, true)
}

pub(crate) fn get_open_windows_fast(kdotool_path: &Path, theme: &str) -> Option<Vec<WindowInfo>> {
    get_open_windows_with_snapshot_mode(kdotool_path, theme, false)
}

pub(crate) fn merge_reconciled_window(
    existing: &WindowInfo,
    mut discovered: WindowInfo,
) -> WindowInfo {
    if discovered.desktop_file_name.is_none() {
        discovered.desktop_file_name = existing.desktop_file_name.clone();
    }
    if discovered.geometry.is_none() {
        discovered.geometry = existing.geometry;
    }
    if discovered.minimized.is_none() {
        discovered.minimized = existing.minimized;
    }
    if discovered.icon_path.is_none() {
        discovered.icon_path = existing.icon_path.clone();
    }
    // Fast snapshots cannot inspect these KWin-only state fields.
    discovered.demands_attention = existing.demands_attention;
    if discovered.last_activated_at_ms.is_none() {
        discovered.last_activated_at_ms = existing.last_activated_at_ms;
    }
    if discovered.activation_sequence == 0 {
        discovered.activation_sequence = existing.activation_sequence;
    }
    discovered
}

pub(crate) fn merge_reconciled_windows(
    current: &mut Vec<WindowInfo>,
    discovered: Vec<WindowInfo>,
) -> (bool, bool, Vec<(WindowInfo, WindowInfo)>) {
    let mut changed = false;
    let mut search_changed = false;
    let mut cache_updates = Vec::new();
    let mut index_by_id: HashMap<String, usize> = current
        .iter()
        .enumerate()
        .map(|(index, window)| (window.id.clone(), index))
        .collect();

    for discovered_window in discovered {
        if let Some(index) = index_by_id.get(&discovered_window.id).copied() {
            let old_window = current[index].clone();
            let merged_window = merge_reconciled_window(&old_window, discovered_window);
            if old_window != merged_window {
                let window_search_changed =
                    !window_search_metadata_equal(&old_window, &merged_window);
                search_changed |= window_search_changed;
                current[index] = merged_window.clone();
                if !window_search_changed {
                    cache_updates.push((old_window, merged_window));
                }
                changed = true;
            }
        } else {
            index_by_id.insert(discovered_window.id.clone(), current.len());
            current.push(discovered_window);
            changed = true;
            search_changed = true;
        }
    }

    (changed, search_changed, cache_updates)
}

pub(crate) fn load_pinned_apps() -> Vec<PathBuf> {
    let mut pinned = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(format!(
            "{}/.config/applicationlauncher/pinned_apps.txt",
            home
        ));
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(path) {
                for line in content.lines() {
                    let line = line.trim();
                    if !line.is_empty() {
                        let p = PathBuf::from(line);
                        if !pinned.contains(&p) {
                            pinned.push(p);
                        }
                    }
                }
            }
        }
    }
    pinned
}

pub(crate) fn get_window_geometry(kpath: &Path, id: &str) -> Option<(f32, f32, f32, f32)> {
    let output = Command::new(kpath)
        .args(["getwindowgeometry", id])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut x = None;
    let mut y = None;
    let mut width = None;
    let mut height = None;

    for line in stdout.lines() {
        let line = line.trim();
        if line.starts_with("Position:") {
            let pos_part = line.strip_prefix("Position:")?.trim();
            let coords: Vec<&str> = pos_part.split(',').collect();
            if coords.len() >= 2 {
                x = coords[0].parse::<f32>().ok();
                y = coords[1].parse::<f32>().ok();
            }
        } else if line.starts_with("Geometry:") {
            let geom_part = line.strip_prefix("Geometry:")?.trim();
            let dims: Vec<&str> = geom_part.split('x').collect();
            if dims.len() >= 2 {
                width = dims[0].parse::<f32>().ok();
                height = dims[1].parse::<f32>().ok();
            }
        }
    }

    Some((x?, y?, width?, height?))
}

pub(crate) fn get_snapshot_window_details(id: &str) -> SnapshotWindowDetails {
    let output = Command::new("qdbus6")
        .args(["org.kde.KWin", "/KWin", "org.kde.KWin.getWindowInfo", id])
        .output();

    let Ok(output) = output else {
        return SnapshotWindowDetails {
            desktop_file_name: None,
            geometry: None,
            minimized: None,
        };
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut desktop_file_name = None;
    let mut minimized = None;
    let mut x = None;
    let mut y = None;
    let mut width = None;
    let mut height = None;

    for line in stdout.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("desktopFile:") {
            let value = value.trim();
            if !value.is_empty() {
                desktop_file_name = Some(value.to_string());
            }
        } else if let Some(value) = line.strip_prefix("minimized:") {
            minimized = match value.trim() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            };
        } else if let Some(value) = line.strip_prefix("x:") {
            x = value.trim().parse::<f64>().ok().map(|v| v.round() as i32);
        } else if let Some(value) = line.strip_prefix("y:") {
            y = value.trim().parse::<f64>().ok().map(|v| v.round() as i32);
        } else if let Some(value) = line.strip_prefix("width:") {
            width = value.trim().parse::<f64>().ok().map(|v| v.round() as i32);
        } else if let Some(value) = line.strip_prefix("height:") {
            height = value.trim().parse::<f64>().ok().map(|v| v.round() as i32);
        }
    }

    SnapshotWindowDetails {
        desktop_file_name,
        geometry: match (x, y, width, height) {
            (Some(x), Some(y), Some(width), Some(height)) if width > 0 && height > 0 => {
                Some((x, y, width, height))
            }
            _ => None,
        },
        minimized,
    }
}
