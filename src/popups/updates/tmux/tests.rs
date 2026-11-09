use super::*;
use applicationlauncher::tracker::{
    TmuxRestartPane, TmuxRestartRemoteClient, TmuxServerRestartScope,
};

const UPDATE: &str = "Update server and restart all sessions";
const ACCEPT: &str = "I accept restarting every session, interrupting its applications and disconnecting remote clients.";

fn state() -> State {
    let server = TmuxServerIdentity {
        pid: 123,
        start_ticks: 456,
        socket_path: "/fixture/server.sock".into(),
    };
    State {
        window: "exact-window".into(),
        server: server.clone(),
        open: true,
        busy: false,
        accepted: false,
        error: None,
        report: Some(ApplicationUpdateReport {
            operation_id: "fixture-no-IPC".into(),
            window_id: "exact-window".into(),
            phase: TerminalUpgradePhase::Ready,
            actions: vec![
                ApplicationUpdateAction {
                    id: "program".into(),
                    label: "Update Codex only".into(),
                    eligible: true,
                    message: "not a server action".into(),
                    tmux_server: None,
                    program: None,
                },
                ApplicationUpdateAction {
                    id: "tmux_server".into(),
                    label: "Update server".into(),
                    eligible: true,
                    message: "All sessions will restart".into(),
                    program: None,
                    tmux_server: Some(TmuxServerRestartScope {
                        identity: server,
                        executable: "/fixture/new/tmux".into(),
                        sessions: vec!["work".into(), "detached".into()],
                        windows: 2,
                        terminal_tabs: 1,
                        remote_clients: Vec::new(),
                        panes: vec![
                            TmuxRestartPane {
                                session: "work".into(),
                                window_index: "0".into(),
                                pane_index: "0".into(),
                                application: "codex".into(),
                                cwd: "/fixture/work".into(),
                            },
                            TmuxRestartPane {
                                session: "detached".into(),
                                window_index: "1".into(),
                                pane_index: "0".into(),
                                application: "fish".into(),
                                cwd: "/fixture/detached".into(),
                            },
                        ],
                    }),
                },
            ],
            ..Default::default()
        }),
    }
}

fn frame(
    ctx: &egui::Context,
    state: &mut State,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, Option<Action>) {
    frame_at(ctx, state, events, egui::vec2(1200.0, 900.0))
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
            action = dialog(ctx, egui::Id::new("test-server-dialog"), state);
        },
    );
    (output, action)
}

fn text(output: &egui::FullOutput) -> String {
    output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
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
        .unwrap_or_else(|| panic!("missing {label}: {}", text(output)))
}

fn click(pos: egui::Pos2) -> Vec<egui::Event> {
    let button = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    vec![egui::Event::PointerMoved(pos), button(true), button(false)]
}

fn settled(ctx: &egui::Context, state: &mut State) -> egui::FullOutput {
    for _ in 0..3 {
        assert!(frame(ctx, state, vec![]).1.is_none());
    }
    frame(ctx, state, vec![]).0
}

#[test]
fn server_popup_only_offers_server_restart_and_requires_explicit_consent() {
    let ctx = egui::Context::default();
    let mut state = state();
    let output = settled(&ctx, &mut state);
    let content = text(&output);
    for expected in [
        "stops ALL sessions",
        "SAME existing terminal windows",
        "Detached sessions remain detached",
        "Session: detached",
        "codex - /fixture/work",
        "fish - /fixture/detached",
        "2 sessions, 2 tmux windows, 2 panes, 1 attached terminal tabs",
    ] {
        assert!(content.contains(expected), "missing {expected}: {content}");
    }
    for forbidden in [
        "This window",
        "All outdated windows",
        "Update Codex only",
        "Update Terminal Windows",
    ] {
        assert!(!content.contains(forbidden));
    }
    assert!(
        frame(&ctx, &mut state, click(position(&output, UPDATE)))
            .1
            .is_none()
    );
    let output = settled(&ctx, &mut state);
    assert!(
        frame(&ctx, &mut state, click(position(&output, ACCEPT)))
            .1
            .is_none()
    );
    assert!(state.accepted);
    let output = settled(&ctx, &mut state);
    assert_eq!(
        frame(&ctx, &mut state, click(position(&output, UPDATE))).1,
        Some(Action::Start("fixture-no-IPC".into()))
    );
}

