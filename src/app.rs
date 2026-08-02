use eframe::egui;
use fuzzy_rank::metadata::{MetadataCandidate, MetadataQuery};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Arc,
    mpsc::{Receiver, Sender},
};
use std::time::{Duration, Instant};

use crate::models::*;
use crate::*;

const HISTORY_POPUP_REFRESH_INTERVAL_MS: u64 = 750;

#[derive(Default)]
struct HistoryPopupState {
    history: Vec<applicationlauncher::tracker::HistoryEntry>,
    snapshots: Vec<applicationlauncher::tracker::SnapshotSummary>,
    snapshot_name: String,
    show_sessions: bool,
    loading: bool,
    action_pending: bool,
    refresh_in_flight: bool,
    last_refresh_started: Option<Instant>,
    message: Option<String>,
}

fn refresh_history_popup(state: Arc<std::sync::Mutex<HistoryPopupState>>, ctx: egui::Context) {
    if let Ok(mut state) = state.lock() {
        state.refresh_in_flight = true;
        state.last_refresh_started = Some(Instant::now());
    }
    std::thread::spawn(move || {
        let result = applicationlauncher::tracker::TrackerClient::connect()
            .and_then(|client| Ok((client.history(500)?, client.snapshots()?)));
        if let Ok(mut state) = state.lock() {
            state.loading = false;
            state.refresh_in_flight = false;
            match result {
                Ok((history, snapshots)) => {
                    state.history = history;
                    state.snapshots = snapshots;
                }
                Err(err) => state.message = Some(format!("Tracker unavailable: {err}")),
            }
        }
        ctx.request_repaint();
    });
}

