use eframe::egui;
use std::sync::{Arc, Mutex};

use crate::models::{InfoUpdateTarget, PopupEvent, TerminalMaintenanceState};

mod copyq;
mod program;
mod tmux;

#[derive(Clone)]
struct TerminalStatus {
    window: String,
    shared: Arc<Mutex<TerminalMaintenanceState>>,
    error: Option<String>,
}

fn terminal_status_id(popup: egui::ViewportId) -> egui::Id {
    popup.0.with("direct-terminal-update-status")
}

pub(super) fn bind_terminal_status(
    ctx: &egui::Context,
    info: egui::ViewportId,
    window: String,
    shared: Arc<Mutex<TerminalMaintenanceState>>,
    error: Option<String>,
) {
    ctx.data_mut(|data| {
        data.insert_temp(
            terminal_status_id(info),
            TerminalStatus {
                window,
                shared,
                error,
            },
        )
    });
}

pub(super) fn show_terminal_status(ui: &mut egui::Ui, info: egui::ViewportId) {
    use applicationlauncher::tracker::TerminalUpgradePhase;
    let id = terminal_status_id(info);
    let Some(status) = ui.ctx().data(|data| data.get_temp::<TerminalStatus>(id)) else {
        return;
    };
    let (message, failed, busy) = if let Some(error) = status.error {
        (error, true, false)
    } else {
        let Ok(state) = status.shared.lock() else {
            return;
        };
        if state.scope_window.as_ref() != Some(&status.window) || state.info_viewport != Some(info)
        {
            return;
        }
        let error = state.error.clone().or_else(|| {
            state
                .report
                .as_ref()
                .filter(|report| report.phase == TerminalUpgradePhase::Failed)
                .map(|report| report.message.clone())
        });
        let failed = error.is_some();
        let message =
            error.unwrap_or_else(|| match state.report.as_ref().map(|report| report.phase) {
                Some(TerminalUpgradePhase::Running) => "Updating this terminal in place...".into(),
                Some(TerminalUpgradePhase::Complete) => {
                    state.report.as_ref().unwrap().message.clone()
                }
                Some(TerminalUpgradePhase::Ready) => "Starting this terminal's update...".into(),
                _ => "Checking this terminal for an in-place update...".into(),
            });
        let busy = !failed
            && (state.worker_running
                || state.report.as_ref().is_none_or(|report| {
                    matches!(
                        report.phase,
                        TerminalUpgradePhase::Preparing | TerminalUpgradePhase::Running
                    )
                }));
        (message, failed, busy)
    };
    ui.horizontal_wrapped(|ui| {
        if busy {
            ui.spinner();
        }
        ui.colored_label(
            if failed {
                egui::Color32::LIGHT_RED
            } else {
                ui.visuals().text_color()
            },
            message,
        );
        if !busy && ui.small_button("Dismiss").clicked() {
            ui.ctx().data_mut(|data| data.remove::<TerminalStatus>(id));
        }
    });
    ui.add_space(6.0);
    if busy {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(200));
    }
}

pub(super) fn terminal_choice_is_open(ctx: &egui::Context, popup: egui::ViewportId) -> bool {
    ctx.data(|data| data.get_temp::<String>(popup.0.with("terminal-update-choice")))
        .is_some()
}

