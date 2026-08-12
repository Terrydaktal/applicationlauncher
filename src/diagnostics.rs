use eframe::egui;
use std::backtrace::Backtrace;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

use crate::*;

pub(crate) struct SingleInstanceLock {
    pub(crate) path: PathBuf,
    inode: u64,
}

impl SingleInstanceLock {
    pub(crate) fn new(path: PathBuf) -> Result<Self, String> {
        let inode = std::fs::symlink_metadata(&path)
            .map_err(|err| format!("could not inspect the bound launcher socket: {err}"))?
            .ino();
        Ok(Self { path, inode })
    }
}

impl Drop for SingleInstanceLock {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.path)
            .ok()
            .is_some_and(|metadata| metadata.ino() == self.inode)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

static DEBUG_ATTACH_GENERATION: AtomicU64 = AtomicU64::new(0);

#[cfg(target_os = "linux")]
fn control_peer_pid(stream: &std::os::unix::net::UnixStream) -> Option<i32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    (result == 0 && length >= std::mem::size_of::<libc::ucred>() as libc::socklen_t)
        .then_some(credentials.pid)
}

#[cfg(not(target_os = "linux"))]
fn control_peer_pid(_stream: &std::os::unix::net::UnixStream) -> Option<i32> {
    None
}

#[cfg(target_os = "linux")]
fn pid_belongs_to_current_user(pid: u32) -> bool {
    let uid = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("Uid:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|uid| uid.parse::<u32>().ok())
        });
    uid == Some(rustix::process::getuid().as_raw())
}

#[cfg(not(target_os = "linux"))]
fn pid_belongs_to_current_user(_pid: u32) -> bool {
    true
}

#[cfg(target_os = "linux")]
fn set_debugger_attach_enabled(tracer_pid: Option<u32>) -> Result<(), String> {
    use rustix::process::{PTracer, set_ptracer};

    let tracer = match tracer_pid {
        Some(pid) => PTracer::ProcessID(
            rustix::process::Pid::from_raw(pid as i32)
                .ok_or_else(|| format!("invalid diagnostic PID {pid}"))?,
        ),
        None => PTracer::None,
    };
    set_ptracer(tracer).map_err(|err| format!("failed to update ptrace permission: {err}"))
}

#[cfg(not(target_os = "linux"))]
fn set_debugger_attach_enabled(_tracer_pid: Option<u32>) -> Result<(), String> {
    Err("on-demand debugger attachment is only supported on Linux".to_string())
}

pub(crate) fn send_launcher_control_request(
    socket_path: &Path,
    request: &str,
    wait_for_response: bool,
) -> Result<String, String> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket_path)
        .map_err(|err| format!("failed to connect to the running launcher: {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|err| format!("failed to configure launcher control socket: {err}"))?;
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("failed to send launcher control request: {err}"))?;
    let _ = stream.shutdown(std::net::Shutdown::Write);

    if !wait_for_response {
        return Ok(String::new());
    }

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|err| format!("failed to read launcher control response: {err}"))?;
    Ok(response.trim().to_string())
}

fn ensure_private_state_dir(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path)
        .map_err(|err| format!("failed to create launcher state directory: {err}"))?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|err| format!("failed to restrict launcher state directory: {err}"))?;
    Ok(())
}

