use super::*;

impl App {
    pub(super) fn launch_app_and_exit(&self, app: &AppInfo, ctx: &egui::Context) {
        self.rapid_polling
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if is_dolphin_app(app) {
            launch_dolphin_app();
        } else if !launch_desktop_entry(&app.desktop_file_path) {
            launch_app(&app.exec);
        }
        ctx.request_repaint();
    }

    pub(super) fn open_window_for_app_and_exit(&self, app: &AppInfo, ctx: &egui::Context) {
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

    pub(super) fn open_or_launch_app_and_exit(&self, app: &AppInfo, ctx: &egui::Context) {
        if self.windows.iter().any(|window| {
            self.desktop_file_path_for_window(window).as_ref() == Some(&app.desktop_file_path)
        }) {
            self.open_window_for_app_and_exit(app, ctx);
        } else {
            self.launch_app_and_exit(app, ctx);
        }
    }

    pub(super) fn find_app_for_window<'a>(&'a self, win: &WindowInfo) -> Option<&'a AppInfo> {
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

    pub(super) fn desktop_file_path_for_window(&self, win: &WindowInfo) -> Option<PathBuf> {
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

    pub(super) fn desktop_file_path_for_process(&self, process_name: &str) -> Option<PathBuf> {
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

    pub(super) fn launch_window_app_and_exit(&self, win: &WindowInfo, ctx: &egui::Context) {
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

    pub(super) fn clone_window_and_exit(&self, win: &WindowInfo, ctx: &egui::Context) {
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

    pub(super) fn activate_and_exit(&self, id: String, ctx: &egui::Context) {
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

    pub(super) fn close_window_and_exit(&self, id: String, ctx: &egui::Context) {
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
