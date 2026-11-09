use super::*;
use applicationlauncher::tracker::{LiveTmuxPane, TmuxSession};

const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/tmux-info.json");

fn node(pid: i32, start_ticks: u64) -> ProcessInfoNode {
    ProcessInfoNode {
        identity: ProcessIdentity { pid, start_ticks },
        parent_pid: 1,
        depth: 0,
        name: "tmux".into(),
        script_name: None,
        header: format!("tmux (pid {pid})").into(),
        executable: Some("/fixture/tmux".into()),
        executable_error: None,
        executable_identity: None,
        cwd: Some("/own-process-directory".into()),
        cwd_error: None,
        raw_argv0: None,
        command_line: Some("tmux".into()),
    }
}

fn fixture() -> (
    WindowInfoPopup,
    ProcessInfoNode,
    ProcessInfoNode,
    ProcessTreeSnapshot,
) {
    let InfoTarget::Window(mut popup) = super::super::tests::window("tmux-fixture") else {
        unreachable!()
    };
    popup.window.tmux_pane = Some(LiveTmuxPane {
        client_pid: 5,
        session: TmuxSession {
            socket_path: "/fixture/server.sock".into(),
            server_pid: 20,
            session_id: "$1".into(),
            session_name: "work".into(),
            created_at: 1793232000,
        },
        pane_id: "%1".into(),
        pane_pid: 6,
        process_pid: 7,
        process_name: "pi".into(),
        cwd: "/selected-pane-directory".into(),
        title: "Selected client task".into(),
    });
    let client = node(5, 500);
    let server = node(20, 200);
    let snapshot = ProcessTreeSnapshot {
        nodes: vec![client.clone(), server.clone()].into(),
        notice: "fixture".into(),
        text: "fixture".into(),
    };
    let target = Target::for_window(&popup.window, &snapshot).unwrap();
    popup.tmux = Some(Lookup::completed(target, FIXTURE));
    (popup, client, server, snapshot)
}

fn expanded(
    ctx: &egui::Context,
    popup: &WindowInfoPopup,
    node: &ProcessInfoNode,
) -> Vec<InfoMetadataSection> {
    let id = egui::Id::new(("tmux-fixture", node.identity));
    for section in sections(ctx, id, &popup.window, node, popup.tmux.as_ref(), &[]) {
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
            ctx, section.id, false,
        );
        state.set_open(true);
        state.store(ctx);
    }
    sections(ctx, id, &popup.window, node, popup.tmux.as_ref(), &[])
}

fn merged(
    popup: &WindowInfoPopup,
    node: &ProcessInfoNode,
    shown: &[ProcessInfoNode],
) -> Vec<InfoPopupRow> {
    let mut rows = super::super::process_info_rows(&popup.window, node, Vec::new());
    let mut versions = Vec::new();
    merge_process_rows(
        &mut rows,
        &mut versions,
        &popup.window,
        node,
        popup.tmux.as_ref(),
        shown,
    );
    rows.extend(versions);
    rows
}

fn field<'a>(rows: &'a [InfoPopupRow], label: &str) -> &'a str {
    &rows
        .iter()
        .find(|row| row.label == label)
        .unwrap_or_else(|| panic!("missing {label}"))
        .value
}

#[test]
fn only_the_verified_outdated_tmux_server_has_a_server_update_target() {
    let (mut popup, client, mut server, _) = fixture();
    popup.window.class = "xfce4-terminal".into();
    assert!(
        matches!(super::super::process_update_target(&popup.window, &server, Some(false)),
        Some(InfoUpdateTarget::TmuxServer { window, server: identity })
            if window == popup.window.id && identity.pid == 20 && identity.start_ticks == 200
                && identity.socket_path == "/fixture/server.sock")
    );
    for status in [None, Some(true)] {
        assert!(super::super::process_update_target(&popup.window, &server, status).is_none());
    }
    assert!(super::super::process_update_target(&popup.window, &client, Some(false)).is_none());
    server.identity.start_ticks = 0;
    assert!(super::super::process_update_target(&popup.window, &server, Some(false)).is_none());
    server.identity.start_ticks = 200;
    server.executable = Some("/fixture/bash".into());
    assert!(super::super::process_update_target(&popup.window, &server, Some(false)).is_none());
    server.executable = Some("/fixture/tmux (deleted)".into());
    assert!(super::super::process_update_target(&popup.window, &server, Some(false)).is_some());
    popup.window.tmux_pane = None;
    assert!(super::super::process_update_target(&popup.window, &server, Some(false)).is_none());
}

