use eframe::egui;

pub(crate) mod foreground;
mod selection;
mod tree;
mod updates;

const PROCESS_TREE_INDENT: f32 = 12.0;
pub(crate) const INFO_POPUP_MARGIN: i8 = 16;
use std::sync::mpsc::Sender;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::models::{
    InfoFieldScope, InfoPopupData, InfoPopupRow, InfoUpdateTarget, PopupEvent, ProcessIdentity,
};

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

pub(crate) fn installation_badge(value: &str) -> Option<&str> {
    let value = value.trim();
    let badge = match value.strip_prefix('[') {
        Some(value) => value.split_once(']')?.0,
        None => value,
    };
    if badge.len() != 3 || !badge.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    Some(match badge {
        "FKM" => "FRK",
        "DVM" => "DEV",
        "CLX" => "CLO",
        "OPX" => "OPT",
        "SCU" => "USC",
        "BIU" => "UBI",
        "NPT" => "NUT",
        "UVT" => "UUT",
        "CAT" => "CUT",
        _ => badge,
    })
}

fn installation_type_color(value: &str) -> Option<egui::Color32> {
    let badge = installation_badge(value)?;
    Some(installation_palette_color(badge).unwrap_or(egui::Color32::from_rgb(180, 180, 180)))
}

fn installation_palette_color(badge: &str) -> Option<egui::Color32> {
    let muted = |red, green, blue| {
        let channel = |value: u8| ((2 * u16::from(value) + 3 * 144) / 5) as u8;
        egui::Color32::from_rgb(channel(red), channel(green), channel(blue))
    };
    // Mirror progs/src/labels.rs::badge_color after normalizing legacy aliases.
    Some(match badge {
        "PEP" | "PET" | "PEL" => egui::Color32::from_rgb(34, 224, 94),
        "PSP" | "PPT" => egui::Color32::from_rgb(16, 185, 129),
        "PDP" | "PDT" | "PDL" => muted(16, 185, 129),
        "AEP" | "ASP" | "AET" | "AEL" | "APT" => egui::Color32::from_rgb(217, 70, 239),
        "ADP" | "ADT" | "ADL" => muted(217, 70, 239),
        "CEP" | "CET" | "CEL" | "RUT" => egui::Color32::from_rgb(198, 91, 50),
        "CUP" | "CUT" | "CUL" => muted(198, 91, 50),
        "NEP" | "NET" | "NEL" => egui::Color32::from_rgb(100, 155, 255),
        "NDP" | "NDT" | "NDL" | "NUP" | "NUT" | "NUL" => muted(59, 130, 246),
        "UEP" | "UET" | "UEL" | "UVR" => egui::Color32::from_rgb(168, 85, 247),
        "UDP" | "UDT" | "UDL" | "UUP" | "UUT" | "UUL" => muted(168, 85, 247),
        "POP" | "AOP" => egui::Color32::from_rgb(120, 120, 128),
        "DEV" => egui::Color32::from_rgb(255, 158, 170),
        "FRK" => egui::Color32::from_rgb(34, 211, 238),
        "CLO" => egui::Color32::from_rgb(199, 210, 254),
        "UNC" => egui::Color32::from_rgb(113, 113, 122),
        "BRK" => egui::Color32::from_rgb(82, 82, 91),
        "UBI" => egui::Color32::from_rgb(217, 249, 157),
        "USC" => egui::Color32::from_rgb(255, 239, 208),
        "OPT" => egui::Color32::from_rgb(253, 224, 71),
        _ => return None,
    })
}

fn append_info_text(job: &mut egui::text::LayoutJob, text: &str, mut format: egui::TextFormat) {
    let trimmed = text.trim();
    if trimmed.len() == 3
        && let Some(color) = installation_badge(trimmed).and_then(installation_palette_color)
    {
        format.color = color;
        job.append(text, 0.0, format);
        return;
    }

    let mut cursor = 0;
    for (start, _) in text.match_indices('[') {
        let Some(badge) = text
            .get(start..start + 5)
            .filter(|badge| badge.ends_with(']'))
        else {
            continue;
        };
        let Some(color) = installation_badge(badge).and_then(installation_palette_color) else {
            continue;
        };
        if cursor < start {
            job.append(&text[cursor..start], 0.0, format.clone());
        }
        let mut badge_format = format.clone();
        badge_format.color = color;
        job.append(badge, 0.0, badge_format);
        cursor = start + badge.len();
    }
    if cursor < text.len() {
        job.append(&text[cursor..], 0.0, format);
    }
}

fn info_text_layout(
    text: &str,
    font_id: egui::FontId,
    color: egui::Color32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    append_info_text(&mut job, text, egui::TextFormat::simple(font_id, color));
    job
}

fn binary_current_color(current: Option<bool>) -> Option<egui::Color32> {
    match current {
        Some(true) => Some(egui::Color32::from_rgb(130, 225, 145)),
        Some(false) => Some(egui::Color32::from_rgb(255, 110, 110)),
        None => None,
    }
}

fn uncompiled_changes_color(value: &str) -> Option<egui::Color32> {
    let status = value
        .trim()
        .split(|character: char| !character.is_ascii_alphabetic())
        .next()?;
    if status.eq_ignore_ascii_case("Unknown") {
        Some(egui::Color32::from_rgb(255, 220, 100))
    } else if status.eq_ignore_ascii_case("No") {
        binary_current_color(Some(true))
    } else if status.eq_ignore_ascii_case("Yes") {
        binary_current_color(Some(false))
    } else {
        None
    }
}

fn append_uncompiled_changes(
    job: &mut egui::text::LayoutJob,
    value: &str,
    font: egui::FontId,
    base: egui::Color32,
) {
    if value.contains(" · ") {
        use program_metadata::build_status::{SummaryTone, uncompiled_changes_clause_tone};

        for (index, clause) in value.split(" · ").enumerate() {
            if index > 0 {
                job.append(" · ", 0.0, egui::TextFormat::simple(font.clone(), base));
            }
            let color = match uncompiled_changes_clause_tone(clause) {
                SummaryTone::Good => binary_current_color(Some(true)).unwrap(),
                SummaryTone::Bad => binary_current_color(Some(false)).unwrap(),
                SummaryTone::Warning => egui::Color32::from_rgb(255, 220, 100),
                SummaryTone::Neutral => uncompiled_changes_color(clause).unwrap_or(base),
            };
            append_info_text(job, clause, egui::TextFormat::simple(font.clone(), color));
        }
        return;
    }

    // Older receipts and non-structured failure explanations retain their status
    // colour. A caveat in a legacy sentence must not turn a clean observation red.
    let color = uncompiled_changes_color(value).unwrap_or(base);
    let caveat = [
        "added: unknown",
        "full build status unknown",
        "scan incomplete",
    ]
    .into_iter()
    .filter_map(|caveat| value.find(caveat))
    .min()
    .unwrap_or(value.len());
    append_info_text(
        job,
        &value[..caveat],
        egui::TextFormat::simple(font.clone(), color),
    );
    if caveat < value.len() {
        append_info_text(
            job,
            &value[caveat..],
            egui::TextFormat::simple(font, egui::Color32::from_rgb(255, 220, 100)),
        );
    }
}

fn info_process_header_layout(
    header: &str,
    current: Option<bool>,
    font: egui::FontId,
    base: egui::Color32,
) -> egui::text::LayoutJob {
    let name = header
        .rsplit_once(" (pid ")
        .map_or(header, |(name, _)| name);
    let mut job = info_text_layout(
        name,
        font.clone(),
        binary_current_color(current).unwrap_or(base),
    );
    append_info_text(
        &mut job,
        &header[name.len()..],
        egui::TextFormat::simple(font, base),
    );
    job
}

fn info_field_label_color(scope: InfoFieldScope) -> egui::Color32 {
    match scope {
        InfoFieldScope::Neutral => egui::Color32::from_rgba_unmultiplied(255, 255, 255, 170),
        InfoFieldScope::Window => egui::Color32::from_rgb(144, 166, 188),
        InfoFieldScope::RunningInstance => egui::Color32::from_rgb(146, 180, 160),
        InfoFieldScope::InstanceContext => egui::Color32::from_rgb(188, 152, 165),
        InfoFieldScope::Installation => egui::Color32::from_rgb(184, 167, 143),
    }
}

