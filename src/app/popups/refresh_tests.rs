use super::*;
use crate::models::{ProcessIdentity, ProcessTreeSnapshot};
use crate::windows::versions::ProcessVersion;

fn node(pid: i32, name: &str) -> ProcessInfoNode {
    ProcessInfoNode {
        identity: ProcessIdentity {
            pid,
            start_ticks: pid as u64 * 100,
        },
        parent_pid: 10,
        depth: 1,
        name: name.into(),
        script_name: None,
        header: format!("{name} (pid {pid})").into(),
        executable: Some(format!("/fixture/{name}").into()),
        executable_identity: Some((1, pid as u64)),
        executable_error: None,
        cwd: Some("/fixture/work".into()),
        cwd_error: None,
        raw_argv0: None,
        command_line: Some(name.into()),
    }
}

fn capture(window: &WindowInfo, nodes: &[ProcessInfoNode]) -> WindowProcessTreeLookup {
    WindowProcessTreeLookup::completed(
        WindowProcessTreeTarget::for_window(window),
        ProcessTreeSnapshot {
            nodes: Arc::from(nodes),
            notice: "Fixture snapshot".into(),
            text: "Fixture snapshot".into(),
        },
    )
}

fn set_open(ctx: &egui::Context, id: egui::Id, open: bool) {
    let mut state =
        egui::collapsing_header::CollapsingState::load_with_default_open(ctx, id, false);
    state.set_open(open);
    state.store(ctx);
}

fn wait(cache: &ProcessVersionCache, node: &ProcessInfoNode) -> Arc<ProcessVersion> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(version) = cache.request(node) {
            return version;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[test]
fn live_title_frames_wake_only_the_owning_info_viewport_without_refetching_metadata() {
    let ctx = egui::Context::default();
    ctx.set_embed_viewports(false);
    let viewport = egui::ViewportId::from_hash_of("animated-info");
    let other = egui::ViewportId::from_hash_of("unrelated-info");
    let InfoTarget::Window(mut popup) = super::tests::window("animated-window") else {
        unreachable!()
    };
    popup.lookup = Some(capture(&popup.window, &[node(10, "terminal")]));
    let snapshot = popup.lookup.as_ref().unwrap().result.clone().unwrap();
    let settle = || {
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            for id in [viewport, other] {
                ctx.show_viewport_deferred(id, egui::ViewportBuilder::default(), |_, _| {});
            }
        });
        for id in [viewport, other] {
            for _ in 0..3 {
                let mut input = egui::RawInput {
                    viewport_id: id,
                    ..Default::default()
                };
                input.viewports.insert(
                    id,
                    egui::ViewportInfo {
                        parent: Some(egui::ViewportId::ROOT),
                        ..Default::default()
                    },
                );
                let _ = ctx.run(input, |_| {});
            }
        }
        assert!(!ctx.has_requested_repaint_for(&viewport));
        assert!(!ctx.has_requested_repaint_for(&other));
    };
    for (display, raw) in [
        ("codex: \u{280b} project", "codex: \u{280b} project"),
        ("codex: \u{2819} project", "codex: \u{280b} project"),
        ("codex: \u{2819} project", "codex: \u{2839} project"),
        ("codex: project", "codex: project"),
    ] {
        settle();
        let mut current = popup.window.clone();
        current.title = display.into();
        current.raw_title = raw.into();
        popup.sync_window(&ctx, viewport, &current);
        assert!(
            ctx.has_requested_repaint_for(&viewport),
            "a changed title left the Info viewport asleep"
        );
        assert!(!ctx.has_requested_repaint_for(&other));
        assert_eq!(popup.window.title, display);
        assert_eq!(popup.window.raw_title, raw);
        let captured = popup.lookup.as_ref().unwrap().result.as_ref().unwrap();
        assert!(Arc::ptr_eq(&captured.nodes, &snapshot.nodes));
        assert!(popup.versions.is_none());

        settle();
        popup.sync_window(&ctx, viewport, &current);
        assert!(
            !ctx.has_requested_repaint_for(&viewport),
            "unchanged titles must not create a repaint loop"
        );
    }
}

