use std::sync::{Arc, Mutex};
use std::time::Duration;

use applicationlauncher::tracker::{
    ApplicationRestoreStatus, ApplicationUpdateAction, ApplicationUpdateReport,
    TerminalUpgradePhase, TmuxServerIdentity, TrackerClient,
};
use eframe::egui;

struct State {
    window: String,
    server: TmuxServerIdentity,
    open: bool,
    busy: bool,
    accepted: bool,
    started: bool,
    report: Option<ApplicationUpdateReport>,
    error: Option<String>,
}

type Shared = Arc<Mutex<State>>;

fn state_id(popup: egui::ViewportId) -> egui::Id {
    popup.0.with("tmux-server-update")
}

pub(super) fn open(
    ctx: &egui::Context,
    popup: egui::ViewportId,
    window: String,
    server: TmuxServerIdentity,
) {
    let previous = ctx.data(|data| data.get_temp::<Shared>(state_id(popup)));
    if let Some(previous) = previous {
        let mut state = previous.lock().unwrap();
        if state.busy
            || state
                .report
                .as_ref()
                .is_some_and(|report| report.phase == TerminalUpgradePhase::Running)
        {
            state.open = true;
            ctx.request_repaint();
            return;
        }
    }
    ctx.data_mut(|data| {
        data.insert_temp(
            state_id(popup),
            Arc::new(Mutex::new(State {
                window,
                server,
                open: true,
                busy: false,
                accepted: false,
                started: false,
                report: None,
                error: None,
            })),
        );
    });
    // Do not let the click on the inline link also confirm the new modal.
    ctx.request_repaint();
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Preview,
    Poll(String),
    Start(String),
    Close,
}

fn verified_action(state: &State) -> Result<&ApplicationUpdateAction, &str> {
    let report = state
        .report
        .as_ref()
        .ok_or("Inspecting server restart capabilities...")?;
    if report.window_id != state.window {
        return Err("The preview belongs to a different window; reopen Show Info.");
    }
    let mut actions = report
        .actions
        .iter()
        .filter(|action| action.id == "tmux_server");
    let action = actions
        .next()
        .ok_or("No safe tmux server update is available from this terminal tab.")?;
    if actions.next().is_some() {
        return Err("Ambiguous tmux server update preview; nothing will be restarted.");
    }
    if !action.eligible {
        return Err(&action.message);
    }
    let scope = action.tmux_server.as_ref().ok_or("The daemon did not provide a verified server scope. Rebuild both launcher and daemon and reopen Show Info.")?;
    if scope.identity != state.server {
        return Err(
            "The terminal now points to a different tmux server. Reopen Show Info; no sessions have been stopped.",
        );
    }
    if scope.sessions.is_empty() || scope.panes.is_empty() || scope.executable.is_empty() {
        return Err("The server restart preview is incomplete; nothing will be restarted.");
    }
    if scope.remote_clients.iter().any(|client| {
        client.pid <= 1 || client.transport != "mosh" || !scope.sessions.contains(&client.session)
    }) || scope
        .remote_clients
        .iter()
        .map(|client| client.pid)
        .collect::<std::collections::HashSet<_>>()
        .len()
        != scope.remote_clients.len()
    {
        return Err(
            "The remote-client disconnect preview is incomplete or ambiguous; nothing will be restarted.",
        );
    }
    Ok(action)
}

