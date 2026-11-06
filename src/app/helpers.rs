use super::*;

pub(super) fn compact_panel_scrollbars(ui: &mut egui::Ui) {
    let scroll = &mut ui.spacing_mut().scroll;
    scroll.bar_width = 4.0;
    scroll.floating_width = 2.0;
}

pub(super) fn application_panel_status(ui: &mut egui::Ui, loading: bool) -> egui::Response {
    ui.set_min_width(ui.available_width());
    ui.vertical_centered(|ui| {
        ui.add_space(20.0);
        ui.label(
            egui::RichText::new(if loading {
                "Loading applications..."
            } else {
                "No matching applications found"
            })
            .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 120))
            .size(13.0),
        )
    })
    .inner
}

pub(super) fn application_scroll_content(ui: &mut egui::Ui, viewport: egui::Rect) {
    let scroll = ui.spacing().scroll;
    if scroll.floating {
        // Reserve the fully expanded bar even while it is hidden, so grid columns stay stable.
        let gutter = (scroll.bar_width + scroll.bar_outer_margin + 2.0
            - scroll.floating_allocated_width)
            .max(0.0);
        ui.set_width((ui.available_width() - gutter).max(0.0));
    }
    if egui::DragAndDrop::has_payload_of_type::<PinnedAppDrag>(ui.ctx()) {
        let screen_viewport = viewport.translate(ui.max_rect().min.to_vec2());
        let delta = ui.input(|input| {
            let pos = input.pointer.interact_pos()?;
            if !input.pointer.primary_down() || !screen_viewport.contains(pos) {
                return None;
            }
            let edge = 28.0_f32.min(screen_viewport.height() / 2.0);
            if edge <= 0.0 {
                return None;
            }
            let up = ((screen_viewport.top() + edge - pos.y) / edge).clamp(0.0, 1.0);
            let down = ((pos.y - screen_viewport.bottom() + edge) / edge).clamp(0.0, 1.0);
            Some((up - down) * 320.0 * input.stable_dt.min(0.05))
        });
        if let Some(delta) = delta.filter(|delta| *delta != 0.0) {
            ui.scroll_with_delta_animation(
                egui::vec2(0.0, delta),
                egui::style::ScrollAnimation::none(),
            );
            ui.ctx().request_repaint();
        }
    }
}

struct PinnedAppDrag {
    path: PathBuf,
    name: String,
    icon: Option<PathBuf>,
}

pub(super) struct PinnedAppMove {
    pub from: PathBuf,
    pub to: PathBuf,
}

pub(super) fn application_item_sense(pinned: bool) -> egui::Sense {
    if pinned {
        egui::Sense::click_and_drag()
    } else {
        egui::Sense::click()
    }
}

pub(super) fn pinned_app_drag_drop(
    ui: &egui::Ui,
    response: &egui::Response,
    app: &AppInfo,
    pinned: bool,
) -> Option<PinnedAppMove> {
    if !pinned {
        return None;
    }
    if response.drag_started_by(egui::PointerButton::Primary) {
        egui::DragAndDrop::set_payload(
            ui.ctx(),
            PinnedAppDrag {
                path: app.desktop_file_path.clone(),
                name: app.name.clone(),
                icon: app.icon_path.clone(),
            },
        );
    }
    let dragged = response.dnd_hover_payload::<PinnedAppDrag>()?;
    if dragged.path == app.desktop_file_path {
        return None;
    }
    ui.painter().rect_stroke(
        response.rect.shrink(1.0),
        egui::CornerRadius::same(8),
        egui::Stroke::new(2.0, egui::Color32::from_rgb(61, 174, 233)),
        egui::StrokeKind::Inside,
    );
    if ui.input(|input| input.pointer.button_released(egui::PointerButton::Primary)) {
        response.dnd_release_payload::<PinnedAppDrag>()?;
        return Some(PinnedAppMove {
            from: dragged.path.clone(),
            to: app.desktop_file_path.clone(),
        });
    }
    None
}

