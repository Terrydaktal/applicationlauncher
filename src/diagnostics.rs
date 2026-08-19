use eframe::egui;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

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

pub(crate) fn handle_launcher_control_connection(
    mut stream: std::os::unix::net::UnixStream,
    ui_event_tx: &Sender<UiEvent>,
    repaint_ctx: &egui::Context,
) {
    applicationlauncher::observability::increment(
        applicationlauncher::observability::Counter::GuiControlRequests,
    );
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    let mut request = [0_u8; CONTROL_REQUEST_LIMIT];
    let request_len = stream.read(&mut request).unwrap_or(0);
    let request = std::str::from_utf8(&request[..request_len])
        .unwrap_or_default()
        .trim();
    match request {
        "shutdown" => {
            let _ = ui_event_tx.send(UiEvent::ShutdownLauncher);
            repaint_ctx.request_repaint();
            let _ = stream.write_all(b"shutdown-requested\n");
        }
        "focus-source-changes-pending" => {
            applicationlauncher::observability::increment(
                applicationlauncher::observability::Counter::FocusRequests,
            );
            applicationlauncher::observability::record(
                applicationlauncher::observability::Event::new("gui-control", "focus")
                    .reason("source-changes-pending"),
            );
            let _ = ui_event_tx.send(UiEvent::FocusLauncher {
                source_changes_pending: true,
                symbols_unarchived: false,
            });
            repaint_ctx.request_repaint();
            let _ = stream.write_all(b"focus-requested\n");
        }
        "focus-symbols-unarchived" => {
            applicationlauncher::observability::increment(
                applicationlauncher::observability::Counter::FocusRequests,
            );
            let _ = ui_event_tx.send(UiEvent::FocusLauncher {
                source_changes_pending: false,
                symbols_unarchived: true,
            });
            repaint_ctx.request_repaint();
            let _ = stream.write_all(b"focus-requested\n");
        }
        "focus-deployment-warnings" => {
            applicationlauncher::observability::increment(
                applicationlauncher::observability::Counter::FocusRequests,
            );
            let _ = ui_event_tx.send(UiEvent::FocusLauncher {
                source_changes_pending: true,
                symbols_unarchived: true,
            });
            repaint_ctx.request_repaint();
            let _ = stream.write_all(b"focus-requested\n");
        }
        _ => {
            applicationlauncher::observability::increment(
                applicationlauncher::observability::Counter::FocusRequests,
            );
            applicationlauncher::observability::record(
                applicationlauncher::observability::Event::new("gui-control", "focus")
                    .reason("launcher-trigger"),
            );
            let _ = ui_event_tx.send(UiEvent::FocusLauncher {
                source_changes_pending: false,
                symbols_unarchived: false,
            });
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

pub(crate) fn install_panic_hook() {
    applicationlauncher::observability::install_panic_hook(
        applicationlauncher::observability::Component::Gui,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_control_request_carries_source_change_state() {
        for (request, expected_source, expected_symbols) in [
            ("focus\n", false, false),
            ("focus-source-changes-pending\n", true, false),
            ("focus-symbols-unarchived\n", false, true),
            ("focus-deployment-warnings\n", true, true),
        ] {
            let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
            client.write_all(request.as_bytes()).unwrap();
            let (event_tx, event_rx) = std::sync::mpsc::channel();

            handle_launcher_control_connection(server, &event_tx, &egui::Context::default());

            match event_rx.recv().unwrap() {
                UiEvent::FocusLauncher {
                    source_changes_pending,
                    symbols_unarchived,
                } => {
                    assert_eq!(source_changes_pending, expected_source);
                    assert_eq!(symbols_unarchived, expected_symbols);
                }
                UiEvent::ShutdownLauncher => panic!("focus request became a shutdown request"),
            }
        }
    }
}
