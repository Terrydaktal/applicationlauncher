use eframe::egui;

const PROCESS_TREE_INDENT: f32 = 12.0;
const INFO_POPUP_MARGIN: i8 = 16;
use std::sync::mpsc::Sender;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::models::{InfoPopupData, InfoPopupRow, PopupEvent, ProcessIdentity};

pub(crate) fn process_info_section_id(
    window: impl std::hash::Hash,
    identity: ProcessIdentity,
) -> egui::Id {
    egui::Id::new(("process-info-section", window, identity))
}

pub(crate) fn process_info_section_is_open(
    ctx: &egui::Context,
    id: egui::Id,
    default_open: bool,
) -> bool {
    egui::collapsing_header::CollapsingState::load_with_default_open(ctx, id, default_open)
        .is_open()
}

fn info_popup_layout(
    heading: &str,
    subtitle: &str,
    rows: &[InfoPopupRow],
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

    if !heading.is_empty() {
        layout_job.append(heading, 0.0, format(heading_font, egui::Color32::WHITE));
        layout_job.append("\n", 0.0, format(body_font.clone(), egui::Color32::WHITE));
    }
    if !subtitle.is_empty() {
        layout_job.append(
            subtitle,
            0.0,
            format(
                body_font.clone(),
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 170),
            ),
        );
        layout_job.append("\n\n", 0.0, format(body_font.clone(), egui::Color32::WHITE));
    }

    let label_width = rows
        .iter()
        .map(|row| row.label.chars().count())
        .max()
        .unwrap_or(0);
    for row in rows {
        if row.separator_before {
            divider(&mut layout_job);
        }
        let label = format!("{:<label_width$}  ", row.label);
        let label_color = if row.searched {
            searchable_label_color
        } else {
            neutral_label_color
        };
        let value_color = match row.binary_is_current {
            Some(true) => egui::Color32::from_rgb(130, 225, 145),
            Some(false) => egui::Color32::from_rgb(255, 110, 110),
            None if row.searched => searchable_value_color,
            None => egui::Color32::WHITE,
        };
        layout_job.append(&label, 0.0, format(body_font.clone(), label_color));
        layout_job.append(
            &format!("{}\n", row.value),
            0.0,
            format(body_font.clone(), value_color),
        );
    }

    (layout_job, divider_cursors)
}

struct InfoText {
    job: egui::text::LayoutJob,
    raw: Option<String>,
    // Display-character boundaries mapped to raw UTF-8, including trailing strike marks.
    raw_boundaries: Vec<usize>,
}

impl InfoText {
    fn new(job: egui::text::LayoutJob) -> Self {
        if !job.text.contains(['\u{0335}', '\u{0336}']) {
            return Self {
                job,
                raw: None,
                raw_boundaries: Vec::new(),
            };
        }
        let raw = job.text.clone();
        let mut raw_boundaries = vec![0];
        for (start, character) in raw.char_indices() {
            let end = start + character.len_utf8();
            if crate::search::is_title_strikethrough_mark(character) {
                if raw_boundaries.len() > 1 {
                    *raw_boundaries.last_mut().unwrap() = end;
                }
            } else {
                raw_boundaries.push(end);
            }
        }
        Self {
            job: crate::search::render_title_strikethrough(job),
            raw: Some(raw),
            raw_boundaries,
        }
    }

    fn display_cursor(&self, raw_cursor: usize) -> usize {
        self.raw.as_deref().map_or(raw_cursor, |raw| {
            raw.chars()
                .take(raw_cursor)
                .filter(|character| !crate::search::is_title_strikethrough_mark(*character))
                .count()
        })
    }

    fn copy_checkpoint(
        &self,
        ctx: &egui::Context,
        id: egui::Id,
    ) -> Option<(usize, Option<egui::text::CCursorRange>)> {
        self.raw.as_ref()?;
        Some((
            ctx.output(|output| output.commands.len()),
            egui::text_edit::TextEditState::load(ctx, id)
                .and_then(|state| state.cursor.char_range()),
        ))
    }

    fn restore_raw_copy(
        &self,
        ctx: &egui::Context,
        editor: &egui::text_edit::TextEditOutput,
        checkpoint: Option<(usize, Option<egui::text::CCursorRange>)>,
    ) {
        let Some((command_start, before_selection)) = checkpoint else {
            return;
        };
        // Only replace this editor's clipboard commands. Cut may have collapsed its selection.
        let selections = [editor.cursor_range, before_selection];
        ctx.output_mut(|output| {
            for command in output.commands.iter_mut().skip(command_start) {
                let egui::OutputCommand::CopyText(copied) = command else {
                    continue;
                };
                for selection in selections.into_iter().flatten() {
                    let range = selection.as_sorted_char_range();
                    if range.is_empty() || range.end >= self.raw_boundaries.len() {
                        continue;
                    }
                    if selection.slice_str(&self.job.text) == copied {
                        let raw = self.raw.as_ref().unwrap();
                        *copied = raw
                            [self.raw_boundaries[range.start]..self.raw_boundaries[range.end]]
                            .to_owned();
                        break;
                    }
                }
            }
        });
    }
}