pub(crate) fn capture_running_launcher_diagnostics(socket_path: &Path) -> Result<PathBuf, String> {
    let request = format!("diagnose {}\n", std::process::id());
    let response = send_launcher_control_request(socket_path, &request, true)?;
    let mut response_fields = response.split_whitespace();
    let pid = response_fields
        .next()
        .filter(|prefix| *prefix == "debug-ready")
        .and_then(|_| response_fields.next())
        .ok_or_else(|| {
            if response.is_empty() {
                "the running launcher did not support diagnostic attachment".to_string()
            } else {
                response.clone()
            }
        })?
        .parse::<u32>()
        .map_err(|err| format!("invalid launcher PID in diagnostic response: {err}"))?;
    let token = response_fields
        .next()
        .ok_or_else(|| "diagnostic response did not include an authorization token".to_string())?;

    let result = (|| {
        let state_dir = launcher_state_dir();
        ensure_private_state_dir(&state_dir)?;

        let status = std::fs::read_to_string(format!("/proc/{pid}/status"))
            .unwrap_or_else(|err| format!("unable to read process status: {err}\n"));
        let thread_snapshot = applicationlauncher::process::output_with_timeout(
            {
                let mut command = Command::new("ps");
                command.args([
                    "-L",
                    "-p",
                    &pid.to_string(),
                    "-o",
                    "pid=,tid=,psr=,stat=,pcpu=,time=,wchan:32=,comm=",
                ]);
                command
            },
            Duration::from_secs(5),
        );
        let stack_output = applicationlauncher::process::output_with_timeout(
            {
                let mut command = Command::new("timeout");
                command.args(["5s", "eu-stack", "-p", &pid.to_string(), "-n", "48", "-s"]);
                command
            },
            Duration::from_secs(8),
        );
        let stack_output = match stack_output {
            Ok(output) if output.status.success() || !output.stdout.is_empty() => Ok(output),
            _ => applicationlauncher::process::output_with_timeout(
                {
                    let mut command = Command::new("timeout");
                    command.env("DEBUGINFOD_URLS", "").args([
                        "10s",
                        "gdb",
                        "-q",
                        "-batch",
                        "-iex",
                        "set pagination off",
                        "-iex",
                        "set debuginfod enabled off",
                        "-ex",
                        "set print thread-events off",
                        "-ex",
                        "info threads",
                        "-ex",
                        "thread apply all bt 40",
                        &format!("/proc/{pid}/exe"),
                        "-p",
                        &pid.to_string(),
                    ]);
                    command
                },
                Duration::from_secs(13),
            ),
        };

        let mut report = String::new();
        report.push_str("==== applicationlauncher hang report ====\n");
        report.push_str(&format!(
            "captured: {:?}\npid: {pid}\n\n",
            std::time::SystemTime::now()
        ));
        report.push_str("---- /proc status ----\n");
        report.push_str(&status);
        report.push_str("\n---- thread snapshot ----\n");
        match thread_snapshot {
            Ok(output) => {
                report.push_str(&String::from_utf8_lossy(&output.stdout));
                report.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            Err(err) => report.push_str(&format!("failed to run ps: {err}\n")),
        }
        report.push_str("\n---- all-thread backtrace ----\n");
        match stack_output {
            Ok(output) => {
                report.push_str(&format!("exit status: {}\n", output.status));
                report.push_str(&String::from_utf8_lossy(&output.stdout));
                report.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            Err(err) => report.push_str(&format!("failed to capture thread stacks: {err}\n")),
        }

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let report_path = state_dir.join(format!("hang-{timestamp}.log"));
        write_private_report(&report_path, report.as_bytes())?;
        write_private_report(&state_dir.join("hang-latest.log"), report.as_bytes())?;
        Ok(report_path)
    })();

    let _ = send_launcher_control_request(socket_path, &format!("diagnose-done {token}\n"), true);
    result
}

pub(crate) fn handle_launcher_control_connection(
    mut stream: std::os::unix::net::UnixStream,
    ui_event_tx: &Sender<UiEvent>,
    repaint_ctx: &egui::Context,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    let mut request = [0_u8; CONTROL_REQUEST_LIMIT];
    let request_len = stream.read(&mut request).unwrap_or(0);
    let request = std::str::from_utf8(&request[..request_len])
        .unwrap_or_default()
        .trim();
    let peer_pid = control_peer_pid(&stream);

    match request {
        request if request.starts_with("diagnose ") => {
            let tracer_pid = request
                .split_whitespace()
                .nth(1)
                .and_then(|pid| pid.parse::<u32>().ok());
            let response = match tracer_pid
                .filter(|pid| *pid > 0)
                .filter(|pid| pid_belongs_to_current_user(*pid))
                .filter(|pid| cfg!(not(target_os = "linux")) || peer_pid == Some(*pid as i32))
                .ok_or_else(|| "diagnose requires the requesting debugger PID".to_string())
                .and_then(|pid| set_debugger_attach_enabled(Some(pid)))
            {
                Ok(()) => {
                    let token = DEBUG_ATTACH_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(DEBUG_ATTACH_TIMEOUT_SECS));
                        if DEBUG_ATTACH_GENERATION.load(Ordering::Acquire) == token {
                            let _ = set_debugger_attach_enabled(None);
                        }
                    });
                    format!("debug-ready {} {token}\n", std::process::id())
                }
                Err(err) => format!("debug-error {err}\n"),
            };
            let _ = stream.write_all(response.as_bytes());
        }
        request if request.starts_with("diagnose-done") => {
            let token = request
                .split_whitespace()
                .nth(1)
                .and_then(|token| token.parse::<u64>().ok());
            let response = if token
                .is_some_and(|token| DEBUG_ATTACH_GENERATION.load(Ordering::Acquire) == token)
            {
                DEBUG_ATTACH_GENERATION.fetch_add(1, Ordering::AcqRel);
                match set_debugger_attach_enabled(None) {
                    Ok(()) => "debug-disabled\n".to_string(),
                    Err(err) => format!("debug-error {err}\n"),
                }
            } else {
                "debug-disabled\n".to_string()
            };
            let _ = stream.write_all(response.as_bytes());
        }
        _ => {
            let _ = ui_event_tx.send(UiEvent::FocusLauncher);
            repaint_ctx.request_repaint();
            let _ = stream.write_all(b"focus-requested\n");
        }
    }
}

