use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const ENTRY: &str = "#!/bin/sh\nexit 0\n";
// Independent SHA-256 oracle for the exact literal above, not a production helper.
const SHA: &str = "306c6ca7407560340797866e077e053627ad409277d1b9da58106fce4cf717cb";

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "launcher-stamped-script-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(path.join("tool.sh"), ENTRY).unwrap();
        Self(path)
    }
    fn entry(&self) -> PathBuf {
        self.0.join("tool.sh")
    }
    fn record(&self, version: &str) {
        let path = self.0.join("tool.sh.program-metadata.json");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version":1, "product":"tool.sh", "version":version,
                "source_commit":"ab".repeat(20), "entry_sha256":SHA
            }))
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn version(path: &Path) -> ProcessVersion {
    let metadata = program_metadata::interpreted::stamped_script_version_until(
        path,
        Instant::now() + Duration::from_secs(1),
    );
    ProcessVersion {
        runtime_name: Some("Shell"),
        version: Some("native shell version remains separate".into()),
        script_application: Some(super::super::ScriptApplication {
            name: "tool".into(),
            target: path.display().to_string(),
            kind: "Shell script",
            entry_point: fs::canonicalize(path).ok(),
            descriptor: None,
            identification: "Exact argument",
        }),
        application_installed_metadata: metadata.as_ref().ok().cloned().flatten(),
        application_metadata_error: metadata.err(),
        ..Default::default()
    }
}
fn node() -> ProcessInfoNode {
    super::tests::node(4242)
}
fn expire(cache: &ProcessVersionCache, identity: ProcessIdentity) {
    cache
        .shared
        .0
        .lock()
        .unwrap()
        .script_checked
        .insert(identity, Instant::now() - SCRIPT_METADATA_REFRESH);
}
fn wait(
    cache: &ProcessVersionCache,
    node: &ProcessInfoNode,
    expected: Option<&str>,
    failed: bool,
) -> Arc<ProcessVersion> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(result) = cache.request(node)
            && result
                .application_installed_metadata
                .as_ref()
                .map(|metadata| metadata.version.as_str())
                == expected
            && result.application_metadata_error.is_some() == failed
        {
            return result;
        }
        assert!(
            Instant::now() < deadline,
            "stamped metadata did not refresh"
        );
        std::thread::yield_now();
    }
}

#[test]
fn stamped_script_refreshes_sidecar_only_without_restart_and_skips_unchanged_probes() {
    let f = Fixture::new();
    f.record("0.0.17-1");
    let path = f.entry();
    let count = Arc::new(AtomicUsize::new(0));
    let probes = count.clone();
    let cache = ProcessVersionCache::start_with_packages(
        egui::Context::default(),
        egui::ViewportId::ROOT,
        move |_| {
            probes.fetch_add(1, Ordering::SeqCst);
            version(&path)
        },
        |_| None,
    );
    let process = node();
    wait(&cache, &process, Some("0.0.17-1"), false);
    f.record("0.0.17-2");
    expire(&cache, process.identity);
    assert_eq!(
        cache
            .request_expanded(&process, false)
            .unwrap()
            .application_installed_metadata
            .as_ref()
            .unwrap()
            .version,
        "0.0.17-1"
    );
    assert!(
        !cache
            .shared
            .0
            .lock()
            .unwrap()
            .script_pending
            .contains(&process.identity)
    );
    let updated = wait(&cache, &process, Some("0.0.17-2"), false);
    assert_eq!(
        updated.version.as_deref(),
        Some("native shell version remains separate")
    );
    assert_eq!(count.load(Ordering::SeqCst), 2);
    expire(&cache, process.identity);
    cache.request(&process);
    let deadline = Instant::now() + Duration::from_secs(5);
    while cache
        .shared
        .0
        .lock()
        .unwrap()
        .script_pending
        .contains(&process.identity)
    {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "unchanged passive hints must not re-probe the interpreter"
    );
    fs::write(f.entry(), "changed entry").unwrap();
    expire(&cache, process.identity);
    wait(&cache, &process, None, true);
    assert_eq!(count.load(Ordering::SeqCst), 3);
    fs::write(f.entry(), ENTRY).unwrap();
    expire(&cache, process.identity);
    wait(&cache, &process, Some("0.0.17-2"), false);
    assert_eq!(count.load(Ordering::SeqCst), 4);
}

