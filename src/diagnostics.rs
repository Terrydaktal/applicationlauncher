use eframe::egui;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
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
    applicationlauncher::observe! { applicationlauncher::observability::increment(
        applicationlauncher::observability::Counter::GuiControlRequests,
    ); }
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    let mut request = Vec::new();
    if BufReader::new((&mut stream).take(CONTROL_REQUEST_LIMIT as u64 + 1))
        .read_until(b'\n', &mut request)
        .is_err()
        || request.len() > CONTROL_REQUEST_LIMIT
    {
        let _ = stream.write_all(b"error: invalid or oversized request\n");
        return;
    }
    let request = std::str::from_utf8(&request).unwrap_or_default().trim();
    match request {
        "shutdown" => {
            let _ = ui_event_tx.send(UiEvent::ShutdownLauncher);
            repaint_ctx.request_repaint();
            let _ = stream.write_all(b"shutdown-requested\n");
        }
        "focus-source-changes-pending" => {
            applicationlauncher::observe! { applicationlauncher::observability::increment(
                applicationlauncher::observability::Counter::FocusRequests,
            ); }
            applicationlauncher::observe! { applicationlauncher::observability::record(
                applicationlauncher::observability::Event::new("gui-control", "focus")
                    .reason("source-changes-pending"),
            ); }
            let _ = ui_event_tx.send(UiEvent::FocusLauncher {
                source_changes_pending: true,
                symbols_unarchived: false,
            });
            repaint_ctx.request_repaint();
            let _ = stream.write_all(b"focus-requested\n");
        }
        "focus-symbols-unarchived" => {
            applicationlauncher::observe! { applicationlauncher::observability::increment(
                applicationlauncher::observability::Counter::FocusRequests,
            ); }
            let _ = ui_event_tx.send(UiEvent::FocusLauncher {
                source_changes_pending: false,
                symbols_unarchived: true,
            });
            repaint_ctx.request_repaint();
            let _ = stream.write_all(b"focus-requested\n");
        }
        "focus-deployment-warnings" => {
            applicationlauncher::observe! { applicationlauncher::observability::increment(
                applicationlauncher::observability::Counter::FocusRequests,
            ); }
            let _ = ui_event_tx.send(UiEvent::FocusLauncher {
                source_changes_pending: true,
                symbols_unarchived: true,
            });
            repaint_ctx.request_repaint();
            let _ = stream.write_all(b"focus-requested\n");
        }
        request if request.starts_with("show-terminal-info") => {
            match crate::window_info_request::TerminalInfoRequest::from_control(request) {
                Ok(request) => {
                    crate::window_info_request::request_info(
                        request,
                        ui_event_tx.clone(),
                        repaint_ctx.clone(),
                    );
                    let _ = stream.write_all(b"info-requested\n");
                }
                Err(error) => {
                    let _ = writeln!(stream, "error: {error}");
                }
            }
        }
        _ => {
            applicationlauncher::observe! { applicationlauncher::observability::increment(
                applicationlauncher::observability::Counter::FocusRequests,
            ); }
            applicationlauncher::observe! { applicationlauncher::observability::record(
                applicationlauncher::observability::Event::new("gui-control", "focus")
                    .reason("launcher-trigger"),
            ); }
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

pub(crate) fn request_launcher_foreground() {
    crate::popups::foreground::request(egui::ViewportId::ROOT);
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

#[cfg(feature = "observability")]
pub(crate) fn install_panic_hook() {
    applicationlauncher::observe! { applicationlauncher::observability::install_panic_hook(
        applicationlauncher::observability::Component::Gui,
    ); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_info_request_does_not_focus_an_unrelated_window() {
        let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        client
            .write_all(b"show-terminal-info 42 not-a-uuid\n")
            .unwrap();
        let (events, received) = std::sync::mpsc::channel();
        handle_launcher_control_connection(server, &events, &egui::Context::default());
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("error:"));
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn control_requests_are_framed_across_socket_reads() {
        let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let sender = std::thread::spawn(move || {
            client.write_all(b"foc").unwrap();
            std::thread::sleep(Duration::from_millis(20));
            client.write_all(b"us\n").unwrap();
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            assert_eq!(response, "focus-requested\n");
        });
        let (events, received) = std::sync::mpsc::channel();
        handle_launcher_control_connection(server, &events, &egui::Context::default());
        assert!(matches!(
            received.recv().unwrap(),
            UiEvent::FocusLauncher { .. }
        ));
        sender.join().unwrap();
    }

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
                _ => panic!("focus request became a different UI request"),
            }
        }
    }
}
