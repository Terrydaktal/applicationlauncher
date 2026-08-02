use serde::{Deserialize, Serialize};

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
    pub minimized: bool,
    #[serde(default)]
    pub maximized: bool,
    #[serde(default)]
    pub fullscreen: bool,
    #[serde(default)]
    pub demands_attention: bool,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub desktop: i32,
    #[serde(default)]
    pub on_all_desktops: bool,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub opened_at_ms: i64,
    #[serde(default)]
    pub updated_at_ms: i64,
    #[serde(default)]
    pub last_activated_at_ms: Option<i64>,
    #[serde(default)]
    pub activation_sequence: i64,
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
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct RestoreReport {
    pub matched: usize,
    pub launched: usize,
    pub failures: Vec<String>,
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
        if title.contains("codex") {
            "codex"
        } else if process_name == "codex" {
            "codex"
        } else if title.contains("agy") {
            "agy"
        } else if process_name == "agy" {
            "agy"
        } else if title.contains("htop") {
            "htop"
        } else if process_name == "htop" {
            "htop"
        } else if title.contains("nvtop") {
            "nvtop"
        } else if process_name == "nvtop" {
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

fn terminal_process_details(root_pid: i32) -> (String, Option<String>, Option<String>) {
    let mut stack = vec![root_pid];
    let mut best = root_pid;
    while let Some(pid) = stack.pop() {
        let children =
            std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap_or_default();
        let child_pids = children
            .split_whitespace()
            .filter_map(|value| value.parse::<i32>().ok())
            .collect::<Vec<_>>();
        if child_pids.is_empty() {
            best = pid;
        } else {
            stack.extend(child_pids);
        }
    }
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