pub(super) fn show_pinned_app_drag(ctx: &egui::Context) {
    let Some(dragged) = egui::DragAndDrop::payload::<PinnedAppDrag>(ctx) else {
        return;
    };
    let Some(pos) = ctx.pointer_interact_pos() else {
        return;
    };
    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    egui::Area::new(egui::Id::new("pinned_app_drag_preview"))
        .order(egui::Order::Tooltip)
        .interactable(false)
        .fixed_pos(pos + egui::vec2(14.0, 14.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let size = egui::vec2(24.0, 24.0);
                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    paint_icon_in_rect(ui, dragged.icon.as_ref(), rect, size);
                    ui.label(&dragged.name);
                });
            });
        });
}

pub(super) fn move_pinned_app(pins: &mut Vec<PathBuf>, movement: &PinnedAppMove) -> bool {
    let Some(from) = pins.iter().position(|path| *path == movement.from) else {
        return false;
    };
    let Some(to) = pins.iter().position(|path| *path == movement.to) else {
        return false;
    };
    if from == to {
        return false;
    }
    let path = pins.remove(from);
    pins.insert(to, path);
    true
}

pub(super) fn virtual_items(
    ui: &mut egui::Ui,
    viewport: egui::Rect,
    count: usize,
    columns: usize,
    height: f32,
    gap: f32,
    scroll_to: Option<usize>,
) -> (std::ops::Range<usize>, Vec<f32>) {
    let columns = columns.max(1);
    let stride = height + gap;
    let rows = count.div_ceil(columns);
    let origin = ui.next_widget_position();
    ui.set_height((rows as f32 * stride - gap).max(0.0));
    if let Some(index) = scroll_to.filter(|index| *index < count) {
        let target = egui::Rect::from_min_size(
            origin + egui::vec2(0.0, (index / columns) as f32 * stride),
            egui::vec2(ui.available_width(), height),
        );
        ui.scroll_to_rect(target, None);
    }
    let start = ((viewport.min.y / stride).floor().max(0.0) as usize).min(rows);
    let end = ((viewport.max.y / stride).ceil().max(0.0) as usize + 1).min(rows);
    let centers = (0..count)
        .map(|index| origin.y + (index / columns) as f32 * stride + height / 2.0)
        .collect();
    ui.add_space(start as f32 * stride);
    ((start * columns)..(end * columns).min(count), centers)
}

pub(super) fn grid_columns(width: f32, tile: f32, gap: f32) -> usize {
    ((width + gap) / (tile + gap)).floor().max(1.0) as usize
}

pub(super) fn virtual_item<R>(
    ui: &mut egui::Ui,
    index: usize,
    size: egui::Vec2,
    render: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let (_, rect) = ui.allocate_space(size);
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id(ui.id().with(("virtual-item", index)))
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    render(&mut child)
}

struct WindowSortRecord {
    window: WindowInfo,
    app_key: String,
    terminal_subgroup_key: Option<String>,
    title_key: String,
}

#[derive(Default)]
pub(super) struct DefaultWindowsCache {
    entry: Option<(bool, Arc<Vec<WindowInfo>>)>,
}

