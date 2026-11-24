use super::*;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "launcher-development-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str) -> (PathBuf, File) {
        let path = self.0.join(name);
        fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        let file = File::open(&path).unwrap();
        (path, file)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn report(value: &str) -> Metadata {
    Metadata {
        source_languages: vec![value.into()],
        build_system: vec![value.into()],
        ..Default::default()
    }
}

#[test]
fn development_exact_artifact_reuse_rejects_different_and_modified_files() {
    let fixture = Fixture::new();
    let (path, file) = fixture.file("running");
    let (_, different) = fixture.file("installed");
    let mut metadata = report("Selected running evidence");
    metadata.artifact_stamp = stamp(&file);
    assert!(same_artifact(
        &metadata,
        Some(&file),
        &file.try_clone().unwrap()
    ));
    assert!(!same_artifact(&metadata, Some(&file), &different));
    assert!(!same_artifact(&metadata, None, &file));
    assert!(artifact_unchanged(&path, &metadata));
    fs::write(
        &path,
        b"modified selected artifact, not the captured observation",
    )
    .unwrap();
    assert!(!same_artifact(&metadata, Some(&file), &file));
    assert!(!artifact_unchanged(&path, &metadata));
    assert!(!artifact_unchanged(&path, &report("unbound facts")));
    assert!(artifact_unchanged(&path, &Metadata::default()));
    let components = Metadata {
        binary_languages: vec![program_metadata::development::LanguageEvidence {
            language: "C++".into(),
            kind: program_metadata::development::LanguageEvidenceKind::SymbolSignature,
            detail: "Unbound component fact".into(),
        }],
        ..Default::default()
    };
    assert!(!artifact_unchanged(&path, &components));
    let ownership = Metadata {
        application_languages: vec![program_metadata::development::ApplicationLanguageEvidence {
            language: "C".into(),
            confidence: program_metadata::development::LanguageConfidence::Confirmed,
            kind: program_metadata::development::LanguageEvidenceKind::DebugInfo,
            detail: "Unbound ownership claim".into(),
        }],
        ..Default::default()
    };
    assert!(!artifact_unchanged(&path, &ownership));
}

#[test]
fn development_reuse_revalidates_external_debug_routes_including_missing_files() {
    use program_metadata::development::LanguageDependency;
    let fixture = Fixture::new();
    let (path, file) = fixture.file("image");
    let debug = fixture.0.join("image.debug");
    let mut metadata = report("Externally matched evidence");
    metadata.artifact_stamp = stamp(&file);
    metadata.language_dependencies = vec![LanguageDependency::capture(debug.clone())];
    assert!(same_artifact(&metadata, Some(&file), &file));
    fs::write(&debug, b"newly installed matching debug information").unwrap();
    assert!(!same_artifact(&metadata, Some(&file), &file));
    assert!(!artifact_unchanged(&path, &metadata));
    metadata.language_dependencies = vec![LanguageDependency::capture(debug.clone())];
    assert!(artifact_unchanged(&path, &metadata));
    fs::rename(&debug, fixture.0.join("old.debug")).unwrap();
    fs::write(&debug, b"replacement debug information").unwrap();
    assert!(!same_artifact(&metadata, Some(&file), &file));
    assert!(!artifact_unchanged(&path, &metadata));
}

#[test]
fn development_inspection_shares_one_deadline_without_filling_hosted_facts_from_runtime() {
    let fixture = Fixture::new();
    let (running_path, running) = fixture.file("runtime");
    let (installed_path, installed) = fixture.file("installed-runtime");
    let (script, _) = fixture.file("hosted.sh");
    let mut result = ProcessVersion {
        script_application: Some(ScriptApplication {
            name: "hosted".into(),
            target: script.display().to_string(),
            kind: "Shell script",
            entry_point: Some(script),
            descriptor: None,
            identification: "Owned worker fixture",
        }),
        ..Default::default()
    };
    inspect_process(
        &mut result,
        Some(&running),
        Some(&installed),
        &running_path,
        Some(&installed_path),
        Instant::now(),
    );
    for metadata in [
        result.development,
        result.latest_development,
        result.application_development,
    ] {
        let metadata = metadata.unwrap();
        assert!(metadata.source_languages.is_empty());
        assert!(metadata.build_system.is_empty());
        assert!(
            metadata
                .source_languages_display()
                .contains("limit reached")
        );
        assert!(metadata.build_system_display().contains("limit reached"));
        assert_eq!(metadata.language_reason, metadata.build_reason);
    }
}