fn info_popup_layout(
    heading: &str,
    subtitle: &str,
    rows: &[InfoPopupRow],
    body_font: egui::FontId,
    heading_font: egui::FontId,
) -> (egui::text::LayoutJob, Vec<usize>) {
    let searchable_value_color = egui::Color32::from_rgb(255, 236, 170);
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
        append_info_text(
            &mut layout_job,
            heading,
            format(heading_font, egui::Color32::WHITE),
        );
        layout_job.append("\n", 0.0, format(body_font.clone(), egui::Color32::WHITE));
    }
    if !subtitle.is_empty() {
        append_info_text(
            &mut layout_job,
            subtitle,
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
        let label_color = info_field_label_color(row.scope);
        let value_color = binary_current_color(row.binary_is_current).unwrap_or_else(|| match row
            .label
            .as_str()
        {
            "Type" => installation_type_color(&row.value).unwrap_or(egui::Color32::WHITE),
            "Uncompiled changes" => {
                uncompiled_changes_color(&row.value).unwrap_or(egui::Color32::WHITE)
            }
            _ if row.searched => searchable_value_color,
            _ => egui::Color32::WHITE,
        });
        append_info_text(
            &mut layout_job,
            &label,
            format(body_font.clone(), label_color),
        );
        if row.label == "Uncompiled changes" {
            append_uncompiled_changes(
                &mut layout_job,
                &format!("{}\n", row.value),
                body_font.clone(),
                if row.searched {
                    searchable_value_color
                } else {
                    egui::Color32::WHITE
                },
            );
        } else {
            append_info_text(
                &mut layout_job,
                &format!("{}\n", row.value),
                format(body_font.clone(), value_color),
            );
        }
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
    render_info_text(ui, &text, &divider_cursors)
}

fn render_info_text(
    ui: &mut egui::Ui,
    text: &InfoText,
    divider_cursors: &[usize],
) -> egui::text_edit::TextEditOutput {
    let divider_cursors: Vec<_> = divider_cursors
        .iter()
        .map(|cursor| text.display_cursor(*cursor))
        .collect();
    let document = &text.job.text;
    let mut immutable_document = document.as_str();
    let mut layouter = |ui: &egui::Ui, _text: &dyn egui::TextBuffer, _wrap_width: f32| {
        let mut job = text.job.clone();
        job.wrap.max_width = f32::INFINITY;
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    };
    let id = ui.make_persistent_id(("info-popup-document", ui.ctx().viewport_id()));
    let previous_id = id.with("previous-info-text");
    let previous = ui
        .ctx()
        .data_mut(|data| data.get_temp::<Arc<str>>(previous_id));
    if previous.as_deref() != Some(document.as_str()) {
        if let Some(previous) = previous
            && let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), id)
            && let Some(range) = state.cursor.char_range()
        {
            state
                .cursor
                .set_char_range(Some(rebase_info_selection(&previous, document, range)));
            state.store(ui.ctx(), id);
        }
        ui.ctx()
            .data_mut(|data| data.insert_temp(previous_id, Arc::<str>::from(document.as_str())));
    }
    let checkpoint = text.copy_checkpoint(ui.ctx(), id);
    let before = egui::text_edit::TextEditState::load(ui.ctx(), id)
        .and_then(|state| state.cursor.char_range());
    let mut output = egui::TextEdit::multiline(&mut immutable_document)
        .id(id)
        .frame(false)
        .margin(egui::Margin::ZERO)
        .desired_width(f32::INFINITY)
        .desired_rows(1)
        .layouter(&mut layouter)
        .show(ui);
    InfoText::preserve_secondary_selection(ui, &mut output, before);
    text.restore_raw_copy(ui.ctx(), &output, checkpoint);
    text.show_copy_menu(ui, &output);
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

