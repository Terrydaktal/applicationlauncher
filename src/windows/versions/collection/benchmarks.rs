use crate::windows::versions::{
    ExecutableContext, application, clean_executable, disk_executable_version, executable_identity,
    freshness, installation, installed_package, probe_executable_context,
};
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::time::Instant;

fn timed<T>(name: &str, operation: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let result = operation();
    eprintln!(
        "metadata-stage {name}: {:.3} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
    result
}

#[test]
#[ignore = "read-only full lookup benchmark; requires APPLICATIONLAUNCHER_TEST_VERSION_PIDS pointing to an explicitly selected fixture process"]
fn live_full_metadata_timings() {
    use crate::windows::process_tree::process_identity_is_current;
    use crate::windows::versions::{
        ProcessLaunchContext, ProcessVersionTarget, probe_version_checked, process_argv0,
    };
    let pid: i32 = std::env::var("APPLICATIONLAUNCHER_TEST_VERSION_PIDS")
        .expect("supply an explicit fixture process PID")
        .parse()
        .unwrap();
    assert!(pid > 0);
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let identity = crate::models::ProcessIdentity {
        pid,
        start_ticks: stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap()
            .parse()
            .unwrap(),
    };
    let image = std::fs::metadata(format!("/proc/{pid}/exe")).unwrap();
    let current = || process_identity_is_current(identity, Some((image.dev(), image.ino())));
    let target = ProcessVersionTarget {
        pid,
        executable: std::fs::read_link(format!("/proc/{pid}/exe")).unwrap(),
        launch_context: ProcessLaunchContext {
            raw_argv0: process_argv0(pid),
            executable_unavailable: None,
        },
        program: crate::windows::read_process_stat(pid).map(|process| process.name),
        application_entry: None,
        window_declared_entry: false,
    };
    for pass in 1..=5 {
        let version = timed(&format!("full-metadata-{pass}"), || {
            probe_version_checked(&target, current)
        });
        assert!(current());
        assert!(
            version.version.is_some(),
            "fixture version unavailable: {version:?}"
        );
        assert!(version.build_id.is_some());
        eprintln!(
            "full-check-result: {}",
            version
                .uncompiled_changes
                .as_deref()
                .unwrap_or("Unavailable")
        );
    }
}

#[test]
#[ignore = "selected-process progressive timing; may run already-authorized bounded version queries; requires APPLICATIONLAUNCHER_TEST_VERSION_PIDS"]
fn live_progressive_metadata_timings() {
    use crate::windows::process_tree::process_identity_is_current;
    use crate::windows::versions::{
        ProcessLaunchContext, ProcessVersionTarget, probe_version_deferred, process_argv0,
    };
    let pid: i32 = std::env::var("APPLICATIONLAUNCHER_TEST_VERSION_PIDS")
        .expect("supply an explicit fixture process PID")
        .parse()
        .unwrap();
    assert!(pid > 0);
    let process = crate::windows::read_process_stat(pid).unwrap();
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let identity = crate::models::ProcessIdentity {
        pid,
        start_ticks: stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap()
            .parse()
            .unwrap(),
    };
    let image = std::fs::metadata(format!("/proc/{pid}/exe")).unwrap();
    let current = || process_identity_is_current(identity, Some((image.dev(), image.ino())));
    let target = ProcessVersionTarget {
        pid,
        executable: std::fs::read_link(format!("/proc/{pid}/exe")).unwrap(),
        launch_context: ProcessLaunchContext {
            raw_argv0: process_argv0(pid),
            executable_unavailable: None,
        },
        program: Some(process.name),
        application_entry: None,
        window_declared_entry: false,
    };
    let mut version = timed("first-metadata-without-build-inputs", || {
        probe_version_deferred(&target, current)
    });
    assert!(current());
    assert!(version.version.is_some(), "{version:?}");
    assert!(version.latest_version.is_some(), "{version:?}");
    assert!(version.latest_development.is_some(), "{version:?}");
    let development = version.latest_development.as_ref().unwrap();
    eprintln!(
        "first-metadata: version={:?}, latest={:?}, languages={:?}, build={:?}, inputs={:?}",
        version.version,
        version.latest_version,
        development.source_languages,
        development.build_system,
        version.uncompiled_changes
    );
    if let Some(check) = version.source_check.take() {
        assert_eq!(
            version.uncompiled_changes.as_deref(),
            Some(freshness::PENDING)
        );
        let status = timed("deferred-build-inputs", || check.inspect());
        assert!(current());
        eprintln!("deferred-check-result: {status}");
    }
}

#[test]
#[ignore = "read-only stage benchmark; requires APPLICATIONLAUNCHER_TEST_VERSION_PIDS pointing to an explicitly selected fixture process"]
fn live_metadata_stage_timings() {
    let pid: i32 = std::env::var("APPLICATIONLAUNCHER_TEST_VERSION_PIDS")
        .expect("supply an explicit fixture process PID")
        .parse()
        .unwrap();
    let process_path = format!("/proc/{pid}/exe");
    let executable = clean_executable(&std::fs::read_link(&process_path).unwrap());
    let running = File::open(process_path).unwrap();
    let installed = timed("installation-resolution", || {
        installation::installed_process_executable(pid as u32, &executable, &running)
    })
    .unwrap();
    let disk = File::open(&installed.path).unwrap();
    let running_version = timed("running-version", || {
        probe_executable_context(&running, &executable, ExecutableContext::Process(pid))
    });
    timed("disk-version", || {
        let same_observation = installed.path == executable
            && executable_identity(&running.metadata().unwrap())
                == executable_identity(&disk.metadata().unwrap());
        disk_executable_version(&running_version, same_observation, || {
            probe_executable_context(&disk, &installed.path, ExecutableContext::OnDisk)
        })
    });
    let package = timed("installed-owner", || installed_package(&installed.path));
    let details = timed("installation-details", || {
        installation::inspect_with_owner_details(&installed.path, package.as_deref())
    });
    timed("package-target", || {
        program_metadata::package::availability_target_with_desktops(
            &installed.path,
            package.as_deref(),
            &details.desktops,
        )
    });
    let mut launch = timed("launch-chain", || {
        application::process_launch(&executable, Some(&installed), None)
    });
    timed("desktop-association", || {
        application::associate_installed_desktop_from_matches(&mut launch, &disk, &details.desktops)
    });
    for pass in [
        "source-inputs-cold",
        "source-inputs-warm",
        "source-inputs-warm-repeat",
    ] {
        let sources = timed(pass, || {
            freshness::uncompiled_changes(&installed.path, &disk)
        });
        eprintln!("source-check-result: {sources}");
    }
}
