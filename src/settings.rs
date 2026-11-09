use eframe::egui;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::app::App;
use crate::models::{IdleCodexActionState, LauncherSettings, SettingsWindowState};
mod persistence;
pub(crate) use persistence::{flush_writes, queue_write};

pub(crate) const SETTINGS_GRID_SPACING: [f32; 2] = [12.0, 4.0];

impl SettingsWindowState {
    pub(crate) fn new(
        settings: LauncherSettings,
        idle_codex_action: Arc<Mutex<IdleCodexActionState>>,
        terminal_maintenance: Arc<Mutex<crate::models::TerminalMaintenanceState>>,
    ) -> Self {
        let settings = settings.sanitized();
        if let Ok(mut state) = idle_codex_action.lock() {
            state.confirmation = false;
        }
        Self {
            pending_ui_scale: settings.ui_scale,
            scale_anchor: settings.ui_scale,
            settings,
            revision: 0,
            pending_save: None,
            save_deadline: None,
            idle_codex_action,
            terminal_maintenance,
        }
    }

    pub(crate) fn save_changed_settings(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.pending_save = Some(self.settings);
        self.save_deadline = Some(Instant::now() + Duration::from_millis(150));
    }

    pub(crate) fn flush_pending_save(&mut self) {
        if self
            .save_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.save_deadline = None;
            if let Some(settings) = self.pending_save.take() {
                save_launcher_settings(settings);
            }
        }
    }

    pub(crate) fn flush_pending_save_now(&mut self) {
        self.save_deadline = None;
        if let Some(settings) = self.pending_save.take() {
            save_launcher_settings(settings);
        }
    }
}
pub(crate) fn render_deferred_settings_panel(
    ui: &mut egui::Ui,
    state: &mut SettingsWindowState,
) -> bool {
    let scale_factor = (state.scale_anchor / state.settings.ui_scale).clamp(0.2, 5.0);
    if (scale_factor - 1.0).abs() > 0.001 {
        ui.set_style(App::scaled_style(ui.style().as_ref(), scale_factor));
    }

    let mut settings_changed = false;
    ui.add_space(8.0);
    ui.vertical_centered(|ui| {
        ui.heading(
            egui::RichText::new("Launcher Settings")
                .color(egui::Color32::WHITE)
                .strong(),
        );
    });
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(12.0);

    settings_changed |= ui
        .checkbox(
            &mut state.settings.disable_ibeam,
            egui::RichText::new("Disable text select cursor (I-beam)")
                .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220))
                .size(13.0),
        )
        .changed();

    ui.add_space(10.0);
    egui::Grid::new("deferred_global_scale_settings_grid")
        .num_columns(2)
        .spacing([12.0, 10.0])
        .show(ui, |ui| {
            settings_row_label(ui, "Global Scale:");
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut state.pending_ui_scale, 0.5..=2.5).show_value(true));
                let scale_changed =
                    (state.pending_ui_scale - state.settings.ui_scale).abs() > 0.001;
                if ui
                    .add_enabled(scale_changed, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.settings.ui_scale = state.pending_ui_scale.clamp(0.5, 2.5);
                    settings_changed = true;
                }
                if ui
                    .add_enabled(scale_changed, egui::Button::new("Reset"))
                    .clicked()
                {
                    state.pending_ui_scale = state.settings.ui_scale;
                }
            });
            if state.pending_ui_scale.is_nan() {
                state.pending_ui_scale = state.settings.ui_scale;
            }
            ui.end_row();
        });

    ui.add_space(14.0);
    ui.separator();
    ui.add_space(10.0);
    ui.label(
        egui::RichText::new("Application panel")
            .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220))
            .strong()
            .size(13.0),
    );
    ui.add_space(6.0);
    egui::Grid::new("deferred_app_panel_settings_grid")
        .num_columns(2)
        .spacing([12.0, 10.0])
        .show(ui, |ui| {
            settings_changed |= settings_checkbox_row(
                ui,
                "Show System Modules:",
                &mut state.settings.show_system_settings_modules,
            );
            settings_changed |=
                settings_checkbox_row(ui, "Icon Grid Mode:", &mut state.settings.app_icon_mode);
            settings_changed |= settings_slider_row(
                ui,
                "Icon Size:",
                &mut state.settings.app_icon_size,
                16.0..=64.0,
            );
            settings_changed |= settings_slider_row(
                ui,
                "Tile Size:",
                &mut state.settings.app_icon_tile_size,
                48.0..=128.0,
            );
            settings_changed |= settings_slider_row(
                ui,
                "Top Padding:",
                &mut state.settings.app_top_padding,
                0.0..=24.0,
            );
            settings_changed |= settings_slider_row(
                ui,
                "Bottom Padding:",
                &mut state.settings.app_bottom_padding,
                0.0..=24.0,
            );
            settings_changed |= settings_slider_row(
                ui,
                "Left Padding:",
                &mut state.settings.app_left_padding,
                0.0..=32.0,
            );
            settings_changed |= settings_slider_row(
                ui,
                "Right Padding:",
                &mut state.settings.app_right_padding,
                0.0..=32.0,
            );
            settings_changed |=
                settings_checkbox_row(ui, "Show Names:", &mut state.settings.app_icon_show_name);
            settings_changed |= settings_slider_row(
                ui,
                "Name Size:",
                &mut state.settings.app_icon_name_size,
                8.0..=20.0,
            );
            settings_changed |= settings_slider_row(
                ui,
                "Scroll Sensitivity:",
                &mut state.settings.app_scroll_sensitivity,
                0.1..=5.0,
            );
        });

    ui.add_space(14.0);
    ui.separator();
    ui.add_space(10.0);
    ui.label(
        egui::RichText::new("Open window view")
            .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220))
            .strong()
            .size(13.0),
    );
    ui.add_space(6.0);
    egui::Grid::new("deferred_window_settings_grid")
        .num_columns(2)
        .spacing([12.0, 10.0])
        .show(ui, |ui| {
            settings_changed |= settings_slider_row(
                ui,
                "Icon Size:",
                &mut state.settings.win_icon_size,
                16.0..=64.0,
            );
            settings_changed |= settings_slider_row(
                ui,
                "Top Padding:",
                &mut state.settings.win_top_padding,
                0.0..=24.0,
            );
            settings_changed |= settings_slider_row(
                ui,
                "Bottom Padding:",
                &mut state.settings.win_bottom_padding,
                0.0..=24.0,
            );
            settings_changed |= settings_slider_row(
                ui,
                "Left Padding:",
                &mut state.settings.win_left_padding,
                0.0..=32.0,
            );
            settings_changed |= settings_slider_row(
                ui,
                "Right Padding:",
                &mut state.settings.win_right_padding,
                0.0..=32.0,
            );
            settings_changed |= settings_slider_row(
                ui,
                "Row Height:",
                &mut state.settings.win_row_height,
                12.0..=100.0,
            );
            settings_changed |= settings_slider_row(
                ui,
                "Text Spacing:",
                &mut state.settings.win_text_spacing,
                0.0..=12.0,
            );
            settings_changed |= settings_slider_row(
                ui,
                "Line Height:",
                &mut state.settings.win_line_height,
                6.0..=30.0,
            );
            settings_changed |=
                settings_checkbox_row(ui, "Show Path:", &mut state.settings.win_show_path);
            settings_changed |= settings_checkbox_row(
                ui,
                "Show Last Activation:",
                &mut state.settings.win_show_last_activation,
            );
            settings_changed |= settings_checkbox_row(
                ui,
                "Show Run in Terminal:",
                &mut state.settings.show_run_in_terminal,
            );
            settings_changed |= settings_checkbox_row(
                ui,
                "Show CD in Terminal:",
                &mut state.settings.show_cd_in_terminal,
            );
            settings_changed |= settings_checkbox_row(
                ui,
                "Auto-send Enter on Attention (5s):",
                &mut state.settings.auto_send_enter_on_attention,
            );
            settings_changed |= settings_slider_row(
                ui,
                "Window Title Font Size:",
                &mut state.settings.win_title_size,
                6.0..=24.0,
            );
            settings_changed |= settings_slider_row(
                ui,
                "Window Path Font Size:",
                &mut state.settings.win_path_size,
                6.0..=20.0,
            );
            settings_changed |= settings_slider_row(
                ui,
                "Scroll Sensitivity:",
                &mut state.settings.win_scroll_sensitivity,
                0.1..=5.0,
            );
        });

    ui.add_space(14.0);
    ui.separator();
    ui.add_space(10.0);
    ui.label(egui::RichText::new("Show Info").strong());
    egui::Grid::new("deferred_info_settings_grid")
        .num_columns(2)
        .spacing([12.0, 10.0])
        .show(ui, |ui| {
            settings_changed |= settings_slider_row(
                ui,
                "Scroll Sensitivity:",
                &mut state.settings.info_scroll_sensitivity,
                0.1..=10.0,
            );
        });
    ui.add_space(14.0);

    if settings_changed {
        state.save_changed_settings();
    }

    let mut close_requested = false;
    ui.vertical_centered(|ui| {
        close_requested = ui
            .add(
                egui::Button::new(
                    egui::RichText::new("Close Settings (F10)")
                        .color(egui::Color32::WHITE)
                        .size(13.0),
                )
                .fill(egui::Color32::from_rgba_unmultiplied(61, 174, 233, 200)),
            )
            .clicked();
    });
    ui.add_space(8.0);
    close_requested
}

