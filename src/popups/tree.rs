use eframe::egui;

use super::{
    InfoText, PROCESS_TREE_INDENT, info_popup_layout, info_process_header_layout, info_text_layout,
    render_info_text,
};
use crate::models::{InfoPopupData, InfoPopupRow, InfoUpdateTarget};

struct Toggle {
    id: egui::Id,
    cursor: usize,
    end: usize,
    indent: f32,
    open: bool,
    update: Option<UpdateLink>,
}

struct UpdateLink {
    start: usize,
    end: usize,
    target: InfoUpdateTarget,
}

struct Document {
    job: egui::text::LayoutJob,
    cursor: usize,
    toggles: Vec<Toggle>,
    dividers: Vec<usize>,
    font: egui::FontId,
    space_width: f32,
    ctx: egui::Context,
}

impl Document {
    fn append(&mut self, text: &str, leading: f32, format: egui::TextFormat) {
        self.job.append(text, leading, format);
        self.cursor += text.chars().count();
    }

    fn gap(&mut self, height: f32) {
        if height > 0.0 {
            let mut format = egui::TextFormat::simple(self.font.clone(), egui::Color32::WHITE);
            format.line_height = Some(height);
            self.append("\n", 0.0, format);
        }
    }

    fn block(
        &mut self,
        job: egui::text::LayoutJob,
        spaces: usize,
        indent: f32,
        leading: f32,
        dividers: &[usize],
    ) {
        let mut start = 0;
        let mut source_cursor = 0;
        let mut section_index = 0;
        for line in job.text.split_inclusive('\n') {
            if dividers.contains(&source_cursor) {
                self.dividers.push(self.cursor + spaces);
            }
            let mut pending_leading = leading;
            if spaces > 0 {
                let mut font = self.font.clone();
                // Keep plain-text tree indentation without widening the existing 12px levels.
                font.size *= (indent - leading) / (spaces as f32 * self.space_width);
                let width =
                    self.ctx.fonts_mut(|fonts| fonts.glyph_width(&font, ' ')) * spaces as f32;
                pending_leading += indent - leading - width;
                self.append(
                    &" ".repeat(spaces),
                    pending_leading,
                    egui::TextFormat::simple(font, egui::Color32::WHITE),
                );
                pending_leading = 0.0;
            }
            let end = start + line.len();
            while section_index < job.sections.len()
                && job.sections[section_index].byte_range.end <= start
            {
                section_index += 1;
            }
            for section in &job.sections[section_index..] {
                if section.byte_range.start >= end {
                    break;
                }
                let left = start.max(section.byte_range.start);
                let right = end.min(section.byte_range.end);
                if left < right {
                    self.append(
                        &job.text[left..right],
                        pending_leading
                            + if left == section.byte_range.start {
                                section.leading_space
                            } else {
                                0.0
                            },
                        section.format.clone(),
                    );
                    pending_leading = 0.0;
                }
            }
            start = end;
            source_cursor += line.chars().count();
        }
        if !job.text.is_empty() && !job.text.ends_with('\n') {
            self.append(
                "\n",
                0.0,
                egui::TextFormat::simple(self.font.clone(), egui::Color32::WHITE),
            );
        }
    }

    fn rows(&mut self, rows: &[InfoPopupRow], depth: usize, level: usize, gutter: f32) {
        let (job, dividers) = info_popup_layout("", "", rows, self.font.clone(), self.font.clone());
        self.block(
            job,
            depth * 2 + level * 4,
            depth as f32 * PROCESS_TREE_INDENT + level as f32 * gutter,
            0.0,
            &dividers,
        );
    }