#[test]
fn tmux_server_information_does_not_borrow_the_selected_clients_pane_or_session() {
    let (popup, client, server, _) = fixture();
    let client_rows = process_rows(&popup.window, &client);
    let server_rows = process_rows(&popup.window, &server);
    assert_eq!(field(&client_rows, "Tmux role"), "Client");
    assert_eq!(field(&client_rows, "Tmux session"), "work");
    assert_eq!(field(&client_rows, "Tmux active pane"), "%1");
    assert_eq!(
        field(&client_rows, "Tmux pane directory"),
        "/selected-pane-directory"
    );
    assert_eq!(
        server_rows
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        ["Tmux role", "Tmux listener"]
    );
    assert_eq!(field(&server_rows, "Tmux role"), "Server");
    let ctx = egui::Context::default();
    let groups = expanded(&ctx, &popup, &server);
    assert!(
        groups
            .iter()
            .all(|group| group.header.as_ref() != "Tmux server")
    );
    let rows = merged(&popup, &server, &[]);
    assert_eq!(field(&rows, "Session count"), "2");
    assert_eq!(field(&rows, "Client count"), "2");
    assert_eq!(field(&rows, "SHA256"), "running-server-sha");
    assert_eq!(
        field(&rows, "Latest SHA256 (on disk)"),
        "installed-tmux-sha"
    );
    assert!(!rows.iter().any(|row| {
        [
            "TTY",
            "Session",
            "Pane PID",
            "Width (columns)",
            "Tmux client socket",
        ]
        .contains(&row.label.as_str())
    }));
    let quiet = groups
        .iter()
        .find(|section| section.header.contains("quiet"))
        .unwrap();
    assert_eq!(field(&quiet.rows, "Attached clients"), "0");
    assert_eq!(field(&quiet.rows, "Foreground executable"), "fish");
    assert!(
        !quiet
            .rows
            .iter()
            .any(|row| row.value == "Selected client task")
    );
}

