//! Explicit, conservative interruption of local idle Codex sessions.
//! No polling or signals occur until the settings action is confirmed.

use std::collections::{HashMap, HashSet};
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use applicationlauncher::tracker::{SERVICE_NAME, TRACKER_INTERFACE, TRACKER_PATH, TrackedWindow};
use rustix::process::{Pid, PidfdFlags, pidfd_open};
#[cfg(not(test))]
use rustix::process::{Signal, pidfd_send_signal};
use zbus::blocking::{Connection, Proxy};

use super::{parse_terminal_dbus_records, terminal_dbus_service_names};
use crate::config::{TERMINAL_DBUS_INTERFACE, TERMINAL_DBUS_PATH};
use crate::models::TerminalDbusRecord;
use crate::search::is_braille_spinner_char;
use crate::windows::process::{is_shell, parse_proc_stat};

const MAX_TARGETS: usize = 512;
const MAX_PROCESS_NODES: usize = 4096;
const BATCH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default, Debug, PartialEq, Eq)]
struct Report {
    interrupted: usize,
    skipped: usize,
    failed: usize,
    first_error: Option<String>,
}

impl Report {
    fn fail(&mut self, phase: &str, error: &str) {
        self.failed += 1;
        if self.first_error.is_none() {
            let detail: String = error
                .chars()
                .filter(|ch| !ch.is_control())
                .take(240)
                .collect();
            self.first_error = Some(format!("{phase}: {detail}"));
        }
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            out,
            "Interrupt sent to {} idle Codex session(s); {} busy, changed or unverified skipped; {} failed. No interrupt was sent to a terminal or shell.",
            self.interrupted, self.skipped, self.failed
        )?;
        if let Some(error) = &self.first_error {
            write!(out, " First failure: {error}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcessIdentity {
    pid: i32,
    start_ticks: u64,
    uid: u32,
    ppid: i32,
    group: i32,
    session: i32,
    tty: i32,
    foreground_group: i32,
    executable: PathBuf,
}

impl ProcessIdentity {
    fn is_codex(&self) -> bool {
        // Never classify a process from its title, argv, or truncated comm name.
        self.executable
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                matches!(name, "codex" | "codex-code-mode" | "codex-code-mode-host")
            })
    }
}

#[derive(Clone)]
struct Candidate {
    owner: String,
    terminal: TerminalDbusRecord,
    window: TrackedWindow,
    process: ProcessIdentity,
}

fn idle_title(title: &str) -> bool {
    let title = title.trim();
    !title.is_empty()
        && !title.chars().any(is_braille_spinner_char)
        && !title.to_ascii_lowercase().contains("action required")
        && !title.contains("[ ! ]")
        && !title.contains("[!]")
        && !title.contains("[ . ]")
}

fn matching_window<'a>(
    record: &TerminalDbusRecord,
    windows: &'a [TrackedWindow],
) -> Option<&'a TrackedWindow> {
    if !record.active
        || record.terminal_pid == 0
        || record.window_uuid.is_empty()
        || record.tab_uuid.is_empty()
        || record.window_title.trim().is_empty()
    {
        return None;
    }
    let mut matches = windows.iter().filter(|window| {
        window.pid > 0
            && window.pid as u32 == record.terminal_pid
            && window.class.eq_ignore_ascii_case("xfce4-terminal")
            && window.title.trim() == record.window_title.trim()
            && !window.id.is_empty()
            && !window.skip_taskbar
            && !window.skip_switcher
    });
    let window = matches.next()?;
    matches.next().is_none().then_some(window)
}

fn eligible(candidate: &Candidate) -> bool {
    let record = &candidate.terminal;
    let process = &candidate.process;
    matching_window(record, std::slice::from_ref(&candidate.window)).is_some()
        && !candidate.window.demands_attention
        && idle_title(&candidate.window.title)
        && idle_title(&record.window_title)
        && process.is_codex()
        && process.pid > 1
        && process.start_ticks > 0
        && process.pid as u32 != record.terminal_pid
        && process.pid as u32 != record.child_pid
        && process.tty > 0
        && process.session > 0
        && process.group > 1
        && process.group == process.foreground_group
        && process.group as u32 == record.foreground_pgid
        && record.foreground_pid == record.foreground_pgid
}

fn unchanged_idle(before: &Candidate, after: &Candidate) -> bool {
    eligible(after)
        && before.owner == after.owner
        && before.terminal == after.terminal
        && before.window.id == after.window.id
        && before.window.title == after.window.title
        && before.process == after.process
}

