use super::*;
use program_metadata::interpreted::python_process::{
    DistributionMetadata, PythonDependency, PythonEnvironment, PythonProcessMetadata,
    SourceRepository,
};

fn fixture() -> ProcessVersion {
    ProcessVersion {
        runtime_name: Some("Python"),
        python_context: Some(PythonProcessMetadata {
            arguments: vec![
                "--port".into(),
                "8000".into(),
                "".into(),
                "two words".into(),
            ],
            interpreter_options: vec!["-u".into()],
            interpreter_launch: Some("/fixture/.venv/bin/python".into()),
            environment: Some(PythonEnvironment {
                root: "/fixture/.venv".into(),
                kind: "Virtual environment",
                configuration: Some("/fixture/.venv/pyvenv.cfg".into()),
                base_executable: Some("/usr/bin/python3.14".into()),
                base_directory: Some("/usr/bin".into()),
                created_with: Some("uv 0.9.1".into()),
                includes_system_packages: Some(false),
                package_directories: vec!["/fixture/.venv/lib/python3.14/site-packages".into()],
            }),
            distribution: Some(DistributionMetadata {
                installer: Some("uv".into()),
                location: "/fixture/.venv/lib/python3.14/site-packages".into(),
                entry_point: Some("viewer = viewer.cli:main".into()),
            }),
            dependencies: vec![PythonDependency {
                name: "requests".into(),
                requirement: "requests>=2; python_version >= '3.10'".into(),
                installed_version: Some("2.32".into()),
                installed_lookup_complete: true,
                ambiguous: false,
            }],
            script_sha256: Some("a".repeat(64)),
            repository: Some(SourceRepository {
                root: "/fixture".into(),
                revision: Some("b".repeat(40)),
                branch: Some("main".into()),
            }),
            ..Default::default()
        }),
        application_installed_metadata: Some(program_metadata::interpreted::InstalledVersion {
            name: "viewer".into(),
            version: "1.2".into(),
            kind: "Python distribution",
            metadata_path: "/fixture/viewer.dist-info/METADATA".into(),
            identification: "exact fixture entry".into(),
        }),
        script_application: Some(crate::windows::versions::ScriptApplication {
            name: "serve_viewer".into(),
            target: "/fixture/serve_viewer.py".into(),
            kind: "Python script",
            entry_point: Some("/fixture/serve_viewer.py".into()),
            descriptor: None,
            identification: "process arguments fixture",
        }),
        ..Default::default()
    }
}

#[test]
fn python_details_keep_live_context_separate_from_disk_metadata_and_do_not_claim_loaded_code() {
    let version = fixture();
    let rows = application_version_rows("Application", Some(&version), false);
    for label in [
        "Interpreter",
        "Script path",
        "Script arguments",
        "Interpreter options",
        "Virtual environment",
        "Environment interpreter",
    ] {
        let row = rows.iter().find(|row| row.label == label).unwrap();
        assert_eq!(row.scope, InfoFieldScope::InstanceContext, "{label}");
    }
    let arguments = &rows
        .iter()
        .find(|row| row.label == "Script arguments")
        .unwrap()
        .value;
    assert_eq!(
        serde_json::from_str::<Vec<String>>(arguments).unwrap(),
        ["--port", "8000", "", "two words"]
    );
    for label in [
        "Environment configuration",
        "Base interpreter (configured)",
        "Package installer",
        "Installed entry point",
        "Script SHA256 (on disk)",
    ] {
        let row = rows.iter().find(|row| row.label == label).unwrap();
        assert_eq!(row.scope, InfoFieldScope::Installation, "{label}");
    }
    assert!(rows.iter().all(|row| row.binary_is_current.is_none()));
    assert!(!rows.iter().any(|row| row.label == "Runtime"));
    for absent in [
        "Running version",
        "Uncompiled changes",
        "Desktop file",
        "Package installation directory",
        "Base interpreter directory (configured)",
        "Source repository",
        "Source revision (on disk)",
        "Source branch",
    ] {
        assert!(
            rows.iter().all(|row| row.label != absent),
            "redundant or unproven field: {absent}"
        );
    }
    let repository = version
        .python_context
        .as_ref()
        .unwrap()
        .repository
        .as_ref()
        .unwrap();
    assert_eq!(repository.root, Path::new("/fixture"));
    assert_eq!(
        repository.revision.as_deref(),
        Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
    );
    assert_eq!(repository.branch.as_deref(), Some("main"));
    let first_installation = rows
        .iter()
        .position(|row| row.scope == InfoFieldScope::Installation)
        .unwrap();
    assert!(
        rows[first_installation..]
            .iter()
            .all(|row| row.scope == InfoFieldScope::Installation)
    );
}

