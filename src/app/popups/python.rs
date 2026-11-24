use super::*;
use crate::windows::versions::ProcessVersion;

fn row(role: &str, scope: InfoFieldScope, label: &str, value: String) -> InfoPopupRow {
    InfoPopupRow {
        label: info_metadata_label(role, label),
        scope,
        value,
        searched: false,
        separator_before: false,
        binary_is_current: None,
    }
}

pub(super) fn interpreter_name(version: &ProcessVersion) -> &'static str {
    // The shared reader binds Python's ELF version object to CPython Py_Version;
    // a filename, configured environment or plain version banner does not.
    if version.running_unavailable.is_none()
        && version.version_source
            == Some(program_metadata::executable::VersionSource::ElfVersionObject)
        && version
            .version
            .as_deref()
            .is_some_and(|value| value.starts_with("Python "))
    {
        "CPython"
    } else {
        "Python"
    }
}

pub(super) fn context_rows(role: &str, version: &ProcessVersion) -> Vec<InfoPopupRow> {
    let scope = InfoFieldScope::InstanceContext;
    let Some(context) = &version.python_context else {
        return version
            .python_context_error
            .as_ref()
            .map(|reason| {
                row(
                    role,
                    scope,
                    "script context",
                    format!("Unavailable ({reason})"),
                )
            })
            .into_iter()
            .collect();
    };
    let mut rows = vec![row(
        role,
        scope,
        "script arguments",
        if context.arguments.is_empty() {
            "None".into()
        } else {
            serde_json::to_string(&context.arguments).expect("string arguments serialize")
        },
    )];
    if !context.interpreter_options.is_empty() {
        rows.push(row(
            role,
            scope,
            "interpreter options",
            serde_json::to_string(&context.interpreter_options).expect("string options serialize"),
        ));
    }
    if let Some(environment) = &context.environment {
        rows.push(row(
            role,
            scope,
            if environment.kind == "Conda" {
                "Conda environment"
            } else {
                "virtual environment"
            },
            environment.root.display().to_string(),
        ));
        if let Some(launch) = &context.interpreter_launch {
            rows.push(row(
                role,
                scope,
                "environment interpreter",
                launch.display().to_string(),
            ));
        }
    } else {
        let value = context.environment_issue.as_ref().map_or_else(
            || "None detected from recorded interpreter launch path".into(),
            |reason| format!("Unknown ({reason})"),
        );
        rows.push(row(role, scope, "virtual environment", value));
        if let Some(hint) = &context.virtual_environment_hint {
            rows.push(row(
                role,
                scope,
                "inherited VIRTUAL_ENV",
                format!(
                    "{} (inherited marker; not a verified active environment)",
                    hint.display()
                ),
            ));
        }
    }
    rows
}

pub(super) fn installation_rows(role: &str, version: &ProcessVersion) -> Vec<InfoPopupRow> {
    let Some(context) = &version.python_context else {
        return Vec::new();
    };
    let scope = InfoFieldScope::Installation;
    let mut rows = Vec::new();
    let mut add = |label: &str, value: String| rows.push(row(role, scope, label, value));
    if let Some(environment) = &context.environment {
        for (label, path) in [
            ("environment configuration", &environment.configuration),
            (
                "base interpreter (configured)",
                &environment.base_executable,
            ),
        ] {
            if let Some(path) = path {
                add(label, path.display().to_string());
            }
        }
        if environment.base_executable.is_none()
            && let Some(path) = &environment.base_directory
        {
            add(
                "base interpreter directory (configured)",
                path.display().to_string(),
            );
        }
        if let Some(creator) = &environment.created_with {
            add("environment created with", creator.clone());
        }
        if let Some(includes) = environment.includes_system_packages {
            add(
                "system packages (configured)",
                if includes { "Included" } else { "Excluded" }.into(),
            );
        }
        if !environment.package_directories.is_empty() {
            add(
                "environment package directories",
                environment
                    .package_directories
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            );
        }
    }
    if let Some(distribution) = &context.distribution {
        if let Some(installer) = &distribution.installer {
            add("package installer", installer.clone());
        }
        if let Some(entry) = &distribution.entry_point {
            add("installed entry point", entry.clone());
        }
        if !context.environment.as_ref().is_some_and(|environment| {
            environment
                .package_directories
                .contains(&distribution.location)
        }) {
            add(
                "package installation directory",
                distribution.location.display().to_string(),
            );
        }
    }
    if let Some(path) = &context.dependency_metadata
        && !version
            .application_installed_metadata
            .as_ref()
            .is_some_and(|metadata| &metadata.metadata_path == path)
    {
        add("dependency metadata", path.display().to_string());
    }
    if let Some(hash) = &context.script_sha256 {
        add("script SHA256 (on disk)", hash.clone());
    }
    if !context.notes.is_empty() {
        add("inspection notes", context.notes.join("; "));
    }
    rows
}

