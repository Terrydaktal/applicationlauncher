use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use applicationlauncher::tracker::{
    ApplicationUpdateAction, ApplicationUpdateReport, ProgramUpdateIdentity, TerminalUpgradePhase,
    TrackerClient,
};
use eframe::egui;

use crate::models::{InfoProgram, ProcessIdentity};

struct State {
    window: String,
    process: ProcessIdentity,
    application: InfoProgram,
    open: bool,
    busy: bool,
    accepted: bool,
    report: Option<ApplicationUpdateReport>,
    error: Option<String>,
}

type Shared = Arc<Mutex<State>>;

fn state_id(popup: egui::ViewportId) -> egui::Id {
    popup.0.with("single-program-update")
}

pub(super) fn open(
    ctx: &egui::Context,
    popup: egui::ViewportId,
    window: String,
    process: ProcessIdentity,
    application: InfoProgram,
) {
    if window.is_empty() || process.pid <= 1 || process.start_ticks == 0 {
        return;
    }
    if let Some(previous) = ctx.data(|data| data.get_temp::<Shared>(state_id(popup))) {
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
                process,
                application,
                open: true,
                busy: false,
                accepted: false,
                report: None,
                error: None,
            })),
        )
    });
    ctx.request_repaint();
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Preview,
    Start(String),
    Poll(String),
    Close,
}

fn verified_action(state: &State) -> Result<&ApplicationUpdateAction, &str> {
    let report = state
        .report
        .as_ref()
        .ok_or("Checking this exact process; no application has been stopped.")?;
    if report.window_id != state.window {
        return Err("The preview belongs to another window. Reopen Show Info.");
    }
    let mut actions = report
        .actions
        .iter()
        .filter(|action| action.id == "program");
    let action = actions
        .next()
        .ok_or("No safe restart is available for this process.")?;
    if actions.next().is_some() {
        return Err("The process restart scope is ambiguous; nothing will be restarted.");
    }
    if !action.eligible {
        return Err(&action.message);
    }
    let scope = action.program.as_ref().ok_or("The daemon did not provide the exact program identity. Rebuild both binaries and reopen Show Info.")?;
    if scope.identity.pid != state.process.pid
        || scope.identity.start_ticks != state.process.start_ticks
        || scope.identity.application != state.application.name()
    {
        return Err(
            "The selected process changed. Reopen Show Info; no other process will be restarted.",
        );
    }
    if !Path::new(&scope.executable).is_absolute() || !Path::new(&scope.cwd).is_absolute() {
        return Err("The process restart recipe is incomplete; nothing will be restarted.");
    }
    match (&scope.tmux_pane, &scope.tmux_socket) {
        (None, None) => {}
        (Some(pane), Some(socket))
            if Path::new(socket).is_absolute()
                && pane.strip_prefix('%').is_some_and(|digits| {
                    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
                }) => {}
        _ => return Err("The tmux pane identity is incomplete; nothing will be restarted."),
    }
    Ok(action)
}

fn can_start(state: &State, id: &str) -> bool {
    state.open
        && !state.busy
        && state.accepted
        && state.error.is_none()
        && state.report.as_ref().is_some_and(|report| {
            report.phase == TerminalUpgradePhase::Ready && report.operation_id == id
        })
        && verified_action(state).is_ok()
}

fn details(ui: &mut egui::Ui, state: &State) {
    ui.heading(format!("Update {}", state.application.name()));
    ui.label(format!(
        "{} (pid {})",
        state.application.name(),
        state.process.pid
    ));
    ui.label(concat!(
        "Restart only this program using its latest installed build, in the same terminal tab or tmux pane. ",
        "The terminal window, tmux server and other panes keep running. ",
        "This does not build, install or download an update."
    ));
    ui.label(match state.application {
        InfoProgram::Codex => "Codex resumes this exact conversation, never --last. Running work is interrupted.",
        InfoProgram::Fish => concat!(
            "Only an idle Fish shell can be restarted. A foreground application or background job blocks the update; ",
            "shell-local state is not preserved."
        ),
    });
    if let Some(report) = &state.report {
        ui.separator();
        ui.label(&report.message);
        if let Some(path) = &report.backup_path {
            ui.monospace(path);
        }
        if report.phase == TerminalUpgradePhase::Ready {
            match verified_action(state) {
                Ok(action) => {
                    let scope = action.program.as_ref().unwrap();
                    ui.monospace(&scope.executable);
                    ui.label(format!("Working directory: {}", scope.cwd));
                    if let Some(pane) = &scope.tmux_pane {
                        ui.label(format!("tmux pane: {pane} (server is not restarted)"));
                    }
                }
                Err(error) => {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }
            }
        }
        if report.phase == TerminalUpgradePhase::Complete {
            ui.small("Reopen Show Info to capture the restarted process.");
        }
    } else {
        ui.label("Checking this exact process; no application has been stopped.");
    }
    if let Some(error) = &state.error {
        ui.colored_label(egui::Color32::LIGHT_RED, error);
    }
    if state.busy {
        ui.spinner();
        ui.small("Closing this dialog does not cancel a restart already confirmed.");
    }
}

