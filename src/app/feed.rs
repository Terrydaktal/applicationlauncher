use super::*;

impl App {
    pub(super) fn start_terminal_metadata_refresh(&mut self) {
        if self.terminal_records_receiver.is_some() {
            self.terminal_metadata_refresh_queued = true;
            return;
        }

        let repaint_ctx = self.repaint_ctx.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.terminal_records_receiver = Some(rx);
        self.terminal_metadata_refresh_queued = false;
        std::thread::spawn(move || {
            let _ = tx.send(fetch_terminal_dbus_records());
            repaint_ctx.request_repaint();
        });
    }

    pub(super) fn apply_terminal_metadata_records(&mut self, records: Vec<TerminalDbusRecord>) {
        self.terminal_records = records;
        if self.terminal_records.is_empty() {
            return;
        }

        let terminal_windows = self
            .windows
            .iter()
            .filter(|window| is_terminal_class(&window.class.to_lowercase()))
            .cloned()
            .collect::<Vec<_>>();
        if terminal_windows.is_empty() {
            return;
        }

        let theme = self
            .force_theme
            .as_deref()
            .unwrap_or("breeze-dark")
            .to_string();
        let records = self.terminal_records.clone();
        let (ppid_to_children, pid_to_name, pid_to_ppid) = get_process_tree();
        let mut rebuilt = Vec::new();
        for old_window in terminal_windows {
            let demands_attention = old_window.demands_attention;
            if let Some(mut window) = build_window_info(
                old_window.id,
                old_window.raw_title,
                old_window.class,
                old_window.desktop_file_name,
                old_window.pid,
                old_window.geometry,
                old_window.minimized,
                &theme,
                &mut self.window_icon_cache,
                &ppid_to_children,
                &pid_to_name,
                &pid_to_ppid,
                &records,
            ) {
                window.demands_attention = demands_attention;
                rebuilt.push(window);
            }
        }
        self.apply_window_reconciliation(rebuilt);
    }
    pub(super) fn update_cached_windows_without_rerank(
        &mut self,
        updates: &[(WindowInfo, WindowInfo)],
    ) {
        let Some(cache) = self.filtered_search_cache.as_mut() else {
            return;
        };
        let cached_windows = Arc::make_mut(&mut cache.results.windows);
        let display_titles = Arc::make_mut(&mut cache.results.window_display_titles);

        for (old_window, new_window) in updates {
            let Some(index) = cached_windows
                .iter()
                .position(|window| window.id == new_window.id)
            else {
                continue;
            };
            if let Some(display_title) = display_titles.get_mut(index) {
                refresh_cached_transient_title(display_title, &old_window.title, &new_window.title);
            }
            cached_windows[index] = new_window.clone();
        }
    }

    pub(super) fn seed_window_icon_cache(&mut self) {
        for window in &self.windows {
            let icon_key = window_icon_cache_key(
                &window.class,
                window.desktop_file_name.as_deref(),
                window.active_process.as_deref(),
                window.exe_path.as_deref(),
            );
            self.window_icon_cache
                .entry(icon_key)
                .or_insert_with(|| window.icon_path.clone());
        }
    }

    pub(super) fn schedule_window_search_refresh(&mut self) {
        if self.search_query.trim().is_empty() {
            self.pending_window_search_refresh_at = None;
            self.windows_generation = self.windows_generation.wrapping_add(1);
            return;
        }

        self.pending_window_search_refresh_at = Some(window_search_refresh_deadline(
            self.pending_window_search_refresh_at,
            Instant::now(),
        ));
    }

    pub(super) fn flush_pending_window_search_refresh(&mut self) -> bool {
        if self.pending_window_search_refresh_at.take().is_none() {
            return false;
        }

        self.windows_generation = self.windows_generation.wrapping_add(1);
        true
    }

