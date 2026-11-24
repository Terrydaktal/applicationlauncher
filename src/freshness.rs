//! Passive, daemon-owned native executable currency. No programs are executed.
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use program_metadata::build_status::artifact::ArtifactStamp;
use serde::{Deserialize, Serialize};

pub const PATH: &str = "/Freshness";
pub const INTERFACE: &str = "com.terrydaktal.ApplicationLauncher.Freshness1";
const POLL: Duration = Duration::from_secs(2);
const LEASE: Duration = Duration::from_secs(60);
const INSTALLATION_RECHECK: Duration = Duration::from_secs(30);
const LIMIT: usize = 512;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub struct Target {
    pub pid: u32,
    pub start_time: u64,
    pub image_dev: u64,
    pub image_ino: u64,
    pub launch_path: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Unknown,
    Current,
    Outdated,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Report {
    pub schema_version: u32,
    pub generation: u64,
    pub target: Target,
    pub status: Status,
    pub reason: String,
    pub start_time: u64,
    pub image_dev: u64,
    pub image_ino: u64,
    pub running_build_id: Option<String>,
    pub current_build_id: Option<String>,
}

impl Report {
    fn unknown(target: &Target, reason: impl Into<String>) -> Self {
        Self {
            schema_version: 1,
            generation: 0,
            target: target.clone(),
            status: Status::Unknown,
            reason: reason.into(),
            start_time: target.start_time,
            image_dev: target.image_dev,
            image_ino: target.image_ino,
            running_build_id: None,
            current_build_id: None,
        }
    }
}

/// Shared comparison policy for the daemon and the launcher's metadata adapter.
pub fn same_native_build(
    running: Option<&str>,
    current: Option<&str>,
    same_file: bool,
) -> Option<bool> {
    if same_file {
        Some(true)
    } else {
        Some(running? == current?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Image {
    pid: u32,
    start: u64,
    dev: u64,
    ino: u64,
    executable: PathBuf,
}

fn image(pid: u32) -> Result<(Image, File), String> {
    let root = PathBuf::from(format!("/proc/{pid}"));
    if fs::metadata(&root)
        .map_err(|error| error.to_string())?
        .uid()
        != unsafe { libc::geteuid() }
    {
        return Err("The process is not owned by this user".into());
    }
    let mut stat = Vec::new();
    File::open(root.join("stat"))
        .and_then(|file| file.take(8193).read_to_end(&mut stat))
        .map_err(|error| error.to_string())?;
    if stat.len() > 8192 {
        return Err("Oversized process identity".into());
    }
    let stat = std::str::from_utf8(&stat).map_err(|error| error.to_string())?;
    let fields: Vec<_> = stat
        .rsplit_once(") ")
        .ok_or("Invalid process identity")?
        .1
        .split_whitespace()
        .collect();
    if fields.len() < 20 || matches!(fields[0], "Z" | "X") {
        return Err("The process is not running".into());
    }
    let start: u64 = fields[19]
        .parse()
        .map_err(|_| "Invalid process start time")?;
    let executable = fs::read_link(root.join("exe")).map_err(|error| error.to_string())?;
    let executable = executable.to_str().ok_or("Non-UTF-8 executable path")?;
    let executable = PathBuf::from(executable.strip_suffix(" (deleted)").unwrap_or(executable));
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(root.join("exe"))
        .map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || start == 0 {
        return Err("Invalid native process image".into());
    }
    Ok((
        Image {
            pid,
            start,
            dev: metadata.dev(),
            ino: metadata.ino(),
            executable,
        },
        file,
    ))
}

#[derive(Default)]
struct Inspector {
    images: HashMap<Image, Option<String>>,
    disks: HashMap<PathBuf, (ArtifactStamp, Option<String>)>,
    approved: HashMap<Target, Image>,
    installations: HashMap<Image, InstallationRoute>,
}

struct InstallationRoute {
    checked: Instant,
    launcher: Option<PathBuf>,
    ambiguous: bool,
}

impl Inspector {
    fn published_route(&mut self, running: &Image, file: &File) -> &InstallationRoute {
        if self.installations.len() >= LIMIT && !self.installations.contains_key(running) {
            self.installations.clear();
        }
        if self
            .installations
            .get(running)
            .is_none_or(|route| route.checked.elapsed() >= INSTALLATION_RECHECK)
        {
            let installed = program_metadata::installation::installed_process_executable(
                running.pid,
                &running.executable,
                file,
            );
            self.installations.insert(
                running.clone(),
                InstallationRoute {
                    checked: Instant::now(),
                    launcher: installed
                        .as_ref()
                        .filter(|installed| installed.is_published())
                        .map(|installed| installed.launcher().to_owned()),
                    ambiguous: installed.is_none(),
                },
            );
        }
        &self.installations[running]
    }

    fn inspect(&mut self, target: &Target) -> Report {
        self.try_inspect(target)
            .unwrap_or_else(|reason| Report::unknown(target, reason))
    }

    fn try_inspect(&mut self, target: &Target) -> Result<Report, String> {
        let (running, file) = image(target.pid)?;
        if (target.start_time != 0 && running.start != target.start_time)
            || (target.image_dev != 0 && running.dev != target.image_dev)
            || (target.image_ino != 0 && running.ino != target.image_ino)
        {
            return Err("The process identity changed".into());
        }
        if self.images.len() >= LIMIT && !self.images.contains_key(&running) {
            self.images.clear();
        }
        let running_id = self
            .images
            .entry(running.clone())
            .or_insert_with(|| program_metadata::executable::elf_build_id(&file))
            .clone()
            .ok_or("The running image has no qualified ELF build ID")?;

        let direct_image =
            target.launch_path.is_empty() || Path::new(&target.launch_path) == running.executable;
        let association = self.published_route(&running, &file);
        if direct_image && association.ambiguous {
            return Err("The current published installation is ambiguous".into());
        }
        let published = association.launcher.clone();
        // A retained image is not the current installation. Resolve its verified
        // publication again for a new PID, even when the terminal supplied the image path.
        let launch = if direct_image {
            published
                .clone()
                .unwrap_or_else(|| running.executable.clone())
        } else {
            PathBuf::from(&target.launch_path)
        };
        let launch = if launch
            .file_name()
            .is_some_and(|name| name == "run-patched-xfce4-terminal.sh")
        {
            fs::canonicalize(&launch)
                .map_err(|error| error.to_string())?
                .parent()
                .ok_or("Invalid terminal wrapper")?
                .join("build/terminal/xfce4-terminal")
        } else {
            launch
        };
        let disk_path = fs::canonicalize(&launch).map_err(|error| error.to_string())?;
        let disk = File::options()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&disk_path)
            .map_err(|error| error.to_string())?;
        let metadata = disk.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.mode() & 0o111 == 0 {
            return Err("The current launch target is not an executable regular file".into());
        }
        let stamp = ArtifactStamp::from_metadata(&metadata);
        let same_file = running.dev == metadata.dev() && running.ino == metadata.ino();
        let published_target = published
            .as_ref()
            .and_then(|path| fs::canonicalize(path).ok())
            .is_some_and(|path| path == disk_path);
        if self.approved.get(target) != Some(&running)
            && !same_file
            && disk_path != running.executable
            && !published_target
        {
            return Err("The launch path was not verified against the running image".into());
        }
        if self.approved.len() >= LIMIT && !self.approved.contains_key(target) {
            self.approved.clear();
        }
        self.approved.insert(target.clone(), running.clone());
        if self.disks.len() >= LIMIT && !self.disks.contains_key(&disk_path) {
            self.disks.clear();
        }
        let current_id = match self.disks.get(&disk_path) {
            Some((previous, id)) if previous == &stamp => id.clone(),
            _ => {
                let id = program_metadata::executable::elf_build_id(&disk);
                self.disks
                    .insert(disk_path.clone(), (stamp.clone(), id.clone()));
                id
            }
        }
        .ok_or("The current launch target has no qualified ELF build ID")?;
        let (after, _) = image(target.pid)?;
        if after != running
            || disk
                .metadata()
                .map(|value| ArtifactStamp::from_metadata(&value))
                .ok()
                != Some(stamp.clone())
            || fs::metadata(&disk_path)
                .map(|value| ArtifactStamp::from_metadata(&value))
                .ok()
                != Some(stamp)
            || fs::canonicalize(&launch).ok().as_ref() != Some(&disk_path)
        {
            return Err("The process or launch target changed during inspection".into());
        }
        let current = same_native_build(Some(&running_id), Some(&current_id), same_file).unwrap();
        Ok(Report {
            schema_version: 1,
            generation: 0,
            target: target.clone(),
            status: if current {
                Status::Current
            } else {
                Status::Outdated
            },
            reason: if published_target {
                "Compared pinned running image with the verified published installation"
            } else {
                "Compared pinned running and current local ELF images"
            }
            .into(),
            start_time: running.start,
            image_dev: running.dev,
            image_ino: running.ino,
            running_build_id: Some(running_id),
            current_build_id: Some(current_id),
        })
    }
}

struct Entry {
    report: Report,
    requested: Instant,
}
#[derive(Default)]
struct State {
    entries: HashMap<Target, Entry>,
    stopping: bool,
    dirty: bool,
    generation: u64,
}

#[derive(Clone, Default)]
pub struct Service {
    shared: Arc<(Mutex<State>, Condvar)>,
}

#[zbus::interface(name = "com.terrydaktal.ApplicationLauncher.Freshness1")]
impl Service {
    /// Cache lookup/enqueue only. Filesystem inspection never runs on the bus dispatcher.
    fn get_process_status(
        &self,
        pid: u32,
        start_time: u64,
        image_dev: u64,
        image_ino: u64,
        launch_path: &str,
    ) -> String {
        let target = Target {
            pid,
            start_time,
            image_dev,
            image_ino,
            launch_path: if launch_path.len() <= 4096 {
                launch_path.into()
            } else {
                String::new()
            },
        };
        if pid <= 1
            || pid > i32::MAX as u32
            || launch_path.len() > 4096
            || launch_path.contains('\0')
            || (!launch_path.is_empty() && !Path::new(launch_path).is_absolute())
        {
            return serde_json::to_string(&Report::unknown(
                &target,
                "Invalid process status request",
            ))
            .unwrap();
        }
        let (mutex, wake) = &*self.shared;
        let mut state = mutex.lock().unwrap();
        if state.entries.len() >= LIMIT && !state.entries.contains_key(&target) {
            if let Some(oldest) = state
                .entries
                .iter()
                .min_by_key(|(_, value)| value.requested)
                .map(|(key, _)| key.clone())
            {
                state.entries.remove(&oldest);
            }
        }
        let missing = !state.entries.contains_key(&target);
        if missing {
            state.generation += 1;
            let mut report = Report::unknown(&target, "Inspection pending");
            report.generation = state.generation;
            state.entries.insert(
                target.clone(),
                Entry {
                    report,
                    requested: Instant::now(),
                },
            );
        }
        let entry = state.entries.get_mut(&target).unwrap();
        entry.requested = Instant::now();
        let result = serde_json::to_string(&entry.report).unwrap();
        if missing {
            state.dirty = true;
            wake.notify_one();
        }
        result
    }

    #[zbus(signal)]
    async fn process_status_changed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        report: &str,
    ) -> zbus::Result<()>;
}

pub struct Worker {
    shared: Arc<(Mutex<State>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        let (mutex, wake) = &*self.shared;
        mutex.lock().unwrap().stopping = true;
        wake.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Service {
    pub fn start(&self, connection: zbus::Connection) -> std::io::Result<Worker> {
        let shared = self.shared.clone();
        let thread = std::thread::Builder::new()
            .name("process-freshness".into())
            .spawn(move || {
                let mut inspector = Inspector::default();
                let connection = zbus::blocking::Connection::from(connection);
                loop {
                    let (mutex, wake) = &*shared;
                    let state = mutex.lock().unwrap();
                    if state.stopping {
                        break;
                    }
                    let mut state = if state.dirty {
                        state
                    } else if state.entries.is_empty() {
                        wake.wait(state).unwrap()
                    } else {
                        wake.wait_timeout(state, POLL).unwrap().0
                    };
                    if state.stopping {
                        break;
                    }
                    state.dirty = false;
                    state
                        .entries
                        .retain(|_, value| value.requested.elapsed() < LEASE);
                    let keys: Vec<_> = state.entries.keys().cloned().collect();
                    drop(state);
                    for key in keys {
                        let mut report = inspector.inspect(&key);
                        let mut state = mutex.lock().unwrap();
                        let changed = state.entries.get(&key).is_some_and(|entry| {
                            report.generation = entry.report.generation;
                            entry.report != report
                        });
                        if changed {
                            state.generation += 1;
                            report.generation = state.generation;
                            state.entries.get_mut(&key).unwrap().report = report.clone();
                        }
                        drop(state);
                        if changed {
                            let json = serde_json::to_string(&report).unwrap();
                            let _ = connection.emit_signal(
                                None::<&str>,
                                PATH,
                                INTERFACE,
                                "ProcessStatusChanged",
                                &(json,),
                            );
                        }
                    }
                }
            })?;
        Ok(Worker {
            shared: self.shared.clone(),
            thread: Some(thread),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    struct Fixture {
        root: PathBuf,
        child: Child,
        target: Target,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "freshness-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ));
            fs::create_dir(&root).unwrap();
            let source = root.join("child.c");
            fs::write(
                &source,
                "#include <unistd.h>\nint main(void) { for (;;) pause(); }\n",
            )
            .unwrap();
            for (name, id) in [("one", "11111111"), ("two", "22222222")] {
                let result = Command::new("cc")
                    .arg(&source)
                    .arg(format!("-Wl,--build-id=0x{id}"))
                    .arg("-o")
                    .arg(root.join(name))
                    .output()
                    .unwrap();
                assert!(
                    result.status.success(),
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
            }
            let launch = root.join("launch");
            symlink(root.join("one"), &launch).unwrap();
            let child = Command::new(&launch).spawn().unwrap();
            let (running, _) = image(child.id()).unwrap();
            let target = Target {
                pid: child.id(),
                start_time: running.start,
                image_dev: running.dev,
                image_ino: running.ino,
                launch_path: launch.to_str().unwrap().into(),
            };
            Self {
                root,
                child,
                target,
            }
        }

        fn retarget(&self, name: &str) {
            let next = self.root.join("next-link");
            symlink(self.root.join(name), &next).unwrap();
            fs::rename(next, self.root.join("launch")).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn comparison_requires_native_identity_not_version_text() {
        assert_eq!(same_native_build(None, None, true), Some(true));
        assert_eq!(same_native_build(None, Some("a"), false), None);
        assert_eq!(same_native_build(Some("a"), None, false), None);
        assert_eq!(same_native_build(Some("a"), Some("a"), false), Some(true));
        assert_eq!(same_native_build(Some("a"), Some("b"), false), Some(false));
    }

    #[test]
    fn retained_binary_restart_keeps_the_published_installation_comparison() {
        if std::env::var_os("FRESHNESS_PUBLISHED_RESTART_TEST").is_none() {
            let cache = std::env::temp_dir()
                .join(format!("freshness-published-cache-{}", std::process::id()));
            fs::create_dir_all(&cache).unwrap();
            let status = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "freshness::tests::retained_binary_restart_keeps_the_published_installation_comparison", "--nocapture"])
                .env("FRESHNESS_PUBLISHED_RESTART_TEST", "1")
                .env("XDG_CACHE_HOME", &cache)
                .status().unwrap();
            fs::remove_dir_all(cache).unwrap();
            assert!(status.success());
            return;
        }

        let mut fixture = Fixture::new();
        let old = fixture.root.join("old/codex");
        let latest = fixture.root.join("latest/codex");
        let launch = fixture.root.join("bin/codex");
        for path in [&old, &latest, &launch] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
        }
        fs::copy(fixture.root.join("one"), &old).unwrap();
        fs::copy(fixture.root.join("two"), &latest).unwrap();
        symlink(&old, &launch).unwrap();
        let inventory =
            PathBuf::from(std::env::var_os("XDG_CACHE_HOME").unwrap()).join("progs/scan-v1.json");
        fs::create_dir_all(inventory.parent().unwrap()).unwrap();
        fs::write(
            &inventory,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 4, "scan": {"apps": [{
                    "name": "codex", "origin": "Local", "install_role": "Standalone",
                    "state": {"binary": true, "fork": true},
                    "binaries": [{"name": "codex", "path": launch, "target": old}]
                }]}
            }))
            .unwrap(),
        )
        .unwrap();
        fixture.child.kill().unwrap();
        fixture.child.wait().unwrap();
        fixture.child = Command::new(&launch).arg0("codex").spawn().unwrap();
        let first = image(fixture.child.id()).unwrap().0;
        fixture.target = Target {
            pid: first.pid,
            start_time: first.start,
            image_dev: first.dev,
            image_ino: first.ino,
            launch_path: launch.to_str().unwrap().into(),
        };
        let mut inspector = Inspector::default();
        assert_eq!(inspector.inspect(&fixture.target).status, Status::Current);
        let next = fixture.root.join("next-published");
        symlink(&latest, &next).unwrap();
        fs::rename(next, &launch).unwrap();
        assert_eq!(inspector.inspect(&fixture.target).status, Status::Outdated);

        let pinned = File::open(format!("/proc/{}/exe", fixture.child.id())).unwrap();
        use std::os::fd::AsRawFd;
        let pinned_path = format!("/proc/{}/fd/{}", std::process::id(), pinned.as_raw_fd());
        fixture.child.kill().unwrap();
        fixture.child.wait().unwrap();
        fixture.child = Command::new(pinned_path).arg0("codex").spawn().unwrap();
        let restarted = image(fixture.child.id()).unwrap().0;
        assert_eq!((first.dev, first.ino), (restarted.dev, restarted.ino));
        fixture.target = Target {
            pid: restarted.pid,
            start_time: restarted.start,
            image_dev: restarted.dev,
            image_ino: restarted.ino,
            launch_path: old.to_str().unwrap().into(),
        };
        let mut inspector = Inspector::default();
        let report = inspector.inspect(&fixture.target);
        assert_eq!(report.status, Status::Outdated, "{report:?}");
        assert_eq!(report.running_build_id.as_deref(), Some("11111111"));
        assert_eq!(report.current_build_id.as_deref(), Some("22222222"));
        fixture.target.launch_path = launch.to_str().unwrap().into();
        assert_eq!(inspector.inspect(&fixture.target).status, Status::Outdated);
        fixture.target.launch_path = fixture.root.join("two").to_str().unwrap().into();
        assert_eq!(inspector.inspect(&fixture.target).status, Status::Unknown);
    }

    #[test]
    fn native_symlink_update_and_rollback() {
        let fixture = Fixture::new();
        let mut inspector = Inspector::default();
        let first = inspector.inspect(&fixture.target);
        assert_eq!(first.status, Status::Current, "{first:?}");
        assert_eq!(first.running_build_id.as_deref(), Some("11111111"));
        fixture.retarget("two");
        let updated = inspector.inspect(&fixture.target);
        assert_eq!(updated.status, Status::Outdated, "{updated:?}");
        assert_eq!(updated.current_build_id.as_deref(), Some("22222222"));
        fixture.retarget("one");
        assert_eq!(inspector.inspect(&fixture.target).status, Status::Current);
    }

    #[test]
    fn identical_build_new_inode_is_current() {
        let fixture = Fixture::new();
        let mut inspector = Inspector::default();
        assert_eq!(inspector.inspect(&fixture.target).status, Status::Current);
        fs::copy(fixture.root.join("one"), fixture.root.join("copy")).unwrap();
        fixture.retarget("copy");
        let report = inspector.inspect(&fixture.target);
        assert_eq!(report.status, Status::Current, "{report:?}");
    }

    #[test]
    fn unverified_launch_path_and_changed_identity_are_unknown() {
        let fixture = Fixture::new();
        let mut inspector = Inspector::default();
        let mut target = fixture.target.clone();
        target.launch_path = fixture.root.join("two").to_str().unwrap().into();
        let unverified = inspector.inspect(&target);
        assert_eq!(unverified.status, Status::Unknown);
        assert!(unverified.reason.contains("not verified"));
        for field in ["start", "dev", "ino"] {
            let mut target = fixture.target.clone();
            match field {
                "start" => target.start_time += 1,
                "dev" => target.image_dev += 1,
                _ => target.image_ino += 1,
            }
            let report = inspector.inspect(&target);
            assert_eq!(report.status, Status::Unknown);
            assert!(report.reason.contains("identity changed"));
        }
    }

    #[test]
    fn missing_non_executable_and_script_targets_are_passive_unknown() {
        let fixture = Fixture::new();
        let mut inspector = Inspector::default();
        assert_eq!(inspector.inspect(&fixture.target).status, Status::Current);
        let marker = fixture.root.join("must-not-be-created");
        let script = fixture.root.join("script");
        fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        fixture.retarget("script");
        let report = inspector.inspect(&fixture.target);
        assert_eq!(report.status, Status::Unknown, "{report:?}");
        assert!(!marker.exists(), "Inspection executed a script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(inspector.inspect(&fixture.target).status, Status::Unknown);
        fixture.retarget("missing");
        assert_eq!(inspector.inspect(&fixture.target).status, Status::Unknown);
    }

    #[test]
    fn dead_process_is_unknown() {
        let mut fixture = Fixture::new();
        fixture.child.kill().unwrap();
        fixture.child.wait().unwrap();
        assert_eq!(
            Inspector::default().inspect(&fixture.target).status,
            Status::Unknown
        );
    }

    #[test]
    fn requests_only_enqueue_bounded_versioned_cache_entries() {
        let service = Service::default();
        for offset in 0..LIMIT + 2 {
            let report: Report = serde_json::from_str(&service.get_process_status(
                (offset + 2) as u32,
                9,
                10,
                11,
                "/missing",
            ))
            .unwrap();
            assert_eq!(report.status, Status::Unknown);
            assert_eq!(report.generation, offset as u64 + 1);
        }
        let state = service.shared.0.lock().unwrap();
        assert_eq!(state.entries.len(), LIMIT);
        assert!(state.dirty);
        drop(state);
        let cached: Report = serde_json::from_str(&service.get_process_status(
            (LIMIT + 3) as u32,
            9,
            10,
            11,
            "/missing",
        ))
        .unwrap();
        assert_eq!(cached.generation, LIMIT as u64 + 2);
        for (pid, path) in [(0, "/absolute"), (2, "relative"), (2, "/a\0b")] {
            let invalid: Report =
                serde_json::from_str(&service.get_process_status(pid, 0, 0, 0, path)).unwrap();
            assert_eq!(invalid.status, Status::Unknown);
            assert_eq!(invalid.generation, 0);
        }
        let oversized = service.get_process_status(2, 0, 0, 0, &"x".repeat(65536));
        assert!(
            oversized.len() < 1024,
            "Invalid requests must not be echoed unboundedly"
        );
        assert_eq!(service.shared.0.lock().unwrap().entries.len(), LIMIT);
    }
}