pub(crate) fn load_launcher_settings() -> LauncherSettings {
    let path = std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".config/applicationlauncher/settings.txt"));
    load_launcher_settings_from_path(path)
}

fn load_launcher_settings_from_path(path: Option<PathBuf>) -> LauncherSettings {
    let mut settings = LauncherSettings::default();

    if let Some(path) = path {
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(path) {
                let mut saw_win_top_padding = false;
                let mut saw_win_bottom_padding = false;
                let mut saw_app_top_padding = false;
                let mut saw_app_bottom_padding = false;
                for line in content.lines() {
                    let mut parts = line.splitn(2, '=');
                    let key = parts.next().unwrap_or("").trim();
                    let value = parts.next().unwrap_or("").trim();

                    match key {
                        "show_system_settings_modules" => {
                            settings.show_system_settings_modules = value
                                .parse::<bool>()
                                .unwrap_or(settings.show_system_settings_modules);
                        }
                        "app_icon_mode" => {
                            settings.app_icon_mode =
                                value.parse::<bool>().unwrap_or(settings.app_icon_mode);
                        }
                        "win_icon_size" => {
                            settings.win_icon_size = value
                                .parse::<f32>()
                                .map(|v| v.clamp(16.0, 64.0))
                                .unwrap_or(settings.win_icon_size);
                        }
                        "win_padding" => {
                            let padding = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.0, 24.0))
                                .unwrap_or(settings.win_top_padding);
                            if !saw_win_top_padding {
                                settings.win_top_padding = padding;
                            }
                            if !saw_win_bottom_padding {
                                settings.win_bottom_padding = padding;
                            }
                            if !saw_app_top_padding {
                                settings.app_top_padding = padding;
                            }
                            if !saw_app_bottom_padding {
                                settings.app_bottom_padding = padding;
                            }
                        }
                        "win_horizontal_padding" => {
                            let padding = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.0, 32.0))
                                .unwrap_or(settings.win_left_padding);
                            settings.win_left_padding = padding;
                            settings.win_right_padding = padding;
                        }
                        "win_top_padding" => {
                            settings.win_top_padding = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.0, 24.0))
                                .unwrap_or(settings.win_top_padding);
                            saw_win_top_padding = true;
                        }
                        "win_bottom_padding" => {
                            settings.win_bottom_padding = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.0, 24.0))
                                .unwrap_or(settings.win_bottom_padding);
                            saw_win_bottom_padding = true;
                        }
                        "win_left_padding" => {
                            settings.win_left_padding = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.0, 32.0))
                                .unwrap_or(settings.win_left_padding);
                        }
                        "win_right_padding" => {
                            settings.win_right_padding = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.0, 32.0))
                                .unwrap_or(settings.win_right_padding);
                        }
                        "win_row_height" => {
                            settings.win_row_height = value
                                .parse::<f32>()
                                .map(|v| v.clamp(12.0, 100.0))
                                .unwrap_or(settings.win_row_height);
                        }
                        "win_text_spacing" => {
                            settings.win_text_spacing = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.0, 12.0))
                                .unwrap_or(settings.win_text_spacing);
                        }
                        "win_line_height" => {
                            settings.win_line_height = value
                                .parse::<f32>()
                                .map(|v| v.clamp(6.0, 30.0))
                                .unwrap_or(settings.win_line_height);
                        }
                        "win_show_path" => {
                            settings.win_show_path =
                                value.parse::<bool>().unwrap_or(settings.win_show_path);
                        }
                        "win_show_last_activation" => {
                            settings.win_show_last_activation = value
                                .parse::<bool>()
                                .unwrap_or(settings.win_show_last_activation);
                        }
                        "show_run_in_terminal" => {
                            settings.show_run_in_terminal = value
                                .parse::<bool>()
                                .unwrap_or(settings.show_run_in_terminal);
                        }
                        "show_cd_in_terminal" => {
                            settings.show_cd_in_terminal = value
                                .parse::<bool>()
                                .unwrap_or(settings.show_cd_in_terminal);
                        }
                        "auto_send_enter_on_attention" => {
                            settings.auto_send_enter_on_attention = value
                                .parse::<bool>()
                                .unwrap_or(settings.auto_send_enter_on_attention);
                        }
                        "win_title_size" => {
                            settings.win_title_size = value
                                .parse::<f32>()
                                .map(|v| v.clamp(6.0, 24.0))
                                .unwrap_or(settings.win_title_size);
                        }
                        "win_path_size" => {
                            settings.win_path_size = value
                                .parse::<f32>()
                                .map(|v| v.clamp(6.0, 20.0))
                                .unwrap_or(settings.win_path_size);
                        }
                        "app_icon_size" => {
                            settings.app_icon_size = value
                                .parse::<f32>()
                                .map(|v| v.clamp(16.0, 64.0))
                                .unwrap_or(settings.app_icon_size);
                        }
                        "app_icon_tile_size" => {
                            settings.app_icon_tile_size = value
                                .parse::<f32>()
                                .map(|v| v.clamp(48.0, 128.0))
                                .unwrap_or(settings.app_icon_tile_size);
                        }
                        "app_horizontal_padding" => {
                            let padding = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.0, 32.0))
                                .unwrap_or(settings.app_left_padding);
                            settings.app_left_padding = padding;
                            settings.app_right_padding = padding;
                        }
                        "app_top_padding" => {
                            settings.app_top_padding = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.0, 24.0))
                                .unwrap_or(settings.app_top_padding);
                            saw_app_top_padding = true;
                        }
                        "app_bottom_padding" => {
                            settings.app_bottom_padding = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.0, 24.0))
                                .unwrap_or(settings.app_bottom_padding);
                            saw_app_bottom_padding = true;
                        }
                        "app_left_padding" => {
                            settings.app_left_padding = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.0, 32.0))
                                .unwrap_or(settings.app_left_padding);
                        }
                        "app_right_padding" => {
                            settings.app_right_padding = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.0, 32.0))
                                .unwrap_or(settings.app_right_padding);
                        }
                        "app_icon_show_name" => {
                            settings.app_icon_show_name =
                                value.parse::<bool>().unwrap_or(settings.app_icon_show_name);
                        }
                        "app_icon_name_size" => {
                            settings.app_icon_name_size = value
                                .parse::<f32>()
                                .map(|v| v.clamp(8.0, 20.0))
                                .unwrap_or(settings.app_icon_name_size);
                        }
                        "disable_ibeam" => {
                            settings.disable_ibeam =
                                value.parse::<bool>().unwrap_or(settings.disable_ibeam);
                        }
                        "app_scroll_sensitivity" => {
                            settings.app_scroll_sensitivity = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.1, 10.0))
                                .unwrap_or(settings.app_scroll_sensitivity);
                        }
                        "win_scroll_sensitivity" => {
                            settings.win_scroll_sensitivity = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.1, 10.0))
                                .unwrap_or(settings.win_scroll_sensitivity);
                        }
                        "info_scroll_sensitivity" => {
                            settings.info_scroll_sensitivity = value
                                .parse::<f32>()
                                .unwrap_or(settings.info_scroll_sensitivity);
                        }
                        "ui_scale" => {
                            settings.ui_scale = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.5, 2.5))
                                .unwrap_or(settings.ui_scale);
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    settings.sanitized()
}