pub(crate) fn get_socket_path(mode: LauncherMode) -> PathBuf {
    let filename = match mode {
        LauncherMode::Apps => "applicationlauncher-apps.sock",
        LauncherMode::Windows => "applicationlauncher-windows.sock",
    };
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime_dir).join(filename)
    } else {
        launcher_state_dir().join(filename)
    }
}

pub(crate) fn focus_existing_launcher_window() {
    let kpath = get_kdotool_path();
    let mut ids = Vec::new();

    for args in [["search", "--title", "Open Application Windows"].as_slice()] {
        if let Ok(output) = applicationlauncher::process::output_with_timeout(
            {
                let mut command = Command::new(&kpath);
                command.args(args);
                command
            },
            Duration::from_secs(3),
        ) {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for id in stdout.lines().map(str::trim).filter(|id| !id.is_empty()) {
                    if !ids.iter().any(|existing: &String| existing == id) {
                        ids.push(id.to_string());
                    }
                }
            }
        }
    }

    for id in ids {
        let _ = applicationlauncher::process::status_with_timeout(
            {
                let mut command = Command::new(&kpath);
                command.args(["windowstate", "--remove", "MINIMIZED", &id]);
                command
            },
            Duration::from_secs(3),
        );
        std::thread::sleep(std::time::Duration::from_millis(60));
        let _ = applicationlauncher::process::status_with_timeout(
            {
                let mut command = Command::new(&kpath);
                command.args(["windowactivate", &id]);
                command
            },
            Duration::from_secs(3),
        );
        let _ = applicationlauncher::process::status_with_timeout(
            {
                let mut command = Command::new(&kpath);
                command.args(["windowraise", &id]);
                command
            },
            Duration::from_secs(3),
        );
    }
}

pub(crate) fn request_launcher_foreground() {
    static FOCUS_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
    if FOCUS_IN_FLIGHT.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(|| {
        focus_existing_launcher_window();
        FOCUS_IN_FLIGHT.store(false, Ordering::Release);
    });
}
pub(crate) fn launcher_state_dir() -> PathBuf {
    if let Ok(state_home) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(state_home).join("applicationlauncher");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".local/state/applicationlauncher");
    }
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join("applicationlauncher");
    }
    PathBuf::from(format!(
        "/tmp/applicationlauncher-{}",
        rustix::process::getuid().as_raw()
    ))
}

pub(crate) fn write_stderr_line(message: &str) {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "{message}");
}

fn write_private_report(path: &Path, contents: &[u8]) -> Result<(), String> {
    applicationlauncher::process::atomic_write(path, contents)
        .map_err(|err| format!("failed to write {}: {err}", path.display()))?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|err| format!("failed to restrict {}: {err}", path.display()))?;
    Ok(())
}

pub(crate) fn install_panic_hook() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let mut message = String::new();
        message.push_str(&format!("panic: {panic_info}\n"));
        if let Some(location) = panic_info.location() {
            message.push_str(&format!(
                "location: {}:{}:{}\n",
                location.file(),
                location.line(),
                location.column()
            ));
        }
        message.push_str(&format!("backtrace:\n{}\n", Backtrace::force_capture()));

        let state_dir = launcher_state_dir();
        if ensure_private_state_dir(&state_dir).is_ok() {
            let panic_log = state_dir.join("panic.log");
            let mut panic_entry = String::new();
            panic_entry.push_str("\n==== applicationlauncher panic ====\n");
            panic_entry.push_str(&format!("{:?}\n", std::time::SystemTime::now()));
            panic_entry.push_str(&message);
            const MAX_PANIC_LOG_BYTES: u64 = 4 * 1024 * 1024;
            let current_size = std::fs::metadata(&panic_log)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            if current_size < MAX_PANIC_LOG_BYTES {
                if let Ok(mut file) = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .mode(0o600)
                    .open(&panic_log)
                {
                    let remaining = (MAX_PANIC_LOG_BYTES - current_size) as usize;
                    let entry = panic_entry.as_bytes();
                    let _ = file.write_all(&entry[..entry.len().min(remaining)]);
                }
            } else {
                let _ =
                    applicationlauncher::process::atomic_write(&panic_log, panic_entry.as_bytes());
            }
            let latest_log = state_dir.join("panic-latest.log");
            let _ = applicationlauncher::process::atomic_write(&latest_log, message.as_bytes());
            #[cfg(unix)]
            let _ = std::fs::set_permissions(
                state_dir.join("panic-latest.log"),
                std::fs::Permissions::from_mode(0o600),
            );
        }

        write_stderr_line(&message);
        previous_hook(panic_info);
    }));
}
