use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc::Receiver};
use std::time::{Duration, Instant};

use eframe::egui;

use super::process::{is_shell, parse_proc_stat, summarize_command_line};
use crate::models::{ProcessIdentity, ProcessInfoNode, ProcessTreeSnapshot, WindowInfo};

const MAX_SCANNED_PROCESSES: usize = 32_768;
const MAX_TREE_PROCESSES: usize = 512;
const MAX_DEPTH: usize = 64;
const MAX_STAT_BYTES: usize = 8192;
const CAPTURE_BUDGET: Duration = Duration::from_millis(500);
const BRANCH: &str = "\u{251c}\u{2500}\u{2500} ";
const LAST_BRANCH: &str = "\u{2514}\u{2500}\u{2500} ";
const CONTINUATION: &str = "\u{2502}   ";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowProcessTreeTarget {
    id: String,
    owner: Option<i32>,
    active: Option<i32>,
    pane: Option<i32>,
    client: Option<i32>,
}

impl WindowProcessTreeTarget {
    pub(crate) fn for_window(window: &WindowInfo) -> Self {
        Self {
            id: window.id.clone(),
            owner: window.pid.filter(|pid| *pid > 0),
            active: window
                .active_process
                .as_ref()
                .and_then(|_| window.process_chain.first())
                .map(|entry| entry.pid)
                .filter(|pid| *pid > 0),
            pane: window.tmux_pane.as_ref().map(|pane| pane.pane_pid),
            client: window.tmux_pane.as_ref().map(|pane| pane.client_pid),
        }
    }

    fn seeds(&self) -> impl Iterator<Item = i32> + '_ {
        [self.owner, self.active, self.pane, self.client]
            .into_iter()
            .flatten()
            .filter(|pid| *pid > 0)
    }
}

pub(crate) struct WindowProcessTreeLookup {
    pub(crate) target: WindowProcessTreeTarget,
    receiver: Receiver<Arc<ProcessTreeSnapshot>>,
    pub(crate) result: Option<Arc<ProcessTreeSnapshot>>,
}

impl WindowProcessTreeLookup {
    pub(crate) fn start(
        target: WindowProcessTreeTarget,
        ctx: egui::Context,
        viewport_id: egui::ViewportId,
    ) -> Self {
        let (tx, receiver) = std::sync::mpsc::sync_channel(1);
        let request = target.clone();
        applicationlauncher::observability::spawn_named("window-process-tree", move |worker| {
            worker.set_state("capturing");
            let document = capture_snapshot(&request, Path::new("/proc"));
            let _ = tx.send(Arc::from(document));
            ctx.request_repaint_of(egui::ViewportId::ROOT);
            ctx.request_repaint_of(viewport_id);
        });
        Self {
            target,
            receiver,
            result: None,
        }
    }

