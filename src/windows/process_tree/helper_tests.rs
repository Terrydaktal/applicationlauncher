use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) fn node(pid: i32, parent_pid: i32, depth: usize, executable: &str) -> ProcessInfoNode {
    let name = Path::new(executable)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    ProcessInfoNode {
        identity: ProcessIdentity {
            pid,
            start_ticks: pid as u64,
        },
        parent_pid,
        depth,
        header: format!("{name} (pid {pid})").into(),
        name,
        script_name: None,
        executable: Some(executable.into()),
        executable_error: None,
        executable_identity: Some((1, pid as u64)),
        cwd: Some("/captured-directory".into()),
        cwd_error: None,
        raw_argv0: Some(executable.into()),
        command_line: Some("captured command line".into()),
    }
}

fn target() -> WindowProcessTreeTarget {
    WindowProcessTreeTarget {
        id: "helpers".into(),
        owner: Some(10),
        active: None,
        pane: None,
        client: None,
    }
}

pub(super) fn snapshot(nodes: Vec<ProcessInfoNode>) -> Arc<ProcessTreeSnapshot> {
    Arc::new(ProcessTreeSnapshot {
        nodes: nodes.into(),
        notice: "Captured metadata; exited identities removed".into(),
        text: "Captured fixture".into(),
    })
}

fn image_nodes() -> Vec<ProcessInfoNode> {
    vec![
        node(10, 0, 0, "/usr/bin/xfce4-terminal"),
        node(20, 10, 1, "/usr/bin/bwrap"),
        node(21, 20, 2, "/usr/bin/bwrap"),
        node(22, 21, 3, "/usr/lib/glycin-loaders/2+/glycin-image-rs"),
        node(30, 10, 1, "/usr/bin/tmux"),
        node(40, 10, 1, "/usr/bin/bwrap"),
        node(41, 40, 2, "/usr/bin/bwrap"),
        node(42, 41, 3, "/usr/lib/glycin-loaders/2+/glycin-svg (deleted)"),
    ]
}

fn helper_pids(target: &WindowProcessTreeTarget, nodes: &[ProcessInfoNode]) -> BTreeSet<i32> {
    visibility::image_loader_helpers(target, nodes)
        .iter()
        .map(|identity| identity.pid)
        .collect()
}

#[test]
fn image_loader_filter_hides_only_recognised_decoder_branches_and_their_wrappers() {
    let mut nodes = image_nodes();
    nodes.push(node(43, 42, 4, "/usr/bin/decoder-worker"));
    assert_eq!(
        helper_pids(&target(), &nodes),
        BTreeSet::from([20, 21, 22, 40, 41, 42, 43])
    );
}

#[test]
fn image_loader_filter_preserves_unrelated_sandboxes_mixed_branches_and_name_lookalikes() {
    let mut nodes = image_nodes();
    nodes.insert(4, node(23, 20, 2, "/usr/bin/real-application"));
    nodes.extend([
        node(50, 10, 1, "/usr/bin/bwrap"),
        node(51, 50, 2, "/usr/bin/application"),
        node(60, 10, 1, "/usr/bin/glycin-svg"),
        node(70, 10, 1, "/tmp/glycin-loaders-lookalike/glycin-svg"),
    ]);
    let mut unknown = node(80, 10, 1, "/usr/lib/glycin-loaders/2+/glycin-svg");
    unknown.executable = None;
    nodes.push(unknown);
    assert_eq!(
        helper_pids(&target(), &nodes),
        BTreeSet::from([21, 22, 40, 41, 42])
    );
}

#[test]
fn image_loader_filter_never_hides_the_owner_active_process_pane_client_or_their_ancestors() {
    let nodes = image_nodes();
    for role in ["owner", "active", "pane", "client"] {
        let mut target = target();
        match role {
            "owner" => target.owner = Some(22),
            "active" => target.active = Some(22),
            "pane" => target.pane = Some(22),
            "client" => target.client = Some(22),
            _ => unreachable!(),
        }
        let hidden = helper_pids(&target, &nodes);
        assert!(!hidden.contains(&20), "{role}");
        assert!(!hidden.contains(&21), "{role}");
        assert!(!hidden.contains(&22), "{role}");
    }
}

