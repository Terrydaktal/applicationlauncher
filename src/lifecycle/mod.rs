//! Headless restart policy and transaction coordination. Native terminals supply mechanics.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub(crate) mod application_update;

pub const PATH: &str = "/Lifecycle";
pub const INTERFACE: &str = "com.terrydaktal.ApplicationLauncher.Lifecycle1";
const LIMIT: usize = 2 * 1024 * 1024;

pub(crate) struct OperationLock(File);

impl OperationLock {
    pub(crate) fn acquire_in(directory: &Path) -> Result<Self, String> {
        private_directory(directory)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(directory.join("operation.lock"))
            .map_err(|e| e.to_string())?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err("The lifecycle lock is not private".into());
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("Another lifecycle operation or restore is in progress".into());
        }
        Ok(Self(file))
    }

    pub(crate) fn busy_in(directory: &Path) -> Result<bool, String> {
        let path = directory.join("operation.lock");
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path);
        let file = match file {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.to_string()),
        };
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err("The lifecycle lock is not private".into());
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(false);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            Ok(true)
        } else {
            Err(error.to_string())
        }
    }
}

impl Drop for OperationLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

pub fn state_dir() -> PathBuf {
    crate::tracker::state_dir().join("lifecycle")
}

pub(crate) fn private_directory(path: &Path) -> Result<(), String> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| e.to_string())?;
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("Lifecycle directory must be private and owned by this user".into());
    }
    Ok(())
}

pub(crate) fn write_private(path: &Path, value: &Value) -> Result<(), String> {
    let data = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if data.len() > LIMIT {
        return Err("Lifecycle request exceeds its size limit".into());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(&data)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    File::open(path.parent().ok_or("Missing lifecycle directory")?)
        .and_then(|file| file.sync_all())
        .map_err(|e| e.to_string())
}

pub(crate) fn read_private(path: &Path) -> Result<Value, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err("Lifecycle record must be a private same-user regular file".into());
    }
    let mut data = Vec::new();
    file.take((LIMIT + 1) as u64)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() > LIMIT {
        return Err("Lifecycle record exceeds its size limit".into());
    }
    serde_json::from_slice(&data).map_err(|e| e.to_string())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 120 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

fn valid_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

