use super::*;
use crate::windows::versions::{ApplicationInfo, ApplicationLaunch};

fn app() -> AppInfo {
    AppInfo {
        name: "Primary desktop name".into(),
        exec: "/fixture/primary".into(),
        icon_path: Some("/fixture/primary.png".into()),
        comment: Some("Primary comment".into()),
        desktop_file_path: "/user/applications/tool.desktop".into(),
        is_settings_module: false,
    }
}

fn related(path: &str, id: &str) -> RelatedEntry {
    RelatedEntry {
        path: path.into(),
        desktop_id: id.into(),
        overridden_by: None,
        ambiguous_precedence: false,
        target: TargetMatch::SameExecutable,
        hidden: Some(false),
        no_display: Some(false),
    }
}

fn desktop(rows: &[InfoPopupRow]) -> &InfoPopupRow {
    let rows = rows
        .iter()
        .filter(|row| row.label == "Desktop file")
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    rows[0]
}

#[test]
fn related_desktops_append_to_the_existing_field_without_replacing_primary_metadata() {
    let primary = app();
    let mut overridden = related("/usr/share/applications/tool.desktop", "tool.desktop");
    overridden.overridden_by = Some(primary.desktop_file_path.clone());
    overridden.target = TargetMatch::DifferentExecutable;
    let alternate = related(
        "/usr/share/applications/tool-private.desktop",
        "tool-private.desktop",
    );
    let files = Ok(vec![
        related(primary.desktop_file_path.to_str().unwrap(), "tool.desktop"),
        overridden,
        alternate.clone(),
        alternate,
    ]);
    let launch = ApplicationLaunch {
        desktop_entry: Some(primary.clone()),
        desktop_exists: Some(true),
        desktop_files: Some(files.clone()),
        ..Default::default()
    };
    let process = App::process_desktop_rows(Some(&launch), false);
    let mut popup = AppInfoPopup::new(primary.clone());
    popup.metadata = Some(ApplicationInfo {
        desktop_exists: Some(true),
        desktop_files: Some(files),
        ..Default::default()
    });
    let application = application_info_rows(&popup);
    assert_eq!(desktop(&process).value, desktop(&application).value);
    for rows in [&process, &application] {
        let row = desktop(rows);
        assert!(
            row.value
                .starts_with("/user/applications/tool.desktop [ID tool.desktop; effective")
        );
        assert!(
            row.value
                .contains("overridden by /user/applications/tool.desktop")
        );
        assert!(row.value.contains("different application target"));
        assert!(
            row.value
                .contains("same on-disk application target; launch options may differ")
        );
        for path in [
            "/user/applications/tool.desktop [",
            "/usr/share/applications/tool.desktop [",
            "/usr/share/applications/tool-private.desktop [",
        ] {
            assert_eq!(row.value.matches(path).count(), 1);
        }
        assert_eq!(row.scope, InfoFieldScope::Installation);
        assert!(row.searched && row.binary_is_current.is_none());
        assert!(
            rows.iter()
                .any(|row| row.label == "Name" && row.value == primary.name)
        );
        assert!(
            rows.iter()
                .any(|row| row.label == "Icon path" && row.value == "/fixture/primary.png")
        );
        assert!(
            !rows.iter().any(|row| row.label.contains("Related desktop")
                || row.label == "Overridden desktop files")
        );
    }
}

#[test]
fn ambiguous_desktop_matches_are_listed_without_inventing_a_primary_entry() {
    let launch = ApplicationLaunch {
        desktop_files: Some(Ok(vec![
            related("/apps/first.desktop", "first.desktop"),
            related("/apps/second.desktop", "second.desktop"),
        ])),
        ..Default::default()
    };
    let rows = App::process_desktop_rows(Some(&launch), false);
    assert_eq!(rows.len(), 1);
    assert!(
        desktop(&rows)
            .value
            .starts_with("No single associated desktop entry; /apps/first.desktop [")
    );
    assert!(
        desktop(&rows)
            .value
            .contains("; /apps/second.desktop [ID second.desktop; effective")
    );
    assert!(!desktop(&rows).value.contains("used to launch"));
}

#[test]
fn desktop_search_errors_missing_paths_hidden_entries_and_unknown_targets_remain_explicit() {
    let primary = app();
    let mut entry = related("/apps/disabled.desktop", "disabled.desktop");
    entry.ambiguous_precedence = true;
    entry.target = TargetMatch::Unresolved;
    entry.hidden = Some(true);
    entry.no_display = Some(true);
    for result in [Ok(vec![entry]), Err("read deadline reached".into())] {
        let launch = ApplicationLaunch {
            desktop_entry: Some(primary.clone()),
            desktop_exists: Some(false),
            desktop_files: Some(result.clone()),
            ..Default::default()
        };
        let rows = App::process_desktop_rows(Some(&launch), false);
        let value = &desktop(&rows).value;
        assert!(value.starts_with("/user/applications/tool.desktop (missing)"));
        if result.is_ok() {
            assert!(value.contains("precedence ambiguous"));
            assert!(value.contains("application target unverified"));
            assert!(value.contains("Hidden=true") && value.contains("NoDisplay=true"));
        } else {
            assert!(
                value.contains("[related desktop-file search unavailable: read deadline reached]")
            );
        }
    }
    let rows = App::process_desktop_rows(Some(&ApplicationLaunch::default()), false);
    assert_eq!(desktop(&rows).value, "None");
}

#[test]
fn long_related_desktop_values_render_and_copy_as_one_complete_field_at_high_dpi() {
    let path = format!("/apps/{}/tool.desktop", "long-path-é/".repeat(40));
    let launch = ApplicationLaunch {
        desktop_entry: Some(app()),
        desktop_exists: Some(true),
        desktop_files: Some(Ok(vec![related(&path, "different.desktop")])),
        ..Default::default()
    };
    let data = InfoPopupData {
        title: "Desktop-file fixture".into(),
        heading: "Desktop files".into(),
        subtitle: String::new(),
        scroll_sensitivity: 1.0,
        rows: App::process_desktop_rows(Some(&launch), false),
        process_tree: None,
    };
    let value = desktop(&data.rows).value.clone();
    for scale in [1.0, 2.0] {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(scale);
        let frame = |events| {
            let mut editor = None;
            let output = ctx.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        editor = crate::popups::render_info_popup_panel(ui, &data)
                            .0
                            .into_iter()
                            .next();
                    });
                },
            );
            (output, editor.unwrap())
        };
        let (_, editor) = frame(Vec::new());
        let text = &editor.galley.job.text;
        let start = text.find(&value).unwrap();
        let start = text[..start].chars().count();
        let mut state = editor.state;
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(start),
                egui::text::CCursor::new(start + value.chars().count()),
            )));
        state.store(&ctx, editor.response.id);
        ctx.memory_mut(|memory| memory.request_focus(editor.response.id));
        let (output, _) = frame(vec![egui::Event::Copy]);
        assert!(output.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::CopyText(copied) if copied == &value)
        }));
    }
}
