use applicationlauncher::tracker::{ApplicationUpdateReport, TerminalUpgradeReport, TrackerClient};
use eframe::egui;
use std::sync::{Arc, Mutex};

use crate::models::{IdleCodexActionState, LauncherSettings, SettingsWindowState};

#[test]
fn window_context_menu_starts_with_show_info_and_labels_close_as_close_window() {
    let view = include_str!("view.rs");
    let menu = view
        .split_once("overlay_response.clone().context_menu(|ui| {")
        .unwrap()
        .1
        .split_once("LauncherMode::Windows => {")
        .unwrap()
        .1
        .split_once("// Volume Control")
        .unwrap()
        .0;
    let first_button = menu.split_once("ui.button(").unwrap().1;
    assert!(first_button.starts_with("\"Show info\""));
    assert_eq!(menu.matches("ui.button(\"Show info\")").count(), 1);
    assert!(view.contains("ui.button(\"Close window\")"));
    assert!(!view.contains("ui.button(\"Close application\")"));
}

#[test]
fn maintenance_actions_are_absent_from_menu_settings_and_popup_wiring() {
    let view = include_str!("view.rs");
    for removed in [
        "Update application...",
        "open_application_update(",
        "show_application_update(",
        "application_update_is_open(",
    ] {
        assert!(!view.contains(removed), "Obsolete window UI: {removed}");
    }
    assert!(!include_str!("mod.rs").contains("mod application_update;"));

    let deferred = include_str!("../settings.rs")
        .split_once("pub(crate) fn render_deferred_settings_panel(")
        .unwrap()
        .1
        .split_once("pub(crate) fn load_launcher_settings(")
        .unwrap()
        .0;
    for settings in [include_str!("settings.rs"), deferred] {
        assert!(settings.contains("Close Settings (F10)"));
        for removed in [
            "Terminal Maintenance",
            "Update outdated terminal windows...",
            "Stop idle Codex sessions...",
            "terminal_maintenance::settings_button(",
            "render_idle_codex_action(",
        ] {
            assert!(
                !settings.contains(removed),
                "Obsolete settings UI: {removed}"
            );
        }
    }
}

#[test]
fn deferred_settings_hide_maintenance_controls_without_touching_retained_jobs() {
    let ctx = egui::Context::default();
    let idle: Arc<Mutex<IdleCodexActionState>> = Arc::default();
    let terminal = Arc::default();
    let mut state = SettingsWindowState::new(
        LauncherSettings::default(),
        idle.clone(),
        Arc::clone(&terminal),
    );
    idle.lock().unwrap().confirmation = true;
    for _ in 0..3 {
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 8000.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    assert!(!crate::settings::render_deferred_settings_panel(
                        ui, &mut state
                    ));
                });
            },
        );
        let text: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                _ => None,
            })
            .collect();
        assert!(text.contains(&"Launcher Settings"));
        assert!(text.contains(&"Close Settings (F10)"));
        for removed in [
            "Terminal Maintenance",
            "Update outdated terminal windows...",
            "Stop idle Codex sessions...",
            "Interrupt idle Codex",
        ] {
            assert!(!text.iter().any(|text| text.contains(removed)));
        }
        assert!(ctx.memory(|memory| memory.top_modal_layer().is_none()));
        assert_eq!(state.revision, 0);
        let idle = idle.lock().unwrap();
        assert!(idle.confirmation);
        assert!(!idle.running && idle.message.is_none());
        let terminal = terminal.lock().unwrap();
        assert!(!terminal.open && !terminal.worker_running && terminal.report.is_none());
    }
}

#[test]
fn maintenance_backend_apis_remain_available_without_invoking_them() {
    let _: fn(&TrackerClient, &str) -> Result<String, String> =
        TrackerClient::begin_application_update_preview;
    let _: fn(&TrackerClient, &str) -> Result<ApplicationUpdateReport, String> =
        TrackerClient::application_update_report;
    let _: fn(&TrackerClient, &str, &str, bool) -> Result<ApplicationUpdateReport, String> =
        TrackerClient::start_application_update;
    let _: fn(&TrackerClient) -> Result<String, String> =
        TrackerClient::begin_terminal_upgrade_preview;
    let _: fn(&TrackerClient, &str) -> Result<TerminalUpgradeReport, String> =
        TrackerClient::terminal_upgrade_report;
    let _: fn(&TrackerClient, &str, &[String], bool) -> Result<TerminalUpgradeReport, String> =
        TrackerClient::start_terminal_upgrade;
    let _: fn() -> Result<String, String> = crate::windows::terminal::idle::stop_idle_codex;
}
