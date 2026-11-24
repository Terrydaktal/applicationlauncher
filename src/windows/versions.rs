use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
#[cfg(test)]
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

mod application;
mod available;
mod collection;
#[cfg(test)]
mod descriptor_tests;
mod development;
mod freshness;
mod installation;
mod lookup;
mod node;
mod npm;
mod pi;
#[cfg(test)]
mod probe_reuse_tests;
mod script;

pub(crate) use application::{
    ApplicationInfo, ApplicationInfoUpdate, ApplicationLaunch, DesktopFileSearch,
    inspect_application,
};
#[cfg(test)]
pub(crate) use available::PackageVersionSource;
#[cfg(test)]
pub(crate) use available::fixture as available_package_fixture;
pub(crate) use available::{AvailablePackageVersion, PackageTarget};
pub(crate) use installation::ProgsMetadata;
pub(crate) use lookup::ProcessVersionCache;
pub(crate) use lookup::{PI_RECEIPT_REFRESH, SCRIPT_METADATA_REFRESH};
pub(crate) use node::NodeApplication;
pub(crate) use npm::NpmTarget;
pub(crate) use pi::{BuildStatus as PiBuildStatus, PiBuildEvidence};
pub(crate) use script::ScriptApplication;
pub(crate) fn runtime_name(name: &str) -> Option<&'static str> {
    // An interactive shell remains a native program. It becomes a script
    // runtime only after the passive argv parser finds a concrete file entry.
    script::runtime_name(name).filter(|runtime| *runtime != "Shell")
}

const CACHE_TTL: Duration = Duration::from_secs(300);
const MAX_CACHE_ENTRIES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessVersionTarget {
    pid: i32,
    executable: PathBuf,
    launch_context: ProcessLaunchContext,
    program: Option<String>,
    application_entry: Option<crate::models::AppInfo>,
    window_declared_entry: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ProcessLaunchContext {
    raw_argv0: Option<PathBuf>,
    // Set only when the captured /proc executable was unreadable. argv[0]
    // remains a candidate, never evidence for the running image.
    executable_unavailable: Option<&'static str>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ProcessVersion {
    pub(crate) details_pending: bool,
    pub(crate) source_check: Option<std::sync::Arc<freshness::Check>>,
    pub(crate) development: Option<program_metadata::development::Metadata>,
    pub(crate) latest_development: Option<program_metadata::development::Metadata>,
    pub(crate) application_development: Option<program_metadata::development::Metadata>,
    pub(crate) running_unavailable: Option<&'static str>,
    pub(crate) on_disk_candidate: Option<PathBuf>,
    pub(crate) on_disk_candidate_reason: Option<String>,
    pub(crate) runtime_name: Option<&'static str>,
    pub(crate) application: Option<NodeApplication>,
    pub(crate) script_application: Option<ScriptApplication>,
    pub(crate) script_details: Option<program_metadata::interpreted::script::DescriptorDetails>,
    pub(crate) script_details_error: Option<String>,
    pub(crate) python_context:
        Option<program_metadata::interpreted::python_process::PythonProcessMetadata>,
    pub(crate) python_context_error: Option<String>,
    pub(crate) application_installed_metadata:
        Option<program_metadata::interpreted::InstalledVersion>,
    pub(crate) application_metadata_error: Option<String>,
    pub(crate) application_assets_unverified: bool,
    pub(crate) application_build: Option<PiBuildEvidence>,
    pub(crate) application_installation: Option<String>,
    pub(crate) application_progs_metadata: ProgsMetadata,
    pub(crate) application_package_target: Option<PackageTarget>,
    pub(crate) application_latest_package_version: Option<AvailablePackageVersion>,
    pub(crate) application_npm_target: Option<NpmTarget>,
    pub(crate) application_latest_npm_version: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) version_source: Option<program_metadata::executable::VersionSource>,
    pub(crate) runtime_version: Option<program_metadata::runtime::RuntimeVersion>,
    pub(crate) version_reason: Option<String>,
    pub(crate) version_issue: Option<program_metadata::executable::MetadataIssue>,
    pub(crate) latest_version: Option<String>,
    pub(crate) latest_version_reason: Option<String>,
    pub(crate) latest_version_issue: Option<program_metadata::executable::MetadataIssue>,
    pub(crate) package_target: Option<PackageTarget>,
    pub(crate) latest_package_version: Option<AvailablePackageVersion>,
    pub(crate) npm_target: Option<NpmTarget>,
    pub(crate) latest_npm_version: Option<String>,
    pub(crate) latest_build_id: Option<String>,
    pub(crate) latest_build_id_reason: Option<String>,
    pub(crate) running_latest_binary: Option<bool>,
    pub(crate) uncompiled_changes: Option<String>,
    pub(crate) package: Option<String>,
    pub(crate) installation: Option<String>,
    pub(crate) progs_metadata: ProgsMetadata,
    pub(crate) build_id: Option<String>,
    pub(crate) build_id_reason: Option<String>,
    pub(crate) launch: Option<ApplicationLaunch>,
    pub(crate) application_launch: Option<ApplicationLaunch>,
}

impl ProcessVersion {

    /// A script descriptor can change while its interpreter remains the same
    /// process. Invalidate only metadata attributed through that descriptor.
    fn discard_changed_script_metadata(&mut self) -> bool {
        if self
            .script_application
            .as_ref()
            .is_none_or(ScriptApplication::unchanged)
        {
            self.discard_changed_script_details();
            return false;
        }
        self.script_application = None;
        self.script_details = None;
        self.script_details_error =
            Some("Script descriptor or process changed during inspection".into());
        self.python_context = None;
        self.python_context_error = None;
        self.application_installed_metadata = None;
        self.application_metadata_error =
            Some("Script descriptor or process changed during inspection".into());
        self.application_build = None;
        development::retain_checkout(&mut self.application_development);
        self.application_installation = None;
        self.application_progs_metadata = ProgsMetadata::default();
        self.application_package_target = None;
        self.application_latest_package_version = None;
        self.application_npm_target = None;
        self.application_latest_npm_version = None;
        self.application_launch = None;
        true
    }

    fn discard_changed_script_details(&mut self) {
        if self
            .script_details
            .as_ref()
            .is_some_and(|details| !details.unchanged())
        {
            self.script_details = None;
            self.script_details_error = Some(
                "Retained script or installed-wrapper candidate changed during inspection".into(),
            );
        }
    }

    pub(crate) fn process_header_is_current(&self) -> Option<bool> {
        // A shell process identifies the interpreter, even after expanded
        // inspection discovers its script. Its executable's verified status
        // must not disappear on expansion or certify the hosted script below.
        // Other hosted applications retain their existing receipt-based policy.
        if self.runtime_name == Some("Shell") && self.running_unavailable.is_none() {
            return self.running_latest_binary;
        }
        self.process_build_is_current()
    }

    pub(crate) fn process_build_is_current(&self) -> Option<bool> {
        if self.running_unavailable.is_some() {
            return None;
        }
        if let Some(build) = &self.application_build {
            return match build.status {
                PiBuildStatus::Current => Some(true),
                PiBuildStatus::RestartRequired | PiBuildStatus::InstallationUpdateRequired => {
                    Some(false)
                }
                PiBuildStatus::Unknown => None,
            };
        }
        // The interpreter's ELF identity does not prove which application code was loaded.
        if self.application.is_some()
            || self.script_application.is_some()
            || self.runtime_name.is_some()
            || self
                .application_installed_metadata
                .as_ref()
                .is_some_and(|metadata| metadata.kind == "Electron package")
            || self.application_metadata_error.is_some()
            || self.application_assets_unverified
        {
            return None;
        }
        self.running_latest_binary
    }

    fn discard_running_metadata(&mut self, reason: &'static str) {
        self.running_unavailable = Some(reason);
        development::retain_checkout(&mut self.development);
        development::retain_checkout(&mut self.application_development);
        self.version = None;
        self.version_source = None;
        self.runtime_version = None;
        self.version_reason = None;
        self.version_issue = None;
        self.build_id = None;
        self.build_id_reason = None;
        self.running_latest_binary = None;
        self.application = None;
        self.script_application = None;
        self.python_context = None;
        self.python_context_error = None;
        if self
            .application_installed_metadata
            .as_ref()
            .is_some_and(|metadata| metadata.kind != "Electron package")
        {
            self.application_installed_metadata = None;
        }
        self.application_build = None;
        self.application_installation = None;
        self.application_progs_metadata = ProgsMetadata::default();
        self.application_package_target = None;
        self.application_latest_package_version = None;
        self.application_npm_target = None;
        self.application_latest_npm_version = None;
        self.application_launch = None;
    }
}

pub(crate) fn development_source_managed(
    installation: Option<&str>,
    metadata: Option<&ProgsMetadata>,
) -> bool {
    metadata.is_some_and(|metadata| metadata.source_checkout.is_some())
        || [
            installation,
            metadata.and_then(|metadata| metadata.package_type.as_deref()),
        ]
        .into_iter()
        .flatten()
        .filter_map(crate::popups::installation_badge)
        .any(|badge| matches!(badge, "FRK" | "DEV" | "CLO"))
}

#[derive(Clone, Debug, Default)]
struct ExecutableVersion {
    version: Option<String>,
    source: Option<program_metadata::executable::VersionSource>,
    reason: Option<String>,
    reason_kind: Option<program_metadata::executable::MetadataIssue>,
    build_id: Option<String>,
    build_id_reason: Option<String>,
    components: Vec<program_metadata::executable::ExecutableComponent>,
    components_required: bool,
}

type ExecutableIdentity = program_metadata::build_status::artifact::ArtifactStamp;

fn executable_identity(metadata: &std::fs::Metadata) -> ExecutableIdentity {
    ExecutableIdentity::from_metadata(metadata)
}

fn same_executable_build(
    running: &ExecutableVersion,
    latest: &ExecutableVersion,
    same_file: bool,
) -> Option<bool> {
    let main = applicationlauncher::freshness::same_native_build(
        running.build_id.as_deref(),
        latest.build_id.as_deref(),
        same_file,
    );
    if main == Some(false) {
        return main;
    }
    if !running.components_required
        && !latest.components_required
        && running.components.is_empty()
        && latest.components.is_empty()
    {
        return main;
    }
    if running.components.is_empty() || running.components.len() != latest.components.len() {
        return None;
    }
    // A private product image can change while the main executable stays the
    // same. Its pinned identity/build ID is independent evidence, never the
    // main image's build ID or a bare version-string comparison.
    let mut components_current = Some(true);
    for (running, latest) in running.components.iter().zip(&latest.components) {
        if running.path.file_name() != latest.path.file_name() {
            return None;
        }
        match applicationlauncher::freshness::same_native_build(
            running.build_id.as_deref(),
            latest.build_id.as_deref(),
            running.identity == latest.identity,
        ) {
            Some(false) => return Some(false),
            None => components_current = None,
            Some(true) => {}
        }
    }
    main.and(components_current)
}

fn clean_executable(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().trim_end_matches(" (deleted)"))
}

#[cfg(test)]
fn valid_package_name(name: &str) -> bool {
    program_metadata::package::valid_package_name(name)
}

fn installed_package(path: &Path) -> Option<String> {
    program_metadata::package::installed_owner(path)
}

fn is_elf(file: &File) -> bool {
    program_metadata::executable::is_elf(file)
}

#[derive(Clone, Copy)]
enum ExecutableContext {
    OnDisk,
    Process(i32),
}

#[cfg(test)]
fn probe_executable(file: &File, name: &str) -> ExecutableVersion {
    let descriptor = format!("/proc/self/fd/{}", file.as_raw_fd());
    let path = std::fs::read_link(descriptor)
        .map(|path| clean_executable(&path))
        .unwrap_or_else(|_| PathBuf::from(name));
    probe_executable_context(file, &path, ExecutableContext::OnDisk)
}

fn probe_executable_context(
    file: &File,
    path: &Path,
    context: ExecutableContext,
) -> ExecutableVersion {
    let Ok(identity) = file.metadata() else {
        return ExecutableVersion::default();
    };
    type CacheKey = (PathBuf, ExecutableIdentity);
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, (Instant, ExecutableVersion)>>> =
        OnceLock::new();
    let key = (path.to_owned(), executable_identity(&identity));
    // This inner cache is keyed only by the main image. Re-read supported
    // private version components on each worker query, and never share a
    // process-bound component observation with an installed-image query.
    // A local installation's explicit probe authorization can be granted or
    // revoked independently of the executable's inode. Reconsult that policy
    // on each bounded worker query instead of caching an old denial (or grant).
    let cacheable = identity.uid() == 0
        && (path.starts_with("/usr/") || path.starts_with("/bin/"))
        && !program_metadata::executable::pinned_requires_product_components(file, path);
    let cache = CACHE.get_or_init(Default::default);
    if cacheable
        && let Some(result) = cache.lock().ok().and_then(|cache| {
            cache
                .get(&key)
                .filter(|(checked, _)| checked.elapsed() < CACHE_TTL)
                .map(|(_, result)| result.clone())
        })
    {
        return result;
    }
    let policy = program_metadata::executable::ProbePolicy::TrustedInstallations;
    let metadata = match context {
        ExecutableContext::OnDisk => {
            program_metadata::executable::inspect_on_disk(file, path, policy)
        }
        ExecutableContext::Process(pid) => {
            program_metadata::executable::inspect_process(file, path, policy, pid)
        }
    };
    let result = ExecutableVersion {
        source: metadata.version.as_ref().map(|version| version.source),
        version: metadata.version.map(|version| version.value),
        reason: metadata.reason,
        reason_kind: metadata.reason_kind,
        build_id: metadata.build_id,
        build_id_reason: metadata
            .build_id_issue
            .map(|issue| issue.description().into()),
        components: metadata.components,
        components_required: metadata.components_required,
    };
    if cacheable && let Ok(mut cache) = cache.lock() {
        cache.retain(|_, (checked, _)| checked.elapsed() < CACHE_TTL);
        if cache.len() >= MAX_CACHE_ENTRIES
            && let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (checked, _))| *checked)
                .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest);
        }
        cache.insert(key, (Instant::now(), result.clone()));
    }
    result
}

fn process_argv0(pid: i32) -> Option<PathBuf> {
    let mut bytes = Vec::new();
    File::open(format!("/proc/{pid}/cmdline"))
        .ok()?
        .take(16_385)
        .read_to_end(&mut bytes)
        .ok()?;
    let end = bytes.iter().take(16_384).position(|byte| *byte == 0)?;
    (end > 0).then(|| PathBuf::from(std::ffi::OsString::from_vec(bytes[..end].to_vec())))
}

fn process_launch_from_argv0(
    target: &ProcessVersionTarget,
    expected: &std::fs::Metadata,
) -> Option<program_metadata::launch::PathResolution> {
    let launcher = target.launch_context.raw_argv0.as_ref()?;
    if !launcher.is_absolute() || process_argv0(target.pid).as_ref() != Some(launcher) {
        return None;
    }
    let resolved = program_metadata::launch::resolve_path(launcher);
    if !resolved.unchanged()
        || !std::fs::metadata(&resolved.path)
            .is_ok_and(|actual| executable_identity(&actual) == executable_identity(expected))
    {
        return None;
    }
    Some(resolved)
}

