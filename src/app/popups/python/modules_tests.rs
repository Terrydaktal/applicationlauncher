use super::*;
use crate::models::{InfoPopupData, InfoProcessSection, InfoProcessTree};
use program_metadata::interpreted::python_process::{
    PythonDependency, PythonProcessMetadata, PythonProjectModule, PythonProjectModules,
};

const ENTRY: &str = "/fixture/project/main.py";
const HELPER: &str = "/fixture/project/helpers/helper_é.py";
const CONFIG: &str = "/fixture/project/other/Config.py";
const EVIDENCE: &str = "Static on-disk references; not a loaded-module list";

fn report(complete: bool) -> PythonProjectModules {
    let mut report = PythonProjectModules::default();
    report.modules = vec![
        // A defensive presentation boundary: the entry already has Script path.
        PythonProjectModule {
            path: ENTRY.into(),
            referenced_by: Vec::new(),
            conditional: false,
        },
        PythonProjectModule {
            path: HELPER.into(),
            referenced_by: vec![ENTRY.into(), CONFIG.into()],
            conditional: false,
        },
        PythonProjectModule {
            path: CONFIG.into(),
            referenced_by: vec![HELPER.into()],
            conditional: true,
        },
    ];
    report.complete = complete;
    if !complete {
        report.notes = vec!["Dynamic import target could not be resolved: café.plugins".into()];
    }
    report
}

fn version(report: Option<PythonProjectModules>) -> ProcessVersion {
    let mut context = PythonProcessMetadata::default();
    context.project_modules = report;
    context.dependencies = vec![PythonDependency {
        name: "requests".into(),
        requirement: "requests>=2".into(),
        installed_version: Some("2.32".into()),
        installed_lookup_complete: true,
        ambiguous: false,
    }];
    ProcessVersion {
        runtime_name: Some("Python"),
        running_latest_binary: Some(false),
        python_context: Some(context),
        script_application: Some(crate::windows::versions::ScriptApplication {
            name: "main".into(),
            target: ENTRY.into(),
            kind: "Python script",
            entry_point: Some(ENTRY.into()),
            descriptor: None,
            identification: "fixture arguments; not loaded-code evidence",
        }),
        ..Default::default()
    }
}

fn set_open(ctx: &egui::Context, id: egui::Id, open: bool) {
    let mut state =
        egui::collapsing_header::CollapsingState::load_with_default_open(ctx, id, false);
    state.set_open(open);
    state.store(ctx);
}

#[test]
fn python_project_module_sections_are_cached_scoped_and_closed_by_default() {
    let ctx = egui::Context::default();
    let parent = egui::Id::new("python-module-model");
    let version = version(Some(report(true)));
    let closed = module_sections(&ctx, parent, Some(&version));
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].id, parent.with("python-project-modules"));
    assert_eq!(closed[0].header.as_ref(), "Referenced project modules (2)");
    assert!(!closed[0].default_open && closed[0].rows.is_empty());
    assert!(closed[0].binary_is_current.is_none());
    assert_ne!(
        closed[0].id,
        dependency_sections(&ctx, parent, Some(&version))[0].id
    );
    set_open(&ctx, closed[0].id, true);
    let expanded = module_sections(&ctx, parent, Some(&version));
    let rows = &expanded[0].rows;
    assert_eq!(rows[0].value, EVIDENCE);
    assert_eq!(rows.iter().filter(|row| row.label == "main.py").count(), 0);
    let helper = rows.iter().find(|row| row.label == "helper_é.py").unwrap();
    assert_eq!(
        helper.value,
        format!("{HELPER} · referenced by: {ENTRY}; {CONFIG}")
    );
    let config = rows.iter().find(|row| row.label == "Config.py").unwrap();
    assert_eq!(
        config.value,
        format!("{CONFIG} · conditional reference · referenced by: {HELPER}")
    );
    assert!(
        rows.iter()
            .all(|row| row.scope == InfoFieldScope::Installation)
    );
    assert!(rows.iter().all(|row| row.binary_is_current.is_none()));
    assert!(!rows.iter().any(|row| row.value.contains("requests")));
    assert!(
        !rows
            .iter()
            .any(|row| row.label.contains("Source directory") || row.label.contains("Git"))
    );
    assert!(
        module_sections(&ctx, egui::Id::new("other-script"), Some(&version))[0]
            .rows
            .is_empty()
    );
}

