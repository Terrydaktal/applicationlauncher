use std::sync::{Arc, Mutex};
use std::time::Duration;

use applicationlauncher::tracker::{TerminalUpgradePhase, TrackerClient};
use eframe::egui;

use super::App;
use crate::models::{InfoUpdateTarget, TerminalMaintenanceState, WindowInfo};
use crate::settings::{ConfirmationResponse, destructive_action_confirmation};

const POPUP_ID: &str = "terminal_maintenance_popup";

pub(super) fn window_update_target(window: &WindowInfo) -> Option<InfoUpdateTarget> {
    (!window.id.is_empty()
        && window.pid.is_some_and(|pid| pid > 1)
        && window.class.to_ascii_lowercase().contains("xfce4-terminal")
        && window.exe_path.as_ref().is_none_or(|path| {
            path.file_name()
                .is_some_and(|name| name == "xfce4-terminal" || name == "xfce4-terminal (deleted)")
        }))
    .then(|| InfoUpdateTarget::TerminalWindow(window.id.clone()))
}

fn running(state: &TerminalMaintenanceState) -> bool {
    (state.open && state.info_viewport.is_some() && state.update_window_on_ready)
        || state.worker_running
        || state
            .report
            .as_ref()
            .is_some_and(|report| report.phase == TerminalUpgradePhase::Running)
}

fn reset_request(state: &mut TerminalMaintenanceState, window: Option<String>) {
    state.update_window_on_ready = window.is_some();
    state.scope_window = window;
    state.info_viewport = None;
    state.report = None;
    state.selected.clear();
    state.error = None;
    state.confirmed = false;
    state.confirming_update = false;
}

pub(super) fn open(shared: &Arc<Mutex<TerminalMaintenanceState>>, window: Option<String>) {
    if let Ok(mut state) = shared.lock() {
        if !running(&state) {
            reset_request(&mut state, window);
        }
        state.open = true;
        state.focus_requested = true;
    }
}

pub(super) fn open_direct(
    shared: &Arc<Mutex<TerminalMaintenanceState>>,
    window: String,
    info: egui::ViewportId,
) -> Result<(), String> {
    let mut state = shared
        .lock()
        .map_err(|_| "Terminal update state is unavailable")?;
    if running(&state) {
        return Err("Another terminal update is already being checked or requested. No additional update was queued.".into());
    }
    reset_request(&mut state, Some(window));
    state.open = true;
    state.focus_requested = false;
    state.info_viewport = Some(info);
    Ok(())
}

fn review_popup_required(state: &TerminalMaintenanceState) -> bool {
    state.open && state.info_viewport.is_none()
}

impl App {
    pub(super) fn terminal_maintenance_is_open(&self) -> bool {
        self.terminal_maintenance
            .lock()
            .is_ok_and(|state| state.open)
    }

    pub(super) fn show_terminal_maintenance(&self, ctx: &egui::Context) {
        if !self.terminal_maintenance_is_open() {
            return;
        }
        let action = self.terminal_maintenance.lock().ok().and_then(|state| {
            if state.worker_running || state.error.is_some() {
                return None;
            }
            match &state.report {
                None => Some(Action::Refresh),
                Some(report)
                    if matches!(
                        report.phase,
                        TerminalUpgradePhase::Preparing | TerminalUpgradePhase::Running
                    ) =>
                {
                    Some(Action::Poll {
                        id: report.operation_id.clone(),
                    })
                }
                _ => None,
            }
        });
        if let Some(action) = action {
            request(self.terminal_maintenance.clone(), ctx.clone(), action);
        }
        if !self
            .terminal_maintenance
            .lock()
            .is_ok_and(|state| review_popup_required(&state))
        {
            return;
        }
        let shared = self.terminal_maintenance.clone();
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of(POPUP_ID),
            egui::ViewportBuilder::default()
                .with_title("Update Terminal Windows")
                .with_inner_size([820.0, 620.0])
                .with_min_inner_size([560.0, 380.0])
                .with_resizable(true)
                .with_always_on_top(),
            move |ctx, _| {
                let modal_open = ctx.memory(|memory| memory.top_modal_layer().is_some());
                if ctx.input(|input| {
                    input.viewport().close_requested()
                        || (!modal_open && input.key_pressed(egui::Key::Escape))
                }) {
                    close_popup(&shared, ctx);
                    return;
                }
                let mut action = None;
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().inner_margin(12))
                    .show(ctx, |ui| {
                        if let Ok(mut state) = shared.lock() {
                            if std::mem::take(&mut state.focus_requested) {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                            }
                            action = panel(ui, &mut state);
                        }
                    });
                if let Some(action) = action {
                    if matches!(action, Action::Close) {
                        close_popup(&shared, ctx);
                    } else {
                        request(shared.clone(), ctx.clone(), action);
                    }
                }
            },
        );
    }
}

