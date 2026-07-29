use eframe::egui;
use std::path::PathBuf;

use crate::app::App;
use crate::models::{LauncherSettings, SettingsWindowState};

impl SettingsWindowState {
    pub(crate) fn new(settings: LauncherSettings) -> Self {
        Self {
            pending_ui_scale: settings.ui_scale,
            scale_anchor: settings.ui_scale,
            settings,
            revision: 0,
        }
    }

    pub(crate) fn save_changed_settings(&mut self) {
        save_launcher_settings(self.settings);
        self.revision = self.revision.wrapping_add(1);
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

    if settings_changed {
        state.save_changed_settings();
    }

    ui.add_space(16.0);
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
    let mut settings = LauncherSettings::default();

    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(format!("{}/.config/applicationlauncher/settings.txt", home));
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

    settings
}

pub(crate) fn save_launcher_settings(settings: LauncherSettings) {
    if let Ok(home) = std::env::var("HOME") {
        let dir = PathBuf::from(format!("{}/.config/applicationlauncher", home));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.txt");
        let content = format!(
            "show_system_settings_modules={}\napp_icon_mode={}\nwin_icon_size={:.1}\nwin_top_padding={:.1}\nwin_bottom_padding={:.1}\nwin_left_padding={:.1}\nwin_right_padding={:.1}\nwin_row_height={:.1}\nwin_text_spacing={:.1}\nwin_line_height={:.1}\nwin_show_path={}\nshow_run_in_terminal={}\nshow_cd_in_terminal={}\nauto_send_enter_on_attention={}\nwin_title_size={:.1}\nwin_path_size={:.1}\napp_icon_size={:.1}\napp_icon_tile_size={:.1}\napp_top_padding={:.1}\napp_bottom_padding={:.1}\napp_left_padding={:.1}\napp_right_padding={:.1}\napp_icon_show_name={}\napp_icon_name_size={:.1}\ndisable_ibeam={}\napp_scroll_sensitivity={:.2}\nwin_scroll_sensitivity={:.2}\nui_scale={:.2}\n",
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
            settings.ui_scale
        );
        let _ = std::fs::write(path, content);
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