impl DefaultWindowsCache {
    pub(super) fn invalidate(&mut self) {
        self.entry = None;
    }
    pub(super) fn update(&mut self, updates: &[(WindowInfo, WindowInfo)]) {
        if updates.iter().any(|(old, new)| {
            old.last_activated_at_ms != new.last_activated_at_ms
                || old.activation_sequence != new.activation_sequence
                || old.tmux_pane.is_some() != new.tmux_pane.is_some()
        }) {
            self.invalidate();
            return;
        }
        if let Some((_, rows)) = &mut self.entry {
            let changes: HashMap<_, _> = updates
                .iter()
                .map(|(_, new)| (new.id.as_str(), new))
                .collect();
            for row in Arc::make_mut(rows) {
                if let Some(new) = changes.get(row.id.as_str()) {
                    row.clone_from(new);
                }
            }
        }
    }
    pub(super) fn get(
        &mut self,
        windows: &[WindowInfo],
        recent_first: bool,
    ) -> Arc<Vec<WindowInfo>> {
        if let Some((recent, rows)) = &self.entry
            && *recent == recent_first
        {
            return Arc::clone(rows);
        }
        let mut sortable_windows = windows
            .iter()
            .cloned()
            .map(|window| {
                let app_key = window_grouping_key(&window);
                let terminal_subgroup_key = is_terminal_class(&window.class.trim().to_lowercase())
                    .then(|| terminal_window_subgroup_key(&window));
                let title_key = window_sort_title_key(&window);
                WindowSortRecord {
                    window,
                    app_key,
                    terminal_subgroup_key,
                    title_key,
                }
            })
            .collect::<Vec<_>>();
        let mut app_window_counts: HashMap<String, usize> = HashMap::new();
        let mut terminal_subgroup_counts: HashMap<String, usize> = HashMap::new();
        for record in &sortable_windows {
            *app_window_counts.entry(record.app_key.clone()).or_default() += 1;
            if let Some(subgroup_key) = &record.terminal_subgroup_key {
                *terminal_subgroup_counts
                    .entry(subgroup_key.clone())
                    .or_default() += 1;
            }
        }
        sortable_windows.sort_by(|a, b| {
            if recent_first {
                let activation_time_order =
                    match (a.window.last_activated_at_ms, b.window.last_activated_at_ms) {
                        (Some(a), Some(b)) => b.cmp(&a),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => std::cmp::Ordering::Equal,
                    };
                return activation_time_order
                    .then_with(|| {
                        b.window
                            .activation_sequence
                            .cmp(&a.window.activation_sequence)
                    })
                    .then_with(|| a.title_key.cmp(&b.title_key))
                    .then_with(|| a.window.id.cmp(&b.window.id));
            }
            let count_a = app_window_counts.get(&a.app_key).copied().unwrap_or(0);
            let count_b = app_window_counts.get(&b.app_key).copied().unwrap_or(0);
            count_a
                .cmp(&count_b)
                .then_with(|| a.app_key.cmp(&b.app_key))
                .then_with(
                    || match (&a.terminal_subgroup_key, &b.terminal_subgroup_key) {
                        (Some(subgroup_key_a), Some(subgroup_key_b)) if a.app_key == b.app_key => {
                            let subgroup_count_a = terminal_subgroup_counts
                                .get(subgroup_key_a)
                                .copied()
                                .unwrap_or(0);
                            let subgroup_count_b = terminal_subgroup_counts
                                .get(subgroup_key_b)
                                .copied()
                                .unwrap_or(0);
                            subgroup_count_a
                                .cmp(&subgroup_count_b)
                                .then_with(|| subgroup_key_a.cmp(&subgroup_key_b))
                                .then_with(|| {
                                    b.window
                                        .tmux_pane
                                        .is_some()
                                        .cmp(&a.window.tmux_pane.is_some())
                                })
                        }
                        _ => std::cmp::Ordering::Equal,
                    },
                )
                .then_with(|| a.title_key.cmp(&b.title_key))
                .then_with(|| a.window.id.cmp(&b.window.id))
        });
        let rows = Arc::new(
            sortable_windows
                .into_iter()
                .map(|record| record.window)
                .collect(),
        );
        self.entry = Some((recent_first, Arc::clone(&rows)));
        rows
    }
}

pub(super) fn index_pinned_apps(pins: &[PathBuf]) -> HashMap<PathBuf, usize> {
    let mut positions = HashMap::with_capacity(pins.len());
    for (index, path) in pins.iter().enumerate() {
        positions.entry(path.clone()).or_insert(index);
    }
    positions
}

#[derive(Default)]
pub(super) struct DefaultAppsCache {
    entry: Option<((u64, u64, bool), Arc<Vec<(AppInfo, bool)>>)>,
}

