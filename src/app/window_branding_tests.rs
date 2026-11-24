//! Headless branding contracts. These never initialise App/session workers,
//! create native OS windows, access the compositor, or change user settings.
use eframe::egui;
use std::collections::HashSet;
use std::sync::{Arc, atomic::AtomicBool};

fn expected_child_identity(viewport: egui::ViewportId) -> String {
    // Preserve the existing externally-used foreground identity, independent of
    // window title and of the new shared branding implementation.
    format!(
        "applicationlauncher-info-{}-{:016x}",
        std::process::id(),
        viewport.0.value()
    )
}

fn assert_branding(builder: &egui::ViewportBuilder, expected: &str) {
    assert_eq!(builder.app_id.as_deref(), Some(expected));
    let icon = builder.icon.as_ref().expect("bundled launcher logo");
    assert!(Arc::ptr_eq(icon, &crate::icon::window_icon()));
    assert_eq!((icon.width, icon.height), (128, 128));
}

#[test]
fn auxiliary_window_branding_emits_distinct_stable_ids_and_the_shared_logo() {
    let ctx = egui::Context::default();
    ctx.set_embed_viewports(false);
    let cases = [
        ("launcher_settings_popup", "Launcher Settings"),
        ("launcher_history_popup", "Window History and Sessions"),
        ("terminal_maintenance_popup", "Update Terminal Windows"),
    ];
    let mut identities = HashSet::new();
    for suffix in ["", " — title changed"] {
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            for (key, title) in cases {
                let id = egui::ViewportId::from_hash_of(key);
                ctx.show_viewport_deferred(
                    id,
                    crate::icon::viewport(id).with_title(format!("{title}{suffix}")),
                    |_, _| {},
                );
            }
        });
        for (key, title) in cases {
            let id = egui::ViewportId::from_hash_of(key);
            let viewport = &output.viewport_output[&id];
            let expected = expected_child_identity(id);
            assert_branding(&viewport.builder, &expected);
            assert_eq!(
                viewport.builder.title.as_deref(),
                Some(format!("{title}{suffix}").as_str())
            );
            assert_ne!(expected, "applicationlauncher");
            identities.insert(expected);
        }
        assert_eq!(identities.len(), cases.len());
    }
    assert_branding(
        &crate::icon::viewport(egui::ViewportId::ROOT),
        "applicationlauncher",
    );
    let overlay = egui::ViewportId::from_hash_of("launcher_border_overlay");
    let expected = expected_child_identity(overlay);
    assert_branding(&crate::icon::viewport(overlay), &expected);
    assert!(!identities.contains(&expected));
}

#[test]
fn actual_show_info_builder_keeps_its_unique_focus_identity_and_shared_logo() {
    let ctx = egui::Context::default();
    ctx.set_embed_viewports(false);
    let id = egui::ViewportId::from_hash_of("branding-info-window");
    let expected = expected_child_identity(id);
    let (sender, _receiver) = std::sync::mpsc::channel();
    let output = ctx.run(egui::RawInput::default(), |ctx| {
        crate::popups::show_deferred_info_popup(
            ctx,
            id,
            crate::models::InfoPopupData {
                title: "Info: independent title π".into(),
                heading: "Captured fixture".into(),
                subtitle: "No worker or filesystem inspection".into(),
                scroll_sensitivity: 1.0,
                rows: Vec::new(),
                process_tree: None,
            },
            [1200.0, 800.0],
            [520.0, 360.0],
            Arc::new(AtomicBool::new(false)),
            sender.clone(),
        );
    });
    let viewport = &output.viewport_output[&id];
    assert_branding(&viewport.builder, &expected);
    assert_eq!(
        viewport.builder.title.as_deref(),
        Some("Info: independent title π")
    );
    assert_eq!(viewport.builder.inner_size, Some(egui::vec2(1200.0, 800.0)));
    assert_eq!(
        viewport.builder.min_inner_size,
        Some(egui::vec2(520.0, 360.0))
    );
    assert_eq!(viewport.builder.resizable, Some(true));
    assert!(viewport.viewport_ui_cb.is_some());
}

fn scope<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing {start}"))
        .1
        .split_once(end)
        .unwrap_or_else(|| panic!("missing {end}"))
        .0
}

#[test]
fn all_production_native_window_builders_use_shared_branding_with_existing_viewport_ids() {
    // App::new starts live audio/KWin/tracker workers. This narrow wiring check
    // complements the real egui builder/output tests above without launching
    // those services or duplicating App's large state as a test-only constructor.
    let app = include_str!("mod.rs");
    let popups = include_str!("popups.rs");
    let terminal = include_str!("terminal_maintenance.rs");
    let main = include_str!("../main.rs");
    let info = include_str!("../popups.rs");
    let cases = [
        (
            "settings",
            scope(
                popups,
                "fn show_settings_native_viewport(",
                "fn window_info_popup_data(",
            ),
            "crate::icon::viewport(egui::ViewportId::from_hash_of(\"launcher_settings_popup\"))",
        ),
        (
            "history",
            scope(
                app,
                "fn show_history_native_viewport(",
                "pub(crate) fn new(",
            ),
            "crate::icon::viewport(egui::ViewportId::from_hash_of(\"launcher_history_popup\"))",
        ),
        (
            "terminal maintenance",
            scope(terminal, "fn show_terminal_maintenance(", "fn close_popup("),
            "crate::icon::viewport(egui::ViewportId::from_hash_of(POPUP_ID))",
        ),
        (
            "Show Info",
            scope(
                info,
                "pub(crate) fn show_deferred_info_popup(",
                "#[cfg(test)]",
            ),
            "crate::icon::viewport(viewport_id)",
        ),
        (
            "main/info-only root",
            scope(main, "fn launcher_native_options(", "mod popups;"),
            "icon::viewport(egui::ViewportId::ROOT)",
        ),
        (
            "border overlay",
            scope(main, "args[1] == \"--draw-border\"", "let terminal_info ="),
            "icon::viewport(egui::ViewportId::from_hash_of(\"launcher_border_overlay\"))",
        ),
    ];
    for (name, body, expected) in cases {
        let compact = body.split_whitespace().collect::<String>();
        assert!(
            compact.contains(expected),
            "{name} bypassed branded viewport/changed ID"
        );
        assert_eq!(compact.matches("icon::viewport(").count(), 1, "{name}");
        assert!(
            !compact.contains("ViewportBuilder::default()"),
            "{name} constructs an unbranded native viewport"
        );
    }
    assert!(terminal.contains("const POPUP_ID: &str = \"terminal_maintenance_popup\";"));
}
