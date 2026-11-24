use super::*;
use crate::windows::versions::{ProcessVersion, ProgsMetadata, ScriptApplication};
use program_metadata::interpreted::script::{self, ScriptCandidate, ScriptComparison};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::time::{Duration, Instant};

fn shell_version() -> ProcessVersion {
    ProcessVersion {
        runtime_name: Some("Shell"),
        script_application: Some(ScriptApplication {
            name: "pi-remote-qwen-radiance".into(),
            target: "/proc/self/fd/3".into(),
            kind: "Shell script",
            entry_point: None,
            descriptor: None,
            identification: "Captured retained script identity",
        }),
        progs_metadata: ProgsMetadata {
            description: Some("The Bash interpreter, not the script".into()),
            capabilities: Some("CLI • Primary role: Command-line tool".into()),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn value<'a>(rows: &'a [InfoPopupRow], label: &str) -> &'a str {
    &rows
        .iter()
        .find(|row| row.label == label)
        .unwrap_or_else(|| panic!("missing {label}"))
        .value
}

fn retained_version(comparison: ScriptComparison) -> (File, ProcessVersion) {
    let descriptor = unsafe {
        libc::memfd_create(
            c"pi-remote-qwen-radiance".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    assert!(descriptor >= 0, "{}", std::io::Error::last_os_error());
    let mut file = unsafe { File::from_raw_fd(descriptor) };
    file.write_all(b"retained script\n").unwrap();
    let target = format!("/proc/self/fd/{}", file.as_raw_fd());
    let application = script::invocation_from_process_args(
        "Shell",
        &["bash", &target],
        None,
        std::process::id() as i32,
    )
    .unwrap()
    .application;
    let mut details = application
        .descriptor
        .as_ref()
        .unwrap()
        .inspect_details(None, Instant::now() + Duration::from_secs(2))
        .unwrap();
    // Real source/guard, synthetic public display values: these are component
    // presentation oracles, not a test of backend hashing/candidate discovery.
    details.sealed_write = Some(true);
    details.sealed_resize = Some(true);
    details.sha256 = Some("a".repeat(64));
    details.candidate = Some(ScriptCandidate {
        route: format!(
            "/home/fixture/{}/pi-remote-qwen-radiance",
            "long path π/".repeat(24)
        )
        .into(),
        resolved_path: "/home/fixture/Dev/wrappers/pi-remote-qwen-radiance".into(),
        sha256: Some(if comparison == ScriptComparison::Same {
            "a".repeat(64)
        } else {
            "b".repeat(64)
        }),
    });
    details.comparison = comparison;
    details.comparison_reason = Some("Complete retained/current-candidate comparison".into());
    let mut version = shell_version();
    version.script_application = Some(application);
    version.script_details = Some(details);
    (file, version)
}

fn shell_node(executable: Option<&str>) -> ProcessInfoNode {
    ProcessInfoNode {
        identity: ProcessIdentity {
            pid: 42,
            start_ticks: 1,
        },
        parent_pid: 1,
        depth: 1,
        name: "bash".into(),
        script_name: Some("pi-remote-qwen-radiance".into()),
        header: "bash: pi-remote-qwen-radiance (pid 42)".into(),
        executable: executable.map(PathBuf::from),
        executable_identity: None,
        executable_error: None,
        cwd: None,
        cwd_error: None,
        raw_argv0: Some("/usr/bin/bash".into()),
        command_line: None,
    }
}

#[test]
fn shell_script_suffix_does_not_borrow_a_different_scripts_comparison_or_file_stem() {
    let (_file, mut version) = retained_version(ScriptComparison::Different);
    let mut node = shell_node(Some("/usr/bin/bash"));
    // Synthetic display records isolate correspondence with the captured tree;
    // real descriptor/source identity remains tested by the backend fixtures.
    for (application_name, storage_target, header_name, expected) in [
        (
            "new-script",
            "/memfd:new-script (deleted)",
            "old-script",
            None,
        ),
        ("tool", "/tmp/tool.sh", "tool.sh", Some(false)),
        ("tool", "/tmp/tool.sh", "tool", None),
        ("工具", "/tmp/工具.sh", "工具.sh", Some(false)),
    ] {
        version.script_application.as_mut().unwrap().name = application_name.into();
        version.script_details.as_mut().unwrap().storage_target = storage_target.into();
        node.script_name = Some(header_name.into());
        node.header = format!("bash: {header_name} (pid 42)").into();
        let span = process_script_header(&node, Some(&version)).unwrap();
        assert_eq!(
            span.is_current, expected,
            "{header_name} / {storage_target}"
        );
        assert_eq!(&node.header[span.name_range], header_name);
    }
}

#[test]
fn shell_script_sections_name_the_script_and_exact_interpreter_with_content_only_colour() {
    let node = shell_node(Some("/usr/bin/bash"));
    for (comparison, expected) in [
        (ScriptComparison::Same, Some(true)),
        (ScriptComparison::Different, Some(false)),
        (ScriptComparison::Unknown, None),
    ] {
        let (_file, mut version) = retained_version(comparison);
        // Today's installation must not override the captured running interpreter.
        version.launch = Some(crate::windows::versions::ApplicationLaunch {
            executable: Some("/usr/bin/zsh".into()),
            ..Default::default()
        });
        for native in [Some(true), Some(false), None] {
            version.running_latest_binary = native;
            let span = process_script_header(&node, Some(&version)).unwrap();
            let prefix = "bash: ";
            assert_eq!(
                span.name_range,
                prefix.len()..prefix.len() + "pi-remote-qwen-radiance".len()
            );
            assert_eq!(
                &node.header[span.name_range.clone()],
                "pi-remote-qwen-radiance"
            );
            assert_eq!(span.is_current, expected);
            let sections = hosted_metadata_sections(
                &egui::Context::default(),
                egui::Id::new(("shell-labels", native)),
                &node,
                Some(&version),
            );
            assert_eq!(sections.len(), 1);
            let section = &sections[0];
            assert_eq!(section.header.as_ref(), "Script  pi-remote-qwen-radiance");
            let field = section.field_header.as_ref().unwrap();
            assert_eq!(
                &section.header[field.value_range.clone()],
                "pi-remote-qwen-radiance"
            );
            assert!(matches!(field.label_scope, InfoFieldScope::InstanceContext));
            assert_eq!(section.binary_is_current, expected);
            assert_eq!(value(&section.rows, "Interpreter"), "Bash");
            assert_eq!(value(&section.rows, "Kind"), "Shell script");
            assert!(!section.rows.iter().any(|row| row.label == "Runtime"));
            assert_eq!(
                section
                    .rows
                    .iter()
                    .find(|row| row.label == "Interpreter")
                    .unwrap()
                    .scope,
                InfoFieldScope::InstanceContext
            );
        }
    }
}

#[test]
fn shell_interpreter_names_require_recognized_captured_executable_not_argv_or_installation() {
    let mut version = shell_version();
    version.launch = Some(crate::windows::versions::ApplicationLaunch {
        executable: Some("/usr/bin/bash".into()),
        ..Default::default()
    });
    for (path, expected) in [
        (Some("/usr/bin/bash"), "Bash"),
        (Some("/usr/bin/bash (deleted)"), "Bash"),
        (Some("/usr/bin/dash"), "Dash"),
        (Some("/usr/bin/zsh"), "Zsh"),
        (Some("/usr/bin/fish"), "Fish"),
        (Some("/usr/bin/ksh"), "Ksh"),
        (Some("/usr/bin/mksh"), "Mksh"),
        (Some("/usr/bin/sh"), "sh"),
        (Some("/usr/bin/renamed-runtime"), "Unknown"),
        (None, "Unknown"),
    ] {
        let sections = hosted_metadata_sections(
            &egui::Context::default(),
            egui::Id::new(("shell-identity", path)),
            &shell_node(path),
            Some(&version),
        );
        assert_eq!(value(&sections[0].rows, "Interpreter"), expected);
    }
    assert_eq!(
        value(
            &application_version_rows("Application", Some(&version), false),
            "Interpreter"
        ),
        "Unknown"
    );
}

#[test]
fn shell_script_heading_comparison_is_neutral_for_missing_error_or_stale_evidence() {
    for failure in ["missing", "error", "stale", "no script"] {
        let (_file, mut version) = retained_version(ScriptComparison::Different);
        match failure {
            "missing" => version.script_details = None,
            "error" => version.script_details_error = Some("descriptor changed".into()),
            "stale" => version.running_unavailable = Some("process changed"),
            "no script" => version.script_application = None,
            _ => unreachable!(),
        }
        let sections = hosted_metadata_sections(
            &egui::Context::default(),
            egui::Id::new(failure),
            &shell_node(Some("/usr/bin/bash")),
            Some(&version),
        );
        assert_eq!(sections[0].binary_is_current, None, "{failure}");
        assert!(
            !sections[0]
                .rows
                .iter()
                .any(|row| row.label == "Running version"),
            "{failure} shell details must not restore an unknown running version"
        );
        let span =
            process_script_header(&shell_node(Some("/usr/bin/bash")), Some(&version)).unwrap();
        assert_eq!(span.is_current, None, "{failure}");
        if failure == "stale" {
            assert_eq!(value(&sections[0].rows, "Interpreter"), "Unknown");
        }
    }
}

#[test]
fn python_script_field_heading_uses_captured_comparison_not_interpreter_freshness() {
    for (comparison, expected) in [
        (ScriptComparison::Same, Some(true)),
        (ScriptComparison::Different, Some(false)),
        (ScriptComparison::Unknown, None),
    ] {
        let (_file, mut version) = retained_version(comparison);
        version.runtime_name = Some("Python");
        version.script_application.as_mut().unwrap().kind = "Python script";
        let mut node = shell_node(Some("/usr/bin/python3.14"));
        node.name = "python3".into();
        node.header = "python3: pi-remote-qwen-radiance (pid 42)".into();
        for native in [Some(true), Some(false), None] {
            version.running_latest_binary = native;
            let sections = hosted_metadata_sections(
                &egui::Context::default(),
                egui::Id::new(("python-script-comparison", native)),
                &node,
                Some(&version),
            );
            let section = &sections[0];
            assert_eq!(section.header.as_ref(), "Script  pi-remote-qwen-radiance");
            let field = section.field_header.as_ref().expect("aligned script field");
            assert_eq!(
                &section.header[field.value_range.clone()],
                "pi-remote-qwen-radiance"
            );
            assert_eq!(field.label_scope, InfoFieldScope::InstanceContext);
            assert_eq!(section.header_scope, None);
            assert_eq!(section.binary_is_current, expected);
            let span = process_script_header(&node, Some(&version)).unwrap();
            assert_eq!(&node.header[span.name_range], "pi-remote-qwen-radiance");
            assert_eq!(span.is_current, expected);
        }
        for failure in ["missing", "error", "stale"] {
            let mut unavailable = version.clone();
            match failure {
                "missing" => unavailable.script_details = None,
                "error" => unavailable.script_details_error = Some("descriptor changed".into()),
                "stale" => unavailable.running_unavailable = Some("process changed"),
                _ => unreachable!(),
            }
            let sections = hosted_metadata_sections(
                &egui::Context::default(),
                egui::Id::new(failure),
                &node,
                Some(&unavailable),
            );
            assert_eq!(sections[0].binary_is_current, None, "{failure}");
            assert_eq!(
                process_script_header(&node, Some(&unavailable))
                    .unwrap()
                    .is_current,
                None
            );
        }
    }
}

#[test]
fn non_shell_hosted_labels_and_heading_status_do_not_borrow_shell_content_comparisons() {
    for (runtime, kind) in [
        ("Node.js", "JavaScript script"),
        ("Java", "Java source"),
        ("Electron", "JavaScript script"),
    ] {
        let (_file, mut version) = retained_version(ScriptComparison::Different);
        version.runtime_name = Some(runtime);
        version.script_application.as_mut().unwrap().kind = kind;
        let sections = hosted_metadata_sections(
            &egui::Context::default(),
            egui::Id::new(("non-shell-labels", runtime)),
            &shell_node(Some("/usr/bin/bash")),
            Some(&version),
        );
        let section = &sections[0];
        assert_eq!(section.binary_is_current, None);
        assert_eq!(value(&section.rows, "Runtime"), runtime);
        assert_eq!(
            value(&section.rows, "Build status"),
            "Unknown (no application build identity published)"
        );
        assert_eq!(
            value(&section.rows, "Running version"),
            "Unknown (no application build identity published)"
        );
        assert!(!section.rows.iter().any(|row| row.label == "Interpreter"));
        assert!(section.header.starts_with("Application: "));
    }
}

#[test]
fn retained_shell_script_omits_build_status_without_borrowed_runtime_roles() {
    let mut version = shell_version();
    version.application_progs_metadata.source_checkout = Some("/fixture/script-source".into());
    let mut rows = application_version_rows("Application", Some(&version), false);
    assert!(!rows.iter().any(|row| row.label == "Source directory"));
    assert_eq!(
        version
            .application_progs_metadata
            .source_checkout
            .as_deref(),
        Some(Path::new("/fixture/script-source"))
    );
    assert_eq!(value(&rows, "Kind"), "Shell script");
    assert!(!rows.iter().any(|row| row.label == "Build status"));
    assert!(!rows.iter().any(|row| row.label == "Running version"));
    assert!(
        !rows
            .iter()
            .any(|row| row.value == "Not applicable (interpreted shell script)")
    );
    assert_eq!(value(&rows, "Description"), "Unavailable");
    assert_eq!(value(&rows, "Capabilities"), "Unavailable");
    assert!(!rows.iter().any(|row| row.label == "Package"));
    append_desktop_entry_rows(&mut rows, desktop_entry_rows(None, None, false, None));
    assert_eq!(value(&rows, "Desktop file"), "None");
    assert!(
        !rows
            .iter()
            .any(|row| matches!(row.label.as_str(), "Icon path" | "Name" | "Comment"))
    );
    assert!(
        rows.iter()
            .all(|row| !row.value.contains("The Bash interpreter"))
    );
}

#[test]
fn retained_script_rows_show_captured_observations_without_promoting_wrapper_candidates() {
    for (comparison, status, colour) in [
        (ScriptComparison::Same, "Same (", Some(true)),
        (ScriptComparison::Different, "Different (", Some(false)),
        (ScriptComparison::Unknown, "Unknown", None),
    ] {
        let (_file, version) = retained_version(comparison);
        let details = version.script_details.as_ref().unwrap();
        let candidate = details.candidate.as_ref().unwrap();
        let mut rows = application_version_rows("Application", Some(&version), false);
        for (label, expected) in [
            ("Script storage", "Memory-backed file (memfd)".to_owned()),
            ("Retained script size", "16 bytes".to_owned()),
            (
                "Retained descriptor path",
                details.retained_path.display().to_string(),
            ),
            (
                "Retained storage target",
                "/memfd:pi-remote-qwen-radiance (deleted)".to_owned(),
            ),
            ("Write sealing", "Sealed".to_owned()),
            ("Resize sealing", "Sealed".to_owned()),
            ("Retained SHA-256", "a".repeat(64)),
            (
                "Installed wrapper candidate",
                candidate.route.display().to_string(),
            ),
            (
                "Resolved wrapper candidate",
                candidate.resolved_path.display().to_string(),
            ),
            (
                "Current candidate SHA-256",
                candidate.sha256.clone().unwrap(),
            ),
        ] {
            assert_eq!(value(&rows, label), expected, "{label}");
        }
        assert!(value(&rows, "Script content comparison").starts_with(status));
        assert!(
            value(&rows, "Script content comparison")
                .contains("Complete retained/current-candidate comparison")
        );
        assert!(value(&rows, "Candidate association").contains(
            "not proof of the original launch path, package ownership or application version"
        ));
        if comparison != ScriptComparison::Unknown {
            assert!(
                value(&rows, "Script content comparison")
                    .contains("original launch path remains unproven")
            );
        }
        for label in ["Retained SHA-256", "Script content comparison"] {
            assert_eq!(
                rows.iter()
                    .find(|row| row.label == label)
                    .unwrap()
                    .binary_is_current,
                colour
            );
        }
        assert_eq!(
            rows.iter()
                .find(|row| row.label == "Current candidate SHA-256")
                .unwrap()
                .binary_is_current,
            None
        );
        assert!(!rows.iter().any(|row| row.label == "Package"));
        assert_eq!(value(&rows, "Launch chain"), "Unavailable");
        assert!(value(&rows, "Installed version").starts_with("Unknown"));
        assert!(!rows.iter().any(|row| row.label == "Running version"));
        assert_eq!(value(&rows, "Capabilities"), "Unavailable");
        append_desktop_entry_rows(&mut rows, desktop_entry_rows(None, None, false, None));
        assert_eq!(value(&rows, "Desktop file"), "None");
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row.label.as_str(), "Icon path" | "Name" | "Comment"))
        );
    }
}