pub(crate) fn save_launcher_settings(settings: LauncherSettings) {
    if let Ok(home) = std::env::var("HOME") {
        let dir = PathBuf::from(format!("{}/.config/applicationlauncher", home));
        let path = dir.join("settings.txt");
        queue_write(path, format_launcher_settings(settings));
    }
}

fn format_launcher_settings(settings: LauncherSettings) -> String {
    format!(
        "show_system_settings_modules={}\napp_icon_mode={}\nwin_icon_size={:.1}\nwin_top_padding={:.1}\nwin_bottom_padding={:.1}\nwin_left_padding={:.1}\nwin_right_padding={:.1}\nwin_row_height={:.1}\nwin_text_spacing={:.1}\nwin_line_height={:.1}\nwin_show_path={}\nwin_show_last_activation={}\nshow_run_in_terminal={}\nshow_cd_in_terminal={}\nauto_send_enter_on_attention={}\nwin_title_size={:.1}\nwin_path_size={:.1}\napp_icon_size={:.1}\napp_icon_tile_size={:.1}\napp_top_padding={:.1}\napp_bottom_padding={:.1}\napp_left_padding={:.1}\napp_right_padding={:.1}\napp_icon_show_name={}\napp_icon_name_size={:.1}\ndisable_ibeam={}\napp_scroll_sensitivity={:.2}\nwin_scroll_sensitivity={:.2}\ninfo_scroll_sensitivity={:.2}\nui_scale={:.2}\n",
        settings.show_system_settings_modules,
        settings.app_icon_mode,
        settings.win_icon_size,
        settings.win_top_padding,
        settings.win_bottom_padding,
        settings.win_left_padding,
        settings.win_right_padding,
        settings.win_row_height,
        settings.win_text_spacing,
        settings.win_line_height,
        settings.win_show_path,
        settings.win_show_last_activation,
        settings.show_run_in_terminal,
        settings.show_cd_in_terminal,
        settings.auto_send_enter_on_attention,
        settings.win_title_size,
        settings.win_path_size,
        settings.app_icon_size,
        settings.app_icon_tile_size,
        settings.app_top_padding,
        settings.app_bottom_padding,
        settings.app_left_padding,
        settings.app_right_padding,
        settings.app_icon_show_name,
        settings.app_icon_name_size,
        settings.disable_ibeam,
        settings.app_scroll_sensitivity,
        settings.win_scroll_sensitivity,
        settings.info_scroll_sensitivity,
        settings.ui_scale
    )
}