    pub(crate) fn poll(&mut self) {
        if self.result.is_some() {
            return;
        }
        match self.receiver.try_recv() {
            Ok(document) => self.result = Some(document),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.result = Some(Arc::new(ProcessTreeSnapshot::unavailable(
                    "Process tree unavailable: collection did not complete.",
                )));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Process {
    pid: i32,
    parent: i32,
    started: u64,
    name: String,
}

fn read_process(root: &Path, pid: i32) -> Option<Process> {
    let mut bytes = Vec::new();
    File::open(root.join(pid.to_string()).join("stat"))
        .ok()?
        .take(MAX_STAT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_STAT_BYTES {
        return None;
    }
    let stat = std::str::from_utf8(&bytes).ok()?;
    let process = parse_proc_stat(stat)?;
    if process.pid != pid {
        return None;
    }
    let fields = &stat[stat.rfind(')')? + 1..];
    Some(Process {
        pid,
        parent: process.ppid,
        started: fields.split_whitespace().nth(19)?.parse().ok()?,
        name: process.name,
    })
}

fn parent_of(process: &Process, index: &BTreeMap<i32, Process>) -> Option<i32> {
    index.get(&process.parent).and_then(|parent| {
        (parent.pid != process.pid && parent.started <= process.started).then_some(parent.pid)
    })
}

fn select_processes(
    target: &WindowProcessTreeTarget,
    index: &BTreeMap<i32, Process>,
) -> (BTreeSet<i32>, bool) {
    let mut selected = BTreeSet::new();
    let mut incomplete = false;
    // Add the important paths first so a large application's helpers cannot hide its active pane.
    for seed in target.seeds() {
        let mut current = Some(seed);
        let mut path = HashSet::new();
        for _ in 0..MAX_DEPTH {
            let Some(pid) = current else { break };
            let Some(process) = index.get(&pid) else {
                incomplete = true;
                break;
            };
            if !path.insert(pid) {
                incomplete = true;
                break;
            }
            if selected.len() >= MAX_TREE_PROCESSES && !selected.contains(&pid) {
                incomplete = true;
                break;
            }
            selected.insert(pid);
            current = parent_of(process, index);
            incomplete |= process.parent > 0 && current.is_none();
        }
        incomplete |= current.is_some();
    }

    let mut children: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    for process in index.values() {
        if let Some(parent) = parent_of(process, index) {
            children.entry(parent).or_default().push(process.pid);
        }
    }
    let mut pending: VecDeque<_> = target.seeds().map(|pid| (pid, 0)).collect();
    let mut visited = HashSet::new();
    // Traverse only the application's roots, never the descendants of its systemd ancestors.
    while let Some((pid, depth)) = pending.pop_front() {
        if !visited.insert(pid) || !index.contains_key(&pid) {
            continue;
        }
        if depth >= MAX_DEPTH || (selected.len() >= MAX_TREE_PROCESSES && !selected.contains(&pid))
        {
            incomplete = true;
            continue;
        }
        selected.insert(pid);
        if pid > 1
            && let Some(children) = children.get(&pid)
        {
            for child in children {
                if pending.len() >= MAX_TREE_PROCESSES {
                    incomplete = true;
                    break;
                }
                pending.push_back((*child, depth + 1));
            }
        }
    }
    (selected, incomplete)
}

fn single_line(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                ' '
            } else {
                ch
            }
        })
        .collect()
}

struct TreeNode {
    process: Process,
    executable: Option<PathBuf>,
    script: Option<String>,
    executable_identity: Option<(u64, u64)>,
    cwd: Option<PathBuf>,
    command_line: Option<String>,
    command_summary: Option<String>,
}

fn script_name(root: &Path, process: &Process, arguments: &[String]) -> Option<String> {
    if !is_shell(&process.name) && !process.name.starts_with("python") {
        return None;
    }
    let script = arguments.get(1)?.as_str();
    if script.is_empty() || script.starts_with('-') {
        return None;
    }
    let process_root = root.join(process.pid.to_string());
    let descriptor = ["/proc/self/fd/", "/dev/fd/"]
        .into_iter()
        .find_map(|prefix| script.strip_prefix(prefix))
        .and_then(|fd| fd.parse::<u32>().ok());
    if let Some(fd) = descriptor {
        let path = std::fs::read_link(process_root.join("fd").join(fd.to_string())).ok()?;
        let path = path.to_str()?;
        if let Some(name) = path.strip_prefix("/memfd:") {
            return Some(single_line(name.trim_end_matches(" (deleted)")));
        }
        return path.rsplit('/').next().map(single_line);
    }
    Path::new(script).file_name()?.to_str().map(single_line)
}

fn read_node(root: &Path, process: &Process) -> Option<TreeNode> {
    let process_root = root.join(process.pid.to_string());
    let executable = std::fs::read_link(process_root.join("exe")).ok();
    let executable_identity = std::fs::metadata(process_root.join("exe"))
        .ok()
        .map(|metadata| (metadata.dev(), metadata.ino()));
    let cwd = std::fs::read_link(process_root.join("cwd")).ok();
    let mut bytes = Vec::new();
    let command_read = File::open(process_root.join("cmdline"))
        .ok()
        .and_then(|file| file.take(16_385).read_to_end(&mut bytes).ok());
    let truncated = bytes.len() > 16_384;
    bytes.truncate(16_384);
    let arguments: Vec<_> = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    let script = script_name(root, process, &arguments);
    let command_summary = command_read.and_then(|_| summarize_command_line(&arguments));
    let command_line = command_read.map(|_| {
        let mut command = single_line(&arguments.join(" "));
        if truncated {
            command.push_str(" [truncated at 16384 bytes]");
        }
        command
    });
    // Do not attach details read from a reused PID or a newly reparented process.
    (read_process(root, process.pid).as_ref() == Some(process)).then(|| TreeNode {
        process: process.clone(),
        executable,
        script,
        executable_identity,
        cwd,
        command_line,
        command_summary,
    })
}

pub(crate) fn process_identity_is_current(
    identity: ProcessIdentity,
    executable_identity: Option<(u64, u64)>,
) -> bool {
    read_process(Path::new("/proc"), identity.pid)
        .is_some_and(|process| process.started == identity.start_ticks)
        && executable_identity.is_none_or(|expected| {
            std::fs::metadata(format!("/proc/{}/exe", identity.pid))
                .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == expected)
        })
}

