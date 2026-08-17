use eframe::egui;
use std::time::Instant;

use applicationlauncher::diagnostic_capture::{CaptureOptions, capture_auto, run_debug_doctor};
use applicationlauncher::observability::{self, Component};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

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
        Alias for --diagnose auto.

    --diagnose auto [--perf] [--core]
        Independently capture the running GUI and daemon: repeated all-thread
        stacks, bounded /proc state, semantic snapshots, loaded modules,
        relevant journal records, and checksums. --perf adds a short profile.
        --core explicitly adds full live cores, which may contain secrets.

    debug-doctor
        Verify build IDs, symbolization data, diagnostic attachment, output
        permissions, required tools, and flight-recorder budgets.

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

    applicationlauncher --close-on-blur
        Launch the application launcher and close it when focus is lost.

    applicationlauncher --diagnose auto
        Capture bounded evidence from the running GUI and daemon.

    applicationlauncher --diagnose auto --perf
        Add a short call-graph profile for each running component.

FILES
    $HOME/.config/applicationlauncher/config.toml
        Optional configuration file (reserved for future use).

    $HOME/.config/applicationlauncher/window_size.txt
        Stores the persisted width and height of the launcher window.

    $HOME/.config/applicationlauncher/pinned_apps.txt
        Stores absolute paths of pinned desktop applications.

    $XDG_STATE_HOME/applicationlauncher/diagnostics/
        Contains bounded, checksummed GUI and daemon diagnostic bundles.

    $XDG_STATE_HOME/applicationlauncher/panic-gui-latest.log
        Contains the most recently captured GUI Rust panic and backtrace.

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
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|argument| argument == "--build-id") {
        println!("{}", applicationlauncher::BUILD_ID);
        return Ok(());
    }

    if let Some(position) = args
        .iter()
        .position(|argument| argument == "--diagnostic-probe")
    {
        observability::initialize(Component::Gui);
        install_panic_hook();
        let probe = args.get(position + 1).map(String::as_str).ok_or_else(|| {
            eframe::Error::AppCreation("--diagnostic-probe requires a kind".into())
        })?;
        if let Err(err) = applicationlauncher::diagnostic_capture::run_internal_probe(probe) {
            eprintln!("Diagnostic probe failed: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }

    if args.iter().any(|argument| argument == "debug-doctor") {
        observability::initialize(Component::Test);
        observability::install_panic_hook(Component::Test);
        match run_debug_doctor() {
            Ok((path, report)) => {
                println!("Debug doctor report written to {}", path.display());
                for check in &report.checks {
                    println!(
                        "{} {}: {}",
                        if check.passed { "PASS" } else { "FAIL" },
                        check.name,
                        check.detail
                    );
                }
                if !report.passed() {
                    std::process::exit(1);
                }
            }
            Err(err) => {
                eprintln!("Debug doctor failed: {err}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    if let Some(position) = args.iter().position(|argument| argument == "--diagnose") {
        observability::initialize(Component::Test);
        observability::install_panic_hook(Component::Test);
        if let Some(mode) = args.get(position + 1)
            && !mode.starts_with('-')
            && mode != "auto"
        {
            eprintln!("Unsupported diagnostic mode {mode}; expected auto");
            std::process::exit(1);
        }
        let options = CaptureOptions {
            include_perf: args.iter().any(|argument| argument == "--perf"),
            include_core: args.iter().any(|argument| argument == "--core"),
            ..CaptureOptions::default()
        };
        if options.include_core {
            eprintln!(
                "Full cores were explicitly requested; core files can contain unredacted secrets"
            );
        }
        match capture_auto(options) {
            Ok(path) => println!("Diagnostic bundle written to {}", path.display()),
            Err(err) => {
                eprintln!("Diagnostic capture failed: {err}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    if args
        .iter()
        .skip(1)
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        print_help();
        return Ok(());
    }

    observability::initialize(Component::Gui);
    install_panic_hook();

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
    let source_changes_pending = std::env::var_os("APPLICATIONLAUNCHER_SOURCE_CHANGES_PENDING")
        .is_some_and(|value| value == "1");
    if args.iter().any(|arg| arg == "--shutdown") {
        let socket_path = get_socket_path(mode);
        if let Err(err) = send_launcher_control_request(&socket_path, "shutdown\n", true) {
            eprintln!("Could not shut down the running launcher: {err}");
        }
        return Ok(());
    }

    // Single instance check using Unix domain socket
    let socket_path = get_socket_path(mode);
    if let Some(parent) = socket_path.parent() {
        let _ = std::fs::create_dir_all(parent);
        if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
            #[cfg(unix)]
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    let listener = match std::os::unix::net::UnixListener::bind(&socket_path) {
        Ok(listener) => listener,
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
            let focus_request = if source_changes_pending {
                "focus-source-changes-pending\n"
            } else {
                "focus\n"
            };
            if send_launcher_control_request(&socket_path, focus_request, false).is_ok() {
                focus_existing_launcher_window();
                return Ok(());
            }
            // No listener answered, so this is a stale socket. Only remove it
            // after bind established that it is blocking this instance.
            if let Err(remove_err) = std::fs::remove_file(&socket_path) {
                eprintln!("Could not remove stale launcher socket: {remove_err}");
                return Ok(());
            }
            match std::os::unix::net::UnixListener::bind(&socket_path) {
                Ok(listener) => listener,
                Err(err) => {
                    eprintln!("Could not reclaim stale launcher socket: {err}");
                    return Ok(());
                }
            }
        }
        Err(err) => {
            eprintln!("Could not bind launcher control socket: {err}");
            return Ok(());
        }
    };

    let (ui_event_tx, ui_event_rx) = std::sync::mpsc::channel();

    let _lock = match SingleInstanceLock::new(socket_path) {
        Ok(lock) => lock,
        Err(err) => {
            eprintln!("Could not own launcher control socket: {err}");
            return Ok(());
        }
    };
    let _diagnostic_server = match observability::start_diagnostic_server(Component::Gui) {
        Ok(server) => Some(server),
        Err(err) => {
            eprintln!("Could not start the independent GUI diagnostic endpoint: {err}");
            None
        }
    };
    let _gui_worker = observability::register_worker("gui-main");

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
            observability::spawn_named("gui-control", move |worker| {
                worker.set_state("accepting");
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
                source_changes_pending,
                ui_event_rx,
            )))
        }),
    )
}