pub(super) fn dependency_sections(
    ctx: &egui::Context,
    parent: egui::Id,
    version: Option<&ProcessVersion>,
) -> Vec<InfoMetadataSection> {
    let Some(context) = version
        .and_then(|version| version.python_context.as_ref())
        .filter(|context| !context.dependencies.is_empty())
    else {
        return Vec::new();
    };
    let id = parent.with("python-dependencies");
    let rows = if process_info_section_is_open(ctx, id, false) {
        context
            .dependencies
            .iter()
            .map(|dependency| {
                let availability = if dependency.ambiguous {
                    "ambiguous installed metadata"
                } else if let Some(version) = &dependency.installed_version {
                    version
                } else if dependency.installed_lookup_complete {
                    "not found in inspected package directories"
                } else {
                    "not checked"
                };
                row(
                    "Application",
                    InfoFieldScope::Installation,
                    &dependency.name,
                    format!("{} [installed: {availability}]", dependency.requirement),
                )
            })
            .collect()
    } else {
        Vec::new()
    };
    vec![InfoMetadataSection {
        id,
        header: format!("Declared dependencies ({})", context.dependencies.len()).into(),
        header_scope: Some(InfoFieldScope::Installation),
        field_header: None,
        binary_is_current: None,
        default_open: false,
        rows,
        children: Vec::new(),
    }]
}

pub(super) fn module_sections(
    ctx: &egui::Context,
    parent: egui::Id,
    version: Option<&ProcessVersion>,
) -> Vec<InfoMetadataSection> {
    let Some(version) = version else {
        return Vec::new();
    };
    let Some(context) = &version.python_context else {
        return Vec::new();
    };
    let report = context.project_modules.as_ref();
    let entry = version
        .script_application
        .as_ref()
        .and_then(|script| script.entry_point.as_deref());
    let modules: Vec<_> = report
        .map(|report| {
            report
                .modules
                .iter()
                .filter(|module| Some(module.path.as_path()) != entry)
                .collect()
        })
        .unwrap_or_default();
    let id = parent.with("python-project-modules");
    let header = match report {
        Some(report) => format!(
            "Referenced project modules ({}){}",
            modules.len(),
            if report.complete {
                ""
            } else {
                " — incomplete"
            },
        ),
        None => "Referenced project modules (unknown)".into(),
    };
    let rows = if process_info_section_is_open(ctx, id, false) {
        let scope = InfoFieldScope::Installation;
        let mut rows = vec![row(
            "Application",
            scope,
            "evidence",
            "Static on-disk references; not a loaded-module list".into(),
        )];
        match report {
            None => rows.push(row(
                "Application",
                scope,
                "status",
                "Unknown (entry file or project-root inspection unavailable)".into(),
            )),
            Some(report) => {
                if !report.complete {
                    rows.push(row(
                        "Application",
                        scope,
                        "status",
                        "Static analysis is incomplete; additional project references may be unresolved".into(),
                    ));
                } else if modules.is_empty() {
                    rows.push(row(
                        "Application",
                        scope,
                        "status",
                        "No statically referenced project modules found".into(),
                    ));
                }
                for module in modules {
                    let mut value = module.path.display().to_string();
                    if module.conditional {
                        value.push_str(" · conditional reference");
                    }
                    if !module.referenced_by.is_empty() {
                        value.push_str(" · referenced by: ");
                        value.push_str(
                            &module
                                .referenced_by
                                .iter()
                                .map(|path| path.display().to_string())
                                .collect::<Vec<_>>()
                                .join("; "),
                        );
                    }
                    rows.push(InfoPopupRow {
                        label: module
                            .path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| module.path.display().to_string()),
                        scope,
                        value,
                        searched: false,
                        separator_before: false,
                        binary_is_current: None,
                    });
                }
                if !report.notes.is_empty() {
                    rows.push(row(
                        "Application",
                        scope,
                        "inspection notes",
                        report.notes.join("; "),
                    ));
                }
            }
        }
        rows
    } else {
        Vec::new()
    };
    vec![InfoMetadataSection {
        id,
        header: header.into(),
        header_scope: Some(InfoFieldScope::Installation),
        field_header: None,
        binary_is_current: None,
        default_open: false,
        rows,
        children: Vec::new(),
    }]
}

#[cfg(test)]
mod modules_tests;
#[cfg(test)]
mod tests;
