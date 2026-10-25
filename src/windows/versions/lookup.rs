use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

use super::{ProcessVersion, ProcessVersionTarget, probe_version_checked};
use crate::models::{ProcessIdentity, ProcessInfoNode};
use crate::windows::process_tree::process_identity_is_current;

const MAX_PENDING: usize = 16;
const MAX_RESULTS: usize = 512;
pub(crate) const PI_RECEIPT_REFRESH: Duration = Duration::from_secs(2);

struct Request {
    generation: u64,
    identity: ProcessIdentity,
    executable_identity: Option<(u64, u64)>,
    target: ProcessVersionTarget,
}

struct PackageRequest {
    generation: u64,
    identity: ProcessIdentity,
    target: super::PackageTarget,
    application: bool,
}

struct ReceiptRequest {
    generation: u64,
    identity: ProcessIdentity,
    executable_identity: Option<(u64, u64)>,
    application: super::NodeApplication,
    program: String,
}

#[derive(Default)]
struct State {
    generation: u64,
    stopped: bool,
    pending: VecDeque<Request>,
    packages: VecDeque<PackageRequest>,
    receipts: VecDeque<ReceiptRequest>,
    receipt_pending: HashSet<ProcessIdentity>,
    receipt_checked: HashMap<ProcessIdentity, Instant>,
    results: HashMap<ProcessIdentity, Option<Arc<ProcessVersion>>>,
}

pub(crate) struct ProcessVersionCache {
    shared: Arc<(Mutex<State>, Condvar)>,
}

impl ProcessVersionCache {
    pub(crate) fn new(ctx: egui::Context, viewport_id: egui::ViewportId) -> Self {
        Self::start(ctx, viewport_id, |request| {
            probe_version_checked(&request.target, || {
                process_identity_is_current(request.identity, request.executable_identity)
            })
        })
    }

    fn start(
        ctx: egui::Context,
        viewport_id: egui::ViewportId,
        probe: impl Fn(&Request) -> ProcessVersion + Send + 'static,
    ) -> Self {
        Self::start_with_packages(ctx, viewport_id, probe, |request| {
            // A repository version belongs to a package, not the lifetime of its process.
            Some(super::available::query(&request.target))
        })
    }