/// argv[0] is mutable process data, not an executable identity. Inspecting this
/// candidate is strictly passive and cannot establish a running version/build.
fn probe_on_disk_candidate(
    target: &ProcessVersionTarget,
    identity_is_current: impl Fn() -> bool,
    details: bool,
    defer_source_check: bool,
) -> ProcessVersion {
    let unavailable = target
        .launch_context
        .executable_unavailable
        .unwrap_or("process executable unavailable; absolute argv[0] is only an on-disk candidate");
    let mut result = ProcessVersion {
        running_unavailable: Some(unavailable),
        ..Default::default()
    };
    let context_current = || {
        target.pid > 0
            && identity_is_current()
            && target.launch_context.raw_argv0.as_ref() == Some(&target.executable)
            && process_argv0(target.pid).as_ref() == Some(&target.executable)
    };
    if !target.executable.is_absolute() || !context_current() {
        result.on_disk_candidate_reason = Some(
            "No current, complete absolute argv[0] candidate; running executable remains unverified"
                .into(),
        );
        return result;
    }
    let resolved = program_metadata::launch::resolve_path(&target.executable);
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&resolved.path)
        .ok();
    let Some((file, stamp)) = file.and_then(|file| {
        let metadata = file.metadata().ok()?;
        (metadata.is_file() && is_elf(&file)).then(|| (file, executable_identity(&metadata)))
    }) else {
        result.on_disk_candidate_reason =
            Some("Absolute argv[0] does not identify a readable regular ELF candidate".into());
        return result;
    };
    if resolved.problem.is_some() || !resolved.unchanged() {
        result.on_disk_candidate_reason =
            Some("On-disk candidate launch path changed or is unresolved".into());
        return result;
    }
    result.on_disk_candidate = Some(target.executable.clone());
    result.on_disk_candidate_reason =
        Some("Inferred from absolute argv[0]; not verified against the running executable".into());
    let package_image = details
        .then(|| installation::installed_package_image(&resolved.path, &file))
        .flatten();
    if details {
        let metadata = program_metadata::executable::inspect_on_disk(
            &file,
            &resolved.path,
            program_metadata::executable::ProbePolicy::Passive,
        );
        result.latest_version = metadata.version.map(|version| version.value);
        result.latest_version_reason = metadata.reason;
        result.latest_version_issue = metadata.reason_kind;
        result.latest_build_id = metadata.build_id;
        result.latest_build_id_reason = metadata
            .build_id_issue
            .map(|issue| issue.description().into());
        result.package = package_image
            .as_ref()
            .map(|proof| proof.package.clone())
            .or_else(|| installed_package(&resolved.path));
        let (installation, progs_metadata) =
            installation::inspect_with_owner(&resolved.path, result.package.as_deref());
        result.installation = Some(installation);
        result.progs_metadata = progs_metadata;
        result.latest_development = Some(development::inspect_selected(
            &file,
            &resolved.path,
            result.progs_metadata.source_checkout.as_deref(),
            development_source_managed(
                result.installation.as_deref(),
                Some(&result.progs_metadata),
            ),
            Instant::now() + Duration::from_millis(750),
        ));
        result.package_target = available::target(&resolved.path, result.package.as_deref());
        result.uncompiled_changes = Some(
            if package_image
                .as_ref()
                .is_some_and(|proof| proof.unchanged(&file))
            {
                "Not applicable (package-managed installation)".into()
            } else if defer_source_check {
                freshness::PENDING.into()
            } else {
                freshness::uncompiled_changes(&resolved.path, &file)
            },
        );
        result.launch = Some(ApplicationLaunch {
            executable: Some(resolved.path.clone()),
            chain: resolved.chain.clone(),
            identification:
                "On-disk argv[0] candidate only; the running executable is inaccessible".into(),
            desktop_image: Some(stamp),
            ..Default::default()
        });
        application::collect_desktop_files(&mut result);
    }
    let unchanged = resolved.unchanged()
        && file
            .metadata()
            .is_ok_and(|metadata| executable_identity(&metadata) == stamp)
        && std::fs::metadata(&resolved.path)
            .is_ok_and(|metadata| executable_identity(&metadata) == stamp);
    if !unchanged || !context_current() {
        return ProcessVersion {
            running_unavailable: Some(unavailable),
            on_disk_candidate_reason: Some(
                "On-disk candidate, process identity or argv[0] changed during inspection".into(),
            ),
            ..Default::default()
        };
    }
    if package_image
        .as_ref()
        .is_some_and(|proof| !proof.unchanged(&file))
    {
        result.uncompiled_changes =
            Some("Unknown (package ownership or image changed during inspection)".into());
        result.package = None;
        result.package_target = None;
    }
    if result.uncompiled_changes.as_deref() == Some(freshness::PENDING) {
        result.source_check = Some(std::sync::Arc::new(
            freshness::Check::candidate(resolved, file).with_expected_stamp(stamp),
        ));
    }
    result
}

fn probe_build_id(file: &File) -> Option<String> {
    program_metadata::executable::elf_build_id(file)
}

fn running_executable_version(
    file: &File,
    runtime: Option<&program_metadata::runtime::RuntimeVersion>,
    fallback: impl FnOnce() -> ExecutableVersion,
) -> ExecutableVersion {
    if let Some(version) = runtime {
        let build_id = probe_build_id(file);
        ExecutableVersion {
            version: Some(version.value.clone()),
            source: Some(program_metadata::executable::VersionSource::RunningApplicationApi),
            build_id_reason: build_id
                .is_none()
                .then(|| "No GNU build ID was embedded".into()),
            build_id,
            ..Default::default()
        }
    } else {
        fallback()
    }
}

fn disk_executable_version(
    running: &ExecutableVersion,
    same_observation: bool,
    inspect: impl FnOnce() -> ExecutableVersion,
) -> ExecutableVersion {
    use program_metadata::executable::VersionSource;
    // Reuse exact main-image evidence within this lookup, not cached authority.
    // Process APIs and private-library observations remain independent probes.
    if same_observation
        && running.version.is_some()
        && running.reason_kind.is_none()
        && !running.components_required
        && running.components.is_empty()
        && matches!(
            running.source,
            Some(
                VersionSource::ElfProductMetadata
                    | VersionSource::ElfProductBanner
                    | VersionSource::TrustedSystemProbe
                    | VersionSource::TrustedLocalProbe
                    | VersionSource::VerifiedInstallationProbe
            )
        )
    {
        running.clone()
    } else {
        inspect()
    }
}

fn installed_application_metadata(
    path: &Path,
) -> Result<Option<program_metadata::interpreted::InstalledVersion>, String> {
    program_metadata::interpreted::installed_version_until(
        path,
        Instant::now() + Duration::from_millis(750),
    )
}

#[cfg(test)]
fn probe_version(target: &ProcessVersionTarget) -> ProcessVersion {
    probe_version_checked(target, || true)
}

#[cfg(test)]
fn probe_version_checked(
    target: &ProcessVersionTarget,
    identity_is_current: impl Fn() -> bool,
) -> ProcessVersion {
    probe_process_checked(target, identity_is_current, true)
}

fn probe_version_deferred(
    target: &ProcessVersionTarget,
    identity_is_current: impl Fn() -> bool,
) -> ProcessVersion {
    probe_process_with_source_policy(target, identity_is_current, true, true)
}

fn probe_header_checked(
    target: &ProcessVersionTarget,
    identity_is_current: impl Fn() -> bool,
) -> ProcessVersion {
    probe_process_checked(target, identity_is_current, false)
}

fn probe_process_checked(
    target: &ProcessVersionTarget,
    identity_is_current: impl Fn() -> bool,
    details: bool,
) -> ProcessVersion {
    probe_process_with_source_policy(target, identity_is_current, details, false)
}