#[test]
fn passive_script_snapshot_detects_stamp_creation_and_launch_alias_retarget() {
    let f = Fixture::new();
    let alias = f.0.join("launch");
    symlink(f.entry(), &alias).unwrap();
    let initial = ScriptSnapshot::for_version(&version(&alias), None).unwrap();
    assert_eq!(initial, initial.current());
    f.record("0.0.17-1");
    assert_ne!(initial, initial.current());
    let stamped = ScriptSnapshot::for_version(&version(&alias), None).unwrap();
    fs::write(f.0.join("other.sh"), ENTRY).unwrap();
    fs::remove_file(&alias).unwrap();
    symlink(f.0.join("other.sh"), &alias).unwrap();
    assert_ne!(stamped, stamped.current());
    assert!(
        program_metadata::interpreted::stamped_script_version_until(
            &alias,
            Instant::now() + Duration::from_secs(1)
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn descriptor_script_refresh_tracks_target_fd_and_never_callers_same_number() {
    for alias in [false, true] {
        descriptor_script_refresh_case(alias);
    }
}

fn descriptor_script_refresh_case(alias: bool) {
    let mut fixture = super::super::descriptor_tests::DescriptorFixture::with_alias(false, alias);
    let application = fixture.application();
    assert_eq!(
        application.entry_point.as_deref(),
        Some(fixture.entry.as_path())
    );
    let version = ProcessVersion {
        runtime_name: Some("Shell"),
        script_application: Some(application),
        ..Default::default()
    };
    let snapshot = ScriptSnapshot::for_version(&version, None).unwrap();
    assert_eq!(
        snapshot,
        snapshot.current(),
        "a stable target fd must not be compared with the caller's recording fd"
    );

    // A sidecar on the actual script, not on the caller's recording, changes
    // the cheap refresh hint even though neither file descriptor is replaced.
    let record = fixture
        .entry
        .with_file_name("tool.sh.program-metadata.json");
    fs::write(&record, "malformed target-side metadata").unwrap();
    fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
    assert_ne!(snapshot, snapshot.current());

    let before_reuse = ScriptSnapshot::for_version(&version, None).unwrap();
    fixture.change_descriptor("reuse");
    assert!(!version.script_application.as_ref().unwrap().unchanged());
    assert_ne!(before_reuse, before_reuse.current());
}

#[test]
fn descriptor_memfd_scripts_have_no_installed_metadata_refresh_route() {
    let fixture = super::super::descriptor_tests::DescriptorFixture::new(true);
    let version = ProcessVersion {
        runtime_name: Some("Shell"),
        script_application: Some(fixture.application()),
        ..Default::default()
    };
    assert!(
        version
            .script_application
            .as_ref()
            .unwrap()
            .entry_point
            .is_none()
    );
    assert!(ScriptSnapshot::for_version(&version, None).is_none());
}

#[test]
fn retained_memfd_details_refresh_without_inventing_an_installed_script() {
    let mut fixture = super::super::descriptor_tests::DescriptorFixture::new(true);
    let mut application = fixture.application();
    // No host PATH installation should participate in this owned-process test.
    application.name = format!("launcher-owned-descriptor-{}", std::process::id());
    let details = application
        .descriptor
        .as_ref()
        .unwrap()
        .inspect_details(None, Instant::now() + Duration::from_secs(1))
        .unwrap();
    let version = ProcessVersion {
        runtime_name: Some("Shell"),
        script_application: Some(application),
        script_details: Some(details),
        ..Default::default()
    };
    assert!(
        version
            .script_application
            .as_ref()
            .unwrap()
            .entry_point
            .is_none()
    );
    let snapshot = ScriptSnapshot::for_version(&version, None).unwrap();
    assert_eq!(snapshot, snapshot.current());
    fixture.change_descriptor("reuse");
    assert_ne!(snapshot, snapshot.current());
    assert!(version.application_launch.is_none());
    assert!(version.application_installation.is_none());
}

#[test]
fn development_script_edits_racing_publication_keep_the_captured_refresh_baseline() {
    use program_metadata::{build_status::artifact::ArtifactStamp, development::Metadata};
    let fixture = Fixture::new();
    let entry = fixture.entry();
    let mut inspected = version(&entry);
    let captured = ArtifactStamp::from_metadata(&fs::metadata(&entry).unwrap());
    inspected.application_development = Some(Metadata {
        source_languages: vec!["Shell".into()],
        artifact_stamp: Some(captured),
        ..Default::default()
    });
    let before = ScriptSnapshot::for_version(&inspected, None).unwrap();
    assert_eq!(before, before.current());
    fs::write(&entry, b"#!/bin/sh\necho changed after source inspection\n").unwrap();
    let raced = ScriptSnapshot::for_version(&inspected, None).unwrap();
    assert_ne!(raced, raced.current());
    inspected
        .application_development
        .as_mut()
        .unwrap()
        .artifact_stamp = Some(ArtifactStamp::from_metadata(&fs::metadata(&entry).unwrap()));
    let refreshed = ScriptSnapshot::for_version(&inspected, None).unwrap();
    assert_eq!(refreshed, refreshed.current());
}

fn python_project_version(fixture: &Fixture) -> ProcessVersion {
    use program_metadata::interpreted::python_process::{self, PythonProcessMetadata};
    let entry = fixture.0.join("main.py");
    ProcessVersion {
        runtime_name: Some("Python"),
        version: Some("independent Python interpreter evidence".into()),
        script_application: Some(super::super::ScriptApplication {
            name: "main".into(),
            target: entry.display().to_string(),
            kind: "Python script",
            entry_point: Some(entry.clone()),
            descriptor: None,
            identification: "Owned Python source fixture",
        }),
        python_context: Some(PythonProcessMetadata {
            project_modules: Some(python_process::inspect_project_modules(
                &entry,
                &fixture.0,
                Instant::now() + Duration::from_secs(2),
            )),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn python_project_module_snapshot_tracks_transitive_edits_and_new_local_candidates() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("main.py"),
        "import helper\nimport optional_local\n",
    )
    .unwrap();
    fs::write(fixture.0.join("helper.py"), "import leaf\n").unwrap();
    fs::write(fixture.0.join("leaf.py"), "VALUE = 1\n").unwrap();
    let inspected = python_project_version(&fixture);
    let modules = &inspected
        .python_context
        .as_ref()
        .unwrap()
        .project_modules
        .as_ref()
        .unwrap()
        .modules;
    assert!(
        modules
            .iter()
            .any(|module| module.path.ends_with("helper.py"))
    );
    assert!(
        modules
            .iter()
            .any(|module| module.path.ends_with("leaf.py"))
    );
    let initial = ScriptSnapshot::for_version(&inspected, None).unwrap();
    assert_eq!(initial, initial.current());
    fs::write(fixture.0.join("unrelated-build.log"), "build output\n").unwrap();
    assert_eq!(
        initial,
        initial.current(),
        "unrelated directory activity must not invalidate a named-import graph"
    );
    let mut entry_only = inspected.clone();
    entry_only.python_context = None;
    let old_entry_only = ScriptSnapshot::for_version(&entry_only, None).unwrap();

    fs::write(
        fixture.0.join("leaf.py"),
        "VALUE = 'changed transitive module'\n",
    )
    .unwrap();
    assert_eq!(
        old_entry_only,
        old_entry_only.current(),
        "the original entry-only refresh mechanism cannot see this edit"
    );
    assert_ne!(
        initial,
        initial.current(),
        "entry-only fingerprints miss imported-file edits"
    );
    let updated = ScriptSnapshot::for_version(&python_project_version(&fixture), None).unwrap();
    assert_eq!(updated, updated.current());
    fs::write(fixture.0.join("optional_local.py"), "VALUE = 2\n").unwrap();
    assert_ne!(
        updated,
        updated.current(),
        "a newly resolvable local import must invalidate the old graph"
    );
}

#[test]
fn python_project_module_publication_race_discards_only_graph_and_keeps_old_baseline() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("main.py"), "import helper\n").unwrap();
    fs::write(fixture.0.join("helper.py"), "VALUE = 1\n").unwrap();
    let mut inspected = python_project_version(&fixture);
    fs::write(
        fixture.0.join("helper.py"),
        "VALUE = 'edited before publication'\n",
    )
    .unwrap();
    let baseline = ScriptSnapshot::for_version(&inspected, None).unwrap();
    inspected.discard_changed_python_modules();
    assert!(
        inspected
            .python_context
            .as_ref()
            .unwrap()
            .project_modules
            .is_none()
    );
    assert!(inspected.script_application.is_some());
    assert_eq!(
        inspected.version.as_deref(),
        Some("independent Python interpreter evidence")
    );
    assert_ne!(
        baseline,
        baseline.current(),
        "stale graph observations must not become a fresh baseline"
    );
}

#[test]
fn python_project_module_worker_refresh_reprobes_changes_but_not_stable_graphs() {
    use program_metadata::interpreted::python_process::inspect_project_modules;
    let fixture = Fixture::new();
    fs::write(fixture.0.join("main.py"), "import helper\n").unwrap();
    fs::write(fixture.0.join("helper.py"), "VALUE = 1\n").unwrap();
    let template = python_project_version(&fixture);
    let root = fixture.0.clone();
    let probes = Arc::new(AtomicUsize::new(0));
    let count = probes.clone();
    let cache = ProcessVersionCache::start_with_packages(
        egui::Context::default(),
        egui::ViewportId::ROOT,
        move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            let mut version = template.clone();
            version.python_context.as_mut().unwrap().project_modules =
                Some(inspect_project_modules(
                    &root.join("main.py"),
                    &root,
                    Instant::now() + Duration::from_secs(2),
                ));
            version
        },
        |_| None,
    );
    let process = node();
    let wait_modules = |expected| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(version) = cache.request(&process)
                && version
                    .python_context
                    .as_ref()
                    .and_then(|context| context.project_modules.as_ref())
                    .is_some_and(|modules| modules.modules.len() == expected)
            {
                assert_eq!(
                    version.version.as_deref(),
                    Some("independent Python interpreter evidence")
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "module worker did not publish the expected graph"
            );
            std::thread::yield_now();
        }
    };
    wait_modules(1);
    fs::write(fixture.0.join("leaf.py"), "VALUE = 2\n").unwrap();
    fs::write(fixture.0.join("helper.py"), "import leaf\n").unwrap();
    expire(&cache, process.identity);
    wait_modules(2);
    assert_eq!(probes.load(Ordering::SeqCst), 2);
    fs::write(fixture.0.join("unrelated-cache.tmp"), "cache output\n").unwrap();
    expire(&cache, process.identity);
    cache.request(&process);
    let deadline = Instant::now() + Duration::from_secs(5);
    while cache
        .shared
        .0
        .lock()
        .unwrap()
        .script_pending
        .contains(&process.identity)
    {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(
        probes.load(Ordering::SeqCst),
        2,
        "unchanged import graphs must not re-probe the interpreter"
    );
}

#[test]
fn stale_retained_details_never_become_an_unchanged_refresh_baseline() {
    let mut fixture = super::super::descriptor_tests::DescriptorFixture::new(true);
    let mut application = fixture.application();
    application.name = format!("launcher-owned-stale-descriptor-{}", std::process::id());
    let details = application
        .descriptor
        .as_ref()
        .unwrap()
        .inspect_details(None, Instant::now() + Duration::from_secs(1))
        .unwrap();
    fixture.change_descriptor("reuse");
    let version = ProcessVersion {
        script_application: Some(application),
        script_details: Some(details),
        ..Default::default()
    };
    let snapshot = ScriptSnapshot::for_version(&version, None).unwrap();
    assert_ne!(snapshot, snapshot.current());
}

#[test]
fn descriptor_script_package_response_cannot_restore_discarded_application_metadata() {
    let mut fixture = super::super::descriptor_tests::DescriptorFixture::new(false);
    let application = fixture.application();
    let (started, receiving) = std::sync::mpsc::sync_channel(1);
    let (release, waiting) = std::sync::mpsc::sync_channel(1);
    let cache = ProcessVersionCache::start_with_packages(
        egui::Context::default(),
        egui::ViewportId::ROOT,
        move |_| ProcessVersion {
            runtime_name: Some("Shell"),
            version: Some("independent shell evidence".into()),
            script_application: Some(application.clone()),
            package_target: Some(super::super::PackageTarget {
                name: "fixture-runtime".into(),
                inferred: false,
            }),
            application_package_target: Some(super::super::PackageTarget {
                name: "fixture-script".into(),
                inferred: false,
            }),
            npm_target: super::super::NpmTarget::package("fixture-runtime"),
            ..Default::default()
        },
        move |request| {
            if request.application {
                assert!(request.descriptor.is_some());
                started.send(()).unwrap();
                waiting.recv_timeout(Duration::from_secs(5)).unwrap();
                Some("stale script package result".into())
            } else {
                assert!(request.descriptor.is_none());
                Some("independent runtime package result".into())
            }
        },
    );
    let process = node();
    cache.request(&process);
    receiving.recv_timeout(Duration::from_secs(5)).unwrap();
    let cached = cache.request_expanded(&process, false).unwrap();
    fixture.change_descriptor("reuse");
    let mut discarded = (*cached).clone();
    assert!(discarded.discard_changed_script_metadata());
    cache
        .shared
        .0
        .lock()
        .unwrap()
        .results
        .insert(process.identity, Some(Arc::new(discarded)));
    release.send(()).unwrap();

    // The runtime npm request is behind the held app request, so its published
    // result is an event barrier proving the late app response was handled.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let version = cache.request_expanded(&process, false).unwrap();
        if version.latest_npm_version.is_some() {
            assert!(version.script_application.is_none());
            assert!(version.application_latest_package_version.is_none());
            assert!(version.application_package_target.is_none());
            assert!(version.application_metadata_error.is_some());
            assert_eq!(
                version.version.as_deref(),
                Some("independent shell evidence")
            );
            assert_eq!(
                version.latest_package_version.as_ref().unwrap().text,
                "independent runtime package result"
            );
            break;
        }
        assert!(Instant::now() < deadline, "package worker did not finish");
        std::thread::yield_now();
    }
}

#[test]
fn passive_script_snapshot_invalidates_when_only_a_declared_version_donor_changes() {
    let f = Fixture::new();
    fs::write(f.0.join("APP-VERSION"), ENTRY).unwrap();
    let record = f.0.join("tool.sh.program-metadata.json");
    fs::write(
        &record,
        serde_json::to_vec(&serde_json::json!({
            "schema_version":2, "product":"tool.sh", "version":"0.0.17-1",
            "source_commit":"ab".repeat(20), "entry_sha256":SHA,
            "input_root_depth":0, "inputs":[{"path":"APP-VERSION", "sha256":SHA}]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
    let entry = f.entry();
    let before = ScriptSnapshot::for_version(&version(&entry), None).unwrap();
    assert_eq!(
        version(&entry)
            .application_installed_metadata
            .unwrap()
            .version,
        "0.0.17-1"
    );
    let entry_identity = fs::metadata(&entry)
        .map(|metadata| {
            program_metadata::build_status::artifact::ArtifactStamp::from_metadata(&metadata)
        })
        .unwrap();
    let record_identity = fs::metadata(&record)
        .map(|metadata| {
            program_metadata::build_status::artifact::ArtifactStamp::from_metadata(&metadata)
        })
        .unwrap();
    fs::write(f.0.join("APP-VERSION"), "changed only the donor").unwrap();
    let after = before.current();
    assert_ne!(before, after);
    assert!(
        version(&entry)
            .application_metadata_error
            .unwrap()
            .contains("input changed")
    );
    assert_eq!(
        entry_identity,
        fs::metadata(&entry)
            .map(
                |metadata| program_metadata::build_status::artifact::ArtifactStamp::from_metadata(
                    &metadata
                )
            )
            .unwrap()
    );
    assert_eq!(
        record_identity,
        fs::metadata(&record)
            .map(
                |metadata| program_metadata::build_status::artifact::ArtifactStamp::from_metadata(
                    &metadata
                )
            )
            .unwrap()
    );
    fs::write(f.0.join("APP-VERSION"), ENTRY).unwrap();
    assert_eq!(before, before.current());
}
