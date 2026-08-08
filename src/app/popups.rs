use super::*;

impl App {
    pub(super) fn process_popup_events(&mut self) {
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

    pub(super) fn show_settings_native_viewport(&mut self, ctx: &egui::Context) {
        let Some(shared_state) = self.settings_popup_state.clone() else {
            self.settings_popup_state = Some(Arc::new(std::sync::Mutex::new(
                SettingsWindowState::new(self.launcher_settings_snapshot()),
            )));
            ctx.request_repaint();
            return;
        };

        let state_snapshot = shared_state.lock().ok().map(|mut state| {
            state.flush_pending_save();
            (
                state.settings,
                state.revision,
                (state.scale_anchor / state.settings.ui_scale).clamp(0.2, 5.0),
                state.save_deadline,
            )
        });
        let Some((settings, revision, viewport_scale_factor, save_deadline)) = state_snapshot
        else {
            return;
        };
        if let Some(deadline) = save_deadline {
            ctx.request_repaint_after(
                deadline.saturating_duration_since(std::time::Instant::now()),
            );
        }
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
                                let close_requested =
                                    render_deferred_settings_panel(ui, &mut state);
                                if close_requested {
                                    state.flush_pending_save_now();
                                    let _ = event_sender.send(PopupEvent::CloseSettings);
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                }
                                if state.revision != previous_revision {
                                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                                }
                                if let Some(deadline) = state.save_deadline {
                                    ctx.request_repaint_after(
                                        deadline
                                            .saturating_duration_since(std::time::Instant::now()),
                                    );
                                }
                            });
                    });
            },
        );
    }

    pub(super) fn window_info_popup_data(&self, window_info: &WindowInfo) -> InfoPopupData {
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

    pub(super) fn app_info_popup_data(&self, app_info: &AppInfo) -> InfoPopupData {
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

    pub(super) fn show_settings_popup(&mut self, ctx: &egui::Context) {
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

    pub(super) fn show_window_info_popup(&mut self, ctx: &egui::Context) {
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

    pub(super) fn show_app_info_popup(&mut self, ctx: &egui::Context) {
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
}
