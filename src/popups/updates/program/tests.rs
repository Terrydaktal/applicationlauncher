use super::*;

fn state(application: InfoProgram, tmux: bool) -> State {
    let mut scope = serde_json::json!({
        "identity": {"pid": 123, "start_ticks": 456, "application": application.name()},
        "executable": format!("/fixture/bin/{}", application.name()), "cwd": "/fixture/work",
        "tmux_pane": if tmux { Some("%7") } else { None },
        "tmux_socket": if tmux { Some("/fixture/tmux.sock") } else { None },
    });
    if application == InfoProgram::TmuxClient {
        scope["tmux_pane"] = serde_json::Value::Null;
        scope["tmux_socket"] = serde_json::Value::Null;
        scope["tmux_client"] = serde_json::json!({
            "server": {"pid": 234, "start_ticks": 567, "socket_path": "/fixture/tmux.sock"},
            "session_id": "$1", "session_name": "work", "session_created": 12345, "read_only": true
        });
    }
    State {
        window: "exact-window".into(), process: ProcessIdentity { pid: 123, start_ticks: 456 },
        application, open: true, busy: false, completion_reported: false, error: None,
        report: Some(serde_json::from_value(serde_json::json!({
            "operation_id": "fixture-no-IPC", "window_id": "exact-window", "title": "fixture",
            "phase": "ready", "selected_action": null, "backup_path": null, "message": "Preview only",
            "actions": [{"id": "program", "label": "Update only this program", "eligible": true,
                "message": "Only this program restarts", "program": scope}]
        })).unwrap()),
    }
}

fn frame(
    ctx: &egui::Context,
    state: &mut State,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, Option<Action>) {
    frame_at(ctx, state, events, egui::vec2(1100.0, 850.0))
}

fn frame_at(
    ctx: &egui::Context,
    state: &mut State,
    events: Vec<egui::Event>,
    size: egui::Vec2,
) -> (egui::FullOutput, Option<Action>) {
    let mut action = None;
    let output = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |_| {});
            action = dialog(ctx, egui::ViewportId::from_hash_of("program-tests"), state);
        },
    );
    (output, action)
}

fn settled(ctx: &egui::Context, state: &mut State) -> egui::FullOutput {
    for _ in 0..2 {
        assert!(frame(ctx, state, vec![]).1.is_none());
    }
    frame(ctx, state, vec![]).0
}

fn position(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("Missing control: {label}"))
}

fn click(pos: egui::Pos2) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

fn key(key: egui::Key) -> Vec<egui::Event> {
    vec![
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        },
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

#[test]
fn program_dialog_controls_remain_visible_and_usable_in_a_small_info_window() {
    for application in [
        InfoProgram::Fish,
        InfoProgram::Codex,
        InfoProgram::TmuxClient,
    ] {
        let ctx = egui::Context::default();
        let mut state = state(application, true);
        let size = egui::vec2(520.0, 360.0);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        for _ in 0..3 {
            frame_at(&ctx, &mut state, vec![], size);
        }
        let output = frame_at(&ctx, &mut state, vec![], size).0;
        let update_label = format!("Update {} only", application.label());
        for label in ["Close", &update_label] {
            assert!(
                screen.contains(position(&output, label)),
                "{label} must stay onscreen"
            );
        }
        let update = position(&output, &update_label);
        assert_eq!(
            frame_at(&ctx, &mut state, click(update), size).1,
            Some(Action::Start("fixture-no-IPC".into()))
        );
    }
}

#[test]
fn program_confirmation_restarts_only_the_verified_fish_or_codex_after_consent() {
    for application in [
        InfoProgram::Fish,
        InfoProgram::Codex,
        InfoProgram::TmuxClient,
    ] {
        for tmux in [false, true] {
            let ctx = egui::Context::default();
            let mut state = state(application, tmux);
            assert!(verified_action(&state).is_ok());
            assert!(can_start(&state, "fixture-no-IPC"));
            assert!(!can_start(&state, "another-operation"));
            let output = settled(&ctx, &mut state);
            assert_eq!(
                frame(
                    &ctx,
                    &mut state,
                    click(position(
                        &output,
                        &format!("Update {} only", application.label())
                    ))
                )
                .1,
                Some(Action::Start("fixture-no-IPC".into()))
            );
        }
    }
}

#[test]
fn program_update_button_confirms_without_a_checkbox_across_separate_frames() {
    for application in [
        InfoProgram::Fish,
        InfoProgram::Codex,
        InfoProgram::TmuxClient,
    ] {
        for scale in [1.0, 2.0] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(scale);
            let mut state = state(application, true);
            let output = settled(&ctx, &mut state);
            assert!(output.shapes.iter().all(|shape| !matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.job.text.starts_with("I accept "))));
            let pos = position(&output, &format!("Update {} only", application.label()));
            let events = click(pos);
            assert!(frame(&ctx, &mut state, events[..2].to_vec()).1.is_none());
            for _ in 0..3 {
                assert!(frame(&ctx, &mut state, Vec::new()).1.is_none());
            }
            assert_eq!(
                frame(&ctx, &mut state, events[2..].to_vec()).1,
                Some(Action::Start("fixture-no-IPC".into())),
                "{application:?}, scale={scale}"
            );
        }
    }
}

