use super::*;
use crate::windows::versions::{ProcessVersion, ScriptApplication};
use program_metadata::development::Metadata;

fn metadata(language: &str, stack: &[&str]) -> Metadata {
    Metadata {
        source_languages: vec![language.into()],
        build_system: stack.iter().map(|value| (*value).into()).collect(),
        // Deliberately nonexistent evidence: formatting must never open it.
        evidence: vec!["/fixture/not-a-real-checkout/build/CMakeCache.txt".into()],
        ..Default::default()
    }
}

fn value<'a>(rows: &'a [InfoPopupRow], label: &str) -> &'a str {
    assert_one_language_row(rows);
    &rows
        .iter()
        .find(|row| row.label == label)
        .unwrap_or_else(|| panic!("missing development field: {label}"))
        .value
}

fn assert_one_language_row(rows: &[InfoPopupRow]) {
    assert_eq!(
        rows.iter()
            .filter(|row| row.label.starts_with("Application languages"))
            .count(),
        1
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.label.starts_with("Other detected language components"))
    );
}

#[test]
fn development_metadata_checkout_rows_are_source_scoped_without_binary_secondaries() {
    let snapshot = Metadata {
        source_checkout: Some("/fixture/current-checkout".into()),
        source_languages: vec!["Rust".into()],
        build_system: vec!["Cargo".into()],
        // Even a stale pre-policy aggregate must not become checkout evidence.
        binary_languages: vec![program_metadata::development::LanguageEvidence {
            language: "C++".into(),
            kind: program_metadata::development::LanguageEvidenceKind::SymbolSignature,
            detail: "Not inspected under source-only policy".into(),
        }],
        component_reason: Some("Binary components were not inspected".into()),
        ..Default::default()
    };
    let version = ProcessVersion {
        version: Some("old-running-version".into()),
        build_id: Some("old-running-build".into()),
        latest_version: Some("current-installed-version".into()),
        latest_build_id: Some("current-installed-build".into()),
        latest_development: Some(snapshot.clone()),
        ..Default::default()
    };
    let rows = process_version_rows("Application", Some(&version), false);
    let languages = value(&rows, "Application languages (source checkout)").to_owned();
    assert_eq!(languages, "Rust (primary)");
    assert_eq!(value(&rows, "Build system (source checkout)"), "Cargo");
    assert!(!languages.contains("secondary"));
    assert!(
        !rows
            .iter()
            .any(|row| row.label == "Application languages (on disk)"
                || row.label.contains("running executable"))
    );
    assert!(rows.iter().filter(|row|row.label.ends_with("(source checkout)")).all(|row|row.scope==InfoFieldScope::Installation&&row.binary_is_current.is_none()));
    assert_language_rows_render_and_copy(rows, &languages);
    let rows = development_rows(
        "Application",
        Some(&snapshot),
        InfoFieldScope::InstanceContext,
        "retained script",
        false,
        Some("old process exited"),
    );
    assert_eq!(
        value(&rows, "Application languages (source checkout)"),
        "Rust (primary)"
    );
    assert!(
        rows.iter()
            .all(|row| row.scope == InfoFieldScope::Installation)
    );
}

#[test]
fn development_metadata_known_checkout_never_relabels_stale_binary_or_loading_facts() {
    let mut version = ProcessVersion {
        latest_development: Some(metadata("C++", &["CMake", "Ninja"])),
        ..Default::default()
    };
    version.progs_metadata.source_checkout = Some("/fixture/current-checkout".into());
    let rows = process_version_rows("Application", Some(&version), false);
    assert_eq!(
        value(&rows, "Application languages (source checkout)"),
        "Unknown"
    );
    assert_eq!(value(&rows, "Build system (source checkout)"), "Unknown");
    let rows = process_version_rows("Application", Some(&version), true);
    assert_eq!(
        value(&rows, "Application languages (source checkout)"),
        "Loading..."
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.label == "Application languages (on disk)")
    );
}