pub(crate) fn spawn_job(id: &str, mode: &str, request: &Path) -> Result<(), String> {
    if !valid_id(id)
        || !matches!(mode, "program" | "window" | "window-external")
        || !request.is_absolute()
    {
        return Err("Invalid lifecycle operation identity".into());
    }
    let binary = crate::tracker::tracker_binary_path()?;
    let mut command = Command::new("systemd-run");
    command
        .args(["--user", "--quiet", "--collect", "--service-type=exec"])
        .arg(format!("--unit=applicationlauncher-lifecycle-{id}"))
        // A recreated tmux server outlives the operation that created it.
        .arg("--property=KillMode=process");
    for key in [
        "PATH",
        "HOME",
        "XDG_RUNTIME_DIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "DBUS_SESSION_BUS_ADDRESS",
        "TMUX_TMPDIR",
    ] {
        if let Some(value) = std::env::var_os(key) {
            let mut entry = std::ffi::OsString::from(format!("--setenv={key}="));
            entry.push(value);
            command.arg(entry);
        }
    }
    command
        .arg("--")
        .arg(binary)
        .arg("--lifecycle-worker")
        .arg(mode)
        .arg(request);
    let output = crate::process::output_with_timeout(command, Duration::from_secs(8))
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "Cannot start independently managed lifecycle operation: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

pub(crate) fn wait_result(directory: &Path, timeout: Duration) -> Result<Value, String> {
    let started = Instant::now();
    let deadline = Instant::now() + timeout;
    loop {
        if directory.join("result.json").exists() {
            return read_private(&directory.join("result.json"));
        }
        if started.elapsed() > Duration::from_secs(10) && !worker_alive(directory) {
            return Err("Lifecycle worker exited without a completion record; inspect the retained plan before retrying".into());
        }
        if Instant::now() >= deadline {
            return Err("Lifecycle worker is still running or was interrupted. Its private plan/status were retained; no automatic retry is allowed".into());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

pub(crate) fn worker_alive(directory: &Path) -> bool {
    let Ok(saved) = read_private(&directory.join("worker.json")) else {
        return false;
    };
    let Some(pid) = saved["pid"]
        .as_u64()
        .filter(|pid| *pid > 1 && *pid <= u32::MAX as u64)
    else {
        return false;
    };
    let Ok(value) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some((_, fields)) = value.rsplit_once(") ") else {
        return false;
    };
    let fields: Vec<_> = fields.split_whitespace().collect();
    !matches!(fields.first().copied(), None | Some("Z" | "X" | "x"))
        && fields.get(19).and_then(|field| field.parse::<u64>().ok()) == saved["start"].as_u64()
}

pub(crate) fn worker_pending_or_alive(directory: &Path, request: &Path) -> bool {
    if directory.join("worker.json").exists() {
        return worker_alive(directory);
    }
    fs::metadata(request)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age <= Duration::from_secs(10))
}

pub fn worker_command(mode: &str, request: &Path) -> Result<Command, String> {
    if mode != "program" { return Err("Terminal replacement transport is not enabled".into()); }
    if !matches!(
        mode,
        "program" | "window" | "window-external" | "window-preview" | "gate"
    ) {
        return Err("Unknown lifecycle worker mode".into());
    }
    read_private(request)?;
    let code = format!(
        "import sys, types\nm=types.ModuleType('lifecycle_worker');sys.modules[m.__name__]=m\nexec({:?},m.__dict__)\nw=types.ModuleType('lifecycle_window');sys.modules[w.__name__]=w\nexec({:?},w.__dict__)\ne=types.ModuleType('lifecycle_external');sys.modules[e.__name__]=e\ne.PLACEMENT_JS={:?}\nexec({:?},e.__dict__)\nexec({:?})",
        include_str!("worker.py"),
        include_str!("window.py"),
        "",
        "",
        include_str!("runner.py"),
    );
    // Keep the PTY on stdin for startup gates. The anonymous script file avoids
    // Linux's per-argument limit without creating writable module files.
    let descriptor =
        unsafe { libc::memfd_create(c"applicationlauncher-lifecycle".as_ptr(), libc::MFD_CLOEXEC) };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let mut script = unsafe { File::from_raw_fd(descriptor) };
    let bootstrap = format!(
        "import os;os.set_inheritable({}, False)\n{code}",
        script.as_raw_fd()
    );
    script
        .write_all(bootstrap.as_bytes())
        .and_then(|_| script.rewind())
        .map_err(|e| e.to_string())?;
    let mut command = Command::new("/usr/bin/python3");
    command
        .arg("-I")
        .arg(format!("/proc/self/fd/{}", script.as_raw_fd()))
        .arg(mode)
        .arg(request);
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(script.as_raw_fd(), libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(command)
}

pub(crate) fn inspect_external_windows(
    windows: &[crate::tracker::TrackedWindow],
) -> Result<Value, String> {
    let directory = state_dir().join(format!(
        "preview-{}-{}",
        std::process::id(),
        crate::observability::next_operation_id()
    ));
    private_directory(&directory)?;
    let result = (|| {
        let request = directory.join("request.json");
        write_private(&request, &json!({"windows":windows}))?;
        let command = worker_command("window-preview", &request)?;
        let output = crate::process::output_with_timeout(command, Duration::from_secs(30))
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        if output.stdout.len() > LIMIT {
            return Err("Oversized terminal compatibility preview".into());
        }
        serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
    })();
    let _ = fs::remove_dir_all(directory);
    result
}

pub(crate) fn execute_external_window(id: &str, plan: &Value) -> Result<Value, String> {
    if !valid_uuid(id) {
        return Err("Invalid external terminal operation ID".into());
    }
    let directory = state_dir().join(id);
    if directory.exists() {
        return wait_result(&directory, Duration::from_secs(300));
    }
    private_directory(&directory)?;
    let mut request = json!({"protocol":1,"transport":"external","job_id":id,"confirmed":true,
        "scope_fingerprint":plan["scope_fingerprint"],"external":plan,
        "daemon_binary":crate::tracker::tracker_binary_path()?.display().to_string()});
    let environment: serde_json::Map<String, Value> = [
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "GDK_BACKEND",
        "XDG_CURRENT_DESKTOP",
    ]
    .into_iter()
    .filter_map(|key| {
        std::env::var(key)
            .ok()
            .map(|value| (key.to_owned(), json!(value)))
    })
    .collect();
    request["display_environment"] = plan
        .get("display_environment")
        .cloned()
        .unwrap_or_else(|| json!(environment));
    let path = directory.join("request.json");
    write_private(&path, &request)?;
    if let Err(error) = spawn_job(id, "window-external", &path) {
        write_private(
            &directory.join("result.json"),
            &json!({"ok":false,"message":error}),
        )?;
        return Err(error);
    }
    wait_result(&directory, Duration::from_secs(300))
}

#[derive(Default)]
pub struct Service;

#[zbus::interface(name = "com.terrydaktal.ApplicationLauncher.Lifecycle1")]
impl Service {
    fn get_capabilities(&self) -> String {
        json!({"protocol":1,"terminal_replacement":0,"application_adapters":1,"external_terminal_replacement":0}).to_string()
    }

    fn get_window_update_report(&self, id: &str) -> zbus::fdo::Result<String> {
        if !valid_uuid(id) {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Invalid lifecycle job ID".into(),
            ));
        }
        let directory = state_dir().join(id);
        let request = directory.join("request.json");
        read_private(&request).map_err(zbus::fdo::Error::Failed)?;
        let mut status = read_private(&directory.join("status.json"))
            .unwrap_or_else(|_| json!({"phase":"preparing","message":"Starting lifecycle worker"}));
        if let Ok(result) = read_private(&directory.join("result.json")) {
            status["phase"] = json!(if result["ok"] == true {
                "complete"
            } else {
                "failed"
            });
            status["message"] = result["message"].clone();
        } else if !worker_pending_or_alive(&directory, &request) {
            status["phase"] = json!("failed");
            status["message"] = json!(
                "Lifecycle worker stopped without a completion record; inspect recovery records before retrying"
            );
        }
        Ok(status.to_string())
    }

}

#[cfg(test)]
mod tests;