    pub(super) fn apply_window_snapshot(&mut self, new_windows: Vec<WindowInfo>) {
        if self.windows.is_empty() {
            self.windows = new_windows;
            self.seed_window_icon_cache();
            self.missing_window_counts.clear();
            self.windows_generation = self.windows_generation.wrapping_add(1);
            self.refresh_window_audio_cache();
            return;
        }

        let old_windows = self.windows.clone();
        let mut new_by_id: HashMap<String, WindowInfo> = new_windows
            .iter()
            .cloned()
            .map(|window| (window.id.clone(), window))
            .collect();
        let old_ids: HashSet<String> = self
            .windows
            .iter()
            .map(|window| window.id.clone())
            .collect();
        let mut merged = Vec::new();

        for old_window in &old_windows {
            if let Some(new_window) = new_by_id.remove(&old_window.id) {
                self.missing_window_counts.remove(&old_window.id);
                merged.push(merge_reconciled_window(old_window, new_window));
                continue;
            }

            let missing_count = self
                .missing_window_counts
                .entry(old_window.id.clone())
                .or_insert(0);
            *missing_count += 1;

            if *missing_count < WINDOW_REMOVAL_CONFIRMATION_POLLS {
                merged.push(old_window.clone());
            }
        }

        for new_window in new_windows {
            if !old_ids.contains(&new_window.id) {
                self.missing_window_counts.remove(&new_window.id);
                merged.push(new_window);
            }
        }

        let retained_ids: HashSet<String> = merged.iter().map(|window| window.id.clone()).collect();
        self.missing_window_counts
            .retain(|window_id, _| retained_ids.contains(window_id));

        let old_by_id: HashMap<&str, &WindowInfo> = old_windows
            .iter()
            .map(|window| (window.id.as_str(), window))
            .collect();
        let search_changed = old_windows.len() != merged.len()
            || merged.iter().any(|window| {
                old_by_id
                    .get(window.id.as_str())
                    .is_none_or(|old| !window_search_metadata_equal(old, window))
            });
        let cache_updates = merged
            .iter()
            .filter_map(|window| {
                let old = old_by_id.get(window.id.as_str())?;
                window_search_metadata_equal(old, window).then(|| ((*old).clone(), window.clone()))
            })
            .collect::<Vec<_>>();
        self.windows = merged;
        self.seed_window_icon_cache();
        self.update_cached_windows_without_rerank(&cache_updates);
        if search_changed {
            self.schedule_window_search_refresh();
        }
    }

    pub(super) fn apply_window_feed_events(&mut self, events: Vec<WindowFeedEvent>) {
        if events.is_empty() {
            return;
        }

        let events = coalesce_window_feed_events(events);
        let theme = self
            .force_theme
            .as_deref()
            .unwrap_or("breeze-dark")
            .to_string();
        let (ppid_to_children, pid_to_name, pid_to_ppid) = get_process_tree();
        let terminal_records = self.terminal_records.clone();
        let mut changed = false;
        let mut search_changed = false;
        let mut cache_updates = Vec::new();
        let mut needs_terminal_metadata_refresh = false;

        for event in events {
            match event {
                WindowFeedEvent::Reset => {
                    changed |= !self.windows.is_empty();
                    search_changed |= !self.windows.is_empty();
                    self.windows.clear();
                    self.missing_window_counts.clear();
                    self.last_selected_window_id = None;
                    cache_updates.clear();
                }
                WindowFeedEvent::Snapshot(_) => {
                    unreachable!("snapshot events are expanded before application");
                }
                WindowFeedEvent::Upsert(payload) => {
                    let window_id = payload.id.clone();
                    let payload_class = if payload.class.trim().is_empty() {
                        payload.desktop_file_name.as_str()
                    } else {
                        payload.class.as_str()
                    };
                    let terminal_payload = is_terminal_class(&payload_class.to_lowercase())
                        || self.windows.iter().any(|window| {
                            window.id == window_id
                                && is_terminal_class(&window.class.to_lowercase())
                        });
                    needs_terminal_metadata_refresh |= terminal_payload
                        && terminal_record_for_window_title(&payload.title, &terminal_records)
                            .is_none();
                    if let Some(window) = window_info_from_kwin_payload(
                        payload,
                        &theme,
                        &mut self.window_icon_cache,
                        &ppid_to_children,
                        &pid_to_name,
                        &pid_to_ppid,
                        &terminal_records,
                    ) {
                        self.missing_window_counts.remove(&window.id);
                        if let Some(existing) =
                            self.windows.iter_mut().find(|item| item.id == window.id)
                        {
                            let old_window = existing.clone();
                            let window_search_changed =
                                !window_search_metadata_equal(&old_window, &window);
                            search_changed |= window_search_changed;
                            *existing = window.clone();
                            if !window_search_changed {
                                cache_updates.push((old_window, window));
                            }
                        } else {
                            self.windows.push(window);
                            search_changed = true;
                        }
                        changed = true;
                    } else {
                        self.missing_window_counts.remove(&window_id);
                        let previous_len = self.windows.len();
                        self.windows.retain(|window| window.id != window_id);
                        let removed = self.windows.len() != previous_len;
                        changed |= removed;
                        search_changed |= removed;
                    }
                }
                WindowFeedEvent::Remove(id) => {
                    self.missing_window_counts.remove(&id);
                    let previous_len = self.windows.len();
                    self.windows.retain(|window| window.id != id);
                    let removed = self.windows.len() != previous_len;
                    changed |= removed;
                    search_changed |= removed;
                }
                WindowFeedEvent::RearmAttentionAutomation => {}
            }
        }

        if changed {
            self.update_cached_windows_without_rerank(&cache_updates);
            if search_changed {
                self.schedule_window_search_refresh();
            }
            self.refresh_window_audio_cache();
        }
        if needs_terminal_metadata_refresh {
            self.start_terminal_metadata_refresh();
        }
    }