trait Backend {
    type Handle;
    fn scan(&mut self) -> Result<(Vec<Candidate>, usize), String>;
    fn pin(&mut self, process: &ProcessIdentity) -> Result<Self::Handle, String>;
    fn refresh(&mut self, candidate: &Candidate) -> Result<Option<Candidate>, String>;
    fn interrupt(&mut self, candidate: &Candidate, handle: &Self::Handle) -> Result<(), String>;
}

fn run_batch(backend: &mut impl Backend, deadline: Instant) -> Result<Report, String> {
    let (candidates, skipped) = backend.scan()?;
    let mut report = Report {
        skipped,
        ..Report::default()
    };
    let mut seen = HashSet::new();
    // One finite pass, one SIGINT at most per process; never retry or escalate.
    for (index, candidate) in candidates.iter().enumerate() {
        if index >= MAX_TARGETS || Instant::now() >= deadline {
            report.skipped += candidates.len() - index;
            break;
        }
        if !eligible(candidate)
            || !seen.insert((candidate.process.pid, candidate.process.start_ticks))
        {
            report.skipped += 1;
            continue;
        }
        let handle = match backend.pin(&candidate.process) {
            Ok(handle) => handle,
            Err(error) => {
                report.fail("Process identity check", &error);
                continue;
            }
        };
        match backend.refresh(candidate) {
            Ok(Some(current))
                if unchanged_idle(candidate, &current) && Instant::now() < deadline =>
            {
                match backend.interrupt(&current, &handle) {
                    Ok(()) => report.interrupted += 1,
                    Err(error) => report.fail("Interrupt", &error),
                }
            }
            Ok(_) => report.skipped += 1,
            Err(error) => report.fail("Fresh terminal state", &error),
        }
    }
    Ok(report)
}

fn read_identity(pid: i32) -> Option<ProcessIdentity> {
    if pid <= 1 {
        return None;
    }
    let path = PathBuf::from(format!("/proc/{pid}"));
    let uid = path.metadata().ok()?.uid();
    if uid != unsafe { libc::geteuid() } {
        return None;
    }
    let text = std::fs::read_to_string(path.join("stat")).ok()?;
    let stat = parse_proc_stat(&text)?;
    let fields: Vec<_> = text[text.rfind(')')? + 1..].split_whitespace().collect();
    if matches!(*fields.first()?, "Z" | "X") {
        return None;
    }
    let start_ticks = fields.get(19)?.parse().ok()?;
    let executable = std::fs::read_link(path.join("exe")).ok()?;
    let executable = executable.to_str()?;
    Some(ProcessIdentity {
        pid: stat.pid,
        start_ticks,
        uid,
        ppid: stat.ppid,
        group: stat.process_group,
        session: stat.session,
        tty: stat.tty,
        foreground_group: stat.foreground_process_group,
        executable: PathBuf::from(executable.strip_suffix(" (deleted)").unwrap_or(executable)),
    })
}

fn foreground_codex_with(
    record: &TerminalDbusRecord,
    mut inspect: impl FnMut(i32) -> Option<ProcessIdentity>,
    mut children: impl FnMut(i32) -> Option<Vec<i32>>,
) -> Option<ProcessIdentity> {
    let root = inspect(i32::try_from(record.child_pid).ok()?)?;
    // Do not interrupt a terminal's main command: its exit can close the window.
    if !root
        .executable
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_shell)
        || root.ppid != i32::try_from(record.terminal_pid).ok()?
        || root.tty <= 0
        || root.foreground_group <= 1
        || root.foreground_group as u32 != record.foreground_pgid
        || record.foreground_pid != record.foreground_pgid
    {
        return None;
    }
    let mut pending = vec![root.pid];
    let mut seen = HashSet::new();
    let mut found = None;
    while let Some(pid) = pending.pop() {
        if !seen.insert(pid) || seen.len() > MAX_PROCESS_NODES {
            return None;
        }
        let process = inspect(pid)?;
        if process.session != root.session || process.tty != root.tty {
            continue;
        }
        if process.is_codex() {
            // An outer Codex owns its descendants, including nested agents.
            if process.group == root.foreground_group
                && process.foreground_group == root.foreground_group
            {
                if found.is_some() {
                    return None;
                }
                found = Some(process);
            }
            continue;
        }
        let child_pids = children(pid)?;
        if child_pids.len() + pending.len() + seen.len() > MAX_PROCESS_NODES {
            return None;
        }
        for child in child_pids {
            let child_process = inspect(child)?;
            if child_process.ppid != pid {
                return None;
            }
            pending.push(child);
        }
    }
    found
}

