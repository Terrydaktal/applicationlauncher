use eframe::egui;
use std::sync::mpsc::Sender;

use crate::models::{InfoPopupData, PopupEvent};

fn info_popup_layout(
    data: &InfoPopupData,
    body_font: egui::FontId,
    heading_font: egui::FontId,
) -> (egui::text::LayoutJob, Vec<usize>) {
    let searchable_label_color = egui::Color32::from_rgb(214, 184, 86);
    let searchable_value_color = egui::Color32::from_rgb(255, 236, 170);
    let neutral_label_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 170);
    let mut layout_job = egui::text::LayoutJob::default();
    let mut divider_cursors = Vec::new();
    let format = |font_id: egui::FontId, color: egui::Color32| egui::TextFormat {
        font_id,
        color,
        ..Default::default()
    };
    let mut divider = |job: &mut egui::text::LayoutJob| {
        divider_cursors.push(job.text.chars().count());
        job.append("\n", 0.0, format(body_font.clone(), egui::Color32::WHITE));
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
        if row.separator_before {
            divider(&mut layout_job);
        }
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
        divider(&mut layout_job);
        layout_job.append(
            "Execution chain\n\n",
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

    (layout_job, divider_cursors)
}

pub(crate) fn render_info_popup_panel(
    ui: &mut egui::Ui,
    data: &InfoPopupData,
) -> egui::text_edit::TextEditOutput {
    let (layout_job, divider_cursors) = info_popup_layout(
        data,
        egui::TextStyle::Monospace.resolve(ui.style()),
        egui::TextStyle::Heading.resolve(ui.style()),
    );
    let document = layout_job.text.clone();
    let desired_rows = document.lines().count().max(1);
    let mut immutable_document = document.as_str();
    let mut layouter = move |ui: &egui::Ui, _text: &dyn egui::TextBuffer, _wrap_width: f32| {
        let mut job = layout_job.clone();
        job.wrap.max_width = f32::INFINITY;
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    };
    let output = egui::TextEdit::multiline(&mut immutable_document)
        .id_salt("info-popup-document")
        .frame(false)
        .margin(egui::Margin::ZERO)
        .desired_width(f32::INFINITY)
        .desired_rows(desired_rows)
        .layouter(&mut layouter)
        .show(ui);
    // Paint inside the single selectable document instead of creating separate widgets.
    let clip = output.text_clip_rect.intersect(ui.clip_rect());
    let painter = ui.painter().with_clip_rect(clip);
    for cursor in divider_cursors {
        let row = output
            .galley
            .pos_from_cursor(egui::text::CCursor::new(cursor));
        let y = output.galley_pos.y + row.center().y;
        painter.line_segment(
            [egui::pos2(clip.left(), y), egui::pos2(clip.right(), y)],
            ui.visuals().widgets.noninteractive.bg_stroke,
        );
    }
    output
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::InfoPopupRow;

    fn window_info() -> InfoPopupData {
        InfoPopupData {
            title: "Window Info".into(),
            heading: "codex - \u{280b} project - Terminal".into(),
            subtitle: "Window metadata, process details, and execution chain".into(),
            rows: vec![
                InfoPopupRow {
                    label: "Window PID".into(),
                    value: "1234".into(),
                    searched: false,
                    separator_before: false,
                },
                InfoPopupRow {
                    label: "Window application version".into(),
                    value: "1.2.0".into(),
                    searched: false,
                    separator_before: false,
                },
                InfoPopupRow {
                    label: "Active process".into(),
                    value: "codex".into(),
                    searched: true,
                    separator_before: true,
                },
                InfoPopupRow {
                    label: "Working directory".into(),
                    value: "~/project".into(),
                    searched: true,
                    separator_before: false,
                },
            ],
            execution_chain: vec![("codex (pid 4321)".into(), "/usr/bin/codex".into())],
        }
    }

    fn frame(
        ctx: &egui::Context,
        data: &InfoPopupData,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, egui::text_edit::TextEditOutput) {
        let mut editor = None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 1000.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    editor = Some(render_info_popup_panel(ui, data));
                });
            },
        );
        (output, editor.unwrap())
    }

    #[test]
    fn info_dividers_are_painted_in_blank_rows_without_changing_text_or_highlights() {
        let data = window_info();
        for scale in [1.0, 2.0] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(scale);
            let (output, editor) = frame(&ctx, &data, Vec::new());
            let (layout, cursors) = info_popup_layout(
                &data,
                egui::TextStyle::Monospace.resolve(&ctx.style()),
                egui::TextStyle::Heading.resolve(&ctx.style()),
            );
            assert_eq!(editor.galley.job.text, layout.text);
            assert_eq!(cursors.len(), 2);
            assert!(layout.text.contains("\n\nActive process"));
            assert!(layout.text.contains("\n\nExecution chain"));
            let lines: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::LineSegment { points, stroke } => Some((points, stroke)),
                    _ => None,
                })
                .collect();
            assert_eq!(lines.len(), 2);
            for ((points, stroke), cursor) in lines.iter().zip(cursors) {
                let blank_row = editor
                    .galley
                    .pos_from_cursor(egui::text::CCursor::new(cursor));
                assert_eq!(layout.text.chars().nth(cursor), Some('\n'));
                assert!((points[0].y - editor.galley_pos.y - blank_row.center().y).abs() < 0.01);
                assert_eq!(points[0].y, points[1].y);
                assert!(points[1].x > points[0].x);
                assert_eq!(
                    **stroke,
                    ctx.style().visuals.widgets.noninteractive.bg_stroke
                );
            }
            let active_value = layout
                .sections
                .iter()
                .find(|section| &layout.text[section.byte_range.clone()] == "codex\n")
                .unwrap();
            assert_eq!(
                active_value.format.color,
                egui::Color32::from_rgb(255, 236, 170)
            );
        }
        let mut app_info = data;
        for row in &mut app_info.rows {
            row.separator_before = false;
        }
        app_info.execution_chain.clear();
        let ctx = egui::Context::default();
        let (output, _) = frame(&ctx, &app_info, Vec::new());
        assert!(
            !output
                .shapes
                .iter()
                .any(|shape| matches!(shape.shape, egui::Shape::LineSegment { .. }))
        );
    }

    #[test]
    fn dragging_and_copying_selects_across_both_info_dividers() {
        let ctx = egui::Context::default();
        let data = window_info();
        let (_, editor) = frame(&ctx, &data, Vec::new());
        let text = editor.galley.job.text.clone();
        let start = text[..text.find("Window PID").unwrap()].chars().count();
        let end = text[..text.find("/usr/bin/codex").unwrap()].chars().count()
            + "/usr/bin/codex".chars().count();
        let position = |cursor| {
            editor.galley_pos
                + editor
                    .galley
                    .pos_from_cursor(egui::text::CCursor::new(cursor))
                    .center()
                    .to_vec2()
        };
        let pointer = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            &ctx,
            &data,
            vec![
                egui::Event::PointerMoved(position(start)),
                pointer(position(start), true),
            ],
        );
        frame(&ctx, &data, vec![egui::Event::PointerMoved(position(end))]);
        let (_, selected) = frame(&ctx, &data, vec![pointer(position(end), false)]);
        let range = selected
            .state
            .cursor
            .char_range()
            .unwrap()
            .as_sorted_char_range();
        let selected_text: String = selected
            .galley
            .job
            .text
            .chars()
            .skip(range.start)
            .take(range.end - range.start)
            .collect();
        assert!(selected_text.contains("Window PID"));
        assert!(selected_text.contains("Active process"));
        assert!(selected_text.contains("Execution chain"));
        assert!(selected_text.contains("/usr/bin/codex"));
        let (output, _) = frame(&ctx, &data, vec![egui::Event::Copy]);
        assert!(output.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::CopyText(copied) if copied == &selected_text)
        }));
    }
}