    pub(super) fn prune_stale_windows(&mut self) {
        let now = Instant::now();
        if self
            .last_stale_prune
            .is_some_and(|last| now.duration_since(last) < std::time::Duration::from_secs(1))
        {
            return;
        }
        self.last_stale_prune = Some(now);

        let stale_ids: HashSet<String> = self
            .windows
            .iter()
            .filter(|window| window.pid.is_some_and(|pid| !process_exists(pid)))
            .map(|window| window.id.clone())
            .collect();

        if stale_ids.is_empty() {
            return;
        }

        self.windows
            .retain(|window| !stale_ids.contains(&window.id));
        self.missing_window_counts
            .retain(|window_id, _| !stale_ids.contains(window_id));
        self.schedule_window_search_refresh();
        self.refresh_window_audio_cache();

        if self
            .last_selected_window_id
            .as_ref()
            .is_some_and(|window_id| stale_ids.contains(window_id))
        {
            self.last_selected_window_id = None;
        }
    }

    pub(super) fn refresh_window_audio_cache(&mut self) -> bool {
        let previous_level_buckets = self.window_audio_cache.level_buckets.clone();
        let previous_sink_signature = sink_match_signature(&self.window_audio_cache);
        let mut new_cache = WindowAudioCache::default();
        for window in &self.windows {
            let sink_matches = find_sink_inputs_for_window(window, &self.cached_sink_inputs);
            if !sink_matches.is_empty() {
                new_cache.sink_matches.insert(
                    window.id.clone(),
                    dedup_sink_inputs_for_controls(&sink_matches),
                );
            }

            if let Some(level) = active_audio_level_for_sinks(
                &sink_matches,
                &self.active_media_app_keys,
                &self.observed_pipewire_node_ids,
                &self.active_pipewire_node_ids,
                self.pipewire_activity_cache_valid,
            ) {
                new_cache
                    .level_buckets
                    .insert(window.id.clone(), quantize_audio_level(level));
            }
        }

        let changed = previous_level_buckets != new_cache.level_buckets
            || previous_sink_signature != sink_match_signature(&new_cache);
        self.window_audio_cache = new_cache;
        changed
    }

    pub(super) fn has_any_active_audio(&self) -> bool {
        self.cached_sink_inputs.iter().any(|sink| {
            sink_input_level(
                sink,
                &self.active_media_app_keys,
                &self.observed_pipewire_node_ids,
                &self.active_pipewire_node_ids,
                self.pipewire_activity_cache_valid,
            ) > 0.0
        })
    }

    pub(super) fn refresh_windows(&mut self) {
        if let Some(ref kpath) = self.kdotool_path {
            let kpath = kpath.clone();
            let theme = self
                .force_theme
                .as_deref()
                .unwrap_or("breeze-dark")
                .to_string();
            let (tx, rx) = std::sync::mpsc::channel();
            self.loading = true;
            self.receiver = Some(rx);

            std::thread::spawn(
                move || match Command::new(&kpath).arg("--version").output() {
                    Ok(_) => {
                        let windows = get_open_windows_fast(&kpath, &theme).unwrap_or_default();
                        let _ = tx.send(LoadResult::WindowsSuccess(windows));
                    }
                    Err(_) => {
                        let _ = tx.send(LoadResult::Error(format!(
                            "kdotool utility not found.\n\nPlease install it using cargo:\n\ncargo install kdotool"
                        )));
                    }
                },
            );
        }
    }