fn probe_process_with_source_policy(
    target: &ProcessVersionTarget,
    identity_is_current: impl Fn() -> bool,
    details: bool,
    defer_source_check: bool,
) -> ProcessVersion {
    if target.launch_context.executable_unavailable.is_some() {
        return probe_on_disk_candidate(target, identity_is_current, details, defer_source_check);
    }
    let clean_path = clean_executable(&target.executable);
    let path = PathBuf::from(format!("/proc/{}/exe", target.pid));
    let running: Result<_, &'static str> = (|| {
        if target.pid <= 0 || !identity_is_current() {
            return Err("process exited, changed or is inaccessible since capture");
        }
        let executable = std::fs::read_link(&path).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => "process exited before inspection",
            std::io::ErrorKind::PermissionDenied => "permission denied reading process executable",
            _ => "process executable cannot be inspected",
        })?;
        if clean_path != clean_executable(&executable) {
            return Err("process executable changed since capture");
        }
        let file = File::open(&path).map_err(|_| "cannot open the running executable")?;
        let identity = file
            .metadata()
            .map_err(|_| "cannot identify the running executable")?;
        Ok((file, identity))
    })();
    // Pin the current installation independently of the running image. A
    // self-updater can rename/unlink a regular executable as well as retarget a
    // symlink; the shared resolver validates the process before recovering it.
    let installed = match &running {
        Ok((file, _)) => {
            installation::installed_process_executable(target.pid as u32, &clean_path, file)
        }
        Err(_) => installation::installed_executable(&clean_path),
    };
    // Recovery corroborates the original command identity, including runtimes
    // whose retained filename no longer says node/python/java. This name only
    // selects classification/payload inspection: native probe authorization
    // still uses the independently pinned descriptor's actual path.
    let name = installed
        .as_ref()
        .filter(|installed| installed.recovered_from_process())
        .map_or(clean_path.as_path(), |installed| installed.launcher())
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let latest = installed
        .as_ref()
        .and_then(|installed| {
            File::options()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&installed.path)
                .ok()
        })
        .filter(is_elf);
    let latest_identity = latest.as_ref().and_then(|file| file.metadata().ok());
    // An off-PATH native image can be current without sharing the desktop
    // owner's name. This is fresh, exact manager ownership of this pinned
    // artifact, not an inherited window/parent or a cached package label.
    let package_image = installed
        .as_ref()
        .zip(latest.as_ref())
        .and_then(|(installed, file)| {
            (details || !installed.is_published())
                .then(|| installation::installed_package_image(&installed.path, file))
                .flatten()
        });
    let package_image_is_current = || {
        package_image
            .as_ref()
            .zip(latest.as_ref())
            .is_some_and(|(proof, file)| proof.unchanged(file))
    };
    let same_file = running.as_ref().is_ok_and(|(_, identity)| {
        latest_identity
            .as_ref()
            .is_some_and(|disk| disk.dev() == identity.dev() && disk.ino() == identity.ino())
    });
    let inspect = |file: &File, path: &Path, context| {
        if details {
            probe_executable_context(file, path, context)
        } else if same_file {
            ExecutableVersion {
                components_required:
                    program_metadata::executable::pinned_requires_product_components(file, path),
                ..Default::default()
            }
        } else {
            ExecutableVersion {
                version: None,
                build_id: probe_build_id(file),
                components_required:
                    program_metadata::executable::pinned_requires_product_components(file, path),
                ..Default::default()
            }
        }
    };
    // A direct server self-report is stronger than executing another image.
    // Discovery is by this PID's owned listening descriptor, not TMUX/PATH or a
    // window's associated server: an attached tmux client is a distinct image.
    let runtime_version = if details && name == "tmux" && identity_is_current() {
        running.as_ref().ok().and_then(|(file, _)| {
            let mut stat = String::new();
            File::open(format!("/proc/{}/stat", target.pid))
                .ok()?
                .take(16_385)
                .read_to_string(&mut stat)
                .ok()?;
            if stat.len() > 16_384 {
                return None;
            }
            let ticks = stat
                .rsplit_once(')')?
                .1
                .split_whitespace()
                .nth(19)?
                .parse()
                .ok()?;
            program_metadata::runtime::tmux_server_version(
                target.pid,
                ticks,
                file,
                Instant::now() + Duration::from_millis(500),
            )
            .ok()
            .flatten()
        })
    } else {
        None
    };
    let running_info = running
        .as_ref()
        .map(|(file, _)| {
            running_executable_version(file, runtime_version.as_ref(), || {
                inspect(file, &clean_path, ExecutableContext::Process(target.pid))
            })
        })
        .unwrap_or_default();
    let latest_info = latest.as_ref().map(|file| {
        let path = installed
            .as_ref()
            .map_or(clean_path.as_path(), |installed| installed.path.as_path());
        let same_observation = path == clean_path
            && running.as_ref().is_ok_and(|(_, image)| {
                latest_identity
                    .as_ref()
                    .is_some_and(|disk| executable_identity(image) == executable_identity(disk))
            });
        disk_executable_version(&running_info, same_observation, || {
            inspect(file, path, ExecutableContext::OnDisk)
        })
    });
    // Header checks remain cheap: do not parse Electron payloads or scan native
    // marker bytes until details expand. Adjacent resources make whole-app
    // currency unproven without confusing that with the engine ELF identity.
    let application_assets_unverified = !details
        && installed.as_ref().is_some_and(|installed| {
            installed
                .path
                .parent()
                .is_some_and(|root| root.join("resources").is_dir())
        });
    let installed_metadata = if details {
        installed
            .as_ref()
            .map(|installed| installed_application_metadata(&installed.path))
            .transpose()
    } else {
        Ok(None)
    };
    let installed_metadata_error = installed_metadata.as_ref().err().cloned();
    let installed_application_metadata = installed_metadata.ok().flatten().flatten();
    let captured_launcher = running
        .as_ref()
        .ok()
        .and(latest_identity.as_ref())
        .and_then(|expected| process_launch_from_argv0(target, expected));
    let captured_launch_is_current = || {
        captured_launcher.as_ref().is_some_and(|resolved| {
            resolved.unchanged() && process_argv0(target.pid) == target.launch_context.raw_argv0
        })
    };
    let desktop_launch = || {
        target
            .application_entry
            .as_ref()
            .filter(|_| runtime_name(name).is_none())
            .map(|entry| application::process_launch(&clean_path, installed.as_ref(), Some(entry)))
    };
    // An exact captured absolute launch path also identifies a local build
    // outside PATH; merely finding a retained image at its old path does not.
    let launch_target_verified = |launch: Option<&ApplicationLaunch>| {
        installed
            .as_ref()
            .is_some_and(|installed| installed.is_published())
            || launch.is_some_and(|launch| launch.published_target_verified)
            || captured_launch_is_current()
    };
    let running_latest_binary = latest_info.as_ref().and_then(|latest| {
        running.as_ref().ok()?;
        same_executable_build(&running_info, latest, same_file)
    });
    if !details {
        let desktop_launch = desktop_launch();
        let launch_target_verified = launch_target_verified(desktop_launch.as_ref());
        let mut result = ProcessVersion {
            running_unavailable: running.as_ref().err().copied(),
            runtime_name: runtime_name(name),
            application_installed_metadata: installed_application_metadata,
            application_metadata_error: installed_metadata_error,
            application_assets_unverified,
            running_latest_binary: running_latest_binary
                .filter(|_| launch_target_verified || package_image_is_current()),
            ..Default::default()
        };
        if running.is_ok() && matches!(name, "node" | "nodejs") {
            result.application = target
                .program
                .as_deref()
                .and_then(|program| node::probe(target.pid, program));
            result.application_build = result
                .application
                .as_ref()
                .filter(|application| application.package == pi::PACKAGE)
                .map(|application| pi::probe(application, target.pid));
        }
        if latest_identity.as_ref().is_some_and(|disk| {
            !installed
                .as_ref()
                .is_some_and(|installed| installed.unchanged(disk))
        }) {
            result.running_latest_binary = None;
        }
        if (!launch_target_verified && !package_image_is_current())
            || (captured_launcher.is_some() && !captured_launch_is_current())
        {
            result.running_latest_binary = None;
        }
        if !identity_is_current() {
            result.discard_running_metadata("process exited or changed during inspection");
        }
        return result;
    }
    let (uncompiled_changes, disk) = collection::with_source_check(
        defer_source_check
            && latest.is_some()
            && installed.as_ref().is_some_and(|installed| {
                !package_image_is_current() && installation::uv_runtime(&installed.path).is_none()
            }),
        || match (&installed, &latest) {
            (Some(_), Some(_)) if package_image_is_current() => {
                "Not applicable (package-managed installation)".to_owned()
            }
            (Some(installed), Some(_)) if installation::uv_runtime(&installed.path).is_some() => {
                "Not applicable (prebuilt uv-managed runtime)".to_owned()
            }
            (Some(installed), Some(file)) => freshness::uncompiled_changes(&installed.path, file),
            (None, _) => {
                "Unknown (installed executable aliases identify different builds)".to_owned()
            }
            _ => "Unknown (on-disk binary unavailable)".to_owned(),
        },
        || {
            let disk_path = installed
                .as_ref()
                .map(|installed| installed.path.as_path())
                .unwrap_or(&clean_path);
            let package = package_image
                .as_ref()
                .filter(|_| package_image_is_current())
                .map(|proof| proof.package.clone())
                .or_else(|| installed_package(disk_path));
            let installation::InstallationDetails {
                label: installation,
                metadata: progs_metadata,
                desktops,
            } = installation::inspect_with_owner_details(disk_path, package.as_deref());
            let package_target = program_metadata::package::availability_target_with_desktops(
                disk_path,
                package.as_deref(),
                &desktops,
            );
            let npm_target = npm::target(
                disk_path,
                latest_info
                    .as_ref()
                    .and_then(|info| info.version.as_deref()),
            );
            let launch = desktop_launch().unwrap_or_else(|| {
                application::process_launch(&clean_path, installed.as_ref(), None)
            });
            let launch_target_verified = launch_target_verified(Some(&launch));
            let runtime_launcher = captured_launcher
                .as_ref()
                .filter(|_| runtime_name(name).is_some());
            let mut launch = if let Some(resolved) = runtime_launcher {
                ApplicationLaunch {
                    executable: Some(resolved.path.clone()),
                    chain: resolved.chain.clone(),
                    identification: "Current runtime launcher from captured argv[0]; the original process launch route is not proven".into(),
                    desktop_image: latest_identity.as_ref().map(executable_identity),
                    ..Default::default()
                }
            } else {
                launch
            };
            if runtime_name(name).is_none() {
                if let Some(file) = latest.as_ref() {
                    application::associate_installed_desktop_from_matches(
                        &mut launch,
                        file,
                        &desktops,
                    );
                }
                if let Ok((file, _)) = running.as_ref() {
                    application::associate_running_desktop(
                        &mut launch,
                        file,
                        target.application_entry.as_ref(),
                    );
                }
            }
            collection::DiskDetails {
                package,
                installation,
                progs_metadata,
                package_target,
                npm_target,
                launch,
                launch_target_verified,
            }
        },
    );
    let collection::DiskDetails {
        package,
        installation,
        progs_metadata,
        package_target,
        npm_target,
        launch,
        launch_target_verified,
    } = disk;
    let runtime_launcher = captured_launcher
        .as_ref()
        .filter(|_| runtime_name(name).is_some());
    let latest_version_issue =
        if installed_metadata_error.is_some() || installed_application_metadata.is_some() {
            None
        } else {
            latest_info.as_ref().and_then(|info| info.reason_kind)
        };
    let mut result = ProcessVersion {
        details_pending: false,
        source_check: None,
        development: None,
        latest_development: None,
        application_development: None,
        running_unavailable: running.as_ref().err().copied(),
        runtime_name: runtime_name(name),
        application: None,
        script_application: None,
        script_details: None,
        script_details_error: None,
        python_context: None,
        python_context_error: None,
        application_installed_metadata: installed_application_metadata.clone(),
        application_metadata_error: installed_metadata_error.clone(),
        application_assets_unverified: false,
        application_build: None,
        application_installation: None,
        application_progs_metadata: ProgsMetadata::default(),
        application_package_target: None,
        application_latest_package_version: None,
        application_npm_target: None,
        application_latest_npm_version: None,
        version: running_info.version,
        version_source: running_info.source,
        runtime_version,
        version_reason: running_info.reason,
        version_issue: running_info.reason_kind,
        latest_version: if installed_metadata_error.is_some() {
            None
        } else {
            installed_application_metadata
                .as_ref()
                .map(|metadata| metadata.version.clone())
                .or_else(|| latest_info.as_ref().and_then(|info| info.version.clone()))
        },
        latest_version_reason: installed_metadata_error.or_else(|| {
            if installed_application_metadata.is_some() {
                None
            } else {
                latest_info.as_ref().and_then(|info| info.reason.clone())
            }
        }),
        latest_version_issue,
        package_target,
        latest_package_version: None,
        npm_target,
        latest_npm_version: None,
        latest_build_id: latest_info.as_ref().and_then(|info| info.build_id.clone()),
        latest_build_id_reason: latest_info
            .as_ref()
            .and_then(|info| info.build_id_reason.clone()),
        running_latest_binary: running_latest_binary
            .filter(|_| launch_target_verified || package_image_is_current()),
        uncompiled_changes: Some(uncompiled_changes),
        package,
        installation: Some(installation),
        progs_metadata,
        build_id: running_info.build_id,
        build_id_reason: running_info.build_id_reason,
        on_disk_candidate: None,
        on_disk_candidate_reason: None,
        launch: Some(launch),
        application_launch: None,
    };
    if running.is_ok()
        && let Some(program) = target.program.as_deref()
        && matches!(name, "node" | "nodejs")
    {
        result.application = node::probe(target.pid, program);
        result.application_build = result
            .application
            .as_ref()
            .filter(|application| application.package == pi::PACKAGE)
            .map(|application| pi::probe(application, target.pid));
        if let Some(application) = &result.application {
            let (installation, metadata) = installation::inspect(&application.entry_point);
            result.application_installation = Some(installation);
            result.application_progs_metadata = metadata;
            result.application_launch = Some(self::application::process_launch(
                &application.entry_point,
                installation::installed_executable(&application.entry_point).as_ref(),
                target.application_entry.as_ref(),
            ));
        }
        result.application_package_target = result.application.as_ref().and_then(|application| {
            let owner = installed_package(&application.entry_point);
            available::target(&application.entry_point, owner.as_deref())
        });
        result.application_npm_target = result
            .application
            .as_ref()
            .and_then(|application| NpmTarget::package(&application.package));
    }
    if running.is_ok()
        && result.application.is_none()
        && let Some(runtime @ ("Python" | "Java" | "Node.js" | "Shell")) =
            script::runtime_name(name)
    {
        result.script_application = script::probe(runtime, target.pid);
        if result.script_application.is_some() {
            result.runtime_name = Some(runtime);
        }
        if let Some(application) = result.script_application.as_ref()
            && let Some(source) = application.descriptor.as_ref()
        {
            let candidate = script_comparison_candidate(application);
            match source.inspect_details(
                candidate.as_deref(),
                Instant::now() + Duration::from_millis(750),
            ) {
                Ok(details) => result.script_details = Some(details),
                Err(reason) => result.script_details_error = Some(reason),
            }
        }
        if let Some(entry) = result
            .script_application
            .as_ref()
            .filter(|application| application.unchanged())
            .and_then(|app| app.entry_point.as_ref())
        {
            result.application_installed_metadata =
                program_metadata::interpreted::installed_version(entry);
            let (installation, metadata) = installation::inspect(entry);
            result.application_installation = Some(installation);
            result.application_progs_metadata = metadata;
            let package = installed_package(entry);
            result.application_package_target = available::target(entry, package.as_deref());
            result.application_launch = Some(application::process_launch(
                entry,
                installation::installed_executable(entry).as_ref(),
                target.application_entry.as_ref(),
            ));
        }
        result.discard_changed_script_metadata();
        if runtime == "Python"
            && let Some(script) = &result.script_application
        {
            match program_metadata::interpreted::python_process::inspect(
                target.pid,
                script,
                result.application_installed_metadata.as_ref(),
                result.application_progs_metadata.source_checkout.as_deref(),
                Instant::now() + Duration::from_millis(750),
            ) {
                Ok(context) => result.python_context = Some(context),
                Err(reason) => result.python_context_error = Some(reason),
            }
        }
    }
    // Detailed worker only: share one bounded budget across the independently
    // pinned running image, current installation and hosted entry point.
    development::inspect_process(
        &mut result,
        running.as_ref().ok().map(|(file, _)| file),
        latest.as_ref(),
        &clean_path,
        installed.as_ref().map(|installed| installed.path.as_path()),
        Instant::now() + Duration::from_millis(750),
    );
    if let Some(disk) = latest_identity.as_ref()
        && !installed
            .as_ref()
            .is_some_and(|installed| installed.unchanged(disk))
    {
        result.latest_version = None;
        development::retain_checkout(&mut result.latest_development);
        result.application_installed_metadata = None;
        result.latest_version_reason = Some("on-disk binary changed during inspection".into());
        result.latest_version_issue = Some(program_metadata::executable::MetadataIssue::Changed);
        result.latest_build_id = None;
        result.latest_build_id_reason = Some("On-disk binary changed during inspection".into());
        result.package = None;
        result.package_target = None;
        result.npm_target = None;
        result.installation = Some("Unknown (on-disk binary changed during inspection)".into());
        result.progs_metadata = ProgsMetadata::default();
        result.launch = Some(ApplicationLaunch {
            identification: "Unknown (on-disk binary changed during inspection)".into(),
            ..Default::default()
        });
        result.running_latest_binary = None;
        result.uncompiled_changes =
            Some("Unknown (on-disk binary changed during inspection)".into());
    }
    if target.window_declared_entry
        && let Some(entry) = &target.application_entry
    {
        let launch = if result.runtime_name.is_some() {
            result
                .application_launch
                .get_or_insert_with(Default::default)
        } else {
            result.launch.get_or_insert_with(Default::default)
        };
        launch.declare_window_entry(entry);
    }
    application::collect_desktop_files(&mut result);
    if package_image.is_some() && !package_image_is_current() {
        if !launch_target_verified {
            result.running_latest_binary = None;
        }
        result.uncompiled_changes =
            Some("Unknown (package ownership or image changed during inspection)".into());
        result.package = None;
        result.package_target = None;
    }
    if let Ok((_, identity)) = &running
        && (!identity_is_current()
            || !std::fs::metadata(&path).is_ok_and(|current| {
                current.dev() == identity.dev() && current.ino() == identity.ino()
            }))
    {
        result.discard_running_metadata("process exited or changed during inspection");
    }
    if captured_launcher.is_some() && !captured_launch_is_current() {
        result.running_latest_binary = None;
        if runtime_launcher.is_some() {
            result.launch = Some(ApplicationLaunch {
                identification: "Unknown (runtime argv[0] launch path changed during inspection)"
                    .into(),
                ..Default::default()
            });
        }
    }
    result.discard_changed_script_metadata();
    // Transfer the independently pinned installation only after all validation
    // above. A slow input check must not hold up this result or the next process.
    if result.uncompiled_changes.as_deref() == Some(freshness::PENDING)
        && let (Some(installed), Some(file), Some(disk)) = (installed, latest, latest_identity)
    {
        result.source_check = Some(std::sync::Arc::new(
            freshness::Check::installed(installed, file, captured_launcher)
                .with_expected_stamp(executable_identity(&disk)),
        ));
    }
    result
}