#[test]
fn retained_script_missing_observations_and_errors_stay_explicit_and_neutral() {
    let (_file, mut version) = retained_version(ScriptComparison::Unknown);
    let details = version.script_details.as_mut().unwrap();
    details.sealed_write = Some(false);
    details.sealed_resize = None;
    details.sha256 = None;
    details.candidate = None;
    details.comparison_reason = Some("Hash inspection reached its deadline".into());
    let rows = application_version_rows("Application", Some(&version), false);
    assert!(!rows.iter().any(|row| row.label == "Running version"));
    assert_eq!(value(&rows, "Write sealing"), "Not sealed");
    assert_eq!(
        value(&rows, "Resize sealing"),
        "Unavailable (storage does not report seals)"
    );
    assert_eq!(
        value(&rows, "Retained SHA-256"),
        "Unavailable (hash not captured)"
    );
    assert_eq!(
        value(&rows, "Installed wrapper candidate"),
        "None identified (original launch path not inferred)"
    );
    assert_eq!(
        value(&rows, "Script content comparison"),
        "Unknown — Hash inspection reached its deadline"
    );
    assert!(rows.iter().all(|row| row.binary_is_current.is_none()));

    version.script_details = None;
    let rows = application_version_rows("Application", Some(&version), false);
    assert!(!rows.iter().any(|row| row.label == "Running version"));
    assert_eq!(
        value(&rows, "Retained script details"),
        "Unknown (retained-script details unavailable)"
    );
    version.script_details_error = Some("permission denied reading retained descriptor".into());
    let rows = application_version_rows("Application", Some(&version), false);
    assert!(!rows.iter().any(|row| row.label == "Running version"));
    assert_eq!(
        value(&rows, "Retained script details"),
        "Unavailable (permission denied reading retained descriptor)"
    );
    assert!(!rows.iter().any(|row| row.label == "Retained SHA-256"));
}