fn render_info_document(
    ui: &mut egui::Ui,
    heading: &str,
    subtitle: &str,
    rows: &[InfoPopupRow],
) -> egui::text_edit::TextEditOutput {
    let (layout_job, divider_cursors) = info_popup_layout(
        heading,
        subtitle,
        rows,
        egui::TextStyle::Monospace.resolve(ui.style()),
        egui::TextStyle::Heading.resolve(ui.style()),
    );
    let text = InfoText::new(layout_job);
    let divider_cursors: Vec<_> = divider_cursors
        .into_iter()
        .map(|cursor| text.display_cursor(cursor))
        .collect();
    let document = &text.job.text;
    let desired_rows = document.lines().count().max(1);
    let mut immutable_document = document.as_str();
    let mut layouter = |ui: &egui::Ui, _text: &dyn egui::TextBuffer, _wrap_width: f32| {
        let mut job = text.job.clone();
        job.wrap.max_width = f32::INFINITY;
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    };
    let id = ui.make_persistent_id(egui::Id::new("info-popup-document"));
    let checkpoint = text.copy_checkpoint(ui.ctx(), id);
    let output = egui::TextEdit::multiline(&mut immutable_document)
        .id(id)
        .frame(false)
        .margin(egui::Margin::ZERO)
        .desired_width(f32::INFINITY)
        .desired_rows(desired_rows)
        .layouter(&mut layouter)
        .show(ui);
    text.restore_raw_copy(ui.ctx(), &output, checkpoint);
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

fn copy_shown_info(data: &InfoPopupData) -> String {
    let mut output = format!("{}\n{}\n\n", data.heading, data.subtitle);
    let mut append_rows = |rows: &[InfoPopupRow]| {
        let width = rows
            .iter()
            .map(|row| row.label.chars().count())
            .max()
            .unwrap_or(0);
        for row in rows {
            output.push_str(&format!("{:<width$}  {}\n", row.label, row.value));
        }
    };
    append_rows(&data.rows);
    if let Some(tree) = &data.process_tree {
        for section in &tree.sections {
            let indent = section.depth * 2;
            output.push_str(&format!("{:indent$}{}\n", "", section.header));
            let width = section
                .rows
                .iter()
                .map(|row| row.label.chars().count())
                .max()
                .unwrap_or(0);
            for row in &section.rows {
                output.push_str(&format!(
                    "{:padding$}{:<width$}  {}\n",
                    "",
                    row.label,
                    row.value,
                    padding = indent + 4
                ));
            }
            for metadata in &section.metadata_sections {
                if metadata.rows.is_empty() {
                    continue;
                }
                output.push_str(&format!(
                    "{:padding$}{}\n",
                    "",
                    metadata.header,
                    padding = indent + 4
                ));
                let width = metadata
                    .rows
                    .iter()
                    .map(|row| row.label.chars().count())
                    .max()
                    .unwrap_or(0);
                for row in &metadata.rows {
                    output.push_str(&format!(
                        "{:padding$}{:<width$}  {}\n",
                        "",
                        row.label,
                        row.value,
                        padding = indent + 8
                    ));
                }
            }
        }
        output.push_str(&format!("\n{}\n", tree.notice));
    }
    output
}

fn render_collapsible_info(
    ui: &mut egui::Ui,
    id: egui::Id,
    header: &str,
    default_open: bool,
    body: impl FnOnce(&mut egui::Ui) -> Vec<egui::text_edit::TextEditOutput>,
) -> Vec<egui::text_edit::TextEditOutput> {
    let state = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        id,
        default_open,
    );
    let header = state.show_header(ui, |ui| {
        let mut job = egui::text::LayoutJob::default();
        job.append(
            header,
            0.0,
            egui::TextFormat {
                font_id: egui::TextStyle::Monospace.resolve(ui.style()),
                color: ui.visuals().text_color(),
                ..Default::default()
            },
        );
        let text = InfoText::new(job);
        let mut display = text.job.text.as_str();
        let mut layouter = |ui: &egui::Ui, _: &dyn egui::TextBuffer, _: f32| {
            ui.fonts_mut(|fonts| fonts.layout_job(text.job.clone()))
        };
        let id = id.with("process-header-text");
        let checkpoint = text.copy_checkpoint(ui.ctx(), id);
        let output = egui::TextEdit::singleline(&mut display)
            .id(id)
            .font(egui::TextStyle::Monospace)
            .frame(false)
            .margin(egui::Margin::ZERO)
            .desired_width(0.0)
            .clip_text(false)
            .layouter(&mut layouter)
            .show(ui);
        text.restore_raw_copy(ui.ctx(), &output, checkpoint);
        output
    });
    let (toggle, _, contents) = header.body(|ui| ui.push_id(id, body).inner);
    if toggle.clicked() {
        ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
    }
    contents.map(|body| body.inner).unwrap_or_default()
}

pub(crate) fn render_info_popup_panel(
    ui: &mut egui::Ui,
    data: &InfoPopupData,
) -> Vec<egui::text_edit::TextEditOutput> {
    let Some(tree) = &data.process_tree else {
        return vec![render_info_document(
            ui,
            &data.heading,
            &data.subtitle,
            &data.rows,
        )];
    };
    let mut heading = egui::text::LayoutJob::default();
    heading.append(
        &data.heading,
        0.0,
        egui::TextFormat {
            font_id: egui::TextStyle::Heading.resolve(ui.style()),
            color: ui.visuals().text_color(),
            ..Default::default()
        },
    );
    ui.label(crate::search::render_title_strikethrough(heading));
    ui.label(egui::RichText::new(&data.subtitle).weak());
    if ui.button("Copy shown info").clicked() {
        ui.ctx().copy_text(copy_shown_info(data));
    }
    ui.add_space(8.0);
    let mut editors = Vec::new();
    for section in &tree.sections {
        let mut rect = ui.available_rect_before_wrap();
        rect.min.x += section.depth as f32 * PROCESS_TREE_INDENT;
        // Indent the whole section, not the label, so arrows and details follow the same tree.
        ui.scope_builder(
            egui::UiBuilder::new().id_salt(section.id).max_rect(rect),
            |ui| {
                editors.extend(render_collapsible_info(
                    ui,
                    section.id,
                    &section.header,
                    section.default_open,
                    |ui| {
                        let mut documents = Vec::new();
                        if !section.rows.is_empty() {
                            documents.push(render_info_document(ui, "", "", &section.rows));
                        } else if section.metadata_sections.is_empty() {
                            ui.label("Loading process details...");
                        }
                        for metadata in &section.metadata_sections {
                            documents.extend(render_collapsible_info(
                                ui,
                                metadata.id,
                                &metadata.header,
                                metadata.default_open,
                                |ui| {
                                    if metadata.rows.is_empty() {
                                        ui.label("Loading application details...");
                                        Vec::new()
                                    } else {
                                        vec![render_info_document(ui, "", "", &metadata.rows)]
                                    }
                                },
                            ));
                        }
                        documents
                    },
                ));
            },
        );
    }
    ui.add_space(8.0);
    ui.label(egui::RichText::new(tree.notice.as_ref()).small().weak());
    editors
}

pub(crate) fn render_info_popup_scroll_area(
    ui: &mut egui::Ui,
    data: &InfoPopupData,
) -> egui::scroll_area::ScrollAreaOutput<Vec<egui::text_edit::TextEditOutput>> {
    egui::ScrollArea::both()
        .scroll_source(egui::scroll_area::ScrollSource {
            drag: false,
            ..egui::scroll_area::ScrollSource::ALL
        })
        .auto_shrink([false, false])
        .wheel_scroll_multiplier(egui::Vec2::splat(data.scroll_sensitivity))
        .show(ui, |ui| render_info_popup_panel(ui, data))
}