struct IdleCodexJob {
    state: Arc<Mutex<IdleCodexActionState>>,
    ctx: egui::Context,
    message: String,
}

impl Drop for IdleCodexJob {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.running = false;
            state.message = Some(std::mem::take(&mut self.message));
        }
        self.ctx.request_repaint();
        self.ctx.request_repaint_of(egui::ViewportId::ROOT);
        self.ctx
            .request_repaint_of(egui::ViewportId::from_hash_of("launcher_settings_popup"));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfirmationResponse {
    Pending,
    Cancel,
    Confirm,
}

pub(crate) fn destructive_action_confirmation(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    message: &str,
    confirm_label: &str,
) -> ConfirmationResponse {
    let response = egui::Modal::new(egui::Id::new(id)).show(ctx, |ui| {
        ui.set_width((ctx.content_rect().width() - 48.0).clamp(1.0, 460.0));
        ui.heading(title);
        ui.add_space(8.0);
        ui.label(message);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let cancel = ui.button("Cancel");
            let confirm = ui.button(confirm_label);
            let focused = ctx.memory(|memory| memory.focused());
            if focused != Some(cancel.id) && focused != Some(confirm.id) {
                cancel.request_focus();
            }
            if cancel.clicked() {
                ConfirmationResponse::Cancel
            } else if confirm.clicked() {
                ConfirmationResponse::Confirm
            } else {
                ConfirmationResponse::Pending
            }
        })
        .inner
    });
    if response.should_close() {
        ConfirmationResponse::Cancel
    } else {
        response.inner
    }
}

