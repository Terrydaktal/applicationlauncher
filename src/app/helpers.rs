use super::*;

pub(crate) fn filtered_search_cache_key(
    mode: LauncherMode,
    query: &str,
    show_system_settings_modules: bool,
    pinned_apps_generation: u64,
    apps_generation: u64,
    windows_generation: u64,
) -> FilteredSearchCacheKey {
    FilteredSearchCacheKey {
        mode,
        query: query.to_string(),
        show_system_settings_modules,
        pinned_apps_generation,
        apps_generation,
        windows_generation,
    }
}

pub(crate) fn load_window_size() -> (f32, f32) {
    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(format!(
            "{}/.config/applicationlauncher/window_size.txt",
            home
        ));
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(path) {
                let lines: Vec<&str> = content.lines().collect();
                if lines.len() >= 2 {
                    if let (Ok(w), Ok(h)) = (
                        lines[0].trim().parse::<f32>(),
                        lines[1].trim().parse::<f32>(),
                    ) {
                        let w = w.clamp(300.0, 1920.0);
                        let h = h.clamp(200.0, 1080.0);
                        return (w, h);
                    }
                }
            }
        }
    }
    (980.0, 560.0) // Default size
}

pub(crate) fn setup_system_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // Fallback paths for symbol fonts supporting Braille
    let paths = [
        "/usr/share/fonts/noto/NotoSansSymbols-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    ];

    let mut loaded_any = false;
    for (i, path) in paths.iter().enumerate() {
        if let Ok(data) = std::fs::read(path) {
            let key = format!("sys_symbol_{}", i);
            fonts.font_data.insert(
                key.clone(),
                std::sync::Arc::new(egui::FontData::from_owned(data)),
            );
            if let Some(vec) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                vec.push(key.clone());
            }
            if let Some(vec) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                vec.push(key);
            }
            loaded_any = true;
        }
    }

    if loaded_any {
        ctx.set_fonts(fonts);
    }
}
pub(crate) fn effective_list_row_height(
    configured_height: f32,
    icon_height: f32,
    vertical_padding_total: f32,
    line_height: f32,
    text_spacing: f32,
    show_path: bool,
) -> f32 {
    let text_height = if show_path {
        line_height + text_spacing + line_height * 0.8
    } else {
        line_height
    };

    configured_height
        .max(icon_height + vertical_padding_total)
        .max(text_height + vertical_padding_total)
}

pub(crate) fn selected_row_accent_size(row_height: f32) -> egui::Vec2 {
    let row_height = row_height.max(0.0);
    egui::vec2((row_height * 0.1).min(3.0), (row_height * 0.65).min(28.0))
}

pub(crate) fn window_search_refresh_deadline(current: Option<Instant>, now: Instant) -> Instant {
    current.unwrap_or(now + Duration::from_millis(WINDOW_SEARCH_REFRESH_INTERVAL_MS))
}

pub(crate) fn inset_rect(
    rect: egui::Rect,
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(rect.min.x + left, rect.min.y + top),
        egui::pos2(
            (rect.max.x - right).max(rect.min.x + left),
            (rect.max.y - bottom).max(rect.min.y + top),
        ),
    )
}

pub(crate) fn paint_wayland_fallback_icon(painter: &egui::Painter, rect: egui::Rect) {
    let radius = (rect.width().min(rect.height()) * 0.18).clamp(4.0, 9.0);
    painter.rect_filled(
        rect,
        egui::CornerRadius::same(radius as u8),
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 12),
    );
    painter.rect_stroke(
        rect.shrink(0.5),
        egui::CornerRadius::same(radius as u8),
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 28),
        ),
        egui::StrokeKind::Inside,
    );

    let c = rect.center();
    let scale = rect.width().min(rect.height()) / 48.0;
    let stroke = egui::Stroke::new(
        (3.0 * scale).max(1.5),
        egui::Color32::from_rgba_unmultiplied(230, 245, 255, 210),
    );
    let accent = egui::Color32::from_rgb(61, 174, 233);

    let points = [
        egui::pos2(c.x - 14.0 * scale, c.y - 9.0 * scale),
        egui::pos2(c.x - 7.0 * scale, c.y + 12.0 * scale),
        egui::pos2(c.x, c.y - 2.0 * scale),
        egui::pos2(c.x + 7.0 * scale, c.y + 12.0 * scale),
        egui::pos2(c.x + 14.0 * scale, c.y - 9.0 * scale),
    ];
    painter.line(points.to_vec(), stroke);
    painter.circle_filled(points[0], 3.2 * scale, accent);
    painter.circle_filled(points[2], 3.2 * scale, accent);
    painter.circle_filled(points[4], 3.2 * scale, accent);
}

pub(crate) fn paint_icon_in_rect(
    ui: &mut egui::Ui,
    icon_path: Option<&PathBuf>,
    rect: egui::Rect,
    icon_size: egui::Vec2,
) {
    if let Some(path) = icon_path {
        let uri = format!("file://{}", path.to_string_lossy());
        let mut child_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        child_ui.add(egui::Image::new(uri).max_size(icon_size));
    } else {
        paint_wayland_fallback_icon(ui.painter(), rect);
    }
}

pub(crate) fn paint_centered_title_job(
    ui: &egui::Ui,
    rect: egui::Rect,
    text: &str,
    font_size: f32,
    highlight_segments: &[(usize, usize, bool)],
    fallback_color: egui::Color32,
) {
    let galley = ui.ctx().fonts_mut(|fonts| {
        fonts.layout_job(highlighted_title_job_from_segments(
            text,
            font_size,
            highlight_segments,
        ))
    });
    let position = egui::pos2(
        rect.center().x - galley.size().x / 2.0,
        rect.center().y - galley.size().y / 2.0,
    );
    ui.painter().galley(position, galley, fallback_color);
}

pub(crate) fn grid_move_down(index: usize, len: usize, columns: usize) -> usize {
    if len == 0 {
        return 0;
    }

    let columns = columns.max(1);
    let next = index.saturating_add(columns);
    if next < len { next } else { index % columns }
}

pub(crate) fn grid_move_up(index: usize, len: usize, columns: usize) -> usize {
    if len == 0 {
        return 0;
    }

    let columns = columns.max(1);
    if index >= columns {
        return index - columns;
    }

    let column = index % columns;
    let mut last_in_column = column.min(len - 1);
    while last_in_column + columns < len {
        last_in_column += columns;
    }
    last_in_column
}

pub(crate) fn nearest_center_index(centers: &[f32], target_y: f32) -> Option<usize> {
    centers
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let da = (*a - target_y).abs();
            let db = (*b - target_y).abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(index, _)| index)
}

pub(crate) fn show_immediate_icon_tooltip(response: &egui::Response, text: &str) {
    if !response.hovered() {
        return;
    }

    let _ = egui::Tooltip::always_open(
        response.ctx.clone(),
        response.layer_id,
        response.id.with("icon_tooltip"),
        response.rect,
    )
    .gap(8.0)
    .show(|ui| {
        ui.label(text);
    });
}