pub(crate) fn info_popup_initial_size(
    ctx: &egui::Context,
    viewport_id: egui::ViewportId,
    data: &InfoPopupData,
    default_size: [f32; 2],
    min_size: [f32; 2],
) -> [f32; 2] {
    let screen_reserve = egui::vec2(32.0, 96.0);
    let monitor = ctx
        .input(|input| input.viewport().monitor_size)
        .unwrap_or(egui::Vec2::from(default_size) + screen_reserve);
    let maximum = (monitor - screen_reserve).max(egui::Vec2::splat(1.0));
    let width = default_size[0].min(maximum.x);
    let padding = 2.0 * f32::from(INFO_POPUP_MARGIN);
    let measurement_id = viewport_id
        .0
        .with(("initial-size", ctx.cumulative_pass_nr()));
    let mut measured = data.clone();
    // Isolate hidden widgets from the real popup's selection, focus and collapse state.
    if let Some(tree) = &mut measured.process_tree {
        for section in &mut tree.sections {
            section.default_open =
                process_info_section_is_open(ctx, section.id, section.default_open);
            section.id = measurement_id.with(section.id);
            for metadata in &mut section.metadata_sections {
                metadata.default_open =
                    process_info_section_is_open(ctx, metadata.id, metadata.default_open);
                metadata.id = measurement_id.with(metadata.id);
            }
        }
    }
    let mut ui = egui::Ui::new(
        ctx.clone(),
        measurement_id,
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2((width - padding).max(1.0), (maximum.y - padding).max(1.0)),
            ))
            // A sizing_pass changes header alignment; use the real layout without painting it.
            .invisible(),
    );
    let content = render_info_popup_scroll_area(&mut ui, &measured).content_size;
    let horizontal_scrollbar = if content.x > width - padding {
        ui.spacing().scroll.allocated_width()
    } else {
        0.0
    };
    let fitted_height = (content.y + padding + horizontal_scrollbar)
        .ceil()
        .max(min_size[1]);
    let height = (fitted_height + 8.0).min(maximum.y);
    [width, height]
}

