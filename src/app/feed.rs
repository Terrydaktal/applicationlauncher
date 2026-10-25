use super::*;
use crate::search::normalize_window_identity_title;

const PROCESS_TREE_CACHE_MAX_AGE: Duration = Duration::from_millis(500);

fn enrichment_reusable(checked_at: Option<Instant>, now: Instant) -> bool {
    checked_at.is_some_and(|checked_at| {
        now.saturating_duration_since(checked_at) < PROCESS_TREE_CACHE_MAX_AGE
    })
}

fn transient_payload_update(window: &WindowInfo, payload: &KWinWindowPayload) -> bool {
    !payload.skip_taskbar
        && !payload.skip_switcher
        && window.pid == (payload.pid > 0).then_some(payload.pid)
        && &window.class
            == if payload.class.trim().is_empty() {
                &payload.desktop_file_name
            } else {
                &payload.class
            }
        && window.desktop_file_name.as_deref().unwrap_or("") == payload.desktop_file_name.trim()
        && window.raw_title != payload.title
        && (!is_terminal_class(&window.class.to_lowercase())
            || window
                .active_process
                .as_deref()
                .is_some_and(|program| !crate::windows::is_shell(program)))
        && window.raw_title.chars().any(is_braille_spinner_char)
            == payload.title.chars().any(is_braille_spinner_char)
        && attention_required_frame(&window.raw_title).is_some()
            == attention_required_frame(&payload.title).is_some()
        && normalize_window_identity_title(&window.raw_title)
            == normalize_window_identity_title(&payload.title)
}

fn audio_metadata_equal(old: &WindowInfo, new: &WindowInfo, sinks: &[PactlSinkInput]) -> bool {
    old.pid == new.pid
        && old.class == new.class
        && old.active_process == new.active_process
        && old.process_chain == new.process_chain
        && (old.raw_title == new.raw_title
            || (normalize_window_identity_title(&old.raw_title)
                == normalize_window_identity_title(&new.raw_title)
                && !sinks.iter().any(|sink| {
                    sink.properties
                        .get("window.name")
                        .is_some_and(|name| name == &old.raw_title || name == &new.raw_title)
                })))
}

#[cfg(test)]
mod optimization_tests {
    use super::*;
    #[test]
    fn title_fast_path_is_limited_to_same_identity_and_same_transient_state() {
        let payload: KWinWindowPayload = serde_json::from_str(r#"{"id":"test","class":"xfce4-terminal","pid":-1,"title":"codex - \u280b project - Terminal"}"#).unwrap();
        let mut cache = HashMap::new();
        let mut window = window_info_from_kwin_payload(
            payload.clone(),
            "breeze-dark",
            &mut cache,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &[],
        )
        .unwrap();
        window.active_process = Some("codex".into());
        let mut changed = payload.clone();
        changed.title = "codex - \u{2819} project - Terminal".into();
        assert!(transient_payload_update(&window, &changed));
        assert!(audio_metadata_equal(
            &window,
            &WindowInfo {
                raw_title: changed.title.clone(),
                ..window.clone()
            },
            &[]
        ));
        changed.pid = 42;
        assert!(!transient_payload_update(&window, &changed));
        changed = payload.clone();
        changed.title = "codex - \u{2819} other - Terminal".into();
        assert!(!transient_payload_update(&window, &changed));
        changed = payload;
        changed.title = "codex - project - Terminal".into();
        assert!(!transient_payload_update(&window, &changed));
    }

    #[test]
    fn transient_updates_do_not_extend_process_enrichment_deadline() {
        let checked_at = Instant::now();
        assert!(!enrichment_reusable(None, checked_at));
        assert!(enrichment_reusable(
            Some(checked_at),
            checked_at + Duration::from_millis(499)
        ));
        assert!(!enrichment_reusable(
            Some(checked_at),
            checked_at + Duration::from_millis(500)
        ));
        assert!(!enrichment_reusable(
            Some(checked_at),
            checked_at + Duration::from_secs(5)
        ));
    }
}

pub(super) fn terminal_metadata_refresh_due(
    receiver_active: bool,
    refresh_queued: bool,
    retry_not_before: Option<Instant>,
    now: Instant,
) -> bool {
    !receiver_active && refresh_queued && retry_not_before.is_some_and(|deadline| now >= deadline)
}

pub(super) fn process_tree_cache_refresh_due(
    cache_updated_at: Option<Instant>,
    refresh_active: bool,
    now: Instant,
) -> bool {
    !refresh_active
        && cache_updated_at.is_none_or(|updated_at| {
            now.saturating_duration_since(updated_at) >= PROCESS_TREE_CACHE_MAX_AGE
        })
}

impl App {
    pub(super) fn rebuild_window_search_documents(&mut self) {
        crate::search::update_window_search_documents(
            &self.windows,
            &mut self.window_search_documents,
            &mut self.dirty_window_search_documents,
        );
    }

