use eframe::egui;
use std::backtrace::Backtrace;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::*;

pub(crate) struct SingleInstanceLock {
    pub(crate) path: PathBuf,
}

impl Drop for SingleInstanceLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(target_os = "linux")]
fn set_debugger_attach_enabled(enabled: bool) -> Result<(), String> {
    use rustix::process::{PTracer, set_ptracer};

    let tracer = if enabled { PTracer::Any } else { PTracer::None };
    set_ptracer(tracer).map_err(|err| format!("failed to update ptrace permission: {err}"))
}

#[cfg(not(target_os = "linux"))]
fn set_debugger_attach_enabled(_enabled: bool) -> Result<(), String> {
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

pub(crate) fn capture_running_launcher_diagnostics(socket_path: &Path) -> Result<PathBuf, String> {
    let response = send_launcher_control_request(socket_path, "diagnose\n", true)?;
    let pid = response
        .strip_prefix("debug-ready ")
        .ok_or_else(|| {
            if response.is_empty() {
                "the running launcher did not support diagnostic attachment".to_string()
            } else {
                response.clone()
            }
        })?
        .parse::<u32>()
        .map_err(|err| format!("invalid launcher PID in diagnostic response: {err}"))?;

    let result = (|| {
        let state_dir = launcher_state_dir();
        std::fs::create_dir_all(&state_dir)
            .map_err(|err| format!("failed to create launcher state directory: {err}"))?;

        let status = std::fs::read_to_string(format!("/proc/{pid}/status"))
            .unwrap_or_else(|err| format!("unable to read process status: {err}\n"));
        let thread_snapshot = Command::new("ps")
            .args([
                "-L",
                "-p",
                &pid.to_string(),
                "-o",
                "pid=,tid=,psr=,stat=,pcpu=,time=,wchan:32=,comm=",
            ])
            .output();
        let stack_output = Command::new("timeout")
            .args(["5s", "eu-stack", "-p", &pid.to_string(), "-n", "48", "-s"])
            .output();
        let stack_output = match stack_output {
            Ok(output) if output.status.success() || !output.stdout.is_empty() => Ok(output),
            _ => Command::new("timeout")
                .env("DEBUGINFOD_URLS", "")
                .args([
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
                ])
                .output(),
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
        std::fs::write(&report_path, report.as_bytes())
            .map_err(|err| format!("failed to write hang report: {err}"))?;
        std::fs::write(state_dir.join("hang-latest.log"), report.as_bytes())
            .map_err(|err| format!("failed to write latest hang report: {err}"))?;
        Ok(report_path)
    })();

    let _ = send_launcher_control_request(socket_path, "diagnose-done\n", true);
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

    match request {
        "diagnose" => {
            let response = match set_debugger_attach_enabled(true) {
                Ok(()) => {
                    std::thread::spawn(|| {
                        std::thread::sleep(Duration::from_secs(DEBUG_ATTACH_TIMEOUT_SECS));
                        let _ = set_debugger_attach_enabled(false);
                    });
                    format!("debug-ready {}\n", std::process::id())
                }
                Err(err) => format!("debug-error {err}\n"),
            };
            let _ = stream.write_all(response.as_bytes());
        }
        "diagnose-done" => {
            let response = match set_debugger_attach_enabled(false) {
                Ok(()) => "debug-disabled\n".to_string(),
                Err(err) => format!("debug-error {err}\n"),
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
        std::env::temp_dir().join(filename)
    }
}

pub(crate) fn focus_existing_launcher_window() {
    let kpath = get_kdotool_path();
    let mut ids = Vec::new();

    for args in [
        ["search", "--class", "applicationlauncher"].as_slice(),
        ["search", "--title", "Open Application Windows"].as_slice(),
    ] {
        if let Ok(output) = Command::new(&kpath).args(args).output() {
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
        let _ = Command::new(&kpath)
            .args(["windowstate", "--remove", "MINIMIZED", &id])
            .status();
        std::thread::sleep(std::time::Duration::from_millis(60));
        let _ = Command::new(&kpath).args(["windowactivate", &id]).status();
        let _ = Command::new(&kpath).args(["windowraise", &id]).status();
    }
}

pub(crate) fn request_launcher_foreground() {
    std::thread::spawn(focus_existing_launcher_window);
}
pub(crate) fn launcher_state_dir() -> PathBuf {
    if let Ok(state_home) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(state_home).join("applicationlauncher");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".local/state/applicationlauncher");
    }
    std::env::temp_dir().join("applicationlauncher")
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
        if std::fs::create_dir_all(&state_dir).is_ok() {
            let panic_log = state_dir.join("panic.log");
            let mut panic_entry = String::new();
            panic_entry.push_str("\n==== applicationlauncher panic ====\n");
            panic_entry.push_str(&format!("{:?}\n", std::time::SystemTime::now()));
            panic_entry.push_str(&message);
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&panic_log)
            {
                let _ = file.write_all(panic_entry.as_bytes());
            }
            let latest_log = state_dir.join("panic-latest.log");
            let _ = std::fs::write(latest_log, message.as_bytes());
        }

        eprintln!("{message}");
        previous_hook(panic_info);
    }));
}
