use std::collections::HashMap;
use std::path::Path;

use crate::models::ProcessChainEntry;

pub(crate) type ProcessTree = (
    HashMap<i32, Vec<i32>>,
    HashMap<i32, String>,
    HashMap<i32, i32>,
);

#[derive(Clone)]
pub(crate) struct ProcessStat {
    pub(crate) pid: i32,
    pub(crate) name: String,
    pub(crate) ppid: i32,
    pub(crate) process_group: i32,
    pub(crate) session: i32,
    pub(crate) tty: i32,
    pub(crate) foreground_process_group: i32,
}

pub(crate) fn parse_proc_stat(stat_content: &str) -> Option<ProcessStat> {
    let last_paren = stat_content.rfind(')')?;
    let (left, right) = stat_content.split_at(last_paren);

    let pid_part = left.split_whitespace().next()?;
    let pid: i32 = pid_part.parse().ok()?;

    let name_start = left.find('(')? + 1;
    let name = left[name_start..].to_string();

    let tokens: Vec<&str> = right[1..].split_whitespace().collect();
    if tokens.len() < 6 {
        return None;
    }

    Some(ProcessStat {
        pid,
        name,
        ppid: tokens[1].parse().ok()?,
        process_group: tokens[2].parse().ok()?,
        session: tokens[3].parse().ok()?,
        tty: tokens[4].parse().ok()?,
        foreground_process_group: tokens[5].parse().ok()?,
    })
}

pub(crate) fn read_process_stat(pid: i32) -> Option<ProcessStat> {
    let content = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_proc_stat(&content)
}

pub(crate) fn process_pty_path(pid: i32) -> Option<String> {
    let path = std::fs::read_link(format!("/proc/{pid}/fd/0")).ok()?;
    let path = path.to_string_lossy();
    path.starts_with("/dev/pts/").then(|| path.into_owned())
}

pub(crate) fn is_terminal_foreground_process(
    process: &ProcessStat,
    terminal: &ProcessStat,
) -> bool {
    terminal.tty > 0
        && terminal.foreground_process_group > 0
        && process.session == terminal.session
        && process.tty == terminal.tty
        && process.process_group == terminal.foreground_process_group
}