#[test]
fn image_loader_wrapper_classification_survives_its_decoder_exiting() {
    let captured = snapshot(image_nodes());
    let mut lookup = WindowProcessTreeLookup::completed(target(), (*captured).clone());
    let decoder = captured.nodes[3].identity;
    presence::remove_exited(lookup.result.as_mut().unwrap(), &HashSet::from([decoder]));
    assert!(lookup.is_image_loader_helper(captured.nodes[1].identity));
    assert!(lookup.is_image_loader_helper(captured.nodes[2].identity));
    assert_eq!(lookup.result.as_ref().unwrap().nodes.len(), 7);
    assert!(!lookup.owner_has_exited());
    presence::remove_exited(
        lookup.result.as_mut().unwrap(),
        &HashSet::from([captured.nodes[0].identity]),
    );
    assert!(lookup.owner_has_exited());
}

struct ProcFixture(PathBuf);

impl ProcFixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "applicationlauncher-presence-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn stat(&self, pid: i32, start: u64, state: &str) {
        let directory = self.0.join(pid.to_string());
        std::fs::create_dir_all(&directory).unwrap();
        let mut fields = vec!["0".to_owned(); 20];
        fields[0] = state.into();
        fields[1] = "1".into();
        fields[19] = start.to_string();
        std::fs::write(
            directory.join("stat"),
            format!("{pid} (a name with ) brackets) {}\n", fields.join(" ")),
        )
        .unwrap();
    }
}

impl Drop for ProcFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn presence_checks_remove_only_proven_exits_zombies_and_reused_pids_without_reading_metadata() {
    let fixture = ProcFixture::new();
    for (pid, started, state) in [
        (10, 10, "S"),
        (20, 21, "S"),
        (30, 30, "Z"),
        (40, 40, "X"),
        (50, 50, "x"),
    ] {
        fixture.stat(pid, started, state);
    }
    let nodes = [10, 20, 30, 40, 50, 60]
        .into_iter()
        .map(|pid| node(pid, 0, 0, "/unreadable/exe"))
        .collect();
    let captured = snapshot(nodes);
    let exited = presence::exited_processes(&captured, &fixture.0);
    assert_eq!(
        exited
            .iter()
            .map(|identity| identity.pid)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([20, 30, 40, 50, 60])
    );
    assert!(captured.nodes.iter().all(|node| {
        !fixture
            .0
            .join(node.identity.pid.to_string())
            .join("exe")
            .exists()
    }));
}