fn dialog(ctx: &egui::Context, popup: egui::ViewportId, state: &mut State) -> Option<Action> {
    let response = egui::Modal::new(state_id(popup)).show(ctx, |ui| {
        ui.set_width((ctx.content_rect().width() - 48.0).clamp(1.0, 540.0));
        egui::ScrollArea::vertical()
            .id_salt("program-update-details")
            .max_height((ctx.content_rect().height() - 140.0).max(60.0))
            .show(ui, |ui| details(ui, state));
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
        let consent = ui.add_enabled(
            ready,
            egui::Checkbox::new(
                &mut state.accepted,
                "I accept restarting only this program and interrupting its current work.",
            ),
        );
        ui.horizontal_wrapped(|ui| {
            let close = ui.button("Close");
            let update = ui.add_enabled(
                ready && state.accepted,
                egui::Button::new(format!("Update {} only", state.application.name())),
            );
            if ![close.id, update.id, consent.id]
                .iter()
                .any(|id| ctx.memory(|memory| memory.focused()) == Some(*id))
            {
                close.request_focus();
            }
            if close.clicked() {
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
        let action = dialog(ctx, popup, &mut state);
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
                state.error = Some("The request worker stopped unexpectedly. Check the retained daemon report before retrying.".into());
            }
        }
        self.ctx.request_repaint_of(self.popup);
    }
}

fn request(shared: Shared, ctx: egui::Context, popup: egui::ViewportId, action: Action) {
    let (window, target) = {
        let mut state = shared.lock().unwrap();
        if state.busy || !state.open {
            return;
        }
        if let Action::Start(id) = &action {
            if !can_start(&state, id) {
                return;
            }
        }
        state.busy = true;
        state.accepted = false;
        (
            state.window.clone(),
            ProgramUpdateIdentity {
                pid: state.process.pid,
                start_ticks: state.process.start_ticks,
                application: state.application.name().into(),
            },
        )
    };
    let guard = RequestGuard {
        shared: shared.clone(),
        ctx: ctx.clone(),
        popup,
    };
    let worker_shared = shared.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("single-program-update-client".into())
        .spawn(move || {
            let _guard = guard;
            #[cfg(feature = "observability")]
            let worker =
                applicationlauncher::observability::register_worker("single-program-update-client");
            applicationlauncher::observe! { worker.set_state("requesting-exact-process-update"); }
            let result = (|| {
                let client = TrackerClient::connect()?;
                let mut report = match action {
                    Action::Preview => {
                        let id = client.begin_process_update_preview(&window, &target)?;
                        client.application_update_report(&id)?
                    }
                    Action::Start(id) => client.start_application_update(&id, "program", true)?,
                    Action::Poll(id) => client.application_update_report(&id)?,
                    Action::Close => return Ok::<_, String>(()),
                };
                loop {
                    let running = matches!(
                        report.phase,
                        TerminalUpgradePhase::Preparing | TerminalUpgradePhase::Running
                    );
                    let open = {
                        let mut state = worker_shared
                            .lock()
                            .map_err(|_| "Program update dialog state unavailable")?;
                        state.report = Some(report.clone());
                        state.open
                    };
                    ctx.request_repaint_of(popup);
                    if !running || !open {
                        break;
                    }
                    applicationlauncher::observe! { worker.set_state("polling-exact-process-update"); }
                    std::thread::sleep(Duration::from_millis(300));
                    report = client.application_update_report(&report.operation_id)?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                if let Ok(mut state) = worker_shared.lock() {
                    state.error = Some(format!(
                        "Program update unavailable: {error}. No automatic retry will be attempted."
                    ));
                }
            }
        })
    {
        if let Ok(mut state) = shared.lock() {
            state.error = Some(format!("Could not start program update request: {error}"));
        }
    }
}

#[cfg(test)]
mod tests;