#[test]
fn program_update_button_requires_a_verified_ready_preview() {
    for application in [
        InfoProgram::Fish,
        InfoProgram::Codex,
        InfoProgram::TmuxClient,
    ] {
        for mutation in 0..12 {
            let ctx = egui::Context::default();
            let mut state = state(application, true);
            match mutation {
                0 => state.report = None,
                1 => state.report.as_mut().unwrap().phase = TerminalUpgradePhase::Preparing,
                2 => state.report.as_mut().unwrap().phase = TerminalUpgradePhase::Running,
                3 => state.report.as_mut().unwrap().phase = TerminalUpgradePhase::Failed,
                4 => state.report.as_mut().unwrap().phase = TerminalUpgradePhase::Complete,
                5 => state.busy = true,
                6 => state.error = Some("Preview failed".into()),
                7 => state.report.as_mut().unwrap().window_id = "another-window".into(),
                8 => state.process.start_ticks += 1,
                9 => state.report.as_mut().unwrap().actions[0].eligible = false,
                10 => {
                    state.report.as_mut().unwrap().actions[0]
                        .program
                        .as_mut()
                        .unwrap()
                        .executable = "relative-path".into();
                }
                _ => state.open = false,
            }
            assert!(!can_start(&state, "fixture-no-IPC"));
            let output = settled(&ctx, &mut state);
            let update = position(&output, &format!("Update {} only", application.label()));
            assert!(
                frame(&ctx, &mut state, click(update)).1.is_none(),
                "{application:?}, mutation={mutation}"
            );
        }
    }
}