#[test]
fn development_process_invalidation_preserves_only_independent_installed_facts() {
    let installed = report("Current installed artifact");
    let mut result = ProcessVersion {
        development: Some(report("Old running artifact")),
        latest_development: Some(installed.clone()),
        application_development: Some(report("Hosted entry")),
        ..Default::default()
    };
    result.discard_running_metadata("process identity changed");
    assert!(result.development.is_none());
    assert!(result.application_development.is_none());
    assert_eq!(result.latest_development, Some(installed));
}

#[test]
fn development_checkout_snapshot_is_shared_without_any_executable() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("Cargo.toml"),
        "[package]\nname = \"owned-checkout\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::create_dir(fixture.0.join("src")).unwrap();
    fs::write(fixture.0.join("src/main.rs"), "fn main() {}\n").unwrap();
    let mut result = ProcessVersion::default();
    result.progs_metadata.source_checkout = Some(fixture.0.clone());
    result.application_progs_metadata.source_checkout = Some(fixture.0.clone());
    inspect_process(
        &mut result,
        None,
        None,
        &fixture.0.join("owned-checkout"),
        None,
        Instant::now() + std::time::Duration::from_millis(750),
    );
    let snapshot = result
        .latest_development
        .as_ref()
        .expect("checkout facts do not require a running/installed executable")
        .clone();
    assert_eq!(
        snapshot.source_checkout.as_deref(),
        Some(fixture.0.as_path())
    );
    assert_eq!(snapshot.source_languages, ["Rust"]);
    assert_eq!(snapshot.build_system, ["Cargo"]);
    assert!(snapshot.application_languages.is_empty());
    assert!(snapshot.binary_languages.is_empty());
    assert!(snapshot.artifact_stamp.is_none());
    assert!(snapshot.language_dependencies_current());
    assert_eq!(result.development.as_ref(), Some(&snapshot));
    assert_eq!(result.application_development.as_ref(), Some(&snapshot));
    assert!(artifact_unchanged(
        &fixture.0.join("missing-artifact"),
        &snapshot
    ));
    result.discard_running_metadata("owned process disappeared");
    assert_eq!(result.development.as_ref(), Some(&snapshot));
    fs::write(
        fixture.0.join("Cargo.toml"),
        "[package]\nname = \"changed-checkout\"\nversion = \"0.2.0\"\n",
    )
    .unwrap();
    assert!(!artifact_unchanged(
        &fixture.0.join("missing-artifact"),
        &snapshot
    ));
    retain_checkout(&mut result.development);
    assert!(result.development.is_none());
}

#[test]
fn development_exact_root_target_survives_deep_notes_and_refreshes_after_source_edits() {
    let fixture = Fixture::new();
    let manifest = fixture.0.join("Makefile.am");
    fs::write(
        &manifest,
        "bin_PROGRAMS = selected\nselected_SOURCES = main.c\n",
    )
    .unwrap();
    let notes = fixture.0.join("notes/a/b/c/d/e/f");
    fs::create_dir_all(&notes).unwrap();
    fs::write(notes.join("report.txt"), "unrelated diagnostic notes\n").unwrap();
    let logical = fixture.0.join("selected");
    assert!(!logical.exists());
    let mut result = ProcessVersion::default();
    result.progs_metadata.source_checkout = Some(fixture.0.clone());
    result.application_progs_metadata.source_checkout = Some(fixture.0.clone());
    inspect_process(
        &mut result,
        None,
        None,
        &logical,
        None,
        Instant::now() + std::time::Duration::from_millis(750),
    );
    let original = result.latest_development.as_ref().unwrap().clone();
    assert_eq!(original.source_languages, ["C"]);
    assert_eq!(original.build_system, ["Autotools", "Make"]);
    assert!(original.artifact_stamp.is_none());
    assert!(original.language_dependencies_current());
    assert_eq!(result.development.as_ref(), Some(&original));

    fs::write(&manifest, "selected_SOURCES = main.cpp\n").unwrap();
    assert!(!artifact_unchanged(&logical, &original));
    retain_checkout(&mut result.latest_development);
    assert!(result.latest_development.is_none());
    inspect_process(
        &mut result,
        None,
        None,
        &logical,
        None,
        Instant::now() + std::time::Duration::from_millis(750),
    );
    let refreshed = result.latest_development.as_ref().unwrap();
    assert_eq!(refreshed.source_languages, ["C++"]);
    assert_eq!(refreshed.build_system, ["Autotools", "Make"]);
    assert!(refreshed.language_dependencies_current());
    assert!(!logical.exists());
}

