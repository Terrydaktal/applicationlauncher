use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl WindowGeometry {
    pub fn is_valid(self) -> bool {
        self.width > 0 && self.height > 0
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OutputGeometry {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    #[serde(default = "default_scale_milli")]
    pub scale_milli: i32,
}

fn default_scale_milli() -> i32 {
    1_000
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrackedWindow {
    pub id: String,
    pub title: String,
    pub class: String,
    #[serde(default)]
    pub pid: i32,
    #[serde(default)]
    pub desktop_file_name: String,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
    #[serde(default)]
    pub normal_geometry: Option<WindowGeometry>,
    #[serde(default)]
    pub minimized: bool,
    #[serde(default)]
    pub maximized: bool,
    #[serde(default)]
    pub maximized_horizontally: bool,
    #[serde(default)]
    pub maximized_vertically: bool,
    #[serde(default)]
    pub fullscreen: bool,
    #[serde(default)]
    pub demands_attention: bool,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub skip_taskbar: bool,
    #[serde(default)]
    pub skip_switcher: bool,
    #[serde(default)]
    pub desktop: i32,
    #[serde(default)]
    pub on_all_desktops: bool,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub output_geometry: Option<OutputGeometry>,
    #[serde(default)]
    pub activities: Vec<String>,
    #[serde(default)]
    pub stacking_order: i32,
    #[serde(default)]
    pub keep_above: bool,
    #[serde(default)]
    pub keep_below: bool,
    #[serde(default)]
    pub shaded: bool,
    #[serde(default)]
    pub skip_pager: bool,
    #[serde(default)]
    pub no_border: bool,
    #[serde(default)]
    pub opened_at_ms: i64,
    #[serde(default)]
    pub updated_at_ms: i64,
    #[serde(default)]
    pub last_activated_at_ms: Option<i64>,
    #[serde(default)]
    pub activation_sequence: i64,
}

pub fn is_compact_chromium_helper_surface(
    class: &str,
    desktop_file_name: Option<&str>,
    width: i32,
) -> bool {
    let identity = desktop_file_name
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(class);
    (1..=200).contains(&width)
        && has_chromium_isolated_app_id(identity)
        && !desktop_entry_exists(identity)
}

fn has_chromium_isolated_app_id(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    ["google-chrome-", "chrome-", "chromium-", "brave-browser-"]
        .into_iter()
        .filter_map(|prefix| value.strip_prefix(prefix))
        .filter_map(|suffix| suffix.split('-').next())
        .any(|id| id.len() == 32 && id.bytes().all(|byte| (b'a'..=b'p').contains(&byte)))
}

fn desktop_entry_exists(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    let path = std::path::Path::new(value);
    if path.is_absolute() {
        return path.is_file();
    }
    let file_name = if value.ends_with(".desktop") {
        value.to_string()
    } else {
        format!("{value}.desktop")
    };
    let mut directories = Vec::new();
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
        directories.push(std::path::PathBuf::from(data_home).join("applications"));
    } else if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::PathBuf::from(home);
        directories.push(home.join(".local/share/applications"));
        directories.push(home.join(".local/share/flatpak/exports/share/applications"));
    }
    directories.extend(
        std::env::var_os("XDG_DATA_DIRS")
            .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
            .unwrap_or_else(|| {
                vec![
                    std::path::PathBuf::from("/usr/local/share"),
                    std::path::PathBuf::from("/usr/share"),
                ]
            })
            .into_iter()
            .map(|path| path.join("applications")),
    );
    directories.push(std::path::PathBuf::from(
        "/var/lib/flatpak/exports/share/applications",
    ));
    directories
        .into_iter()
        .any(|directory| directory.join(&file_name).is_file())
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct RestoreSpec {
    pub app_key: String,
    pub desktop_file: Option<String>,
    pub executable: Option<String>,
    pub cwd: Option<String>,
    pub terminal_kind: Option<String>,
    pub safe_arguments: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: i64,
    pub window: TrackedWindow,
    pub closed_at_ms: i64,
    pub restore: RestoreSpec,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SnapshotSummary {
    pub id: i64,
    pub name: Option<String>,
    pub kind: String,
    pub created_at_ms: i64,
    pub window_count: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SnapshotDetail {
    pub summary: SnapshotSummary,
    pub windows: Vec<(TrackedWindow, RestoreSpec)>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct TrackerStatus {
    pub generation: u64,
    pub history_generation: u64,
    pub window_count: usize,
    pub recovery_pending: bool,
    pub database_path: String,
    pub run_id: String,
    pub build_id: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct RestoreReport {
    #[serde(default)]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub in_progress: bool,
    #[serde(default)]
    pub started_at_ms: i64,
    #[serde(default)]
    pub finished_at_ms: Option<i64>,
    pub matched: usize,
    pub launched: usize,
    pub failures: Vec<String>,
    #[serde(default)]
    pub outcomes: Vec<WindowRestoreOutcome>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct WindowRestoreOutcome {
    pub title: String,
    pub saved_window_id: String,
    #[serde(default)]
    pub restored_window_id: Option<String>,
    #[serde(default)]
    pub status: RestoreOutcomeStatus,
    #[serde(default)]
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RestoreOutcomeStatus {
    #[default]
    Pending,
    Exact,
    Adjusted,
    Failed,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

pub fn app_key(window: &TrackedWindow) -> String {
    let desktop = window.desktop_file_name.trim();
    if !desktop.is_empty() {
        return desktop.trim_end_matches(".desktop").to_lowercase();
    }
    window
        .class
        .split_whitespace()
        .last()
        .unwrap_or_default()
        .trim()
        .to_lowercase()
}

pub fn infer_restore_spec(window: &TrackedWindow) -> RestoreSpec {
    let key = app_key(window);
    let terminal = key.contains("terminal") || window.class.to_lowercase().contains("terminal");
    let title = window.title.to_lowercase();
    let process = terminal.then(|| terminal_process_details(window.pid));
    let terminal_kind = terminal.then(|| {
        let process_name = process
            .as_ref()
            .map(|details| details.0.as_str())
            .unwrap_or("");
        let process_executable = process.as_ref().and_then(|details| details.2.as_deref());
        if title.contains("codex")
            || terminal_process_matches(process_name, process_executable, "codex")
        {
            "codex"
        } else if title.contains("agy")
            || terminal_process_matches(process_name, process_executable, "agy")
        {
            "agy"
        } else if title.contains("htop")
            || terminal_process_matches(process_name, process_executable, "htop")
        {
            "htop"
        } else if title.contains("nvtop")
            || terminal_process_matches(process_name, process_executable, "nvtop")
        {
            "nvtop"
        } else {
            "shell"
        }
        .to_string()
    });
    RestoreSpec {
        app_key: key,
        desktop_file: (!window.desktop_file_name.trim().is_empty())
            .then(|| window.desktop_file_name.clone()),
        executable: process.as_ref().and_then(|details| details.2.clone()),
        cwd: process
            .as_ref()
            .and_then(|details| details.1.clone())
            .or_else(|| terminal.then(|| title_path_hint(&window.title)).flatten()),
        terminal_kind,
        safe_arguments: Vec::new(),
    }
}

pub(crate) fn terminal_process_matches(
    name: &str,
    executable: Option<&str>,
    expected: &str,
) -> bool {
    let matches = |value: &str| {
        let normalized = value
            .chars()
            .filter(|character| character.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>();
        normalized == expected || (expected == "codex" && normalized.starts_with("codexcodemode"))
    };
    matches(name)
        || executable
            .and_then(|path| std::path::Path::new(path).file_name())
            .and_then(|name| name.to_str())
            .is_some_and(matches)
}

fn terminal_process_details(root_pid: i32) -> (String, Option<String>, Option<String>) {
    let mut stack = vec![root_pid];
    let mut leaves = Vec::new();
    let mut visited = std::collections::HashSet::new();
    while let Some(pid) = stack.pop() {
        if visited.len() >= 4096 || !visited.insert(pid) {
            continue;
        }
        let children =
            std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap_or_default();
        let child_pids = children
            .split_whitespace()
            .filter_map(|value| value.parse::<i32>().ok())
            .collect::<Vec<_>>();
        if child_pids.is_empty() {
            leaves.push(pid);
        } else {
            stack.extend(
                child_pids
                    .into_iter()
                    .take(4096usize.saturating_sub(visited.len())),
            );
        }
    }
    // A terminal server can own multiple tabs. Do not select an arbitrary tab's
    // process when the process tree has more than one leaf.
    let best = if leaves.len() == 1 {
        leaves[0]
    } else {
        root_pid
    };
    let name = std::fs::read_to_string(format!("/proc/{best}/comm"))
        .unwrap_or_default()
        .trim()
        .to_string();
    let cwd = std::fs::read_link(format!("/proc/{best}/cwd"))
        .ok()
        .map(|path| path.display().to_string());
    let executable = std::fs::read_link(format!("/proc/{best}/exe"))
        .ok()
        .map(|path| path.display().to_string());
    (name, cwd, executable)
}

fn title_path_hint(title: &str) -> Option<String> {
    title
        .split(" - ")
        .map(str::trim)
        .find(|part| part == &"~" || part.starts_with("~/") || part.starts_with('/'))
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::terminal_process_matches;

    #[test]
    fn codex_code_mode_processes_restore_as_codex() {
        assert!(terminal_process_matches("codex-code-mode", None, "codex"));
        assert!(terminal_process_matches(
            "codex-code-mode",
            Some("/home/user/codex-code-mode-host"),
            "codex"
        ));
        assert!(!terminal_process_matches(
            "fish",
            Some("/usr/bin/fish"),
            "codex"
        ));
    }
}