fn rebase_info_selection(
    old: &str,
    new: &str,
    mut range: egui::text::CCursorRange,
) -> egui::text::CCursorRange {
    let selected = range.slice_str(old);
    if !selected.is_empty()
        && let Some(start) = new.find(selected)
        && new.rfind(selected) == Some(start)
    {
        let start = new[..start].chars().count();
        let end = start + selected.chars().count();
        let forward = range.primary.index >= range.secondary.index;
        range.primary.index = if forward { end } else { start };
        range.secondary.index = if forward { start } else { end };
        return range;
    }
    let prefix = old
        .chars()
        .zip(new.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let old_len = old.chars().count();
    let new_len = new.chars().count();
    let suffix = old
        .chars()
        .rev()
        .zip(new.chars().rev())
        .take(old_len.min(new_len) - prefix)
        .take_while(|(a, b)| a == b)
        .count();
    let map = |index: usize| {
        if index <= prefix {
            index
        } else if index >= old_len - suffix {
            (new_len - suffix + index.saturating_sub(old_len - suffix)).min(new_len)
        } else {
            index.min(new_len - suffix)
        }
    };
    range.primary.index = map(range.primary.index);
    range.secondary.index = map(range.secondary.index);
    range
}

#[cfg(test)]
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

type InfoPopupOutput = (
    Vec<egui::text_edit::TextEditOutput>,
    Option<InfoUpdateTarget>,
);

pub(crate) fn render_info_popup_panel(ui: &mut egui::Ui, data: &InfoPopupData) -> InfoPopupOutput {
    if data.process_tree.is_none() {
        return (
            vec![render_info_document(
                ui,
                &data.heading,
                &data.subtitle,
                &data.rows,
            )],
            None,
        );
    }
    let (editor, update) = tree::render(ui, data);
    (vec![editor], update)
}

pub(crate) fn render_info_popup_scroll_area(
    ui: &mut egui::Ui,
    data: &InfoPopupData,
) -> egui::scroll_area::ScrollAreaOutput<InfoPopupOutput> {
    egui::ScrollArea::both()
        .id_salt(("show-info-scroll", ui.ctx().viewport_id()))
        .scroll_source(egui::scroll_area::ScrollSource {
            drag: false,
            ..egui::scroll_area::ScrollSource::ALL
        })
        .auto_shrink([false, false])
        .wheel_scroll_multiplier(egui::Vec2::splat(data.scroll_sensitivity))
        .show(ui, |ui| render_info_popup_panel(ui, data))
}

pub(crate) fn bind_terminal_update_status(
    ctx: &egui::Context,
    info: egui::ViewportId,
    window: String,
    shared: Arc<std::sync::Mutex<crate::models::TerminalMaintenanceState>>,
    error: Option<String>,
) {
    updates::bind_terminal_status(ctx, info, window, shared, error);
}

pub(crate) fn show_terminal_update_status(ui: &mut egui::Ui) {
    updates::show_terminal_status(ui, ui.ctx().viewport_id());
}

pub(crate) fn window_update_dialog_is_open(ctx: &egui::Context) -> bool {
    updates::terminal_choice_is_open(ctx, egui::ViewportId::ROOT)
}

pub(crate) fn show_window_update_dialog(
    ctx: &egui::Context,
    clicked_target: Option<InfoUpdateTarget>,
) -> Option<PopupEvent> {
    updates::show(ctx, egui::ViewportId::ROOT, clicked_target)
}

pub(crate) fn render_info_popup_contents(
    ui: &mut egui::Ui,
    data: &InfoPopupData,
    popup_id: egui::ViewportId,
    event_sender: &Sender<PopupEvent>,
) {
    updates::show_terminal_status(ui, popup_id);
    let (_, update) = render_info_popup_scroll_area(ui, data).inner;
    if let Some(event) = updates::show(ui.ctx(), popup_id, update) {
        let _ = event_sender.send(event);
        ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
    }
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
    let builder = crate::icon::viewport(viewport_id)
        .with_title(data.title.clone())
        .with_inner_size(inner_size)
        .with_min_inner_size(min_inner_size)
        .with_resizable(true)
        .with_always_on_top();

    ctx.show_viewport_deferred(viewport_id, builder, move |ctx, _class| {
        let modal_open = ctx.memory(|memory| memory.top_modal_layer().is_some());
        let close_requested = ctx.input(|input| {
            input.viewport().close_requested()
                || (!modal_open
                    && (input.key_pressed(egui::Key::Escape) || input.key_pressed(egui::Key::F10)))
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
                render_info_popup_contents(ui, &data, viewport_id, &event_sender);
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
                        let editor = &output.inner.0[0];
                        let gutter = egui::pos2(
                            output.inner_rect.left() + 2.0,
                            output.inner_rect.bottom() - 32.0,
                        );
                        let label_byte = editor.galley.job.text.find("Metadata 0").unwrap();
                        let label_cursor = editor.galley.job.text[..label_byte].chars().count();
                        let label_x = editor.galley_pos.x
                            + editor
                                .galley
                                .pos_from_cursor(egui::text::CCursor::new(label_cursor))
                                .left();
                        assert!(gutter.x < label_x);
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
                scope: InfoFieldScope::Neutral,
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
                        result = Some((output.state.offset, output.inner.0[0].response.id));
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
                    scope: InfoFieldScope::Neutral,
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
    fn inline_terminal_update_status_preserves_scroll_selection_and_expanded_sections() {
        let ctx = egui::Context::default();
        let info = egui::ViewportId::ROOT;
        let mut data = tree_info();
        let tree = data.process_tree.as_mut().unwrap();
        let child = tree.sections[1].id;
        let mut expanded =
            egui::collapsing_header::CollapsingState::load_with_default_open(&ctx, child, false);
        expanded.set_open(true);
        expanded.store(&ctx);
        tree.sections[0].rows = (0..80)
            .map(|index| InfoPopupRow {
                label: format!("Metadata {index}"),
                scope: InfoFieldScope::Neutral,
                value: "Captured value".into(),
                searched: false,
                separator_before: false,
                binary_is_current: None,
            })
            .collect();
        let shared = Arc::new(std::sync::Mutex::new(
            crate::models::TerminalMaintenanceState {
                info_viewport: Some(info),
                scope_window: Some("other-window".into()),
                error: Some("Unsupported legacy terminal; restart it once manually".into()),
                ..Default::default()
            },
        ));
        bind_terminal_update_status(&ctx, info, "fixture-window".into(), shared.clone(), None);
        let frame = || {
            let mut result = None;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(640.0, 300.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        show_terminal_update_status(ui);
                        let output = render_info_popup_scroll_area(ui, &data);
                        result = Some((output.id, output.state, output.inner.0[0].response.id));
                    });
                },
            );
            result.unwrap()
        };
        frame();
        let (scroll_id, mut scroll, editor_id) = frame();
        scroll.offset.y = 120.0;
        scroll.store(&ctx, scroll_id);
        let mut selection = egui::text_edit::TextEditState::load(&ctx, editor_id).unwrap();
        let range = egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(12),
        );
        selection.cursor.set_char_range(Some(range));
        selection.store(&ctx, editor_id);
        for visible in [true, false, true] {
            shared.lock().unwrap().scope_window = Some(
                if visible {
                    "fixture-window"
                } else {
                    "other-window"
                }
                .into(),
            );
            let (current_scroll, state, current_editor) = frame();
            assert_eq!(current_scroll, scroll_id);
            assert_eq!(current_editor, editor_id);
            assert_eq!(state.offset.y, 120.0);
            assert_eq!(
                egui::text_edit::TextEditState::load(&ctx, editor_id)
                    .unwrap()
                    .cursor
                    .char_range(),
                Some(range)
            );
            assert!(process_info_section_is_open(&ctx, child, false));
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
            assert_eq!(
                viewport.builder.app_id.as_deref(),
                Some(foreground::app_id(std::process::id(), id).as_str())
            );
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
                    scope: InfoFieldScope::Window,
                    value: "1234".into(),
                    searched: false,
                    separator_before: false,
                    binary_is_current: None,
                },
                InfoPopupRow {
                    label: "Version".into(),
                    scope: InfoFieldScope::RunningInstance,
                    value: "1.2.0".into(),
                    searched: false,
                    separator_before: false,
                    binary_is_current: None,
                },
                InfoPopupRow {
                    label: "Active process".into(),
                    scope: InfoFieldScope::RunningInstance,
                    value: "codex".into(),
                    searched: true,
                    separator_before: true,
                    binary_is_current: None,
                },
                InfoPopupRow {
                    label: "Working directory".into(),
                    scope: InfoFieldScope::RunningInstance,
                    value: "~/project".into(),
                    searched: true,
                    separator_before: false,
                    binary_is_current: None,
                },
                InfoPopupRow {
                    label: "Running executable image original path".into(),
                    scope: InfoFieldScope::RunningInstance,
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
                    editor = render_info_popup_panel(ui, data).0.into_iter().next();
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
                        scope: InfoFieldScope::Window,
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
            scope: InfoFieldScope::Window,
            value: "\u{3bb} - T\u{0336}e\u{0336}r\u{0336}m\u{0336}i\u{0336}n\u{0336}a\u{0336}l\u{0336} [FRK]".into(),
            searched: true,
            separator_before: false,
            binary_is_current: None,
        });
        data.rows.insert(
            1,
            InfoPopupRow {
                label: "Package".into(),
                scope: InfoFieldScope::Installation,
                value: "bash 5.3 [PDP]".into(),
                searched: false,
                separator_before: false,
                binary_is_current: None,
            },
        );
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
        for badge in ["[FRK]", "[PDP]"] {
            assert!(projection.job.sections.iter().any(|section| {
                &projection.job.text[section.byte_range.clone()] == badge
                    && Some(section.format.color) == installation_type_color(badge)
            }));
        }
        for (text, colour) in [
            ("Script name", egui::Color32::from_rgb(188, 152, 165)),
            ("pi-remote-qwen-radiance", egui::Color32::WHITE),
        ] {
            assert!(projection.job.sections.iter().any(|section| {
                projection.job.text[section.byte_range.clone()].trim_end() == text
                    && section.format.color == colour
            }));
        }
    }

    #[test]
    fn info_scope_colours_apply_only_to_labels_independently_of_search_and_status() {
        let cases = [
            ("Ordinary", "value", false, None, egui::Color32::WHITE),
            (
                "Searchable",
                "match",
                true,
                None,
                egui::Color32::from_rgb(255, 236, 170),
            ),
            (
                "Current",
                "current",
                false,
                Some(true),
                egui::Color32::from_rgb(130, 225, 145),
            ),
            (
                "Outdated",
                "outdated",
                true,
                Some(false),
                egui::Color32::from_rgb(255, 110, 110),
            ),
            (
                "Type",
                "FRK",
                false,
                None,
                egui::Color32::from_rgb(34, 211, 238),
            ),
        ];
        for (scope, label_colour) in [
            (
                InfoFieldScope::Neutral,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 170),
            ),
            (
                InfoFieldScope::Window,
                egui::Color32::from_rgb(144, 166, 188),
            ),
            (
                InfoFieldScope::RunningInstance,
                egui::Color32::from_rgb(146, 180, 160),
            ),
            (
                InfoFieldScope::InstanceContext,
                egui::Color32::from_rgb(188, 152, 165),
            ),
            (
                InfoFieldScope::Installation,
                egui::Color32::from_rgb(184, 167, 143),
            ),
        ] {
            let rows: Vec<_> = cases
                .iter()
                .map(|(label, value, searched, status, _)| InfoPopupRow {
                    label: (*label).into(),
                    scope,
                    value: (*value).into(),
                    searched: *searched,
                    separator_before: false,
                    binary_is_current: *status,
                })
                .collect();
            let (layout, _) = info_popup_layout(
                "",
                "",
                &rows,
                egui::FontId::monospace(12.0),
                egui::FontId::proportional(20.0),
            );
            assert_eq!(layout.sections.len(), cases.len() * 2);
            for (sections, (label, value, _, _, value_colour)) in
                layout.sections.chunks_exact(2).zip(cases)
            {
                assert_eq!(
                    layout.text[sections[0].byte_range.clone()].trim_end(),
                    label
                );
                assert_eq!(sections[0].format.color, label_colour);
                assert_eq!(
                    &layout.text[sections[1].byte_range.clone()],
                    format!("{value}\n")
                );
                assert_eq!(sections[1].format.color, value_colour);
            }
            let renamed = vec![InfoPopupRow {
                label: "Renamed field".into(),
                ..rows[0].clone()
            }];
            let (layout, _) = info_popup_layout(
                "",
                "",
                &renamed,
                egui::FontId::monospace(12.0),
                egui::FontId::proportional(20.0),
            );
            assert_eq!(layout.sections[0].format.color, label_colour);
        }
    }

    #[test]
    fn classification_values_use_the_progs_palette_in_every_field() {
        let colours = [
            ("PEP PET PEL", [34, 224, 94]),
            ("PSP PPT", [16, 185, 129]),
            ("PDP PDT PDL", [92, 160, 138]),
            ("AEP ASP AET AEL APT", [217, 70, 239]),
            ("ADP ADT ADL", [173, 114, 182]),
            ("CEP CET CEL RUT", [198, 91, 50]),
            ("CUP CUT CUL CAT", [165, 122, 106]),
            ("NEP NET NEL", [100, 155, 255]),
            ("NDP NDT NDL NUP NUT NUL NPT", [110, 138, 184]),
            ("UEP UET UEL", [168, 85, 247]),
            ("UDP UDT UDL UUP UUT UUL UVT", [153, 120, 185]),
            ("POP AOP", [120, 120, 128]),
            ("DEV DVM", [255, 158, 170]),
            ("FRK FKM", [34, 211, 238]),
            ("CLO CLX", [199, 210, 254]),
            ("UNC", [113, 113, 122]),
            ("BRK", [82, 82, 91]),
            ("UBI BIU", [217, 249, 157]),
            ("USC SCU", [255, 239, 208]),
            ("OPT OPX", [253, 224, 71]),
        ];
        for (codes, [red, green, blue]) in colours {
            for code in codes.split_whitespace() {
                let mut data = window_info();
                let value = code.to_owned();
                assert_eq!(
                    installation_type_color(&format!("[{code}] Installation classification")),
                    installation_type_color(code)
                );
                for label in ["Type", "Description"] {
                    data.rows.push(InfoPopupRow {
                        label: label.into(),
                        scope: InfoFieldScope::Installation,
                        value: value.clone(),
                        searched: false,
                        separator_before: false,
                        binary_is_current: None,
                    });
                }
                for label in ["Package", "Root package", "Other field"] {
                    data.rows.push(InfoPopupRow {
                        label: label.into(),
                        scope: InfoFieldScope::Installation,
                        value: format!("package 1.0 [{code}]"),
                        searched: false,
                        separator_before: false,
                        binary_is_current: None,
                    });
                }
                let (layout, _) = info_popup_layout(
                    "",
                    "",
                    &data.rows,
                    egui::FontId::monospace(12.0),
                    egui::FontId::proportional(20.0),
                );
                let values: Vec<_> = layout
                    .sections
                    .iter()
                    .filter(|section| {
                        layout.text[section.byte_range.clone()] == format!("{value}\n")
                    })
                    .collect();
                assert_eq!(values.len(), 2);
                assert_eq!(
                    values[0].format.color,
                    egui::Color32::from_rgb(red, green, blue),
                    "installation badge: {code}"
                );
                assert_eq!(
                    values[1].format.color,
                    egui::Color32::from_rgb(red, green, blue)
                );
                let embedded: Vec<_> = layout
                    .sections
                    .iter()
                    .filter(|section| {
                        layout.text[section.byte_range.clone()] == format!("[{code}]")
                    })
                    .collect();
                assert_eq!(embedded.len(), 3);
                assert!(embedded.iter().all(
                    |section| section.format.color == egui::Color32::from_rgb(red, green, blue)
                ));
                assert!(
                    layout
                        .sections
                        .iter()
                        .filter(|section| {
                            &layout.text[section.byte_range.clone()] == "package 1.0 "
                        })
                        .all(|section| section.format.color == egui::Color32::WHITE)
                );
                let label = layout
                    .sections
                    .iter()
                    .find(|section| layout.text[section.byte_range.clone()].starts_with("Type "))
                    .unwrap();
                assert_eq!(
                    label.format.color,
                    info_field_label_color(InfoFieldScope::Installation)
                );
                assert!(copy_shown_info(&data).contains("Type"));
                assert!(copy_shown_info(&data).contains(&value));
            }
        }
    }

    #[test]
    fn inline_badges_preserve_unicode_surroundings_and_ignore_malformed_or_unrelated_brackets() {
        let text = "\u{3bb} [PEP] [PDP][FRK] [NEW] [PE] [peP] [P\u{00c9}P] [PDP [CLO]\n";
        let base = egui::Color32::from_rgb(255, 236, 170);
        let job = info_text_layout(text, egui::FontId::monospace(12.0), base);
        assert_eq!(job.text, text);
        let mut cursor = 0;
        let mut badges = Vec::new();
        for section in &job.sections {
            assert_eq!(section.byte_range.start, cursor);
            cursor = section.byte_range.end;
            let token = &job.text[section.byte_range.clone()];
            if ["[PEP]", "[PDP]", "[FRK]", "[CLO]"].contains(&token) {
                assert_eq!(Some(section.format.color), installation_type_color(token));
                badges.push(token);
            } else {
                assert_eq!(section.format.color, base, "{token}");
            }
        }
        assert_eq!(cursor, text.len());
        assert_eq!(badges, ["[PEP]", "[PDP]", "[FRK]", "[CLO]"]);
        for text in ["[".repeat(8192), "[PEP".repeat(2048)] {
            let job = info_text_layout(&text, egui::FontId::monospace(12.0), base);
            assert_eq!(job.text, text);
            assert_eq!(job.sections.len(), 1);
            assert_eq!(job.sections[0].format.color, base);
        }
    }

    #[test]
    fn badges_are_coloured_in_tree_headings_process_headers_metadata_and_notices() {
        use crate::models::InfoMetadataSection;
        let ctx = egui::Context::default();
        let mut data = tree_info();
        data.heading = "Heading [PEP]".into();
        data.subtitle = "Subtitle [FRK]".into();
        let tree = data.process_tree.as_mut().unwrap();
        tree.notice = "Snapshot [CLO]".into();
        let owner = &mut tree.sections[0];
        owner.header = "xfce4-terminal [PDP]".into();
        owner.metadata_sections.push(InfoMetadataSection {
            id: owner.id.with("metadata"),
            header: "Application [NET]".into(),
            binary_is_current: None,
            default_open: true,
            rows: vec![InfoPopupRow {
                label: "Root package".into(),
                scope: InfoFieldScope::Installation,
                value: "root 1.0 [AEP]".into(),
                searched: false,
                separator_before: false,
                binary_is_current: None,
            }],
        });
        let (output, _) = frame(&ctx, &data, Vec::new());
        for badge in ["[PEP]", "[FRK]", "[CLO]", "[PDP]", "[NET]", "[AEP]"] {
            assert!(
                output.shapes.iter().any(|shape| matches!(&shape.shape,
                    egui::Shape::Text(text) if text.galley.job.sections.iter().any(|section| {
                        &text.galley.job.text[section.byte_range.clone()] == badge
                            && Some(section.format.color) == installation_type_color(badge)
                    })
                )),
                "{badge}"
            );
            assert!(copy_shown_info(&data).contains(badge));
        }
    }

    #[test]
    fn legacy_installation_codes_display_current_progs_names_without_guessing_unknown_roles() {
        for (legacy, current) in [
            ("FKM", "FRK"),
            ("DVM", "DEV"),
            ("CLX", "CLO"),
            ("OPX", "OPT"),
            ("SCU", "USC"),
            ("BIU", "UBI"),
            ("NPT", "NUT"),
            ("UVT", "UUT"),
            ("CAT", "CUT"),
        ] {
            assert_eq!(installation_badge(legacy), Some(current));
            assert_eq!(
                installation_badge(&format!("[{legacy}] Legacy classification")),
                Some(current)
            );
            assert_eq!(
                installation_type_color(legacy),
                installation_type_color(current)
            );
        }
    }

    #[test]
    fn installation_type_placeholders_stay_neutral_and_unknown_badges_use_progs_fallback() {
        for value in [
            "",
            "Loading...",
            "Unavailable",
            "Unknown",
            "pdp",
            "[PDP",
            "[pd]",
            "PDT tool",
        ] {
            assert_eq!(installation_type_color(value), None);
        }
        for value in ["NEW", "[NEW] Future classification"] {
            assert_eq!(
                installation_type_color(value),
                Some(egui::Color32::from_rgb(180, 180, 180))
            );
        }
    }

    #[test]
    fn pi_identity_rows_render_independent_colours_and_neutral_installed_hashes() {
        let green = egui::Color32::from_rgb(130, 225, 145);
        let red = egui::Color32::from_rgb(255, 110, 110);
        for scale in [1.0, 2.0] {
            let mut data = window_info();
            let fields = [
                (
                    "Running code fingerprint",
                    'a',
                    InfoFieldScope::RunningInstance,
                    Some(true),
                    green,
                ),
                (
                    "Running extensions fingerprint",
                    'b',
                    InfoFieldScope::RunningInstance,
                    Some(false),
                    red,
                ),
                (
                    "Latest installed code fingerprint",
                    'c',
                    InfoFieldScope::Installation,
                    None,
                    egui::Color32::WHITE,
                ),
                (
                    "Latest extensions fingerprint",
                    'd',
                    InfoFieldScope::Installation,
                    None,
                    egui::Color32::WHITE,
                ),
            ];
            for (label, digit, scope, status, _) in fields {
                data.rows.push(InfoPopupRow {
                    label: label.into(),
                    scope,
                    value: digit.to_string().repeat(64),
                    searched: false,
                    separator_before: false,
                    binary_is_current: status,
                });
            }
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(scale);
            let (_, editor) = frame(&ctx, &data, Vec::new());
            let layout = &editor.galley.job;
            for (label, digit, scope, _, colour) in fields {
                let value = format!("{}\n", digit.to_string().repeat(64));
                let section = layout
                    .sections
                    .iter()
                    .find(|section| layout.text[section.byte_range.clone()] == value)
                    .unwrap();
                assert_eq!(section.format.color, colour, "{label}");
                let section = layout
                    .sections
                    .iter()
                    .find(|section| layout.text[section.byte_range.clone()].starts_with(label))
                    .unwrap();
                assert_eq!(
                    section.format.color,
                    info_field_label_color(scope),
                    "{label}"
                );
            }
        }
    }

    #[test]
    fn identity_values_are_red_or_green_without_colouring_labels_or_other_values() {
        let hash = "6e689278f21a31797320afd5269395c09d75e390";
        let labels = [
            "ELF build ID",
            "Running code fingerprint",
            "Running extensions fingerprint",
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
                            scope: InfoFieldScope::RunningInstance,
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
                        info_field_label_color(InfoFieldScope::RunningInstance)
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
    fn uncompiled_changes_values_use_status_colours_in_application_and_process_info() {
        let green = binary_current_color(Some(true)).unwrap();
        let red = binary_current_color(Some(false)).unwrap();
        let yellow = egui::Color32::from_rgb(255, 220, 100);
        for tree in [false, true] {
            for (value, color, uncertainty) in [
                ("No", green, None),
                ("No newer recorded inputs detected", green, None),
                (
                    "No newer recorded inputs detected (qualified timestamp evidence)",
                    green,
                    None,
                ),
                (
                    "No newer recorded inputs detected (977 checked; full build status unknown)",
                    green,
                    Some("full build status unknown"),
                ),
                ("Yes", red, None),
                ("Yes (recorded build inputs changed; source.c)", red, None),
                ("Unknown", yellow, None),
                ("Unknown (build evidence incomplete)", yellow, None),
                ("unknown", yellow, None),
                (
                    "Not applicable (package-managed binary)",
                    egui::Color32::WHITE,
                    None,
                ),
                ("Loading...", egui::Color32::WHITE, None),
                (
                    "Checking recorded build inputs…",
                    egui::Color32::WHITE,
                    None,
                ),
                ("Unavailable", egui::Color32::WHITE, None),
                ("-", egui::Color32::WHITE, None),
            ] {
                let ctx = egui::Context::default();
                let row = InfoPopupRow {
                    label: "Uncompiled changes".into(),
                    scope: InfoFieldScope::Installation,
                    value: value.into(),
                    searched: false,
                    separator_before: false,
                    binary_is_current: None,
                };
                let mut data = if tree { tree_info() } else { window_info() };
                let editor = if let Some(tree) = &mut data.process_tree {
                    tree.sections[0].rows.push(row);
                    tree_frame(&ctx, &data, Vec::new()).1.remove(0)
                } else {
                    data.rows.push(row);
                    frame(&ctx, &data, Vec::new()).1
                };
                let layout = &editor.galley.job;
                let value_start = layout.text.rfind(&format!("{value}\n")).unwrap();
                let caveat_start = uncertainty.map(|text| value_start + value.find(text).unwrap());
                for index in value_start..value_start + value.len() + 1 {
                    let section = layout
                        .sections
                        .iter()
                        .find(|section| section.byte_range.contains(&index))
                        .unwrap();
                    let expected = if caveat_start.is_some_and(|start| index >= start) {
                        yellow
                    } else {
                        color
                    };
                    assert_eq!(
                        section.format.color, expected,
                        "{value}, byte={index}, tree={tree}"
                    );
                }
                let label = layout
                    .sections
                    .iter()
                    .find(|section| {
                        layout.text[section.byte_range.clone()].starts_with("Uncompiled changes")
                    })
                    .unwrap();
                assert_eq!(
                    label.format.color,
                    info_field_label_color(InfoFieldScope::Installation)
                );
                assert!(copy_shown_info(&data).contains(value));
                let start = layout.text[..value_start].chars().count();
                let end = start + value.chars().count();
                let mut state = editor.state;
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(start),
                        egui::text::CCursor::new(end),
                    )));
                state.store(&ctx, editor.response.id);
                ctx.memory_mut(|memory| memory.request_focus(editor.response.id));
                let output = if tree {
                    tree_frame(&ctx, &data, vec![egui::Event::Copy]).0
                } else {
                    frame(&ctx, &data, vec![egui::Event::Copy]).0
                };
                assert!(output.platform_output.commands.iter().any(
                    |command| matches!(command, egui::OutputCommand::CopyText(copied) if copied == value)
                ), "copied bytes differ: {value}, tree={tree}");
            }
        }
    }

    #[test]
    fn compact_uncompiled_changes_paint_each_clause_independently_and_copy_plain_text() {
        let green = binary_current_color(Some(true)).unwrap();
        let red = binary_current_color(Some(false)).unwrap();
        let yellow = egui::Color32::from_rgb(255, 220, 100);
        let white = egui::Color32::WHITE;
        for tree in [false, true] {
            for scale in [1.0, 2.0] {
                for (clauses, searched, current) in [
                    (
                        vec![
                            ("0 newer", green),
                            ("1 missing", red),
                            ("added: unknown", yellow),
                            ("1815 inputs checked", white),
                            ("full build status unknown", yellow),
                        ],
                        false,
                        Some(false),
                    ),
                    (
                        vec![
                            ("2 newer", red),
                            ("0 missing", green),
                            ("added: 3", red),
                            ("1 changed", red),
                            ("2 touched unchanged", white),
                            ("1815 inputs checked", white),
                        ],
                        false,
                        Some(true),
                    ),
                    (
                        vec![
                            ("0 newer", green),
                            ("0 missing", green),
                            ("added: 0", green),
                            ("0 changed", green),
                            ("0 touched unchanged", white),
                            ("1815 inputs checked", white),
                            ("scan incomplete", yellow),
                        ],
                        true,
                        None,
                    ),
                    (
                        vec![("Unknown", yellow), ("scan incomplete", yellow)],
                        false,
                        None,
                    ),
                ] {
                    let base = if searched {
                        egui::Color32::from_rgb(255, 236, 170)
                    } else {
                        white
                    };
                    let value = clauses
                        .iter()
                        .map(|(clause, _)| *clause)
                        .collect::<Vec<_>>()
                        .join(" · ");
                    let row = InfoPopupRow {
                        label: "Uncompiled changes".into(),
                        scope: InfoFieldScope::Installation,
                        value: value.clone(),
                        searched,
                        separator_before: false,
                        // Executable identity colouring must not override input evidence.
                        binary_is_current: current,
                    };
                    let ctx = egui::Context::default();
                    ctx.set_pixels_per_point(scale);
                    let mut data = if tree { tree_info() } else { window_info() };
                    let editor = if let Some(tree) = &mut data.process_tree {
                        tree.sections[0].rows.push(row);
                        tree_frame(&ctx, &data, Vec::new()).1.remove(0)
                    } else {
                        data.rows.push(row);
                        frame(&ctx, &data, Vec::new()).1
                    };
                    let layout = &editor.galley.job;
                    let start = layout.text.rfind(&value).unwrap();
                    let mut cursor = start;
                    for (index, (clause, color)) in clauses.iter().enumerate() {
                        if index > 0 {
                            let separator = &layout.text[cursor..cursor + " · ".len()];
                            assert_eq!(separator, " · ");
                            for byte in cursor..cursor + separator.len() {
                                let section = layout
                                    .sections
                                    .iter()
                                    .find(|section| section.byte_range.contains(&byte))
                                    .unwrap();
                                assert_eq!(section.format.color, base);
                            }
                            cursor += separator.len();
                        }
                        for byte in cursor..cursor + clause.len() {
                            let section = layout
                                .sections
                                .iter()
                                .find(|section| section.byte_range.contains(&byte))
                                .unwrap();
                            assert_eq!(
                                section.format.color,
                                if *color == white { base } else { *color },
                                "clause={clause}, tree={tree}, scale={scale}"
                            );
                        }
                        cursor += clause.len();
                    }
                    assert_eq!(cursor, start + value.len());
                    assert!(copy_shown_info(&data).contains(&value));
                    let first = layout.text[..start].chars().count();
                    let mut state = editor.state;
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(first),
                            egui::text::CCursor::new(first + value.chars().count()),
                        )));
                    state.store(&ctx, editor.response.id);
                    ctx.memory_mut(|memory| memory.request_focus(editor.response.id));
                    let output = if tree {
                        tree_frame(&ctx, &data, vec![egui::Event::Copy]).0
                    } else {
                        frame(&ctx, &data, vec![egui::Event::Copy]).0
                    };
                    assert!(
                        output
                            .platform_output
                            .commands
                            .iter()
                            .any(|command| matches!(
                                command, egui::OutputCommand::CopyText(copied) if copied == &value
                            ))
                    );
                }
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
            assert!(
                layout
                    .text
                    .contains("\n\nRunning executable image original path")
            );
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
        assert!(selected_text.contains("Running executable image original path"));
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
                        header: "xfce4-terminal (pid 1)".into(),
                        binary_is_current: None,
                        update_target: None,
                        default_open: true,
                        metadata_sections: Vec::new(),
                        rows: vec![InfoPopupRow {
                            label: "Window PID".into(),
                            scope: InfoFieldScope::Window,
                            value: "1".into(),
                            searched: false,
                            separator_before: false,
                            binary_is_current: None,
                        }],
                    },
                    InfoProcessSection {
                        id: fish,
                        depth: 1,
                        header: "fish (pid 2)".into(),
                        binary_is_current: None,
                        update_target: None,
                        default_open: false,
                        metadata_sections: Vec::new(),
                        rows: vec![InfoPopupRow {
                            label: "Process name".into(),
                            scope: InfoFieldScope::RunningInstance,
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
            section.header = format!("{struck} (pid 1)").into();
            let (output, editors) = tree_frame(&ctx, &data, Vec::new());
            let header_id = editors[0].response.id;
            for expected in ["codex - ~/tasks/test - Terminal", "codex (pid 1)"] {
                assert!(
                    output.shapes.iter().any(|shape| matches!(&shape.shape,
                        egui::Shape::Text(text) if text.galley.job.text.contains(expected)
                            && text.galley.job.sections.iter().any(|section|
                                &text.galley.job.text[section.byte_range.clone()] == "codex"
                                    && section.format.strikethrough.width > 0.0)
                    )),
                    "{expected}"
                );
            }
            let mut state = egui::text_edit::TextEditState::load(&ctx, header_id).unwrap();
            let start = editors[0].galley.job.text.find("codex (pid 1)").unwrap();
            let start = editors[0].galley.job.text[..start].chars().count();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(start),
                    egui::text::CCursor::new(start + 5),
                )));
            state.store(&ctx, header_id);
            ctx.memory_mut(|memory| memory.request_focus(header_id));
            let (output, _) = tree_frame(&ctx, &data, vec![egui::Event::Copy]);
            assert!(output.platform_output.commands.iter().any(|command|
                matches!(command, egui::OutputCommand::CopyText(value) if value == &struck)
            ));
        }
    }

    #[test]
    fn process_headers_colour_only_the_name_for_current_outdated_and_unknown_builds() {
        for scale in [1.0, 2.0] {
            for open in [false, true] {
                let ctx = egui::Context::default();
                ctx.set_pixels_per_point(scale);
                let mut data = tree_info();
                let section = &mut data.process_tree.as_mut().unwrap().sections[1];
                section.header = "fish (pid 456416) [tmux pane]".into();
                section.default_open = open;
                for (current, colour) in [
                    (Some(true), egui::Color32::from_rgb(130, 225, 145)),
                    (Some(false), egui::Color32::from_rgb(255, 110, 110)),
                    (None, ctx.style().visuals.text_color()),
                ] {
                    data.process_tree.as_mut().unwrap().sections[1].binary_is_current = current;
                    let (output, _) = tree_frame(&ctx, &data, Vec::new());
                    let header = output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::Shape::Text(text)
                                if text
                                    .galley
                                    .job
                                    .text
                                    .contains("fish (pid 456416) [tmux pane]") =>
                            {
                                Some(&text.galley.job)
                            }
                            _ => None,
                        })
                        .expect("the collapsed and expanded process header must be painted");
                    assert!(
                        header
                            .sections
                            .iter()
                            .any(|section| &header.text[section.byte_range.clone()] == "fish"
                                && section.format.color == colour)
                    );
                    let start = header.text.find("fish (pid 456416) [tmux pane]").unwrap();
                    let end = start + "fish (pid 456416) [tmux pane]".len();
                    assert!(header.sections.iter().filter(|section| section.byte_range.start >= start + "fish".len() && section.byte_range.start < end).all(|section| section.format.color == ctx.style().visuals.text_color()));
                    assert!(copy_shown_info(&data).contains("fish (pid 456416) [tmux pane]"));
                }
            }
        }
    }

    #[test]
    fn hosted_metadata_headers_show_independent_freshness_when_open_or_collapsed() {
        use crate::models::InfoMetadataSection;
        for scale in [1.0, 2.0] {
            for open in [false, true] {
                for application in [Some(true), Some(false), None] {
                    for runtime in [Some(true), Some(false), None] {
                        let ctx = egui::Context::default();
                        ctx.set_pixels_per_point(scale);
                        let mut data = tree_info();
                        let owner = &mut data.process_tree.as_mut().unwrap().sections[0];
                        owner.metadata_sections = [
                            ("Application: Pi", application),
                            ("Runtime: Node.js", runtime),
                        ]
                        .into_iter()
                        .map(|(header, current)| InfoMetadataSection {
                            id: owner.id.with(header),
                            header: header.into(),
                            binary_is_current: current,
                            default_open: open,
                            rows: owner.rows.clone(),
                        })
                        .collect();
                        let (_, editors) = tree_frame(&ctx, &data, Vec::new());
                        assert_eq!(editors.len(), 1, "the whole tree stays selectable");
                        let job = &editors[0].galley.job;
                        for (header, current) in [
                            ("Application: Pi", application),
                            ("Runtime: Node.js", runtime),
                        ] {
                            let colour = binary_current_color(current)
                                .unwrap_or(ctx.style().visuals.text_color());
                            assert!(job.sections.iter().any(|section| {
                                &job.text[section.byte_range.clone()] == header
                                    && section.format.color == colour
                            }));
                            assert!(copy_shown_info(&data).contains(header));
                        }
                        assert!(!job.text.contains("Update"));
                    }
                }
            }
        }
    }

    #[test]
    fn browser_headers_and_running_versions_are_coloured_but_disk_versions_stay_neutral() {
        for browser in ["firefox", "chrome"] {
            for scale in [1.0, 2.0] {
                for (current, running, installed) in [
                    (Some(true), "matching-build-id", "matching-build-id"),
                    (Some(false), "old-build-id", "new-build-id"),
                    (None, "unverified-build-id", "unverified-build-id"),
                ] {
                    let ctx = egui::Context::default();
                    ctx.set_pixels_per_point(scale);
                    let mut data = tree_info();
                    let owner = &mut data.process_tree.as_mut().unwrap().sections[0];
                    owner.header = format!("{browser} (pid 1) [window owner]").into();
                    owner.binary_is_current = current;
                    let running_value = format!("{browser} 1.0 ({running})");
                    let installed_value = format!("{browser} 2.0 ({installed})");
                    owner.rows = [
                        (
                            "Executable Version (ELF build ID)",
                            running_value.as_str(),
                            InfoFieldScope::RunningInstance,
                        ),
                        (
                            "Latest version on disk (ELF build ID)",
                            installed_value.as_str(),
                            InfoFieldScope::Installation,
                        ),
                    ]
                    .into_iter()
                    .map(|(label, value, scope)| InfoPopupRow {
                        label: label.into(),
                        value: value.into(),
                        scope,
                        searched: false,
                        separator_before: false,
                        binary_is_current: if scope == InfoFieldScope::RunningInstance {
                            current
                        } else {
                            None
                        },
                    })
                    .collect();
                    let (_, editors) = tree_frame(&ctx, &data, vec![]);
                    let job = &editors[0].galley.job;
                    assert!(job.sections.iter().any(|section| {
                        &job.text[section.byte_range.clone()] == browser
                            && section.format.color
                                == binary_current_color(current)
                                    .unwrap_or(ctx.style().visuals.text_color())
                    }));
                    for (value, expected_colour) in [
                        (
                            running_value.as_str(),
                            binary_current_color(current).unwrap_or(egui::Color32::WHITE),
                        ),
                        (installed_value.as_str(), egui::Color32::WHITE),
                    ] {
                        let field = job
                            .sections
                            .iter()
                            .find(|section| job.text[section.byte_range.clone()].trim() == value)
                            .unwrap();
                        assert_eq!(field.format.color, expected_colour, "{value}");
                    }
                }
            }
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
                    scope: InfoFieldScope::Neutral,
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
                editors = render_info_popup_panel(ui, data).0;
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
                let text = "fish (pid 946747) [tmux pane]";
                section.header = text.into();
                let section_id = section.id;
                let (output, editors) = tree_viewport_frame(&ctx, viewport, &data, Vec::new());
                let header_id = editors[0].response.id;
                let (position, galley) = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(shape) if shape.galley.job.text.contains(text) => {
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
                let offset = galley.job.text.find(text).unwrap();
                let offset = galley.job.text[..offset].chars().count();
                let start = cursor_position(offset);
                let end = cursor_position(offset + text.chars().count());
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
                        .expect("tree needs a persistent text selection")
                        .cursor
                        .char_range()
                        .unwrap()
                        .as_sorted_char_range()
                };
                assert_eq!(selection(), offset..offset + text.chars().count());
                assert_eq!(process_info_section_is_open(&ctx, section_id, open), open);

                // The launcher's root viewport keeps repainting while native info is open.
                root_frame();
                data.heading = "New spinner frame".into();
                data.process_tree.as_mut().unwrap().sections[0]
                    .rows
                    .push(InfoPopupRow {
                        label: "Version".into(),
                        scope: InfoFieldScope::RunningInstance,
                        value: "Background probe finished".into(),
                        searched: false,
                        separator_before: false,
                        binary_is_current: None,
                    });
                let (output, editors) =
                    tree_viewport_frame(&ctx, viewport, &data, vec![egui::Event::Copy]);
                let current: String = editors[0]
                    .galley
                    .job
                    .text
                    .chars()
                    .skip(selection().start)
                    .take(selection().len())
                    .collect();
                assert_eq!(current, text);
                assert!(output.platform_output.commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyText(copied) if copied == text)));
                assert_eq!(process_info_section_is_open(&ctx, section_id, open), open);
                let (output, _) = tree_viewport_frame(
                    &ctx,
                    viewport,
                    &data,
                    vec![egui::Event::Text("MUST NOT EDIT".into())],
                );
                assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(shape) if shape.galley.job.text.contains(text) && !shape.galley.job.text.contains("MUST NOT EDIT"))));
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
            let (_, editors) = tree_frame(&ctx, &data, Vec::new());
            let sections = &data.process_tree.as_ref().unwrap().sections;
            assert_eq!(editors.len(), 1);
            let editor = &editors[0];
            let root_x = ctx
                .read_response(sections[0].id.with("tree-arrow"))
                .unwrap()
                .rect
                .left();
            for section in sections {
                let arrow = ctx
                    .read_response(section.id.with("tree-arrow"))
                    .unwrap()
                    .rect;
                let indent = arrow.left() - root_x;
                assert!(
                    (indent - section.depth as f32 * 12.0).abs() < 0.1,
                    "Arrow must follow the compact tree depth: depth={}, indent={indent}",
                    section.depth
                );
                let byte = editor
                    .galley
                    .job
                    .text
                    .find(section.header.as_ref())
                    .unwrap();
                let cursor = editor.galley.job.text[..byte].chars().count();
                let header_x = editor.galley_pos.x
                    + editor
                        .galley
                        .pos_from_cursor(egui::text::CCursor::new(cursor))
                        .center()
                        .x;
                assert!(
                    (header_x - arrow.right() - 2.0).abs() < 0.1,
                    "Arrow must sit beside its own header"
                );
                let body_start = byte + section.header.len() + 1;
                let body = &editor.galley.job.text[body_start..];
                let offset = body.len() - body.trim_start_matches(char::is_whitespace).len();
                let body_cursor = editor.galley.job.text[..body_start + offset]
                    .chars()
                    .count();
                let body_x = editor.galley_pos.x
                    + editor
                        .galley
                        .pos_from_cursor(egui::text::CCursor::new(body_cursor))
                        .center()
                        .x;
                assert!(
                    (body_x - header_x).abs() < 0.1,
                    "Details must align beneath their process header"
                );
            }
        }
    }

    fn pointer_events(pos: egui::Pos2) -> Vec<egui::Event> {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        vec![egui::Event::PointerMoved(pos), button(true), button(false)]
    }

    #[test]
    fn outdated_terminal_update_link_is_inline_text_height_and_does_not_toggle_the_tree() {
        for scale in [1.0, 2.0] {
            for font_size in [13.0, 24.0] {
                for open in [false, true] {
                    let ctx = egui::Context::default();
                    ctx.set_pixels_per_point(scale);
                    ctx.style_mut(|style| {
                        style.animation_time = 0.0;
                        style.text_styles.insert(
                            egui::TextStyle::Monospace,
                            egui::FontId::monospace(font_size),
                        );
                    });
                    let mut data = tree_info();
                    let owner = &mut data.process_tree.as_mut().unwrap().sections[0];
                    owner.header = "xfce4-terminal (pid 176434) [window owner]".into();
                    owner.binary_is_current = Some(false);
                    owner.update_target =
                        Some(InfoUpdateTarget::TerminalWindow("fixture-window".into()));
                    owner.default_open = open;
                    let id = owner.id;
                    let header = owner.header.clone();
                    let (_, editors) = tree_frame(&ctx, &data, vec![]);
                    let editor = &editors[0];
                    assert!(
                        editor
                            .galley
                            .job
                            .text
                            .contains(&format!("{header}  Update\n"))
                    );
                    let link = ctx.read_response(id.with("terminal-update-link")).unwrap();
                    let index = editor.galley.job.text.find(header.as_ref()).unwrap();
                    let cursor = editor.galley.job.text[..index].chars().count();
                    let row = editor
                        .galley
                        .pos_from_cursor(egui::text::CCursor::new(cursor))
                        .translate(editor.galley_pos.to_vec2());
                    assert!(link.rect.left() > row.left());
                    assert!(link.rect.top() >= row.top() - 0.5);
                    assert!(link.rect.bottom() <= row.bottom() + 0.5);
                    let height = ctx
                        .fonts_mut(|fonts| fonts.row_height(&egui::FontId::monospace(font_size)));
                    assert!((link.rect.height() - height).abs() <= 1.0);
                    let pos = link.rect.center();
                    let (output, _) = tree_frame(&ctx, &data, vec![egui::Event::PointerMoved(pos)]);
                    assert_eq!(
                        output.platform_output.cursor_icon,
                        egui::CursorIcon::PointingHand
                    );
                    tree_frame(&ctx, &data, pointer_events(pos));
                    assert_eq!(process_info_section_is_open(&ctx, id, !open), open);
                }
            }
        }
    }

    #[test]
    fn fish_and_codex_update_links_emit_only_the_exact_program_without_toggling_sections() {
        use crate::models::{InfoProgram, ProcessIdentity};
        for application in [InfoProgram::Fish, InfoProgram::Codex] {
            for open in [false, true] {
                for scale in [1.0, 2.0] {
                    let ctx = egui::Context::default();
                    ctx.set_pixels_per_point(scale);
                    let mut data = tree_info();
                    let section = &mut data.process_tree.as_mut().unwrap().sections[0];
                    let target = InfoUpdateTarget::Program {
                        window: "exact-window".into(),
                        process: ProcessIdentity {
                            pid: 123,
                            start_ticks: 456,
                        },
                        application,
                    };
                    section.header = format!("{} (pid 123) [tmux pane]", application.name()).into();
                    section.binary_is_current = Some(false);
                    section.default_open = open;
                    section.update_target = Some(target.clone());
                    let id = section.id;
                    let render = |events| {
                        let mut clicked = None;
                        let _ = ctx.run(
                            egui::RawInput {
                                screen_rect: Some(egui::Rect::from_min_size(
                                    egui::Pos2::ZERO,
                                    egui::vec2(1200.0, 800.0),
                                )),
                                events,
                                ..Default::default()
                            },
                            |ctx| {
                                egui::CentralPanel::default().show(ctx, |ui| {
                                    clicked = render_info_popup_panel(ui, &data).1;
                                });
                            },
                        );
                        clicked
                    };
                    render(vec![]);
                    render(vec![]);
                    let link = ctx.read_response(id.with("terminal-update-link")).unwrap();
                    assert_eq!(render(pointer_events(link.rect.center())), Some(target));
                    assert_eq!(process_info_section_is_open(&ctx, id, !open), open);
                }
            }
        }
    }

    #[test]
    fn copyq_update_link_remains_inline_for_current_outdated_and_unknown_builds() {
        for scale in [1.0, 2.0] {
            for status in [None, Some(true), Some(false)] {
                for open in [false, true] {
                    let ctx = egui::Context::default();
                    ctx.set_pixels_per_point(scale);
                    let mut data = tree_info();
                    let owner = &mut data.process_tree.as_mut().unwrap().sections[0];
                    owner.header = "copyq (pid 123) [window owner]".into();
                    owner.binary_is_current = status;
                    owner.default_open = open;
                    owner.update_target =
                        Some(InfoUpdateTarget::CopyQ(crate::models::ProcessIdentity {
                            pid: 123,
                            start_ticks: 456,
                        }));
                    let id = owner.id;
                    let (_, editors) = tree_frame(&ctx, &data, vec![]);
                    assert!(
                        editors[0]
                            .galley
                            .job
                            .text
                            .contains("copyq (pid 123) [window owner]  Update\n")
                    );
                    let link = ctx.read_response(id.with("terminal-update-link")).unwrap();
                    let height =
                        ctx.fonts_mut(|fonts| fonts.row_height(&egui::FontId::monospace(13.0)));
                    assert!((link.rect.height() - height).abs() <= 1.0);
                    tree_frame(&ctx, &data, pointer_events(link.rect.center()));
                    assert_eq!(process_info_section_is_open(&ctx, id, !open), open);
                }
            }
        }
    }

    #[test]
    fn update_link_is_absent_for_current_unknown_and_unsupported_processes() {
        for (status, target) in [
            (Some(true), Some("window")),
            (None, Some("window")),
            (Some(false), None),
        ] {
            let ctx = egui::Context::default();
            let mut data = tree_info();
            let owner = &mut data.process_tree.as_mut().unwrap().sections[0];
            owner.binary_is_current = status;
            owner.update_target =
                target.map(|window| InfoUpdateTarget::TerminalWindow(window.to_owned()));
            let id = owner.id;
            let (_, editors) = tree_frame(&ctx, &data, vec![]);
            assert!(!editors[0].galley.job.text.contains("Update"));
            assert!(ctx.read_response(id.with("terminal-update-link")).is_none());
        }
    }

    #[test]
    fn native_info_update_link_opens_a_cancellable_scope_dialog_and_emits_scoped_update_events() {
        for choice in ["Cancel", "Escape", "This window", "All outdated windows"] {
            let ctx = egui::Context::default();
            ctx.set_embed_viewports(false);
            let popup = egui::ViewportId::from_hash_of("native-info-update-fixture");
            let mut data = tree_info();
            let owner = &mut data.process_tree.as_mut().unwrap().sections[0];
            owner.binary_is_current = Some(false);
            owner.update_target = Some(InfoUpdateTarget::TerminalWindow(
                "exact-kwin-window-id".into(),
            ));
            let id = owner.id;
            let (sender, receiver) = std::sync::mpsc::channel();
            let registered = ctx.run(egui::RawInput::default(), |ctx| {
                show_deferred_info_popup(
                    ctx,
                    popup,
                    data.clone(),
                    [1200.0, 800.0],
                    [520.0, 360.0],
                    Arc::new(AtomicBool::new(false)),
                    sender.clone(),
                );
            });
            let callback = registered.viewport_output[&popup]
                .viewport_ui_cb
                .as_ref()
                .unwrap();
            let link_rect = std::cell::Cell::new(None);
            let frame = |events| {
                let mut input = egui::RawInput {
                    viewport_id: popup,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 800.0),
                    )),
                    events,
                    ..Default::default()
                };
                input.viewports.insert(
                    popup,
                    egui::ViewportInfo {
                        parent: Some(egui::ViewportId::ROOT),
                        ..Default::default()
                    },
                );
                ctx.run(input, |ctx| {
                    callback(ctx);
                    if let Some(link) = ctx.read_response(id.with("terminal-update-link")) {
                        link_rect.set(Some(link.rect));
                    }
                })
            };
            for _ in 0..3 {
                frame(vec![]);
            }
            let pos = link_rect
                .get()
                .expect("native info header has an Update link")
                .center();
            frame(pointer_events(pos));
            assert!(receiver.try_recv().is_err());
            for _ in 0..2 {
                frame(vec![]);
            }
            let output = frame(vec![]);
            let events = if choice == "Escape" {
                vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }]
            } else {
                let pos = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == choice => {
                            Some(text.pos + text.galley.size() * 0.5)
                        }
                        _ => None,
                    })
                    .expect("scope choice must appear inside Show Info");
                pointer_events(pos)
            };
            let output = frame(events);
            assert!(
                !output.viewport_output[&popup]
                    .commands
                    .iter()
                    .any(|command| matches!(command, egui::ViewportCommand::Close)),
                "Cancelling the dialog must leave Show Info open"
            );
            assert!(process_info_section_is_open(&ctx, id, false));
            if matches!(choice, "Cancel" | "Escape") {
                assert!(receiver.try_recv().is_err());
            } else {
                match receiver.try_recv().unwrap() {
                    PopupEvent::UpdateTerminalWindow { window, info } => {
                        assert_eq!(choice, "This window");
                        assert_eq!(window, "exact-kwin-window-id");
                        assert_eq!(info, popup);
                    }
                    PopupEvent::UpdateTerminalWindows { window: None } => {
                        assert_eq!(choice, "All outdated windows")
                    }
                    other => panic!("unexpected update scope: {other:?}"),
                }
                assert!(receiver.try_recv().is_err());
            }
        }
    }

    #[test]
    fn process_headers_use_arrow_pointers_without_changing_field_text_cursors() {
        for scale in [1.0, 2.0] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(scale);
            let mut data = tree_info();
            let owner = &mut data.process_tree.as_mut().unwrap().sections[0];
            owner.metadata_sections = vec![crate::models::InfoMetadataSection {
                id: owner.id.with("application"),
                header: "Application: Pi".into(),
                binary_is_current: None,
                default_open: true,
                rows: owner.rows.clone(),
            }];
            tree_frame(&ctx, &data, Vec::new());
            let (_, editors) = tree_frame(&ctx, &data, Vec::new());
            let editor = &editors[0];
            let pointer = |label: &str| {
                let byte = editor.galley.job.text.find(label).unwrap();
                editor.galley_pos
                    + editor
                        .galley
                        .pos_from_cursor(egui::text::CCursor::new(
                            editor.galley.job.text[..byte].chars().count(),
                        ))
                        .center()
                        .to_vec2()
                    + egui::vec2(4.0, 0.0)
            };
            for header in ["xfce4-terminal (pid 1)", "fish (pid 2)", "Application: Pi"] {
                let (output, _) = tree_frame(
                    &ctx,
                    &data,
                    vec![egui::Event::PointerMoved(pointer(header))],
                );
                assert_eq!(
                    output.platform_output.cursor_icon,
                    egui::CursorIcon::Default,
                    "{header}"
                );
            }
            let (output, _) = tree_frame(
                &ctx,
                &data,
                vec![egui::Event::PointerMoved(pointer("Window PID"))],
            );
            assert_eq!(output.platform_output.cursor_icon, egui::CursorIcon::Text);
        }
    }

    #[test]
    fn process_tree_restores_heading_and_section_spacing_without_widening_field_rows() {
        use crate::models::InfoMetadataSection;

        for scale in [1.0, 2.0] {
            for font_size in [13.0, 24.0] {
                let ctx = egui::Context::default();
                ctx.set_pixels_per_point(scale);
                ctx.style_mut(|style| {
                    style
                        .text_styles
                        .get_mut(&egui::TextStyle::Monospace)
                        .unwrap()
                        .size = font_size;
                });
                let mut data = tree_info();
                let owner = &mut data.process_tree.as_mut().unwrap().sections[0];
                let mut field = owner.rows[0].clone();
                field.label = "Window class".into();
                owner.rows.push(field.clone());
                owner.metadata_sections = ["Application: Pi", "Runtime: Node.js"]
                    .into_iter()
                    .enumerate()
                    .map(|(index, header)| {
                        let mut row = field.clone();
                        row.label = format!("Nested field {index}");
                        InfoMetadataSection {
                            id: owner.id.with(index),
                            header: header.into(),
                            binary_is_current: None,
                            default_open: true,
                            rows: vec![row],
                        }
                    })
                    .collect();
                let (output, editors) = tree_frame(&ctx, &data, Vec::new());
                assert!(!output.shapes.iter().any(|shape| matches!(
                    &shape.shape,
                    egui::Shape::Text(text) if text.galley.job.text == "Copy shown info"
                )));
                assert_eq!(editors.len(), 1, "The tree remains continuously selectable");
                let editor = &editors[0];
                let row = |label: &str| {
                    let byte = editor.galley.job.text.find(label).unwrap();
                    let cursor = editor.galley.job.text[..byte].chars().count();
                    editor
                        .galley
                        .pos_from_cursor(egui::text::CCursor::new(cursor))
                };
                let gap = ctx.style().spacing.item_spacing.y;
                let minimum_header = ctx.style().spacing.interact_size.y;
                let owner = row("xfce4-terminal (pid 1)");
                assert!(owner.height() >= minimum_header - 0.5);
                assert!(
                    (owner.top() - row("Process details").bottom() - gap - 8.0).abs() <= 0.5,
                    "Removing the copy-all button must not leave its reserved row"
                );
                for (above, below) in [
                    ("xfce4-terminal (pid 1)", "Window PID"),
                    ("Window class", "Application: Pi"),
                    ("Application: Pi", "Nested field 0"),
                    ("Nested field 0", "Runtime: Node.js"),
                    ("Runtime: Node.js", "Nested field 1"),
                ] {
                    let actual = row(below).top() - row(above).bottom();
                    assert!(
                        (actual - gap).abs() <= 0.5,
                        "{above} -> {below}: gap={actual}, expected={gap}, scale={scale}"
                    );
                }
                let between_processes = row("fish (pid 2)").top() - row("Nested field 1").bottom();
                assert!((between_processes - 8.0).abs() <= 0.5);
                assert!((row("Window class").top() - row("Window PID").bottom()).abs() <= 0.5);

                let id = data.process_tree.as_ref().unwrap().sections[0].id;
                let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
                    &ctx, id, true,
                );
                state.set_open(false);
                state.store(&ctx);
                let (_, collapsed) = tree_frame(&ctx, &data, Vec::new());
                let galley = &collapsed[0].galley;
                let header = |label| {
                    let byte = galley.job.text.find(label).unwrap();
                    galley.pos_from_cursor(egui::text::CCursor::new(
                        galley.job.text[..byte].chars().count(),
                    ))
                };
                assert!(
                    (header("fish (pid 2)").top()
                        - header("xfce4-terminal (pid 1)").bottom()
                        - gap)
                        .abs()
                        <= 0.5
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
        assert!(ctx.read_response(owner.with("tree-arrow")).is_some());
        let position = ctx
            .read_response(fish.with("tree-arrow"))
            .unwrap()
            .rect
            .center();
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
        tree_frame(&ctx, &data, vec![event(false)]);
        let (_, editors) = tree_frame(&ctx, &data, Vec::new());
        assert_eq!(editors.len(), 1);
        assert!(process_info_section_is_open(&ctx, owner, true));
        assert!(process_info_section_is_open(&ctx, fish, false));
        assert!(editors[0].galley.job.text.contains("Window PID"));
        assert!(editors[0].galley.job.text.contains("Process name"));
        data.heading = "pi - \u{280b} work - Terminal".into();
        let (_, editors) = tree_frame(&ctx, &data, Vec::new());
        assert_eq!(editors.len(), 1);
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
        assert!(ctx.read_response(fish.with("tree-arrow")).is_some());
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
                scope: InfoFieldScope::Window,
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
        let start_byte = editor.galley.job.text.find("Window PID").unwrap();
        let end_byte = editor.galley.job.text.find("fish (pid 2)").unwrap();
        let start = position(editor.galley.job.text[..start_byte].chars().count());
        let end = position(editor.galley.job.text[..end_byte].chars().count());
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
                .slice_str(&editors[0].galley.job.text),
            selected
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
                "Package",
                "@earendil-works/pi-coding-agent",
            ),
            ("runtime", "Runtime: Node.js", "Version", "v26.4.0"),
        ]
        .into_iter()
        .map(|(key, header, label, value)| InfoMetadataSection {
            id: process.id.with(key),
            header: header.into(),
            binary_is_current: None,
            default_open: true,
            rows: vec![InfoPopupRow {
                label: label.into(),
                scope: InfoFieldScope::RunningInstance,
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
        assert_eq!(documents.len(), 1);
        let runtime_editor_id = documents[0].response.id;
        let editor = &documents[0];
        let position = |index| {
            editor.galley_pos
                + editor
                    .galley
                    .pos_from_cursor(egui::text::CCursor::new(index))
                    .center()
                    .to_vec2()
        };
        let start_byte = editor.galley.job.text.find("Version").unwrap();
        let end_byte = start_byte + editor.galley.job.text[start_byte..].find('\n').unwrap();
        let start = position(editor.galley.job.text[..start_byte].chars().count());
        let end = position(editor.galley.job.text[..end_byte].chars().count());
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
            egui::OutputCommand::CopyText(text) if text.contains("Version") && text.contains("v26.4.0"))));
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
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].response.id, runtime_editor_id);
        assert!(process_info_section_is_open(&ctx, runtime_id, true));
        assert!(!process_info_section_is_open(&ctx, app_id, true));
    }

    #[test]
    fn copying_process_sections_includes_headers_and_each_sections_own_fields() {
        let data = tree_info();
        let document = copy_shown_info(&data);
        assert!(document.contains("xfce4-terminal (pid 1)\n    Window PID  1"));
        assert!(document.contains("  fish (pid 2)\n      Process name  fish"));
    }

    #[test]
    fn selection_spans_the_entire_tree_including_headings_and_supports_the_copy_menu() {
        for scale in [1.0, 2.0] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(scale);
            let mut data = tree_info();
            data.process_tree.as_mut().unwrap().sections[1].default_open = true;
            tree_frame(&ctx, &data, Vec::new());
            let (_, editors) = tree_frame(&ctx, &data, Vec::new());
            assert_eq!(
                editors.len(),
                1,
                "The whole visible tree is one selectable document"
            );
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
            let pointer = |pos, button, pressed| egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            tree_frame(
                &ctx,
                &data,
                vec![
                    egui::Event::PointerMoved(start),
                    pointer(start, egui::PointerButton::Primary, true),
                ],
            );
            tree_frame(&ctx, &data, vec![egui::Event::PointerMoved(end)]);
            let (_, editors) = tree_frame(
                &ctx,
                &data,
                vec![pointer(end, egui::PointerButton::Primary, false)],
            );
            let selected = editors[0]
                .state
                .cursor
                .char_range()
                .unwrap()
                .slice_str(&editors[0].galley.job.text)
                .to_owned();
            for required in [
                &data.heading,
                "xfce4-terminal (pid 1)",
                "Window PID",
                "fish (pid 2)",
                "Process name",
            ] {
                assert!(
                    selected.contains(required),
                    "Missing {required} from cross-tree selection"
                );
            }
            tree_frame(
                &ctx,
                &data,
                vec![
                    egui::Event::PointerMoved(end),
                    pointer(end, egui::PointerButton::Secondary, true),
                    pointer(end, egui::PointerButton::Secondary, false),
                ],
            );
            let (menu, _) = tree_frame(&ctx, &data, Vec::new());
            let copy = menu
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == "Copy" => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .expect("The tree selection has a Copy context-menu item");
            let (output, _) = tree_frame(
                &ctx,
                &data,
                vec![
                    egui::Event::PointerMoved(copy),
                    pointer(copy, egui::PointerButton::Primary, true),
                    pointer(copy, egui::PointerButton::Primary, false),
                ],
            );
            assert!(output.platform_output.commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyText(text) if text == &selected)));
            for section in &data.process_tree.as_ref().unwrap().sections {
                assert!(process_info_section_is_open(
                    &ctx,
                    section.id,
                    section.default_open
                ));
            }
        }
    }

    #[test]
    fn live_updates_keep_unique_tree_selections_anchored_and_clamp_changed_text() {
        let old = "old title\n\u{03c0} process\nPID 12\nold details";
        let new = "a much longer title\n\u{03c0} process\nPID 12\nupdated details";
        let start = old[..old.find('\u{03c0}').unwrap()].chars().count();
        let end = start + "\u{03c0} process\nPID 12".chars().count();
        for (a, b) in [(start, end), (end, start)] {
            let before = egui::text::CCursorRange::two(
                egui::text::CCursor::new(a),
                egui::text::CCursor::new(b),
            );
            let after = rebase_info_selection(old, new, before);
            assert_eq!(after.slice_str(new), before.slice_str(old));
            assert_eq!(
                after.primary.index > after.secondary.index,
                before.primary.index > before.secondary.index
            );
        }
        let before = egui::text::CCursorRange::two(
            egui::text::CCursor::new(1),
            egui::text::CCursor::new(old.chars().count()),
        );
        let after = rebase_info_selection(old, "", before);
        assert_eq!(after.as_sorted_char_range(), 0..0);
    }
}