fn idle_codex_action_panel(ui: &mut egui::Ui, state: &mut IdleCodexActionState) -> bool {
    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);
    let was_confirming = state.confirmation;
    if ui
        .add_enabled(
            !state.running,
            egui::Button::new("Stop idle Codex sessions..."),
        )
        .clicked()
    {
        state.confirmation = true;
        ui.ctx().request_repaint();
    }
    ui.small("Only sessions with no spinner or attention request. Interrupts Codex, not its terminal or shell.");
    let mut start = false;
    // Do not let the click that opens the dialog also approve the action.
    if was_confirming && state.confirmation && !state.running {
        match destructive_action_confirmation(
            ui.ctx(),
            "stop_idle_codex_confirmation",
            "Stop idle Codex sessions?",
            "Send SIGINT to all verified local idle Codex sessions? This interrupts Codex but leaves its terminals and shells open. Busy, changed or unverified sessions are skipped. This cannot be undone.",
            "Interrupt idle Codex",
        ) {
            ConfirmationResponse::Confirm => {
                state.confirmation = false;
                state.running = true;
                state.message = None;
                start = true;
            }
            ConfirmationResponse::Cancel => state.confirmation = false,
            ConfirmationResponse::Pending => {}
        }
    }
    if state.running {
        ui.small("Checking terminal state and interrupting idle Codex sessions...");
    }
    if let Some(message) = &state.message {
        ui.small(message);
    }
    start
}