    fn header(
        &mut self,
        ui: &egui::Ui,
        id: egui::Id,
        header: &str,
        current: Option<bool>,
        depth: usize,
        level: usize,
        default_open: bool,
        update_target: Option<&InfoUpdateTarget>,
    ) -> usize {
        let state = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            id,
            default_open,
        );
        let open = state.is_open();
        state.store(ui.ctx());
        let gutter = ui.spacing().indent;
        let spaces = depth * 2 + level * 4;
        let cursor = self.cursor + spaces;
        let mut job = info_process_header_layout(
            header,
            current,
            self.font.clone(),
            ui.visuals().text_color(),
        );
        let height = ui
            .fonts_mut(|fonts| fonts.row_height(&self.font))
            .max(ui.spacing().interact_size.y);
        let update = update_target
            .filter(|target| current == Some(false) || matches!(target, InfoUpdateTarget::CopyQ(_)))
            .map(|target| {
                job.append(
                    "  ",
                    0.0,
                    egui::TextFormat::simple(self.font.clone(), ui.visuals().text_color()),
                );
                let start = cursor + job.text.chars().count();
                let mut format =
                    egui::TextFormat::simple(self.font.clone(), ui.visuals().hyperlink_color);
                format.underline = egui::Stroke::new(1.0, format.color);
                job.append("Update", 0.0, format);
                UpdateLink {
                    start,
                    end: start + "Update".len(),
                    target: target.clone(),
                }
            });
        for section in &mut job.sections {
            section.format.line_height = Some(height);
            section.format.valign = egui::Align::Center;
        }
        self.block(
            job,
            spaces,
            depth as f32 * PROCESS_TREE_INDENT + (level + 1) as f32 * gutter,
            gutter,
            &[],
        );
        self.toggles.push(Toggle {
            id,
            cursor,
            end: self.cursor,
            indent: gutter,
            open,
            update,
        });
        self.toggles.len() - 1
    }
}

fn build(ui: &egui::Ui, data: &InfoPopupData) -> Document {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let space_width = ui.fonts_mut(|fonts| fonts.glyph_width(&font, ' ')).max(1.0);
    let mut document = Document {
        job: Default::default(),
        cursor: 0,
        toggles: Vec::new(),
        dividers: Vec::new(),
        font: font.clone(),
        space_width,
        ctx: ui.ctx().clone(),
    };
    for (text, font, colour) in [
        (
            data.heading.as_str(),
            egui::TextStyle::Heading.resolve(ui.style()),
            ui.visuals().text_color(),
        ),
        (
            data.subtitle.as_str(),
            egui::TextStyle::Body.resolve(ui.style()),
            ui.visuals().weak_text_color(),
        ),
    ] {
        document.block(info_text_layout(text, font, colour), 0, 0.0, 0.0, &[]);
        document.gap(ui.spacing().item_spacing.y);
    }
    document.gap(8.0);
    let tree = data.process_tree.as_ref().unwrap();
    let mut section_gap = 0.0;
    for section in &tree.sections {
        document.gap(section_gap);
        let toggle = document.header(
            ui,
            section.id,
            &section.header,
            section.binary_is_current,
            section.depth,
            0,
            section.default_open,
            section.update_target.as_ref(),
        );
        if document.toggles[toggle].open {
            document.gap(ui.spacing().item_spacing.y);
            document.rows(&section.rows, section.depth, 1, ui.spacing().indent);
            if section.rows.is_empty() && section.metadata_sections.is_empty() {
                document.block(
                    info_text_layout(
                        "Loading process details...",
                        font.clone(),
                        ui.visuals().text_color(),
                    ),
                    section.depth * 2 + 4,
                    section.depth as f32 * PROCESS_TREE_INDENT + ui.spacing().indent,
                    0.0,
                    &[],
                );
            }
            for (index, metadata) in section.metadata_sections.iter().enumerate() {
                if !section.rows.is_empty() || index > 0 {
                    document.gap(ui.spacing().item_spacing.y);
                }
                let nested = document.header(
                    ui,
                    metadata.id,
                    &metadata.header,
                    metadata.binary_is_current,
                    section.depth,
                    1,
                    metadata.default_open,
                    None,
                );
                if document.toggles[nested].open {
                    document.gap(ui.spacing().item_spacing.y);
                    document.rows(&metadata.rows, section.depth, 2, ui.spacing().indent);
                    if metadata.rows.is_empty() {
                        document.block(
                            info_text_layout(
                                "Loading application details...",
                                font.clone(),
                                ui.visuals().text_color(),
                            ),
                            section.depth * 2 + 8,
                            section.depth as f32 * PROCESS_TREE_INDENT + 2.0 * ui.spacing().indent,
                            0.0,
                            &[],
                        );
                    }
                }
                document.toggles[nested].end = document.cursor;
            }
        }
        document.toggles[toggle].end = document.cursor;
        section_gap = if document.toggles[toggle].open {
            8.0
        } else {
            ui.spacing().item_spacing.y
        };
    }
    document.gap(8.0);
    document.block(
        info_text_layout(
            &tree.notice,
            egui::TextStyle::Small.resolve(ui.style()),
            ui.visuals().weak_text_color(),
        ),
        0,
        0.0,
        0.0,
        &[],
    );
    document
}

