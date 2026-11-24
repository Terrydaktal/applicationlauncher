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
        application, open: true, busy: false, accepted: false, error: None,
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
        for label in [
            "Close",
            "I accept restarting only this program and interrupting its current work.",
            &format!("Update {} only", application.name()),
        ] {
            assert!(
                screen.contains(position(&output, label)),
                "{label} must stay onscreen"
            );
        }
        let consent = position(
            &output,
            "I accept restarting only this program and interrupting its current work.",
        );
        assert!(frame_at(&ctx, &mut state, click(consent), size).1.is_none());
        assert!(state.accepted);
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
            assert!(!can_start(&state, "fixture-no-IPC"));
            let output = settled(&ctx, &mut state);
            let start = position(&output, &format!("Update {} only", application.name()));
            assert!(frame(&ctx, &mut state, click(start)).1.is_none());
            let output = settled(&ctx, &mut state);
            let consent = position(
                &output,
                "I accept restarting only this program and interrupting its current work.",
            );
            assert!(frame(&ctx, &mut state, click(consent)).1.is_none());
            assert!(state.accepted && can_start(&state, "fixture-no-IPC"));
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
            assert!(!can_start(&state, "fixture-no-IPC"));
        }
    }
}

#[test]
fn program_scope_rejects_changed_pid_start_kind_window_missing_and_ambiguous_metadata() {
    for mutation in 0..10 {
        let mut state = state(InfoProgram::Fish, true);
        state.accepted = true;
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
        state.accepted = true;
        state.report.as_mut().unwrap().phase = phase;
        assert!(!can_start(&state, "fixture-no-IPC"));
    }
    let mut state = state(InfoProgram::Codex, true);
    state.accepted = true;
    state.busy = true;
    assert!(!can_start(&state, "fixture-no-IPC"));
    state.busy = false;
    state.error = Some("failed".into());
    assert!(!can_start(&state, "fixture-no-IPC"));
}
