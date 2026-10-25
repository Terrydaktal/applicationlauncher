use std::collections::HashMap;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock, mpsc::Receiver};

use crate::models::WindowInfo;
use eframe::egui;
use std::time::{Duration, Instant};

mod available;
mod freshness;
mod installation;
mod lookup;
mod node;
mod pi;
mod script;

pub(crate) use available::PackageTarget;
pub(crate) use installation::ProgsMetadata;
pub(crate) use lookup::PI_RECEIPT_REFRESH;
pub(crate) use lookup::ProcessVersionCache;
pub(crate) use node::NodeApplication;
pub(crate) use pi::{BuildStatus as PiBuildStatus, PiBuildEvidence};
pub(crate) use script::{ScriptApplication, runtime_name};

const PROBE_TIMEOUT: Duration = Duration::from_millis(750);
const CACHE_TTL: Duration = Duration::from_secs(300);
const MAX_CACHE_ENTRIES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessVersionTarget {
    pid: i32,
    executable: PathBuf,
    program: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowVersionTarget {
    id: String,
    owner: Option<ProcessVersionTarget>,
    active: Option<ProcessVersionTarget>,
}

impl WindowVersionTarget {
    pub(crate) fn for_window(window: &WindowInfo) -> Self {
        let owner = window
            .pid
            .zip(window.exe_path.clone())
            .map(|(pid, executable)| ProcessVersionTarget {
                pid,
                executable,
                program: None,
            });
        let active = crate::windows::is_terminal_class(&window.class.to_lowercase())
            .then(|| window.process_chain.first())
            .flatten()
            .filter(|_| window.active_process.is_some())
            .and_then(|entry| {
                entry
                    .exe_path
                    .clone()
                    .map(|executable| ProcessVersionTarget {
                        pid: entry.pid,
                        executable,
                        program: Some(entry.name.clone()),
                    })
            });
        Self {
            id: window.id.clone(),
            owner,
            active,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ProcessVersion {
    pub(crate) running_unavailable: Option<&'static str>,
    pub(crate) runtime_name: Option<&'static str>,
    pub(crate) application: Option<NodeApplication>,
    pub(crate) script_application: Option<ScriptApplication>,
    pub(crate) application_build: Option<PiBuildEvidence>,
    pub(crate) application_installation: Option<String>,
    pub(crate) application_progs_metadata: ProgsMetadata,
    pub(crate) application_package_target: Option<PackageTarget>,
    pub(crate) application_latest_package_version: Option<String>,
    pub(crate) source: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) latest_version: Option<String>,
    pub(crate) package_target: Option<PackageTarget>,
    pub(crate) latest_package_version: Option<String>,
    pub(crate) latest_build_id: Option<String>,
    pub(crate) running_latest_binary: Option<bool>,
    pub(crate) uncompiled_changes: Option<String>,
    pub(crate) package: Option<String>,
    pub(crate) installation: Option<String>,
    pub(crate) progs_metadata: ProgsMetadata,
    pub(crate) build_id: Option<String>,
}

impl ProcessVersion {
    fn discard_running_metadata(&mut self, reason: &'static str) {
        self.running_unavailable = Some(reason);
        self.version = None;
        self.build_id = None;
        self.running_latest_binary = None;
        self.application = None;
        self.script_application = None;
        self.application_build = None;
        self.application_installation = None;
        self.application_progs_metadata = ProgsMetadata::default();
        self.application_package_target = None;
        self.application_latest_package_version = None;
    }
}

#[derive(Clone, Debug, Default)]
struct ExecutableVersion {
    version: Option<String>,
    build_id: Option<String>,
}

type ExecutableIdentity = (u64, u64, u64, i64, i64, i64, i64);

fn executable_identity(metadata: &std::fs::Metadata) -> ExecutableIdentity {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

fn same_executable_build(
    running: &ExecutableVersion,
    latest: &ExecutableVersion,
    same_file: bool,
) -> Option<bool> {
    if same_file {
        return Some(true);
    }
    // Version strings and Git revisions can stay identical across dirty builds.
    Some(running.build_id.as_ref()? == latest.build_id.as_ref()?)
}

#[derive(Default)]
pub(crate) struct WindowVersions {
    pub(crate) owner: ProcessVersion,
    pub(crate) active: ProcessVersion,
}

pub(crate) struct WindowVersionLookup {
    pub(crate) target: WindowVersionTarget,
    receiver: Receiver<WindowVersions>,
    pub(crate) result: Option<WindowVersions>,
}

impl WindowVersionLookup {
    pub(crate) fn start(target: WindowVersionTarget, ctx: egui::Context) -> Self {
        let (tx, receiver) = std::sync::mpsc::sync_channel(1);
        let request = target.clone();
        applicationlauncher::observability::spawn_named("window-version-info", move |worker| {
            worker.set_state("probing");
            let result = WindowVersions {
                owner: request
                    .owner
                    .as_ref()
                    .map(probe_version)
                    .unwrap_or_default(),
                active: request
                    .active
                    .as_ref()
                    .map(probe_version)
                    .unwrap_or_default(),
            };
            let _ = tx.send(result);
            ctx.request_repaint();
            ctx.request_repaint_of(egui::ViewportId::from_hash_of(
                "launcher_process_chain_popup",
            ));
        });
        Self {
            target,
            receiver,
            result: None,
        }
    }

    pub(crate) fn poll(&mut self) {
        if self.result.is_none() {
            match self.receiver.try_recv() {
                Ok(result) => self.result = Some(result),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.result = Some(WindowVersions::default())
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
    }
}

fn clean_executable(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().trim_end_matches(" (deleted)"))
}

fn version_argument(name: &str) -> Option<&'static str> {
    // Only known noninteractive switches: unknown apps must never be launched
    // merely to inspect them. Package metadata remains available for those apps.
    match name {
        "tmux" => Some("-V"),
        "java" => Some("-version"),
        "xfce4-terminal" | "fish" | "bash" | "zsh" | "bwrap" | "codex" | "codex-code-mode"
        | "htop" | "nvtop" | "python" | "python3" | "node" | "firefox" | "dolphin" | "pcmanfm"
        | "mousepad" | "copyq" | "konsole" | "alacritty" | "kitty" | "mpv" | "vim" | "nvim" => {
            Some("--version")
        }
        name if runtime_name(name) == Some("Python") => Some("--version"),
        _ => None,
    }
}

fn version_line(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    text.lines()
        .map(str::trim)
        .find(|line| {
            !line.is_empty()
                && line.len() <= 512
                && line.chars().any(|ch| ch.is_ascii_digit())
                && !line.chars().any(|ch| ch.is_control())
        })
        .map(str::to_owned)
}

fn valid_package_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"@._+-".contains(&byte))
}

fn bounded_output(command: Command) -> Option<Vec<u8>> {
    let output = applicationlauncher::process::output_with_timeout(command, PROBE_TIMEOUT).ok()?;
    if !output.status.success() || output.stdout.len() + output.stderr.len() > 64 * 1024 {
        return None;
    }
    let mut bytes = output.stdout;
    bytes.push(b'\n');
    bytes.extend(output.stderr);
    Some(bytes)
}

fn installed_package(path: &Path) -> Option<String> {
    let pacman = applicationlauncher::process::executable_path("pacman")?;
    let mut owner = Command::new(&pacman);
    owner.args(["-Qqo", "--"]).arg(path).env("LC_ALL", "C");
    let output = bounded_output(owner)?;
    let name = std::str::from_utf8(&output).ok()?.trim();
    if name.is_empty()
        || name.len() > 256
        || !name
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || b"@._+-".contains(&ch))
    {
        return None;
    }
    let mut version = Command::new(pacman);
    version.args(["-Q", "--", name]).env("LC_ALL", "C");
    version_line(&bounded_output(version)?)
}

fn is_elf(file: &File) -> bool {
    let mut magic = [0; 4];
    file.metadata().is_ok_and(|metadata| metadata.is_file())
        && file.read_exact_at(&mut magic, 0).is_ok()
        && magic == *b"\x7fELF"
}

fn probe_executable(file: &File, name: &str) -> ExecutableVersion {
    let Ok(identity) = file.metadata() else {
        return ExecutableVersion::default();
    };
    if !is_elf(file) {
        return ExecutableVersion::default();
    }
    type CacheKey = (String, ExecutableIdentity);
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, (Instant, ExecutableVersion)>>> =
        OnceLock::new();
    let key = (name.to_owned(), executable_identity(&identity));
    let cache = CACHE.get_or_init(Default::default);
    if let Ok(cache) = cache.lock()
        && let Some((_, result)) = cache
            .get(&key)
            .filter(|(checked, _)| checked.elapsed() < CACHE_TTL)
    {
        return result.clone();
    }
    // Both images are pinned independently: replacing the on-disk binary must
    // not change the reported running version or reuse its cached disk version.
    let pinned_path = format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd());
    let mut result = ExecutableVersion::default();
    if let Some(argument) = version_argument(name) {
        let mut command = Command::new(&pinned_path);
        command.arg0(name).arg(argument).env("LC_ALL", "C");
        if name == "java" {
            // Version inspection must not load agents from the launcher's environment.
            for variable in [
                "JAVA_TOOL_OPTIONS",
                "JDK_JAVA_OPTIONS",
                "_JAVA_OPTIONS",
                "CLASSPATH",
            ] {
                command.env_remove(variable);
            }
        }
        if let Some(version) = bounded_output(command).as_deref().and_then(version_line) {
            result.version = Some(version);
        }
    }
    if let Some(readelf) = applicationlauncher::process::executable_path("readelf") {
        let mut command = Command::new(readelf);
        command.args(["-n", &pinned_path]).env("LC_ALL", "C");
        result.build_id = bounded_output(command).and_then(|bytes| {
            String::from_utf8(bytes).ok()?.lines().find_map(|line| {
                let id = line.trim().strip_prefix("Build ID: ")?;
                (id.len() <= 128
                    && !id.is_empty()
                    && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .then(|| id.to_owned())
            })
        });
    }
    if let Ok(mut cache) = cache.lock() {
        cache.retain(|_, (checked, _)| checked.elapsed() < CACHE_TTL);
        if cache.len() >= MAX_CACHE_ENTRIES {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (checked, _))| *checked)
                .map(|(key, _)| key.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(key, (Instant::now(), result.clone()));
    }
    result
}

fn probe_version(target: &ProcessVersionTarget) -> ProcessVersion {
    probe_version_checked(target, || true)
}

fn probe_version_checked(
    target: &ProcessVersionTarget,
    identity_is_current: impl Fn() -> bool,
) -> ProcessVersion {
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
    let name = clean_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let running_info = running
        .as_ref()
        .map(|(file, _)| probe_executable(file, name))
        .unwrap_or_default();
    // A running immutable build may outlive a retargeted installation symlink.
    // Pin its current installed target separately, without changing running identity.
    let installed = installation::installed_executable(&clean_path);
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
    let latest_info = latest.as_ref().map(|file| probe_executable(file, name));
    let running_latest_binary = latest_info.as_ref().and_then(|latest| {
        let (_, identity) = running.as_ref().ok()?;
        let same_file = latest_identity
            .as_ref()
            .is_some_and(|disk| disk.dev() == identity.dev() && disk.ino() == identity.ino());
        same_executable_build(&running_info, latest, same_file)
    });
    let uncompiled_changes = match (&installed, &latest) {
        (Some(installed), Some(file)) => freshness::uncompiled_changes(&installed.path, file),
        (None, _) => "Unknown (installed executable aliases identify different builds)".to_owned(),
        _ => "Unknown (on-disk binary unavailable)".to_owned(),
    };
    let package = installed_package(&clean_path);
    let (installation, progs_metadata) = installation::inspect(&clean_path);
    let package_target = available::target(&clean_path, package.as_deref());
    let mut result = ProcessVersion {
        running_unavailable: running.as_ref().err().copied(),
        runtime_name: runtime_name(name),
        application: None,
        script_application: None,
        application_build: None,
        application_installation: None,
        application_progs_metadata: ProgsMetadata::default(),
        application_package_target: None,
        application_latest_package_version: None,
        source: running_info
            .version
            .as_ref()
            .map(|_| "Running executable version switch (exact binary)".into()),
        version: running_info.version,
        latest_version: latest_info.as_ref().and_then(|info| info.version.clone()),
        package_target,
        latest_package_version: None,
        latest_build_id: latest_info.and_then(|info| info.build_id),
        running_latest_binary,
        uncompiled_changes: Some(uncompiled_changes),
        package,
        installation: Some(installation),
        progs_metadata,
        build_id: running_info.build_id,
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
        }
        result.application_package_target = result.application.as_ref().and_then(|application| {
            let owner = installed_package(&application.entry_point);
            available::target(&application.entry_point, owner.as_deref())
        });
    }
    if running.is_ok()
        && let Some(runtime @ ("Python" | "Java")) = result.runtime_name
    {
        result.script_application = script::probe(runtime, target.pid);
        if let Some(entry) = result
            .script_application
            .as_ref()
            .and_then(|app| app.entry_point.as_ref())
        {
            let (installation, metadata) = installation::inspect(entry);
            result.application_installation = Some(installation);
            result.application_progs_metadata = metadata;
            let package = installed_package(entry);
            result.application_package_target = available::target(entry, package.as_deref());
        }
    }
    if let Some(disk) = latest_identity
        && !installed
            .as_ref()
            .is_some_and(|installed| installed.unchanged(&disk))
    {
        result.latest_version = None;
        result.latest_build_id = None;
        result.running_latest_binary = None;
        result.uncompiled_changes =
            Some("Unknown (on-disk binary changed during inspection)".into());
    }
    if let Ok((_, identity)) = &running
        && (!identity_is_current()
            || !std::fs::metadata(&path).is_ok_and(|current| {
                current.dev() == identity.dev() && current.ino() == identity.ino()
            }))
    {
        result.discard_running_metadata("process exited or changed during inspection");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_version_strings_do_not_mean_identical_executable_builds() {
        let running = ExecutableVersion {
            version: Some("xfce4-terminal 1.2.0-dev-b3149143".into()),
            build_id: Some("7f024090".into()),
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
    fn unknown_executables_are_never_probed_as_programs() {
        for name in [
            "agy",
            "electron",
            "unknown-app",
            "python3.evil",
            "codex-helper",
            "",
        ] {
            assert_eq!(version_argument(name), None);
        }
        assert_eq!(version_argument("tmux"), Some("-V"));
        assert_eq!(version_argument("bwrap"), Some("--version"));
        assert_eq!(version_argument("java"), Some("-version"));
        assert_eq!(version_argument("python3.14t"), Some("--version"));
        assert_eq!(version_argument("python3.14"), Some("--version"));
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
        });
        assert_ne!(
            result.version, expected.version,
            "the live fixture must be running an older tmux build"
        );
        assert_eq!(result.latest_version, expected.version);
        assert_eq!(result.latest_build_id, expected.build_id);
        assert_eq!(result.running_latest_binary, Some(false));
        eprintln!(
            "{}",
            serde_json::json!({"pid":pid,"running_version":result.version,"latest_installed_version":result.latest_version,"running_build_id":result.build_id,"latest_build_id":result.latest_build_id,"running_latest_binary":result.running_latest_binary})
        );
    }

    #[test]
    fn versions_accept_known_multiline_output_but_not_errors_or_controls() {
        assert_eq!(
            version_line(b"\nxfce4-terminal 1.2.0-dev\nCopyright 2026\n"),
            Some("xfce4-terminal 1.2.0-dev".into())
        );
        assert_eq!(
            version_line(b"Python 3.14.0\n"),
            Some("Python 3.14.0".into())
        );
        assert_eq!(version_line(b"unavailable\n"), None);
        assert_eq!(version_line(b"app \x1b[31m1.2\n"), None);
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
            program: None,
        };
        let result = probe_version(&target);
        assert!(result.version.is_none() && result.package.is_none() && result.build_id.is_none());
    }

    #[test]
    fn exited_process_keeps_disk_metadata_without_claiming_a_running_binary() {
        let target = ProcessVersionTarget {
            pid: i32::MAX,
            executable: std::env::current_exe().unwrap(),
            program: None,
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
            program: None,
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
    #[ignore = "read-only installed bwrap and Progs inventory checks; requires this system's package metadata"]
    fn installed_bwrap_metadata_survives_process_exit() {
        let target = ProcessVersionTarget {
            pid: i32::MAX,
            executable: "/usr/bin/bwrap".into(),
            program: Some("bwrap".into()),
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
        let latest_package = available::query(result.package_target.as_ref().unwrap());
        assert!(latest_package.contains("/bubblewrap "), "{latest_package}");
        eprintln!(
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
            program: None,
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
        assert_eq!(updated.running_latest_binary, Some(true));

        std::fs::remove_file(&path).unwrap();
        std::fs::copy(
            applicationlauncher::process::executable_path("python3").unwrap(),
            &path,
        )
        .unwrap();
        let different = probe_version(&target);
        assert_eq!(different.version, version.version);
        assert!(
            different
                .latest_version
                .as_deref()
                .is_some_and(|value| value.starts_with("Python "))
        );
        assert_ne!(different.latest_version, different.version);
        assert_eq!(different.running_latest_binary, Some(false));
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
    #[ignore = "read-only version probes; requires APPLICATIONLAUNCHER_TEST_VERSION_PIDS"]
    fn live_process_versions() {
        let pids = std::env::var("APPLICATIONLAUNCHER_TEST_VERSION_PIDS")
            .expect("provide exact owner/active process PIDs");
        for pid in pids.split(',').map(|pid| pid.parse::<i32>().unwrap()) {
            let target = ProcessVersionTarget {
                pid,
                executable: std::fs::read_link(format!("/proc/{pid}/exe")).unwrap(),
                program: crate::windows::read_process_stat(pid).map(|stat| stat.name),
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
            eprintln!(
                "pid={pid} version={:?} latest={:?} running_latest={:?} uncompiled_changes={:?} package={:?} build_id={:?} latest_build_id={:?}",
                version.version,
                version.latest_version,
                version.running_latest_binary,
                version.uncompiled_changes,
                version.package,
                version.build_id,
                version.latest_build_id
            );
            eprintln!(
                "runtime={:?} application={:?}",
                version.runtime_name, version.application
            );
        }
    }
}