#[test]
fn program_confirmation_is_clickable_over_the_full_info_tree() {
    // App::new starts live workers; guard the standalone host's modal ownership
    // separately while exercising its real panel and dialog below without IPC.
    let host = include_str!("../../../app/popups.rs")
        .split("pub(super) fn show_info_only(")
        .nth(1)
        .unwrap()
        .split("pub(super) fn show_info_popups(")
        .next()
        .unwrap();
    assert_eq!(host.matches("self.show_info_popups(ctx)").count(), 1);
    assert!(!host.contains("show_window_update_dialog"));
    use crate::models::{
        InfoFieldScope, InfoPopupData, InfoPopupRow, InfoProcessSection, InfoProcessTree,
    };
    for scale in [1.0, 2.0] {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(scale);
        let popup = egui::ViewportId::ROOT;
        let shared = Arc::new(Mutex::new(state(InfoProgram::Codex, true)));
        ctx.data_mut(|data| data.insert_temp(state_id(popup), shared.clone()));
        let data = InfoPopupData {
            title: "Window Info".into(),
            heading: String::new(),
            subtitle: "Expand a process".into(),
            scroll_sensitivity: 2.5,
            rows: Vec::new(),
            process_tree: Some(InfoProcessTree {
                image_loader_helper_count: 0,
                notice: "Captured process tree".into(),
                sections: vec![InfoProcessSection {
                    id: egui::Id::new("owner"),
                    depth: 1,
                    header: "codex (pid 123) [active process]".into(),
                    binary_is_current: Some(false),
                    script_header: None,
                    update_target: None,
                    default_open: true,
                    rows: (0..35)
                        .map(|index| InfoPopupRow {
                            label: format!("Field {index}"),
                            scope: InfoFieldScope::RunningInstance,
                            value: "Long process metadata value".repeat(5),
                            searched: false,
                            separator_before: false,
                            binary_is_current: None,
                        })
                        .collect(),
                    metadata_sections: Vec::new(),
                }],
            }),
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let frame = |events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1179.0, 695.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        crate::popups::render_info_popup_contents(ui, &data, popup, &sender);
                    });
                },
            )
        };
        for _ in 0..3 {
            frame(Vec::new());
        }
        let output = frame(Vec::new());
        position(&output, "Update codex only");
        assert!(can_start(&shared.lock().unwrap(), "fixture-no-IPC"));
        let pos = position(&output, "Close");
        let events = click(pos);
        frame(events[..2].to_vec());
        for _ in 0..3 {
            frame(Vec::new());
        }
        frame(events[2..].to_vec());
        assert!(!shared.lock().unwrap().open, "scale={scale}");
        assert!(!can_start(&shared.lock().unwrap(), "fixture-no-IPC"));
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn program_cancel_escape_backdrop_and_default_enter_never_start_an_update() {
    for application in [
        InfoProgram::Fish,
        InfoProgram::Codex,
        InfoProgram::TmuxClient,
    ] {
        for event in [0, 1, 2, 3] {
            let ctx = egui::Context::default();
            let mut state = state(application, true);
            let output = settled(&ctx, &mut state);
            let events = match event {
                0 => click(position(&output, "Close")),
                1 => key(egui::Key::Escape),
                2 => click(egui::pos2(1.0, 1.0)),
                _ => key(egui::Key::Enter),
            };
            let action = frame(&ctx, &mut state, events).1;
            assert!(action.is_none() || action == Some(Action::Close));
            assert!(!state.busy);
            assert_eq!(state.report.unwrap().phase, TerminalUpgradePhase::Ready);
        }
    }
}

#[test]
fn program_scope_rejects_changed_pid_start_kind_window_missing_and_ambiguous_metadata() {
    for mutation in 0..10 {
        let mut state = state(InfoProgram::Fish, true);
        let report = state.report.as_mut().unwrap();
        match mutation {
            0 => report.window_id = "another-window".into(),
            1 => report.actions[0].program.as_mut().unwrap().identity.pid += 1,
            2 => {
                report.actions[0]
                    .program
                    .as_mut()
                    .unwrap()
                    .identity
                    .start_ticks += 1
            }
            3 => {
                report.actions[0]
                    .program
                    .as_mut()
                    .unwrap()
                    .identity
                    .application = "codex".into()
            }
            4 => report.actions[0].program = None,
            5 => report.actions.push(report.actions[0].clone()),
            6 => report.actions[0].program.as_mut().unwrap().executable = "fish".into(),
            7 => report.actions[0].program.as_mut().unwrap().tmux_socket = None,
            8 => {
                report.actions[0].program.as_mut().unwrap().tmux_pane =
                    Some("%7; kill-server".into())
            }
            _ => {
                report.actions[0].eligible = false;
                report.actions[0].message = "Fish has a foreground application".into();
            }
        }
        assert!(verified_action(&state).is_err(), "mutation {mutation}");
        assert!(!can_start(&state, "fixture-no-IPC"));
    }
}