#[test]
fn tmux_server_inventory_merges_into_one_version_and_fingerprint_sequence() {
    let (mut popup, _, server, _) = fixture();
    let target = popup.tmux.as_ref().unwrap().target.clone();
    let mut data: Value = serde_json::from_slice(FIXTURE).unwrap();
    data["servers"]["tmux"]["future_metadata"] = serde_json::json!({"count": 0, "proof": null});
    data["servers"]["tmux_latest"]["build_flavor"] = Value::from("fork");
    popup.tmux = Some(Lookup::completed(
        target,
        &serde_json::to_vec(&data).unwrap(),
    ));
    let mut rows = super::super::process_info_rows(&popup.window, &server, Vec::new());
    let mut versions = vec![
        row(VERSION_FIELD_LABEL, "tmux 3.7c-old (running-elf)".into()),
        row(
            LATEST_VERSION_FIELD_LABEL,
            "tmux 3.7c-new (installed-elf)".into(),
        ),
    ];
    versions[0].binary_is_current = Some(false);
    for field in &mut versions[1..] {
        field.scope = InfoFieldScope::Installation;
    }
    merge_process_rows(
        &mut rows,
        &mut versions,
        &popup.window,
        &server,
        popup.tmux.as_ref(),
        &[],
    );
    let version_labels = versions
        .iter()
        .map(|row| row.label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        version_labels,
        [
            VERSION_FIELD_LABEL,
            "SHA256",
            LATEST_VERSION_FIELD_LABEL,
            "Latest SHA256 (on disk)",
            "Latest installed tmux / Build flavor"
        ]
    );
    assert_eq!(versions[0].binary_is_current, Some(false));
    assert_eq!(versions[1].scope, InfoFieldScope::RunningInstance);
    assert_eq!(versions[3].scope, InfoFieldScope::Installation);
    rows.extend(versions);
    assert_eq!(
        field(&rows, VERSION_FIELD_LABEL),
        "tmux 3.7c-old (running-elf)"
    );
    assert_eq!(
        field(&rows, LATEST_VERSION_FIELD_LABEL),
        "tmux 3.7c-new (installed-elf)"
    );
    assert_eq!(field(&rows, "Host"), "fixture-host");
    assert_eq!(
        field(&rows, "Snapshot captured"),
        "29-10-2026 00:00:00.000 UTC"
    );
    assert_eq!(field(&rows, "Sizing monitor running"), "true");
    assert_eq!(
        field(&rows, "Running server / Future metadata / Count"),
        "0"
    );
    assert_eq!(
        field(&rows, "Running server / Future metadata / Proof"),
        "Unavailable"
    );
    assert_eq!(field(&rows, "Latest installed tmux / Build flavor"), "fork");
    let labels: HashSet<_> = rows.iter().map(|row| row.label.as_str()).collect();
    assert_eq!(labels.len(), rows.len());
    for redundant in [
        "Running server / Version",
        "Latest installed tmux / Version",
        "Running server / Outdated",
        "Running server / PID",
        "Running server / Socket",
        "Running server / SHA256",
        "Latest installed tmux / SHA256",
    ] {
        assert!(!labels.contains(redundant), "duplicate {redundant}");
    }
}

#[test]
fn tmux_server_merge_keeps_failures_flat_and_rejects_stale_inventory() {
    for variant in 0..4 {
        let (mut popup, _, mut server, _) = fixture();
        match variant {
            0 => popup.tmux = None,
            1 => {
                let target = popup.tmux.as_ref().unwrap().target.clone();
                popup.tmux = Some(Lookup::completed(target, b"invalid JSON"));
            }
            2 => server.identity.start_ticks += 1,
            _ => {
                popup.window.tmux_pane.as_mut().unwrap().session.socket_path =
                    "/fixture/other.sock".into()
            }
        }
        let rows = merged(&popup, &server, &[]);
        assert!(field(&rows, "Tmux inventory").starts_with("Unavailable:"));
        assert!(!rows.iter().any(|row| matches!(
            row.label.as_str(),
            "SHA256" | "Latest SHA256 (on disk)" | "Session count" | "Host"
        )));
        assert!(expanded(&egui::Context::default(), &popup, &server).is_empty());
    }
}