#[test]
fn development_metadata_missing_source_root_stays_checkout_unknown_without_secondary() {
    for badge in ["FRK", "DEV", "CLO"] {
        let version = ProcessVersion {
            installation: Some(format!("[{badge}] Fixture")),
            latest_development: Some(Metadata::unavailable_checkout()),
            ..Default::default()
        };
        let rows = process_version_rows("Application", Some(&version), false);
        let text = value(&rows, "Application languages (source checkout)");
        assert!(text.starts_with("Unknown (primary;"), "{text}");
        assert!(!text.contains("secondary"), "{text}");
        assert!(
            !rows
                .iter()
                .any(|row| row.label == "Application languages (on disk)")
        );
    }
}

#[test]
fn development_metadata_language_line_omits_empty_components_without_hiding_unknowns() {
    use program_metadata::development::{LanguageEvidence, LanguageEvidenceKind};

    for evidence in [
        Metadata::default(),
        metadata("Rust", &["Cargo"]),
        Metadata {
            source_languages: vec!["Rust".into()],
            binary_languages: vec![LanguageEvidence {
                language: "Rust".into(),
                kind: LanguageEvidenceKind::DebugInfo,
                detail: "Already established application language".into(),
            }],
            ..Default::default()
        },
    ] {
        let expected = format!("{} (primary)", evidence.application_languages_display());
        assert_eq!(development_languages_display(&evidence), expected);
        let version = ProcessVersion {
            latest_development: Some(evidence),
            ..Default::default()
        };
        let rows = process_version_rows("Application", Some(&version), false);
        assert_eq!(value(&rows, "Application languages (on disk)"), expected);
        assert!(!rows.iter().any(|row| row.value.contains("None detected")));
        assert!(!value(&rows, "Application languages (on disk)").contains("secondary"));
    }

    let evidence = Metadata {
        source_languages: vec!["Rust".into()],
        component_reason: Some("Auxiliary language metadata changed during inspection".into()),
        ..Default::default()
    };
    assert_eq!(
        development_languages_display(&evidence),
        "Rust (primary) · Unknown (secondary; Auxiliary language metadata changed during inspection)"
    );
}

#[test]
fn development_metadata_language_roles_follow_each_language_with_nested_reasons_intact() {
    use program_metadata::development::{
        ApplicationLanguageEvidence, LanguageConfidence, LanguageEvidence, LanguageEvidenceKind,
    };
    let component = |language: &str| LanguageEvidence {
        language: language.into(),
        kind: LanguageEvidenceKind::DebugInfo,
        detail: "Fixture linked component; not application ownership".into(),
    };
    let mixed = Metadata {
        source_languages: vec!["C".into(), "C++".into()],
        binary_languages: ["C", "C++", "Rust", "Go"].map(component).to_vec(),
        ..Default::default()
    };
    assert_eq!(
        development_languages_display(&mixed),
        "C (primary), C++ (primary) · Go (secondary, binary evidence; may include linked/generated components), Rust (secondary, binary evidence; may include linked/generated components)"
    );
    let confidence = Metadata {
        application_languages: [
            ("Go", LanguageConfidence::Probable),
            ("C++", LanguageConfidence::Confirmed),
        ]
        .map(|(language, confidence)| ApplicationLanguageEvidence {
            language: language.into(),
            confidence,
            kind: LanguageEvidenceKind::EmbeddedMetadata,
            detail: "Fixture application ownership evidence".into(),
        })
        .to_vec(),
        ..Default::default()
    };
    assert_eq!(
        development_languages_display(&confidence),
        "C++ (primary), Go (primary, probable)"
    );
    let unknown = Metadata {
        language_reason: Some(
            "No matching (local, separate) debug information, not inferred".into(),
        ),
        component_reason: Some("Unreadable index (held, changed), incomplete".into()),
        ..Default::default()
    };
    assert_eq!(
        development_languages_display(&unknown),
        "Unknown (primary; No matching (local, separate) debug information, not inferred) · Unknown (secondary; Unreadable index (held, changed), incomplete)"
    );
    let entries = Metadata {
        binary_languages: ["JavaScript", "TypeScript"]
            .map(|language| LanguageEvidence {
                language: language.into(),
                kind: LanguageEvidenceKind::ScriptDeclaration,
                detail: "Fixture declared entry language".into(),
            })
            .to_vec(),
        ..Default::default()
    };
    assert_eq!(
        development_languages_display(&entries),
        "JavaScript (primary, entry-language evidence; original source may differ), TypeScript (primary, entry-language evidence; original source may differ)"
    );
}

