//! Presentation contract for independently published metadata fields.
use super::*;
use crate::windows::versions::{ProcessVersion, ProgsMetadata};

const CHECKING: &str = "Checking recorded build inputs…";

fn row<'a>(rows: &'a [InfoPopupRow], label: &str) -> &'a InfoPopupRow {
    rows.iter()
        .find(|row| row.label == label)
        .unwrap_or_else(|| panic!("missing progressive metadata row: {label}"))
}

fn staged_version() -> ProcessVersion {
    ProcessVersion {
        version: Some("fixture 1.2.3".into()),
        build_id: Some("fixture-running-image".into()),
        latest_version: Some("fixture 1.2.3".into()),
        latest_build_id: Some("fixture-running-image".into()),
        running_latest_binary: Some(true),
        installation: Some("[FRK] Fork (Source-managed)".into()),
        package: Some("fixture-package 1.2.3-1".into()),
        progs_metadata: ProgsMetadata {
            description: Some("Already available description".into()),
            capabilities: Some("CLI".into()),
            source_checkout: Some("/fixture/not-opened/checkout".into()),
            ..Default::default()
        },
        latest_development: Some(program_metadata::development::Metadata {
            source_only: true,
            source_checkout: Some("/fixture/not-opened/checkout".into()),
            source_languages: vec!["Rust".into()],
            build_system: vec!["Cargo".into()],
            ..Default::default()
        }),
        uncompiled_changes: Some(CHECKING.into()),
        ..Default::default()
    }
}

#[test]
fn progressive_header_rows_keep_version_and_current_marker_while_missing_details_are_loading() {
    let version = ProcessVersion {
        details_pending: true,
        version: Some("captured running version".into()),
        build_id: Some("captured-image".into()),
        running_latest_binary: Some(true),
        ..Default::default()
    };
    for role in ["Application", "Runtime"] {
        let rows = process_version_rows(role, Some(&version), false);
        let running = row(&rows, VERSION_FIELD_LABEL);
        assert_eq!(running.value, "captured running version (captured-image)");
        assert_eq!(running.binary_is_current, Some(true));
        for label in [
            "Type",
            "Description",
            "Capabilities",
            "Application languages (on disk)",
            "Uncompiled changes",
            LATEST_VERSION_FIELD_LABEL,
            "Package",
            "Launch chain",
        ] {
            assert_eq!(row(&rows, label).value, "Loading...", "{role}: {label}");
        }
        assert!(!rows.iter().any(|row| row.label == "Build system (on disk)"));
    }
    assert_eq!(version.process_build_is_current(), Some(true));
}

#[test]
fn progressive_source_check_rows_show_known_metadata_immediately_and_replace_only_its_pending_field()
 {
    let mut version = staged_version();
    let initial = process_version_rows("Application", Some(&version), false);
    assert_eq!(
        row(&initial, VERSION_FIELD_LABEL).value,
        "fixture 1.2.3 (fixture-running-image)"
    );
    assert_eq!(
        row(&initial, VERSION_FIELD_LABEL).binary_is_current,
        Some(true)
    );
    assert_eq!(
        row(&initial, LATEST_VERSION_FIELD_LABEL).value,
        "fixture 1.2.3 (fixture-running-image)"
    );
    assert_eq!(
        row(&initial, "Application languages (source checkout)").value,
        "Rust (primary)"
    );
    assert_eq!(
        row(&initial, "Build system (source checkout)").value,
        "Cargo"
    );
    assert!(!initial.iter().any(|row| row.label == "Package"));
    assert_eq!(
        row(&initial, "Description").value,
        "Already available description"
    );
    assert_eq!(row(&initial, "Uncompiled changes").value, CHECKING);
    assert!(initial.iter().all(|row| row.value != "Loading..."));

    let complete =
        "0 newer · 1 missing · added: unknown · 3111 inputs checked · full build status unknown";
    version.uncompiled_changes = Some(complete.into());
    let finished = process_version_rows("Application", Some(&version), false);
    assert_eq!(row(&finished, "Uncompiled changes").value, complete);
    assert_eq!(finished.len(), initial.len());
    for (before, after) in initial.iter().zip(&finished) {
        assert_eq!(before.label, after.label);
        assert_eq!(before.scope, after.scope);
        assert_eq!(before.searched, after.searched);
        assert_eq!(before.separator_before, after.separator_before);
        assert_eq!(before.binary_is_current, after.binary_is_current);
        if before.label != "Uncompiled changes" {
            assert_eq!(
                before.value, after.value,
                "unrelated field changed: {}",
                before.label
            );
        }
    }
}

#[test]
fn progressive_source_check_rendering_keeps_pending_neutral_and_completed_status_colours() {
    for (status, colour) in [
        (CHECKING, egui::Color32::WHITE),
        (
            "0 newer · 0 missing · added: unknown · 42 inputs checked · full build status unknown",
            egui::Color32::from_rgb(130, 225, 145),
        ),
        (
            "1 newer · 0 missing · added: unknown · 42 inputs checked",
            egui::Color32::from_rgb(255, 110, 110),
        ),
        (
            "Unknown (on-disk executable changed during inspection)",
            egui::Color32::from_rgb(255, 220, 100),
        ),
    ] {
        let mut version = staged_version();
        version.uncompiled_changes = Some(status.into());
        let data = InfoPopupData {
            title: "Progressive metadata fixture".into(),
            heading: String::new(),
            subtitle: String::new(),
            scroll_sensitivity: 1.0,
            rows: process_version_rows("Application", Some(&version), false),
            process_tree: None,
        };
        for scale in [1.0, 2.0] {
            let context = egui::Context::default();
            context.set_pixels_per_point(scale);
            let mut job = None;
            let _ = context.run(egui::RawInput::default(), |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    let rendered = crate::popups::render_info_popup_scroll_area(ui, &data);
                    job = rendered
                        .inner
                        .0
                        .first()
                        .map(|editor| editor.galley.job.clone());
                });
            });
            let job = job.expect("progressive metadata did not render");
            for available in ["fixture 1.2.3", "Rust (primary)", "Cargo"] {
                assert!(
                    job.text.contains(available),
                    "missing rendered metadata: {available}"
                );
            }
            assert!(!job.text.contains("fixture-package 1.2.3-1"));
            let start = job
                .text
                .find(status)
                .expect("source status is not rendered");
            let section = job
                .sections
                .iter()
                .find(|section| section.byte_range.contains(&start))
                .unwrap();
            assert_eq!(section.format.color, colour, "{scale}x: {status}");
        }
    }
}
