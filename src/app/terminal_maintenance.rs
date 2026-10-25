use std::sync::{Arc, Mutex};
use std::time::Duration;

use applicationlauncher::tracker::{TerminalUpgradePhase, TrackerClient};
use eframe::egui;

use super::App;
use crate::models::TerminalMaintenanceState;

const POPUP_ID: &str = "terminal_maintenance_popup";

pub(crate) fn settings_button(ui: &mut egui::Ui, shared: &Arc<Mutex<TerminalMaintenanceState>>) {
    if ui.button("Update outdated terminal windows...").clicked() {
        if let Ok(mut state) = shared.lock() {
            state.open = true;
        }
        ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
    }
    ui.small("Preview, back up and restart only outdated XFCE4 Terminal windows. Windows already running their latest built binary are left untouched.");
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
                if ctx.input(|input| {
                    input.viewport().close_requested() || input.key_pressed(egui::Key::Escape)
                }) {
                    close_popup(&shared, ctx);
                    return;
                }
                let mut action = None;
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().inner_margin(12))
                    .show(ctx, |ui| {
                        if let Ok(mut state) = shared.lock() {
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

fn panel(ui: &mut egui::Ui, state: &mut TerminalMaintenanceState) -> Option<Action> {
    let mut action = None;
    ui.heading("Update Terminal Windows");
    ui.label("Each selected terminal manages its own update, confirmation, recovery commands and KWin placement. Application Launcher only requests the update and observes its result.");
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
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!state.worker_running, egui::Button::new("Refresh preview"))
            .clicked()
        {
            action = Some(Action::Refresh);
        }
        if ui
            .add_enabled(
                ready && state.confirmed && !state.selected.is_empty(),
                egui::Button::new("Request updates in selected terminals"),
            )
            .clicked()
        {
            action = state.report.as_ref().map(|report| Action::Start {
                id: report.operation_id.clone(),
                selected: state.selected.iter().cloned().collect(),
            });
        }
        if ui.button("Close").clicked() {
            action = Some(Action::Close);
        }
    });
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
        match render(&ctx, &mut state, click(position)).1 {
            Some(Action::Start { id, selected }) => {
                assert_eq!(id, "fixture-only-no-live-IPC");
                assert_eq!(selected, ["supported"]);
            }
            _ => panic!("confirmed button should emit only the selected fixture operation"),
        }
        // panel returns an action; no request(), session bus or native window is used.
    }

    #[test]
    fn empty_selection_and_in_progress_jobs_disable_restart_even_after_acknowledgement() {
        for running in [false, true] {
            let ctx = egui::Context::default();
            let mut state = state();
            state.confirmed = true;
            if running {
                state.worker_running = true;
                state.report.as_mut().unwrap().phase = TerminalUpgradePhase::Running;
            } else {
                state.selected.clear();
            }
            let (output, _) = render(&ctx, &mut state, vec![]);
            let position = button_center(&output, "Request updates in selected terminals");
            assert!(render(&ctx, &mut state, click(position)).1.is_none());
        }
    }

    #[test]
    fn up_to_date_terminal_cannot_be_selected_or_restarted_even_with_stale_selection() {
        let ctx = egui::Context::default();
        let mut state = state();
        state.confirmed = true;
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
                state.error = Some("Terminal maintenance request worker stopped unexpectedly. Check the daemon's report before retrying.".into());
            }
            if state.open {
                self.ctx
                    .request_repaint_of(egui::ViewportId::from_hash_of(POPUP_ID));
            }
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
        let worker = applicationlauncher::observability::register_worker("terminal-maintenance-client");
        worker.set_state("requesting-preview-or-restart");
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
                let open = {
                    let mut state = shared.lock().map_err(|_| "Terminal maintenance popup state is unavailable")?;
                    if report.phase == TerminalUpgradePhase::Ready && state.report.as_ref().is_none_or(|previous| previous.phase != TerminalUpgradePhase::Ready) {
                        state.selected = report.windows.iter().filter(|window| window.eligible && window.recommended).map(|window| window.window_id.clone()).collect();
                        state.confirmed = false;
                    }
                    state.report = Some(report.clone());
                    state.open
                };
                if open { ctx.request_repaint_of(egui::ViewportId::from_hash_of(POPUP_ID)); }
                if !running || !open { break; }
                worker.set_state("polling-daemon-progress");
                std::thread::sleep(Duration::from_millis(300));
                report = client.terminal_upgrade_report(&report.operation_id)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            if let Ok(mut state) = shared.lock() {
                state.error = Some(format!("Terminal maintenance unavailable: {error}. If the daemon lacks this API, rebuild both binaries and relaunch normally."));
            }
        }
    }) {
        if let Ok(mut state) = shared.lock() { state.error = Some(format!("Could not start terminal maintenance request: {error}")); }
    }
}