impl DefaultAppsCache {
    pub(super) fn get(
        &mut self,
        apps: &[AppInfo],
        pins: &HashMap<PathBuf, usize>,
        apps_generation: u64,
        pins_generation: u64,
        show_system_modules: bool,
    ) -> Arc<Vec<(AppInfo, bool)>> {
        let key = (apps_generation, pins_generation, show_system_modules);
        if let Some((cached_key, apps)) = &self.entry {
            if *cached_key == key {
                return Arc::clone(apps);
            }
        }

        // Reuse the list already required by the first render, not a second
        // cloned row model. Live window titles never enter this cache.
        let mut sorted: Vec<_> = apps
            .iter()
            .filter(|app| show_system_modules || !app.is_settings_module)
            .map(|app| (app.clone(), pins.contains_key(&app.desktop_file_path)))
            .collect();
        sorted.sort_by_cached_key(|(app, pinned)| {
            (
                app.is_settings_module,
                !pinned,
                pins.get(&app.desktop_file_path)
                    .copied()
                    .unwrap_or(usize::MAX),
                if *pinned {
                    String::new()
                } else {
                    app.name.to_lowercase()
                },
            )
        });
        let sorted = Arc::new(sorted);
        self.entry = Some((key, Arc::clone(&sorted)));
        sorted
    }
}

#[cfg(test)]
mod default_apps_tests {
    use super::*;

    fn app(name: &str, settings: bool) -> AppInfo {
        AppInfo {
            name: name.into(),
            exec: name.into(),
            icon_path: None,
            desktop_file_path: PathBuf::from(format!("/apps/{name}.desktop")),
            comment: None,
            is_settings_module: settings,
        }
    }

    fn identities(apps: &[(AppInfo, bool)]) -> Vec<(String, bool)> {
        apps.iter()
            .map(|(app, pinned)| (app.name.clone(), *pinned))
            .collect()
    }