fn frame(
    ctx: &egui::Context,
    data: &InfoPopupData,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, egui::text_edit::TextEditOutput) {
    let mut editor = None;
    let output = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(640.0, 480.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let rendered = crate::popups::render_info_popup_scroll_area(ui, data);
                editor = rendered.inner.0.into_iter().next();
            });
        },
    );
    (output, editor.unwrap())
}

#[test]
fn retained_script_long_paths_and_hashes_render_and_copy_without_truncation() {
    for scale in [1.0, 2.0] {
        for (comparison, colour) in [
            (
                ScriptComparison::Same,
                egui::Color32::from_rgb(130, 225, 145),
            ),
            (
                ScriptComparison::Different,
                egui::Color32::from_rgb(255, 110, 110),
            ),
        ] {
            let (_file, version) = retained_version(comparison);
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(scale);
            let data = InfoPopupData {
                title: "Retained script details".into(),
                heading: "pi-remote-qwen-radiance".into(),
                subtitle: "Captured process observations and a qualified current wrapper candidate"
                    .into(),
                scroll_sensitivity: 1.0,
                rows: application_version_rows("Application", Some(&version), false),
                process_tree: None,
            };
            let (_, editor) = frame(&ctx, &data, Vec::new());
            assert!(editor.galley.rect.width() > 640.0);
            let job = &editor.galley.job;
            let all_text = job.text.clone();
            for row in &data.rows {
                assert!(all_text.contains(&row.label), "{}", row.label);
                assert!(all_text.contains(&row.value), "{}", row.label);
            }
            for (label, expected) in [
                ("Retained SHA-256", colour),
                ("Script content comparison", colour),
                ("Current candidate SHA-256", egui::Color32::WHITE),
            ] {
                let label_start = job.text.find(label).unwrap();
                let value_start = label_start
                    + job.text[label_start..]
                        .find(value(&data.rows, label))
                        .unwrap();
                let section = job
                    .sections
                    .iter()
                    .find(|section| section.byte_range.contains(&value_start))
                    .unwrap();
                assert_eq!(section.format.color, expected, "{label}");
            }
            let mut state = editor.state;
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(all_text.chars().count()),
                )));
            state.store(&ctx, editor.response.id);
            ctx.memory_mut(|memory| memory.request_focus(editor.response.id));
            let (output, _) = frame(&ctx, &data, vec![egui::Event::Copy]);
            let copies: Vec<_> = output
                .platform_output
                .commands
                .iter()
                .filter_map(|command| match command {
                    egui::OutputCommand::CopyText(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(copies, [all_text.as_str()]);
        }
    }
}
