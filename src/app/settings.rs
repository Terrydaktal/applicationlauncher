use super::*;

impl App {
    pub(super) fn render_settings_panel(&mut self, ui: &mut egui::Ui) -> bool {
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
                    egui::RichText::new("Show Last Activation:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut show_last_activation = self.win_show_last_activation;
                if ui.checkbox(&mut show_last_activation, "").changed() {
                    self.win_show_last_activation = show_last_activation;
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
}
