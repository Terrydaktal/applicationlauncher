use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use applicationlauncher::tracker::{LiveTmuxPane, capture_live_client_panes};
use eframe::egui;

use crate::models::{ProcessChainEntry, WindowInfo};
use crate::windows::{
    display_path, is_codex_process, is_configured_codex_title, is_terminal_class,
    read_proc_cmdline, read_process_stat, resolve_window_icon, summarize_command_line,
    terminal_display_title, terminal_parent_program, tmux_codex_display_title,
};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const FAILURE_GRACE: Duration = Duration::from_secs(2);
const MAX_TARGETS: usize = 512;

#[derive(Clone, PartialEq, Eq)]
struct Target {
    id: String,
    owner_pid: Option<i32>,
    client_pid: Option<i32>,
    root_pid: i32,
    class: String,
    desktop_file_name: Option<String>,
    executable: Option<PathBuf>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct TmuxWindowMetadata {
    owner_pid: Option<i32>,
    pub(crate) pane: LiveTmuxPane,
    chain: Vec<ProcessChainEntry>,
    command_line: Option<String>,
    command_summary: Option<String>,
    icon_path: Option<PathBuf>,
}

impl TmuxWindowMetadata {
    pub(crate) fn apply(&self, window: &mut WindowInfo) {
        if window.pid != self.owner_pid || !is_terminal_class(&window.class.to_lowercase()) {
            return;
        }
        let parent = terminal_parent_program(&self.pane.process_name, &self.chain);
        let cwd = display_path(&self.pane.cwd);
        let mut title = if is_codex_process(&self.pane.process_name)
            || parent == Some("codex")
            || is_configured_codex_title(&self.pane.title)
        {
            tmux_codex_display_title(&self.pane.title, &cwd)
        } else {
            terminal_display_title(
                &self.pane.title,
                &self.pane.process_name,
                self.command_summary.as_deref(),
                Some(&cwd),
                parent,
            )
        };
        // Match tmux's desktop-title separator conversion without changing the pane title.
        if title.contains(" | ") {
            title = title.replace(" | ", " - ");
        }
        window.title = format!("tmux: {} - {title}", self.pane.session.session_name);
        window.active_process = Some(self.pane.process_name.clone());
        window.cwd_path = Some(self.pane.cwd.clone());
        window.command_line = self.command_line.clone();
        window.command_summary = self.command_summary.clone();
        window.process_chain = self.chain.clone();
        window.icon_path = self.icon_path.clone();
        window.tmux_pane = Some(self.pane.clone());
    }
}

#[derive(Default)]
struct State {
    targets: Vec<Target>,
    latest: Option<HashMap<String, TmuxWindowMetadata>>,
    stopped: bool,
}

pub(crate) struct TmuxMonitor {
    shared: Arc<(Mutex<State>, Condvar)>,
    targets: Vec<Target>,
}

impl TmuxMonitor {
    pub(crate) fn new(ctx: egui::Context, theme: String) -> Self {
        let shared: Arc<(Mutex<State>, Condvar)> = Arc::default();
        let worker_state = Arc::clone(&shared);
        applicationlauncher::observability::spawn_named("tmux-live-panes", move |worker| {
            let mut previous = HashMap::new();
            let mut successes = HashMap::new();
            worker.set_state("waiting");
            loop {
                let started = Instant::now();
                let targets = {
                    let (mutex, wake) = &*worker_state;
                    let mut state = mutex.lock().unwrap_or_else(|err| err.into_inner());
                    while !state.stopped && state.targets.is_empty() {
                        previous.clear();
                        successes.clear();
                        state = wake.wait(state).unwrap_or_else(|err| err.into_inner());
                    }
                    if state.stopped {
                        break;
                    }
                    state.targets.clone()
                };
                worker.set_state("checking-terminal-foreground");
                let clients: HashMap<_, _> = targets
                    .iter()
                    .filter_map(|target| {
                        let pid = client_for_target(target)?;
                        Some((target.id.as_str(), pid))
                    })
                    .collect();
                let pids: Vec<_> = clients
                    .values()
                    .copied()
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                worker.set_state("reading-tmux-panes");
                let panes = capture_live_client_panes(&pids);
                let now = Instant::now();
                let mut current = HashMap::new();
                for target in &targets {
                    let Some(&client_pid) = clients.get(target.id.as_str()) else {
                        continue;
                    };
                    match panes.get(&client_pid) {
                        Some(Some(pane)) => {
                            let metadata = if let Some(old) =
                                previous
                                    .get(&target.id)
                                    .filter(|old: &&TmuxWindowMetadata| {
                                        same_process_context(&old.pane, pane)
                                    }) {
                                let mut metadata = old.clone();
                                metadata.pane = pane.clone();
                                metadata.owner_pid = target.owner_pid;
                                metadata
                            } else {
                                pane_metadata(target, pane, &theme)
                            };
                            current.insert(target.id.clone(), metadata);
                            successes.insert(target.id.clone(), now);
                        }
                        None if successes
                            .get(&target.id)
                            .is_some_and(|last| now.duration_since(*last) < FAILURE_GRACE) =>
                        {
                            if let Some(old) = previous.get(&target.id).filter(|old| {
                                old.pane.client_pid == client_pid
                                    && old.owner_pid == target.owner_pid
                            }) {
                                current.insert(target.id.clone(), old.clone());
                            }
                        }
                        _ => {}
                    }
                }
                successes.retain(|id, _| current.contains_key(id));
                let (mutex, wake) = &*worker_state;
                let mut state = mutex.lock().unwrap_or_else(|err| err.into_inner());
                if state.stopped {
                    break;
                }
                if state.targets != targets {
                    continue;
                }
                let changed = current != previous;
                if changed {
                    // Latest state only: hidden/minimized launchers never replay a backlog.
                    state.latest = Some(current.clone());
                    previous = current;
                }
                drop(state);
                if changed {
                    ctx.request_repaint();
                }
                let state = mutex.lock().unwrap_or_else(|err| err.into_inner());
                if state.stopped {
                    break;
                }
                if state.targets != targets {
                    continue;
                }
                // Discover a shell entering tmux without repeatedly scanning /proc.
                // Idle terminals need only a cheap foreground-group check every two seconds.
                let interval = if pids.is_empty() {
                    Duration::from_secs(2)
                } else {
                    POLL_INTERVAL
                };
                let delay = interval.saturating_sub(started.elapsed());
                worker.set_state("waiting");
                let _ = wake.wait_timeout(state, delay);
            }
        });
        Self {
            shared,
            targets: Vec::new(),
        }
    }