fn foreground_codex(record: &TerminalDbusRecord) -> Option<ProcessIdentity> {
    foreground_codex_with(record, read_identity, |pid| {
        std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
            .ok()?
            .split_whitespace()
            .map(|pid| pid.parse().ok())
            .collect()
    })
}

struct LiveBackend {
    connection: Connection,
    deadline: Instant,
}

impl LiveBackend {
    fn check_deadline(&self) -> Result<(), String> {
        (Instant::now() < self.deadline)
            .then_some(())
            .ok_or_else(|| "Idle Codex check timed out; remaining sessions were skipped".into())
    }

    fn windows(&self) -> Result<Vec<TrackedWindow>, String> {
        self.check_deadline()?;
        let proxy = Proxy::new(
            &self.connection,
            SERVICE_NAME,
            TRACKER_PATH,
            TRACKER_INTERFACE,
        )
        .map_err(|err| err.to_string())?;
        let json: String = proxy
            .call("GetWindows", &())
            .map_err(|err| err.to_string())?;
        serde_json::from_str(&json).map_err(|err| err.to_string())
    }

    fn records(&self, owner: &str, terminal_pid: u32) -> Result<Vec<TerminalDbusRecord>, String> {
        self.check_deadline()?;
        let proxy = Proxy::new(
            &self.connection,
            owner,
            TERMINAL_DBUS_PATH,
            TERMINAL_DBUS_INTERFACE,
        )
        .map_err(|err| err.to_string())?;
        let raw: Vec<HashMap<String, zbus::zvariant::OwnedValue>> = proxy
            .call("ListTerminals", &())
            .map_err(|err| err.to_string())?;
        let mut records = parse_terminal_dbus_records(raw);
        for record in &mut records {
            record.terminal_pid = terminal_pid;
        }
        Ok(records)
    }
}

impl Backend for LiveBackend {
    type Handle = OwnedFd;