fn preview(ui: &mut egui::Ui, state: &State) {
    ui.label(format!("tmux-server (pid {})", state.server.pid));
    ui.monospace(&state.server.socket_path);
    ui.add_space(8.0);
    ui.label("This replaces the server with the latest installed tmux binary. It stops ALL sessions on this server, recreates their windows and panes, and reopens their applications in the same working directories.");
    ui.label("Local clients reconnect in the SAME existing terminal windows and tabs. Detached sessions remain detached. Remote mosh tmux clients are disconnected, not reattached automatically. Other tmux servers and terminal windows are not updated.");
    ui.colored_label(egui::Color32::LIGHT_RED, "Running work is interrupted. Scrollback, unsaved shell state and runtime-only tmux options are not preserved. This cannot be undone.");
    ui.small("Nothing is stopped unless every application and local terminal attachment has a verified restart plan and every disconnected remote client has verified mosh ancestry. A private recovery plan is saved before shutdown.");
    ui.separator();
    if let Some(error) = &state.error {
        ui.colored_label(egui::Color32::LIGHT_RED, error);
    }
    if state.busy {
        ui.spinner();
    }
    if let Some(report) = &state.report {
        ui.label(&report.message);
        if let Some(path) = &report.backup_path {
            ui.label("Recovery plan:");
            ui.monospace(path);
        }
        if report.phase == TerminalUpgradePhase::Ready {
            match verified_action(state) {
                Ok(action) => {
                    let scope = action.tmux_server.as_ref().unwrap();
                    ui.label(format!(
                        "{} sessions, {} tmux windows, {} panes, {} attached terminal tabs",
                        scope.sessions.len(),
                        scope.windows,
                        scope.panes.len(),
                        scope.terminal_tabs
                    ));
                    ui.label(format!("Replacement: {}", scope.executable));
                    if !scope.remote_clients.is_empty() {
                        ui.colored_label(egui::Color32::LIGHT_RED, format!(
                            "{} remote tmux client(s) will be disconnected. Reattach manually after the update; their sessions are recreated but remain detached unless a local client also reconnects.",
                            scope.remote_clients.len()
                        ));
                        for client in &scope.remote_clients {
                            ui.label(format!(
                                "Disconnect: {} ({}; tmux client PID {})",
                                client.session, client.transport, client.pid
                            ));
                        }
                    }
                    for session in &scope.sessions {
                        ui.strong(format!("Session: {session}"));
                        for pane in scope.panes.iter().filter(|pane| &pane.session == session) {
                            ui.label(format!(
                                "  {}.{}: {} - {}",
                                pane.window_index, pane.pane_index, pane.application, pane.cwd
                            ));
                        }
                    }
                }
                Err(error) => {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }
            }
        }
    } else {
        ui.label(
            "Inspecting every session and terminal attachment. No applications have been stopped.",
        );
    }
}

#[derive(Debug, PartialEq, Eq)]
enum View {
    Preview,
    Progress,
    Result,
}

fn view(state: &State) -> View {
    if state.error.is_some()
        || state.report.as_ref().is_some_and(|report| {
            matches!(
                report.phase,
                TerminalUpgradePhase::Complete | TerminalUpgradePhase::Failed
            )
        })
    {
        View::Result
    } else if state.started
        || state
            .report
            .as_ref()
            .is_some_and(|report| report.phase == TerminalUpgradePhase::Running)
    {
        View::Progress
    } else {
        View::Preview
    }
}

fn status_heading(state: &State) -> (&'static str, egui::Color32) {
    match state.report.as_ref() {
        Some(report) if report.phase == TerminalUpgradePhase::Complete => {
            if report.restoration.as_ref().is_some_and(|status| {
                status
                    .panes
                    .iter()
                    .any(|pane| matches!(pane.status, ApplicationRestoreStatus::Failed))
            }) {
                ("Tmux server updated with warnings", egui::Color32::YELLOW)
            } else {
                ("Tmux server update complete", egui::Color32::LIGHT_GREEN)
            }
        }
        Some(report) if report.phase == TerminalUpgradePhase::Failed => {
            ("Tmux server update failed", egui::Color32::LIGHT_RED)
        }
        _ => ("Tmux update status unavailable", egui::Color32::YELLOW),
    }
}