#[test]
fn python_project_module_sections_distinguish_unknown_empty_and_incomplete_reports() {
    let ctx = egui::Context::default();
    let parent = egui::Id::new("python-module-uncertainty");
    assert!(module_sections(&ctx, parent, None).is_empty());
    assert!(module_sections(&ctx, parent, Some(&ProcessVersion::default())).is_empty());
    set_open(&ctx, parent.with("python-project-modules"), true);
    let unknown = version(None);
    let unknown = module_sections(&ctx, parent, Some(&unknown));
    assert_eq!(
        unknown[0].header.as_ref(),
        "Referenced project modules (unknown)"
    );
    assert!(
        unknown[0]
            .rows
            .iter()
            .any(|row| row.value.starts_with("Unknown"))
    );
    assert!(
        !unknown[0]
            .rows
            .iter()
            .any(|row| row.value.starts_with("No statically"))
    );

    let mut empty = PythonProjectModules::default();
    empty.complete = true;
    let empty = version(Some(empty));
    let empty = module_sections(&ctx, parent, Some(&empty));
    assert_eq!(empty[0].header.as_ref(), "Referenced project modules (0)");
    assert!(
        empty[0]
            .rows
            .iter()
            .any(|row| row.value == "No statically referenced project modules found")
    );

    for populated in [false, true] {
        let mut incomplete = report(false);
        if !populated {
            incomplete.modules.clear();
        }
        let incomplete = version(Some(incomplete));
        let incomplete = module_sections(&ctx, parent, Some(&incomplete));
        assert!(incomplete[0].header.ends_with(" — incomplete"));
        assert!(
            incomplete[0]
                .rows
                .iter()
                .any(|row| row.value.contains("Static analysis is incomplete"))
        );
        assert!(
            incomplete[0]
                .rows
                .iter()
                .any(|row| row.value.contains("café.plugins"))
        );
        assert!(
            !incomplete[0]
                .rows
                .iter()
                .any(|row| row.value.starts_with("No statically"))
        );
    }
}

fn node() -> ProcessInfoNode {
    ProcessInfoNode {
        identity: crate::models::ProcessIdentity {
            pid: 42,
            start_ticks: 100,
        },
        parent_pid: 1,
        depth: 0,
        name: "python3".into(),
        script_name: Some("main.py".into()),
        header: "python3 (pid 42)".into(),
        executable: Some("/fixture/bin/python3".into()),
        executable_identity: Some((1, 2)),
        executable_error: None,
        cwd: None,
        cwd_error: None,
        raw_argv0: None,
        command_line: None,
    }
}

fn process_id() -> egui::Id {
    egui::Id::new("python-modules-renderer")
}

fn frame(
    ctx: &egui::Context,
    version: &ProcessVersion,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, Vec<egui::text_edit::TextEditOutput>) {
    let mut editors = Vec::new();
    let output = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(2000.0, 1800.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            let data = InfoPopupData {
                title: "Window Info".into(),
                heading: "Python module fixture".into(),
                subtitle: "Static references are not loaded-code evidence".into(),
                scroll_sensitivity: 1.0,
                rows: Vec::new(),
                process_tree: Some(InfoProcessTree {
                    notice: "Fixture snapshot".into(),
                    image_loader_helper_count: 0,
                    sections: vec![InfoProcessSection {
                        id: process_id(),
                        depth: 0,
                        header: "python3 (pid 42)".into(),
                        binary_is_current: Some(false),
                        script_header: None,
                        update_target: None,
                        default_open: true,
                        metadata_sections: hosted_metadata_sections(
                            ctx,
                            process_id(),
                            &node(),
                            Some(version),
                        ),
                        rows: vec![row(
                            "",
                            InfoFieldScope::RunningInstance,
                            "version",
                            "Python 3.14.0".into(),
                        )],
                    }],
                }),
            };
            egui::CentralPanel::default().show(ctx, |ui| {
                editors = crate::popups::render_info_popup_panel(ui, &data).0;
            });
        },
    );
    (output, editors)
}