/// This is a current content-comparison hint, never the historical launch route
/// or permission to attribute the candidate's package/version to the process.
fn script_comparison_candidate(application: &ScriptApplication) -> Option<PathBuf> {
    let by_name =
        program_metadata::interpreted::script::installed_script_candidate(&application.name);
    let by_filename = || {
        let name = application.entry_point.as_ref()?.file_name()?.to_str()?;
        (name != application.name)
            .then(|| program_metadata::interpreted::script::installed_script_candidate(name))
            .flatten()
    };
    by_name
        .or_else(by_filename)
        .or_else(|| application.entry_point.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_runtime_version_precedes_executable_queries_without_replacing_build_identity() {
        let file = File::open("/proc/self/exe").unwrap();
        let report = program_metadata::runtime::RuntimeVersion {
            value: "tmux 3.7c-fork0.2.2-2".into(),
            token: "3.7c-fork0.2.2-2".into(),
            pid: 4123,
            start_ticks: 5678,
            socket: "/private/tmux.sock".into(),
        };
        let result = running_executable_version(&file, Some(&report), || {
            panic!("a successful direct API must not start a redundant executable version query")
        });
        assert_eq!(result.version.as_deref(), Some("tmux 3.7c-fork0.2.2-2"));
        assert_eq!(
            result.source,
            Some(program_metadata::executable::VersionSource::RunningApplicationApi)
        );
        assert_eq!(result.build_id, probe_build_id(&file));
        let mut process = ProcessVersion {
            version: result.version,
            version_source: result.source,
            runtime_version: Some(report),
            build_id: result.build_id,
            latest_version: Some("tmux 3.8-new-disk-build".into()),
            python_context: Some(Default::default()),
            python_context_error: Some("previous inspection failed".into()),
            ..Default::default()
        };
        process.discard_running_metadata("process identity changed");
        assert!(
            process.version.is_none()
                && process.version_source.is_none()
                && process.runtime_version.is_none()
                && process.python_context.is_none()
                && process.python_context_error.is_none()
        );
        assert_eq!(
            process.latest_version.as_deref(),
            Some("tmux 3.8-new-disk-build")
        );
    }

    #[test]
    fn missing_runtime_endpoint_preserves_exact_client_executable_evidence() {
        let file = File::open("/proc/self/exe").unwrap();
        let expected = "tmux 3.7c-distinct-client-build";
        let result = running_executable_version(&file, None, || ExecutableVersion {
            version: Some(expected.into()),
            source: Some(program_metadata::executable::VersionSource::ElfProductBanner),
            ..Default::default()
        });
        assert_eq!(result.version.as_deref(), Some(expected));
        assert_eq!(
            result.source,
            Some(program_metadata::executable::VersionSource::ElfProductBanner)
        );
    }

    #[test]
    #[ignore = "read-only direct tmux server acceptance; requires PROGRAM_METADATA_TMUX_SERVER_PID, _START_TICKS, _VERSION; optional APPLICATIONLAUNCHER_TEST_TMUX_CLIENT_PID proves clients never borrow the server's version"]
    fn live_tmux_server_uses_direct_runtime_api_and_keeps_clients_separate() {
        let pid = std::env::var("PROGRAM_METADATA_TMUX_SERVER_PID")
            .unwrap()
            .parse()
            .unwrap();
        let start_ticks = std::env::var("PROGRAM_METADATA_TMUX_SERVER_START_TICKS")
            .unwrap()
            .parse()
            .unwrap();
        let expected = std::env::var("PROGRAM_METADATA_TMUX_SERVER_VERSION").unwrap();
        let image = std::fs::metadata(format!("/proc/{pid}/exe")).unwrap();
        let current = || {
            crate::windows::process_tree::process_identity_is_current(
                crate::models::ProcessIdentity { pid, start_ticks },
                Some((image.dev(), image.ino())),
            )
        };
        let result = probe_version_checked(
            &ProcessVersionTarget {
                pid,
                executable: std::fs::read_link(format!("/proc/{pid}/exe")).unwrap(),
                launch_context: ProcessLaunchContext::default(),
                program: Some("tmux: server".into()),
                application_entry: None,
                window_declared_entry: false,
            },
            current,
        );
        assert_eq!(
            result.version.as_deref(),
            Some(expected.as_str()),
            "{result:?}"
        );
        assert_eq!(
            result.version_source,
            Some(program_metadata::executable::VersionSource::RunningApplicationApi),
            "{result:?}"
        );
        let direct = result.runtime_version.as_ref().unwrap();
        assert_eq!((direct.pid, direct.start_ticks), (pid, start_ticks));
        assert_eq!(
            result.build_id,
            probe_build_id(&File::open(format!("/proc/{pid}/exe")).unwrap())
        );
        assert!(current());
        applicationlauncher::diagnostic_log!(
            "Launcher direct runtime: pid={pid}, source={:?}, running={:?}, installed={:?}, server endpoint={}",
            result.version_source,
            result.version,
            result.latest_version,
            direct.socket.display()
        );
        if let Some(client) = std::env::var("APPLICATIONLAUNCHER_TEST_TMUX_CLIENT_PID")
            .ok()
            .map(|value| value.parse::<i32>().unwrap())
        {
            assert_ne!(client, pid);
            let client = probe_version(&ProcessVersionTarget {
                pid: client,
                executable: std::fs::read_link(format!("/proc/{client}/exe")).unwrap(),
                launch_context: ProcessLaunchContext::default(),
                program: Some("tmux: client".into()),
                application_entry: None,
                window_declared_entry: false,
            });
            assert!(client.runtime_version.is_none(), "{client:?}");
            assert_ne!(
                client.version_source,
                Some(program_metadata::executable::VersionSource::RunningApplicationApi)
            );
            applicationlauncher::diagnostic_log!(
                "Distinct client keeps its own executable evidence: {:?}, {:?}",
                client.version,
                client.version_source
            );
        }
    }

    fn wait_for_fixture_argv0(child: &mut std::process::Child, expected: &Path) {
        // posix_spawn/vfork may release the parent before /proc/PID/cmdline
        // exposes the newly installed argument range. The fixture must establish
        // the same complete-argv0 precondition as a captured production node.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if process_argv0(child.id() as i32).as_deref() == Some(expected) {
                return;
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "private fixture exited before argv[0] was ready"
            );
            assert!(
                Instant::now() < deadline,
                "private fixture argv[0] did not become ready: {:?}",
                std::fs::read(format!("/proc/{}/cmdline", child.id()))
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn native_process_metadata_survives_an_unresolved_same_name_wrapper() {
        use std::io::Write;
        use std::process::Stdio;
        const CHILD: &str = "APPLICATIONLAUNCHER_WRAPPER_IDENTITY_FIXTURE";
        if let Some(directory) = std::env::var_os(CHILD).map(PathBuf::from) {
            let executable = directory.join("checkout/target/release/tool");
            let mut child = Command::new(&executable)
                .arg("60")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            wait_for_fixture_argv0(&mut child, &executable);
            struct OwnedChild(std::process::Child);
            impl Drop for OwnedChild {
                fn drop(&mut self) {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                }
            }
            let child = OwnedChild(child);
            let mut target = ProcessVersionTarget {
                pid: child.0.id() as i32,
                executable: executable.clone(),
                launch_context: ProcessLaunchContext::default(),
                program: Some("tool".into()),
                application_entry: None,
                window_declared_entry: false,
            };
            let result = probe_version(&target);
            assert!(result.build_id.is_some(), "{result:?}");
            assert_eq!(result.latest_build_id, result.build_id, "{result:?}");
            assert_eq!(result.version.as_deref(), Some("tool 2.3.4"), "{result:?}");
            assert_eq!(result.latest_version, result.version, "{result:?}");
            assert_eq!(
                result.progs_metadata.capabilities.as_deref(),
                Some("GUI  •  Primary role: GUI application"),
                "{result:?}"
            );
            assert!(
                result
                    .uncompiled_changes
                    .as_deref()
                    .is_some_and(|status| { !status.contains("on-disk binary unavailable") }),
                "{result:?}"
            );
            assert_eq!(
                result.process_build_is_current(),
                None,
                "an unresolved wrapper alone does not prove the current installation"
            );
            let launch = result.launch.unwrap();
            assert_eq!(launch.executable.as_ref(), Some(&executable));
            assert_eq!(launch.chain, vec![executable.clone()]);
            assert!(!launch.published_target_verified);
            target.launch_context.raw_argv0 = Some(executable.clone());
            for expected_current in [true, false] {
                if !expected_current {
                    std::fs::rename(directory.join("tool-next"), &executable).unwrap();
                }
                let details = probe_version_checked(&target, || true);
                assert_eq!(details.version.as_deref(), Some("tool 2.3.4"));
                assert_eq!(details.latest_version, details.version);
                assert_eq!(
                    details.build_id == details.latest_build_id,
                    expected_current,
                    "{details:?}"
                );
                assert_eq!(
                    details.running_latest_binary,
                    Some(expected_current),
                    "an independently verified absolute launch path identifies the disk image: {details:?}"
                );
                assert_eq!(details.process_build_is_current(), Some(expected_current));
                let header = probe_header_checked(&target, || true);
                assert_eq!(header.process_build_is_current(), Some(expected_current));
                assert!(header.version.is_none() && header.latest_version.is_none());

                let mut unverified = target.clone();
                for argv0 in [
                    PathBuf::from("tool"),
                    directory.join("checkout/scripts/tool"),
                ] {
                    unverified.launch_context.raw_argv0 = Some(argv0);
                    assert_eq!(probe_version(&unverified).running_latest_binary, None);
                    assert_eq!(
                        probe_header_checked(&unverified, || true).process_build_is_current(),
                        None
                    );
                }
                assert_eq!(
                    probe_version_checked(&target, || false).process_build_is_current(),
                    None
                );
            }
            assert!(!directory.join("SHOULD_NOT_RUN").exists());
            return;
        }
        let directory = std::env::temp_dir().join(format!(
            "launcher-wrapper-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir(&directory).unwrap();
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let fixture = Fixture(directory);
        let checkout = fixture.0.join("checkout");
        let executable = checkout.join("target/release/tool");
        let wrapper = checkout.join("scripts/tool");
        let alias = fixture.0.join("bin/tool");
        let cache = fixture.0.join("cache/progs/scan-v1.json");
        for path in [&executable, &wrapper, &alias, &cache] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        }
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        // An owned native fixture publishes metadata and only waits. Neither
        // the user's application nor its deployment wrapper is executed.
        let source = r#"#include <unistd.h>
#define RECORD "PROGRAM-METADATA-V1\ntool\n2.3.4\ngui\n"
__attribute__((section(".program_metadata"), used))
const char metadata[sizeof(RECORD)-1] = RECORD;
__attribute__((used)) const int image_revision = IMAGE_REVISION;
int main(void) { for (;;) pause(); }
"#;
        for (path, revision) in [(&executable, 1), (&fixture.0.join("tool-next"), 2)] {
            let mut compiler = Command::new("cc")
                .args(["-x", "c", "-O2", "-Wl,--build-id", "-o"])
                .arg(path)
                .arg(format!("-DIMAGE_REVISION={revision}"))
                .arg("-")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("the wrapper regression requires the configured C compiler");
            compiler
                .stdin
                .take()
                .unwrap()
                .write_all(source.as_bytes())
                .unwrap();
            let compiled = compiler.wait_with_output().unwrap();
            assert!(
                compiled.status.success(),
                "{}",
                String::from_utf8_lossy(&compiled.stderr)
            );
        }
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\ntouch '{}'\nexec \"$binary\" \"$@\"\n",
                fixture.0.join("SHOULD_NOT_RUN").display(),
            ),
        )
        .unwrap();
        std::os::unix::fs::symlink(&wrapper, &alias).unwrap();
        std::fs::write(
            &cache,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 4, "scan": {"apps": [{
                    "name": "tool", "origin": "Local", "install_role": "Standalone",
                    "state": {"dev": true},
                    "desc": "Local custom tool sitting at an indexed wrapper",
                    "capabilities": {"has_cli": true}, "binaries": [
                        {"name":"tool", "path":alias, "target":wrapper},
                        {"name":"tool", "path":executable}
                    ]
                }]}
            }))
            .unwrap(),
        )
        .unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "windows::versions::tests::native_process_metadata_survives_an_unresolved_same_name_wrapper", "--nocapture"])
            .env(CHILD, &fixture.0)
            .env("HOME", &fixture.0)
            .env("XDG_CACHE_HOME", fixture.0.join("cache"))
            .env("XDG_CONFIG_HOME", fixture.0.join("config"))
            .env("PATH", "/usr/bin:/bin")
            .output().unwrap();
        assert!(
            output.status.success(),
            "stdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }

    #[test]
    fn trusted_local_probe_consumers_recheck_grants_without_executing_inferred_candidates() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::process::CommandExt;
        use std::process::Stdio;

        const CHILD: &str = "APPLICATIONLAUNCHER_LOCAL_PROBE_FIXTURE";
        if let Some(directory) = std::env::var_os(CHILD).map(PathBuf::from) {
            let executable = directory.join("fish");
            let counter = directory.join("probe-count");
            let file = File::open(&executable).unwrap();
            let desktop = directory.join("fish.desktop");
            let config = directory.join("config/program-metadata/version-probes.toml");
            let query = || probe_executable_context(&file, &executable, ExecutableContext::OnDisk);
            assert!(
                query().version.is_none(),
                "an ungranted local image was executed"
            );
            assert!(!counter.exists());
            std::fs::create_dir_all(config.parent().unwrap()).unwrap();
            std::fs::write(
                &config,
                format!(
                    "schema_version = 1\n[[trusted_programs]]\nname = \"fish\"\npaths = [{}]\n",
                    serde_json::to_string(&executable).unwrap()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(query().version.as_deref(), Some("fish, version 9.8.7"));
            let app = inspect_application(executable.to_str().unwrap(), &desktop);
            assert_eq!(
                app.version.as_deref(),
                Some("fish, version 9.8.7"),
                "{app:?}"
            );
            let count = std::fs::read(&counter).unwrap();
            assert_eq!(
                count.len(),
                2,
                "each exact-image consumer must query independently"
            );

            // An inferred argv[0] can name a granted executable without proving
            // that it is the process image. This path must remain passive.
            let mut child = Command::new("/usr/bin/sleep")
                .arg0(&executable)
                .arg("30")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            struct OwnedChild(std::process::Child);
            impl Drop for OwnedChild {
                fn drop(&mut self) {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                }
            }
            wait_for_fixture_argv0(&mut child, &executable);
            let child = OwnedChild(child);
            let inferred = probe_version(&ProcessVersionTarget {
                pid: child.0.id() as i32,
                executable: executable.clone(),
                launch_context: ProcessLaunchContext {
                    raw_argv0: Some(executable.clone()),
                    executable_unavailable: Some("permission denied reading process executable"),
                },
                program: Some("fish".into()),
                application_entry: None,
                window_declared_entry: false,
            });
            assert!(
                inferred.version.is_none() && inferred.latest_version.is_none(),
                "{inferred:?}"
            );
            assert_eq!(inferred.on_disk_candidate.as_ref(), Some(&executable));
            assert_eq!(std::fs::read(&counter).unwrap(), count);

            std::fs::remove_file(&config).unwrap();
            assert!(query().version.is_none(), "a revoked grant was cached");
            assert!(
                inspect_application(executable.to_str().unwrap(), &desktop)
                    .version
                    .is_none()
            );
            assert_eq!(std::fs::read(&counter).unwrap(), count);
            return;
        }

        let directory = std::env::temp_dir().join(format!(
            "launcher-trusted-probe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let fixture = Fixture(directory);
        let counter = fixture.0.join("probe-count");
        let source = format!(
            r#"#include <stdio.h>
#include <string.h>
int main(int argc, char **argv) {{
    if (argc != 3 || strcmp(argv[1], "--no-config") || strcmp(argv[2], "--version")) return 31;
    FILE *counter = fopen({}, "a");
    if (!counter) return 32;
    fputc('x', counter); fclose(counter);
    printf("fi%s, version %u.%u.%u\n", "sh", 9u, 8u, 7u);
    return 0;
}}
"#,
            serde_json::to_string(&counter).unwrap()
        );
        let mut compiler = Command::new("cc")
            .args(["-x", "c", "-O0", "-Wl,--build-id", "-o"])
            .arg(fixture.0.join("fish"))
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the native probe regression requires the configured C compiler");
        compiler
            .stdin
            .take()
            .unwrap()
            .write_all(source.as_bytes())
            .unwrap();
        let compiled = compiler.wait_with_output().unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let result = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "windows::versions::tests::trusted_local_probe_consumers_recheck_grants_without_executing_inferred_candidates", "--nocapture"])
            .env(CHILD, &fixture.0)
            .env("XDG_CONFIG_HOME", fixture.0.join("config"))
            .env("XDG_CACHE_HOME", fixture.0.join("cache"))
            .env("HOME", &fixture.0)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "child stdout={}\nchild stderr={}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
    }

    #[test]
    fn process_name_status_uses_running_build_identity_and_not_version_strings() {
        for status in [Some(true), Some(false), None] {
            let mut version = ProcessVersion {
                version: Some("tool 1.0".into()),
                latest_version: Some("tool 1.0".into()),
                running_latest_binary: status,
                ..Default::default()
            };
            assert_eq!(version.process_build_is_current(), status);
            assert_eq!(version.process_header_is_current(), status);
            version.running_unavailable = Some("process changed");
            assert_eq!(version.process_build_is_current(), None);
            assert_eq!(version.process_header_is_current(), None);
        }
    }

    #[test]
    fn hosted_process_name_status_never_substitutes_the_interpreters_build() {
        let mut version = ProcessVersion {
            runtime_name: Some("Node.js"),
            running_latest_binary: Some(true),
            ..Default::default()
        };
        assert_eq!(version.process_build_is_current(), None);
        assert_eq!(version.process_header_is_current(), None);
        for (status, current) in [
            (PiBuildStatus::Current, Some(true)),
            (PiBuildStatus::RestartRequired, Some(false)),
            (PiBuildStatus::InstallationUpdateRequired, Some(false)),
            (PiBuildStatus::Unknown, None),
        ] {
            version.application_build = Some(PiBuildEvidence {
                status,
                ..Default::default()
            });
            assert_eq!(version.process_build_is_current(), current);
            assert_eq!(version.process_header_is_current(), current);
        }
    }

    #[test]
    fn non_shell_hosted_headers_still_require_application_evidence_not_runtime_identity() {
        for runtime in ["Node.js", "Python", "Java", "Electron"] {
            for current in [Some(true), Some(false), None] {
                let version = ProcessVersion {
                    runtime_name: Some(runtime),
                    running_latest_binary: current,
                    ..Default::default()
                };
                assert_eq!(version.process_header_is_current(), None);
            }
        }
    }

    #[test]
    fn identical_version_strings_do_not_mean_identical_executable_builds() {
        let running = ExecutableVersion {
            version: Some("xfce4-terminal 1.2.0-dev-b3149143".into()),
            build_id: Some("7f024090".into()),
            ..Default::default()
        };
        let mut latest = running.clone();
        latest.build_id = Some("48d08f2b".into());
        assert_eq!(same_executable_build(&running, &latest, false), Some(false));
        latest.build_id = running.build_id.clone();
        assert_eq!(same_executable_build(&running, &latest, false), Some(true));
        latest.build_id = None;
        assert_eq!(same_executable_build(&running, &latest, false), None);
        assert_eq!(same_executable_build(&running, &latest, true), Some(true));
    }

    #[test]
    fn private_version_components_are_independent_of_the_main_executable_build() {
        use program_metadata::executable::ExecutableComponent;
        let path = std::env::current_exe().unwrap();
        let identity = executable_identity(&std::fs::metadata(&path).unwrap());
        let other_identity = executable_identity(&std::fs::metadata("/usr/bin/env").unwrap());
        assert_ne!(identity, other_identity);
        let mut running = ExecutableVersion {
            version: Some("Mousepad 0.7.0-dev-same-token".into()),
            build_id: Some("same-main-build".into()),
            components_required: true,
            components: vec![ExecutableComponent {
                path: "/build/mousepad/libmousepad.so.0.0.0".into(),
                identity,
                build_id: Some("loaded-library-build".into()),
            }],
            ..Default::default()
        };
        let mut latest = running.clone();
        assert_eq!(same_executable_build(&running, &latest, true), Some(true));
        latest.components[0].identity = other_identity;
        latest.components[0].build_id = Some("replacement-library-build".into());
        assert_eq!(same_executable_build(&running, &latest, true), Some(false));
        latest.components[0].build_id = None;
        assert_eq!(same_executable_build(&running, &latest, true), None);
        latest.components[0].identity = identity;
        assert_eq!(same_executable_build(&running, &latest, true), Some(true));
        running.components.clear();
        assert_eq!(same_executable_build(&running, &latest, true), None);
        latest.components.clear();
        assert_eq!(same_executable_build(&running, &latest, true), None);
        running.components_required = false;
        latest.components_required = false;
        assert_eq!(same_executable_build(&running, &latest, true), Some(true));
    }

    #[test]
    #[ignore = "passive installed fork checks; requires APPLICATIONLAUNCHER_TEST_MOUSEPAD and APPLICATIONLAUNCHER_TEST_COPYQ"]
    fn installed_mousepad_and_copyq_versions_use_shared_passive_product_evidence() {
        for (variable, expected_version, expected_build_id, component_count) in [
            (
                "APPLICATIONLAUNCHER_TEST_MOUSEPAD",
                "Mousepad 0.7.0-dev-92e61131",
                "d1e2863bbd5acdbd5fe2af8f375c4b18b91cd008",
                1,
            ),
            (
                "APPLICATIONLAUNCHER_TEST_COPYQ",
                "CopyQ 15.0.0.21-gfb3f9622",
                "7a61e14139b9167dec2b45f7d1392cfa8cd574fc",
                0,
            ),
        ] {
            let path = PathBuf::from(std::env::var_os(variable).expect(variable));
            let file = File::open(&path).unwrap();
            for _ in 0..2 {
                let installed = probe_executable_context(&file, &path, ExecutableContext::OnDisk);
                assert_eq!(
                    installed.version.as_deref(),
                    Some(expected_version),
                    "{installed:?}"
                );
                assert_eq!(installed.build_id.as_deref(), Some(expected_build_id));
                assert_eq!(installed.components.len(), component_count);
                let tile = application::inspect_application(
                    path.to_str().unwrap(),
                    &path.with_extension("desktop"),
                );
                assert_eq!(tile.version.as_deref(), Some(expected_version), "{tile:?}");
                assert_eq!(tile.build_id.as_deref(), Some(expected_build_id));
            }
            if component_count != 0 {
                // Installed component evidence must never be reused as a
                // running-version claim. No process with this PID exists.
                let running =
                    probe_executable_context(&file, &path, ExecutableContext::Process(-1));
                assert!(running.version.is_none(), "{running:?}");
                assert!(running.components.is_empty(), "{running:?}");
                assert!(running.components_required);
                let installed = probe_executable_context(&file, &path, ExecutableContext::OnDisk);
                assert_eq!(installed.version.as_deref(), Some(expected_version));
            }
        }
    }

    #[test]
    fn electron_process_details_prefer_installed_product_metadata_without_claiming_loaded_assets() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "launcher-electron-process-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("resources/app")).unwrap();
        let path = root.join("electron-app");
        let mut bytes = vec![0u8; 64];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
        bytes.extend_from_slice(b"ELECTRON_RUN_AS_NODE");
        std::fs::write(&path, bytes).unwrap();
        std::fs::write(
            root.join("resources/app/package.json"),
            r#"{"name":"example-editor","version":"2.4.1"}"#,
        )
        .unwrap();
        let result = probe_version(&ProcessVersionTarget {
            pid: 0,
            executable: path.clone(),
            launch_context: ProcessLaunchContext::default(),
            program: None,
            application_entry: None,
            window_declared_entry: false,
        });
        assert_eq!(result.latest_version.as_deref(), Some("2.4.1"));
        assert_eq!(
            result.application_installed_metadata.as_ref().unwrap().kind,
            "Electron package"
        );
        assert!(
            result.version.is_none(),
            "an exited process has no observed running engine version"
        );
        let engine_same_but_assets_unknown = ProcessVersion {
            running_latest_binary: Some(true),
            application_installed_metadata: result.application_installed_metadata,
            ..Default::default()
        };
        assert_eq!(
            engine_same_but_assets_unknown.process_build_is_current(),
            None
        );
        std::fs::write(
            root.join("resources/app/package.json"),
            "malformed payload metadata",
        )
        .unwrap();
        let malformed = probe_version(&ProcessVersionTarget {
            pid: 0,
            executable: path.clone(),
            launch_context: ProcessLaunchContext::default(),
            program: None,
            application_entry: None,
            window_declared_entry: false,
        });
        assert!(malformed.latest_version.is_none());
        assert!(
            malformed
                .application_metadata_error
                .as_deref()
                .is_some_and(|reason| reason.contains("Electron"))
        );
        let header = probe_header_checked(
            &ProcessVersionTarget {
                pid: 0,
                executable: path.clone(),
                launch_context: ProcessLaunchContext::default(),
                program: None,
                application_entry: None,
                window_declared_entry: false,
            },
            || true,
        );
        assert!(header.application_assets_unverified);
        assert!(
            header.application_installed_metadata.is_none()
                && header.application_metadata_error.is_none(),
            "collapsed headers must not parse or scan application payloads"
        );
        let tile =
            application::inspect_application(path.to_str().unwrap(), &root.join("example.desktop"));
        assert!(
            tile.version.is_none()
                && tile
                    .version_reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("Electron"))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unknown_executables_are_never_probed_as_programs() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "launcher-shared-version-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("xfce4-terminal");
        let sentinel = root.join("EXECUTED");
        std::fs::write(
            &path,
            format!("#!/bin/sh\ntouch '{}'\n", sentinel.display()),
        )
        .unwrap();
        let result = probe_executable(&File::open(&path).unwrap(), "xfce4-terminal");
        assert!(result.version.is_none() && result.build_id.is_none());
        assert!(!sentinel.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    #[ignore = "read-only archived tmux artifact checks; requires PROGRAM_METADATA_TEST_TMUX_OLD and PROGRAM_METADATA_TEST_TMUX_CURRENT"]
    fn archived_tmux_versions_remain_distinct_through_shared_metadata_and_launcher_cache() {
        let old = File::open(
            std::env::var_os("PROGRAM_METADATA_TEST_TMUX_OLD")
                .expect("provide the archived tmux 3.7c-simple6 executable"),
        )
        .unwrap();
        let current = File::open(
            std::env::var_os("PROGRAM_METADATA_TEST_TMUX_CURRENT")
                .expect("provide the installed tmux 3.7c-fork0.2.2-2 executable"),
        )
        .unwrap();
        for _ in 0..2 {
            let running = probe_executable(&old, "tmux");
            let installed = probe_executable(&current, "tmux");
            assert_eq!(running.version.as_deref(), Some("tmux 3.7c-simple6"));
            assert_eq!(installed.version.as_deref(), Some("tmux 3.7c-fork0.2.2-2"));
            assert_eq!(
                running.build_id.as_deref(),
                Some("5d54545114a19c9065d441e3e2fbd101eb9b310f")
            );
            assert_eq!(
                installed.build_id.as_deref(),
                Some("3b856a405ee503651bbaee33f59f35f6de27a3c1")
            );
            assert_eq!(
                same_executable_build(&running, &installed, false),
                Some(false)
            );
        }
    }

    #[test]
    #[ignore = "read-only tmux version checks; requires APPLICATIONLAUNCHER_TEST_TMUX_PID and APPLICATIONLAUNCHER_TEST_TMUX_INSTALLED"]
    fn live_tmux_installed_alias_is_distinct_from_an_archived_running_build() {
        let pid = std::env::var("APPLICATIONLAUNCHER_TEST_TMUX_PID")
            .unwrap()
            .parse::<i32>()
            .unwrap();
        let installed =
            PathBuf::from(std::env::var_os("APPLICATIONLAUNCHER_TEST_TMUX_INSTALLED").unwrap());
        let executable = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap();
        assert_eq!(clean_executable(&executable).file_name().unwrap(), "tmux");
        let expected = probe_executable(&File::open(&installed).unwrap(), "tmux");
        let result = probe_version(&ProcessVersionTarget {
            pid,
            executable,
            program: Some("tmux".into()),
            launch_context: ProcessLaunchContext::default(),
            application_entry: None,
            window_declared_entry: false,
        });
        assert_ne!(
            result.version, expected.version,
            "the live fixture must be running an older tmux build"
        );
        assert_eq!(result.latest_version, expected.version);
        assert_eq!(result.latest_build_id, expected.build_id);
        assert_eq!(result.running_latest_binary, Some(false));
        applicationlauncher::diagnostic_log!(
            "{}",
            serde_json::json!({"pid":pid,"running_version":result.version,"latest_installed_version":result.latest_version,"running_build_id":result.build_id,"latest_build_id":result.latest_build_id,"running_latest_binary":result.running_latest_binary})
        );
    }

    #[test]
    fn versions_accept_known_multiline_output_but_not_errors_or_controls() {
        assert!(!valid_package_name("package\x1b[31m1.2"));
        assert!(!valid_package_name("package\nother"));
        assert_eq!(
            clean_executable(Path::new("/usr/bin/fish (deleted)")),
            PathBuf::from("/usr/bin/fish")
        );
    }

    #[test]
    fn mismatched_or_missing_running_binary_is_not_reported() {
        let target = ProcessVersionTarget {
            pid: std::process::id() as i32,
            executable: "/definitely/not/the/running/binary".into(),
            launch_context: ProcessLaunchContext::default(),
            program: None,
            application_entry: None,
            window_declared_entry: false,
        };
        let result = probe_version(&target);
        assert!(result.version.is_none() && result.package.is_none() && result.build_id.is_none());
    }

    #[test]
    fn inaccessible_running_image_recovers_only_passive_absolute_argv0_disk_metadata() {
        use std::os::unix::process::CommandExt;
        use std::process::Stdio;

        struct CandidateFixture {
            child: std::process::Child,
            directory: PathBuf,
        }
        impl Drop for CandidateFixture {
            fn drop(&mut self) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                let _ = std::fs::remove_dir_all(&self.directory);
            }
        }
        let directory = std::env::temp_dir().join(format!(
            "launcher-argv0-candidate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let candidate = directory.join("bash");
        std::fs::copy("/usr/bin/bash", &candidate).unwrap();
        // Launch a known system sleep, not the candidate. argv[0] deliberately
        // refers to a different image, proving it must not become running proof.
        let child = Command::new("/usr/bin/sleep")
            .arg0(&candidate)
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut fixture = CandidateFixture { child, directory };
        wait_for_fixture_argv0(&mut fixture.child, &candidate);
        let target = ProcessVersionTarget {
            pid: fixture.child.id() as i32,
            executable: candidate.clone(),
            launch_context: ProcessLaunchContext {
                raw_argv0: Some(candidate.clone()),
                executable_unavailable: Some("Permission denied reading process executable"),
            },
            program: Some("systemd".into()),
            application_entry: None,
            window_declared_entry: false,
        };
        let initial_argv0 = process_argv0(target.pid);
        let result = probe_version_checked(&target, || true);
        assert_eq!(
            result.on_disk_candidate,
            Some(candidate),
            "reason={:?}; initial argv0={initial_argv0:?}; current argv0={:?}; child status={:?}",
            result.on_disk_candidate_reason,
            process_argv0(target.pid),
            fixture.child.try_wait()
        );
        assert_eq!(
            result.running_unavailable,
            target.launch_context.executable_unavailable
        );
        assert!(result.version.is_none() && result.build_id.is_none());
        assert!(result.running_latest_binary.is_none());
        assert_eq!(result.process_build_is_current(), None);
        assert!(
            result
                .latest_version
                .as_deref()
                .is_some_and(|value| value.contains("bash"))
        );
        assert_eq!(
            result.latest_build_id,
            probe_build_id(&File::open(&target.executable).unwrap())
        );
        assert!(
            result
                .on_disk_candidate_reason
                .as_deref()
                .unwrap()
                .contains("not verified")
        );
        assert!(result.installation.is_some());
        let rejected = probe_version_checked(&target, || false);
        assert!(rejected.latest_build_id.is_none() && rejected.on_disk_candidate.is_none());
        let mut mismatched = target.clone();
        mismatched.executable = "/usr/bin/bash".into();
        mismatched.launch_context.raw_argv0 = Some(mismatched.executable.clone());
        let rejected = probe_version_checked(&mismatched, || true);
        assert!(rejected.latest_build_id.is_none() && rejected.on_disk_candidate.is_none());
        let calls = std::cell::Cell::new(0);
        let changed = probe_version_checked(&target, || {
            calls.set(calls.get() + 1);
            calls.get() == 1
        });
        assert!(changed.latest_build_id.is_none() && changed.on_disk_candidate.is_none());
        assert!(changed.package.is_none() && changed.progs_metadata.description.is_none());
        // An absolute argv[0] is not a license to execute a script or to block
        // on a device/FIFO. Its file must be independently pinned as ELF.
        let marker = fixture.directory.join("must-not-be-created");
        std::fs::write(
            &target.executable,
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )
        .unwrap();
        let rejected = probe_version_checked(&target, || true);
        assert!(rejected.latest_build_id.is_none() && rejected.on_disk_candidate.is_none());
        assert!(!marker.exists());
        std::fs::remove_file(&target.executable).unwrap();
        let path =
            std::ffi::CString::new(target.executable.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let started = Instant::now();
        let rejected = probe_version_checked(&target, || true);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(rejected.on_disk_candidate.is_none());
    }

    #[test]
    fn python_runtime_launch_keeps_exact_venv_argv0_without_claiming_a_different_image() {
        use std::os::unix::fs::symlink;
        use std::os::unix::process::CommandExt;
        use std::process::Stdio;

        struct RuntimeFixture {
            child: std::process::Child,
            directory: PathBuf,
        }
        impl Drop for RuntimeFixture {
            fn drop(&mut self) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                let _ = std::fs::remove_dir_all(&self.directory);
            }
        }
        let directory = std::env::temp_dir().join(format!(
            "launcher-venv-route-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(directory.join(".venv/bin")).unwrap();
        let launcher = directory.join(".venv/bin/python3");
        symlink("/usr/bin/python3", &launcher).unwrap();
        let child = Command::new("/usr/bin/python3")
            .arg0(&launcher)
            .args(["-c", "import sys; sys.stdin.buffer.read()"])
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut fixture = RuntimeFixture { child, directory };
        wait_for_fixture_argv0(&mut fixture.child, &launcher);
        let target = ProcessVersionTarget {
            pid: fixture.child.id() as i32,
            executable: std::fs::read_link(format!("/proc/{}/exe", fixture.child.id())).unwrap(),
            launch_context: ProcessLaunchContext {
                raw_argv0: Some(launcher.clone()),
                executable_unavailable: None,
            },
            program: Some("python3".into()),
            application_entry: None,
            window_declared_entry: false,
        };
        let result = probe_version_checked(&target, || true);
        assert_eq!(result.runtime_name, Some("Python"));
        let launch = result.launch.unwrap();
        assert_eq!(launch.chain.first(), Some(&launcher));
        assert_eq!(launch.executable.as_ref(), Some(&target.executable));
        assert!(
            launch
                .identification
                .contains("original process launch route is not proven")
        );
        let expected = std::fs::metadata(&target.executable).unwrap();
        let recorded = process_launch_from_argv0(&target, &expected).unwrap();
        std::fs::remove_file(&launcher).unwrap();
        symlink("/usr/bin/sleep", &launcher).unwrap();
        assert!(!recorded.unchanged());
        assert!(process_launch_from_argv0(&target, &expected).is_none());
        let mut mismatched = target.clone();
        mismatched.launch_context.raw_argv0 = Some("/usr/bin/python3".into());
        assert!(process_launch_from_argv0(&mismatched, &expected).is_none());
    }

    #[test]
    fn exited_process_keeps_disk_metadata_without_claiming_a_running_binary() {
        let target = ProcessVersionTarget {
            pid: i32::MAX,
            executable: std::env::current_exe().unwrap(),
            launch_context: ProcessLaunchContext::default(),
            program: None,
            application_entry: None,
            window_declared_entry: false,
        };
        let result = probe_version(&target);
        assert!(
            result.installation.is_some(),
            "path metadata was lost when the PID exited"
        );
        assert!(result.uncompiled_changes.is_some());
        assert!(result.version.is_none());
        assert!(result.build_id.is_none());
        assert!(result.running_latest_binary.is_none());
        assert_eq!(
            result.running_unavailable,
            Some("process exited before inspection")
        );
    }

    #[test]
    fn identity_change_during_inspection_discards_only_running_metadata() {
        let target = ProcessVersionTarget {
            pid: std::process::id() as i32,
            executable: std::fs::read_link("/proc/self/exe").unwrap(),
            launch_context: ProcessLaunchContext::default(),
            program: None,
            application_entry: None,
            window_declared_entry: false,
        };
        let checks = std::cell::Cell::new(0);
        let result = probe_version_checked(&target, || {
            checks.set(checks.get() + 1);
            checks.get() == 1
        });
        assert_eq!(checks.get(), 2);
        assert_eq!(
            result.running_unavailable,
            Some("process exited or changed during inspection")
        );
        assert!(result.installation.is_some());
        assert!(result.uncompiled_changes.is_some());
        assert!(result.version.is_none());
        assert!(result.build_id.is_none());
        assert!(result.running_latest_binary.is_none());
    }

    #[test]
    #[ignore = "read-only Arch package ownership integration; optional APPLICATIONLAUNCHER_TEST_PACKAGE_COMPILATION_IMAGE must name the package-owned true executable"]
    fn package_managed_compilation_status_is_consistent_across_native_info_consumers() {
        let executable = std::env::var_os("APPLICATIONLAUNCHER_TEST_PACKAGE_COMPILATION_IMAGE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/bin/true"));
        assert_eq!(executable.file_name(), Some(std::ffi::OsStr::new("true")));
        let file = File::open(&executable).unwrap();
        let proof = installation::installed_package_image(&executable, &file)
            .expect("fixture must be a current exact package-owned native true image");
        let expected = "Not applicable (package-managed installation)";
        let inspect = |executable: PathBuf| {
            let target = ProcessVersionTarget {
                pid: -1,
                executable: executable.clone(),
                launch_context: ProcessLaunchContext::default(),
                program: Some("true".into()),
                application_entry: None,
                window_declared_entry: false,
            };
            let process = probe_version_checked(&target, || false);
            let application = inspect_application(
                executable.to_str().unwrap(),
                Path::new("/nonexistent/compilation-status-test.desktop"),
            );
            assert!(process.running_unavailable.is_some());
            assert!(process.version.is_none() && process.build_id.is_none());
            assert_eq!(process.process_build_is_current(), None);
            // No running process is attached. The package-owned on-disk true
            // may use its bounded --version recipe; the unmanaged copy cannot.
            (process, application)
        };
        let (process, application) = inspect(executable.clone());
        assert_eq!(
            process.uncompiled_changes.as_deref(),
            Some(expected),
            "{process:?}"
        );
        assert_eq!(
            application.uncompiled_changes.as_deref(),
            Some(expected),
            "{application:?}"
        );
        assert!(proof.unchanged(&file));

        struct CopyFixture(PathBuf);
        impl Drop for CopyFixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let fixture = CopyFixture(std::env::temp_dir().join(format!(
            "launcher-package-compilation-{}-{nonce}",
            std::process::id()
        )));
        std::fs::create_dir(&fixture.0).unwrap();
        let copied = fixture.0.join("true");
        std::fs::copy(&executable, &copied).unwrap();
        assert!(
            installation::installed_package_image(&copied, &File::open(&copied).unwrap()).is_none()
        );
        let (process, application) = inspect(copied);
        assert_ne!(
            process.uncompiled_changes.as_deref(),
            Some(expected),
            "{process:?}"
        );
        assert_ne!(
            application.uncompiled_changes.as_deref(),
            Some(expected),
            "{application:?}"
        );
        applicationlauncher::diagnostic_log!(
            "native process and ApplicationInfo compilation status: exact package image=N/A; identical unmanaged true copy is not N/A; no wrapper or unmanaged image executed"
        );
    }

    #[test]
    #[ignore = "Arch coreutils integration; requires APPLICATIONLAUNCHER_TEST_COREUTILS_VERSION, starts only an owned cat stdin fixture and bounded cat --version probes"]
    fn package_owned_cat_versions_and_compilation_status_reach_native_info_consumers() {
        use std::process::Stdio;

        let version = std::env::var("APPLICATIONLAUNCHER_TEST_COREUTILS_VERSION")
            .expect("provide the independently established upstream GNU coreutils version");
        assert!(!version.is_empty() && !version.chars().any(char::is_whitespace));
        let expected_version = format!("cat (GNU coreutils) {version}");
        let executable = PathBuf::from("/usr/bin/cat");
        let file = File::open(&executable).unwrap();
        let proof = installation::installed_package_image(&executable, &file)
            .expect("fixture must be the exact current package-owned native cat image");
        assert!(proof.package.starts_with("coreutils "), "{proof:?}");

        struct OwnedCat(std::process::Child);
        impl Drop for OwnedCat {
            fn drop(&mut self) {
                // This guard owns the only persistent process started by the
                // test. Never attach, write to, or terminate a user's cat.
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = OwnedCat(
            Command::new(&executable)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        wait_for_fixture_argv0(&mut child.0, &executable);
        let pid = child.0.id() as i32;
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let identity = crate::models::ProcessIdentity {
            pid,
            start_ticks: stat[stat.rfind(')').unwrap() + 1..]
                .split_whitespace()
                .nth(19)
                .unwrap()
                .parse()
                .unwrap(),
        };
        let pinned = std::fs::metadata(format!("/proc/{pid}/exe")).unwrap();
        let current = || {
            crate::windows::process_tree::process_identity_is_current(
                identity,
                Some((pinned.dev(), pinned.ino())),
            )
        };
        assert!(current() && proof.unchanged(&file));
        let target = ProcessVersionTarget {
            pid,
            executable: executable.clone(),
            launch_context: ProcessLaunchContext::default(),
            program: Some("cat".into()),
            application_entry: None,
            window_declared_entry: false,
        };
        let process = probe_version_checked(&target, current);
        assert_eq!(process.running_unavailable, None, "{process:?}");
        assert_eq!(
            process.version.as_deref(),
            Some(expected_version.as_str()),
            "{process:?}"
        );
        assert_eq!(process.latest_version, process.version, "{process:?}");
        assert_eq!(process.version_reason, None, "{process:?}");
        assert_eq!(process.latest_version_reason, None, "{process:?}");
        assert!(process.build_id.is_some(), "{process:?}");
        assert_eq!(process.latest_build_id, process.build_id, "{process:?}");
        assert_eq!(
            process.process_build_is_current(),
            Some(true),
            "{process:?}"
        );
        assert_eq!(process.package.as_deref(), Some(proof.package.as_str()));
        let expected_compilation = "Not applicable (package-managed installation)";
        assert_eq!(
            process.uncompiled_changes.as_deref(),
            Some(expected_compilation),
            "{process:?}"
        );

        let application = inspect_application(
            executable.to_str().unwrap(),
            Path::new("/nonexistent/coreutils-version-test.desktop"),
        );
        assert_eq!(
            application.version.as_deref(),
            Some(expected_version.as_str()),
            "{application:?}"
        );
        assert_eq!(application.version_reason, None, "{application:?}");
        assert_eq!(application.build_id, process.build_id, "{application:?}");
        assert_eq!(application.package, process.package, "{application:?}");
        assert_eq!(
            application.uncompiled_changes.as_deref(),
            Some(expected_compilation),
            "{application:?}"
        );

        let stale = probe_version_checked(&target, || false);
        assert!(stale.running_unavailable.is_some(), "{stale:?}");
        assert!(stale.version.is_none() && stale.build_id.is_none());
        assert_eq!(stale.process_build_is_current(), None, "{stale:?}");
        assert_eq!(stale.latest_version, process.latest_version, "{stale:?}");
        assert!(current() && proof.unchanged(&file));
        assert!(child.0.try_wait().unwrap().is_none());
        applicationlauncher::diagnostic_log!(
            "owned cat: running={:?}; disk={:?}; package={:?}; compilation=N/A; stale process retains only disk evidence",
            process.version,
            process.latest_version,
            process.package
        );
    }

    #[test]
    #[ignore = "read-only installed bwrap and Progs inventory checks; requires this system's package metadata"]
    fn installed_bwrap_metadata_survives_process_exit() {
        let target = ProcessVersionTarget {
            pid: i32::MAX,
            executable: "/usr/bin/bwrap".into(),
            launch_context: ProcessLaunchContext::default(),
            program: Some("bwrap".into()),
            application_entry: None,
            window_declared_entry: false,
        };
        let result = probe_version(&target);
        assert!(
            result
                .installation
                .as_deref()
                .unwrap()
                .contains("Pacman Dependency")
        );
        assert!(
            result
                .package
                .as_deref()
                .unwrap()
                .starts_with("bubblewrap ")
        );
        assert_eq!(
            result.progs_metadata.description.as_deref(),
            Some("Unprivileged sandboxing tool")
        );
        assert!(
            result
                .progs_metadata
                .capabilities
                .as_deref()
                .unwrap()
                .starts_with("CLI")
        );
        assert!(
            result
                .latest_version
                .as_deref()
                .unwrap()
                .starts_with("bubblewrap ")
        );
        assert!(result.latest_build_id.is_some());
        assert!(result.version.is_none() && result.build_id.is_none());
        let latest_package = available::query(result.package_target.as_ref().unwrap()).display();
        assert!(latest_package.contains("/bubblewrap "), "{latest_package}");
        applicationlauncher::diagnostic_log!(
            "installation={:?}; package={:?}; description={:?}; capabilities={:?}; disk_version={:?}; disk_build_id={:?}; latest_package={latest_package}; running_unavailable={:?}",
            result.installation,
            result.package,
            result.progs_metadata.description,
            result.progs_metadata.capabilities,
            result.latest_version,
            result.latest_build_id,
            result.running_unavailable
        );
    }

    #[test]
    fn renamed_running_image_recovers_installation_without_a_backup_filename_pattern() {
        use std::process::Stdio;

        struct PrivateProcess {
            child: std::process::Child,
            directory: PathBuf,
        }
        impl Drop for PrivateProcess {
            fn drop(&mut self) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                let _ = std::fs::remove_dir_all(&self.directory);
            }
        }
        for (name, arguments, runtime) in [
            (
                "bash",
                &[
                    "--noprofile",
                    "--norc",
                    "-c",
                    "while IFS= read -r line; do :; done",
                ][..],
                None,
            ),
            (
                "python3",
                &["-c", "import sys; sys.stdin.buffer.read()"][..],
                Some("Python"),
            ),
        ] {
            let directory = std::env::temp_dir().join(format!(
                "al-renamed-image-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&directory).unwrap();
            let published = directory.join(name);
            std::fs::copy(format!("/usr/bin/{name}"), &published).unwrap();
            let expected_running_id = probe_build_id(&File::open(&published).unwrap());
            let deadline = Instant::now() + Duration::from_secs(2);
            let child = loop {
                match Command::new(&published)
                    .args(arguments)
                    .env_clear()
                    .env("LC_ALL", "C")
                    .current_dir(&directory)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    Ok(child) => break child,
                    Err(error)
                        if error.raw_os_error() == Some(libc::ETXTBSY)
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("private {name} did not start: {error}"),
                }
            };
            let process = PrivateProcess { child, directory };
            let proc_exe = format!("/proc/{}/exe", process.child.id());
            let deadline = Instant::now() + Duration::from_secs(2);
            while std::fs::read_link(&proc_exe).ok().as_deref() != Some(&published)
                || std::fs::read_to_string(format!("/proc/{}/comm", process.child.id()))
                    .ok()
                    .as_deref()
                    .map(str::trim_end)
                    != Some(name)
            {
                assert!(Instant::now() < deadline, "private {name} did not start");
                std::thread::sleep(Duration::from_millis(5));
            }
            let retained = process
                .directory
                .join("retired-image-with-no-conventional-suffix");
            std::fs::rename(&published, &retained).unwrap();
            std::fs::remove_file(&retained).unwrap();
            std::fs::copy("/usr/bin/sleep", &published).unwrap();
            let expected_installed_id = probe_build_id(&File::open(&published).unwrap());
            assert_ne!(expected_running_id, expected_installed_id);
            let target = ProcessVersionTarget {
                pid: process.child.id() as i32,
                executable: std::fs::read_link(&proc_exe).unwrap(),
                launch_context: ProcessLaunchContext::default(),
                program: Some(name.into()),
                application_entry: None,
                window_declared_entry: false,
            };
            let result = probe_version(&target);
            assert_eq!(result.build_id, expected_running_id, "{result:?}");
            assert_eq!(result.latest_build_id, expected_installed_id, "{result:?}");
            assert_eq!(result.runtime_name, runtime);
            let header = probe_header_checked(&target, || true);
            assert_eq!(header.runtime_name, runtime);
            if runtime.is_some() {
                assert!(result.process_build_is_current().is_none());
                assert!(header.process_build_is_current().is_none());
            }
            let launch = result.launch.as_ref().unwrap();
            assert_eq!(launch.executable.as_ref(), Some(&published));
            assert_eq!(launch.chain.first(), Some(&published));
            assert!(
                launch.identification.contains("process launch"),
                "{launch:?}"
            );
            assert!(!launch.identification.contains("installation symlink"));

            // Losing the captured process identity must not authorize this recovery.
            let changed = probe_version_checked(&target, || false);
            assert!(changed.build_id.is_none());
            assert!(changed.latest_build_id.is_none());
        }
    }

    #[test]
    fn replaced_binary_reports_the_pinned_running_version_not_the_replacement() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Stdio;
        struct PrivateProcess {
            child: std::process::Child,
            directory: PathBuf,
        }
        impl Drop for PrivateProcess {
            fn drop(&mut self) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                let _ = std::fs::remove_dir_all(&self.directory);
            }
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("al-version-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("bash");
        std::fs::copy(
            applicationlauncher::process::executable_path("bash").unwrap(),
            &path,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let child = loop {
            match Command::new(&path)
                .args([
                    "--noprofile",
                    "--norc",
                    "-c",
                    "while IFS= read -r line; do :; done",
                ])
                .env_remove("BASH_ENV")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => break child,
                // Parallel subprocess tests can briefly inherit the copy's
                // writable descriptor before exec closes it.
                Err(error)
                    if error.raw_os_error() == Some(libc::ETXTBSY) && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("private bash did not start: {error}"),
            }
        };
        let process = PrivateProcess { child, directory };
        let deadline = Instant::now() + Duration::from_secs(2);
        while std::fs::read_link(format!("/proc/{}/exe", process.child.id()))
            .ok()
            .as_deref()
            != Some(path.as_path())
        {
            assert!(Instant::now() < deadline, "private bash did not start");
            std::thread::sleep(Duration::from_millis(5));
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"not the running ELF binary").unwrap();
        let target = ProcessVersionTarget {
            pid: process.child.id() as i32,
            executable: path.clone(),
            launch_context: ProcessLaunchContext::default(),
            program: None,
            application_entry: None,
            window_declared_entry: false,
        };
        let version = probe_version(&target);
        assert!(
            version
                .version
                .as_deref()
                .is_some_and(|value| value.contains("bash") && value.contains("version")),
            "{version:?}"
        );
        assert!(version.latest_version.is_none());
        assert!(version.running_latest_binary.is_none());
        assert!(version.build_id.is_some());

        // Changing only the disk image must refresh the latest version even
        // though the process keeps running the same deleted bash executable.
        std::fs::remove_file(&path).unwrap();
        std::fs::copy(
            applicationlauncher::process::executable_path("bash").unwrap(),
            &path,
        )
        .unwrap();
        let updated = probe_version(&target);
        assert_eq!(updated.version, version.version);
        assert_eq!(updated.latest_version, version.version);
        assert_eq!(updated.build_id, version.build_id);
        assert_eq!(updated.latest_build_id, version.build_id);
        // A fixture's retained off-PATH path is inspectable, but does not prove
        // which installation the user currently publishes by command name.
        assert_eq!(updated.running_latest_binary, None);
        let header = probe_header_checked(&target, || true);
        assert_eq!(header.process_build_is_current(), None);
        assert!(header.version.is_none() && header.latest_version.is_none());
        assert!(header.package.is_none() && header.package_target.is_none());
        assert!(header.uncompiled_changes.is_none());

        std::fs::remove_file(&path).unwrap();
        std::fs::copy(
            applicationlauncher::process::executable_path("python3").unwrap(),
            &path,
        )
        .unwrap();
        let different = probe_version(&target);
        assert_eq!(different.version, version.version);
        assert!(different.latest_build_id.is_some());
        assert_ne!(different.latest_version, different.version);
        assert_ne!(different.latest_build_id, different.build_id);
        assert_eq!(different.running_latest_binary, None);
        assert_eq!(
            probe_header_checked(&target, || true).process_build_is_current(),
            None
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\ntouch '{}'\n",
                process.directory.join("MUST_NOT_RUN").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let wrapper = probe_version(&target);
        assert_eq!(wrapper.version, version.version);
        assert!(wrapper.latest_version.is_none());
        assert!(wrapper.running_latest_binary.is_none());
        assert!(!process.directory.join("MUST_NOT_RUN").exists());
    }

    #[test]
    #[ignore = "passive exact old-process/current-publication regression; requires APPLICATIONLAUNCHER_TEST_MOUSEPAD_PID and APPLICATIONLAUNCHER_TEST_COPYQ_PID"]
    fn running_mousepad_and_copyq_keep_old_versions_separate_from_current_published_builds() {
        for (
            variable,
            program,
            reported_start_ticks,
            running_version,
            running_id,
            latest_version,
            latest_id,
        ) in [
            (
                "APPLICATIONLAUNCHER_TEST_MOUSEPAD_PID",
                "mousepad",
                10678260,
                "Mousepad 0.7.0-dev-92e61131",
                "d1e2863bbd5acdbd5fe2af8f375c4b18b91cd008",
                "Mousepad 0.7.0-fork0.0.3-1",
                "72ceb10d59517c3b182c18591a405d31e223c7c1",
            ),
            (
                "APPLICATIONLAUNCHER_TEST_COPYQ_PID",
                "copyq",
                380944273,
                "CopyQ 15.0.0.21-gfb3f9622",
                "7a61e14139b9167dec2b45f7d1392cfa8cd574fc",
                "CopyQ 15.0.0-fork0.0.3-1",
                "555d2d63246ee86007cae3220be8b98f20539c39",
            ),
        ] {
            let pid = std::env::var(variable)
                .expect("provide the explicit unchanged fixture process PID")
                .parse::<i32>()
                .unwrap();
            assert!(pid > 0);
            let start_ticks = std::env::var(format!("{variable}_START_TICKS"))
                .ok()
                .map(|value| value.parse::<u64>().unwrap())
                .unwrap_or(reported_start_ticks);
            let identity = crate::models::ProcessIdentity { pid, start_ticks };
            let path = PathBuf::from(format!("/proc/{pid}/exe"));
            let executable = std::fs::read_link(&path).unwrap();
            let metadata = std::fs::metadata(&path).unwrap();
            let executable_identity = Some((metadata.dev(), metadata.ino()));
            let current = || {
                crate::windows::process_tree::process_identity_is_current(
                    identity,
                    executable_identity,
                )
            };
            assert!(
                current(),
                "{program} fixture process changed or PID was reused"
            );
            let target = ProcessVersionTarget {
                pid,
                executable,
                launch_context: ProcessLaunchContext::default(),
                program: Some(program.into()),
                application_entry: None,
                window_declared_entry: false,
            };
            let result = probe_version_checked(&target, current);
            assert_eq!(result.running_unavailable, None, "{result:?}");
            assert_eq!(
                result.version.as_deref(),
                Some(running_version),
                "{result:?}"
            );
            assert_eq!(result.build_id.as_deref(), Some(running_id), "{result:?}");
            assert_eq!(
                result.latest_version.as_deref(),
                Some(latest_version),
                "{result:?}"
            );
            assert_eq!(
                result.latest_build_id.as_deref(),
                Some(latest_id),
                "{result:?}"
            );
            assert_eq!(result.running_latest_binary, Some(false), "{result:?}");
            assert_eq!(result.process_build_is_current(), Some(false));
            assert!(
                current(),
                "{program} fixture process changed during inspection"
            );
            applicationlauncher::diagnostic_log!(
                "{program}: running={:?} latest={:?} running_id={:?} latest_id={:?} current={:?}",
                result.version,
                result.latest_version,
                result.build_id,
                result.latest_build_id,
                result.running_latest_binary
            );
        }
    }

    #[test]
    #[ignore = "controlled exact-image Fish regression; requires APPLICATIONLAUNCHER_TEST_FISH_PID and an explicit local probe grant"]
    fn trusted_fish_keeps_deleted_running_version_separate_from_current_installation() {
        let pid = std::env::var("APPLICATIONLAUNCHER_TEST_FISH_PID")
            .expect("provide the exact reported Fish PID")
            .parse::<i32>()
            .unwrap();
        let start_ticks = std::env::var("APPLICATIONLAUNCHER_TEST_FISH_START_TICKS")
            .ok()
            .map(|value| value.parse::<u64>().unwrap())
            .unwrap_or(1646497);
        let identity = crate::models::ProcessIdentity { pid, start_ticks };
        let executable = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap();
        let pinned = std::fs::metadata(format!("/proc/{pid}/exe")).unwrap();
        let current = || {
            crate::windows::process_tree::process_identity_is_current(
                identity,
                Some((pinned.dev(), pinned.ino())),
            )
        };
        assert!(
            current(),
            "the reported Fish process changed or PID was reused"
        );
        let target = ProcessVersionTarget {
            pid,
            executable,
            launch_context: ProcessLaunchContext::default(),
            program: Some("fish".into()),
            application_entry: None,
            window_declared_entry: false,
        };
        let result = probe_version_checked(&target, current);
        assert_eq!(result.running_unavailable, None, "{result:?}");
        assert_eq!(
            result.version.as_deref(),
            Some("fish, version 4.6.0-80-g86b86596a-dirty"),
            "{result:?}"
        );
        assert_eq!(
            result.build_id.as_deref(),
            Some("43b72e74f30bc0689bbbc1707e272a298d96fd7e"),
            "{result:?}"
        );
        assert_eq!(
            result.latest_version.as_deref(),
            Some("fish, version 4.6.0-fork0.0.3-0"),
            "{result:?}"
        );
        assert_eq!(
            result.latest_build_id.as_deref(),
            Some("5a72d02d41713713ad4a102de7ac9c00281fe000"),
            "{result:?}"
        );
        assert_eq!(result.process_build_is_current(), Some(false), "{result:?}");
        assert!(
            current(),
            "the reported Fish process changed during inspection"
        );
        applicationlauncher::diagnostic_log!(
            "fish: running={:?} latest={:?} running_id={:?} latest_id={:?}",
            result.version,
            result.latest_version,
            result.build_id,
            result.latest_build_id
        );
    }

    #[test]
    #[ignore = "requires independent PID/start/product/path/version/full-build-ID oracle in APPLICATIONLAUNCHER_TEST_HISTORICAL_PROCESS_JSON; never changes grants, inventory, permissions or applications"]
    fn trusted_historical_process_keeps_independent_running_and_current_image_evidence() {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Case {
            pid: i32,
            start_ticks: u64,
            product: String,
            running_path: PathBuf,
            running_version: String,
            running_build_id: String,
            latest_path: PathBuf,
            latest_version: String,
            latest_build_id: String,
        }
        let text = std::env::var("APPLICATIONLAUNCHER_TEST_HISTORICAL_PROCESS_JSON")
            .expect("explicit independent process and installed-image oracle required");
        assert!(text.len() <= 16 * 1024);
        let case: Case = serde_json::from_str(&text).expect("complete exact-image oracle schema");
        assert!(case.pid > 0 && case.start_ticks > 0);
        assert!(case.product.len() <= 64 && !case.product.is_empty());
        assert!(
            case.product
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        );
        assert!(case.running_path.is_absolute() && case.latest_path.is_absolute());
        for id in [&case.running_build_id, &case.latest_build_id] {
            assert!((16..=128).contains(&id.len()));
            assert!(id.len().is_multiple_of(2));
            assert!(id.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
        assert_ne!(case.running_build_id, case.latest_build_id);
        assert_ne!(case.running_version, case.latest_version);
        let identity = crate::models::ProcessIdentity {
            pid: case.pid,
            start_ticks: case.start_ticks,
        };
        let proc_path = PathBuf::from(format!("/proc/{}/exe", case.pid));
        let executable = std::fs::read_link(&proc_path).unwrap();
        assert_eq!(clean_executable(&executable), case.running_path);
        let pinned = std::fs::metadata(&proc_path).unwrap();
        let current = || {
            crate::windows::process_tree::process_identity_is_current(
                identity,
                Some((pinned.dev(), pinned.ino())),
            )
        };
        assert!(current(), "fixture process changed or PID was reused");
        let disk_identity = |metadata: &std::fs::Metadata| {
            (
                metadata.dev(),
                metadata.ino(),
                metadata.len(),
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            )
        };
        let disk_before = disk_identity(&std::fs::metadata(&case.latest_path).unwrap());
        assert_ne!((pinned.dev(), pinned.ino()), (disk_before.0, disk_before.1));
        let target = ProcessVersionTarget {
            pid: case.pid,
            executable,
            launch_context: ProcessLaunchContext::default(),
            program: Some(case.product),
            application_entry: None,
            window_declared_entry: false,
        };
        let result = probe_version_checked(&target, current);
        assert_eq!(result.running_unavailable, None, "{result:?}");
        assert_eq!(
            result.version.as_deref(),
            Some(case.running_version.as_str()),
            "{result:?}"
        );
        assert_eq!(
            result.build_id.as_deref(),
            Some(case.running_build_id.as_str()),
            "{result:?}"
        );
        assert_eq!(
            result.latest_version.as_deref(),
            Some(case.latest_version.as_str()),
            "{result:?}"
        );
        assert_eq!(
            result.latest_build_id.as_deref(),
            Some(case.latest_build_id.as_str()),
            "{result:?}"
        );
        assert_eq!(
            result.version_source,
            Some(program_metadata::executable::VersionSource::VerifiedInstallationProbe),
            "the retained running version must have its own exact-image query evidence: {result:?}"
        );
        assert_eq!(result.running_latest_binary, Some(false), "{result:?}");
        assert_eq!(result.process_build_is_current(), Some(false), "{result:?}");
        assert_eq!(
            result
                .launch
                .as_ref()
                .and_then(|launch| launch.executable.as_deref()),
            Some(case.latest_path.as_path()),
            "latest metadata must follow the independently identified installation: {result:?}"
        );
        assert!(current(), "fixture process changed during inspection");
        assert_eq!(
            clean_executable(&std::fs::read_link(&proc_path).unwrap()),
            case.running_path
        );
        assert_eq!(
            disk_identity(&std::fs::metadata(&case.latest_path).unwrap()),
            disk_before
        );
        eprintln!(
            "historical pid={}: running={:?} latest={:?} running_id={:?} latest_id={:?} current={:?}",
            case.pid,
            result.version,
            result.latest_version,
            result.build_id,
            result.latest_build_id,
            result.process_build_is_current()
        );
    }

    #[test]
    #[ignore = "passive installed-image/header inspection; requires APPLICATIONLAUNCHER_TEST_PACKAGE_IMAGE_PROCESSES=pid:start_ticks,... and optional APPLICATIONLAUNCHER_TEST_PACKAGE_DESKTOP_FILE"]
    fn package_owned_off_path_process_headers_use_independent_image_proofs() {
        let processes = std::env::var("APPLICATIONLAUNCHER_TEST_PACKAGE_IMAGE_PROCESSES")
            .expect("provide explicitly captured pid:start_ticks pairs");
        let entry = std::env::var_os("APPLICATIONLAUNCHER_TEST_PACKAGE_DESKTOP_FILE")
            .map(PathBuf::from)
            .map(|desktop_file_path| {
                let entry = program_metadata::desktop::read(
                    &desktop_file_path,
                    None,
                    Instant::now() + Duration::from_millis(200),
                )
                .unwrap();
                crate::models::AppInfo {
                    name: entry.name.expect("desktop Name"),
                    exec: entry.exec.expect("desktop Exec"),
                    desktop_file_path,
                    icon_path: None,
                    comment: entry.comment,
                    is_settings_module: false,
                }
            });
        for process in processes.split(',') {
            let (pid, ticks) = process.split_once(':').expect("expected pid:start_ticks");
            let identity = crate::models::ProcessIdentity {
                pid: pid.parse().unwrap(),
                start_ticks: ticks.parse().unwrap(),
            };
            let proc_exe = PathBuf::from(format!("/proc/{}/exe", identity.pid));
            let running = File::open(&proc_exe).unwrap();
            let metadata = running.metadata().unwrap();
            let executable = std::fs::read_link(&proc_exe).unwrap();
            let current = || {
                crate::windows::process_tree::process_identity_is_current(
                    identity,
                    Some((metadata.dev(), metadata.ino())),
                )
            };
            assert!(current(), "explicit process changed or PID was reused");
            let installed = installation::installed_process_executable(
                identity.pid as u32,
                &executable,
                &running,
            )
            .unwrap();
            assert!(
                !installed.is_published(),
                "fixture must exercise the off-PATH publication gap"
            );
            let disk = File::open(&installed.path).unwrap();
            let package = installation::installed_package_image(&installed.path, &disk)
                .expect("exact current package-owned native image");
            assert!(package.unchanged(&disk));
            let target = ProcessVersionTarget {
                pid: identity.pid,
                executable,
                launch_context: ProcessLaunchContext::default(),
                program: None,
                application_entry: None,
                window_declared_entry: false,
            };
            let header = probe_header_checked(&target, current);
            assert_eq!(header.process_build_is_current(), Some(true), "{header:?}");
            assert!(
                header.version.is_none() && header.latest_version.is_none(),
                "header inspection must remain passive"
            );
            if let Some(entry) = &entry {
                let launch =
                    application::process_launch(&target.executable, Some(&installed), Some(entry));
                assert!(launch.published_target_verified, "{launch:?}");
                let actual = launch
                    .desktop_entry
                    .as_ref()
                    .expect("verified current desktop");
                assert_eq!(actual.desktop_file_path, entry.desktop_file_path);
                let fresh = program_metadata::desktop::read(
                    &actual.desktop_file_path,
                    program_metadata::desktop::current_locale().as_deref(),
                    Instant::now() + Duration::from_millis(200),
                )
                .unwrap();
                assert_eq!(Some(actual.name.as_str()), fresh.name.as_deref());
                assert_eq!(Some(actual.exec.as_str()), fresh.exec.as_deref());
                assert_eq!(actual.comment, fresh.comment);
                assert!(actual.icon_path.as_ref().is_none_or(|path| path.is_file()));
            }
            assert_eq!(
                probe_header_checked(&target, || false).process_build_is_current(),
                None
            );
            assert!(current() && package.unchanged(&disk));
            applicationlauncher::diagnostic_log!(
                "pid={} native_package={:?} header_current={:?}; no application or wrapper executed",
                identity.pid,
                package.package,
                header.process_build_is_current()
            );
        }
    }

    #[test]
    #[ignore = "existing SSH tunnel metadata only; requires APPLICATIONLAUNCHER_TEST_SSH_PID, _START_TICKS, _VERSION and _BUILD_ID; may run only the trusted fixed ssh -V recipe"]
    fn reported_ssh_keeps_exact_product_version_and_direct_tool_roles_without_touching_the_tunnel()
    {
        let pid: i32 = std::env::var("APPLICATIONLAUNCHER_TEST_SSH_PID")
            .expect("explicit existing SSH PID")
            .parse()
            .unwrap();
        let identity = crate::models::ProcessIdentity {
            pid,
            start_ticks: std::env::var("APPLICATIONLAUNCHER_TEST_SSH_START_TICKS")
                .expect("independently captured process start ticks")
                .parse()
                .unwrap(),
        };
        let pinned = std::fs::metadata(format!("/proc/{pid}/exe")).unwrap();
        let current = || {
            crate::windows::process_tree::process_identity_is_current(
                identity,
                Some((pinned.dev(), pinned.ino())),
            )
        };
        assert!(current());
        let cmdline_before = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
        let executable = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap();
        assert_eq!(executable, Path::new("/usr/bin/ssh"));
        let expected = format!(
            "OpenSSH_{}",
            std::env::var("APPLICATIONLAUNCHER_TEST_SSH_VERSION").unwrap()
        );
        let expected_id = std::env::var("APPLICATIONLAUNCHER_TEST_SSH_BUILD_ID").unwrap();
        let process = probe_version_checked(
            &ProcessVersionTarget {
                pid,
                executable: executable.clone(),
                launch_context: ProcessLaunchContext::default(),
                program: Some("ssh".into()),
                application_entry: None,
                window_declared_entry: false,
            },
            current,
        );
        assert_eq!(
            process.version.as_deref(),
            Some(expected.as_str()),
            "{process:?}"
        );
        assert_eq!(process.latest_version, process.version);
        assert_eq!(process.build_id.as_deref(), Some(expected_id.as_str()));
        assert_eq!(process.latest_build_id, process.build_id);
        assert_eq!(process.process_build_is_current(), Some(true));
        assert!(process.package.as_deref().unwrap().starts_with("openssh "));
        assert!(
            process
                .installation
                .as_deref()
                .unwrap()
                .starts_with("[PET]")
        );
        assert!(
            process
                .progs_metadata
                .package_type
                .as_deref()
                .unwrap()
                .starts_with("[PEP]")
        );
        let capabilities = process.progs_metadata.capabilities.as_deref().unwrap();
        let tags = capabilities.split("  •").next().unwrap();
        assert!(tags.split(" · ").any(|tag| tag == "CLI"), "{capabilities}");
        assert!(
            tags.split(" · ").any(|tag| tag == "SERVICE"),
            "the reported host has direct SSH tunnel units: {capabilities}"
        );
        assert!(
            !tags
                .split(" · ")
                .any(|tag| matches!(tag, "JOB" | "GUI" | "LIB")),
            "sibling capabilities must not leak: {capabilities}"
        );
        assert!(
            process
                .uncompiled_changes
                .as_deref()
                .unwrap()
                .starts_with("Not applicable")
        );
        let application = inspect_application(
            executable.to_str().unwrap(),
            Path::new("/nonexistent/ssh-version-test.desktop"),
        );
        assert_eq!(application.version.as_deref(), Some(expected.as_str()));
        assert_eq!(
            application.metadata.capabilities.as_deref(),
            Some(capabilities)
        );
        assert_eq!(
            std::fs::read(format!("/proc/{pid}/cmdline")).unwrap(),
            cmdline_before
        );
        assert!(current(), "the original SSH tunnel must remain untouched");
        applicationlauncher::diagnostic_log!(
            "version={:?}; installation={:?}; capabilities={capabilities}; original PID/start/image unchanged",
            process.version,
            process.installation
        );
    }

    #[test]
    #[ignore = "read-only Codex helper integration; requires APPLICATIONLAUNCHER_TEST_CODEX_HOST_PID, _START_TICKS, _VERSION, _BUILD_ID and _CHECKOUT; optional _NEWER_INPUT independently identifies a newer recorded configuration file"]
    fn reported_codex_helper_preserves_pinned_version_source_and_missing_generated_evidence() {
        let pid: i32 = std::env::var("APPLICATIONLAUNCHER_TEST_CODEX_HOST_PID")
            .expect("explicit fixture PID")
            .parse()
            .unwrap();
        let identity = crate::models::ProcessIdentity {
            pid,
            start_ticks: std::env::var("APPLICATIONLAUNCHER_TEST_CODEX_HOST_START_TICKS")
                .expect("independently captured start ticks")
                .parse()
                .unwrap(),
        };
        let current = || crate::windows::process_tree::process_identity_is_current(identity, None);
        assert!(current(), "the original helper must still be running");
        let executable = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap();
        assert!(
            executable
                .as_os_str()
                .as_encoded_bytes()
                .ends_with(b" (deleted)")
        );
        let disk_path = clean_executable(&executable);
        let expected_version = format!(
            "codex-code-mode-host {}",
            std::env::var("APPLICATIONLAUNCHER_TEST_CODEX_HOST_VERSION").unwrap()
        );
        let expected_id = std::env::var("APPLICATIONLAUNCHER_TEST_CODEX_HOST_BUILD_ID").unwrap();
        let expected_root = std::fs::canonicalize(
            std::env::var_os("APPLICATIONLAUNCHER_TEST_CODEX_HOST_CHECKOUT").unwrap(),
        )
        .unwrap();
        let result = probe_version(&ProcessVersionTarget {
            pid,
            executable,
            program: crate::windows::read_process_stat(pid).map(|stat| stat.name),
            launch_context: ProcessLaunchContext::default(),
            application_entry: None,
            window_declared_entry: false,
        });
        assert_eq!(
            result.version.as_deref(),
            Some(expected_version.as_str()),
            "{result:?}"
        );
        assert_eq!(
            result.latest_version, result.version,
            "the separately inspected disk image has the same embedded version"
        );
        assert_eq!(result.build_id.as_deref(), Some(expected_id.as_str()));
        assert_eq!(result.latest_build_id, result.build_id);
        assert!(result.installation.as_deref().unwrap().starts_with("[FRK]"));
        assert_eq!(
            result.progs_metadata.source_checkout.as_deref(),
            Some(expected_root.as_path())
        );
        assert_eq!(
            result.progs_metadata.description.as_deref(),
            Some("Codex code-mode host")
        );
        assert!(
            result
                .progs_metadata
                .capabilities
                .as_deref()
                .unwrap()
                .starts_with("CLI")
        );
        assert!(
            result.package.is_none(),
            "a source helper is not a package-managed executable"
        );
        let changes = result.uncompiled_changes.as_deref().unwrap();
        let report = program_metadata::build_status::inspect_artifact(
            &disk_path,
            Some(&expected_root),
            std::time::Instant::now() + std::time::Duration::from_millis(1500),
        );
        applicationlauncher::diagnostic_log!(
            "recorded input evidence: {:?}; {}",
            report.newer_files,
            report.explanation
        );
        assert_eq!(report.missing_count, 1);
        if let Some(newer) = std::env::var_os("APPLICATIONLAUNCHER_TEST_CODEX_HOST_NEWER_INPUT") {
            let newer = std::fs::canonicalize(newer).unwrap();
            assert!(newer.starts_with(&expected_root));
            let input_time = std::fs::metadata(&newer).unwrap().modified().unwrap();
            let binary_time = std::fs::metadata(&disk_path).unwrap().modified().unwrap();
            assert!(
                input_time > binary_time,
                "independent newer-input oracle must be real"
            );
            assert_eq!(report.newer_count, 1);
            assert!(report.newer_files.contains(&newer));
            assert!(changes.starts_with("1 newer · 1 missing ·"), "{changes}");
            assert!(changes.contains("1 newer · 1 missing"), "{changes}");
        } else {
            assert_eq!(report.newer_count, 0);
            assert!(changes.starts_with("0 newer · 1 missing ·"), "{changes}");
            assert!(changes.ends_with("full build status unknown"), "{changes}");
            assert!(changes.contains("0 newer · 1 missing"), "{changes}");
        }
        assert!(current());
        applicationlauncher::diagnostic_log!(
            "version={:?}; disk={:?}; type={:?}; source={:?}; changes={changes}",
            result.version,
            result.latest_version,
            result.installation,
            result.progs_metadata.source_checkout
        );
    }

    #[test]
    #[ignore = "read-only reported uv/Python process integration; requires APPLICATIONLAUNCHER_TEST_UV_PID, _PYTHON_SOURCE_PID, their _START_TICKS, _PYTHON_SOURCE and _UV_VERSION"]
    fn reported_uv_and_python_source_keep_product_runtime_and_source_evidence_separate() {
        let target = |key: &str| {
            let pid: i32 = std::env::var(key)
                .expect("explicit fixture PID")
                .parse()
                .unwrap();
            let identity = crate::models::ProcessIdentity {
                pid,
                start_ticks: std::env::var(format!("{key}_START_TICKS"))
                    .expect("independently captured process start ticks")
                    .parse()
                    .unwrap(),
            };
            assert!(crate::windows::process_tree::process_identity_is_current(
                identity, None
            ));
            let stat = crate::windows::read_process_stat(pid).expect("fixture still running");
            (
                ProcessVersionTarget {
                    pid,
                    executable: std::fs::read_link(format!("/proc/{pid}/exe")).unwrap(),
                    launch_context: ProcessLaunchContext::default(),
                    program: Some(stat.name),
                    application_entry: None,
                    window_declared_entry: false,
                },
                identity,
            )
        };
        let (uv_target, uv_start) = target("APPLICATIONLAUNCHER_TEST_UV_PID");
        let expected_uv = std::env::var("APPLICATIONLAUNCHER_TEST_UV_VERSION").unwrap();
        let uv = probe_version(&uv_target);
        assert_eq!(
            uv.version.as_deref(),
            Some(format!("uv {expected_uv}").as_str())
        );
        assert_eq!(uv.latest_version, uv.version);
        assert!(uv.build_id.is_some());
        assert_eq!(uv.latest_build_id, uv.build_id);
        assert!(uv.installation.as_deref().unwrap().starts_with("[UBI]"));
        assert!(crate::windows::process_tree::process_identity_is_current(
            uv_start, None
        ));

        let (python_target, python_start) = target("APPLICATIONLAUNCHER_TEST_PYTHON_SOURCE_PID");
        let source = std::fs::canonicalize(
            std::env::var_os("APPLICATIONLAUNCHER_TEST_PYTHON_SOURCE").unwrap(),
        )
        .unwrap();
        let python = probe_version(&python_target);
        assert_eq!(python.runtime_name, Some("Python"));
        assert!(python.version.as_deref().unwrap().starts_with("Python "));
        assert_eq!(python.latest_version, python.version);
        assert!(python.installation.as_deref().unwrap().starts_with("[UVR]"));
        assert!(
            python
                .uncompiled_changes
                .as_deref()
                .unwrap()
                .starts_with("Not applicable")
        );
        let application = python.script_application.as_ref().unwrap();
        assert_eq!(application.entry_point.as_deref(), Some(source.as_path()));
        assert!(
            python
                .application_installation
                .as_deref()
                .unwrap()
                .starts_with("[DEV]")
        );
        assert!(python.application_progs_metadata.source_checkout.is_some());
        assert!(python.application_progs_metadata.description.is_some());
        assert!(
            python
                .application_progs_metadata
                .capabilities
                .as_deref()
                .unwrap()
                .starts_with("CLI")
        );
        assert!(
            python.application_installed_metadata.is_none(),
            "an unrelated project version is not a helper version"
        );
        assert!(
            python.application_build.is_none(),
            "no startup build identity was published"
        );
        assert!(crate::windows::process_tree::process_identity_is_current(
            python_start,
            None
        ));
        applicationlauncher::diagnostic_log!(
            "uv={:?}; Python={:?}; source={}; attribution={:?}; description={:?}; capabilities={:?}",
            uv.version,
            python.version,
            source.display(),
            python.application_installation,
            python.application_progs_metadata.description,
            python.application_progs_metadata.capabilities,
        );
    }

    #[test]
    #[ignore = "read-only cross-application acceptance; requires APPLICATIONLAUNCHER_TEST_SHARED_METADATA_JSON with independent process identities, versions and role expectations"]
    fn cross_application_metadata_uses_the_shared_backend_without_losing_process_evidence() {
        #[derive(serde::Deserialize)]
        struct Case {
            pid: i32,
            start_ticks: u64,
            version: Option<String>,
            version_reason: Option<String>,
            disk_version: String,
            #[serde(default)]
            roles: Vec<String>,
            upstream: Option<String>,
            #[serde(default)]
            no_newer: bool,
            installation: Option<String>,
            uncompiled_display: Option<String>,
            description_contains: Option<String>,
        }
        let cases: Vec<Case> = serde_json::from_str(
            &std::env::var("APPLICATIONLAUNCHER_TEST_SHARED_METADATA_JSON")
                .expect("explicit existing processes and independent expected metadata"),
        )
        .unwrap();
        assert!(!cases.is_empty() && cases.len() <= 32);
        for case in cases {
            let identity = crate::models::ProcessIdentity {
                pid: case.pid,
                start_ticks: case.start_ticks,
            };
            let pinned = std::fs::metadata(format!("/proc/{}/exe", case.pid)).unwrap();
            let current = || {
                crate::windows::process_tree::process_identity_is_current(
                    identity,
                    Some((pinned.dev(), pinned.ino())),
                )
            };
            assert!(current());
            let command_line = std::fs::read(format!("/proc/{}/cmdline", case.pid)).unwrap();
            let target = ProcessVersionTarget {
                pid: case.pid,
                executable: std::fs::read_link(format!("/proc/{}/exe", case.pid)).unwrap(),
                launch_context: ProcessLaunchContext::default(),
                program: crate::windows::read_process_stat(case.pid).map(|stat| stat.name),
                application_entry: None,
                window_declared_entry: false,
            };
            let process = probe_version_checked(&target, current);
            assert_eq!(
                process.version.as_deref(),
                case.version.as_deref(),
                "{process:?}"
            );
            if let Some(reason) = case.version_reason {
                assert!(
                    process
                        .version_reason
                        .as_deref()
                        .unwrap_or("")
                        .contains(&reason),
                    "{process:?}"
                );
            } else {
                assert!(
                    case.version.is_some(),
                    "an expected unknown requires an independently specified reason"
                );
            }
            assert_eq!(
                process.latest_version.as_deref(),
                Some(case.disk_version.as_str()),
                "{process:?}"
            );
            for role in case.roles {
                assert!(
                    process
                        .progs_metadata
                        .capabilities
                        .as_deref()
                        .unwrap_or("")
                        .split("  •")
                        .next()
                        .unwrap()
                        .split(" · ")
                        .any(|tag| tag == role),
                    "missing {role}: {process:?}"
                );
            }
            if let Some(upstream) = case.upstream {
                let target = process.package_target.as_ref().unwrap();
                assert_eq!(target.name, upstream);
                assert!(target.inferred);
                assert!(
                    process.package.is_none(),
                    "comparison must not confer executable ownership"
                );
            }
            if case.no_newer {
                assert!(
                    process
                        .uncompiled_changes
                        .as_deref()
                        .unwrap_or("")
                        .starts_with("0 newer · 0 missing ·"),
                    "{process:?}"
                );
            }
            if let Some(expected) = case.installation {
                assert_eq!(
                    process.installation.as_deref(),
                    Some(expected.as_str()),
                    "{process:?}"
                );
            }
            if let Some(expected) = case.uncompiled_display {
                assert_eq!(
                    program_metadata::build_status::display_uncompiled_changes(
                        process
                            .uncompiled_changes
                            .as_deref()
                            .unwrap_or("Unavailable")
                    ),
                    expected,
                    "{process:?}"
                );
            }
            if let Some(expected) = case.description_contains {
                assert!(
                    process
                        .progs_metadata
                        .description
                        .as_deref()
                        .is_some_and(|description| description.contains(&expected)),
                    "{process:?}"
                );
            }
            assert!(current(), "original process identity must remain unchanged");
            assert_eq!(
                std::fs::read(format!("/proc/{}/cmdline", case.pid)).unwrap(),
                command_line
            );
            applicationlauncher::diagnostic_log!(
                "pid={}: running={:?}; disk={:?}; installation={:?}; roles={:?}; inputs={:?}; current={:?}; original process unchanged",
                case.pid,
                process.version,
                process.latest_version,
                process.installation,
                process.progs_metadata.capabilities,
                process.uncompiled_changes,
                process.process_build_is_current()
            );
        }
    }

    #[test]
    #[ignore = "read-only version probes; requires APPLICATIONLAUNCHER_TEST_VERSION_PIDS"]
    fn live_process_versions() {
        let pids = std::env::var("APPLICATIONLAUNCHER_TEST_VERSION_PIDS")
            .expect("provide exact owner/active process PIDs");
        for pid in pids.split(',').map(|pid| pid.parse::<i32>().unwrap()) {
            let target = ProcessVersionTarget {
                pid,
                executable: std::fs::read_link(format!("/proc/{pid}/exe")).unwrap(),
                launch_context: ProcessLaunchContext {
                    raw_argv0: process_argv0(pid),
                    executable_unavailable: None,
                },
                program: crate::windows::read_process_stat(pid).map(|stat| stat.name),
                application_entry: None,
                window_declared_entry: false,
            };
            let version = probe_version(&target);
            assert!(
                version.version.is_some(),
                "known running program should report its version: {pid}"
            );
            assert!(version.build_id.is_some());
            if let Some(latest) = &version.latest_build_id {
                assert_eq!(
                    version.running_latest_binary,
                    Some(version.build_id.as_ref() == Some(latest))
                );
            }
            applicationlauncher::diagnostic_log!(
                "pid={pid} version={:?} latest={:?} running_latest={:?} uncompiled_changes={:?} package={:?} build_id={:?} latest_build_id={:?}",
                version.version,
                version.latest_version,
                version.running_latest_binary,
                version.uncompiled_changes,
                version.package,
                version.build_id,
                version.latest_build_id
            );
            applicationlauncher::diagnostic_log!(
                "runtime={:?} application={:?} npm_target={:?} application_npm_target={:?}",
                version.runtime_name,
                version.application,
                version.npm_target,
                version.application_npm_target
            );
            applicationlauncher::diagnostic_log!(
                "current_launch={:?} hosted_launch={:?}",
                version.launch,
                version.application_launch
            );
        }
    }
}