fn status(ui: &mut egui::Ui, state: &State, running: bool) {
    if running {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("The update is running. Closing this status window does not cancel it.");
        });
    }
    if let Some(error) = &state.error {
        ui.colored_label(egui::Color32::LIGHT_RED, error);
    }
    let Some(report) = &state.report else { return };
    if let Some(restoration) = &report.restoration {
        if restoration.server_replaced {
            if report.phase == TerminalUpgradePhase::Failed {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "Server: replacement was started; inspect the failure below",
                );
            } else {
                ui.colored_label(egui::Color32::LIGHT_GREEN, "Server: replacement verified");
            }
            ui.label(format!(
                "Local terminal tabs reattached: {}",
                restoration.clients_reattached
            ));
        }
        let restored = restoration
            .panes
            .iter()
            .filter(|pane| matches!(pane.status, ApplicationRestoreStatus::Restored))
            .count();
        let failed = restoration
            .panes
            .iter()
            .filter(|pane| matches!(pane.status, ApplicationRestoreStatus::Failed))
            .count();
        let pending = restoration.panes.len() - restored - failed;
        ui.strong(format!("Applications: {restored} restored, {failed} need attention, {pending} awaiting verification"));
        if failed > 0 {
            ui.colored_label(egui::Color32::YELLOW, "Recover only the applications listed below as needing attention. Do not repeat the server update; working sessions remain running.");
        }
        ui.separator();
        let panes = [
            ApplicationRestoreStatus::Failed,
            ApplicationRestoreStatus::Pending,
            ApplicationRestoreStatus::Restored,
        ]
        .into_iter()
        .flat_map(|status| {
            restoration
                .panes
                .iter()
                .filter(move |pane| pane.status == status)
        });
        for pane in panes {
            let (label, colour) = match pane.status {
                ApplicationRestoreStatus::Pending => ("Waiting", egui::Color32::GRAY),
                ApplicationRestoreStatus::Restored => ("Restored", egui::Color32::LIGHT_GREEN),
                ApplicationRestoreStatus::Failed => ("Needs attention", egui::Color32::LIGHT_RED),
            };
            ui.colored_label(
                colour,
                format!(
                    "{label}: {} - {}.{} - {}",
                    pane.session, pane.window_index, pane.pane_index, pane.application
                ),
            );
            if !matches!(pane.status, ApplicationRestoreStatus::Restored) {
                ui.label(&pane.message);
                ui.monospace(&pane.cwd);
            }
        }
    } else if running {
        ui.label(
            "Replacing the tmux server, rebuilding sessions and reconnecting terminal tabs...",
        );
    }
    ui.separator();
    ui.label(&report.message);
    if let Some(path) = &report.backup_path {
        ui.collapsing("Recovery details", |ui| {
            ui.label("Saved recovery plan:");
            ui.monospace(path);
        });
    }
}

fn dialog(ctx: &egui::Context, id: egui::Id, state: &mut State) -> Option<Action> {
    let response = egui::Modal::new(id).show(ctx, |ui| {
        ui.set_width((ctx.content_rect().width() - 48.0).clamp(1.0, 620.0));
        let view = view(state);
        match view {
            View::Preview => { ui.heading("Update tmux server"); }
            View::Progress => { ui.heading("Tmux server update progress"); }
            View::Result => {
                let (heading, colour) = status_heading(state);
                ui.heading(egui::RichText::new(heading).color(colour));
            }
        }
        egui::ScrollArea::vertical()
            .id_salt(if view == View::Preview { "server-restart-preview" } else { "server-restart-status" })
            .max_height((ctx.content_rect().height() - 170.0).max(60.0))
            .show(ui, |ui| match view {
                View::Preview => preview(ui, state),
                View::Progress => status(ui, state, true),
                View::Result => status(ui, state, false),
            });
        if view != View::Preview {
            state.accepted = false;
            if state.error.is_some() && !state.busy && state.started {
                if let Some(report) = &state.report {
                    if ui.button("Refresh status").clicked() {
                        return Some(Action::Poll(report.operation_id.clone()));
                    }
                }
            }
            let close = ui.button("Close");
            if ctx.memory(|memory| memory.focused()) != Some(close.id) {
                close.request_focus();
            }
            return close.clicked().then_some(Action::Close);
        }
        let ready = !state.busy
            && state.error.is_none()
            && state
                .report
                .as_ref()
                .is_some_and(|report| report.phase == TerminalUpgradePhase::Ready)
            && verified_action(state).is_ok();
        if !ready {
            state.accepted = false;
        }
        ui.add_space(8.0);
        ui.add_enabled(
            ready,
            egui::Checkbox::new(
                &mut state.accepted,
                "I accept restarting every session, interrupting its applications and disconnecting remote clients.",
            ),
        );
        ui.horizontal_wrapped(|ui| {
            let cancel = ui.button("Close");
            let update = ui.add_enabled(
                ready && state.accepted,
                egui::Button::new("Update server and restart all sessions"),
            );
            if ctx.memory(|memory| memory.focused()).is_none() {
                cancel.request_focus();
            }
            if cancel.clicked() {
                Some(Action::Close)
            } else if update.clicked() {
                Some(Action::Start(
                    state.report.as_ref().unwrap().operation_id.clone(),
                ))
            } else {
                None
            }
        })
        .inner
    });
    if response.should_close() {
        Some(Action::Close)
    } else {
        response.inner
    }
}