#[test]
fn development_checkout_cache_reads_once_per_target_and_revalidates_source_routes() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("Cargo.toml"),
        "[package]\nname = \"owned-checkout\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let logical = fixture.0.join("owned-checkout");
    let mut cache = Vec::new();
    let deadline = Instant::now() + std::time::Duration::from_millis(750);
    let first = checkout_snapshot(&mut cache, &logical, &fixture.0, deadline);
    assert_eq!(first.source_languages, ["Rust"]);
    let other = checkout_snapshot(
        &mut cache,
        &fixture.0.join("different-target"),
        &fixture.0,
        deadline,
    );
    assert!(
        other.source_languages.is_empty(),
        "target-aware snapshots must not borrow another target's language"
    );
    // A nonregular FD is deliberately unsuitable for executable inspection.
    // Checkout routing must work before any executable validation or reads.
    let directory = File::open(&fixture.0).unwrap();
    let inspected = inspect_selected(&directory, &logical, Some(&fixture.0), true, deadline);
    assert_eq!(inspected.source_languages, ["Rust"]);
    assert!(inspected.artifact_stamp.is_none());
    fs::write(
        fixture.0.join("Cargo.toml"),
        "[package]\nname = \"new-target\"\nversion = \"0.2.0\"\n",
    )
    .unwrap();
    let reused = checkout_snapshot(&mut cache, &logical, &fixture.0, deadline);
    assert_eq!(
        first, reused,
        "one expanded request does not rescan the same checkout target"
    );
    assert!(!reused.language_dependencies_current());
    assert!(!artifact_unchanged(&logical, &reused));
}

#[test]
fn development_source_managed_badges_without_roots_never_fall_back_to_binary_readers() {
    let fixture = Fixture::new();
    let (path, file) = fixture.file("selected.sh");
    for badge in ["FRK", "DEV", "CLO", "FKM", "DVM", "CLX"] {
        for files in [false, true] {
            let mut result = ProcessVersion {
                installation: Some(format!("[{badge}] Source-managed fixture")),
                ..Default::default()
            };
            result.application_progs_metadata.package_type =
                Some(format!("[{badge}] Source-managed fixture"));
            inspect_process(
                &mut result,
                files.then_some(&file),
                files.then_some(&file),
                &path,
                Some(&path),
                Instant::now() + std::time::Duration::from_millis(750),
            );
            for report in [
                result.development,
                result.latest_development,
                result.application_development,
            ] {
                let report = report.expect(
                    "source-only Unknown exists even without a file or recorded checkout root",
                );
                assert!(report.is_source_checkout(), "{badge}");
                assert!(report.source_checkout.is_none());
                assert!(report.source_languages.is_empty());
                assert!(report.application_languages.is_empty());
                assert!(report.binary_languages.is_empty());
                assert!(report.artifact_stamp.is_none());
                assert!(report.component_reason.is_none());
            }
        }
    }
    for badge in ["PET", "UBI"] {
        let mut result = ProcessVersion {
            installation: Some(format!("[{badge}] Binary-only fixture")),
            ..Default::default()
        };
        inspect_process(
            &mut result,
            Some(&file),
            Some(&file),
            &path,
            Some(&path),
            Instant::now() + std::time::Duration::from_millis(750),
        );
        let report = result.latest_development.unwrap();
        assert!(
            !report.is_source_checkout(),
            "{badge} is not source-managed"
        );
        assert!(report.artifact_stamp.is_some());
    }
}