#[test]
fn development_metadata_missing_and_loading_values_are_explicit_without_runtime_guesses() {
    for pending in [false, true] {
        let rows = process_version_rows("Runtime", None, pending);
        assert!(
            !rows
                .iter()
                .any(|row| row.label.ends_with("(running executable)"))
        );
        assert_eq!(
            value(&rows, "Application languages (on disk)"),
            if pending { "Loading..." } else { "Unknown" }
        );
        assert!(!rows.iter().any(|row| row.label == "Build system (on disk)"));
    }
    let version = ProcessVersion {
        latest_development: Some(Metadata {
            language_reason: Some(
                "JVM runtime format without application-language evidence".into(),
            ),
            component_reason: Some("JVM runtime format without component-language evidence".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let runtime_only = process_version_rows("Application", Some(&version), false);
    assert_eq!(
        value(&runtime_only, "Application languages (on disk)"),
        "Unknown (primary; JVM runtime format without application-language evidence) · Unknown (secondary; JVM runtime format without component-language evidence)"
    );
    assert!(!value(&runtime_only, "Application languages (on disk)").contains("Java"));
    let version = ProcessVersion {
        latest_development: Some(Metadata {
            language_reason: Some("No matched source-language debug information".into()),
            language_notes: vec!["Compiler/linker annotation: GCC".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    let stripped = process_version_rows("Application", Some(&version), false);
    assert_eq!(
        value(&stripped, "Application languages (on disk)"),
        "Unknown (primary; No matched source-language debug information)"
    );
}

#[test]
fn development_metadata_native_rows_show_only_installed_facts() {
    let version = ProcessVersion {
        development: Some(metadata("C", &["Autotools", "Make"])),
        latest_development: Some(metadata("C++", &["CMake", "Ninja"])),
        version: Some("old application".into()),
        latest_version: Some("new application".into()),
        running_latest_binary: Some(false),
        ..Default::default()
    };
    let rows = process_version_rows("Application", Some(&version), false);
    assert!(
        !rows
            .iter()
            .any(|row| row.label.ends_with("(running executable)"))
    );
    for (label, expected, scope) in [
        (
            "Application languages (on disk)",
            "C++ (primary)",
            InfoFieldScope::Installation,
        ),
        (
            "Build system (on disk)",
            "CMake → Ninja",
            InfoFieldScope::Installation,
        ),
    ] {
        assert_one_language_row(&rows);
        let field = rows.iter().find(|row| row.label == label).unwrap();
        assert_eq!(field.value, expected);
        assert_eq!(field.scope, scope);
        assert_eq!(field.binary_is_current, None);
    }
    assert_eq!(version.process_build_is_current(), Some(false));
}

#[test]
fn development_metadata_source_free_languages_keep_qualifiers_and_installed_scope() {
    use program_metadata::development::{LanguageEvidence, LanguageEvidenceKind};
    let mut version = ProcessVersion {
        latest_development: Some(Metadata {
            binary_languages: ["Go", "C++"]
                .map(|language| LanguageEvidence {
                    language: language.into(),
                    kind: LanguageEvidenceKind::EmbeddedMetadata,
                    detail: "Selected-image compiler record, main module absent".into(),
                })
                .to_vec(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let rows = process_version_rows("Application", Some(&version), false);
    let components = "C++ (secondary, binary evidence; may include linked/generated components), Go (secondary, binary evidence; may include linked/generated components)";
    let expected = format!(
        "Unknown (primary; only component-language evidence; application language not established) · {components}"
    );
    assert_eq!(value(&rows, "Application languages (on disk)"), expected);
    assert!(!rows.iter().any(|row| row.label == "Build system (on disk)"));
    let field = rows
        .iter()
        .find(|row| row.label == "Application languages (on disk)")
        .unwrap();
    assert_eq!(field.scope, InfoFieldScope::Installation);
    assert!(field.binary_is_current.is_none());
    assert!(
        !rows
            .iter()
            .any(|row| row.label.ends_with("(running executable)"))
    );
    let data = InfoPopupData {
        title: "Owned source-free fixture".into(),
        heading: String::new(),
        subtitle: String::new(),
        scroll_sensitivity: 1.0,
        rows,
        process_tree: None,
    };
    for scale in [1.0, 2.0] {
        let context = egui::Context::default();
        context.set_pixels_per_point(scale);
        let (_, editor) = frame(&context, &data, Vec::new());
        assert!(editor.galley.job.text.contains(&expected));
        assert!(
            !editor
                .galley
                .job
                .text
                .contains("Other detected language components")
        );
    }
    // An interpreted application's language must not borrow even qualified
    // implementation-language evidence from its hosting interpreter/runtime.
    version.runtime_name = Some("Node.js");
    version.script_application = Some(ScriptApplication {
        name: "Owned hosted fixture".into(),
        target: "/fixture/app.js".into(),
        kind: "JavaScript script",
        entry_point: Some("/fixture/app.js".into()),
        descriptor: None,
        identification: "Owned static test fixture",
    });
    let hosted = application_version_rows("Application", Some(&version), false);
    assert_one_language_row(&hosted);
    assert!(!hosted.iter().any(|row| row.value.contains(components)));
}

#[test]
fn development_metadata_application_confidence_and_components_share_one_copyable_line() {
    use program_metadata::development::{
        ApplicationLanguageEvidence, LanguageConfidence, LanguageEvidence, LanguageEvidenceKind,
    };
    for (confidence, application) in [
        (LanguageConfidence::Confirmed, "Go (primary)"),
        (LanguageConfidence::Probable, "Go (primary, probable)"),
    ] {
        let version = ProcessVersion {
            latest_development: Some(Metadata {
                application_languages: vec![ApplicationLanguageEvidence {
                    language: "Go".into(),
                    confidence,
                    kind: LanguageEvidenceKind::EmbeddedMetadata,
                    detail: "Fixture application ownership evidence".into(),
                }],
                binary_languages: ["Go", "C++"]
                    .map(|language| LanguageEvidence {
                        language: language.into(),
                        kind: LanguageEvidenceKind::DebugInfo,
                        detail: "Fixture linked compilation unit".into(),
                    })
                    .to_vec(),
                component_reason: Some("Local debug data inspection deadline reached".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let rows = process_version_rows("Application", Some(&version), false);
        let components = "C++ (secondary, binary evidence; may include linked/generated components; inspection incomplete: Local debug data inspection deadline reached)";
        let combined = format!("{application} · {components}");
        assert_eq!(value(&rows, "Application languages (on disk)"), combined);
        assert!(
            rows.iter()
                .filter(|row| row.label.contains("languages"))
                .all(|row| row.scope == InfoFieldScope::Installation
                    && row.binary_is_current.is_none())
        );
        let data = InfoPopupData {
            title: "Owned combined language fixture".into(),
            heading: String::new(),
            subtitle: String::new(),
            scroll_sensitivity: 1.0,
            rows,
            process_tree: None,
        };
        for scale in [1.0, 2.0] {
            let context = egui::Context::default();
            context.set_pixels_per_point(scale);
            let (_, editor) = frame(&context, &data, Vec::new());
            let text = editor.galley.job.text.clone();
            assert!(text.contains(&combined));
            assert_eq!(text.matches("(primary").count(), 1);
            assert_eq!(text.matches("(secondary").count(), 1);
            assert!(!text.contains("Primary:") && !text.contains("Secondary:"));
            assert!(!text.contains("None detected"));
            assert_eq!(text.matches("Application languages (on disk)").count(), 1);
            assert!(!text.contains("Other detected language components"));
            assert!(!text.contains("Source languages"));
            let mut state = editor.state;
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(text.chars().count()),
                )));
            state.store(&context, editor.response.id);
            context.memory_mut(|memory| memory.request_focus(editor.response.id));
            let (output, _) = frame(&context, &data, vec![egui::Event::Copy]);
            let copied: Vec<_> = output
                .platform_output
                .commands
                .iter()
                .filter_map(|command| match command {
                    egui::OutputCommand::CopyText(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(copied, vec![text.as_str()]);
        }
    }
}

#[test]
#[ignore = "passive host-corpus acceptance; requires PROGRAM_METADATA_TEST_GO_APPLICATION_PATH pointing to the current custom Go application with probable Go ownership and C++ components"]
fn development_metadata_current_go_artifact_reaches_the_copyable_language_row() {
    use program_metadata::{
        build_status::artifact::{self, ArtifactStamp},
        development::{self, LanguageConfidence},
    };
    use std::{
        fs::OpenOptions,
        os::unix::fs::OpenOptionsExt,
        path::PathBuf,
        time::{Duration, Instant},
    };

    let path = PathBuf::from(
        std::env::var_os("PROGRAM_METADATA_TEST_GO_APPLICATION_PATH")
            .expect("explicit current Go/C++ host corpus path is required; no binary is executed"),
    );
    assert!(path.is_absolute(), "host corpus path must be absolute");
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
        .unwrap();
    let file_metadata = file.metadata().unwrap();
    assert!(
        file_metadata.is_file(),
        "host corpus must be a regular file"
    );
    let expected_stamp = ArtifactStamp::from_metadata(&file_metadata);
    let assert_current = || {
        assert_eq!(
            ArtifactStamp::from_metadata(&file.metadata().unwrap()),
            expected_stamp,
            "held host corpus changed"
        );
        assert_eq!(
            artifact::stamp(&path).unwrap(),
            expected_stamp,
            "host corpus route changed"
        );
    };
    assert_current();
    let metadata =
        development::inspect_artifact(&path, None, Instant::now() + Duration::from_millis(750));
    assert_current();
    assert_eq!(metadata.artifact_stamp, Some(expected_stamp));
    assert!(metadata.language_dependencies_current());
    assert!(
        metadata.application_languages.iter().any(|evidence| {
            evidence.language == "Go" && evidence.confidence == LanguageConfidence::Probable
        }),
        "{metadata:?}"
    );
    let version = ProcessVersion {
        latest_development: Some(metadata),
        ..Default::default()
    };
    let rows = process_version_rows("Application", Some(&version), false);
    let languages = value(&rows, "Application languages (on disk)").to_owned();
    assert!(languages.contains("Go (primary, probable)"), "{languages}");
    assert!(
        languages
            .contains("C++ (secondary, binary evidence; may include linked/generated components"),
        "{languages}"
    );
    assert!(!languages.contains("Unknown (primary"), "{languages}");
    assert!(!languages.contains("Go (secondary"), "{languages}");
    let row = rows
        .iter()
        .find(|row| row.label == "Application languages (on disk)")
        .unwrap();
    assert_eq!(row.scope, InfoFieldScope::Installation);
    assert_eq!(row.binary_is_current, None);
    assert_language_rows_render_and_copy(rows, &languages);
    assert_current();
    assert!(
        version
            .latest_development
            .as_ref()
            .unwrap()
            .language_dependencies_current()
    );
}

#[test]
#[ignore = "passive host-corpus acceptance; requires PROGRAM_METADATA_TEST_RUST_APPLICATION_PATH pointing to the current source-free Rust/Codex image with provable application ownership"]
fn development_metadata_current_rust_artifact_reaches_the_copyable_language_row() {
    use program_metadata::{
        build_status::artifact::{self, ArtifactStamp},
        development,
    };
    use std::{
        fs::OpenOptions,
        os::unix::fs::OpenOptionsExt,
        path::PathBuf,
        time::{Duration, Instant},
    };

    let path = PathBuf::from(
        std::env::var_os("PROGRAM_METADATA_TEST_RUST_APPLICATION_PATH")
            .expect("explicit Rust/Codex host corpus path required; no application is executed"),
    );
    assert!(path.is_absolute(), "host corpus path must be absolute");
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
        .unwrap();
    let before = ArtifactStamp::from_metadata(&file.metadata().unwrap());
    assert_eq!(artifact::stamp(&path).unwrap(), before);
    let started = Instant::now();
    let metadata = development::inspect_artifact(&path, None, started + Duration::from_millis(750));
    eprintln!(
        "passive Rust/Codex build ID {:?}, elapsed {:?}, metadata {metadata:?}",
        program_metadata::executable::elf_build_id(&file),
        started.elapsed()
    );
    assert_eq!(metadata.artifact_stamp, Some(before));
    assert_eq!(
        ArtifactStamp::from_metadata(&file.metadata().unwrap()),
        before
    );
    assert_eq!(artifact::stamp(&path).unwrap(), before);
    assert!(metadata.language_dependencies_current());
    assert!(
        metadata
            .application_languages
            .iter()
            .any(|evidence| evidence.language == "Rust"),
        "Rust component presence alone cannot prove application ownership: {metadata:?}"
    );
    let version = ProcessVersion {
        latest_development: Some(metadata),
        ..Default::default()
    };
    let rows = process_version_rows("Application", Some(&version), false);
    let languages = value(&rows, "Application languages (on disk)").to_owned();
    assert!(languages.contains("Rust (primary"), "{languages}");
    assert!(!languages.contains("Unknown (primary"), "{languages}");
    assert!(!languages.contains("Rust (secondary"), "{languages}");
    let row = rows
        .iter()
        .find(|row| row.label == "Application languages (on disk)")
        .unwrap();
    assert_eq!(row.scope, InfoFieldScope::Installation);
    assert_eq!(row.binary_is_current, None);
    assert_language_rows_render_and_copy(rows, &languages);
    assert_eq!(
        ArtifactStamp::from_metadata(&file.metadata().unwrap()),
        before
    );
    assert_eq!(artifact::stamp(&path).unwrap(), before);
    assert!(
        version
            .latest_development
            .as_ref()
            .unwrap()
            .language_dependencies_current()
    );
}

#[test]
#[ignore = "passive source-checkout acceptance; requires PROGRAM_METADATA_TEST_SOURCE_CHECKOUT and PROGRAM_METADATA_TEST_RUST_APPLICATION_PATH selecting the Codex checkout/target; never opens or executes the image"]
fn development_metadata_current_source_checkout_reaches_the_copyable_language_row() {
    use std::{
        path::PathBuf,
        time::{Duration, Instant},
    };
    let checkout = PathBuf::from(
        std::env::var_os("PROGRAM_METADATA_TEST_SOURCE_CHECKOUT")
            .expect("explicit source checkout required"),
    );
    let logical = PathBuf::from(
        std::env::var_os("PROGRAM_METADATA_TEST_RUST_APPLICATION_PATH")
            .expect("explicit logical Codex application path required; not opened"),
    );
    assert!(checkout.is_absolute() && logical.is_absolute());
    let started = Instant::now();
    let snapshot = program_metadata::development::inspect_checkout(
        &logical,
        &checkout,
        started + Duration::from_millis(750),
    );
    eprintln!(
        "source-only checkout elapsed {:?}, languages {:?}, build {:?}, reasons {:?}/{:?}, {} source dependencies",
        started.elapsed(),
        snapshot.source_languages,
        snapshot.build_system,
        snapshot.language_reason,
        snapshot.build_reason,
        snapshot.language_dependencies.len()
    );
    assert_eq!(
        snapshot.source_checkout.as_deref(),
        Some(checkout.as_path())
    );
    assert!(
        snapshot
            .source_languages
            .iter()
            .any(|language| language == "Rust"),
        "source languages {:?}; reason {:?}",
        snapshot.source_languages,
        snapshot.language_reason
    );
    assert!(
        snapshot.build_system.iter().any(|tool| tool == "Cargo"),
        "build tools {:?}; reason {:?}",
        snapshot.build_system,
        snapshot.build_reason
    );
    assert!(snapshot.application_languages.is_empty());
    assert!(snapshot.binary_languages.is_empty());
    assert!(snapshot.artifact_stamp.is_none());
    assert!(snapshot.language_dependencies_current());
    let version = ProcessVersion {
        latest_development: Some(snapshot),
        ..Default::default()
    };
    let rows = process_version_rows("Application", Some(&version), false);
    let languages = value(&rows, "Application languages (source checkout)").to_owned();
    assert!(languages.contains("Rust (primary)"), "{languages}");
    assert!(
        !languages.contains("secondary"),
        "binary components were not inspected: {languages}"
    );
    assert_eq!(value(&rows, "Build system (source checkout)"), "Cargo");
    assert!(
        !rows
            .iter()
            .any(|row| row.label == "Application languages (on disk)"
                || row.label.contains("running executable"))
    );
    assert_language_rows_render_and_copy(rows, &languages);
    assert!(
        version
            .latest_development
            .as_ref()
            .unwrap()
            .language_dependencies_current()
    );
}

fn assert_language_rows_render_and_copy(rows: Vec<InfoPopupRow>, languages: &str) {
    let language_label = rows
        .iter()
        .find(|row| row.label.starts_with("Application languages"))
        .unwrap()
        .label
        .clone();
    let data = InfoPopupData {
        title: "Passive selected-file language acceptance".into(),
        heading: String::new(),
        subtitle: String::new(),
        scroll_sensitivity: 1.0,
        rows,
        process_tree: None,
    };
    for scale in [1.0, 2.0] {
        let context = egui::Context::default();
        context.set_pixels_per_point(scale);
        let (_, editor) = frame(&context, &data, Vec::new());
        let text = editor.galley.job.text.clone();
        assert!(text.contains(languages));
        assert_eq!(text.matches(&language_label).count(), 1);
        assert!(!text.contains("Other detected language components"));
        let mut state = editor.state;
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(text.chars().count()),
            )));
        state.store(&context, editor.response.id);
        context.memory_mut(|memory| memory.request_focus(editor.response.id));
        let (output, _) = frame(&context, &data, vec![egui::Event::Copy]);
        let copied: Vec<_> = output
            .platform_output
            .commands
            .iter()
            .filter_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(copied, vec![text.as_str()]);
    }
}

#[test]
fn development_metadata_entry_language_is_qualified_without_borrowing_runtime_components() {
    use program_metadata::development::{LanguageEvidence, LanguageEvidenceKind};
    let version = ProcessVersion {
        runtime_name: Some("Node.js"),
        development: Some(metadata("C++", &["CMake", "Ninja"])),
        application_development: Some(Metadata {
            binary_languages: vec![LanguageEvidence {
                language: "JavaScript".into(),
                kind: LanguageEvidenceKind::ScriptDeclaration,
                detail: "Literal selected Node entry".into(),
            }],
            ..Default::default()
        }),
        script_application: Some(ScriptApplication {
            name: "fixture".into(),
            target: "/fixture/entry.js".into(),
            kind: "JavaScript script",
            entry_point: Some("/fixture/entry.js".into()),
            descriptor: None,
            identification: "Owned fixture",
        }),
        ..Default::default()
    };
    let rows = application_version_rows("Application", Some(&version), false);
    assert_eq!(
        value(&rows, "Application languages (on disk)"),
        "JavaScript (primary, entry-language evidence; original source may differ)"
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.value.contains("C++") || row.value.contains("CMake"))
    );
}

#[test]
fn development_metadata_hosted_fields_never_borrow_runtime_languages_or_build_systems() {
    for (runtime, kind, target) in [
        ("Node.js", "JavaScript script", "/fixture/tool.js"),
        ("Python", "Python script", "/fixture/tool.py"),
        ("Shell", "Shell script", "/fixture/tool.sh"),
    ] {
        let mut version = ProcessVersion {
            runtime_name: Some(runtime),
            development: Some(metadata("C++", &["CMake", "Make"])),
            latest_development: Some(metadata("C++", &["CMake", "Ninja"])),
            script_application: Some(ScriptApplication {
                name: "tool".into(),
                target: target.into(),
                kind,
                entry_point: Some(target.into()),
                descriptor: None,
                identification: "Owned display fixture",
            }),
            ..Default::default()
        };
        let rows = application_version_rows("Application", Some(&version), false);
        assert_eq!(value(&rows, "Application languages (on disk)"), "Unknown");
        assert!(!rows.iter().any(|row| row.label == "Build system (on disk)"));
        assert!(!rows.iter().any(|row| row.value.contains("C++")));
        version.application_development = Some(metadata("TypeScript", &["npm", "tsc"]));
        let rows = application_version_rows("Application", Some(&version), false);
        assert_eq!(
            value(&rows, "Application languages (on disk)"),
            "TypeScript (primary)"
        );
        assert_eq!(value(&rows, "Build system (on disk)"), "npm → tsc");
        assert_eq!(version.process_build_is_current(), None);
        assert!(
            rows.iter()
                .filter(|row| row.label.starts_with("Application languages")
                    || row.label.starts_with("Build system"))
                .all(|row| row.binary_is_current.is_none())
        );
    }
}

#[test]
fn development_metadata_application_tiles_keep_installed_scope_without_running_claims() {
    let mut popup = AppInfoPopup::new(AppInfo {
        name: "Owned application fixture".into(),
        exec: "/fixture/no-execution".into(),
        icon_path: None,
        comment: None,
        desktop_file_path: "/fixture/not-a-real-desktop-file.desktop".into(),
        is_settings_module: false,
    });
    popup.metadata = Some(crate::windows::versions::ApplicationInfo {
        development: Some(metadata("Rust", &["Cargo"])),
        ..Default::default()
    });
    let rows = application_info_rows(&popup);
    assert_eq!(
        value(&rows, "Application languages (on disk)"),
        "Rust (primary)"
    );
    assert_eq!(value(&rows, "Build system (on disk)"), "Cargo");
    assert!(
        rows.iter()
            .all(|row| row.scope == InfoFieldScope::Installation)
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.label.contains("running executable"))
    );
    assert!(popup.receiver.is_none());
}

#[test]
fn development_metadata_retained_scripts_are_not_labelled_as_installed_files() {
    use std::os::fd::{AsRawFd, FromRawFd};
    let descriptor =
        unsafe { libc::memfd_create(c"development-script".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(descriptor >= 0);
    let mut file = unsafe { std::fs::File::from_raw_fd(descriptor) };
    std::io::Write::write_all(&mut file, b"#!/bin/sh\nexit 0\n").unwrap();
    let route = format!("/proc/self/fd/{}", file.as_raw_fd());
    let script = program_metadata::interpreted::script::invocation_from_process_args(
        "Shell",
        &["bash", &route],
        None,
        std::process::id() as i32,
    )
    .unwrap()
    .application;
    assert!(script.entry_point.is_none());
    let version = ProcessVersion {
        runtime_name: Some("Shell"),
        script_application: Some(script),
        application_development: Some(metadata("Shell", &[])),
        ..Default::default()
    };
    let rows = application_version_rows("Application", Some(&version), false);
    assert_eq!(
        value(&rows, "Application languages (retained script)"),
        "Shell (primary)"
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.label == "Application languages (on disk)")
    );
    assert!(
        rows.iter()
            .filter(|row| row.label.ends_with("(retained script)"))
            .all(|row| row.scope == InfoFieldScope::InstanceContext)
    );
}

#[test]
fn development_metadata_long_values_are_in_the_rendered_copyable_document() {
    let version = ProcessVersion {
        development: Some(metadata("C, C++, Rust", &["CMake", "Ninja"])),
        latest_development: Some(Metadata {
            language_reason: Some(format!(
                "No artifact-associated evidence at {}",
                "long source path π/".repeat(32)
            )),
            build_reason: Some("configured backend is not established".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let data = InfoPopupData {
        title: "Development metadata".into(),
        heading: "Artifact-bound source and build facts".into(),
        subtitle: String::new(),
        scroll_sensitivity: 1.0,
        rows: process_version_rows("Application", Some(&version), false),
        process_tree: None,
    };
    assert_one_language_row(&data.rows);
    for scale in [1.0, 2.0] {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(scale);
        let (_, editor) = frame(&ctx, &data, Vec::new());
        let text = editor.galley.job.text.clone();
        assert!(!text.contains("Source languages"));
        assert!(!text.contains("Application languages (running executable)"));
        assert!(!text.contains("Other detected language components"));
        assert!(!text.contains("Build system (running executable)"));
        for field in data.rows.iter().filter(|row| {
            row.label.starts_with("Application languages") || row.label.starts_with("Build system")
        }) {
            assert!(text.contains(&field.label));
            assert!(text.contains(&field.value));
        }
        let mut state = editor.state;
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(text.chars().count()),
            )));
        state.store(&ctx, editor.response.id);
        ctx.memory_mut(|memory| memory.request_focus(editor.response.id));
        let (output, _) = frame(&ctx, &data, vec![egui::Event::Copy]);
        let copied: Vec<_> = output
            .platform_output
            .commands
            .iter()
            .filter_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(copied, vec![text.as_str()]);
    }
}

fn frame(
    ctx: &egui::Context,
    data: &InfoPopupData,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, egui::text_edit::TextEditOutput) {
    let mut editor = None;
    let output = ctx.run(
        egui::RawInput {
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