fn render_snapshot(
    target: &WindowProcessTreeTarget,
    nodes: &BTreeMap<i32, TreeNode>,
) -> (String, Vec<ProcessInfoNode>) {
    fn append(
        pid: i32,
        depth: usize,
        prefix: &str,
        branch: Option<bool>,
        target: &WindowProcessTreeTarget,
        nodes: &BTreeMap<i32, TreeNode>,
        visited: &mut HashSet<i32>,
        output: &mut (String, Vec<ProcessInfoNode>),
    ) {
        if !visited.insert(pid) {
            return;
        }
        let node = &nodes[&pid];
        let mut output_line = String::new();
        let output_text = &mut output_line;
        output_text.push_str(prefix);
        if let Some(last) = branch {
            output_text.push_str(if last { LAST_BRANCH } else { BRANCH });
        }
        let header_start = output_text.len();
        output_text.push_str(&single_line(&node.process.name));
        if let Some(script) = &node.script {
            output_text.push_str(&format!(": {script}"));
        }
        output_text.push_str(&format!(" (pid {pid})"));
        for (role_pid, label) in [
            (target.owner, "window owner"),
            (target.active, "active process"),
            (target.pane, "tmux pane"),
            (target.client, "tmux client"),
        ] {
            if role_pid == Some(pid) {
                output_text.push_str(&format!(" [{label}]"));
            }
        }
        output_text.push_str(&format!(
            " - {}\n",
            node.executable
                .as_ref()
                .map(|path| single_line(&path.to_string_lossy()))
                .unwrap_or_else(|| "Executable path unavailable".into())
        ));
        #[cfg(test)]
        output.0.push_str(&output_line);
        output.1.push(ProcessInfoNode {
            identity: ProcessIdentity {
                pid,
                start_ticks: node.process.started,
            },
            parent_pid: node.process.parent,
            depth,
            name: node.process.name.clone(),
            header: output_line[header_start..].trim_end().into(),
            executable: node.executable.clone(),
            executable_identity: node.executable_identity,
            cwd: node.cwd.clone(),
            command_line: node.command_line.clone(),
            command_summary: node.command_summary.clone(),
        });
        let next_prefix = format!(
            "{prefix}{}",
            match branch {
                Some(false) => CONTINUATION,
                Some(true) => "    ",
                None => "",
            }
        );
        let children: Vec<_> = nodes
            .values()
            .filter(|child| {
                child.process.parent == pid
                    && child.process.started >= node.process.started
                    && child.process.pid != pid
            })
            .map(|child| child.process.pid)
            .collect();
        for (index, child) in children.iter().enumerate() {
            append(
                *child,
                depth + 1,
                &next_prefix,
                Some(index + 1 == children.len()),
                target,
                nodes,
                visited,
                output,
            );
        }
    }

    let mut output = (String::new(), Vec::new());
    let mut visited = HashSet::new();
    for node in nodes.values().filter(|node| {
        !nodes.get(&node.process.parent).is_some_and(|parent| {
            parent.process.pid != node.process.pid && parent.process.started <= node.process.started
        })
    }) {
        append(
            node.process.pid,
            0,
            "",
            None,
            target,
            nodes,
            &mut visited,
            &mut output,
        );
    }
    // A malformed/reparented snapshot must neither loop nor silently lose a disconnected branch.
    for pid in nodes
        .keys()
        .filter(|pid| !visited.contains(pid))
        .copied()
        .collect::<Vec<_>>()
    {
        if !visited.contains(&pid) {
            #[cfg(test)]
            output.0.push_str("[Parent chain incomplete]\n");
            append(pid, 0, "", None, target, nodes, &mut visited, &mut output);
        }
    }
    output
}