    fn start_with_packages(
        ctx: egui::Context,
        viewport_id: egui::ViewportId,
        probe: impl Fn(&Request) -> ProcessVersion + Send + 'static,
        package_probe: impl Fn(&PackageRequest) -> Option<String> + Send + 'static,
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
        package_probe: impl Fn(&PackageRequest) -> Option<String> + Send + 'static,
        receipt_probe: impl Fn(&ReceiptRequest) -> (super::NodeApplication, super::PiBuildEvidence)
        + Send
        + 'static,
    ) -> Self {
        let shared: Arc<(Mutex<State>, Condvar)> = Arc::default();
        let worker_shared = Arc::clone(&shared);
        let package_shared = Arc::clone(&shared);
        let receipt_shared = Arc::clone(&shared);
        let package_ctx = ctx.clone();
        let receipt_ctx = ctx.clone();
        applicationlauncher::observability::spawn_named(
            "process-info-pi-receipts",
            move |worker| {
                loop {
                    worker.set_state("waiting-for-expanded-pi-section");
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
                    worker.set_state("reading-pi-build-receipt");
                    let (application, evidence) = receipt_probe(&request);
                    let publish = {
                        let mut state = receipt_shared
                            .0
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        if state.stopped {
                            break;
                        }
                        if state.generation != request.generation {
                            continue;
                        }
                        state.receipt_pending.remove(&request.identity);
                        if let Some(Some(version)) = state.results.get_mut(&request.identity) {
                            let version = Arc::make_mut(version);
                            version.application = Some(application);
                            version.application_build = Some(evidence);
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
            },
        );
        applicationlauncher::observability::spawn_named(
            "process-info-package-versions",
            move |worker| {
                // One bounded lookup per expanded process; no timer or idle polling.
                loop {
                    worker.set_state("waiting-for-package-request");
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
                    worker.set_state("querying-pacman-or-aur");
                    let result = package_probe(&request)
                        .unwrap_or_else(|| "Unavailable (package lookup failed)".into());
                    let publish = {
                        let mut state = package_shared
                            .0
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        if state.stopped {
                            break;
                        }
                        if state.generation != request.generation {
                            continue;
                        }
                        if let Some(Some(version)) = state.results.get_mut(&request.identity) {
                            let version = Arc::make_mut(version);
                            if request.application {
                                version.application_latest_package_version = Some(result);
                            } else {
                                version.latest_package_version = Some(result);
                            }
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
            },
        );
        applicationlauncher::observability::spawn_named("process-info-versions", move |worker| {
            // One worker consumes bounded, explicit expansion requests; an empty queue sleeps.
            loop {
                worker.set_state("waiting-for-expanded-section");
                let request = {
                    let (mutex, wake) = &*worker_shared;
                    let mut state = mutex.lock().unwrap_or_else(|error| error.into_inner());
                    while !state.stopped && state.pending.is_empty() {
                        state = wake.wait(state).unwrap_or_else(|error| error.into_inner());
                    }
                    if state.stopped {
                        break;
                    }
                    state.pending.pop_front().unwrap()
                };
                worker.set_state("probing-expanded-process");
                let mut version = probe(&request);
                let publish = {
                    let mut state = worker_shared
                        .0
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if state.stopped {
                        break;
                    }
                    if state.generation == request.generation {
                        if version
                            .application
                            .as_ref()
                            .is_some_and(|application| application.package == super::pi::PACKAGE)
                        {
                            state
                                .receipt_checked
                                .insert(request.identity, Instant::now());
                        }
                        for (target, application) in [
                            (version.package_target.clone(), false),
                            (version.application_package_target.clone(), true),
                        ] {
                            if let Some(target) = target {
                                if state.packages.len() < MAX_PENDING {
                                    state.packages.push_back(PackageRequest {
                                        generation: request.generation,
                                        identity: request.identity,
                                        target,
                                        application,
                                    });
                                } else {
                                    let result = Some("Unavailable (package lookup queue full; close and reopen Show Info to retry)".into());
                                    if application {
                                        version.application_latest_package_version = result;
                                    } else {
                                        version.latest_package_version = result;
                                    }
                                }
                            }
                        }
                        state
                            .results
                            .insert(request.identity, Some(Arc::new(version)));
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

    pub(crate) fn reset(&self) {
        let mut state = self
            .shared
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.generation = state.generation.wrapping_add(1);
        state.pending.clear();
        state.packages.clear();
        state.receipts.clear();
        state.receipt_pending.clear();
        state.receipt_checked.clear();
        state.results.clear();
    }

    #[cfg(test)]
    pub(crate) fn request(&self, node: &ProcessInfoNode) -> Option<Arc<ProcessVersion>> {
        self.request_expanded(node, true)
    }

    pub(crate) fn request_expanded(
        &self,
        node: &ProcessInfoNode,
        refresh_receipt: bool,
    ) -> Option<Arc<ProcessVersion>> {
        let mut state = self
            .shared
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(result) = state.results.get(&node.identity).cloned() {
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
                state.receipts.push_back(ReceiptRequest {
                    generation,
                    identity: node.identity,
                    executable_identity: node.executable_identity,
                    application: application.clone(),
                    program: node.name.clone(),
                });
                state.receipt_pending.insert(node.identity);
                state.receipt_checked.insert(node.identity, Instant::now());
                self.shared.1.notify_all();
            }
            return result;
        }
        let Some(executable) = node.executable.as_ref() else {
            return Some(Arc::new(ProcessVersion::default()));
        };
        if state.results.len() >= MAX_RESULTS || state.pending.len() >= MAX_PENDING {
            return None;
        }
        let generation = state.generation;
        state.pending.push_back(Request {
            generation,
            identity: node.identity,
            executable_identity: node.executable_identity,
            target: ProcessVersionTarget {
                pid: node.identity.pid,
                executable: executable.clone(),
                program: Some(node.name.clone()),
            },
        });
        state.results.insert(node.identity, None);
        self.shared.1.notify_all();
        None
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

    fn node(pid: i32) -> ProcessInfoNode {
        ProcessInfoNode {
            identity: ProcessIdentity {
                pid,
                start_ticks: pid as u64,
            },
            parent_pid: 0,
            depth: 0,
            name: "test".into(),
            header: "test".into(),
            executable: Some("/not-executed/test".into()),
            executable_identity: None,
            cwd: None,
            command_line: None,
            command_summary: None,
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
                Some(format!("repository generation {}", request.generation))
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
                    version.latest_package_version.as_deref(),
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
