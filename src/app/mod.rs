mod commands;
mod feed;
mod helpers;
mod popups;
mod settings;
#[cfg(test)]
mod tests;
mod view;
use eframe::egui;
use fuzzy_rank::metadata::{MetadataCandidate, MetadataQuery};
pub(crate) use helpers::{
    effective_list_row_height, filtered_search_cache_key, grid_move_down, grid_move_up, inset_rect,
    load_window_size, nearest_center_index, paint_centered_title_job, paint_icon_in_rect,
    selected_row_accent_size, setup_system_fonts, show_immediate_icon_tooltip,
    window_search_refresh_deadline,
};
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
const WINDOW_LAST_ACTIVATION_COLUMN_WIDTH: f32 = 42.0;

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

fn format_last_activation_age(timestamp_ms: Option<i64>) -> String {
    let Some(timestamp_ms) = timestamp_ms.filter(|timestamp| *timestamp > 0) else {
        return "-".to_string();
    };

    let elapsed_seconds =
        ((applicationlauncher::tracker::now_ms() - timestamp_ms).max(0) / 1000) as u64;
    match elapsed_seconds {
        0..=59 => format!("{elapsed_seconds}s"),
        60..=3_599 => format!("{}m", elapsed_seconds / 60),
        _ => format!("{}h", elapsed_seconds / 3_600),
    }
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
    win_show_last_activation: bool,
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
    pending_settings_save: Option<LauncherSettings>,
    settings_save_deadline: Option<Instant>,
    process_tree_cache: Option<crate::windows::process::ProcessTree>,
    process_tree_cache_updated_at: Option<Instant>,
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
    app_audio_levels: HashMap<PathBuf, f32>,
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
        let (audio_cache_tx, audio_cache_rx) = std::sync::mpsc::sync_channel(1);
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
            win_show_last_activation: settings.win_show_last_activation,
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
            pending_settings_save: None,
            settings_save_deadline: None,
            process_tree_cache: None,
            process_tree_cache_updated_at: None,
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
            app_audio_levels: HashMap::new(),
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

        let audio_repaint_ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            let mut recent_active_pipewire_nodes: HashMap<u32, std::time::Instant> = HashMap::new();
            let mut last_update = None;
            loop {
                let sink_inputs = fetch_sink_inputs();
                let has_active_playback = sink_inputs.iter().any(|sink| {
                    !sink.mute
                        && !sink.corked
                        && sink
                            .properties
                            .get("media.category")
                            .is_none_or(|category| category.eq_ignore_ascii_case("Playback"))
                });
                let active_media_app_keys =
                    if has_active_playback && sink_inputs.iter().any(sink_input_is_browser_like) {
                        fetch_active_media_app_keys()
                    } else {
                        HashSet::new()
                    };
                let (
                    observed_pipewire_node_ids,
                    active_pipewire_node_ids,
                    pipewire_activity_cache_valid,
                ) = if has_active_playback {
                    fetch_pipewire_activity()
                } else {
                    (HashSet::new(), HashSet::new(), false)
                };
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

                let update = AudioCacheUpdate {
                    sink_inputs,
                    active_media_app_keys,
                    observed_pipewire_node_ids,
                    active_pipewire_node_ids: effective_active_pipewire_node_ids,
                    pipewire_activity_cache_valid,
                };
                if last_update.as_ref() != Some(&update) {
                    match audio_cache_tx.try_send(update.clone()) {
                        Ok(()) => {
                            last_update = Some(update);
                            audio_repaint_ctx.request_repaint();
                        }
                        Err(std::sync::mpsc::TrySendError::Full(_)) => {}
                        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => break,
                    }
                }
                let poll_ms = if has_active_playback {
                    AUDIO_SINK_POLL_MS as u64
                } else {
                    AUDIO_IDLE_POLL_MS
                };
                std::thread::sleep(std::time::Duration::from_millis(poll_ms));
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
            win_show_last_activation: self.win_show_last_activation,
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

    fn save_settings(&mut self) {
        self.pending_settings_save = Some(self.launcher_settings_snapshot());
        self.settings_save_deadline = Some(Instant::now() + Duration::from_millis(150));
    }

    fn flush_settings_save(&mut self) {
        if self
            .settings_save_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.settings_save_deadline = None;
            if let Some(settings) = self.pending_settings_save.take() {
                save_launcher_settings(settings);
            }
        }
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
        self.win_show_last_activation = settings.win_show_last_activation;
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