#[test]
fn presence_checks_keep_unreadable_malformed_and_truncated_processes_as_unknown() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = ProcFixture::new();
    fixture.stat(10, 10, "S");
    let denied = fixture.0.join("10/stat");
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0)).unwrap();
    for pid in [20, 30, 40, 50] {
        fixture.stat(pid, pid as u64, "S");
    }
    std::fs::write(fixture.0.join("20/stat"), "20 (partial) S").unwrap();
    std::fs::write(fixture.0.join("30/stat"), vec![b'x'; MAX_STAT_BYTES + 1]).unwrap();
    std::fs::write(fixture.0.join("40/stat"), b"\xff\x00").unwrap();
    std::fs::remove_file(fixture.0.join("50/stat")).unwrap();
    std::fs::create_dir(fixture.0.join("50/stat")).unwrap();
    let captured = snapshot(
        [10, 20, 30, 40, 50]
            .into_iter()
            .map(|pid| node(pid, 0, 0, "/fixture"))
            .collect(),
    );
    assert!(presence::exited_processes(&captured, &fixture.0).is_empty());
    std::fs::set_permissions(denied, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn exit_removal_promotes_surviving_children_without_changing_identity_or_captured_metadata() {
    let original = snapshot(vec![
        node(10, 0, 0, "/fixture/ancestor"),
        node(20, 10, 1, "/fixture/exiting"),
        node(30, 20, 2, "/fixture/surviving"),
        node(40, 30, 3, "/fixture/grandchild"),
        node(50, 10, 1, "/fixture/sibling"),
    ]);
    let mut pruned = original.clone();
    presence::remove_exited(&mut pruned, &HashSet::new());
    assert!(Arc::ptr_eq(&original, &pruned));
    presence::remove_exited(&mut pruned, &HashSet::from([original.nodes[1].identity]));
    assert_eq!(
        pruned
            .nodes
            .iter()
            .map(|node| (node.identity.pid, node.depth))
            .collect::<Vec<_>>(),
        vec![(10, 0), (30, 1), (40, 2), (50, 1)]
    );
    assert_eq!(pruned.nodes[1].identity, original.nodes[2].identity);
    assert_eq!(pruned.nodes[1].parent_pid, 20);
    assert_eq!(pruned.nodes[1].cwd, original.nodes[2].cwd);
    assert_eq!(pruned.nodes[1].command_line, original.nodes[2].command_line);
    assert!(Arc::ptr_eq(
        &pruned.nodes[1].header,
        &original.nodes[2].header
    ));
    assert!(Arc::ptr_eq(&pruned.notice, &original.notice));
    let unchanged = pruned.clone();
    presence::remove_exited(&mut pruned, &HashSet::from([original.nodes[1].identity]));
    assert!(Arc::ptr_eq(&unchanged, &pruned));
}

#[test]
fn presence_worker_prunes_off_thread_and_keeps_an_unchanged_snapshot_between_checks() {
    let process = read_process(Path::new("/proc"), std::process::id() as i32).unwrap();
    let mut own = node(process.pid, 0, 0, "/captured/not-reopened");
    own.identity.start_ticks = process.started;
    let gone = node(i32::MAX, process.pid, 1, "/fixture/gone");
    let original = snapshot(vec![own.clone(), gone]);
    let mut lookup = WindowProcessTreeLookup::completed(target(), (*original).clone());
    let ctx = egui::Context::default();
    let before = lookup.result.clone().unwrap();
    lookup.poll_presence(&ctx, egui::ViewportId::ROOT);
    assert!(Arc::ptr_eq(&before, lookup.result.as_ref().unwrap()));
    let deadline = Instant::now() + Duration::from_secs(5);
    while lookup.result.as_ref().unwrap().nodes.len() != 1 {
        assert!(Instant::now() < deadline, "presence check did not complete");
        std::thread::sleep(Duration::from_millis(1));
        lookup.poll_presence(&ctx, egui::ViewportId::ROOT);
    }
    let pruned = lookup.result.clone().unwrap();
    assert_eq!(pruned.nodes[0].identity, own.identity);
    assert_eq!(pruned.nodes[0].executable, own.executable);
    for _ in 0..10 {
        lookup.poll_presence(&ctx, egui::ViewportId::ROOT);
    }
    assert!(Arc::ptr_eq(&pruned, lookup.result.as_ref().unwrap()));
}

#[test]
fn presence_checks_are_limited_to_the_captured_process_budget() {
    let fixture = ProcFixture::new();
    let captured = snapshot(
        (1..=MAX_TREE_PROCESSES as i32 + 1)
            .map(|pid| node(pid, 0, 0, "/fixture/missing"))
            .collect(),
    );
    let exited = presence::exited_processes(&captured, &fixture.0);
    assert_eq!(exited.len(), MAX_TREE_PROCESSES);
    assert!(!exited.contains(&captured.nodes[MAX_TREE_PROCESSES].identity));
}

#[test]
fn presence_worker_removes_an_exited_disposable_child() {
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Child(
        std::process::Command::new("/usr/bin/sleep")
            .arg("60")
            .spawn()
            .unwrap(),
    );
    let pid = child.0.id() as i32;
    let process = read_process(Path::new("/proc"), pid).unwrap();
    let mut captured_child = node(pid, 0, 0, "/captured/sleep");
    captured_child.identity.start_ticks = process.started;
    let captured = snapshot(vec![captured_child]);
    let mut lookup = WindowProcessTreeLookup::completed(target(), (*captured).clone());
    let ctx = egui::Context::default();
    lookup.poll_presence(&ctx, egui::ViewportId::ROOT);
    let original = lookup.result.clone().unwrap();
    let initial_wait = Instant::now() + Duration::from_millis(20);
    while Instant::now() < initial_wait {
        lookup.poll_presence(&ctx, egui::ViewportId::ROOT);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(Arc::ptr_eq(&original, lookup.result.as_ref().unwrap()));
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !lookup.result.as_ref().unwrap().nodes.is_empty() {
        assert!(
            Instant::now() < deadline,
            "exited child remained in the displayed snapshot"
        );
        lookup.poll_presence(&ctx, egui::ViewportId::ROOT);
        std::thread::sleep(Duration::from_millis(5));
    }
}
