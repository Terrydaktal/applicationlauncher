//! Regression contract: expensive source checks never hold up process metadata,
//! and delayed replies may update only the still-current selected installation.
use super::tests::node;
use super::*;
use std::fs::{self, File};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

const WAIT: Duration = Duration::from_secs(5);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "launcher-source-queue-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("image"), "original fixture image").unwrap();
        Self(root)
    }

    fn image(&self) -> PathBuf {
        self.0.join("image")
    }

    fn check(&self, path: &std::path::Path) -> Arc<super::super::freshness::Check> {
        Arc::new(super::super::freshness::Check::candidate(
            program_metadata::launch::resolve_path(path),
            File::open(path).unwrap(),
        ))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn with_check(check: &Arc<super::super::freshness::Check>, name: &str) -> ProcessVersion {
    ProcessVersion {
        version: Some(name.into()),
        uncompiled_changes: Some(super::super::freshness::PENDING.into()),
        source_check: Some(Arc::clone(check)),
        ..Default::default()
    }
}

fn cache(
    probe: impl Fn(&Request) -> ProcessVersion + Send + 'static,
    source_probe: impl Fn(&source_checks::Request) -> String + Send + 'static,
) -> ProcessVersionCache {
    ProcessVersionCache::start_with_source_checks(
        egui::Context::default(),
        egui::ViewportId::ROOT,
        probe,
        |_| None,
        |request| (request.application.clone(), Default::default()),
        source_probe,
    )
}

fn wait_version(
    cache: &ProcessVersionCache,
    identity: ProcessIdentity,
    ready: impl Fn(&ProcessVersion) -> bool,
) -> Arc<ProcessVersion> {
    let deadline = Instant::now() + WAIT;
    let (mutex, wake) = &*cache.shared;
    let mut state = mutex.lock().unwrap();
    loop {
        if let Some(Some(version)) = state.results.get(&identity)
            && ready(version)
        {
            return Arc::clone(version);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "process metadata did not make progress"
        );
        // Main metadata publication signals this condition variable; field-only
        // workers repaint the UI instead, so also recheck their state changes.
        state = wake
            .wait_timeout(state, remaining.min(Duration::from_millis(10)))
            .unwrap()
            .0;
    }
}

fn stop_and_wait(cache: ProcessVersionCache) -> Arc<(Mutex<State>, Condvar)> {
    let shared = Arc::clone(&cache.shared);
    drop(cache);
    let deadline = Instant::now() + WAIT;
    while Arc::strong_count(&shared) != 1 {
        assert!(Instant::now() < deadline, "metadata workers did not stop");
        std::thread::yield_now();
    }
    shared
}

#[test]
fn blocked_source_check_publishes_metadata_and_does_not_block_another_process_or_package_reply() {
    let fixture = Fixture::new();
    let check = fixture.check(&fixture.image());
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let cache = ProcessVersionCache::start_with_source_checks(
        egui::Context::default(),
        egui::ViewportId::ROOT,
        move |request| {
            if request.identity.pid == 1 {
                ProcessVersion {
                    package_target: Some(super::super::PackageTarget {
                        name: "fixture-package".into(),
                        inferred: false,
                    }),
                    ..with_check(&check, "first executable")
                }
            } else {
                ProcessVersion {
                    version: Some("second executable".into()),
                    ..Default::default()
                }
            }
        },
        |_| Some("independent package result".into()),
        |request| (request.application.clone(), Default::default()),
        move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(WAIT).unwrap();
            "No newer recorded inputs detected · 17 inputs checked".into()
        },
    );
    cache.request(&node(1));
    started_rx.recv_timeout(WAIT).unwrap();
    let initial = wait_version(&cache, node(1).identity, |version| {
        version.latest_package_version.is_some()
    });
    assert_eq!(initial.version.as_deref(), Some("first executable"));
    assert_eq!(
        initial.uncompiled_changes.as_deref(),
        Some(super::super::freshness::PENDING)
    );
    cache.request(&node(2));
    let second = wait_version(&cache, node(2).identity, |_| true);
    assert_eq!(second.version.as_deref(), Some("second executable"));
    release_tx.send(()).unwrap();
    let finished = wait_version(&cache, node(1).identity, |version| {
        version.uncompiled_changes.as_deref() != Some(super::super::freshness::PENDING)
    });
    assert_eq!(finished.version, initial.version);
    assert_eq!(
        finished.latest_package_version.as_ref().unwrap().text,
        "independent package result"
    );
    assert_eq!(
        finished.uncompiled_changes.as_deref(),
        Some("No newer recorded inputs detected · 17 inputs checked")
    );
    assert_eq!(
        initial.uncompiled_changes.as_deref(),
        Some(super::super::freshness::PENDING),
        "updating a delayed field must not mutate an already-rendered snapshot"
    );
    stop_and_wait(cache);
}