#[test]
fn tmux_client_information_contains_only_its_own_verified_connection() {
    let (popup, client, _, _) = fixture();
    let ctx = egui::Context::default();
    let groups = expanded(&ctx, &popup, &client);
    assert!(
        groups.is_empty(),
        "connection details belong in the main client fields, not a nested section"
    );
    let rows = merged(&popup, &client, &[node(3, 300)]);
    for (label, expected) in [
        ("Transport", "LOCAL"),
        ("Tmux session", "work"),
        ("TTY", "/dev/pts/1"),
        ("Width (columns)", "126"),
        ("Height (rows)", "41"),
        ("Tmux client socket", "socket:[2101]"),
        ("Tmux server connected socket", "socket:[3101]"),
        ("Network local socket", "Unavailable"),
        ("Network peer socket", "Unavailable"),
        ("Sizing controlling client PID", "5"),
        ("Controls window sizing", "true"),
        ("Input idle (seconds)", "12"),
        ("Network packet age (seconds)", "Unavailable"),
    ] {
        assert_eq!(field(&rows, label), expected, "{label}");
    }
    assert!(
        !rows
            .iter()
            .any(|row| row.value.contains("192.0.2.1") || row.value.contains("mosh-client"))
    );
    let labels: HashSet<_> = rows.iter().map(|row| row.label.as_str()).collect();
    assert_eq!(labels.len(), rows.len(), "merged fields must appear once");
    for redundant in [
        "Session",
        "Session ID",
        "Pane PID",
        "Foreground PID",
        "Socket",
        "Socket inode",
        "Peer inode",
        "Local address",
        "Local port",
        "Peer",
        "Peer port",
        "Transport state",
        "Frontend PID",
        "Frontend start (ticks since boot)",
        "Frontend running version",
        "Frontend running SHA256",
        "Frontend outdated",
        "Frontend binary modified",
        "Frontend latest installed build / Version",
    ] {
        assert!(!labels.contains(redundant), "redundant {redundant}");
    }
    assert_eq!(field(&rows, "PID"), "5");
    assert_eq!(field(&rows, "Process start (ticks since boot)"), "500");
    assert_eq!(field(&rows, "Tmux pane PID"), "6");
    assert_eq!(field(&rows, "Tmux foreground PID"), "7");
    assert_eq!(field(&rows, "Tmux foreground program"), "pi");
    assert_eq!(field(&rows, "Tmux foreground executable"), "node");
    let mut reused = client.clone();
    reused.identity.start_ticks += 1;
    let rows = merged(&popup, &reused, &[node(3, 300)]);
    assert!(field(&rows, "Tmux inventory").contains("PID and start time"));
    assert!(!rows.iter().any(|row| row.label == "TTY"));
}

#[test]
fn tmux_server_connected_client_groups_preserve_remote_sockets_builds_sizing_and_packet_age() {
    let (popup, _, server, _) = fixture();
    let ctx = egui::Context::default();
    let groups = expanded(&ctx, &popup, &server);
    let rows = &groups
        .iter()
        .find(|group| group.header.starts_with("Attached tmux client: 8"))
        .unwrap()
        .rows;
    for (label, expected) in [
        ("PID", "8"),
        ("Process start (ticks since boot)", "800"),
        ("Transport", "MOSH"),
        ("Attachment state", "ATTACHED"),
        ("Network reachability", "UNREACHABLE"),
        ("Network packet age (seconds)", "45"),
        ("Tmux client socket", "socket:[2102]"),
        ("Tmux server connected socket", "socket:[3102]"),
        ("Network local socket", "udp:0.0.0.0:60001"),
        ("Network peer socket", "192.0.2.1:54321"),
        ("Sizing participation", "ignored-auto"),
        ("Controls window sizing", "false"),
        ("Mosh client version", "mosh-client old"),
        ("Mosh client build / SHA256", "running-mosh-client-sha"),
        (
            "Mosh client latest installed build / Version",
            "mosh-client new",
        ),
    ] {
        assert_eq!(field(rows, label), expected, "{label}");
    }
    assert!(
        !rows
            .iter()
            .any(|row| row.value == "unix:/fixture/server.sock")
    );
}

#[test]
fn tmux_inventory_collection_is_lazy_and_groups_keep_independent_ids() {
    let (popup, client, server, snapshot) = fixture();
    let ctx = egui::Context::default();
    let viewport = egui::ViewportId::from_hash_of("lazy-tmux-info");
    let target = Target::for_window(&popup.window, &snapshot).unwrap();
    assert!(!requested(
        &ctx,
        viewport,
        &popup.window,
        &snapshot,
        &target
    ));
    let id = process_info_section_id((viewport, &popup.window.id), client.identity);
    let mut state =
        egui::collapsing_header::CollapsingState::load_with_default_open(&ctx, id, false);
    state.set_open(true);
    state.store(&ctx);
    assert!(requested(&ctx, viewport, &popup.window, &snapshot, &target));
    let groups = sections(
        &ctx,
        egui::Id::new("server-groups"),
        &popup.window,
        &server,
        popup.tmux.as_ref(),
        &[],
    );
    assert_eq!(groups.len(), 4);
    assert!(
        groups
            .iter()
            .all(|group| group.rows.is_empty() && !group.default_open)
    );
    let ids: HashSet<_> = groups.iter().map(|group| group.id).collect();
    assert_eq!(ids.len(), groups.len());
    let other = sections(
        &ctx,
        egui::Id::new("another-window"),
        &popup.window,
        &server,
        popup.tmux.as_ref(),
        &[],
    );
    assert!(other.iter().all(|group| !ids.contains(&group.id)));
}

