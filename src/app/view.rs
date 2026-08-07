use super::*;
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Track window size changes in memory
        let current_size = ctx.viewport_rect().size();
        if (current_size.x - self.width).abs() > 1.0 || (current_size.y - self.height).abs() > 1.0 {
            self.width = current_size.x;
            self.height = current_size.y;
        }

        let mut handled_focus_launcher = false;
        let mut ui_event_count = 0;
        for _ in 0..UI_EVENTS_PER_FRAME {
            let Ok(event) = self.ui_event_rx.try_recv() else {
                break;
            };
            ui_event_count += 1;
            match event {
                UiEvent::FocusLauncher => {
                    handled_focus_launcher = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                        egui::WindowLevel::AlwaysOnTop,
                    ));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                        egui::UserAttentionType::Informational,
                    ));
                    request_launcher_foreground();
                    self.search_focus_until = Some(Instant::now() + Duration::from_millis(1200));
                    self.search_query.clear();
                    self.selected_index = 0;
                    self.side_panel_selected_index = 0;
                    self.last_selected_window_id = None;
                    self.scroll_to_first_window_on_focus = self.mode == LauncherMode::Windows;
                    self.active_pane = if self.mode == LauncherMode::Windows {
                        ActivePane::Windows
                    } else {
                        ActivePane::Apps
                    };
                    self.start_background_app_load();
                    self.start_terminal_metadata_refresh();
                    if self.use_kwin_window_feed {
                        self.schedule_window_reconciliation(Duration::ZERO);
                    }
                }
            }
        }
        if ui_event_count == UI_EVENTS_PER_FRAME || handled_focus_launcher {
            ctx.request_repaint();
        }
        self.process_popup_events();
        while let Ok(status) = self.tracker_status_receiver.try_recv() {
            self.recovery_prompt = status.recovery_pending;
        }

        match self
            .terminal_records_receiver
            .as_ref()
            .map(|rx| rx.try_recv())
        {
            Some(Ok(result)) => {
                self.terminal_records_receiver = None;
                match result {
                    Ok(records) => self.apply_terminal_metadata_records(records),
                    Err(err) => eprintln!("Could not refresh XFCE4 Terminal metadata: {err}"),
                }
                if self.terminal_metadata_refresh_queued {
                    self.start_terminal_metadata_refresh();
                }
                ctx.request_repaint();
            }
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.terminal_records_receiver = None;
                if self.terminal_metadata_refresh_queued {
                    self.start_terminal_metadata_refresh();
                }
            }
            _ => {}
        }

        if let Some(result) = self
            .kwin_window_feed_setup_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.kwin_window_feed_setup_rx = None;
            match result {
                Ok(()) => {
                    self.use_kwin_window_feed = true;
                    self.schedule_window_reconciliation(Duration::from_secs(1));
                    ctx.request_repaint();
                }
                Err(err) => {
                    eprintln!("Falling back to kdotool window polling: {err}");
                    self.terminal_action_message = Some((
                        format!(
                            "Session history unavailable; using a one-time window snapshot: {err}"
                        ),
                        false,
                        Instant::now(),
                    ));
                    self.start_window_polling_thread(ctx);
                }
            }
        }

        if !handled_focus_launcher && !self.loading && self.use_kwin_window_feed {
            let mut pending_events = Vec::with_capacity(WINDOW_FEED_EVENTS_PER_FRAME);
            for _ in 0..WINDOW_FEED_EVENTS_PER_FRAME {
                match self.window_feed_receiver.try_recv() {
                    Ok(event) => pending_events.push(event),
                    Err(_) => break,
                }
            }
            let hit_window_feed_budget = pending_events.len() == WINDOW_FEED_EVENTS_PER_FRAME;
            if hit_window_feed_budget {
                ctx.request_repaint();
            }
            self.apply_window_feed_events(pending_events);
        } else if !self.use_kwin_window_feed {
            // Check background receiver for periodic window updates
            let mut latest_windows = None;
            let mut window_snapshot_count = 0;
            for _ in 0..WINDOW_SNAPSHOTS_PER_FRAME {
                match self.window_receiver.try_recv() {
                    Ok(new_windows) => {
                        latest_windows = Some(new_windows);
                        window_snapshot_count += 1;
                    }
                    Err(_) => break,
                }
            }
            if let Some(new_windows) = latest_windows {
                if !self.loading {
                    self.apply_window_snapshot(new_windows);
                }
            }
            if window_snapshot_count == WINDOW_SNAPSHOTS_PER_FRAME {
                ctx.request_repaint();
            }
        }

        if !handled_focus_launcher {
            match self
                .background_window_enrichment_receiver
                .as_ref()
                .map(|rx| rx.try_recv())
            {
                Some(Ok(windows)) => {
                    self.apply_window_snapshot(windows);
                    self.background_window_enrichment_receiver = None;
                    ctx.request_repaint();
                }
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                    self.background_window_enrichment_receiver = None;
                }
                _ => {}
            }
        }

        match self
            .background_window_reconciliation_receiver
            .as_ref()
            .map(|rx| rx.try_recv())
        {
            Some(Ok(Some(windows))) => {
                self.background_window_reconciliation_receiver = None;
                self.apply_window_reconciliation(windows);
                self.schedule_window_reconciliation(Duration::from_secs(
                    WINDOW_RECONCILIATION_INTERVAL_SECS,
                ));
                ctx.request_repaint();
            }
            Some(Ok(None)) | Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.background_window_reconciliation_receiver = None;
                self.schedule_window_reconciliation(Duration::from_secs(
                    WINDOW_RECONCILIATION_RETRY_SECS,
                ));
            }
            _ => {}
        }

        if self.use_kwin_window_feed
            && !self.loading
            && self.background_window_reconciliation_receiver.is_none()
        {
            if let Some(deadline) = self.next_window_reconciliation_at {
                let now = Instant::now();
                if now >= deadline {
                    self.start_window_reconciliation();
                } else {
                    ctx.request_repaint_after(deadline.saturating_duration_since(now));
                }
            }
        }

        if self.background_apps_receiver.is_some() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        match self
            .background_apps_receiver
            .as_ref()
            .map(|rx| rx.try_recv())
        {
            Some(Ok(apps)) => {
                self.apps = apps;
                self.apps_generation = self.apps_generation.wrapping_add(1);
                self.background_apps_receiver = None;
                ctx.request_repaint();
            }
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.background_apps_receiver = None;
            }
            _ => {}
        }

        // Check background receiver for window query results
        if !handled_focus_launcher && self.loading {
            ctx.request_repaint(); // Keep repainting until loaded to check channel promptly
            if let Some(ref rx) = self.receiver {
                if let Ok(result) = rx.try_recv() {
                    self.loading = false;
                    match result {
                        LoadResult::AppsSuccess(apps) => {
                            self.apps = apps;
                            self.apps_generation = self.apps_generation.wrapping_add(1);
                            self.selected_index = 0;
                            self.side_panel_selected_index = 0;
                            self.active_pane = ActivePane::Apps;
                        }
                        LoadResult::WindowsSuccess(windows) => {
                            self.windows = windows;
                            self.seed_window_icon_cache();
                            self.missing_window_counts.clear();
                            self.windows_generation = self.windows_generation.wrapping_add(1);
                            self.refresh_window_audio_cache();
                            self.selected_index = 0;
                            self.side_panel_selected_index = 0;
                            self.active_pane = ActivePane::Windows;
                            self.start_background_window_enrichment();
                        }
                        LoadResult::Error(err) => {
                            self.error_message = Some(err);
                            self.kdotool_path = None;
                        }
                    }
                }
            }
        }

        if !handled_focus_launcher {
            let mut latest_audio_update = None;
            for _ in 0..AUDIO_UPDATES_PER_FRAME {
                match self.audio_cache_receiver.try_recv() {
                    Ok(update) => {
                        latest_audio_update = Some(update);
                    }
                    Err(_) => break,
                }
            }
            if let Some(update) = latest_audio_update {
                let previous_has_active_audio = self.has_active_audio;
                self.cached_sink_inputs = update.sink_inputs;
                self.active_media_app_keys = update.active_media_app_keys;
                self.observed_pipewire_node_ids = update.observed_pipewire_node_ids;
                self.active_pipewire_node_ids = update.active_pipewire_node_ids;
                self.pipewire_activity_cache_valid = update.pipewire_activity_cache_valid;
                self.has_active_audio = self.has_any_active_audio();
                let window_audio_changed = self.refresh_window_audio_cache();
                if window_audio_changed || self.has_active_audio != previous_has_active_audio {
                    ctx.request_repaint();
                }
            }
        }

        if self.has_active_audio {
            let audio_repaint_ms = AUDIO_ACTIVE_REPAINT_MS;
            ctx.request_repaint_after(std::time::Duration::from_millis(audio_repaint_ms));
        }

        if self.mode == LauncherMode::Windows
            && self.win_show_last_activation
            && !self.windows.is_empty()
        {
            ctx.request_repaint_after(Duration::from_secs(1));
        }

        if !handled_focus_launcher {
            self.prune_stale_windows();
        }

        if let Some(deadline) = self.pending_window_search_refresh_at {
            let now = Instant::now();
            if self.search_query.trim().is_empty() || now >= deadline {
                if self.flush_pending_window_search_refresh() {
                    ctx.request_repaint();
                }
            } else {
                ctx.request_repaint_after(deadline.saturating_duration_since(now));
            }
        }

        // Focus loss auto-close
        if self.close_on_blur
            && self.start_time.elapsed().as_millis() > 500
            && !ctx.input(|i| i.focused)
            && !self.show_settings_menu
            && !self.show_history_popup
            && self.process_chain_popup.is_none()
            && self.app_info_popup.is_none()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // Main transparent window frame
        let panel_frame = egui::Frame {
            fill: egui::Color32::TRANSPARENT,
            ..Default::default()
        };

        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                // Translucent acrylic-like container
                let container_frame = egui::Frame {
                    fill: egui::Color32::from_rgba_unmultiplied(22, 23, 27, 240), // Dark glass fill
                    corner_radius: egui::CornerRadius::same(12),
                    stroke: egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18),
                    ),
                    inner_margin: egui::Margin::same(14),
                    ..Default::default()
                };

                container_frame.show(ui, |ui| {
                    if let Some(ref err) = self.error_message {
                        ui.vertical_centered(|ui| {
                            ui.add_space(20.0);
                            ui.label(
                                egui::RichText::new("⚠️ Error")
                                    .color(egui::Color32::from_rgb(218, 68, 83))
                                    .strong()
                                    .size(24.0),
                            );
                            ui.add_space(10.0);
                            ui.label(egui::RichText::new(err).size(14.0));
                            ui.add_space(20.0);
                            if ui.button("Exit").clicked() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        });
                        return;
                    }

                    // 1. Search Bar Container
                    let search_bar_frame = egui::Frame {
                        fill: egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10),
                        corner_radius: egui::CornerRadius::same(8),
                        inner_margin: egui::Margin::symmetric(12, 8),
                        stroke: egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 15),
                        ),
                        ..Default::default()
                    };

                    let mut text_edit_response = None;
                    let mut search_query_changed = false;

                    search_bar_frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("🔍")
                                    .size(18.0)
                                    .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 160)),
                            );
                            ui.add_space(4.0);
                            let hint_text = match self.mode {
                                LauncherMode::Apps => "Search applications...",
                                LauncherMode::Windows => "Search open windows...",
                            };
                            let search_width = (ui.available_width() - 78.0).max(120.0);
                            let text_edit = egui::TextEdit::singleline(&mut self.search_query)
                                .hint_text(hint_text)
                                .desired_width(search_width)
                                .frame(false)
                                .font(egui::FontId::proportional(16.0));

                            let response = ui.add(text_edit);
                            search_query_changed = response.changed();
                            text_edit_response = Some(response);
                            ui.add_space(8.0);
                            let ordering_button = egui::Button::new(
                                egui::RichText::new("↕").size(16.0),
                            )
                            .selected(self.order_windows_by_last_activation);
                            if ui
                                .add(ordering_button)
                                .on_hover_text(
                                    "Order windows by last activation, newest first",
                                )
                                .clicked()
                            {
                                self.order_windows_by_last_activation =
                                    !self.order_windows_by_last_activation;
                                self.selected_index = 0;
                                self.scroll_to_first_window_on_focus = true;
                            }
                            if ui
                                .button(egui::RichText::new("H").size(15.0))
                                .on_hover_text("Window history and sessions (F9)")
                                .clicked()
                            {
                                if self.show_history_popup { self.close_history_popup(); } else { self.open_history_popup(); }
                            }
                            if ui
                                .button(
                                    egui::RichText::new("⚙")
                                        .size(16.0)
                                        .color(egui::Color32::from_rgba_unmultiplied(
                                            255, 255, 255, 190,
                                        )),
                                )
                                .on_hover_text("Settings")
                                .clicked()
                            {
                                if self.show_settings_menu {
                                    self.close_settings_menu();
                                } else {
                                    self.open_settings_menu();
                                }
                            }
                        });
                    });

                    // KWin may raise the window a few frames after we request it.
                    // Keep retrying briefly so shortcut activation lands in search.
                    if let Some(ref resp) = text_edit_response {
                        if self
                            .search_focus_until
                            .is_some_and(|deadline| Instant::now() <= deadline)
                        {
                            resp.request_focus();
                        } else {
                            self.search_focus_until = None;
                        }
                    }

                    if search_query_changed && self.flush_pending_window_search_refresh() {
                        ctx.request_repaint();
                    }

                    ui.add_space(10.0);

	                    // 2. Filtering list
	                    let mut filtered_apps: Arc<Vec<(AppInfo, bool)>> = Arc::new(Vec::new());
	                    let mut filtered_windows: Arc<Vec<WindowInfo>> = Arc::new(Vec::new());
                        let mut filtered_app_display_titles: Arc<Vec<String>> = Arc::new(Vec::new());
                        let mut filtered_window_display_titles: Arc<Vec<String>> = Arc::new(Vec::new());
	                        let mut filtered_app_highlight_segments: Arc<Vec<Vec<(usize, usize, bool)>>> =
	                            Arc::new(Vec::new());
	                        let mut filtered_app_name_highlight_segments: Arc<Vec<Vec<(usize, usize, bool)>>> =
	                            Arc::new(Vec::new());
	                        let mut filtered_window_highlight_segments: Arc<Vec<Vec<(usize, usize, bool)>>> =
                            Arc::new(Vec::new());
                        let search_query = self.search_query.trim().to_string();
                        let has_search_query = !search_query.is_empty();
	                    let mut filtered_app_title_is_typos: Arc<Vec<bool>> = Arc::new(Vec::new());
	                    let mut filtered_window_title_is_typos: Arc<Vec<bool>> = Arc::new(Vec::new());
                        let filter_cache_key = has_search_query.then(|| {
                            filtered_search_cache_key(
                                self.mode,
                                &search_query,
                                self.show_system_settings_modules,
                                self.pinned_apps_generation,
                                self.apps_generation,
                                self.windows_generation,
                            )
                        });

                        if let Some(cache) = filter_cache_key.as_ref().and_then(|cache_key| {
                            self.filtered_search_cache
                                .as_ref()
                                .filter(|cache| cache.key == *cache_key)
                        }) {
	                            filtered_apps = Arc::clone(&cache.results.apps);
	                            filtered_windows = Arc::clone(&cache.results.windows);
                            filtered_app_display_titles = Arc::clone(&cache.results.app_display_titles);
                            filtered_window_display_titles =
                                Arc::clone(&cache.results.window_display_titles);
	                            filtered_app_highlight_segments =
	                                Arc::clone(&cache.results.app_highlight_segments);
	                            filtered_app_name_highlight_segments =
	                                Arc::clone(&cache.results.app_name_highlight_segments);
	                            filtered_window_highlight_segments =
                                Arc::clone(&cache.results.window_highlight_segments);
                            filtered_app_title_is_typos = Arc::clone(&cache.results.app_title_is_typos);
                            filtered_window_title_is_typos =
                                Arc::clone(&cache.results.window_title_is_typos);
                        } else {
		                    match self.mode {
	                        LauncherMode::Apps => {
		                            if !has_search_query {
	                                filtered_apps = Arc::new(self.apps
                                    .iter()
                                    .filter(|app| self.show_system_settings_modules || !app.is_settings_module)
                                    .map(|app| {
                                        let is_pinned = self.pinned_apps.contains(&app.desktop_file_path);
                                        (app.clone(), is_pinned)
                                    })
                                    .collect());
	                                Arc::make_mut(&mut filtered_apps).sort_by(|a, b| {
                                    a.0.is_settings_module
                                        .cmp(&b.0.is_settings_module)
                                        .then_with(|| match (a.1, b.1) {
                                            (true, false) => std::cmp::Ordering::Less,
                                            (false, true) => std::cmp::Ordering::Greater,
                                            (true, true) => {
                                                pinned_app_position(&self.pinned_apps, &a.0)
                                                    .cmp(&pinned_app_position(&self.pinned_apps, &b.0))
                                            }
                                            (false, false) => a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase()),
                                        })
				                                });
			                            } else if let (Some(base_query), Some(typo_query)) = (
                                    MetadataQuery::new(&search_query),
                                    MetadataQuery::new(&search_query).map(|q| q.with_typo_fallback(true)),
                                ) {
		                                let mut ranked_apps: Vec<RankedAppMatch> = self.apps
		                                    .iter()
                                    .filter(|app| self.show_system_settings_modules || !app.is_settings_module)
                                    .filter_map(|app| {
                                        let is_pinned = self.pinned_apps.contains(&app.desktop_file_path);
	                                        let search_values = app_search_values(app);
                                        let (rank, display_title, highlight_segments, title_is_typo) =
                                            compute_display_title_and_highlights(
                                                &full_search_visible_app_title(app),
                                                &search_values,
                                                &base_query,
                                                &typo_query,
                                                70,
                                            )?;
                                        let visible_match_priority = visible_match_priority(
                                            &full_search_visible_app_title(app),
                                            &search_query,
                                        );
                                        let pin_position = pinned_app_position(&self.pinned_apps, app);
                                        let candidate_score = if is_pinned {
                                            2_000_000.0 - pin_position as f64
                                        } else if !app.is_settings_module {
                                            1_000_000.0
                                        } else {
                                            0.0
                                        };
	                                        Some(RankedAppMatch {
	                                            app: app.clone(),
	                                            rank,
	                                            title_is_typo,
	                                            visible_match_priority,
	                                            is_pinned,
                                                display_title,
                                                highlight_segments,
	                                            search_values,
	                                            candidate_key: format!(
	                                                "{}\u{0}{}",
	                                                app.name.to_lowercase(),
                                                app.desktop_file_path.to_string_lossy()
                                            ),
                                            candidate_score,
	                                        })
		                                    })
		                                    .collect();
			                                sort_ranked_matches_with_visible(
	                                    &mut ranked_apps,
	                                            |item| item.visible_match_priority,
		                                    |left, right| {
		                                        let left_fields =
		                                            metadata_fields_for_values(&left.search_values);
		                                        let right_fields =
		                                            metadata_fields_for_values(&right.search_values);
		                                        typo_query.compare_candidates(
		                                            MetadataCandidate {
		                                                key: &left.candidate_key,
		                                                fields: &left_fields,
		                                                score: left.candidate_score,
		                                            },
		                                            &left.rank,
		                                            MetadataCandidate {
		                                                key: &right.candidate_key,
		                                                fields: &right_fields,
		                                                score: right.candidate_score,
		                                            },
		                                            &right.rank,
		                                        )
		                                    },
		                                );
                                        filtered_app_title_is_typos = Arc::new(ranked_apps
                                            .iter()
                                            .map(|item| item.title_is_typo)
                                            .collect());
                                        filtered_app_display_titles = Arc::new(ranked_apps
                                            .iter()
                                            .map(|item| item.display_title.clone())
                                            .collect());
                                        filtered_app_highlight_segments = Arc::new(ranked_apps
                                            .iter()
                                            .map(|item| item.highlight_segments.clone())
                                            .collect());
	                                    filtered_app_name_highlight_segments = Arc::new(ranked_apps
	                                        .iter()
	                                        .map(|item| title_highlight_segments(&item.app.name, &search_query))
	                                        .collect());
	                                filtered_apps = Arc::new(ranked_apps
                                    .into_iter()
                                    .map(|item| (item.app, item.is_pinned))
                                    .collect());
	                            } else {
	                                    filtered_apps = Arc::new(Vec::new());
                                }
	                        }
		                        LauncherMode::Windows => {
		                            if !has_search_query {
	                                filtered_windows = Arc::new(self.windows.clone());
	                                let mut app_window_counts: HashMap<String, usize> =
	                                    HashMap::new();
	                                let mut terminal_subgroup_counts: HashMap<String, usize> =
	                                    HashMap::new();
	                                for win in filtered_windows.iter() {
	                                    *app_window_counts
	                                        .entry(window_grouping_key(win))
	                                        .or_default() += 1;
	                                    if is_terminal_class(&win.class.trim().to_lowercase()) {
	                                        *terminal_subgroup_counts
	                                            .entry(terminal_window_subgroup_key(win))
	                                            .or_default() += 1;
	                                    }
	                                }
                                Arc::make_mut(&mut filtered_windows).sort_by(|a, b| {
                                    if self.order_windows_by_last_activation {
                                        return compare_windows_by_last_activation(a, b);
                                    }
                                    let app_key_a = window_grouping_key(a);
	                                    let app_key_b = window_grouping_key(b);
	                                    let count_a =
                                        app_window_counts.get(&app_key_a).copied().unwrap_or(0);
                                    let count_b =
                                        app_window_counts.get(&app_key_b).copied().unwrap_or(0);
	                                    count_a
	                                        .cmp(&count_b)
	                                        .then_with(|| app_key_a.cmp(&app_key_b))
	                                        .then_with(|| {
	                                            let a_is_terminal =
	                                                is_terminal_class(&a.class.trim().to_lowercase());
	                                            let b_is_terminal =
	                                                is_terminal_class(&b.class.trim().to_lowercase());
	                                            match (a_is_terminal, b_is_terminal) {
	                                                (true, true) if app_key_a == app_key_b => {
	                                                    let subgroup_key_a =
	                                                        terminal_window_subgroup_key(a);
	                                                    let subgroup_key_b =
	                                                        terminal_window_subgroup_key(b);
	                                                    let subgroup_count_a = terminal_subgroup_counts
	                                                        .get(&subgroup_key_a)
	                                                        .copied()
	                                                        .unwrap_or(0);
	                                                    let subgroup_count_b = terminal_subgroup_counts
	                                                        .get(&subgroup_key_b)
	                                                        .copied()
	                                                        .unwrap_or(0);
	                                                    subgroup_count_a
	                                                        .cmp(&subgroup_count_b)
	                                                        .then_with(|| {
	                                                            subgroup_key_a.cmp(&subgroup_key_b)
	                                                        })
	                                                }
	                                                _ => std::cmp::Ordering::Equal,
	                                            }
	                                        })
	                                        .then_with(|| {
	                                            window_sort_title_key(a).cmp(&window_sort_title_key(b))
	                                        })
	                                        .then_with(|| a.id.cmp(&b.id))
					                                });
				                            } else if let (Some(base_query), Some(typo_query)) = (
                                    MetadataQuery::new(&search_query),
                                    MetadataQuery::new(&search_query).map(|q| q.with_typo_fallback(true)),
                                ) {
		                                let mut ranked_windows: Vec<RankedWindowMatch> = self.windows
                                    .iter()
                                    .filter_map(|win| {
                                        let search_values = window_search_values(win);
                                        let (rank, display_title, highlight_segments, title_is_typo) =
                                            compute_display_title_and_highlights(
                                                &full_search_visible_window_title(win),
                                                &search_values,
                                                &base_query,
                                                &typo_query,
                                                70,
                                            )?;
                                        let visible_match_priority = 0;
                                        Some(RankedWindowMatch {
                                            window: win.clone(),
                                            rank,
                                            title_is_typo,
                                            visible_match_priority,
                                            display_title,
                                            highlight_segments,
                                            search_values,
                                            candidate_key: format!(
                                                "{}\u{0}{}\u{0}{}",
                                                window_grouping_key(win),
                                                window_sort_title_key(win),
                                                win.id
                                            ),
                                            candidate_score: 0.0,
                                        })
                                    })
                                    .collect();
			                                sort_ranked_matches_with_visible(
	                                    &mut ranked_windows,
	                                            |item| item.visible_match_priority,
		                                    |left, right| {
		                                        let left_fields =
		                                            metadata_fields_for_values(&left.search_values);
		                                        let right_fields =
		                                            metadata_fields_for_values(&right.search_values);
		                                        typo_query.compare_candidates(
		                                            MetadataCandidate {
		                                                key: &left.candidate_key,
		                                                fields: &left_fields,
		                                                score: left.candidate_score,
		                                            },
		                                            &left.rank,
		                                            MetadataCandidate {
		                                                key: &right.candidate_key,
		                                                fields: &right_fields,
		                                                score: right.candidate_score,
		                                            },
		                                            &right.rank,
		                                        )
		                                    },
		                                );
                                        filtered_window_title_is_typos = Arc::new(ranked_windows
                                            .iter()
                                            .map(|item| item.title_is_typo)
                                            .collect());
                                        filtered_window_display_titles = Arc::new(ranked_windows
                                            .iter()
                                            .map(|item| item.display_title.clone())
                                            .collect());
                                        filtered_window_highlight_segments = Arc::new(ranked_windows
                                            .iter()
                                            .map(|item| item.highlight_segments.clone())
                                            .collect());
	                                filtered_windows = Arc::new(
	                                    ranked_windows.into_iter().map(|item| item.window).collect(),
                                );
	                            } else {
	                                    filtered_windows = Arc::new(Vec::new());
                                }
	                        }
	                    }

		                    if self.mode == LauncherMode::Windows {
		                        if !has_search_query {
	                            filtered_apps = Arc::new(self.apps
                                .iter()
                                .filter(|app| self.show_system_settings_modules || !app.is_settings_module)
                                .map(|app| {
                                    let is_pinned = self.pinned_apps.contains(&app.desktop_file_path);
                                    (app.clone(), is_pinned)
                                })
                                .collect());
	                            Arc::make_mut(&mut filtered_apps).sort_by(|a, b| {
		                                a.0.is_settings_module
		                                    .cmp(&b.0.is_settings_module)
		                                    .then_with(|| match (a.1, b.1) {
	                                        (true, false) => std::cmp::Ordering::Less,
	                                        (false, true) => std::cmp::Ordering::Greater,
	                                        (true, true) => {
                                            pinned_app_position(&self.pinned_apps, &a.0)
                                                .cmp(&pinned_app_position(&self.pinned_apps, &b.0))
			                    }
		                                        (false, false) => a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase()),
		                                    })
				                            });
				                        } else if let (Some(base_query), Some(typo_query)) = (
                                MetadataQuery::new(&search_query),
                                MetadataQuery::new(&search_query).map(|q| q.with_typo_fallback(true)),
                            ) {
		                            let mut ranked_apps: Vec<RankedAppMatch> = self.apps
		                                .iter()
		                                .filter(|app| self.show_system_settings_modules || !app.is_settings_module)
		                                .filter_map(|app| {
		                                    let is_pinned = self.pinned_apps.contains(&app.desktop_file_path);
			                                    let search_values = app_search_values(app);
                                            let (rank, display_title, highlight_segments, title_is_typo) =
                                                compute_display_title_and_highlights(
                                                    &full_search_visible_app_title(app),
                                                    &search_values,
                                                    &base_query,
                                                    &typo_query,
                                                    70,
                                                )?;
		                                    let visible_match_priority = visible_match_priority(
		                                        &full_search_visible_app_title(app),
		                                        &search_query,
		                                    );
		                                    let pin_position = pinned_app_position(&self.pinned_apps, app);
		                                    let candidate_score = if is_pinned {
		                                        2_000_000.0 - pin_position as f64
	                                    } else if !app.is_settings_module {
	                                        1_000_000.0
	                                    } else {
	                                        0.0
	                                    };
			                                    Some(RankedAppMatch {
			                                        app: app.clone(),
			                                        rank,
			                                        title_is_typo,
			                                        visible_match_priority,
			                                        is_pinned,
                                                    display_title,
                                                    highlight_segments,
			                                        search_values,
			                                        candidate_key: format!(
		                                            "{}\u{0}{}",
	                                            app.name.to_lowercase(),
	                                            app.desktop_file_path.to_string_lossy()
	                                        ),
	                                        candidate_score,
	                                    })
		                                })
		                                .collect();
				                            sort_ranked_matches_with_visible(
				                                &mut ranked_apps,
	                                            |item| item.visible_match_priority,
			                                |left, right| {
			                                    let left_fields =
			                                        metadata_fields_for_values(&left.search_values);
			                                    let right_fields =
			                                        metadata_fields_for_values(&right.search_values);
			                                    typo_query.compare_candidates(
			                                        MetadataCandidate {
			                                            key: &left.candidate_key,
			                                            fields: &left_fields,
			                                            score: left.candidate_score,
			                                        },
			                                        &left.rank,
			                                        MetadataCandidate {
			                                            key: &right.candidate_key,
			                                            fields: &right_fields,
			                                            score: right.candidate_score,
			                                        },
			                                        &right.rank,
			                                    )
			                                },
			                            );
	                                    filtered_app_title_is_typos = Arc::new(ranked_apps
                                        .iter()
                                        .map(|item| item.title_is_typo)
                                        .collect());
                                    filtered_app_display_titles = Arc::new(ranked_apps
                                        .iter()
                                        .map(|item| item.display_title.clone())
                                        .collect());
	                                    filtered_app_highlight_segments = Arc::new(ranked_apps
	                                        .iter()
	                                        .map(|item| item.highlight_segments.clone())
	                                        .collect());
	                                filtered_app_name_highlight_segments = Arc::new(ranked_apps
	                                    .iter()
	                                    .map(|item| title_highlight_segments(&item.app.name, &search_query))
	                                    .collect());
	                            filtered_apps = Arc::new(ranked_apps
                                .into_iter()
                                .map(|item| (item.app, item.is_pinned))
                                .collect());
		                        } else {
	                                filtered_apps = Arc::new(Vec::new());
	                            }
		                    }

                            if let Some(cache_key) = filter_cache_key {
                                self.filtered_search_cache = Some(FilteredSearchCache {
                                    key: cache_key,
                                    results: FilteredSearchResults {
                                        apps: Arc::clone(&filtered_apps),
                                        windows: Arc::clone(&filtered_windows),
                                        app_display_titles: Arc::clone(&filtered_app_display_titles),
                                        window_display_titles: Arc::clone(&filtered_window_display_titles),
                                        app_highlight_segments: Arc::clone(&filtered_app_highlight_segments),
                                        app_name_highlight_segments: Arc::clone(&filtered_app_name_highlight_segments),
                                        window_highlight_segments: Arc::clone(&filtered_window_highlight_segments),
                                        app_title_is_typos: Arc::clone(&filtered_app_title_is_typos),
                                        window_title_is_typos: Arc::clone(&filtered_window_title_is_typos),
                                    },
                                });
                            } else {
                                self.filtered_search_cache = None;
                            }
                        }

		                    if self.mode == LauncherMode::Windows {
                            if search_query_changed {
                                self.selected_index = 0;
                                self.last_selected_window_id = None;
                                self.active_pane = ActivePane::Windows;
                            } else if let Some(ref last_id) = self.last_selected_window_id {
	                            if let Some(pos) = filtered_windows.iter().position(|w| &w.id == last_id) {
	                                self.selected_index = pos;
	                            }
	                        }
                    }

                    let show_run_in_terminal_action = self.mode == LauncherMode::Windows
                        && has_search_query
                        && self.show_run_in_terminal;
                    let show_cd_in_terminal_action = self.mode == LauncherMode::Windows
                        && has_search_query
                        && self.show_cd_in_terminal;
                    let terminal_run_result_index =
                        show_run_in_terminal_action.then_some(filtered_windows.len());
                    let terminal_cd_result_index = show_cd_in_terminal_action
                        .then_some(filtered_windows.len() + usize::from(show_run_in_terminal_action));
                    let terminal_action_count =
                        usize::from(show_run_in_terminal_action) + usize::from(show_cd_in_terminal_action);

                    let total_items = match self.mode {
                        LauncherMode::Apps => filtered_apps.len(),
                        LauncherMode::Windows => filtered_windows.len() + terminal_action_count,
                    };

                    // Safety bounds check for list changes (run early to prevent index out of bounds)
                    if self.selected_index >= total_items {
                        self.selected_index = 0;
                    }
                    if self.side_panel_selected_index >= filtered_apps.len() {
                        self.side_panel_selected_index = 0;
                    }

                    let duplicate_window_titles: HashMap<(String, String), usize> = if self.mode
                        == LauncherMode::Windows
                    {
                        let mut counts = HashMap::new();
                        for window in filtered_windows.iter() {
                            if let Some(key) = duplicate_window_group_key(window) {
                                *counts.entry(key).or_insert(0) += 1;
                            }
                        }
                        counts
                    } else {
                        HashMap::new()
                    };

                    let render_side_panel = self.mode == LauncherMode::Windows;
                    let mut scroll_to_selected = false;
                    let mut scroll_to_side_selected = false;
                    if self.scroll_to_first_window_on_focus {
                        self.selected_index = 0;
                        self.active_pane = ActivePane::Windows;
                        scroll_to_selected = true;
                        self.scroll_to_first_window_on_focus = false;
                    }
                    let columns = if self.icon_only && self.mode == LauncherMode::Apps {
                        self.rendered_app_grid_columns.max(1)
                    } else {
                        1
                    };
                    let side_panel_columns = if render_side_panel && self.icon_only {
                        self.rendered_side_panel_grid_columns.max(1)
                    } else {
                        1
                    };

                    if ctx.input(|i| i.key_pressed(egui::Key::F10)) {
                        if self.show_settings_menu {
                            self.close_settings_menu();
                        } else {
                            self.open_settings_menu();
                        }
                    }

                    if ctx.input(|i| i.key_pressed(egui::Key::F9)) {
                        if self.show_history_popup { self.close_history_popup(); } else { self.open_history_popup(); }
                    }

                    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        if self.show_history_popup {
                            self.close_history_popup();
                        } else if self.show_settings_menu {
                            self.close_settings_menu();
                        } else if self.process_chain_popup.is_some() {
                            self.process_chain_popup = None;
                        } else if self.app_info_popup.is_some() {
                            self.app_info_popup = None;
                        } else {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }

                    if !self.show_settings_menu {
                        // Keyboard navigation inputs
                        if render_side_panel && self.icon_only {
	                            if self.active_pane == ActivePane::Windows {
	                                if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight))
	                                    && !filtered_apps.is_empty()
		                                {
			                                    let target_y = self
			                                        .rendered_window_row_centers
			                                        .get(self.selected_index)
			                                        .copied()
			                                        .unwrap_or_else(|| {
                                                    let row_height = effective_list_row_height(
                                                        self.win_row_height,
                                                        self.win_icon_size,
                                                        self.win_top_padding
                                                            + self.win_bottom_padding,
                                                        self.win_line_height,
                                                        self.win_text_spacing,
                                                        self.win_show_path,
                                                    );
                                                    self.selected_index as f32 * row_height
                                                });
		                                    self.side_panel_selected_index = nearest_center_index(
		                                        &self.rendered_side_panel_item_centers,
		                                        target_y,
		                                    )
		                                    .unwrap_or(0)
		                                    .min(filtered_apps.len() - 1);
		                                    self.active_pane = ActivePane::Apps;
		                                    scroll_to_side_selected = true;
		                                }
                                if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown))
                                    && total_items > 0
                                {
                                    self.selected_index = (self.selected_index + 1) % total_items;
                                    scroll_to_selected = true;
                                }
                                if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp))
                                    && total_items > 0
                                {
                                    self.selected_index = if self.selected_index == 0 {
                                        total_items - 1
                                    } else {
                                        self.selected_index - 1
                                    };
                                    scroll_to_selected = true;
                                }
		                            } else if !filtered_apps.is_empty() {
		                                if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
		                                    if self.side_panel_selected_index % side_panel_columns == 0 {
		                                        let target_y = self
		                                            .rendered_side_panel_item_centers
		                                            .get(self.side_panel_selected_index)
		                                            .copied()
		                                            .unwrap_or(self.side_panel_selected_index as f32);
		                                        if total_items > 0 {
		                                            self.selected_index = nearest_center_index(
		                                                &self.rendered_window_row_centers,
		                                                target_y,
		                                            )
		                                            .unwrap_or(0)
		                                            .min(total_items - 1);
		                                        }
		                                        self.active_pane = ActivePane::Windows;
		                                        scroll_to_selected = true;
		                                    } else {
	                                        self.side_panel_selected_index -= 1;
	                                        scroll_to_side_selected = true;
	                                    }
	                                }
                                if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                                    self.side_panel_selected_index =
                                        (self.side_panel_selected_index + 1) % filtered_apps.len();
                                    scroll_to_side_selected = true;
                                }
	                                if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
	                                    self.side_panel_selected_index =
	                                        grid_move_down(
	                                            self.side_panel_selected_index,
	                                            filtered_apps.len(),
	                                            side_panel_columns,
	                                        );
	                                    scroll_to_side_selected = true;
	                                }
	                                if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
	                                    self.side_panel_selected_index =
	                                        grid_move_up(
	                                            self.side_panel_selected_index,
	                                            filtered_apps.len(),
	                                            side_panel_columns,
	                                        );
	                                    scroll_to_side_selected = true;
	                                }
                            }
                        } else if self.icon_only && self.mode == LauncherMode::Apps {
                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) && total_items > 0 {
                                self.selected_index = (self.selected_index + 1) % total_items;
                                scroll_to_selected = true;
                            }
                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) && total_items > 0 {
                                self.selected_index = if self.selected_index == 0 {
                                    total_items - 1
                                } else {
                                    self.selected_index - 1
                                };
                                scroll_to_selected = true;
                            }
	                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) && total_items > 0 {
	                                self.selected_index =
	                                    grid_move_down(self.selected_index, total_items, columns);
	                                scroll_to_selected = true;
	                            }
	                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) && total_items > 0 {
	                                self.selected_index =
	                                    grid_move_up(self.selected_index, total_items, columns);
	                                scroll_to_selected = true;
	                            }
                        } else {
                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) && total_items > 0 {
                                self.selected_index = (self.selected_index + 1) % total_items;
                                scroll_to_selected = true;
                            }
                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) && total_items > 0 {
                                self.selected_index = if self.selected_index == 0 {
                                    total_items - 1
                                } else {
                                    self.selected_index - 1
                                };
                                scroll_to_selected = true;
                            }
                        }

                        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) && total_items > 0 {
                            match self.mode {
                                LauncherMode::Apps => {
                                    let app = &filtered_apps[self.selected_index].0;
                                    self.open_or_launch_app_and_exit(app, ctx);
                                }
                                LauncherMode::Windows => {
                                    if render_side_panel && self.icon_only && self.active_pane == ActivePane::Apps {
                                        if let Some(app) =
                                            filtered_apps.get(self.side_panel_selected_index).map(|item| &item.0)
                                        {
                                            self.open_or_launch_app_and_exit(app, ctx);
                                        }
                                    } else if terminal_run_result_index == Some(self.selected_index)
                                    {
                                        launch_terminal_command(&search_query);
                                        ctx.request_repaint();
                                    } else if terminal_cd_result_index == Some(self.selected_index)
                                    {
                                        launch_terminal_cd(&search_query);
                                        ctx.request_repaint();
                                    } else {
                                        let win = &filtered_windows[self.selected_index];
                                        self.activate_and_exit(win.id.clone(), ctx);
                                    }
                                }
                            }
                        }
                        if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
                            match self.mode {
                                LauncherMode::Apps => self.refresh_apps(),
                                LauncherMode::Windows => {
                                    self.refresh_windows();
                                    self.start_background_app_load();
                                }
                            }
                        }
                        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::P)) && total_items > 0 {
                            if let LauncherMode::Apps = self.mode {
                                let app = &filtered_apps[self.selected_index].0;
                                let path = app.desktop_file_path.clone();
                                if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                    self.pinned_apps.remove(pos);
                                } else {
                                    self.pinned_apps.push(path);
                                }
                                self.save_pinned_apps();
                            }
                        }
                        if let LauncherMode::Apps = self.mode {
                            if total_items > 0 {
                                let app = &filtered_apps[self.selected_index].0;
                                let path = app.desktop_file_path.clone();
                                if self.pinned_apps.contains(&path) {
                                    if self.icon_only {
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowLeft)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos > 0 {
                                                    self.pinned_apps.swap(pos, pos - 1);
                                                    self.selected_index -= 1;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowRight)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos + 1 < self.pinned_apps.len() {
                                                    self.pinned_apps.swap(pos, pos + 1);
                                                    self.selected_index += 1;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowUp)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos >= columns {
                                                    self.pinned_apps.swap(pos, pos - columns);
                                                    self.selected_index -= columns;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowDown)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos + columns < self.pinned_apps.len() {
                                                    self.pinned_apps.swap(pos, pos + columns);
                                                    self.selected_index += columns;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                    } else {
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowUp)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos > 0 {
                                                    self.pinned_apps.swap(pos, pos - 1);
                                                    self.selected_index -= 1;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowDown)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos + 1 < self.pinned_apps.len() {
                                                    self.pinned_apps.swap(pos, pos + 1);
                                                    self.selected_index += 1;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

	                    // 3. Render Items ScrollArea
		                    let list_height = ui.available_height().max(100.0);
	                    let window_icon_size = egui::vec2(self.win_icon_size, self.win_icon_size);
	                    let app_icon_size = egui::vec2(self.app_icon_size, self.app_icon_size);
                        let window_row_height = effective_list_row_height(
                            self.win_row_height,
                            window_icon_size.y,
                            self.win_top_padding + self.win_bottom_padding,
                            self.win_line_height,
                            self.win_text_spacing,
                            self.win_show_path,
                        );
                        let app_row_height = effective_list_row_height(
                            self.win_row_height,
                            app_icon_size.y,
                            self.app_top_padding + self.app_bottom_padding,
                            self.win_line_height,
                            self.win_text_spacing,
                            self.win_show_path,
                        );
	                    let row_height = match self.mode {
                            LauncherMode::Apps => app_row_height,
                            LauncherMode::Windows => window_row_height,
                        };

                    if render_side_panel {
                        let previous_spacing = ui.spacing().item_spacing;
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.columns(2, |panes| {
                            let ui = &mut panes[0];

	                    if total_items == 0 {
	                        if self.mode == LauncherMode::Windows {
	                            self.rendered_window_row_centers.clear();
	                        }
	                        ui.allocate_ui(egui::vec2(ui.available_width(), list_height), |ui| {
                            ui.vertical_centered(|ui| {
                                ui.add_space(80.0);
                                if self.loading {
                                    ui.add(egui::Spinner::new().size(24.0));
                                    ui.add_space(10.0);
                                    ui.label(
                                        egui::RichText::new(match self.mode {
                                            LauncherMode::Apps => "Loading installed applications...",
                                            LauncherMode::Windows => "Loading open windows...",
                                        })
                                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 120))
                                        .size(14.0),
                                    );
                                } else {
                                    ui.label(
                                        egui::RichText::new(match self.mode {
                                            LauncherMode::Apps => "No matching installed applications found",
                                            LauncherMode::Windows => "No matching open windows found",
                                        })
                                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 120))
                                        .size(15.0),
                                    );
                                }
                            });
                        });
                    } else if self.icon_only && self.mode == LauncherMode::Apps {
                        let sensitivity = self.app_scroll_sensitivity;
                        egui::ScrollArea::vertical()
                            .wheel_scroll_multiplier(egui::vec2(1.0, sensitivity))
                            .id_salt("apps_main_icon_grid_scroll")
                            .max_height(list_height)
                            .show(ui, |ui| {
	                                ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
	                                let mut rendered_columns = 0usize;
	                                let mut first_row_y = None;
	                                ui.horizontal_wrapped(|ui| {
		                                    for index in 0..total_items {
		                                        let is_selected = index == self.selected_index;
			                                        let app = &filtered_apps[index].0;
			                                        let tile_size = self.app_icon_tile_size;
                                                let audio_level =
                                                    app_audio_level(
                                                        app,
                                                        &self.cached_sink_inputs,
                                                        &self.active_media_app_keys,
                                                        &self.observed_pipewire_node_ids,
                                                        &self.active_pipewire_node_ids,
                                                        self.pipewire_activity_cache_valid,
                                                    );

                                        let (rect, response) = ui.allocate_exact_size(
                                            egui::vec2(tile_size, tile_size),
	                                            egui::Sense::click(),
	                                        );

	                                        let center_y = rect.center().y;
	                                        match first_row_y {
	                                            None => {
	                                                first_row_y = Some(center_y);
	                                                rendered_columns = 1;
	                                            }
	                                            Some(row_y) if (center_y - row_y).abs() < 1.0 => {
	                                                rendered_columns += 1;
	                                            }
	                                            Some(_) => {}
	                                        }

		                                        show_immediate_icon_tooltip(&response, &app.name);

                                        response.clone().context_menu(|ui| {
                                            let path = app.desktop_file_path.clone();
                                            let is_pinned = self.pinned_apps.contains(&path);
                                            let label = if is_pinned { "📌 Unpin application" } else { "📌 Pin application" };
                                            if ui.button(label).clicked() {
                                                if is_pinned {
                                                    if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                        self.pinned_apps.remove(pos);
                                                    }
                                                } else {
                                                    self.pinned_apps.push(path.clone());
                                                }
                                                self.save_pinned_apps();
                                                ui.close();
                                            }

                                            if is_pinned {
                                                if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                    if pos >= columns {
                                                        if ui.button("⬆ Move up").clicked() {
                                                            self.pinned_apps.swap(pos, pos - columns);
                                                            self.save_pinned_apps();
                                                            ui.close();
                                                        }
                                                    }
                                                    if pos + columns < self.pinned_apps.len() {
                                                        if ui.button("⬇ Move down").clicked() {
                                                            self.pinned_apps.swap(pos, pos + columns);
                                                            self.save_pinned_apps();
                                                            ui.close();
                                                        }
                                                    }
                                                    if pos > 0 {
                                                        if ui.button("⬅ Move left").clicked() {
                                                            self.pinned_apps.swap(pos, pos - 1);
                                                            self.save_pinned_apps();
                                                            ui.close();
                                                        }
                                                    }
                                                    if pos + 1 < self.pinned_apps.len() {
                                                        if ui.button("➡ Move right").clicked() {
                                                            self.pinned_apps.swap(pos, pos + 1);
                                                            self.save_pinned_apps();
                                                            ui.close();
                                                        }
                                                    }
                                                }
                                            }
                                            ui.separator();
                                            let has_open_window = self.windows.iter().any(|window| {
                                                self.desktop_file_path_for_window(window).as_ref()
                                                    == Some(&app.desktop_file_path)
                                            });
                                            if ui
                                                .add_enabled(has_open_window, egui::Button::new("Open window"))
                                                .clicked()
                                            {
                                                self.open_window_for_app_and_exit(app, ctx);
                                                ui.close();
                                            }
                                            if ui.button("Open new window").clicked() {
                                                self.launch_app_and_exit(app, ctx);
                                                ui.close();
                                            }
                                            if ui.button("Show info").clicked() {
                                                self.app_info_popup = Some(app.clone());
                                                ui.close();
                                            }
                                        });

                                        if is_selected && scroll_to_selected {
                                            response.scroll_to_me(None);
                                        }

                                        if response.clicked() {
                                            self.open_or_launch_app_and_exit(app, ctx);
                                        } else if response.middle_clicked() {
                                            self.launch_app_and_exit(app, ctx);
                                        }

	                                        let bg_color = if is_selected {
	                                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18)
	                                        } else if response.hovered() {
	                                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10)
	                                        } else {
	                                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 4)
	                                        };

                                        ui.painter().rect_filled(
                                            rect,
                                            egui::CornerRadius::same(12),
                                            bg_color,
                                        );

                                        if is_selected {
                                            ui.painter().rect_stroke(
                                                rect,
                                                egui::CornerRadius::same(12),
                                                egui::Stroke::new(1.5, egui::Color32::from_rgb(61, 174, 233)),
                                                egui::StrokeKind::Inside,
                                            );
                                        } else {
                                            ui.painter().rect_stroke(
                                                rect,
                                                egui::CornerRadius::same(12),
                                                egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10)),
                                                egui::StrokeKind::Inside,
                                            );
                                        }

                                        let inner_rect = rect.shrink2(egui::vec2(6.0, 6.0));
                                        let label_height = if self.app_icon_show_name {
                                            (self.app_icon_name_size + 10.0).max(16.0)
                                        } else {
                                            0.0
                                        };
                                        let icon_center_y = inner_rect.min.y
                                            + (inner_rect.height() - label_height) / 2.0;
	                                        let icon_rect = egui::Rect::from_center_size(
	                                            egui::pos2(rect.center().x, icon_center_y),
	                                            app_icon_size,
	                                        );
                                                if let Some(level) = audio_level {
                                                    paint_audio_activity_ring(
                                                        ui.painter(),
                                                        icon_rect,
                                                        level,
                                                        ctx.input(|i| i.time) as f32,
                                                    );
                                                }
	                                        let label_rect = egui::Rect::from_min_max(
                                            egui::pos2(inner_rect.min.x, inner_rect.max.y - label_height),
                                            inner_rect.max,
                                        );

                                        paint_icon_in_rect(
                                            ui,
                                            app.icon_path.as_ref(),
                                            icon_rect,
                                            app_icon_size,
                                        );

                                        if self.pinned_apps.contains(&app.desktop_file_path) {
                                            let badge_pos = egui::pos2(rect.max.x - 12.0, rect.min.y + 12.0);
                                            ui.painter().text(
                                                badge_pos,
                                                egui::Align2::CENTER_CENTER,
                                                "📌",
                                                egui::FontId::proportional(11.0),
                                                egui::Color32::WHITE,
                                            );
                                        }

                                        if self.app_icon_show_name {
                                            let label = truncate_tile_label(&app.name, tile_size);
                                            paint_centered_title_job(
                                                ui,
                                                label_rect,
                                                &label,
                                                self.app_icon_name_size,
                                                filtered_app_name_highlight_segments
                                                    .get(index)
                                                    .map(Vec::as_slice)
                                                    .unwrap_or(&[]),
                                                egui::Color32::from_rgba_unmultiplied(
                                                    255,
                                                    255,
                                                    255,
                                                    210,
                                                ),
                                            );
                                        }
	                                    }
	                                });
	                                self.rendered_app_grid_columns = rendered_columns.max(1);
	                            });
                    } else {
                        let mut rendered_window_row_centers = Vec::new();
                        let sensitivity = match self.mode {
                            LauncherMode::Apps => self.app_scroll_sensitivity,
                            LauncherMode::Windows => self.win_scroll_sensitivity,
                        };
                        egui::ScrollArea::vertical()
                            .wheel_scroll_multiplier(egui::vec2(1.0, sensitivity))
                            .id_salt(match self.mode {
                                LauncherMode::Apps => "apps_main_list_scroll",
                                LauncherMode::Windows => "windows_main_list_scroll",
	                            })
	                            .max_height(list_height)
	                            .show(ui, |ui| {
                                    let previous_item_spacing = ui.spacing().item_spacing;
                                    ui.spacing_mut().item_spacing.y = 0.0;
				                                for index in 0..total_items {
                                            let terminal_action_label =
                                                if self.mode == LauncherMode::Windows
                                                    && terminal_run_result_index == Some(index)
                                                {
                                                    Some("run in Terminal")
                                                } else if self.mode == LauncherMode::Windows
                                                    && terminal_cd_result_index == Some(index)
                                                {
                                                    Some("cd in Terminal")
                                                } else {
                                                    None
                                                };
			                                    let is_selected = index == self.selected_index
			                                        && (self.mode == LauncherMode::Apps
			                                            || self.active_pane == ActivePane::Windows);
                                            let has_duplicate_window_title = self.mode
                                                == LauncherMode::Windows
                                                && filtered_windows
                                                    .get(index)
                                                    .and_then(duplicate_window_group_key)
                                                    .and_then(|key| {
                                                        duplicate_window_titles
                                                            .get(&key)
                                                            .copied()
                                                    })
                                                    .is_some_and(|count| count > 1);
                                            let demands_attention = self.mode
                                                == LauncherMode::Windows
                                                && filtered_windows
                                                    .get(index)
                                                    .is_some_and(window_requires_attention);

		                                    let (rect, response) = ui.allocate_exact_size(
	                                        egui::vec2(ui.available_width(), row_height),
	                                        egui::Sense::click(),
	                                    );
	                                    let row_visual_rect = rect.intersect(ui.clip_rect());
	                                    if self.mode == LauncherMode::Windows {
	                                        rendered_window_row_centers.push(rect.center().y);
	                                    }

                                     if is_selected && scroll_to_selected {
                                         response.scroll_to_me(None);
                                     }

		                                    let bg_color = if is_selected && demands_attention {
		                                        egui::Color32::from_rgba_unmultiplied(235, 64, 64, 96)
		                                    } else if demands_attention && response.hovered() {
		                                        egui::Color32::from_rgba_unmultiplied(235, 64, 64, 82)
		                                    } else if demands_attention {
		                                        egui::Color32::from_rgba_unmultiplied(235, 64, 64, 64)
		                                    } else if is_selected && has_duplicate_window_title {
		                                        egui::Color32::from_rgba_unmultiplied(255, 214, 92, 48)
		                                    } else if is_selected {
		                                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18)
		                                    } else if has_duplicate_window_title && response.hovered() {
		                                        egui::Color32::from_rgba_unmultiplied(255, 214, 92, 42)
		                                    } else if has_duplicate_window_title {
		                                        egui::Color32::from_rgba_unmultiplied(255, 214, 92, 30)
		                                    } else if response.hovered() {
		                                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 9)
		                                    } else {
		                                        egui::Color32::TRANSPARENT
		                                    };

                                    ui.painter().rect_filled(
                                        row_visual_rect,
                                        egui::CornerRadius::same(8),
                                        bg_color,
                                    );
                                    if is_selected {
                                        ui.painter().rect_stroke(
                                            row_visual_rect.shrink(0.5),
                                            egui::CornerRadius::same(8),
                                            egui::Stroke::new(
                                                1.0,
                                                egui::Color32::from_rgba_unmultiplied(
                                                    61, 174, 233, 140,
                                                ),
                                            ),
                                            egui::StrokeKind::Inside,
                                        );
                                    }

                                    // Premium left accent highlight bar
                                    if is_selected {
                                        let accent_size =
                                            selected_row_accent_size(row_visual_rect.height());
                                        let accent_rect = egui::Rect::from_center_size(
                                            egui::pos2(
                                                row_visual_rect.min.x + 2.0 + accent_size.x / 2.0,
                                                row_visual_rect.center().y,
                                            ),
                                            accent_size,
                                        );
                                        ui.painter().rect_filled(
                                            accent_rect,
                                            egui::CornerRadius::same(2),
                                            egui::Color32::from_rgb(61, 174, 233), // KDE blue theme accent
                                        );
                                    }

                                    // Content placement
                                    let content_rect = inset_rect(
                                        rect,
                                        self.win_left_padding,
                                        self.win_right_padding,
                                        self.win_top_padding,
                                        self.win_bottom_padding,
                                    );
                                    let last_activation_rect = if self.mode == LauncherMode::Windows
                                        && self.win_show_last_activation
                                    {
                                        Some(egui::Rect::from_min_max(
                                            egui::pos2(
                                                content_rect.max.x
                                                    - WINDOW_LAST_ACTIVATION_COLUMN_WIDTH,
                                                content_rect.min.y,
                                            ),
                                            content_rect.max,
                                        ))
                                    } else {
                                        None
                                    };
                                    let content_rect = if last_activation_rect.is_some() {
                                        egui::Rect::from_min_max(
                                            content_rect.min,
                                            egui::pos2(
                                                content_rect.max.x
                                                    - WINDOW_LAST_ACTIVATION_COLUMN_WIDTH,
                                                content_rect.max.y,
                                            ),
                                        )
                                    } else {
                                        content_rect
                                    };
                                    let mut child_ui = ui.new_child(
                                        egui::UiBuilder::new()
                                            .max_rect(content_rect)
                                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                                    );

                                    match self.mode {
	                                        LauncherMode::Apps => {
	                                            let app = &filtered_apps[index].0;
                                                let audio_level =
                                                    app_audio_level(
                                                        app,
                                                        &self.cached_sink_inputs,
                                                        &self.active_media_app_keys,
                                                        &self.observed_pipewire_node_ids,
                                                        &self.active_pipewire_node_ids,
                                                        self.pipewire_activity_cache_valid,
                                                    );

		                                            // Icon render
                                                let (icon_rect, _) = child_ui.allocate_exact_size(
                                                    app_icon_size,
                                                    egui::Sense::hover(),
                                                );
                                                if let Some(level) = audio_level {
                                                    paint_audio_activity_ring(
                                                        child_ui.painter(),
                                                        icon_rect,
                                                        level,
                                                        ctx.input(|i| i.time) as f32,
                                                    );
                                                }
                                                paint_icon_in_rect(
                                                    &mut child_ui,
                                                    app.icon_path.as_ref(),
                                                    icon_rect,
                                                    app_icon_size,
                                                );

                                            child_ui.add_space(10.0);

	                                            let display_title = filtered_app_display_titles
	                                                .get(index)
	                                                .map(String::as_str)
	                                                .unwrap_or(&app.name);
			                                            let show_search_metadata =
			                                                !search_query.trim().is_empty();
			                                            let mut label_clicked = false;
                                                let _title_is_typo = filtered_app_title_is_typos
                                                    .get(index)
                                                    .copied()
                                                    .unwrap_or(false);
	                                            if self.win_show_path {
                                                let text_min_x = content_rect.min.x
                                                    + app_icon_size.x
                                                    + 10.0;
                                                let text_rect = egui::Rect::from_min_max(
                                                    egui::pos2(text_min_x, content_rect.min.y),
                                                    content_rect.max,
                                                );
                                                let mut text_ui = ui.new_child(
                                                    egui::UiBuilder::new()
                                                        .max_rect(text_rect)
                                                        .layout(egui::Layout::top_down(
                                                            egui::Align::Min,
                                                        )),
                                                );
		                                                text_ui.spacing_mut().item_spacing.y = 0.0;
		                                                let text_block_height = if show_search_metadata {
		                                                    self.win_line_height
		                                                } else {
		                                                    self.win_line_height
		                                                        + self.win_line_height * 0.8
		                                                        + self.win_text_spacing
		                                                };
		                                                text_ui.add_space(
		                                                    ((content_rect.height() - text_block_height) / 2.0)
		                                                        .max(0.0),
		                                                );

	                                                let title_response = text_ui.add(
		                                                    egui::Label::new(
                                                                highlighted_title_job_from_segments(
                                                                    &display_title,
                                                                    self.win_title_size,
                                                                    filtered_app_highlight_segments
                                                                        .get(index)
                                                                        .map(|segments| segments.as_slice())
                                                                        .unwrap_or(&[]),
                                                                ),
                                                            )
	                                                    .sense(egui::Sense::click())
	                                                    .truncate(),
	                                                );
                                                if title_response.clicked() {
                                                    label_clicked = true;
                                                }
                                                if self.disable_ibeam && title_response.hovered() {
                                                    text_ui
                                                        .ctx()
                                                        .set_cursor_icon(egui::CursorIcon::Default);
                                                }

                                                    if !show_search_metadata {
	                                                    text_ui.add_space(self.win_text_spacing);

	                                                    let is_link =
	                                                        std::fs::symlink_metadata(&app.desktop_file_path)
	                                                            .map(|m| m.file_type().is_symlink())
	                                                            .unwrap_or(false);
	                                                    let mut subtext =
	                                                        app.desktop_file_path.to_string_lossy().to_string();
	                                                    if is_link {
	                                                        subtext.push('@');
	                                                    }
	                                                    let path_response = text_ui.add(
	                                                        egui::Label::new(
	                                                            egui::RichText::new(subtext)
	                                                                .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 130))
	                                                                .size(self.win_path_size)
	                                                                .line_height(Some(self.win_line_height * 0.8)),
	                                                        )
	                                                        .sense(egui::Sense::click())
	                                                        .truncate(),
	                                                    );
	                                                    if path_response.clicked() {
	                                                        label_clicked = true;
	                                                    }
	                                                    if self.disable_ibeam && path_response.hovered() {
	                                                        text_ui
	                                                            .ctx()
	                                                            .set_cursor_icon(egui::CursorIcon::Default);
	                                                    }
                                                    }
                                                if self.pinned_apps.contains(&app.desktop_file_path) {
                                                    text_ui.add_space(4.0);
                                                    text_ui.label(
                                                        egui::RichText::new("📌")
                                                            .size(11.0)
                                                            .color(egui::Color32::from_rgb(61, 174, 233)),
                                                    );
                                                }
                                            } else {
	                                                let title_response = child_ui.add(
		                                                    egui::Label::new(
                                                                highlighted_title_job_from_segments(
                                                                    &display_title,
                                                                    self.win_title_size,
                                                                    filtered_app_highlight_segments
                                                                        .get(index)
                                                                        .map(|segments| segments.as_slice())
                                                                        .unwrap_or(&[]),
                                                                ),
                                                            )
	                                                    .sense(egui::Sense::click())
	                                                    .truncate(),
	                                                );
                                                if title_response.clicked() {
                                                    label_clicked = true;
                                                }
                                                if self.disable_ibeam && title_response.hovered() {
                                                    child_ui
                                                        .ctx()
                                                        .set_cursor_icon(egui::CursorIcon::Default);
                                                }
                                                if self.pinned_apps.contains(&app.desktop_file_path) {
                                                    child_ui.add_space(4.0);
                                                    child_ui.label(
                                                        egui::RichText::new("📌")
                                                            .size(11.0)
                                                            .color(egui::Color32::from_rgb(61, 174, 233)),
                                                    );
                                                }
                                            }

                                            if label_clicked {
                                                self.open_or_launch_app_and_exit(app, ctx);
                                            }
	                                        }
		                                        LauncherMode::Windows => {
                                                if let Some(terminal_action_label) = terminal_action_label {
                                                    let (icon_rect, _) = child_ui.allocate_exact_size(
                                                        window_icon_size,
                                                        egui::Sense::hover(),
                                                    );
                                                    child_ui.painter().rect_filled(
                                                        icon_rect.shrink(2.0),
                                                        egui::CornerRadius::same(5),
                                                        egui::Color32::from_rgba_unmultiplied(
                                                            61, 174, 233, 45,
                                                        ),
                                                    );
                                                    child_ui.painter().rect_stroke(
                                                        icon_rect.shrink(2.0),
                                                        egui::CornerRadius::same(5),
                                                        egui::Stroke::new(
                                                            1.0,
                                                            egui::Color32::from_rgba_unmultiplied(
                                                                61, 174, 233, 120,
                                                            ),
                                                        ),
                                                        egui::StrokeKind::Inside,
                                                    );
                                                    child_ui.painter().text(
                                                        icon_rect.center(),
                                                        egui::Align2::CENTER_CENTER,
                                                        ">_",
                                                        egui::FontId::monospace(15.0),
                                                        egui::Color32::from_rgba_unmultiplied(
                                                            255, 255, 255, 220,
                                                        ),
                                                    );
                                                    child_ui.add_space(10.0);

                                                    if self.win_show_path {
                                                        child_ui.vertical(|ui| {
                                                            ui.spacing_mut().item_spacing.y = 0.0;
                                                            ui.label(
                                                                egui::RichText::new(terminal_action_label)
                                                                    .color(egui::Color32::WHITE)
                                                                    .strong()
                                                                    .size(self.win_title_size)
                                                                    .line_height(Some(self.win_line_height)),
                                                            );
                                                            ui.add_space(self.win_text_spacing);
                                                            ui.label(
                                                                egui::RichText::new(&search_query)
                                                                    .color(egui::Color32::from_rgba_unmultiplied(
                                                                        255, 255, 255, 130,
                                                                    ))
                                                                    .size(self.win_path_size)
                                                                    .line_height(Some(
                                                                        self.win_line_height * 0.8,
                                                                    )),
                                                            );
                                                        });
                                                    } else {
                                                        child_ui.label(
                                                            egui::RichText::new(terminal_action_label)
                                                                .color(egui::Color32::WHITE)
                                                                .strong()
                                                                .size(self.win_title_size)
                                                                .line_height(Some(self.win_line_height)),
                                                        );
                                                    }
                                                } else {
		                                            let win = &filtered_windows[index];
	                                                let audio_level = self
                                                        .window_audio_cache
                                                        .level_buckets
                                                        .get(&win.id)
                                                        .map(|level| *level as f32 / 100.0);

		                                            // Icon render
                                                let (icon_rect, _) = child_ui.allocate_exact_size(
                                                    window_icon_size,
                                                    egui::Sense::hover(),
                                                );
                                                if let Some(level) = audio_level {
                                                    paint_audio_activity_ring(
                                                        child_ui.painter(),
                                                        icon_rect,
                                                        level,
                                                        ctx.input(|i| i.time) as f32,
                                                    );
                                                }
                                                paint_icon_in_rect(
                                                    &mut child_ui,
                                                    win.icon_path.as_ref(),
                                                    icon_rect,
                                                    window_icon_size,
                                                );

                                            child_ui.add_space(10.0);

                                            let display_title = filtered_window_display_titles
                                                .get(index)
                                                .map(String::as_str)
                                                .unwrap_or(win.title.as_str());
		                                            let show_search_metadata =
		                                                !search_query.trim().is_empty();
                                                let _title_is_typo = filtered_window_title_is_typos
                                                    .get(index)
                                                    .copied()
                                                    .unwrap_or(false);

                                            if self.win_show_path {
                                                child_ui.vertical(|ui| {
                                                    ui.spacing_mut().item_spacing.y = 0.0;

	                                                    let title_response = ui.add(
	                                                        egui::Label::new(
                                                                    highlighted_title_job_from_segments(
                                                                        &display_title,
                                                                        self.win_title_size,
                                                                        filtered_window_highlight_segments
                                                                            .get(index)
                                                                            .map(|segments| segments.as_slice())
                                                                            .unwrap_or(&[]),
                                                                    ),
                                                                )
	                                                        .sense(egui::Sense::hover())
	                                                        .truncate(),
	                                                    );
                                                    if self.disable_ibeam
                                                        && title_response.hovered()
                                                    {
                                                        ui.ctx().set_cursor_icon(
                                                            egui::CursorIcon::Default,
                                                        );
                                                    }

                                                    if !show_search_metadata {
	                                                        ui.add_space(self.win_text_spacing);

	                                                        let subtext = if let Some(ref path) = win.cwd_path
	                                                        {
	                                                            let path_display = display_path(path);
	                                                            if let Some(ref command_summary) =
	                                                                win.command_summary
	                                                            {
	                                                                if !normalize_app_match_key(command_summary)
	                                                                    .eq(&normalize_app_match_key(&path_display))
	                                                                {
	                                                                    format!(
	                                                                        "{} | {}",
	                                                                        path_display, command_summary
	                                                                    )
	                                                                } else {
	                                                                    path_display
	                                                                }
	                                                            } else {
	                                                                path_display
	                                                            }
	                                                        } else if let Some(ref command_summary) =
	                                                            win.command_summary
	                                                        {
	                                                            command_summary.clone()
	                                                        } else if let Some(ref path) = win.exe_path {
	                                                            let is_link = std::fs::symlink_metadata(path)
	                                                                .map(|m| m.file_type().is_symlink())
	                                                                .unwrap_or(false);
	                                                            let mut path_str = display_path(path);
	                                                            if is_link {
	                                                                path_str.push('@');
	                                                            }
	                                                            path_str
	                                                        } else if let Some(ref proc_name) =
	                                                            win.active_process
	                                                        {
	                                                            format!(
	                                                                "{} (running: {})",
	                                                                win.class, proc_name
	                                                            )
	                                                        } else {
	                                                            win.class.clone()
	                                                        };
	                                                        let path_response = ui.add(
	                                                            egui::Label::new(
	                                                                egui::RichText::new(subtext)
	                                                                    .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 130))
	                                                                    .size(self.win_path_size)
	                                                                    .line_height(Some(self.win_line_height * 0.8)),
	                                                            )
	                                                            .sense(egui::Sense::hover())
	                                                            .truncate(),
	                                                        );
	                                                        if self.disable_ibeam && path_response.hovered()
	                                                        {
	                                                            ui.ctx().set_cursor_icon(
	                                                                egui::CursorIcon::Default,
	                                                            );
	                                                        }
                                                    }
                                                });
                                            } else {
	                                                let title_response = child_ui.add(
	                                                    egui::Label::new(
                                                                highlighted_title_job_from_segments(
                                                                    &display_title,
                                                                    self.win_title_size,
                                                                    filtered_window_highlight_segments
                                                                        .get(index)
                                                                        .map(|segments| segments.as_slice())
                                                                        .unwrap_or(&[]),
                                                                ),
                                                            )
	                                                    .sense(egui::Sense::hover())
	                                                    .truncate(),
	                                                );
	                                                if self.disable_ibeam && title_response.hovered() {
	                                                    child_ui
	                                                        .ctx()
	                                                        .set_cursor_icon(egui::CursorIcon::Default);
	                                                }
	                                            }
                                                }
                                            }
                                        }

                                    if let Some(last_activation_rect) = last_activation_rect {
                                        if terminal_action_label.is_none()
                                            && let Some(window) = filtered_windows.get(index)
                                        {
                                            ui.painter().text(
                                                last_activation_rect.right_center(),
                                                egui::Align2::RIGHT_CENTER,
                                                format_last_activation_age(
                                                    window.last_activated_at_ms,
                                                ),
                                                egui::FontId::proportional(self.win_path_size),
                                                egui::Color32::from_rgba_unmultiplied(
                                                    255, 255, 255, 150,
                                                ),
                                            );
                                        }
                                    }

                                    let overlay_response = ui.interact(
                                        rect,
                                        ui.id().with(("main_row_overlay", index)),
                                        egui::Sense::click(),
                                    );

                                    if terminal_action_label.is_none() {
                                        overlay_response.clone().context_menu(|ui| {
                                            match self.mode {
                                                LauncherMode::Apps => {
                                                    let app = &filtered_apps[index].0;
                                                    let path = app.desktop_file_path.clone();
                                                    let is_pinned = self.pinned_apps.contains(&path);
                                                    let label = if is_pinned { "📌 Unpin application" } else { "📌 Pin application" };
                                                    if ui.button(label).clicked() {
                                                        if is_pinned {
                                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                                self.pinned_apps.remove(pos);
                                                            }
                                                        } else {
                                                            self.pinned_apps.push(path.clone());
                                                        }
                                                        self.save_pinned_apps();
                                                        ui.close();
                                                    }

                                                    if is_pinned {
                                                        if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                            if pos > 0 {
                                                                if ui.button("⬆ Move up").clicked() {
                                                                    self.pinned_apps.swap(pos, pos - 1);
                                                                    self.save_pinned_apps();
                                                                    ui.close();
                                                                }
                                                            }
                                                            if pos + 1 < self.pinned_apps.len() {
                                                                if ui.button("⬇ Move down").clicked() {
                                                                    self.pinned_apps.swap(pos, pos + 1);
                                                                    self.save_pinned_apps();
                                                                    ui.close();
                                                                }
                                                            }
                                                        }
                                                    }
                                                    ui.separator();
                                                    let has_open_window = self.windows.iter().any(|window| {
                                                        self.desktop_file_path_for_window(window).as_ref()
                                                            == Some(&app.desktop_file_path)
                                                    });
                                                    if ui
                                                        .add_enabled(has_open_window, egui::Button::new("Open window"))
                                                        .clicked()
                                                    {
                                                        self.open_window_for_app_and_exit(app, ctx);
                                                        ui.close();
                                                    }
                                                    if ui.button("Open new window").clicked() {
                                                        self.launch_app_and_exit(app, ctx);
                                                        ui.close();
                                                    }
                                                    if ui.button("Show info").clicked() {
                                                        self.app_info_popup = Some(app.clone());
                                                        ui.close();
                                                    }
                                                }
                                                LauncherMode::Windows => {
                                                    let win = &filtered_windows[index];
                                                    if ui.button("Clone window").clicked() {
                                                        self.active_pane = ActivePane::Windows;
                                                        self.selected_index = index;
                                                        self.clone_window_and_exit(win, ctx);
                                                        ui.close();
                                                    }
                                                    if ui.button("Open new window").clicked() {
                                                        self.active_pane = ActivePane::Windows;
                                                        self.selected_index = index;
                                                        self.launch_window_app_and_exit(win, ctx);
                                                        ui.close();
                                                    }
                                                    if ui.button("Show info").clicked() {
                                                        self.process_chain_popup = Some(win.clone());
                                                        ui.close();
                                                    }
                                                    if ui.button("Open window").clicked() {
                                                        self.active_pane = ActivePane::Windows;
                                                        self.selected_index = index;
                                                        self.activate_and_exit(win.id.clone(), ctx);
                                                        ui.close();
                                                    }
                                                    // Volume Control
                                                    let matching_sinks =
                                                        self.window_audio_cache
                                                            .sink_matches
                                                            .get(&win.id)
                                                            .cloned()
                                                            .unwrap_or_default();
                                                    if !matching_sinks.is_empty() {
                                                        ui.separator();
                                                        ui.label("🔊 Volume Control");
                                                        for sink in &matching_sinks {
                                                            let sink_index = sink.index;
                                                            let sink_process_id = sink
                                                                .properties
                                                                .get("application.process.id")
                                                                .cloned();
                                                            let current_vol =
                                                                sink_display_volume_percent(sink)
                                                                    as f32;
                                                            let mut current_mute = sink.mute;

                                                            ui.horizontal(|ui| {
                                                                // Mute button
                                                                let mute_label = if current_mute { "🔇" } else { "🔊" };
                                                                if ui.button(mute_label).clicked() {
                                                                    current_mute = !current_mute;
                                                                    for cached_sink in
                                                                        self.cached_sink_inputs.iter_mut()
                                                                    {
                                                                        let same_group = cached_sink.index == sink_index
                                                                            || sink_process_id.as_ref().is_some_and(|pid| {
                                                                                cached_sink
                                                                                    .properties
                                                                                    .get("application.process.id")
                                                                                    == Some(pid)
                                                                            });
                                                                        if same_group {
                                                                            set_sink_input_mute(
                                                                                cached_sink.index,
                                                                                current_mute,
                                                                            );
                                                                            cached_sink.mute =
                                                                                current_mute;
                                                                        }
                                                                    }
                                                                }

                                                                // Volume slider
                                                                let mut vol_val = current_vol as u32;
                                                                if ui.add(egui::Slider::new(&mut vol_val, 0..=100).show_value(true)).changed() {
                                                                    for cached_sink in
                                                                        self.cached_sink_inputs.iter_mut()
                                                                    {
                                                                        let same_group = cached_sink.index == sink_index
                                                                            || sink_process_id.as_ref().is_some_and(|pid| {
                                                                                cached_sink
                                                                                    .properties
                                                                                    .get("application.process.id")
                                                                                    == Some(pid)
                                                                            });
                                                                        if same_group {
                                                                            set_sink_input_volume(
                                                                                cached_sink.index,
                                                                                vol_val,
                                                                            );
                                                                            if let Some(chan) =
                                                                                cached_sink
                                                                                    .volume
                                                                                    .values_mut()
                                                                                    .next()
                                                                            {
                                                                                chan.value_percent =
                                                                                    format!(
                                                                                        "{}%",
                                                                                        vol_val
                                                                                    );
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            });
                                                        }
                                                    }

                                                    ui.separator();
                                                    if ui.button("Close application").clicked() {
                                                        self.close_window_and_exit(win.id.clone(), ctx);
                                                        ui.close();
                                                    }
                                                }
                                            }
                                        });
                                    }

	                                    if overlay_response.hovered() {
	                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
	                                    }

		                                    if overlay_response.clicked() {
		                                        match self.mode {
		                                            LauncherMode::Apps => {
		                                                self.selected_index = index;
		                                                let app = &filtered_apps[index].0;
		                                                self.launch_app_and_exit(app, ctx);
		                                            }
                                                    LauncherMode::Windows => {
		                                                self.active_pane = ActivePane::Windows;
		                                                self.selected_index = index;
                                                    if terminal_run_result_index == Some(index)
                                                    {
                                                        launch_terminal_command(&search_query);
                                                        ctx.request_repaint();
                                                    } else if terminal_cd_result_index == Some(index)
                                                    {
                                                        launch_terminal_cd(&search_query);
                                                        ctx.request_repaint();
                                                    } else {
		                                                let win = &filtered_windows[index];
		                                                self.activate_and_exit(win.id.clone(), ctx);
                                                    }
		                                            }
	                                        }
		                                    }

			                                    if overlay_response.middle_clicked() {
			                                        if let LauncherMode::Apps = self.mode {
                                                    self.selected_index = index;
                                                    let app = &filtered_apps[index].0;
                                                    self.launch_app_and_exit(app, ctx);
                                                } else if let LauncherMode::Windows = self.mode {
                                                    if terminal_action_label.is_some() {
                                                        continue;
                                                    }
											self.active_pane = ActivePane::Windows;
											self.selected_index = index;
											let win = &filtered_windows[index];
											self.clone_window_and_exit(win, ctx);
										}
			                                    }
		                                }
                                    ui.spacing_mut().item_spacing = previous_item_spacing;
			                            });
                        if self.mode == LauncherMode::Windows {
                            self.rendered_window_row_centers = rendered_window_row_centers;
                        } else {
                            self.rendered_window_row_centers.clear();
                        }
	                    }

	                            let ui = &mut panes[1];
	                            let edge_x = ui.min_rect().min.x;
	                            let edge_y = ui.min_rect().min.y;
	                            ui.painter().line_segment(
	                                [
	                                    egui::pos2(edge_x, edge_y),
	                                    egui::pos2(edge_x, edge_y + list_height),
	                                ],
	                                egui::Stroke::new(
	                                    1.0,
	                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18),
	                                ),
	                            );
                                    ui.vertical(|ui| {
                                        let sensitivity = self.app_scroll_sensitivity;
                                        egui::ScrollArea::vertical()
                                            .wheel_scroll_multiplier(egui::vec2(1.0, sensitivity))
                                            .id_salt("apps_side_panel_scroll")
                                            .max_height(list_height)
                                            .show(ui, |ui| {
		                                        if filtered_apps.is_empty() {
		                                            self.rendered_side_panel_item_centers.clear();
		                                            self.rendered_side_panel_grid_columns = 1;
		                                            ui.add_space(20.0);
	                                            ui.label(
	                                                egui::RichText::new(if self.loading {
	                                                    "Loading applications..."
	                                                } else {
	                                                    "No matching applications found"
	                                                })
	                                                .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 120))
	                                                .size(13.0),
	                                            );
		                                                } else if self.icon_only {
		                                                    ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
		                                                    let mut rendered_columns = 0usize;
		                                                    let mut first_row_y = None;
		                                                    let mut rendered_item_centers = Vec::new();
		                                                    ui.horizontal_wrapped(|ui| {
				                                                        for (index, item) in filtered_apps.iter().enumerate() {
			                                                            let app = &item.0;
			                                                            let tile_size = self.app_icon_tile_size;
                                                                    let audio_level =
                                                                        app_audio_level(
                                                                            app,
                                                                            &self.cached_sink_inputs,
                                                                            &self.active_media_app_keys,
                                                                            &self.observed_pipewire_node_ids,
                                                                            &self.active_pipewire_node_ids,
                                                                            self.pipewire_activity_cache_valid,
                                                                        );
		                                                            let is_selected = self.active_pane == ActivePane::Apps
	                                                                && index == self.side_panel_selected_index;
                                                            let (rect, response) = ui.allocate_exact_size(
                                                                egui::vec2(tile_size, tile_size),
	                                                                egui::Sense::click(),
	                                                            );
		                                                            let center_y = rect.center().y;
		                                                            rendered_item_centers.push(center_y);
		                                                            match first_row_y {
	                                                                None => {
	                                                                    first_row_y = Some(center_y);
	                                                                    rendered_columns = 1;
	                                                                }
	                                                                Some(row_y)
	                                                                    if (center_y - row_y).abs() < 1.0 =>
	                                                                {
	                                                                    rendered_columns += 1;
	                                                                }
	                                                                Some(_) => {}
	                                                            }
	                                                            show_immediate_icon_tooltip(&response, &app.name);
                                                            if is_selected && scroll_to_side_selected {
                                                                response.scroll_to_me(None);
                                                            }
		                                                    response.clone().context_menu(|ui| {
		                                                        let path = app.desktop_file_path.clone();
		                                                        let is_pinned = self.pinned_apps.contains(&path);
	                                                        let label = if is_pinned { "📌 Unpin application" } else { "📌 Pin application" };
	                                                        if ui.button(label).clicked() {
	                                                            if is_pinned {
	                                                                if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
	                                                                    self.pinned_apps.remove(pos);
	                                                                }
	                                                            } else {
	                                                                self.pinned_apps.push(path.clone());
	                                                            }
	                                                            self.save_pinned_apps();
		                                                            ui.close();
		                                                        }
                                                                if is_pinned {
                                                                    if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                                        if pos > 0 {
                                                                            if ui.button("⬆ Move up").clicked() {
                                                                                self.pinned_apps.swap(pos, pos - 1);
                                                                                self.save_pinned_apps();
                                                                                ui.close();
                                                                            }
                                                                        }
                                                                        if pos + 1 < self.pinned_apps.len() {
                                                                            if ui.button("⬇ Move down").clicked() {
                                                                                self.pinned_apps.swap(pos, pos + 1);
                                                                                self.save_pinned_apps();
                                                                                ui.close();
                                                                            }
                                                                        }
                                                                    }
                                                                }
	                                                ui.separator();
	                                                let has_open_window = self.windows.iter().any(|window| {
	                                                    self.desktop_file_path_for_window(window).as_ref()
	                                                        == Some(&app.desktop_file_path)
	                                                });
	                                                if ui
	                                                    .add_enabled(has_open_window, egui::Button::new("Open window"))
	                                                    .clicked()
	                                                {
	                                                    self.open_window_for_app_and_exit(app, ctx);
	                                                    ui.close();
	                                                }
	                                                if ui.button("Open new window").clicked() {
	                                                    self.launch_app_and_exit(app, ctx);
	                                                    ui.close();
	                                                }
	                                                if ui.button("Show info").clicked() {
                                                                    self.app_info_popup = Some(app.clone());
                                                                    ui.close();
                                                                }
		                                                    });
			                                                    if response.clicked() || response.middle_clicked() {
		                                                                self.active_pane = ActivePane::Apps;
		                                                                self.side_panel_selected_index = index;
			                                                        if response.clicked() {
			                                                            self.open_or_launch_app_and_exit(app, ctx);
			                                                        } else if response.middle_clicked() {
			                                                            self.launch_app_and_exit(app, ctx);
			                                                        }
			                                                    }
		                                                    ui.painter().rect_filled(
		                                                        rect,
		                                                        egui::CornerRadius::same(10),
			                                                        if is_selected {
			                                                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18)
			                                                        } else if response.hovered() {
			                                                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10)
			                                                        } else {
			                                                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 5)
			                                                        },
		                                                    );
                                                            if is_selected {
                                                                ui.painter().rect_stroke(
                                                                    rect,
                                                                    egui::CornerRadius::same(10),
                                                                    egui::Stroke::new(
                                                                        1.5,
                                                                        egui::Color32::from_rgb(61, 174, 233),
                                                                    ),
                                                                    egui::StrokeKind::Inside,
                                                                );
                                                            }
                                                            let inner_rect = rect.shrink2(egui::vec2(6.0, 6.0));
                                                            let label_height = if self.app_icon_show_name {
                                                                (self.app_icon_name_size + 10.0).max(16.0)
                                                            } else {
                                                                0.0
                                                            };
                                                            let icon_center_y = inner_rect.min.y
                                                                + (inner_rect.height() - label_height)
                                                                    / 2.0;
	                                                            let icon_rect = egui::Rect::from_center_size(
	                                                                egui::pos2(rect.center().x, icon_center_y),
	                                                                app_icon_size,
	                                                            );
                                                                    if let Some(level) = audio_level {
                                                                        paint_audio_activity_ring(
                                                                            ui.painter(),
                                                                            icon_rect,
                                                                            level,
                                                                            ctx.input(|i| i.time)
                                                                                as f32,
                                                                        );
                                                                    }
	                                                            let label_rect = egui::Rect::from_min_max(
                                                                egui::pos2(
                                                                    inner_rect.min.x,
                                                                    inner_rect.max.y - label_height,
                                                                ),
                                                                inner_rect.max,
                                                            );
                                                            paint_icon_in_rect(
                                                                ui,
                                                                app.icon_path.as_ref(),
                                                                icon_rect,
                                                                app_icon_size,
                                                            );
                                                            if self.pinned_apps.contains(&app.desktop_file_path) {
		                                                        ui.painter().text(
		                                                            egui::pos2(rect.max.x - 10.0, rect.min.y + 10.0),
	                                                            egui::Align2::CENTER_CENTER,
	                                                            "📌",
	                                                            egui::FontId::proportional(10.0),
		                                                            egui::Color32::WHITE,
		                                                        );
		                                                    }

                                                            if self.app_icon_show_name {
                                                                let label =
                                                                    truncate_tile_label(&app.name, tile_size);
                                                                paint_centered_title_job(
                                                                    ui,
                                                                    label_rect,
                                                                    &label,
                                                                    self.app_icon_name_size,
                                                                    filtered_app_name_highlight_segments
                                                                        .get(index)
                                                                        .map(Vec::as_slice)
                                                                        .unwrap_or(&[]),
                                                                    egui::Color32::from_rgba_unmultiplied(
                                                                        255, 255, 255, 210,
                                                                    ),
                                                                );
                                                            }
		                                                    let _ = index;
		                                                        }
	                                                    });
		                                                    self.rendered_side_panel_grid_columns =
		                                                        rendered_columns.max(1);
			                                                    self.rendered_side_panel_item_centers =
			                                                        rendered_item_centers;
	                                                } else {
	                                                    self.rendered_side_panel_item_centers.clear();
	                                                    self.rendered_side_panel_grid_columns = 1;
			                                                    for (index, item) in filtered_apps.iter().enumerate() {
	                                                        let app = &item.0;
                                                            let audio_level =
                                                                app_audio_level(
                                                                    app,
                                                                    &self.cached_sink_inputs,
                                                                    &self.active_media_app_keys,
                                                                    &self.observed_pipewire_node_ids,
                                                                    &self.active_pipewire_node_ids,
                                                                    self.pipewire_activity_cache_valid,
                                                                );
	                                                        let (rect, response) = ui.allocate_exact_size(
                                                            egui::vec2(ui.available_width(), app_row_height),
                                                            egui::Sense::click(),
                                                        );
	                                                response.clone().context_menu(|ui| {
	                                                    let path = app.desktop_file_path.clone();
	                                                    let is_pinned = self.pinned_apps.contains(&path);
	                                                    let label = if is_pinned { "📌 Unpin application" } else { "📌 Pin application" };
	                                                    if ui.button(label).clicked() {
	                                                        if is_pinned {
	                                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
	                                                                self.pinned_apps.remove(pos);
	                                                            }
	                                                        } else {
	                                                            self.pinned_apps.push(path.clone());
	                                                        }
	                                                        self.save_pinned_apps();
	                                                        ui.close();
	                                                    }
                                                        if is_pinned {
                                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                                if pos > 0 {
                                                                    if ui.button("⬆ Move up").clicked() {
                                                                        self.pinned_apps.swap(pos, pos - 1);
                                                                        self.save_pinned_apps();
                                                                        ui.close();
                                                                    }
                                                                }
                                                                if pos + 1 < self.pinned_apps.len() {
                                                                    if ui.button("⬇ Move down").clicked() {
                                                                        self.pinned_apps.swap(pos, pos + 1);
                                                                        self.save_pinned_apps();
                                                                        ui.close();
                                                                    }
                                                                }
                                                            }
                                                        }
	                                                    ui.separator();
	                                                    let has_open_window = self.windows.iter().any(|window| {
	                                                        self.desktop_file_path_for_window(window).as_ref()
	                                                            == Some(&app.desktop_file_path)
	                                                    });
	                                                    if ui
	                                                        .add_enabled(has_open_window, egui::Button::new("Open window"))
	                                                        .clicked()
	                                                    {
	                                                        self.open_window_for_app_and_exit(app, ctx);
	                                                        ui.close();
	                                                    }
	                                                    if ui.button("Open new window").clicked() {
	                                                        self.launch_app_and_exit(app, ctx);
	                                                        ui.close();
	                                                    }
	                                                    if ui.button("Show info").clicked() {
		                                                    self.app_info_popup = Some(app.clone());
		                                                    ui.close();
		                                                }
		                                                });
		                                                if response.clicked() || response.middle_clicked() {
		                                                    if response.clicked() {
		                                                        self.open_or_launch_app_and_exit(app, ctx);
		                                                    } else if response.middle_clicked() {
		                                                        self.launch_app_and_exit(app, ctx);
		                                                    }
		                                                }
                                                        if response.hovered() {
                                                            ui.painter().rect_filled(
                                                                rect,
		                                                        egui::CornerRadius::same(8),
                                                                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 12),
                                                            );
                                                        }
                                                        let content_rect = inset_rect(
                                                            rect,
                                                            self.app_left_padding,
                                                            self.app_right_padding,
                                                            self.app_top_padding,
                                                            self.app_bottom_padding,
                                                        );
                                                        let mut child_ui = ui.new_child(
                                                            egui::UiBuilder::new()
                                                                .max_rect(content_rect)
                                                                .layout(egui::Layout::left_to_right(egui::Align::Center)),
                                                        );
                                                            let (icon_rect, _) = child_ui.allocate_exact_size(
                                                                app_icon_size,
                                                                egui::Sense::hover(),
                                                            );
                                                            if let Some(level) = audio_level {
                                                                paint_audio_activity_ring(
                                                                    child_ui.painter(),
                                                                    icon_rect,
                                                                    level,
                                                                    ctx.input(|i| i.time) as f32,
                                                                );
                                                            }
                                                            paint_icon_in_rect(
                                                                &mut child_ui,
                                                                app.icon_path.as_ref(),
                                                                icon_rect,
                                                                app_icon_size,
                                                            );
                                                        child_ui.add_space(10.0);

	                                                        let display_title = filtered_app_display_titles
	                                                            .get(index)
	                                                            .map(String::as_str)
	                                                            .unwrap_or(&app.name);
		                                                        let show_search_metadata =
		                                                            !search_query.trim().is_empty();
		                                                        let mut label_clicked = false;
	                                                        if self.win_show_path {
                                                            let text_min_x =
                                                                content_rect.min.x + app_icon_size.x + 10.0;
                                                            let text_rect = egui::Rect::from_min_max(
                                                                egui::pos2(text_min_x, content_rect.min.y),
                                                                content_rect.max,
                                                            );
                                                            let mut text_ui = ui.new_child(
                                                                egui::UiBuilder::new()
                                                                    .max_rect(text_rect)
                                                                    .layout(egui::Layout::top_down(
                                                                        egui::Align::Min,
                                                                    )),
                                                            );
	                                                            text_ui.spacing_mut().item_spacing.y = 0.0;
	                                                            let text_block_height = if show_search_metadata {
	                                                                self.win_line_height
	                                                            } else {
	                                                                self.win_line_height
	                                                                    + self.win_line_height * 0.8
	                                                                    + self.win_text_spacing
	                                                            };
	                                                            text_ui.add_space(
	                                                                ((content_rect.height() - text_block_height) / 2.0)
	                                                                    .max(0.0),
	                                                            );

	                                                            let title_response = text_ui.add(
	                                                                egui::Label::new(
                                                                            highlighted_title_job_from_segments(
                                                                                &display_title,
                                                                                self.win_title_size,
                                                                                filtered_app_highlight_segments
                                                                                    .get(index)
                                                                                    .map(|segments| segments.as_slice())
                                                                                    .unwrap_or(&[]),
                                                                            ),
                                                                        )
	                                                                .sense(egui::Sense::click())
	                                                                .truncate(),
	                                                            );
                                                            if title_response.clicked() {
                                                                label_clicked = true;
                                                            }
                                                            if self.disable_ibeam && title_response.hovered() {
                                                                text_ui
                                                                    .ctx()
                                                                    .set_cursor_icon(egui::CursorIcon::Default);
                                                            }

                                                            if !show_search_metadata {
	                                                            text_ui.add_space(self.win_text_spacing);

	                                                            let is_link = std::fs::symlink_metadata(
	                                                                &app.desktop_file_path,
	                                                            )
	                                                            .map(|m| m.file_type().is_symlink())
	                                                            .unwrap_or(false);
	                                                            let mut subtext = app
	                                                                .desktop_file_path
	                                                                .to_string_lossy()
	                                                                .to_string();
	                                                            if is_link {
	                                                                subtext.push('@');
	                                                            }
	                                                            let path_response = text_ui.add(
	                                                                egui::Label::new(
	                                                                    egui::RichText::new(subtext)
	                                                                        .color(egui::Color32::from_rgba_unmultiplied(
	                                                                            255, 255, 255, 130,
	                                                                        ))
	                                                                        .size(self.win_path_size)
	                                                                        .line_height(Some(
	                                                                            self.win_line_height * 0.8,
	                                                                        )),
	                                                                )
	                                                                .sense(egui::Sense::click())
	                                                                .truncate(),
	                                                            );
	                                                            if path_response.clicked() {
	                                                                label_clicked = true;
	                                                            }
	                                                            if self.disable_ibeam && path_response.hovered() {
	                                                                text_ui
	                                                                    .ctx()
	                                                                    .set_cursor_icon(egui::CursorIcon::Default);
	                                                            }
                                                            }
                                                        } else {
	                                                            let title_response = child_ui.add(
	                                                                egui::Label::new(
                                                                            highlighted_title_job_from_segments(
                                                                                &display_title,
                                                                                self.win_title_size,
                                                                                filtered_app_highlight_segments
                                                                                    .get(index)
                                                                                    .map(|segments| segments.as_slice())
                                                                                    .unwrap_or(&[]),
                                                                            ),
                                                                        )
	                                                                .sense(egui::Sense::click())
	                                                                .truncate(),
	                                                            );
                                                            if title_response.clicked() {
                                                                label_clicked = true;
                                                            }
                                                            if self.disable_ibeam && title_response.hovered() {
                                                                child_ui
                                                                    .ctx()
                                                                    .set_cursor_icon(egui::CursorIcon::Default);
                                                            }
                                                        }
                                                        if label_clicked {
                                                            self.open_or_launch_app_and_exit(app, ctx);
                                                        }
                                                    }
                                                }
                                            });
	                            });
	                        });
	                        ui.spacing_mut().item_spacing = previous_spacing;
	                    }

                    // Custom drag-resize handle at the bottom-right corner
                    let resize_handle_size = egui::vec2(16.0, 16.0);
                    let resize_rect = egui::Rect::from_min_size(
                        ui.max_rect().max - resize_handle_size,
                        resize_handle_size,
                    );

                    let resize_response = ui.allocate_rect(resize_rect, egui::Sense::drag());

                    // Draw visual resize handle (diagonal grip lines)
                    let color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 100);
                    let br = resize_rect.max - egui::vec2(2.0, 2.0);
                    for i in 0..3 {
                        let offset = i as f32 * 4.0;
                        ui.painter().line_segment(
                            [
                                br - egui::vec2(offset + 4.0, 0.0),
                                br - egui::vec2(0.0, offset + 4.0),
                            ],
                            egui::Stroke::new(1.0, color),
                        );
                    }

                    if resize_response.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeSouthEast);
                    }

                    if resize_response.drag_started() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::BeginResize(
                            egui::ResizeDirection::SouthEast,
                        ));
                    }

                    if self.show_settings_menu {
                        self.show_settings_popup(ctx);
                    }
                    if self.show_history_popup {
                        self.show_history_native_viewport(ctx);
                    }
                    if self.process_chain_popup.is_some() {
                        self.show_window_info_popup(ctx);
                    }
                    if self.app_info_popup.is_some() {
                        self.show_app_info_popup(ctx);
                    }
                    self.show_terminal_action_message(ctx);

                    if let Some(ref resp) = text_edit_response {
                        if ctx.input(|i| i.focused) {
                            resp.request_focus();
                        }
                    }

                    if self.mode == LauncherMode::Windows && !filtered_windows.is_empty() {
                        self.last_selected_window_id = filtered_windows.get(self.selected_index).map(|w| w.id.clone());
                    } else {
                        self.last_selected_window_id = None;
                    }
                });
            });

        if self.recovery_prompt {
            egui::Window::new("Restore previous session?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label("The previous system session ended unexpectedly. A recovery snapshot is available.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Restore missing windows").clicked() {
                            self.recovery_prompt = false;
                            std::thread::spawn(|| {
                                match applicationlauncher::tracker::TrackerClient::connect().and_then(|client| client.restore_recovery()) {
                                    Ok(report) => eprintln!("Recovery restore: {} matched, {} launched, {} failures", report.matched, report.launched, report.failures.len()),
                                    Err(err) => eprintln!("Recovery restore failed: {err}"),
                                }
                            });
                        }
                        if ui.button("Not now").clicked() {
                            self.recovery_prompt = false;
                            std::thread::spawn(|| {
                                if let Ok(client) = applicationlauncher::tracker::TrackerClient::connect() { let _ = client.dismiss_recovery(); }
                            });
                        }
                    });
                });
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_window_size();
    }
}
