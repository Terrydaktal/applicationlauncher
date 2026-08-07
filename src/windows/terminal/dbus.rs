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
use crate::models::{TerminalDbusRecord, TerminalWindowIdentity, WindowInfo};
use crate::{
    TERMINAL_DBUS_INTERFACE, TERMINAL_DBUS_PATH, TERMINAL_DBUS_SERVICE,
    normalize_window_sort_title, process_pty_path, read_process_stat,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