#[test]
fn tmux_metadata_preserves_extra_guest_fields_unknown_values_and_day_month_year_dates() {
    let mut rows = Vec::new();
    fields(
        &mut rows,
        "",
        &serde_json::json!({
            "created_at": 1793232000, "unknown_status": null, "count": 0,
            "opsec_connection": { "guest_clients": [{ "tmux_server_socket": "vm:socket:[200]" }] },
            "future_metadata": { "proof": "not dropped" }, "command": "pi\nnext"
        }),
        InfoFieldScope::RunningInstance,
        0,
    );
    assert_eq!(field(&rows, "Created"), "29-10-2026 00:00:00.000 UTC");
    assert_eq!(field(&rows, "Unknown status"), "Unavailable");
    assert_eq!(field(&rows, "Count"), "0");
    assert_eq!(field(&rows, "Future metadata / Proof"), "not dropped");
    assert_eq!(
        field(
            &rows,
            "Guest connection / Guest clients [1] / Tmux server connected socket"
        ),
        "vm:socket:[200]"
    );
    assert_eq!(field(&rows, "Foreground command line"), "pi\\nnext");
}

#[test]
fn tmux_inventory_failures_are_visible_and_cannot_leave_loading_or_borrow_another_frontend_build() {
    let (mut popup, client, _, _) = fixture();
    let mut inventory: Value = serde_json::from_slice(FIXTURE).unwrap();
    inventory["servers"]["xfce4-terminal"][0]["started_ticks"] = Value::from(301);
    let rows = client_rows(
        &Inventory { data: inventory },
        &serde_json::from_slice::<Value>(FIXTURE).unwrap()["clients"][0],
        true,
        &[],
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.label == "Frontend running SHA256")
    );
    let target = popup.tmux.as_ref().unwrap().target.clone();
    popup.tmux = Some(Lookup::completed(target, b"invalid fixture JSON"));
    let rows = merged(&popup, &client, &[]);
    assert!(field(&rows, "Tmux inventory").starts_with("Unavailable: Invalid tmux-mosh JSON"));
    popup.tmux = None;
    let rows = merged(&popup, &client, &[]);
    assert!(field(&rows, "Tmux inventory").starts_with("Unavailable:"));
    assert!(!field(&rows, "Tmux inventory").contains("Loading"));
}

