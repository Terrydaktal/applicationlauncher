use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

use super::{ProcessVersion, ProcessVersionTarget, probe_header_checked, probe_version_deferred};
use crate::models::{ProcessIdentity, ProcessInfoNode};
use crate::windows::process_tree::process_identity_is_current;

const MAX_PENDING: usize = 16;
const MAX_RESULTS: usize = 512;
pub(crate) const PI_RECEIPT_REFRESH: Duration = Duration::from_secs(2);
pub(crate) const SCRIPT_METADATA_REFRESH: Duration = Duration::from_secs(2);
#[cfg(test)]
mod development_tests;
#[cfg(test)]
mod scheduling_tests;
mod script_refresh;
#[cfg(test)]
mod script_tests;
#[cfg(test)]
mod source_check_tests;
mod source_checks;
use script_refresh::Snapshot as ScriptSnapshot;

struct Request {
    generation: u64,
    revision: u64,
    identity: ProcessIdentity,
    executable_identity: Option<(u64, u64)>,
    executable_snapshot: ExecutableSnapshot,
    target: ProcessVersionTarget,
    include_packages: bool,
    cwd: Option<PathBuf>,
    script_refresh: Option<ScriptSnapshot>,
}

enum PackageLookup {
    System(super::PackageTarget),
    Npm(super::NpmTarget),
}

impl PackageLookup {
    fn set_result(
        &self,
        version: &mut ProcessVersion,
        application: bool,
        result: super::AvailablePackageVersion,
    ) {
        match (self, application) {
            (Self::System(_), false) => version.latest_package_version = Some(result),
            (Self::System(_), true) => version.application_latest_package_version = Some(result),
            (Self::Npm(_), false) => version.latest_npm_version = Some(result.text),
            (Self::Npm(_), true) => version.application_latest_npm_version = Some(result.text),
        }
    }
}

struct PackageRequest {
    generation: u64,
    revision: u64,
    identity: ProcessIdentity,
    target: PackageLookup,
    application: bool,
    descriptor: Option<program_metadata::interpreted::script::DescriptorSource>,
}

struct ReceiptRequest {
    generation: u64,
    revision: u64,
    identity: ProcessIdentity,
    executable_identity: Option<(u64, u64)>,
    application: super::NodeApplication,
    development: Option<program_metadata::development::Metadata>,
    program: String,
    desktop_entry: Option<crate::models::AppInfo>,
}

#[derive(Clone)]
struct ExecutableSnapshot {
    identity: Option<(u64, u64)>,
    path: Option<PathBuf>,
    raw_argv0: Option<PathBuf>,
    executable_error: Option<&'static str>,
}

impl ExecutableSnapshot {
    fn of(node: &ProcessInfoNode) -> Self {
        Self {
            identity: node.executable_identity,
            path: node.executable.clone(),
            raw_argv0: node.raw_argv0.clone(),
            executable_error: node.executable_error,
        }
    }

    fn matches(&self, node: &ProcessInfoNode) -> bool {
        self.identity == node.executable_identity
            && self.path == node.executable
            && self.raw_argv0 == node.raw_argv0
            && self.executable_error == node.executable_error
    }
}

#[derive(Default)]
struct State {
    generation: u64,
    next_revision: u64,
    active_revisions: HashMap<ProcessIdentity, u64>,
    metadata_checked: HashMap<ProcessIdentity, (Instant, ExecutableSnapshot)>,
    stopped: bool,
    pending: VecDeque<Request>,
    packages: VecDeque<PackageRequest>,
    receipts: VecDeque<ReceiptRequest>,
    sources: VecDeque<source_checks::Request>,
    receipt_pending: HashSet<ProcessIdentity>,
    receipt_checked: HashMap<ProcessIdentity, Instant>,
    script_checked: HashMap<ProcessIdentity, Instant>,
    script_snapshots: HashMap<ProcessIdentity, ScriptSnapshot>,
    script_pending: HashSet<ProcessIdentity>,
    results: HashMap<ProcessIdentity, Option<Arc<ProcessVersion>>>,
    detailed: HashSet<ProcessIdentity>,
    detail_pending: HashSet<ProcessIdentity>,
}

impl State {
    fn invalidate(&mut self, identity: ProcessIdentity) {
        self.results.remove(&identity);
        self.detailed.remove(&identity);
        self.detail_pending.remove(&identity);
        self.metadata_checked.remove(&identity);
        self.active_revisions.remove(&identity);
        self.pending.retain(|request| request.identity != identity);
        self.receipts.retain(|request| request.identity != identity);
        self.receipt_pending.remove(&identity);
        self.receipt_checked.remove(&identity);
        self.script_checked.remove(&identity);
        self.script_snapshots.remove(&identity);
        self.script_pending.remove(&identity);
        self.packages.retain(|request| request.identity != identity);
        self.sources.retain(|request| request.identity != identity);
    }
}

pub(crate) struct ProcessVersionCache {
    shared: Arc<(Mutex<State>, Condvar)>,
}

impl ProcessVersionCache {
    #[cfg(test)]
    pub(crate) fn with_test_probe(
        ctx: egui::Context,
        viewport: egui::ViewportId,
        probe: impl Fn(ProcessIdentity) -> ProcessVersion + Send + 'static,
    ) -> Self {
        Self::start_with_packages(
            ctx,
            viewport,
            move |request| probe(request.identity),
            |_| None,
        )
    }

    pub(crate) fn new(ctx: egui::Context, viewport_id: egui::ViewportId) -> Self {
        Self::start(ctx, viewport_id, |request| {
            let current =
                || process_identity_is_current(request.identity, request.executable_identity);
            if request.include_packages {
                probe_version_deferred(&request.target, current)
            } else {
                probe_header_checked(&request.target, current)
            }
        })
    }

    fn start(
        ctx: egui::Context,
        viewport_id: egui::ViewportId,
        probe: impl Fn(&Request) -> ProcessVersion + Send + 'static,
    ) -> Self {
        Self::start_with_packages(ctx, viewport_id, probe, |request| {
            // A repository version belongs to a package, not the lifetime of its process.
            Some(match &request.target {
                PackageLookup::System(target) => super::available::query(target),
                PackageLookup::Npm(target) => super::npm::query(target).into(),
            })
        })
    }