pub(super) fn show(ctx: &egui::Context, popup: egui::ViewportId) -> bool {
    let Some(shared) = ctx.data(|data| data.get_temp::<Shared>(state_id(popup))) else {
        return false;
    };
    let action = {
        let mut state = shared.lock().unwrap();
        if !state.open {
            return false;
        }
        let action = dialog(ctx, state_id(popup), &mut state);
        if action == Some(Action::Close) {
            state.open = false;
            state.accepted = false;
            ctx.request_repaint();
            return true;
        }
        action.or_else(|| {
            if state.busy || state.error.is_some() {
                return None;
            }
            match &state.report {
                None => Some(Action::Preview),
                Some(report)
                    if matches!(
                        report.phase,
                        TerminalUpgradePhase::Preparing | TerminalUpgradePhase::Running
                    ) =>
                {
                    Some(Action::Poll(report.operation_id.clone()))
                }
                _ => None,
            }
        })
    };
    if let Some(action) = action {
        request(shared, ctx.clone(), popup, action);
    }
    true
}

struct RequestGuard {
    shared: Shared,
    ctx: egui::Context,
    popup: egui::ViewportId,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.lock() {
            state.busy = false;
            if std::thread::panicking() {
                state.error = Some("The request worker stopped unexpectedly. Check the daemon report before retrying.".into());
            }
        }
        self.ctx.request_repaint_of(self.popup);
    }
}

fn request(shared: Shared, ctx: egui::Context, popup: egui::ViewportId, action: Action) {
    let window = {
        let mut state = shared.lock().unwrap();
        if state.busy || !state.open {
            return;
        }
        if let Action::Start(id) = &action {
            if !state.accepted
                || state.error.is_some()
                || verified_action(&state).is_err()
                || !state.report.as_ref().is_some_and(|report| {
                    report.phase == TerminalUpgradePhase::Ready && &report.operation_id == id
                })
            {
                return;
            }
            state.started = true;
        }
        state.busy = true;
        state.accepted = false;
        state.error = None;
        state.window.clone()
    };
    let guard = RequestGuard {
        shared: shared.clone(),
        ctx: ctx.clone(),
        popup,
    };
    let worker_shared = shared.clone();
    if let Err(error) = std::thread::Builder::new().name("tmux-server-update-client".into()).spawn(move || {
        let _guard = guard;
        #[cfg(feature = "observability")]
        let worker = applicationlauncher::observability::register_worker("tmux-server-update-client");
        applicationlauncher::observe! { worker.set_state("requesting-preview-or-server-update"); }
        let result = (|| {
            let client = TrackerClient::connect()?;
            let mut report = match action {
                Action::Preview => {
                    let id = client.begin_application_update_preview(&window)?;
                    client.application_update_report(&id)?
                }
                Action::Start(id) => client.start_application_update(&id, "tmux_server", true)?,
                Action::Poll(id) => client.application_update_report(&id)?,
                Action::Close => return Ok::<_, String>(()),
            };
            loop {
                let running = matches!(report.phase, TerminalUpgradePhase::Preparing | TerminalUpgradePhase::Running);
                let open = {
                    let mut state = worker_shared.lock().map_err(|_| "Server update dialog state unavailable")?;
                    if report.phase == TerminalUpgradePhase::Ready && report.selected_action.is_none() {
                        state.started = false;
                    }
                    state.report = Some(report.clone());
                    state.open
                };
                ctx.request_repaint_of(popup);
                if !running || !open { break; }
                applicationlauncher::observe! { worker.set_state("polling-daemon-progress"); }
                std::thread::sleep(Duration::from_millis(300));
                report = client.application_update_report(&report.operation_id)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            if let Ok(mut state) = worker_shared.lock() {
                state.error = Some(format!("Tmux server update unavailable: {error}. No automatic retry will be attempted."));
            }
        }
    }) {
        if let Ok(mut state) = shared.lock() { state.error = Some(format!("Could not start server update request: {error}")); }
    }
}

#[cfg(test)]
mod tests;
