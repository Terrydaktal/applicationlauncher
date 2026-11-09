use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use eframe::egui;

use crate::models::ProcessIdentity;

const SERVICE: &str = "app-com.github.hluk.copyq@autostart.service";
const RESTART: &str = "Restart CopyQ";

struct State {
    process: ProcessIdentity,
    busy: bool,
    result: Option<Result<(), String>>,
}

type Shared = Arc<Mutex<State>>;

fn state_id(popup: egui::ViewportId) -> egui::Id {
    popup.0.with("copyq-service-update")
}

pub(super) fn open(ctx: &egui::Context, popup: egui::ViewportId, process: ProcessIdentity) {
    let previous = ctx.data(|data| data.get_temp::<Shared>(state_id(popup)));
    if previous.is_some_and(|state| state.lock().unwrap().busy) {
        ctx.request_repaint();
        return;
    }
    ctx.data_mut(|data| {
        data.insert_temp(
            state_id(popup),
            Arc::new(Mutex::new(State {
                process,
                busy: false,
                result: None,
            })),
        );
    });
    // The link opens the modal; it cannot also confirm the restart.
    ctx.request_repaint();
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Restart,
    Close,
}

fn dialog(ctx: &egui::Context, popup: egui::ViewportId, state: &State) -> Option<Action> {
    let response = egui::Modal::new(state_id(popup)).show(ctx, |ui| {
        ui.set_width((ctx.content_rect().width() - 48.0).clamp(1.0, 520.0));
        ui.heading("Update CopyQ");
        ui.label(format!("Main CopyQ process: {}", state.process.pid));
        ui.label("Restart the user autostart service to load the installed CopyQ application. CopyQ will briefly close and reopen; running CopyQ commands will be interrupted. This does not build or install an update.");
        ui.monospace(format!("systemctl --user restart {SERVICE}"));
        if state.busy {
            ui.horizontal(|ui| { ui.spinner(); ui.label("Restarting CopyQ..."); });
            ui.small("Closing this dialog does not cancel a restart already requested.");
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        match &state.result {
            Some(Ok(())) => { ui.label("CopyQ's user service restarted successfully."); }
            Some(Err(error)) => { ui.colored_label(egui::Color32::LIGHT_RED, error); }
            None => {}
        }
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let close = ui.button(if state.busy || state.result.is_some() { "Close" } else { "Cancel" });
            let restart = ui.add_enabled(!state.busy && state.result.is_none(), egui::Button::new(RESTART));
            let focused = ctx.memory(|memory| memory.focused());
            if ![close.id, restart.id].iter().any(|id| focused == Some(*id)) {
                close.request_focus();
            }
            if close.clicked() { Some(Action::Close) }
            else if restart.clicked() { Some(Action::Restart) }
            else { None }
        }).inner
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
    let action = dialog(ctx, popup, &shared.lock().unwrap());
    match action {
        Some(Action::Close) => {
            ctx.data_mut(|data| data.remove::<Shared>(state_id(popup)));
            ctx.request_repaint();
        }
        Some(Action::Restart) => {
            static IN_FLIGHT: OnceLock<Arc<AtomicBool>> = OnceLock::new();
            request(
                shared,
                ctx.clone(),
                popup,
                IN_FLIGHT
                    .get_or_init(|| Arc::new(AtomicBool::new(false)))
                    .clone(),
                restart,
            );
        }
        None => {}
    }
    true
}

fn restart_command() -> Command {
    let mut command = Command::new("/usr/bin/systemctl");
    command
        .args(["--user", "restart", SERVICE])
        .stdin(Stdio::null());
    command
}

fn restart(process: ProcessIdentity) -> Result<(), String> {
    if process.pid <= 1
        || process.start_ticks == 0
        || !crate::windows::process_tree::process_identity_is_current(process, None)
        || !std::fs::read_link(format!("/proc/{}/exe", process.pid)).is_ok_and(|path| {
            path.file_name()
                .is_some_and(|name| name == "copyq" || name == "copyq (deleted)")
        })
    {
        return Err("The inspected CopyQ process changed or exited. Reopen Show Info before restarting its service.".into());
    }
    let output = applicationlauncher::process::output_with_timeout(restart_command(), Duration::from_secs(120))
        .map_err(|error| if error.kind() == std::io::ErrorKind::TimedOut {
            "Timed out waiting for systemctl. The service restart may still be running; no automatic retry was attempted.".into()
        } else { format!("Could not restart CopyQ's user service: {error}") })?;
    if output.status.success() {
        return Ok(());
    }
    let detail = if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    let detail: String = String::from_utf8_lossy(detail).chars().take(2000).collect();
    Err(format!(
        "CopyQ's user service restart failed ({}): {}",
        output.status,
        detail.trim()
    ))
}

struct RequestGuard {
    shared: Shared,
    in_flight: Arc<AtomicBool>,
    ctx: egui::Context,
    popup: egui::ViewportId,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.lock() {
            state.busy = false;
            if state.result.is_none() {
                state.result = Some(Err(
                    "The restart worker stopped unexpectedly. Check the service before retrying."
                        .into(),
                ));
            }
        }
        self.in_flight.store(false, Ordering::Release);
        self.ctx.request_repaint_of(self.popup);
    }
}

fn request(
    shared: Shared,
    ctx: egui::Context,
    popup: egui::ViewportId,
    in_flight: Arc<AtomicBool>,
    execute: impl FnOnce(ProcessIdentity) -> Result<(), String> + Send + 'static,
) {
    let process = {
        let mut state = shared.lock().unwrap();
        if state.busy || state.result.is_some() {
            return;
        }
        if in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            state.result = Some(Err(
                "A CopyQ restart is already in progress. No second restart was queued.".into(),
            ));
            return;
        }
        state.busy = true;
        state.process
    };
    let guard = RequestGuard {
        shared: shared.clone(),
        in_flight,
        ctx,
        popup,
    };
    let result_state = shared.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("copyq-service-update".into())
        .spawn(move || {
            let _guard = guard;
            let worker =
                applicationlauncher::observability::register_worker("copyq-service-update");
            worker.set_state("restarting-user-autostart-service");
            let result = execute(process);
            if let Ok(mut state) = result_state.lock() {
                state.result = Some(result);
            }
        })
    {
        if let Ok(mut state) = shared.lock() {
            state.result = Some(Err(format!(
                "Could not start CopyQ restart worker: {error}"
            )));
        }
    }
}

#[cfg(test)]
mod tests;