    fn start_with_packages(
        ctx: egui::Context,
        viewport_id: egui::ViewportId,
        probe: impl Fn(&Request) -> ProcessVersion + Send + 'static,
        package_probe: impl Fn(&PackageRequest) -> Option<super::AvailablePackageVersion>
        + Send
        + 'static,
    ) -> Self {
        Self::start_with_receipts(ctx, viewport_id, probe, package_probe, |request| {
            if !process_identity_is_current(request.identity, request.executable_identity) {
                return (
                    request.application.clone(),
                    super::PiBuildEvidence::default(),
                );
            }
            let application = super::node::probe(request.identity.pid, &request.program)
                .filter(|application| application.package == super::pi::PACKAGE);
            let evidence = application
                .as_ref()
                .map(|application| super::pi::probe(application, request.identity.pid))
                .unwrap_or_default();
            if process_identity_is_current(request.identity, request.executable_identity) {
                (
                    application.unwrap_or_else(|| request.application.clone()),
                    evidence,
                )
            } else {
                (
                    request.application.clone(),
                    super::PiBuildEvidence::default(),
                )
            }
        })
    }

    fn start_with_receipts(
        ctx: egui::Context,
        viewport_id: egui::ViewportId,
        probe: impl Fn(&Request) -> ProcessVersion + Send + 'static,
        package_probe: impl Fn(&PackageRequest) -> Option<super::AvailablePackageVersion>
        + Send
        + 'static,
        receipt_probe: impl Fn(&ReceiptRequest) -> (super::NodeApplication, super::PiBuildEvidence)
        + Send
        + 'static,
    ) -> Self {
        Self::start_with_source_checks(
            ctx,
            viewport_id,
            probe,
            package_probe,
            receipt_probe,
            source_checks::inspect,
        )
    }

    fn start_with_source_checks(
        ctx: egui::Context,
        viewport_id: egui::ViewportId,
        probe: impl Fn(&Request) -> ProcessVersion + Send + 'static,
        package_probe: impl Fn(&PackageRequest) -> Option<super::AvailablePackageVersion>
        + Send
        + 'static,
        receipt_probe: impl Fn(&ReceiptRequest) -> (super::NodeApplication, super::PiBuildEvidence)
        + Send
        + 'static,
        source_probe: impl Fn(&source_checks::Request) -> String + Send + 'static,
    ) -> Self {
        let shared: Arc<(Mutex<State>, Condvar)> = Arc::default();
        source_checks::start(Arc::clone(&shared), ctx.clone(), viewport_id, source_probe);
        let worker_shared = Arc::clone(&shared);
        let package_shared = Arc::clone(&shared);
        let receipt_shared = Arc::clone(&shared);
        let package_ctx = ctx.clone();
        let receipt_ctx = ctx.clone();
        applicationlauncher::spawn_worker!("process-info-pi-receipts", move |worker| {
            loop {
                applicationlauncher::observe! { worker.set_state("waiting-for-expanded-pi-section"); }
                let request = {
                    let mut state = receipt_shared
                        .0
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    while !state.stopped && state.receipts.is_empty() {
                        state = receipt_shared
                            .1
                            .wait(state)
                            .unwrap_or_else(|error| error.into_inner());
                    }
                    if state.stopped {
                        break;
                    }
                    state.receipts.pop_front().unwrap()
                };
                applicationlauncher::observe! { worker.set_state("reading-pi-build-receipt"); }
                let (application, evidence) = receipt_probe(&request);
                let updated_launch = (application.entry_point != request.application.entry_point)
                    .then(|| {
                        super::application::process_launch(
                            &application.entry_point,
                            super::installation::installed_executable(&application.entry_point)
                                .as_ref(),
                            request.desktop_entry.as_ref(),
                        )
                    });
                // Lightweight refresh must not rescan source/build metadata.
                // Keep captured facts only while their exact entry artifact is
                // unchanged; validate off-lock before publishing this reply.
                let development_current = updated_launch.is_none()
                    && request.development.as_ref().is_none_or(|metadata| {
                        super::development::artifact_unchanged(&application.entry_point, metadata)
                    });
                let publish = {
                    let mut state = receipt_shared
                        .0
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if state.stopped {
                        break;
                    }
                    if state.generation != request.generation
                        || state.active_revisions.get(&request.identity) != Some(&request.revision)
                    {
                        continue;
                    }
                    state.receipt_pending.remove(&request.identity);
                    if let Some(Some(version)) = state.results.get_mut(&request.identity) {
                        let version = Arc::make_mut(version);
                        version.application = Some(application);
                        version.application_build = Some(evidence);
                        if !development_current {
                            version.application_development = None;
                        }
                        if let Some(launch) = updated_launch {
                            version.application_launch = Some(launch);
                        }
                        true
                    } else {
                        false
                    }
                };
                if publish {
                    receipt_ctx.request_repaint_of(egui::ViewportId::ROOT);
                    receipt_ctx.request_repaint_of(viewport_id);
                }
            }
        },);
        applicationlauncher::spawn_worker!("process-info-package-versions", move |worker| {
            // One bounded lookup per expanded process; no timer or idle polling.
            loop {
                applicationlauncher::observe! { worker.set_state("waiting-for-package-request"); }
                let request = {
                    let (mutex, wake) = &*package_shared;
                    let mut state = mutex.lock().unwrap_or_else(|error| error.into_inner());
                    while !state.stopped && state.packages.is_empty() {
                        state = wake.wait(state).unwrap_or_else(|error| error.into_inner());
                    }
                    if state.stopped {
                        break;
                    }
                    state.packages.pop_front().unwrap()
                };
                applicationlauncher::observe! { worker.set_state(match &request.target {
                    PackageLookup::System(_) => "querying-pacman-or-aur",
                    PackageLookup::Npm(_) => "querying-npm-registry",
                }); }
                let result = package_probe(&request)
                    .unwrap_or_else(|| "Unavailable (package lookup failed)".into());
                // The app's fd can close or be reused during a registry query.
                // Validate outside the state lock; never restore such an app's
                // package field after its script metadata has been discarded.
                if request
                    .descriptor
                    .as_ref()
                    .is_some_and(|source| !source.unchanged())
                {
                    continue;
                }
                let publish = {
                    let mut state = package_shared
                        .0
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if state.stopped {
                        break;
                    }
                    if state.generation != request.generation
                        || state.active_revisions.get(&request.identity) != Some(&request.revision)
                    {
                        continue;
                    }
                    if let Some(Some(version)) = state.results.get_mut(&request.identity) {
                        let version = Arc::make_mut(version);
                        request
                            .target
                            .set_result(version, request.application, result);
                        true
                    } else {
                        false
                    }
                };
                if publish {
                    package_ctx.request_repaint_of(egui::ViewportId::ROOT);
                    package_ctx.request_repaint_of(viewport_id);
                }
            }
        },);
        applicationlauncher::spawn_worker!("process-info-versions", move |worker| {
            // Header checks and expanded metadata share a bounded worker; idle queues sleep.
            loop {
                applicationlauncher::observe! { worker.set_state("waiting-for-process-info-request"); }
                let mut request = {
                    let (mutex, wake) = &*worker_shared;
                    let mut state = mutex.lock().unwrap_or_else(|error| error.into_inner());
                    while !state.stopped && state.pending.is_empty() {
                        state = wake.wait(state).unwrap_or_else(|error| error.into_inner());
                    }
                    if state.stopped {
                        break;
                    }
                    // Visible details must not wait for every collapsed ancestor.
                    let next = state
                        .pending
                        .iter()
                        .position(|request| {
                            request.include_packages && request.script_refresh.is_none()
                        })
                        .unwrap_or(0);
                    state.pending.remove(next).unwrap()
                };
                if let Some(previous) = &request.script_refresh {
                    let unchanged = previous.current() == *previous;
                    let mut state = worker_shared
                        .0
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if state.stopped {
                        break;
                    }
                    if state.generation != request.generation
                        || state.active_revisions.get(&request.identity) != Some(&request.revision)
                    {
                        continue;
                    }
                    if unchanged {
                        state.script_pending.remove(&request.identity);
                        state
                            .script_checked
                            .insert(request.identity, Instant::now());
                        continue;
                    }
                    // Only a real change invalidates older metadata/package
                    // replies. Unchanged fingerprints never reprobe runtimes.
                    state.next_revision = state.next_revision.wrapping_add(1);
                    request.revision = state.next_revision;
                    state
                        .active_revisions
                        .insert(request.identity, request.revision);
                }
                applicationlauncher::observe! { worker.set_state("probing-process-info"); }
                let mut version = probe(&request);
                version.details_pending = !request.include_packages;
                let mut script_snapshot =
                    ScriptSnapshot::for_version(&version, request.cwd.as_deref());
                version.discard_changed_python_modules();
                if version.discard_changed_script_metadata() {
                    script_snapshot = None;
                }
                let publish = {
                    let mut state = worker_shared
                        .0
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if state.stopped {
                        break;
                    }
                    if state.generation == request.generation
                        && state.active_revisions.get(&request.identity) == Some(&request.revision)
                    {
                        state.script_pending.remove(&request.identity);
                        state
                            .script_checked
                            .insert(request.identity, Instant::now());
                        if let Some(snapshot) = script_snapshot {
                            state.script_snapshots.insert(request.identity, snapshot);
                        } else {
                            state.script_snapshots.remove(&request.identity);
                        }
                        if version
                            .application
                            .as_ref()
                            .is_some_and(|application| application.package == super::pi::PACKAGE)
                        {
                            state
                                .receipt_checked
                                .insert(request.identity, Instant::now());
                        }
                        if request.include_packages {
                            state.detailed.insert(request.identity);
                            state.detail_pending.remove(&request.identity);
                            source_checks::enqueue(&mut state, &request, &mut version);
                            for (target, application) in [
                                (
                                    version.package_target.clone().map(PackageLookup::System),
                                    false,
                                ),
                                (
                                    version
                                        .application_package_target
                                        .clone()
                                        .map(PackageLookup::System),
                                    true,
                                ),
                                (version.npm_target.clone().map(PackageLookup::Npm), false),
                                (
                                    version
                                        .application_npm_target
                                        .clone()
                                        .map(PackageLookup::Npm),
                                    true,
                                ),
                            ] {
                                if let Some(target) = target {
                                    if state.packages.len() < MAX_PENDING {
                                        state.packages.push_back(PackageRequest {
                                            generation: request.generation,
                                            revision: request.revision,
                                            identity: request.identity,
                                            target,
                                            application,
                                            descriptor: application
                                                .then(|| {
                                                    version
                                                        .script_application
                                                        .as_ref()?
                                                        .descriptor
                                                        .clone()
                                                })
                                                .flatten(),
                                        });
                                    } else {
                                        let result = "Unavailable (package lookup queue full; close and reopen Show Info to retry)".into();
                                        target.set_result(&mut version, application, result);
                                    }
                                }
                            }
                        }
                        state
                            .results
                            .insert(request.identity, Some(Arc::new(version)));
                        state.metadata_checked.insert(
                            request.identity,
                            (Instant::now(), request.executable_snapshot.clone()),
                        );
                        worker_shared.1.notify_all();
                        true
                    } else {
                        false
                    }
                };
                if publish {
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                    ctx.request_repaint_of(viewport_id);
                }
            }
        });
        Self { shared }
    }