fn history_age(timestamp_ms: i64) -> String {
    let seconds = ((applicationlauncher::tracker::now_ms() - timestamp_ms).max(0) / 1000) as u64;
    match seconds {
        0..=59 => format!("{seconds}s ago"),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86_399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

fn format_activation_time(timestamp_ms: Option<i64>) -> String {
    let Some(timestamp_ms) = timestamp_ms.filter(|timestamp| *timestamp > 0) else {
        return "Not recorded yet".to_string();
    };
    let seconds = timestamp_ms.div_euclid(1000);
    let milliseconds = timestamp_ms.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let hour = day_seconds / 3_600;
    let minute = day_seconds % 3_600 / 60;
    let second = day_seconds % 60;

    // Convert days since Unix epoch to a proleptic Gregorian UTC date.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);

    format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{milliseconds:03} UTC ({})",
        history_age(timestamp_ms)
    )
}

fn run_history_action(
    state: Arc<std::sync::Mutex<HistoryPopupState>>,
    ctx: egui::Context,
    action: impl FnOnce(applicationlauncher::tracker::TrackerClient) -> Result<String, String>
    + Send
    + 'static,
) {
    if let Ok(mut state) = state.lock() {
        if state.action_pending {
            return;
        }
        state.action_pending = true;
    }
    ctx.request_repaint();
    std::thread::spawn(move || {
        let result = applicationlauncher::tracker::TrackerClient::connect().and_then(action);
        if let Ok(mut state) = state.lock() {
            state.message = Some(result.unwrap_or_else(|err| format!("Failed: {err}")));
            state.loading = true;
            state.action_pending = false;
        }
        refresh_history_popup(state, ctx);
    });
}

pub(crate) struct App {
    mode: LauncherMode,
    windows: Vec<WindowInfo>,
    window_icon_cache: HashMap<WindowIconCacheKey, Option<PathBuf>>,
    apps: Vec<AppInfo>,
    pinned_apps: Vec<PathBuf>,
    search_query: String,
    selected_index: usize,
    side_panel_selected_index: usize,
    active_pane: ActivePane,
    order_windows_by_last_activation: bool,
    rendered_app_grid_columns: usize,
    rendered_side_panel_grid_columns: usize,
    rendered_window_row_centers: Vec<f32>,
    rendered_side_panel_item_centers: Vec<f32>,
    scroll_to_first_window_on_focus: bool,
    kdotool_path: Option<PathBuf>,
    error_message: Option<String>,
    start_time: Instant,
    search_focus_until: Option<Instant>,
    close_on_blur: bool,
    force_theme: Option<String>,
    loading: bool,
    receiver: Option<std::sync::mpsc::Receiver<LoadResult>>,
    background_apps_receiver: Option<Receiver<Vec<AppInfo>>>,
    background_window_enrichment_receiver: Option<Receiver<Vec<WindowInfo>>>,
    background_window_reconciliation_receiver: Option<Receiver<Option<Vec<WindowInfo>>>>,
    next_window_reconciliation_at: Option<Instant>,
    ui_event_rx: std::sync::mpsc::Receiver<UiEvent>,
    kwin_window_feed_setup_rx: Option<Receiver<Result<(), String>>>,
    repaint_ctx: egui::Context,
    width: f32,
    height: f32,
    ui_scale: f32,
    pending_ui_scale: f32,
    settings_menu_scale_anchor: f32,
    icon_only: bool,
    show_settings_menu: bool,
    show_history_popup: bool,
    history_popup_state: Option<Arc<std::sync::Mutex<HistoryPopupState>>>,
    tracker_status_receiver: Receiver<applicationlauncher::tracker::TrackerStatus>,
    recovery_prompt: bool,
    show_system_settings_modules: bool,
    win_icon_size: f32,
    win_top_padding: f32,
    win_bottom_padding: f32,
    win_left_padding: f32,
    win_right_padding: f32,
    win_row_height: f32,
    win_text_spacing: f32,
    win_line_height: f32,
    win_show_path: bool,
    show_run_in_terminal: bool,
    show_cd_in_terminal: bool,
    auto_send_enter_on_attention: bool,
    win_title_size: f32,
    win_path_size: f32,
    app_icon_size: f32,
    app_icon_tile_size: f32,
    app_top_padding: f32,
    app_bottom_padding: f32,
    app_left_padding: f32,
    app_right_padding: f32,
    app_icon_show_name: bool,
    app_icon_name_size: f32,
    disable_ibeam: bool,
    process_chain_popup: Option<WindowInfo>,
    app_info_popup: Option<AppInfo>,
    settings_popup_state: Option<Arc<std::sync::Mutex<SettingsWindowState>>>,
    settings_popup_applied_revision: u64,
    popup_event_sender: Sender<PopupEvent>,
    popup_event_receiver: Receiver<PopupEvent>,
    window_sender: Sender<Vec<WindowInfo>>,
    window_receiver: Receiver<Vec<WindowInfo>>,
    window_feed_receiver: Receiver<WindowFeedEvent>,
    audio_cache_receiver: Receiver<AudioCacheUpdate>,
    terminal_action_receiver: Receiver<Result<String, String>>,
    terminal_action_message: Option<(String, bool, Instant)>,
    terminal_records: Vec<TerminalDbusRecord>,
    terminal_records_receiver: Option<Receiver<Result<Vec<TerminalDbusRecord>, String>>>,
    terminal_metadata_refresh_queued: bool,
    rapid_polling: std::sync::Arc<std::sync::atomic::AtomicBool>,
    last_selected_window_id: Option<String>,
    missing_window_counts: HashMap<String, usize>,
    use_kwin_window_feed: bool,
    window_polling_started: bool,
    cached_sink_inputs: Vec<PactlSinkInput>,
    active_media_app_keys: HashSet<String>,
    observed_pipewire_node_ids: HashSet<u32>,
    active_pipewire_node_ids: HashSet<u32>,
    pipewire_activity_cache_valid: bool,
    window_audio_cache: WindowAudioCache,
    has_active_audio: bool,
    app_scroll_sensitivity: f32,
    win_scroll_sensitivity: f32,
    last_stale_prune: Option<Instant>,
    filtered_search_cache: Option<FilteredSearchCache>,
    pending_window_search_refresh_at: Option<Instant>,
    apps_generation: u64,
    windows_generation: u64,
    pinned_apps_generation: u64,
}

impl App {
    pub(crate) fn scaled_style(style: &egui::Style, factor: f32) -> egui::Style {
        let mut style = style.clone();

        for font_id in style.text_styles.values_mut() {
            font_id.size *= factor;
        }

        style.spacing.item_spacing *= factor;
        style.spacing.window_margin = style.spacing.window_margin * factor;
        style.spacing.menu_margin = style.spacing.menu_margin * factor;
        style.spacing.button_padding *= factor;
        style.spacing.indent *= factor;
        style.spacing.interact_size *= factor;
        style.spacing.slider_width *= factor;
        style.spacing.slider_rail_height *= factor;
        style.spacing.combo_width *= factor;
        style.spacing.text_edit_width *= factor;
        style.spacing.icon_width *= factor;
        style.spacing.icon_width_inner *= factor;
        style.spacing.icon_spacing *= factor;
        style.spacing.default_area_size *= factor;
        style.spacing.tooltip_width *= factor;
        style.spacing.menu_width *= factor;
        style.spacing.menu_spacing *= factor;
        style.spacing.combo_height *= factor;
        style.interaction.interact_radius *= factor;
        style.interaction.resize_grab_radius_side *= factor;
        style.interaction.resize_grab_radius_corner *= factor;

        style
    }

    fn apply_ui_scale(&mut self, ctx: &egui::Context, ui_scale: f32) {
        let clamped = ui_scale.clamp(0.5, 2.5);
        if (self.ui_scale - clamped).abs() < f32::EPSILON {
            return;
        }

        self.ui_scale = clamped;
        ctx.set_zoom_factor(clamped);
        self.save_settings();
        ctx.request_repaint();
    }

    fn open_settings_menu(&mut self) {
        self.settings_menu_scale_anchor = self.ui_scale;
        self.pending_ui_scale = self.ui_scale;
        self.settings_popup_state = Some(Arc::new(std::sync::Mutex::new(
            SettingsWindowState::new(self.launcher_settings_snapshot()),
        )));
        self.settings_popup_applied_revision = 0;
        self.show_settings_menu = true;
    }

    fn close_settings_menu(&mut self) {
        self.repaint_ctx.send_viewport_cmd_to(
            egui::ViewportId::from_hash_of("launcher_settings_popup"),
            egui::ViewportCommand::Close,
        );
        self.settings_menu_scale_anchor = self.ui_scale;
        self.pending_ui_scale = self.ui_scale;
        self.settings_popup_state = None;
        self.show_settings_menu = false;
    }

    fn open_history_popup(&mut self) {
        let state = Arc::new(std::sync::Mutex::new(HistoryPopupState {
            loading: true,
            ..Default::default()
        }));
        refresh_history_popup(Arc::clone(&state), self.repaint_ctx.clone());
        self.history_popup_state = Some(state);
        self.show_history_popup = true;
    }

    fn close_history_popup(&mut self) {
        self.repaint_ctx.send_viewport_cmd_to(
            egui::ViewportId::from_hash_of("launcher_history_popup"),
            egui::ViewportCommand::Close,
        );
        self.history_popup_state = None;
        self.show_history_popup = false;
    }

    fn show_history_native_viewport(&mut self, ctx: &egui::Context) {
        let Some(shared_state) = self.history_popup_state.clone() else {
            return;
        };
        let event_sender = self.popup_event_sender.clone();
        let builder = egui::ViewportBuilder::default()
            .with_title("Window History and Sessions")
            .with_inner_size([760.0, 620.0])
            .with_min_inner_size([560.0, 420.0])
            .with_resizable(true)
            .with_always_on_top();
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("launcher_history_popup"),
            builder,
            move |ctx, _class| {
                let refresh_due = shared_state
                    .lock()
                    .map(|state| {
                        !state.show_sessions
                            && !state.loading
                            && !state.action_pending
                            && !state.refresh_in_flight
                            && state.last_refresh_started.is_none_or(|started| {
                                started.elapsed()
                                    >= Duration::from_millis(HISTORY_POPUP_REFRESH_INTERVAL_MS)
                            })
                    })
                    .unwrap_or(false);
                if refresh_due {
                    refresh_history_popup(Arc::clone(&shared_state), ctx.clone());
                }
                ctx.request_repaint_after(Duration::from_millis(
                    HISTORY_POPUP_REFRESH_INTERVAL_MS,
                ));

                if ctx.input(|input| input.viewport().close_requested() || input.key_pressed(egui::Key::Escape) || input.key_pressed(egui::Key::F9)) {
                    let _ = event_sender.send(PopupEvent::CloseHistory);
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }

                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.heading("Window History and Sessions");
                    ui.add_space(8.0);
                    let mut action: Option<Box<dyn FnOnce(applicationlauncher::tracker::TrackerClient) -> Result<String, String> + Send>> = None;
                    let mut close = false;
                    if let Ok(mut state) = shared_state.lock() {
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut state.show_sessions, false, "Recently closed");
                            ui.selectable_value(&mut state.show_sessions, true, "Saved sessions");
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("Close").clicked() { close = true; }
                            });
                        });
                        ui.separator();
                        if let Some(message) = &state.message { ui.label(message); ui.separator(); }
                        if state.loading || state.action_pending { ui.spinner(); }

                        if state.show_sessions {
                            ui.horizontal(|ui| {
                                ui.label("Name");
                                ui.text_edit_singleline(&mut state.snapshot_name);
                                if ui.button("Save current session").clicked() {
                                    let name = if state.snapshot_name.trim().is_empty() { format!("Session {}", state.snapshots.len() + 1) } else { state.snapshot_name.trim().to_string() };
                                    action = Some(Box::new(move |client| client.create_snapshot(&name).map(|_| format!("Saved session '{name}'"))));
                                    state.snapshot_name.clear();
                                }
                            });
                            ui.add_space(8.0);
                            egui::ScrollArea::vertical().show(ui, |ui| {
                                for snapshot in state.snapshots.clone() {
                                    if snapshot.kind == "recovery" { continue; }
                                    ui.horizontal(|ui| {
                                        ui.label(snapshot.name.clone().unwrap_or_else(|| "Unnamed session".into()));
                                        ui.label(format!("{} windows, {}", snapshot.window_count, history_age(snapshot.created_at_ms)));
                                        if ui.add_enabled(!state.action_pending, egui::Button::new("Restore")).clicked() { let id = snapshot.id; action = Some(Box::new(move |client| client.restore_snapshot(id).map(|report| format!("Restored: {} existing, {} launched, {} failures", report.matched, report.launched, report.failures.len())))); }
                                        if ui.add_enabled(!state.action_pending, egui::Button::new("Delete")).clicked() { let id = snapshot.id; action = Some(Box::new(move |client| client.delete_snapshot(id).map(|_| "Session deleted".into()))); }
                                    });
                                    ui.separator();
                                }
                            });
                        } else {
                            if ui.add_enabled(!state.action_pending, egui::Button::new("Clear history")).clicked() { action = Some(Box::new(|client| client.clear_history().map(|_| "History cleared".into()))); }
                            egui::ScrollArea::vertical().show(ui, |ui| {
                                for entry in state.history.clone() {
                                    ui.horizontal(|ui| {
                                        ui.vertical(|ui| { ui.label(&entry.window.title); ui.small(format!("{} | {}", entry.restore.app_key, history_age(entry.closed_at_ms))); });
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            if ui.add_enabled(!state.action_pending, egui::Button::new("Reopen")).clicked() { let id = entry.id; action = Some(Box::new(move |client| client.reopen_history(id).map(|report| format!("Reopened: {} existing, {} launched, {} failures", report.matched, report.launched, report.failures.len())))); }
                                        });
                                    });
                                    ui.separator();
                                }
                            });
                        }
                    }
                    if close {
                        let _ = event_sender.send(PopupEvent::CloseHistory);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if let Some(action) = action { run_history_action(Arc::clone(&shared_state), ctx.clone(), action); }
                });
            },
        );
    }

    pub(crate) fn new(
        cc: &eframe::CreationContext<'_>,
        close_on_blur: bool,
        force_theme: Option<String>,
        mode: LauncherMode,
        icon_only: bool,
        ui_event_rx: std::sync::mpsc::Receiver<UiEvent>,
    ) -> Self {
        // Install loaders to enable SVG and PNG image support
        egui_extras::install_image_loaders(&cc.egui_ctx);

        setup_system_fonts(&cc.egui_ctx);

        // Styling the theme for custom dark acrylic style
        let mut visuals = egui::Visuals::dark();
        visuals.window_corner_radius = egui::CornerRadius::same(12);
        visuals.widgets.active.corner_radius = egui::CornerRadius::same(8);
        visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(8);
        visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(8);

        visuals.widgets.inactive.weak_bg_fill =
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 6);
        visuals.widgets.hovered.weak_bg_fill =
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 16);
        visuals.widgets.active.weak_bg_fill =
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30);
        visuals.override_text_color = Some(egui::Color32::WHITE);

        cc.egui_ctx.set_visuals(visuals);

        let kdotool_path = get_kdotool_path();
        let (width, height) = load_window_size();
        let pinned_apps = load_pinned_apps();
        let settings = load_launcher_settings();
        cc.egui_ctx.set_zoom_factor(settings.ui_scale);

        let (window_tx, window_rx) = std::sync::mpsc::channel();
        let (window_feed_tx, window_feed_rx) = std::sync::mpsc::channel();
        let (audio_cache_tx, audio_cache_rx) = std::sync::mpsc::channel();
        let (_terminal_action_tx, terminal_action_rx) = std::sync::mpsc::channel();
        let (popup_event_tx, popup_event_rx) = std::sync::mpsc::channel();
        let (tracker_status_tx, tracker_status_rx) = std::sync::mpsc::channel();
        let (kwin_window_feed_setup_tx, kwin_window_feed_setup_rx) = std::sync::mpsc::channel();
        let rapid_polling = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let kwin_window_feed_repaint_ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            let result = setup_kwin_window_feed(window_feed_tx, kwin_window_feed_repaint_ctx);
            let _ = kwin_window_feed_setup_tx.send(result);
        });

        let configured_auto_enter = settings.auto_send_enter_on_attention;
        std::thread::spawn(move || {
            for _ in 0..30 {
                if let Ok(client) = applicationlauncher::tracker::TrackerClient::connect()
                    && let Ok(status) = client.status()
                {
                    let _ = client.set_auto_enter(configured_auto_enter);
                    let _ = tracker_status_tx.send(status);
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });

        let now = Instant::now();
        let mut app = Self {
            mode,
            windows: Vec::new(),
            window_icon_cache: HashMap::new(),
            apps: Vec::new(),
            pinned_apps,
            search_query: String::new(),
            selected_index: 0,
            side_panel_selected_index: 0,
            active_pane: if mode == LauncherMode::Windows {
                ActivePane::Windows
            } else {
                ActivePane::Apps
            },
            order_windows_by_last_activation: false,
            rendered_app_grid_columns: 1,
            rendered_side_panel_grid_columns: 1,
            rendered_window_row_centers: Vec::new(),
            rendered_side_panel_item_centers: Vec::new(),
            scroll_to_first_window_on_focus: false,
            kdotool_path: Some(kdotool_path),
            error_message: None,
            start_time: now,
            search_focus_until: Some(now + Duration::from_millis(1200)),
            close_on_blur,
            force_theme,
            loading: false,
            receiver: None,
            background_apps_receiver: None,
            background_window_enrichment_receiver: None,
            background_window_reconciliation_receiver: None,
            next_window_reconciliation_at: None,
            ui_event_rx,
            kwin_window_feed_setup_rx: Some(kwin_window_feed_setup_rx),
            repaint_ctx: cc.egui_ctx.clone(),
            width,
            height,
            ui_scale: settings.ui_scale,
            pending_ui_scale: settings.ui_scale,
            settings_menu_scale_anchor: settings.ui_scale,
            icon_only: icon_only || settings.app_icon_mode,
            show_settings_menu: false,
            show_history_popup: false,
            history_popup_state: None,
            tracker_status_receiver: tracker_status_rx,
            recovery_prompt: false,
            show_system_settings_modules: settings.show_system_settings_modules,
            win_icon_size: settings.win_icon_size,
            win_top_padding: settings.win_top_padding,
            win_bottom_padding: settings.win_bottom_padding,
            win_left_padding: settings.win_left_padding,
            win_right_padding: settings.win_right_padding,
            win_row_height: settings.win_row_height,
            win_text_spacing: settings.win_text_spacing,
            win_line_height: settings.win_line_height,
            win_show_path: settings.win_show_path,
            show_run_in_terminal: settings.show_run_in_terminal,
            show_cd_in_terminal: settings.show_cd_in_terminal,
            auto_send_enter_on_attention: settings.auto_send_enter_on_attention,
            win_title_size: settings.win_title_size,
            win_path_size: settings.win_path_size,
            app_icon_size: settings.app_icon_size,
            app_icon_tile_size: settings.app_icon_tile_size,
            app_top_padding: settings.app_top_padding,
            app_bottom_padding: settings.app_bottom_padding,
            app_left_padding: settings.app_left_padding,
            app_right_padding: settings.app_right_padding,
            app_icon_show_name: settings.app_icon_show_name,
            app_icon_name_size: settings.app_icon_name_size,
            disable_ibeam: settings.disable_ibeam,
            process_chain_popup: None,
            app_info_popup: None,
            settings_popup_state: None,
            settings_popup_applied_revision: 0,
            popup_event_sender: popup_event_tx,
            popup_event_receiver: popup_event_rx,
            window_sender: window_tx.clone(),
            window_receiver: window_rx,
            window_feed_receiver: window_feed_rx,
            audio_cache_receiver: audio_cache_rx,
            terminal_action_receiver: terminal_action_rx,
            terminal_action_message: None,
            terminal_records: Vec::new(),
            terminal_records_receiver: None,
            terminal_metadata_refresh_queued: false,
            rapid_polling: std::sync::Arc::clone(&rapid_polling),
            last_selected_window_id: None,
            missing_window_counts: HashMap::new(),
            use_kwin_window_feed: false,
            window_polling_started: false,
            cached_sink_inputs: Vec::new(),
            active_media_app_keys: HashSet::new(),
            observed_pipewire_node_ids: HashSet::new(),
            active_pipewire_node_ids: HashSet::new(),
            pipewire_activity_cache_valid: false,
            window_audio_cache: WindowAudioCache::default(),
            has_active_audio: false,
            app_scroll_sensitivity: settings.app_scroll_sensitivity,
            win_scroll_sensitivity: settings.win_scroll_sensitivity,
            last_stale_prune: None,
            filtered_search_cache: None,
            pending_window_search_refresh_at: None,
            apps_generation: 0,
            windows_generation: 0,
            pinned_apps_generation: 0,
        };

        std::thread::spawn(move || {
            let mut recent_active_pipewire_nodes: HashMap<u32, std::time::Instant> = HashMap::new();
            loop {
                let sink_inputs = fetch_sink_inputs();
                let active_media_app_keys = fetch_active_media_app_keys();
                let (
                    observed_pipewire_node_ids,
                    active_pipewire_node_ids,
                    pipewire_activity_cache_valid,
                ) = fetch_pipewire_activity();
                let now = std::time::Instant::now();

                if pipewire_activity_cache_valid {
                    for id in active_pipewire_node_ids {
                        recent_active_pipewire_nodes.insert(id, now);
                    }
                    recent_active_pipewire_nodes.retain(|_, last_seen| {
                        now.duration_since(*last_seen).as_millis() <= AUDIO_ACTIVITY_GRACE_MS
                    });
                } else {
                    recent_active_pipewire_nodes.clear();
                }

                let effective_active_pipewire_node_ids = recent_active_pipewire_nodes
                    .keys()
                    .copied()
                    .collect::<HashSet<u32>>();

                if audio_cache_tx
                    .send(AudioCacheUpdate {
                        sink_inputs,
                        active_media_app_keys,
                        observed_pipewire_node_ids,
                        active_pipewire_node_ids: effective_active_pipewire_node_ids,
                        pipewire_activity_cache_valid,
                    })
                    .is_err()
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(AUDIO_SINK_POLL_MS as u64));
            }
        });

        match app.mode {
            LauncherMode::Apps => app.refresh_apps(),
            LauncherMode::Windows => {
                app.refresh_windows();
                app.start_background_app_load();
            }
        }
        app.start_terminal_metadata_refresh();

        app
    }

    fn save_window_size(&self) {
        if let Ok(home) = std::env::var("HOME") {
            let dir = PathBuf::from(format!("{}/.config/applicationlauncher", home));
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join("window_size.txt");
            let base_width = (self.width * self.ui_scale).clamp(300.0, 1920.0);
            let base_height = (self.height * self.ui_scale).clamp(200.0, 1080.0);
            let content = format!("{}\n{}", base_width, base_height);
            let _ = std::fs::write(path, content);
        }
    }

    fn launcher_settings_snapshot(&self) -> LauncherSettings {
        LauncherSettings {
            show_system_settings_modules: self.show_system_settings_modules,
            app_icon_mode: self.icon_only,
            win_icon_size: self.win_icon_size,
            win_top_padding: self.win_top_padding,
            win_bottom_padding: self.win_bottom_padding,
            win_left_padding: self.win_left_padding,
            win_right_padding: self.win_right_padding,
            win_row_height: self.win_row_height,
            win_text_spacing: self.win_text_spacing,
            win_line_height: self.win_line_height,
            win_show_path: self.win_show_path,
            show_run_in_terminal: self.show_run_in_terminal,
            show_cd_in_terminal: self.show_cd_in_terminal,
            auto_send_enter_on_attention: self.auto_send_enter_on_attention,
            win_title_size: self.win_title_size,
            win_path_size: self.win_path_size,
            app_icon_size: self.app_icon_size,
            app_icon_tile_size: self.app_icon_tile_size,
            app_top_padding: self.app_top_padding,
            app_bottom_padding: self.app_bottom_padding,
            app_left_padding: self.app_left_padding,
            app_right_padding: self.app_right_padding,
            app_icon_show_name: self.app_icon_show_name,
            app_icon_name_size: self.app_icon_name_size,
            disable_ibeam: self.disable_ibeam,
            app_scroll_sensitivity: self.app_scroll_sensitivity,
            win_scroll_sensitivity: self.win_scroll_sensitivity,
            ui_scale: self.ui_scale,
        }
    }

    fn save_settings(&self) {
        save_launcher_settings(self.launcher_settings_snapshot());
    }

    fn apply_launcher_settings_snapshot(
        &mut self,
        settings: LauncherSettings,
        ctx: &egui::Context,
    ) {
        self.show_system_settings_modules = settings.show_system_settings_modules;
        self.icon_only = settings.app_icon_mode;
        self.win_icon_size = settings.win_icon_size;
        self.win_top_padding = settings.win_top_padding;
        self.win_bottom_padding = settings.win_bottom_padding;
        self.win_left_padding = settings.win_left_padding;
        self.win_right_padding = settings.win_right_padding;
        self.win_row_height = settings.win_row_height;
        self.win_text_spacing = settings.win_text_spacing;
        self.win_line_height = settings.win_line_height;
        self.win_show_path = settings.win_show_path;
        self.show_run_in_terminal = settings.show_run_in_terminal;
        self.show_cd_in_terminal = settings.show_cd_in_terminal;
        self.auto_send_enter_on_attention = settings.auto_send_enter_on_attention;
        self.win_title_size = settings.win_title_size;
        self.win_path_size = settings.win_path_size;
        self.app_icon_size = settings.app_icon_size;
        self.app_icon_tile_size = settings.app_icon_tile_size;
        self.app_top_padding = settings.app_top_padding;
        self.app_bottom_padding = settings.app_bottom_padding;
        self.app_left_padding = settings.app_left_padding;
        self.app_right_padding = settings.app_right_padding;
        self.app_icon_show_name = settings.app_icon_show_name;
        self.app_icon_name_size = settings.app_icon_name_size;
        self.disable_ibeam = settings.disable_ibeam;
        self.app_scroll_sensitivity = settings.app_scroll_sensitivity;
        self.win_scroll_sensitivity = settings.win_scroll_sensitivity;
        let enabled = settings.auto_send_enter_on_attention;
        std::thread::spawn(move || {
            if let Ok(client) = applicationlauncher::tracker::TrackerClient::connect() {
                let _ = client.set_auto_enter(enabled);
            }
        });
        if (self.ui_scale - settings.ui_scale).abs() > 0.001 {
            self.ui_scale = settings.ui_scale;
            ctx.set_zoom_factor(settings.ui_scale);
        }
        ctx.request_repaint();
    }

    fn save_pinned_apps(&mut self) {
        if let Ok(home) = std::env::var("HOME") {
            let dir = PathBuf::from(format!("{}/.config/applicationlauncher", home));
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join("pinned_apps.txt");
            let mut content = String::new();
            for p in &self.pinned_apps {
                content.push_str(&p.to_string_lossy());
                content.push('\n');
            }
            let _ = std::fs::write(path, content);
        }
        self.pinned_apps_generation = self.pinned_apps_generation.wrapping_add(1);
    }

    fn render_settings_panel(&mut self, ui: &mut egui::Ui) -> bool {
        let mut close_requested = false;
        let scale_factor = (self.settings_menu_scale_anchor / self.ui_scale).clamp(0.2, 5.0);
        if (scale_factor - 1.0).abs() > 0.001 {
            let scaled_style = Self::scaled_style(ui.style().as_ref(), scale_factor);
            ui.set_style(scaled_style);
        }

        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            ui.heading(
                egui::RichText::new("Launcher Settings")
                    .color(egui::Color32::WHITE)
                    .strong(),
            );
        });
        ui.add_space(8.0);

        ui.add(egui::Separator::default());
        ui.add_space(12.0);

        ui.horizontal(|ui| {
            let mut disable_ibeam = self.disable_ibeam;
            let checkbox_response = ui.checkbox(
                &mut disable_ibeam,
                egui::RichText::new("Disable text select cursor (I-beam)")
                    .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220))
                    .size(13.0),
            );
            if checkbox_response.changed() {
                self.disable_ibeam = disable_ibeam;
                self.save_settings();
            }
        });

        ui.add_space(10.0);
        egui::Grid::new("global_scale_settings_grid")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("Global Scale:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Slider::new(&mut self.pending_ui_scale, 0.5..=2.5).show_value(true),
                    );
                    let scale_changed = (self.pending_ui_scale - self.ui_scale).abs() > 0.001;
                    if ui
                        .add_enabled(scale_changed, egui::Button::new("Apply"))
                        .clicked()
                    {
                        self.apply_ui_scale(ui.ctx(), self.pending_ui_scale);
                    }
                    if ui
                        .add_enabled(scale_changed, egui::Button::new("Reset"))
                        .clicked()
                    {
                        self.pending_ui_scale = self.ui_scale;
                    }
                });
                if self.pending_ui_scale.is_nan() {
                    self.pending_ui_scale = self.ui_scale;
                }
                ui.end_row();
            });

        ui.add_space(14.0);
        ui.add(egui::Separator::default());
        ui.add_space(10.0);

        ui.label(
            egui::RichText::new("Application panel")
                .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220))
                .strong()
                .size(13.0),
        );
        ui.add_space(6.0);

        egui::Grid::new("app_panel_settings_grid")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("Show System Modules:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut show_val = self.show_system_settings_modules;
                if ui.checkbox(&mut show_val, "").changed() {
                    self.show_system_settings_modules = show_val;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Icon Grid Mode:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut icon_mode = self.icon_only;
                if ui.checkbox(&mut icon_mode, "").changed() {
                    self.icon_only = icon_mode;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Icon Size:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut app_icon_size = self.app_icon_size;
                if ui
                    .add(egui::Slider::new(&mut app_icon_size, 16.0..=64.0).show_value(true))
                    .changed()
                {
                    self.app_icon_size = app_icon_size;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Tile Size:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut app_tile_size = self.app_icon_tile_size;
                if ui
                    .add(egui::Slider::new(&mut app_tile_size, 48.0..=128.0).show_value(true))
                    .changed()
                {
                    self.app_icon_tile_size = app_tile_size;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Top Padding:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut app_top_padding = self.app_top_padding;
                if ui
                    .add(egui::Slider::new(&mut app_top_padding, 0.0..=24.0).show_value(true))
                    .changed()
                {
                    self.app_top_padding = app_top_padding;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Bottom Padding:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut app_bottom_padding = self.app_bottom_padding;
                if ui
                    .add(egui::Slider::new(&mut app_bottom_padding, 0.0..=24.0).show_value(true))
                    .changed()
                {
                    self.app_bottom_padding = app_bottom_padding;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Left Padding:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut app_left_padding = self.app_left_padding;
                if ui
                    .add(egui::Slider::new(&mut app_left_padding, 0.0..=32.0).show_value(true))
                    .changed()
                {
                    self.app_left_padding = app_left_padding;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Right Padding:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut app_right_padding = self.app_right_padding;
                if ui
                    .add(egui::Slider::new(&mut app_right_padding, 0.0..=32.0).show_value(true))
                    .changed()
                {
                    self.app_right_padding = app_right_padding;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Show Names:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut app_icon_show_name = self.app_icon_show_name;
                if ui.checkbox(&mut app_icon_show_name, "").changed() {
                    self.app_icon_show_name = app_icon_show_name;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Name Size:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut app_icon_name_size = self.app_icon_name_size;
                if ui
                    .add(egui::Slider::new(&mut app_icon_name_size, 8.0..=20.0).show_value(true))
                    .changed()
                {
                    self.app_icon_name_size = app_icon_name_size;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Scroll Sensitivity:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut app_scroll_sens = self.app_scroll_sensitivity;
                if ui
                    .add(egui::Slider::new(&mut app_scroll_sens, 0.1..=5.0).show_value(true))
                    .changed()
                {
                    self.app_scroll_sensitivity = app_scroll_sens;
                    self.save_settings();
                }
                ui.end_row();
            });

        ui.add_space(14.0);
        ui.add(egui::Separator::default());
        ui.add_space(10.0);

        ui.label(
            egui::RichText::new("Open window view")
                .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220))
                .strong()
                .size(13.0),
        );
        ui.add_space(6.0);

        egui::Grid::new("win_settings_grid")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("Icon Size:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut icon_size = self.win_icon_size;
                if ui
                    .add(egui::Slider::new(&mut icon_size, 16.0..=64.0).show_value(true))
                    .changed()
                {
                    self.win_icon_size = icon_size;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Top Padding:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut win_top_padding = self.win_top_padding;
                if ui
                    .add(egui::Slider::new(&mut win_top_padding, 0.0..=24.0).show_value(true))
                    .changed()
                {
                    self.win_top_padding = win_top_padding;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Bottom Padding:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut win_bottom_padding = self.win_bottom_padding;
                if ui
                    .add(egui::Slider::new(&mut win_bottom_padding, 0.0..=24.0).show_value(true))
                    .changed()
                {
                    self.win_bottom_padding = win_bottom_padding;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Left Padding:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut win_left_padding = self.win_left_padding;
                if ui
                    .add(egui::Slider::new(&mut win_left_padding, 0.0..=32.0).show_value(true))
                    .changed()
                {
                    self.win_left_padding = win_left_padding;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Right Padding:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut win_right_padding = self.win_right_padding;
                if ui
                    .add(egui::Slider::new(&mut win_right_padding, 0.0..=32.0).show_value(true))
                    .changed()
                {
                    self.win_right_padding = win_right_padding;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Row Height:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut row_height = self.win_row_height;
                if ui
                    .add(egui::Slider::new(&mut row_height, 12.0..=100.0).show_value(true))
                    .changed()
                {
                    self.win_row_height = row_height;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Text Spacing:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut text_spacing = self.win_text_spacing;
                if ui
                    .add(egui::Slider::new(&mut text_spacing, 0.0..=12.0).show_value(true))
                    .changed()
                {
                    self.win_text_spacing = text_spacing;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Line Height:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut line_height = self.win_line_height;
                if ui
                    .add(egui::Slider::new(&mut line_height, 6.0..=30.0).show_value(true))
                    .changed()
                {
                    self.win_line_height = line_height;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Show Path:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut show_path = self.win_show_path;
                if ui.checkbox(&mut show_path, "").changed() {
                    self.win_show_path = show_path;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Show Run in Terminal:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut show_run_in_terminal = self.show_run_in_terminal;
                if ui.checkbox(&mut show_run_in_terminal, "").changed() {
                    self.show_run_in_terminal = show_run_in_terminal;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Show CD in Terminal:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut show_cd_in_terminal = self.show_cd_in_terminal;
                if ui.checkbox(&mut show_cd_in_terminal, "").changed() {
                    self.show_cd_in_terminal = show_cd_in_terminal;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Auto-send Enter on Attention (5s):")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut auto_send_enter = self.auto_send_enter_on_attention;
                if ui.checkbox(&mut auto_send_enter, "").changed() {
                    self.auto_send_enter_on_attention = auto_send_enter;
                    std::thread::spawn(move || {
                        if let Ok(client) = applicationlauncher::tracker::TrackerClient::connect() {
                            let _ = client.set_auto_enter(auto_send_enter);
                        }
                    });
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Window Title Font Size:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut title_size = self.win_title_size;
                if ui
                    .add(egui::Slider::new(&mut title_size, 6.0..=24.0).show_value(true))
                    .changed()
                {
                    self.win_title_size = title_size;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Window Path Font Size:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut path_size = self.win_path_size;
                if ui
                    .add(egui::Slider::new(&mut path_size, 6.0..=20.0).show_value(true))
                    .changed()
                {
                    self.win_path_size = path_size;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Scroll Sensitivity:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut win_scroll_sens = self.win_scroll_sensitivity;
                if ui
                    .add(egui::Slider::new(&mut win_scroll_sens, 0.1..=5.0).show_value(true))
                    .changed()
                {
                    self.win_scroll_sensitivity = win_scroll_sens;
                    self.save_settings();
                }
                ui.end_row();
            });

        ui.add_space(16.0);
        ui.vertical_centered(|ui| {
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("Close Settings (F10)")
                            .color(egui::Color32::WHITE)
                            .size(13.0),
                    )
                    .fill(egui::Color32::from_rgba_unmultiplied(61, 174, 233, 200)),
                )
                .clicked()
            {
                close_requested = true;
            }
        });
        ui.add_space(8.0);

        close_requested
    }

    fn start_terminal_metadata_refresh(&mut self) {
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

    fn apply_terminal_metadata_records(&mut self, records: Vec<TerminalDbusRecord>) {
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

    fn show_terminal_action_message(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.terminal_action_receiver.try_recv() {
            let (message, success) = match result {
                Ok(message) => (message, true),
                Err(message) => (message, false),
            };
            self.terminal_action_message = Some((message, success, Instant::now()));
        }

        let Some((message, success, created_at)) = self.terminal_action_message.clone() else {
            return;
        };
        let lifetime = Duration::from_secs(TERMINAL_ACTION_MESSAGE_SECS);
        let elapsed = created_at.elapsed();
        if elapsed >= lifetime {
            self.terminal_action_message = None;
            return;
        }

        ctx.request_repaint_after(lifetime.saturating_sub(elapsed));
        egui::Area::new(egui::Id::new("terminal_action_message"))
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -18.0])
            .order(egui::Order::Tooltip)
            .show(ctx, |ui| {
                let color = if success {
                    egui::Color32::from_rgb(98, 205, 142)
                } else {
                    egui::Color32::from_rgb(244, 112, 112)
                };
                egui::Frame::popup(&ctx.style())
                    .fill(egui::Color32::from_rgba_unmultiplied(24, 24, 24, 245))
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new(message).color(color).strong());
                    });
            });
    }

    fn process_popup_events(&mut self) {
        let mut restore_launcher_focus = false;
        while let Ok(event) = self.popup_event_receiver.try_recv() {
            match event {
                PopupEvent::CloseSettings => self.close_settings_menu(),
                PopupEvent::CloseWindowInfo => self.process_chain_popup = None,
                PopupEvent::CloseAppInfo => self.app_info_popup = None,
                PopupEvent::CloseHistory => self.close_history_popup(),
            }
            restore_launcher_focus = true;
        }
        if restore_launcher_focus {
            self.start_time = Instant::now();
            self.repaint_ctx
                .send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
        }
    }

    fn show_settings_native_viewport(&mut self, ctx: &egui::Context) {
        let Some(shared_state) = self.settings_popup_state.clone() else {
            self.settings_popup_state = Some(Arc::new(std::sync::Mutex::new(
                SettingsWindowState::new(self.launcher_settings_snapshot()),
            )));
            ctx.request_repaint();
            return;
        };

        let state_snapshot = shared_state.lock().ok().map(|state| {
            (
                state.settings,
                state.revision,
                (state.scale_anchor / state.settings.ui_scale).clamp(0.2, 5.0),
            )
        });
        let Some((settings, revision, viewport_scale_factor)) = state_snapshot else {
            return;
        };
        if revision != self.settings_popup_applied_revision {
            self.apply_launcher_settings_snapshot(settings, ctx);
            self.settings_popup_applied_revision = revision;
        }

        let builder = egui::ViewportBuilder::default()
            .with_title("Launcher Settings")
            .with_inner_size([
                (SETTINGS_VIEWPORT_SIZE[0] + 20.0) * viewport_scale_factor,
                (SETTINGS_VIEWPORT_SIZE[1] + 80.0) * viewport_scale_factor,
            ])
            .with_min_inner_size([
                SETTINGS_VIEWPORT_MIN_SIZE[0] * viewport_scale_factor,
                SETTINGS_VIEWPORT_MIN_SIZE[1] * viewport_scale_factor,
            ])
            .with_resizable(true)
            .with_always_on_top();
        let event_sender = self.popup_event_sender.clone();
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("launcher_settings_popup"),
            builder,
            move |ctx, _class| {
                let close_requested = ctx.input(|input| {
                    input.viewport().close_requested()
                        || input.key_pressed(egui::Key::Escape)
                        || input.key_pressed(egui::Key::F10)
                });
                if close_requested {
                    let _ = event_sender.send(PopupEvent::CloseSettings);
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }

                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgba_unmultiplied(20, 20, 20, 248))
                            .inner_margin(egui::Margin::same(12)),
                    )
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let Ok(mut state) = shared_state.lock() else {
                                    return;
                                };
                                let previous_revision = state.revision;
                                if render_deferred_settings_panel(ui, &mut state) {
                                    let _ = event_sender.send(PopupEvent::CloseSettings);
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                }
                                if state.revision != previous_revision {
                                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                                }
                            });
                    });
            },
        );
    }

    fn window_info_popup_data(&self, window_info: &WindowInfo) -> InfoPopupData {
        let app_key = window_application_key(window_info);
        let exe_basename = window_info
            .exe_path
            .as_ref()
            .and_then(|path| path.file_name().and_then(|name| name.to_str()))
            .unwrap_or("Unavailable")
            .to_string();
        let active_process_exe_path = window_info
            .active_process
            .as_ref()
            .and_then(|_| window_info.process_chain.first())
            .and_then(|entry| entry.exe_path.clone())
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unavailable".to_string());
        let active_process_desktop_file = window_info
            .active_process
            .as_deref()
            .and_then(|process| self.desktop_file_path_for_process(process))
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unavailable".to_string());
        let desktop_file_path = self
            .desktop_file_path_for_window(window_info)
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unavailable".to_string());
        let cwd_search_value = window_info
            .cwd_path
            .as_ref()
            .map(|path| display_path(path))
            .unwrap_or_else(|| "Unavailable".to_string());
        let class_is_searched = !window_info.class.eq_ignore_ascii_case(&app_key);
        let row = |label: &str, value: String, searched: bool| InfoPopupRow {
            label: label.to_string(),
            value,
            searched,
        };

        InfoPopupData {
            title: format!("Window Info: {}", window_info.title),
            heading: window_info.title.clone(),
            subtitle: "Window metadata, process details, and execution chain".to_string(),
            rows: vec![
                row("Title", window_info.title.clone(), true),
                row("Raw window title", window_info.raw_title.clone(), false),
                row("Application key", app_key, true),
                row("Window ID", window_info.id.clone(), false),
                row("Class", window_info.class.clone(), class_is_searched),
                row("Window desktop file", desktop_file_path, false),
                row(
                    "Window PID",
                    window_info
                        .pid
                        .map(|pid| pid.to_string())
                        .unwrap_or_else(|| "Unavailable".to_string()),
                    false,
                ),
                row(
                    "Active process",
                    window_info
                        .active_process
                        .clone()
                        .unwrap_or_else(|| "Unavailable".to_string()),
                    true,
                ),
                row("Window executable", exe_basename, true),
                row(
                    "Window executable path",
                    window_info
                        .exe_path
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Unavailable".to_string()),
                    false,
                ),
                row(
                    "Active process executable path",
                    active_process_exe_path,
                    false,
                ),
                row(
                    "Active process desktop file",
                    active_process_desktop_file,
                    false,
                ),
                row("Working directory", cwd_search_value, true),
                row(
                    "Command summary",
                    window_info
                        .command_summary
                        .clone()
                        .unwrap_or_else(|| "Unavailable".to_string()),
                    true,
                ),
                row(
                    "Command line",
                    window_info
                        .command_line
                        .clone()
                        .unwrap_or_else(|| "Unavailable".to_string()),
                    true,
                ),
                row(
                    "Geometry",
                    window_info
                        .geometry
                        .map(|(x, y, width, height)| {
                            format!("x={x}, y={y}, width={width}, height={height}")
                        })
                        .unwrap_or_else(|| "Unavailable".to_string()),
                    false,
                ),
                row(
                    "Minimized",
                    window_info
                        .minimized
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "Unavailable".to_string()),
                    false,
                ),
                row(
                    "Last activated",
                    format_activation_time(window_info.last_activated_at_ms),
                    false,
                ),
                row(
                    "Activation sequence",
                    window_info.activation_sequence.to_string(),
                    false,
                ),
            ],
            execution_chain: window_info
                .process_chain
                .iter()
                .map(|entry| {
                    (
                        format!("{} (pid {})", entry.name, entry.pid),
                        entry
                            .exe_path
                            .as_ref()
                            .map(|path| path.to_string_lossy().to_string())
                            .unwrap_or_else(|| "Executable path unavailable".to_string()),
                    )
                })
                .collect(),
        }
    }

    fn app_info_popup_data(&self, app_info: &AppInfo) -> InfoPopupData {
        let desktop_stem = app_info
            .desktop_file_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Unavailable")
            .to_string();
        let executable_basename =
            command_basename(&app_info.exec).unwrap_or_else(|| "Unavailable".to_string());
        let executable_path = executable_path_from_exec(&app_info.exec);
        let executable_path_display = executable_path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unavailable".to_string());
        let executable_exists = executable_path.as_ref().is_some_and(|path| path.is_file());
        let desktop_file_exists = app_info.desktop_file_path.exists();
        let icon_path = app_info
            .icon_path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unavailable".to_string());
        let is_pinned = self.pinned_apps.contains(&app_info.desktop_file_path);
        let row = |label: &str, value: String, searched: bool| InfoPopupRow {
            label: label.to_string(),
            value,
            searched,
        };

        InfoPopupData {
            title: format!("Application Info: {}", app_info.name),
            heading: app_info.name.clone(),
            subtitle: "Desktop-entry and application search metadata".to_string(),
            rows: vec![
                row("Name", app_info.name.clone(), true),
                row("Executable basename", executable_basename, true),
                row("Desktop file stem", desktop_stem, true),
                row(
                    "Comment",
                    app_info
                        .comment
                        .clone()
                        .unwrap_or_else(|| "Unavailable".to_string()),
                    true,
                ),
                row("Cleaned command", clean_exec_cmd(&app_info.exec), true),
                row("Executable path", executable_path_display, false),
                row("Executable exists", executable_exists.to_string(), false),
                row("Raw Exec", app_info.exec.clone(), false),
                row(
                    "Desktop file",
                    app_info.desktop_file_path.to_string_lossy().to_string(),
                    false,
                ),
                row(
                    "Desktop file exists",
                    desktop_file_exists.to_string(),
                    false,
                ),
                row("Icon path", icon_path, false),
                row("Pinned", is_pinned.to_string(), false),
                row(
                    "System settings module",
                    app_info.is_settings_module.to_string(),
                    false,
                ),
            ],
            execution_chain: Vec::new(),
        }
    }

    fn show_settings_popup(&mut self, ctx: &egui::Context) {
        if !ctx.embed_viewports() {
            self.show_settings_native_viewport(ctx);
            return;
        }
        let viewport_scale_factor =
            (self.settings_menu_scale_anchor / self.ui_scale).clamp(0.2, 5.0);
        let mut is_open = true;
        let mut should_close = false;
        egui::Window::new("Launcher Settings")
            .id(egui::Id::new("launcher_settings_popup"))
            .default_size([
                (SETTINGS_VIEWPORT_SIZE[0] + 20.0) * viewport_scale_factor,
                (SETTINGS_VIEWPORT_SIZE[1] + 80.0) * viewport_scale_factor,
            ])
            .min_size([
                SETTINGS_VIEWPORT_MIN_SIZE[0] * viewport_scale_factor,
                SETTINGS_VIEWPORT_MIN_SIZE[1] * viewport_scale_factor,
            ])
            .resizable(true)
            .collapsible(false)
            .order(egui::Order::Foreground)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(egui::Color32::from_rgba_unmultiplied(20, 20, 20, 240))
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20),
                    ))
                    .corner_radius(egui::CornerRadius::same(12)),
            )
            .open(&mut is_open)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.render_settings_panel(ui) {
                            should_close = true;
                        }
                    });
            });

        if should_close || !is_open {
            self.close_settings_menu();
        }
    }

    fn show_window_info_popup(&mut self, ctx: &egui::Context) {
        let Some(window_snapshot) = self.process_chain_popup.clone() else {
            return;
        };
        let window_info = self
            .windows
            .iter()
            .find(|window| window.id == window_snapshot.id)
            .cloned()
            .unwrap_or(window_snapshot);

        if !ctx.embed_viewports() {
            show_deferred_info_popup(
                ctx,
                egui::ViewportId::from_hash_of("launcher_process_chain_popup"),
                self.window_info_popup_data(&window_info),
                [760.0, 680.0],
                [520.0, 360.0],
                PopupEvent::CloseWindowInfo,
                self.popup_event_sender.clone(),
            );
            return;
        }

        let mut is_open = true;
        egui::Window::new(format!("Window Info: {}", window_info.title))
            .id(egui::Id::new("launcher_process_chain_popup"))
            .default_size([760.0, 680.0])
            .min_size([520.0, 360.0])
            .resizable(true)
            .collapsible(false)
            .vscroll(true)
            .order(egui::Order::Foreground)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(egui::Color32::from_rgba_unmultiplied(20, 20, 20, 240))
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20),
                    ))
                    .corner_radius(egui::CornerRadius::same(12)),
            )
            .open(&mut is_open)
            .show(ctx, |ui| {
                let searchable_label_color = egui::Color32::from_rgb(214, 184, 86);
                let searchable_value_color = egui::Color32::from_rgb(255, 236, 170);
                let neutral_label_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 170);
                let neutral_value_color = egui::Color32::WHITE;
                let app_key = window_application_key(&window_info);
                let exe_basename = window_info
                    .exe_path
                    .as_ref()
                    .and_then(|path| path.file_name().and_then(|name| name.to_str()))
                    .map(|name| name.to_string())
                    .unwrap_or_else(|| "Unavailable".to_string());
                let active_process_exe_path = window_info
                    .active_process
                    .as_ref()
                    .and_then(|_| window_info.process_chain.first())
                    .and_then(|entry| entry.exe_path.clone());
                let active_process_desktop_file = window_info
                    .active_process
                    .as_deref()
                    .and_then(|process| self.desktop_file_path_for_process(process))
                    .map(|path| path.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Unavailable".to_string());
                let desktop_file_path = self
                    .desktop_file_path_for_window(&window_info)
                    .map(|path| path.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Unavailable".to_string());
                let cwd_search_value = window_info
                    .cwd_path
                    .as_ref()
                    .map(|path| display_path(path))
                    .unwrap_or_else(|| "Unavailable".to_string());
                let class_is_searched = !window_info.class.eq_ignore_ascii_case(&app_key);

                let info_row = |ui: &mut egui::Ui, label: &str, value: String, searched: bool| {
                    let label_color = if searched {
                        searchable_label_color
                    } else {
                        neutral_label_color
                    };
                    let value_color = if searched {
                        searchable_value_color
                    } else {
                        neutral_value_color
                    };
                    ui.label(egui::RichText::new(label).color(label_color).strong());
                    ui.label(egui::RichText::new(value).color(value_color).monospace());
                    ui.end_row();
                };

                ui.heading(
                    egui::RichText::new(&window_info.title)
                        .color(egui::Color32::WHITE)
                        .strong(),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("Window metadata, process details, and execution chain")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 170)),
                );
                ui.add_space(10.0);

                egui::Grid::new("window_info_grid")
                    .num_columns(2)
                    .spacing([14.0, 8.0])
                    .striped(true)
                    .show(ui, |ui| {
                        info_row(ui, "Title", window_info.title.clone(), true);
                        info_row(ui, "Raw window title", window_info.raw_title.clone(), false);
                        info_row(ui, "Application key", app_key.clone(), true);
                        info_row(ui, "Window ID", window_info.id.clone(), false);
                        info_row(ui, "Class", window_info.class.clone(), class_is_searched);
                        info_row(ui, "Window desktop file", desktop_file_path, false);
                        info_row(
                            ui,
                            "Window PID",
                            window_info
                                .pid
                                .map(|pid| pid.to_string())
                                .unwrap_or_else(|| "Unavailable".to_string()),
                            false,
                        );
                        info_row(
                            ui,
                            "Active process",
                            window_info
                                .active_process
                                .clone()
                                .unwrap_or_else(|| "Unavailable".to_string()),
                            true,
                        );
                        info_row(ui, "Window executable", exe_basename, true);
                        info_row(
                            ui,
                            "Window executable path",
                            window_info
                                .exe_path
                                .as_ref()
                                .map(|path| path.to_string_lossy().to_string())
                                .unwrap_or_else(|| "Unavailable".to_string()),
                            false,
                        );
                        info_row(
                            ui,
                            "Active process executable path",
                            active_process_exe_path
                                .as_ref()
                                .map(|path| path.to_string_lossy().to_string())
                                .unwrap_or_else(|| "Unavailable".to_string()),
                            false,
                        );
                        info_row(
                            ui,
                            "Active process desktop file",
                            active_process_desktop_file,
                            false,
                        );
                        info_row(ui, "Working directory", cwd_search_value, true);
                        info_row(
                            ui,
                            "Command summary",
                            window_info
                                .command_summary
                                .clone()
                                .unwrap_or_else(|| "Unavailable".to_string()),
                            true,
                        );
                        info_row(
                            ui,
                            "Command line",
                            window_info
                                .command_line
                                .clone()
                                .unwrap_or_else(|| "Unavailable".to_string()),
                            true,
                        );
                        info_row(
                            ui,
                            "Geometry",
                            window_info
                                .geometry
                                .map(|(x, y, width, height)| {
                                    format!("x={}, y={}, width={}, height={}", x, y, width, height)
                                })
                                .unwrap_or_else(|| "Unavailable".to_string()),
                            false,
                        );
                        info_row(
                            ui,
                            "Minimized",
                            window_info
                                .minimized
                                .map(|value| value.to_string())
                                .unwrap_or_else(|| "Unavailable".to_string()),
                            false,
                        );
                        info_row(
                            ui,
                            "Last activated",
                            format_activation_time(window_info.last_activated_at_ms),
                            false,
                        );
                        info_row(
                            ui,
                            "Activation sequence",
                            window_info.activation_sequence.to_string(),
                            false,
                        );
                    });

                ui.add_space(14.0);
                ui.separator();
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("Execution chain")
                        .color(egui::Color32::WHITE)
                        .strong(),
                );
                ui.add_space(6.0);
                for entry in &window_info.process_chain {
                    ui.group(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{} (pid {})", entry.name, entry.pid))
                                .color(egui::Color32::WHITE)
                                .strong(),
                        );
                        let path_text = entry
                            .exe_path
                            .as_ref()
                            .map(|path| path.to_string_lossy().to_string())
                            .unwrap_or_else(|| "Executable path unavailable".to_string());
                        ui.label(
                            egui::RichText::new(path_text)
                                .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 160)),
                        );
                    });
                    ui.add_space(6.0);
                }
            });

        if !is_open {
            self.process_chain_popup = None;
        }
    }

    fn show_app_info_popup(&mut self, ctx: &egui::Context) {
        let Some(app_info) = self.app_info_popup.clone() else {
            return;
        };

        if !ctx.embed_viewports() {
            show_deferred_info_popup(
                ctx,
                egui::ViewportId::from_hash_of("launcher_app_info_popup"),
                self.app_info_popup_data(&app_info),
                [720.0, 520.0],
                [480.0, 320.0],
                PopupEvent::CloseAppInfo,
                self.popup_event_sender.clone(),
            );
            return;
        }

        let mut is_open = true;
        egui::Window::new(format!("Application Info: {}", app_info.name))
            .id(egui::Id::new("launcher_app_info_popup"))
            .default_size([720.0, 520.0])
            .min_size([480.0, 320.0])
            .resizable(true)
            .collapsible(false)
            .order(egui::Order::Foreground)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(egui::Color32::from_rgba_unmultiplied(20, 20, 20, 240))
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20),
                    ))
                    .corner_radius(egui::CornerRadius::same(12)),
            )
            .open(&mut is_open)
            .show(ctx, |ui| {
                let searchable_label_color = egui::Color32::from_rgb(214, 184, 86);
                let searchable_value_color = egui::Color32::from_rgb(255, 236, 170);
                let neutral_label_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 170);
                let neutral_value_color = egui::Color32::WHITE;
                let desktop_stem = app_info
                    .desktop_file_path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("Unavailable")
                    .to_string();
                let executable_basename =
                    command_basename(&app_info.exec).unwrap_or_else(|| "Unavailable".to_string());
                let executable_path = executable_path_from_exec(&app_info.exec);
                let executable_path_display = executable_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Unavailable".to_string());
                let executable_exists = executable_path.as_ref().is_some_and(|path| path.is_file());
                let desktop_file_exists = app_info.desktop_file_path.exists();
                let icon_path = app_info
                    .icon_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Unavailable".to_string());
                let is_pinned = self.pinned_apps.contains(&app_info.desktop_file_path);

                let info_row = |ui: &mut egui::Ui, label: &str, value: String, searched: bool| {
                    let label_color = if searched {
                        searchable_label_color
                    } else {
                        neutral_label_color
                    };
                    let value_color = if searched {
                        searchable_value_color
                    } else {
                        neutral_value_color
                    };
                    ui.label(egui::RichText::new(label).color(label_color).strong());
                    ui.label(egui::RichText::new(value).color(value_color).monospace());
                    ui.end_row();
                };

                ui.heading(
                    egui::RichText::new(&app_info.name)
                        .color(egui::Color32::WHITE)
                        .strong(),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("Desktop-entry and application search metadata")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 170)),
                );
                ui.add_space(10.0);

                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("app_info_grid")
                        .num_columns(2)
                        .spacing([14.0, 8.0])
                        .striped(true)
                        .show(ui, |ui| {
                            info_row(ui, "Name", app_info.name.clone(), true);
                            info_row(ui, "Executable basename", executable_basename, true);
                            info_row(ui, "Desktop file stem", desktop_stem, true);
                            info_row(
                                ui,
                                "Comment",
                                app_info
                                    .comment
                                    .clone()
                                    .unwrap_or_else(|| "Unavailable".to_string()),
                                true,
                            );
                            info_row(ui, "Cleaned command", clean_exec_cmd(&app_info.exec), true);
                            info_row(ui, "Executable path", executable_path_display, false);
                            info_row(
                                ui,
                                "Executable exists",
                                executable_exists.to_string(),
                                false,
                            );
                            info_row(ui, "Raw Exec", app_info.exec.clone(), false);
                            info_row(
                                ui,
                                "Desktop file",
                                app_info.desktop_file_path.to_string_lossy().to_string(),
                                false,
                            );
                            info_row(
                                ui,
                                "Desktop file exists",
                                desktop_file_exists.to_string(),
                                false,
                            );
                            info_row(ui, "Icon path", icon_path, false);
                            info_row(ui, "Pinned", is_pinned.to_string(), false);
                            info_row(
                                ui,
                                "System settings module",
                                app_info.is_settings_module.to_string(),
                                false,
                            );
                        });
                });
            });

        if !is_open {
            self.app_info_popup = None;
        }
    }

    fn update_cached_windows_without_rerank(&mut self, updates: &[(WindowInfo, WindowInfo)]) {
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

    fn seed_window_icon_cache(&mut self) {
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

    fn schedule_window_search_refresh(&mut self) {
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

    fn flush_pending_window_search_refresh(&mut self) -> bool {
        if self.pending_window_search_refresh_at.take().is_none() {
            return false;
        }

        self.windows_generation = self.windows_generation.wrapping_add(1);
        true
    }

    fn apply_window_snapshot(&mut self, new_windows: Vec<WindowInfo>) {
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
                merged.push(new_window);
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
        let cache_updates = if search_changed {
            Vec::new()
        } else {
            merged
                .iter()
                .filter_map(|window| {
                    old_by_id
                        .get(window.id.as_str())
                        .map(|old| ((*old).clone(), window.clone()))
                })
                .collect()
        };
        self.windows = merged;
        self.seed_window_icon_cache();
        if search_changed {
            self.schedule_window_search_refresh();
        } else {
            self.update_cached_windows_without_rerank(&cache_updates);
        }
    }

    fn apply_window_feed_events(&mut self, events: Vec<WindowFeedEvent>) {
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
                            search_changed |= !window_search_metadata_equal(&old_window, &window);
                            *existing = window.clone();
                            cache_updates.push((old_window, window));
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
            if search_changed {
                self.schedule_window_search_refresh();
            } else {
                self.update_cached_windows_without_rerank(&cache_updates);
            }
            self.refresh_window_audio_cache();
        }
        if needs_terminal_metadata_refresh {
            self.start_terminal_metadata_refresh();
        }
    }

    fn prune_stale_windows(&mut self) {
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

    fn refresh_window_audio_cache(&mut self) -> bool {
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

    fn has_any_active_audio(&self) -> bool {
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

    fn refresh_windows(&mut self) {
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

    fn start_background_window_enrichment(&mut self) {
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

    fn schedule_window_reconciliation(&mut self, delay: Duration) {
        self.next_window_reconciliation_at = Some(Instant::now() + delay);
    }

    fn start_window_reconciliation(&mut self) {
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

    fn apply_window_reconciliation(&mut self, discovered: Vec<WindowInfo>) {
        for window in &discovered {
            self.missing_window_counts.remove(&window.id);
        }
        let (changed, search_changed, cache_updates) =
            merge_reconciled_windows(&mut self.windows, discovered);
        if !changed {
            return;
        }

        self.seed_window_icon_cache();
        if search_changed {
            self.schedule_window_search_refresh();
        } else {
            self.update_cached_windows_without_rerank(&cache_updates);
        }
        self.refresh_window_audio_cache();
    }

    fn start_background_app_load(&mut self) {
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

    fn refresh_apps(&mut self) {
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

    fn start_window_polling_thread(&mut self, ctx: &egui::Context) {
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

    fn launch_app_and_exit(&self, app: &AppInfo, ctx: &egui::Context) {
        self.rapid_polling
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if is_dolphin_app(app) {
            launch_dolphin_app();
        } else if !launch_desktop_entry(&app.desktop_file_path) {
            launch_app(&app.exec);
        }
        ctx.request_repaint();
    }

    fn open_window_for_app_and_exit(&self, app: &AppInfo, ctx: &egui::Context) {
        let Some(window_id) = self
            .windows
            .iter()
            .find(|window| {
                self.desktop_file_path_for_window(window).as_ref() == Some(&app.desktop_file_path)
            })
            .map(|window| window.id.clone())
        else {
            return;
        };

        self.activate_and_exit(window_id, ctx);
    }

    fn open_or_launch_app_and_exit(&self, app: &AppInfo, ctx: &egui::Context) {
        if self.windows.iter().any(|window| {
            self.desktop_file_path_for_window(window).as_ref() == Some(&app.desktop_file_path)
        }) {
            self.open_window_for_app_and_exit(app, ctx);
        } else {
            self.launch_app_and_exit(app, ctx);
        }
    }

    fn find_app_for_window<'a>(&'a self, win: &WindowInfo) -> Option<&'a AppInfo> {
        let mut window_keys = Vec::new();

        let class = win.class.trim();
        if !class.is_empty() {
            window_keys.push(normalize_app_match_key(class));
            if let Some(last_segment) = class.rsplit('.').next() {
                window_keys.push(normalize_app_match_key(last_segment));
            }
        }

        if let Some(path) = &win.exe_path {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                window_keys.push(normalize_app_match_key(name));
            }
        }

        if let Some(proc_name) = &win.active_process {
            window_keys.push(normalize_app_match_key(proc_name));
        }

        window_keys.retain(|key| !key.is_empty());
        if window_keys.is_empty() {
            return None;
        }

        self.apps
            .iter()
            .filter_map(|app| best_app_match_score(&window_keys, app).map(|score| (app, score)))
            .min_by_key(|(app, score)| (*score, app.is_settings_module))
            .map(|(app, _)| app)
    }

    fn desktop_file_path_for_window(&self, win: &WindowInfo) -> Option<PathBuf> {
        if let Some(path) = win
            .desktop_file_name
            .as_deref()
            .and_then(resolve_desktop_file_path)
        {
            return Some(path);
        }

        self.find_app_for_window(win)
            .map(|app| app.desktop_file_path.clone())
    }

    fn desktop_file_path_for_process(&self, process_name: &str) -> Option<PathBuf> {
        let process_key = normalize_app_match_key(process_name);
        if process_key.is_empty() {
            return None;
        }

        let matching_app = self
            .apps
            .iter()
            .filter_map(|app| {
                let exec_matches = command_basename(&app.exec)
                    .is_some_and(|name| normalize_app_match_key(&name) == process_key);
                let stem_matches = app
                    .desktop_file_path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| normalize_app_match_key(stem) == process_key);
                let name_matches = normalize_app_match_key(&app.name) == process_key;
                let score = if exec_matches {
                    0
                } else if stem_matches {
                    1
                } else if name_matches {
                    2
                } else {
                    return None;
                };
                Some((score, app))
            })
            .min_by_key(|(score, app)| (*score, app.is_settings_module))
            .map(|(_, app)| app.desktop_file_path.clone());

        matching_app.or_else(|| {
            let executable_name = Path::new(process_name)
                .file_name()
                .and_then(|name| name.to_str())?;
            resolve_desktop_file_path(&format!("{executable_name}.desktop"))
        })
    }

    fn launch_window_app_and_exit(&self, win: &WindowInfo, ctx: &egui::Context) {
        self.rapid_polling
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if is_terminal_class(&win.class.to_lowercase()) {
            launch_terminal_window();
            ctx.request_repaint();
            return;
        }

        if let Some(desktop_file_path) = self.desktop_file_path_for_window(win) {
            if launch_desktop_entry(&desktop_file_path) {
                ctx.request_repaint();
                return;
            }
        }

        if let Some(exe_path) = &win.exe_path {
            let exe = exe_path.clone();
            std::thread::spawn(move || {
                let mut cmd = Command::new(exe);
                cmd.env_remove("PYTHONPATH");
                cmd.env_remove("PYTHONHOME");
                cmd.env_remove("VIRTUAL_ENV");
                cmd.env_remove("UV_ACTIVE");
                let _ = cmd.spawn();
            });
            ctx.request_repaint();
            return;
        }

        if let Some(proc_name) = &win.active_process {
            launch_app(proc_name);
            ctx.request_repaint();
            return;
        }

        if let Some(app) = self.find_app_for_window(win) {
            self.launch_app_and_exit(app, ctx);
        }
    }

    fn clone_window_and_exit(&self, win: &WindowInfo, ctx: &egui::Context) {
        self.rapid_polling
            .store(true, std::sync::atomic::Ordering::SeqCst);

        if is_terminal_class(&win.class.to_lowercase()) {
            let cwd = win
                .cwd_path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string());
            launch_fish_terminal(
                cwd,
                clone_terminal_command_for_window(win),
                Some(source_terminal_title_for_clone(win)),
            );
            ctx.request_repaint();
            return;
        }

        if is_pcmanfm_window(win) && clone_pcmanfm_window(win) {
            ctx.request_repaint();
            return;
        }

        if is_dolphin_window(win) && clone_dolphin_window(win) {
            ctx.request_repaint();
            return;
        }

        if is_chrome_like_window(win) && clone_chrome_window(win) {
            ctx.request_repaint();
            return;
        }

        self.launch_window_app_and_exit(win, ctx);
    }

    fn activate_and_exit(&self, id: String, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        if let Some(ref kpath) = self.kdotool_path {
            let kpath = kpath.clone();
            let id_clone = id.clone();
            std::thread::spawn(move || {
                // Query geometry first
                let geom = get_window_geometry(&kpath, &id_clone);

                // Activate the window and raise it to make sure it comes to the top
                let _ = Command::new(&kpath)
                    .args(["windowactivate", &id_clone, "windowraise", &id_clone])
                    .status();

                // If we got geometry, spawn the border overlay process
                if let Some((x, y, w, h)) = geom {
                    if let Ok(current_exe) = std::env::current_exe() {
                        let _ = Command::new(current_exe)
                            .args([
                                "--draw-border",
                                &x.to_string(),
                                &y.to_string(),
                                &w.to_string(),
                                &h.to_string(),
                                &id_clone,
                            ])
                            .spawn();
                    }
                }
            });
        }
        ctx.request_repaint();
    }

    fn close_window_and_exit(&self, id: String, ctx: &egui::Context) {
        self.rapid_polling
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(ref kpath) = self.kdotool_path {
            let kpath = kpath.clone();
            std::thread::spawn(move || {
                let _ = Command::new(&kpath).args(["windowclose", &id]).status();
            });
        }
        ctx.request_repaint();
    }
}

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
                                    "Order windows by last activation, oldest first",
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
pub(crate) struct BorderOverlay {
    pub(crate) start_time: Instant,
    pub(crate) duration: std::time::Duration,
    pub(crate) local_x: f32,
    pub(crate) local_y: f32,
    pub(crate) tw: f32,
    pub(crate) th: f32,
}

impl eframe::App for BorderOverlay {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.start_time.elapsed() >= self.duration {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        ctx.request_repaint();

        let panel_frame = egui::Frame {
            fill: egui::Color32::TRANSPARENT,
            ..Default::default()
        };

        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                let elapsed = self.start_time.elapsed().as_secs_f32();
                let total_secs = self.duration.as_secs_f32();
                let progress = (elapsed / total_secs).clamp(0.0, 1.0);
                let alpha = ((1.0 - progress) * 255.0) as u8;

                let rect = egui::Rect::from_min_size(
                    egui::pos2(self.local_x, self.local_y),
                    egui::vec2(self.tw, self.th),
                );

                ui.painter().rect_stroke(
                    rect,
                    egui::CornerRadius::same(6),
                    egui::Stroke::new(
                        3.0,
                        egui::Color32::from_rgba_unmultiplied(235, 40, 40, alpha),
                    ),
                    egui::StrokeKind::Inside,
                );
            });
    }
}

pub(crate) struct MonitorInfo {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
    pub(crate) scale: f32,
}

pub(crate) fn get_monitors() -> Vec<MonitorInfo> {
    let mut monitors = Vec::new();
    let output = match Command::new("kscreen-doctor").arg("-j").output() {
        Ok(o) => o,
        Err(_) => return monitors,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);

    let blocks: Vec<&str> = stdout.split("\"name\":").collect();
    if blocks.len() <= 1 {
        return monitors;
    }

    for block in &blocks[1..] {
        let mut x = None;
        let mut y = None;
        let mut width = None;
        let mut height = None;
        let mut scale = Some(1.0);

        if let Some(pos_idx) = block.find("\"pos\":") {
            let pos_str = &block[pos_idx..];
            if let Some(brace_open) = pos_str.find('{') {
                if let Some(brace_close) = pos_str.find('}') {
                    let pos_content = &pos_str[brace_open + 1..brace_close];
                    for line in pos_content.lines() {
                        let line = line.trim();
                        if line.starts_with("\"x\":") {
                            x = line
                                .strip_prefix("\"x\":")
                                .map(|s| s.trim_matches(|c| c == ',' || c == ' ' || c == '\n'))
                                .and_then(|s| s.parse::<f32>().ok());
                        } else if line.starts_with("\"y\":") {
                            y = line
                                .strip_prefix("\"y\":")
                                .map(|s| s.trim_matches(|c| c == ',' || c == ' ' || c == '\n'))
                                .and_then(|s| s.parse::<f32>().ok());
                        }
                    }
                }
            }
        }

        if let Some(size_idx) = block.find("\"size\":") {
            let size_str = &block[size_idx..];
            if let Some(brace_open) = size_str.find('{') {
                if let Some(brace_close) = size_str.find('}') {
                    let size_content = &size_str[brace_open + 1..brace_close];
                    for line in size_content.lines() {
                        let line = line.trim();
                        if line.starts_with("\"width\":") {
                            width = line
                                .strip_prefix("\"width\":")
                                .map(|s| s.trim_matches(|c| c == ',' || c == ' ' || c == '\n'))
                                .and_then(|s| s.parse::<f32>().ok());
                        } else if line.starts_with("\"height\":") {
                            height = line
                                .strip_prefix("\"height\":")
                                .map(|s| s.trim_matches(|c| c == ',' || c == ' ' || c == '\n'))
                                .and_then(|s| s.parse::<f32>().ok());
                        }
                    }
                }
            }
        }

        for line in block.lines() {
            let line = line.trim();
            if line.starts_with("\"scale\":") {
                if let Some(s_val) = line
                    .strip_prefix("\"scale\":")
                    .map(|s| s.trim_matches(|c| c == ',' || c == ' ' || c == '\n'))
                    .and_then(|s| s.parse::<f32>().ok())
                {
                    scale = Some(s_val);
                }
            }
        }

        if let (Some(x), Some(y), Some(width), Some(height), Some(scale)) =
            (x, y, width, height, scale)
        {
            monitors.push(MonitorInfo {
                x,
                y,
                width,
                height,
                scale,
            });
        }
    }
    monitors
}
#[cfg(test)]
mod tests {
    use super::*;
    use fuzzy_rank::metadata::SearchField;

    fn test_window_info(title: &str) -> WindowInfo {
        WindowInfo {
            id: "test-window".to_string(),
            title: title.to_string(),
            raw_title: title.to_string(),
            class: "xfce4-terminal".to_string(),
            desktop_file_name: Some("xfce4-terminal.desktop".to_string()),
            minimized: Some(false),
            demands_attention: false,
            icon_path: None,
            active_process: Some("codex".to_string()),
            exe_path: Some(PathBuf::from("/usr/bin/xfce4-terminal")),
            cwd_path: Some(PathBuf::from("/home/lewis/Dev/applicationlauncher")),
            command_line: Some("codex resume".to_string()),
            command_summary: Some("codex resume".to_string()),
            geometry: Some((0, 0, 800, 600)),
            process_chain: Vec::new(),
            pid: Some(1234),
            last_activated_at_ms: Some(0),
            activation_sequence: 1,
        }
    }

    fn test_kwin_payload(title: &str, demands_attention: bool) -> KWinWindowPayload {
        KWinWindowPayload {
            id: "test-window".to_string(),
            title: title.to_string(),
            class: "xfce4-terminal".to_string(),
            pid: 1234,
            desktop_file_name: "xfce4-terminal.desktop".to_string(),
            x: 0,
            y: 0,
            width: 800,
            height: 600,
            minimized: true,
            demands_attention,
            last_activated_at_ms: Some(0),
            activation_sequence: 1,
        }
    }

    #[test]
    fn terminal_attention_sends_every_due_window_once() {
        let now = Instant::now();
        let mut deadlines = HashMap::new();
        let mut handled = HashSet::new();
        let eligible_ids = (0..5)
            .map(|index| format!("terminal-{index}"))
            .collect::<HashSet<_>>();

        let (due, next) = update_terminal_attention_schedule(
            true,
            &eligible_ids,
            &mut deadlines,
            &mut handled,
            now,
        );
        assert!(due.is_empty());
        assert_eq!(next, Some(now + Duration::from_secs(5)));

        let (due, next) = update_terminal_attention_schedule(
            true,
            &eligible_ids,
            &mut deadlines,
            &mut handled,
            now + Duration::from_secs(5),
        );
        assert_eq!(due.len(), 5);
        assert!(next.is_none());
        assert!(handled.is_empty());
        handled.extend(due);

        let (due, _) = update_terminal_attention_schedule(
            true,
            &eligible_ids,
            &mut deadlines,
            &mut handled,
            now + Duration::from_secs(10),
        );
        assert!(due.is_empty());
    }

    #[test]
    fn terminal_attention_clear_cancels_and_rearms_delay() {
        let now = Instant::now();
        let mut deadlines = HashMap::new();
        let mut handled = HashSet::new();
        let terminal_id = "terminal".to_string();
        let eligible_ids = HashSet::from([terminal_id.clone()]);

        update_terminal_attention_schedule(true, &eligible_ids, &mut deadlines, &mut handled, now);
        let no_eligible_ids = HashSet::new();
        let (due, next) = update_terminal_attention_schedule(
            true,
            &no_eligible_ids,
            &mut deadlines,
            &mut handled,
            now + Duration::from_secs(4),
        );
        assert!(due.is_empty());
        assert!(next.is_none());

        update_terminal_attention_schedule(
            true,
            &eligible_ids,
            &mut deadlines,
            &mut handled,
            now + Duration::from_secs(10),
        );
        let (due, _) = update_terminal_attention_schedule(
            true,
            &eligible_ids,
            &mut deadlines,
            &mut handled,
            now + Duration::from_secs(15),
        );
        assert_eq!(due.len(), 1);
        assert_eq!(due[0], terminal_id);
    }

    #[test]
    fn terminal_attention_feed_preserves_back_to_back_prompt_generation() {
        let id = "test-window".to_string();
        let mut windows = HashMap::from([(
            id.clone(),
            test_kwin_payload("codex - [ ! ] Action Required - Terminal", true),
        )]);
        let mut feed_last_seen = HashMap::new();
        let mut deadlines = HashMap::from([(id.clone(), Instant::now())]);
        let mut handled = HashSet::from([id.clone()]);
        let mut exhausted = HashSet::from([id.clone()]);
        let mut retry_attempts = HashMap::from([(id.clone(), 2)]);
        let mut generations = HashMap::from([(id.clone(), 4)]);

        assert!(apply_terminal_attention_feed_upsert(
            // The terminal title can lag behind KWin's attention clear when the
            // next approval appears immediately.
            test_kwin_payload("codex - [ ! ] Action Required - Terminal", false),
            &mut windows,
            &mut feed_last_seen,
            &mut deadlines,
            &mut handled,
            &mut exhausted,
            &mut retry_attempts,
            &mut generations,
        ));
        assert!(!apply_terminal_attention_feed_upsert(
            test_kwin_payload("codex - [ . ] Action Required - Terminal", true),
            &mut windows,
            &mut feed_last_seen,
            &mut deadlines,
            &mut handled,
            &mut exhausted,
            &mut retry_attempts,
            &mut generations,
        ));

        assert!(deadlines.is_empty());
        assert!(handled.is_empty());
        assert!(exhausted.is_empty());
        assert!(retry_attempts.is_empty());
        assert!(!terminal_attention_attempt_is_current(&id, 4, &generations));
        assert!(terminal_attention_attempt_is_current(&id, 5, &generations));
        assert!(terminal_attention_payload_requires_attention(
            windows.get(&id).unwrap(),
            true,
        ));
    }

    #[test]
    fn terminal_attention_success_rechecks_a_continuous_back_to_back_prompt() {
        let id = "test-window".to_string();
        let payload = test_kwin_payload("codex - [ ! ] Action Required - Terminal", false);
        let now = Instant::now();
        let mut deadlines = HashMap::new();
        let mut handled = HashSet::from([id.clone()]);

        record_terminal_attention_success(&id, Some(&payload), &mut deadlines, &mut handled, now);

        assert!(!handled.contains(&id));
        assert_eq!(
            deadlines.get(&id),
            Some(&(now + Duration::from_secs(AUTO_SEND_ENTER_DELAY_SECS)))
        );

        let cleared = test_kwin_payload("codex - project - Terminal", false);
        record_terminal_attention_success(&id, Some(&cleared), &mut deadlines, &mut handled, now);
        assert!(handled.contains(&id));
        assert!(!deadlines.contains_key(&id));
    }

    #[test]
    fn window_feed_reset_discards_stale_pre_snapshot_windows() {
        let mut stale = test_kwin_payload("<EMPTY> — CopyQ", false);
        stale.id = "stale-copyq".to_string();
        let mut current = test_kwin_payload("first title", false);
        current.id = "current-copyq".to_string();
        let mut updated = current.clone();
        updated.title = "http://127.0.0.1/word/angstrom — CopyQ".to_string();

        let events = coalesce_window_feed_events(vec![
            WindowFeedEvent::Upsert(stale),
            WindowFeedEvent::Reset,
            WindowFeedEvent::Upsert(current),
            WindowFeedEvent::Upsert(updated.clone()),
        ]);

        assert_eq!(events.len(), 2);
        assert!(matches!(events.first(), Some(WindowFeedEvent::Reset)));
        assert!(matches!(
            events.get(1),
            Some(WindowFeedEvent::Upsert(payload))
                if payload.id == updated.id && payload.title == updated.title
        ));
    }

    #[test]
    fn window_feed_coalesces_to_the_latest_full_snapshot() {
        let mut first = test_kwin_payload("first", false);
        first.id = "first".into();
        let mut latest = test_kwin_payload("latest", false);
        latest.id = "latest".into();

        let events = coalesce_window_feed_events(vec![
            WindowFeedEvent::Snapshot(vec![first]),
            WindowFeedEvent::Snapshot(vec![latest.clone()]),
        ]);

        assert_eq!(events.len(), 2);
        assert!(matches!(events.first(), Some(WindowFeedEvent::Reset)));
        assert!(matches!(
            events.get(1),
            Some(WindowFeedEvent::Upsert(payload))
                if payload.id == latest.id && payload.title == latest.title
        ));
    }

    #[test]
    fn terminal_attention_relaunch_rearms_an_existing_prompt() {
        let id = "test-window".to_string();
        let windows = HashMap::from([(
            id.clone(),
            test_kwin_payload("codex - [ ! ] Action Required - Terminal", true),
        )]);
        let mut deadlines = HashMap::from([(id.clone(), Instant::now())]);
        let mut handled = HashSet::from([id.clone()]);
        let mut exhausted = HashSet::from([id.clone()]);
        let mut retry_attempts = HashMap::from([(id.clone(), 2)]);
        let cancellation = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let in_flight = HashMap::from([(id.clone(), Arc::clone(&cancellation))]);
        let mut generations = HashMap::from([(id.clone(), 7)]);

        rearm_terminal_attention_automation(
            &windows,
            &mut deadlines,
            &mut handled,
            &mut exhausted,
            &mut retry_attempts,
            &in_flight,
            &mut generations,
        );

        assert!(deadlines.is_empty());
        assert!(handled.is_empty());
        assert!(exhausted.is_empty());
        assert!(retry_attempts.is_empty());
        assert!(cancellation.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(generations.get(&id), Some(&8));

        let now = Instant::now();
        let eligible = HashSet::from([id.clone()]);
        let (due, next) =
            update_terminal_attention_schedule(true, &eligible, &mut deadlines, &mut handled, now);
        assert!(due.is_empty());
        assert_eq!(next, Some(now + Duration::from_secs(5)));
    }

    #[test]
    fn terminal_attention_clear_cancels_pending_dbus_send() {
        let cancellation = std::sync::atomic::AtomicBool::new(false);
        assert!(!terminal_attention_send_is_cancelled(Some(&cancellation)));

        cancellation.store(true, std::sync::atomic::Ordering::Release);

        assert!(terminal_attention_send_is_cancelled(Some(&cancellation)));
        assert!(!terminal_attention_send_is_cancelled(None));
    }

    #[test]
    fn terminal_attention_worker_ignores_non_terminal_and_idle_payloads() {
        let payload =
            |id: &str, class: &str, title: &str, demands_attention: bool| KWinWindowPayload {
                id: id.to_string(),
                title: title.to_string(),
                class: class.to_string(),
                pid: 1,
                desktop_file_name: String::new(),
                x: 0,
                y: 0,
                width: 800,
                height: 600,
                minimized: true,
                demands_attention,
                last_activated_at_ms: None,
                activation_sequence: 0,
            };

        let feed_attention = payload(
            "terminal",
            "xfce4-terminal",
            "codex - project - Terminal",
            true,
        );
        assert!(terminal_attention_payload_requires_attention(
            &feed_attention,
            true,
        ));
        assert!(!terminal_attention_payload_requires_attention(
            &feed_attention,
            false,
        ));
        assert!(terminal_attention_payload_requires_attention(
            &payload(
                "terminal-title",
                "xfce4-terminal",
                "[ ! ] Action Required - Terminal",
                false,
            ),
            false,
        ));
        assert!(terminal_attention_payload_requires_attention(
            &payload(
                "terminal-title-fallback",
                "xfce4-terminal",
                "[ ! ] Action Required - Terminal",
                false,
            ),
            true,
        ));
        assert!(!terminal_attention_payload_requires_attention(
            &payload(
                "idle-terminal",
                "xfce4-terminal",
                "fish - ~ - Terminal",
                false,
            ),
            true,
        ));
        assert!(!terminal_attention_payload_requires_attention(
            &payload("browser", "firefox", "Action Required", true),
            true,
        ));
    }

    #[test]
    fn terminal_attention_retries_use_bounded_exponential_backoff() {
        assert_eq!(
            terminal_attention_retry_delay(1),
            Duration::from_millis(750)
        );
        assert_eq!(
            terminal_attention_retry_delay(2),
            Duration::from_millis(1500)
        );
        assert_eq!(
            terminal_attention_retry_delay(3),
            Duration::from_millis(3000)
        );
    }

    fn terminal_record(
        tab_uuid: &str,
        window_uuid: &str,
        title: &str,
        pty: &str,
    ) -> TerminalDbusRecord {
        TerminalDbusRecord {
            window_uuid: window_uuid.to_string(),
            tab_uuid: tab_uuid.to_string(),
            active: true,
            window_title: title.to_string(),
            working_directory: "/home/lewis/Dev/applicationlauncher".to_string(),
            child_pid: 0,
            foreground_pid: 0,
            foreground_pgid: 0,
            pty: pty.to_string(),
        }
    }

    #[test]
    fn terminal_tab_matching_prefers_unique_pty_over_duplicate_titles() {
        let identity = TerminalWindowIdentity {
            normalized_title: normalize_window_sort_title("⠇ xfce4-terminal - Terminal"),
            cwd: Some(PathBuf::from("/home/lewis/Dev/applicationlauncher")),
            ptys: HashSet::from(["/dev/pts/23".to_string()]),
            ..Default::default()
        };
        let records = vec![
            terminal_record(
                "tab-a",
                "window-a",
                "⠋ xfce4-terminal - Terminal",
                "/dev/pts/22",
            ),
            terminal_record(
                "tab-b",
                "window-b",
                "⠴ xfce4-terminal - Terminal",
                "/dev/pts/23",
            ),
        ];

        assert_eq!(select_terminal_tab(&identity, &records).unwrap(), "tab-b");
    }

    #[test]
    fn terminal_tab_matching_allows_a_unique_raw_window_title() {
        let identity = TerminalWindowIdentity {
            normalized_title: normalize_window_sort_title("codex - project - Terminal"),
            ..Default::default()
        };
        let records = vec![
            terminal_record("tab-a", "window-a", "fish - ~ - Terminal", "/dev/pts/22"),
            terminal_record(
                "tab-b",
                "window-b",
                "codex - project - Terminal",
                "/dev/pts/23",
            ),
        ];

        assert_eq!(select_terminal_tab(&identity, &records).unwrap(), "tab-b");
    }

    #[test]
    fn terminal_tab_matching_rejects_ambiguous_titles_without_process_identity() {
        let identity = TerminalWindowIdentity {
            normalized_title: normalize_window_sort_title("fish - ~ - Terminal"),
            ..Default::default()
        };
        let records = vec![
            terminal_record("tab-a", "window-a", "fish - ~ - Terminal", "/dev/pts/22"),
            terminal_record("tab-b", "window-b", "fish - ~ - Terminal", "/dev/pts/23"),
        ];

        assert!(select_terminal_tab(&identity, &records).is_err());
    }

    #[test]
    fn terminal_metadata_matches_a_unique_raw_window_title() {
        let mut fish = terminal_record("tab-fish", "window-fish", "~ - Terminal", "/dev/pts/19");
        fish.child_pid = 4152349;
        fish.foreground_pid = 4152349;
        let mut chatgpt = terminal_record(
            "tab-chatgpt",
            "window-chatgpt",
            "npm run build - Terminal",
            "/dev/pts/17",
        );
        chatgpt.child_pid = 4096357;
        chatgpt.foreground_pid = 4097817;
        let records = vec![fish, chatgpt];

        let matched = terminal_record_for_window_title("~ - Terminal", &records).unwrap();
        assert_eq!(matched.tab_uuid, "tab-fish");
        assert_eq!(matched.foreground_pid, 4152349);
    }

    #[test]
    fn terminal_metadata_matches_blank_dynamic_title_spacing_variants() {
        let mut nvtop = terminal_record("tab-nvtop", "window-nvtop", " - Terminal", "/dev/pts/31");
        nvtop.child_pid = 2816861;
        nvtop.foreground_pid = 2816861;

        assert_eq!(
            normalize_window_sort_title("- Terminal"),
            normalize_window_sort_title(&nvtop.window_title)
        );
        let records = [nvtop];
        let matched = terminal_record_for_window_title("- Terminal", &records).unwrap();
        assert_eq!(matched.tab_uuid, "tab-nvtop");
        assert_eq!(matched.foreground_pid, 2816861);
        assert_eq!(
            terminal_display_title(
                "- Terminal",
                "nvtop",
                Some("nvtop"),
                Some("~/Dev/applicationlauncher"),
                None,
            ),
            "nvtop - ~/Dev/applicationlauncher - Terminal"
        );
    }

    #[test]
    fn terminal_metadata_does_not_guess_between_duplicate_titles() {
        let mut first = terminal_record("tab-a", "window-a", "~ - Terminal", "/dev/pts/19");
        first.child_pid = 100;
        let mut second = terminal_record("tab-b", "window-b", "~ - Terminal", "/dev/pts/20");
        second.child_pid = 200;

        assert!(terminal_record_for_window_title("~ - Terminal", &[first, second]).is_none());
    }

    #[test]
    fn terminal_metadata_identifies_its_own_shared_server_process() {
        let mut record = terminal_record("tab-a", "window-a", "~ - Terminal", "/dev/pts/19");
        record.child_pid = 100;
        record.foreground_pid = 101;
        let parents = HashMap::from([(101, 100), (100, 10), (10, 1)]);

        assert!(terminal_server_has_dbus_records(
            10,
            &[record.clone()],
            &parents
        ));
        assert!(!terminal_server_has_dbus_records(20, &[record], &parents));
    }

    #[test]
    #[ignore = "requires the patched XFCE4 Terminal D-Bus service"]
    fn live_terminal_dbus_schema_deserializes() {
        let connection = zbus::blocking::Connection::session().unwrap();
        let proxy = zbus::blocking::Proxy::new(
            &connection,
            TERMINAL_DBUS_SERVICE,
            TERMINAL_DBUS_PATH,
            TERMINAL_DBUS_INTERFACE,
        )
        .unwrap();
        let raw_records: Vec<HashMap<String, zbus::zvariant::OwnedValue>> =
            proxy.call("ListTerminals", &()).unwrap();
        let records = parse_terminal_dbus_records(raw_records);

        assert!(!records.is_empty());
        assert!(records.iter().all(|record| !record.tab_uuid.is_empty()));
        assert!(records.iter().all(|record| !record.pty.is_empty()));
    }

    #[test]
    #[ignore = "requires a live KWin session and kdotool"]
    fn live_fast_window_snapshot_reports_discovered_windows() {
        let kdotool = get_kdotool_path();
        let probe = Command::new(&kdotool)
            .args(["search", "--title", ""])
            .output()
            .expect("kdotool search should execute");
        eprintln!(
            "kdotool={} status={} ids={} stderr={}",
            kdotool.display(),
            probe.status,
            String::from_utf8_lossy(&probe.stdout).lines().count(),
            String::from_utf8_lossy(&probe.stderr).trim()
        );
        let windows = get_open_windows_fast(&kdotool, "breeze-dark")
            .expect("the live KWin window snapshot should be available");

        eprintln!("discovered {} application windows", windows.len());
        for window in &windows {
            eprintln!(
                "{}\t{}\t{}\tactive={:?}\texe={:?}\ticon={:?}",
                window.id,
                window.class,
                window.raw_title,
                window.active_process,
                window.exe_path,
                window.icon_path
            );
        }
        assert!(!windows.is_empty());
    }

    #[test]
    #[ignore = "requires a live KWin session, kdotool, and an XFCE4 Terminal window"]
    fn live_terminal_attention_reconciliation_recovers_kwin_captions() {
        let kdotool = get_kdotool_path();
        let output = Command::new(kdotool)
            .args(["search", "--classname", "xfce4-terminal"])
            .output()
            .expect("kdotool should find XFCE4 Terminal windows");
        assert!(output.status.success());
        let terminal_windows = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(|id| {
                let mut window = test_window_info("unknown - Terminal");
                window.id = id.to_string();
                (window.id.clone(), window)
            })
            .collect::<HashMap<_, _>>();
        assert!(!terminal_windows.is_empty());

        let expected_ids = terminal_windows.keys().cloned().collect::<HashSet<_>>();
        let shared_windows = Arc::new(std::sync::Mutex::new(terminal_windows));
        let mut payloads = HashMap::new();
        reconcile_terminal_attention_windows_from_kwin(&mut payloads, &shared_windows).unwrap();

        assert!(expected_ids.iter().all(|id| payloads.contains_key(id)));
        assert!(payloads.values().all(|payload| {
            is_xfce4_terminal_class(&payload.class) && !payload.title.trim().is_empty()
        }));
    }

    #[test]
    #[ignore = "requires a live Tor Browser window, KWin, and kdotool"]
    fn live_tor_browser_window_uses_tor_icon() {
        let kdotool = get_kdotool_path();
        let windows = get_open_windows_fast(&kdotool, "breeze-dark").unwrap();
        let tor_window = windows
            .iter()
            .find(|window| is_tor_browser_identity(&window.class))
            .expect("a live Tor Browser window should be open");
        let icon = tor_window
            .icon_path
            .as_ref()
            .expect("the Tor Browser window should resolve a dedicated icon");
        let icon_name = icon.to_string_lossy().to_lowercase();

        assert!(icon_name.contains("tor"));
        assert!(!icon_name.contains("firefox"));
    }

    #[test]
    #[ignore = "requires a live idle XFCE4 Terminal window, KWin, and the patched D-Bus API"]
    fn live_idle_terminal_does_not_inherit_electron_process() {
        let kdotool = get_kdotool_path();
        let windows = get_open_windows_fast(&kdotool, "breeze-dark").unwrap();
        let idle_terminals = windows
            .iter()
            .filter(|window| {
                is_terminal_class(&window.class.to_lowercase())
                    && normalize_window_sort_title(&window.raw_title)
                        == normalize_window_sort_title("~ - Terminal")
            })
            .collect::<Vec<_>>();

        assert!(!idle_terminals.is_empty());
        assert!(idle_terminals.iter().all(|window| {
            window.active_process.as_deref() != Some("electron")
                && !window
                    .command_line
                    .as_deref()
                    .is_some_and(|command| command.contains("electron"))
        }));
    }

    #[test]
    fn reconciliation_adds_missing_windows_without_dropping_feed_state() {
        let mut existing = test_window_info("old title");
        existing.id = "existing".to_string();
        existing.demands_attention = true;
        existing.icon_path = Some(PathBuf::from("/tmp/existing.svg"));
        let mut feed_only = test_window_info("feed only");
        feed_only.id = "feed-only".to_string();
        let mut current = vec![existing.clone(), feed_only];

        let mut refreshed = test_window_info("new title");
        refreshed.id = "existing".to_string();
        refreshed.desktop_file_name = None;
        refreshed.geometry = None;
        refreshed.minimized = None;
        refreshed.icon_path = None;
        let mut newly_discovered = test_window_info("new window");
        newly_discovered.id = "new".to_string();

        let (changed, search_changed, cache_updates) =
            merge_reconciled_windows(&mut current, vec![refreshed, newly_discovered]);

        assert!(changed);
        assert!(search_changed);
        assert_eq!(cache_updates.len(), 1);
        assert_eq!(current.len(), 3);
        let merged = current
            .iter()
            .find(|window| window.id == "existing")
            .unwrap();
        assert_eq!(merged.title, "new title");
        assert_eq!(merged.desktop_file_name, existing.desktop_file_name);
        assert_eq!(merged.geometry, existing.geometry);
        assert_eq!(merged.minimized, existing.minimized);
        assert_eq!(merged.icon_path, existing.icon_path);
        assert!(merged.demands_attention);
        assert!(current.iter().any(|window| window.id == "feed-only"));
        assert!(current.iter().any(|window| window.id == "new"));
    }

    #[test]
    fn icon_cache_separates_terminal_children_from_standalone_apps() {
        let terminal_child = window_icon_cache_key(
            "xfce4-terminal",
            Some("xfce4-terminal"),
            Some("electron"),
            Some(Path::new("/usr/bin/xfce4-terminal")),
        );
        let standalone_app = window_icon_cache_key(
            "electron",
            None,
            None,
            Some(Path::new(
                "/home/lewis/Dev/chatgpt/node_modules/electron/dist/electron",
            )),
        );

        assert_ne!(terminal_child, standalone_app);
        let mut cache = HashMap::new();
        cache.insert(
            terminal_child,
            Some(PathBuf::from("/usr/share/icons/xfce4-terminal.svg")),
        );
        assert!(!cache.contains_key(&standalone_app));
    }

    #[test]
    fn non_terminal_window_does_not_use_terminal_executable_fallback() {
        assert_eq!(
            resolve_window_icon(
                "breeze-dark",
                "application-with-no-installed-icon",
                None,
                None,
                Some(Path::new("/usr/bin/xfce4-terminal")),
            ),
            None
        );
    }

    #[test]
    fn tor_browser_never_uses_firefox_icon_fallback() {
        let icon = resolve_window_icon(
            "breeze-dark",
            "Tor Browser",
            None,
            None,
            Some(Path::new("/opt/tor-browser/Browser/firefox.real")),
        );

        assert!(
            icon.as_ref()
                .is_none_or(|path| { !path.to_string_lossy().to_lowercase().contains("firefox") })
        );
    }

    #[test]
    fn spinner_and_geometry_updates_do_not_invalidate_window_search() {
        let old = test_window_info("codex - ⠇ applicationlauncher - Terminal");
        let mut new = test_window_info("codex - ⠧ applicationlauncher - Terminal");
        new.geometry = Some((100, 80, 1200, 900));
        new.minimized = Some(true);
        new.demands_attention = true;

        assert!(window_search_metadata_equal(&old, &new));
    }

    #[test]
    fn attention_animation_does_not_change_window_order_or_search_metadata() {
        let old = test_window_info("codex - [ . ] Action Required - ~/Dev/sites/dictai - Terminal");
        let new = test_window_info("codex - [ ! ] Action Required - ~/Dev/sites/dictai - Terminal");

        assert_eq!(window_sort_title_key(&old), window_sort_title_key(&new));
        assert!(window_search_metadata_equal(&old, &new));
    }

    #[test]
    fn last_activation_order_puts_oldest_windows_first() {
        let mut older = test_window_info("older");
        older.last_activated_at_ms = Some(100);
        older.activation_sequence = 2;
        let mut newer = test_window_info("newer");
        newer.last_activated_at_ms = Some(200);
        newer.activation_sequence = 1;
        let mut unknown = test_window_info("unknown");
        unknown.last_activated_at_ms = None;
        unknown.activation_sequence = 0;

        assert_eq!(
            compare_windows_by_last_activation(&older, &newer),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_windows_by_last_activation(&newer, &unknown),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn cached_attention_animation_updates_without_reranking() {
        let old = "codex - [ . ] Action Required - ~/Dev/sites/dictai - Terminal";
        let new = "codex - [ ! ] Action Required - ~/Dev/sites/dictai - Terminal";
        let mut display_title = format!("{old} | codex | ~/Dev/sites/dictai");

        refresh_cached_transient_title(&mut display_title, old, new);

        assert_eq!(display_title, format!("{new} | codex | ~/Dev/sites/dictai"));
    }

    #[test]
    fn searchable_window_changes_still_invalidate_search() {
        let old = test_window_info("codex - ⠇ applicationlauncher - Terminal");
        let renamed = test_window_info("codex - ⠧ fuzzy-rank - Terminal");
        let mut changed_process = old.clone();
        changed_process.active_process = Some("htop".to_string());

        assert!(!window_search_metadata_equal(&old, &renamed));
        assert!(!window_search_metadata_equal(&old, &changed_process));
    }

    #[test]
    fn repeated_window_changes_do_not_extend_search_refresh_deadline() {
        let now = Instant::now();
        let first_deadline = window_search_refresh_deadline(None, now);
        let later_event = now + Duration::from_millis(50);

        assert_eq!(
            first_deadline,
            now + Duration::from_millis(WINDOW_SEARCH_REFRESH_INTERVAL_MS)
        );
        assert_eq!(
            window_search_refresh_deadline(Some(first_deadline), later_event),
            first_deadline
        );
    }

    #[test]
    fn unicode_window_title_is_safe_in_typo_visibility_check() {
        let title = "videos — pcmanfm | pcmanfm | /usr/bin/pcmanfm | ~";

        assert!(!visible_title_has_typo_match(title, "mp"));
    }

    #[test]
    fn row_height_can_be_configured_below_thirty_when_content_fits() {
        assert_eq!(
            effective_list_row_height(18.0, 14.0, 2.0, 8.0, 0.0, false),
            18.0
        );
        assert_eq!(
            effective_list_row_height(12.0, 14.0, 4.0, 8.0, 0.0, false),
            18.0
        );
    }

    #[test]
    fn selection_accent_scales_with_compact_rows() {
        let compact = selected_row_accent_size(12.0);
        assert!(compact.x < 3.0);
        assert!(compact.y < 12.0);
        assert_eq!(selected_row_accent_size(52.0), egui::vec2(3.0, 28.0));
    }

    #[test]
    fn codex_code_mode_uses_codex_terminal_title() {
        assert_eq!(terminal_primary_title("codex-code-mode", None), "codex");
        assert_eq!(terminal_primary_title("codex", None), "codex");
    }

    #[test]
    fn fish_home_directory_keeps_tilde_in_terminal_title() {
        assert_eq!(
            terminal_display_title("~ - Terminal", "fish", Some("fish"), Some("~"), None),
            "fish - ~ - Terminal"
        );
    }

    #[test]
    fn nested_shell_does_not_hide_foreground_codex_process() {
        let ppid_to_children =
            HashMap::from([(1, vec![2]), (2, vec![3]), (3, vec![4]), (4, vec![5])]);
        let pid_to_name = HashMap::from([
            (2, "fish".to_string()),
            (3, "fish".to_string()),
            (4, "codex".to_string()),
            (5, "plasma-browser-".to_string()),
        ]);
        let stats = HashMap::from([
            (
                2,
                ProcessStat {
                    pid: 2,
                    name: "fish".to_string(),
                    ppid: 1,
                    process_group: 2,
                    session: 2,
                    tty: 10,
                    foreground_process_group: 4,
                },
            ),
            (
                3,
                ProcessStat {
                    pid: 3,
                    name: "fish".to_string(),
                    ppid: 2,
                    process_group: 3,
                    session: 2,
                    tty: 10,
                    foreground_process_group: 4,
                },
            ),
            (
                4,
                ProcessStat {
                    pid: 4,
                    name: "codex".to_string(),
                    ppid: 3,
                    process_group: 4,
                    session: 2,
                    tty: 10,
                    foreground_process_group: 4,
                },
            ),
            (
                5,
                ProcessStat {
                    pid: 5,
                    name: "plasma-browser-".to_string(),
                    ppid: 4,
                    process_group: 5,
                    session: 5,
                    tty: 11,
                    foreground_process_group: 5,
                },
            ),
        ]);

        assert_eq!(
            find_terminal_leaf_with_stat_reader(1, &ppid_to_children, &pid_to_name, |pid| stats
                .get(&pid)
                .cloned(),),
            Some((4, "codex".to_string()))
        );
    }

    #[test]
    fn spinner_is_not_duplicated_on_terminal_cwd_segment() {
        assert_eq!(
            terminal_display_title(
                "⠋ dictai - ⠋ ~ - Terminal",
                "codex",
                Some("codex resume --last"),
                Some("~"),
                None,
            ),
            "codex - ⠋ dictai - ~ - Terminal"
        );
    }

    #[test]
    fn spinner_on_cwd_basename_is_merged_into_full_cwd() {
        assert_eq!(
            terminal_display_title(
                "⠸ dictai - Terminal",
                "codex",
                Some("codex resume --last"),
                Some("~/Dev/sites/dictai"),
                None,
            ),
            "codex - ⠸ ~/Dev/sites/dictai - Terminal"
        );
    }

    #[test]
    fn action_required_status_is_not_duplicated_when_adding_cwd() {
        assert_eq!(
            terminal_display_title(
                "[ . ] Action Required | dictionary-extension - Terminal",
                "codex",
                Some("codex resume --last"),
                Some("~"),
                None,
            ),
            "codex - [ . ] Action Required - dictionary-extension - ~ - Terminal"
        );
    }

    #[test]
    fn codex_child_process_keeps_codex_in_terminal_title() {
        let process_chain = vec![
            ProcessChainEntry {
                pid: 20,
                name: "curl".to_string(),
                exe_path: None,
            },
            ProcessChainEntry {
                pid: 19,
                name: "codex-code-mode".to_string(),
                exe_path: None,
            },
        ];

        let parent_program = terminal_parent_program("curl", &process_chain);
        assert_eq!(parent_program, Some("codex"));
        assert_eq!(
            terminal_display_title(
                "codex - ~/Dev/applicationlauncher - Terminal",
                "curl",
                Some("curl https://example.com"),
                Some("~/Dev/applicationlauncher"),
                parent_program,
            ),
            "codex - curl - ~/Dev/applicationlauncher - Terminal"
        );
    }

    #[test]
    fn terminal_leaf_filter_rejects_detached_browser_helpers() {
        let shell = parse_proc_stat("100 (fish) S 10 100 100 34840 200").unwrap();
        let codex = parse_proc_stat("200 (codex) S 100 200 100 34840 200").unwrap();
        let curl = parse_proc_stat("201 (curl) S 200 200 100 34840 200").unwrap();
        let browser = parse_proc_stat("300 (plasma-browser-) S 200 300 300 34841 300").unwrap();

        assert!(is_terminal_foreground_process(&codex, &shell));
        assert!(is_terminal_foreground_process(&curl, &shell));
        assert!(!is_terminal_foreground_process(&browser, &shell));
    }

    #[test]
    fn typo_highlight_for_mpv_has_visible_yellow_word() {
        let title = "what is the generic term for a movie and an episode - Google Search";
        let segments = title_highlight_segments(title, "mpv");

        assert!(
            segments
                .iter()
                .any(|(start, end, is_red)| !*is_red && &title[*start..*end] == "movie"),
            "expected mpv typo match to visibly highlight movie, got {segments:?}"
        );
    }

    #[test]
    fn focus_text_around_typo_match_does_not_panic() {
        let title = "python3 whisper_service.py faster-whisper | /home/lewis/Dev/assistant/.venv/bin/python3 whisper_service.py";
        let focused = focus_text_around_match(title, "whipser", None, None, 40);

        assert!(focused.contains("whisper"));
    }

    #[test]
    fn focused_title_uses_ranked_visible_match_for_mpv_gimp_match() {
        let title = "colour 8-bit non-linear integer, sRGB IEC61966-2.1, 1 layer, 3840x2160, imported image metadata for a screenshot edited in GIMP";
        let fields = [SearchField {
            priority: 0,
            value: "gimp",
        }];
        let candidate = MetadataCandidate {
            key: "gimp",
            fields: &fields,
            score: 0.0,
        };
        let rank = MetadataQuery::new("mpv")
            .unwrap()
            .with_typo_fallback(true)
            .search_rank(candidate)
            .unwrap();
        let focused = focus_text_around_match(title, "mpv", Some("gimp"), Some(&rank), 70);

        assert!(
            focused.to_lowercase().contains("gimp"),
            "expected focused title to include GIMP match, got {focused}"
        );
        let segments =
            title_highlight_segments_with_ranked_field(&focused, "mpv", Some("gimp"), Some(&rank));
        assert!(
            segments.iter().any(|(start, end, is_red)| !*is_red
                && focused[*start..*end].eq_ignore_ascii_case("gimp")),
            "expected focused title to visibly highlight GIMP, got {focused}"
        );
        assert!(
            segments
                .iter()
                .all(|(start, end, _)| end.saturating_sub(*start) < focused.len()),
            "should not highlight the whole focused title: {segments:?}"
        );
    }

    #[test]
    fn highlighted_title_job_ignores_invalid_ranges() {
        let title = "movie";
        let segments = [(0, title.len() + 10, false), (1, 3, false)];

        highlighted_title_job_from_segments(title, 12.0, &segments);
    }

    #[test]
    fn titleless_application_clients_are_not_listed_as_windows() {
        let mut icon_cache = HashMap::new();
        let ppid_to_children = HashMap::new();
        let pid_to_name = HashMap::new();
        let pid_to_ppid = HashMap::new();
        let window = build_window_info(
            "mousepad-internal-client".to_string(),
            String::new(),
            "Org.xfce.mousepad".to_string(),
            Some("org.xfce.mousepad".to_string()),
            None,
            None,
            Some(false),
            "breeze-dark",
            &mut icon_cache,
            &ppid_to_children,
            &pid_to_name,
            &pid_to_ppid,
            &[],
        );

        assert!(window.is_none());
    }

    #[test]
    fn test_compute_display_title_and_highlights_typo() {
        let base_query = MetadataQuery::new("fiom").unwrap();
        let typo_query = MetadataQuery::new("fiom").unwrap().with_typo_fallback(true);
        let search_values = vec![(0, "fish".to_string())];
        let (_rank, display_title, highlights, title_is_typo) =
            compute_display_title_and_highlights(
                "fish",
                &search_values,
                &base_query,
                &typo_query,
                70,
            )
            .unwrap();
        println!("display_title: {}", display_title);
        println!("highlights: {:?}", highlights);
        println!("title_is_typo: {}", title_is_typo);
        assert_eq!(display_title, "fish");
        assert_eq!(highlights, vec![(0, 4, false)]);
        assert!(title_is_typo);
    }
}
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