#[test]
fn first_info_load_queues_only_expanded_sections_before_collapsed_ancestors() {
    let ctx = egui::Context::default();
    let viewport = egui::ViewportId::ROOT;
    let InfoTarget::Window(mut popup) = super::tests::window("priority") else {
        unreachable!()
    };
    popup.window.pid = Some(10);
    let nodes = [
        node(1, "systemd"),
        node(2, "systemd"),
        node(10, "terminal"),
        node(20, "fish"),
    ];
    set_open(
        &ctx,
        process_info_section_id((viewport, &popup.window.id), nodes[3].identity),
        true,
    );
    let (started, requests) = std::sync::mpsc::channel();
    let (finish, release) = std::sync::mpsc::channel();
    popup.versions = Some(ProcessVersionCache::with_test_probe(
        ctx.clone(),
        viewport,
        move |identity| {
            started.send(identity.pid).unwrap();
            release.recv_timeout(Duration::from_secs(5)).unwrap();
            ProcessVersion::default()
        },
    ));
    popup.queue_expanded_metadata(&ctx, viewport, &[], &nodes);
    assert_eq!(requests.recv_timeout(Duration::from_secs(5)).unwrap(), 10);
    finish.send(()).unwrap();
    assert_eq!(requests.recv_timeout(Duration::from_secs(5)).unwrap(), 20);
    assert!(requests.try_recv().is_err());
    drop(popup);
    finish.send(()).unwrap();
}

#[test]
fn hidden_image_loader_branches_do_not_queue_metadata_and_revealing_them_preserves_other_cache_entries()
 {
    let ctx = egui::Context::default();
    let viewport = egui::ViewportId::from_hash_of("helper-info");
    let other_viewport = egui::ViewportId::from_hash_of("other-info");
    let InfoTarget::Window(mut popup) = super::tests::window("helpers") else {
        unreachable!()
    };
    popup.window.pid = Some(10);
    let mut nodes = [
        node(10, "terminal"),
        node(20, "bwrap"),
        node(21, "bwrap"),
        node(22, "glycin-image-rs"),
        node(30, "tmux"),
    ];
    nodes[0].depth = 0;
    nodes[0].parent_pid = 0;
    nodes[1].executable = Some("/usr/bin/bwrap".into());
    nodes[2].executable = Some("/usr/bin/bwrap".into());
    nodes[2].depth = 2;
    nodes[2].parent_pid = 20;
    nodes[3].executable = Some("/usr/lib/glycin-loaders/2+/glycin-image-rs".into());
    nodes[3].depth = 3;
    nodes[3].parent_pid = 21;
    popup.lookup = Some(capture(&popup.window, &nodes));
    for node in &nodes {
        set_open(
            &ctx,
            process_info_section_id((viewport, &popup.window.id), node.identity),
            true,
        );
    }
    let (started, requests) = std::sync::mpsc::channel();
    popup.versions = Some(ProcessVersionCache::with_test_probe(
        ctx.clone(),
        viewport,
        move |identity| {
            started.send(identity.pid).unwrap();
            ProcessVersion::default()
        },
    ));
    let visible = popup.visible_process_nodes(&ctx, viewport, &nodes);
    assert_eq!(
        visible
            .iter()
            .map(|node| node.identity.pid)
            .collect::<Vec<_>>(),
        vec![10, 30]
    );
    popup.queue_expanded_metadata(&ctx, viewport, &[], visible);
    assert_eq!(requests.recv_timeout(Duration::from_secs(5)).unwrap(), 10);
    assert_eq!(requests.recv_timeout(Duration::from_secs(5)).unwrap(), 30);
    let cache = popup.versions.as_ref().unwrap();
    let owner_version = wait(cache, &nodes[0]);
    assert!(requests.try_recv().is_err());
    ctx.data_mut(|data| data.insert_temp(viewport.0.with("show-image-loader-helpers"), true));
    let visible = popup.visible_process_nodes(&ctx, viewport, &nodes);
    assert_eq!(visible.len(), nodes.len());
    popup.queue_expanded_metadata(&ctx, viewport, &[], visible);
    for pid in [20, 21, 22] {
        assert_eq!(requests.recv_timeout(Duration::from_secs(5)).unwrap(), pid);
    }
    assert!(requests.try_recv().is_err());
    assert!(Arc::ptr_eq(&owner_version, &wait(cache, &nodes[0])));
    assert_eq!(
        popup
            .visible_process_nodes(&ctx, other_viewport, &nodes)
            .len(),
        2
    );
    assert!(process_info_section_is_open(
        &ctx,
        process_info_section_id((viewport, &popup.window.id), nodes[3].identity),
        false
    ));
}