    pub(crate) fn invalidate(&self, identity: ProcessIdentity) {
        self.shared
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .invalidate(identity);
    }

    pub(crate) fn reset(&self) {
        let mut state = self
            .shared
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.generation = state.generation.wrapping_add(1);
        state.pending.clear();
        state.packages.clear();
        state.sources.clear();
        state.receipts.clear();
        state.receipt_pending.clear();
        state.receipt_checked.clear();
        state.script_checked.clear();
        state.script_snapshots.clear();
        state.script_pending.clear();
        state.results.clear();
        state.active_revisions.clear();
        state.metadata_checked.clear();
        state.detailed.clear();
        state.detail_pending.clear();
    }

    #[cfg(test)]
    pub(crate) fn request(&self, node: &ProcessInfoNode) -> Option<Arc<ProcessVersion>> {
        self.request_expanded(node, true)
    }

    #[cfg(test)]
    pub(crate) fn request_expanded(
        &self,
        node: &ProcessInfoNode,
        refresh_receipt: bool,
    ) -> Option<Arc<ProcessVersion>> {
        self.request_for_application(node, refresh_receipt, || None)
    }

    pub(crate) fn request_for_application<'a>(
        &self,
        node: &ProcessInfoNode,
        refresh_receipt: bool,
        application: impl FnOnce() -> Option<(&'a crate::models::AppInfo, bool)>,
    ) -> Option<Arc<ProcessVersion>> {
        self.request_scoped(node, refresh_receipt, true, application)
    }

    #[cfg(test)]
    pub(crate) fn request_for_header(&self, node: &ProcessInfoNode) -> Option<Arc<ProcessVersion>> {
        self.request_header_for_application(node, || None)
    }

    pub(crate) fn request_header_for_application<'a>(
        &self,
        node: &ProcessInfoNode,
        application: impl FnOnce() -> Option<(&'a crate::models::AppInfo, bool)>,
    ) -> Option<Arc<ProcessVersion>> {
        self.request_scoped(node, true, false, application)
    }

