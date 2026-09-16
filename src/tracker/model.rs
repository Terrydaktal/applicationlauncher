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
pub struct TmuxSession {
    pub socket_path: String,
    pub server_pid: i32,
    pub session_id: String,
    pub session_name: String,
    pub created_at: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct RestoreSpec {
    pub app_key: String,
    pub desktop_file: Option<String>,
    pub executable: Option<String>,
    pub cwd: Option<String>,
    pub terminal_kind: Option<String>,
    #[serde(default)]
    pub safe_arguments: Vec<String>,
    /// The connection arguments from a local ssh process, excluding any
    /// remote command. An empty vector means the session was remote but its
    /// command line was unavailable when it was recorded.
    #[serde(default)]
    pub ssh_arguments: Option<Vec<String>>,
    #[serde(default)]
    pub tmux_session: Option<TmuxSession>,
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
    let process_is_tmux = process
        .as_ref()
        .is_some_and(|details| terminal_process_matches(&details.0, details.2.as_deref(), "tmux"));
    let tmux_session = process
        .as_ref()
        .filter(|_| process_is_tmux)
        .and_then(|details| {
            super::tmux::capture_session(details.4, &details.3, details.1.as_deref())
        });
    let process_is_ssh = process
        .as_ref()
        .is_some_and(|details| terminal_process_matches(&details.0, details.2.as_deref(), "ssh"));
    let title_is_ssh = title.starts_with("ssh -") || title.starts_with("ssh ");
    let ssh_invocation = process
        .as_ref()
        .and_then(|details| ssh_invocation(&details.3));
    let ssh_arguments = (process_is_ssh || title_is_ssh).then(|| {
        ssh_invocation
            .as_ref()
            .map(|(arguments, _)| arguments.clone())
            .unwrap_or_default()
    });
    let terminal_kind = terminal.then(|| {
        let process_name = process
            .as_ref()
            .map(|details| details.0.as_str())
            .unwrap_or("");
        let process_executable = process.as_ref().and_then(|details| details.2.as_deref());
        if process_is_tmux {
            "tmux"
        } else if let Some((_, Some(remote_kind))) = ssh_invocation.as_ref() {
            remote_kind.as_str()
        } else if title.contains("codex")
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
        safe_arguments: process
            .as_ref()
            .map(|details| codex_restore_arguments(&details.3))
            .unwrap_or_default(),
        ssh_arguments,
        tmux_session,
    }
}

pub(crate) fn refresh_live_restore_spec(
    window: &TrackedWindow,
    stored: RestoreSpec,
) -> RestoreSpec {
    merge_live_restore_spec(stored, infer_restore_spec(window))
}

fn merge_live_restore_spec(mut stored: RestoreSpec, inferred: RestoreSpec) -> RestoreSpec {
    if inferred.terminal_kind.as_deref() == Some("tmux") {
        stored.terminal_kind = inferred.terminal_kind;
        stored.executable = inferred.executable;
        stored.cwd = inferred.cwd;
        stored.ssh_arguments = None;
        stored.safe_arguments.clear();
        if inferred.tmux_session.is_some() {
            stored.tmux_session = inferred.tmux_session;
        }
        return stored;
    }
    if stored.terminal_kind.as_deref() == Some("tmux") {
        // A missing/ambiguous child can leave only the terminal host observable.
        // Do not discard a known session on that transient fallback.
        let observed_child = inferred
            .executable
            .as_deref()
            .map(|path| path.strip_suffix(" (deleted)").unwrap_or(path))
            .and_then(|path| std::path::Path::new(path).file_name())
            .and_then(|name| name.to_str())
            .is_some_and(|name| !name.to_ascii_lowercase().contains("terminal"));
        return if observed_child { inferred } else { stored };
    }
    if inferred.terminal_kind.as_deref() == Some("codex")
        && inferred
            .executable
            .as_deref()
            .is_some_and(|path| terminal_process_matches("", Some(path), "codex"))
    {
        stored.safe_arguments = inferred.safe_arguments.clone();
    }
    let stored_is_ssh = stored.ssh_arguments.is_some()
        || stored
            .executable
            .as_deref()
            .is_some_and(|path| terminal_process_matches("", Some(path), "ssh"));
    if inferred.ssh_arguments.is_none() {
        if stored_is_ssh && inferred.executable.is_some() {
            stored.ssh_arguments = None;
            stored.executable = inferred.executable;
            stored.cwd = inferred.cwd;
            stored.terminal_kind = inferred.terminal_kind;
            stored.safe_arguments = inferred.safe_arguments;
        }
        return stored;
    }

    stored.ssh_arguments = inferred.ssh_arguments;
    stored.safe_arguments = inferred.safe_arguments;
    if stored.executable.is_none() {
        stored.executable = inferred.executable;
    }
    if stored.cwd.is_none() {
        stored.cwd = inferred.cwd;
    }
    if stored
        .terminal_kind
        .as_deref()
        .is_none_or(|kind| kind == "shell")
    {
        stored.terminal_kind = inferred.terminal_kind;
    }
    stored
}

const CODEX_BYPASS_ARGUMENT: &str = "--dangerously-bypass-approvals-and-sandbox";

fn is_codex_bypass_argument(argument: &str) -> bool {
    matches!(
        argument,
        CODEX_BYPASS_ARGUMENT | "--yolo" | "--dangerously-bypass-all-permissions"
    )
}

/// Only copy explicitly observed Codex options, never prompts or shell text.
pub fn codex_restore_arguments(command_arguments: &[String]) -> Vec<String> {
    if !command_arguments
        .first()
        .is_some_and(|program| terminal_process_matches("", Some(program), "codex"))
    {
        return Vec::new();
    }
    if command_arguments
        .iter()
        .skip(1)
        .take_while(|argument| argument.as_str() != "--")
        .any(|argument| is_codex_bypass_argument(argument))
    {
        vec![CODEX_BYPASS_ARGUMENT.into()]
    } else {
        Vec::new()
    }
}

/// Validate saved options again at launch; arbitrary saved arguments are not replayed.
pub fn codex_resume_arguments(saved_arguments: &[String]) -> Vec<&'static str> {
    let mut arguments = vec!["codex", "resume", "--last"];
    if saved_arguments
        .iter()
        .any(|argument| is_codex_bypass_argument(argument))
    {
        arguments.push(CODEX_BYPASS_ARGUMENT);
    }
    arguments
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
        normalized == expected
            || (expected == "codex" && normalized.starts_with("codexcodemode"))
            || (expected == "tmux" && normalized == "tmuxclient")
    };
    matches(name)
        || executable
            .map(|path| path.strip_suffix(" (deleted)").unwrap_or(path))
            .and_then(|path| std::path::Path::new(path).file_name())
            .and_then(|name| name.to_str())
            .is_some_and(matches)
}

