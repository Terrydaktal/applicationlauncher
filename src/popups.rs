use eframe::egui;
use std::sync::mpsc::Sender;

use crate::models::{InfoPopupData, PopupEvent};

pub(crate) fn render_info_popup_panel(ui: &mut egui::Ui, data: &InfoPopupData) {
    let searchable_label_color = egui::Color32::from_rgb(214, 184, 86);
    let searchable_value_color = egui::Color32::from_rgb(255, 236, 170);
    let neutral_label_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 170);

    ui.heading(
        egui::RichText::new(&data.heading)
            .color(egui::Color32::WHITE)
            .strong(),
    );
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(&data.subtitle)
            .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 170)),
    );
    ui.add_space(10.0);

    egui::Grid::new("deferred_info_grid")
        .num_columns(2)
        .spacing([14.0, 8.0])
        .striped(true)
        .show(ui, |ui| {
            for row in &data.rows {
                let label_color = if row.searched {
                    searchable_label_color
                } else {
                    neutral_label_color
                };
                let value_color = if row.searched {
                    searchable_value_color
                } else {
                    egui::Color32::WHITE
                };
                ui.label(egui::RichText::new(&row.label).color(label_color).strong());
                ui.label(
                    egui::RichText::new(&row.value)
                        .color(value_color)
                        .monospace(),
                );
                ui.end_row();
            }
        });

    if !data.execution_chain.is_empty() {
        ui.add_space(14.0);
        ui.separator();
        ui.add_space(10.0);
        ui.label(
            egui::RichText::new("Execution chain")
                .color(egui::Color32::WHITE)
                .strong(),
        );
        ui.add_space(6.0);
        for (process, executable) in &data.execution_chain {
            ui.group(|ui| {
                ui.label(
                    egui::RichText::new(process)
                        .color(egui::Color32::WHITE)
                        .strong(),
                );
                ui.label(
                    egui::RichText::new(executable)
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 160)),
                );
            });
            ui.add_space(6.0);
        }
    }
}

pub(crate) fn show_deferred_info_popup(
    ctx: &egui::Context,
    viewport_id: egui::ViewportId,
    data: InfoPopupData,
    inner_size: [f32; 2],
    min_inner_size: [f32; 2],
    close_event: PopupEvent,
    event_sender: Sender<PopupEvent>,
) {
    let builder = egui::ViewportBuilder::default()
        .with_title(data.title.clone())
        .with_inner_size(inner_size)
        .with_min_inner_size(min_inner_size)
        .with_resizable(true)
        .with_always_on_top();

    ctx.show_viewport_deferred(viewport_id, builder, move |ctx, _class| {
        let close_requested = ctx.input(|input| {
            input.viewport().close_requested()
                || input.key_pressed(egui::Key::Escape)
                || input.key_pressed(egui::Key::F10)
        });
        if close_requested {
            let _ = event_sender.send(close_event.clone());
            ctx.request_repaint_of(egui::ViewportId::ROOT);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_unmultiplied(20, 20, 20, 248))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| render_info_popup_panel(ui, &data));
            });
    });
}
