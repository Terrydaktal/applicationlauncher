//! Cheap change hints read only on the metadata worker. These are not authority
//! receipts: a change triggers the shared guarded metadata reader again.
use super::super::ProcessVersion;
use program_metadata::build_status::artifact::ArtifactStamp;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub(super) struct Snapshot {
    route: PathBuf,
    canonical: Option<PathBuf>,
    entry: Option<ArtifactStamp>,
    record: Option<ArtifactStamp>,
    stamp: Result<Option<program_metadata::interpreted::InstalledVersion>, String>,
    descriptor: Option<program_metadata::interpreted::script::DescriptorSource>,
    details: Option<program_metadata::interpreted::script::DescriptorDetails>,
    details_current: bool,
    comparison_route: Option<PathBuf>,
    comparison_application: Option<super::super::ScriptApplication>,
    project_modules: Option<program_metadata::interpreted::python_process::PythonProjectModules>,
    project_modules_current: bool,
}
impl PartialEq for Snapshot {
    fn eq(&self, other: &Self) -> bool {
        // Compare observations, not the retained authority used to validate
        // them. `current` always reuses and rechecks that same receipt.
        self.route == other.route
            && self.canonical == other.canonical
            && self.entry == other.entry
            && self.record == other.record
            && self.stamp == other.stamp
            && self.details_current == other.details_current
            && self.comparison_route == other.comparison_route
            && self.project_modules_current == other.project_modules_current
    }
}
impl Eq for Snapshot {}
impl Snapshot {
    pub(super) fn for_version(version: &ProcessVersion, cwd: Option<&Path>) -> Option<Self> {
        let application = version.script_application.as_ref()?;
        let entry = application.entry_point.as_ref();
        if entry.is_none() && version.script_details.is_none() {
            return None;
        }
        let target = Path::new(&application.target);
        let route = if let Some(descriptor) = &application.descriptor {
            descriptor.process_path().to_path_buf()
        } else if target.is_absolute() {
            target.to_path_buf()
        } else {
            cwd.map(|cwd| cwd.join(target)).or_else(|| entry.cloned())?
        };
        let mut result = Self::observe(route, application.descriptor.clone());
        // If an alias changed after the metadata query, retain the query's old
        // canonical target as the baseline so the next check retries it.
        if let Some(entry) = entry {
            result.canonical = Some(entry.clone());
        }
        if let Some(stamp) = version
            .application_development
            .as_ref()
            .and_then(|metadata| metadata.artifact_stamp)
        {
            // A script edit between inspection and baseline capture must not
            // make the older development observations look freshly checked.
            result.entry = Some(stamp);
        }
        result.details = version.script_details.clone();
        // Baseline means the query's receipt was current, not that a stale
        // receipt is now normal. A change racing publication must trigger the
        // next worker refresh instead of becoming a permanently equal baseline.
        result.details_current = true;
        result.comparison_route = result
            .details
            .as_ref()
            .and_then(|details| details.candidate.as_ref())
            .map(|candidate| candidate.route.clone());
        result.comparison_application = result.details.as_ref().map(|_| application.clone());
        result.project_modules = version
            .python_context
            .as_ref()
            .and_then(|context| context.project_modules.clone());
        // Keep the inspection's observations as the baseline, including when
        // an imported source changed before this snapshot was constructed.
        result.project_modules_current = true;
        Some(result)
    }
    fn observe(
        route: PathBuf,
        descriptor: Option<program_metadata::interpreted::script::DescriptorSource>,
    ) -> Self {
        // Never inspect a reused descriptor, even for a cheap refresh hint.
        if descriptor
            .as_ref()
            .is_some_and(|source| !source.unchanged())
        {
            return Self::changed_descriptor(route, descriptor);
        }
        let canonical = fs::canonicalize(&route).ok();
        let entry = fs::metadata(&route)
            .ok()
            .map(|metadata| ArtifactStamp::from_metadata(&metadata));
        let record = canonical.as_ref().and_then(|entry| {
            let mut name = entry.file_name()?.to_os_string();
            name.push(".program-metadata.json");
            fs::symlink_metadata(entry.with_file_name(name))
                .ok()
                .map(|metadata| ArtifactStamp::from_metadata(&metadata))
        });
        let stamp = program_metadata::interpreted::stamped_script_version_until(
            &route,
            std::time::Instant::now() + std::time::Duration::from_millis(750),
        );
        if descriptor
            .as_ref()
            .is_some_and(|source| !source.unchanged())
        {
            return Self::changed_descriptor(route, descriptor);
        }
        Self {
            route,
            canonical,
            entry,
            record,
            stamp,
            descriptor,
            details: None,
            details_current: true,
            comparison_route: None,
            comparison_application: None,
            project_modules: None,
            project_modules_current: true,
        }
    }
    fn changed_descriptor(
        route: PathBuf,
        descriptor: Option<program_metadata::interpreted::script::DescriptorSource>,
    ) -> Self {
        Self {
            route,
            canonical: None,
            entry: None,
            record: None,
            stamp: Err("Script descriptor or process changed during refresh".into()),
            descriptor,
            details: None,
            details_current: true,
            comparison_route: None,
            comparison_application: None,
            project_modules: None,
            project_modules_current: true,
        }
    }
    pub(super) fn current(&self) -> Self {
        let mut current = Self::observe(self.route.clone(), self.descriptor.clone());
        current.details = self.details.clone();
        current.comparison_application = self.comparison_application.clone();
        current.details_current = self
            .details
            .as_ref()
            .is_none_or(|details| details.unchanged());
        current.comparison_route = self
            .comparison_application
            .as_ref()
            .and_then(super::super::script_comparison_candidate);
        current.project_modules = self.project_modules.clone();
        current.project_modules_current = self
            .project_modules
            .as_ref()
            .is_none_or(|modules| modules.observations_current());
        current
    }
}