fn stale_reply_does_not_restore_metadata(reset: bool) {
    let fixture = Fixture::new();
    let check = fixture.check(&fixture.image());
    let calls = AtomicUsize::new(0);
    let source_calls = AtomicUsize::new(0);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let cache = cache(
        move |_| {
            if calls.fetch_add(1, Ordering::Relaxed) == 0 {
                with_check(&check, "old revision")
            } else {
                with_check(&check, "new revision")
            }
        },
        move |request| {
            started_tx
                .send((request.generation, request.revision))
                .unwrap();
            release_rx.recv_timeout(WAIT).unwrap();
            if source_calls.fetch_add(1, Ordering::Relaxed) == 0 {
                "obsolete clean source result".into()
            } else {
                "new revision owns this field".into()
            }
        },
    );
    cache.request(&node(1));
    let original_request = started_rx.recv_timeout(WAIT).unwrap();
    wait_version(&cache, node(1).identity, |_| true);
    if reset {
        cache.reset();
    } else {
        cache.invalidate(node(1).identity);
    }
    assert!(cache.shared.0.lock().unwrap().sources.is_empty());
    cache.request(&node(1));
    wait_version(&cache, node(1).identity, |version| {
        version.version.as_deref() == Some("new revision")
    });
    release_tx.send(()).unwrap();
    // Starting the next check proves the stale reply was already considered,
    // without relying on shutdown to reject it or a timing-dependent sleep.
    let current_request = started_rx.recv_timeout(WAIT).unwrap();
    assert_ne!(current_request, original_request);
    assert_eq!(
        cache
            .request(&node(1))
            .unwrap()
            .uncompiled_changes
            .as_deref(),
        Some(super::super::freshness::PENDING),
        "an obsolete reply must not fill the newer revision's pending field"
    );
    release_tx.send(()).unwrap();
    wait_version(&cache, node(1).identity, |version| {
        version.uncompiled_changes.as_deref() == Some("new revision owns this field")
    });
    let shared = stop_and_wait(cache);
    let state = shared.0.lock().unwrap();
    let version = state
        .results
        .get(&node(1).identity)
        .unwrap()
        .as_ref()
        .unwrap();
    assert_eq!(version.version.as_deref(), Some("new revision"));
    assert_eq!(
        version.uncompiled_changes.as_deref(),
        Some("new revision owns this field")
    );
}

#[test]
fn reset_discards_in_flight_source_reply_for_a_reopened_process() {
    stale_reply_does_not_restore_metadata(true);
}

#[test]
fn invalidation_discards_in_flight_source_reply_for_a_new_revision() {
    stale_reply_does_not_restore_metadata(false);
}

#[test]
fn closing_the_popup_never_waits_for_or_publishes_a_blocked_source_reply() {
    let fixture = Fixture::new();
    let check = fixture.check(&fixture.image());
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let cache = cache(
        move |_| with_check(&check, "visible executable"),
        move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(WAIT + WAIT).unwrap();
            "must never publish after close".into()
        },
    );
    cache.request(&node(1));
    started_rx.recv_timeout(WAIT).unwrap();
    wait_version(&cache, node(1).identity, |_| true);
    let shared = Arc::clone(&cache.shared);
    let (dropped_tx, dropped_rx) = mpsc::channel();
    let drop_worker = std::thread::spawn(move || {
        drop(cache);
        dropped_tx.send(()).unwrap();
    });
    let dropped_without_joining = dropped_rx.recv_timeout(WAIT);
    // Always release the fixture even if an accidental join delayed Drop.
    release_tx.send(()).unwrap();
    assert!(
        dropped_without_joining.is_ok(),
        "closing the popup waited for the blocked source check"
    );
    drop_worker.join().unwrap();
    assert!(shared.0.lock().unwrap().stopped);
    assert!(shared.0.lock().unwrap().sources.is_empty());
    let deadline = Instant::now() + WAIT;
    while Arc::strong_count(&shared) != 1 {
        assert!(
            Instant::now() < deadline,
            "closed popup retained source worker"
        );
        std::thread::yield_now();
    }
    assert!(shared.0.lock().unwrap().results.values().all(|result| {
        result.as_ref().is_none_or(|version| {
            version.uncompiled_changes.as_deref() != Some("must never publish after close")
        })
    }));
}