fn click(ctx: &egui::Context, version: &ProcessVersion, id: egui::Id) {
    let pos = ctx
        .read_response(id.with("tree-arrow"))
        .unwrap()
        .rect
        .center();
    let pointer = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    frame(
        ctx,
        version,
        vec![
            egui::Event::PointerMoved(pos),
            pointer(true),
            pointer(false),
        ],
    );
}

#[test]
fn python_project_modules_render_select_copy_and_collapse_independently_of_dependencies() {
    for scale in [1.0, 2.0] {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(scale);
        ctx.style_mut(|style| style.animation_time = 0.0);
        let version = version(Some(report(false)));
        let app = process_id().with("hosted-application");
        let modules = app.with("python-project-modules");
        let dependencies = app.with("python-dependencies");
        let (_, initial) = frame(&ctx, &version, Vec::new());
        assert_eq!(initial.len(), 1);
        let text = &initial[0].galley.job.text;
        assert!(text.contains("Referenced project modules (2) — incomplete"));
        assert!(text.contains("Declared dependencies (1)"));
        assert!(!text.contains(HELPER) && !text.contains("requests>=2"));

        click(&ctx, &version, modules);
        let (_, expanded) = frame(&ctx, &version, Vec::new());
        let editor = &expanded[0];
        let text = &editor.galley.job.text;
        for expected in [
            "helper_é.py",
            HELPER,
            CONFIG,
            EVIDENCE,
            "conditional reference",
            "café.plugins",
        ] {
            assert!(text.contains(expected), "missing {expected} at {scale}x");
        }
        assert!(!text.contains("Helper_é.py") && !text.contains("requests>=2"));
        for path in [HELPER, CONFIG] {
            let start = text.find(path).unwrap();
            let range = start..start + path.len();
            assert!(
                editor
                    .galley
                    .job
                    .sections
                    .iter()
                    .filter(|part| {
                        part.byte_range.start < range.end && part.byte_range.end > range.start
                    })
                    .all(|part| part.format.color == egui::Color32::WHITE),
                "static module paths cannot inherit freshness colour"
            );
        }
        let start_byte = text.find("helper_é.py").unwrap();
        let end_byte = text.find("café.plugins").unwrap() + "café.plugins".len();
        let expected = text[start_byte..end_byte].to_owned();
        let mut state = egui::text_edit::TextEditState::load(&ctx, editor.response.id).unwrap();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(text[..start_byte].chars().count()),
                egui::text::CCursor::new(text[..end_byte].chars().count()),
            )));
        state.store(&ctx, editor.response.id);
        ctx.memory_mut(|memory| memory.request_focus(editor.response.id));
        let (output, _) = frame(&ctx, &version, vec![egui::Event::Copy]);
        assert!(
            output
                .platform_output
                .commands
                .iter()
                .any(|command| matches!(command,
                    egui::OutputCommand::CopyText(text) if text == &expected
                ))
        );

        click(&ctx, &version, dependencies);
        let (_, both) = frame(&ctx, &version, Vec::new());
        assert!(both[0].galley.job.text.contains(HELPER));
        assert!(both[0].galley.job.text.contains("requests>=2"));
        click(&ctx, &version, modules);
        let (_, closed) = frame(&ctx, &version, Vec::new());
        assert!(!closed[0].galley.job.text.contains(HELPER));
        assert!(closed[0].galley.job.text.contains("requests>=2"));
        assert!(closed[0].galley.job.text.contains("Python 3.14.0"));
        click(&ctx, &version, app);
        let (_, script_closed) = frame(&ctx, &version, Vec::new());
        assert!(
            !script_closed[0]
                .galley
                .job
                .text
                .contains("Referenced project modules")
        );
        click(&ctx, &version, app);
        let (_, restored) = frame(&ctx, &version, Vec::new());
        assert!(
            restored[0]
                .galley
                .job
                .text
                .contains("Referenced project modules")
        );
        assert!(!restored[0].galley.job.text.contains(HELPER));
        assert!(restored[0].galley.job.text.contains("requests>=2"));
    }
}