#[test]
fn tmux_socket_aliases_fold_into_four_endpoints_without_losing_ipv6_or_unknowns() {
    let inventory = Inventory {
        data: serde_json::json!({}),
    };
    for (client, expected) in [
        (
            serde_json::json!({"transport": "LOCAL", "socket": "socket:[11]", "peer": "socket:[22]", "local_address": null, "local_port": null, "peer_port": null}),
            ["socket:[11]", "socket:[22]", "Unavailable", "Unavailable"],
        ),
        (
            serde_json::json!({"transport": "UNKNOWN", "socket": "socket:[11]", "peer": "socket:[22]", "socket_protocol": "unix"}),
            ["socket:[11]", "socket:[22]", "Unavailable", "Unavailable"],
        ),
        (
            serde_json::json!({"transport": "SSH", "socket_inode": 11, "peer_inode": 22, "socket_protocol": "tcp", "local_address": "2001:db8::1", "local_port": 22, "peer": "2001:db8::2", "peer_port": 5555}),
            [
                "socket:[11]",
                "socket:[22]",
                "tcp:[2001:db8::1]:22",
                "[2001:db8::2]:5555",
            ],
        ),
        (
            serde_json::json!({"transport": "MOSH", "socket": "udp:0.0.0.0:60001", "socket_protocol": "udp", "peer_port": 5555}),
            ["Unavailable", "Unavailable", "udp:0.0.0.0:60001", "?:5555"],
        ),
        (
            serde_json::json!({"transport": "LOCAL", "tmux_client_socket": "socket:[11]", "tmux_server_socket": "socket:[22]", "network_local_socket": "tcp:192.0.2.1:1234", "network_peer_socket": "192.0.2.2:22", "vm_transport": {"protocol": "tcp"}}),
            [
                "socket:[11]",
                "socket:[22]",
                "tcp:192.0.2.1:1234",
                "192.0.2.2:22",
            ],
        ),
    ] {
        let rows = client_rows(&inventory, &client, true, &[]);
        for (label, expected) in [
            "Tmux client socket",
            "Tmux server connected socket",
            "Network local socket",
            "Network peer socket",
        ]
        .into_iter()
        .zip(expected)
        {
            assert_eq!(field(&rows, label), expected, "{client}");
            assert_eq!(rows.iter().filter(|row| row.label == label).count(), 1);
        }
        for alias in [
            "Socket",
            "Socket inode",
            "Peer inode",
            "Socket protocol",
            "Local address",
            "Local port",
            "Peer",
            "Peer port",
        ] {
            assert!(!rows.iter().any(|row| row.label == alias));
        }
    }
}

#[test]
fn tmux_frontend_details_are_only_omitted_for_the_exact_process_already_shown() {
    let inventory = Inventory {
        data: serde_json::from_slice(FIXTURE).unwrap(),
    };
    let client = &inventory.clients()[0];
    for (shown, expected) in [
        (vec![node(3, 300)], false),
        (vec![node(3, 301)], true),
        (vec![node(30, 300)], true),
        (vec![], true),
    ] {
        let rows = client_rows(&inventory, client, true, &shown);
        assert_eq!(rows.iter().any(|row| row.label == "Frontend PID"), expected);
        assert_eq!(
            rows.iter()
                .any(|row| row.label == "Frontend running version"),
            expected
        );
        assert_eq!(
            rows.iter()
                .any(|row| row.label == "Frontend latest installed build / Version"),
            expected
        );
        assert_eq!(
            rows.iter()
                .any(|row| row.label == "Frontend running SHA256"),
            expected
        );
        assert_eq!(field(&rows, "Connection frontend"), "xfce4-terminal");
    }
}

#[test]
fn tmux_client_pruning_keeps_distinct_state_and_equal_but_unrelated_values() {
    let inventory = Inventory {
        data: serde_json::json!({}),
    };
    let rows = client_rows(
        &inventory,
        &serde_json::json!({
            "state": "SUSPENDED", "attachment": "ATTACHED", "reachability": "UNKNOWN",
            "active_sizing": false, "app_outdated": false, "app_build_unknown": false,
            "unknown_one": null, "unknown_two": null, "future_metadata": { "proof": "retained" }
        }),
        true,
        &[],
    );
    assert_eq!(field(&rows, "Transport state"), "SUSPENDED");
    assert_eq!(field(&rows, "Attachment state"), "ATTACHED");
    for label in [
        "Controls window sizing",
        "Foreground application outdated",
        "Foreground build unknown",
    ] {
        assert_eq!(field(&rows, label), "false");
    }
    for label in ["Unknown one", "Unknown two"] {
        assert_eq!(field(&rows, label), "Unavailable");
    }
    assert_eq!(field(&rows, "Future metadata / Proof"), "retained");
}
