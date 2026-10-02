use std::collections::HashMap;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock, mpsc::Receiver};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::models::WindowInfo;

const PROBE_TIMEOUT: Duration = Duration::from_millis(750);
const CACHE_TTL: Duration = Duration::from_secs(300);
const MAX_CACHE_ENTRIES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessVersionTarget {
    pid: i32,
    executable: PathBuf,
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
            .map(|(pid, executable)| ProcessVersionTarget { pid, executable });
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
    pub(crate) version: Option<String>,
    pub(crate) source: Option<String>,
    pub(crate) package: Option<String>,
    pub(crate) build_id: Option<String>,
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
        "xfce4-terminal" | "fish" | "bash" | "zsh" | "codex" | "codex-code-mode" | "htop"
        | "nvtop" | "python" | "python3" | "node" | "firefox" | "dolphin" | "pcmanfm"
        | "mousepad" | "copyq" | "konsole" | "alacritty" | "kitty" | "mpv" | "vim" | "nvim" => {
            Some("--version")
        }
        name if name.strip_prefix("python3.").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        }) =>
        {
            Some("--version")
        }
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

fn probe_version(target: &ProcessVersionTarget) -> ProcessVersion {
    if target.pid <= 0 {
        return ProcessVersion::default();
    }
    let path = PathBuf::from(format!("/proc/{}/exe", target.pid));
    let Ok(executable) = std::fs::read_link(&path) else {
        return ProcessVersion::default();
    };
    if clean_executable(&executable) != clean_executable(&target.executable) {
        return ProcessVersion::default();
    }
    let Ok(file) = File::open(&path) else {
        return ProcessVersion::default();
    };
    let Ok(identity) = file.metadata() else {
        return ProcessVersion::default();
    };
    type CacheKey = (u64, u64, u64, i64, i64);
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, (Instant, ProcessVersion)>>> = OnceLock::new();
    let key = (
        identity.dev(),
        identity.ino(),
        identity.len(),
        identity.mtime(),
        identity.mtime_nsec(),
    );
    let cache = CACHE.get_or_init(Default::default);
    if let Ok(cache) = cache.lock()
        && let Some((_, result)) = cache
            .get(&key)
            .filter(|(checked, _)| checked.elapsed() < CACHE_TTL)
    {
        return result.clone();
    }
    // Pin the actual running ELF, including a deleted old binary. Probing the
    // pathname on disk could otherwise report a newer, unrelated build.
    let pinned_path = format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd());
    let clean_path = clean_executable(&executable);
    let name = clean_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let mut result = ProcessVersion::default();
    if let Some(argument) = version_argument(name) {
        let mut command = Command::new(&pinned_path);
        command.arg0(name).arg(argument).env("LC_ALL", "C");
        if let Some(version) = bounded_output(command).as_deref().and_then(version_line) {
            result.version = Some(version);
            result.source = Some("Running executable version switch (exact binary)".into());
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
    result.package = installed_package(&clean_path);
    if !std::fs::metadata(&path)
        .is_ok_and(|current| current.dev() == identity.dev() && current.ino() == identity.ino())
    {
        return ProcessVersion::default();
    }
    if let Ok(mut cache) = cache.lock() {
        cache.retain(|_, (checked, _)| checked.elapsed() < CACHE_TTL);
        if cache.len() >= MAX_CACHE_ENTRIES {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (checked, _))| *checked)
                .map(|(key, _)| *key)
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(key, (Instant::now(), result.clone()));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(version_argument("python3.14"), Some("--version"));
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
        };
        let result = probe_version(&target);
        assert!(result.version.is_none() && result.package.is_none() && result.build_id.is_none());
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
        let child = Command::new(&path)
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
            .unwrap();
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
        let version = probe_version(&ProcessVersionTarget {
            pid: process.child.id() as i32,
            executable: path,
        });
        assert!(
            version
                .version
                .as_deref()
                .is_some_and(|value| value.contains("bash") && value.contains("version")),
            "{version:?}"
        );
        assert!(
            version
                .source
                .as_deref()
                .is_some_and(|value| value.contains("exact binary"))
        );
        assert!(version.build_id.is_some());
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
            };
            let version = probe_version(&target);
            assert!(
                version.version.is_some(),
                "known running program should report its version: {pid}"
            );
            assert!(version.build_id.is_some());
            eprintln!(
                "pid={pid} version={:?} source={:?} package={:?} build_id={:?}",
                version.version, version.source, version.package, version.build_id
            );
        }
    }
}