#[test]
fn completed_program_update_refreshes_the_same_info_window_and_rechecks_green_without_reloading_other_processes()
 {
    for application in [
        InfoProgram::Fish,
        InfoProgram::Codex,
        InfoProgram::TmuxClient,
    ] {
        for expanded in [false, true] {
            let ctx = egui::Context::default();
            let mut popups = InfoPopups::default();
            let first = popups.open(super::tests::window("updated-window")).unwrap();
            let second = popups.open(super::tests::window("other-window")).unwrap();
            popups.cancel_focus_requests();
            let owner = node(10, "xfce4-terminal");
            let old = node(20, application.name());
            let other = node(30, application.name());
            let replacement = node(40, application.name());
            let previous = [owner.clone(), old.clone(), other.clone()];
            let popup = &mut popups.items[0];
            popup.initial_size = Some([1200.0, 640.0]);
            popup.initial_size_ready = true;
            let InfoTarget::Window(state) = &mut popup.target else {
                unreachable!()
            };
            state.window.pid = Some(10);
            state.lookup = Some(capture(&state.window, &previous));
            state.versions = Some(ProcessVersionCache::with_test_probe(
                ctx.clone(),
                first,
                move |identity| {
                    let current = identity != old.identity;
                    ProcessVersion {
                        runtime_name: crate::windows::versions::runtime_name(application.name()),
                        version: Some("fixture version".into()),
                        latest_version: Some("latest fixture version".into()),
                        build_id: Some(if current { "latest-build" } else { "old-build" }.into()),
                        latest_build_id: Some("latest-build".into()),
                        running_latest_binary: Some(current),
                        ..Default::default()
                    }
                },
            ));
            let cache = state.versions.as_ref().unwrap();
            assert_eq!(wait(cache, &old).process_build_is_current(), Some(false));
            let unaffected = wait(cache, &owner);
            set_open(
                &ctx,
                process_info_section_id((first, &state.window.id), old.identity),
                expanded,
            );
            set_open(
                &ctx,
                process_info_section_id((first, &state.window.id), owner.identity),
                true,
            );
            assert!(popups.refresh_program_info(
                &ctx,
                first,
                "updated-window",
                (old.identity, application),
                true
            ));
            assert_eq!(popups.items.len(), 2);
            let popup = &mut popups.items[0];
            assert_eq!(popup.id, first);
            assert_eq!(popup.initial_size, Some([1200.0, 640.0]));
            assert!(popup.initial_size_ready);
            assert!(popup.focus_requested.load(Ordering::Relaxed));
            let InfoTarget::Window(state) = &mut popup.target else {
                unreachable!()
            };
            assert!(
                state
                    .lookup
                    .as_ref()
                    .unwrap()
                    .result
                    .as_ref()
                    .unwrap()
                    .nodes
                    .iter()
                    .any(|node| node.identity == old.identity)
            );
            assert!(Arc::ptr_eq(
                &unaffected,
                &state.versions.as_ref().unwrap().request(&owner).unwrap()
            ));
            state.refresh_lookup = Some(capture(
                &state.window,
                &[owner.clone(), replacement.clone(), other.clone()],
            ));
            state.poll_updated_process(&ctx, first);
            let new_id = process_info_section_id((first, &state.window.id), replacement.identity);
            assert_eq!(process_info_section_is_open(&ctx, new_id, false), expanded);
            assert!(process_info_section_is_open(
                &ctx,
                process_info_section_id((first, &state.window.id), owner.identity),
                false
            ));
            assert!(!process_info_section_is_open(
                &ctx,
                process_info_section_id((first, &state.window.id), other.identity),
                false
            ));
            assert!(state.updated_process.is_none());
            let refreshed = state
                .lookup
                .as_ref()
                .unwrap()
                .result
                .as_ref()
                .unwrap()
                .clone();
            for _ in 0..10 {
                state.poll_updated_process(&ctx, first);
            }
            assert!(state.refresh_lookup.is_none());
            assert!(Arc::ptr_eq(
                &refreshed,
                state.lookup.as_ref().unwrap().result.as_ref().unwrap()
            ));
            let current = wait(state.versions.as_ref().unwrap(), &replacement);
            assert_eq!(current.process_build_is_current(), Some(true));
            let rows = process_version_rows("Application", Some(&current), false);
            assert_eq!(
                rows.iter()
                    .find(|row| row.label == VERSION_FIELD_LABEL)
                    .unwrap()
                    .binary_is_current,
                Some(true)
            );
            assert_eq!(
                rows.iter()
                    .find(|row| row.label == LATEST_VERSION_FIELD_LABEL)
                    .unwrap()
                    .binary_is_current,
                None
            );
            assert!(Arc::ptr_eq(
                &unaffected,
                &state.versions.as_ref().unwrap().request(&owner).unwrap()
            ));
            assert_eq!(popups.items[1].id, second);
            assert!(!popups.items[1].focus_requested.load(Ordering::Relaxed));
            let InfoTarget::Window(other) = &popups.items[1].target else {
                unreachable!()
            };
            assert!(other.lookup.is_none() && other.updated_process.is_none());
        }
    }
}