    #[test]
    fn cached_app_order_matches_the_original_comparator() {
        let apps = vec![
            app("Zulu", false),
            app("alpha", false),
            app("Beta", false),
            app("Settings", true),
            app("Another setting", true),
        ];
        for pins in [vec![], vec![0, 1], vec![1, 0], vec![3, 2, 4], vec![0, 0, 1]] {
            let pins: Vec<_> = pins
                .into_iter()
                .map(|index| apps[index].desktop_file_path.clone())
                .collect();
            for show_modules in [false, true] {
                let mut expected: Vec<_> = apps
                    .iter()
                    .filter(|app| show_modules || !app.is_settings_module)
                    .map(|app| (app.clone(), pins.contains(&app.desktop_file_path)))
                    .collect();
                expected.sort_by(|a, b| {
                    a.0.is_settings_module
                        .cmp(&b.0.is_settings_module)
                        .then_with(|| match (a.1, b.1) {
                            (true, false) => std::cmp::Ordering::Less,
                            (false, true) => std::cmp::Ordering::Greater,
                            (true, true) => pinned_app_position(&pins, &a.0)
                                .cmp(&pinned_app_position(&pins, &b.0)),
                            (false, false) => a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase()),
                        })
                });
                let actual = DefaultAppsCache::default().get(
                    &apps,
                    &index_pinned_apps(&pins),
                    0,
                    0,
                    show_modules,
                );
                assert_eq!(identities(&actual), identities(&expected));
            }
        }
    }

    #[test]
    fn metadata_frames_reuse_the_same_app_rows_without_cloning_or_sorting() {
        let apps = vec![app("Example", false)];
        let pins = HashMap::new();
        let mut cache = DefaultAppsCache::default();
        let first = cache.get(&apps, &pins, 1, 1, false);
        assert_eq!(Arc::strong_count(&first), 2);
        for _ in 0..100 {
            let next = cache.get(&apps, &pins, 1, 1, false);
            assert!(Arc::ptr_eq(&first, &next));
        }
    }

    #[test]
    fn installs_pin_changes_and_module_visibility_invalidate_the_cached_app_list() {
        let mut cache = DefaultAppsCache::default();
        let mut apps = vec![app("Zulu", false)];
        let first = cache.get(&apps, &HashMap::new(), 1, 1, false);
        apps.push(app("alpha", false));
        apps.push(app("Settings", true));
        let installed = cache.get(&apps, &HashMap::new(), 2, 1, false);
        assert!(!Arc::ptr_eq(&first, &installed));
        assert_eq!(
            identities(&installed),
            vec![("alpha".into(), false), ("Zulu".into(), false)]
        );
        let pins = index_pinned_apps(&[apps[0].desktop_file_path.clone()]);
        let pinned = cache.get(&apps, &pins, 2, 2, false);
        assert!(!Arc::ptr_eq(&installed, &pinned));
        assert_eq!(pinned[0].0.name, "Zulu");
        assert!(pinned[0].1);
        let modules = cache.get(&apps, &pins, 2, 2, true);
        assert!(!Arc::ptr_eq(&pinned, &modules));
        assert_eq!(modules.len(), 3);
        assert!(modules.last().unwrap().0.is_settings_module);
    }

    #[test]
    fn pin_index_keeps_first_duplicate_and_path_equality_semantics() {
        let pins = vec![
            PathBuf::from("/apps/a.desktop"),
            PathBuf::from("/apps/./a.desktop"),
        ];
        let positions = index_pinned_apps(&pins);
        assert_eq!(positions.len(), 1);
        assert_eq!(positions.get(Path::new("/apps/a.desktop")), Some(&0));
    }

    #[test]
    fn pinned_moves_preserve_every_other_apps_relative_order() {
        for len in 1..12 {
            let original: Vec<_> = (0..len).map(|i| PathBuf::from(i.to_string())).collect();
            for from in 0..len {
                for to in 0..len {
                    let mut pins = original.clone();
                    assert_eq!(
                        move_pinned_app(
                            &mut pins,
                            &PinnedAppMove {
                                from: original[from].clone(),
                                to: original[to].clone(),
                            },
                        ),
                        from != to
                    );
                    assert_eq!(pins[to], original[from]);
                    assert_eq!(pins.len(), original.len());
                    let remaining: Vec<_> = pins.iter().filter(|p| **p != original[from]).collect();
                    assert_eq!(
                        remaining,
                        original
                            .iter()
                            .filter(|p| **p != original[from])
                            .collect::<Vec<_>>()
                    );
                }
            }
            for (from, to) in [
                (PathBuf::from("missing"), original[0].clone()),
                (original[0].clone(), PathBuf::from("missing")),
            ] {
                let mut pins = original.clone();
                assert!(!move_pinned_app(&mut pins, &PinnedAppMove { from, to }));
                assert_eq!(pins, original);
            }
        }
    }

    #[derive(Default)]
    struct DragFrame {
        rects: Vec<egui::Rect>,
        moves: Vec<PinnedAppMove>,
        clicked: bool,
        middle_clicked: bool,
    }

    fn drag_frame(
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        grid: bool,
        overlay: bool,
    ) -> DragFrame {
        let mut result = DragFrame::default();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(280.0, 300.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.set_width(if grid { 164.0 } else { 220.0 });
                    ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
                    result.rects.clear();
                    ui.horizontal_wrapped(|ui| {
                        for index in 0..5 {
                            let app = app(&index.to_string(), false);
                            let pinned = index < 4;
                            let sense = application_item_sense(pinned);
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(if grid { 48.0 } else { 220.0 }, 48.0),
                                if overlay { egui::Sense::click() } else { sense },
                            );
                            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                            child.add(
                                egui::Label::new(&app.name)
                                    .selectable(false)
                                    .sense(egui::Sense::hover()),
                            );
                            let response = if overlay {
                                ui.interact(rect, ui.id().with(("overlay", index)), sense)
                            } else {
                                response
                            };
                            result.clicked |= response.clicked();
                            result.middle_clicked |= response.middle_clicked();
                            if let Some(movement) =
                                pinned_app_drag_drop(ui, &response, &app, pinned)
                            {
                                result.moves.push(movement);
                            }
                            result.rects.push(rect);
                        }
                    });
                });
                show_pinned_app_drag(ctx);
            },
        );
        result
    }

    fn pointer_button(pos: egui::Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn pinned_drag_only_reorders_once_on_a_valid_drop_and_never_launches() {
        for grid in [false, true] {
            for overlay in [false, true] {
                for (from, to) in [(0, 3), (3, 0), (1, 1), (0, 4), (4, 0)] {
                    let ctx = egui::Context::default();
                    let initial = drag_frame(&ctx, Vec::new(), grid, overlay);
                    let start = initial.rects[from].left_top() + egui::vec2(6.0, 6.0);
                    let target = initial.rects[to].center();
                    for events in [
                        vec![
                            egui::Event::PointerMoved(start),
                            pointer_button(start, egui::PointerButton::Primary, true),
                        ],
                        vec![egui::Event::PointerMoved(start + egui::vec2(10.0, 10.0))],
                        vec![egui::Event::PointerMoved(target)],
                        Vec::new(),
                    ] {
                        let frame = drag_frame(&ctx, events, grid, overlay);
                        assert!(frame.moves.is_empty(), "must not reorder while hovering");
                        assert!(!frame.clicked, "dragging must not launch an application");
                    }
                    let frame = drag_frame(
                        &ctx,
                        vec![pointer_button(target, egui::PointerButton::Primary, false)],
                        grid,
                        overlay,
                    );
                    assert!(!frame.clicked, "dropping must not launch an application");
                    let valid = from != to && from < 4 && to < 4;
                    assert_eq!(
                        frame.moves.len(),
                        usize::from(valid),
                        "drop {from}->{to}, grid={grid}, overlay={overlay}"
                    );
                    if valid {
                        assert_eq!(
                            frame.moves[0].from,
                            app(&from.to_string(), false).desktop_file_path
                        );
                        assert_eq!(
                            frame.moves[0].to,
                            app(&to.to_string(), false).desktop_file_path
                        );
                    }
                    assert!(drag_frame(&ctx, Vec::new(), grid, overlay).moves.is_empty());
                    assert!(!egui::DragAndDrop::has_payload_of_type::<PinnedAppDrag>(
                        &ctx
                    ));
                }
            }
        }
    }

    #[test]
    fn pinned_drag_escape_cancels_without_reordering_or_launching() {
        let ctx = egui::Context::default();
        let initial = drag_frame(&ctx, Vec::new(), true, false);
        let start = initial.rects[0].center();
        let target = initial.rects[3].center();
        drag_frame(
            &ctx,
            vec![
                egui::Event::PointerMoved(start),
                pointer_button(start, egui::PointerButton::Primary, true),
            ],
            true,
            false,
        );
        drag_frame(&ctx, vec![egui::Event::PointerMoved(target)], true, false);
        assert!(egui::DragAndDrop::has_payload_of_type::<PinnedAppDrag>(
            &ctx
        ));
        let cancelled = drag_frame(
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            true,
            false,
        );
        assert!(cancelled.moves.is_empty());
        assert!(!ctx.input(|input| input.key_pressed(egui::Key::Escape)));
        assert!(!egui::DragAndDrop::has_payload_of_type::<PinnedAppDrag>(
            &ctx
        ));
        let released = drag_frame(
            &ctx,
            vec![pointer_button(target, egui::PointerButton::Primary, false)],
            true,
            false,
        );
        assert!(released.moves.is_empty());
        assert!(!released.clicked);
    }

    #[test]
    fn pinned_drag_edge_scrolling_stays_in_its_panel_and_stops_on_release() {
        let ctx = egui::Context::default();
        egui::DragAndDrop::set_payload(
            &ctx,
            PinnedAppDrag {
                path: PathBuf::from("/apps/test.desktop"),
                name: "test".into(),
                icon: None,
            },
        );
        let render = |events| {
            let mut offsets = [0.0; 2];
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600.0, 240.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.columns(2, |panes| {
                            for (index, ui) in panes.iter_mut().enumerate() {
                                let area = egui::ScrollArea::vertical()
                                    .id_salt(index)
                                    .auto_shrink([false, true])
                                    .max_height(180.0)
                                    .show_viewport(ui, |ui, viewport| {
                                        application_scroll_content(ui, viewport);
                                        ui.set_height(1000.0);
                                    });
                                offsets[index] = area.state.offset.y;
                            }
                        });
                    });
                },
            );
            offsets
        };
        let pos = egui::pos2(450.0, 186.0);
        let mut offsets = render(vec![
            egui::Event::PointerMoved(pos),
            pointer_button(pos, egui::PointerButton::Primary, true),
        ]);
        for _ in 0..10 {
            offsets = render(Vec::new());
            assert_eq!(offsets[0], 0.0);
        }
        assert!(offsets[1] > 0.0 && offsets[1] < 200.0);
        let released = render(vec![pointer_button(
            pos,
            egui::PointerButton::Primary,
            false,
        )]);
        // egui applies the preceding frame's queued scroll target at the next frame start.
        assert_eq!(released[0], 0.0);
        assert!(released[1] >= offsets[1] && released[1] - offsets[1] <= 16.0);
        assert!(!egui::DragAndDrop::has_payload_of_type::<PinnedAppDrag>(
            &ctx
        ));
        for _ in 0..5 {
            assert_eq!(render(Vec::new()), released);
        }
    }

    #[test]
    fn pinned_items_keep_normal_click_and_middle_click_actions() {
        for button in [egui::PointerButton::Primary, egui::PointerButton::Middle] {
            let ctx = egui::Context::default();
            let initial = drag_frame(&ctx, Vec::new(), false, false);
            let pos = initial.rects[0].left_top() + egui::vec2(6.0, 6.0);
            drag_frame(
                &ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    pointer_button(pos, button, true),
                ],
                false,
                false,
            );
            let released = drag_frame(&ctx, vec![pointer_button(pos, button, false)], false, false);
            assert_eq!(released.clicked, button == egui::PointerButton::Primary);
            assert_eq!(
                released.middle_clicked,
                button == egui::PointerButton::Middle
            );
            assert!(released.moves.is_empty());
            assert!(!egui::DragAndDrop::has_payload_of_type::<PinnedAppDrag>(
                &ctx
            ));
        }
    }
}