pub(crate) fn render_idle_codex_action(
    ui: &mut egui::Ui,
    shared: &Arc<Mutex<IdleCodexActionState>>,
) {
    let start = match shared.lock() {
        Ok(mut state) => idle_codex_action_panel(ui, &mut state),
        Err(_) => return,
    };

    if start {
        let mut job = IdleCodexJob {
            state: shared.clone(),
            ctx: ui.ctx().clone(),
            message: "Idle Codex action did not complete; no automatic retry was scheduled.".into(),
        };
        // The job guard clears the busy state even if spawning or the worker fails.
        if let Err(err) = std::thread::Builder::new()
            .name("stop-idle-codex".into())
            .spawn(move || {
                let worker = applicationlauncher::observability::register_worker("stop-idle-codex");
                worker.set_state("checking-idle-sessions");
                job.message = match crate::windows::terminal::idle::stop_idle_codex() {
                    Ok(message) => message,
                    Err(error) => format!("Idle Codex action failed: {error}"),
                };
            })
        {
            crate::diagnostics::write_stderr_line(&format!(
                "Could not start idle Codex action: {err}"
            ));
        }
    }
}

pub(crate) fn settings_row_label(ui: &mut egui::Ui, label: &str) {
    ui.label(
        egui::RichText::new(label).color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
    );
}