#[test]
fn post_update_refresh_keeps_the_old_snapshot_until_the_current_target_finishes() {
    let ctx = egui::Context::default();
    let mut popups = InfoPopups::default();
    let info = popups.open(super::tests::window("same-window")).unwrap();
    let old = node(20, "fish");
    let replacement = node(40, "fish");
    let InfoTarget::Window(state) = &mut popups.items[0].target else {
        unreachable!()
    };
    state.lookup = Some(capture(&state.window, std::slice::from_ref(&old)));
    let previous = state
        .lookup
        .as_ref()
        .unwrap()
        .result
        .as_ref()
        .unwrap()
        .clone();
    let outdated_result = capture(&state.window, std::slice::from_ref(&replacement));
    assert!(popups.refresh_program_info(
        &ctx,
        info,
        "same-window",
        (old.identity, InfoProgram::Fish),
        true
    ));
    let InfoTarget::Window(state) = &mut popups.items[0].target else {
        unreachable!()
    };
    state.refresh_lookup = Some(outdated_result);
    state.window.active_process = Some("fish".into());
    state.window.process_chain = vec![crate::models::ProcessChainEntry {
        pid: replacement.identity.pid,
        name: "fish".into(),
        exe_path: replacement.executable.clone(),
    }];
    state.poll_updated_process(&ctx, info);
    assert!(state.updated_process.is_some() && state.refresh_lookup.is_none());
    assert!(Arc::ptr_eq(
        &previous,
        state.lookup.as_ref().unwrap().result.as_ref().unwrap()
    ));
    state.refresh_lookup = Some(capture(&state.window, std::slice::from_ref(&replacement)));
    state.poll_updated_process(&ctx, info);
    assert!(state.updated_process.is_none());
    assert_eq!(
        state
            .lookup
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap()
            .nodes[0]
            .identity,
        replacement.identity
    );
}

#[test]
fn completed_program_refresh_does_not_reopen_closed_or_different_info_windows_or_steal_focus_after_dismissal()
 {
    let ctx = egui::Context::default();
    let mut popups = InfoPopups::default();
    let info = popups.open(super::tests::window("same-window")).unwrap();
    popups.cancel_focus_requests();
    let program = (node(20, "fish").identity, InfoProgram::Fish);
    assert!(!popups.refresh_program_info(&ctx, info, "different-window", program, true));
    assert!(popups.refresh_program_info(&ctx, info, "same-window", program, false));
    assert!(!popups.items[0].focus_requested.load(Ordering::Relaxed));
    assert!(popups.close(info));
    assert!(!popups.refresh_program_info(&ctx, info, "same-window", program, true));
    assert!(popups.items.is_empty());
    let replacement = popups.open(super::tests::window("same-window")).unwrap();
    assert_ne!(replacement, info);
    assert!(!popups.refresh_program_info(&ctx, info, "same-window", program, true));
    assert_eq!(popups.items.len(), 1);
}

#[test]
fn updated_process_dropdown_is_not_transferred_to_an_ambiguous_replacement_or_a_different_directory()
 {
    for ambiguous in [false, true] {
        let ctx = egui::Context::default();
        let mut popups = InfoPopups::default();
        let info = popups.open(super::tests::window("same-window")).unwrap();
        let old = node(20, "fish");
        let InfoTarget::Window(state) = &mut popups.items[0].target else {
            unreachable!()
        };
        state.lookup = Some(capture(&state.window, std::slice::from_ref(&old)));
        set_open(
            &ctx,
            process_info_section_id((info, &state.window.id), old.identity),
            true,
        );
        assert!(popups.refresh_program_info(
            &ctx,
            info,
            "same-window",
            (old.identity, InfoProgram::Fish),
            true
        ));
        let InfoTarget::Window(state) = &mut popups.items[0].target else {
            unreachable!()
        };
        let mut one = node(40, "fish");
        let two = node(41, "fish");
        let nodes = if ambiguous {
            vec![one.clone(), two.clone()]
        } else {
            one.cwd = Some("/different-directory".into());
            vec![one.clone()]
        };
        state.refresh_lookup = Some(capture(&state.window, &nodes));
        state.poll_updated_process(&ctx, info);
        for node in [one, two] {
            assert!(!process_info_section_is_open(
                &ctx,
                process_info_section_id((info, &state.window.id), node.identity),
                false
            ));
        }
    }
}

