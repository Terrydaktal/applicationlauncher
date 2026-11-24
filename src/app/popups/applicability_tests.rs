use super::*;
use crate::windows::versions::{
    NodeApplication, PackageTarget, ProcessVersion, ProgsMetadata, ScriptApplication,
};

fn field<'a>(rows: &'a [InfoPopupRow], role: &str, label: &str) -> Option<&'a InfoPopupRow> {
    let label = info_metadata_label(role, label);
    rows.iter().find(|row| row.label == label)
}

#[test]
fn source_managed_and_unmanaged_info_omits_package_roots_but_keeps_upstream_comparisons() {
    for classification in [
        "FRK", "DEV", "CLO", "OPT", "USC", "UBI", "FKM", "DVM", "CLX", "OPX", "SCU", "BIU",
    ] {
        for label in [
            classification.to_owned(),
            format!("[{classification}] Local install"),
        ] {
            for metadata_only in [false, true] {
                let metadata = ProgsMetadata {
                    package_type: metadata_only.then_some(label.clone()),
                    root_packages: Some("unrelated-root 1.0 [PEP]".into()),
                    ..Default::default()
                };
                for package in [None, Some("upstream-package 1.0")] {
                    for pending in [false, true] {
                        for role in ["Application", "Runtime"] {
                            let rows = installed_application_rows(
                                role,
                                InstalledApplicationFields {
                                    installation: (!metadata_only).then_some(label.as_str()),
                                    metadata: Some(&metadata),
                                    package,
                                    package_target: true,
                                    ..Default::default()
                                },
                                pending,
                            );
                            assert!(field(&rows, role, "package").is_none(), "{label}");
                            assert!(field(&rows, role, "root package").is_none(), "{label}");
                            assert!(field(&rows, role, "latest version (Pacman/AUR)").is_some());
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn native_package_rows_require_an_owner_and_preserve_loading_values_badges_and_roots() {
    let metadata = ProgsMetadata {
        package_type: Some("[PDP] Pacman Dependency (Package-managed)".into()),
        root_packages: Some("base 3-3 [PEP]".into()),
        ..Default::default()
    };
    for role in ["Application", "Runtime", "Custom role"] {
        for package in [None, Some(""), Some("bash 5.3.15-2")] {
            for pending in [false, true] {
                let rows = installed_application_rows(
                    role,
                    InstalledApplicationFields {
                        installation: Some("PDT"),
                        metadata: Some(&metadata),
                        package,
                        ..Default::default()
                    },
                    pending,
                );
                let owner = field(&rows, role, "package");
                let root = field(&rows, role, "root package");
                if package.is_some_and(|package| !package.is_empty()) {
                    let owner = owner.unwrap();
                    assert_eq!(owner.value, "bash 5.3.15-2 [PDP]");
                    assert_eq!(owner.scope, InfoFieldScope::Installation);
                    assert_eq!(owner.binary_is_current, None);
                    assert_eq!(root.unwrap().value, "base 3-3 [PEP]");
                } else {
                    assert_eq!(
                        owner.map(|row| row.value.as_str()),
                        pending.then_some("Loading...")
                    );
                    assert!(root.is_none());
                }
            }
        }
    }
}

#[test]
fn hosted_package_rows_use_actual_installed_identities_not_inferred_upstream_packages() {
    for runtime in ["Node.js", "Python", "Java", "Shell"] {
        for classification in [None, Some("NET"), Some("PET"), Some("FRK"), Some("DEV")] {
            for inferred in [false, true] {
                let mut version = ProcessVersion {
                    runtime_name: Some(runtime),
                    application_installation: classification.map(str::to_owned),
                    application_package_target: Some(PackageTarget {
                        name: "script-package".into(),
                        inferred,
                    }),
                    ..Default::default()
                };
                if runtime == "Node.js" {
                    version.application = Some(NodeApplication {
                        package: "@example/installed-tool".into(),
                        installed_version: "1.0".into(),
                        entry_point: "/fixture/cli.js".into(),
                        manifest: "/fixture/package.json".into(),
                        identification: "Installed manifest",
                    });
                } else {
                    version.script_application = Some(ScriptApplication {
                        name: "tool".into(),
                        target: "/fixture/tool".into(),
                        entry_point: Some("/fixture/tool".into()),
                        descriptor: None,
                        kind: runtime,
                        identification: "Captured argument",
                    });
                }
                let rows = application_version_rows("Application", Some(&version), false);
                let expected = if matches!(classification, Some("FRK" | "DEV")) {
                    None
                } else if runtime == "Node.js" {
                    Some("@example/installed-tool")
                } else {
                    (!inferred).then_some("script-package")
                };
                assert_eq!(
                    field(&rows, "Application", "package").map(|row| row.value.as_str()),
                    expected,
                    "{runtime}, {classification:?}, inferred={inferred}"
                );
                assert!(field(&rows, "Application", "latest version (Pacman/AUR)").is_some());
            }
        }
    }
}

#[test]
fn system_package_info_omits_uncompiled_changes_even_while_details_are_pending() {
    for classification in [
        "PEP", "PET", "PEL", "PSP", "PPT", "PDP", "PDT", "PDL", "POP", "AEP", "ASP", "AET", "AEL",
        "APT", "ADP", "ADT", "ADL", "AOP",
    ] {
        for metadata_only in [false, true] {
            let metadata = ProgsMetadata {
                package_type: metadata_only.then(|| format!("[{classification}] Package-managed")),
                ..Default::default()
            };
            for status in [
                None,
                Some("Unknown (no supported local build metadata)"),
                Some("No"),
            ] {
                for role in ["Application", "Runtime"] {
                    let rows = installed_application_rows(
                        role,
                        InstalledApplicationFields {
                            installation: (!metadata_only).then_some(classification),
                            metadata: Some(&metadata),
                            uncompiled_changes: status,
                            ..Default::default()
                        },
                        true,
                    );
                    assert!(
                        field(&rows, role, "uncompiled changes").is_none(),
                        "{classification}"
                    );
                }
            }
        }
    }
    let rows = installed_application_rows(
        "Application",
        InstalledApplicationFields {
            package: Some("owner-package 1.0"),
            ..Default::default()
        },
        false,
    );
    assert!(field(&rows, "Application", "uncompiled changes").is_none());
}

#[test]
fn source_build_info_keeps_unknown_pending_and_changed_results_without_mutating_them() {
    for classification in [None, Some("FRK"), Some("DEV"), Some("CET")] {
        for status in [
            "Unknown",
            "Unknown (no supported local build metadata)",
            "Loading...",
            "No",
            "Yes",
        ] {
            let version = ProcessVersion {
                installation: classification.map(str::to_owned),
                uncompiled_changes: Some(status.into()),
                ..Default::default()
            };
            for role in ["Application", "Runtime"] {
                let rows = process_version_rows(role, Some(&version), false);
                let row = field(&rows, role, "uncompiled changes").unwrap();
                assert_eq!(row.value, status);
                assert_eq!(row.scope, InfoFieldScope::Installation);
            }
            assert_eq!(version.uncompiled_changes.as_deref(), Some(status));
        }
    }
    let rows = installed_application_rows(
        "Application",
        InstalledApplicationFields {
            native: Some(false),
            ..Default::default()
        },
        false,
    );
    assert!(field(&rows, "Application", "uncompiled changes").is_none());
}

#[test]
fn system_package_info_omits_build_system_across_views_even_with_build_evidence() {
    use crate::windows::versions::ApplicationInfo;
    use program_metadata::development::Metadata;

    let mut popup = AppInfoPopup::new(AppInfo {
        name: "Owned fixture".into(),
        exec: "/fixture/not-executed".into(),
        desktop_file_path: "/fixture/tool.desktop".into(),
        icon_path: None,
        comment: None,
        is_settings_module: false,
    });
    for classification in [
        "PEP", "PET", "PEL", "PSP", "PPT", "PDP", "PDT", "PDL", "POP", "AEP", "ASP", "AET", "AEL",
        "APT", "ADP", "ADT", "ADL", "AOP",
    ] {
        for metadata_only in [false, true] {
            let metadata = ProgsMetadata {
                package_type: metadata_only.then(|| format!("[{classification}] Package-managed")),
                ..Default::default()
            };
            for development in [
                None,
                Some(Metadata::default()),
                Some(Metadata {
                    source_languages: vec!["C".into()],
                    build_system: vec!["CMake".into(), "Ninja".into()],
                    ..Default::default()
                }),
            ] {
                let installation = (!metadata_only).then(|| classification.to_owned());
                let version = ProcessVersion {
                    installation: installation.clone(),
                    progs_metadata: metadata.clone(),
                    latest_development: development.clone(),
                    ..Default::default()
                };
                for pending in [false, true] {
                    for role in ["Application", "Runtime", "Custom role"] {
                        let rows = process_version_rows(role, Some(&version), pending);
                        assert!(field(&rows, role, "build system (on disk)").is_none());
                        assert!(field(&rows, role, "application languages (on disk)").is_some());
                    }
                }
                popup.metadata = Some(ApplicationInfo {
                    installation,
                    metadata: metadata.clone(),
                    development,
                    ..Default::default()
                });
                let rows = application_info_rows(&popup);
                assert!(field(&rows, "Application", "build system (on disk)").is_none());
                assert!(field(&rows, "Application", "application languages (on disk)").is_some());
            }
        }
    }
}

#[test]
fn build_system_rows_require_local_source_scope_or_detected_non_package_build_steps() {
    use program_metadata::development::Metadata;

    let configured = Metadata {
        build_system: vec!["Cargo".into()],
        ..Default::default()
    };
    for development in [None, Some(&configured)] {
        let rows = installed_application_rows(
            "Application",
            InstalledApplicationFields {
                development,
                package: Some("owner-package 1.0"),
                ..Default::default()
            },
            false,
        );
        assert!(field(&rows, "Application", "build system (on disk)").is_none());
    }
    for classification in [None, Some("UBI"), Some("FRK"), Some("DEV"), Some("CLO")] {
        for pending in [false, true] {
            let local = matches!(classification, Some("FRK" | "DEV" | "CLO"));
            let rows = installed_application_rows(
                "Application",
                InstalledApplicationFields {
                    installation: classification,
                    ..Default::default()
                },
                pending,
            );
            let build = field(&rows, "Application", "build system (source checkout)");
            assert_eq!(
                build.map(|row| row.value.as_str()),
                local.then_some(if pending { "Loading..." } else { "Unknown" })
            );
            assert!(field(&rows, "Application", "build system (on disk)").is_none());
        }
    }
    let rows = installed_application_rows(
        "Application",
        InstalledApplicationFields {
            development: Some(&configured),
            native: Some(false),
            ..Default::default()
        },
        false,
    );
    let build = field(&rows, "Application", "build system (on disk)").unwrap();
    assert_eq!(build.value, "Cargo");
    assert_eq!(build.scope, InfoFieldScope::Installation);
    assert_eq!(build.binary_is_current, None);
}

#[test]
fn hosted_build_system_rows_use_the_scripts_own_build_steps_and_package_identity() {
    use program_metadata::development::Metadata;

    for runtime in ["Node.js", "Python", "Shell"] {
        for package_managed in [false, true] {
            for configured in [false, true] {
                let version = ProcessVersion {
                    runtime_name: Some(runtime),
                    application_installation: package_managed.then(|| "PDT".into()),
                    script_application: Some(ScriptApplication {
                        name: "tool".into(),
                        target: "/fixture/tool".into(),
                        entry_point: Some("/fixture/tool".into()),
                        kind: runtime,
                        descriptor: None,
                        identification: "Owned fixture",
                    }),
                    application_development: configured.then(|| Metadata {
                        build_system: vec!["npm".into(), "tsc".into()],
                        ..Default::default()
                    }),
                    latest_development: Some(Metadata {
                        build_system: vec!["CMake".into(), "Ninja".into()],
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                let rows = application_version_rows("Application", Some(&version), false);
                assert_eq!(
                    field(&rows, "Application", "build system (on disk)")
                        .map(|row| row.value.as_str()),
                    (configured && !package_managed).then_some("npm → tsc")
                );
                assert!(field(&rows, "Application", "application languages (on disk)").is_some());
                assert!(!rows.iter().any(|row| row.value.contains("CMake")));
            }
        }
    }
}