pub(crate) fn settings_checkbox_row(ui: &mut egui::Ui, label: &str, value: &mut bool) -> bool {
    settings_row_label(ui, label);
    let changed = ui.checkbox(value, "").changed();
    ui.end_row();
    changed
}

pub(crate) fn settings_slider_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
) -> bool {
    settings_row_label(ui, label);
    let changed = ui
        .add(egui::Slider::new(value, range).show_value(true))
        .changed();
    ui.end_row();
    changed
}

#[cfg(test)]
mod info_settings_tests {
    use super::*;

    #[test]
    fn info_scroll_sensitivity_persists_independently_and_loads_safe_legacy_defaults() {
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let fixture = Fixture(std::env::temp_dir().join(format!(
            "launcher-info-scroll-settings-{}-{nonce}",
            std::process::id()
        )));
        std::fs::create_dir(&fixture.0).unwrap();
        let path = fixture.0.join("settings.txt");
        let settings = LauncherSettings {
            app_scroll_sensitivity: 1.75,
            win_scroll_sensitivity: 3.25,
            info_scroll_sensitivity: 4.25,
            ..LauncherSettings::default()
        };
        let content = format_launcher_settings(settings);
        assert!(
            content
                .lines()
                .any(|line| line == "info_scroll_sensitivity=4.25")
        );
        std::fs::write(&path, content).unwrap();
        let reloaded = load_launcher_settings_from_path(Some(path.clone()));
        assert_eq!(reloaded.info_scroll_sensitivity, 4.25);
        assert_eq!(reloaded.app_scroll_sensitivity, 1.75);
        assert_eq!(reloaded.win_scroll_sensitivity, 3.25);

        let default = LauncherSettings::default().info_scroll_sensitivity;
        for (content, expected) in [
            ("win_scroll_sensitivity=3.25\n", default),
            ("info_scroll_sensitivity=not-a-number\n", default),
            ("info_scroll_sensitivity=NaN\n", default),
            ("info_scroll_sensitivity=inf\n", default),
            ("info_scroll_sensitivity=-1\n", 0.1),
            ("info_scroll_sensitivity=100\n", 10.0),
        ] {
            std::fs::write(&path, content).unwrap();
            let reloaded = load_launcher_settings_from_path(Some(path.clone()));
            assert_eq!(reloaded.info_scroll_sensitivity, expected, "{content}");
        }
        assert_eq!(
            load_launcher_settings_from_path(None).info_scroll_sensitivity,
            default
        );
    }
}

#[cfg(test)]
mod idle_codex_action_tests {
    use super::*;

