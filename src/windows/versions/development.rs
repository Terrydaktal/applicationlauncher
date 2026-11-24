//! Detailed-worker snapshots: current checkout declarations are source-only;
//! binary-only observations belong to their selected artifact, never its runtime.
use super::{ProcessVersion, ScriptApplication};
use program_metadata::{build_status::artifact::ArtifactStamp, development::Metadata};
use std::{
    fs::File,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Instant,
};

#[cfg(test)]
mod tests;

pub(super) fn inspect_process(
    result: &mut ProcessVersion,
    running: Option<&File>,
    installed: Option<&File>,
    running_path: &Path,
    installed_path: Option<&Path>,
    deadline: Instant,
) {
    let mut checkouts = Vec::new();
    let checkout = result.progs_metadata.source_checkout.as_deref();
    if let Some(checkout) = checkout {
        let snapshot = checkout_snapshot(
            &mut checkouts,
            installed_path.unwrap_or(running_path),
            checkout,
            deadline,
        );
        result.development = Some(snapshot.clone());
        result.latest_development = Some(snapshot);
    } else if super::development_source_managed(
        result.installation.as_deref(),
        Some(&result.progs_metadata),
    ) {
        let snapshot = Metadata::unavailable_checkout();
        result.development = Some(snapshot.clone());
        result.latest_development = Some(snapshot);
    } else {
        result.development =
            running.map(|file| inspect_selected(file, running_path, None, false, deadline));
        result.latest_development = installed.zip(installed_path).map(|(file, path)| {
            // Reuse only an exact, freshly revalidated artifact observation. The
            // same pathname/build family is not enough after an installation update.
            result
                .development
                .as_ref()
                .filter(|metadata| same_artifact(metadata, running, file))
                .cloned()
                .unwrap_or_else(|| inspect_selected(file, path, None, false, deadline))
        });
    }
    let checkout = result.application_progs_metadata.source_checkout.as_deref();
    result.application_development = if let Some(checkout) = checkout {
        let logical = result
            .application
            .as_ref()
            .map(|application| application.entry_point.as_path())
            .or_else(|| {
                result
                    .script_application
                    .as_ref()
                    .and_then(|script| script.entry_point.as_deref())
            })
            .unwrap_or(running_path);
        Some(checkout_snapshot(
            &mut checkouts,
            logical,
            checkout,
            deadline,
        ))
    } else if super::development_source_managed(
        result.application_installation.as_deref(),
        Some(&result.application_progs_metadata),
    ) {
        Some(Metadata::unavailable_checkout())
    } else if let Some(script) = &result.script_application {
        inspect_script(script, None, deadline)
    } else {
        result.application.as_ref().map(|application| {
            program_metadata::development::inspect_artifact(
                &application.entry_point,
                None,
                deadline,
            )
        })
    };
    for snapshot in [
        &mut result.development,
        &mut result.latest_development,
        &mut result.application_development,
    ] {
        if snapshot.as_ref().is_some_and(|metadata| {
            metadata.is_source_checkout() && !metadata.language_dependencies_current()
        }) {
            *snapshot = None;
        }
    }
}

fn checkout_snapshot(
    cache: &mut Vec<(PathBuf, PathBuf, Metadata)>,
    logical: &Path,
    checkout: &Path,
    deadline: Instant,
) -> Metadata {
    if let Some((_, _, snapshot)) = cache
        .iter()
        .find(|(root, target, _)| root == checkout && target == logical)
    {
        return snapshot.clone();
    }
    let snapshot = program_metadata::development::inspect_checkout(logical, checkout, deadline);
    cache.push((checkout.into(), logical.into(), snapshot.clone()));
    snapshot
}

pub(super) fn inspect_selected(
    file: &File,
    path: &Path,
    checkout: Option<&Path>,
    source_managed: bool,
    deadline: Instant,
) -> Metadata {
    if let Some(checkout) = checkout {
        program_metadata::development::inspect_checkout(path, checkout, deadline)
    } else if source_managed {
        Metadata::unavailable_checkout()
    } else {
        program_metadata::development::inspect_pinned(file, path, None, deadline)
    }
}

pub(super) fn checkout_current(metadata: &Metadata) -> bool {
    metadata.is_source_checkout() && metadata.language_dependencies_current()
}

pub(super) fn retain_checkout(metadata: &mut Option<Metadata>) {
    if !metadata.as_ref().is_some_and(checkout_current) {
        *metadata = None;
    }
}

fn same_artifact(metadata: &Metadata, running: Option<&File>, installed: &File) -> bool {
    metadata.language_dependencies_current()
        && metadata.artifact_stamp.is_some_and(|expected| {
            running.is_some_and(|file| stamp(file) == Some(expected))
                && stamp(installed) == Some(expected)
        })
}

fn stamp(file: &File) -> Option<ArtifactStamp> {
    file.metadata()
        .ok()
        .map(|metadata| ArtifactStamp::from_metadata(&metadata))
}

pub(super) fn artifact_unchanged(path: &Path, metadata: &Metadata) -> bool {
    if !metadata.language_dependencies_current() {
        return false;
    }
    if metadata.is_source_checkout() {
        return true;
    }
    let Some(expected) = metadata.artifact_stamp else {
        // An unsupported observation with no asserted facts can stay unknown;
        // affirmative facts without an artifact identity must never survive.
        return metadata.source_languages.is_empty()
            && metadata.application_languages.is_empty()
            && metadata.binary_languages.is_empty()
            && metadata.build_system.is_empty();
    };
    std::fs::metadata(path)
        .ok()
        .is_some_and(|current| ArtifactStamp::from_metadata(&current) == expected)
}

fn inspect_script(
    script: &ScriptApplication,
    checkout: Option<&Path>,
    deadline: Instant,
) -> Option<Metadata> {
    if let Some(checkout) = checkout {
        return Some(program_metadata::development::inspect_checkout(
            script
                .entry_point
                .as_deref()
                .unwrap_or_else(|| Path::new(&script.target)),
            checkout,
            deadline,
        ));
    }
    if !script.unchanged() {
        return None;
    }
    let inspected = if let Some(descriptor) = &script.descriptor {
        // /proc/self/fd is relative to the inspecting process. Follow only the
        // bound target-process route, including anonymous/deleted retained files.
        let route = descriptor.process_path();
        let file = File::options()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(route)
            .ok()?;
        if !descriptor.matches_file(&file) {
            return None;
        }
        let metadata = program_metadata::development::inspect_pinned(
            &file,
            script.entry_point.as_deref().unwrap_or(route),
            checkout,
            deadline,
        );
        if !descriptor.matches_file(&file) {
            return None;
        }
        metadata
    } else {
        program_metadata::development::inspect_artifact(
            script.entry_point.as_deref()?,
            checkout,
            deadline,
        )
    };
    script.unchanged().then_some(inspected)
}