#[test]
fn python_interpreter_names_use_only_captured_running_cpython_object_evidence() {
    use program_metadata::executable::VersionSource;

    let mut version = fixture();
    version.version = Some("Python 3.12.12".into());
    version.latest_version = Some("Python 3.14.0".into());
    for (source, expected) in [
        (None, "Python"),
        (Some(VersionSource::TrustedLocalProbe), "Python"),
        (Some(VersionSource::ElfProductBanner), "Python"),
        (Some(VersionSource::ElfVersionObject), "CPython"),
    ] {
        version.version_source = source;
        for module in [false, true] {
            version.script_application.as_mut().unwrap().kind = if module {
                "Python module"
            } else {
                "Python script"
            };
            let rows = application_version_rows("Application", Some(&version), false);
            let interpreter = rows.iter().find(|row| row.label == "Interpreter").unwrap();
            assert_eq!(interpreter.value, expected, "{source:?}, module={module}");
            assert_eq!(interpreter.scope, InfoFieldScope::InstanceContext);
            assert_eq!(interpreter.binary_is_current, None);
            assert!(!rows.iter().any(|row| row.label == "Runtime"));
        }
    }
    version.running_unavailable = Some("process identity changed");
    assert_eq!(interpreter_name(&version), "Python");
    version.running_unavailable = None;
    version.version = Some("uv 0.10.4".into());
    assert_eq!(interpreter_name(&version), "Python");
    version.version = None;
    assert_eq!(interpreter_name(&version), "Python");
    version.script_application = None;
    version.version = Some("Python 3.12.12".into());
    let rows = application_version_rows("Application", Some(&version), false);
    assert_eq!(rows[0].label, "Interpreter");
    assert_eq!(rows[0].value, "CPython");
}

#[test]
fn python_environment_uncertainty_qualifies_inherited_markers_without_hiding_arguments() {
    let mut version = fixture();
    let context = version.python_context.as_mut().unwrap();
    context.environment = None;
    context.virtual_environment_hint = Some("/stale/venv".into());
    context.environment_issue = Some("interpreter launch path is not absolute".into());
    let rows = context_rows("Application", &version);
    assert!(
        rows.iter()
            .any(|row| row.label == "Virtual environment" && row.value.starts_with("Unknown"))
    );
    assert!(rows.iter().any(|row| row.label == "Inherited VIRTUAL_ENV"
        && row.value.contains("not a verified active environment")));
    assert!(rows.iter().any(|row| row.label == "Script arguments"));
    version.python_context = None;
    version.python_context_error = Some("process changed".into());
    let rows = context_rows("Application", &version);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].value, "Unavailable (process changed)");
}

#[test]
fn python_dependency_section_uses_cached_evidence_and_distinguishes_missing_unknown_and_ambiguous()
{
    let ctx = egui::Context::default();
    let parent = egui::Id::new("python-script");
    let mut version = fixture();
    let closed = dependency_sections(&ctx, parent, Some(&version));
    assert_eq!(closed.len(), 1);
    assert!(!closed[0].default_open);
    assert!(closed[0].rows.is_empty());
    let mut state =
        egui::collapsing_header::CollapsingState::load_with_default_open(&ctx, closed[0].id, false);
    state.set_open(true);
    state.store(&ctx);
    for (installed, complete, ambiguous, expected) in [
        (Some("2.32"), true, false, "[installed: 2.32]"),
        (
            None,
            true,
            false,
            "not found in inspected package directories",
        ),
        (None, false, false, "not checked"),
        (None, true, true, "ambiguous installed metadata"),
    ] {
        let dependency = &mut version.python_context.as_mut().unwrap().dependencies[0];
        dependency.installed_version = installed.map(str::to_owned);
        dependency.installed_lookup_complete = complete;
        dependency.ambiguous = ambiguous;
        let sections = dependency_sections(&ctx, parent, Some(&version));
        assert_eq!(sections[0].id, closed[0].id);
        assert_eq!(sections[0].header_scope, Some(InfoFieldScope::Installation));
        assert_eq!(sections[0].rows.len(), 1);
        let row = &sections[0].rows[0];
        assert!(row.value.contains("python_version >= '3.10'"));
        assert!(row.value.contains(expected), "{}", row.value);
        assert!(row.binary_is_current.is_none());
    }
    assert!(
        dependency_sections(&ctx, egui::Id::new("another-python"), Some(&version))[0]
            .rows
            .is_empty()
    );
    assert!(dependency_sections(&ctx, parent, None).is_empty());
}