fn terminal_process_details(
    root_pid: i32,
) -> (String, Option<String>, Option<String>, Vec<String>, i32) {
    let best = terminal_restore_process_pid(root_pid, |pid| {
        let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        let children =
            std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap_or_default();
        (
            name.trim().to_string(),
            children
                .split_whitespace()
                .filter_map(|value| value.parse::<i32>().ok())
                .collect(),
        )
    });
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
    let command_arguments = std::fs::read(format!("/proc/{best}/cmdline"))
        .ok()
        .map(|bytes| {
            bytes
                .split(|byte| *byte == 0)
                .filter(|argument| !argument.is_empty())
                .map(|argument| String::from_utf8_lossy(argument).into_owned())
                .collect()
        })
        .unwrap_or_default();
    (name, cwd, executable, command_arguments, best)
}

fn terminal_restore_process_pid(
    root_pid: i32,
    mut read_process: impl FnMut(i32) -> (String, Vec<i32>),
) -> i32 {
    let mut stack = vec![root_pid];
    let mut leaves = Vec::new();
    let mut visited = std::collections::HashSet::new();
    while let Some(pid) = stack.pop() {
        if visited.len() >= 4096 {
            return root_pid;
        }
        if !visited.insert(pid) {
            continue;
        }
        let (name, child_pids) = read_process(pid);
        // Codex owns its tool subprocesses. Stop at that session process so
        // curl, Python and nested agents cannot replace its launch options.
        if terminal_process_matches(&name, None, "codex")
            || terminal_process_matches(&name, None, "tmux")
            || child_pids.is_empty()
        {
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
    if leaves.len() == 1 {
        leaves[0]
    } else {
        root_pid
    }
}

fn ssh_invocation(arguments: &[String]) -> Option<(Vec<String>, Option<String>)> {
    let executable = arguments.first()?;
    let is_ssh = std::path::Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("ssh"));
    if !is_ssh {
        return None;
    }

    let mut connection_arguments = arguments.iter().skip(1).cloned().collect::<Vec<_>>();
    let remote_kind = connection_arguments
        .last()
        .and_then(|argument| terminal_kind_from_command(argument))
        .map(str::to_string);
    if remote_kind.is_some() {
        connection_arguments.pop();
    }
    Some((connection_arguments, remote_kind))
}

fn terminal_kind_from_command(command: &str) -> Option<&'static str> {
    let command = std::path::Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())?;
    match command {
        "codex" => Some("codex"),
        "agy" => Some("agy"),
        "htop" => Some("htop"),
        "nvtop" => Some("nvtop"),
        _ => None,
    }
}