#[test]
fn tmux_client_scope_requires_server_session_and_client_only_metadata() {
    for mutation in 0..9 {
        let mut state = state(InfoProgram::TmuxClient, false);
        let scope = state.report.as_mut().unwrap().actions[0]
            .program
            .as_mut()
            .unwrap();
        match mutation {
            0 => scope.tmux_client = None,
            1 => scope.tmux_client.as_mut().unwrap().server.pid = state.process.pid,
            2 => scope.tmux_client.as_mut().unwrap().server.start_ticks = 0,
            3 => scope.tmux_client.as_mut().unwrap().server.socket_path = "relative".into(),
            4 => scope.tmux_client.as_mut().unwrap().session_id = "$1; kill-server".into(),
            5 => scope.tmux_client.as_mut().unwrap().session_created = 0,
            6 => scope.tmux_client.as_mut().unwrap().session_name.clear(),
            7 => {
                scope.tmux_pane = Some("%7".into());
                scope.tmux_socket = Some("/fixture/server.sock".into());
            }
            _ => state.application = InfoProgram::Fish,
        }
        assert!(verified_action(&state).is_err(), "mutation {mutation}");
        assert!(!can_start(&state, "fixture-no-IPC"));
    }
    let ctx = egui::Context::default();
    let mut state = state(InfoProgram::TmuxClient, false);
    let output = settled(&ctx, &mut state);
    position(&output, "Update tmux client");
    position(&output, "Read-only access is preserved.");
    let text = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("pane applications and other clients keep running"));
    assert!(!text.contains("Running work is interrupted"));
    assert!(!text.contains("interrupting its current work"));
}

#[test]
fn program_dialog_opening_is_read_only_independent_and_does_not_retarget_running_work() {
    let ctx = egui::Context::default();
    let first = egui::ViewportId::from_hash_of("first-program");
    let second = egui::ViewportId::from_hash_of("second-program");
    let process = ProcessIdentity {
        pid: 123,
        start_ticks: 456,
    };
    assert!(
        super::super::show(
            &ctx,
            first,
            Some(crate::models::InfoUpdateTarget::Program {
                window: "exact-window".into(),
                process,
                application: InfoProgram::Fish,
            })
        )
        .is_none()
    );
    let one = ctx
        .data(|data| data.get_temp::<Shared>(state_id(first)))
        .unwrap();
    {
        let state = one.lock().unwrap();
        assert!(state.report.is_none() && !state.busy);
    }
    open(
        &ctx,
        second,
        "second-window".into(),
        process,
        InfoProgram::Codex,
    );
    let two = ctx
        .data(|data| data.get_temp::<Shared>(state_id(second)))
        .unwrap();
    assert!(!Arc::ptr_eq(&one, &two));
    assert_eq!(two.lock().unwrap().application, InfoProgram::Codex);
    one.lock().unwrap().busy = true;
    open(
        &ctx,
        first,
        "wrong-window".into(),
        ProcessIdentity {
            pid: 999,
            start_ticks: 888,
        },
        InfoProgram::Codex,
    );
    let current = ctx
        .data(|data| data.get_temp::<Shared>(state_id(first)))
        .unwrap();
    assert!(Arc::ptr_eq(&one, &current));
    assert_eq!(current.lock().unwrap().process, process);
    assert_eq!(current.lock().unwrap().application, InfoProgram::Fish);
}

#[test]
fn busy_failed_and_completed_program_jobs_cannot_be_started_again() {
    for phase in [
        TerminalUpgradePhase::Preparing,
        TerminalUpgradePhase::Running,
        TerminalUpgradePhase::Failed,
        TerminalUpgradePhase::Complete,
    ] {
        let mut state = state(InfoProgram::Codex, false);
        state.report.as_mut().unwrap().phase = phase;
        assert!(!can_start(&state, "fixture-no-IPC"));
    }
    let mut state = state(InfoProgram::Codex, true);
    state.busy = true;
    assert!(!can_start(&state, "fixture-no-IPC"));
    state.busy = false;
    state.error = Some("failed".into());
    assert!(!can_start(&state, "fixture-no-IPC"));
}