    pub(super) fn start_process_tree_cache_refresh(&mut self) {
        if !process_tree_cache_refresh_due(
            self.process_tree_cache_updated_at,
            self.process_tree_cache_receiver.is_some(),
            Instant::now(),
        ) {
            return;
        }

        let repaint_ctx = self.repaint_ctx.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.process_tree_cache_receiver = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(get_process_tree());
            repaint_ctx.request_repaint();
        });
    }

    pub(super) fn start_terminal_metadata_refresh(&mut self) {
        if self.terminal_records_receiver.is_some() {
            self.terminal_metadata_refresh_queued = true;
            return;
        }
        if let Some(deadline) = self.terminal_metadata_retry_not_before {
            let now = Instant::now();
            if now < deadline {
                self.terminal_metadata_refresh_queued = true;
                self.repaint_ctx
                    .request_repaint_after(deadline.saturating_duration_since(now));
                return;
            }
            self.terminal_metadata_retry_not_before = None;
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
        self.terminal_metadata_apply_queued = false;
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
        self.start_process_tree_cache_refresh();
        let Some((ppid_to_children, pid_to_name, pid_to_ppid)) = self.process_tree_cache.as_ref()
        else {
            self.terminal_metadata_apply_queued = true;
            return;
        };
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

    pub(super) fn refresh_app_audio_routing(&mut self) {
        self.app_audio_sinks.clear();
        for app in &self.apps {
            let indices = app_audio_sink_indices(app, &self.cached_sink_inputs);
            if !indices.is_empty() {
                self.app_audio_sinks
                    .insert(app.desktop_file_path.clone(), indices);
            }
        }
    }
    pub(super) fn update_cached_windows_without_rerank(
        &mut self,
        updates: &[(WindowInfo, WindowInfo)],
    ) {
        self.default_windows_cache.update(updates);
        let Some(cache) = self.filtered_search_cache.as_mut() else {
            return;
        };
        let cached_windows = Arc::make_mut(&mut cache.results.windows);
        let display_titles = Arc::make_mut(&mut cache.results.window_display_titles);
        let mut activation_changed = false;

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
            activation_changed |= old_window.last_activated_at_ms
                != new_window.last_activated_at_ms
                || old_window.activation_sequence != new_window.activation_sequence;
            cached_windows[index] = new_window.clone();
        }
        if cache.key.order_windows_by_last_activation
            && activation_changed
            && !helpers::sort_cached_windows_by_last_activation(&mut cache.results)
        {
            self.filtered_search_cache = None;
        }
    }

    pub(super) fn seed_window_icon_cache(&mut self) {
        let active_keys = self
            .windows
            .iter()
            .map(|window| {
                window_icon_cache_key(
                    &window.class,
                    window.desktop_file_name.as_deref(),
                    window.active_process.as_deref(),
                    window.exe_path.as_deref(),
                )
            })
            .collect::<HashSet<_>>();
        self.window_icon_cache
            .retain(|key, _| active_keys.contains(key));
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
        self.default_windows_cache.invalidate();
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

        self.rebuild_window_search_documents();

        self.windows_generation = self.windows_generation.wrapping_add(1);
        true
    }

    pub(super) fn apply_window_snapshot(&mut self, mut new_windows: Vec<WindowInfo>) {
        self.overlay_tmux_metadata(&mut new_windows);
        if self.windows.is_empty() {
            self.windows = new_windows;
            self.default_windows_cache.invalidate();
            self.dirty_window_search_documents
                .extend(self.windows.iter().map(|window| window.id.clone()));
            if !self.search_query.trim().is_empty() {
                self.rebuild_window_search_documents();
            }
            self.seed_window_icon_cache();
            applicationlauncher::observability::set_gauge(
                applicationlauncher::observability::Gauge::GuiWindows,
                self.windows.len(),
            );
            self.missing_window_counts.clear();
            self.windows_generation = self.windows_generation.wrapping_add(1);
            self.refresh_window_audio_cache();
            self.tmux_monitor.sync_windows(&self.windows);
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
        self.window_enrichment_checked_at
            .retain(|window_id, _| retained_ids.contains(window_id));
        self.dirty_window_search_documents
            .retain(|window_id| retained_ids.contains(window_id));

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
        let audio_changed = old_windows.len() != merged.len()
            || merged.iter().any(|window| {
                old_by_id
                    .get(window.id.as_str())
                    .is_none_or(|old| !audio_metadata_equal(old, window, &self.cached_sink_inputs))
            });
        self.dirty_window_search_documents.extend(
            merged
                .iter()
                .filter(|window| {
                    old_by_id
                        .get(window.id.as_str())
                        .is_none_or(|old| !window_search_metadata_equal(old, window))
                })
                .map(|window| window.id.clone()),
        );
        let cache_updates = merged
            .iter()
            .filter_map(|window| {
                let old = old_by_id.get(window.id.as_str())?;
                window_search_metadata_equal(old, window).then(|| ((*old).clone(), window.clone()))
            })
            .collect::<Vec<_>>();
        self.windows = merged;
        self.tmux_monitor.sync_windows(&self.windows);
        applicationlauncher::observability::set_gauge(
            applicationlauncher::observability::Gauge::GuiWindows,
            self.windows.len(),
        );
        self.seed_window_icon_cache();
        self.update_cached_windows_without_rerank(&cache_updates);
        if search_changed {
            self.schedule_window_search_refresh();
        }
        if audio_changed {
            self.refresh_window_audio_cache();
        }
    }

    pub(super) fn apply_window_feed_events(&mut self, events: Vec<WindowFeedEvent>) {
        if events.is_empty() {
            return;
        }

        let now = Instant::now();
        let needs_enrichment = events.iter().any(|event| match event {
            WindowFeedEvent::Upsert(payload) => !self.windows.iter().any(|window| {
                window.id == payload.id
                    && transient_payload_update(window, payload)
                    && enrichment_reusable(
                        self.window_enrichment_checked_at.get(&window.id).copied(),
                        now,
                    )
            }),
            WindowFeedEvent::Reset | WindowFeedEvent::Snapshot(_) => true,
            WindowFeedEvent::Remove(_) => false,
        });
        if needs_enrichment {
            self.start_process_tree_cache_refresh();
        }
        if self.process_tree_cache.is_none() {
            self.deferred_window_feed_events.extend(events);
            self.deferred_window_feed_events =
                coalesce_window_feed_events(std::mem::take(&mut self.deferred_window_feed_events));
            applicationlauncher::observability::set_gauge(
                applicationlauncher::observability::Gauge::PendingFeedEvents,
                self.deferred_window_feed_events.len(),
            );
            return;
        }

        let mut events = events;
        if !self.deferred_window_feed_events.is_empty() {
            let mut deferred = std::mem::take(&mut self.deferred_window_feed_events);
            deferred.extend(events);
            events = deferred;
        }
        applicationlauncher::observability::set_gauge(
            applicationlauncher::observability::Gauge::PendingFeedEvents,
            0,
        );
        let events = coalesce_window_feed_events(events);
        let mut events = events;
        if applicationlauncher::replay::environment_faults_enabled() {
            let fault_operation = applicationlauncher::observability::next_operation_id();
            if applicationlauncher::replay::environment_should_inject(
                applicationlauncher::replay::FaultPoint::WindowFeedDrop,
                fault_operation,
            ) {
                applicationlauncher::observability::record_boundary(
                    applicationlauncher::replay::BoundaryKind::FaultInjection,
                    "window-feed-drop",
                    &serde_json::json!({
                        "operation_id": fault_operation,
                        "event_count": events.len(),
                    })
                    .to_string(),
                );
                return;
            }
            if applicationlauncher::replay::environment_should_inject(
                applicationlauncher::replay::FaultPoint::WindowFeedDuplicate,
                fault_operation,
            ) {
                applicationlauncher::observability::record_boundary(
                    applicationlauncher::replay::BoundaryKind::FaultInjection,
                    "window-feed-duplicate",
                    &serde_json::json!({
                        "operation_id": fault_operation,
                        "event_count": events.len(),
                    })
                    .to_string(),
                );
                events.extend(events.clone());
            }
        }
        let theme = self
            .force_theme
            .as_deref()
            .unwrap_or("breeze-dark")
            .to_string();
        let (ppid_to_children, pid_to_name, pid_to_ppid) = self
            .process_tree_cache
            .as_ref()
            .expect("process tree cache initialized");
        let terminal_records = self.terminal_records.clone();
        let mut changed = false;
        let mut audio_changed = false;
        let mut search_changed = false;
        let mut cache_updates = Vec::new();
        let mut needs_terminal_metadata_refresh = false;

        for event in events {
            match event {
                WindowFeedEvent::Reset => {
                    audio_changed |= !self.windows.is_empty();
                    changed |= !self.windows.is_empty();
                    search_changed |= !self.windows.is_empty();
                    self.windows.clear();
                    self.window_enrichment_checked_at.clear();
                    self.dirty_window_search_documents.clear();
                    self.missing_window_counts.clear();
                    self.last_selected_window_id = None;
                    cache_updates.clear();
                }
                WindowFeedEvent::Snapshot(_) => {
                    unreachable!("snapshot events are expanded before application");
                }
                WindowFeedEvent::Upsert(payload) => {
                    let window_id = payload.id.clone();
                    if let Some(existing) = self
                        .windows
                        .iter_mut()
                        .find(|window| window.id == window_id)
                        && transient_payload_update(existing, &payload)
                        && enrichment_reusable(
                            self.window_enrichment_checked_at.get(&window_id).copied(),
                            now,
                        )
                    {
                        let old = existing.clone();
                        refresh_cached_transient_title(
                            &mut existing.title,
                            &existing.raw_title,
                            &payload.title,
                        );
                        existing.raw_title = payload.title;
                        existing.demands_attention = payload.demands_attention;
                        existing.geometry = (payload.width > 0 && payload.height > 0).then_some((
                            payload.x,
                            payload.y,
                            payload.width,
                            payload.height,
                        ));
                        existing.minimized = Some(payload.minimized);
                        existing.last_activated_at_ms = payload.last_activated_at_ms;
                        existing.activation_sequence = payload.activation_sequence;
                        audio_changed |=
                            !audio_metadata_equal(&old, existing, &self.cached_sink_inputs);
                        cache_updates.push((old, existing.clone()));
                        self.missing_window_counts.remove(&window_id);
                        changed = true;
                        continue;
                    }
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
                    if let Some(mut window) = window_info_from_kwin_payload(
                        payload,
                        &theme,
                        &mut self.window_icon_cache,
                        &ppid_to_children,
                        &pid_to_name,
                        &pid_to_ppid,
                        &terminal_records,
                    ) {
                        // Reused frames must not postpone the next process refresh.
                        self.window_enrichment_checked_at.insert(
                            window.id.clone(),
                            self.process_tree_cache_updated_at.unwrap_or(now),
                        );
                        if let Some(metadata) = self.tmux_metadata.get(&window.id) {
                            metadata.apply(&mut window);
                        }
                        self.missing_window_counts.remove(&window.id);
                        if let Some(existing) =
                            self.windows.iter_mut().find(|item| item.id == window.id)
                        {
                            let old_window = existing.clone();
                            let window_search_changed =
                                !window_search_metadata_equal(&old_window, &window);
                            search_changed |= window_search_changed;
                            if window_search_changed {
                                self.dirty_window_search_documents.insert(window.id.clone());
                            }
                            audio_changed |= !audio_metadata_equal(
                                &old_window,
                                &window,
                                &self.cached_sink_inputs,
                            );
                            *existing = window.clone();
                            if !window_search_changed {
                                cache_updates.push((old_window, window));
                            }
                        } else {
                            self.dirty_window_search_documents.insert(window.id.clone());
                            audio_changed = true;
                            self.windows.push(window);
                            search_changed = true;
                        }
                        changed = true;
                    } else {
                        // KWin owns window lifetime. A missing PID or a transient
                        // enrichment failure is not evidence that the surface closed.
                        if self.windows.iter().any(|window| window.id == window_id) {
                            let count = self.missing_window_counts.entry(window_id).or_default();
                            *count = count.saturating_add(1);
                        }
                    }
                }
                WindowFeedEvent::Remove(id) => {
                    self.missing_window_counts.remove(&id);
                    self.window_enrichment_checked_at.remove(&id);
                    self.dirty_window_search_documents.remove(&id);
                    let previous_len = self.windows.len();
                    self.windows.retain(|window| window.id != id);
                    let removed = self.windows.len() != previous_len;
                    changed |= removed;
                    audio_changed |= removed;
                    search_changed |= removed;
                }
            }
        }

        if changed {
            self.tmux_monitor.sync_windows(&self.windows);
            applicationlauncher::observability::set_gauge(
                applicationlauncher::observability::Gauge::GuiWindows,
                self.windows.len(),
            );
            self.update_cached_windows_without_rerank(&cache_updates);
            if search_changed {
                self.schedule_window_search_refresh();
            }
            if audio_changed {
                self.refresh_window_audio_cache();
            }
        }
        if needs_terminal_metadata_refresh {
            self.start_terminal_metadata_refresh();
        }
    }

    pub(super) fn refresh_window_audio_cache(&mut self) -> bool {
        let previous_sinks = self.window_audio_cache.visualization_sinks.clone();
        let previous_sink_signature = sink_match_signature(&self.window_audio_cache);
        let mut new_cache = build_window_audio_cache(&self.windows, &self.cached_sink_inputs);
        apply_firefox_attribution(
            &mut new_cache,
            &self.windows,
            &self.cached_sink_inputs,
            &self.firefox_audio_processes,
        );

        let changed = previous_sinks != new_cache.visualization_sinks
            || previous_sink_signature != sink_match_signature(&new_cache);
        self.window_audio_cache = new_cache;
        changed
    }

    pub(super) fn refresh_windows(&mut self) {
        if self.loading || self.receiver.is_some() {
            return;
        }
        if let Some(ref kpath) = self.kdotool_path {
            let kpath = kpath.clone();
            let theme = self
                .force_theme
                .as_deref()
                .unwrap_or("breeze-dark")
                .to_string();
            let (tx, rx) = std::sync::mpsc::channel();
            let repaint_ctx = self.repaint_ctx.clone();
            self.loading = true;
            self.receiver = Some(rx);

            std::thread::spawn(move || {
                match applicationlauncher::process::output_with_timeout(
                    {
                        let mut command = Command::new(&kpath);
                        command.arg("--version");
                        command
                    },
                    Duration::from_secs(2),
                ) {
                    Ok(_) => {
                        let windows = get_open_windows_fast(&kpath, &theme).unwrap_or_default();
                        let _ = tx.send(LoadResult::WindowsSuccess(windows));
                        repaint_ctx.request_repaint();
                    }
                    Err(_) => {
                        let _ = tx.send(LoadResult::Error(format!(
                            "kdotool utility not found.\n\nPlease install it using cargo:\n\ncargo install kdotool"
                        )));
                        repaint_ctx.request_repaint();
                    }
                }
            });
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

    pub(super) fn apply_window_reconciliation(&mut self, mut discovered: Vec<WindowInfo>) {
        self.overlay_tmux_metadata(&mut discovered);
        let mut audio_changed = false;
        for window in &discovered {
            let old = self.windows.iter().find(|old| old.id == window.id);
            if old.is_none_or(|old| !window_search_metadata_equal(old, window)) {
                self.dirty_window_search_documents.insert(window.id.clone());
            }
            audio_changed |=
                old.is_none_or(|old| !audio_metadata_equal(old, window, &self.cached_sink_inputs));
            self.missing_window_counts.remove(&window.id);
        }
        let (changed, search_changed, cache_updates) =
            merge_reconciled_windows(&mut self.windows, discovered);
        if !changed {
            return;
        }

        self.tmux_monitor.sync_windows(&self.windows);
        self.seed_window_icon_cache();
        self.update_cached_windows_without_rerank(&cache_updates);
        if search_changed {
            self.schedule_window_search_refresh();
        }
        if audio_changed {
            self.refresh_window_audio_cache();
        }
    }

    fn overlay_tmux_metadata(&self, windows: &mut [WindowInfo]) {
        for window in windows {
            if let Some(metadata) = self.tmux_metadata.get(&window.id) {
                metadata.apply(window);
            }
        }
    }

    pub(super) fn update_tmux_metadata(&mut self) {
        let Some(latest) = self.tmux_monitor.take_latest() else {
            return;
        };
        self.tmux_metadata = latest;
        let mut updates = Vec::new();
        for window in &self.windows {
            if window.tmux_pane.is_none() && !self.tmux_metadata.contains_key(&window.id) {
                continue;
            }
            if let Some(metadata) = self.tmux_metadata.get(&window.id)
                && window.tmux_pane.as_ref() == Some(&metadata.pane)
            {
                continue;
            }
            let mut updated = window.clone();
            if let Some(metadata) = self.tmux_metadata.get(&window.id) {
                metadata.apply(&mut updated);
            } else if updated.tmux_pane.take().is_some() {
                updated.title = normalize_terminal_title_marker_position(&updated.raw_title);
                updated.active_process = None;
                updated.cwd_path = None;
                updated.command_line = None;
                updated.command_summary = None;
                updated.process_chain.clear();
            }
            if updated != *window {
                updates.push(updated);
            }
        }
        // Spinner frames follow the existing transient-title cache path, not fuzzy reranking.
        self.apply_window_reconciliation(updates);
    }

    pub(super) fn start_background_app_load(&mut self) {
        if self.background_apps_receiver.is_some() {
            self.background_apps_refresh_queued = true;
            return;
        }
        self.background_apps_refresh_queued = false;
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
        if self.loading || self.receiver.is_some() {
            return;
        }
        let theme = self
            .force_theme
            .as_deref()
            .unwrap_or("breeze-dark")
            .to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        let repaint_ctx = self.repaint_ctx.clone();
        self.loading = true;
        self.receiver = Some(rx);

        std::thread::spawn(move || {
            let apps = get_installed_apps(&theme);
            let _ = tx.send(LoadResult::AppsSuccess(apps));
            repaint_ctx.request_repaint();
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

        applicationlauncher::observability::spawn_named("window-poll", move |worker| {
            worker.set_state("polling");
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