    pub(crate) fn sync_windows(&mut self, windows: &[WindowInfo]) {
        let targets: Vec<_> = windows
            .iter()
            .filter_map(|window| {
                let previous = self
                    .targets
                    .iter()
                    .find(|target| target.id == window.id && target.owner_pid == window.pid);
                let mut target = target_for_window(window).or_else(|| {
                    is_terminal_class(&window.class.to_lowercase())
                        .then(|| previous.cloned())
                        .flatten()
                })?;
                if window.tmux_pane.is_some()
                    && let Some(previous) = previous
                {
                    // Keep the outer shell's PTY even after the visible execution
                    // chain moves into the pane. It survives detach and server failure.
                    target.root_pid = previous.root_pid;
                }
                Some(target)
            })
            .take(MAX_TARGETS)
            .collect();
        if targets == self.targets {
            return;
        }
        let (mutex, wake) = &*self.shared;
        let mut state = mutex.lock().unwrap_or_else(|err| err.into_inner());
        state.targets = targets.clone();
        if targets.is_empty() {
            state.latest = Some(HashMap::new());
        }
        self.targets = targets;
        wake.notify_one();
    }

    pub(crate) fn take_latest(&self) -> Option<HashMap<String, TmuxWindowMetadata>> {
        self.shared
            .0
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .latest
            .take()
    }
}

impl Drop for TmuxMonitor {
    fn drop(&mut self) {
        let (mutex, wake) = &*self.shared;
        mutex.lock().unwrap_or_else(|err| err.into_inner()).stopped = true;
        wake.notify_one();
    }
}

fn target_for_window(window: &WindowInfo) -> Option<Target> {
    if !is_terminal_class(&window.class.to_lowercase()) {
        return None;
    }
    let client_pid = window
        .tmux_pane
        .as_ref()
        .map(|pane| pane.client_pid)
        .or_else(|| {
            window
                .process_chain
                .iter()
                .find(|entry| entry.name.starts_with("tmux") && !entry.name.contains("server"))
                .map(|entry| entry.pid)
        });
    let root_pid = if window.tmux_pane.is_some() {
        client_pid
    } else {
        window
            .process_chain
            .iter()
            .rev()
            .find(|entry| crate::windows::is_shell(&entry.name))
            .or_else(|| window.process_chain.first())
            .map(|entry| entry.pid)
            .or(client_pid)
    }?;
    Some(Target {
        id: window.id.clone(),
        owner_pid: window.pid,
        client_pid,
        root_pid,
        class: window.class.clone(),
        desktop_file_name: window.desktop_file_name.clone(),
        executable: window.exe_path.clone(),
    })
}

fn client_for_target(target: &Target) -> Option<i32> {
    let root = read_process_stat(target.root_pid)?;
    let foreground = read_process_stat(root.foreground_process_group)?;
    if !foreground.name.starts_with("tmux")
        || !crate::windows::is_terminal_foreground_process(&foreground, &root)
    {
        return None;
    }
    let owner = target.owner_pid?;
    let mut pid = foreground.pid;
    let mut visited = HashSet::new();
    for _ in 0..64 {
        if pid == owner {
            return Some(foreground.pid);
        }
        if pid <= 0 || !visited.insert(pid) {
            break;
        }
        pid = read_process_stat(pid)?.ppid;
    }
    None
}

fn same_process_context(old: &LiveTmuxPane, new: &LiveTmuxPane) -> bool {
    old.client_pid == new.client_pid
        && old.session == new.session
        && old.pane_id == new.pane_id
        && old.pane_pid == new.pane_pid
        && old.process_pid == new.process_pid
        && old.process_name == new.process_name
        && old.cwd == new.cwd
}

fn pane_metadata(target: &Target, pane: &LiveTmuxPane, theme: &str) -> TmuxWindowMetadata {
    let mut chain = Vec::new();
    let mut visited = HashSet::new();
    let mut pid = pane.process_pid;
    while pid > 0 && chain.len() < 64 && visited.insert(pid) {
        let Some(stat) = read_process_stat(pid) else {
            break;
        };
        chain.push(ProcessChainEntry {
            pid,
            name: stat.name,
            exe_path: std::fs::read_link(format!("/proc/{pid}/exe")).ok(),
        });
        pid = stat.ppid;
    }
    let args = read_proc_cmdline(pane.process_pid);
    TmuxWindowMetadata {
        owner_pid: target.owner_pid,
        pane: pane.clone(),
        chain,
        command_summary: args.as_deref().and_then(summarize_command_line),
        command_line: args.map(|args| args.join(" ")),
        icon_path: resolve_window_icon(
            theme,
            &target.class,
            target.desktop_file_name.as_deref(),
            Some(&pane.process_name),
            target.executable.as_deref(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> WindowInfo {
        WindowInfo {
            id: "window".into(),
            pid: Some(100),
            title: "tmux new-session -A ~ - Terminal".into(),
            raw_title: "tmux new-session -A ~ - Terminal".into(),
            class: "xfce4-terminal".into(),
            desktop_file_name: None,
            minimized: Some(false),
            demands_attention: false,
            icon_path: None,
            active_process: Some("tmux: client".into()),
            exe_path: Some("/usr/bin/xfce4-terminal".into()),
            cwd_path: Some("/home/lewis".into()),
            command_line: None,
            command_summary: None,
            geometry: Some((10, 20, 800, 600)),
            process_chain: vec![ProcessChainEntry {
                pid: 101,
                name: "tmux: client".into(),
                exe_path: Some("/usr/bin/tmux".into()),
            }],
            tmux_pane: None,
            last_activated_at_ms: Some(123),
            activation_sequence: 2,
        }
    }

    fn metadata() -> TmuxWindowMetadata {
        TmuxWindowMetadata {
            owner_pid: Some(100),
            pane: LiveTmuxPane {
                client_pid: 101,
                session: applicationlauncher::tracker::TmuxSession {
                    socket_path: "/tmp/server".into(),
                    server_pid: 200,
                    session_id: "$1".into(),
                    session_name: "diet".into(),
                    created_at: 123,
                },
                pane_id: "%1".into(),
                pane_pid: 300,
                process_pid: 301,
                process_name: "codex".into(),
                cwd: "/tasks/diet".into(),
                title: "\u{280b} diet".into(),
            },
            chain: vec![
                ProcessChainEntry {
                    pid: 301,
                    name: "codex".into(),
                    exe_path: Some("/repos/codex/codex".into()),
                },
                ProcessChainEntry {
                    pid: 200,
                    name: "tmux: server".into(),
                    exe_path: Some("/usr/bin/tmux".into()),
                },
            ],
            command_line: Some("codex resume --last".into()),
            command_summary: Some("codex resume".into()),
            icon_path: None,
        }
    }

    #[test]
    fn pane_overlays_only_inner_metadata_and_keeps_outer_client_mapping() {
        let mut window = window();
        metadata().apply(&mut window);
        assert_eq!(
            window.title,
            "tmux: diet - codex: \u{280b} /tasks/diet - Terminal"
        );
        assert_eq!(window.active_process.as_deref(), Some("codex"));
        assert_eq!(
            window.cwd_path.as_deref(),
            Some(std::path::Path::new("/tasks/diet"))
        );
        assert_eq!(window.pid, Some(100));
        assert_eq!(
            window.exe_path.as_deref(),
            Some(std::path::Path::new("/usr/bin/xfce4-terminal"))
        );
        assert_eq!(window.process_chain[0].pid, 301);
        assert_eq!(target_for_window(&window).unwrap().client_pid, Some(101));
        assert_eq!(window.raw_title, "tmux new-session -A ~ - Terminal");
        assert_eq!(window.geometry, Some((10, 20, 800, 600)));
    }

    #[test]
    fn spinner_frames_do_not_invalidate_search_but_pane_switches_do() {
        let mut window = window();
        let mut first = metadata();
        first.apply(&mut window);
        let previous = window.clone();
        first.pane.title = "\u{2834} diet".into();
        assert!(same_process_context(
            &previous.tmux_pane.as_ref().unwrap(),
            &first.pane
        ));
        first.apply(&mut window);
        assert_ne!(window.title, previous.title);
        assert!(crate::search::window_search_metadata_equal(
            &previous, &window
        ));
        first.pane.pane_id = "%2".into();
        first.pane.cwd = "/tasks/other".into();
        first.pane.title = "other".into();
        assert!(!same_process_context(
            previous.tmux_pane.as_ref().unwrap(),
            &first.pane
        ));
        first.apply(&mut window);
        assert!(!crate::search::window_search_metadata_equal(
            &previous, &window
        ));
    }

    #[test]
    fn session_label_is_searchable_and_updates_on_session_rename_without_duplication() {
        let mut window = window();
        let mut metadata = metadata();
        metadata.apply(&mut window);
        let previous = window.clone();
        metadata.apply(&mut window);
        assert_eq!(window, previous);

        metadata.pane.session.session_name = "opsec".into();
        metadata.apply(&mut window);
        assert_eq!(
            window.title,
            "tmux: opsec - codex: \u{280b} /tasks/diet - Terminal"
        );
        assert!(!crate::search::window_search_metadata_equal(
            &previous, &window
        ));
        assert!(
            crate::search::window_search_values(&window)
                .iter()
                .any(|(priority, value)| *priority == 0 && value.contains("tmux: opsec"))
        );
        let document = crate::search::window_search_document(&window);
        let query = fuzzy_rank::fields::fuzzy::MetadataQuery::new("opsec").unwrap();
        assert!(
            query
                .search_rank_prepared(document.candidate(0.0))
                .is_some()
        );
        assert_eq!(window.active_process.as_deref(), Some("codex"));
        assert_eq!(
            crate::search::terminal_window_subgroup_key(&window),
            "codex"
        );
    }

    #[test]
    fn session_label_preserves_attention_and_nonstandard_title_separators() {
        let mut window = window();
        let mut metadata = metadata();
        metadata.pane.session.session_name = "Terminal".into();
        metadata.pane.title = "[ . ] Action Required - diet".into();
        metadata.apply(&mut window);
        assert_eq!(
            window.title,
            "tmux: Terminal - codex: [ . ] Action Required /tasks/diet - Terminal"
        );
        assert!(crate::search::window_requires_attention(&window));
        let previous_sort_title = crate::search::window_sort_title_key(&window);
        metadata.pane.title = "[ ! ] Action Required - diet".into();
        metadata.apply(&mut window);
        assert_eq!(
            crate::search::window_sort_title_key(&window),
            previous_sort_title
        );

        metadata.pane.session.session_name = "opsec".into();
        metadata.pane.process_name = "fish".into();
        metadata.chain.clear();
        metadata.command_summary = Some("fish".into());
        metadata.pane.title = "diet | Terminal".into();
        metadata.apply(&mut window);
        assert_eq!(window.title, "tmux: opsec - fish - /tasks/diet - Terminal");
    }

    #[test]
    fn configured_codex_title_matches_the_actual_terminal_title_without_duplicate_fields() {
        let mut window = window();
        let mut metadata = metadata();
        for (title, status) in [
            ("codex | /tasks/diet | Check ChatGPT link access", ""),
            (
                "codex \u{280b} /tasks/diet | Check ChatGPT link access",
                "\u{280b} ",
            ),
            (
                "codex \u{2834} /tasks/diet | Check ChatGPT link access",
                "\u{2834} ",
            ),
            (
                "[ ! ] Action Required | codex | /tasks/diet | Check ChatGPT link access",
                "[ ! ] Action Required ",
            ),
            (
                "[ . ] Action Required | codex | /tasks/diet | Check ChatGPT link access",
                "[ . ] Action Required ",
            ),
            ("Check ChatGPT link access | diet", ""),
            ("\u{280b} Check ChatGPT link access | diet", "\u{280b} "),
            (
                "[ ! ] Action Required | Check ChatGPT link access | diet",
                "[ ! ] Action Required ",
            ),
        ] {
            metadata.pane.title = title.into();
            metadata.apply(&mut window);
            assert_eq!(
                window.title,
                format!(
                    "tmux: diet - codex: {status}/tasks/diet - Check ChatGPT link access - Terminal"
                )
            );
            assert_eq!(window.tmux_pane.as_ref().unwrap().title, title);
            assert_eq!(window.title.matches("tmux: diet").count(), 1);
            assert_eq!(window.title.matches("/tasks/diet").count(), 1);
            assert_eq!(window.active_process.as_deref(), Some("codex"));
            assert_eq!(
                crate::search::window_requires_attention(&window),
                title.contains("Action Required")
            );
        }

        metadata.pane.title = "codex \u{280b} /tasks/diet | Check ChatGPT link access".into();
        metadata.pane.process_name = "curl".into();
        metadata.chain.insert(
            0,
            ProcessChainEntry {
                pid: 302,
                name: "curl".into(),
                exe_path: Some("/usr/bin/curl".into()),
            },
        );
        metadata.apply(&mut window);
        assert_eq!(
            window.title,
            "tmux: diet - codex: \u{280b} /tasks/diet - Check ChatGPT link access - Terminal"
        );
        assert_eq!(window.active_process.as_deref(), Some("curl"));
    }

    #[test]
    fn configured_codex_spinner_changes_keep_search_and_order_stable() {
        let mut window = window();
        let mut metadata = metadata();
        metadata.pane.title = "codex \u{280b} /tasks/diet | Check ChatGPT link access".into();
        metadata.apply(&mut window);
        let previous = window.clone();
        metadata.pane.title = "codex \u{2834} /tasks/diet | Check ChatGPT link access".into();
        metadata.apply(&mut window);
        assert_ne!(window.title, previous.title);
        assert!(crate::search::window_search_metadata_equal(
            &previous, &window
        ));
        assert_eq!(
            crate::search::window_sort_title_key(&window),
            crate::search::window_sort_title_key(&previous)
        );
        let document = crate::search::window_search_document(&window);
        for text in ["diet", "Check ChatGPT link access"] {
            let query = fuzzy_rank::fields::fuzzy::MetadataQuery::new(text).unwrap();
            assert!(
                query
                    .search_rank_prepared(document.candidate(0.0))
                    .is_some()
            );
        }
    }

    #[test]
    fn reused_window_owner_does_not_receive_stale_pane_metadata() {
        let mut window = window();
        window.pid = Some(999);
        let original = window.clone();
        metadata().apply(&mut window);
        assert_eq!(window, original);
    }

    #[test]
    fn monitoring_keeps_the_outer_shell_across_pane_overlays_and_temporary_failure() {
        let mut window = window();
        window.process_chain.push(ProcessChainEntry {
            pid: 90,
            name: "fish".into(),
            exe_path: Some("/usr/bin/fish".into()),
        });
        let mut monitor = TmuxMonitor::new(egui::Context::default(), "breeze-dark".into());
        monitor.sync_windows(std::slice::from_ref(&window));
        assert_eq!(monitor.targets[0].root_pid, 90);
        metadata().apply(&mut window);
        monitor.sync_windows(std::slice::from_ref(&window));
        assert_eq!(monitor.targets[0].root_pid, 90);
        window.tmux_pane = None;
        window.process_chain.clear();
        monitor.sync_windows(std::slice::from_ref(&window));
        assert_eq!(monitor.targets[0].root_pid, 90);
        monitor.sync_windows(&[]);
        assert!(monitor.targets.is_empty());
        assert!(monitor.take_latest().unwrap().is_empty());
    }

    #[test]
    #[ignore = "read-only live monitor; requires APPLICATIONLAUNCHER_TEST_TMUX_TERMINAL_PIDS"]
    fn live_window_monitor_resolves_real_terminal_owners_and_panes() {
        let pids: HashSet<i32> = std::env::var("APPLICATIONLAUNCHER_TEST_TMUX_TERMINAL_PIDS")
            .expect("provide exact existing terminal owner PIDs")
            .split(',')
            .map(|pid| pid.parse().unwrap())
            .collect();
        let tracked = applicationlauncher::tracker::TrackerClient::connect()
            .unwrap()
            .windows()
            .unwrap();
        let (children, names, parents) = crate::windows::get_process_tree();
        let mut icons = HashMap::new();
        let windows: Vec<_> = tracked
            .into_iter()
            .filter(|window| pids.contains(&window.pid))
            .map(|window| {
                crate::windows::build_window_info(
                    window.id,
                    window.title,
                    window.class,
                    Some(window.desktop_file_name),
                    Some(window.pid),
                    None,
                    Some(window.minimized),
                    "breeze-dark",
                    &mut icons,
                    &children,
                    &names,
                    &parents,
                    &[],
                )
                .unwrap()
            })
            .collect();
        assert_eq!(windows.len(), pids.len());
        let mut monitor = TmuxMonitor::new(egui::Context::default(), "breeze-dark".into());
        monitor.sync_windows(&windows);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(latest) = monitor.take_latest()
                && latest.len() == windows.len()
            {
                for mut window in windows {
                    let original_pid = window.pid;
                    let metadata = &latest[&window.id];
                    metadata.apply(&mut window);
                    assert_eq!(window.pid, original_pid);
                    assert_eq!(window.process_chain[0].pid, metadata.pane.process_pid);
                    assert_eq!(window.cwd_path.as_ref(), Some(&metadata.pane.cwd));
                    assert_eq!(
                        client_for_target(&target_for_window(&window).unwrap()),
                        Some(metadata.pane.client_pid)
                    );
                    assert!(!window.title.contains("tmux new-session"));
                    eprintln!(
                        "window_owner={:?} client={} session={} raw_title={:?} title={}",
                        window.pid,
                        metadata.pane.client_pid,
                        metadata.pane.session.session_name,
                        window.raw_title,
                        window.title
                    );
                }
                break;
            }
            assert!(
                Instant::now() < deadline,
                "live window monitor failed to resolve all exact owners"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