    fn scan(&mut self) -> Result<(Vec<Candidate>, usize), String> {
        let windows = self.windows()?;
        let dbus = Proxy::new(
            &self.connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .map_err(|err| err.to_string())?;
        let names: Vec<String> = dbus.call("ListNames", &()).map_err(|err| err.to_string())?;
        let services = terminal_dbus_service_names(names);
        if services.is_empty() {
            return Err(
                "XFCE4 Terminal's metadata API is unavailable; nothing was interrupted".into(),
            );
        }
        let mut owners = HashSet::new();
        let mut candidates = Vec::new();
        let mut skipped = 0;
        for service in services.into_iter().take(MAX_TARGETS) {
            self.check_deadline()?;
            let Ok(owner) = dbus.call::<_, _, String>("GetNameOwner", &(service.as_str(),)) else {
                skipped += 1;
                continue;
            };
            if !owners.insert(owner.clone()) {
                continue;
            }
            let Ok(pid) = dbus.call::<_, _, u32>("GetConnectionUnixProcessID", &(owner.as_str(),))
            else {
                skipped += 1;
                continue;
            };
            let Ok(records) = self.records(&owner, pid) else {
                skipped += 1;
                continue;
            };
            for record in records
                .into_iter()
                .filter(|record| record.active)
                .take(MAX_TARGETS)
            {
                self.check_deadline()?;
                let Some(window) = matching_window(&record, &windows) else {
                    skipped += 1;
                    continue;
                };
                let Some(process) = foreground_codex(&record) else {
                    skipped += 1;
                    continue;
                };
                candidates.push(Candidate {
                    owner: owner.clone(),
                    terminal: record,
                    window: window.clone(),
                    process,
                });
                if candidates.len() >= MAX_TARGETS {
                    break;
                }
            }
            if candidates.len() >= MAX_TARGETS {
                break;
            }
        }
        Ok((candidates, skipped))
    }

    fn pin(&mut self, process: &ProcessIdentity) -> Result<OwnedFd, String> {
        let pid = Pid::from_raw(process.pid).ok_or("Invalid Codex PID")?;
        let handle = pidfd_open(pid, PidfdFlags::empty()).map_err(|err| err.to_string())?;
        if read_identity(process.pid).as_ref() != Some(process) {
            return Err("Codex process changed".into());
        }
        Ok(handle)
    }

    fn refresh(&mut self, candidate: &Candidate) -> Result<Option<Candidate>, String> {
        let windows = self.windows()?;
        let records = self.records(&candidate.owner, candidate.terminal.terminal_pid)?;
        let mut matches = records
            .into_iter()
            .filter(|record| record.tab_uuid == candidate.terminal.tab_uuid);
        let Some(record) = matches.next() else {
            return Ok(None);
        };
        if matches.next().is_some() {
            return Ok(None);
        }
        let Some(window) = matching_window(&record, &windows) else {
            return Ok(None);
        };
        let Some(process) = foreground_codex(&record) else {
            return Ok(None);
        };
        Ok(Some(Candidate {
            owner: candidate.owner.clone(),
            terminal: record,
            window: window.clone(),
            process,
        }))
    }

    fn interrupt(&mut self, candidate: &Candidate, handle: &OwnedFd) -> Result<(), String> {
        self.check_deadline()?;
        if read_identity(candidate.process.pid).as_ref() != Some(&candidate.process) {
            return Err("Codex identity or foreground process changed; skipped".into());
        }
        send_interrupt(handle)
    }
}

#[cfg(not(test))]
fn send_interrupt(handle: &OwnedFd) -> Result<(), String> {
    // Signal only the pinned Codex process, never its shell, group, or terminal.
    pidfd_send_signal(handle, Signal::INT).map_err(|err| err.to_string())
}

#[cfg(test)]
fn send_interrupt(_: &OwnedFd) -> Result<(), String> {
    Err("Real Codex interruption is disabled in test builds".into())
}

pub(crate) fn stop_idle_codex() -> Result<String, String> {
    let deadline = Instant::now() + BATCH_TIMEOUT;
    let connection = zbus::blocking::connection::Builder::session()
        .map_err(|err| err.to_string())?
        .method_timeout(Duration::from_millis(750))
        .build()
        .map_err(|err| err.to_string())?;
    let mut backend = LiveBackend {
        connection,
        deadline,
    };
    let report = run_batch(&mut backend, deadline)?;
    let message = report.to_string();
    applicationlauncher::observability::record(
        applicationlauncher::observability::Event::new("terminal", "stop-idle-codex")
            .reason(&message),
    );
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: i32, ppid: i32, program: &str) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            ppid,
            start_ticks: 12345,
            uid: 1000,
            group: 30,
            session: 20,
            tty: 34817,
            foreground_group: 30,
            executable: PathBuf::from(format!("/usr/bin/{program}")),
        }
    }

    fn candidate(pid: i32) -> Candidate {
        let title = format!("codex - ~/project-{pid} - Terminal");
        Candidate {
            owner: ":1.50".into(),
            terminal: TerminalDbusRecord {
                terminal_pid: 10,
                window_uuid: format!("window-{pid}"),
                tab_uuid: format!("tab-{pid}"),
                active: true,
                window_title: title.clone(),
                working_directory: "/home/user/project".into(),
                child_pid: 20,
                foreground_pid: 30,
                foreground_pgid: 30,
                pty: "/dev/pts/1".into(),
            },
            window: TrackedWindow {
                id: format!("kwin-{pid}"),
                pid: 10,
                class: "xfce4-terminal".into(),
                title,
                ..Default::default()
            },
            process: process(pid, 20, "codex"),
        }
    }

    #[derive(Default)]
    struct FakeBackend {
        candidates: Vec<Candidate>,
        refreshed: HashMap<i32, Option<Candidate>>,
        sent: Vec<i32>,
        pin_failure: bool,
        refresh_failure: bool,
        send_failure: bool,
    }

    impl Backend for FakeBackend {
        type Handle = i32;
        fn scan(&mut self) -> Result<(Vec<Candidate>, usize), String> {
            Ok((self.candidates.clone(), 0))
        }
        fn pin(&mut self, identity: &ProcessIdentity) -> Result<i32, String> {
            if self.pin_failure {
                Err("pidfd unavailable".into())
            } else {
                Ok(identity.pid)
            }
        }
        fn refresh(&mut self, candidate: &Candidate) -> Result<Option<Candidate>, String> {
            if self.refresh_failure {
                return Err("D-Bus unavailable".into());
            }
            Ok(self
                .refreshed
                .get(&candidate.process.pid)
                .cloned()
                .unwrap_or_else(|| Some(candidate.clone())))
        }
        fn interrupt(&mut self, candidate: &Candidate, handle: &i32) -> Result<(), String> {
            assert_eq!(*handle, candidate.process.pid);
            self.sent.push(*handle);
            if self.send_failure {
                Err("signal rejected".into())
            } else {
                Ok(())
            }
        }
    }

    fn run(backend: &mut FakeBackend) -> Report {
        run_batch(backend, Instant::now() + Duration::from_secs(2)).unwrap()
    }

    #[test]
    fn interrupts_each_idle_session_once_and_preserves_busy_or_attention_sessions() {
        let mut spinner = candidate(32);
        spinner.terminal.window_title = "codex - \u{2834} project - Terminal".into();
        spinner.window.title = spinner.terminal.window_title.clone();
        let mut attention = candidate(33);
        attention.window.demands_attention = true;
        let mut backend = FakeBackend {
            candidates: vec![
                candidate(30),
                spinner,
                attention,
                candidate(31),
                candidate(30),
            ],
            ..Default::default()
        };
        assert_eq!(
            run(&mut backend),
            Report {
                interrupted: 2,
                skipped: 3,
                ..Default::default()
            }
        );
        assert_eq!(backend.sent, [30, 31]);
    }

    #[test]
    fn both_spinner_frames_attention_blinks_and_missing_titles_are_protected() {
        for title in [
            "",
            "   ",
            "codex - \u{2800} project",
            "codex - \u{28ff} project",
            "codex - [ ! ] Action Required",
            "codex - [ . ] ACTION REQUIRED",
            "codex [!]",
            "codex [ . ]",
        ] {
            assert!(!idle_title(title), "{title:?}");
        }
        assert!(idle_title("codex - ~/project - Terminal"));
    }

    #[test]
    fn a_session_that_starts_work_or_changes_identity_after_click_is_skipped() {
        let before = candidate(30);
        for mutation in 0..9 {
            let mut after = before.clone();
            match mutation {
                0 => after.window.demands_attention = true,
                1 => after.terminal.window_title.push('\u{280b}'),
                2 => after.window.title.push_str(" - Action Required"),
                3 => after.process.start_ticks += 1,
                4 => after.process.executable = PathBuf::from("/usr/bin/fish"),
                5 => after.process.foreground_group += 1,
                6 => after.terminal.tab_uuid = "new-tab".into(),
                7 => after.terminal.active = false,
                _ => after.owner = ":1.51".into(),
            }
            let mut backend = FakeBackend {
                candidates: vec![before.clone()],
                refreshed: HashMap::from([(30, Some(after))]),
                ..Default::default()
            };
            assert_eq!(run(&mut backend).interrupted, 0, "mutation {mutation}");
            assert!(backend.sent.is_empty());
        }
    }

    #[test]
    fn never_targets_terminals_shells_ssh_or_helpers_based_on_a_codex_title() {
        for executable in [
            "xfce4-terminal",
            "fish",
            "ssh",
            "node",
            "chrome_crashpad",
            "not-codex",
            "codex-helper",
        ] {
            let mut value = candidate(30);
            value.process.executable = PathBuf::from(format!("/usr/bin/{executable}"));
            assert!(!eligible(&value), "{executable}");
        }
        let mut value = candidate(10);
        assert!(!eligible(&value));
        value = candidate(30);
        value.process.tty = 0;
        assert!(!eligible(&value));
        value = candidate(30);
        value.terminal.foreground_pgid = 0;
        assert!(!eligible(&value));
    }

    #[test]
    fn requires_one_matching_window_and_never_guesses_shared_server_tabs() {
        let value = candidate(30);
        assert!(matching_window(&value.terminal, &[value.window.clone()]).is_some());
        let mut second = value.window.clone();
        second.id = "another-window".into();
        assert!(
            matching_window(&value.terminal, &[value.window.clone(), second.clone()]).is_none()
        );
        second.title = "fish - Terminal".into();
        assert!(matching_window(&value.terminal, &[value.window.clone(), second]).is_some());
        let mut wrong = value.window.clone();
        wrong.class = "CopyQ".into();
        assert!(matching_window(&value.terminal, &[wrong]).is_none());
        wrong = value.window.clone();
        wrong.pid += 1;
        assert!(matching_window(&value.terminal, &[wrong]).is_none());
        assert!(matching_window(&value.terminal, &[]).is_none());
    }

    #[test]
    fn finds_outer_foreground_codex_through_wrappers_but_not_nested_agents() {
        let value = candidate(30);
        let mut processes = HashMap::from([
            (20, process(20, 10, "fish")),
            (30, process(30, 20, "node")),
            (31, process(31, 30, "codex")),
            (32, process(32, 31, "codex")),
        ]);
        let children = HashMap::from([(20, vec![30]), (30, vec![31]), (31, vec![32])]);
        let inspect = |pid| processes.get(&pid).cloned();
        let read_children = |pid| Some(children.get(&pid).cloned().unwrap_or_default());
        assert_eq!(
            foreground_codex_with(&value.terminal, inspect, read_children)
                .unwrap()
                .pid,
            31
        );
        processes.get_mut(&31).unwrap().group = 40;
        assert!(
            foreground_codex_with(
                &value.terminal,
                |pid| processes.get(&pid).cloned(),
                read_children
            )
            .is_none()
        );
        processes.get_mut(&31).unwrap().group = 30;
        processes.get_mut(&31).unwrap().session = 99;
        assert!(
            foreground_codex_with(
                &value.terminal,
                |pid| processes.get(&pid).cloned(),
                read_children
            )
            .is_none()
        );
    }

    #[test]
    fn ambiguous_foreground_processes_missing_children_and_cycles_are_skipped() {
        let value = candidate(30);
        let processes = HashMap::from([
            (20, process(20, 10, "fish")),
            (30, process(30, 20, "codex")),
            (31, process(31, 20, "codex")),
        ]);
        assert!(
            foreground_codex_with(
                &value.terminal,
                |pid| processes.get(&pid).cloned(),
                |pid| Some(if pid == 20 { vec![30, 31] } else { vec![] })
            )
            .is_none()
        );
        assert!(
            foreground_codex_with(
                &value.terminal,
                |pid| processes.get(&pid).cloned(),
                |_| None
            )
            .is_none()
        );
        assert!(
            foreground_codex_with(
                &value.terminal,
                |pid| processes.get(&pid).cloned(),
                |_| Some(vec![20])
            )
            .is_none()
        );
    }

    #[test]
    fn terminal_main_commands_and_unrelated_shell_roots_are_skipped() {
        let value = candidate(30);
        for program in ["codex", "node", "ssh"] {
            assert!(
                foreground_codex_with(
                    &value.terminal,
                    |pid| Some(process(pid, 10, program)),
                    |_| panic!("must reject the root before walking its descendants")
                )
                .is_none()
            );
        }
        assert!(
            foreground_codex_with(
                &value.terminal,
                |pid| Some(process(pid, 999, "fish")),
                |_| panic!("unrelated shell must not be traversed")
            )
            .is_none()
        );
        let mut direct = candidate(30);
        direct.terminal.child_pid = 30;
        assert!(!eligible(&direct));
    }

    #[test]
    fn failures_and_disappearing_sessions_never_retry_or_escalate() {
        for failure in 0..4 {
            let mut backend = FakeBackend {
                candidates: vec![candidate(30)],
                ..Default::default()
            };
            match failure {
                0 => backend.pin_failure = true,
                1 => backend.refresh_failure = true,
                2 => backend.send_failure = true,
                _ => {
                    backend.refreshed.insert(30, None);
                }
            }
            let report = run(&mut backend);
            assert_eq!(report.interrupted, 0);
            assert_eq!(report.failed + report.skipped, 1);
            assert_eq!(backend.sent.len(), usize::from(failure == 2));
            assert_eq!(report.first_error.is_some(), failure < 3);
            assert_eq!(report.to_string().contains("First failure:"), failure < 3);
        }
    }

    #[test]
    fn failure_details_are_bounded_and_keep_the_first_reason() {
        let mut report = Report::default();
        report.fail("Interrupt", &"error\n".repeat(1000));
        let first_error = report.first_error.clone();
        report.fail("Fresh terminal state", "a later error");
        assert_eq!(report.failed, 2);
        assert_eq!(report.first_error, first_error);
        let detail = report.first_error.unwrap();
        assert!(detail.len() <= 251);
        assert!(!detail.contains('\n'));
    }

    #[test]
    fn deadline_prevents_any_interrupt() {
        let mut backend = FakeBackend {
            candidates: vec![candidate(30)],
            ..Default::default()
        };
        let report = run_batch(&mut backend, Instant::now()).unwrap();
        assert_eq!(report.skipped, 1);
        assert!(backend.sent.is_empty());
    }

    #[test]
    fn real_signal_delivery_is_disabled_in_tests() {
        let file = std::fs::File::open("/dev/null").unwrap();
        assert!(
            send_interrupt(&OwnedFd::from(file))
                .unwrap_err()
                .contains("disabled in test builds")
        );
    }
}