pub(super) fn render(
    ui: &mut egui::Ui,
    data: &InfoPopupData,
) -> (egui::text_edit::TextEditOutput, Option<InfoUpdateTarget>) {
    let document = build(ui, data);
    let link_height = ui.fonts_mut(|fonts| fonts.row_height(&document.font));
    let text = InfoText::new(document.job);
    let output = render_info_text(ui, &text, &[]);
    let position = |cursor| {
        output.galley_pos
            + output
                .galley
                .pos_from_cursor(egui::text::CCursor::new(text.display_cursor(cursor)))
                .center()
                .to_vec2()
    };
    let clip = output.text_clip_rect.intersect(ui.clip_rect());
    let mut update_target = None;
    for toggle in document.toggles {
        let header = output
            .galley
            .pos_from_cursor(egui::text::CCursor::new(text.display_cursor(toggle.cursor)))
            .translate(output.galley_pos.to_vec2());
        let header = egui::Rect::from_min_max(
            egui::pos2(clip.left(), header.top()),
            egui::pos2(clip.right(), header.bottom()),
        );
        if output.response.contains_pointer() && ui.rect_contains_pointer(header) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
        }
        let point = position(toggle.cursor);
        let size = egui::vec2(toggle.indent, ui.spacing().interact_size.y);
        let rect = egui::Rect::from_center_size(point - egui::vec2(toggle.indent / 2.0, 0.0), size);
        let response = ui
            .interact(
                rect.shrink(2.0),
                toggle.id.with("tree-arrow"),
                egui::Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        egui::collapsing_header::paint_default_icon(
            ui,
            if toggle.open { 1.0 } else { 0.0 },
            &response,
        );
        if toggle.open {
            let bottom = position(toggle.end.saturating_sub(1)).y + ui.spacing().item_spacing.y;
            ui.painter().with_clip_rect(clip).line_segment(
                [
                    egui::pos2(rect.center().x, rect.bottom()),
                    egui::pos2(rect.center().x, bottom),
                ],
                ui.visuals().widgets.noninteractive.bg_stroke,
            );
        }
        if response.clicked() {
            let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                toggle.id,
                toggle.open,
            );
            state.toggle(ui);
            state.store(ui.ctx());
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
        }
        if let Some(link) = toggle.update {
            let start = position(link.start);
            let end = position(link.end);
            let rect = egui::Rect::from_center_size(
                egui::pos2((start.x + end.x) * 0.5, start.y),
                egui::vec2(end.x - start.x, link_height),
            );
            let response = ui
                .interact(
                    rect.intersect(clip),
                    toggle.id.with("terminal-update-link"),
                    egui::Sense::click(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(match &link.target {
                    InfoUpdateTarget::TerminalWindow(_) => "Update this terminal window or all outdated terminal windows",
                    InfoUpdateTarget::CopyQ(_) => "Restart CopyQ's user autostart service to load the installed application",
                    InfoUpdateTarget::Program { .. } => "Update only this program in its existing terminal tab or tmux pane",
                    InfoUpdateTarget::TmuxServer { .. } => "Update this tmux server and restart all its sessions in the existing terminal windows",
                });
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Link, ui.is_enabled(), "Update")
            });
            if response.clicked() {
                update_target = Some(link.target);
            }
        }
    }
    for cursor in document.dividers {
        let point = position(cursor);
        ui.painter().with_clip_rect(clip).line_segment(
            [point, egui::pos2(clip.right(), point.y)],
            ui.visuals().widgets.noninteractive.bg_stroke,
        );
    }
    (output, update_target)
}
