//! Owned-process regressions for the caller-fd/target-fd attribution incident.
use super::*;
use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(super) struct DescriptorFixture {
    directory: PathBuf,
    pub(super) entry: PathBuf,
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    // Holding this file makes the caller's fd N name a recording, while the
    // child's same fd N names the script. No shared test-runner fd is replaced.
    decoy: File,
}

impl DescriptorFixture {
    pub(super) fn new(memfd: bool) -> Self {
        Self::with_alias(memfd, false)
    }

    pub(super) fn with_alias(memfd: bool, alias: bool) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "launcher-script-descriptor-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let decoy_path = directory.join("flight-recorder-gui.ring");
        std::fs::write(&decoy_path, "not a script or application\n").unwrap();
        let decoy = File::open(&decoy_path).unwrap();
        let descriptor = decoy.as_raw_fd();
        let entry = directory.join("tool.sh");
        let content = format!(
            "printf 'ready\\n'\nIFS= read -r action\ncase \"$action\" in\nclose) exec {descriptor}<&- ;;\nreuse) exec {descriptor}<\"$1\" ;;\nesac\nprintf 'changed\\n'\nIFS= read -r finish\n"
        );
        let mut script = if memfd {
            let name = c"pi-remote-qwen-radiance";
            let descriptor = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
            assert!(descriptor >= 0, "{}", std::io::Error::last_os_error());
            unsafe { File::from_raw_fd(descriptor) }
        } else {
            File::create(&entry).unwrap()
        };
        script.write_all(content.as_bytes()).unwrap();
        let source = script.as_raw_fd();
        let descriptor_target = format!("/proc/self/fd/{descriptor}");
        let target = if alias {
            let alias = directory.join("descriptor-entry");
            std::os::unix::fs::symlink(&descriptor_target, &alias).unwrap();
            alias
        } else {
            PathBuf::from(descriptor_target)
        };
        let mut command = Command::new("/usr/bin/bash");
        command
            .arg(target)
            .arg(&decoy_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // dup2 is async-signal-safe and changes only the new owned child.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(source, descriptor) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut fixture = Self {
            directory,
            entry,
            child,
            input,
            output,
            decoy,
        };
        fixture.message("ready");
        fixture
    }

    fn message(&mut self, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut poll = libc::pollfd {
                fd: self.output.get_ref().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut poll, 1, 50) };
            assert!(result >= 0, "{}", std::io::Error::last_os_error());
            if result > 0 {
                let mut message = String::new();
                self.output.read_line(&mut message).unwrap();
                assert_eq!(message.trim_end(), expected);
                return;
            }
            assert!(self.child.try_wait().unwrap().is_none());
            assert!(
                Instant::now() < deadline,
                "owned descriptor fixture timed out"
            );
        }
    }

    pub(super) fn application(&self) -> ScriptApplication {
        script::probe("Shell", self.child.id() as i32).unwrap()
    }

    pub(super) fn change_descriptor(&mut self, action: &str) {
        assert!(matches!(action, "close" | "reuse"));
        writeln!(self.input, "{action}").unwrap();
        self.input.flush().unwrap();
        self.message("changed");
    }

    fn target(&self) -> ProcessVersionTarget {
        let pid = self.child.id() as i32;
        ProcessVersionTarget {
            pid,
            executable: std::fs::read_link(format!("/proc/{pid}/exe")).unwrap(),
            launch_context: ProcessLaunchContext::default(),
            program: Some("bash".into()),
            application_entry: None,
            window_declared_entry: false,
        }
    }
}

impl Drop for DescriptorFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

#[test]
fn descriptor_script_metadata_uses_target_memfd_not_callers_recording() {
    let fixture = DescriptorFixture::new(true);
    let target = fixture.target();
    let version = probe_version(&target);
    let application = version.script_application.as_ref().unwrap();
    assert_eq!(application.name, "pi-remote-qwen-radiance");
    assert_eq!(
        application.target,
        format!("/proc/self/fd/{}", fixture.decoy.as_raw_fd())
    );
    assert!(application.descriptor.is_some());
    assert!(application.entry_point.is_none());
    let development = version.application_development.as_ref().unwrap();
    let retained_stamp = program_metadata::build_status::artifact::ArtifactStamp::from_metadata(
        &std::fs::metadata(application.descriptor.as_ref().unwrap().process_path()).unwrap(),
    );
    assert_eq!(development.artifact_stamp, Some(retained_stamp));
    assert!(application.unchanged());
    assert_eq!(version.runtime_name, Some("Shell"));
    assert!(
        version
            .version
            .as_deref()
            .is_some_and(|value| value.contains("bash"))
    );
    assert!(version.build_id.is_some());
    assert!(version.application_installed_metadata.is_none());
    assert!(version.application_installation.is_none());
    assert!(version.application_package_target.is_none());
    assert!(version.application_launch.is_none());
    let details = version.script_details.as_ref().unwrap();
    assert_eq!(details.storage, "Memory-backed file (memfd)");
    assert!(details.size_bytes > 0);
    assert!(details.sha256.is_some());
    assert!(details.unchanged());
}