    fn request_scoped<'a>(
        &self,
        node: &ProcessInfoNode,
        refresh_receipt: bool,
        include_packages: bool,
        application: impl FnOnce() -> Option<(&'a crate::models::AppInfo, bool)>,
    ) -> Option<Arc<ProcessVersion>> {
        let mut state = self
            .shared
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state
            .metadata_checked
            .get(&node.identity)
            .is_some_and(|(_, executable)| !executable.matches(node))
        {
            state.invalidate(node.identity);
        }
        let cached = state.results.get(&node.identity).cloned().flatten();
        let upgrade_header = include_packages
            && !state.detailed.contains(&node.identity)
            && !state.detail_pending.contains(&node.identity)
            && cached.is_some();
        if include_packages && !state.detailed.contains(&node.identity) {
            if let Some(request) = state
                .pending
                .iter_mut()
                .find(|request| request.identity == node.identity && !request.include_packages)
            {
                let application = application();
                request.include_packages = true;
                request.target.application_entry = application.map(|(entry, _)| entry.clone());
                request.target.window_declared_entry =
                    application.is_some_and(|(_, declared)| declared);
                state.detail_pending.insert(node.identity);
                return cached;
            }
        }
        if let Some(result) = state
            .results
            .get(&node.identity)
            .cloned()
            .filter(|_| !upgrade_header)
        {
            if refresh_receipt
                && include_packages
                && result.is_some()
                && state.pending.len() < MAX_PENDING
                && !state.script_pending.contains(&node.identity)
                && state
                    .script_checked
                    .get(&node.identity)
                    .is_none_or(|checked| checked.elapsed() >= SCRIPT_METADATA_REFRESH)
                && let Some(snapshot) = state.script_snapshots.get(&node.identity).cloned()
            {
                let application = application();
                let generation = state.generation;
                let revision = state
                    .active_revisions
                    .get(&node.identity)
                    .copied()
                    .unwrap_or_default();
                state.script_pending.insert(node.identity);
                state.script_checked.insert(node.identity, Instant::now());
                state.pending.push_back(Request {
                    generation,
                    revision,
                    identity: node.identity,
                    executable_identity: node.executable_identity,
                    executable_snapshot: ExecutableSnapshot::of(node),
                    include_packages: true,
                    cwd: node.cwd.clone(),
                    script_refresh: Some(snapshot),
                    target: ProcessVersionTarget {
                        pid: node.identity.pid,
                        executable: node
                            .executable
                            .clone()
                            .or_else(|| node.raw_argv0.clone())
                            .unwrap_or_default(),
                        launch_context: super::ProcessLaunchContext {
                            raw_argv0: node.raw_argv0.clone(),
                            executable_unavailable: node.executable.is_none().then_some(
                                node.executable_error
                                    .unwrap_or("process executable unavailable"),
                            ),
                        },
                        program: Some(node.name.clone()),
                        application_entry: application.map(|(entry, _)| entry.clone()),
                        window_declared_entry: application.is_some_and(|(_, declared)| declared),
                    },
                });
                self.shared.1.notify_all();
                return result;
            }
            if refresh_receipt
                && let Some(application) = result
                    .as_ref()
                    .and_then(|version| version.application.as_ref())
                    .filter(|application| application.package == super::pi::PACKAGE)
                && !state.receipt_pending.contains(&node.identity)
                && state.receipts.len() < MAX_PENDING
                && state
                    .receipt_checked
                    .get(&node.identity)
                    .is_none_or(|checked| checked.elapsed() >= PI_RECEIPT_REFRESH)
            {
                let generation = state.generation;
                let revision = state
                    .active_revisions
                    .get(&node.identity)
                    .copied()
                    .unwrap_or_default();
                state.receipts.push_back(ReceiptRequest {
                    generation,
                    revision,
                    identity: node.identity,
                    executable_identity: node.executable_identity,
                    application: application.clone(),
                    development: result
                        .as_ref()
                        .and_then(|version| version.application_development.clone()),
                    program: node.name.clone(),
                    desktop_entry: result
                        .as_ref()
                        .and_then(|version| version.application_launch.as_ref())
                        .and_then(|launch| launch.desktop_entry.clone()),
                });
                state.receipt_pending.insert(node.identity);
                state.receipt_checked.insert(node.identity, Instant::now());
                self.shared.1.notify_all();
            }
            return result;
        }
        let Some(executable) = node
            .executable
            .as_ref()
            .or_else(|| node.raw_argv0.as_ref().filter(|path| path.is_absolute()))
        else {
            return Some(Arc::new(ProcessVersion {
                running_unavailable: node
                    .executable_error
                    .or(Some("process executable unavailable")),
                ..Default::default()
            }));
        };
        if (state.results.len() >= MAX_RESULTS && cached.is_none())
            || state.pending.len() >= MAX_PENDING
        {
            return cached;
        }
        let generation = state.generation;
        state.next_revision = state.next_revision.wrapping_add(1);
        let revision = state.next_revision;
        state.active_revisions.insert(node.identity, revision);
        // Record queued/in-flight snapshots as well as completed probes. A
        // renamed or unlinked executable retains its inode, but its observed
        // path changes and must retire all results from the previous snapshot.
        state.metadata_checked.insert(
            node.identity,
            (Instant::now(), ExecutableSnapshot::of(node)),
        );
        let application = application();
        if include_packages {
            state.detail_pending.insert(node.identity);
        }
        state.pending.push_back(Request {
            generation,
            revision,
            identity: node.identity,
            executable_identity: node.executable_identity,
            executable_snapshot: ExecutableSnapshot::of(node),
            include_packages,
            cwd: node.cwd.clone(),
            script_refresh: None,
            target: ProcessVersionTarget {
                pid: node.identity.pid,
                executable: executable.clone(),
                launch_context: super::ProcessLaunchContext {
                    raw_argv0: node.raw_argv0.clone(),
                    executable_unavailable: node.executable.is_none().then_some(
                        node.executable_error
                            .unwrap_or("process executable unavailable"),
                    ),
                },
                program: Some(node.name.clone()),
                application_entry: application.map(|(entry, _)| entry.clone()),
                window_declared_entry: application.is_some_and(|(_, declared)| declared),
            },
        });
        // Retain cheap header evidence (notably current-build highlighting)
        // while the expanded metadata request is queued or in flight.
        state.results.entry(node.identity).or_insert(None);
        self.shared.1.notify_all();
        cached
    }
}