#[test]
fn successful_program_updates_close_the_modal_and_refresh_their_info_window_once() {
    for application in [
        InfoProgram::Fish,
        InfoProgram::Codex,
        InfoProgram::TmuxClient,
    ] {
        for dismissed in [false, true] {
            let ctx = egui::Context::default();
            let popup =
                egui::ViewportId::from_hash_of(("completed-update", application.name(), dismissed));
            let mut completed = state(application, true);
            completed.open = !dismissed;
            let report = completed.report.as_mut().unwrap();
            report.phase = TerminalUpgradePhase::Complete;
            report.selected_action = Some("program".into());
            report.message =
                "Update completed in the existing terminal window(s). Backup retained.".into();
            let shared = Arc::new(Mutex::new(completed));
            ctx.data_mut(|data| data.insert_temp(state_id(popup), shared.clone()));
            let mut event = None;
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |_| {});
                event = super::super::show(ctx, popup, None);
            });
            match event {
                Some(crate::models::PopupEvent::ProgramUpdated {
                    info,
                    window,
                    process,
                    application: actual,
                    focus,
                }) => {
                    assert_eq!(info, popup);
                    assert_eq!(window, "exact-window");
                    assert_eq!(
                        process,
                        ProcessIdentity {
                            pid: 123,
                            start_ticks: 456
                        }
                    );
                    assert_eq!(actual, application);
                    assert_eq!(focus, !dismissed);
                }
                event => panic!("missing completion refresh: {event:?}"),
            }
            assert!(output.shapes.iter().all(|shape| !matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.job.text.starts_with("Update ")
                    || text.galley.job.text.contains("Reopen Show Info"))));
            let completed = shared.lock().unwrap();
            assert!(!completed.open);
            assert!(completed.completion_reported);
            assert!(!can_start(&completed, "fixture-no-IPC"));
            drop(completed);
            assert!(take_completion(&ctx, popup).is_none());
            assert!(!show(&ctx, popup));
            assert!(
                take_completion(&ctx, egui::ViewportId::from_hash_of("another-info")).is_none()
            );
        }
    }
}

#[test]
fn failed_busy_or_mismatched_program_reports_do_not_dismiss_or_refresh_info() {
    for mutation in 0..7 {
        let ctx = egui::Context::default();
        let popup = egui::ViewportId::from_hash_of(("not-completed", mutation));
        let mut pending = state(InfoProgram::Fish, false);
        pending.report.as_mut().unwrap().phase = TerminalUpgradePhase::Complete;
        pending.report.as_mut().unwrap().selected_action = Some("program".into());
        match mutation {
            0 => {
                let report = pending.report.as_mut().unwrap();
                report.phase = TerminalUpgradePhase::Failed;
                report.message = "Restart failed; recovery plan retained".into();
                report.backup_path = Some("/fixture/recovery/restart-plan.json".into());
            }
            1 => pending.busy = true,
            2 => pending.error = Some("Failed to confirm daemon completion".into()),
            3 => pending.report.as_mut().unwrap().window_id = "different-window".into(),
            4 => pending.report.as_mut().unwrap().selected_action = Some("tmux_server".into()),
            5 => {
                pending.report.as_mut().unwrap().actions[0]
                    .program
                    .as_mut()
                    .unwrap()
                    .identity
                    .start_ticks += 1
            }
            _ => pending.report.as_mut().unwrap().selected_action = None,
        }
        let shared = Arc::new(Mutex::new(pending));
        ctx.data_mut(|data| data.insert_temp(state_id(popup), shared.clone()));
        assert!(
            take_completion(&ctx, popup).is_none(),
            "mutation {mutation}"
        );
        let current = shared.lock().unwrap();
        assert!(current.open);
        assert!(!current.completion_reported);
        drop(current);
        for frame in 0..3 {
            let mut event = None;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1100.0, 850.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |_| {});
                    event = super::super::show(ctx, popup, None);
                },
            );
            assert!(event.is_none());
            if mutation == 0 && frame == 2 {
                position(&output, "Restart failed; recovery plan retained");
                position(&output, "/fixture/recovery/restart-plan.json");
            }
        }
    }
}
