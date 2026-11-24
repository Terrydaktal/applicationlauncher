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
                let _ = applicationlauncher::process::spawn_and_reap(cmd);
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
        if self.windows.iter().any(|window| {
            window.id == id
                && window.pid == Some(std::process::id() as i32)
                && window.raw_title == LAUNCHER_WINDOW_TITLE
        }) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        if let Some(ref kpath) = self.kdotool_path {
            let kpath = kpath.clone();
            let id_clone = id.clone();
            std::thread::spawn(move || {
                // Query geometry first
                let geom = get_window_geometry(&kpath, &id_clone);

                // Activate the window and raise it to make sure it comes to the top
                let _ = applicationlauncher::process::status_with_timeout(
                    {
                        let mut command = Command::new(&kpath);
                        command.args(["windowactivate", &id_clone, "windowraise", &id_clone]);
                        command
                    },
                    Duration::from_secs(3),
                );

                if let Some(geometry) = geom {
                    let overlay = border_overlay_command(geometry, &id_clone);
                    if let Err(error) = applicationlauncher::process::spawn_and_reap(overlay) {
                        applicationlauncher::diagnostic_log!(
                            "Could not show activation border for window {id_clone}: {error}"
                        );
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
                let _ = applicationlauncher::process::status_with_timeout(
                    {
                        let mut command = Command::new(&kpath);
                        command.args(["windowclose", &id]);
                        command
                    },
                    Duration::from_secs(3),
                );
            });
        }
        ctx.request_repaint();
    }
}

fn border_overlay_command((x, y, width, height): (f32, f32, f32, f32), id: &str) -> Command {
    // Pin the running image across rebuilds. /proc/self/exe would name systemd-run.
    let mut command = Command::new(format!("/proc/{}/exe", std::process::id()));
    command.args([
        "--draw-border",
        &x.to_string(),
        &y.to_string(),
        &width.to_string(),
        &height.to_string(),
        id,
    ]);
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn activation_border_uses_the_launcher_pid_and_preserves_its_arguments() {
        let command = border_overlay_command((1.5, -2.0, 800.25, 600.0), "{window id}");
        assert_eq!(
            command.get_program(),
            OsStr::new(&format!("/proc/{}/exe", std::process::id()))
        );
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["--draw-border", "1.5", "-2", "800.25", "600", "{window id}"].map(OsStr::new)
        );
    }

    #[test]
    fn activation_border_survives_replaced_launcher_binary() {
        const CHILD: &str = "APPLICATIONLAUNCHER_BORDER_REEXEC_TEST";
        const TEST: &str =
            "app::commands::tests::activation_border_survives_replaced_launcher_binary";

        if let Some(path) = std::env::var_os(CHILD).map(PathBuf::from) {
            assert_eq!(fs::read_link("/proc/self/exe").unwrap(), path);
            fs::remove_file(path).unwrap();
            let deleted_path = std::env::current_exe().unwrap();
            let error = Command::new(deleted_path)
                .arg("--list")
                .output()
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound);

            let overlay = border_overlay_command((1.0, 2.0, 800.0, 600.0), "{window}");
            // A different intermediary executable catches accidental /proc/self/exe use.
            let mut probe = Command::new("/bin/sh");
            probe
                .args(["-c", "exec \"$1\" --list", "border-helper-probe"])
                .arg(overlay.get_program());
            let output =
                applicationlauncher::process::output_with_timeout(probe, Duration::from_secs(3))
                    .unwrap();
            assert!(output.status.success(), "{:?}", output.stderr);
            assert!(String::from_utf8_lossy(&output.stdout).contains(TEST));
            return;
        }

        struct Directory(PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let directory = Directory(std::env::temp_dir().join(format!(
            "applicationlauncher-border-reexec-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
        fs::create_dir(&directory.0).unwrap();
        let executable = fs::canonicalize(&directory.0).unwrap().join("launcher");
        fs::copy("/proc/self/exe", &executable).unwrap();
        let mut child = Command::new(&executable);
        child
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, &executable);
        let output =
            applicationlauncher::process::output_with_timeout(child, Duration::from_secs(8))
                .unwrap();
        assert!(
            output.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
}