impl Drop for ProcessVersionCache {
    fn drop(&mut self) {
        let mut state = self
            .shared
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.stopped = true;
        state.pending.clear();
        state.packages.clear();
        state.sources.clear();
        state.receipts.clear();
        state.receipt_pending.clear();
        self.shared.1.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    pub(super) fn node(pid: i32) -> ProcessInfoNode {
        ProcessInfoNode {
            identity: ProcessIdentity {
                pid,
                start_ticks: pid as u64,
            },
            parent_pid: 0,
            depth: 0,
            name: "test".into(),
            script_name: None,
            header: "test".into(),
            executable: Some("/not-executed/test".into()),
            executable_error: None,
            executable_identity: None,
            cwd: None,
            cwd_error: None,
            raw_argv0: None,
            command_line: None,
        }
    }

    #[test]
    fn stale_process_identity_keeps_path_metadata_without_using_the_replacement_process() {
        let mut captured = node(std::process::id() as i32);
        captured.identity.start_ticks = u64::MAX;
        captured.executable = Some(std::env::current_exe().unwrap());
        let cache = ProcessVersionCache::new(egui::Context::default(), egui::ViewportId::ROOT);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = cache.request(&captured) {
                assert!(
                    result.installation.is_some(),
                    "identity guard discarded path metadata"
                );
                assert!(result.version.is_none());
                assert!(result.build_id.is_none());
                assert!(result.running_latest_binary.is_none());
                break;
            }
            assert!(Instant::now() < deadline, "path metadata did not complete");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn explicit_process_refresh_discards_in_flight_results_but_retains_other_cached_processes() {
        let probes = Arc::new(AtomicUsize::new(0));
        let count = probes.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let cache = ProcessVersionCache::with_test_probe(
            egui::Context::default(),
            egui::ViewportId::ROOT,
            move |identity| {
                let value = if identity.pid == 1 {
                    let revision = count.fetch_add(1, Ordering::SeqCst);
                    if revision == 0 {
                        started_tx.send(()).unwrap();
                        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    }
                    format!("updated process revision {revision}")
                } else {
                    "unaffected process".into()
                };
                ProcessVersion {
                    version: Some(value),
                    ..Default::default()
                }
            },
        );
        let wait = |node: &ProcessInfoNode| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(result) = cache.request(node) {
                    break result;
                }
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
        };
        let unchanged = wait(&node(2));
        assert!(cache.request(&node(1)).is_none());
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        cache.invalidate(node(1).identity);
        assert!(Arc::ptr_eq(&unchanged, &cache.request(&node(2)).unwrap()));
        assert!(cache.request(&node(1)).is_none());
        release_tx.send(()).unwrap();
        assert_eq!(
            wait(&node(1)).version.as_deref(),
            Some("updated process revision 1")
        );
        assert_eq!(probes.load(Ordering::SeqCst), 2);
        assert!(Arc::ptr_eq(&unchanged, &cache.request(&node(2)).unwrap()));
    }

    #[test]
    fn completed_metadata_survives_elapsed_time_and_refreshes_only_for_identity_changes() {
        let probes = Arc::new(AtomicUsize::new(0));
        let count = probes.clone();
        let cache = ProcessVersionCache::start(
            egui::Context::default(),
            egui::ViewportId::ROOT,
            move |_| ProcessVersion {
                version: Some(format!("revision {}", count.fetch_add(1, Ordering::SeqCst))),
                ..Default::default()
            },
        );
        let mut process = node(42);
        process.executable_identity = Some((1, 1));
        let wait = |process: &ProcessInfoNode| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(result) = cache.request(process) {
                    break result;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        assert_eq!(wait(&process).version.as_deref(), Some("revision 0"));
        assert_eq!(wait(&process).version.as_deref(), Some("revision 0"));
        cache
            .shared
            .0
            .lock()
            .unwrap()
            .metadata_checked
            .get_mut(&process.identity)
            .unwrap()
            .0 = Instant::now() - Duration::from_secs(3600);
        for _ in 0..100 {
            assert_eq!(
                cache.request(&process).unwrap().version.as_deref(),
                Some("revision 0")
            );
        }
        assert_eq!(probes.load(Ordering::SeqCst), 1);
        process.executable_identity = Some((1, 2));
        assert_eq!(wait(&process).version.as_deref(), Some("revision 1"));
        assert_eq!(probes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn completed_metadata_refreshes_for_renamed_or_deleted_path_with_same_running_inode() {
        let probes = Arc::new(AtomicUsize::new(0));
        let count = probes.clone();
        let cache = ProcessVersionCache::start(
            egui::Context::default(),
            egui::ViewportId::ROOT,
            move |request| {
                count.fetch_add(1, Ordering::SeqCst);
                ProcessVersion {
                    version: Some(request.target.executable.display().to_string()),
                    ..Default::default()
                }
            },
        );
        let mut process = node(42);
        process.executable_identity = Some((1, 1));
        for path in [
            "/fixture/program",
            "/fixture/retained-old-image",
            "/fixture/retained-old-image (deleted)",
        ] {
            process.executable = Some(path.into());
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(result) = cache.request(&process) {
                    assert_eq!(result.version.as_deref(), Some(path));
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        for _ in 0..100 {
            assert!(cache.request(&process).is_some());
        }
        assert_eq!(probes.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn renamed_running_path_cancels_queued_requests_and_rejects_in_flight_results() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let cache = ProcessVersionCache::start(
            egui::Context::default(),
            egui::ViewportId::ROOT,
            move |request| {
                started_tx.send(request.target.executable.clone()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                ProcessVersion {
                    version: Some(request.target.executable.display().to_string()),
                    ..Default::default()
                }
            },
        );
        let mut process = node(42);
        process.executable_identity = Some((1, 1));
        process.executable = Some("/fixture/program".into());
        assert!(cache.request(&process).is_none());
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            PathBuf::from("/fixture/program")
        );
        process.executable = Some("/fixture/retained-image".into());
        assert!(cache.request(&process).is_none());
        process.executable = Some("/fixture/retained-image (deleted)".into());
        assert!(cache.request(&process).is_none());
        {
            let state = cache.shared.0.lock().unwrap();
            assert_eq!(state.pending.len(), 1);
            assert_eq!(
                state.pending[0].target.executable,
                process.executable.clone().unwrap()
            );
        }
        release_tx.send(()).unwrap();
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            process.executable.clone().unwrap()
        );
        assert!(
            cache.request(&process).is_none(),
            "obsolete in-flight result was published"
        );
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = cache.request(&process) {
                assert_eq!(
                    result.version.as_deref(),
                    Some("/fixture/retained-image (deleted)")
                );
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn lazy_version_requests_are_deduplicated_bounded_and_cancelled_on_reopen() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let probes = Arc::new(AtomicUsize::new(0));
        let probe_count = probes.clone();
        let cache = ProcessVersionCache::start(
            egui::Context::default(),
            egui::ViewportId::ROOT,
            move |request| {
                probe_count.fetch_add(1, Ordering::SeqCst);
                started_tx.send(request.identity.pid).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                ProcessVersion {
                    version: Some(format!("version {}", request.identity.pid)),
                    ..Default::default()
                }
            },
        );
        assert_eq!(probes.load(Ordering::SeqCst), 0);
        assert!(cache.request(&node(1)).is_none());
        assert_eq!(started_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
        for _ in 0..20 {
            assert!(cache.request(&node(1)).is_none());
        }
        for pid in 2..MAX_PENDING as i32 + 10 {
            cache.request(&node(pid));
        }
        {
            let state = cache.shared.0.lock().unwrap();
            assert_eq!(state.pending.len(), MAX_PENDING);
            assert_eq!(state.results.len(), MAX_PENDING + 1);
        }
        cache.reset();
        assert!(cache.shared.0.lock().unwrap().results.is_empty());
        release_tx.send(()).unwrap();
        assert!(cache.request(&node(100)).is_none());
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            100
        );
        assert_eq!(probes.load(Ordering::SeqCst), 2);
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = cache.request(&node(100)) {
                assert_eq!(result.version.as_deref(), Some("version 100"));
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            !cache
                .shared
                .0
                .lock()
                .unwrap()
                .results
                .contains_key(&node(1).identity)
        );
    }

    #[test]
    fn unreadable_executables_do_not_start_version_probes() {
        let cache =
            ProcessVersionCache::start(egui::Context::default(), egui::ViewportId::ROOT, |_| {
                panic!("no probe expected")
            });
        let mut missing = node(1);
        missing.executable = None;
        assert!(cache.request(&missing).unwrap().version.is_none());
        assert!(cache.shared.0.lock().unwrap().pending.is_empty());
    }

    #[test]
    fn absolute_raw_argv0_candidates_are_background_bounded_and_never_use_display_text() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let cache = ProcessVersionCache::start(
            egui::Context::default(),
            egui::ViewportId::ROOT,
            move |request| {
                started_tx.send(request.target.clone()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                ProcessVersion {
                    running_unavailable: request.target.launch_context.executable_unavailable,
                    on_disk_candidate: Some(request.target.executable.clone()),
                    ..Default::default()
                }
            },
        );
        let mut missing = node(1);
        missing.executable = None;
        missing.executable_error = Some("Permission denied reading process executable");
        missing.command_line = Some("/untrusted/display path --user".into());
        assert!(cache.request(&missing).unwrap().on_disk_candidate.is_none());
        missing.raw_argv0 = Some("relative-command".into());
        assert!(cache.request(&missing).unwrap().on_disk_candidate.is_none());
        missing.raw_argv0 = Some("/exact argv0/with spaces".into());
        let started = Instant::now();
        assert!(cache.request(&missing).is_none());
        assert!(started.elapsed() < Duration::from_millis(100));
        let request = started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            request.executable,
            PathBuf::from("/exact argv0/with spaces")
        );
        assert_eq!(
            request.launch_context.executable_unavailable,
            missing.executable_error
        );
        for _ in 0..100 {
            assert!(cache.request(&missing).is_none());
        }
        assert!(cache.shared.0.lock().unwrap().pending.is_empty());
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = cache.request(&missing) {
                assert!(result.version.is_none() && result.build_id.is_none());
                assert!(result.running_latest_binary.is_none());
                assert_eq!(result.on_disk_candidate, missing.raw_argv0);
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[test]
    fn unreadable_executable_keeps_candidate_and_package_metadata_without_timed_refresh() {
        let probes = Arc::new(AtomicUsize::new(0));
        let count = probes.clone();
        let packages = Arc::new(AtomicUsize::new(0));
        let package_count = packages.clone();
        let cache = ProcessVersionCache::start_with_packages(
            egui::Context::default(),
            egui::ViewportId::ROOT,
            move |_| ProcessVersion {
                latest_version: Some(format!("revision {}", count.fetch_add(1, Ordering::SeqCst))),
                package_target: Some(super::super::PackageTarget {
                    name: "systemd".into(),
                    inferred: false,
                }),
                ..Default::default()
            },
            move |_| {
                package_count.fetch_add(1, Ordering::SeqCst);
                Some(crate::windows::versions::available_package_fixture(
                    super::super::PackageVersionSource::Pacman,
                    "core/systemd 262-1",
                ))
            },
        );
        let mut missing = node(1);
        missing.executable = None;
        missing.executable_error = Some("Permission denied reading process executable");
        missing.raw_argv0 = Some("/candidate/not-statted-by-renderer".into());
        let wait = |process: &ProcessInfoNode| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(result) = cache.request(process)
                    && result.latest_package_version.is_some()
                {
                    break result;
                }
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
        };
        assert_eq!(wait(&missing).latest_version.as_deref(), Some("revision 0"));
        for age in [10, 20, 3600] {
            cache
                .shared
                .0
                .lock()
                .unwrap()
                .metadata_checked
                .get_mut(&missing.identity)
                .unwrap()
                .0 = Instant::now() - Duration::from_secs(age);
            for _ in 0..100 {
                let result = cache
                    .request(&missing)
                    .expect("cached info must not reload");
                assert_eq!(result.latest_version.as_deref(), Some("revision 0"));
                assert_eq!(
                    result.latest_package_version.as_ref().unwrap().text,
                    "core/systemd 262-1"
                );
            }
        }
        assert_eq!(probes.load(Ordering::SeqCst), 1);
        assert_eq!(packages.load(Ordering::SeqCst), 1);
        missing.raw_argv0 = Some("/different/candidate".into());
        assert_eq!(wait(&missing).latest_version.as_deref(), Some("revision 1"));
        assert_eq!(probes.load(Ordering::SeqCst), 2);
        assert_eq!(packages.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn expanded_pi_receipts_refresh_without_reprobing_node_or_packages() {
        let versions = Arc::new(AtomicUsize::new(0));
        let version_count = versions.clone();
        let receipts = Arc::new(AtomicUsize::new(0));
        let receipt_count = receipts.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let cache = ProcessVersionCache::start_with_receipts(
            egui::Context::default(),
            egui::ViewportId::ROOT,
            move |_| {
                version_count.fetch_add(1, Ordering::SeqCst);
                ProcessVersion {
                    application: Some(super::super::NodeApplication {
                        package: super::super::pi::PACKAGE.into(),
                        installed_version: "0.84.2".into(),
                        entry_point: "/installed/pi/dist/cli.js".into(),
                        manifest: "/installed/pi/package.json".into(),
                        identification: "test fixture",
                    }),
                    version: Some("Node identity must stay cached".into()),
                    ..Default::default()
                }
            },
            |_| panic!("No package lookup should be queued"),
            move |request| {
                receipt_count.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                (
                    request.application.clone(),
                    super::super::PiBuildEvidence {
                        reason: "Fresh receipt".into(),
                        ..Default::default()
                    },
                )
            },
        );
        let target = node(42);
        let deadline = Instant::now() + Duration::from_secs(5);
        while cache.request(&target).is_none() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(versions.load(Ordering::SeqCst), 1);
        assert_eq!(receipts.load(Ordering::SeqCst), 0);
        {
            let mut state = cache.shared.0.lock().unwrap();
            state
                .receipt_checked
                .insert(target.identity, Instant::now() - PI_RECEIPT_REFRESH);
        }
        // No background timer queues a receipt until the section asks for it again.
        assert!(cache.shared.0.lock().unwrap().receipts.is_empty());
        cache.request_expanded(&target, false);
        assert!(cache.shared.0.lock().unwrap().receipts.is_empty());
        assert!(cache.shared.0.lock().unwrap().receipt_pending.is_empty());
        cache.request(&target);
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        for _ in 0..20 {
            cache.request(&target);
        }
        assert_eq!(receipts.load(Ordering::SeqCst), 1);
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let value = cache.request(&target).unwrap();
            if let Some(build) = &value.application_build {
                assert_eq!(build.reason, "Fresh receipt");
                assert_eq!(
                    value.version.as_deref(),
                    Some("Node identity must stay cached")
                );
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(versions.load(Ordering::SeqCst), 1);
        cache
            .shared
            .0
            .lock()
            .unwrap()
            .receipt_checked
            .insert(target.identity, Instant::now() - PI_RECEIPT_REFRESH);
        cache.request(&target);
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        cache.reset();
        release_tx.send(()).unwrap();
        assert!(cache.shared.0.lock().unwrap().results.is_empty());
    }

    #[test]
    fn closing_or_resetting_one_popup_does_not_clear_another_popups_versions() {
        let ctx = egui::Context::default();
        let caches = ["first", "second"].map(|label| {
            ProcessVersionCache::start(
                ctx.clone(),
                egui::ViewportId::from_hash_of(label),
                move |_| ProcessVersion {
                    version: Some(label.into()),
                    ..Default::default()
                },
            )
        });
        let identity = node(1);
        for (index, cache) in caches.iter().enumerate() {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(version) = cache.request(&identity) {
                    assert_eq!(version.version.as_deref(), Some(["first", "second"][index]));
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
        }
        let [first, second] = caches;
        first.reset();
        drop(first);
        assert_eq!(
            second.request(&identity).unwrap().version.as_deref(),
            Some("second")
        );
    }

    #[test]
    fn child_desktop_candidates_remain_unverified_and_results_stay_per_process() {
        let entry = crate::models::AppInfo {
            name: "Browser".into(),
            exec: "/fixture/google-chrome-fast".into(),
            icon_path: None,
            comment: None,
            desktop_file_path: "/fixture/browser.desktop".into(),
            is_settings_module: false,
        };
        let count = Arc::new(AtomicUsize::new(0));
        let probes = count.clone();
        let cache = ProcessVersionCache::start(
            egui::Context::default(),
            egui::ViewportId::ROOT,
            move |request| {
                assert!(!request.target.window_declared_entry);
                assert_eq!(
                    request.target.application_entry.as_ref().unwrap().exec,
                    "/fixture/google-chrome-fast"
                );
                assert_eq!(request.target.pid, request.identity.pid);
                assert_eq!(
                    request.target.executable,
                    PathBuf::from(format!("/fixture/child-{}", request.identity.pid))
                );
                probes.fetch_add(1, Ordering::SeqCst);
                ProcessVersion {
                    running_latest_binary: match request.identity.pid {
                        1 => Some(true),
                        2 => Some(false),
                        _ => None,
                    },
                    ..Default::default()
                }
            },
        );
        for pid in [1, 2, 3] {
            let mut child = node(pid);
            child.executable = Some(format!("/fixture/child-{pid}").into());
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(version) =
                    cache.request_header_for_application(&child, || Some((&entry, false)))
                {
                    assert_eq!(
                        version.process_build_is_current(),
                        match pid {
                            1 => Some(true),
                            2 => Some(false),
                            _ => None,
                        }
                    );
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
            assert!(
                cache
                    .request_header_for_application(&child, || panic!(
                        "candidate should only be snapshotted once"
                    ))
                    .is_some()
            );
        }
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn cached_process_metadata_does_not_resolve_desktop_candidates_again_each_frame() {
        let cache =
            ProcessVersionCache::start(egui::Context::default(), egui::ViewportId::ROOT, |_| {
                ProcessVersion::default()
            });
        let resolutions = std::cell::Cell::new(0);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let result = cache.request_for_application(&node(1), false, || {
                resolutions.set(resolutions.get() + 1);
                None
            });
            if result.is_some() {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        for _ in 0..100 {
            assert!(
                cache
                    .request_for_application(&node(1), false, || panic!(
                        "cached metadata must not rescan desktop entries"
                    ))
                    .is_some()
            );
        }
        assert_eq!(resolutions.get(), 1);
    }

    #[test]
    fn collapsed_header_checks_are_cached_without_package_lookups_and_expand_on_demand() {
        let entry = crate::models::AppInfo {
            name: "Browser".into(),
            exec: "/fixture/browser-wrapper".into(),
            icon_path: None,
            comment: None,
            desktop_file_path: "/fixture/browser.desktop".into(),
            is_settings_module: false,
        };
        let probes = Arc::new(AtomicUsize::new(0));
        let packages = Arc::new(AtomicUsize::new(0));
        let count = probes.clone();
        let package_count = packages.clone();
        let cache = ProcessVersionCache::start_with_packages(
            egui::Context::default(),
            egui::ViewportId::ROOT,
            move |request| {
                if !request.include_packages {
                    assert_eq!(
                        request
                            .target
                            .application_entry
                            .as_ref()
                            .map(|entry| entry.exec.as_str()),
                        Some("/fixture/browser-wrapper")
                    );
                    assert!(request.target.window_declared_entry);
                }
                count.fetch_add(1, Ordering::SeqCst);
                ProcessVersion {
                    running_latest_binary: Some(true),
                    package_target: Some(super::super::PackageTarget {
                        name: "fixture".into(),
                        inferred: false,
                    }),
                    ..Default::default()
                }
            },
            move |_| {
                package_count.fetch_add(1, Ordering::SeqCst);
                Some("fixture 2.0".to_string().into())
            },
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(version) =
                cache.request_header_for_application(&node(1), || Some((&entry, true)))
            {
                assert_eq!(version.process_build_is_current(), Some(true));
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        for _ in 0..100 {
            assert!(
                cache
                    .request_header_for_application(&node(1), || panic!(
                        "cached headers must not rescan desktop entries"
                    ))
                    .is_some()
            );
        }
        assert_eq!(probes.load(Ordering::SeqCst), 1);
        assert_eq!(packages.load(Ordering::SeqCst), 0);
        assert!(cache.shared.0.lock().unwrap().packages.is_empty());
        loop {
            if let Some(version) = cache.request(&node(1))
                && version.latest_package_version.is_some()
            {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(probes.load(Ordering::SeqCst), 2);
        assert_eq!(packages.load(Ordering::SeqCst), 1);
        assert!(cache.request_for_header(&node(1)).is_some());
        cache.reset();
        assert!(cache.shared.0.lock().unwrap().detailed.is_empty());
    }

    #[test]
    fn expanding_a_queued_header_keeps_its_desktop_entry_and_resolves_it_only_once() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let cache = ProcessVersionCache::start(
            egui::Context::default(),
            egui::ViewportId::ROOT,
            move |request| {
                if request.identity.pid == 1 {
                    started_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                ProcessVersion::default()
            },
        );
        cache.request_for_header(&node(1));
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        cache.request_for_header(&node(2));
        let entry = crate::models::AppInfo {
            name: "Fixture".into(),
            exec: "/fixture/app".into(),
            icon_path: None,
            comment: None,
            desktop_file_path: "/fixture/app.desktop".into(),
            is_settings_module: false,
        };
        let resolutions = std::cell::Cell::new(0);
        for _ in 0..10 {
            assert!(
                cache
                    .request_for_application(&node(2), false, || {
                        resolutions.set(resolutions.get() + 1);
                        Some((&entry, true))
                    })
                    .is_none()
            );
        }
        {
            let state = cache.shared.0.lock().unwrap();
            let request = state
                .pending
                .iter()
                .find(|request| request.identity.pid == 2)
                .unwrap();
            assert!(request.include_packages && request.target.window_declared_entry);
            assert_eq!(request.target.application_entry.as_ref(), Some(&entry));
        }
        assert_eq!(resolutions.get(), 1);
        drop(cache);
        release_tx.send(()).unwrap();
    }

    #[test]
    fn npm_and_system_queries_publish_to_separate_application_and_runtime_fields() {
        let cache = ProcessVersionCache::start_with_packages(
            egui::Context::default(),
            egui::ViewportId::ROOT,
            |_| ProcessVersion {
                version: Some("node running".into()),
                package_target: Some(super::super::PackageTarget {
                    name: "nodejs".into(),
                    inferred: false,
                }),
                application_package_target: Some(super::super::PackageTarget {
                    name: "packaged-script".into(),
                    inferred: false,
                }),
                npm_target: super::super::NpmTarget::package("runtime-fixture"),
                application_npm_target: super::super::NpmTarget::package("@example/app"),
                ..Default::default()
            },
            |request| {
                Some(match &request.target {
                    PackageLookup::System(target) => {
                        crate::windows::versions::available_package_fixture(
                            super::super::PackageVersionSource::Pacman,
                            &format!("system {}", target.name),
                        )
                    }
                    PackageLookup::Npm(target) => format!("npm {}", target.name).into(),
                })
            },
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(version) = cache.request(&node(1))
                && version.application_latest_npm_version.is_some()
            {
                assert_eq!(version.version.as_deref(), Some("node running"));
                assert_eq!(
                    version
                        .latest_package_version
                        .as_ref()
                        .map(|version| version.text.as_str()),
                    Some("system nodejs")
                );
                assert_eq!(
                    version
                        .application_latest_package_version
                        .as_ref()
                        .map(|version| version.text.as_str()),
                    Some("system packaged-script")
                );
                assert_eq!(
                    version.latest_npm_version.as_deref(),
                    Some("npm runtime-fixture")
                );
                assert_eq!(
                    version.application_latest_npm_version.as_deref(),
                    Some("npm @example/app")
                );
                break;
            }
            assert!(Instant::now() < deadline, "npm results did not complete");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn npm_query_does_not_block_native_details_or_survive_popup_reset() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let cache = ProcessVersionCache::start_with_packages(
            egui::Context::default(),
            egui::ViewportId::ROOT,
            |_| ProcessVersion {
                version: Some("running binary".into()),
                npm_target: super::super::NpmTarget::package("tool"),
                ..Default::default()
            },
            move |_| {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Some("obsolete registry result".into())
            },
        );
        cache.request(&node(1));
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let version = cache.request(&node(1)).unwrap();
        assert_eq!(version.version.as_deref(), Some("running binary"));
        assert!(version.latest_npm_version.is_none());
        let shared = cache.shared.clone();
        cache.reset();
        drop(cache);
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Arc::strong_count(&shared) > 1 {
            assert!(
                Instant::now() < deadline,
                "workers did not finish after closing"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(shared.0.lock().unwrap().results.is_empty());
        assert!(version.latest_npm_version.is_none());
    }

    #[test]
    fn slow_package_lookups_do_not_block_binary_metadata_and_discard_reset_results() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let cache = ProcessVersionCache::start_with_packages(
            egui::Context::default(),
            egui::ViewportId::ROOT,
            |request| ProcessVersion {
                version: Some(format!("binary {}", request.identity.pid)),
                package_target: Some(super::super::PackageTarget {
                    name: format!("package-{}", request.identity.pid),
                    inferred: false,
                }),
                ..Default::default()
            },
            move |request| {
                started_tx
                    .send((request.identity.pid, request.generation))
                    .unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Some(format!("repository generation {}", request.generation).into())
            },
        );
        cache.request(&node(1));
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            (1, 0)
        );
        for pid in 1..=MAX_PENDING as i32 + 3 {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(version) = cache.request(&node(pid)) {
                    assert_eq!(version.version, Some(format!("binary {pid}")));
                    if pid > MAX_PENDING as i32 + 1 {
                        assert!(
                            version
                                .latest_package_version
                                .as_ref()
                                .unwrap()
                                .text
                                .contains("queue full")
                        );
                    }
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "package lookup blocked ordinary version probes"
                );
                std::thread::yield_now();
            }
        }
        assert_eq!(cache.shared.0.lock().unwrap().packages.len(), MAX_PENDING);
        cache.reset();
        assert!(cache.shared.0.lock().unwrap().packages.is_empty());
        let deadline = Instant::now() + Duration::from_secs(5);
        while cache.request(&node(1)).is_none() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        release_tx.send(()).unwrap();
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            (1, 1)
        );
        assert!(
            cache
                .request(&node(1))
                .unwrap()
                .latest_package_version
                .is_none(),
            "stale repository result overwrote the reopened popup"
        );
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(version) = cache.request(&node(1))
                && version.latest_package_version.is_some()
            {
                assert_eq!(
                    version
                        .latest_package_version
                        .as_ref()
                        .map(|version| version.text.as_str()),
                    Some("repository generation 1")
                );
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[test]
    fn closing_info_cancels_package_publication_without_waiting_for_network_work() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let cache = ProcessVersionCache::start_with_packages(
            egui::Context::default(),
            egui::ViewportId::ROOT,
            |_| ProcessVersion {
                package_target: Some(super::super::PackageTarget {
                    name: "tool".into(),
                    inferred: false,
                }),
                ..Default::default()
            },
            move |_| {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Some("MUST NOT PUBLISH".into())
            },
        );
        cache.request(&node(1));
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let shared = cache.shared.clone();
        drop(cache);
        assert!(shared.0.lock().unwrap().stopped);
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Arc::strong_count(&shared) != 1 {
            assert!(
                Instant::now() < deadline,
                "closed popup workers did not exit"
            );
            std::thread::yield_now();
        }
        assert!(
            shared.0.lock().unwrap().results[&node(1).identity]
                .as_ref()
                .unwrap()
                .latest_package_version
                .is_none()
        );
    }
}
