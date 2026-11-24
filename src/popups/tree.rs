use eframe::egui;

use super::{
    InfoText, PROCESS_TREE_INDENT, append_info_text, binary_current_color, info_field_label_color,
    info_popup_layout_with_label_width, info_process_header_layout, info_text_layout,
    render_info_text,
};
use crate::models::{
    InfoFieldScope, InfoMetadataSection, InfoPopupData, InfoPopupRow, InfoProgram, InfoUpdateTarget,
};

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
    label: &'static str,
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
        self.rows_with_label_width(rows, depth, level, gutter, 0);
    }

    fn rows_with_label_width(
        &mut self,
        rows: &[InfoPopupRow],
        depth: usize,
        level: usize,
        gutter: f32,
        label_width: usize,
    ) {
        let (job, dividers) = info_popup_layout_with_label_width(
            "",
            "",
            rows,
            self.font.clone(),
            self.font.clone(),
            label_width,
        );
        self.block(
            job,
            depth * 2 + level * 4,
            depth as f32 * PROCESS_TREE_INDENT + level as f32 * gutter,
            0.0,
            &dividers,
        );
    }

    fn text_width(&self, text: &str) -> f32 {
        let text = InfoText::new(info_text_layout(
            text,
            self.font.clone(),
            egui::Color32::WHITE,
        ));
        self.ctx
            .fonts_mut(|fonts| fonts.layout_job(text.job).size().x)
    }

    fn value_column(
        &self,
        rows: &[InfoPopupRow],
        depth: usize,
        level: usize,
        gutter: f32,
    ) -> Option<f32> {
        let row = rows
            .iter()
            .find(|row| row.label == "Capabilities")
            .or_else(|| rows.first())?;
        let label_width = rows
            .iter()
            .map(|row| row.label.chars().count())
            .max()
            .unwrap_or(0);
        // Match info_popup_layout's actual padded label, including font fallback widths.
        let label = format!("{:<label_width$}  ", row.label);
        Some(depth as f32 * PROCESS_TREE_INDENT + level as f32 * gutter + self.text_width(&label))
    }

    fn metadata_field_layout(
        &self,
        section: &InfoMetadataSection,
        header_x: f32,
        parent_value_column: Option<f32>,
        base: egui::Color32,
    ) -> Option<egui::text::LayoutJob> {
        let field = section.field_header.as_ref()?;
        if field.value_range.is_empty() {
            return None;
        }
        let prefix = section.header.get(..field.value_range.start)?;
        let value = section.header.get(field.value_range.clone())?;
        let suffix = section.header.get(field.value_range.end..)?;
        let mut job = info_text_layout(
            prefix,
            self.font.clone(),
            info_field_label_color(field.label_scope),
        );
        let value_section = job.sections.len();
        append_info_text(
            &mut job,
            value,
            egui::TextFormat::simple(
                self.font.clone(),
                binary_current_color(section.binary_is_current).unwrap_or(base),
            ),
        );
        if let Some(value) = job.sections.get_mut(value_section) {
            // Field-like dropdowns share their parent's value column.
            value.leading_space = parent_value_column
                .map(|column| (column - header_x - self.text_width(prefix)).max(0.0))
                .unwrap_or(0.0);
        }
        append_info_text(
            &mut job,
            suffix,
            egui::TextFormat::simple(self.font.clone(), base),
        );
        Some(job)
    }

    fn header(
        &mut self,
        ui: &egui::Ui,
        id: egui::Id,
        header: &str,
        current: Option<bool>,
        script: Option<&crate::models::InfoScriptHeader>,
        header_scope: Option<InfoFieldScope>,
        layout: Option<egui::text::LayoutJob>,
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
        let field_header = layout.is_some();
        let mut job = layout.unwrap_or_else(|| {
            info_process_header_layout(
                header,
                current,
                script,
                self.font.clone(),
                header_scope.map_or_else(|| ui.visuals().text_color(), info_field_label_color),
            )
        });
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
                let label = match target {
                    InfoUpdateTarget::Program {
                        application: InfoProgram::TmuxClient,
                        ..
                    } => "Update client",
                    _ => "Update",
                };
                job.append(label, 0.0, format);
                UpdateLink {
                    start,
                    end: start + label.len(),
                    target: target.clone(),
                    label,
                }
            });
        for section in &mut job.sections {
            section.format.line_height = Some(height);
            section.format.valign = egui::Align::Center;
        }
        self.block(
            job,
            spaces,
            depth as f32 * PROCESS_TREE_INDENT
                + (level + usize::from(!field_header)) as f32 * gutter,
            if field_header { 0.0 } else { gutter },
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

    fn metadata(
        &mut self,
        ui: &egui::Ui,
        section: &InfoMetadataSection,
        depth: usize,
        level: usize,
        parent_value_column: Option<f32>,
    ) {
        let layout = self.metadata_field_layout(
            section,
            depth as f32 * PROCESS_TREE_INDENT + level as f32 * ui.spacing().indent,
            parent_value_column,
            ui.visuals().text_color(),
        );
        let toggle = self.header(
            ui,
            section.id,
            &section.header,
            section.binary_is_current,
            None,
            section.header_scope,
            layout,
            depth,
            level,
            section.default_open,
            None,
        );
        if self.toggles[toggle].open {
            self.gap(ui.spacing().item_spacing.y);
            self.rows(&section.rows, depth, level + 1, ui.spacing().indent);
            let value_column =
                self.value_column(&section.rows, depth, level + 1, ui.spacing().indent);
            if section.rows.is_empty() && section.children.is_empty() {
                self.block(
                    info_text_layout(
                        "Loading application details...",
                        self.font.clone(),
                        ui.visuals().text_color(),
                    ),
                    depth * 2 + (level + 1) * 4,
                    depth as f32 * PROCESS_TREE_INDENT + (level + 1) as f32 * ui.spacing().indent,
                    0.0,
                    &[],
                );
            }
            for (index, child) in section.children.iter().enumerate() {
                if index > 0 {
                    self.gap(ui.spacing().item_spacing.y);
                }
                self.metadata(ui, child, depth, level + 1, value_column);
            }
        }
        self.toggles[toggle].end = self.cursor;
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
            section.script_header.as_ref(),
            None,
            None,
            section.depth,
            0,
            section.default_open,
            section.update_target.as_ref(),
        );
        if document.toggles[toggle].open {
            document.gap(ui.spacing().item_spacing.y);
            let value_column =
                document.value_column(&section.rows, section.depth, 1, ui.spacing().indent);
            let label_width = section
                .rows
                .iter()
                .map(|row| row.label.chars().count())
                .max()
                .unwrap_or(0);
            let anchor = |metadata: &InfoMetadataSection| {
                metadata
                    .field_header
                    .as_ref()
                    .and_then(|field| field.after_row)
            };
            let mut start = 0;
            for (index, row) in section.rows.iter().enumerate() {
                if section
                    .metadata_sections
                    .iter()
                    .any(|metadata| anchor(metadata) == Some(row.label.as_str()))
                {
                    document.rows_with_label_width(
                        &section.rows[start..=index],
                        section.depth,
                        1,
                        ui.spacing().indent,
                        label_width,
                    );
                    for metadata in section
                        .metadata_sections
                        .iter()
                        .filter(|metadata| anchor(metadata) == Some(row.label.as_str()))
                    {
                        document.metadata(ui, metadata, section.depth, 1, value_column);
                    }
                    start = index + 1;
                }
            }
            document.rows_with_label_width(
                &section.rows[start..],
                section.depth,
                1,
                ui.spacing().indent,
                label_width,
            );
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
            for (index, metadata) in section
                .metadata_sections
                .iter()
                .filter(|metadata| {
                    !anchor(metadata)
                        .is_some_and(|label| section.rows.iter().any(|row| row.label == label))
                })
                .enumerate()
            {
                if index > 0 {
                    document.gap(ui.spacing().item_spacing.y);
                }
                document.metadata(ui, metadata, section.depth, 1, value_column);
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
                    InfoUpdateTarget::Program { application: InfoProgram::TmuxClient, .. } => "Reconnect only this tmux client in the same terminal tab; the server, sessions and pane applications keep running",
                    InfoUpdateTarget::Program { .. } => "Update only this program in its existing terminal tab or tmux pane",
                    InfoUpdateTarget::TmuxServer { .. } => "Update this tmux server and restart all its sessions in the existing terminal windows",
                });
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Link, ui.is_enabled(), link.label)
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