pub(super) fn show(
    ctx: &egui::Context,
    popup_id: egui::ViewportId,
    clicked_target: Option<InfoUpdateTarget>,
) -> Option<PopupEvent> {
    let id = popup_id.0.with("terminal-update-choice");
    if let Some(InfoUpdateTarget::Program {
        window,
        process,
        application,
    }) = clicked_target.as_ref()
    {
        ctx.data_mut(|data| data.remove::<String>(id));
        program::open(ctx, popup_id, window.clone(), *process, *application);
        return None;
    }
    if program::show(ctx, popup_id) {
        return None;
    }
    if let Some(InfoUpdateTarget::CopyQ(process)) = clicked_target.as_ref() {
        ctx.data_mut(|data| data.remove::<String>(id));
        copyq::open(ctx, popup_id, *process);
        return None;
    }
    if copyq::show(ctx, popup_id) {
        return None;
    }
    if let Some(InfoUpdateTarget::TmuxServer { window, server }) = clicked_target.as_ref() {
        ctx.data_mut(|data| data.remove::<String>(id));
        tmux::open(ctx, popup_id, window.clone(), server.clone());
        return None;
    }
    if tmux::show(ctx, popup_id) {
        return None;
    }
    if let Some(InfoUpdateTarget::TerminalWindow(window)) = clicked_target {
        ctx.data_mut(|data| data.insert_temp(id, window));
        // The opening click must not also choose a scope in the new dialog.
        ctx.request_repaint();
        return None;
    }
    let window = ctx.data(|data| data.get_temp::<String>(id))?;
    let response = egui::Modal::new(id).show(ctx, |ui| {
        ui.set_width((ctx.content_rect().width() - 48.0).clamp(1.0, 460.0));
        ui.heading("Update xfce4-terminal?");
        ui.add_space(8.0);
        ui.label("Use the latest installed build for just this window or all outdated xfce4-terminal windows?");
        ui.label("Each terminal is updated in place, keeping its screen position and size. tmux tabs reattach to their existing sessions; the tmux server and its applications keep running. In non-tmux tabs, supported applications are restarted in the same working directory.");
        let status_location = if popup_id == egui::ViewportId::ROOT { "the launcher" } else { "Show Info" };
        ui.small(format!("This window starts after safety checks, without another launcher confirmation; progress and errors stay in {status_location}. All outdated windows opens a preview for review. Up-to-date windows are left alone."));
        ui.small("Non-tmux programs restart; scrollback and unsaved terminal state may be lost. The terminal may request its own confirmation.");
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            let cancel = ui.button("Cancel");
            let this = ui.button("This window");
            let all = ui.button("All outdated windows");
            let focused = ctx.memory(|memory| memory.focused());
            if ![cancel.id, this.id, all.id].iter().any(|id| focused == Some(*id)) {
                cancel.request_focus();
            }
            if cancel.clicked() {
                Some(None)
            } else if this.clicked() {
                Some(Some(PopupEvent::UpdateTerminalWindow { window, info: popup_id }))
            } else if all.clicked() {
                Some(Some(PopupEvent::UpdateTerminalWindows { window: None }))
            } else {
                None
            }
        }).inner
    });
    if response.should_close() || response.inner.is_some() {
        ctx.data_mut(|data| data.remove::<String>(id));
        ctx.request_repaint();
    }
    if response.should_close() {
        None
    } else {
        response.inner.flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        ctx: &egui::Context,
        popup: egui::ViewportId,
        window: Option<&str>,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Option<PopupEvent>) {
        let mut event = None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |_| {});
                event = show(
                    ctx,
                    popup,
                    window.map(|window| InfoUpdateTarget::TerminalWindow(window.to_owned())),
                );
            },
        );
        (output, event)
    }

    fn button(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("expected scope choice button")
    }

    fn click(pos: egui::Pos2) -> Vec<egui::Event> {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            pressed,
            button: egui::PointerButton::Primary,
            modifiers: egui::Modifiers::NONE,
        };
        vec![egui::Event::PointerMoved(pos), button(true), button(false)]
    }

    fn settled(ctx: &egui::Context, popup: egui::ViewportId) -> egui::FullOutput {
        // Native egui areas have an initial invisible sizing pass.
        for _ in 0..2 {
            assert!(frame(ctx, popup, None, vec![]).1.is_none());
        }
        let (output, event) = frame(ctx, popup, None, vec![]);
        assert!(event.is_none());
        output
    }

    #[test]
    fn terminal_update_popup_explains_in_place_reattachment_and_application_restart() {
        let ctx = egui::Context::default();
        let popup = egui::ViewportId::from_hash_of("update-explanation-fixture");
        assert!(
            frame(&ctx, popup, Some("fixture-only-no-IPC"), vec![])
                .1
                .is_none()
        );
        let output = settled(&ctx, popup);
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
            "updated in place",
            "screen position and size",
            "tmux tabs reattach",
            "tmux server and its applications keep running",
            "non-tmux tabs",
            "restarted in the same working directory",
            "without another launcher confirmation",
            "All outdated windows opens a preview for review",
        ] {
            assert!(text.contains(expected), "missing {expected}");
        }
    }

    #[test]
    fn update_scope_choices_only_emit_a_scoped_request_after_an_explicit_choice() {
        for scale in [1.0, 2.0] {
            for label in ["This window", "All outdated windows"] {
                let ctx = egui::Context::default();
                ctx.set_pixels_per_point(scale);
                let popup = egui::ViewportId::from_hash_of("fixture-info-only");
                assert!(
                    frame(&ctx, popup, Some("fixture-original-window"), vec![])
                        .1
                        .is_none()
                );
                let output = settled(&ctx, popup);
                let pos = button(&output, label);
                assert!(frame(&ctx, popup, None, vec![]).1.is_none());
                let (_, event) = frame(&ctx, popup, None, click(pos));
                match (label, event) {
                    ("This window", Some(PopupEvent::UpdateTerminalWindow { window, info })) => {
                        assert_eq!(window, "fixture-original-window");
                        assert_eq!(info, popup);
                    }
                    (
                        "All outdated windows",
                        Some(PopupEvent::UpdateTerminalWindows { window: None }),
                    ) => {}
                    _ => panic!("choosing a scope must emit its exact direct or reviewed request"),
                }
                assert!(frame(&ctx, popup, None, click(pos)).1.is_none());
                assert!(ctx.data(|data| data.get_temp::<String>(popup.0.with("terminal-update-choice"))).is_none());
            }
        }
    }

    #[test]
    fn update_scope_cancel_escape_and_backdrop_discard_the_request() {
        for action in ["Cancel", "Escape", "Backdrop", "Enter"] {
            let ctx = egui::Context::default();
            let popup = egui::ViewportId::from_hash_of("cancel-info-only");
            frame(&ctx, popup, Some("fixture-no-IPC"), vec![]);
            let output = settled(&ctx, popup);
            let events = match action {
                "Cancel" => click(button(&output, "Cancel")),
                "Backdrop" => click(egui::pos2(10.0, 10.0)),
                key => vec![egui::Event::Key {
                    key: if key == "Escape" {
                        egui::Key::Escape
                    } else {
                        egui::Key::Enter
                    },
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            };
            assert!(frame(&ctx, popup, None, events).1.is_none(), "{action}");
            assert!(
                ctx.data(|data| data.get_temp::<String>(popup.0.with("terminal-update-choice")))
                    .is_none(),
                "{action}"
            );
            assert!(frame(&ctx, popup, None, vec![]).1.is_none());
        }
    }

    #[test]
    fn update_scope_dialog_is_isolated_to_its_info_window() {
        let ctx = egui::Context::default();
        let first = egui::ViewportId::from_hash_of("first-info");
        let second = egui::ViewportId::from_hash_of("second-info");
        frame(&ctx, first, Some("first-window"), vec![]);
        let (output, event) = frame(&ctx, second, None, vec![]);
        assert!(event.is_none());
        assert!(!output.shapes.iter().any(|shape| matches!(
            &shape.shape, egui::Shape::Text(text) if text.galley.job.text == "This window"
        )));
        let output = settled(&ctx, first);
        let (_, event) = frame(&ctx, first, None, click(button(&output, "This window")));
        assert!(
            matches!(event, Some(PopupEvent::UpdateTerminalWindow { window, info }) if window == "first-window" && info == first)
        );
    }

    fn status_frame(
        ctx: &egui::Context,
        popup: egui::ViewportId,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| show_terminal_status(ui, popup));
            },
        )
    }

    #[test]
    fn direct_terminal_update_status_is_inline_and_does_not_open_another_viewport_or_confirmation()
    {
        use applicationlauncher::tracker::{TerminalUpgradePhase, TerminalUpgradeReport};
        for phase in [
            None,
            Some(TerminalUpgradePhase::Preparing),
            Some(TerminalUpgradePhase::Ready),
            Some(TerminalUpgradePhase::Running),
            Some(TerminalUpgradePhase::Complete),
            Some(TerminalUpgradePhase::Failed),
        ] {
            let ctx = egui::Context::default();
            let popup = egui::ViewportId::from_hash_of("inline-status-fixture");
            let shared = Arc::new(Mutex::new(TerminalMaintenanceState {
                open: true,
                info_viewport: Some(popup),
                scope_window: Some("exact-window".into()),
                report: phase.map(|phase| TerminalUpgradeReport {
                    phase,
                    message: "fixture result".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }));
            bind_terminal_status(&ctx, popup, "exact-window".into(), shared, None);
            let output = status_frame(&ctx, popup, vec![]);
            let labels: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                    _ => None,
                })
                .collect();
            assert!(!labels.is_empty());
            assert!(
                !labels
                    .iter()
                    .any(|text| text.contains("Update Terminal Windows")
                        || text.contains("I reviewed")
                        || text.contains("Request updates in selected terminals"))
            );
            assert!(
                output
                    .viewport_output
                    .keys()
                    .all(|id| *id == egui::ViewportId::ROOT)
            );
            let other = egui::ViewportId::from_hash_of("other-inline-info");
            assert!(
                !status_frame(&ctx, other, vec![])
                    .shapes
                    .iter()
                    .any(|shape| matches!(shape.shape, egui::Shape::Text(_)))
            );
        }
    }

    #[test]
    fn unsupported_direct_terminal_update_shows_a_dismissible_error_without_arming_a_restart() {
        let ctx = egui::Context::default();
        let popup = egui::ViewportId::from_hash_of("unsupported-inline-status");
        let error =
            "This running terminal lacks the terminal-owned update API; restart it once manually";
        let shared = Arc::new(Mutex::new(TerminalMaintenanceState {
            open: true,
            info_viewport: Some(popup),
            scope_window: Some("old-terminal".into()),
            error: Some(error.into()),
            ..Default::default()
        }));
        bind_terminal_status(&ctx, popup, "old-terminal".into(), shared.clone(), None);
        for _ in 0..2 {
            status_frame(&ctx, popup, vec![]);
        }
        let output = status_frame(&ctx, popup, vec![]);
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.job.text == error && text.galley.job.sections.iter().all(|section| section.format.color == egui::Color32::LIGHT_RED))));
        status_frame(&ctx, popup, click(button(&output, "Dismiss")));
        assert!(
            ctx.data(|data| data.get_temp::<TerminalStatus>(terminal_status_id(popup)))
                .is_none()
        );
        assert!(!shared.lock().unwrap().update_window_on_ready);
        assert!(shared.lock().unwrap().error.is_some());
    }
}