#[test]
fn pending_without_a_selected_image_reports_unknown_without_starting_source_inspection() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = Arc::clone(&calls);
    let cache = cache(
        |_| ProcessVersion {
            version: Some("visible executable metadata".into()),
            uncompiled_changes: Some(super::super::freshness::PENDING.into()),
            source_check: None,
            ..Default::default()
        },
        move |_| {
            calls.fetch_add(1, Ordering::Relaxed);
            "must not inspect an unselected image".into()
        },
    );
    cache.request(&node(1));
    let result = wait_version(&cache, node(1).identity, |_| true);
    assert_eq!(
        result.version.as_deref(),
        Some("visible executable metadata")
    );
    assert_eq!(
        result.uncompiled_changes.as_deref(),
        Some("Unknown (selected build-input image unavailable)")
    );
    assert!(result.source_check.is_none());
    assert!(!result.details_pending);
    assert!(cache.shared.0.lock().unwrap().sources.is_empty());
    stop_and_wait(cache);
    assert_eq!(observed_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn full_source_queue_preserves_metadata_and_reports_unknown_instead_of_permanent_checking() {
    let fixture = Fixture::new();
    let check = fixture.check(&fixture.image());
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let cache = cache(
        move |request| with_check(&check, &format!("executable {}", request.identity.pid)),
        move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(WAIT).unwrap();
            "clean".into()
        },
    );
    cache.request(&node(1));
    started_rx.recv_timeout(WAIT).unwrap();
    for pid in 2..=MAX_PENDING as i32 + 2 {
        cache.request(&node(pid));
        let result = wait_version(&cache, node(pid).identity, |_| true);
        assert_eq!(result.version, Some(format!("executable {pid}")));
        if pid == MAX_PENDING as i32 + 2 {
            let status = result.uncompiled_changes.as_deref().unwrap();
            assert!(status.starts_with("Unknown"), "{status}");
            assert!(status.contains("queue"), "{status}");
        }
    }
    assert_eq!(cache.shared.0.lock().unwrap().sources.len(), MAX_PENDING);
    let shared = Arc::clone(&cache.shared);
    drop(cache);
    release_tx.send(()).unwrap();
    let deadline = Instant::now() + WAIT;
    while Arc::strong_count(&shared) != 1 {
        assert!(Instant::now() < deadline, "source worker did not stop");
        std::thread::yield_now();
    }
}

fn changed_image_or_alias_is_not_reported_clean(alias: bool) {
    let fixture = Fixture::new();
    let selected = if alias {
        let launcher = fixture.0.join("launcher");
        std::os::unix::fs::symlink(fixture.image(), &launcher).unwrap();
        launcher
    } else {
        fixture.image()
    };
    let check = fixture.check(&selected);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let cache = cache(
        move |_| with_check(&check, "already visible executable"),
        move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(WAIT).unwrap();
            "No newer recorded inputs detected · stale image".into()
        },
    );
    cache.request(&node(1));
    started_rx.recv_timeout(WAIT).unwrap();
    wait_version(&cache, node(1).identity, |_| true);
    let replacement = fixture.0.join("replacement");
    fs::write(&replacement, "a different selected image").unwrap();
    if alias {
        let replacement_link = fixture.0.join("replacement-link");
        std::os::unix::fs::symlink(&replacement, &replacement_link).unwrap();
        fs::rename(&replacement_link, &selected).unwrap();
    } else {
        fs::rename(&replacement, &selected).unwrap();
    }
    release_tx.send(()).unwrap();
    let result = wait_version(&cache, node(1).identity, |version| {
        version.uncompiled_changes.as_deref() != Some(super::super::freshness::PENDING)
    });
    assert_eq!(
        result.version.as_deref(),
        Some("already visible executable")
    );
    let status = result.uncompiled_changes.as_deref().unwrap();
    assert!(status.starts_with("Unknown"), "{status}");
    assert!(status.contains("changed"), "{status}");
    stop_and_wait(cache);
}

#[test]
fn replaced_disk_image_cannot_receive_a_stale_clean_source_result() {
    changed_image_or_alias_is_not_reported_clean(false);
}

#[test]
fn retargeted_launcher_cannot_receive_a_stale_clean_source_result() {
    changed_image_or_alias_is_not_reported_clean(true);
}

