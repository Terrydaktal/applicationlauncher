use eframe::egui;
use std::sync::mpsc::Sender;

use crate::models::{InfoPopupData, PopupEvent};

pub(crate) fn render_info_popup_panel(ui: &mut egui::Ui, data: &InfoPopupData) {
    let searchable_label_color = egui::Color32::from_rgb(214, 184, 86);
    let searchable_value_color = egui::Color32::from_rgb(255, 236, 170);
    let neutral_label_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 170);
    let body_font = egui::TextStyle::Monospace.resolve(ui.style());
    let heading_font = egui::TextStyle::Heading.resolve(ui.style());
    let mut layout_job = egui::text::LayoutJob::default();
    let format = |font_id: egui::FontId, color: egui::Color32| egui::TextFormat {
        font_id,
        color,
        ..Default::default()
    };

    layout_job.append(
        &data.heading,
        0.0,
        format(heading_font, egui::Color32::WHITE),
    );
    layout_job.append("\n", 0.0, format(body_font.clone(), egui::Color32::WHITE));
    layout_job.append(
        &data.subtitle,
        0.0,
        format(
            body_font.clone(),
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 170),
        ),
    );
    layout_job.append("\n\n", 0.0, format(body_font.clone(), egui::Color32::WHITE));

    let label_width = data
        .rows
        .iter()
        .map(|row| row.label.chars().count())
        .max()
        .unwrap_or(0);
    for row in &data.rows {
        let label = format!("{:<label_width$}  ", row.label);
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
        layout_job.append(&label, 0.0, format(body_font.clone(), label_color));
        layout_job.append(
            &format!("{}\n", row.value),
            0.0,
            format(body_font.clone(), value_color),
        );
    }

    if !data.execution_chain.is_empty() {
        layout_job.append(
            "\nExecution chain\n\n",
            0.0,
            format(body_font.clone(), egui::Color32::WHITE),
        );
        for (process, executable) in &data.execution_chain {
            layout_job.append(
                &format!("{process}\n"),
                0.0,
                format(body_font.clone(), egui::Color32::WHITE),
            );
            layout_job.append(
                &format!("{executable}\n\n"),
                0.0,
                format(
                    body_font.clone(),
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 160),
                ),
            );
        }
    }

    let document = layout_job.text.clone();
    let desired_rows = document.lines().count().max(1);
    let mut immutable_document = document.as_str();
    let mut layouter = move |ui: &egui::Ui, _text: &dyn egui::TextBuffer, _wrap_width: f32| {
        let mut job = layout_job.clone();
        job.wrap.max_width = f32::INFINITY;
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    };
    ui.add(
        egui::TextEdit::multiline(&mut immutable_document)
            .id_salt("info-popup-document")
            .frame(false)
            .margin(egui::Margin::ZERO)
            .desired_width(f32::INFINITY)
            .desired_rows(desired_rows)
            .layouter(&mut layouter),
    );
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