pub(crate) fn filtered_search_cache_key(
    mode: LauncherMode,
    query: &str,
    show_system_settings_modules: bool,
    order_windows_by_last_activation: bool,
    pinned_apps_generation: u64,
    apps_generation: u64,
    windows_generation: u64,
) -> FilteredSearchCacheKey {
    FilteredSearchCacheKey {
        mode,
        query: query.to_string(),
        show_system_settings_modules,
        order_windows_by_last_activation,
        pinned_apps_generation,
        apps_generation,
        windows_generation,
    }
}

pub(super) fn sort_cached_windows_by_last_activation(results: &mut FilteredSearchResults) -> bool {
    let count = results.windows.len();
    if results.window_display_titles.len() != count
        || results.window_highlight_segments.len() != count
        || results.window_title_is_typos.len() != count
    {
        return false;
    }
    let mut order: Vec<_> = (0..count).collect();
    order.sort_by_cached_key(|&index| window_last_activation_sort_key(&results.windows[index]));
    if order
        .iter()
        .enumerate()
        .all(|(index, &source)| index == source)
    {
        return true;
    }

    let mut destinations = vec![0; count];
    for (destination, source) in order.into_iter().enumerate() {
        destinations[source] = destination;
    }
    let windows = Arc::make_mut(&mut results.windows);
    let titles = Arc::make_mut(&mut results.window_display_titles);
    let highlights = Arc::make_mut(&mut results.window_highlight_segments);
    let typos = Arc::make_mut(&mut results.window_title_is_typos);
    // Keep row data aligned; each swap fixes one destination without fuzzy reranking.
    for source in 0..count {
        while destinations[source] != source {
            let destination = destinations[source];
            windows.swap(source, destination);
            titles.swap(source, destination);
            highlights.swap(source, destination);
            typos.swap(source, destination);
            destinations.swap(source, destination);
        }
    }
    true
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

pub(super) fn history_title_label(ui: &mut egui::Ui, title: &str) -> egui::Response {
    if !title.contains(['\u{0335}', '\u{0336}']) {
        return ui.label(title);
    }
    let mut job = egui::text::LayoutJob::default();
    egui::RichText::new(title)
        .color(ui.visuals().text_color())
        .append_to(
            &mut job,
            ui.style(),
            egui::FontSelection::Default,
            ui.text_valign(),
        );
    ui.label(crate::search::render_title_strikethrough(job))
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
    if !response.hovered() || egui::DragAndDrop::has_payload_of_type::<PinnedAppDrag>(&response.ctx)
    {
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
        ui.add(egui::Label::new(text).extend());
    });
}