fn close_popup(shared: &Arc<Mutex<TerminalMaintenanceState>>, ctx: &egui::Context) {
    if let Ok(mut state) = shared.lock() {
        state.open = false;
        state.update_window_on_ready = false;
        state.confirmed = false;
        state.confirming_update = false;
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    ctx.request_repaint_of(egui::ViewportId::ROOT);
}

enum Action {
    Refresh,
    Poll { id: String },
    Start { id: String, selected: Vec<String> },
    Close,
}

fn automatic_window_update(state: &mut TerminalMaintenanceState) -> Result<Option<Action>, String> {
    if !state.open || state.error.is_some() {
        state.update_window_on_ready = false;
        return Ok(None);
    }
    if !state.update_window_on_ready {
        return Ok(None);
    }
    let Some(report) = state.report.as_ref() else {
        return Ok(None);
    };
    if report.phase != TerminalUpgradePhase::Ready {
        if report.phase != TerminalUpgradePhase::Preparing {
            state.update_window_on_ready = false;
        }
        return Ok(None);
    }
    // The scope chooser confirms exactly one window, never the preview's bulk selection.
    state.update_window_on_ready = false;
    let scope = state
        .scope_window
        .as_ref()
        .ok_or("The confirmed terminal window is unavailable; no windows were updated")?;
    let mut matches = report
        .windows
        .iter()
        .filter(|window| &window.window_id == scope);
    let target = matches
        .next()
        .ok_or("The confirmed terminal window is no longer open; no windows were updated")?;
    if matches.next().is_some() {
        return Err("The terminal window is ambiguous; no windows were updated".into());
    }
    if !target.eligible {
        return Err(format!(
            "This terminal window cannot be updated: {}",
            target.message
        ));
    }
    state.selected = [scope.clone()].into_iter().collect();
    state.confirmed = false;
    state.confirming_update = false;
    Ok(Some(Action::Start {
        id: report.operation_id.clone(),
        selected: vec![scope.clone()],
    }))
}

fn panel(ui: &mut egui::Ui, state: &mut TerminalMaintenanceState) -> Option<Action> {
    let mut action = None;
    let was_confirming = state.confirming_update;
    ui.heading("Update Terminal Windows");
    ui.label("Update the selected xfce4-terminal windows in place, preserving their screen position and size. tmux tabs reattach to their exact existing sessions without restarting the tmux server or its applications. Supported applications in non-tmux tabs are restarted in their working directories.");
    ui.label("The daemon coordinates each update and retains recovery commands. Native terminal controls are optional: older windows can be replaced without an updater API when their applications and tabs can be identified safely.");
    ui.small("Only outdated builds can be selected. Up-to-date windows are not restarted.");
    ui.colored_label(egui::Color32::from_rgb(240, 196, 92), "Non-tmux programs are interrupted. Scrollback, unsaved shell state and in-flight work cannot be preserved. Unsupported or changed windows are skipped; there is no force-kill.");
    ui.small("tmux tabs reattach to their exact surviving sessions before their old window closes. Activity is not always visible in inactive tabs; review the complete tab list.");
    ui.separator();
    if let Some(error) = &state.error {
        ui.colored_label(egui::Color32::LIGHT_RED, error);
    }
    if state.worker_running {
        ui.spinner();
    }
    let ready = state
        .report
        .as_ref()
        .is_some_and(|report| report.phase == TerminalUpgradePhase::Ready)
        && !state.worker_running;
    if let Some(report) = &state.report {
        state.selected.retain(|id| {
            report
                .windows
                .iter()
                .any(|window| window.window_id == *id && window.eligible)
                && state.scope_window.as_ref().is_none_or(|scope| scope == id)
        });
        ui.label(&report.message);
        if let Some(id) = report.backup_snapshot_id {
            ui.label(format!(
                "Backup #{id}: F9 -> Saved sessions -> Before terminal restart"
            ));
        }
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .max_height((ui.available_height() - 130.0).max(80.0))
            .show(ui, |ui| {
                if report.windows.is_empty() {
                    ui.label("No terminal windows were found.");
                }
                for window in &report.windows {
                    if state
                        .scope_window
                        .as_ref()
                        .is_some_and(|scope| scope != &window.window_id)
                    {
                        continue;
                    }
                    ui.push_id(&window.window_id, |ui| {
                        let mut selected = state.selected.contains(&window.window_id);
                        if ui
                            .add_enabled(
                                ready && window.eligible,
                                egui::Checkbox::new(&mut selected, &window.title),
                            )
                            .changed()
                        {
                            if selected {
                                state.selected.insert(window.window_id.clone());
                            } else {
                                state.selected.remove(&window.window_id);
                            }
                        }
                        ui.small(format!("{}: {}", window.status, window.message));
                        for tab in &window.tabs {
                            ui.small(format!("Tab: {tab}"));
                        }
                        if !window.executable.is_empty() {
                            ui.small(format!("Replacement: {}", window.executable));
                            if let Some(id) = &window.build_id {
                                ui.small(format!("Build ID: {id}"));
                            }
                            ui.small(format!("SHA256: {}", window.sha256));
                        }
                        ui.separator();
                    });
                }
            });
    }
    if ready {
        ui.checkbox(&mut state.confirmed, "I reviewed every selected tab and accept interruption and loss of unsaved terminal state.");
    }
    if !ready || !state.confirmed || state.selected.is_empty() {
        state.confirming_update = false;
    }
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!state.worker_running, egui::Button::new("Refresh preview"))
            .clicked()
        {
            action = Some(Action::Refresh);
        }
        if ui
            .add_enabled(
                ready && state.confirmed && !state.selected.is_empty() && !state.confirming_update,
                egui::Button::new("Request updates in selected terminals"),
            )
            .clicked()
        {
            state.confirming_update = true;
            ui.ctx().request_repaint();
        }
        if ui.button("Close").clicked() {
            action = Some(Action::Close);
        }
    });
    if matches!(action, Some(Action::Refresh | Action::Close)) {
        state.confirmed = false;
        state.confirming_update = false;
    }
    if was_confirming && state.confirming_update {
        let count = state.selected.len();
        let message = format!(
            "Request updates for {count} selected outdated terminal window(s)? Up-to-date windows are left untouched. Non-tmux programs may be interrupted; scrollback and unsaved terminal state can be lost. This cannot be undone."
        );
        match destructive_action_confirmation(
            ui.ctx(),
            "update_terminals_confirmation",
            "Update selected terminal windows?",
            &message,
            "Update selected terminals",
        ) {
            ConfirmationResponse::Confirm => {
                state.confirming_update = false;
                state.confirmed = false;
                action = state.report.as_ref().map(|report| Action::Start {
                    id: report.operation_id.clone(),
                    selected: state.selected.iter().cloned().collect(),
                });
            }
            ConfirmationResponse::Cancel => {
                state.confirming_update = false;
                state.confirmed = false;
            }
            ConfirmationResponse::Pending => {}
        }
    }
    ui.small("Closing this popup or the launcher does not cancel a restart already confirmed. Backups remain until manually deleted.");
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use applicationlauncher::tracker::{TerminalUpgradeReport, TerminalUpgradeWindow};

    fn state() -> TerminalMaintenanceState {
        TerminalMaintenanceState {
            open: true,
            selected: ["supported".into()].into_iter().collect(),
            report: Some(TerminalUpgradeReport {
                operation_id: "fixture-only-no-live-IPC".into(),
                phase: TerminalUpgradePhase::Ready,
                windows: vec![TerminalUpgradeWindow {
                    window_id: "supported".into(),
                    terminal_window_uuid: "fixture-window-uuid".into(),
                    owner_pid: 123,
                    title: "codex - fixture - Terminal".into(),
                    tabs: vec!["codex - /fixture (active)".into()],
                    eligible: true,
                    recommended: true,
                    preserves_processes: false,
                    executable: "/fixture/xfce4-terminal".into(),
                    build_id: None,
                    sha256: "fixture".into(),
                    status: "ready".into(),
                    message: "fixture preview".into(),
                }],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn render(
        ctx: &egui::Context,
        state: &mut TerminalMaintenanceState,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Option<Action>) {
        let mut action = None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1100.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    action = panel(ui, state);
                });
            },
        );
        (output, action)
    }

    fn button_center(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("expected button label")
    }

    #[test]
    fn terminal_maintenance_preview_explains_tmux_reattachment_vs_application_restart() {
        let ctx = egui::Context::default();
        let mut state = state();
        let (output, action) = render(&ctx, &mut state, vec![]);
        assert!(action.is_none());
        let text = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "in place",
            "screen position and size",
            "tmux tabs reattach to their exact existing sessions",
            "without restarting the tmux server or its applications",
            "non-tmux tabs are restarted in their working directories",
        ] {
            assert!(text.contains(expected), "missing {expected}");
        }
    }

    fn click(position: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
        ]
    }

    #[test]
    fn direct_info_updates_start_once_without_a_review_viewport() {
        let shared = Arc::new(Mutex::new(state()));
        let info = egui::ViewportId::from_hash_of("direct-terminal-info-fixture");
        open_direct(&shared, "supported".into(), info).unwrap();
        let mut current = shared.lock().unwrap();
        assert!(current.open && !current.focus_requested);
        assert_eq!(current.info_viewport, Some(info));
        assert_eq!(current.scope_window.as_deref(), Some("supported"));
        assert!(!review_popup_required(&current));
        assert!(automatic_window_update(&mut current).unwrap().is_none());
        assert!(current.update_window_on_ready);
        current.report = state().report;
        match automatic_window_update(&mut current).unwrap() {
            Some(Action::Start { selected, .. }) => assert_eq!(selected, ["supported"]),
            _ => panic!("the scope choice must directly start the verified window"),
        }
        assert!(automatic_window_update(&mut current).unwrap().is_none());
        assert!(!review_popup_required(&current));
    }

    #[test]
    fn direct_info_updates_never_fall_back_to_a_review_window_on_failure_or_completion() {
        let shared = Arc::new(Mutex::new(state()));
        let info = egui::ViewportId::from_hash_of("direct-terminal-failure-fixture");
        open_direct(&shared, "supported".into(), info).unwrap();
        {
            let mut current = shared.lock().unwrap();
            current.report = state().report;
            current.report.as_mut().unwrap().windows[0].eligible = false;
            current.report.as_mut().unwrap().windows[0].message = "This running terminal lacks the terminal-owned update API; restart it once manually".into();
            current.error = Some(automatic_window_update(&mut current).err().unwrap());
            assert!(
                current
                    .error
                    .as_ref()
                    .unwrap()
                    .contains("restart it once manually")
            );
            assert!(!current.update_window_on_ready);
            assert!(!review_popup_required(&current));
            for phase in [
                TerminalUpgradePhase::Preparing,
                TerminalUpgradePhase::Running,
                TerminalUpgradePhase::Failed,
                TerminalUpgradePhase::Complete,
            ] {
                current.report.as_mut().unwrap().phase = phase;
                assert!(!review_popup_required(&current));
            }
        }
        open(&shared, None);
        let current = shared.lock().unwrap();
        assert_eq!(current.info_viewport, None);
        assert!(review_popup_required(&current));
    }

    #[test]
    fn direct_info_updates_do_not_retarget_pending_or_running_work() {
        for worker_running in [false, true] {
            let shared = Arc::new(Mutex::new(state()));
            let first = egui::ViewportId::from_hash_of("first-direct-info");
            let second = egui::ViewportId::from_hash_of("second-direct-info");
            open_direct(&shared, "first-window".into(), first).unwrap();
            shared.lock().unwrap().worker_running = worker_running;
            for window in ["first-window", "other-window"] {
                assert!(
                    open_direct(&shared, window.into(), second)
                        .unwrap_err()
                        .contains("No additional update was queued")
                );
                let current = shared.lock().unwrap();
                assert_eq!(current.info_viewport, Some(first));
                assert_eq!(current.scope_window.as_deref(), Some("first-window"));
                assert!(current.update_window_on_ready);
            }
        }
    }

    #[test]
    fn completed_direct_requests_stop_background_ui_and_keep_the_inline_result() {
        let shared = Arc::new(Mutex::new(state()));
        let info = egui::ViewportId::from_hash_of("completed-direct-info");
        open_direct(&shared, "supported".into(), info).unwrap();
        {
            let mut current = shared.lock().unwrap();
            current.worker_running = true;
            current.error = Some("Unsupported legacy terminal; restart it once manually".into());
        }
        drop(RequestGuard {
            shared: shared.clone(),
            ctx: egui::Context::default(),
        });
        let current = shared.lock().unwrap();
        assert!(!current.open && !current.worker_running && !current.update_window_on_ready);
        assert_eq!(current.info_viewport, Some(info));
        assert!(
            current
                .error
                .as_ref()
                .unwrap()
                .contains("restart it once manually")
        );
        assert!(!review_popup_required(&current));
    }

    #[test]
    fn opening_from_info_resets_consent_and_only_single_window_choices_start_automatically() {
        for window in [Some("exact-window-id".to_string()), None] {
            let shared = Arc::new(Mutex::new(state()));
            {
                let mut state = shared.lock().unwrap();
                state.confirmed = true;
                state.confirming_update = true;
            }
            open(&shared, window.clone());
            let state = shared.lock().unwrap();
            assert!(state.open && state.focus_requested);
            assert_eq!(state.scope_window, window);
            assert_eq!(state.update_window_on_ready, window.is_some());
            assert!(state.report.is_none() && state.selected.is_empty());
            assert!(!state.worker_running && !state.confirmed && !state.confirming_update);
        }
    }

    #[test]
    fn confirmed_single_window_starts_once_without_bulk_selection_or_another_confirmation() {
        let mut state = state();
        let mut other = state.report.as_ref().unwrap().windows[0].clone();
        other.window_id = "other".into();
        state.report.as_mut().unwrap().windows.push(other);
        state.selected = ["other".into()].into_iter().collect();
        state.scope_window = Some("supported".into());
        state.update_window_on_ready = true;
        state.worker_running = true;
        assert!(!state.confirmed && !state.confirming_update);
        match automatic_window_update(&mut state).unwrap() {
            Some(Action::Start { id, selected }) => {
                assert_eq!(id, "fixture-only-no-live-IPC");
                assert_eq!(selected, ["supported"]);
            }
            _ => {
                panic!("the explicit single-window choice must start without another confirmation")
            }
        }
        assert_eq!(state.selected, ["supported".into()].into_iter().collect());
        assert!(!state.update_window_on_ready);
        assert!(automatic_window_update(&mut state).unwrap().is_none());
    }

    #[test]
    fn automatic_single_window_update_waits_for_a_ready_preview() {
        let mut state = state();
        state.scope_window = Some("supported".into());
        state.update_window_on_ready = true;
        state.report.as_mut().unwrap().phase = TerminalUpgradePhase::Preparing;
        assert!(automatic_window_update(&mut state).unwrap().is_none());
        assert!(state.update_window_on_ready);
        state.report.as_mut().unwrap().phase = TerminalUpgradePhase::Ready;
        assert!(matches!(
            automatic_window_update(&mut state),
            Ok(Some(Action::Start { .. }))
        ));
    }

    #[test]
    fn automatic_single_window_update_never_falls_back_to_another_window() {
        for condition in ["missing", "ambiguous", "ineligible", "no-scope"] {
            let mut state = state();
            state.scope_window = Some("supported".into());
            state.update_window_on_ready = true;
            match condition {
                "missing" => state.report.as_mut().unwrap().windows[0].window_id = "other".into(),
                "ambiguous" => {
                    let duplicate = state.report.as_ref().unwrap().windows[0].clone();
                    state.report.as_mut().unwrap().windows.push(duplicate);
                }
                "ineligible" => state.report.as_mut().unwrap().windows[0].eligible = false,
                "no-scope" => state.scope_window = None,
                _ => unreachable!(),
            }
            assert!(automatic_window_update(&mut state).is_err(), "{condition}");
            assert!(!state.update_window_on_ready, "{condition}");
            assert!(automatic_window_update(&mut state).unwrap().is_none());
        }
    }

    #[test]
    fn automatic_single_window_update_respects_cancellation_errors_and_bulk_review() {
        for condition in ["closed", "error", "bulk", "failed", "complete", "running"] {
            let mut state = state();
            state.scope_window = Some("supported".into());
            state.update_window_on_ready = true;
            match condition {
                "closed" => state.open = false,
                "error" => state.error = Some("validation failed".into()),
                "bulk" => {
                    state.scope_window = None;
                    state.update_window_on_ready = false;
                }
                "failed" => state.report.as_mut().unwrap().phase = TerminalUpgradePhase::Failed,
                "complete" => state.report.as_mut().unwrap().phase = TerminalUpgradePhase::Complete,
                "running" => state.report.as_mut().unwrap().phase = TerminalUpgradePhase::Running,
                _ => unreachable!(),
            }
            assert!(
                automatic_window_update(&mut state).unwrap().is_none(),
                "{condition}"
            );
            assert!(!state.update_window_on_ready, "{condition}");
            state.open = true;
            state.error = None;
            state.report.as_mut().unwrap().phase = TerminalUpgradePhase::Ready;
            assert!(automatic_window_update(&mut state).unwrap().is_none());
        }
    }

    #[test]
    fn opening_from_info_does_not_change_an_already_running_update() {
        let mut running = state();
        running.report.as_mut().unwrap().phase = TerminalUpgradePhase::Running;
        running.scope_window = Some("original".into());
        running.update_window_on_ready = true;
        let shared = Arc::new(Mutex::new(running));
        open(&shared, None);
        let state = shared.lock().unwrap();
        assert!(state.open && state.focus_requested);
        assert_eq!(state.scope_window.as_deref(), Some("original"));
        assert!(state.update_window_on_ready);
        assert!(state.selected.contains("supported"));
        assert_eq!(
            state.report.as_ref().unwrap().phase,
            TerminalUpgradePhase::Running
        );
    }

    #[test]
    fn headless_confirmation_popup_does_not_start_any_restart_without_explicit_acknowledgement() {
        let ctx = egui::Context::default();
        let mut state = state();
        for _ in 0..3 {
            assert!(render(&ctx, &mut state, vec![]).1.is_none());
        }
        let (output, _) = render(&ctx, &mut state, vec![]);
        let position = button_center(&output, "Request updates in selected terminals");
        assert!(render(&ctx, &mut state, click(position)).1.is_none());
        assert!(!state.confirmed);
        state.confirmed = true;
        let (output, _) = render(&ctx, &mut state, vec![]);
        let position = button_center(&output, "Request updates in selected terminals");
        assert!(render(&ctx, &mut state, click(position)).1.is_none());
        assert!(state.confirming_update);
        for _ in 0..3 {
            assert!(render(&ctx, &mut state, vec![]).1.is_none());
        }
        let (output, _) = render(&ctx, &mut state, vec![]);
        let confirm = button_center(&output, "Update selected terminals");
        match render(&ctx, &mut state, click(confirm)).1 {
            Some(Action::Start { id, selected }) => {
                assert_eq!(id, "fixture-only-no-live-IPC");
                assert_eq!(selected, ["supported"]);
            }
            _ => panic!("only the modal confirmation should emit the selected fixture operation"),
        }
        assert!(!state.confirmed);
        assert!(!state.confirming_update);
        assert!(render(&ctx, &mut state, click(confirm)).1.is_none());
        // panel returns an action; no request(), session bus or native window is used.
    }

    #[test]
    fn modal_cancellation_and_background_clicks_do_not_update_or_refresh_terminals() {
        for dismissal in ["cancel", "escape", "enter", "background-refresh"] {
            let ctx = egui::Context::default();
            let mut state = state();
            state.confirmed = true;
            state.confirming_update = true;
            for _ in 0..3 {
                assert!(render(&ctx, &mut state, vec![]).1.is_none());
            }
            let (output, _) = render(&ctx, &mut state, vec![]);
            let events = match dismissal {
                "cancel" => click(button_center(&output, "Cancel")),
                "background-refresh" => click(button_center(&output, "Refresh preview")),
                _ => vec![egui::Event::Key {
                    key: if dismissal == "escape" {
                        egui::Key::Escape
                    } else {
                        egui::Key::Enter
                    },
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }],
            };
            assert!(render(&ctx, &mut state, events).1.is_none(), "{dismissal}");
            assert!(!state.confirmed, "{dismissal}");
            assert!(!state.confirming_update, "{dismissal}");
            assert_eq!(
                state.report.as_ref().unwrap().operation_id,
                "fixture-only-no-live-IPC"
            );
        }
    }

    #[test]
    fn closing_the_preview_discards_acknowledgement_and_pending_confirmation() {
        let mut state = state();
        state.confirmed = true;
        state.confirming_update = true;
        state.update_window_on_ready = true;
        let shared = Arc::new(Mutex::new(state));
        close_popup(&shared, &egui::Context::default());
        let state = shared.lock().unwrap();
        assert!(!state.open);
        assert!(!state.update_window_on_ready);
        assert!(!state.confirmed);
        assert!(!state.confirming_update);
        assert!(!state.worker_running);
    }

    #[test]
    fn empty_selection_and_in_progress_jobs_disable_restart_even_after_acknowledgement() {
        for running in [false, true] {
            let ctx = egui::Context::default();
            let mut state = state();
            state.confirmed = true;
            state.confirming_update = true;
            if running {
                state.worker_running = true;
                state.report.as_mut().unwrap().phase = TerminalUpgradePhase::Running;
            } else {
                state.selected.clear();
            }
            let (output, _) = render(&ctx, &mut state, vec![]);
            let position = button_center(&output, "Request updates in selected terminals");
            assert!(render(&ctx, &mut state, click(position)).1.is_none());
            assert!(!state.confirming_update);
        }
    }

    #[test]
    fn up_to_date_terminal_cannot_be_selected_or_restarted_even_with_stale_selection() {
        let ctx = egui::Context::default();
        let mut state = state();
        state.confirmed = true;
        state.confirming_update = true;
        let window = &mut state.report.as_mut().unwrap().windows[0];
        window.eligible = false;
        window.recommended = false;
        window.status = "up_to_date".into();
        window.message = "Already running the latest built binary; left untouched".into();
        let title = window.title.clone();
        let (output, action) = render(&ctx, &mut state, vec![]);
        assert!(action.is_none());
        assert!(state.selected.is_empty());
        let checkbox = button_center(&output, &title);
        assert!(render(&ctx, &mut state, click(checkbox)).1.is_none());
        assert!(state.selected.is_empty());
        let (output, _) = render(&ctx, &mut state, vec![]);
        let restart = button_center(&output, "Request updates in selected terminals");
        assert!(render(&ctx, &mut state, click(restart)).1.is_none());
    }

    #[test]
    fn this_window_preview_never_includes_another_preselected_terminal_in_the_request() {
        let ctx = egui::Context::default();
        let mut state = state();
        let mut other = state.report.as_ref().unwrap().windows[0].clone();
        other.window_id = "other".into();
        other.title = "Other terminal".into();
        state.report.as_mut().unwrap().windows.push(other);
        state.selected.insert("other".into());
        state.scope_window = Some("supported".into());
        state.confirmed = true;
        state.confirming_update = true;
        for _ in 0..3 {
            render(&ctx, &mut state, vec![]);
        }
        assert_eq!(state.selected, ["supported".into()].into_iter().collect());
        let (output, _) = render(&ctx, &mut state, vec![]);
        let confirm = button_center(&output, "Update selected terminals");
        match render(&ctx, &mut state, click(confirm)).1 {
            Some(Action::Start { selected, .. }) => assert_eq!(selected, ["supported"]),
            _ => panic!("expected only the selected fixture window"),
        }
    }
}