#[test]
fn server_popup_lists_remote_disconnects_before_accepting_the_server_restart() {
    let ctx = egui::Context::default();
    let mut state = state();
    state.report.as_mut().unwrap().actions[1]
        .tmux_server
        .as_mut()
        .unwrap()
        .remote_clients
        .push(TmuxRestartRemoteClient {
            pid: 789,
            session: "work".into(),
            transport: "mosh".into(),
        });
    let output = settled(&ctx, &mut state);
    let content = text(&output);
    for expected in [
        "1 remote tmux client(s) will be disconnected",
        "Reattach manually",
        "Disconnect: work (mosh; tmux client PID 789)",
        "disconnecting remote clients",
    ] {
        assert!(content.contains(expected), "missing {expected}: {content}");
    }
    assert!(
        frame(&ctx, &mut state, click(position(&output, UPDATE)))
            .1
            .is_none()
    );
    let output = settled(&ctx, &mut state);
    assert!(
        frame(&ctx, &mut state, click(position(&output, ACCEPT)))
            .1
            .is_none()
    );
    let output = settled(&ctx, &mut state);
    assert_eq!(
        frame(&ctx, &mut state, click(position(&output, UPDATE))).1,
        Some(Action::Start("fixture-no-IPC".into()))
    );
}

#[test]
fn server_popup_refuses_invalid_or_ambiguous_remote_disconnect_scopes() {
    for invalid in ["pid", "transport", "session", "duplicate"] {
        let mut state = state();
        let scope = state.report.as_mut().unwrap().actions[1]
            .tmux_server
            .as_mut()
            .unwrap();
        let mut remote = TmuxRestartRemoteClient {
            pid: 789,
            session: "work".into(),
            transport: "mosh".into(),
        };
        match invalid {
            "pid" => remote.pid = 1,
            "transport" => remote.transport = "unknown".into(),
            "session" => remote.session = "different".into(),
            "duplicate" => scope.remote_clients.push(remote.clone()),
            _ => unreachable!(),
        }
        scope.remote_clients.push(remote);
        state.accepted = true;
        let ctx = egui::Context::default();
        let output = settled(&ctx, &mut state);
        assert!(!state.accepted, "{invalid}");
        assert!(
            frame(&ctx, &mut state, click(position(&output, UPDATE)))
                .1
                .is_none(),
            "{invalid}"
        );
    }
}

#[test]
fn server_popup_refuses_stale_missing_ambiguous_and_unsupported_scope() {
    for variant in 0..10 {
        let mut state = state();
        match variant {
            0 => state.server.pid += 1,
            1 => state.server.start_ticks += 1,
            2 => state.server.socket_path.push_str("-other"),
            3 => state.window.push_str("-other"),
            4 => state.report.as_mut().unwrap().actions[1].tmux_server = None,
            5 => state.report.as_mut().unwrap().actions[1].eligible = false,
            6 => state.report.as_mut().unwrap().actions[1]
                .tmux_server
                .as_mut()
                .unwrap()
                .panes
                .clear(),
            7 => {
                let action = state.report.as_ref().unwrap().actions[1].clone();
                state.report.as_mut().unwrap().actions.push(action);
            }
            8 => state
                .report
                .as_mut()
                .unwrap()
                .actions
                .retain(|action| action.id != "tmux_server"),
            9 => state.report.as_mut().unwrap().actions[1]
                .tmux_server
                .as_mut()
                .unwrap()
                .sessions
                .clear(),
            _ => unreachable!(),
        }
        assert!(verified_action(&state).is_err(), "variant {variant}");
        state.accepted = true;
        let ctx = egui::Context::default();
        let output = settled(&ctx, &mut state);
        assert!(!state.accepted);
        assert!(
            frame(&ctx, &mut state, click(position(&output, UPDATE)))
                .1
                .is_none()
        );
    }
}

#[test]
fn server_popup_cancel_escape_and_default_enter_never_start_an_update() {
    for variant in 0..4 {
        let ctx = egui::Context::default();
        let mut state = state();
        let output = settled(&ctx, &mut state);
        state.accepted = true;
        let events = match variant {
            0 => click(position(&output, "Close")),
            1 => vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            2 => vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            _ => click(egui::pos2(4.0, 4.0)),
        };
        assert_eq!(
            frame(&ctx, &mut state, events).1,
            Some(Action::Close),
            "variant {variant}"
        );
    }
}

#[test]
fn server_popup_controls_remain_visible_in_a_small_info_window() {
    let ctx = egui::Context::default();
    let mut state = state();
    let size = egui::vec2(520.0, 360.0);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
    for _ in 0..3 {
        frame_at(&ctx, &mut state, vec![], size);
    }
    let output = frame_at(&ctx, &mut state, vec![], size).0;
    for label in ["Close", ACCEPT, UPDATE] {
        assert!(
            screen.contains(position(&output, label)),
            "{label} must stay onscreen"
        );
    }
    assert!(
        frame_at(&ctx, &mut state, click(position(&output, ACCEPT)), size)
            .1
            .is_none()
    );
    assert!(state.accepted);
    let output = frame_at(&ctx, &mut state, vec![], size).0;
    assert_eq!(
        frame_at(&ctx, &mut state, click(position(&output, UPDATE)), size).1,
        Some(Action::Start("fixture-no-IPC".into()))
    );
}