#[test]
fn shell_runtime_classification_keeps_interactive_shell_freshness_and_avoids_an_unknown_application_child()
 {
    for name in ["sh", "bash", "dash", "zsh", "fish", "ksh", "mksh"] {
        assert_eq!(
            program_metadata::interpreted::script::runtime_name(name),
            Some("Shell")
        );
        let node = node(20, name);
        assert!(process_runtime_name(&node, None).is_none());
        for current in [Some(false), Some(true), None] {
            let version = ProcessVersion {
                runtime_name: crate::windows::versions::runtime_name(name),
                version: Some("shell fixture".into()),
                build_id: Some("loaded-build".into()),
                latest_build_id: Some("installed-build".into()),
                running_latest_binary: current,
                ..Default::default()
            };
            assert_eq!(version.process_build_is_current(), current);
            let rows = process_version_rows("Application", Some(&version), false);
            assert_eq!(
                rows.iter()
                    .find(|row| row.label == VERSION_FIELD_LABEL)
                    .unwrap()
                    .binary_is_current,
                current
            );
            assert!(
                hosted_metadata_sections(
                    &egui::Context::default(),
                    egui::Id::new(name),
                    &node,
                    Some(&version)
                )
                .is_empty()
            );
        }
    }
}

#[test]
fn expanded_shell_header_keeps_native_status_without_colouring_the_hosted_script() {
    for name in ["sh", "bash", "dash", "zsh", "fish", "ksh", "mksh"] {
        let node = node(20, name);
        for current in [Some(false), Some(true), None] {
            let mut version = ProcessVersion {
                runtime_name: Some("Shell"),
                running_latest_binary: current,
                script_application: Some(crate::windows::versions::ScriptApplication {
                    name: "retained-tool".into(),
                    target: "/proc/self/fd/3".into(),
                    kind: "Shell script",
                    entry_point: None,
                    descriptor: None,
                    identification: "owned test invocation",
                }),
                ..Default::default()
            };
            assert_eq!(version.process_header_is_current(), current);
            let sections = hosted_metadata_sections(
                &egui::Context::default(),
                egui::Id::new((name, current)),
                &node,
                Some(&version),
            );
            assert_eq!(sections.len(), 1);
            assert_eq!(sections[0].binary_is_current, None);
            // Independent failures to resolve script metadata cannot remove
            // valid interpreter evidence, but a stale process must remove it.
            version.application_metadata_error = Some("script metadata unavailable".into());
            assert_eq!(version.process_header_is_current(), current);
            version.running_unavailable = Some("process changed");
            assert_eq!(version.process_header_is_current(), None);
        }
    }
}

#[test]
fn shell_header_colour_does_not_enable_interactive_restart_actions_for_scripts() {
    let InfoTarget::Window(mut popup) = super::tests::window("shell-script-window") else {
        unreachable!()
    };
    popup.window.pid = Some(176434);
    popup.window.class = "Xfce4-terminal".into();
    for name in ["bash", "fish"] {
        let mut node = node(20, name);
        node.script_name = Some("foreground-script".into());
        let mut version = ProcessVersion {
            runtime_name: Some("Shell"),
            running_latest_binary: Some(false),
            ..Default::default()
        };
        assert_eq!(version.process_header_is_current(), Some(false));
        assert_eq!(
            process_update_target(&popup.window, &node, Some(false)).is_some(),
            name == "fish",
            "negative control must exercise existing Fish restart eligibility"
        );
        assert!(process_version_update_target(&popup.window, &node, Some(&version)).is_none());
        // Preserve the pre-existing explicit update action for native,
        // interactive Fish; the new header colour grants no new action.
        version.runtime_name = None;
        node.script_name = None;
        assert_eq!(
            process_version_update_target(&popup.window, &node, Some(&version)).is_some(),
            name == "fish"
        );
    }
}