#[test]
fn expanding_bash_descriptor_scripts_preserves_the_interpreters_verified_header_status() {
    for memfd in [false, true] {
        let fixture = DescriptorFixture::new(memfd);
        let target = fixture.target();
        let collapsed = probe_header_checked(&target, || true);
        let expanded = probe_version(&target);
        assert!(collapsed.development.is_none());
        assert!(collapsed.latest_development.is_none());
        assert!(collapsed.application_development.is_none());
        assert!(expanded.development.is_some());
        assert!(expanded.latest_development.is_some());
        assert!(expanded.application_development.is_some());
        assert_eq!(collapsed.running_latest_binary, Some(true));
        assert_eq!(expanded.running_latest_binary, Some(true));
        assert!(collapsed.runtime_name.is_none());
        assert_eq!(expanded.runtime_name, Some("Shell"));
        assert_eq!(collapsed.process_header_is_current(), Some(true));
        assert_eq!(expanded.process_header_is_current(), Some(true));
        // Current Bash is not proof that its hosted script is current.
        assert_eq!(expanded.process_build_is_current(), None);
    }
}

#[test]
fn changed_wrapper_comparison_discards_only_details_not_script_or_runtime_identity() {
    let fixture = DescriptorFixture::new(true);
    let application = fixture.application();
    let candidate = fixture.directory.join("comparison-only.sh");
    std::fs::write(&candidate, "#!/bin/bash\necho candidate\n").unwrap();
    let details = application
        .descriptor
        .as_ref()
        .unwrap()
        .inspect_details(Some(&candidate), Instant::now() + Duration::from_secs(1))
        .unwrap();
    let mut version = ProcessVersion {
        runtime_name: Some("Shell"),
        version: Some("independent Bash version".into()),
        script_application: Some(application),
        script_details: Some(details),
        ..Default::default()
    };
    assert!(!version.discard_changed_script_metadata());
    assert!(version.script_details.is_some());
    std::fs::write(&candidate, "#!/bin/bash\necho changed wrapper\n").unwrap();
    assert!(!version.discard_changed_script_metadata());
    assert!(version.script_details.is_none());
    assert!(
        version
            .script_details_error
            .as_deref()
            .unwrap()
            .contains("candidate changed")
    );
    assert!(version.script_application.as_ref().unwrap().unchanged());
    assert!(version.application_metadata_error.is_none());
    assert_eq!(version.version.as_deref(), Some("independent Bash version"));
    assert!(version.application_launch.is_none());
    assert!(version.application_installation.is_none());
}

#[test]
fn descriptor_script_changes_before_publication_discard_only_application_metadata() {
    for action in ["close", "reuse"] {
        let fixture = RefCell::new(DescriptorFixture::new(false));
        let target = fixture.borrow().target();
        let calls = Cell::new(0);
        let version = probe_version_checked(&target, || {
            let call = calls.get();
            calls.set(call + 1);
            // Bash identity is checked on entry and again immediately before
            // publication. Change only its script fd at the latter boundary.
            if call == 1 {
                fixture.borrow_mut().change_descriptor(action);
            }
            std::fs::metadata(format!("/proc/{}/exe", target.pid)).is_ok()
        });
        assert_eq!(calls.get(), 2);
        assert!(version.script_application.is_none(), "{version:?}");
        assert!(version.application_development.is_none());
        assert!(version.development.is_some());
        assert!(version.application_installed_metadata.is_none());
        assert!(version.application_installation.is_none());
        assert!(version.application_package_target.is_none());
        assert!(version.application_launch.is_none());
        assert!(version.python_context.is_none());
        assert!(
            version
                .application_metadata_error
                .as_deref()
                .is_some_and(|reason| reason.contains("descriptor"))
        );
        assert!(version.running_unavailable.is_none());
        assert_eq!(version.runtime_name, Some("Shell"));
        assert!(
            version
                .version
                .as_deref()
                .is_some_and(|value| value.contains("bash"))
        );
        assert!(version.build_id.is_some());
    }
}