#[test]
fn opening_server_update_is_read_only_and_isolated_from_other_info_windows() {
    let ctx = egui::Context::default();
    let first = egui::ViewportId::from_hash_of("first");
    let second = egui::ViewportId::from_hash_of("second");
    let server = state().server;
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        assert!(
            super::super::show(
                ctx,
                first,
                Some(crate::models::InfoUpdateTarget::TmuxServer {
                    window: "first-window".into(),
                    server: server.clone()
                })
            )
            .is_none()
        );
        assert!(
            super::super::show(
                ctx,
                second,
                Some(crate::models::InfoUpdateTarget::TmuxServer {
                    window: "second-window".into(),
                    server: server.clone()
                })
            )
            .is_none()
        );
    });
    let a = ctx
        .data(|data| data.get_temp::<Shared>(state_id(first)))
        .unwrap();
    let b = ctx
        .data(|data| data.get_temp::<Shared>(state_id(second)))
        .unwrap();
    assert!(!Arc::ptr_eq(&a, &b));
    for (state, window) in [(a, "first-window"), (b, "second-window")] {
        let state = state.lock().unwrap();
        assert_eq!(state.window, window);
        assert!(state.open && !state.busy && !state.accepted && state.report.is_none());
    }
    // No request is spawned in the frame that opens the dialog.
}

#[test]
fn native_info_server_link_opens_the_server_modal_without_terminal_update_events() {
    use crate::models::{InfoPopupData, InfoProcessSection, InfoProcessTree, InfoUpdateTarget};
    use crate::popups::show_deferred_info_popup;
    use std::sync::atomic::AtomicBool;

    let ctx = egui::Context::default();
    ctx.set_embed_viewports(false);
    let popup = egui::ViewportId::from_hash_of("native-server-link-fixture");
    let id = egui::Id::new("server-header-fixture");
    let target = state().server;
    let data = InfoPopupData {
        title: "Window Info".into(),
        heading: "tmux fixture".into(),
        subtitle: "Process tree".into(),
        scroll_sensitivity: 1.0,
        rows: vec![],
        process_tree: Some(InfoProcessTree {
            notice: "fixture".into(),
            sections: vec![InfoProcessSection {
                id,
                depth: 0,
                header: "tmux-server (pid 123)".into(),
                binary_is_current: Some(false),
                update_target: Some(InfoUpdateTarget::TmuxServer {
                    window: "exact-window".into(),
                    server: target.clone(),
                }),
                default_open: false,
                rows: vec![],
                metadata_sections: vec![],
            }],
        }),
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    let registered = ctx.run(egui::RawInput::default(), |ctx| {
        show_deferred_info_popup(
            ctx,
            popup,
            data.clone(),
            [1200.0, 800.0],
            [520.0, 360.0],
            Arc::new(AtomicBool::new(false)),
            sender.clone(),
        );
    });
    let callback = registered.viewport_output[&popup]
        .viewport_ui_cb
        .as_ref()
        .unwrap();
    let link_rect = std::cell::Cell::new(None);
    let frame = |events| {
        let mut input = egui::RawInput {
            viewport_id: popup,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            events,
            ..Default::default()
        };
        input.viewports.insert(
            popup,
            egui::ViewportInfo {
                parent: Some(egui::ViewportId::ROOT),
                ..Default::default()
            },
        );
        ctx.run(input, |ctx| {
            callback(ctx);
            if let Some(link) = ctx.read_response(id.with("terminal-update-link")) {
                link_rect.set(Some(link.rect));
            }
        })
    };
    for _ in 0..3 {
        frame(vec![]);
    }
    frame(click(
        link_rect
            .get()
            .expect("server header must have an inline Update link")
            .center(),
    ));
    let shared = ctx
        .data(|data| data.get_temp::<Shared>(state_id(popup)))
        .expect("server link must open its own dialog");
    {
        let mut state = shared.lock().unwrap();
        assert_eq!(state.server, target);
        assert_eq!(state.window, "exact-window");
        assert!(state.report.is_none() && !state.busy && !state.accepted);
        // Publish a fixture preview rather than connecting to the user's daemon.
        state.report = self::state().report;
    }
    for _ in 0..3 {
        frame(vec![]);
    }
    assert!(ctx.memory(|memory| memory.top_modal_layer().is_some()));
    let output = frame(vec![]);
    assert!(text(&output).contains(UPDATE));
    assert!(!text(&output).contains("This window"));
    frame(vec![egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }]);
    assert!(!shared.lock().unwrap().open);
    assert!(
        receiver.try_recv().is_err(),
        "cancelling the server dialog must not close Show Info or emit terminal maintenance requests"
    );
    assert!(!crate::popups::process_info_section_is_open(
        &ctx, id, false
    ));
}