#[cfg(test)]
fn render_tree(target: &WindowProcessTreeTarget, nodes: &BTreeMap<i32, TreeNode>) -> String {
    render_snapshot(target, nodes).0
}

#[cfg(test)]
fn capture(target: &WindowProcessTreeTarget, root: &Path) -> String {
    capture_snapshot(target, root).text.to_string()
}

fn capture_snapshot(target: &WindowProcessTreeTarget, root: &Path) -> ProcessTreeSnapshot {
    if target.seeds().next().is_none() {
        return ProcessTreeSnapshot::unavailable(
            "Process tree unavailable: this window has no process ID.",
        );
    }
    let deadline = Instant::now() + CAPTURE_BUDGET;
    let mut index = BTreeMap::new();
    let mut incomplete = false;
    for pid in target.seeds() {
        if let Some(process) = read_process(root, pid) {
            index.insert(pid, process);
        }
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return ProcessTreeSnapshot::unavailable(
            "Process tree unavailable: cannot read the process directory.",
        );
    };
    for (count, entry) in entries.flatten().enumerate() {
        if count >= MAX_SCANNED_PROCESSES || Instant::now() >= deadline {
            incomplete = true;
            break;
        }
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        if !index.contains_key(&pid)
            && let Some(process) = read_process(root, pid)
        {
            index.insert(pid, process);
        }
    }
    let (selected, limited) = select_processes(target, &index);
    incomplete |= limited;
    let mut nodes = BTreeMap::new();
    for pid in selected {
        if Instant::now() >= deadline {
            incomplete = true;
            break;
        }
        if let Some(node) = read_node(root, &index[&pid]) {
            nodes.insert(pid, node);
        } else {
            incomplete = true;
        }
    }
    if target.seeds().any(|pid| {
        index
            .get(&pid)
            .is_some_and(|process| read_process(root, pid).as_ref() != Some(process))
    }) {
        return ProcessTreeSnapshot::unavailable(
            "Process tree unavailable: a target process exited or changed during collection. Reopen Show Info for a new snapshot.",
        );
    }
    if nodes.is_empty() {
        return ProcessTreeSnapshot::unavailable(
            "Process tree unavailable: the processes exited or could not be inspected.",
        );
    }
    let (_document, nodes) = render_snapshot(target, &nodes);
    let mut notice = String::new();
    if incomplete {
        notice.push_str("Partial snapshot: a process exited, changed, was unreadable, or a collection limit was reached.\n");
    }
    notice.push_str("Captured when Show Info opened; threads omitted. Ancestors are shown without their unrelated children.");
    #[cfg(test)]
    let text = format!("{_document}\n{notice}\n").into();
    ProcessTreeSnapshot {
        nodes: nodes.into(),
        notice: notice.into(),
        #[cfg(test)]
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "applicationlauncher-process-tree-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn add(&self, pid: i32, parent: i32, name: &str) {
            let directory = self.0.join(pid.to_string());
            std::fs::create_dir(&directory).unwrap();
            let mut fields = vec!["0".to_string(); 20];
            fields[0] = "S".into();
            fields[1] = parent.to_string();
            fields[2] = pid.to_string();
            fields[3] = "1".into();
            fields[5] = "-1".into();
            fields[19] = pid.to_string();
            std::fs::write(
                directory.join("stat"),
                format!("{pid} ({name}) {}\n", fields.join(" ")),
            )
            .unwrap();
            symlink(format!("/usr/bin/{name}"), directory.join("exe")).unwrap();
        }

        fn base(&self) {
            self.add(1, 0, "systemd");
            self.add(2, 1, "systemd");
            self.add(3, 2, "xfce4-terminal");
            self.add(4, 3, "fish");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn target(owner: i32, active: Option<i32>) -> WindowProcessTreeTarget {
        WindowProcessTreeTarget {
            id: "window".into(),
            owner: Some(owner),
            active,
            pane: None,
            client: None,
        }
    }

    fn line<'a>(document: &'a str, marker: &str) -> &'a str {
        document.lines().find(|line| line.contains(marker)).unwrap()
    }

    #[test]
    fn process_tree_renders_each_process_and_its_executable_on_one_line() {
        let request = target(3140, Some(2740504));
        let nodes = [
            (3140, 0, "systemd", None),
            (2740397, 3140, "ssh", Some("/usr/bin/ssh")),
            (2740504, 3140, "pi", Some("/usr/bin/node")),
        ]
        .into_iter()
        .map(|(pid, parent, name, executable)| {
            (
                pid,
                TreeNode {
                    process: Process {
                        pid,
                        parent,
                        started: pid as u64,
                        name: name.into(),
                    },
                    executable: executable.map(PathBuf::from),
                    script: None,
                    executable_identity: None,
                    cwd: None,
                    command_line: None,
                    command_summary: None,
                },
            )
        })
        .collect();
        assert_eq!(
            render_tree(&request, &nodes),
            format!(
                "systemd (pid 3140) [window owner] - Executable path unavailable\n\
                 {BRANCH}ssh (pid 2740397) - /usr/bin/ssh\n\
                 {LAST_BRANCH}pi (pid 2740504) [active process] - /usr/bin/node\n"
            )
        );
        let (_, sections) = render_snapshot(&request, &nodes);
        assert_eq!(
            sections.iter().map(|node| node.depth).collect::<Vec<_>>(),
            [0, 1, 1]
        );
        assert!(sections[1].header.starts_with("ssh (pid "));
        assert!(sections[2].header.starts_with("pi (pid "));
    }

    #[test]
    fn process_tree_shows_pi_and_ssh_as_siblings_not_as_an_ancestor_chain() {
        let fixture = Fixture::new();
        fixture.base();
        fixture.add(5, 4, "bash");
        fixture.add(6, 5, "ssh");
        fixture.add(7, 5, "pi");
        fixture.add(8, 2, "unrelated-application");
        let shell = fixture.0.join("5");
        std::fs::create_dir(shell.join("fd")).unwrap();
        std::fs::write(shell.join("cmdline"), b"bash\0/proc/self/fd/3\0").unwrap();
        symlink(
            "/memfd:pi-remote-qwen-radiance (deleted)",
            shell.join("fd/3"),
        )
        .unwrap();
        std::fs::remove_file(fixture.0.join("7/exe")).unwrap();
        symlink("/usr/bin/node", fixture.0.join("7/exe")).unwrap();

        let document = capture(&target(3, Some(7)), &fixture.0);
        assert!(document.starts_with("systemd (pid 1) - /usr/bin/systemd\n"));
        assert!(document.contains("bash: pi-remote-qwen-radiance (pid 5) - /usr/bin/bash\n"));
        assert_eq!(
            line(&document, "ssh (pid 6)"),
            format!("                {BRANCH}ssh (pid 6) - /usr/bin/ssh")
        );
        assert_eq!(
            line(&document, "pi (pid 7)"),
            format!("                {LAST_BRANCH}pi (pid 7) [active process] - /usr/bin/node")
        );
        assert!(document.contains("/usr/bin/node\n"));
        assert!(!document.contains("unrelated-application"));
        assert!(!document.contains("Partial snapshot"));
        assert!(document.contains("[window owner]"));
    }

    #[test]
    fn process_tree_includes_nonterminal_application_children_but_not_other_apps() {
        let fixture = Fixture::new();
        fixture.add(1, 0, "systemd");
        fixture.add(2, 1, "systemd");
        fixture.add(10, 2, "firefox");
        fixture.add(11, 10, "Web Content");
        fixture.add(12, 10, "Socket Process");
        fixture.add(13, 11, "helper");
        fixture.add(20, 2, "another-firefox");
        let document = capture(&target(10, None), &fixture.0);
        assert!(document.contains("firefox (pid 10) [window owner]"));
        assert_eq!(
            line(&document, "Web Content (pid 11)"),
            format!("        {BRANCH}Web Content (pid 11) - /usr/bin/Web Content")
        );
        assert_eq!(
            line(&document, "helper (pid 13)"),
            format!("        {CONTINUATION}{LAST_BRANCH}helper (pid 13) - /usr/bin/helper")
        );
        assert_eq!(
            line(&document, "Socket Process (pid 12)"),
            format!("        {LAST_BRANCH}Socket Process (pid 12) - /usr/bin/Socket Process")
        );
        assert!(!document.contains("another-firefox"));
        assert!(!document.contains("Partial snapshot"));
    }

    #[test]
    fn process_tree_keeps_detached_tmux_panes_under_their_real_server() {
        let fixture = Fixture::new();
        fixture.base();
        fixture.add(5, 4, "tmux-client");
        fixture.add(20, 2, "tmux-server");
        fixture.add(21, 20, "fish");
        fixture.add(22, 21, "bash");
        fixture.add(23, 22, "ssh");
        fixture.add(24, 22, "pi");
        fixture.add(25, 20, "other-pane");
        let mut request = target(3, Some(24));
        request.pane = Some(21);
        request.client = Some(5);
        let document = capture(&request, &fixture.0);
        assert_eq!(
            line(&document, "xfce4-terminal (pid 3)"),
            format!("    {BRANCH}xfce4-terminal (pid 3) [window owner] - /usr/bin/xfce4-terminal")
        );
        assert_eq!(
            line(&document, "tmux-server (pid 20)"),
            format!("    {LAST_BRANCH}tmux-server (pid 20) - /usr/bin/tmux-server")
        );
        assert!(document.contains("tmux-client (pid 5) [tmux client]"));
        assert!(document.contains("fish (pid 21) [tmux pane]"));
        assert_eq!(
            line(&document, "ssh (pid 23)"),
            format!("                {BRANCH}ssh (pid 23) - /usr/bin/ssh")
        );
        assert_eq!(
            line(&document, "pi (pid 24)"),
            format!("                {LAST_BRANCH}pi (pid 24) [active process] - /usr/bin/pi")
        );
        assert!(!document.contains("other-pane"));
        assert_eq!(document.matches("systemd (pid 1)").count(), 1);
        assert!(!document.contains("Partial snapshot"));
    }

    #[test]
    fn process_tree_output_is_deterministic_and_has_explicit_size_limits() {
        let index: BTreeMap<_, _> = (1..=MAX_TREE_PROCESSES as i32 + 100)
            .rev()
            .map(|pid| {
                (
                    pid,
                    Process {
                        pid,
                        parent: if pid == 1 {
                            0
                        } else if pid == 2 {
                            1
                        } else {
                            2
                        },
                        started: pid as u64,
                        name: "app".into(),
                    },
                )
            })
            .collect();
        let request = target(2, Some(MAX_TREE_PROCESSES as i32 + 99));
        let (selected, incomplete) = select_processes(&request, &index);
        assert!(incomplete);
        assert_eq!(selected.len(), MAX_TREE_PROCESSES);
        assert!(selected.contains(&request.active.unwrap()));
        let nodes = selected
            .into_iter()
            .map(|pid| {
                (
                    pid,
                    TreeNode {
                        process: index[&pid].clone(),
                        executable: None,
                        script: None,
                        executable_identity: None,
                        cwd: None,
                        command_line: None,
                        command_summary: None,
                    },
                )
            })
            .collect();
        let output = render_tree(&request, &nodes);
        assert_eq!(output, render_tree(&request, &nodes));
        assert_eq!(output.matches("(pid ").count(), MAX_TREE_PROCESSES);
        assert!(output.find("app (pid 3)").unwrap() < output.find("app (pid 4)").unwrap());
    }

    #[test]
    fn process_tree_handles_cycles_depth_limits_and_missing_ancestors() {
        let cycle: BTreeMap<_, _> = [(10, 11), (11, 10)]
            .into_iter()
            .map(|(pid, parent)| {
                (
                    pid,
                    Process {
                        pid,
                        parent,
                        started: 1,
                        name: "app".into(),
                    },
                )
            })
            .collect();
        let request = target(10, None);
        let (selected, incomplete) = select_processes(&request, &cycle);
        assert!(incomplete);
        let nodes = selected
            .into_iter()
            .map(|pid| {
                (
                    pid,
                    TreeNode {
                        process: cycle[&pid].clone(),
                        executable: None,
                        script: None,
                        executable_identity: None,
                        cwd: None,
                        command_line: None,
                        command_summary: None,
                    },
                )
            })
            .collect();
        let output = render_tree(&request, &nodes);
        assert!(output.contains("[Parent chain incomplete]"));
        assert_eq!(output.matches("(pid ").count(), 2);

        let deep: BTreeMap<_, _> = (1..=MAX_DEPTH as i32 + 20)
            .map(|pid| {
                (
                    pid,
                    Process {
                        pid,
                        parent: pid - 1,
                        started: pid as u64,
                        name: "app".into(),
                    },
                )
            })
            .collect();
        let (selected, incomplete) = select_processes(&target(2, None), &deep);
        assert!(incomplete);
        assert!(selected.len() <= MAX_DEPTH + 1);

        let fixture = Fixture::new();
        fixture.add(20, 10, "orphan");
        let document = capture(&target(20, None), &fixture.0);
        assert!(document.contains("orphan (pid 20) [window owner]"));
        assert!(document.contains("Partial snapshot"));
    }

    #[test]
    fn process_tree_reports_missing_pids_and_unreadable_paths_without_guessing() {
        let fixture = Fixture::new();
        let mut no_pid = target(20, None);
        no_pid.owner = None;
        assert!(capture(&no_pid, &fixture.0).contains("no process ID"));
        assert!(
            capture(&target(20, None), &fixture.0).contains("exited or could not be inspected")
        );
        fixture.add(20, 0, "app");
        std::fs::remove_file(fixture.0.join("20/exe")).unwrap();
        let document = capture(&target(20, None), &fixture.0);
        assert!(document.contains("Executable path unavailable"));
        assert!(!document.contains("Partial snapshot"));
    }

    #[test]
    fn process_tree_rejects_invalid_stats_and_does_not_forge_reused_parent_links() {
        let fixture = Fixture::new();
        fixture.add(10, 20, "child");
        fixture.add(20, 0, "newer-parent");
        let document = capture(&target(10, None), &fixture.0);
        assert!(document.contains("Partial snapshot"));
        assert!(!document.contains("newer-parent"));
        std::fs::write(fixture.0.join("10/stat"), "20 (wrong-pid) S 0 0 0 0 0").unwrap();
        assert!(read_process(&fixture.0, 10).is_none());
        std::fs::write(fixture.0.join("10/stat"), vec![b'x'; MAX_STAT_BYTES + 1]).unwrap();
        assert!(read_process(&fixture.0, 10).is_none());
    }

    #[test]
    fn process_tree_does_not_attach_metadata_from_a_reused_or_exited_pid() {
        let fixture = Fixture::new();
        fixture.add(10, 0, "app");
        let old = read_process(&fixture.0, 10).unwrap();
        assert!(read_node(&fixture.0, &old).is_some());
        let stat_path = fixture.0.join("10/stat");
        let stat = std::fs::read_to_string(&stat_path).unwrap();
        std::fs::write(&stat_path, stat.replace(" 10\n", " 11\n")).unwrap();
        assert!(read_node(&fixture.0, &old).is_none());
        std::fs::remove_file(stat_path).unwrap();
        assert!(read_node(&fixture.0, &old).is_none());
    }

    #[test]
    fn process_tree_lookup_finishes_once_and_does_not_wait_on_the_gui_thread() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let mut lookup = WindowProcessTreeLookup {
            target: target(10, None),
            receiver,
            result: None,
        };
        lookup.poll();
        assert!(lookup.result.is_none());
        sender
            .send(Arc::new(ProcessTreeSnapshot::unavailable(
                "completed snapshot",
            )))
            .ok()
            .unwrap();
        lookup.poll();
        assert_eq!(
            lookup.result.as_ref().map(|result| result.text.as_ref()),
            Some("completed snapshot")
        );
        drop(sender);
        lookup.poll();
        assert_eq!(
            lookup.result.as_ref().map(|result| result.text.as_ref()),
            Some("completed snapshot")
        );

        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        lookup.receiver = receiver;
        lookup.result = None;
        drop(sender);
        lookup.poll();
        assert!(
            lookup
                .result
                .as_deref()
                .unwrap()
                .notice
                .contains("did not complete")
        );
    }

    #[test]
    fn process_tree_sanitizes_multiline_names_and_ignores_threads() {
        let fixture = Fixture::new();
        fixture.add(10, 0, "bad\nname\u{202e}");
        std::fs::create_dir(fixture.0.join("10/task")).unwrap();
        std::fs::write(fixture.0.join("10/task/not-a-process"), "not a process").unwrap();
        let document = capture(&target(10, None), &fixture.0);
        assert!(document.starts_with("bad name  (pid 10)"));
        assert!(!document.contains('\u{202e}'));
        assert!(!document.contains("not-a-process"));
        assert!(document.contains("threads omitted"));
    }

    #[test]
    fn structured_process_tree_captures_each_process_directory_and_command_independently() {
        let fixture = Fixture::new();
        fixture.base();
        fixture.add(5, 4, "pi");
        symlink("/fish-directory", fixture.0.join("4/cwd")).unwrap();
        symlink("/pi-directory", fixture.0.join("5/cwd")).unwrap();
        std::fs::write(fixture.0.join("4/cmdline"), b"fish\0-l\0").unwrap();
        std::fs::write(fixture.0.join("5/cmdline"), b"pi\0--continue\0").unwrap();
        let snapshot = capture_snapshot(&target(3, Some(5)), &fixture.0);
        assert_eq!(
            snapshot
                .nodes
                .iter()
                .map(|node| node.identity.pid)
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        let fish = snapshot
            .nodes
            .iter()
            .find(|node| node.identity.pid == 4)
            .unwrap();
        let pi = snapshot
            .nodes
            .iter()
            .find(|node| node.identity.pid == 5)
            .unwrap();
        assert_eq!(fish.cwd.as_deref(), Some(Path::new("/fish-directory")));
        assert_eq!(pi.cwd.as_deref(), Some(Path::new("/pi-directory")));
        assert_eq!(fish.command_line.as_deref(), Some("fish -l"));
        assert_eq!(pi.command_line.as_deref(), Some("pi --continue"));
        assert_eq!(pi.parent_pid, 4);
        std::fs::write(fixture.0.join("5/cmdline"), vec![b'a'; 20_000]).unwrap();
        let snapshot = capture_snapshot(&target(3, Some(5)), &fixture.0);
        let pi = snapshot
            .nodes
            .iter()
            .find(|node| node.identity.pid == 5)
            .unwrap();
        let command = pi.command_line.as_ref().unwrap();
        assert!(command.len() < 16_500);
        assert!(command.ends_with("[truncated at 16384 bytes]"));
    }

    #[test]
    fn process_identity_guard_rejects_reused_pids_and_replaced_running_executables() {
        let pid = std::process::id() as i32;
        let process = read_process(Path::new("/proc"), pid).unwrap();
        let identity = ProcessIdentity {
            pid,
            start_ticks: process.started,
        };
        assert!(process_identity_is_current(identity, None));
        assert!(!process_identity_is_current(
            ProcessIdentity {
                start_ticks: process.started + 1,
                ..identity
            },
            None
        ));
        assert!(!process_identity_is_current(identity, Some((0, 0))));
        assert!(!process_identity_is_current(
            ProcessIdentity {
                pid: -1,
                start_ticks: 0
            },
            None
        ));
    }

    #[test]
    #[ignore = "Read-only live process inspection; requires APPLICATIONLAUNCHER_TREE_PID"]
    fn live_process_tree_capture() {
        let pid = std::env::var("APPLICATIONLAUNCHER_TREE_PID")
            .unwrap()
            .parse()
            .unwrap();
        let active = std::env::var("APPLICATIONLAUNCHER_TREE_ACTIVE_PID")
            .ok()
            .map(|pid| pid.parse().unwrap());
        let started = Instant::now();
        let document = capture(&target(pid, active), Path::new("/proc"));
        eprintln!("Capture duration: {:?}\n{document}", started.elapsed());
        assert!(document.contains(&format!("(pid {pid}) [window owner]")));
        assert!(!document.starts_with("Process tree unavailable"));
    }
}