    pub(super) fn start_background_window_enrichment(&mut self) {
        let Some(kpath) = self.kdotool_path.clone() else {
            self.background_window_enrichment_receiver = None;
            return;
        };
        let theme = self
            .force_theme
            .as_deref()
            .unwrap_or("breeze-dark")
            .to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        self.background_window_enrichment_receiver = Some(rx);

        std::thread::spawn(move || {
            let windows = get_open_windows(&kpath, &theme).unwrap_or_default();
            let _ = tx.send(windows);
        });
    }

    pub(super) fn schedule_window_reconciliation(&mut self, delay: Duration) {
        self.next_window_reconciliation_at = Some(Instant::now() + delay);
    }

    pub(super) fn start_window_reconciliation(&mut self) {
        if self.background_window_reconciliation_receiver.is_some() {
            return;
        }
        let Some(kpath) = self.kdotool_path.clone() else {
            self.next_window_reconciliation_at = None;
            return;
        };
        let theme = self
            .force_theme
            .as_deref()
            .unwrap_or("breeze-dark")
            .to_string();
        let repaint_ctx = self.repaint_ctx.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.background_window_reconciliation_receiver = Some(rx);
        self.next_window_reconciliation_at = None;

        std::thread::spawn(move || {
            let windows =
                get_open_windows_fast(&kpath, &theme).filter(|windows| !windows.is_empty());
            let _ = tx.send(windows);
            repaint_ctx.request_repaint();
        });
    }

    pub(super) fn apply_window_reconciliation(&mut self, discovered: Vec<WindowInfo>) {
        for window in &discovered {
            self.missing_window_counts.remove(&window.id);
        }
        let (changed, search_changed, cache_updates) =
            merge_reconciled_windows(&mut self.windows, discovered);
        if !changed {
            return;
        }

        self.seed_window_icon_cache();
        self.update_cached_windows_without_rerank(&cache_updates);
        if search_changed {
            self.schedule_window_search_refresh();
        }
        self.refresh_window_audio_cache();
    }

    pub(super) fn start_background_app_load(&mut self) {
        let theme = self
            .force_theme
            .as_deref()
            .unwrap_or("breeze-dark")
            .to_string();
        let repaint_ctx = self.repaint_ctx.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.background_apps_receiver = Some(rx);

        std::thread::spawn(move || {
            let apps = get_installed_apps(&theme);
            let _ = tx.send(apps);
            repaint_ctx.request_repaint();
        });
    }

    pub(super) fn refresh_apps(&mut self) {
        let theme = self
            .force_theme
            .as_deref()
            .unwrap_or("breeze-dark")
            .to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        self.loading = true;
        self.receiver = Some(rx);

        std::thread::spawn(move || {
            let apps = get_installed_apps(&theme);
            let _ = tx.send(LoadResult::AppsSuccess(apps));
        });
    }

    pub(super) fn start_window_polling_thread(&mut self, ctx: &egui::Context) {
        if self.window_polling_started {
            return;
        }
        self.window_polling_started = true;

        let Some(kpath) = self.kdotool_path.clone() else {
            return;
        };
        let theme = self
            .force_theme
            .as_deref()
            .unwrap_or("breeze-dark")
            .to_string();
        let rapid_polling_thread = std::sync::Arc::clone(&self.rapid_polling);
        let window_tx = self.window_sender.clone();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let mut rapid_poll_count = 0;
            loop {
                if rapid_polling_thread.load(std::sync::atomic::Ordering::SeqCst) {
                    rapid_polling_thread.store(false, std::sync::atomic::Ordering::SeqCst);
                    rapid_poll_count = 15; // 15 * 300ms = 4.5 seconds of rapid polling
                }

                let sleep_dur = if rapid_poll_count > 0 {
                    rapid_poll_count -= 1;
                    std::time::Duration::from_millis(300)
                } else {
                    std::time::Duration::from_millis(1000)
                };

                std::thread::sleep(sleep_dur);

                if let Some(windows) = get_open_windows(&kpath, &theme) {
                    if window_tx.send(windows).is_ok() {
                        ctx.request_repaint();
                    } else {
                        break;
                    }
                }
            }
        });
    }
}
