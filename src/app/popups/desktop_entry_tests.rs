use super::*;
use crate::windows::versions::{ApplicationLaunch, ProcessVersion, ScriptApplication};

fn entry(path: &str) -> AppInfo {
    AppInfo {
        name: "Desktop name".into(),
        exec: "/fixture/tool".into(),
        desktop_file_path: path.into(),
        icon_path: Some("/fixture/tool.png".into()),
        comment: Some("Desktop comment".into()),
        is_settings_module: false,
    }
}

fn assert_no_unassociated_fields(rows: &[InfoPopupRow]) {
    assert!(
        !rows
            .iter()
            .any(|row| matches!(row.label.as_str(), "Icon path" | "Name" | "Comment")),
        "desktop-only fields without an associated desktop file: {:?}",
        rows.iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn absent_desktop_entries_emit_only_the_existing_desktop_status_row() {
    let empty_entry = entry("");
    for associated in [None, Some(&empty_entry)] {
        for exists in [None, Some(false), Some(true)] {
            for pending in [false, true] {
                let rows = desktop_entry_rows(
                    associated,
                    exists,
                    pending,
                    Some(Path::new("/fixture/fallback.png")),
                );
                assert_no_unassociated_fields(&rows);
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].label, "Desktop file");
                assert_eq!(
                    rows[0].value,
                    if associated.is_none() && pending {
                        "Loading..."
                    } else {
                        "None"
                    }
                );
            }
        }
    }
}

#[test]
fn native_and_installed_info_omit_fields_without_a_desktop_association() {
    let launch = ApplicationLaunch {
        desktop_entry: Some(entry("")),
        ..Default::default()
    };
    for pending in [false, true] {
        for launch in [None, Some(&launch)] {
            let rows = App::process_desktop_rows(launch, pending);
            assert_no_unassociated_fields(&rows);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].label, "Desktop file");
        }
    }
    let mut popup = AppInfoPopup::new(entry(""));
    for ready in [false, true] {
        popup.metadata = ready.then(Default::default);
        let rows = application_info_rows(&popup);
        assert_no_unassociated_fields(&rows);
        assert!(
            rows.iter()
                .any(|row| row.label == "Desktop file" && row.value == "None")
        );
    }
}

#[test]
fn hosted_info_does_not_inherit_desktop_only_fields_without_association() {
    let node = ProcessInfoNode {
        identity: ProcessIdentity {
            pid: 42,
            start_ticks: 1,
        },
        parent_pid: 1,
        depth: 1,
        name: "runtime".into(),
        script_name: Some("tool".into()),
        header: "runtime: tool (pid 42)".into(),
        executable: Some("/fixture/runtime".into()),
        executable_identity: None,
        executable_error: None,
        cwd: None,
        cwd_error: None,
        raw_argv0: None,
        command_line: None,
    };
    for runtime in ["Shell", "Node.js", "Python"] {
        for associated in [None, Some(entry(""))] {
            let version = ProcessVersion {
                runtime_name: Some(runtime),
                script_application: Some(ScriptApplication {
                    name: "tool".into(),
                    target: "/fixture/tool".into(),
                    kind: match runtime {
                        "Python" => "Python script",
                        "Node.js" => "JavaScript script",
                        _ => "Shell script",
                    },
                    entry_point: Some("/fixture/tool".into()),
                    identification: "Captured argument vector",
                    descriptor: None,
                }),
                application_launch: Some(ApplicationLaunch {
                    desktop_entry: associated,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let sections = hosted_metadata_sections(
                &egui::Context::default(),
                egui::Id::new(("desktop-fields", runtime)),
                &node,
                Some(&version),
            );
            assert_eq!(sections.len(), 1);
            assert_no_unassociated_fields(&sections[0].rows);
            if runtime != "Python" {
                assert!(
                    sections[0]
                        .rows
                        .iter()
                        .any(|row| row.label == "Desktop file")
                );
            }
        }
    }
}

#[test]
fn associated_desktop_fields_remain_visible_even_with_unavailable_metadata() {
    for exists in [None, Some(false), Some(true)] {
        for pending in [false, true] {
            let mut associated = entry("/fixture/tool.desktop");
            associated.icon_path = None;
            associated.comment = None;
            let rows = desktop_entry_rows(Some(&associated), exists, pending, None);
            assert_eq!(
                rows.iter()
                    .map(|row| row.label.as_str())
                    .collect::<Vec<_>>(),
                ["Desktop file", "Icon path", "Name", "Comment"]
            );
            assert_eq!(rows[1].value, "-");
            assert_eq!(rows[2].value, "Desktop name");
            assert_eq!(rows[3].value, "-");
        }
    }
}