pub(crate) fn get_process_tree() -> ProcessTree {
    let mut ppid_to_children = HashMap::new();
    let mut pid_to_name = HashMap::new();
    let mut pid_to_ppid = HashMap::new();

    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries {
            if let Ok(entry) = entry {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.chars().all(|c| c.is_ascii_digit()) {
                            let stat_path = path.join("stat");
                            if let Ok(content) = std::fs::read_to_string(stat_path) {
                                if let Some(stat) = parse_proc_stat(&content) {
                                    pid_to_name.insert(stat.pid, stat.name);
                                    pid_to_ppid.insert(stat.pid, stat.ppid);
                                    ppid_to_children
                                        .entry(stat.ppid)
                                        .or_insert_with(Vec::new)
                                        .push(stat.pid);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    (ppid_to_children, pid_to_name, pid_to_ppid)
}

pub(crate) fn is_shell(name: &str) -> bool {
    let n = name.to_lowercase();
    n == "bash"
        || n == "fish"
        || n == "zsh"
        || n == "sh"
        || n == "dash"
        || n == "tcsh"
        || n == "ksh"
}

pub(crate) fn find_terminal_leaf(
    terminal_pid: i32,
    ppid_to_children: &HashMap<i32, Vec<i32>>,
    pid_to_name: &HashMap<i32, String>,
) -> Option<(i32, String)> {
    find_terminal_leaf_with_stat_reader(
        terminal_pid,
        ppid_to_children,
        pid_to_name,
        read_process_stat,
    )
}

pub(crate) fn find_terminal_leaf_with_stat_reader(
    terminal_pid: i32,
    ppid_to_children: &HashMap<i32, Vec<i32>>,
    pid_to_name: &HashMap<i32, String>,
    mut stat_for_pid: impl FnMut(i32) -> Option<ProcessStat>,
) -> Option<(i32, String)> {
    let children = ppid_to_children.get(&terminal_pid)?;
    let root_pid = children
        .iter()
        .filter_map(|pid| pid_to_name.get(pid).map(|name| (*pid, name)))
        .min_by_key(|(pid, name)| (!is_shell(name), *pid))
        .map(|(pid, _)| pid)?;
    let root_name = pid_to_name.get(&root_pid)?.clone();
    let Some(terminal_stat) = stat_for_pid(root_pid) else {
        return Some((root_pid, root_name));
    };

    let mut best_foreground = is_terminal_foreground_process(&terminal_stat, &terminal_stat)
        .then(|| (0_usize, root_pid, root_name.clone()));
    let mut pending = ppid_to_children
        .get(&root_pid)
        .into_iter()
        .flatten()
        .map(|pid| (*pid, 1_usize))
        .collect::<Vec<_>>();

    while let Some((pid, depth)) = pending.pop() {
        let Some(process_stat) = stat_for_pid(pid) else {
            continue;
        };

        // A process cannot return to the terminal's session after detaching from it,
        // so its entire subtree is irrelevant to foreground command selection.
        if process_stat.session != terminal_stat.session || process_stat.tty != terminal_stat.tty {
            continue;
        }

        if is_terminal_foreground_process(&process_stat, &terminal_stat) {
            let replace_best = best_foreground
                .as_ref()
                .is_none_or(|(best_depth, best_pid, _)| (depth, pid) > (*best_depth, *best_pid));
            if replace_best {
                if let Some(name) = pid_to_name.get(&pid) {
                    best_foreground = Some((depth, pid, name.clone()));
                }
            }
        }

        if let Some(children) = ppid_to_children.get(&pid) {
            pending.extend(children.iter().map(|child| (*child, depth + 1)));
        }
    }

    best_foreground
        .map(|(_, pid, name)| (pid, name))
        .or(Some((root_pid, root_name)))
}

pub(crate) fn build_process_chain(
    start_pid: i32,
    pid_to_name: &HashMap<i32, String>,
    pid_to_ppid: &HashMap<i32, i32>,
) -> Vec<ProcessChainEntry> {
    let mut chain = Vec::new();
    let mut current_pid = Some(start_pid);

    while let Some(pid) = current_pid {
        let name = pid_to_name
            .get(&pid)
            .cloned()
            .unwrap_or_else(|| pid.to_string());
        let exe_path = std::fs::read_link(format!("/proc/{}/exe", pid)).ok();
        chain.push(ProcessChainEntry {
            pid,
            name,
            exe_path,
        });

        current_pid = pid_to_ppid
            .get(&pid)
            .copied()
            .filter(|ppid| *ppid > 0 && *ppid != pid);
    }

    chain
}
pub(crate) fn display_path(path: &Path) -> String {
    if let Ok(home) = std::env::var("HOME") {
        let home_path = Path::new(&home);
        if let Ok(stripped) = path.strip_prefix(home_path) {
            if stripped.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", stripped.to_string_lossy());
        }
    }
    path.to_string_lossy().to_string()
}

pub(crate) fn normalize_metadata_search_value(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    for ch in value.chars() {
        let mapped = match ch {
            '—' | '–' | '−' => '-',
            '•' | '·' | '●' | '▪' | '◦' | '‣' => ' ',
            c if c.is_ascii() => c,
            _ => ' ',
        };
        normalized.push(mapped);
    }

    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn read_proc_cmdline(pid: i32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{}/cmdline", pid)).ok()?;
    let args = raw
        .split(|byte| *byte == 0)
        .filter_map(|part| {
            if part.is_empty() {
                return None;
            }
            std::str::from_utf8(part)
                .ok()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string())
        })
        .collect::<Vec<_>>();
    (!args.is_empty()).then_some(args)
}

pub(crate) fn compact_command_part(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let candidate = trimmed.trim_end_matches('/');
    Path::new(candidate)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

pub(crate) fn summarize_command_line(args: &[String]) -> Option<String> {
    let first = args.first()?;
    let mut summary = vec![compact_command_part(first)];

    for arg in args.iter().skip(1) {
        if summary.len() >= 3 {
            break;
        }
        if arg.trim().is_empty() || arg.starts_with('-') {
            continue;
        }
        let compact = compact_command_part(arg);
        if compact.is_empty() || summary.iter().any(|part| part == &compact) {
            continue;
        }
        summary.push(compact);
    }

    (!summary.is_empty()).then_some(summary.join(" "))
}
pub(crate) fn process_exists(pid: i32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}