#[test]
fn python_dependency_metadata_and_source_directory_are_not_repeated() {
    let mut version = fixture();
    version.application_progs_metadata.source_checkout = Some("/fixture".into());
    let metadata = version
        .application_installed_metadata
        .as_ref()
        .unwrap()
        .metadata_path
        .clone();
    version.python_context.as_mut().unwrap().dependency_metadata = Some(metadata.clone());
    let rows = application_version_rows("Application", Some(&version), false);
    assert_eq!(
        rows.iter()
            .filter(|row| row.value == metadata.display().to_string())
            .count(),
        1
    );
    assert_eq!(rows.iter().filter(|row| row.value == "/fixture").count(), 0);
    assert!(!rows.iter().any(|row| row.label == "Source directory"));
    assert_eq!(
        version
            .application_progs_metadata
            .source_checkout
            .as_deref(),
        Some(Path::new("/fixture"))
    );
}

fn python_node() -> ProcessInfoNode {
    ProcessInfoNode {
        identity: crate::models::ProcessIdentity {
            pid: 42,
            start_ticks: 100,
        },
        parent_pid: 1,
        depth: 1,
        name: "python3.14".into(),
        script_name: Some("serve_viewer.py".into()),
        header: "python3.14 (pid 42)".into(),
        executable: Some("/not-accessed/python3.14".into()),
        executable_identity: Some((1, 2)),
        executable_error: None,
        cwd: None,
        cwd_error: None,
        raw_argv0: None,
        command_line: None,
    }
}

#[test]
fn python_script_fields_preserve_filenames_without_certifying_unrecorded_source() {
    let node = python_node();
    for native in [Some(true), Some(false), None] {
        let mut version = fixture();
        version.running_latest_binary = native;
        let script = version.script_application.as_mut().unwrap();
        script.name = "viewer".into();
        script.target = "/fixture/unicode π/viewer-é.py".into();
        let sections = hosted_metadata_sections(
            &egui::Context::default(),
            egui::Id::new(native),
            &node,
            Some(&version),
        );
        let section = &sections[0];
        assert_eq!(section.header.as_ref(), "Script  viewer-é.py");
        assert_eq!(
            &section.header[section.field_header.as_ref().unwrap().value_range.clone()],
            "viewer-é.py"
        );
        assert_eq!(
            section.binary_is_current, None,
            "a current interpreter cannot certify script source"
        );
    }
}

#[test]
fn python_module_and_interactive_sections_do_not_become_script_fields() {
    let mut version = fixture();
    let script = version.script_application.as_mut().unwrap();
    script.kind = "Python module";
    script.target = "viewer.main".into();
    let node = python_node();
    let sections = hosted_metadata_sections(
        &egui::Context::default(),
        egui::Id::new("module"),
        &node,
        Some(&version),
    );
    assert_eq!(sections[0].header.as_ref(), "Module name: viewer.main");
    assert_eq!(
        sections[0].header_scope,
        Some(InfoFieldScope::InstanceContext)
    );
    assert!(sections[0].field_header.is_none());
    assert_eq!(sections[0].binary_is_current, None);
    version.script_application = None;
    assert!(
        hosted_metadata_sections(
            &egui::Context::default(),
            egui::Id::new("interactive"),
            &node,
            Some(&version),
        )
        .is_empty()
    );
}

#[test]
fn python_dependency_children_follow_script_collapse_state_without_creating_a_process() {
    let version = fixture();
    let ctx = egui::Context::default();
    let parent = egui::Id::new("python-process");
    let node = python_node();
    let sections = hosted_metadata_sections(&ctx, parent, &node, Some(&version));
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].header.as_ref(), "Script  serve_viewer.py");
    let field = sections[0].field_header.as_ref().unwrap();
    assert_eq!(
        &sections[0].header[field.value_range.clone()],
        "serve_viewer.py"
    );
    assert_eq!(field.label_scope, InfoFieldScope::InstanceContext);
    assert_eq!(sections[0].binary_is_current, None);
    assert!(
        sections[0]
            .children
            .iter()
            .any(|section| section.header.starts_with("Referenced project modules"))
    );
    let dependencies = sections[0]
        .children
        .iter()
        .find(|section| section.header.starts_with("Declared dependencies"))
        .unwrap();
    assert!(dependencies.rows.is_empty());
    assert!(sections[0].rows.iter().all(|row| row.label != "PID"));
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
        &ctx,
        sections[0].id,
        true,
    );
    state.set_open(false);
    state.store(&ctx);
    let closed = hosted_metadata_sections(&ctx, parent, &node, Some(&version));
    assert!(closed[0].rows.is_empty() && closed[0].children.is_empty());
    let other = hosted_metadata_sections(
        &ctx,
        egui::Id::new("other-python-process"),
        &node,
        Some(&version),
    );
    assert!(!other[0].rows.is_empty());
    assert!(
        other[0]
            .children
            .iter()
            .any(|section| section.header.starts_with("Declared dependencies"))
    );
    assert!(
        other[0]
            .children
            .iter()
            .any(|section| section.header.starts_with("Referenced project modules"))
    );
}