struct RequestGuard {
    shared: Arc<Mutex<TerminalMaintenanceState>>,
    ctx: egui::Context,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.lock() {
            state.worker_running = false;
            if std::thread::panicking() {
                state.update_window_on_ready = false;
                state.error = Some("Terminal maintenance request worker stopped unexpectedly. Check the daemon's report before retrying.".into());
            }
            if state.open {
                self.ctx.request_repaint_of(
                    state
                        .info_viewport
                        .unwrap_or_else(|| egui::ViewportId::from_hash_of(POPUP_ID)),
                );
            }
            if state.info_viewport.is_some() {
                state.open = false;
                state.update_window_on_ready = false;
            }
            self.ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
    }
}

fn request(shared: Arc<Mutex<TerminalMaintenanceState>>, ctx: egui::Context, action: Action) {
    {
        let Ok(mut state) = shared.lock() else {
            return;
        };
        if state.worker_running {
            return;
        }
        state.worker_running = true;
        state.error = None;
        if matches!(action, Action::Refresh) {
            state.report = None;
            state.selected.clear();
            state.confirmed = false;
            state.confirming_update = false;
        }
    }
    let guard = RequestGuard {
        shared: shared.clone(),
        ctx: ctx.clone(),
    };
    let worker_shared = shared.clone();
    if let Err(error) = std::thread::Builder::new().name("terminal-maintenance-client".into()).spawn(move || {
        let shared = worker_shared;
        let _guard = guard;
        #[cfg(feature = "observability")]
        let worker = applicationlauncher::observability::register_worker("terminal-maintenance-client");
        applicationlauncher::observe! { worker.set_state("requesting-preview-or-restart"); }
        let result = (|| {
            let client = TrackerClient::connect()?;
            let mut report = match action {
                Action::Refresh => {
                    let id = client.begin_terminal_upgrade_preview()?;
                    client.terminal_upgrade_report(&id)?
                }
                Action::Start { id, selected } => client.start_terminal_upgrade(&id, &selected, true)?,
                Action::Poll { id } => client.terminal_upgrade_report(&id)?,
                Action::Close => return Ok::<_, String>(()),
            };
            loop {
                let running = matches!(report.phase, TerminalUpgradePhase::Preparing | TerminalUpgradePhase::Running);
                let (open, automatic, info) = {
                    let mut state = shared.lock().map_err(|_| "Terminal maintenance popup state is unavailable")?;
                    if report.phase == TerminalUpgradePhase::Ready && state.report.as_ref().is_none_or(|previous| previous.phase != TerminalUpgradePhase::Ready) {
                        state.selected = report.windows.iter().filter(|window| window.eligible && window.recommended
                            && state.scope_window.as_ref().is_none_or(|scope| scope == &window.window_id)).map(|window| window.window_id.clone()).collect();
                        state.confirmed = false;
                    }
                    state.report = Some(report.clone());
                    let automatic = automatic_window_update(&mut state)?;
                    (state.open, automatic, state.info_viewport)
                };
                if open { ctx.request_repaint_of(info.unwrap_or_else(|| egui::ViewportId::from_hash_of(POPUP_ID))); }
                if let Some(Action::Start { id, selected }) = automatic {
                    report = client.start_terminal_upgrade(&id, &selected, true)?;
                    continue;
                }
                if !running || !open { break; }
                applicationlauncher::observe! { worker.set_state("polling-daemon-progress"); }
                std::thread::sleep(Duration::from_millis(300));
                report = client.terminal_upgrade_report(&report.operation_id)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            if let Ok(mut state) = shared.lock() {
                state.update_window_on_ready = false;
                state.error = Some(if state.info_viewport.is_some() { error } else {
                    format!("Terminal maintenance unavailable: {error}. If the daemon lacks this API, rebuild both binaries and relaunch normally.")
                });
            }
        }
    }) {
        if let Ok(mut state) = shared.lock() {
            state.update_window_on_ready = false;
            state.error = Some(format!("Could not start terminal maintenance request: {error}"));
        }
    }
}