pub(crate) fn show_deferred_info_popup(
    ctx: &egui::Context,
    viewport_id: egui::ViewportId,
    data: InfoPopupData,
    inner_size: [f32; 2],
    min_inner_size: [f32; 2],
    focus_requested: Arc<AtomicBool>,
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
            let _ = event_sender.send(PopupEvent::CloseInfo(viewport_id));
            ctx.request_repaint_of(egui::ViewportId::ROOT);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        if focus_requested.swap(false, Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_unmultiplied(20, 20, 20, 248))
                    .inner_margin(egui::Margin::same(INFO_POPUP_MARGIN)),
            )
            .show(ctx, |ui| {
                render_info_popup_scroll_area(ui, &data);
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{InfoPopupRow, InfoProcessSection, InfoProcessTree};

    #[test]
    fn dragging_info_background_does_not_pan_the_document() {
        fn scroll_frame(
            ctx: &egui::Context,
            data: &InfoPopupData,
            events: Vec<egui::Event>,
        ) -> (egui::Vec2, egui::Pos2) {
            let mut result = None;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(640.0, 300.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let output = render_info_popup_scroll_area(ui, data);
                        let editor = &output.inner[0].response;
                        let gutter = egui::pos2(
                            (output.inner_rect.left() + editor.rect.left()) / 2.0,
                            output.inner_rect.bottom() - 32.0,
                        );
                        assert!(gutter.x < editor.rect.left());
                        result = Some((output.state.offset, gutter));
                    });
                },
            );
            result.unwrap()
        }

        let ctx = egui::Context::default();
        ctx.style_mut(|style| style.animation_time = 0.0);
        let mut data = tree_info();
        data.process_tree.as_mut().unwrap().sections[0].rows = (0..80)
            .map(|index| InfoPopupRow {
                label: format!("Metadata {index}"),
                value: "A long metadata value ".repeat(30),
                searched: false,
                separator_before: false,
                binary_is_current: None,
            })
            .collect();
        scroll_frame(&ctx, &data, Vec::new());
        let (initial, start) = scroll_frame(&ctx, &data, Vec::new());
        let pointer = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        scroll_frame(&ctx, &data, vec![egui::Event::PointerMoved(start)]);
        scroll_frame(&ctx, &data, vec![pointer(start, true)]);
        let end = start - egui::vec2(0.0, 80.0);
        scroll_frame(&ctx, &data, vec![egui::Event::PointerMoved(end)]);
        let (dragged, _) = scroll_frame(&ctx, &data, vec![pointer(end, false)]);
        assert_eq!(
            dragged, initial,
            "dragging the empty gutter panned the info document"
        );
        let (scrolled, _) = scroll_frame(
            &ctx,
            &data,
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(-10.0, -10.0),
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(scrolled.x > initial.x && scrolled.y > initial.y);
    }

    #[test]
    fn info_wheel_sensitivity_changes_live_on_both_axes_without_resetting_scroll() {
        fn scroll_frame(
            ctx: &egui::Context,
            data: &InfoPopupData,
            delta: egui::Vec2,
        ) -> (egui::Vec2, egui::Id) {
            let mut result = None;
            let mut events = vec![egui::Event::PointerMoved(egui::pos2(140.0, 150.0))];
            if delta != egui::Vec2::ZERO {
                events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(640.0, 300.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let output = render_info_popup_scroll_area(ui, data);
                        result = Some((output.state.offset, output.inner[0].response.id));
                    });
                },
            );
            result.unwrap()
        }

        for tree in [false, true] {
            let ctx = egui::Context::default();
            let mut data = if tree { tree_info() } else { window_info() };
            let rows = (0..120)
                .map(|index| InfoPopupRow {
                    label: format!("Metadata {index}"),
                    value: "A long metadata value ".repeat(30),
                    searched: false,
                    separator_before: false,
                    binary_is_current: None,
                })
                .collect();
            if let Some(tree) = &mut data.process_tree {
                tree.sections[0].rows = rows;
            } else {
                data.rows = rows;
            }
            data.scroll_sensitivity = 1.0;
            for _ in 0..2 {
                scroll_frame(&ctx, &data, egui::Vec2::ZERO);
            }
            let (normal, editor) = scroll_frame(&ctx, &data, egui::vec2(-2.0, -4.0));
            assert!(normal.x > 0.0 && normal.y > 0.0, "{normal:?}");

            data.scroll_sensitivity = 4.0;
            let (unchanged, same_editor) = scroll_frame(&ctx, &data, egui::Vec2::ZERO);
            assert_eq!(unchanged, normal);
            assert_eq!(same_editor, editor);
            let (faster, same_editor) = scroll_frame(&ctx, &data, egui::vec2(-2.0, -4.0));
            assert_eq!(same_editor, editor);
            assert!(
                ((faster - normal) - normal * 4.0).length() < 0.01,
                "normal={normal:?}, faster={faster:?}"
            );
        }
    }

    #[test]
    fn native_info_viewports_focus_once_and_close_only_their_own_identity() {
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        let ids = [
            egui::ViewportId::from_hash_of("info-a"),
            egui::ViewportId::from_hash_of("info-b"),
        ];
        let focus = [
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(true)),
        ];
        let (sender, receiver) = std::sync::mpsc::channel();
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            for (index, id) in ids.into_iter().enumerate() {
                let mut data = window_info();
                data.title = format!("Info {index}");
                show_deferred_info_popup(
                    ctx,
                    id,
                    data,
                    [1200.0, 800.0],
                    [520.0, 360.0],
                    focus[index].clone(),
                    sender.clone(),
                );
            }
        });
        for (index, id) in ids.into_iter().enumerate() {
            let viewport = &output.viewport_output[&id];
            assert_eq!(viewport.builder.inner_size, Some(egui::vec2(1200.0, 800.0)));
            assert_eq!(viewport.builder.resizable, Some(true));
            assert_eq!(
                viewport.builder.title.as_deref(),
                Some(format!("Info {index}").as_str())
            );
            let callback = viewport.viewport_ui_cb.as_ref().unwrap();
            for initial in [true, false] {
                let mut input = egui::RawInput {
                    viewport_id: id,
                    ..Default::default()
                };
                input.viewports.insert(
                    id,
                    egui::ViewportInfo {
                        parent: Some(egui::ViewportId::ROOT),
                        ..Default::default()
                    },
                );
                let painted = ctx.run(input, |ctx| callback(ctx));
                let commands = &painted.viewport_output[&id].commands;
                assert_eq!(
                    commands
                        .iter()
                        .any(|command| matches!(command, egui::ViewportCommand::Focus)),
                    initial
                );
                assert_eq!(
                    commands
                        .iter()
                        .any(|command| matches!(command, egui::ViewportCommand::Minimized(false))),
                    initial
                );
                assert!(
                    !painted.viewport_output[&egui::ViewportId::ROOT]
                        .commands
                        .iter()
                        .any(|command| matches!(command, egui::ViewportCommand::Focus))
                );
            }
            focus[index].store(true, Ordering::Relaxed);
            let mut input = egui::RawInput {
                viewport_id: id,
                ..Default::default()
            };
            input.viewports.insert(
                id,
                egui::ViewportInfo {
                    parent: Some(egui::ViewportId::ROOT),
                    ..Default::default()
                },
            );
            let refocused = ctx.run(input, |ctx| callback(ctx));
            assert!(
                refocused.viewport_output[&id]
                    .commands
                    .iter()
                    .any(|command| matches!(command, egui::ViewportCommand::Focus))
            );
            let mut input = egui::RawInput {
                viewport_id: id,
                ..Default::default()
            };
            input.viewports.insert(
                id,
                egui::ViewportInfo {
                    parent: Some(egui::ViewportId::ROOT),
                    events: vec![egui::ViewportEvent::Close],
                    ..Default::default()
                },
            );
            let closed = ctx.run(input, |ctx| callback(ctx));
            assert!(
                closed.viewport_output[&id]
                    .commands
                    .iter()
                    .any(|command| matches!(command, egui::ViewportCommand::Close))
            );
            assert!(
                matches!(receiver.try_recv(), Ok(PopupEvent::CloseInfo(closed_id)) if closed_id == id)
            );
            assert!(receiver.try_recv().is_err());
        }
    }

    #[test]
    fn process_dropdown_state_is_scoped_to_its_information_window() {
        let ctx = egui::Context::default();
        let identity = ProcessIdentity {
            pid: 42,
            start_ticks: 123,
        };
        let first =
            process_info_section_id((egui::ViewportId::from_hash_of("a"), "window"), identity);
        let second =
            process_info_section_id((egui::ViewportId::from_hash_of("b"), "window"), identity);
        assert_ne!(first, second);
        let mut state =
            egui::collapsing_header::CollapsingState::load_with_default_open(&ctx, first, true);
        state.set_open(false);
        state.store(&ctx);
        assert!(!process_info_section_is_open(&ctx, first, true));
        assert!(process_info_section_is_open(&ctx, second, true));
    }

    fn window_info() -> InfoPopupData {
        InfoPopupData {
            title: "Window Info".into(),
            heading: "codex - \u{280b} project - Terminal".into(),
            subtitle: "Window metadata, process details, and process tree".into(),
            scroll_sensitivity: 1.0,
            rows: vec![
                InfoPopupRow {
                    label: "Window PID".into(),
                    value: "1234".into(),
                    searched: false,
                    separator_before: false,
                    binary_is_current: None,
                },
                InfoPopupRow {
                    label: "Application version".into(),
                    value: "1.2.0".into(),
                    searched: false,
                    separator_before: false,
                    binary_is_current: None,
                },
                InfoPopupRow {
                    label: "Active process".into(),
                    value: "codex".into(),
                    searched: true,
                    separator_before: true,
                    binary_is_current: None,
                },
                InfoPopupRow {
                    label: "Working directory".into(),
                    value: "~/project".into(),
                    searched: true,
                    separator_before: false,
                    binary_is_current: None,
                },
                InfoPopupRow {
                    label: "Executable path".into(),
                    value: "/usr/bin/codex".into(),
                    searched: false,
                    separator_before: true,
                    binary_is_current: None,
                },
            ],
            process_tree: None,
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
                    editor = render_info_popup_panel(ui, data).into_iter().next();
                });
            },
        );
        (output, editor.unwrap())
    }

    #[test]
    fn info_strikethrough_renders_without_boxes_and_copies_the_exact_raw_title() {
        for mark in ['\u{0335}', '\u{0336}'] {
            for scale in [1.0, 2.0] {
                let ctx = egui::Context::default();
                ctx.set_pixels_per_point(scale);
                let struck: String = "Terminal".chars().flat_map(|ch| [ch, mark]).collect();
                let raw_title = format!("~ - {struck}");
                let mut data = window_info();
                data.rows.insert(
                    1,
                    InfoPopupRow {
                        label: "Raw window title".into(),
                        value: raw_title.clone(),
                        searched: false,
                        separator_before: false,
                        binary_is_current: None,
                    },
                );
                let (_, editor) = frame(&ctx, &data, Vec::new());
                let job = &editor.galley.job;
                assert!(!job.text.contains(['\u{0335}', '\u{0336}']));
                assert!(
                    job.sections
                        .iter()
                        .any(
                            |section| &job.text[section.byte_range.clone()] == "Terminal"
                                && section.format.strikethrough.width > 0.0
                                && section.format.color == egui::Color32::WHITE
                        )
                );
                let start = job.text[..job.text.find("~ - Terminal").unwrap()]
                    .chars()
                    .count();
                let end = start + "~ - Terminal".chars().count();
                let mut state = editor.state;
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(start),
                        egui::text::CCursor::new(end),
                    )));
                state.store(&ctx, editor.response.id);
                ctx.memory_mut(|memory| memory.request_focus(editor.response.id));
                for event in [egui::Event::Copy, egui::Event::Cut] {
                    let (output, _) = frame(&ctx, &data, vec![event]);
                    let copies: Vec<_> = output
                        .platform_output
                        .commands
                        .iter()
                        .filter_map(|command| match command {
                            egui::OutputCommand::CopyText(value) => Some(value),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(copies, [&raw_title]);
                }
                assert!(copy_shown_info(&data).contains(&raw_title));
                assert_eq!(data.rows[1].value, raw_title);
            }
        }
    }

    #[test]
    fn info_strikethrough_preserves_divider_positions_and_cross_row_copying() {
        let ctx = egui::Context::default();
        let mut data = window_info();
        data.rows.insert(0, InfoPopupRow {
            label: "Raw window title".into(),
            value: "\u{3bb} - T\u{0336}e\u{0336}r\u{0336}m\u{0336}i\u{0336}n\u{0336}a\u{0336}l\u{0336}".into(),
            searched: true,
            separator_before: false,
            binary_is_current: None,
        });
        let (output, editor) = frame(&ctx, &data, Vec::new());
        let (raw_job, dividers) = info_popup_layout(
            &data.heading,
            &data.subtitle,
            &data.rows,
            egui::TextStyle::Monospace.resolve(&ctx.style()),
            egui::TextStyle::Heading.resolve(&ctx.style()),
        );
        let raw = raw_job.text.clone();
        let projection = InfoText::new(raw_job);
        let lines: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::LineSegment { points, .. } => Some(points),
                _ => None,
            })
            .collect();
        assert_eq!(lines.len(), dividers.len());
        for (line, cursor) in lines.into_iter().zip(dividers) {
            let cursor = projection.display_cursor(cursor);
            assert_eq!(projection.job.text.chars().nth(cursor), Some('\n'));
            let row = editor
                .galley
                .pos_from_cursor(egui::text::CCursor::new(cursor));
            assert!((line[0].y - editor.galley_pos.y - row.center().y).abs() < 0.01);
        }
        let display = &editor.galley.job.text;
        let start = display[..display.find("Raw window title").unwrap()]
            .chars()
            .count();
        let end = display[..display.find("/usr/bin/codex").unwrap()]
            .chars()
            .count()
            + "/usr/bin/codex".len();
        let expected = &raw[raw.find("Raw window title").unwrap()
            ..raw.find("/usr/bin/codex").unwrap() + "/usr/bin/codex".len()];
        let mut state = editor.state;
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(start),
                egui::text::CCursor::new(end),
            )));
        state.store(&ctx, editor.response.id);
        ctx.memory_mut(|memory| memory.request_focus(editor.response.id));
        let (output, _) = frame(&ctx, &data, vec![egui::Event::Copy]);
        assert!(output.platform_output.commands.iter().any(
            |command| matches!(command, egui::OutputCommand::CopyText(value) if value == expected)
        ));
        assert!(
            projection
                .job
                .sections
                .iter()
                .any(
                    |section| &projection.job.text[section.byte_range.clone()] == "Terminal"
                        && section.format.strikethrough.width > 0.0
                        && section.format.color == egui::Color32::from_rgb(255, 236, 170)
                )
        );
    }

    #[test]
    fn identity_values_are_red_or_green_without_colouring_labels_or_other_values() {
        let hash = "6e689278f21a31797320afd5269395c09d75e390";
        let labels = [
            "Application ELF build ID",
            "Application running code fingerprint",
            "Application latest extensions fingerprint",
        ];
        for scale in [1.0, 2.0] {
            for (status, colour) in [
                (Some(false), egui::Color32::from_rgb(255, 110, 110)),
                (Some(true), egui::Color32::from_rgb(130, 225, 145)),
                (None, egui::Color32::WHITE),
            ] {
                let mut data = window_info();
                for (index, label) in labels.iter().enumerate() {
                    data.rows.insert(
                        1 + index,
                        InfoPopupRow {
                            label: (*label).into(),
                            value: hash.into(),
                            searched: false,
                            separator_before: false,
                            binary_is_current: status,
                        },
                    );
                }
                let ctx = egui::Context::default();
                ctx.set_pixels_per_point(scale);
                let (_, editor) = frame(&ctx, &data, Vec::new());
                let layout = &editor.galley.job;
                let hash_sections: Vec<_> = layout
                    .sections
                    .iter()
                    .filter(|section| {
                        &layout.text[section.byte_range.clone()] == format!("{hash}\n")
                    })
                    .collect();
                assert_eq!(hash_sections.len(), labels.len());
                for hash_section in hash_sections {
                    assert_eq!(hash_section.format.color, colour);
                }
                for label in labels {
                    let label_section = layout
                        .sections
                        .iter()
                        .find(|section| layout.text[section.byte_range.clone()].starts_with(label))
                        .unwrap();
                    assert_eq!(
                        label_section.format.color,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 170)
                    );
                }
                let other = layout
                    .sections
                    .iter()
                    .find(|section| &layout.text[section.byte_range.clone()] == "1234\n")
                    .unwrap();
                assert_eq!(other.format.color, egui::Color32::WHITE);
                assert!(copy_shown_info(&data).contains(hash));
            }
        }
    }

    #[test]
    fn info_dividers_are_painted_in_blank_rows_without_changing_text_or_highlights() {
        let data = window_info();
        for scale in [1.0, 2.0] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(scale);
            let (output, editor) = frame(&ctx, &data, Vec::new());
            let (layout, cursors) = info_popup_layout(
                &data.heading,
                &data.subtitle,
                &data.rows,
                egui::TextStyle::Monospace.resolve(&ctx.style()),
                egui::TextStyle::Heading.resolve(&ctx.style()),
            );
            assert_eq!(editor.galley.job.text, layout.text);
            assert_eq!(cursors.len(), 2);
            assert!(layout.text.contains("\n\nActive process"));
            assert!(layout.text.contains("\n\nExecutable path"));
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
        app_info.process_tree = None;
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
        assert!(selected_text.contains("Executable path"));
        assert!(selected_text.contains("/usr/bin/codex"));
        let (output, _) = frame(&ctx, &data, vec![egui::Event::Copy]);
        assert!(output.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::CopyText(copied) if copied == &selected_text)
        }));
    }

    fn tree_info() -> InfoPopupData {
        let owner = process_info_section_id(
            "window",
            ProcessIdentity {
                pid: 1,
                start_ticks: 10,
            },
        );
        let fish = process_info_section_id(
            "window",
            ProcessIdentity {
                pid: 2,
                start_ticks: 20,
            },
        );
        InfoPopupData {
            title: "Window Info".into(),
            heading: "pi - work - Terminal".into(),
            subtitle: "Process details".into(),
            scroll_sensitivity: 1.0,
            rows: Vec::new(),
            process_tree: Some(InfoProcessTree {
                notice: "Snapshot".into(),
                sections: vec![
                    InfoProcessSection {
                        id: owner,
                        depth: 0,
                        header: "xfce4-terminal (pid 1) - /usr/bin/xfce4-terminal".into(),
                        default_open: true,
                        metadata_sections: Vec::new(),
                        rows: vec![InfoPopupRow {
                            label: "Window PID".into(),
                            value: "1".into(),
                            searched: false,
                            separator_before: false,
                            binary_is_current: None,
                        }],
                    },
                    InfoProcessSection {
                        id: fish,
                        depth: 1,
                        header: "fish (pid 2) - /usr/bin/fish".into(),
                        default_open: false,
                        metadata_sections: Vec::new(),
                        rows: vec![InfoPopupRow {
                            label: "Process name".into(),
                            value: "fish".into(),
                            searched: true,
                            separator_before: false,
                            binary_is_current: None,
                        }],
                    },
                ],
            }),
        }
    }

    #[test]
    fn info_tree_headings_and_selectable_headers_render_strikes_and_keep_raw_copying() {
        for mark in ['\u{0335}', '\u{0336}'] {
            let ctx = egui::Context::default();
            let struck: String = "codex".chars().flat_map(|ch| [ch, mark]).collect();
            let mut data = tree_info();
            data.heading = format!("{struck} - ~/tasks/test - Terminal");
            let section = &mut data.process_tree.as_mut().unwrap().sections[0];
            section.header = format!("{struck} (pid 1) - /local/codex").into();
            let header_id = section.id.with("process-header-text");
            let (output, _) = tree_frame(&ctx, &data, Vec::new());
            for expected in [
                "codex - ~/tasks/test - Terminal",
                "codex (pid 1) - /local/codex",
            ] {
                assert!(
                    output.shapes.iter().any(|shape| matches!(&shape.shape,
                        egui::Shape::Text(text) if text.galley.job.text == expected
                            && text.galley.job.sections.iter().any(|section|
                                &text.galley.job.text[section.byte_range.clone()] == "codex"
                                    && section.format.strikethrough.width > 0.0)
                    )),
                    "{expected}"
                );
            }
            let mut state = egui::text_edit::TextEditState::load(&ctx, header_id).unwrap();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(5),
                )));
            state.store(&ctx, header_id);
            ctx.memory_mut(|memory| memory.request_focus(header_id));
            let (output, _) = tree_frame(&ctx, &data, vec![egui::Event::Copy]);
            assert!(output.platform_output.commands.iter().any(|command|
                matches!(command, egui::OutputCommand::CopyText(value) if value == &struck)
            ));
        }
    }

    fn measured_info_size(
        ctx: &egui::Context,
        data: &InfoPopupData,
        monitor: egui::Vec2,
    ) -> [f32; 2] {
        let mut input = egui::RawInput::default();
        input.viewports.insert(
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                monitor_size: Some(monitor),
                ..Default::default()
            },
        );
        let mut size = [0.0; 2];
        let _ = ctx.run(input, |ctx| {
            size = info_popup_initial_size(
                ctx,
                egui::ViewportId::from_hash_of("sized-info"),
                data,
                [1200.0, 800.0],
                [520.0, 360.0],
            );
        });
        size
    }

    #[test]
    fn natural_info_height_adds_eight_pixels_to_the_actual_visible_tree() {
        for scale in [1.0, 2.0] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(scale);
            let mut data = tree_info();
            let tree = data.process_tree.as_mut().unwrap();
            tree.sections[0].rows = (0..28)
                .map(|index| InfoPopupRow {
                    label: format!("Metadata {index}"),
                    value: "Value".into(),
                    searched: false,
                    separator_before: false,
                    binary_is_current: None,
                })
                .collect();
            tree.sections[1].rows = vec![tree.sections[0].rows[0].clone(); 560];
            let size = measured_info_size(&ctx, &data, egui::vec2(3840.0, 2160.0));
            assert_eq!(size[0], 1200.0);
            assert!(size[1] > 360.0 && size[1] < 800.0, "{size:?}");
            let mut measured_content = 0.0;
            for _ in 0..2 {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size.into())),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default()
                            .frame(
                                egui::Frame::new()
                                    .inner_margin(egui::Margin::same(INFO_POPUP_MARGIN)),
                            )
                            .show(ctx, |ui| {
                                measured_content =
                                    render_info_popup_scroll_area(ui, &data).content_size.y;
                            });
                    },
                );
            }
            let padding = 2.0 * f32::from(INFO_POPUP_MARGIN);
            assert!(
                (size[1] - padding - measured_content - 8.0).abs() <= 1.0,
                "size={size:?}, actual content={measured_content}, scale={scale}"
            );
        }
    }

    #[test]
    fn natural_info_height_respects_fonts_monitor_limits_and_existing_collapse_state() {
        let ctx = egui::Context::default();
        let mut data = tree_info();
        let tree = data.process_tree.as_mut().unwrap();
        tree.sections[0].rows = vec![tree.sections[0].rows[0].clone(); 28];
        let owner_id = tree.sections[0].id;
        let normal = measured_info_size(&ctx, &data, egui::vec2(3840.0, 2160.0));
        ctx.style_mut(|style| {
            style
                .text_styles
                .get_mut(&egui::TextStyle::Monospace)
                .unwrap()
                .size = 24.0;
        });
        let larger = measured_info_size(&ctx, &data, egui::vec2(3840.0, 2160.0));
        assert!(larger[1] > normal[1], "{normal:?} -> {larger:?}");
        let capped = measured_info_size(&ctx, &data, egui::vec2(900.0, 700.0));
        assert_eq!(capped, [868.0, 604.0]);
        let mut state =
            egui::collapsing_header::CollapsingState::load_with_default_open(&ctx, owner_id, true);
        state.set_open(false);
        state.store(&ctx);
        let collapsed = measured_info_size(&ctx, &data, egui::vec2(3840.0, 2160.0));
        assert!(collapsed[1] < normal[1]);
        assert_eq!(collapsed, [1200.0, 368.0]);
        assert!(!process_info_section_is_open(&ctx, owner_id, true));
    }

    fn tree_frame(
        ctx: &egui::Context,
        data: &InfoPopupData,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Vec<egui::text_edit::TextEditOutput>) {
        tree_viewport_frame(ctx, egui::ViewportId::ROOT, data, events)
    }

    fn tree_viewport_frame(
        ctx: &egui::Context,
        viewport_id: egui::ViewportId,
        data: &InfoPopupData,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Vec<egui::text_edit::TextEditOutput>) {
        let mut editors = Vec::new();
        let mut input = egui::RawInput {
            viewport_id,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 1200.0),
            )),
            events,
            ..Default::default()
        };
        input.viewports.insert(
            viewport_id,
            egui::ViewportInfo {
                parent: (viewport_id != egui::ViewportId::ROOT).then_some(egui::ViewportId::ROOT),
                ..Default::default()
            },
        );
        let output = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                editors = render_info_popup_panel(ui, data);
            });
        });
        (output, editors)
    }

    #[test]
    fn process_header_text_is_selectable_and_copyable_across_viewport_and_metadata_updates() {
        for scale in [1.0, 2.0] {
            for open in [false, true] {
                let ctx = egui::Context::default();
                ctx.set_pixels_per_point(scale);
                ctx.set_embed_viewports(false);
                ctx.style_mut(|style| style.animation_time = 0.0);
                let viewport = egui::ViewportId::from_hash_of("info-selection");
                let root_frame = || {
                    ctx.run(egui::RawInput::default(), |ctx| {
                        ctx.show_viewport_deferred(
                            viewport,
                            egui::ViewportBuilder::default(),
                            |_, _| {},
                        );
                    })
                };
                root_frame();
                let mut data = tree_info();
                let section = &mut data.process_tree.as_mut().unwrap().sections[0];
                section.default_open = open;
                let text = "fish (pid 946747) - /home/lewis/repos/fish-shell/target/release/fish (deleted)";
                section.header = text.into();
                let section_id = section.id;
                let header_id = section_id.with("process-header-text");
                let (output, _) = tree_viewport_frame(&ctx, viewport, &data, Vec::new());
                let (position, galley) = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(shape) if shape.galley.job.text == text => {
                            Some((shape.pos, shape.galley.clone()))
                        }
                        _ => None,
                    })
                    .expect("header text must be displayed in full");
                let cursor_position = |index| {
                    position
                        + galley
                            .pos_from_cursor(egui::text::CCursor::new(index))
                            .center()
                            .to_vec2()
                };
                let start = cursor_position(0);
                let end = cursor_position(text.chars().count());
                let pointer = |pos, pressed| egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                };
                tree_viewport_frame(
                    &ctx,
                    viewport,
                    &data,
                    vec![egui::Event::PointerMoved(start), pointer(start, true)],
                );
                tree_viewport_frame(&ctx, viewport, &data, vec![egui::Event::PointerMoved(end)]);
                tree_viewport_frame(&ctx, viewport, &data, vec![pointer(end, false)]);
                let selection = || {
                    egui::text_edit::TextEditState::load(&ctx, header_id)
                        .expect("header needs its own persistent text selection")
                        .cursor
                        .char_range()
                        .unwrap()
                        .as_sorted_char_range()
                };
                assert_eq!(selection(), 0..text.chars().count());
                assert_eq!(process_info_section_is_open(&ctx, section_id, open), open);

                // The launcher's root viewport keeps repainting while native info is open.
                root_frame();
                data.heading = "New spinner frame".into();
                data.process_tree.as_mut().unwrap().sections[0]
                    .rows
                    .push(InfoPopupRow {
                        label: "Version".into(),
                        value: "Background probe finished".into(),
                        searched: false,
                        separator_before: false,
                        binary_is_current: None,
                    });
                let (output, _) =
                    tree_viewport_frame(&ctx, viewport, &data, vec![egui::Event::Copy]);
                assert_eq!(selection(), 0..text.chars().count());
                assert!(output.platform_output.commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyText(copied) if copied == text)));
                assert_eq!(process_info_section_is_open(&ctx, section_id, open), open);
                let (output, _) = tree_viewport_frame(
                    &ctx,
                    viewport,
                    &data,
                    vec![egui::Event::Text("MUST NOT EDIT".into())],
                );
                assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(shape) if shape.galley.job.text == text)));
            }
        }
    }

    #[test]
    fn process_dropdowns_use_compact_depth_for_arrows_headers_and_details() {
        for pixels_per_point in [1.0, 2.0] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(pixels_per_point);
            ctx.style_mut(|style| style.animation_time = 0.0);
            let mut data = tree_info();
            let sections = &mut data.process_tree.as_mut().unwrap().sections;
            sections[1].default_open = true;
            let mut grandchild = sections[1].clone();
            grandchild.id = process_info_section_id(
                "window",
                ProcessIdentity {
                    pid: 3,
                    start_ticks: 30,
                },
            );
            grandchild.depth = 2;
            grandchild.header = "codex (pid 3) - /usr/bin/codex".into();
            let mut sibling = sections[1].clone();
            sibling.id = process_info_section_id(
                "window",
                ProcessIdentity {
                    pid: 4,
                    start_ticks: 40,
                },
            );
            sibling.header = "helper (pid 4) - /usr/bin/helper".into();
            sections.extend([grandchild, sibling]);
            tree_frame(&ctx, &data, Vec::new());
            let (output, editors) = tree_frame(&ctx, &data, Vec::new());
            let sections = &data.process_tree.as_ref().unwrap().sections;
            assert_eq!(editors.len(), sections.len());
            let root_x = ctx.read_response(sections[0].id).unwrap().rect.left();
            for (section, editor) in sections.iter().zip(&editors) {
                let arrow = ctx.read_response(section.id).unwrap().rect;
                let indent = arrow.left() - root_x;
                assert!(
                    (indent - section.depth as f32 * 12.0).abs() < 0.1,
                    "Arrow must follow the compact tree depth: depth={}, indent={indent}",
                    section.depth
                );
                let header_x = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::epaint::Shape::Text(text)
                            if text.galley.job.text == section.header.as_ref() =>
                        {
                            Some(text.pos.x)
                        }
                        _ => None,
                    })
                    .expect("process header must be visible");
                assert!(
                    (header_x - arrow.right()).abs() < 0.1,
                    "Arrow must sit beside its own header"
                );
                assert!(
                    (editor.galley_pos.x - header_x).abs() < 0.1,
                    "Details must align beneath their process header"
                );
            }
        }
    }

    #[test]
    fn process_dropdowns_are_independent_and_keep_the_hierarchy_visible() {
        let ctx = egui::Context::default();
        ctx.style_mut(|style| style.animation_time = 0.0);
        let mut data = tree_info();
        let (_, editors) = tree_frame(&ctx, &data, Vec::new());
        assert_eq!(editors.len(), 1);
        assert!(editors[0].galley.job.text.contains("Window PID"));
        let tree = data.process_tree.as_ref().unwrap();
        let owner = tree.sections[0].id;
        let fish = tree.sections[1].id;
        assert!(ctx.read_response(owner).is_some());
        let position = ctx.read_response(fish).unwrap().rect.center();
        let event = |pressed| egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        tree_frame(
            &ctx,
            &data,
            vec![egui::Event::PointerMoved(position), event(true)],
        );
        let (_, editors) = tree_frame(&ctx, &data, vec![event(false)]);
        assert_eq!(editors.len(), 2);
        assert!(process_info_section_is_open(&ctx, owner, true));
        assert!(process_info_section_is_open(&ctx, fish, false));
        assert_ne!(editors[0].response.id, editors[1].response.id);
        assert!(!editors[1].galley.job.text.contains("Window PID"));
        data.heading = "pi - \u{280b} work - Terminal".into();
        let (_, editors) = tree_frame(&ctx, &data, Vec::new());
        assert_eq!(editors.len(), 2);
        assert_ne!(
            fish,
            process_info_section_id(
                "window",
                ProcessIdentity {
                    pid: 2,
                    start_ticks: 21
                }
            )
        );
        let mut state = egui::collapsing_header::CollapsingState::load(&ctx, owner).unwrap();
        state.set_open(false);
        state.store(&ctx);
        let (_, editors) = tree_frame(&ctx, &data, Vec::new());
        assert_eq!(editors.len(), 1);
        assert!(ctx.read_response(fish).is_some());
        assert!(editors[0].galley.job.text.contains("fish"));
    }

    #[test]
    fn selecting_and_copying_across_rows_in_a_process_dropdown_survives_title_updates() {
        let ctx = egui::Context::default();
        ctx.style_mut(|style| style.animation_time = 0.0);
        let mut data = tree_info();
        data.process_tree.as_mut().unwrap().sections[0]
            .rows
            .push(InfoPopupRow {
                label: "Raw window title".into(),
                value: "Original title - Terminal".into(),
                searched: false,
                separator_before: false,
                binary_is_current: None,
            });
        let (_, editors) = tree_frame(&ctx, &data, Vec::new());
        let editor = &editors[0];
        let position = |index| {
            editor.galley_pos
                + editor
                    .galley
                    .pos_from_cursor(egui::text::CCursor::new(index))
                    .center()
                    .to_vec2()
        };
        let start = position(0);
        let end = position(editor.galley.job.text.chars().count() - 1);
        let pointer = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        tree_frame(
            &ctx,
            &data,
            vec![egui::Event::PointerMoved(start), pointer(start, true)],
        );
        tree_frame(&ctx, &data, vec![egui::Event::PointerMoved(end)]);
        let (_, editors) = tree_frame(&ctx, &data, vec![pointer(end, false)]);
        let range = editors[0]
            .state
            .cursor
            .char_range()
            .unwrap()
            .as_sorted_char_range();
        let selected: String = editors[0]
            .galley
            .job
            .text
            .chars()
            .skip(range.start)
            .take(range.end - range.start)
            .collect();
        assert!(selected.contains("Window PID"));
        assert!(selected.contains("Raw window title"));
        data.heading = "pi - \u{280b} work - Terminal".into();
        let (output, editors) = tree_frame(&ctx, &data, vec![egui::Event::Copy]);
        assert_eq!(
            editors[0]
                .state
                .cursor
                .char_range()
                .unwrap()
                .as_sorted_char_range(),
            range
        );
        assert!(output.platform_output.commands.iter().any(
            |command| matches!(command, egui::OutputCommand::CopyText(text) if text == &selected)
        ));
    }

    #[test]
    fn hosted_metadata_dropdowns_preserve_selection_copying_and_independent_state() {
        use crate::models::InfoMetadataSection;
        let ctx = egui::Context::default();
        ctx.style_mut(|style| style.animation_time = 0.0);
        let mut data = tree_info();
        let process = &mut data.process_tree.as_mut().unwrap().sections[0];
        process.metadata_sections = [
            (
                "hosted-application",
                "Application: Pi",
                "Application package",
                "@earendil-works/pi-coding-agent",
            ),
            ("runtime", "Runtime: Node.js", "Runtime version", "v26.4.0"),
        ]
        .into_iter()
        .map(|(key, header, label, value)| InfoMetadataSection {
            id: process.id.with(key),
            header: header.into(),
            default_open: true,
            rows: vec![InfoPopupRow {
                label: label.into(),
                value: value.into(),
                searched: false,
                separator_before: false,
                binary_is_current: None,
            }],
        })
        .collect();
        let app_id = process.metadata_sections[0].id;
        let runtime_id = process.metadata_sections[1].id;
        let (_, documents) = tree_frame(&ctx, &data, Vec::new());
        assert_eq!(documents.len(), 3);
        let runtime_editor_id = documents[2].response.id;
        let editor = &documents[2];
        let position = |index| {
            editor.galley_pos
                + editor
                    .galley
                    .pos_from_cursor(egui::text::CCursor::new(index))
                    .center()
                    .to_vec2()
        };
        let start = position(0);
        let end = position(editor.galley.job.text.chars().count() - 1);
        let pointer = |pos, pressed| egui::Event::PointerButton {
            pos,
            pressed,
            button: egui::PointerButton::Primary,
            modifiers: egui::Modifiers::NONE,
        };
        tree_frame(
            &ctx,
            &data,
            vec![egui::Event::PointerMoved(start), pointer(start, true)],
        );
        tree_frame(&ctx, &data, vec![egui::Event::PointerMoved(end)]);
        tree_frame(&ctx, &data, vec![pointer(end, false)]);
        let (copied, _) = tree_frame(&ctx, &data, vec![egui::Event::Copy]);
        assert!(copied.platform_output.commands.iter().any(|command| matches!(command,
            egui::OutputCommand::CopyText(text) if text.contains("Runtime version") && text.contains("v26.4.0"))));
        let document = copy_shown_info(&data);
        assert!(document.contains("Application: Pi") && document.contains("Runtime: Node.js"));
        assert!(
            document.contains("@earendil-works/pi-coding-agent") && document.contains("v26.4.0")
        );
        assert_eq!(
            document.matches("(pid ").count(),
            2,
            "metadata groups are not extra processes"
        );
        let mut state =
            egui::collapsing_header::CollapsingState::load_with_default_open(&ctx, app_id, true);
        state.set_open(false);
        state.store(&ctx);
        let (_, documents) = tree_frame(&ctx, &data, Vec::new());
        assert_eq!(documents.len(), 2);
        assert_eq!(documents[1].response.id, runtime_editor_id);
        assert!(process_info_section_is_open(&ctx, runtime_id, true));
        assert!(!process_info_section_is_open(&ctx, app_id, true));
    }

    #[test]
    fn copying_process_sections_includes_headers_and_each_sections_own_fields() {
        let data = tree_info();
        let document = copy_shown_info(&data);
        assert!(
            document
                .contains("xfce4-terminal (pid 1) - /usr/bin/xfce4-terminal\n    Window PID  1")
        );
        assert!(document.contains("  fish (pid 2) - /usr/bin/fish\n      Process name  fish"));
    }
}