#[test]
fn retargeted_installed_alias_cannot_receive_a_stale_clean_source_result() {
    let fixture = Fixture::new();
    let alias = fixture.0.join("published-program");
    std::os::unix::fs::symlink(fixture.image(), &alias).unwrap();
    let installed = program_metadata::installation::installed_executable(&alias).unwrap();
    assert_eq!(installed.path, alias);
    let check = Arc::new(super::super::freshness::Check::installed(
        installed,
        File::open(&alias).unwrap(),
        None,
    ));
    assert!(check.unchanged(), "the installed selection starts valid");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let cache = cache(
        move |_| with_check(&check, "already visible installed executable"),
        move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(WAIT).unwrap();
            "No newer recorded inputs detected · obsolete installed alias".into()
        },
    );
    cache.request(&node(1));
    started_rx.recv_timeout(WAIT).unwrap();
    let initial = wait_version(&cache, node(1).identity, |_| true);
    assert_eq!(
        initial.uncompiled_changes.as_deref(),
        Some(super::super::freshness::PENDING)
    );
    let replacement = fixture.0.join("replacement");
    fs::write(&replacement, "a different installed image").unwrap();
    let replacement_link = fixture.0.join("replacement-link");
    std::os::unix::fs::symlink(&replacement, &replacement_link).unwrap();
    fs::rename(&replacement_link, &alias).unwrap();
    assert_eq!(fs::canonicalize(&alias).unwrap(), replacement);
    assert_eq!(
        fs::read_to_string(fixture.image()).unwrap(),
        "original fixture image",
        "only the publication alias changed; the pinned image remains intact"
    );
    release_tx.send(()).unwrap();
    let result = wait_version(&cache, node(1).identity, |version| {
        version.uncompiled_changes.as_deref() != Some(super::super::freshness::PENDING)
    });
    assert_eq!(result.version, initial.version);
    let status = result.uncompiled_changes.as_deref().unwrap();
    assert!(status.starts_with("Unknown"), "{status}");
    assert!(status.contains("changed"), "{status}");
    assert!(!status.contains("No newer recorded inputs"), "{status}");
    assert_eq!(
        initial.uncompiled_changes.as_deref(),
        Some(super::super::freshness::PENDING),
        "the earlier rendered snapshot stays immutable"
    );
    stop_and_wait(cache);
}

#[test]
fn panicking_source_check_reports_unknown_and_the_worker_continues() {
    let fixture = Fixture::new();
    let check = fixture.check(&fixture.image());
    let cache = cache(
        move |request| with_check(&check, &format!("executable {}", request.identity.pid)),
        |request| {
            if request.identity.pid == 1 {
                panic!("isolated source checker fixture");
            }
            "second source check completed".into()
        },
    );
    cache.request(&node(1));
    let first = wait_version(&cache, node(1).identity, |version| {
        version.uncompiled_changes.as_deref() != Some(super::super::freshness::PENDING)
    });
    let status = first.uncompiled_changes.as_deref().unwrap();
    assert!(status.starts_with("Unknown"), "{status}");
    assert!(status.contains("failed"), "{status}");
    cache.request(&node(2));
    let second = wait_version(&cache, node(2).identity, |version| {
        version.uncompiled_changes.as_deref() == Some("second source check completed")
    });
    assert_eq!(second.version.as_deref(), Some("executable 2"));
    stop_and_wait(cache);
}

#[test]
fn expanding_a_completed_header_retains_its_current_build_evidence_while_details_load() {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = Arc::clone(&calls);
    let cache = cache(
        move |request| {
            calls.fetch_add(1, Ordering::Relaxed);
            if request.include_packages {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(WAIT).unwrap();
                ProcessVersion {
                    version: Some("expanded metadata".into()),
                    running_latest_binary: Some(true),
                    ..Default::default()
                }
            } else {
                ProcessVersion {
                    version: Some("cheap header evidence".into()),
                    running_latest_binary: Some(true),
                    ..Default::default()
                }
            }
        },
        |_| panic!("a header-only fixture must not request source inspection"),
    );
    cache.request_for_header(&node(1));
    let header = wait_version(&cache, node(1).identity, |_| true);
    assert!(header.details_pending);
    assert_eq!(header.running_latest_binary, Some(true));
    let upgrading = cache.request(&node(1)).unwrap();
    assert!(Arc::ptr_eq(&header, &upgrading));
    started_rx.recv_timeout(WAIT).unwrap();
    for _ in 0..10 {
        let retained = cache.request(&node(1)).unwrap();
        assert_eq!(retained.running_latest_binary, Some(true));
        assert_eq!(retained.version.as_deref(), Some("cheap header evidence"));
        assert!(retained.details_pending);
    }
    assert_eq!(observed_calls.load(Ordering::Relaxed), 2);
    release_tx.send(()).unwrap();
    let detailed = wait_version(&cache, node(1).identity, |version| {
        version.version.as_deref() == Some("expanded metadata")
    });
    assert!(!detailed.details_pending);
    assert_eq!(detailed.running_latest_binary, Some(true));
    assert!(
        header.details_pending,
        "the prior rendered snapshot stays immutable"
    );
    stop_and_wait(cache);
}