fn title_path_hint(title: &str) -> Option<String> {
    title
        .split(" - ")
        .map(str::trim)
        .find(|part| part == &"~" || part.starts_with("~/") || part.starts_with('/'))
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod ssh_tests {
    use super::ssh_invocation;

    #[test]
    fn ssh_invocation_separates_connection_arguments_from_remote_program() {
        let arguments =
            ["/usr/bin/ssh", "-p", "2222", "lewis@example.test", "nvtop"].map(str::to_string);

        assert_eq!(
            ssh_invocation(&arguments),
            Some((
                ["-p", "2222", "lewis@example.test"]
                    .map(str::to_string)
                    .to_vec(),
                Some("nvtop".to_string()),
            ))
        );
    }

    #[test]
    fn ssh_invocation_keeps_an_interactive_remote_shell_without_a_command() {
        let arguments = ["ssh", "lewis@example.test"].map(str::to_string);

        assert_eq!(
            ssh_invocation(&arguments),
            Some((["lewis@example.test"].map(str::to_string).to_vec(), None,))
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_capture_preserves_explicit_bypass_flags_but_not_prompt_text() {
        for flag in [
            CODEX_BYPASS_ARGUMENT,
            "--yolo",
            "--dangerously-bypass-all-permissions",
        ] {
            let argv = ["/usr/bin/codex", "resume", "--last", flag].map(str::to_string);
            assert_eq!(codex_restore_arguments(&argv), [CODEX_BYPASS_ARGUMENT]);
        }
        for argv in [
            vec!["codex", "resume", "--last"],
            vec!["codex", "--", "--yolo"],
            vec!["codex", "Explain --yolo in this prompt"],
            vec!["bash", "-c", "codex --yolo"],
            vec!["curl", "--yolo"],
        ] {
            assert!(
                codex_restore_arguments(&argv.into_iter().map(str::to_string).collect::<Vec<_>>())
                    .is_empty()
            );
        }
        assert_eq!(
            codex_resume_arguments(&["--yolo".into(), CODEX_BYPASS_ARGUMENT.into()]),
            ["codex", "resume", "--last", CODEX_BYPASS_ARGUMENT,]
        );
    }

    #[test]
    fn restore_process_stays_on_codex_while_it_runs_tools() {
        let mut reads = Vec::new();
        let selected = terminal_restore_process_pid(1, |pid| {
            reads.push(pid);
            match pid {
                1 => ("xfce4-terminal".into(), vec![2]),
                2 => ("fish".into(), vec![3]),
                3 => ("codex".into(), vec![4, 5, 6]),
                _ => panic!("must not inspect Codex's tool subprocesses"),
            }
        });
        assert_eq!(selected, 3);
        assert_eq!(reads, vec![1, 2, 3]);
    }

    #[test]
    fn tmux_capture_stops_at_client_instead_of_its_server_or_panes() {
        let selected = terminal_restore_process_pid(1, |pid| match pid {
            1 => ("xfce4-terminal".into(), vec![2]),
            2 => ("fish".into(), vec![3]),
            3 => ("tmux: client".into(), vec![4]),
            _ => panic!("must not traverse tmux's server and all of its unrelated panes"),
        });
        assert_eq!(selected, 3);
        assert!(terminal_process_matches("tmux: client", None, "tmux"));
        assert!(!terminal_process_matches("tmux: server", None, "tmux"));
    }

    #[test]
    fn old_shell_records_deserialize_without_inventing_a_tmux_session() {
        let restore: RestoreSpec = serde_json::from_str(r#"{"app_key":"xfce4-terminal","desktop_file":null,"executable":"/usr/bin/tmux","cwd":"/project","terminal_kind":"shell"}"#).unwrap();
        assert!(restore.tmux_session.is_none());
    }

    #[test]
    fn tmux_live_refresh_preserves_identity_on_probe_failure_but_clears_it_after_detach() {
        let stored = RestoreSpec {
            terminal_kind: Some("tmux".into()),
            executable: Some("/usr/bin/tmux".into()),
            tmux_session: Some(TmuxSession {
                socket_path: "/tmp/tmux-test/default".into(),
                server_pid: 800,
                session_id: "$1".into(),
                session_name: "one".into(),
                created_at: 123,
            }),
            ..Default::default()
        };
        for inferred in [
            RestoreSpec::default(),
            RestoreSpec {
                terminal_kind: Some("shell".into()),
                executable: Some("/usr/bin/xfce4-terminal".into()),
                ..Default::default()
            },
            RestoreSpec {
                terminal_kind: Some("tmux".into()),
                executable: Some("/usr/bin/tmux".into()),
                ..Default::default()
            },
        ] {
            let merged = merge_live_restore_spec(stored.clone(), inferred);
            assert_eq!(merged.tmux_session, stored.tmux_session);
            assert_eq!(merged.terminal_kind.as_deref(), Some("tmux"));
        }
        let shell = RestoreSpec {
            terminal_kind: Some("shell".into()),
            executable: Some("/usr/bin/fish".into()),
            ..Default::default()
        };
        assert_eq!(
            merge_live_restore_spec(stored.clone(), shell.clone()),
            shell
        );
        let mut switched = stored.clone();
        switched.tmux_session.as_mut().unwrap().session_id = "$2".into();
        assert_eq!(merge_live_restore_spec(stored, switched.clone()), switched);
    }

    #[test]
    fn restore_process_does_not_borrow_flags_from_another_terminal_tab() {
        let selected = terminal_restore_process_pid(1, |pid| match pid {
            1 => ("xfce4-terminal".into(), vec![2, 3]),
            2 => ("codex".into(), vec![4]),
            3 => ("fish".into(), vec![]),
            _ => panic!("unexpected process"),
        });
        assert_eq!(selected, 1);
    }

    #[test]
    fn old_restore_records_do_not_implicitly_enable_bypass() {
        let old = r#"{"app_key":"xfce4-terminal","desktop_file":null,"executable":"/usr/bin/codex","cwd":"/project","terminal_kind":"codex"}"#;
        let restore: RestoreSpec = serde_json::from_str(old).unwrap();
        assert!(restore.safe_arguments.is_empty());
        assert_eq!(
            codex_resume_arguments(&restore.safe_arguments),
            ["codex", "resume", "--last"]
        );
        assert!(terminal_process_matches(
            "",
            Some("/usr/bin/codex (deleted)"),
            "codex"
        ));
    }

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