    fn render(
        ctx: &egui::Context,
        state: &mut IdleCodexActionState,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, bool) {
        let mut start = false;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 700.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    // Test only the decision, never the renderer that starts a real worker.
                    start = idle_codex_action_panel(ui, state);
                });
            },
        );
        (output, start)
    }

    fn button_center(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("expected button label")
    }

    fn click(position: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
        ]
    }

    fn key(key: egui::Key) -> Vec<egui::Event> {
        vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }]
    }

    #[test]
    fn idle_codex_modal_requires_a_separate_explicit_confirmation_and_emits_only_one_start() {
        let ctx = egui::Context::default();
        let mut state = IdleCodexActionState::default();
        let (output, start) = render(&ctx, &mut state, vec![]);
        assert!(!start);
        let open = button_center(&output, "Stop idle Codex sessions...");
        assert!(!render(&ctx, &mut state, click(open)).1);
        assert!(state.confirmation);
        assert!(!state.running);
        for _ in 0..3 {
            assert!(!render(&ctx, &mut state, vec![]).1);
        }
        let (output, _) = render(&ctx, &mut state, vec![]);
        let confirm = button_center(&output, "Interrupt idle Codex");
        assert!(render(&ctx, &mut state, click(confirm)).1);
        assert!(state.running);
        assert!(!state.confirmation);
        assert!(!render(&ctx, &mut state, click(confirm)).1);
    }

    #[test]
    fn idle_codex_modal_cancel_escape_enter_and_backdrop_never_start_work() {
        for dismissal in ["cancel", "escape", "enter", "backdrop"] {
            let ctx = egui::Context::default();
            let mut state = IdleCodexActionState {
                confirmation: true,
                message: Some("Previous result".into()),
                ..Default::default()
            };
            for _ in 0..3 {
                assert!(!render(&ctx, &mut state, vec![]).1);
            }
            let (output, _) = render(&ctx, &mut state, vec![]);
            let events = match dismissal {
                "cancel" => click(button_center(&output, "Cancel")),
                "escape" => key(egui::Key::Escape),
                "enter" => key(egui::Key::Enter),
                _ => click(egui::pos2(2.0, 2.0)),
            };
            assert!(!render(&ctx, &mut state, events).1, "{dismissal}");
            assert!(!state.running, "{dismissal}");
            assert!(!state.confirmation, "{dismissal}");
            assert_eq!(state.message.as_deref(), Some("Previous result"));
        }
    }

    #[test]
    fn reopening_settings_discards_unconfirmed_idle_codex_consent() {
        let shared = Arc::new(Mutex::new(IdleCodexActionState {
            confirmation: true,
            ..Default::default()
        }));
        let _settings =
            SettingsWindowState::new(LauncherSettings::default(), shared.clone(), Arc::default());
        let state = shared.lock().unwrap();
        assert!(!state.confirmation);
        assert!(!state.running);
    }

    #[test]
    fn rendering_settings_never_arms_or_runs_the_action() {
        let ctx = egui::Context::default();
        let shared = Arc::new(Mutex::new(IdleCodexActionState::default()));
        for _ in 0..3 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    render_idle_codex_action(ui, &shared);
                });
            });
            let state = shared.lock().unwrap();
            assert!(!state.confirmation);
            assert!(!state.running);
            assert!(state.message.is_none());
        }
    }

    #[test]
    fn reopening_settings_keeps_the_same_in_flight_action_and_result() {
        let shared = Arc::new(Mutex::new(IdleCodexActionState {
            running: true,
            ..Default::default()
        }));
        let maintenance: Arc<Mutex<crate::models::TerminalMaintenanceState>> = Arc::default();
        let first = SettingsWindowState::new(
            LauncherSettings::default(),
            shared.clone(),
            maintenance.clone(),
        );
        drop(first);
        let reopened =
            SettingsWindowState::new(LauncherSettings::default(), shared.clone(), maintenance);
        assert!(reopened.idle_codex_action.lock().unwrap().running);
        assert!(Arc::ptr_eq(&reopened.idle_codex_action, &shared));

        drop(IdleCodexJob {
            state: shared.clone(),
            ctx: egui::Context::default(),
            message: "mock action completed".into(),
        });
        let state = reopened.idle_codex_action.lock().unwrap();
        assert!(!state.running);
        assert_eq!(state.message.as_deref(), Some("mock action completed"));
    }

    #[test]
    fn an_abandoned_job_clears_busy_without_scheduling_a_retry() {
        let shared = Arc::new(Mutex::new(IdleCodexActionState {
            running: true,
            ..Default::default()
        }));
        let job = IdleCodexJob {
            state: shared.clone(),
            ctx: egui::Context::default(),
            message: "Idle Codex action did not complete; no automatic retry was scheduled.".into(),
        };
        drop(job);
        let state = shared.lock().unwrap();
        assert!(!state.running);
        assert!(!state.confirmation);
        assert!(
            state
                .message
                .as_deref()
                .unwrap()
                .contains("did not complete")
        );
    }
}
