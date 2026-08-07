use eframe::egui;
use std::time::Instant;

mod models;
use models::*;
mod config;
use config::*;
mod audio;
use audio::*;
mod windows;
use windows::*;
mod settings;
use settings::*;
mod launch;
use launch::*;
mod search;
use search::*;
mod app;
use app::{App, BorderOverlay, get_monitors, load_window_size};
mod popups;
use popups::*;
mod diagnostics;
use diagnostics::*;

fn print_help() {
    println!(
        r#"NAME
    applicationlauncher - A sleek application launcher for KDE Wayland in Rust

SYNOPSIS
    applicationlauncher [OPTIONS]

DESCRIPTION
    applicationlauncher is a fast, visually stunning GUI application launcher
    designed for KDE Plasma Wayland. It queries the list of all open window
    objects using kdotool, allows searching them via a fuzzy-matching interface,
    and switches focus to the selected window.

OPTIONS
    -h, --help
        Print this help information and exit.

    --close-on-blur
        Close the launcher window automatically when it loses focus.

    --theme <THEME>
        Force a specific icon theme (default: automatically detected).

    --diagnose
        Ask the running launcher to permit a temporary debugger attachment,
        capture all thread stacks, and write a hang report. This option does
        not start another launcher instance.

OPERATION
    When launched, the application retrieves a list of all open windows using
    kdotool and installed desktop applications from the local system. It renders
    a frameless GUI window containing a search input, a main window list, and an
    application side panel. As you type, both lists are filtered using a fuzzy
    matcher.

    Keyboard Navigation:
        - Up/Down Arrows: Move selected window.
        - Enter: Activate selected window.
        - Escape: Close launcher.
        - F5: Refresh list.
        - F9: Open window history and saved sessions.
        - F10: Open launcher settings.
        - Ctrl+Shift+T: Reopen the newest recently closed window globally,
          except while Chrome, Chromium, or Firefox is active.

EXAMPLES
    applicationlauncher
        Launch the application launcher.

    applicationlauncher --no-close-on-blur
        Launch the application launcher without closing on focus loss.

    applicationlauncher --diagnose
        Capture a report from a currently running, unresponsive launcher.

FILES
    $HOME/.config/applicationlauncher/config.toml
        Optional configuration file (reserved for future use).

    $HOME/.config/applicationlauncher/window_size.txt
        Stores the persisted width and height of the launcher window.

    $HOME/.config/applicationlauncher/pinned_apps.txt
        Stores absolute paths of pinned desktop applications.

    $XDG_STATE_HOME/applicationlauncher/hang-latest.log
        Contains the most recently captured hang report.

    $XDG_STATE_HOME/applicationlauncher/panic-latest.log
        Contains the most recently captured Rust panic and backtrace.

    $XDG_STATE_HOME/applicationlauncher/history.sqlite3
        Private window history, crash recovery, and saved-session database.

    $HOME/.config/systemd/user/applicationlauncherd.service
        Auto-installed persistent window tracker user service.

PATHS
    /usr/share/icons
        System icon themes.
    /usr/share/pixmaps
        Legacy system application icons.

SECURITY NOTES
    Wayland isolates windows from querying each other directly. This tool relies on
    kdotool, which utilizes internal KWin D-Bus scripting interfaces to securely
    interact with KWin.

    --diagnose temporarily allows another same-user process to attach with ptrace.
    The permission is revoked after capture and automatically expires after 60
    seconds if the diagnostic client is interrupted.

EXIT STATUS
    0   Success.
    1   Failure (e.g., kdotool not found or D-Bus communication failed).

AUTHORS
    Terrydaktal <9lewis9@gmail.com>"#
    );
}

fn main() -> eframe::Result {
    install_panic_hook();
    let args: Vec<String> = std::env::args().collect();

    if args.len() >= 7 && args[1] == "--draw-border" {
        let tx: f32 = args[2].parse().unwrap_or(0.0);
        let ty: f32 = args[3].parse().unwrap_or(0.0);
        let tw: f32 = args[4].parse().unwrap_or(100.0);
        let th: f32 = args[5].parse().unwrap_or(100.0);
        let target_center_x = tx + tw / 2.0;
        let target_center_y = ty + th / 2.0;
        let mut mx = 0.0;
        let mut my = 0.0;

        for monitor in get_monitors() {
            let logical_w = monitor.width / monitor.scale;
            let logical_h = monitor.height / monitor.scale;
            if target_center_x >= monitor.x
                && target_center_x <= monitor.x + logical_w
                && target_center_y >= monitor.y
                && target_center_y <= monitor.y + logical_h
            {
                mx = monitor.x;
                my = monitor.y;
                break;
            }
        }

        let local_x = tx - mx;
        let local_y = ty - my;

        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("Border Overlay")
                .with_decorations(false)
                .with_transparent(true)
                .with_always_on_top()
                .with_fullscreen(true)
                .with_mouse_passthrough(true),
            ..Default::default()
        };

        let _ = eframe::run_native(
            "Border Overlay",
            options,
            Box::new(move |_cc| {
                Ok(Box::new(BorderOverlay {
                    start_time: Instant::now(),
                    duration: std::time::Duration::from_millis(250),
                    local_x,
                    local_y,
                    tw,
                    th,
                }))
            }),
        );
        return Ok(());
    }

    let mode = LauncherMode::Windows;
    let diagnose_requested = args.iter().any(|arg| arg == "--diagnose");

    // Single instance check using Unix domain socket
    let socket_path = get_socket_path(mode);
    if socket_path.exists() {
        if diagnose_requested {
            match capture_running_launcher_diagnostics(&socket_path) {
                Ok(path) => {
                    println!("Hang report written to {}", path.display());
                    return Ok(());
                }
                Err(err) => {
                    eprintln!("Diagnostic capture failed: {err}");
                    std::process::exit(1);
                }
            }
        }
        if send_launcher_control_request(&socket_path, "focus\n", false).is_ok() {
            focus_existing_launcher_window();
            return Ok(());
        }
        let _ = std::fs::remove_file(&socket_path);
    }

    if diagnose_requested {
        eprintln!("Diagnostic capture failed: no running launcher was found");
        std::process::exit(1);
    }

    let listener = match std::os::unix::net::UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(_) => return Ok(()),
    };

    let (ui_event_tx, ui_event_rx) = std::sync::mpsc::channel();

    let _lock = SingleInstanceLock { path: socket_path };

    let mut close_on_blur = false;
    let mut force_theme = None;
    let icon_only = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            "--close-on-blur" => {
                close_on_blur = true;
                i += 1;
            }
            "--theme" => {
                if i + 1 < args.len() {
                    force_theme = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("Error: --theme requires a value");
                    std::process::exit(1);
                }
            }
            _ => {
                eprintln!("Error: Unknown argument: {}", args[i]);
                print_help();
                std::process::exit(1);
            }
        }
    }

    let (width, height) = load_window_size();

    let title = "Open Application Windows";

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_inner_size([width, height])
            .with_resizable(true),
        ..Default::default()
    };

    eframe::run_native(
        title,
        options,
        Box::new(move |cc| {
            let repaint_ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => {
                            handle_launcher_control_connection(stream, &ui_event_tx, &repaint_ctx)
                        }
                        Err(_) => break,
                    }
                }
            });

            Ok(Box::new(App::new(
                cc,
                close_on_blur,
                force_theme,
                mode,
                icon_only,
                ui_event_rx,
            )))
        }),
    )
}
